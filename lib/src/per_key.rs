//! Portable immutable expiry destinations and signed ownership of individual keys.

use std::io;

use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{self, schnorr, Keypair, Message, PublicKey};
use bitcoin::{Network, Script, ScriptBuf};

use crate::{ProtocolDecodingError, ProtocolEncoding, ReadExt, WriteExt, SECP};

const RECORD_VERSION: u8 = 4;
const LINK_VERSION: u8 = 1;
const RECORD_DOMAIN: &[u8; 32] = b"Ark expiry per-key record v1    ";
const LINK_DOMAIN: &[u8; 32] = b"Ark expiry per-key link v1      ";
const MAX_SCRIPT_LEN: usize = 42;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("fallback destination is not a supported standard address")]
	InvalidScript,
	#[error("recovery key index must be non-hardened")]
	HardenedIndex,
	#[error("invalid fallback signature: {0}")]
	Signature(#[from] secp256k1::Error),
}

fn validate_script(spk: &Script) -> Result<(), Error> {
	if spk.is_p2pkh() || spk.is_p2sh() || spk.is_p2wpkh() || spk.is_p2wsh() || spk.is_p2tr() {
		Ok(())
	} else {
		Err(Error::InvalidScript)
	}
}

/// The mailbox authorizes one chain and destination. Signature randomness is
/// not part of the immutable identity. The record is portable between servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRecord {
	pub spk: ScriptBuf,
	pub mailbox_signature: schnorr::Signature,
}

impl FallbackRecord {
	pub fn new(network: Network, spk: ScriptBuf, mailbox: &Keypair) -> Result<Self, Error> {
		validate_script(&spk)?;
		let digest = Self::digest(network, mailbox.public_key(), &spk);
		Ok(Self {
			spk,
			mailbox_signature: SECP.sign_schnorr_with_aux_rand(&digest, mailbox, &rand::random()),
		})
	}

	pub fn verify(&self, network: Network, mailbox: PublicKey) -> Result<(), Error> {
		validate_script(&self.spk)?;
		SECP.verify_schnorr(&self.mailbox_signature,
			&Self::digest(network, mailbox, &self.spk), &mailbox.x_only_public_key().0)?;
		Ok(())
	}

	pub fn same_binding(&self, other: &Self) -> bool { self.spk == other.spk }

	pub fn binding_digest(&self, network: Network, mailbox: PublicKey) -> Message {
		Self::digest(network, mailbox, &self.spk)
	}

	fn digest(network: Network, mailbox: PublicKey, spk: &Script) -> Message {
		let mut engine = sha256::Hash::engine();
		engine.input(RECORD_DOMAIN);
		engine.input(ChainHash::using_genesis_block_const(network).as_bytes());
		engine.input(&mailbox.serialize());
		// Supported standard address scripts have a one-byte CompactSize length.
		engine.input(&[spk.len() as u8]);
		engine.input(spk.as_bytes());
		Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array())
	}
}

impl ProtocolEncoding for FallbackRecord {
	fn encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<(), io::Error> {
		validate_script(&self.spk).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
		w.emit_u8(RECORD_VERSION)?;
		w.emit_compact_size(self.spk.len() as u64)?;
		w.emit_slice(self.spk.as_bytes())?;
		self.mailbox_signature.encode(w)
	}

	fn decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, ProtocolDecodingError> {
		if r.read_u8()? != RECORD_VERSION {
			return Err(ProtocolDecodingError::invalid("unknown per-key fallback record version"));
		}
		let length = r.read_compact_size()?;
		if length == 0 || length > MAX_SCRIPT_LEN as u64 {
			return Err(ProtocolDecodingError::invalid("invalid fallback script length"));
		}
		let mut bytes = [0; MAX_SCRIPT_LEN];
		r.read_slice(&mut bytes[..length as usize])?;
		let spk = ScriptBuf::from(bytes[..length as usize].to_vec());
		validate_script(&spk).map_err(|e| ProtocolDecodingError::invalid_err(e, "invalid fallback script"))?;
		Ok(Self { spk, mailbox_signature: schnorr::Signature::decode(r)? })
	}
}

