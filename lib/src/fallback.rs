//! Authenticated immutable expiry payout identity.

use std::io;

use bitcoin::bip32::{self, ChildNumber, Xpriv, Xpub};
use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{self, schnorr, Keypair, Message, PublicKey};
use bitcoin::{Network, Script, ScriptBuf};

use crate::{ProtocolDecodingError, ProtocolEncoding, ReadExt, WriteExt, SECP};
use crate::mailbox::BlindedMailboxIdentifier;

const RECORD_VERSION: u8 = 3;
const MAX_ADDRESS_SCRIPT_LEN: usize = 42;
const RECORD_DOMAIN: &[u8; 32] = b"Ark expiry fallback record v3   ";
const ACCOUNT_DOMAIN: &[u8; 32] = b"Ark expiry fallback account v3  ";

/// Evidence identifying one normal child of a registered coin account.
///
/// The server must still authenticate the registration and derive the child.
/// The blinded mailbox identifier alone is not an ownership proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerProof {
	pub blinded_mailbox_id: BlindedMailboxIdentifier,
	pub child_index: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerProofError {
	#[error("owner key index is outside the normal child range")]
	InvalidChildIndex,
	#[error("invalid blinded mailbox identifier")]
	InvalidMailboxIdentifier,
}

impl OwnerProof {
	pub fn new(
		blinded_mailbox_id: BlindedMailboxIdentifier, child_index: u32,
	) -> Result<Self, OwnerProofError> {
		let proof = Self { blinded_mailbox_id, child_index };
		proof.validate()?;
		Ok(proof)
	}

	/// Check transport or persisted evidence before using its public key.
	pub fn validate(&self) -> Result<(), OwnerProofError> {
		ChildNumber::from_normal_idx(self.child_index)
			.map_err(|_| OwnerProofError::InvalidChildIndex)?;
		PublicKey::from_slice(self.blinded_mailbox_id.as_ref())
			.map_err(|_| OwnerProofError::InvalidMailboxIdentifier)?;
		Ok(())
	}
}

#[derive(Debug, thiserror::Error)]
pub enum FallbackError {
	#[error("fallback destination is not a supported standard address")]
	InvalidScript,
	#[error("fallback account is for another network")]
	WrongNetwork,
	#[error("invalid fallback key or signature: {0}")]
	Signature(#[from] secp256k1::Error),
	#[error("invalid coin-account derivation: {0}")]
	Derivation(#[from] bip32::Error),
}

fn validate_script(spk: &Script) -> Result<(), FallbackError> {
	if spk.is_p2pkh() || spk.is_p2sh() || spk.is_p2wpkh() || spk.is_p2wsh() || spk.is_p2tr() {
		Ok(())
	} else {
		Err(FallbackError::InvalidScript)
	}
}

/// One write-once mailbox/account/destination binding. Both signers authorize
/// every field, including the chain. The binding is portable between servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRecord {
	pub spk: ScriptBuf,
	pub account_xpub: Xpub,
	pub mailbox_signature: schnorr::Signature,
	pub account_signature: schnorr::Signature,
}

impl FallbackRecord {
	pub fn new(
		network: Network, spk: ScriptBuf, account: &Xpriv, mailbox_key: &Keypair,
	) -> Result<Self, FallbackError> {
		validate_script(&spk)?;
		if account.network != network.into() { return Err(FallbackError::WrongNetwork); }
		let account_xpub = Xpub::from_priv(&SECP, account);
		let child0 = account.derive_priv(&SECP, &[ChildNumber::Normal { index: 0 }])?.to_keypair(&SECP);
		let mailbox_pk = mailbox_key.public_key();
		let chain = ChainHash::using_genesis_block_const(network);
		let mailbox_message = Self::message(RECORD_DOMAIN, chain, mailbox_pk, &spk, &account_xpub);
		let account_message = Self::message(ACCOUNT_DOMAIN, chain, mailbox_pk, &spk, &account_xpub);
		Ok(Self {
			spk, account_xpub,
			mailbox_signature: SECP.sign_schnorr_with_aux_rand(&mailbox_message, mailbox_key, &rand::random()),
			account_signature: SECP.sign_schnorr_with_aux_rand(&account_message, &child0, &rand::random()),
		})
	}

