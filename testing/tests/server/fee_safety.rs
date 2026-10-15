use std::time::Duration;

use bitcoin::{Amount, FeeRate, Transaction};
use bitcoincore_rpc::RpcApi;
use serde_json::json;

use bitcoin_ext::BlockDelta;
use server_log::ClaimBroadcast;

use ark_testing::{btc, require_bark_version, sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::captaind::SweepAdminClient;
use ark_testing::util::FutureExt;

/// A nonurgent expired sweep must not exceed its signed ceiling to replace
/// an earlier cheap sweep. The original transaction remains publishable.
#[tokio::test]
async fn watchman_nonurgent_replacement_retains_signed_fee_ceiling() {
	require_bark_version!(> "0.5.0");

	let ctx = TestContext::new("server/watchman_nonurgent_replacement_retains_signed_fee_ceiling").await;
	let ceiling = FeeRate::from_sat_per_vb_u32(2);
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(144);
	}).watchmand_cfg(move |cfg| {
		cfg.watchman.reaction_interval = Duration::from_secs(3600);
		cfg.watchman.sweep_interval = Duration::from_secs(3600);
		cfg.watchman.incremental_relay_fee = FeeRate::from_sat_per_vb_u32(5);
		cfg.fee_estimator.fallback_fee_rate_fast = FeeRate::from_sat_per_vb_u32(1);
		cfg.fee_estimator.fallback_fee_rate_regular = FeeRate::from_sat_per_vb_u32(1);
		cfg.fee_estimator.fallback_fee_rate_slow = FeeRate::from_sat_per_vb_u32(1);
		cfg.fee_estimator.max_fee_rate = Some(ceiling);
	}).create().await;
	let wm = srv.watchmand();
	let config = wm.config().clone();
	let client = server::bitcoind::build_client(&config.bitcoind.url, config.bitcoind.auth()).unwrap();
	let mut claims = wm.subscribe_log::<ClaimBroadcast>();
	let bark = ctx.bark("bark", &srv).funded(sat(200_000)).create().await;
	bark.board(sat(100_000)).await;
	ctx.generate_blocks(2 * BOARD_CONFIRMATIONS).await;
	bark.sync().await;
	ctx.refresh_all(&srv, &[&bark]).await;
	let tip = ctx.generate_blocks(144 - 2 * BOARD_CONFIRMATIONS + 2).await;
	wm.wait_for_sync_height(tip).await;
	wm.trigger_sweep().await;
	let first = claims.recv().wait(Duration::from_secs(15)).await
		.expect("setup: the real expired board must produce an initial native sweep");
	assert_eq!(first.vtxo_ids.len(), 1, "setup: only the old board may expire");
	assert_eq!(first.total_input_value, sat(100_000));
	let core = wm.bitcoind().sync_client();
	let original = core.get_raw_transaction(&first.txid, None).unwrap();
	assert_eq!(original.input.len(), 1);
	let source = original.input[0].previous_output;
	let source_tx = core.get_raw_transaction(&source.txid, None).unwrap();
	let gross = source_tx.output[source.vout as usize].value;
	let original_fee = gross - original.output.iter().map(|out| out.value).sum::<Amount>();
	assert!(original_fee > Amount::ZERO);
	assert!(original_fee <= ceiling.fee_wu(original.weight()).unwrap());
	assert_eq!(original_fee, first.fee, "the log must agree with actual Core bytes");

	// Core's modified chunk fee is an input to the real watchman replacement
	// calculation. This changes mining priority, not the original signed fee.
	let delta = -i64::try_from(original_fee.to_sat() / 2).unwrap();
	assert!(delta < 0);
	core.call::<bool>("prioritisetransaction", &[
		json!(first.txid), json!(0), json!(delta),
	]).unwrap();
	let entry: serde_json::Value = core.call("getmempoolentry", &[json!(first.txid)]).unwrap();
	let modified = Amount::from_btc(entry["fees"]["chunk"].as_f64().unwrap()).unwrap();
	assert!(modified > Amount::ZERO && modified < original_fee,
		"setup: Core must report a smaller positive chunk fee");
	let log_path = wm.config().data_dir.join("stdout.log");
	wm.stop().await.unwrap();
	let old_log_len = std::fs::metadata(&log_path).unwrap().len() as usize;
	wm.start().await.unwrap();
	let admin_url = format!("http://{}", wm.config().admin_address.unwrap());
	let mut admin = tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			match SweepAdminClient::connect(admin_url.clone()).await {
				Ok(client) => break client,
				Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
			}
		}
	}).await.expect("setup: the restarted native watchman admin must be ready");
	admin.trigger_sweep(server_rpc::protos::Empty {}).await.unwrap();

	// The restarted daemon reconstructs the actual old spender from Core.
	// A signed-ceiling refusal is acceptable only if the original native
	// transaction is still present. An unrelated setup error cannot pass.
	let (observed, refusal) = tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let spender = server::bitcoind::get_mempool_spending_tx(&client, source).await.unwrap();
			let log = std::fs::read(&log_path).unwrap();
			assert!(log.len() >= old_log_len, "setup: the original log must remain preserved");
			let tail = String::from_utf8_lossy(&log[old_log_len..]);
			let refused = tail.contains("configured maximum");
			if spender != Some(first.txid) || refused { break (spender, refused); }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("setup: neither native replacement nor signed-ceiling refusal was observed");
	let txid = observed.expect("setup: the board must retain a real mempool spender");
	let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
	assert_eq!(tx.input.iter().map(|input| input.previous_output).collect::<Vec<_>>(),
		original.input.iter().map(|input| input.previous_output).collect::<Vec<_>>(),
		"the replacement must retain the same native source");
	let fee = gross - tx.output.iter().map(|out| out.value).sum::<Amount>();
	let limit = ceiling.fee_wu(tx.weight()).unwrap();
	std::fs::write(ctx.datadir.join("nonurgent-sweep-signed-ceiling.json"),
		serde_json::to_vec_pretty(&json!({
			"original_txid": first.txid, "observed_txid": txid, "source": source,
			"original_fee_sat": original_fee.to_sat(), "actual_fee_sat": fee.to_sat(),
			"actual_weight_wu": tx.weight().to_wu(), "signed_ceiling_sat": limit.to_sat(),
			"core_original_chunk": entry, "priority_delta_sat": delta,
			"explicit_signed_ceiling_refusal": refusal,
		})).unwrap(),
	).unwrap();
	assert!(fee <= limit,
		"nonurgent sweep exceeded signed ceiling: actual fee {} exceeds {} for weight {}",
		fee, limit, tx.weight());
	assert_eq!(txid, first.txid, "this setup leaves no affordable native replacement");
	assert_eq!(tx, original, "deferral preserves the original signed transaction");
	assert!(refusal, "the test must observe the actual ceiling refusal");
}
