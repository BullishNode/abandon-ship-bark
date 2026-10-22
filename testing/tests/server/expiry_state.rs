use ark::test_util::VTXO_VECTORS;
use ark::ServerVtxo;
use bitcoin::consensus::serialize;
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
		t.query_one("
			INSERT INTO expiry_entitlement(vtxo_id, gross_sat)
			VALUES($1, $2)
			RETURNING id
		", &[&id, &(vtxo.amount().to_sat() as i64)]).await?;
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
		FROM expiry_entitlement
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

/// Upgrade schema fixture: old padding plans differ only in whether a signed
/// parent exists. These signed vector bytes test storage and fencing, not Core
/// acceptance or the economics of a real fallback transaction.
#[tokio::test]
async fn expiry_unsigned_legacy_padding_is_held_without_rewriting_signed_history() {
	let mut ctx = TestContext::new_minimal("expiry/legacy_padding_signing_barrier").await;
	ctx.init_central_postgres().await;
	ctx.init_central_bitcoind().await;
	let cfg = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&cfg).await.unwrap();
	let db = Db::connect(&cfg).await.unwrap();
	let point = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
	let coins = [
		ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone()),
		ServerVtxo::from(VTXO_VECTORS.round1_vtxo.clone()),
	];
	let parent = VTXO_VECTORS.round1_vtxo.transactions().last().unwrap().tx;
	let child = VTXO_VECTORS.board_vtxo.transactions().last().unwrap().tx;
	assert!(parent.input.iter().any(|input| !input.witness.is_empty()));
	assert!(child.input.iter().any(|input| !input.witness.is_empty()));
	let raw_parent = serialize(&parent);
	let raw_child = serialize(&child);
	let allocations = db.write(async |t| {
		t.store_block(BlockTable::Captaind, &point).await?;
		t.store_block(BlockTable::Watchmand, &point).await?;
		t.upsert_vtxos(&coins).await?;
		let mut allocations = Vec::new();
		for coin in &coins {
			let gross = i64::try_from(coin.amount().to_sat())?;
			let entitlement = t.query_one("
				INSERT INTO expiry_entitlement(vtxo_id,gross_sat,irreversible_facts)
				VALUES($1,$2,'{\"kind\":\"padding\",\"operator\":true}') RETURNING id
			", &[&coin.id().to_string(),&gross]).await?.get::<_, i64>(0);
			let allocation = t.query_one("
				INSERT INTO expiry_allocation(expiry_height,generation,status)
				VALUES(1,0,'pending') RETURNING id
			", &[]).await?.get::<_, i64>(0);
			t.execute("
				INSERT INTO expiry_allocation_input(allocation_id,vtxo_id) VALUES($1,$2)
			", &[&allocation,&coin.id().to_string()]).await?;
			t.execute("
				INSERT INTO expiry_allocation_coin(allocation_id,entitlement_id,disposition,
					gross_sat,fee_sat,payout_sat,forfeited_sat,evidence)
				VALUES($1,$2,'operator',$3,0,0,0,'{\"kind\":\"padding\",\"stored_preimage\":false}')
			", &[&allocation,&entitlement,&gross]).await?;
			t.execute("UPDATE vtxo SET spend_state='spent' WHERE vtxo_id=$1", &[&coin.id().to_string()]).await?;
			allocations.push(allocation);
		}
		t.execute("UPDATE expiry_allocation SET sweep_txid=$2,sweep_tx=$3 WHERE id=$1",
			&[&allocations[1],&parent.compute_txid().to_string(),&raw_parent]).await?;
		t.execute("
			INSERT INTO expiry_fee_child(allocation_id,txid,raw_tx,fee_sat,
				operator_remainder_sat,operator_net_contribution_sat)
			VALUES($1,$2,$3,1000,0,1000)
		", &[&allocations[1],&child.compute_txid().to_string(),&raw_child]).await?;
		Ok(allocations)
	}).await.unwrap();
	let server = ExpiryChainListener::start(db.clone(), BlockTable::Captaind).await.unwrap();
	let watchman = ExpiryChainListener::start(db.clone(), BlockTable::Watchmand).await.unwrap();
	server.on_sync_complete(point).await.unwrap();
	watchman.on_sync_complete(point).await.unwrap();
	let view = db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().unwrap();
	assert!(db.write(async |t| t.finish_expiry_rebuild(view).await).await.unwrap());

	let snapshot = async || db.read(async |t| {
		Ok(t.query_one("
			SELECT jsonb_agg(jsonb_build_array(to_jsonb(a),to_jsonb(c),to_jsonb(e),to_jsonb(v),
				(SELECT jsonb_agg(to_jsonb(i) ORDER BY i.vtxo_id) FROM expiry_allocation_input i WHERE i.allocation_id=a.id),
				(SELECT jsonb_agg(to_jsonb(f) ORDER BY f.id) FROM expiry_fee_child f WHERE f.allocation_id=a.id))
				ORDER BY a.id)::TEXT
			FROM expiry_allocation a
			JOIN expiry_allocation_coin c ON c.allocation_id=a.id
			JOIN expiry_entitlement e ON e.id=c.entitlement_id
			JOIN vtxo v ON v.vtxo_id=e.vtxo_id
		", &[]).await?.get::<_, String>(0))
	}).await.unwrap();
	let before = snapshot().await;
	// Reading the pending list before a policy check must not authorize signing.
	let pending = db.read(async |t| t.pending_expiry_payouts().await).await.unwrap();
	assert_eq!(pending.len(), 2);
	assert!(pending.iter().find(|p| p.id == allocations[0]).unwrap().parent.is_none());
	let error = db.write(async |t| t.lock_expiry_parent_for_signing(view, allocations[0]).await).await.unwrap_err();
	assert!(format!("{error:#}").contains("legacy padding lacks economic ownership evidence"));
	let error = db.write(async |t| t.store_expiry_parent(view, allocations[0], &parent).await).await.unwrap_err();
	assert!(format!("{error:#}").contains("legacy padding lacks economic ownership evidence"));
	assert_eq!(snapshot().await, before, "refusal must retain every row, amount, byte and active fence");
	assert_eq!(db.write(async |t| t.lock_expiry_parent_for_signing(view, allocations[1]).await).await.unwrap(), Some(parent.clone()));
	db.write(async |t| t.store_expiry_parent(view, allocations[1], &parent).await).await.unwrap();
	let mut replacement = parent.clone();
	replacement.output[0].value -= bitcoin::Amount::from_sat(1);
	let error = db.write(async |t| t.store_expiry_parent(view, allocations[1], &replacement).await).await.unwrap_err();
	assert!(format!("{error:#}").contains("attempt to replace immutable expiry parent"));
	assert_eq!(snapshot().await, before);

	// A previous executable could also replan through its irreversible fact.
	// Recovery's pending reset must not make either legacy encoding signable.
	db.write(async |t| {
		t.execute("
			UPDATE expiry_allocation_coin SET evidence=jsonb_build_object(
				'kind','irreversible_entitlement','entitlement_id',entitlement_id)
			WHERE allocation_id=$1
		", &[&allocations[0]]).await?;
		t.execute("UPDATE expiry_allocation SET status='conflicted' WHERE id=$1", &[&allocations[0]]).await?;
		t.execute("UPDATE expiry_allocation SET confirmed_height=NULL,confirmed_hash=NULL,status='pending'", &[]).await?;
		Ok(())
	}).await.unwrap();
	let server = ExpiryChainListener::start(db.clone(), BlockTable::Captaind).await.unwrap();
	server.on_sync_complete(point).await.unwrap();
	watchman.on_sync_complete(point).await.unwrap();
	let resumed = db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().unwrap();
	assert!(resumed.generation > view.generation);
	assert!(db.write(async |t| t.finish_expiry_rebuild(resumed).await).await.unwrap());
	let reset = snapshot().await;
	let error = db.write(async |t| t.lock_expiry_parent_for_signing(resumed, allocations[0]).await).await.unwrap_err();
	assert!(format!("{error:#}").contains("legacy padding lacks economic ownership evidence"));
	db.write(async |t| t.store_expiry_parent(resumed, allocations[1], &parent).await).await.unwrap();
	let pending = db.read(async |t| t.pending_expiry_payouts().await).await.unwrap();
	let saved = pending.iter().find(|p| p.id == allocations[1]).unwrap();
	assert_eq!(serialize(saved.parent.as_ref().unwrap()), raw_parent);
	assert_eq!(serialize(&saved.fee_child.as_ref().unwrap().0), raw_child);
	assert_eq!(saved.fee_child.as_ref().unwrap().1, bitcoin::Amount::from_sat(1000));
	let error = db.write(async |t| t.lock_unallocated_watchman_inputs(&[coins[0].id(),coins[1].id()]).await).await.unwrap_err();
	assert!(format!("{error:#}").contains("reserved for expiry recovery"));
	assert_eq!(snapshot().await, reset);
}
