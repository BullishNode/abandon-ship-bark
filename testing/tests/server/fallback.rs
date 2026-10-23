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

async fn remove_fork_schema(client: &tokio_postgres::Client) {
	let tables = client.query("SELECT tablename FROM pg_tables WHERE schemaname='public' \
		AND (LEFT(tablename, 7)='expiry_' OR tablename='fallback_record')", &[]).await.unwrap();
	for table in tables {
		let name: &str = table.get(0);
		client.batch_execute(&format!("DROP TABLE IF EXISTS \"{}\" CASCADE", name.replace('"', "\"\""))).await.unwrap();
	}
	client.batch_execute("DROP INDEX expiry_vtxo_anchor_point_ix;").await.unwrap();
}

#[tokio::test]
async fn fallback_concurrent_initialization_preserves_upstream_history() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_concurrent_initialization_preserves_upstream_history").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	server::database::Db::create(&config).await.unwrap();
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	let before = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0), 1);
	// Simulate the real upstream-to-fork deployment, not an already migrated
	// fixture. There are no coins or financial decisions in this database.
	remove_fork_schema(&client).await;
	client.batch_execute("SELECT pg_advisory_lock(4::BIGINT);").await.unwrap();
	let first_config = config.clone();
	let first = tokio::spawn(async move { server::database::Db::connect(&first_config).await });
	let second = tokio::spawn(async move { server::database::Db::connect(&config).await });
	// Both real startup connections must wait before schema initialization.
	wait_for_migration_lock(&client, false, 2).await;
	assert!(!first.is_finished() && !second.is_finished());
	client.query_one("SELECT pg_advisory_unlock(4::BIGINT)", &[]).await.unwrap();
	let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
		tokio::join!(first, second)
	}).await.expect("concurrent initialization did not finish");
	drop(first.unwrap().expect("first startup must initialize successfully"));
	drop(second.unwrap().expect("second startup must reuse the initialized schema"));
	let after = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	assert_eq!(before, after, "fork startup must not rewrite upstream checksums");
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0), 1);
	assert!(client.query_one("SELECT to_regclass('expiry_refinery_schema_history') IS NULL", &[]).await.unwrap().get::<_, bool>(0));
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
async fn fallback_failed_initialization_rolls_back_schema() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_failed_initialization_rolls_back_schema").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	drop(server::database::Db::create(&config).await.unwrap());
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	remove_fork_schema(&client).await;
	// An empty conflicting relation fails near the end of the schema batch.
	// No ownership, entitlement or payment rows are manufactured.
	client.batch_execute("CREATE TABLE expiry_lightning_receive_claim (id INTEGER);").await.unwrap();
	let Err(error) = server::database::Db::connect(&config).await else {
		panic!("conflicting schema object must refuse initialization");
	};
	let pg = error.downcast_ref::<tokio_postgres::Error>().expect("expected schema statement failure");
	let db_error = pg.as_db_error().expect("expected PostgreSQL schema error");
	assert_eq!(db_error.code(), &tokio_postgres::error::SqlState::DUPLICATE_TABLE);
	assert!(db_error.message().contains("expiry_lightning_receive_claim"));
	assert!(client.query_one("SELECT to_regclass('expiry_chain_view') IS NULL \
		AND to_regclass('fallback_record') IS NULL", &[]).await.unwrap().get::<_, bool>(0),
		"failed initialization must roll back its earlier tables and marker");
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_lightning_receive_claim", &[]).await.unwrap().get::<_, i64>(0), 0);
	wait_for_migration_lock(&client, true, 0).await;
	client.batch_execute("DROP TABLE expiry_lightning_receive_claim;").await.unwrap();
	drop(server::database::Db::connect(&config).await.unwrap());
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0), 1);
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
async fn fallback_schema_initialization_preserves_upstream_history() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_schema_initialization_preserves_upstream_history").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	server::database::Db::create(&config).await.unwrap();
	let client = ctx.postgres_manager().database_client(Some(&config.name)).await;
	let count = client.query_one("SELECT COUNT(*) FROM refinery_schema_history", &[]).await.unwrap().get::<_, i64>(0);
	assert!(count > 0);
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0), 1);
	// Return the database to exactly the plain-upstream schema/history. The
	// fork build must add its own schema without changing an upstream checksum.
	remove_fork_schema(&client).await;
	let before = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	let db = server::database::Db::connect(&config).await.unwrap();
	drop(db);
	let after = client.query("SELECT version, checksum FROM refinery_schema_history ORDER BY version", &[]).await.unwrap()
		.into_iter().map(|r| (r.get::<_, i32>(0), r.get::<_, String>(1))).collect::<Vec<_>>();
	assert_eq!(before, after);
	assert_eq!(client.query_one("SELECT COUNT(*) FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0), 1);
	assert!(client.query_one("SELECT to_regclass('expiry_refinery_schema_history') IS NULL", &[]).await.unwrap().get::<_, bool>(0));
	client.execute("UPDATE expiry_chain_view SET generation=generation+1 WHERE id=1", &[]).await.unwrap();
	let generation = client.query_one("SELECT generation FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0);
	server::database::Db::connect(&config).await.unwrap();
	assert_eq!(client.query_one("SELECT generation FROM expiry_chain_view WHERE id=1", &[]).await.unwrap().get::<_, i64>(0), generation);
}
