use ark::fallback::FallbackRecord;
use ark::{ProtocolEncoding, SECP};
use ark_testing::TestContext;
use bitcoin::bip32::Xpriv;
use bitcoin::secp256k1::Keypair;
use bitcoin::{Network, ScriptBuf};
use server_rpc::protos;

async fn wait_for_migration_lock(client: &tokio_postgres::Client, granted: bool, count: i64) {
	tokio::time::timeout(std::time::Duration::from_secs(15), async {
		loop {
			let found = client.query_one("SELECT COUNT(*) FROM pg_locks WHERE locktype='advisory' \
				AND classid=0 AND objid=4 AND objsubid=1 AND granted=$1 \
				AND database=(SELECT oid FROM pg_database WHERE datname=current_database())", &[&granted])
				.await.unwrap().get::<_, i64>(0);
			if found == count { break; }
			tokio::time::sleep(std::time::Duration::from_millis(25)).await;
		}
	}).await.unwrap_or_else(|_| panic!("migration lock boundary timed out: granted={granted}, count={count}"));
}

#[tokio::test]
async fn fallback_concurrent_migrations_preserve_both_histories() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_concurrent_migrations_preserve_both_histories").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	server::database::Db::create(&config).await.unwrap();
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	let before = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	let fork_count = client.query_one("SELECT COUNT(*) FROM expiry_refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0);
	assert_eq!(fork_count, 1);
	// Simulate the real upstream-to-fork deployment, not an already migrated
	// fixture. There are no coins or financial decisions in this database.
	let tables = client.query("SELECT tablename FROM pg_tables WHERE schemaname='public' \
		AND (LEFT(tablename, 7)='expiry_' OR tablename IN ('fallback_record', 'user_exit_claim'))", &[]).await.unwrap();
	for table in tables {
		let name: &str = table.get(0);
		client.batch_execute(&format!("DROP TABLE IF EXISTS \"{}\" CASCADE", name.replace('"', "\"\""))).await.unwrap();
	}
	client.batch_execute("DROP FUNCTION expiry_record_ledger_commit(); DROP INDEX expiry_vtxo_anchor_point_ix; SELECT pg_advisory_lock(4::BIGINT);").await.unwrap();
	let first_config = config.clone();
	let first = tokio::spawn(async move { server::database::Db::connect(&first_config).await });
	let second = tokio::spawn(async move { server::database::Db::connect(&config).await });
	// Both real startup connections must wait before reading either history.
	wait_for_migration_lock(&client, false, 2).await;
	assert!(!first.is_finished() && !second.is_finished());
	client.query_one("SELECT pg_advisory_unlock(4::BIGINT)", &[]).await.unwrap();
	let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
		tokio::join!(first, second)
	}).await.expect("concurrent migrations did not finish");
	drop(first.unwrap().expect("first startup must migrate successfully"));
	drop(second.unwrap().expect("second startup must reuse the completed migrations"));
	let after = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	assert_eq!(before, after, "fork startup must not rewrite upstream checksums");
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0), fork_count);
	wait_for_migration_lock(&client, true, 0).await;
}

#[tokio::test]
async fn fallback_cancelled_migration_releases_its_session_lock() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_cancelled_migration_releases_its_session_lock").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	server::database::Db::create(&config).await.unwrap();
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	// Stop the runner at a real history read after it acquires its session
	// lock. Cancelling that startup must not leave a lock on a pooled client.
	client.batch_execute("BEGIN; LOCK TABLE refinery_schema_history IN ACCESS EXCLUSIVE MODE;").await.unwrap();
	let startup_config = config.clone();
	let startup = tokio::spawn(async move { server::database::Db::connect(&startup_config).await });
	wait_for_migration_lock(&client, true, 1).await;
	startup.abort();
	let Err(cancelled) = startup.await else { panic!("startup completed before cancellation"); };
	assert!(cancelled.is_cancelled());
	wait_for_migration_lock(&client, true, 0).await;
	client.batch_execute("ROLLBACK").await.unwrap();
	drop(tokio::time::timeout(std::time::Duration::from_secs(15), server::database::Db::connect(&config))
		.await.expect("cancelled startup left migration lock held").unwrap());
	wait_for_migration_lock(&client, true, 0).await;
}

#[tokio::test]
async fn fallback_rejects_old_fork_schema_before_upstream_migrations() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_rejects_old_fork_schema_before_upstream_migrations").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	drop(server::database::Db::create(&config).await.unwrap());
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	let original = client.query_one("SELECT version,name,checksum FROM expiry_refinery_schema_history", &[]).await.unwrap();
	assert_eq!(original.get::<_, i32>("version"), 1);
	let name = original.get::<_, String>("name");
	let checksum = original.get::<_, String>("checksum");
	assert_eq!(name, "abandon_ship");
	// A pending upstream migration would try to recreate its existing index.
	// The fork guard must reject first, before either runner can do work.
	client.execute("DELETE FROM refinery_schema_history WHERE version=(SELECT MAX(version) FROM refinery_schema_history)", &[]).await.unwrap();
	let before = client.query("SELECT version,name,checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1), r.get::<_, String>(2))).collect::<Vec<_>>();
	for (old_name, old_checksum) in [
		("fallback_registration", checksum.as_str()),
		(name.as_str(), "not-the-embedded-checksum"),
	] {
		client.execute("UPDATE expiry_refinery_schema_history SET name=$1,checksum=$2 WHERE version=1", &[&old_name, &old_checksum]).await.unwrap();
		let error = match server::database::Db::connect(&config).await {
			Ok(_) => panic!("an incompatible fork schema was accepted"),
			Err(error) => error,
		};
		assert!(error.to_string().contains("unsupported fork schema history"), "unexpected startup failure: {error:#}");
		let after = client.query("SELECT version,name,checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
			.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1), r.get::<_, String>(2))).collect::<Vec<_>>();
		assert_eq!(before, after, "rejected fork startup must not change upstream history");
		let rejected = client.query_one("SELECT version,name,checksum FROM expiry_refinery_schema_history", &[]).await.unwrap();
		assert_eq!(rejected.get::<_, i32>("version"), 1);
		assert_eq!(rejected.get::<_, &str>("name"), old_name);
		assert_eq!(rejected.get::<_, &str>("checksum"), old_checksum);
		wait_for_migration_lock(&client, true, 0).await;
	}
}

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
	let fork_count = client.query_one("SELECT COUNT(*) FROM expiry_refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0);
	assert_eq!(fork_count, 1);
	// Return the database to exactly the plain-upstream schema/history. The
	// fork build must add its own schema without changing an upstream checksum.
	let tables = client.query("SELECT tablename FROM pg_tables WHERE schemaname='public'
		AND (LEFT(tablename, 7)='expiry_' OR tablename IN ('fallback_record', 'user_exit_claim'))", &[]).await.unwrap();
	for table in tables {
		let name: &str = table.get(0);
		client.batch_execute(&format!("DROP TABLE IF EXISTS \"{}\" CASCADE", name.replace('"', "\"\""))).await.unwrap();
	}
	client.batch_execute("DROP FUNCTION expiry_record_ledger_commit(); DROP INDEX expiry_vtxo_anchor_point_ix;").await.unwrap();
	let before = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	let db = server::database::Db::connect(&config).await.unwrap();
	drop(db);
	let after = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	assert_eq!(before, after);
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0), fork_count);
	server::database::Db::connect(&config).await.unwrap();
}