	pub fn verify(&self, network: Network, mailbox_pk: PublicKey) -> Result<(), FallbackError> {
		validate_script(&self.spk)?;
		if self.account_xpub.network != network.into() { return Err(FallbackError::WrongNetwork); }
		let chain = ChainHash::using_genesis_block_const(network);
		let mailbox_message = Self::message(RECORD_DOMAIN, chain, mailbox_pk, &self.spk, &self.account_xpub);
		let account_message = Self::message(ACCOUNT_DOMAIN, chain, mailbox_pk, &self.spk, &self.account_xpub);
		let child0 = self.account_xpub.derive_pub(&SECP, &[ChildNumber::Normal { index: 0 }])?.public_key;
		SECP.verify_schnorr(&self.mailbox_signature, &mailbox_message, &mailbox_pk.x_only_public_key().0)?;
		SECP.verify_schnorr(&self.account_signature, &account_message, &child0.x_only_public_key().0)?;
		Ok(())
	}

	/// Two devices can sign the same binding with different signature randomness.
	pub fn same_binding(&self, other: &Self) -> bool {
		self.spk == other.spk && self.account_xpub == other.account_xpub
	}

	fn message(
		domain: &[u8; 32], chain: ChainHash, mailbox_pk: PublicKey, spk: &Script, account: &Xpub,
	) -> Message {
		let mut engine = sha256::Hash::engine();
		engine.input(domain);
		engine.input(chain.as_bytes());
		engine.input(&mailbox_pk.serialize());
		// Both callers validate a standard address script before hashing.
		// Its CompactSize length is therefore exactly one byte.
		engine.input(&[spk.len() as u8]);
		engine.input(spk.as_bytes());
		engine.input(&account.encode());
		Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array())
	}
}

impl ProtocolEncoding for FallbackRecord {
	fn encode<W: io::Write + ?Sized>(&self, writer: &mut W) -> Result<(), io::Error> {
		writer.emit_u8(RECORD_VERSION)?;
		writer.emit_compact_size(self.spk.len() as u64)?;
		writer.emit_slice(self.spk.as_bytes())?;
		writer.emit_slice(&self.account_xpub.encode())?;
		self.mailbox_signature.encode(writer)?;
		self.account_signature.encode(writer)
	}

