//! Authenticated immutable expiry payout identity.

use std::io;

use bitcoin::bip32::{self, ChildNumber, Xpriv, Xpub};
use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{self, schnorr, Keypair, Message, PublicKey};
use bitcoin::{Network, Script, ScriptBuf};

use crate::{ProtocolDecodingError, ProtocolEncoding, ReadExt, WriteExt, SECP};
use crate::mailbox::{BlindedMailboxIdentifier, MailboxBlindingError, MailboxIdentifier};

const RECORD_VERSION: u8 = 3;
const MAX_ADDRESS_SCRIPT_LEN: usize = 42;
const RECORD_DOMAIN: &[u8; 32] = b"Ark expiry fallback record v3   ";
const ACCOUNT_DOMAIN: &[u8; 32] = b"Ark expiry fallback account v3  ";

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
/// The active account record below is removed with the server's atomic cutover.
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
		assert!(FallbackRegistration::deserialize_hex(RECORD_VECTOR).is_err());
		assert!(FallbackRecord::deserialize(&record.serialize()).is_err());
		for bad in [vec![3], vec![4, 0], vec![4, 43], vec![4, 253, 34, 0], vec![4, 255, 255, 255, 255, 255, 255, 255, 255, 255]] {
			assert!(FallbackRegistration::deserialize(&bad).is_err());
		}
		assert!(FallbackRegistration::new(Network::Bitcoin, ScriptBuf::new(), &key(17)).is_err());
		let changed = FallbackRegistration {
			spk: ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap(), ..record.clone()
		};
		assert!(!changed.same_binding(&record));
	}


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
