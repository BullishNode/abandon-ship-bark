use ark::{musig, ProtocolEncoding, SECP};
use ark::attestations::{FallbackRecordAttestation, KeyLinkAttestation};
use ark_testing::TestContext;
use ark_testing::daemon::captaind::Captaind;
use bitcoin::{absolute, transaction, OutPoint, ScriptBuf, Transaction};
use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{Keypair, Message, PublicKey};
use server::database::Db;
use server_rpc::protos;

fn bound_record(chain: ChainHash, server: PublicKey, key: &Keypair, spk: &ScriptBuf, seq: u64) -> Vec<u8> {
	let mut bytes = spk.as_bytes().to_vec();
	bytes.extend_from_slice(&seq.to_le_bytes());
	bytes.extend_from_slice(&FallbackRecordAttestation::new(chain, server, spk, seq, key).serialize());
	bytes
}

/// A record signed before records were bound to a chain and a server.
fn legacy_record(key: &Keypair, spk: &ScriptBuf, seq: u64) -> Vec<u8> {
	let mut engine = sha256::Hash::engine();
	engine.input(b"Ark expiry fallback record      ");
	engine.input(spk.as_bytes());
	engine.input(&seq.to_le_bytes());
	let msg = Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array());
	let mut bytes = spk.as_bytes().to_vec();
	bytes.extend_from_slice(&seq.to_le_bytes());
	bytes.extend_from_slice(SECP.sign_schnorr_no_aux_rand(&msg, key).as_ref());
	bytes
}

fn link(key: &Keypair, mailbox: &Keypair) -> Vec<u8> {
	let mut bytes = key.public_key().serialize().to_vec();
	bytes.extend_from_slice(&KeyLinkAttestation::new(mailbox.public_key(), key).serialize());
	bytes
}

#[tokio::test]
async fn fallback_records_and_immutable_links() {
	let ctx = TestContext::new("server/fallback_records_and_immutable_links").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let server_pubkey = srv.ark_info().await.server_pubkey;
	let record = |key: &Keypair, spk: &ScriptBuf, seq: u64| {
		bound_record(ChainHash::REGTEST, server_pubkey, key, spk, seq)
	};
	let mailbox = Keypair::from_seckey_slice(&SECP, &[1; 32]).unwrap();
	let coin = Keypair::from_seckey_slice(&SECP, &[2; 32]).unwrap();
	let other_mailbox = Keypair::from_seckey_slice(&SECP, &[3; 32]).unwrap();
	let second_coin = Keypair::from_seckey_slice(&SECP, &[4; 32]).unwrap();
	let mailbox_pk = mailbox.public_key().serialize().to_vec();
	let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let other_spk = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let first = record(&mailbox, &spk, 100);
	let newest = record(&mailbox, &other_spk, 102);

	let request = |record, key_links| protos::SetFallbackRequest {
		mailbox_pk: mailbox_pk.clone(), record, key_links,
	};
	assert!(rpc.set_fallback(request(None, vec![])).await.unwrap().into_inner().record.is_empty());
	assert_eq!(rpc.set_fallback(request(Some(first.clone()), vec![link(&coin, &mailbox)]))
		.await.unwrap().into_inner().record, first);
	assert_eq!(rpc.set_fallback(request(Some(newest.clone()), vec![]))
		.await.unwrap().into_inner().record, newest);
	for seq in [100, 101, 102] {
		assert_eq!(rpc.set_fallback(request(Some(record(&mailbox, &spk, seq)), vec![]))
			.await.unwrap().into_inner().record, newest);
	}

	// Validate the entire request before storing either a record or a key link.
	let mut invalid = record(&mailbox, &spk, 103);
	invalid[2] ^= 1;
	let err = rpc.set_fallback(request(Some(invalid), vec![link(&second_coin, &mailbox)]))
		.await.unwrap_err();
	assert_eq!(err.code(), tonic::Code::InvalidArgument);
	let err = rpc.set_fallback(request(Some(record(&mailbox, &spk, 103)), vec![
		link(&second_coin, &mailbox), link(&coin, &other_mailbox),
	])).await.unwrap_err();
	assert_eq!(err.code(), tonic::Code::InvalidArgument);
	assert_eq!(rpc.set_fallback(request(None, vec![])).await.unwrap().into_inner().record, newest);
	let second_key = second_coin.public_key().serialize().to_vec();
	let count = db.read(async |t| Ok(t.query_one(
		"SELECT COUNT(*) FROM key_link WHERE user_pubkey = $1", &[&second_key],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, 0, "invalid requests must not partially register keys");

	// Even a valid signature for a different mailbox cannot replace the first link.
	rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other_mailbox.public_key().serialize().to_vec(),
		record: Some(record(&other_mailbox, &spk, 200)),
		key_links: vec![link(&coin, &other_mailbox)],
	}).await.unwrap();
	let coin_key = coin.public_key().serialize().to_vec();
	let row = db.read(async |t| Ok(t.query_one(
		"SELECT l.mailbox_pk, r.spk FROM key_link l JOIN fallback_record r USING (mailbox_pk) WHERE l.user_pubkey = $1",
		&[&coin_key],
	).await?)).await.unwrap();
	assert_eq!(row.get::<_, Vec<u8>>("mailbox_pk"), mailbox_pk);
	assert_eq!(row.get::<_, Vec<u8>>("spk"), other_spk.as_bytes());

	// An output key alone is insufficient: its mailbox must also have a record.
	// No funding transaction is broadcast in this ingress test.
	let orphan_mailbox = Keypair::from_seckey_slice(&SECP, &[5; 32]).unwrap();
	let (_, pub_nonce) = musig::nonce_pair(&second_coin);
	let board_request = protos::BoardCosignRequest {
		amount: 100_000,
		utxo: OutPoint::null().serialize(),
		expiry_height: 1,
		user_pubkey: second_key,
		pub_nonce: pub_nonce.serialize().to_vec(),
		funding_tx: bitcoin::consensus::serialize(&Transaction {
			version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
			input: vec![], output: vec![],
		}),
	};
	for with_link in [false, true] {
		if with_link {
			assert!(rpc.set_fallback(protos::SetFallbackRequest {
				mailbox_pk: orphan_mailbox.public_key().serialize().to_vec(), record: None,
				key_links: vec![link(&second_coin, &orphan_mailbox)],
			}).await.unwrap().into_inner().record.is_empty());
		}
		let err = rpc.request_board_cosign(board_request.clone()).await.unwrap_err();
		assert_eq!(err.code(), tonic::Code::InvalidArgument);
		assert!(err.message().contains("missing fallback link or wallet record"), "{err}");
		assert!(err.message().contains(&second_coin.public_key().to_string()), "{err}");
	}

	// Concurrent devices must leave the higher signed sequence in storage.
	let mut device_a = rpc.clone();
	let mut device_b = rpc.clone();
	let final_record = record(&mailbox, &other_spk, 105);
	let (a, b) = tokio::join!(
		device_a.set_fallback(request(Some(record(&mailbox, &spk, 104)), vec![])),
		device_b.set_fallback(request(Some(final_record.clone()), vec![])),
	);
	a.unwrap();
	b.unwrap();
	assert_eq!(rpc.set_fallback(request(None, vec![])).await.unwrap().into_inner().record,
		final_record);
}

