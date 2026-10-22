use std::fs::OpenOptions;
use std::io::Write;

use bitcoin::FeeRate;
use chrono::Utc;
use serde_json::{json, Value};

use ark_testing::{Captaind, TestContext};
use bitcoin_ext::rpc::{self, RpcApi};
use server::config::{self, Config};
use server::fee_estimator;
use server_rpc::protos;

fn configure_fees(cfg: &mut fee_estimator::Config, fallback: [u64; 3], ceiling: Option<u64>) {
	cfg.fallback_fee_rate_fast = FeeRate::from_sat_per_vb(fallback[0]).unwrap();
	cfg.fallback_fee_rate_regular = FeeRate::from_sat_per_vb(fallback[1]).unwrap();
	cfg.fallback_fee_rate_slow = FeeRate::from_sat_per_vb(fallback[2]).unwrap();
	cfg.max_fee_rate = ceiling.map(|rate| FeeRate::from_sat_per_vb(rate).unwrap());
}

// Only the regular target has a public quote RPC. These observations do not
// identify whether a quote precedes the first background estimator update.
#[allow(deprecated)]
async fn observe_quotes(
	ctx: &TestContext,
	srv: &Captaind,
	case: &str,
	phase: &str,
	fallback: [u64; 3],
	ceiling: Option<u64>,
) {
	let captain = Config::load(srv.config().data_dir.join("config.toml")).unwrap();
	let watchman = config::watchmand::Config::load(srv.watchmand().config().data_dir.join("config.toml")).unwrap();
	let mut configs = Vec::new();
	for (daemon, actual) in [("captaind", captain.fee_estimator), ("watchmand", watchman.fee_estimator)] {
		assert_eq!([
			actual.fallback_fee_rate_fast.to_sat_per_kwu(),
			actual.fallback_fee_rate_regular.to_sat_per_kwu(),
			actual.fallback_fee_rate_slow.to_sat_per_kwu(),
		], fallback.map(|rate| rate * 250));
		assert_eq!(actual.max_fee_rate.map(|rate| rate.to_sat_per_kwu()), ceiling.map(|rate| rate * 250));
		configs.push(json!({"daemon": daemon, "fee_estimator": actual}));
	}

	// The unfunded fixture has no fee samples. Keep the real Core replies so
	// the expected public quote does not depend on a simulated estimator.
	let mut estimates = Vec::new();
	for target in [1, 3, 6] {
		let response: Value = ctx.bitcoind().sync_client().call("estimatesmartfee", &[
			json!(target), json!(rpc::json::EstimateMode::Economical),
		]).unwrap();
		assert!(response["feerate"].is_null(), "fixture unexpectedly has a Core fee estimate: {response}");
		estimates.push(json!({"target": target, "response": response}));
	}
	let mut rpc = srv.get_public_rpc().await;
	let quote = rpc.get_offboard_fee_rate(protos::Empty {}).await.unwrap().into_inner().sat_vkb;
	let legacy = rpc.get_ark_info(protos::Empty {}).await.unwrap().into_inner().offboard_feerate_sat_vkb;
	let expected = ceiling.map(|max| fallback[1].min(max)).unwrap_or(fallback[1]) * 1000;
	let observation = json!({
		"captured_at": Utc::now().to_rfc3339(), "case": case, "phase": phase,
		"transport": "public tonic TCP", "configs": configs, "core_estimates": estimates,
		"offboard_sat_vkb": quote, "ark_info_offboard_sat_vkb": legacy, "expected_sat_vkb": expected,
	});
	// Exclude the RPC credentials and all other daemon config fields.
	let mut file = OpenOptions::new().create(true).append(true)
		.open(ctx.datadir.join("fee-ceiling-observations.jsonl")).unwrap();
	serde_json::to_writer(&mut file, &observation).unwrap();
	file.write_all(b"\n").unwrap();
	assert_eq!(quote, expected, "unexpected public offboard quote for {case}/{phase}");
	assert_eq!(legacy, expected, "unexpected legacy offboard quote for {case}/{phase}");
}

#[tokio::test]
async fn fee_ceiling_public_regular_quotes_survive_daemon_restart() {
	let ctx = TestContext::new("server/fee_ceiling_public_regular_quotes_survive_daemon_restart").await;
	for (case, fallback, ceiling) in [
		("ceiling", [25, 17, 9], Some(5)),
		("no-ceiling", [25, 17, 9], None),
		("below-ceiling", [4, 3, 2], Some(5)),
	] {
		let srv = ctx.captaind(case).bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
			.cfg(move |cfg| configure_fees(&mut cfg.fee_estimator, fallback, ceiling))
			.watchmand_cfg(move |cfg| configure_fees(&mut cfg.fee_estimator, fallback, ceiling))
			.create().await;
		observe_quotes(&ctx, &srv, case, "startup", fallback, ceiling).await;
		srv.watchmand().stop().await.unwrap();
		srv.stop().await.unwrap();
		srv.start().await.unwrap();
		srv.watchmand().start().await.unwrap();
		observe_quotes(&ctx, &srv, case, "restart", fallback, ceiling).await;
		srv.watchmand().stop().await.unwrap();
		srv.stop().await.unwrap();
	}
}
