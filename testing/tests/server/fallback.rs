use ark::fallback::FallbackRecord;
use ark::{ProtocolEncoding, SECP};
use ark_testing::TestContext;
use bitcoin::bip32::Xpriv;
use bitcoin::secp256k1::Keypair;
use bitcoin::{Network, ScriptBuf};
use server_rpc::protos;

#[tokio::test]
async fn fallback_registration_is_authenticated_immutable_and_idempotent() {
	let ctx = TestContext::new("server/fallback_registration_is_authenticated_immutable_and_idempotent").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let mailbox = Keypair::from_seckey_slice(&SECP, &[7; 32]).unwrap();
	let account = Xpriv::new_master(Network::Regtest, &[3; 64]).unwrap();
	let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let record = FallbackRecord::new(Network::Regtest, spk.clone(), &account, &mailbox).unwrap();
	let request = |record| protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record,
	};
	assert_eq!(rpc.set_fallback(request(vec![])).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let canonical = rpc.set_fallback(request(record.serialize())).await.unwrap().into_inner().record;
	// The first reply can be lost: a second device signs the same seed binding afresh.
	let second = FallbackRecord::new(Network::Regtest, spk.clone(), &account, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(second.serialize())).await.unwrap().into_inner().record, canonical);
	let alternate = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let changed = FallbackRecord::new(Network::Regtest, alternate, &account, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(changed.serialize())).await.unwrap().into_inner().record, canonical);
	for offset in [3, canonical.len() - 65, canonical.len() - 1] {
		let mut changed = canonical.clone();
		changed[offset] ^= 1;
		assert_eq!(rpc.set_fallback(request(changed)).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	}
	let replay = FallbackRecord::new(Network::Signet, spk.clone(), &account, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(replay.serialize())).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let other = Keypair::from_seckey_slice(&SECP, &[8; 32]).unwrap();
	let duplicate = FallbackRecord::new(Network::Regtest, spk, &account, &other).unwrap();
	assert_eq!(rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other.public_key().serialize().to_vec(), record: duplicate.serialize(),
	}).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	// Changing only BIP32 ancestry metadata keeps every derived public key.
	// It must not bypass account uniqueness and create a second payee binding.
	let mut alias = account;
	alias.depth = 1;
	alias.parent_fingerprint = bitcoin::bip32::Fingerprint::from([9; 4]);
	alias.child_number = bitcoin::bip32::ChildNumber::Normal { index: 9 };
	let original_xpub = bitcoin::bip32::Xpub::from_priv(&SECP, &account);
	let alias_xpub = bitcoin::bip32::Xpub::from_priv(&SECP, &alias);
	assert_ne!(original_xpub, alias_xpub);
	assert_eq!(original_xpub.public_key, alias_xpub.public_key);
	assert_eq!(original_xpub.chain_code, alias_xpub.chain_code);
	let child = bitcoin::bip32::ChildNumber::Normal { index: 8 };
	assert_eq!(original_xpub.ckd_pub(&SECP, child).unwrap().public_key,
		alias_xpub.ckd_pub(&SECP, child).unwrap().public_key);
	let alias_record = FallbackRecord::new(Network::Regtest, record.spk.clone(), &alias, &other).unwrap();
	alias_record.verify(Network::Regtest, other.public_key()).unwrap();
	assert_eq!(rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other.public_key().serialize().to_vec(), record: alias_record.serialize(),
	}).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	assert_eq!(rpc.set_fallback(request(record.serialize())).await.unwrap().into_inner().record, canonical);
}

#[tokio::test]
async fn fallback_migrations_do_not_share_upstream_history() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_migrations_do_not_share_upstream_history").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	server::database::Db::create(&config).await.unwrap();
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	let count = client.query_one("SELECT COUNT(*) FROM refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0);
	assert!(count > 0);
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0), 2);
	// Return the database to exactly the plain-upstream schema/history. The
	// fork build must add its own schema without changing an upstream checksum.
	client.batch_execute("DROP TABLE expiry_decided_coin; DROP TABLE expiry_entitlement; DROP TABLE expiry_cancelled_participation; DROP INDEX expiry_vtxo_anchor_point_ix; DROP TABLE fallback_record; DROP TABLE expiry_refinery_schema_history;").await.unwrap();
	let before = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	let db = server::database::Db::connect(&config).await.unwrap();
	drop(db);
	let after = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	assert_eq!(before, after);
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0), 2);
	server::database::Db::connect(&config).await.unwrap();
}
