//! Automatic fee policy changes against a genuine absent registered Board.

use super::*;
use std::time::Duration;

async fn wait_version(db: &Db, child: &mut Child, generation_count: usize) -> bitcoin::Transaction {
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			assert!(child.try_wait().unwrap().is_none(), "watchmand exited before automatic acceleration");
			let versions = db.read(async |t| t.query("SELECT signed_tx FROM expiry_carrier ORDER BY generation", &[])
				.await.map_err(Into::into)).await.unwrap();
			if versions.len() >= generation_count {
				return bitcoin::consensus::deserialize(&versions.last().unwrap().get::<_, Vec<u8>>(0)).unwrap();
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("automatic daemon must construct a higher-fee retained generation when its fee policy rises")
}

async fn wait_pending(ctx: &TestContext, tx: &bitcoin::Transaction) {
	tokio::time::timeout(Duration::from_secs(15), async {
		while !ctx.bitcoind().sync_client().get_raw_mempool().unwrap().contains(&tx.compute_txid()) {
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual signed generation must be accepted by Core");
}

#[tokio::test]
async fn automatic_fee_acceleration_replaces_stalled_registered_board() {
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } =
		automatic_board_fixture_with_fee("automatic_fee_acceleration_replaces_stalled_registered_board", sat(1_000)).await;
	// One finite reserve, funded before absence/expiry; no later topups.
	let address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(address, sat(50_000)).await;
	ctx.generate_blocks(3).await;
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let mut cap = srv.config().clone();
	cap.direct_expiry.interval = Duration::from_millis(100);
	cap.direct_expiry.max_groups_per_tick = 1;
	let cap_path = ctx.datadir.join("automatic-fee-captaind.toml");
	write_automatic_config(&cap_path, &toml::to_string_pretty(&cap).unwrap());
	let mut watch = srv.watchmand().config().clone();
	watch.watchman.reaction_interval = Duration::from_secs(1);
	watch.watchman.sweep_interval = Duration::from_secs(1);
	watch.fee_estimator.update_interval = Duration::from_secs(3600);
	watch.fee_estimator.fallback_fee_rate_regular = FeeRate::from_sat_per_vb(2).unwrap();
	watch.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
	let low_path = ctx.datadir.join("automatic-fee-low.toml");
	write_automatic_config(&low_path, &toml::to_string_pretty(&watch).unwrap());
	let mut cap_daemon = spawn_automatic_daemon("CAPTAIND_EXEC", &cap_path, ctx.datadir.join("automatic-fee-captaind.log"));
	let mut watch_daemon = spawn_automatic_daemon("WATCHMAND_EXEC", &low_path, ctx.datadir.join("automatic-fee-low.log"));
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let db = Db::connect(&cap.postgres).await.unwrap();
	let first = wait_version(&db, &mut watch_daemon, 1).await;
	wait_pending(&ctx, &first).await;
	// A miner is permitted to leave the low-fee carrier pending. Mine one
	// genuine empty block, then activate the higher operating fee policy.
	let _: serde_json::Value = core.call("generateblock", &[
		serde_json::json!(ctx.bitcoind().get_new_address().to_string()), serde_json::json!([]),
	]).unwrap();
	assert!(core.get_raw_mempool().unwrap().contains(&first.compute_txid()));
	watch_daemon.kill().await.unwrap();
	watch_daemon.wait().await.unwrap();
	watch.fee_estimator.fallback_fee_rate_regular = FeeRate::from_sat_per_vb(20).unwrap();
	let high_path = ctx.datadir.join("automatic-fee-high.toml");
	write_automatic_config(&high_path, &toml::to_string_pretty(&watch).unwrap());
	let mut watch_daemon = spawn_automatic_daemon("WATCHMAND_EXEC", &high_path, ctx.datadir.join("automatic-fee-high.log"));
	let replacement = wait_version(&db, &mut watch_daemon, 2).await;
	wait_pending(&ctx, &replacement).await;
	assert!(!core.get_raw_mempool().unwrap().contains(&first.compute_txid()));
	let expected = TxOut { value: sat(19_000), script_pubkey: record.spk };
	assert!(first.output.contains(&expected) && replacement.output.contains(&expected));
	assert!(replacement.input.iter().any(|i| i.previous_output == coin.chain_anchor()));
	let rows = db.read(async |t| t.query("SELECT signed_tx,miner_fee_sat,backing_ids FROM expiry_carrier ORDER BY generation", &[])
		.await.map_err(Into::into)).await.unwrap();
	assert_eq!(rows.len(), 2);
	assert_eq!(rows[0].get::<_, Vec<u8>>(0), bitcoin::consensus::serialize(&first));
	assert_eq!(rows[0].get::<_, Vec<String>>(2), rows[1].get::<_, Vec<String>>(2));
	assert!(rows[1].get::<_, i64>(1) > rows[0].get::<_, i64>(1));
	assert!(rows.iter().all(|r| r.get::<_, i64>(1) <= 10_000));
	ctx.generate_blocks(1).await;
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let confirmed = db.read(async |t| Ok(t.query_one("SELECT confirmed_height FROM expiry_carrier WHERE txid=$1",
				&[&replacement.compute_txid().to_string()]).await?.get::<_, Option<i32>>(0))).await.unwrap();
			if confirmed.is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	cap_daemon.kill().await.unwrap();
	cap_daemon.wait().await.unwrap();
	watch_daemon.kill().await.unwrap();
	watch_daemon.wait().await.unwrap();
	let (outputs, spent) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	let payouts = outputs.iter().filter(|(p,_)| p.txid == replacement.compute_txid() || p.txid == first.compute_txid())
		.collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	assert_eq!(payouts[0].1, sat(19_000));
	assert!(spent.input.iter().any(|i| i.previous_output == payouts[0].0));
}
