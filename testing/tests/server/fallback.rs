use ark::{ProtocolEncoding, SECP};
use ark::fallback::{FallbackRecord, KeyLink};
use ark_testing::TestContext;
use ark_testing::daemon::captaind::ArkClient;
use bitcoin::secp256k1::{rand, Keypair};
use bitcoin::{Network, ScriptBuf};
use server::database::Db;
use server_rpc::protos;

fn destination() -> ScriptBuf {
	ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap()
}

/// Register real mailbox and coin possession signatures through the public C API.
/// No database insertion bypasses production possession or admission checks.
pub async fn registered_fallback_key(rpc: &mut ArkClient) -> (Keypair, KeyLink) {
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let key = Keypair::new(&SECP, &mut rand::thread_rng());
	let record = FallbackRecord::new(Network::Regtest, destination(), &mailbox).unwrap();
	let link = KeyLink::new(Network::Regtest, mailbox.public_key(), &record, &key, None).unwrap();
	let ack = rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: record.serialize(),
		key_links: vec![link.serialize()],
	}).await.expect("immutable record and individual key registration failed").into_inner();
	let canonical = KeyLink::deserialize(&ack.key_links[0]).unwrap();
	canonical.verify(Network::Regtest, mailbox.public_key(), &record).unwrap();
	assert_eq!(canonical.user_pubkey, key.public_key());
	(key, canonical)
}

#[tokio::test]
async fn fallback_records_and_immutable_links() {
	let ctx = TestContext::new("server/fallback_records_and_immutable_links").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let record = FallbackRecord::new(Network::Regtest, destination(), &mailbox).unwrap();
	let request = |record: Vec<u8>| protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record, key_links: vec![],
	};
	assert_eq!(rpc.set_fallback(request(vec![])).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let canonical = record.serialize();
	assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	// Randomized signatures from another device cannot create a conflicting binding.
	let resigned = FallbackRecord::new(Network::Regtest, record.spk.clone(), &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(resigned.serialize())).await.unwrap().into_inner().record, canonical);
	let alternate = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let replacement = FallbackRecord::new(Network::Regtest, alternate, &mailbox).unwrap();
	assert!(rpc.set_fallback(request(replacement.serialize())).await.is_err(), "an immutable destination replacement must be rejected");
	// The mailbox signature binds the complete immutable destination.
	for offset in [3, 24, canonical.len() - 1] {
		let mut tampered = canonical.clone();
		tampered[offset] ^= 1;
		assert_eq!(rpc.set_fallback(request(tampered)).await.unwrap_err().code(), tonic::Code::InvalidArgument);
		assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	}
	let other_mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	assert_eq!(rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other_mailbox.public_key().serialize().to_vec(), record: canonical.clone(), key_links: vec![],
	}).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let counts = db.read(async |tx| Ok((
		tx.query_one("SELECT count(*) FROM fallback_record", &[]).await?.get::<_, i64>(0),
		tx.query_one("SELECT count(*) FROM key_link", &[]).await?.get::<_, i64>(0),
	))).await.unwrap();
	assert_eq!(counts, (1, 0), "registration does not pre-link any coin key");
	// Individual possession replaces account-xpub binding. Even a valid
	// re-signature by the same coin key cannot move its retained owner.
	let coin = Keypair::new(&SECP, &mut rand::thread_rng());
	let link = KeyLink::new(Network::Regtest, mailbox.public_key(), &record, &coin, None).unwrap();
	let acknowledged = rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: record.serialize(),
		key_links: vec![link.serialize()],
	}).await.unwrap().into_inner();
	let competing = FallbackRecord::new(Network::Regtest, record.spk.clone(), &other_mailbox).unwrap();
	let moved = KeyLink::new(Network::Regtest, other_mailbox.public_key(), &competing, &coin, None).unwrap();
	assert!(rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other_mailbox.public_key().serialize().to_vec(), record: competing.serialize(),
		key_links: vec![moved.serialize()],
	}).await.is_err(), "a valid competing mailbox signature cannot reassign a retained coin owner");
	let retry = rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: record.serialize(),
		key_links: vec![link.serialize()],
	}).await.unwrap().into_inner();
	assert_eq!(retry, acknowledged);
	// Two simultaneous authenticated initializations converge on the exact stored bytes.
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let first = FallbackRecord::new(Network::Regtest, record.spk.clone(), &mailbox).unwrap();
	let second = FallbackRecord::new(Network::Regtest, record.spk, &mailbox).unwrap();
	let mut a = rpc.clone();
	let mut b = rpc.clone();
	let (a, b) = tokio::join!(
		a.set_fallback(protos::SetFallbackRequest { mailbox_pk: mailbox.public_key().serialize().to_vec(), record: first.serialize(), key_links: vec![] }),
		b.set_fallback(protos::SetFallbackRequest { mailbox_pk: mailbox.public_key().serialize().to_vec(), record: second.serialize(), key_links: vec![] }),
	);
	let a = a.unwrap().into_inner().record;
	let b = b.unwrap().into_inner().record;
	assert_eq!(a, b);
	assert!(a == first.serialize() || a == second.serialize());
}

#[tokio::test]
async fn fallback_record_replay_from_other_network_refused() {
	let ctx = TestContext::new("server/fallback_record_replay_from_other_network_refused").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let record = FallbackRecord::new(Network::Regtest, destination(), &mailbox).unwrap();
	let request = |record: Vec<u8>| protos::SetFallbackRequest { mailbox_pk: mailbox.public_key().serialize().to_vec(), record, key_links: vec![] };
	let canonical = record.serialize();
	assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	for network in [Network::Signet, Network::Testnet4] {
		let replay = FallbackRecord::new(network, record.spk.clone(), &mailbox).unwrap();
		assert_eq!(rpc.set_fallback(request(replay.serialize())).await.unwrap_err().code(), tonic::Code::InvalidArgument);
		assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	}
	let mut legacy = record.spk.as_bytes().to_vec();
	legacy.extend_from_slice(&1u64.to_le_bytes());
	legacy.extend_from_slice(&record.mailbox_signature.serialize());
	assert_eq!(rpc.set_fallback(request(legacy)).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
}
