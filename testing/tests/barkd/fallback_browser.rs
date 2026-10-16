//! Opt-in browser window over the real native barkd and payout runtime.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use bitcoin::{Address, Network, OutPoint};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;

use ark_testing::{sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::barkd::Barkd;

/// The host browser acknowledges each screenshot window. The manifest
/// contains public evidence and a token-file path, never the bearer itself.
/// TestContext and Daemon retain their normal cleanup ownership on failure.
async fn browser_window(gate: &Path, phase: &str, barkd: &Barkd, evidence: serde_json::Value) {
	std::fs::create_dir_all(gate).unwrap();
	let ack = gate.join(format!("{}.ack", phase));
	if ack.exists() { std::fs::remove_file(&ack).unwrap(); }
	let manifest = serde_json::json!({
		"phase": phase, "base_url": barkd.base_url(),
		"auth_token_file": barkd.datadir().join("auth_token"), "evidence": evidence,
	});
	let temporary = gate.join("phase.tmp");
	std::fs::write(&temporary, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
	std::fs::rename(temporary, gate.join("phase.json")).unwrap();
	tokio::time::timeout(Duration::from_secs(60), async {
		while !ack.exists() { tokio::time::sleep(Duration::from_millis(100)).await; }
	}).await.expect("host browser did not acknowledge its bounded evidence window");
}

#[tokio::test]
#[ignore = "requires BIP84/web-enabled barkd and host browser acknowledgement"]
async fn fallback_barkd_web_actual_payout_and_seed_restore() {
	let gate = std::env::var_os("BARK_FALLBACK_BROWSER_GATE")
		.map(std::path::PathBuf::from).expect("set a private browser gate directory");
	let ctx = TestContext::new("barkd/fallback_web_actual_payout").await;
	let srv = ctx.captaind("server").no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24))
		.watchmand().create().await;
	let mnemonic = bip39::Mnemonic::from_entropy(&[43; 32]).unwrap().to_string();
	let barkd = ctx.barkd("wallet", &srv).mnemonic(mnemonic.clone())
		.cfg(|cfg| cfg.daemon_manual_sync = true).funded(sat(200_000)).create().await;
	let config = barkd.client_config();
	let registration: bark_json::web::FallbackRegistrationInfo = config.client
		.get(format!("{}/api/v1/wallet/fallback", config.base_path)).send().await.unwrap()
		.error_for_status().unwrap().json().await.unwrap();
	assert!(registration.registered);
	let address = Address::from_str(registration.payout_address.as_ref().unwrap()).unwrap()
		.require_network(Network::Regtest).unwrap();
	let script = address.script_pubkey();
	assert!(script.is_p2wpkh(), "the native test app must actually use BIP84");
	assert_ne!(address, barkd.onchain_address().await);
	barkd.onchain_sync().await;
	let board = barkd.board_amount(sat(100_000)).await;
	ctx.await_transaction(board.funding_tx.txid).await;
	let admitted_height = ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	srv.wait_for_sync_height(admitted_height).await;
	barkd.sync().await;
	let coins = barkd.vtxos(None).await;
	assert_eq!(coins.len(), 1);
	assert_eq!(coins[0].state, bark_json::primitives::VtxoStateInfo::Spendable);
	let coin = &coins[0];
	let expected = coin.vtxo.amount - sat(1_000);
	let returning_address = barkd.ark_address().await;
	browser_window(&gate, "registered", &barkd, serde_json::json!({
		"payout_address": address, "vtxo_id": coin.vtxo.id,
		"expiry_height": coin.vtxo.expiry_height, "payout_sat": expected.to_sat(),
	})).await;

	// Absence means the actual native process is stopped, including every
	// maintenance and sync task. No wallet observer opens it during expiry.
	barkd.stop().await.unwrap();
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(coin.vtxo.expiry_height.to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let (parent, index) = tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				let tx = ctx.bitcoind().await_transaction(txid).await;
				if let Some(index) = tx.output.iter().position(|output|
					output.script_pubkey == script && output.value == expected) {
					return (tx, index as u32);
				}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual background payout was not broadcast");
	let point = OutPoint::new(parent.compute_txid(), index);
	let confirmed = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	assert_eq!(ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true))
		.unwrap().unwrap().value, expected);
	barkd.start().await.unwrap();
	// Before any status adoption, a returning wallet may still cache the
	// old spendable coin. The actual decided server must explain its fence.
	let returning = barkd.client_config();
	let refusal = returning.client.post(format!("{}/api/v1/wallet/send", returning.base_path))
		.json(&serde_json::json!({ "destination": returning_address, "amount_sat": 1_000 }))
		.send().await.unwrap();
	assert_eq!(refusal.status(), reqwest::StatusCode::CONFLICT);
	let refusal: serde_json::Value = refusal.json().await.unwrap();
	assert_eq!(refusal["code"], "decided_for_fallback");
	assert_eq!(refusal["message"], "This coin expired and is being paid to your fallback address.");
	barkd.sync().await;
	barkd.onchain_sync().await;
	let history = barkd.history(None, None).await;
	let receipts = history.iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout").collect::<Vec<_>>();
	assert_eq!(receipts.len(), 1);
	assert_eq!(receipts[0].sent_to[0].amount, expected);
	assert_eq!(receipts[0].offchain_fee, sat(1_000));
	let evidence = serde_json::json!({
		"payout_address": address, "payout_outpoint": point,
		"payout_sat": expected.to_sat(), "movement": receipts[0],
	});
	browser_window(&gate, "payout", &barkd, evidence.clone()).await;
	barkd.stop().await.unwrap();

	let restored = ctx.barkd("seed_restored", &srv).mnemonic(mnemonic)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	restored.sync().await;
	restored.onchain_sync().await;
	let restored_history = restored.history(None, None).await;
	let restored_receipts = restored_history.iter().filter(|movement|
		movement.subsystem.name == "abandon.fallback_payout").collect::<Vec<_>>();
	assert_eq!(restored_receipts.len(), 1);
	assert_eq!(restored_receipts[0].sent_to[0].amount, expected);
	assert!(restored.onchain_utxos().await.iter().any(|utxo| utxo.outpoint == point));
	browser_window(&gate, "restored", &restored, evidence).await;
	restored.stop().await.unwrap();
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	std::fs::write(gate.join("complete.json"), b"{\"status\":\"PASS\"}\n").unwrap();
}
