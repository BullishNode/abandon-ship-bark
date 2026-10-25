use std::fs;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::process::Command;

use bark::BarkNetwork;
use bitcoin::Network;
use bitcoin_ext::BlockDelta;

use ark_testing::{Bark, TestContext, require_bark_version};
use ark_testing::constants::env::BARK_EXEC;
use ark_testing::util::{resolve_path, ToAltString};

#[tokio::test]
async fn cli_create_releases_lock_after_failure() {
	require_bark_version!(> "0.7.1");
	let bark_exec = resolve_path(std::env::var(BARK_EXEC).expect("BARK_EXEC env not set"))
		.expect("failed to resolve BARK_EXEC");
	let ctx = TestContext::new_minimal("bark/cli_create_releases_lock_after_failure").await;
	let datadir = ctx.datadir.join("new-wallet");
	assert!(!datadir.exists());
	for _ in 0..2 {
		let output = tokio::time::timeout(Duration::from_secs(30), Command::new(&bark_exec)
			.arg("--datadir").arg(&datadir).args(["--no-logfile", "create", "--regtest"])
			.kill_on_drop(true).output(),
		).await.expect("CLI creation hung").unwrap();
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(!output.status.success());
		assert!(stderr.contains("You need to provide a chain source"), "{stderr}");
		// Keep the stable lock filename, but no wallet data from the failed create.
		let remaining: Vec<_> = fs::read_dir(&datadir).unwrap()
			.map(|entry| entry.unwrap().file_name()).collect();
		assert_eq!(remaining, ["barkd.lock"]);
		let lock = fs::File::options().read(true).write(true).open(datadir.join("barkd.lock")).unwrap();
		lock.try_lock().expect("failed creation left the lifecycle lock held");
	}
}

#[tokio::test]
async fn cli_create_refuses_another_cli_create() {
	require_bark_version!(> "0.7.1");
	let bark_exec = resolve_path(std::env::var(BARK_EXEC).expect("BARK_EXEC env not set"))
		.expect("failed to resolve BARK_EXEC");
	let ctx = TestContext::new_minimal("bark/cli_create_refuses_another_cli_create").await;
	let datadir = ctx.datadir.join("new-wallet");
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let esplora = format!("http://{}", listener.local_addr().unwrap());
	let mut first = Command::new(&bark_exec).arg("--datadir").arg(&datadir)
		.args(["create", "--regtest", "--esplora", &esplora, "--ark", "http://127.0.0.1:1"])
		.stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
	// Hold the first creation inside a real chain-source request. This catches a
	// lock that is only checked at entry and dropped before creation completes.
	let (_request, _) = tokio::time::timeout(Duration::from_secs(30), listener.accept())
		.await.expect("first CLI did not reach its chain source").unwrap();
	let result = tokio::time::timeout(Duration::from_secs(30), Command::new(&bark_exec)
		.arg("--datadir").arg(&datadir)
		.args(["create", "--regtest", "--force", "--esplora", &esplora, "--ark", "http://127.0.0.1:1"])
		.kill_on_drop(true).output(),
	).await;
	first.kill().await.unwrap();
	let output = result.expect("second CLI creation hung").unwrap();
	let stderr = String::from_utf8_lossy(&output.stderr);
	assert!(!output.status.success());
	assert!(stderr.contains("another barkd is already running"), "{stderr}");
}

#[tokio::test]
async fn bark_create_is_atomic() {
	let ctx = TestContext::new("bark/bark_create_is_atomic").await;
	let srv = ctx.captaind("server").create().await;

	// Create a bark defines the folder
	let _  = ctx.bark("bark_ok", &srv).try_create().await.expect("Can create bark");
	assert!(ctx.datadir.join("bark_ok").is_dir());

	// You can't create a bark twice
	// If you want to overwrite the folder you need force
	let _ = ctx.bark("bark_twice", &srv).try_create().await.expect("Can create bark");
	assert!(ctx.datadir.join("bark_twice").is_dir());

	let _ = ctx.bark("bark_twice", &srv).try_create().await.expect_err("Can create bark");
	assert!(ctx.datadir.join("bark_twice").is_dir());

	// We stop the server
	// This ensures that clients cannot be created
	srv.stop().await.unwrap();
	let err = ctx.bark("bark_fails", &srv).try_create().await.unwrap_err();
	assert!(err.to_alt_string().contains("Failed to connect to provided server"), "{:?}", err);
	assert!(!ctx.datadir.join("bark_fails").is_dir());
}