	fn decode<R: io::Read + ?Sized>(reader: &mut R) -> Result<Self, ProtocolDecodingError> {
		if reader.read_u8()? != RECORD_VERSION {
			return Err(ProtocolDecodingError::invalid("unknown fallback record version"));
		}
		let length = reader.read_compact_size()?;
		if length == 0 || length > MAX_ADDRESS_SCRIPT_LEN as u64 {
			return Err(ProtocolDecodingError::invalid("invalid fallback address script length"));
		}
		let length = length as usize;
		let mut bytes = [0; MAX_ADDRESS_SCRIPT_LEN];
		reader.read_slice(&mut bytes[..length])?;
		let spk = ScriptBuf::from(bytes[..length].to_vec());
		validate_script(&spk).map_err(|e| ProtocolDecodingError::invalid_err(e, "invalid fallback address"))?;
		let encoded_account = reader.read_byte_array::<78>()?;
		let account_xpub = Xpub::decode(&encoded_account)
			.map_err(|e| ProtocolDecodingError::invalid_err(e, "invalid fallback account"))?;
		Ok(Self {
			spk, account_xpub,
			mailbox_signature: schnorr::Signature::decode(reader)?,
			account_signature: schnorr::Signature::decode(reader)?,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fixture() -> (Keypair, Xpriv, ScriptBuf) {
		let mailbox = Keypair::from_seckey_slice(&SECP, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
			0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]).unwrap();
		let root = Xpriv::new_master(Network::Regtest, &[3; 64]).unwrap();
		let account = root.derive_priv(&SECP, &[
			ChildNumber::from_hardened_idx(350).unwrap(), ChildNumber::from_hardened_idx(0).unwrap(),
		]).unwrap();
		let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		(mailbox, account, spk)
	}

	// Independent stdlib BIP32/BIP340 reference, with zero auxiliary randomness.
	// The reference was first checked against Bitcoin BIPs bip-0340 vector0.
	const RECORD_VECTOR: &str = concat!(
		"031600141111111111111111111111111111111111111111043587cf024476a352800000008c78a9c02948953892",
		"f618a3c1000f6d16ec8865a14c5c29d65c4faed238b87403c3291c3018339178d9d7e1ad87963f723bf54f86e3ec",
		"0698a01437b9e97a49dd9dda9e133cd845ce832ae2c7b6ee513089fa24a64b6b37b17de35eb820a691f384cd05b5",
		"67dd4c4519fe605f3d2c70daa86663804ef1afd0e48f677827e6fcb9e4fff425ee04302876eb327e701c0227e48b",
		"1c17c1bb3ab13944c4fe6c1bced88f90919547831d02800513c51aad73dbcd2c2a73fe5e0069f5547ff08cc58a08",
	);

	#[test]
	fn fallback_record_v3_independent_vector() {
		let (mailbox, account, spk) = fixture();
		let record = FallbackRecord::deserialize_hex(RECORD_VECTOR).unwrap();
		record.verify(Network::Regtest, mailbox.public_key()).unwrap();
		assert_eq!(record.spk, spk);
		assert_eq!(record.account_xpub, Xpub::from_priv(&SECP, &account));
		assert_eq!(record.serialize_hex(), RECORD_VECTOR);
		assert_eq!(record.serialize().len(), 230);
		let signed = FallbackRecord::new(Network::Regtest, spk, &account, &mailbox).unwrap();
		signed.verify(Network::Regtest, mailbox.public_key()).unwrap();
		assert!(record.same_binding(&signed));
		assert!(!record.same_binding(&FallbackRecord { spk: ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([2; 20])), ..signed }));
	}

	#[test]
	fn fallback_record_v3_binds_every_field_and_role() {
		let (mailbox, _, _) = fixture();
		let record = FallbackRecord::deserialize_hex(RECORD_VECTOR).unwrap();
		assert!(record.verify(Network::Signet, mailbox.public_key()).is_err());
		assert!(record.verify(Network::Bitcoin, mailbox.public_key()).is_err());
		assert!(record.verify(Network::Regtest, mailbox.public_key().negate(&SECP)).is_err());
		let mut swapped = record.clone();
		std::mem::swap(&mut swapped.mailbox_signature, &mut swapped.account_signature);
		assert!(swapped.verify(Network::Regtest, mailbox.public_key()).is_err());
		let wire = record.serialize();
		for index in 0..wire.len() {
			let mut changed = wire.clone();
			changed[index] ^= 1;
			if let Ok(changed) = FallbackRecord::deserialize(&changed) {
				assert!(changed.verify(Network::Regtest, mailbox.public_key()).is_err(), "byte {index} was not bound");
			}
		}
	}

	#[test]
	fn fallback_record_v3_decoder_rejects_malformed_lengths_and_versions() {
		let record = FallbackRecord::deserialize_hex(RECORD_VECTOR).unwrap();
		let wire = record.serialize();
		for length in 0..wire.len() {
			assert!(FallbackRecord::deserialize(&wire[..length]).is_err());
		}
		let mut trailing = wire.clone();
		trailing.push(0);
		assert!(FallbackRecord::deserialize(&trailing).is_err());
		for bad in [vec![2], vec![3, 0], vec![3, 43], vec![3, 253, 22, 0], vec![3, 255, 255, 255, 255, 255, 255, 255, 255, 255]] {
			assert!(FallbackRecord::deserialize(&bad).is_err());
		}
		let mut unsupported = wire.clone();
		unsupported[2] = 0x6a;
		assert!(FallbackRecord::deserialize(&unsupported).is_err());
	}

}
