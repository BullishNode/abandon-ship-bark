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

// Declared unfunded registration namespace: fresh random mailbox/policy keys only.
// No wallet, Board, funding, chain-height override, or attribution xpub is used.
fn explicit_per_key_registration(
	mailbox: bitcoin::secp256k1::PublicKey, record: &FallbackRecord, links: &[KeyLink],
) -> tonic::Request<protos::SetFallbackRequest> {
	use server_rpc::RequestExt;
	let mut request = tonic::Request::new(protos::SetFallbackRequest {
		mailbox_pk: mailbox.serialize().to_vec(), record: record.serialize(),
		key_links: links.iter().map(ProtocolEncoding::serialize).collect(),
	});
	request.set_pver(server_rpc::pver::PROTOCOL_VERSION_PER_KEY_FALLBACK);
	assert_eq!(request.pver().unwrap(), 6);
	request
}

// One actual SELECT gives all record/link bytes and zero-capital counters in
// one PostgreSQL snapshot; it is not a wallet-generated admission claim.
async fn unfunded_registration_cut(db: &Db) -> serde_json::Value {
	let raw: String = db.read(async |tx| Ok(tx.query_one("SELECT jsonb_build_object(
		'records',(SELECT coalesce(jsonb_agg(r ORDER BY r.mailbox_pk),'[]'::jsonb) FROM (
			SELECT encode(mailbox_pk,'hex') AS mailbox_pk,encode(spk,'hex') AS spk,
			encode(mailbox_sig,'hex') AS mailbox_sig FROM fallback_record) r),
		'links',(SELECT coalesce(jsonb_agg(l ORDER BY l.user_pubkey),'[]'::jsonb) FROM (
			SELECT encode(user_pubkey,'hex') AS user_pubkey,encode(mailbox_pk,'hex') AS mailbox_pk,
			encode(sig,'hex') AS sig,recovery_index FROM key_link) l),
		'coins',(SELECT count(*) FROM vtxo),'pending_boards',(SELECT count(*) FROM pending_board)
	)::text", &[]).await?.get(0))).await.unwrap();
	let cut: serde_json::Value = serde_json::from_str(&raw).unwrap();
	assert_eq!(cut["coins"], 0, "unfunded probe namespace must have no VTXOs");
	assert_eq!(cut["pending_boards"], 0, "unfunded probe namespace must have no pending Boards");
	cut
}

fn expected_registration_delta(
	mut before: serde_json::Value, mailbox: bitcoin::secp256k1::PublicKey,
	record: &FallbackRecord, links: &[KeyLink],
) -> serde_json::Value {
	let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
	before["records"].as_array_mut().unwrap().push(serde_json::json!({
		"mailbox_pk":mailbox.to_string(),"spk":record.spk.to_hex_string(),
		"mailbox_sig":hex(&record.mailbox_signature.serialize())}));
	for link in links {
		before["links"].as_array_mut().unwrap().push(serde_json::json!({
			"user_pubkey":link.user_pubkey.to_string(),"mailbox_pk":mailbox.to_string(),
			"sig":hex(&link.signature.serialize()),"recovery_index":link.recovery_index}));
	}
	before["records"].as_array_mut().unwrap().sort_by_key(|row| row["mailbox_pk"].as_str().unwrap().to_owned());
	before["links"].as_array_mut().unwrap().sort_by_key(|row| row["user_pubkey"].as_str().unwrap().to_owned());
	before
}

#[tokio::test]
async fn per_key_differing_initializations_concurrently_retain_one_exact_binding() {
	let ctx = TestContext::new("server/per_key_differing_initializations_concurrently_retain_one_exact_binding").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let mut rpc = srv.get_public_rpc().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(unfunded_registration_cut(&db).await, serde_json::json!({"records":[],"links":[],"coins":0,"pending_boards":0}));
	let (_, protected_link) = registered_fallback_key(&mut rpc).await;
	let before = unfunded_registration_cut(&db).await;
	assert_eq!(before["records"].as_array().unwrap().len(), 1);
	assert_eq!(before["links"].as_array().unwrap().len(), 1);

	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let first = FallbackRecord::new(Network::Regtest, destination(), &mailbox).unwrap();
	let second = FallbackRecord::new(Network::Regtest,
		ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap(), &mailbox).unwrap();
	assert!(!first.same_binding(&second), "the requests must conflict in destination, not just signature randomness");
	let first_key = Keypair::new(&SECP, &mut rand::thread_rng());
	let second_key = Keypair::new(&SECP, &mut rand::thread_rng());
	assert_ne!(first_key.public_key(), second_key.public_key());
	assert_ne!(first_key.public_key(), protected_link.user_pubkey);
	assert_ne!(second_key.public_key(), protected_link.user_pubkey);
	let first_link = KeyLink::new(Network::Regtest, mailbox.public_key(), &first, &first_key, None).unwrap();
	let second_link = KeyLink::new(Network::Regtest, mailbox.public_key(), &second, &second_key, None).unwrap();
	first_link.verify(Network::Regtest, mailbox.public_key(), &first).unwrap();
	second_link.verify(Network::Regtest, mailbox.public_key(), &second).unwrap();
	assert!(before["records"].as_array().unwrap().iter().all(|row| row["mailbox_pk"] != mailbox.public_key().to_string()));
	let mut first_rpc = rpc.clone();
	let mut second_rpc = rpc.clone();
	// Both valid immutable requests exist before either call crosses this barrier.
	let first_request = explicit_per_key_registration(mailbox.public_key(), &first, &[first_link.clone()]);
	let second_request = explicit_per_key_registration(mailbox.public_key(), &second, &[second_link.clone()]);
	let barrier = tokio::sync::Barrier::new(2);
	let (a, b) = tokio::join!(
		async { barrier.wait().await; first_rpc.set_fallback(first_request).await },
		async { barrier.wait().await; second_rpc.set_fallback(second_request).await },
	);
	let (record, link, ack) = match (a, b) {
		(Ok(reply), Err(_)) => (&first, &first_link, reply.into_inner()),
		(Err(_), Ok(reply)) => (&second, &second_link, reply.into_inner()),
		outcome => panic!("exactly one differing valid initialization must succeed: {outcome:?}"),
	};
	assert_eq!(ack.record, record.serialize());
	assert_eq!(ack.key_links, vec![link.serialize()]);
	let after = unfunded_registration_cut(&db).await;
	assert_eq!(after, expected_registration_delta(before, mailbox.public_key(), record, &[link.clone()]),
		"only the winning exact record/link may commit; unrelated signature bytes and losing absence are protected");
	let replay = rpc.set_fallback(explicit_per_key_registration(mailbox.public_key(), record, &[link.clone()]))
		.await.unwrap().into_inner();
	assert_eq!(replay, ack);
	assert_eq!(unfunded_registration_cut(&db).await, after);
}

#[tokio::test]
async fn per_key_mixed_new_links_roll_back_record_and_protect_unrelated_canonical_proofs() {
	let ctx = TestContext::new("server/per_key_mixed_new_links_roll_back_record_and_protect_unrelated_canonical_proofs").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let mut rpc = srv.get_public_rpc().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(unfunded_registration_cut(&db).await, serde_json::json!({"records":[],"links":[],"coins":0,"pending_boards":0}));
	let (_, protected_link) = registered_fallback_key(&mut rpc).await;
	let before = unfunded_registration_cut(&db).await;
	assert_eq!(before["records"].as_array().unwrap().len(), 1);
	assert_eq!(before["links"].as_array().unwrap().len(), 1);

	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let record = FallbackRecord::new(Network::Regtest, destination(), &mailbox).unwrap();
	let first_key = Keypair::new(&SECP, &mut rand::thread_rng());
	let second_key = Keypair::new(&SECP, &mut rand::thread_rng());
	assert_ne!(first_key.public_key(), second_key.public_key());
	assert_ne!(first_key.public_key(), protected_link.user_pubkey);
	assert_ne!(second_key.public_key(), protected_link.user_pubkey);
	assert!(before["records"].as_array().unwrap().iter().all(|row| row["mailbox_pk"] != mailbox.public_key().to_string()));
	let first_link = KeyLink::new(Network::Regtest, mailbox.public_key(), &record, &first_key, None).unwrap();
	let second_link = KeyLink::new(Network::Regtest, mailbox.public_key(), &record, &second_key, None).unwrap();
	first_link.verify(Network::Regtest, mailbox.public_key(), &record).unwrap();
	second_link.verify(Network::Regtest, mailbox.public_key(), &record).unwrap();
	let mut bad = second_link.serialize();
	*bad.last_mut().unwrap() ^= 1;
	let bad = KeyLink::deserialize(&bad).expect("negative must retain canonical V1 parsing");
	assert_eq!(bad.user_pubkey, second_link.user_pubkey);
	assert_eq!(bad.recovery_index, second_link.recovery_index);
	assert!(bad.verify(Network::Regtest, mailbox.public_key(), &record).is_err());
	rpc.set_fallback(explicit_per_key_registration(mailbox.public_key(), &record, &[first_link.clone(), bad]))
		.await.expect_err("one invalid signature must reject the whole two-NEW-key batch");
	assert_eq!(unfunded_registration_cut(&db).await, before,
		"failed batch inserts neither NEW key nor its record and cannot change unrelated canonical signature bytes");
	// Positive control: the exact same keys/record with the genuine second
	// signature must commit, demonstrating actual valid registration capability.
	let ack = rpc.set_fallback(explicit_per_key_registration(mailbox.public_key(), &record, &[first_link.clone(), second_link.clone()]))
		.await.unwrap().into_inner();
	assert_eq!(ack.record, record.serialize());
	assert_eq!(ack.key_links, vec![first_link.serialize(), second_link.serialize()]);
	assert_eq!(unfunded_registration_cut(&db).await,
		expected_registration_delta(before, mailbox.public_key(), &record, &[first_link, second_link]));
}