#[tokio::test]
async fn bark_address_works_offline() {
	let ctx = TestContext::new("bark/bark_address_works_offline").await;
	let srv = ctx.captaind("server").create().await;
	let bark = ctx.bark("bark", &srv).create().await;

	let addr_with_server = bark.address().await;
	assert_eq!(addr_with_server, bark.address_at_idx(0).await,
		"the first receiving address must use child zero");

	srv.stop().await.unwrap();

	let addr_without_server = bark.address_at_idx(0).await;
	assert_eq!(addr_with_server, addr_without_server,
		"the same fresh address index must match with the server offline");

	// Derive a brand-new address with the server down.
	let new_addr_without_server = bark.address().await;
	assert_ne!(new_addr_without_server, addr_with_server,
		"new address should use a freshly derived key");
	assert_eq!(new_addr_without_server, bark.address_at_idx(1).await,
		"the next offline address must use child one");
}

#[tokio::test]
async fn bark_create_force_flag() {
	let ctx = TestContext::new("bark/bark_create_force_flag").await;
	let srv = ctx.captaind("server").create().await;

	// Stop the server to simulate unavailability
	srv.stop().await.unwrap();

	// Attempt to create with force_create should succeed
	let datadir = ctx.datadir.join("bark");
	let bitcoind = Arc::new(ctx.new_bitcoind("bark_bitcoind").await);
	let cfg = ctx.bark_default_cfg(&srv, Some(&bitcoind));
	Bark::try_new_with_create_opts(
		"bark", datadir, BarkNetwork::Regtest, cfg, Some(bitcoind), None, None, true, None,
	).await.unwrap();

	assert!(std::path::Path::is_dir(ctx.datadir.join("bark").as_path()));
}

/// Seed restoration must retain the configured refresh and exit margins.
#[tokio::test]
async fn cli_short_profile_create_restore_preserves_private_config() {
	let ctx = TestContext::new("bark/cli_short_profile_create_restore_preserves_private_config").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(12);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let executable = resolve_path(std::env::var(BARK_EXEC).expect("BARK_EXEC env not set")).unwrap();
	let mut config = ctx.bark_default_cfg(&srv, Some(ctx.bitcoind()));
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
	let mnemonic = bip39::Mnemonic::from_entropy(&[51; 32]).unwrap();
	let mut original = None;
	for name in ["created", "restored"] {
		let datadir = ctx.datadir.join(name);
		fs::create_dir(&datadir).unwrap();
		bark::fs_perms::harden(&datadir, 0o700).unwrap();
		let config_file = datadir.join("config.toml");
		bark::fs_perms::write_atomic_owner_only(&config_file, toml::to_string_pretty(&config).unwrap().as_bytes()).unwrap();
		let output = tokio::time::timeout(Duration::from_secs(30), Command::new(&executable)
			.arg("--datadir").arg(&datadir).args(["--no-logfile", "create", "--regtest", "--birthday-height", "0", "--mnemonic"])
			.arg(mnemonic.to_string()).kill_on_drop(true).output()).await.expect("native creation timed out").unwrap();
		assert!(output.status.success(), "native create/restore failed: {}", String::from_utf8_lossy(&output.stderr));
		let actual = bark::Config::load(Network::Regtest, &config_file).unwrap();
		assert_eq!(actual.vtxo_refresh_expiry_threshold, BlockDelta::new(4));
		assert_eq!(actual.vtxo_exit_margin, BlockDelta::new(2));
		assert_eq!(actual.server_address, config.server_address);
		let public = tokio::time::timeout(Duration::from_secs(30), Command::new(&executable)
			.arg("--datadir").arg(&datadir).args(["--no-logfile", "dev", "fallback-record"])
			.kill_on_drop(true).output()).await.expect("native authenticated record timed out").unwrap();
		assert!(public.status.success(), "native registration query failed: {}", String::from_utf8_lossy(&public.stderr));
		let record: serde_json::Value = serde_json::from_slice(&public.stdout).unwrap();
		assert!(record["record_hex"].as_str().is_some_and(|value| !value.is_empty()));
		if let Some(original) = &original { assert_eq!(original, &record, "seed-only restoration changed immutable binding"); }
		else { original = Some(record); }
	}
}