/// The payment guards and coin locks are in memory, so a second process on
/// the same database, such as an old binary left running during an upgrade,
/// would bypass them.
#[tokio::test]
async fn second_captaind_on_same_database_refused() {
	let ctx = TestContext::new("server/second_captaind_on_same_database_refused").await;
	let srv = ctx.captaind("server").create().await;
	// The same seed and database; with the mnemonic present the harness
	// starts it without creating a new server.
	let mut cfg = srv.config().clone();
	cfg.data_dir = ctx.datadir.join("server-dup");
	std::fs::create_dir_all(&cfg.data_dir).unwrap();
	let mnemonic = server::wallet::MNEMONIC_FILE;
	std::fs::copy(srv.config().data_dir.join(mnemonic), cfg.data_dir.join(mnemonic)).unwrap();
	let dup = Captaind::new("server-dup", ctx.bitcoind_arc(), cfg.clone());

	dup.try_start().await.expect_err("a second captaind must not start on the same database");
	let stderr = std::fs::read_to_string(cfg.data_dir.join("stderr.log")).unwrap();
	assert!(stderr.contains("another captaind holds database"), "{stderr}");

	srv.stop().await.unwrap();
	dup.try_start().await.expect("the lock is free once the first captaind stopped");
}

/// A seed has the same mailbox key on every network and server, so a record
/// signed for another one must not redirect this wallet's payouts.
#[tokio::test]
async fn fallback_record_replay_from_other_network_or_server_refused() {
	let ctx = TestContext::new("server/fallback_record_replay_from_other_network_or_server_refused").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let server_pubkey = srv.ark_info().await.server_pubkey;
	let mailbox = Keypair::from_seckey_slice(&SECP, &[1; 32]).unwrap();
	let other_server = Keypair::from_seckey_slice(&SECP, &[9; 32]).unwrap().public_key();
	let ours = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let theirs = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let request = |record| protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: Some(record), key_links: vec![],
	};
	let current = || protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: None, key_links: vec![],
	};

	// A record from before the binding names no chain or server.
	let err = rpc.set_fallback(request(legacy_record(&mailbox, &theirs, 99))).await.unwrap_err();
	assert_eq!(err.code(), tonic::Code::InvalidArgument, "{err}");
	assert!(rpc.set_fallback(current()).await.unwrap().into_inner().record.is_empty());

	let stored = bound_record(ChainHash::REGTEST, server_pubkey, &mailbox, &ours, 100);
	assert_eq!(rpc.set_fallback(request(stored.clone())).await.unwrap().into_inner().record, stored);
	for replay in [
		legacy_record(&mailbox, &theirs, 101),
		bound_record(ChainHash::BITCOIN, server_pubkey, &mailbox, &theirs, 101),
		bound_record(ChainHash::REGTEST, other_server, &mailbox, &theirs, 101),
	] {
		let err = rpc.set_fallback(request(replay)).await.unwrap_err();
		assert_eq!(err.code(), tonic::Code::InvalidArgument, "{err}");
		assert_eq!(rpc.set_fallback(current()).await.unwrap().into_inner().record, stored,
			"a refused record leaves the stored one");
	}

	let rotated = bound_record(ChainHash::REGTEST, server_pubkey, &mailbox, &theirs, 101);
	assert_eq!(rpc.set_fallback(request(rotated.clone())).await.unwrap().into_inner().record, rotated);
}
