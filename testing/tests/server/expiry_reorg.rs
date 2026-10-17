use std::time::Duration;

use ark_testing::{sat, TestContext};
use bitcoin::{BlockHash, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::{BlockDelta, BlockRef};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::{BlockTable, Db};

async fn wait_for_payout(db: &Db, coin: ark::VtxoId) -> (i64, Transaction) {
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt(
				"SELECT a.id,a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND a.broadcast_at IS NOT NULL ORDER BY a.id LIMIT 1",
				&[&coin.to_string()],
			).await?)).await.unwrap();
			if let Some(row) = row {
				break (row.get("id"), deserialize(row.get::<_, &[u8]>("sweep_tx")).unwrap());
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual background payout did not publish")
}

async fn wait_for_confirmation(db: &Db, allocation: i64, point: BlockRef, hash: BlockHash) -> i64 {
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let state = db.read(async |t| {
				let row = t.query_one("SELECT v.generation,v.invalid,a.status,a.confirmed_hash \
					FROM expiry_chain_view v,expiry_allocation a WHERE v.id=1 AND a.id=$1", &[&allocation]).await?;
				let current = t.get_highest_block(BlockTable::Captaind).await? == Some(point)
					&& t.get_highest_block(BlockTable::Watchmand).await? == Some(point);
				Ok((current, row.get::<_, i64>("generation"), row.get::<_, bool>("invalid"),
					row.get::<_, String>("status"), row.get::<_, Option<String>>("confirmed_hash")))
			}).await.unwrap();
			if state.0 && !state.2 && state.3 == "confirmed" && state.4.as_deref() == Some(hash.to_string().as_str()) {
				break state.1;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("canonical payout confirmation was not rebuilt after actual daemon sync")
}

/// No fixture inserts entitlements, allocations, chain acknowledgments or
/// invalidation flags. Real Core forks and daemon restart drive the rebuild.
async fn payout_reorg(depth: u32) {
	let name = format!("server/expiry_payout_reorg_{depth}");
	let ctx = TestContext::new(&name).await;
	// Core invalidation is local to a node. Both daemons must observe the
	// same actual Core that this test forks, as in the Signet deployment.
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24)).watchmand().create().await;
	let wallet = ctx.bark("abandoned", &srv).funded(sat(200_000)).create().await;
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	assert_eq!(ids.len(), 1);
	let client = wallet.client().await;
	let coin = client.get_full_vtxo(ids[0]).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	let expected = coin.amount() - sat(1_000);
	drop(client);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let before = ctx.bitcoind().get_block_count().await as u32;
	assert!(before < coin.expiry_height().to_u32());
	ctx.generate_blocks(coin.expiry_height().to_u32() + 1 - before).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (allocation, parent) = wait_for_payout(&db, ids[0]).await;
	let txid = parent.compute_txid();
	assert_eq!(ctx.bitcoind().await_transaction(txid).await, parent);
	ctx.generate_blocks(depth).await;
	let rpc = ctx.bitcoind().sync_client();
	let original = rpc.custom_get_raw_transaction_info(txid, None).unwrap().unwrap();
	let old_hash = original.blockhash.expect("payout must actually confirm before its block is invalidated");
	assert!(original.confirmations.unwrap() >= depth);
	let old_tip = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
	let old_generation = wait_for_confirmation(&db, allocation, old_tip, old_hash).await;
	let payout_index = parent.output.iter().position(|out| out.script_pubkey == destination.spk).unwrap() as u32;
	assert_eq!(parent.output[payout_index as usize].value, expected);
	let permanent = db.read(async |t| Ok(t.query_one(
		"SELECT e.id,e.irreversible_facts::TEXT AS facts FROM expiry_entitlement e \
		JOIN expiry_decided_coin d ON d.entitlement_id=e.id WHERE d.vtxo_id=$1", &[&ids[0].to_string()],
	).await?)).await.unwrap();
	let entitlement: i64 = permanent.get("id");
	let irreversible: String = permanent.get("facts");

	// The server observes the shorter fork while the watchman is offline.
	// Its old acknowledgment must not authorize a rebuild for the new generation.
	srv.watchmand().stop().await.unwrap();
	rpc.invalidate_block(&old_hash).unwrap();
	let fork = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
	assert!(fork.height < old_tip.height);
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let ready = db.read(async |t| {
				let row = t.query_one("SELECT generation,invalid FROM expiry_chain_view WHERE id=1", &[]).await?;
				Ok(t.get_highest_block(BlockTable::Captaind).await? == Some(fork)
					&& row.get::<_, i64>("generation") > old_generation && row.get::<_, bool>("invalid"))
			}).await.unwrap();
			if ready { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual shorter fork was not durably invalidated before watchman restart");
	assert!(db.read(async |t| t.expiry_rebuild_token().await).await.unwrap().is_none(),
		"the stopped watchman's old acknowledgement cannot authorize this fork");
	assert_eq!(db.read(async |t| Ok(t.get_user_vtxo_by_id(ids[0]).await?.spend_state)).await.unwrap(),
		server::database::SpendState::Spent, "a reorg must not reopen the committed source");

	srv.watchmand().start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let valid = db.read(async |t| {
				let invalid = t.query_one("SELECT invalid FROM expiry_chain_view WHERE id=1", &[]).await?.get::<_, bool>(0);
				Ok(!invalid && t.get_highest_block(BlockTable::Captaind).await? == Some(fork)
					&& t.get_highest_block(BlockTable::Watchmand).await? == Some(fork))
			}).await.unwrap();
			// Core may retain the package through the reorg; otherwise the
			// actual watchman must rebroadcast it before replacement mining.
			if valid && rpc.get_mempool_entry(&txid).is_ok() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("watchman restart did not rebuild and recover the immutable payout package");

	// A fresh coinbase destination prevents reproducing the invalid block.
	let mining_address = ctx.bitcoind().get_new_address();
	rpc.generate_to_address(u64::from(depth + 1), &mining_address).unwrap();
	let new_tip = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
	let current = rpc.custom_get_raw_transaction_info(txid, None).unwrap().unwrap();
	let new_hash = current.blockhash.expect("the original payout must re-confirm on the replacement chain");
	assert_ne!(new_hash, old_hash);
	assert!(current.confirmations.unwrap() >= depth);
	let new_generation = wait_for_confirmation(&db, allocation, new_tip, new_hash).await;
	assert!(new_generation > old_generation);
	assert_eq!(ctx.bitcoind().await_transaction(txid).await, parent);
	assert_eq!(rpc.get_tx_out(&txid, payout_index, Some(true)).unwrap().unwrap().value, expected);
	let row = db.read(async |t| Ok(t.query_one(
		"SELECT e.irreversible_facts::TEXT AS facts, \
			(SELECT COUNT(*) FROM expiry_decided_coin d WHERE d.entitlement_id=e.id) AS fences, \
			(SELECT COUNT(*) FROM expiry_allocation_coin c WHERE c.entitlement_id=e.id AND c.active) AS active, \
			(SELECT COUNT(DISTINCT a.sweep_txid) FROM expiry_allocation_coin c \
			 JOIN expiry_allocation a ON a.id=c.allocation_id WHERE c.entitlement_id=e.id AND a.status='confirmed') AS payments, \
			(SELECT sweep_tx FROM expiry_allocation WHERE id=$2) AS raw \
		FROM expiry_entitlement e WHERE e.id=$1", &[&entitlement,&allocation],
	).await?)).await.unwrap();
	assert_eq!(row.get::<_, String>("facts"), irreversible);
	assert_eq!(row.get::<_, i64>("fences"), 1);
	assert_eq!(row.get::<_, i64>("active"), 1);
	assert_eq!(row.get::<_, i64>("payments"), 1);
	assert_eq!(deserialize::<Transaction>(row.get::<_, &[u8]>("raw")).unwrap(), parent,
		"reorg recovery cannot rewrite the immutable payout parent");
}

