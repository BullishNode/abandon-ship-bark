//! Released daemon payout discovery and seed restoration.

use std::str::FromStr;
use std::time::Duration;

use bitcoin::{Address, Network, OutPoint};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;

use ark_testing::{sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;

fn assert_no_ark_principal(balance: bark_json::cli::Balance) {
	assert_eq!([
		balance.spendable, balance.needs_refresh, balance.awaiting_settlement,
		balance.pending_board, balance.pending_in_round, balance.pending_arkoor_send,
		balance.pending_offboard, balance.pending_lightning_send,
		balance.claimable_lightning_receive, balance.pending_exit,
	], [bitcoin::Amount::ZERO; 10]);
}

#[tokio::test]
async fn fallback_payout_history_and_seed_restore() {
	let ctx = TestContext::new("barkd/fallback_payout_history_and_seed_restore").await;
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
	assert!(script.is_p2tr(), "the released daemon must use its native BIP86 wallet");
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

	// Absence means the native process is stopped, including every
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
	let paid = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	assert_eq!(paid.value, expected);
	assert!(paid.confirmations >= 6);
	assert_eq!(parent.output[index as usize].script_pubkey, script);
	barkd.start().await.unwrap();
	// Before any status adoption, a returning wallet may still cache the
	// old spendable coin. The decided server must explain its fence.
	let returning = barkd.client_config();
	let refusal = returning.client.post(format!("{}/api/v1/wallet/send", returning.base_path))
		.json(&serde_json::json!({ "destination": returning_address, "amount_sat": 1_000 }))
		.send().await.unwrap();
	assert_eq!(refusal.status(), reqwest::StatusCode::CONFLICT);
	let refusal: serde_json::Value = refusal.json().await.unwrap();
	assert_eq!(refusal["code"], "decided_for_fallback");
	assert_eq!(refusal["message"], "This coin expired and is being paid to your fallback address.");
	// The returning wallet retires the retained source after independently
	// observing the confirmed payment through its own chain backend.
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			barkd.sync().await;
			if barkd.get_vtxo(&coin.vtxo.id.to_string()).await.state == bark_json::primitives::VtxoStateInfo::Spent { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the native source retirement did not catch up with Core");
	barkd.onchain_sync().await;
	let utxos = barkd.onchain_utxos().await;
	let payout = utxos.iter().find(|utxo| utxo.outpoint == point).expect("returning wallet must discover its payout");
	assert_eq!(payout.amount, expected);
	assert!(payout.confirmation_height.is_some());
	assert_no_ark_principal(barkd.bark_balance().await);

	barkd.stop().await.unwrap();

	let restored = ctx.barkd("seed_restored", &srv).mnemonic(mnemonic)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	restored.sync().await;
	restored.onchain_sync().await;
	let config = restored.client_config();
	let restored_registration: bark_json::web::FallbackRegistrationInfo = config.client
		.get(format!("{}/api/v1/wallet/fallback", config.base_path)).send().await.unwrap()
		.error_for_status().unwrap().json().await.unwrap();
	assert!(restored_registration.registered);
	assert_eq!(restored_registration.payout_address, registration.payout_address);
	let utxos = restored.onchain_utxos().await;
	let payout = utxos.iter().find(|utxo| utxo.outpoint == point).expect("seed-restored wallet must discover the same payout");
	assert_eq!(payout.amount, expected);
	assert!(payout.confirmation_height.is_some());
	assert_no_ark_principal(restored.bark_balance().await);
	restored.stop().await.unwrap();
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
}
