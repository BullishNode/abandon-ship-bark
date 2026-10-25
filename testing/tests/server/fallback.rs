use ark::fallback::FallbackRegistration;
use ark::mailbox::MailboxIdentifier;
use ark::{ProtocolEncoding, SECP};
use ark_testing::TestContext;
use bitcoin::secp256k1::Keypair;
use bitcoin::{Network, ScriptBuf};
use server_rpc::protos;
use std::time::Duration;

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
	let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let record = FallbackRegistration::new(Network::Regtest, spk.clone(), &mailbox).unwrap();
	let request = |record| protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record,
	};
	assert_eq!(rpc.set_fallback(request(vec![])).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let canonical = rpc.set_fallback(request(record.serialize())).await.unwrap().into_inner().record;
	// The first reply can be lost: a second device signs the same seed binding afresh.
	let second = FallbackRegistration::new(Network::Regtest, spk.clone(), &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(second.serialize())).await.unwrap().into_inner().record, canonical);
	let alternate = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let changed = FallbackRegistration::new(Network::Regtest, alternate, &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(changed.serialize())).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	for offset in [3, canonical.len() - 65, canonical.len() - 1] {
		let mut changed = canonical.clone();
		changed[offset] ^= 1;
		assert_eq!(rpc.set_fallback(request(changed)).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	}
	let replay = FallbackRegistration::new(Network::Signet, spk.clone(), &mailbox).unwrap();
	assert_eq!(rpc.set_fallback(request(replay.serialize())).await.unwrap_err().code(), tonic::Code::InvalidArgument);
	let other = Keypair::from_seckey_slice(&SECP, &[8; 32]).unwrap();
	let duplicate = FallbackRegistration::new(Network::Regtest, spk, &other).unwrap();
	assert_eq!(rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: other.public_key().serialize().to_vec(), record: duplicate.serialize(),
	}).await.unwrap().into_inner().record, duplicate.serialize());
	assert_eq!(rpc.set_fallback(request(record.serialize())).await.unwrap().into_inner().record, canonical);
}

#[tokio::test]
async fn fallback_registration_conflicts_do_not_block_other_mailboxes() {
	let ctx = TestContext::new("server/fallback_registration_conflicts_do_not_block_other_mailboxes").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let postgres = srv.config().postgres.clone();
	let db = server::database::Db::connect(&postgres).await.unwrap();
	let key = |byte| Keypair::from_seckey_slice(&SECP, &[byte; 32]).unwrap();
	let first_script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let other_script = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	let known = key(5);
	let known_record = FallbackRegistration::new(Network::Regtest, first_script.clone(), &known).unwrap();
	rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: known.public_key().serialize().to_vec(), record: known_record.serialize(),
	}).await.unwrap();

	for (byte, conflict) in [(7, false), (9, true)] {
		let mailbox = key(byte);
		let first_record = FallbackRegistration::new(Network::Regtest, first_script.clone(), &mailbox).unwrap();
		let contender_record = FallbackRegistration::new(Network::Regtest,
			if conflict { other_script.clone() } else { first_script.clone() }, &mailbox).unwrap();
		assert_ne!(first_record.mailbox_signature, contender_record.mailbox_signature);
		let mut contender_rpc = rpc.clone();
		let request = protos::SetFallbackRequest {
			mailbox_pk: mailbox.public_key().serialize().to_vec(), record: contender_record.serialize(),
		};
		let (canonical, contender) = db.write(async |tx| {
			let canonical = tx.set_fallback(mailbox.public_key(), &first_record, Network::Regtest).await?;
			let blocker = tx.query_one("SELECT pg_backend_pid()", &[]).await?.get::<_, i32>(0);
			let contender = tokio::spawn(async move { contender_rpc.set_fallback(request).await });
			// Observe the real unique-key wait before allowing the winner to commit.
			tokio::time::timeout(Duration::from_secs(10), async {
				loop {
					tx.query_one("SELECT pg_stat_clear_snapshot()", &[]).await?;
					let blocked = tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity a \
						JOIN pg_locks l ON l.pid=a.pid WHERE a.datname=current_database() \
						AND a.query LIKE '%INSERT INTO fallback_record%' AND l.locktype='transactionid' \
						AND NOT l.granted AND $1=ANY(pg_blocking_pids(a.pid)))", &[&blocker])
						.await?.get::<_, bool>(0);
					if blocked { break; }
					tokio::time::sleep(Duration::from_millis(10)).await;
				}
				Ok::<_, anyhow::Error>(())
			}).await.expect("registration never reached the unique-key conflict")?;

			// Neither an unrelated registration nor a registered mailbox read
			// must wait for the first mailbox's transaction to end.
			let unrelated = key(byte + 1);
			let unrelated_record = FallbackRegistration::new(Network::Regtest, other_script.clone(), &unrelated).unwrap();
			let result = tokio::time::timeout(Duration::from_secs(10), rpc.set_fallback(protos::SetFallbackRequest {
				mailbox_pk: unrelated.public_key().serialize().to_vec(), record: unrelated_record.serialize(),
			})).await.expect("unrelated registration waited for another mailbox").unwrap().into_inner();
			assert_eq!(result.record, unrelated_record.serialize());
			tokio::time::timeout(Duration::from_secs(10), db.read(async |other| {
				other.require_registered_mailbox(MailboxIdentifier::from_pubkey(known.public_key()), Network::Regtest).await
			})).await.expect("registered mailbox read waited for another mailbox").unwrap();
			assert!(!contender.is_finished(), "the same-key request must still await the winner");
			Ok((canonical, contender))
		}).await.unwrap();
		assert_eq!(canonical, first_record);
		let result = tokio::time::timeout(Duration::from_secs(10), contender).await
			.expect("registration did not resume after the winning commit").unwrap();
		if conflict {
			let error = result.unwrap_err();
			assert_eq!(error.code(), tonic::Code::InvalidArgument);
			assert!(error.message().contains("immutable fallback destination differs"), "{error}");
		} else {
			assert_eq!(result.unwrap().into_inner().record, canonical.serialize());
		}
		let stored = db.read(async |tx| tx.get_fallback(mailbox.public_key()).await).await.unwrap().unwrap();
		assert_eq!(stored, canonical);
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM fallback_record WHERE mailbox_pk=$1",
			&[&mailbox.public_key().to_string()]).await?.get::<_, i64>(0))).await.unwrap(), 1);
	}
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