#[tokio::test]
async fn expiry_actual_payout_reorg_preserves_receipt_and_fence() {
	payout_reorg(6).await;
}

#[tokio::test]
async fn expiry_actual_deep_payout_reorg_preserves_receipt_and_fence() {
	payout_reorg(101).await;
}

/// A configured regtest fee rise drives the real watchman replacement path.
/// This is a fee-policy/restart test, not a congested public-network claim.
#[tokio::test]
async fn expiry_payout_fee_replacement_keeps_parent_and_counts_only_confirmed_child() {
	let ctx = TestContext::new("server/expiry_payout_fee_replacement").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24)).watchmand().create().await;
	let wallet = ctx.bark("abandoned", &srv).funded(sat(200_000)).create().await;
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let client = wallet.client().await;
	let coin = client.get_full_vtxo(ids[0]).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	drop(client);
	// Two genuinely confirmed funding outputs allow a replacement to select
	// another usable sponsor while the first child remains in the mempool.
	let fee_address = srv.watchmand().wait_wallet_address().await;
	for _ in 0..2 { ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await; }
	ctx.generate_blocks(2).await;
	let before = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32() + 1 - before).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (allocation, parent) = wait_for_payout(&db, ids[0]).await;
	let first = db.read(async |t| Ok(t.query_one("SELECT raw_tx,fee_sat FROM expiry_fee_child \
		WHERE allocation_id=$1 ORDER BY id DESC LIMIT 1", &[&allocation]).await?)).await.unwrap();
	let first_child: Transaction = deserialize(first.get("raw_tx")).unwrap();
	let first_fee = u64::try_from(first.get::<_, i64>("fee_sat")).unwrap();
	assert_eq!(ctx.bitcoind().await_transaction(first_child.compute_txid()).await, first_child);
	let target = bitcoin::FeeRate::from_sat_per_vb(100).unwrap();
	assert!(sat(first_fee) < target * (parent.weight() + first_child.weight()));
	let payouts = parent.output.iter().filter(|out| out.script_pubkey == destination.spk).collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	assert_eq!(payouts[0].value, coin.amount() - sat(1_000));
	srv.watchmand().stop().await.unwrap();
	{
		let mut config = srv.watchmand().config_mut();
		config.fee_estimator.fallback_fee_rate_fast = target;
		config.fee_estimator.fallback_fee_rate_regular = target;
		config.fee_estimator.fallback_fee_rate_slow = target;
	}
	srv.watchmand().start().await.unwrap();
	let rpc = ctx.bitcoind().sync_client();
	let (replacement, recorded_fee) = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_one("SELECT a.sweep_tx,f.raw_tx,f.fee_sat \
				FROM expiry_allocation a JOIN expiry_fee_child f ON f.allocation_id=a.id \
				WHERE a.id=$1 ORDER BY f.id DESC LIMIT 1", &[&allocation]).await?)).await.unwrap();
			let saved: Transaction = deserialize(row.get("sweep_tx")).unwrap();
			assert_eq!(saved, parent, "fee replacement must never change the payout parent");
			let child: Transaction = deserialize(row.get("raw_tx")).unwrap();
			if child != first_child && rpc.get_mempool_entry(&child.compute_txid()).is_ok() {
				break (child, u64::try_from(row.get::<_, i64>("fee_sat")).unwrap());
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("configured fee rise did not replace the real mempool fee child");
	assert!(rpc.get_mempool_entry(&first_child.compute_txid()).is_err());
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	assert!(sat(recorded_fee) >= target * (parent.weight() + replacement.weight()));
	assert!(recorded_fee > first_fee && recorded_fee > 1_000);
	let mut input_value = 0;
	for input in &replacement.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		input_value += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(recorded_fee, input_value - replacement.output.iter().map(|out| out.value.to_sat()).sum::<u64>());
	ctx.generate_blocks(6).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let rows = db.read(async |t| Ok(t.query("SELECT txid,fee_sat,operator_remainder_sat \
				FROM expiry_fee_child WHERE allocation_id=$1 AND confirmed_hash IS NOT NULL", &[&allocation]).await?)).await.unwrap();
			if !rows.is_empty() {
				assert_eq!(rows.len(), 1, "superseded children must not be counted as miner costs");
				assert_eq!(rows[0].get::<_, String>("txid"), replacement.compute_txid().to_string());
				assert_eq!(rows[0].get::<_, i64>("fee_sat"), i64::try_from(recorded_fee).unwrap());
				assert_eq!(rows[0].get::<_, i64>("operator_remainder_sat"), 1_000);
				break;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual canonical fee child confirmation was not accounted");
}
