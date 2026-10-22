use std::fs;
use std::path::Path;
use std::time::Duration;

use ark_testing::{sat, Captaind, TestContext};
use ark_testing::daemon::watchmand::Watchmand;
use ark_testing::util::wait_timeout;
use bitcoin::Network;
use server::config;
use server_rpc::admin::WalletAdminServiceClient;
use server_rpc::protos;

#[tokio::test]
async fn watchmand_start_refuses_zero_trusted_confirmations_before_opening_state() {
	let ctx = TestContext::new_minimal("server/watchmand_start_refuses_zero_trusted_confirmations").await;
	let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
	let mut cfg = config::watchmand::Config::load(workspace.join("server/watchmand.default.toml")).unwrap();
	cfg.network = Network::Regtest;
	cfg.data_dir = ctx.datadir.join("unused-watchmand-data");
	cfg.bitcoind = config::Bitcoind {
		url: "http://127.0.0.1:1".to_owned(),
		cookie: Some(ctx.datadir.join("test.cookie")),
		rpc_user: None,
		rpc_pass: None,
	};
	fs::write(cfg.bitcoind.cookie.as_ref().unwrap(), b"watchmand-cli:source-only-regtest").unwrap();
	cfg.admin_address = None;

	for confirmations in [1, 0] {
		cfg.min_trusted_confs = confirmations;
		let config_path = ctx.datadir.join(format!("watchmand-{confirmations}.toml"));
		cfg.write_into(&mut fs::File::create(&config_path).unwrap()).unwrap();
		let mut command = if confirmations == 0 {
			let mut command = Watchmand::base_cmd();
			command.arg("start").arg("--config").arg(&config_path);
			command
		} else {
			let mut command = Captaind::base_cmd();
			command.arg("check-config").arg(&config_path).arg("--watchman");
			command
		};
		command.kill_on_drop(true);
		let output = tokio::time::timeout(wait_timeout(), command.output()).await
			.expect("native config command did not terminate").expect("native command spawn failed");
		let prefix = ctx.datadir.join(format!("confirmations-{confirmations}"));
		fs::write(prefix.with_extension("stdout.log"), &output.stdout).unwrap();
		fs::write(prefix.with_extension("stderr.log"), &output.stderr).unwrap();
		fs::write(prefix.with_extension("result.json"), serde_json::to_vec_pretty(&serde_json::json!({
			"min_trusted_confs": confirmations,
			"exit_code": output.status.code(),
			"success": output.status.success(),
			"data_dir_exists": cfg.data_dir.exists(),
		})).unwrap()).unwrap();
		let stderr = String::from_utf8_lossy(&output.stderr);
		if confirmations == 0 {
			assert!(matches!(output.status.code(), Some(code) if code != 0), "{stderr}");
			assert!(stderr.contains("min_trusted_confs must be at least 1"), "{stderr}");
		} else {
			assert!(output.status.success(), "{stderr}");
			assert!(String::from_utf8_lossy(&output.stdout).contains("Watchmand config is valid"));
		}
		assert!(!cfg.data_dir.exists(), "config validation must precede daemon state access");
	}
}

#[tokio::test]
async fn watchman_wallet_admin_status_exposes_its_own_funding_address_and_balance() {
	let ctx = TestContext::new("server/watchman_wallet_admin_status").await;
	let srv = ctx.captaind("server").watchmand().create().await;
	let address = srv.watchmand().config().admin_address.unwrap().to_string();
	let mut rpc = WalletAdminServiceClient::connect(format!("http://{address}")).await.unwrap();
	let status = rpc.wallet_status(protos::Empty {}).await.unwrap().into_inner();
	assert!(status.rounds.is_none());
	let watchman: server_rpc::WalletStatus = status.watchman.unwrap().try_into().unwrap();
	let expected_trusted = watchman.trusted_balance.to_sat() + 10_000;
	let funding = watchman.address.require_network(Network::Regtest).unwrap();
	let rounds = srv.get_rounds_funding_address().await;
	assert_ne!(funding, rounds, "fee subsidy must reach the actual distinct watchman wallet");
	let deposit = ctx.bitcoind().fund_addr(funding, sat(10_000)).await;
	ctx.generate_blocks(2).await;
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let status = rpc.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap();
			if status.trusted_balance >= expected_trusted {
				assert!(status.confirmed_utxos.iter().any(|point| point.parse::<bitcoin::OutPoint>().unwrap().txid == deposit));
				break;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}).await.unwrap();
	// The shared native admin CLI must accept a watchman-only response without
	// assuming that the captaind rounds wallet exists on this endpoint.
	let output = Captaind::base_cmd().env("RUST_LOG", "off").args(["rpc", "--addr", &address, "wallet"]).output().await.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
	assert!(json["rounds"].is_null());
	assert!(json["watchman"]["trusted_balance"].as_u64().unwrap() >= expected_trusted);
}