/// An individual policy key authorizes its exact immutable mailbox/destination.
/// The optional index is a signed recovery hint. It never authorizes attribution
/// or lets a server derive another key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLink {
	pub user_pubkey: PublicKey,
	pub recovery_index: Option<u32>,
	pub signature: schnorr::Signature,
}

impl KeyLink {
	pub fn new(network: Network, mailbox: PublicKey, record: &FallbackRecord,
		coin_key: &Keypair, recovery_index: Option<u32>,
	) -> Result<Self, Error> {
		record.verify(network, mailbox)?;
		Self::validate_index(recovery_index)?;
		let key = coin_key.public_key();
		let message = Self::message(network, mailbox, record, key, recovery_index);
		Ok(Self { user_pubkey: key, recovery_index,
			signature: SECP.sign_schnorr_with_aux_rand(&message, coin_key, &rand::random()) })
	}

	pub fn verify(&self, network: Network, mailbox: PublicKey, record: &FallbackRecord) -> Result<(), Error> {
		record.verify(network, mailbox)?;
		Self::validate_index(self.recovery_index)?;
		SECP.verify_schnorr(&self.signature,
			&Self::message(network, mailbox, record, self.user_pubkey, self.recovery_index),
			&self.user_pubkey.x_only_public_key().0)?;
		Ok(())
	}

	pub fn same_binding(&self, other: &Self) -> bool {
		self.user_pubkey == other.user_pubkey && self.recovery_index == other.recovery_index
	}

	fn validate_index(index: Option<u32>) -> Result<(), Error> {
		if index.is_some_and(|index| index >= 1 << 31) { return Err(Error::HardenedIndex); }
		Ok(())
	}

	fn message(network: Network, mailbox: PublicKey, record: &FallbackRecord,
		key: PublicKey, index: Option<u32>,
	) -> Message {
		let mut engine = sha256::Hash::engine();
		engine.input(LINK_DOMAIN);
		engine.input(record.binding_digest(network, mailbox).as_ref());
		// Include parity even though BIP340 verifies the x-only signing key.
		engine.input(&key.serialize());
		match index {
			None => engine.input(&[0]),
			Some(index) => { engine.input(&[1]); engine.input(&index.to_le_bytes()); },
		}
		Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array())
	}
}

impl ProtocolEncoding for KeyLink {
	fn encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<(), io::Error> {
		Self::validate_index(self.recovery_index).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
		w.emit_u8(LINK_VERSION)?;
		self.user_pubkey.encode(w)?;
		match self.recovery_index {
			None => w.emit_u8(0)?,
			Some(index) => { w.emit_u8(1)?; w.emit_u32(index)?; },
		}
		self.signature.encode(w)
	}

