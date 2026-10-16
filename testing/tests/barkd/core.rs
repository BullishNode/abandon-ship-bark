
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use ark_testing::{require_bark_version, Barkd, TestContext};
use ark_testing::ports::pick_port;
use bark_rest_client::apis::configuration::Configuration;
use bark_rest_client::apis::wallet_api;

/// Verify that `barkd` responds to `GET /ping`.
#[tokio::test]
async fn ping_barkd() {
	let ctx = TestContext::new("barkd/ping_barkd").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;

	// `wait_for_init` (called internally by start) already proved ping works;
	// this test provides explicit REST-level coverage.
	barkd.ping().await;
}

/// Verify that `GET /bitcoin/tip` returns the current block height.
#[tokio::test]
async fn bitcoin_tip_barkd() {
	let ctx = TestContext::new("barkd/bitcoin_tip_barkd").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;

	let height = ctx.bitcoind().get_block_count().await;

	let tip = barkd.tip().await;
	assert_eq!(
		tip.tip_height as u64, height,
		"barkd tip height should match current chain height",
	);
}

/// Verify that `GET /wallet/connected` reports true after wallet creation.
#[tokio::test]
async fn wallet_connected_barkd() {
	let ctx = TestContext::new("barkd/wallet_connected_barkd").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;

	let resp = barkd.connected().await;
	assert!(resp.connected, "wallet should be connected to the Ark server after creation");
}

/// Verify that `GET /wallet/ark-info` returns valid server parameters.
#[tokio::test]
async fn wallet_ark_info_barkd() {
	let ctx = TestContext::new("barkd/wallet_ark_info_barkd").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;

	let expected = srv.ark_info().await;
	let info = barkd.ark_info().await;

	assert_eq!(info.network, expected.network);
	assert_eq!(info.server_pubkey, expected.server_pubkey);
	assert_eq!(info.mailbox_pubkey, expected.mailbox_pubkey);
	assert_eq!(info.round_interval, expected.round_interval);
	assert_eq!(info.nb_round_nonces, expected.nb_round_nonces);
	assert_eq!(info.vtxo_exit_delta, expected.vtxo_exit_delta);
	assert_eq!(info.vtxo_lifetime, expected.vtxo_lifetime);
}

/// Verify that `GET /wallet/next-round` returns a future timestamp.
#[tokio::test]
async fn wallet_next_round_barkd() {
	let ctx = TestContext::new("barkd/wallet_next_round_barkd").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;

	let next_round = barkd.next_round().await;
	let now = chrono::Local::now();

	assert!(
		next_round.start_time > now,
		"next round start_time {:?} should be in the future (now: {:?})",
		next_round.start_time, now,
	);
}

/// With `--no-auth`, unauthenticated requests are accepted.
#[tokio::test]
async fn no_auth_flag_allows_unauthenticated_requests() {
	let ctx = TestContext::new("barkd/no_auth_flag_allows_unauthenticated_requests").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv)
		.no_auth()
		.create().await;

	let unauthed = Configuration {
		base_path: barkd.base_url(),
		..Configuration::default()
	};
	wallet_api::wallet_exists(&unauthed).await
		.expect("unauthenticated request should succeed when --no-auth is set");
}

/// `--dangerously-allow-remote-no-auth` implies `--no-auth`, so it disables
/// authentication without `--no-auth` being passed as well.
#[tokio::test]
async fn dangerous_flag_alone_disables_auth() {
	let ctx = TestContext::new("barkd/dangerous_flag_alone_disables_auth").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv)
		.arg("--dangerously-allow-remote-no-auth")
		.create().await;

	let unauthed = Configuration {
		base_path: barkd.base_url(),
		..Configuration::default()
	};
	wallet_api::wallet_exists(&unauthed).await
		.expect("the dangerous flag alone should disable auth");
}

