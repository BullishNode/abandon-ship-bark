use ark::test_util::VTXO_VECTORS;
use ark::ServerVtxo;
use bitcoin_ext::BlockHeight;

use server::database::{BlockTable, Db};
use server::database::expiry_chain::ExpiryChainListener;
use server::sync::ChainEventListener;

use ark_testing::TestContext;

/// Database barrier tests complement the real Core/listener-order tests in
/// block_index.rs. No payout mapping exists yet, so its derived rebuild is empty.
#[tokio::test]
async fn expiry_generation_rejects_stale_rebuild_and_survives_restart() {
	let mut ctx = TestContext::new_minimal("expiry/generation_restart").await;
	ctx.init_central_postgres().await;
	ctx.init_central_bitcoind().await;
	let cfg = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&cfg).await.unwrap();
	let db = Db::connect(&cfg).await.unwrap();
	let point = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
	db.write(async |t| {
		t.store_block(BlockTable::Captaind, &point).await?;
		t.store_block(BlockTable::Watchmand, &point).await?;
		let vtxo = ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone());
		t.upsert_vtxos([&vtxo]).await?;
		let id = vtxo.id().to_string();
		let entitlement = t.query_one("
			INSERT INTO expiry_entitlement(vtxo_id, gross_sat)
			VALUES($1, $2)
			RETURNING id
		", &[&id, &(vtxo.amount().to_sat() as i64)]).await?.get::<_, i64>("id");
		t.execute("
			INSERT INTO expiry_decided_coin(vtxo_id, entitlement_id)
			VALUES($1, $2)
		", &[&id, &entitlement]).await?;
		Ok(())
	}).await.unwrap();
	let server = ExpiryChainListener::start(db.clone(), BlockTable::Captaind).await.unwrap();
	let watchman = ExpiryChainListener::start(db.clone(), BlockTable::Watchmand).await.unwrap();
	server.on_sync_complete(point).await.unwrap();
	assert!(db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().is_none());
	watchman.on_sync_complete(point).await.unwrap();
	let old = db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().unwrap();
	assert!(db.write(async |t| t.finish_expiry_rebuild(old).await).await.unwrap());
	db.write(async |t| t.lock_valid_expiry_view(old).await).await.unwrap();
	server.on_reorg(point).await.unwrap();
	assert!(!db.write(async |t| t.finish_expiry_rebuild(old).await).await.unwrap());
	assert!(db.write(async |t| t.lock_valid_expiry_view(old).await).await.is_err());
	// Duplicate invalidation can never reopen decided coins or duplicate rights.
	server.on_reorg(point).await.unwrap();
	let n = db.read(async |t| Ok(t.query_one("
		SELECT COUNT(*)
		FROM expiry_decided_coin
	", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(n, 1);
	server.on_sync_complete(point).await.unwrap();
	watchman.on_sync_complete(point).await.unwrap();
	let current = db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().unwrap();
	assert!(current.generation > old.generation);
	assert!(db.write(async |t| t.finish_expiry_rebuild(current).await).await.unwrap());
	let restarted = ExpiryChainListener::start(db.clone(), BlockTable::Captaind).await.unwrap();
	assert!(db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().is_none());
	assert!(!db.write(async |t| t.finish_expiry_rebuild(current).await).await.unwrap());
	restarted.on_sync_complete(point).await.unwrap();
	watchman.on_sync_complete(point).await.unwrap();
	let resumed = db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().unwrap();
	assert!(resumed.generation > current.generation);
	assert!(db.write(async |t| t.finish_expiry_rebuild(resumed).await).await.unwrap());
	assert!(resumed.captaind.height > BlockHeight::ZERO);
}

/// Infrastructure fixture: these allocations have no inputs or payouts. The
/// real PostgreSQL commit ordering, not transaction-ID allocation order, must
/// invalidate paginated history even when a lower sequence value commits late.
#[tokio::test]
async fn expiry_ledger_detects_late_lower_revision_and_ignores_noop_updates() {
	let mut ctx = TestContext::new_minimal("expiry/ledger_commit_order").await;
	ctx.init_central_postgres().await;
	let cfg = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&cfg).await.unwrap();
	let db = Db::connect(&cfg).await.unwrap();
	let ledger = async |db: &Db| db.read(async |t| {
		let row = t.query_one("
			SELECT COUNT(*) AS n, COALESCE(MAX(revision), 0) AS maximum
			FROM expiry_ledger_commit
		", &[]).await?;
		Ok((row.get::<_, i64>("n"), row.get::<_, i64>("maximum")))
	}).await.unwrap();
	let (ready, started) = tokio::sync::oneshot::channel();
	let (release, released) = tokio::sync::oneshot::channel();
	let slow_db = db.clone();
	let slow = tokio::spawn(async move {
		slow_db.write(async |t| {
			let id = t.query_one("
				INSERT INTO expiry_allocation(expiry_height, generation, status)
				VALUES(1, 0, 'pending')
				RETURNING id
			", &[]).await?.get::<_, i64>(0);
			ready.send(()).unwrap();
			released.await.unwrap();
			Ok(id)
		}).await.unwrap()
	});
	started.await.unwrap();
	let second = db.write(async |t| Ok(t.query_one("
		INSERT INTO expiry_allocation(expiry_height, generation, status)
		VALUES(2, 0, 'pending')
		RETURNING id
	", &[]).await?.get::<_, i64>(0))).await.unwrap();
	let before = ledger(&db).await;
	release.send(()).unwrap();
	let first = slow.await.unwrap();
	assert!(first < second);
	let after = ledger(&db).await;
	assert_eq!(after.0, before.0+1, "visible commit count detects the late transaction");
	assert_eq!(after.1, before.1, "MAX alone misses that committed mutation");
	db.write(async |t| {
		t.execute("
			UPDATE expiry_allocation
			SET status=status
			WHERE id=$1
		", &[&second]).await?;
		Ok(())
	}).await.unwrap();
	assert_eq!(ledger(&db).await, after, "a semantic no-op cannot destabilize history tokens");
	db.write(async |t| {
		t.execute("
			UPDATE expiry_allocation
			SET status='conflicted'
			WHERE id=$1
		", &[&second]).await?;
		Ok(())
	}).await.unwrap();
	assert_eq!(ledger(&db).await.0, after.0+1, "an actual same-chain regroup change is visible");
}