	fn decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, ProtocolDecodingError> {
		if r.read_u8()? != LINK_VERSION {
			return Err(ProtocolDecodingError::invalid("unknown fallback key link version"));
		}
		let user_pubkey = PublicKey::decode(r)?;
		let recovery_index = match r.read_u8()? {
			0 => None,
			1 => Some(r.read_u32()?),
			_ => return Err(ProtocolDecodingError::invalid("invalid recovery hint marker")),
		};
		Self::validate_index(recovery_index)
			.map_err(|e| ProtocolDecodingError::invalid_err(e, "invalid recovery hint"))?;
		Ok(Self { user_pubkey, recovery_index, signature: schnorr::Signature::decode(r)? })
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::str::FromStr;
	use bitcoin::hex::DisplayHex;
	const RECORD: &str = "04160014111111111111111111111111111111111111111198168e8bd79a572fe39e84a5cb7d722492284d73e918d182876c27b03ff7023255ad2b6710d8eb4cc7610fbe32fa85fb35699f3056520ab34bdfe2a2d8d5daa4";
	const LINK: &str = "0103774ae7f858a9411e5ef4246b70c65aac5649980be5c17891bbec17895da008cb0132000000bdde1c69e6731b6145a0b6bb74cd1b62416dce6d0896582dcdffa60fa7247c5c3dc37e4ca84ab3423d640a36f615a0f9106d67e9f0f4d73d46e11f800106642c";
	const MAILBOX: &str = "025cbdf0646e5db4eaa398f365f2ea7a0e3d419b7e0330e39ce92bddedcac4f9bc";
	const RECORD_DIGEST: &str = "55b7782ed8cf813461bdb3310ea53345188049afa47a13dab45649b8ef8b7c00";

	#[test]
	fn per_key_independent_vectors_and_semantic_retries() {
		let mailbox = PublicKey::from_str(MAILBOX).unwrap();
		let record = FallbackRecord::deserialize_hex(RECORD).unwrap();
		let link = KeyLink::deserialize_hex(LINK).unwrap();
		record.verify(Network::Regtest, mailbox).unwrap();
		link.verify(Network::Regtest, mailbox, &record).unwrap();
		assert_eq!(record.serialize_hex(), RECORD);
		assert_eq!(link.serialize_hex(), LINK);
		assert_eq!(record.binding_digest(Network::Regtest, mailbox).as_ref().as_hex().to_string(), RECORD_DIGEST);
		assert_eq!(record.serialize().len(), 88);
		assert_eq!(link.serialize().len(), 103);
		assert!(record.verify(Network::Signet, mailbox).is_err());
		assert!(record.verify(Network::Regtest, mailbox.negate(&SECP)).is_err());
		let mut wrong_destination = record.clone();
		wrong_destination.spk = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
		assert!(link.verify(Network::Regtest, mailbox, &wrong_destination).is_err());
		assert!(link.verify(Network::Signet, mailbox, &record).is_err());
		let mut changed = link.clone();
		changed.user_pubkey = changed.user_pubkey.negate(&SECP);
		assert!(changed.verify(Network::Regtest, mailbox, &record).is_err());
		changed = link.clone();
		changed.recovery_index = Some(25);
		assert!(changed.verify(Network::Regtest, mailbox, &record).is_err());
		let key = Keypair::from_seckey_slice(&SECP, &[7; 32]).unwrap();
		let a = FallbackRecord::new(Network::Regtest, record.spk.clone(), &key).unwrap();
		let b = FallbackRecord::new(Network::Regtest, record.spk.clone(), &key).unwrap();
		assert!(a.same_binding(&b));
		let a = KeyLink::new(Network::Regtest, key.public_key(), &a, &key, None).unwrap();
		let b = KeyLink::new(Network::Regtest, key.public_key(), &b, &key, None).unwrap();
		assert!(a.same_binding(&b));
		assert_eq!(a.serialize().len(), 99);
	}

	#[test]
	fn per_key_canonical_bounded_decoders() {
		let record = FallbackRecord::deserialize_hex(RECORD).unwrap().serialize();
		let link = KeyLink::deserialize_hex(LINK).unwrap().serialize();
		for n in 0..record.len() { assert!(FallbackRecord::deserialize(&record[..n]).is_err()); }
		for n in 0..link.len() { assert!(KeyLink::deserialize(&link[..n]).is_err()); }
		let mut extra = record.clone(); extra.push(0);
		assert!(FallbackRecord::deserialize(&extra).is_err());
		let mut extra = link.clone(); extra.push(0);
		assert!(KeyLink::deserialize(&extra).is_err());
		for wire in [vec![3], vec![4,0], vec![4,43]] {
			assert!(FallbackRecord::deserialize(&wire).is_err());
		}
		let mut nonminimal = vec![4,253,22,0];
		nonminimal.extend_from_slice(&record[2..]);
		assert!(FallbackRecord::deserialize(&nonminimal).is_err());
		let mut malformed = link.clone(); malformed[34] = 2;
		assert!(KeyLink::deserialize(&malformed).is_err());
		let mut malformed = link; malformed[38] |= 128;
		assert!(KeyLink::deserialize(&malformed).is_err());
	}
}
