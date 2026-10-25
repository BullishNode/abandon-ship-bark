//! Authenticated immutable expiry payout identity.

use std::io;

use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{self, schnorr, Keypair, Message, PublicKey};
use bitcoin::{Network, Script, ScriptBuf};

use crate::{ProtocolDecodingError, ProtocolEncoding, ReadExt, WriteExt, SECP};
use crate::mailbox::{BlindedMailboxIdentifier, MailboxBlindingError, MailboxIdentifier};

const MAX_ADDRESS_SCRIPT_LEN: usize = 42;

const LINK_TAG: &str = "Ark/ExpiryOwnerLink/v1";
const REGISTRATION_TAG: &str = "Ark/FallbackRegistration/v4";
const REGISTRATION_VERSION: u8 = 4;

fn tagged_message(tag: &str, fields: &[&[u8]]) -> Message {
	let tag_hash = sha256::Hash::hash(tag.as_bytes());
	let mut engine = sha256::Hash::engine();
	engine.input(tag_hash.as_byte_array());
	engine.input(tag_hash.as_byte_array());
	for field in fields { engine.input(field); }
	Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array())
}

/// A coin key's authorization of its fallback mailbox, not proof that the
/// signer owns the mailbox. The link is valid only for this server mailbox key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OwnerLink {
	pub blinded_id: BlindedMailboxIdentifier,
	pub signature: schnorr::Signature,
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerLinkError {
	#[error("invalid blinded mailbox identifier")]
	InvalidMailboxIdentifier,
	#[error("invalid mailbox link signature: {0}")]
	Signature(#[from] secp256k1::Error),
	#[error("mailbox blinding failed: {0}")]
	Blinding(#[from] MailboxBlindingError),
}

impl OwnerLink {
	pub fn new(
		server_mailbox_pubkey: PublicKey, mailbox: MailboxIdentifier, coin_key: &Keypair,
	) -> Result<Self, OwnerLinkError> {
		let blinded_id = mailbox.to_blinded(server_mailbox_pubkey, coin_key)?;
		let message = Self::message(server_mailbox_pubkey, blinded_id);
		Ok(Self {
			blinded_id,
			signature: SECP.sign_schnorr_with_aux_rand(&message, coin_key, &rand::random()),
		})
	}

	pub fn from_parts(
		blinded_id: BlindedMailboxIdentifier, signature: schnorr::Signature,
	) -> Result<Self, OwnerLinkError> {
		PublicKey::from_slice(blinded_id.as_ref()).map_err(|_| OwnerLinkError::InvalidMailboxIdentifier)?;
		Ok(Self { blinded_id, signature })
	}

	pub fn message(server_mailbox_pubkey: PublicKey, blinded_id: BlindedMailboxIdentifier) -> Message {
		tagged_message(LINK_TAG, &[&server_mailbox_pubkey.serialize(), blinded_id.as_ref()])
	}

	/// Verify with the actual output key. Use its full key for later unblinding.
	pub fn verify(&self, server_mailbox_pubkey: PublicKey, coin_pubkey: PublicKey) -> Result<(), OwnerLinkError> {
		PublicKey::from_slice(self.blinded_id.as_ref()).map_err(|_| OwnerLinkError::InvalidMailboxIdentifier)?;
		SECP.verify_schnorr(&self.signature, &Self::message(server_mailbox_pubkey, self.blinded_id),
			&coin_pubkey.x_only_public_key().0)?;
		Ok(())
	}
}

impl ProtocolEncoding for OwnerLink {
	fn encode<W: io::Write + ?Sized>(&self, writer: &mut W) -> Result<(), io::Error> {
		self.blinded_id.encode(writer)?;
		self.signature.encode(writer)
	}

	fn decode<R: io::Read + ?Sized>(reader: &mut R) -> Result<Self, ProtocolDecodingError> {
		let blinded_id = BlindedMailboxIdentifier::decode(reader)?;
		let signature = schnorr::Signature::decode(reader)?;
		Ok(Self { blinded_id, signature })
	}
}

/// Immutable mailbox-to-script registration for the signature-link protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRegistration {
	pub spk: ScriptBuf,
	pub mailbox_signature: schnorr::Signature,
}

impl FallbackRegistration {
	pub fn new(network: Network, spk: ScriptBuf, mailbox_key: &Keypair) -> Result<Self, FallbackError> {
		validate_script(&spk)?;
		let message = Self::message(network, mailbox_key.public_key(), &spk);
		Ok(Self {
			spk,
			mailbox_signature: SECP.sign_schnorr_with_aux_rand(&message, mailbox_key, &rand::random()),
		})
	}

	pub fn verify(&self, network: Network, mailbox_pk: PublicKey) -> Result<(), FallbackError> {
		validate_script(&self.spk)?;
		SECP.verify_schnorr(&self.mailbox_signature, &Self::message(network, mailbox_pk, &self.spk),
			&mailbox_pk.x_only_public_key().0)?;
		Ok(())
	}

	/// Compare authorized fields; valid signatures may use different randomness.
	pub fn same_binding(&self, other: &Self) -> bool { self.spk == other.spk }

	fn message(network: Network, mailbox_pk: PublicKey, spk: &Script) -> Message {
		let chain = ChainHash::using_genesis_block_const(network);
		// Supported standard scripts are at most 42 bytes: CompactSize is one byte.
		tagged_message(REGISTRATION_TAG, &[
			chain.as_bytes(), &mailbox_pk.serialize(), &[spk.len() as u8], spk.as_bytes(),
		])
	}
}

impl ProtocolEncoding for FallbackRegistration {
	fn encode<W: io::Write + ?Sized>(&self, writer: &mut W) -> Result<(), io::Error> {
		writer.emit_u8(REGISTRATION_VERSION)?;
		writer.emit_compact_size(self.spk.len() as u64)?;
		writer.emit_slice(self.spk.as_bytes())?;
		self.mailbox_signature.encode(writer)
	}

	fn decode<R: io::Read + ?Sized>(reader: &mut R) -> Result<Self, ProtocolDecodingError> {
		if reader.read_u8()? != REGISTRATION_VERSION {
			return Err(ProtocolDecodingError::invalid("unknown fallback registration version"));
		}
		let length = reader.read_compact_size()?;
		if length == 0 || length > MAX_ADDRESS_SCRIPT_LEN as u64 {
			return Err(ProtocolDecodingError::invalid("invalid fallback address script length"));
		}
		let mut bytes = [0; MAX_ADDRESS_SCRIPT_LEN];
		reader.read_slice(&mut bytes[..length as usize])?;
		let spk = ScriptBuf::from(bytes[..length as usize].to_vec());
		validate_script(&spk).map_err(|e| ProtocolDecodingError::invalid_err(e, "invalid fallback address"))?;
		Ok(Self { spk, mailbox_signature: schnorr::Signature::decode(reader)? })
	}
}

#[derive(Debug, thiserror::Error)]
pub enum FallbackError {
	#[error("fallback destination is not a supported standard address")]
	InvalidScript,
	#[error("invalid fallback key or signature: {0}")]
	Signature(#[from] secp256k1::Error),
}

fn validate_script(spk: &Script) -> Result<(), FallbackError> {
	if spk.is_p2pkh() || spk.is_p2sh() || spk.is_p2wpkh() || spk.is_p2wsh() || spk.is_p2tr() {
		Ok(())
	} else {
		Err(FallbackError::InvalidScript)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::hex::FromHex;

	fn key(number: u8) -> Keypair {
		let mut secret = [0; 32];
		secret[31] = number;
		Keypair::from_seckey_slice(&SECP, &secret).unwrap()
	}

	// BIP340 reference signatures with fixed test secrets and auxiliary randomness.
	const LINK_VECTORS: &[(u8, &str, &str, &str, &str, &str)] = &[
		(3, "02f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9", "0229757774cc6f3be1d5f1774aefa8f02e50bc64404230e7a67e8fde79bd559a9a", "da42a56153c6f269eea2c97393e88eea256dcd9d554d18386ec6b709ab11b711", "20a352f4f1e0868fa74f8ff5752927ec0b05b2b554daeccdabe8fe02e3d14e7eb42941fc34d91d18ab45b8a44223377a891f39948a59a5e510b1bf4bb0160e5c", "ea9dd7bd98ba7700de8152cadb0586128d49e47ecde94aee356017b04dee49f8dfe082657f00408b680af76e141c159572a16ab7dc33bdf393a10c108797a73d"),
		(6, "03fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a1460297556", "02948dcadf5990e048aa3874d46abef9d701858f95de8041d2a6828c99e2262519", "74903115d7f20b3124b4f5dea1f762879a8dff0f980bdfc53e67a6461842c06d", "0219945c78ec6a9dabec5c9fedc7fad15d49192ff2e5be05be7e935e737d918874b24c5cfd7f488e4517ad7d8999e1789691b0b748ef87bef51095ea98f4d0f0", "16db56328b662f47a92faee52a6d278bd27dbe212573b57e0b3f1c286062bd0f9ef019e48ea3aaef0949f08e2f4196e9ccf0073f377248c67595bf988497a5bc"),
	];

	#[test]
	fn owner_link_tagged_hash_vector() {
		let server = key(1).public_key();
		let blinded = BlindedMailboxIdentifier::from_pubkey(key(2).public_key());
		let message = OwnerLink::message(server, blinded);
		assert_eq!(message.as_ref(), &<[u8; 32]>::from_hex(
			"7fb6a1d20e0d45d209d6d7854bdff6c823d67116c6e7f90249d0885d7bc5b675",
		).unwrap());
	}

	#[test]
	fn owner_link_independent_vectors_and_re_signing() {
		let server = key(11);
		let mailbox = MailboxIdentifier::from_pubkey(key(17).public_key());
		for &(secret, pubkey, blinded, digest, sig0, sig1) in LINK_VECTORS {
			let coin = key(secret);
			assert_eq!(coin.public_key().to_string(), pubkey);
			let blinded = BlindedMailboxIdentifier::deserialize_hex(blinded).unwrap();
			assert_eq!(mailbox.to_blinded(server.public_key(), &coin).unwrap(), blinded);
			let message = OwnerLink::message(server.public_key(), blinded);
			assert_eq!(message.as_ref(), &<[u8; 32]>::from_hex(digest).unwrap());
			for (aux, signature) in [([0; 32], sig0), ([1; 32], sig1)] {
				let signature: schnorr::Signature = signature.parse().unwrap();
				assert_eq!(SECP.sign_schnorr_with_aux_rand(&message, &coin, &aux), signature);
				let link = OwnerLink::from_parts(blinded, signature).unwrap();
				link.verify(server.public_key(), coin.public_key()).unwrap();
				assert_eq!(OwnerLink::deserialize(&link.serialize()).unwrap(), link);
				assert_eq!(link.serialize().len(), 97);
				assert!(link.verify(server.public_key(), key(18).public_key()).is_err());
				assert!(link.verify(key(13).public_key(), coin.public_key()).is_err());
			}
			let link = OwnerLink::new(server.public_key(), mailbox, &coin).unwrap();
			link.verify(server.public_key(), coin.public_key()).unwrap();
			assert_eq!(MailboxIdentifier::from_blinded(link.blinded_id, coin.public_key(), &server).unwrap(), mailbox);
		}
	}

	#[test]
	fn owner_link_rejects_mailbox_translation_and_malformed_evidence() {
		let server = key(11);
		let coin = key(3);
		let mailbox = MailboxIdentifier::from_pubkey(key(17).public_key());
		let mut link = OwnerLink::new(server.public_key(), mailbox, &coin).unwrap();
		let translated = link.blinded_id.as_pubkey().combine(&mailbox.as_pubkey().negate(&SECP)).unwrap()
			.combine(&key(19).public_key()).unwrap();
		link.blinded_id = BlindedMailboxIdentifier::from_pubkey(translated);
		assert!(link.verify(server.public_key(), coin.public_key()).is_err());
		assert_eq!(MailboxIdentifier::from_blinded(link.blinded_id, coin.public_key(), &server).unwrap(),
			MailboxIdentifier::from_pubkey(key(19).public_key()));

		let wire = OwnerLink::new(server.public_key(), mailbox, &coin).unwrap().serialize();
		for length in 0..wire.len() { assert!(OwnerLink::deserialize(&wire[..length]).is_err()); }
		let mut trailing = wire.clone(); trailing.push(0);
		assert!(OwnerLink::deserialize(&trailing).is_err());
		let mut invalid = wire.clone(); invalid[..33].fill(0);
		assert!(OwnerLink::deserialize(&invalid).is_err());
		let mut invalid = wire; invalid[33..].fill(255);
		assert!(OwnerLink::deserialize(&invalid).unwrap().verify(server.public_key(), coin.public_key()).is_err());
		let invalid = BlindedMailboxIdentifier::from_slice(&[0; 33]).unwrap();
		assert!(OwnerLink::from_parts(invalid, link.signature).is_err());
		assert!(OwnerLink { blinded_id: invalid, ..link }.verify(server.public_key(), coin.public_key()).is_err());
	}

	#[test]
	fn owner_link_unblinding_rejects_infinity() {
		let server = key(11);
		let coin = key(3);
		let blinded = BlindedMailboxIdentifier::from_pubkey(key(33).public_key());
		let link = OwnerLink::from_parts(blinded,
			SECP.sign_schnorr_with_aux_rand(&OwnerLink::message(server.public_key(), blinded), &coin, &[0; 32])).unwrap();
		link.verify(server.public_key(), coin.public_key()).unwrap();
		assert!(MailboxIdentifier::from_blinded(link.blinded_id, coin.public_key(), &server).is_err());
		let cancelling_mailbox = MailboxIdentifier::from_pubkey(key(33).public_key().negate(&SECP));
		assert!(OwnerLink::new(server.public_key(), cancelling_mailbox, &coin).is_err());
	}

	const REGISTRATION_VECTORS: &[(Network, &str, &str, &str)] = &[
		(Network::Bitcoin, "5d80a4f2cf6682132e2a740292f3ba1544c5642ee6e73d814eaee13be7b6c7d9", "a4cfa078ef69ba847c04598c059efa1e767e7265a789e75e57005fc79bbdf0b4d5d9e8156cb8f0b9be792095288a343c8443cf8ce71194e90af24a4b6604637b", "04225120defdea4cdb677750a420fee807eacf21eb9898ae79b9768766e4faa04a2d4a34a4cfa078ef69ba847c04598c059efa1e767e7265a789e75e57005fc79bbdf0b4d5d9e8156cb8f0b9be792095288a343c8443cf8ce71194e90af24a4b6604637b"),
		(Network::Signet, "f656e6345420adae9ba7571036223db250078e281e2036808bf489612e157dfa", "542afc35604ba2e43f062dbf36bcb38cf35fcbcd453f88ec7a6ca09b952bd589f236b69e5e7884597e197e45c2e1bb95906a7551ea09b6cbae037c998346dce0", "04225120defdea4cdb677750a420fee807eacf21eb9898ae79b9768766e4faa04a2d4a34542afc35604ba2e43f062dbf36bcb38cf35fcbcd453f88ec7a6ca09b952bd589f236b69e5e7884597e197e45c2e1bb95906a7551ea09b6cbae037c998346dce0"),
	];

	#[test]
	fn fallback_registration_v4_independent_vectors() {
		let mailbox = key(17);
		for &(network, digest, signature, wire) in REGISTRATION_VECTORS {
			let record = FallbackRegistration::deserialize_hex(wire).unwrap();
			record.verify(network, mailbox.public_key()).unwrap();
			assert_eq!(record.serialize_hex(), wire);
			let message = FallbackRegistration::message(network, mailbox.public_key(), &record.spk);
			assert_eq!(message.as_ref(), &<[u8; 32]>::from_hex(digest).unwrap());
			assert_eq!(SECP.sign_schnorr_with_aux_rand(&message, &mailbox, &[0; 32]).to_string(), signature);
			let other = FallbackRegistration::new(network, record.spk.clone(), &mailbox).unwrap();
			assert!(record.same_binding(&other));
			other.verify(network, mailbox.public_key()).unwrap();
			assert!(record.verify(Network::Regtest, mailbox.public_key()).is_err());
			assert!(record.verify(network, key(18).public_key()).is_err());
			assert!(record.verify(network, mailbox.public_key().negate(&SECP)).is_err());
		}
	}

	#[test]
	fn fallback_registration_v4_binds_fields_and_rejects_other_formats() {
		let record = FallbackRegistration::deserialize_hex(REGISTRATION_VECTORS[0].3).unwrap();
		let wire = record.serialize();
		for length in 0..wire.len() { assert!(FallbackRegistration::deserialize(&wire[..length]).is_err()); }
		for index in 0..wire.len() {
			let mut changed = wire.clone(); changed[index] ^= 1;
			if let Ok(changed) = FallbackRegistration::deserialize(&changed) {
				assert!(changed.verify(Network::Bitcoin, key(17).public_key()).is_err(), "byte {index}");
			}
		}
		let mut trailing = wire; trailing.push(0);
		assert!(FallbackRegistration::deserialize(&trailing).is_err());
		for bad in [vec![3], vec![4, 0], vec![4, 43], vec![4, 253, 34, 0], vec![4, 255, 255, 255, 255, 255, 255, 255, 255, 255]] {
			assert!(FallbackRegistration::deserialize(&bad).is_err());
		}
		assert!(FallbackRegistration::new(Network::Bitcoin, ScriptBuf::new(), &key(17)).is_err());
		let changed = FallbackRegistration {
			spk: ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap(), ..record.clone()
		};
		assert!(!changed.same_binding(&record));
	}


}
