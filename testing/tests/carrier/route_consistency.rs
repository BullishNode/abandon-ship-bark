//! Actual daemon startup refusal before another route can freeze backing.

use std::time::Duration;

use ark_testing::{btc, sat, TestContext};
use bitcoincore_rpc::RpcApi;

use super::{automatic_board_fixture, spawn_automatic_daemon, write_automatic_config};

#[tokio::test]
async fn direct_captaind_refuses_already_running_legacy_watchmand() {
	let ctx = TestContext::new("route/legacy_watchmand_first").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(btc(1)).cfg(|c| {
			c.round_interval = Duration::from_secs(3600);
			c.fees.board.base_fee = sat(1_000);
		}).watchmand_cfg(|c| {
			c.watchman.reaction_interval = Duration::from_secs(3600);
			c.watchman.sweep_interval = Duration::from_secs(3600);
		}).create().await;
	let wallet = ctx.bark_sdk("owner", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(21_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coins = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(coins.len(), 1);
	let coin = wallet.get_full_vtxo(coins[0].id()).await.unwrap();
	drop(wallet);
	// The actual legacy watchmand remains alive while the direct producer starts.
	srv.stop().await.unwrap();
	let mut cfg = srv.config().clone();
	cfg.expiry_payout.enabled = false;
	cfg.direct_expiry.enabled = true;
	let path = ctx.datadir.join("mismatched-captaind.toml");
	write_automatic_config(&path, &toml::to_string_pretty(&cfg).unwrap());
	let log = ctx.datadir.join("mismatched-captaind.log");
	let mut child = spawn_automatic_daemon("CAPTAIND_EXEC", &path, log.clone());
	let status = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
	if status.is_err() {
		child.kill().await.unwrap();
		child.wait().await.unwrap();
	}
	let core = ctx.bitcoind().sync_client();
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(true)).unwrap().is_some(),
		"negative startup proof must preserve the genuine Board backing");
	let status = status.expect("direct captaind must refuse an already-running legacy route before starting workers").unwrap();
	assert!(!status.success(), "a mismatch must refuse, not quietly stop successfully");
	assert!(std::fs::read_to_string(log).unwrap().contains("expiry settlement route mismatch"),
		"an unrelated setup failure is not route arbitration evidence");
}

#[tokio::test]
async fn legacy_watchmand_refuses_already_running_direct_captaind() {
	let fixture = automatic_board_fixture("route_direct_captaind_first").await;
	let ctx = &fixture.ctx;
	let srv = &fixture.srv;
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let empty = db.read(async |t| Ok(t.query_one("SELECT NOT EXISTS (SELECT 1 FROM expiry_allocation)
		AND NOT EXISTS (SELECT 1 FROM expiry_carrier)", &[]).await?.get::<_, bool>(0))).await.unwrap();
	assert!(empty, "the immutable route must protect backing even before the first freeze");
	srv.watchmand().stop().await.unwrap();
	let mut watch = srv.watchmand().config().clone();
	watch.watchman.direct_expiry = None;
	let path = ctx.datadir.join("mismatched-watchmand.toml");
	write_automatic_config(&path, &toml::to_string_pretty(&watch).unwrap());
	let log = ctx.datadir.join("mismatched-watchmand.log");
	let mut child = spawn_automatic_daemon("WATCHMAND_EXEC", &path, log.clone());
	let status = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
	if status.is_err() {
		child.kill().await.unwrap();
		child.wait().await.unwrap();
	}
	let core = ctx.bitcoind().sync_client();
	assert!(core.get_tx_out(&fixture.coin.chain_anchor().txid, fixture.coin.chain_anchor().vout, Some(true)).unwrap().is_some(),
		"a refused legacy route must leave the real Board backing unspent");
	let status = status.expect("legacy watchmand must refuse the direct route before starting workers").unwrap();
	assert!(!status.success());
	assert!(std::fs::read_to_string(log).unwrap().contains("expiry settlement route mismatch"),
		"an unrelated witness or configuration failure is not route arbitration evidence");
}