#[tokio::test]
async fn fallback_links_are_authenticated_conflict_safe_and_transactional() {
	let mut ctx = TestContext::new_minimal("postgresd/fallback_links_are_authenticated_conflict_safe_and_transactional").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	let db = server::database::Db::create(&config).await.unwrap();
	let key = |byte| Keypair::from_seckey_slice(&SECP, &[byte; 32]).unwrap();
	let server = key(11);
	let mailbox = key(17);
	let other_mailbox = key(19);
	let coin = key(3);
	let script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let registration = FallbackRegistration::new(Network::Regtest, script.clone(), &mailbox).unwrap();
	let other = FallbackRegistration::new(Network::Regtest, script.clone(), &other_mailbox).unwrap();
	for (mailbox, record) in [(mailbox.public_key(), &registration), (other_mailbox.public_key(), &other)] {
		db.write(async |tx| tx.set_fallback(mailbox, record, Network::Regtest).await).await.unwrap();
	}
	let link = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(mailbox.public_key()), &coin).unwrap();
	let resigned = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(mailbox.public_key()), &coin).unwrap();
	for proof in [&link, &resigned] {
		assert_eq!(db.write(async |tx| tx.verify_owner_link(&server, coin.public_key(), proof, Network::Regtest).await)
			.await.unwrap(), mailbox.public_key());
	}
	let stored = db.read(async |tx| Ok(tx.query_one("SELECT blinded_id,signature FROM expiry_owner_key WHERE user_pubkey=$1",
		&[&coin.public_key().to_string()]).await?)).await.unwrap();
	assert_eq!(stored.get::<_, String>("blinded_id"), link.blinded_id.to_string());
	assert_eq!(stored.get::<_, String>("signature"), link.signature.to_string());
	let conflict = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(other_mailbox.public_key()), &coin).unwrap();
	assert!(db.write(async |tx| tx.verify_owner_link(&server, coin.public_key(), &conflict, Network::Regtest).await).await.is_err());
	assert!(db.write(async |tx| tx.verify_owner_link(&key(13), coin.public_key(), &link, Network::Regtest).await).await.is_err());
	assert!(db.write(async |tx| tx.verify_owner_link(&server, key(5).public_key(), &link, Network::Regtest).await).await.is_err());
	let unregistered = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(key(21).public_key()), &key(7)).unwrap();
	assert!(db.write(async |tx| tx.verify_owner_link(&server, key(7).public_key(), &unregistered, Network::Regtest).await).await.is_err());

	let pending = key(9);
	let pending_link = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(mailbox.public_key()), &pending).unwrap();
	let result = db.write(async |tx| {
		tx.verify_owner_link(&server, pending.public_key(), &pending_link, Network::Regtest).await?;
		tx.verify_owner_link(&server, coin.public_key(), &conflict, Network::Regtest).await?;
		Ok(())
	}).await;
	assert!(result.is_err());
	assert_eq!(db.read(async |tx| tx.expiry_owner(pending.public_key(), Network::Regtest, &server).await).await.unwrap(),
		server::database::expiry_attribution::Attribution::Unresolved);
	let raced = key(23);
	let first = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(mailbox.public_key()), &raced).unwrap();
	let second = ark::fallback::OwnerLink::new(server.public_key(),
		ark::mailbox::MailboxIdentifier::from_pubkey(other_mailbox.public_key()), &raced).unwrap();
	let (first_result, second_result) = tokio::join!(
		db.write(async |tx| tx.verify_owner_link(&server, raced.public_key(), &first, Network::Regtest).await),
		db.write(async |tx| tx.verify_owner_link(&server, raced.public_key(), &second, Network::Regtest).await),
	);
	assert!(first_result.is_ok() ^ second_result.is_ok());
	let winner = if first_result.is_ok() { mailbox.public_key() } else { other_mailbox.public_key() };
	assert_eq!(db.read(async |tx| tx.expiry_owner(raced.public_key(), Network::Regtest, &server).await).await.unwrap(),
		server::database::expiry_attribution::Attribution::Owned { mailbox_pk: winner, spk: script.clone() });
	let expected = server::database::expiry_attribution::Attribution::Owned { mailbox_pk: mailbox.public_key(), spk: script };
	assert_eq!(db.read(async |tx| tx.expiry_owner(coin.public_key(), Network::Regtest, &server).await).await.unwrap(), expected);
	db.write(async |tx| Ok(tx.execute("UPDATE expiry_owner_key SET signature=$1 WHERE user_pubkey=$2",
		&[&"00".repeat(64), &coin.public_key().to_string()]).await?)).await.unwrap();
	assert_eq!(db.read(async |tx| tx.expiry_owner(coin.public_key(), Network::Regtest, &server).await).await.unwrap(),
		server::database::expiry_attribution::Attribution::Unresolved);
}
