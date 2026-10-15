//! Automatic fee policy changes against a genuine absent registered Board.

use super::*;
use std::time::Duration;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use axum::{body::Bytes, extract::State, http::HeaderMap, routing::post, Json, Router};

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
	run_case("automatic_fee_acceleration_replaces_stalled_registered_board", false, false).await;
}

#[tokio::test]
async fn automatic_fee_acceleration_old_winner_retains_union_across_restart() {
	run_case("automatic_fee_acceleration_old_winner_retains_union_across_restart", true, false).await;
}

#[tokio::test]
async fn automatic_fee_acceleration_ceiling_defers_without_reducing_payout() {
	run_case("automatic_fee_acceleration_ceiling_defers_without_reducing_payout", false, true).await;
}

// Only block ingestion can be paused. Transaction status/submission and every
// other response remain genuine Core observations; this creates no chain or
// journal state. An immediate RPC error leaves the wallet unlocked for its
// actual admin inventory while the independent delivery worker records a win.
#[derive(Clone)]
struct BlockGate { upstream: String, paused: Arc<AtomicBool>, client: reqwest::Client }

async fn gated_rpc(State(gate): State<BlockGate>, headers: HeaderMap, body: Bytes) -> Json<serde_json::Value> {
	let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
	if request["method"] == "getblock" && gate.paused.load(Ordering::SeqCst) {
		return Json(serde_json::json!({"result":null,"id":request["id"],
			"error":{"code":-28,"message":"fixture pauses owning-wallet block ingestion"}}));
	}
	let response = gate.client.post(&gate.upstream).headers(headers).body(body.to_vec()).send().await.unwrap();
	Json(serde_json::from_slice(&response.bytes().await.unwrap()).unwrap())
}

async fn inventory(address: std::net::SocketAddr) -> serde_json::Value {
	let out = Command::new(std::env::var_os("CAPTAIND_EXEC").unwrap())
		.args(["rpc", "--addr", &address.to_string(), "wallet-inventory"]).output().await.unwrap();
	assert!(out.status.success(), "actual owning watchmand inventory must be available");
	serde_json::from_slice(&out.stdout).unwrap()
}

