//! A frozen recipient must still be paid when the sender refreshes its change.

use std::collections::BTreeMap;
use std::time::Duration;

use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::util::FutureExt;
use bitcoin::{Amount, Transaction, Txid};
use bitcoin::consensus::deserialize;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;

/// Native reproduction of public-Signet S03. Historic accepted-forfeit rows
/// remain present, but are not the current economic cut through the old root.
#[tokio::test]
async fn expiry_frozen_recipient_is_paid_after_sender_change_refresh() {
	let ctx = TestContext::new("server/expiry_transfer_refresh_recipient").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(btc(1)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(40);
			cfg.vtxo_exit_delta = BlockDelta::new(2);
			// Match the declared public-Signet board fee, rather than the zero-fee test default.
			cfg.fees.board.base_fee = sat(330);
		})
		.watchmand().create().await;
	let absent = ctx.bark("absent", &srv).funded(sat(500_000)).create().await;
	let control = ctx.bark("control", &srv).funded(sat(500_000)).create().await;
	// No intervening block: both actual backing roots have the same expiry.
	let own_board = absent.board(sat(30_000)).await;
	let sender_board = control.board(sat(30_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	absent.sync().await;
	control.sync().await;
	let owner = absent.client().await;
	let own = owner.get_full_vtxo(own_board.vtxos[0]).await.unwrap();
	let destination = owner.fallback_destination().await.unwrap();
	drop(owner);
	let sender = control.client().await;
	let original = sender.get_full_vtxo(sender_board.vtxos[0]).await.unwrap();
	drop(sender);
	assert_eq!(own.expiry_height(), original.expiry_height());
	assert_eq!(own.amount(), sat(29_670));
	assert_eq!(original.amount(), sat(29_670));
	control.send_oor(absent.address().await, sat(10_000)).await;
	absent.sync().await;
	let owner = absent.client().await;
	let held = owner.spendable_vtxos().await.unwrap();
	assert_eq!(held.len(), 2);
	let received = held.iter().find(|coin| coin.id() != own.id()).unwrap();
	assert_eq!(received.amount(), sat(10_000));
	let received_id = received.id();
	let received = owner.get_full_vtxo(received_id).await.unwrap();
	assert_eq!(received.chain_anchor(), original.chain_anchor());
	assert_eq!(received.expiry_height(), own.expiry_height());
	drop(owner);
	// The absent actor is never opened or synchronized again after this point.
	control.refresh_all_delegated_no_sync().await;
	let mut round_finished = srv.subscribe_log::<server_log::RoundFinished>();
	srv.trigger_round().await;
	round_finished.recv().wait(Duration::from_secs(30)).await.unwrap();
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	let controller = control.client().await;
	controller.sync_pending_rounds().await.unwrap();
	drop(controller);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let forfeit_rows = db.read(async |tx| Ok(tx.query("SELECT amount FROM vtxo \
		WHERE anchor_point=$1 AND spend_state='round-forfeit' ORDER BY vtxo_id", &[&original.chain_anchor().to_string()]).await?)).await.unwrap();
	assert_eq!(forfeit_rows.len(), 2, "the native refresh must really accept both change-input forfeits");
	assert!(forfeit_rows.iter().all(|row| row.get::<_, i64>("amount") == 9_835));
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(own.expiry_height().to_u32() + 1 - height).await;
	let ids = vec![own.id().to_string(), received_id.to_string()];
	let parents = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let rows = db.read(async |tx| Ok(tx.query("SELECT a.id,a.sweep_tx,e.vtxo_id FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=ANY($1) AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL", &[&ids]).await?)).await.unwrap();
			if rows.len() == 2 {
				assert_ne!(rows[0].get::<_, String>("vtxo_id"), rows[1].get::<_, String>("vtxo_id"));
				break rows.into_iter().map(|row| (row.get::<_, i64>("id"), deserialize::<Transaction>(row.get("sweep_tx")).unwrap()))
					.collect::<BTreeMap<_, _>>();
			}
			assert!(rows.len() <= 2, "one source must not have duplicate active fallback payments");
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both frozen customer coins must receive actual payouts despite the sender's historical forfeits");
	// Opportunistic ready grouping may pay one or two outputs. Each actual
	// output pays exactly the policy fee; no readiness delay hides a lost coin.
	let mut total = Amount::ZERO;
	let mut outputs = Vec::<(Txid, u32)>::new();
	for parent in parents.values() {
		assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, *parent);
		let matching = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
		assert_eq!(matching.len(), 1);
		total += matching[0].1.value;
		outputs.push((parent.compute_txid(), matching[0].0 as u32));
	}
	assert_eq!(total + sat(parents.len() as u64 * 1_000), own.amount() + received.amount());
	ctx.generate_blocks(6).await;
	for (txid, vout) in outputs {
		assert!(ctx.bitcoind().sync_client().get_tx_out(&txid, vout, Some(true)).unwrap().unwrap().confirmations >= 6);
	}
}
