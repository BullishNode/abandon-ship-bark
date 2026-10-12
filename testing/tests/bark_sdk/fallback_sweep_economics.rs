//! Sweep batching must not strand an affordable cohort's final input.
use std::time::Duration;

use bitcoin::{Transaction, Txid};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoincore_rpc::RpcApi;
use server::database::Db;

use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;

#[tokio::test]
async fn fallback_watchman_replaces_sweeps_with_same_cohort() {
	Box::pin(replace_sweep_cohort(false)).await;
}

#[tokio::test]
async fn fallback_watchman_replaces_sweeps_when_new_input_expires() {
	Box::pin(replace_sweep_cohort(true)).await;
}

async fn replace_sweep_cohort(late_input: bool) {
	let name = if late_input { "fallback_watchman_replaces_sweeps_when_new_input_expires" }
		else { "fallback_watchman_replaces_sweeps_with_same_cohort" };
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	// The deployed operator uses one Core node for captaind and watchmand.
	// Test replacement acceptance there, independently of peer relay ordering.
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(btc(1))
		.cfg(|c| c.vtxo_lifetime = BlockDelta::new(256))
		.watchmand_cfg(|c| {
			c.watchman.claim_chunksize = 15.try_into().unwrap();
			c.watchman.reaction_interval = Duration::from_secs(3600);
			c.watchman.sweep_interval = Duration::from_secs(3600);
			c.fee_estimator.update_interval = Duration::from_millis(100);
		}).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).funded(sat(600_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	for i in 0..16 {
		if late_input && i == 15 { ctx.generate_blocks(40).await; }
		let board = wallet.board_amount(sat(20_000)).await.unwrap();
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
		wallet.sync().await;
	}
	let mut coins = Vec::new();
	for coin in wallet.spendable_vtxos().await.unwrap() {
		coins.push(wallet.get_full_vtxo(coin.id()).await.unwrap());
	}
	assert_eq!(coins.len(), 16);
	coins.sort_by_key(|v| v.expiry_height());
	let record = wallet.fallback_destination().await.unwrap();
	drop(wallet);
	let core = ctx.bitcoind().sync_client();
	let initial_count = if late_input { 15 } else { 16 };
	let expiry = coins[initial_count - 1].expiry_height().to_u32();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	train_selected_blocks(&ctx, 6).await;
	let initial_rate = core.estimate_smart_fee(3, None).unwrap().fee_rate.unwrap().to_sat();
	assert!(initial_rate >= 5500, "real initial market rate required");
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	srv.watchmand().trigger_sweep().await;
	let anchors = coins.iter().map(|v| v.chain_anchor()).collect::<Vec<_>>();
	let old = tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let claims = sweep_mempool(&ctx, &anchors);
			if claims.iter().map(|t| t.input.len()).sum::<usize>() >= initial_count { break claims; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("initial eligible sweeps must reach the real mempool");
	assert_eq!(old.iter().map(|t| t.input.len()).sum::<usize>(), initial_count);
	let old_ids = old.iter().map(|t| t.compute_txid()).collect::<Vec<_>>();
	// Advance without mining the existing claims, so this actually tests RBF.
	while core.get_block_count().unwrap() <= coins.last().unwrap().expiry_height().to_u32() as u64 + 3 {
		let _: serde_json::Value = core.call("generateblock", &[
			ctx.bitcoind().get_new_address().to_string().into(), serde_json::json!([]),
		]).unwrap();
	}
	if !late_input {
		for _ in 0..40 {
			let _: serde_json::Value = core.call("generateblock", &[
				ctx.bitcoind().get_new_address().to_string().into(), serde_json::json!([]),
			]).unwrap();
		}
	}
	train_selected_blocks(&ctx, 7).await;
	let rate = core.estimate_smart_fee(3, None).unwrap().fee_rate.unwrap().to_sat();
	assert!(rate >= initial_rate + 500, "real fee increase required: {initial_rate} -> {rate}");
	assert!(old_ids.iter().all(|id| core.get_raw_mempool().unwrap().contains(id)),
		"fixture must leave every original claim unconfirmed until replacement");
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	tokio::time::sleep(Duration::from_secs(1)).await;
	srv.watchmand().trigger_sweep().await;
	let replaced = tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let claims = sweep_mempool(&ctx, &anchors);
			if claims.iter().map(|t| t.input.len()).sum::<usize>() == 16
				&& old_ids.iter().all(|id| !core.get_raw_mempool().unwrap().contains(id)) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.is_ok();
	let replacements = sweep_mempool(&ctx, &anchors);
	assert!(replaced && old_ids.iter().all(|id| !core.get_raw_mempool().unwrap().contains(id)),
		"higher market fee must replace old sweeps, including when eligible cohort changes");
	let mut inputs = replacements.iter().flat_map(|t| t.input.iter().map(|i| i.previous_output)).collect::<Vec<_>>();
	inputs.sort();
	let mut expected = anchors.clone(); expected.sort();
	assert_eq!(inputs, expected, "every original input must appear exactly once");
	assert!(replacements.iter().all(|t| t.input.len() <= 15));
	println!("RBF evidence: late_input={late_input}, rates={initial_rate}/{rate}, old={old_ids:?}, new={:?}",
		replacements.iter().map(|t| (t.compute_txid(), t.input.len())).collect::<Vec<_>>());
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let (payout, _) = super::fallback_lightning::wait_and_reconcile_payout(
		&ctx, &db, &ids, &record.spk, 320_000,
	).await;
	super::fallback_lightning::restore_and_spend(&ctx, &mnemonic, payout).await;
}

fn sweep_mempool(ctx: &TestContext, anchors: &[bitcoin::OutPoint]) -> Vec<Transaction> {
	let core = ctx.bitcoind().sync_client();
	core.get_raw_mempool().unwrap().into_iter().map(|id| core.get_raw_transaction(&id, None).unwrap())
		.filter(|t| t.input.iter().any(|i| anchors.contains(&i.previous_output))).collect()
}

async fn train_selected_blocks(ctx: &TestContext, fee_sat_vb: u64) {
	let core = ctx.bitcoind().sync_client();
	for _ in 0..12 {
		let mut selected = Vec::new();
		for _ in 0..8 {
			let txid: Txid = core.call("sendtoaddress", &[
				ctx.bitcoind().get_new_address().to_string().into(), 0.001.into(), "".into(), "".into(),
				false.into(), true.into(), serde_json::Value::Null, "unset".into(),
				serde_json::Value::Null, fee_sat_vb.into(),
			]).unwrap();
			selected.push(txid.to_string());
		}
		let _: serde_json::Value = core.call("generateblock", &[
			ctx.bitcoind().get_new_address().to_string().into(), serde_json::json!(selected),
		]).unwrap();
	}
}

#[tokio::test]
async fn fallback_watchman_small_tail_cohort_sweeps_before_payout() {
	Box::pin(small_tail_cohort(
		"fallback_watchman_small_tail_cohort_sweeps_before_payout", 16, 15,
	)).await;
}

#[tokio::test]
async fn fallback_watchman_many_small_batches_sweep_without_singleton_tail() {
	Box::pin(small_tail_cohort(
		"fallback_watchman_many_small_batches_sweep_without_singleton_tail", 31, 3,
	)).await;
}

async fn small_tail_cohort(name: &str, count: usize, limit: usize) {
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1))
		.cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(256);
			c.min_board_amount = sat(1000);
		}).watchmand_cfg(move |c| {
			c.watchman.claim_chunksize = limit.try_into().unwrap();
			c.watchman.reaction_interval = Duration::from_secs(3600);
			c.watchman.sweep_interval = Duration::from_secs(3600);
			c.fee_estimator.update_interval = Duration::from_millis(100);
		}).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).funded(sat(200_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	for _ in 0..count {
		let board = wallet.board_amount(sat(1000)).await.unwrap();
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
		wallet.sync().await;
	}
	let mut coins = Vec::new();
	for coin in wallet.spendable_vtxos().await.unwrap() {
		coins.push(wallet.get_full_vtxo(coin.id()).await.unwrap());
	}
	assert_eq!(coins.len(), count);
	assert!(coins.iter().all(|v| v.amount() == sat(1000)));
	let record = wallet.fallback_destination().await.unwrap();
	drop(wallet);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	// Keep automatic sweeps out of the expiry advance. Restarting watchmand
	// here would let its first tick sweep a partial cohort during block replay.
	let core = ctx.bitcoind().sync_client();
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	for _ in 0..12 {
		for _ in 0..8 {
			let address = ctx.bitcoind().get_new_address();
			let _: Txid = core.call("sendtoaddress", &[
				address.to_string().into(), 0.001.into(), "".into(), "".into(),
				false.into(), true.into(), serde_json::Value::Null, "unset".into(),
				serde_json::Value::Null, 6.into(),
			]).unwrap();
		}
		ctx.generate_blocks(1).await;
	}
	let estimate = core.estimate_smart_fee(3, None).unwrap();
	assert!(estimate.fee_rate.unwrap().to_sat() >= 5500,
		"the real estimator must make a singleton 1000-sat sweep uneconomical: {estimate:?}");
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let logs = std::fs::read_to_string(ctx.datadir.join("server-watchmand/stdout.log")).unwrap();
	assert!(!logs.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
		.any(|r| r["slog_id"] == "ClaimBroadcast"),
		"fixture must not allow a partial sweep before the whole cohort is eligible");
	srv.watchmand().trigger_sweep().await;
	// No fee downgrade, external input or synthetic sweep history until all
	// exact backing paths have really been swept and confirmed.
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	let anchors = coins.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
	let rows = db.read(async |t| Ok(t.query(
		"SELECT vtxo_id,onchain_spent_txid FROM vtxo WHERE vtxo_id=ANY($1)", &[&anchors],
	).await?)).await.unwrap();
	let mut sweep_ids = rows.iter().map(|r| r.get::<_, String>("onchain_spent_txid")).collect::<Vec<_>>();
	sweep_ids.sort();
	sweep_ids.dedup();
	for txid in &sweep_ids {
		let tx = core.get_raw_transaction(&txid.parse::<Txid>().unwrap(), None).unwrap();
		assert!(tx.input.len() <= limit, "configured chunk limit remains enforced");
		let input_sat = tx.input.iter().map(|i| {
			let prev = core.get_raw_transaction(&i.previous_output.txid, None).unwrap();
			prev.output[i.previous_output.vout as usize].value.to_sat()
		}).sum::<u64>();
		let fee_sat = input_sat - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
		assert!(fee_sat >= tx.vsize() as u64 * 5,
			"sweep must use the trained market fee, never a discounted test rate");
		assert!(tx.input.iter().all(|i| anchors.contains(&i.previous_output.to_string())),
			"sweeps use only the original cohort, without operator top-ups");
	}
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let (payout, _) = super::fallback_lightning::wait_and_reconcile_payout(
		&ctx, &db, &ids, &record.spk, count as u64 * 1000,
	).await;
	println!("small-tail evidence: estimate={estimate:?}, sweeps={sweep_ids:?}, payout={payout}");
	super::fallback_lightning::restore_and_spend(&ctx, &mnemonic, payout).await;
}