async fn run_case(name: &str, old_winner: bool, fee_shortfall: bool) {
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } =
		automatic_board_fixture_with_funding(name, sat(1_000), sat(200_000)).await;
	assert_eq!(coin.amount(), sat(20_000), "Board base fee is separate from the eventual flat payout fee");
	// One finite reserve, funded before absence/expiry; no later topups.
	let address = srv.watchmand().wait_wallet_address().await;
	let fee_funding_txid = ctx.bitcoind().fund_addr(address, sat(50_000)).await;
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
	let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
	let gate = BlockGate { upstream: watch.bitcoind.url.clone(), paused: Arc::new(AtomicBool::new(false)), client: reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap() };
	watch.bitcoind.url = format!("http://{}", listener.local_addr().unwrap());
	let rpc = Router::new().route("/", post(gated_rpc)).with_state(gate.clone());
	let relay = tokio::spawn(async move { axum::serve(listener, rpc).await.unwrap(); });
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
	let allocations = db.read(async |t| Ok(t.query_one("SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id)::text FROM expiry_allocation a", &[])
		.await?.get::<_, String>(0))).await.unwrap();
	watch_daemon.kill().await.unwrap();
	watch_daemon.wait().await.unwrap();
	watch.fee_estimator.fallback_fee_rate_regular = FeeRate::from_sat_per_vb(20).unwrap();
	if fee_shortfall {
		watch.watchman.direct_expiry.as_mut().unwrap().max_miner_fee = sat(1_000);
		let path = ctx.datadir.join("automatic-fee-capped.toml");
		write_automatic_config(&path, &toml::to_string_pretty(&watch).unwrap());
		let log = ctx.datadir.join("automatic-fee-capped.log");
		let mut capped = spawn_automatic_daemon("WATCHMAND_EXEC", &path, log.clone());
		tokio::time::timeout(Duration::from_secs(15), async {
			loop {
				assert!(capped.try_wait().unwrap().is_none());
				if std::fs::read_to_string(&log).unwrap().contains("miner fee exceeds ceiling") { break; }
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}).await.expect("higher policy must defer at the actual signed miner ceiling");
		assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_carrier", &[]).await?.get::<_, i64>(0))).await.unwrap(), 1);
		assert!(core.get_raw_mempool().unwrap().contains(&first.compute_txid()));
		capped.kill().await.unwrap(); capped.wait().await.unwrap();
		watch.watchman.direct_expiry.as_mut().unwrap().max_miner_fee = sat(10_000);
	}
	let high_path = ctx.datadir.join("automatic-fee-high.toml");
	write_automatic_config(&high_path, &toml::to_string_pretty(&watch).unwrap());
	let mut watch_daemon = spawn_automatic_daemon("WATCHMAND_EXEC", &high_path, ctx.datadir.join("automatic-fee-high.log"));
	let replacement = wait_version(&db, &mut watch_daemon, 2).await;
	wait_pending(&ctx, &replacement).await;
	assert!(!core.get_raw_mempool().unwrap().contains(&first.compute_txid()));
	let expected = TxOut { value: coin.amount().checked_sub(sat(1_000)).unwrap(), script_pubkey: record.spk };
	assert!(first.output.contains(&expected) && replacement.output.contains(&expected));
	assert!(replacement.input.iter().any(|i| i.previous_output == coin.chain_anchor()));
	let rows = db.read(async |t| t.query("SELECT signed_tx,miner_fee_sat,backing_ids FROM expiry_carrier ORDER BY generation", &[])
		.await.map_err(Into::into)).await.unwrap();
	assert_eq!(rows.len(), 2);
	assert_eq!(rows[0].get::<_, Vec<u8>>(0), bitcoin::consensus::serialize(&first));
	assert_eq!(rows[0].get::<_, Vec<String>>(2), rows[1].get::<_, Vec<String>>(2));
	assert!(rows[1].get::<_, i64>(1) > rows[0].get::<_, i64>(1));
	assert!(rows.iter().all(|r| r.get::<_, i64>(1) <= 10_000));
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id)::text FROM expiry_allocation a", &[])
		.await?.get::<_, String>(0))).await.unwrap(), allocations, "every frozen economic allocation remains immutable");
	for (name, tx) in [("first", &first), ("replacement", &replacement)] {
		std::fs::write(ctx.datadir.join(format!("automatic-fee-{name}.tx")), bitcoin::consensus::serialize(tx)).unwrap();
	}
	let winner = if old_winner { &first } else { &replacement };
	let fee_points = db.read(async |t| Ok(t.query_one("SELECT fee_inputs::text FROM expiry_carrier WHERE txid=$1",
		&[&replacement.compute_txid().to_string()]).await?.get::<_, String>(0))).await.unwrap();
	let fee_inputs: serde_json::Value = serde_json::from_str(&fee_points).unwrap();
	assert_eq!(fee_inputs.as_array().unwrap().len(), 1);
	let fee_point = fee_inputs[0]["outpoint"].as_str().unwrap();
	let admin = watch.admin_address.unwrap();
	if old_winner {
		// Wait until the owner has actually ingested all preceding blocks.
		tokio::time::timeout(Duration::from_secs(15), async {
			while inventory(admin).await["watchman"]["tip_height"].as_u64().unwrap() < core.get_block_count().unwrap() {
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}).await.unwrap();
		gate.paused.store(true, Ordering::SeqCst);
		let _: serde_json::Value = core.call("generateblock", &[
			serde_json::json!(ctx.bitcoind().get_new_address().to_string()),
			serde_json::json!([bitcoin::consensus::encode::serialize_hex(&first)]),
		]).unwrap();
	} else { ctx.generate_blocks(1).await; }
	let winner_height = core.get_block_count().unwrap();
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let confirmed = db.read(async |t| Ok(t.query_one("SELECT confirmed_height FROM expiry_carrier WHERE txid=$1",
				&[&winner.compute_txid().to_string()]).await?.get::<_, Option<i32>>(0))).await.unwrap();
			if confirmed.is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	if old_winner {
		let before = inventory(admin).await;
		assert!(before["watchman"]["tip_height"].as_u64().unwrap() < winner_height);
		assert!(before["watchman"]["reserved_outpoints"].as_array().unwrap().iter().any(|p| p == fee_point));
		let point: OutPoint = fee_point.parse().unwrap();
		assert_eq!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap().value, sat(50_000),
			"actual original winner leaves the losing replacement fee coin physically unspent");
		watch_daemon.kill().await.unwrap(); watch_daemon.wait().await.unwrap();
		// The actual active chain still contains the original winner. Startup
		// restores its complete union before block ingestion can advance.
		watch_daemon = spawn_automatic_daemon("WATCHMAND_EXEC", &high_path, ctx.datadir.join("automatic-fee-restarted.log"));
		let restored = tokio::time::timeout(Duration::from_secs(15), async {
			loop {
				assert!(watch_daemon.try_wait().unwrap().is_none());
				if let Ok(mut rpc) = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{admin}")).await {
					if rpc.wallet_inventory(server_rpc::protos::Empty {}).await.is_ok() { break inventory(admin).await; }
				}
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}).await.unwrap();
		assert!(restored["watchman"]["tip_height"].as_u64().unwrap() < winner_height);
		assert!(restored["watchman"]["reserved_outpoints"].as_array().unwrap().iter().any(|p| p == fee_point),
			"actual startup must restore losing-version fee guards even after a winner was recorded");
		std::fs::write(ctx.datadir.join("owning-inventory-before-sync.json"), serde_json::to_vec_pretty(&restored).unwrap()).unwrap();
		gate.paused.store(false, Ordering::SeqCst);
		tokio::time::timeout(Duration::from_secs(25), async {
			loop {
				let synced = inventory(admin).await;
				if synced["watchman"]["tip_height"].as_u64().unwrap() >= winner_height &&
					synced["watchman"]["reserved_outpoints"].as_array().unwrap().is_empty() {
					assert!(synced["watchman"]["utxos"].as_array().unwrap().iter().any(|u|
						u["outpoint"] == fee_point && u["confirmed_fee_eligible"] == true));
					std::fs::write(ctx.datadir.join("owning-inventory-after-sync.json"), serde_json::to_vec_pretty(&synced).unwrap()).unwrap();
					break;
				}
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}).await.expect("only actual owning-wallet confirmation sync releases the losing input");
	}
	cap_daemon.kill().await.unwrap();
	cap_daemon.wait().await.unwrap();
	watch_daemon.kill().await.unwrap();
	watch_daemon.wait().await.unwrap();
	relay.abort();
	let (outputs, spent) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	let payouts = outputs.iter().filter(|(p,_)| p.txid == replacement.compute_txid() || p.txid == first.compute_txid())
		.collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	assert_eq!(payouts[0].1, sat(19_000));
	assert!(spent.input.iter().any(|i| i.previous_output == payouts[0].0));
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_carrier", &[]).await?.get::<_, i64>(0))).await.unwrap(), 2);
	let evidence = serde_json::json!({
		"first":core.decode_raw_transaction(&bitcoin::consensus::serialize(&first), None).unwrap(),
		"replacement":core.decode_raw_transaction(&bitcoin::consensus::serialize(&replacement), None).unwrap(),
		"winner":core.get_raw_transaction_info(&winner.compute_txid(), None).unwrap(),
		"backing_funding":core.get_raw_transaction_info(&coin.chain_anchor().txid, None).unwrap(),
		"fee_funding":core.get_raw_transaction_info(&fee_funding_txid, None).unwrap(),
		"recovery":core.get_raw_transaction_info(&spent.compute_txid(), None).unwrap(),
		"miner_fee_sat":[rows[0].get::<_, i64>(1),rows[1].get::<_, i64>(1)],
		"allocations":serde_json::from_str::<serde_json::Value>(&allocations).unwrap(),
		"core_incremental_relay_fee":core.get_mempool_info().unwrap().incremental_relay_fee,
	});
	std::fs::write(ctx.datadir.join("carrier-chain-evidence.json"), serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
}
