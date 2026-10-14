use ark::{ProtocolEncoding, SECP};
use ark::fallback::{AccountChildHint, FallbackRecord};
use ark::mailbox::MailboxIdentifier;
use ark_testing::TestContext;
use ark_testing::daemon::captaind::ArkClient;
use bitcoin::bip32::{ChildNumber, Xpriv};
use bitcoin::secp256k1::{rand, Keypair, PublicKey};
use bitcoin::{Network, ScriptBuf};
use server::database::Db;
use server_rpc::protos;

fn account() -> Xpriv {
	Xpriv::new_master(Network::Regtest, &rand::random::<[u8; 32]>()).unwrap()
}

fn destination() -> ScriptBuf {
	ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap()
}

/// A genuine account child and on-demand blinded origin for raw RPC fixtures.
/// No database insertion bypasses the production possession or admission checks.
pub async fn registered_fallback_key(rpc: &mut ArkClient) -> (Keypair, protos::AccountChildHint) {
	let account = account();
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let record = FallbackRecord::new(Network::Regtest, destination(), &account, &mailbox).unwrap();
	rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: record.serialize(),
	}).await.expect("immutable account registration failed");
	let key = account.derive_priv(&SECP, &[ChildNumber::Normal { index: 0 }]).unwrap().to_keypair(&SECP);
	let info = rpc.get_ark_info(protos::Empty {}).await.unwrap().into_inner();
	let server_mailbox = PublicKey::from_slice(&info.mailbox_pubkey).unwrap();
	let blinded = MailboxIdentifier::from_pubkey(mailbox.public_key()).to_blinded(server_mailbox, &key).unwrap();
	(key, AccountChildHint::new(blinded, 0).unwrap().into())
}

#[tokio::test]
async fn fallback_records_and_immutable_links() {
	let ctx = TestContext::new("server/fallback_records_and_immutable_links").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let account = account();
	let record = FallbackRecord::new(Network::Regtest, destination(), &account, &mailbox).unwrap();
	let request = |record: Vec<u8>| protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record,
	};
	assert_eq!(rpc.set_fallback(request(vec![])).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let canonical = record.serialize();
	assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	// Randomized signatures from another device cannot create a conflicting binding.
	let resigned = FallbackRecord::new(Network::Regtest, record.spk.clone(), &account, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(resigned.serialize())).await.unwrap().into_inner().record, canonical);
	let alternate = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let replacement = FallbackRecord::new(Network::Regtest, alternate, &account, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(replacement.serialize())).await.unwrap().into_inner().record, canonical);
	let other_account = Xpriv::new_master(Network::Regtest, &[19; 64]).unwrap();
	let replacement = FallbackRecord::new(Network::Regtest, record.spk.clone(), &other_account, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(replacement.serialize())).await.unwrap().into_inner().record, canonical);
	// Both full-field proofs are mandatory; changed script, xpub or either signature is rejected.
	for offset in [3, 24, canonical.len() - 65, canonical.len() - 1] {
		let mut tampered = canonical.clone();
		tampered[offset] ^= 1;
		assert_eq!(rpc.set_fallback(request(tampered)).await.unwrap_err().code(), tonic::Code::InvalidArgument);
		assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	}
	let other_mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	assert_eq!(rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other_mailbox.public_key().serialize().to_vec(), record: canonical.clone(),
	}).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let counts = db.read(async |tx| Ok((
		tx.query_one("SELECT count(*) FROM fallback_record", &[]).await?.get::<_, i64>(0),
		tx.query_one("SELECT count(*) FROM key_link", &[]).await?.get::<_, i64>(0),
	))).await.unwrap();
	assert_eq!(counts, (1, 0), "registration does not pre-link any coin key");
	// Two simultaneous authenticated initializations converge on the exact stored bytes.
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let first = FallbackRecord::new(Network::Regtest, record.spk.clone(), &account, &mailbox).unwrap();
	let second = FallbackRecord::new(Network::Regtest, record.spk, &account, &mailbox).unwrap();
	let mut a = rpc.clone();
	let mut b = rpc.clone();
	let (a, b) = tokio::join!(
		a.set_fallback(protos::SetFallbackRequest { mailbox_pk: mailbox.public_key().serialize().to_vec(), record: first.serialize() }),
		b.set_fallback(protos::SetFallbackRequest { mailbox_pk: mailbox.public_key().serialize().to_vec(), record: second.serialize() }),
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
	let account = account();
	let record = FallbackRecord::new(Network::Regtest, destination(), &account, &mailbox).unwrap();
	let request = |record: Vec<u8>| protos::SetFallbackRequest { mailbox_pk: mailbox.public_key().serialize().to_vec(), record };
	let canonical = record.serialize();
	assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	for network in [Network::Signet, Network::Testnet4] {
		let replay = FallbackRecord::new(network, record.spk.clone(), &account, &mailbox).unwrap();
		assert_eq!(rpc.set_fallback(request(replay.serialize())).await.unwrap_err().code(), tonic::Code::InvalidArgument);
		assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
	}
	let mut legacy = record.spk.as_bytes().to_vec();
	legacy.extend_from_slice(&1u64.to_le_bytes());
	legacy.extend_from_slice(&record.mailbox_signature.serialize());
	assert_eq!(rpc.set_fallback(request(legacy)).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	assert_eq!(rpc.set_fallback(request(canonical.clone())).await.unwrap().into_inner().record, canonical);
}
