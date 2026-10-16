//! Authentication for the fixed internal fee-reservation service. No balance,
//! ownership or permission to publish follows from authenticating a request.
use anyhow::Context;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{schnorr, Keypair, Message, PublicKey};
use bitcoin::{BlockHash, Transaction};

use super::ordinary_fee_publication::PublicationPath;
use super::ordinary_fee_publication::PublicationRecord;
use super::ordinary_fee_source::FinalFeeSource;

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_TRANSACTION_BYTES: usize = 400_000;
const DOMAIN: &[u8] = b"bark/track-c/native-fee-reservation/v1";
const REPLY_DOMAIN: &[u8] = b"bark/track-c/native-fee-reservation/reply/v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeeReply {
	version: u8,
	request_sha256: sha256::Hash,
	original_publication: PublicationRecord,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	current_source: Option<FinalFeeSource>,
	signature: schnorr::Signature,
}
fn reply_message(identity: &FeeServiceIdentity, request: sha256::Hash, record: &PublicationRecord, source: Option<&FinalFeeSource>) -> anyhow::Result<Message> {
	let mut engine = sha256::Hash::engine(); engine.input(REPLY_DOMAIN);
	engine.input(&identity.server_pubkey.serialize()); engine.input(identity.genesis.as_byte_array());
	engine.input(identity.policy_sha256.as_byte_array()); engine.input(identity.opening_sha256.as_byte_array());
	engine.input(request.as_byte_array()); engine.input(sha256::Hash::hash(&serde_json::to_vec(record)?).as_byte_array());
	if let Some(source) = source {
		engine.input(b"current-source-v1"); engine.input(sha256::Hash::hash(&serde_json::to_vec(source)?).as_byte_array());
	}
	Ok(Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array()))
}
impl FeeReply {
	pub(crate) fn sign(request: &[u8], record: PublicationRecord, identity: &FeeServiceIdentity,
		key: &Keypair,
	) -> anyhow::Result<Vec<u8>> {
		ensure!(key.public_key() == identity.server_pubkey, "native owner response key differs");
		let request_sha256 = sha256::Hash::hash(request);
		let signature = crate::SECP.sign_schnorr(&reply_message(identity, request_sha256, &record, None)?, key);
		let bytes = serde_json::to_vec(&Self { version: 1, request_sha256, original_publication: record, current_source: None, signature })?;
		ensure!(bytes.len() <= 4 * 1024 * 1024, "native owner response exceeds bound");
		Ok(bytes)
	}
	pub(crate) fn sign_current(request: &[u8], record: PublicationRecord, source: FinalFeeSource,
		identity: &FeeServiceIdentity, key: &Keypair,
	) -> anyhow::Result<Vec<u8>> {
		ensure!(key.public_key() == identity.server_pubkey, "native source response key differs");
		let request_sha256 = sha256::Hash::hash(request);
		let signature = crate::SECP.sign_schnorr(&reply_message(identity, request_sha256, &record, Some(&source))?, key);
		let bytes = serde_json::to_vec(&Self { version: 2, request_sha256, original_publication: record, current_source: Some(source), signature })?;
		ensure!(bytes.len() <= 4 * 1024 * 1024, "native source response exceeds bound");
		Ok(bytes)
	}
	fn decode(bytes: &[u8], request: &[u8], identity: &FeeServiceIdentity) -> anyhow::Result<Self> {
		ensure!(bytes.len() <= 4 * 1024 * 1024, "native owner response exceeds bound");
		let reply: Self = serde_json::from_slice(bytes)?;
		ensure!(serde_json::to_vec(&reply)? == bytes
			&& ((reply.version == 1 && reply.current_source.is_none()) || (reply.version == 2 && reply.current_source.is_some()))
			&& reply.request_sha256 == sha256::Hash::hash(request), "native owner reply differs from exact request or source contract");
		crate::SECP.verify_schnorr(&reply.signature, &reply_message(identity, reply.request_sha256, &reply.original_publication, reply.current_source.as_ref())?,
			&identity.server_pubkey.x_only_public_key().0)?;
		Ok(reply)
	}
	pub(crate) fn authenticate(bytes: &[u8], request: &[u8], identity: &FeeServiceIdentity) -> anyhow::Result<PublicationRecord> {
		let reply = Self::decode(bytes, request, identity)?;
		ensure!(reply.current_source.is_none(), "final source evidence requires source-aware consumer");
		Ok(reply.original_publication)
	}
	pub(crate) fn authenticate_current(bytes: &[u8], request: &[u8], identity: &FeeServiceIdentity)
		-> anyhow::Result<(PublicationRecord, FinalFeeSource)> {
		let reply = Self::decode(bytes, request, identity)?;
		Ok((reply.original_publication, reply.current_source.context("native final source evidence missing")?))
	}
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FeeRequestAction { Reserve, Verify, Revalidate }

// The service builds its expected identity from actual verified Core, the
// existing server key, stored native policy and immutable original opening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FeeServiceIdentity {
	pub server_pubkey: PublicKey,
	pub genesis: BlockHash,
	pub policy_sha256: sha256::Hash,
	pub opening_sha256: sha256::Hash,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeeRequest {
	version: u8,
	action: FeeRequestAction,
	purpose: PublicationPath,
	server_pubkey: PublicKey,
	genesis: BlockHash,
	policy_sha256: sha256::Hash,
	opening_sha256: sha256::Hash,
	raw_transaction: Vec<u8>,
	signature: schnorr::Signature,
}
fn message(action: FeeRequestAction, purpose: PublicationPath, identity: &FeeServiceIdentity, raw: &[u8]) -> Message {
	let mut engine = sha256::Hash::engine();
	engine.input(DOMAIN); engine.input(&[1]);
	engine.input(&[match action { FeeRequestAction::Reserve => 1, FeeRequestAction::Verify => 2, FeeRequestAction::Revalidate => 3 }]);
	engine.input(&[match purpose { PublicationPath::Round => 1, PublicationPath::Pool => 2 }]);
	engine.input(&identity.server_pubkey.serialize()); engine.input(identity.genesis.as_byte_array());
	engine.input(identity.policy_sha256.as_byte_array()); engine.input(identity.opening_sha256.as_byte_array());
	engine.input(&(raw.len() as u64).to_le_bytes()); engine.input(sha256::Hash::hash(raw).as_byte_array());
	Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array())
}
impl FeeRequest {
	pub(crate) fn sign(action: FeeRequestAction, purpose: PublicationPath, identity: &FeeServiceIdentity,
		tx: &Transaction, server_key: &Keypair,
	) -> anyhow::Result<Vec<u8>> {
		ensure!(server_key.public_key() == identity.server_pubkey, "fee requester server key differs");
		let raw_transaction = serialize(tx);
		ensure!(raw_transaction.len() <= MAX_TRANSACTION_BYTES, "fee request transaction exceeds bound");
		let signature = crate::SECP.sign_schnorr(&message(action, purpose, identity, &raw_transaction), server_key);
		let request = Self { version: 1, action, purpose, server_pubkey: identity.server_pubkey,
			genesis: identity.genesis, policy_sha256: identity.policy_sha256, opening_sha256: identity.opening_sha256,
			raw_transaction, signature };
		let bytes = serde_json::to_vec(&request)?;
		ensure!(bytes.len() <= MAX_REQUEST_BYTES, "fee request exceeds encoded byte bound");
		Ok(bytes)
	}
	pub(crate) fn authenticate(bytes: &[u8], expected_action: FeeRequestAction,
		expected: &FeeServiceIdentity,
	) -> anyhow::Result<Self> {
		ensure!(bytes.len() <= MAX_REQUEST_BYTES, "fee request exceeds encoded byte bound");
		ensure!(expected.policy_sha256.to_byte_array().iter().any(|byte| *byte != 0)
			&& expected.opening_sha256.to_byte_array().iter().any(|byte| *byte != 0),
			"fee service requires original policy and opening identities");
		let request: Self = serde_json::from_slice(bytes)?;
		ensure!(serde_json::to_vec(&request)? == bytes, "fee request is not canonical JSON");
		ensure!(request.version == 1 && request.action == expected_action,
			"fee request version or RPC action differs");
		ensure!(request.server_pubkey == expected.server_pubkey && request.genesis == expected.genesis
			&& request.policy_sha256 == expected.policy_sha256 && request.opening_sha256 == expected.opening_sha256,
			"fee request differs from actual owning service identity");
		ensure!(request.raw_transaction.len() <= MAX_TRANSACTION_BYTES, "fee request transaction exceeds bound");
		crate::SECP.verify_schnorr(&request.signature, &message(request.action, request.purpose, expected, &request.raw_transaction), &expected.server_pubkey.x_only_public_key().0)?;
		let tx: Transaction = deserialize(&request.raw_transaction)?;
		ensure!(serialize(&tx) == request.raw_transaction && !tx.is_coinbase() && !tx.input.is_empty()
			&& tx.input.len() <= 4096 && tx.weight().to_wu() <= 400_000,
			"fee request is not a bounded canonical spending transaction");
		Ok(request)
	}
	pub(crate) fn transaction(&self) -> anyhow::Result<Transaction> { Ok(deserialize(&self.raw_transaction)?) }
	pub(crate) fn purpose(&self) -> PublicationPath { self.purpose }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn source() -> (Keypair, FeeServiceIdentity, Transaction) {
		let key = Keypair::new(&crate::SECP, &mut bitcoin::secp256k1::rand::thread_rng());
		let identity = FeeServiceIdentity { server_pubkey: key.public_key(),
			genesis: bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash(),
			policy_sha256: sha256::Hash::hash(b"synthetic exact policy, not run authority"),
			opening_sha256: sha256::Hash::hash(b"synthetic exact opening, not run authority") };
		let tx = Transaction { version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint::new(bitcoin::Txid::all_zeros(), 1),
				script_sig: bitcoin::ScriptBuf::new(), sequence: bitcoin::Sequence::MAX,
				witness: bitcoin::Witness::from_slice(&[vec![0; 64]]) }],
			output: vec![bitcoin::TxOut { value: bitcoin::Amount::from_sat(10_000), script_pubkey: bitcoin::ScriptBuf::new() }] };
		(key, identity, tx)
	}
	#[test]
	fn exact_server_request_does_not_grant_another_method_purpose_or_candidate() {
		let (key, identity, tx) = source();
		let bytes = FeeRequest::sign(FeeRequestAction::Reserve, PublicationPath::Round, &identity, &tx, &key).unwrap();
		let original = FeeRequest::authenticate(&bytes, FeeRequestAction::Reserve, &identity).unwrap();
		assert_eq!(original.transaction().unwrap(), tx);
		assert!(FeeRequest::authenticate(&bytes, FeeRequestAction::Verify, &identity).is_err());
		for which in 0..3 {
			let mut changed = original.clone();
			match which { 0 => changed.purpose = PublicationPath::Pool, 1 => changed.action = FeeRequestAction::Verify,
				_ => changed.raw_transaction.push(0) }
			assert!(FeeRequest::authenticate(&serde_json::to_vec(&changed).unwrap(), changed.action, &identity).is_err());
		}
	}
	#[test]
	fn request_refuses_foreign_key_chain_policy_and_opening() {
		let (key, identity, tx) = source();
		let bytes = FeeRequest::sign(FeeRequestAction::Reserve, PublicationPath::Pool, &identity, &tx, &key).unwrap();
		for which in 0..4 {
			let mut other = identity.clone();
			match which { 0 => other.server_pubkey = source().0.public_key(),
				1 => other.genesis = bitcoin::constants::genesis_block(bitcoin::Network::Signet).block_hash(),
				2 => other.policy_sha256 = sha256::Hash::hash(b"other policy"),
				_ => other.opening_sha256 = sha256::Hash::hash(b"other opening") }
			assert!(FeeRequest::authenticate(&bytes, FeeRequestAction::Reserve, &other).is_err());
		}
	}
	#[test]
	fn duplicate_fields_oversize_and_noncanonical_frames_are_refused() {
		let (key, identity, tx) = source();
		let bytes = FeeRequest::sign(FeeRequestAction::Reserve, PublicationPath::Round, &identity, &tx, &key).unwrap();
		let duplicate = String::from_utf8(bytes.clone()).unwrap().replacen("{", "{\"version\":1,", 1);
		assert!(FeeRequest::authenticate(duplicate.as_bytes(), FeeRequestAction::Reserve, &identity).is_err());
		let mut padded = bytes; padded.push(b'\n');
		assert!(FeeRequest::authenticate(&padded, FeeRequestAction::Reserve, &identity).is_err());
		assert!(FeeRequest::authenticate(&vec![b' '; MAX_REQUEST_BYTES + 1], FeeRequestAction::Reserve, &identity).is_err());
	}
	fn reply_vector() -> (Keypair, FeeServiceIdentity, Transaction, PublicationRecord) {
		let (key, mut identity, mut tx) = source();
		let policy = super::super::ordinary_fee::OrdinaryFeePolicy::default();
		identity.policy_sha256 = sha256::Hash::hash(&serde_json::to_vec(&policy).unwrap());
		let parent = Transaction { version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint::null(), script_sig: bitcoin::ScriptBuf::new(),
				sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::new() }],
			output: vec![bitcoin::TxOut { value: bitcoin::Amount::from_sat(11_000), script_pubkey: bitcoin::ScriptBuf::new() }] };
		tx.input[0].previous_output = bitcoin::OutPoint::new(parent.compute_txid(), 0);
		let record = PublicationRecord::new_precommit(PublicationPath::Round, policy, &tx, &[parent],
			bitcoin::FeeRate::from_sat_per_vb_u32(100)).unwrap();
		(key, identity, tx, record)
	}
	#[test]
	fn owner_reply_requires_actual_endpoint_key_and_exact_action_candidate_request() {
		let (key, identity, tx, record) = reply_vector();
		let request = FeeRequest::sign(FeeRequestAction::Reserve, PublicationPath::Round, &identity, &tx, &key).unwrap();
		let reply = FeeReply::sign(&request, record.clone(), &identity, &key).unwrap();
		assert_eq!(FeeReply::authenticate(&reply, &request, &identity).unwrap(), record);
		let verify = FeeRequest::sign(FeeRequestAction::Verify, PublicationPath::Round, &identity, &tx, &key).unwrap();
		assert!(FeeReply::authenticate(&reply, &verify, &identity).is_err());
		let mut changed = tx; changed.lock_time = bitcoin::absolute::LockTime::from_height(7).unwrap();
		let another = FeeRequest::sign(FeeRequestAction::Reserve, PublicationPath::Round, &identity, &changed, &key).unwrap();
		assert!(FeeReply::authenticate(&reply, &another, &identity).is_err());
		let mut foreign = identity.clone(); foreign.server_pubkey = source().0.public_key();
		assert!(FeeReply::authenticate(&reply, &request, &foreign).is_err());
	}
	#[test]
	fn owner_reply_refuses_reencoded_duplicate_and_foreign_original_opening() {
		let (key, identity, tx, record) = reply_vector();
		let request = FeeRequest::sign(FeeRequestAction::Reserve, PublicationPath::Round, &identity, &tx, &key).unwrap();
		let reply = FeeReply::sign(&request, record, &identity, &key).unwrap();
		let duplicate = String::from_utf8(reply.clone()).unwrap().replacen("{", "{\"version\":1,", 1);
		assert!(FeeReply::authenticate(duplicate.as_bytes(), &request, &identity).is_err());
		let mut padded = reply.clone(); padded.push(b' ');
		assert!(FeeReply::authenticate(&padded, &request, &identity).is_err());
		let mut foreign = identity; foreign.opening_sha256 = sha256::Hash::hash(b"another actual original opening");
		assert!(FeeReply::authenticate(&reply, &request, &foreign).is_err());
	}

	#[test]
	fn current_owner_reply_cannot_be_discarded_replayed_as_verify_or_changed() {
		let (key, identity, tx, record) = reply_vector();
		let request = FeeRequest::sign(FeeRequestAction::Revalidate, PublicationPath::Round, &identity, &tx, &key).unwrap();
		let source = FinalFeeSource::synthetic_for_test(&identity, &tx, 40_000);
		let reply = FeeReply::sign_current(&request, record.clone(), source.clone(), &identity, &key).unwrap();
		assert_eq!(FeeReply::authenticate_current(&reply, &request, &identity).unwrap(), (record.clone(), source));
		assert!(FeeReply::authenticate(&reply, &request, &identity).is_err());
		let verify = FeeRequest::sign(FeeRequestAction::Verify, PublicationPath::Round, &identity, &tx, &key).unwrap();
		assert!(FeeReply::authenticate_current(&reply, &verify, &identity).is_err());
		let old = FeeReply::sign(&request, record, &identity, &key).unwrap();
		assert!(FeeReply::authenticate_current(&old, &request, &identity).is_err());
		let mut changed: FeeReply = serde_json::from_slice(&reply).unwrap();
		changed.current_source = Some(FinalFeeSource::synthetic_for_test(&identity, &tx, 40_001));
		assert!(FeeReply::authenticate_current(&serde_json::to_vec(&changed).unwrap(), &request, &identity).is_err());
	}

}
