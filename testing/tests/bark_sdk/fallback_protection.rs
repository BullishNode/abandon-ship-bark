//! A real server admission decision is not evidence that the customer was paid.

use std::time::Duration;

use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
use bitcoin::Amount;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;

use ark::ProtocolEncoding;
use ark_testing::{sat, TestContext};

#[tokio::test]
async fn fallback_unfunded_decision_keeps_exit_material_until_actual_chain_payment() {
	let ctx = TestContext::new("bark_sdk/fallback_unfunded_protection").await;
	let srv = ctx.captaind("server").no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24))
		.watchmand().create().await;
	let wallet = ctx.bark_sdk("owner", &srv).cfg(|cfg| cfg.daemon_manual_sync = true)
		.boarded(sat(100_000)).create().await;
	let coin = wallet.spendable_vtxos().await.unwrap().remove(0);
	let original = wallet.get_full_vtxo(coin.id()).await.unwrap();
	let registration = wallet.fallback_destination().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(coin.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;

	// The watchman is deliberately unfunded. Actual background readiness and
	// allocation create the fence; the test writes no ledger or receipt rows.
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |tx| Ok(tx.query_opt(
				"SELECT e.id FROM expiry_entitlement e JOIN expiry_decided_coin d ON d.entitlement_id=e.id WHERE e.vtxo_id=$1",
				&[&coin.id().to_string()],
			).await?)).await.unwrap();
			if row.is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual background allocation never decided the native coin");
	wallet.sync().await;
	let client_db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	assert_eq!(client_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable,
		"the server's spent admission fence must not stop the client's exit protection");
	assert_eq!(wallet.get_full_vtxo(coin.id()).await.unwrap().serialize(), original.serialize());
	assert!(wallet.spendable_vtxos().await.unwrap().is_empty(), "protected is not available for new payments");
	assert!(wallet.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	let pending = wallet.history().await.unwrap();
	assert!(pending.iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout")
		.all(|movement| movement.status != bark::movement::MovementStatus::Successful));
	assert!(client_db.get_fallback_snapshot().await.unwrap().verified_payouts.is_empty());

	// A real subsidy now permits the existing unchanged payout to confirm.
	let address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let payout = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				let tx = ctx.bitcoind().await_transaction(txid).await;
				if tx.output.iter().any(|output| output.script_pubkey == registration.spk
					&& output.value == coin.amount() - sat(1_000)) { return txid; }
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual fallback did not broadcast after fee funding");
	let first_confirmation = ctx.generate_blocks(1).await;
	let confirmation_hash = ctx.bitcoind().sync_client().get_block_hash(first_confirmation.into()).unwrap();
	ctx.generate_blocks(5).await;
	wallet.chain().invalidate_caches().await;
	wallet.sync_fallback_payouts().await.unwrap();
	assert_eq!(client_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	let snapshot = client_db.get_fallback_snapshot().await.unwrap();
	assert!(snapshot.verified_payouts.values().any(|proof| proof.outpoint.txid == payout));
	assert!(!wallet.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert_eq!(wallet.balance().await.unwrap().spendable, Amount::ZERO);

	// No server RPC is needed to learn that the actual chain undid payment.
	srv.stop().await.unwrap();
	ctx.bitcoind().sync_client().invalidate_block(&confirmation_hash).unwrap();
	wallet.chain().invalidate_caches().await;
	assert!(wallet.sync_fallback_payouts().await.is_err());
	assert_eq!(client_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
	assert!(client_db.get_fallback_snapshot().await.unwrap().verified_payouts.is_empty());
	assert!(wallet.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert_eq!(wallet.get_full_vtxo(coin.id()).await.unwrap().serialize(), original.serialize());
}