/// The flag that `no_auth_remote_bind_refuses_to_start` shows is required does
/// let barkd start and serve on a bind address other hosts can reach.
#[tokio::test]
async fn dangerous_flag_allows_remote_bind() {
	let ctx = TestContext::new("barkd/dangerous_flag_allows_remote_bind").await;

	let srv = ctx.captaind("server").create().await;
	// `create` panics if the daemon never answers, so reaching the request
	// below already proves the guard let a wildcard bind through.
	let barkd = ctx.barkd("barkd1", &srv)
		.arg("--host").arg("0.0.0.0")
		.arg("--dangerously-allow-remote-no-auth")
		.create().await;

	let unauthed = Configuration {
		base_path: barkd.base_url(),
		..Configuration::default()
	};
	wallet_api::wallet_exists(&unauthed).await
		.expect("a wildcard bind with the dangerous flag should serve unauthenticated requests");

	// The request above reaches barkd over loopback either way, so confirm the
	// socket really is the wildcard one and `--host` wasn't quietly dropped.
	let log = std::fs::read_to_string(ctx.datadir.join("barkd1").join("debug.log"))
		.expect("failed to read barkd debug log");
	assert!(
		log.contains("Server starting on http://0.0.0.0:"),
		"expected barkd to bind the wildcard address, log was: {}", log,
	);
}

/// Auth can only be disabled by the `--no-auth` flag: `BARKD_NO_AUTH` in the
/// environment must not weaken a daemon started without the flag.
#[tokio::test]
async fn no_auth_env_var_is_ignored() {
	let ctx = TestContext::new("barkd/no_auth_env_var_is_ignored").await;

	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv)
		.env("BARKD_NO_AUTH", "true")
		.create().await;

	let unauthed = Configuration {
		base_path: barkd.base_url(),
		..Configuration::default()
	};
	let err = match wallet_api::wallet_exists(&unauthed).await {
		Err(e) => e.to_string(),
		Ok(_) => panic!("BARKD_NO_AUTH must not disable auth"),
	};
	assert!(err.contains("401"), "expected 401, got: {}", err);
}

/// `--no-auth` on a bind address reachable from other hosts refuses to start;
/// exposing barkd that way takes `--dangerously-allow-remote-no-auth` instead.
#[tokio::test]
async fn no_auth_remote_bind_refuses_to_start() {
	let ctx = TestContext::new("barkd/no_auth_remote_bind_refuses_to_start").await;
	let datadir = ctx.datadir.join("barkd1");

	let out = Barkd::base_cmd()
		.args([
			"--datadir", datadir.to_str().unwrap(),
			"--host", "0.0.0.0",
			"--port", &pick_port().to_string(),
			"--no-auth",
		])
		.output().await
		.expect("failed to run barkd");

	assert!(!out.status.success(), "barkd should refuse to start");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("refusing to start"),
		"expected refusal on stderr, got: {}", stderr,
	);
}

/// The wallet-fallback web client uses native BIP84 recovery, not separate sweeps.
#[tokio::test]
async fn wallet_fallback_web_contract_barkd() {
	require_bark_version!(> "0.7.1");
	let ctx = TestContext::new("barkd/wallet_fallback_web_contract_barkd").await;
	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;
	let config = barkd.client_config();
	let url = |path: &str| format!("{}/api/v1{}", barkd.base_url(), path);

	let unauthed = reqwest::Client::new().get(url("/wallet/fallback-destination"))
		.timeout(Duration::from_secs(15)).send().await.unwrap();
	assert_eq!(unauthed.status(), reqwest::StatusCode::UNAUTHORIZED);

	let response = config.client.get(url("/wallet/fallback-destination"))
		.timeout(Duration::from_secs(15)).send().await.unwrap();
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	let destination: serde_json::Value = response.json().await.unwrap();
	let fields = destination.as_object().expect("fallback destination must be an object");
	assert_eq!(fields.len(), 2);
	let address: bitcoin::Address<bitcoin::address::NetworkUnchecked> =
		destination["address"].as_str().unwrap().parse().unwrap();
	let address = address.require_network(bitcoin::Network::Regtest).unwrap();
	assert_eq!(address.address_type(), Some(bitcoin::AddressType::P2wpkh));
	assert!(destination["seq"].as_u64().unwrap() <= 9_007_199_254_740_991);

	// Exercise both real routes in the same order as the client. No funds or
	// pending transactions are created in this fresh wallet.
	for path in ["/onchain/sync", "/wallet/sync"] {
		let response = config.client.post(url(path))
			.timeout(Duration::from_secs(15)).send().await.unwrap();
		assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
	}
	let response = config.client.post(url("/wallet/vtxos/adopt-server-status"))
		.json(&serde_json::json!({"vtxo_ids": []}))
		.timeout(Duration::from_secs(15)).send().await.unwrap();
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	assert_eq!(response.json::<serde_json::Value>().await.unwrap(), serde_json::json!([]));

	let response = config.client.post(url("/wallet/vtxos/adopt-server-status"))
		.json(&serde_json::json!({"vtxo_ids": ["not-a-vtxo-id"]}))
		.timeout(Duration::from_secs(15)).send().await.unwrap();
	assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
	let response = config.client.post(url("/wallet/vtxos/adopt-server-status"))
		.json(&serde_json::json!({"vtxos": []}))
		.timeout(Duration::from_secs(15)).send().await.unwrap();
	assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

	// The first old path matches the read-only /vtxos/{id} route, so its
	// unsupported POST is 405; the absent sweep path is 404.
	for (path, status) in [
		("/wallet/vtxos/expiry-payouts", reqwest::StatusCode::METHOD_NOT_ALLOWED),
		("/onchain/sweep-expiry-payouts", reqwest::StatusCode::NOT_FOUND),
	] {
		let response = config.client.post(url(path)).json(&serde_json::json!({}))
			.timeout(Duration::from_secs(15)).send().await.unwrap();
		assert_eq!(response.status(), status, "{path}");
	}

	let response = config.client.get(url("/wallet/fallback-destination"))
		.timeout(Duration::from_secs(15)).send().await.unwrap();
	assert_eq!(response.status(), reqwest::StatusCode::OK);
	assert_eq!(response.json::<serde_json::Value>().await.unwrap(), destination);
	assert_eq!(barkd.onchain_balance().await, bitcoin::Amount::ZERO);
	assert!(barkd.onchain_utxos().await.is_empty());
	assert!(barkd.onchain_transactions().await.is_empty());
	assert!(barkd.vtxos(Some(true)).await.is_empty());
}

