use std::time::Duration;

use bitcoin::{Address, Network};
use bitcoin_ext::BlockDelta;

use ark::VtxoId;
use ark_testing::{TestContext, btc, sat};
use ark_testing::constants::ROUND_CONFIRMATIONS;
use ark_testing::daemon::captaind::Captaind;
use bark::expiry_payout::{self, AdoptedVtxoState, EXPIRY_PAYOUT_MOVEMENT_KIND, EXPIRY_PAYOUT_SUBSYSTEM, expiry_payout_script};
use bark::movement::MovementStatus;
use bark::vtxo::VtxoStateKind;
use server::database::Db;

/// Mark `vtxo` spent in the server's database, like an operator that paid it
/// out on-chain does.
async fn server_marks_spent(srv: &Captaind, vtxo: VtxoId) {
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let n = db.write(async |t| {
		Ok(t.execute(
			"UPDATE vtxo SET spend_state = 'spent', updated_at = NOW() WHERE vtxo_id = $1",
			&[&vtxo.to_string()],
		).await?)
	}).await.unwrap();
	assert_eq!(n, 1, "server should know vtxo {vtxo}");
}

/// The server pays an expired VTXO out on-chain to `tr(user_pubkey)` and marks
/// it spent. The wallet, which was never told:
/// - still refreshes its other VTXOs, dropping the paid one;
/// - adopts the spent state, so the VTXO leaves the balance;
/// - finds the payout and sweeps it into its on-chain wallet.
#[tokio::test]
async fn expiry_payout_adopt_find_sweep() {
	let ctx = TestContext::new("bark_sdk/expiry_payout_adopt_find_sweep").await;
	let srv = ctx.captaind("server").funded(btc(1))
		.cfg(|c| c.vtxo_lifetime = BlockDelta::new(32))
		.create().await;

	let wallet = ctx.bark_sdk("bark", &srv)
		.cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(300_000))
		.boarded(sat(400_000))
		.create().await;

	let vtxos = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(vtxos.len(), 2);
	let paid = vtxos.iter().min_by_key(|v| v.amount()).unwrap().vtxo.clone();
	let kept = vtxos.iter().max_by_key(|v| v.amount()).unwrap().id();

	// The server settles `paid`. An explicit refresh of both must still
	// refresh `kept`.
	server_marks_spent(&srv, paid.id()).await;
	let (res, _) = tokio::join!(
		wallet.refresh_vtxos(vec![paid.id(), kept]),
		async {
			tokio::time::sleep(Duration::from_secs(2)).await;
			srv.trigger_round().await;
		},
	);
	res.expect("refresh should retry without the paid vtxo").expect("a round happened");
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.sync().await;
	let ids = wallet.spendable_vtxos().await.unwrap().into_iter().map(|v| v.id()).collect::<Vec<_>>();
	assert!(!ids.contains(&kept), "kept vtxo should have been refreshed: {ids:?}");
	assert!(ids.contains(&paid.id()), "the wallet does not know yet that `paid` is gone");

	// Let `paid` expire, but not the refreshed vtxo, and pay `paid` out
	// on-chain, as the operator does.
	let tip = wallet.chain().tip().await.unwrap();
	ctx.generate_blocks(paid.expiry_height().checked_blocks_since(tip).unwrap()).await;
	wallet.sync().await;
	let payout_addr = Address::from_script(
		&expiry_payout_script(paid.user_pubkey()), Network::Regtest,
	).unwrap();
	let payout_amount = paid.amount() - sat(1_000);
	let payout_txid = ctx.bitcoind().fund_addr(&payout_addr, payout_amount).await;
	ctx.generate_blocks(1).await;

	// Adopt: only expired, unspent vtxos are checked by default.
	let balance_before = wallet.balance().await.unwrap().total();
	let adopted = expiry_payout::adopt_server_vtxo_status(&wallet, None).await.unwrap();
	assert_eq!(adopted.len(), 1, "{adopted:?}");
	assert_eq!(adopted[0].vtxo_id, paid.id());
	assert_eq!(adopted[0].state, AdoptedVtxoState::Spent);
	assert_eq!(wallet.get_vtxo_by_id(paid.id()).await.unwrap().state.kind(), VtxoStateKind::Spent);
	assert_eq!(wallet.balance().await.unwrap().total(), balance_before - paid.amount());

	let payouts = expiry_payout::find_expiry_payouts(&wallet, None).await.unwrap();
	assert_eq!(payouts.len(), 1, "{payouts:?}");
	assert_eq!(payouts[0].vtxo_id, paid.id());
	assert_eq!(payouts[0].outpoint.txid, payout_txid);
	assert_eq!(payouts[0].amount, payout_amount);
	assert_eq!(payouts[0].confirmations, 1);

	let onchain = wallet.onchain().unwrap();
	let onchain_before = onchain.read().await.balance().await;
	let sweep = expiry_payout::sweep_expiry_payouts(&wallet, None).await.unwrap();
	assert!(sweep.swept < payout_amount && sweep.swept > payout_amount - sat(1_000));
	ctx.await_transaction(sweep.txid).await;
	ctx.generate_blocks(1).await;
	wallet.sync_onchain().await.unwrap();
	assert_eq!(onchain.read().await.balance().await, onchain_before + sweep.swept);
	assert!(expiry_payout::find_expiry_payouts(&wallet, None).await.unwrap().is_empty(), "payout was swept");

	let movement = wallet.history().await.unwrap().into_iter()
		.find(|m| m.subsystem.name == EXPIRY_PAYOUT_SUBSYSTEM.as_name())
		.expect("an expiry payout movement");
	assert_eq!(movement.subsystem.kind, EXPIRY_PAYOUT_MOVEMENT_KIND);
	assert_eq!(movement.status, MovementStatus::Successful);
	assert!(movement.input_vtxos.contains(&paid.id()));
	let metadata = &movement.metadata;
	assert_eq!(metadata["sweep_txid"], serde_json::to_value(sweep.txid).unwrap());
	assert_eq!(metadata["payout_txids"], serde_json::to_value([payout_txid]).unwrap());
}