/// Run the selected web source in a real browser against this fresh authenticated daemon.
/// This is an explicit fixture; a missing browser is a failure, never a skip/pass.
#[tokio::test]
#[ignore = "requires the admitted owned browser/web pairing runner"]
async fn wallet_fallback_web_browser_pairing_barkd() {
	require_bark_version!(> "0.7.1");
	let output = std::path::PathBuf::from(
		std::env::var("BARK_WEB_PAIR_CONTEXT").expect("BARK_WEB_PAIR_CONTEXT required")
	);
	assert!(output.is_absolute());
	let marker = output.with_extension("client-finished");
	assert!(!output.exists() && !marker.exists(), "fresh private handoff paths required");
	let ctx = TestContext::new("barkd/wallet_fallback_web_browser_pairing_barkd").await;
	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("barkd1", &srv).create().await;
	let cfg: bark::Config = toml::from_str(
		&std::fs::read_to_string(barkd.datadir().join("config.toml")).unwrap()
	).unwrap();
	assert!(cfg.bitcoind_address.is_some(), "owned Core fixture required");
	assert!(cfg.esplora_address.is_none(), "no external chain fixture");
	assert_eq!(barkd.onchain_balance().await, bitcoin::Amount::ZERO);
	assert!(barkd.vtxos(Some(true)).await.is_empty());
	let context = serde_json::json!({
		"schema_version": 1, "network": "regtest",
		"completion_marker": marker,
		"barkd_url": barkd.base_url(), "ark_server": cfg.server_address,
		"bitcoind_url": cfg.bitcoind_address.unwrap(),
		"bitcoind_cookie_file": cfg.bitcoind_cookiefile.unwrap(),
		"barkd_token_file": barkd.datadir().join("auth_token"),
		"test_name": "core::wallet_fallback_web_browser_pairing_barkd",
		"expected_onchain_sat": 0, "expected_ark_sat": 0,
	});
	let path = output;
	let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
		.mode(0o600).open(&path).unwrap();
	file.write_all(&serde_json::to_vec(&context).unwrap()).unwrap();
	file.sync_all().unwrap();
	// This marker only releases the fixture. It is never a browser/financial verdict;
	// the outer pinned runner independently checks the real browser result and exit.
	tokio::time::timeout(Duration::from_secs(240), async {
		while !marker.exists() {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("owned browser client did not release fixture within240s");
	assert_eq!(barkd.onchain_balance().await, bitcoin::Amount::ZERO);
	assert!(barkd.onchain_transactions().await.is_empty());
	assert!(barkd.onchain_utxos().await.is_empty());
	assert!(barkd.vtxos(Some(true)).await.is_empty());
}
