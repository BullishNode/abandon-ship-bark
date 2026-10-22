use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark::{ProtocolEncoding, Vtxo, VtxoId};
use ark::vtxo::Full;
use bitcoin::Amount;
use bitcoin_ext::rpc::BitcoinRpcExt;
use bark_json::movements::MovementStatus;
use bark_json::primitives::VtxoStateInfo;
use server_log::RoundFinished;
use server_rpc::protos;
use bitcoin_ext::BlockDelta;

use ark_testing::{btc, require_bark_version, sat, Captaind, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::daemon::captaind::{self, ArkClient};
use ark_testing::util::FutureExt;

use super::helpers::{
	wait_for_boards_synced, wait_for_onchain_balance, wait_for_rounds_complete, wait_for_tip,
};

/// A captaind proxy that rejects round submissions containing a designated
/// "unusable" VTXO, mimicking the real server's response: `InvalidArgument`
/// carrying the offending id in the `identifiers` metadata. The id is shared so
/// a test can set it after boarding (before then the proxy passes everything
/// through). Lets us exercise how the client handles server-rejected refresh
/// inputs without having to genuinely corrupt server state.
#[derive(Clone)]
struct RejectVtxoProxy {
	bad: Arc<Mutex<Option<VtxoId>>>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for RejectVtxoProxy {
	async fn submit_payment(
		&self,
		upstream: &mut ArkClient,
		req: protos::SubmitPaymentRequest,
	) -> Result<protos::SubmitPaymentResponse, tonic::Status> {
		let bad = *self.bad.lock().unwrap();
		if let Some(bad) = bad {
			if req.input_vtxos.iter().any(|i| i.vtxo_id == bad.to_bytes().to_vec()) {
				let mut status = tonic::Status::invalid_argument(
					format!("input vtxo(s) not spendable: [{}]", bad),
				);
				status.metadata_mut().insert("identifiers", bad.to_string().parse().unwrap());
				return Err(status);
			}
		}
		Ok(upstream.submit_payment(req).await?.into_inner())
	}
}

/// Board two VTXOs and set up the daemon behind a proxy that rejects one of
/// them as unusable. Returns `(barkd, proxy, bad_id, good_id)` with the proxy
/// already poisoned; the caller must keep `proxy` alive (dropping it shuts the
/// proxy down). The bad VTXO is the smaller of the two.
async fn setup_barkd_with_rejected_vtxo(
	ctx: &TestContext,
	srv: &Captaind,
	refresh_threshold: Option<BlockDelta>,
) -> (
	ark_testing::daemon::barkd::Barkd,
	captaind::proxy::ArkRpcProxyServer,
	VtxoId,
	VtxoId,
) {
	let bad = Arc::new(Mutex::new(None::<VtxoId>));
	let proxy = srv.start_proxy_no_mailbox(RejectVtxoProxy { bad: bad.clone() }).await;
	let proxy_url = proxy.address.clone();

	let barkd = ctx.barkd("barkd", srv)
		.funded(sat(1_000_000))
		.cfg(move |c| {
			c.server_address = proxy_url;
			if let Some(threshold) = refresh_threshold { c.vtxo_refresh_expiry_threshold = threshold; }
		})
		.create().await;

	wait_for_onchain_balance(&barkd, sat(1_000_000)).await;

	// Two independent vtxos so we can poison one and still refresh the other.
	barkd.board_amount(sat(300_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wait_for_boards_synced(&barkd).await;
	barkd.board_amount(sat(400_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wait_for_boards_synced(&barkd).await;

	let vtxos = barkd.vtxos(None).await;
	assert_eq!(vtxos.len(), 2, "expected two boarded vtxos");
	let bad_id = vtxos.iter().min_by_key(|v| v.vtxo.amount).unwrap().vtxo.id;
	let good_id = vtxos.iter().max_by_key(|v| v.vtxo.amount).unwrap().vtxo.id;

	*bad.lock().unwrap() = Some(bad_id);

	(barkd, proxy, bad_id, good_id)
}

/// Derive the common early-refresh window from the signed boards and
/// current chain. Keep normal protection enabled and leave room to confirm
/// the cooperative replacement before any original coin's exit deadline.
async fn age_boards_for_maintenance(
	ctx: &TestContext,
	barkd: &ark_testing::daemon::barkd::Barkd,
	ids: &[VtxoId],
) -> u32 {
	let config: bark::Config = toml::from_str(&tokio::fs::read_to_string(
		barkd.datadir().join("config.toml"),
	).await.unwrap()).unwrap();
	let tip = u32::try_from(ctx.bitcoind().get_block_count().await).unwrap();
	let chain = ctx.bitcoind().sync_client();
	let mut ordinary_due = tip;
	let mut protection_refresh_by = u32::MAX;
	let mut act_by = u32::MAX;
	for id in ids {
		let coin = Vtxo::<Full>::deserialize_hex(
			&barkd.get_vtxo_encoded(&id.to_string()).await.encoded.0,
		).unwrap();
		assert_eq!(coin.id(), *id);
		assert!(coin.policy().as_pubkey().is_some());
		let funding = ctx.bitcoind().await_transaction(coin.chain_anchor().txid).await;
		coin.validate(&funding).unwrap();
		let mut path_blocks = 0u32;
		for step in coin.transactions() {
			let info = chain.custom_get_raw_transaction_info(step.tx.compute_txid(), None).unwrap();
			if info.is_some_and(|i| i.confirmations.unwrap_or(0) > 0) { continue; }
			let wait = step.tx.input.iter().filter(|i| i.sequence.is_relative_lock_time())
				.map(|i| { assert_eq!(i.sequence.0 & (1 << 22), 0); i.sequence.0 & 0xffff })
				.max().unwrap_or(1).max(1);
			path_blocks += wait;
		}
		let leaf = chain.custom_get_raw_transaction_info(coin.point().txid, None).unwrap();
		assert!(!leaf.is_some_and(|i| i.confirmations.unwrap_or(0) > 0), "the boarded leaf must not already be exited");
		let csv = coin.exit_delta().to_u32();
		let margin = config.vtxo_exit_margin.to_u32();
		let deadline = coin.expiry_height().to_u32().checked_sub(path_blocks + csv + 1 + margin).unwrap();
		ordinary_due = ordinary_due.max(coin.expiry_height().to_u32()
			.saturating_sub(config.vtxo_refresh_expiry_threshold.to_u32()) + 1);
		protection_refresh_by = protection_refresh_by.min(deadline.checked_sub(margin).unwrap());
		act_by = act_by.min(deadline);
	}
	assert!(ordinary_due + ROUND_CONFIRMATIONS < protection_refresh_by,
		"ordinary maintenance must confirm before early protection schedules an explicit refresh");
	assert!(protection_refresh_by < act_by);
	ctx.generate_blocks(ordinary_due - tip).await;
	wait_for_tip(barkd, u64::from(ordinary_due)).await;
	assert!(u32::try_from(ctx.bitcoind().get_block_count().await).unwrap() < protection_refresh_by);
	protection_refresh_by
}

/// The server has finished the round; confirm it without mining repeatedly
/// while the daemon catches up and consuming the originals' protection window.
async fn confirm_maintenance_round(
	ctx: &TestContext,
	barkd: &ark_testing::daemon::barkd::Barkd,
	protection_refresh_by: u32,
	funding_txid: bitcoin::Txid,
) {
	ctx.await_transaction(funding_txid).await;
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	let tip = ctx.bitcoind().get_block_count().await;
	assert!(tip < u64::from(protection_refresh_by));
	wait_for_tip(barkd, tip).await;
	async {
		while !barkd.pending_rounds().await.is_empty() {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}.wait(Duration::from_secs(60)).await;
}

/// Verify that the daemon's round-event loop automatically schedules a
/// maintenance refresh for VTXOs approaching expiry. Triggers a round on
/// the server and expects barkd to join it on its own (no REST refresh
/// call) and replace the expiring VTXO.
#[tokio::test]
async fn maintenance_refresh_auto_barkd() {
	require_bark_version!(> "0.5.0");

	let ctx = TestContext::new("barkd/maintenance_refresh_auto_barkd").await;

	// Short vtxo lifetime so the test doesn't have to mine 432 blocks
	// to age vtxos into the maintenance refresh window.
	let srv = ctx.captaind("server").funded(btc(1))
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(96))
		.create().await;
	let barkd = ctx.barkd("barkd1", &srv).funded(sat(100_000))
		.cfg(|c| c.vtxo_refresh_expiry_threshold = BlockDelta::new(64))
		.create().await;

	wait_for_onchain_balance(&barkd, sat(100_000)).await;
	barkd.board_all().await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wait_for_boards_synced(&barkd).await;

	let vtxos_before = barkd.vtxos(None).await;
	assert_eq!(vtxos_before.len(), 1, "should have one vtxo after boarding");
	let vtxo_id_before = vtxos_before[0].vtxo.id;

	let principal = vtxos_before[0].vtxo.amount;
	let protection_refresh_by = age_boards_for_maintenance(&ctx, &barkd, &[vtxo_id_before]).await;

	// Subscribe before triggering so we can't miss the log.
	let mut log_round_finished = srv.subscribe_log::<RoundFinished>();

	// Trigger a round; the daemon should see the Attempt event and
	// auto-join to refresh the expiring vtxo.
	srv.trigger_round().await;

	let finished = log_round_finished.recv().wait(Duration::from_secs(60)).await.unwrap();
	assert!(
		finished.nb_input_vtxos >= 1,
		"round should include at least one input vtxo from the auto refresh",
	);

	confirm_maintenance_round(&ctx, &barkd, protection_refresh_by, finished.txid).await;

	let vtxos_after = barkd.vtxos(None).await;
	assert_eq!(vtxos_after.len(), 1, "should still have one vtxo after refresh");
	assert_eq!(vtxos_after[0].state, VtxoStateInfo::Spendable);
	assert_eq!(vtxos_after[0].vtxo.amount, principal, "refresh must preserve boarded principal");
	let refreshes = barkd.history(None, None).await;
	let refreshed = refreshes.iter().find(|m| m.status == MovementStatus::Successful
		&& m.subsystem.name == "bark.round" && m.subsystem.kind == "refresh"
		&& m.input_vtxos == vec![vtxo_id_before]).expect("the original board must complete a refresh");
	assert_eq!(refreshed.output_vtxos, vec![vtxos_after[0].vtxo.id]);
	assert_eq!(refreshed.offchain_fee, Amount::ZERO);
	assert_ne!(
		vtxos_after[0].vtxo.id, vtxo_id_before,
		"vtxo id should change after maintenance refresh",
	);
}

/// An explicit, developer-chosen refresh must NOT silently drop an input the
/// server rejects: the whole round fails and nothing is refreshed. Only
/// maintenance is allowed to skip rejected inputs (see the test below).
#[tokio::test]
async fn manual_refresh_does_not_drop_rejected_vtxo_barkd() {
	require_bark_version!(> "0.5.0");

	let ctx = TestContext::new("barkd/manual_refresh_does_not_drop_rejected_vtxo_barkd").await;
	let srv = ctx.captaind("server").funded(btc(1)).create().await;

	let (barkd, _proxy, bad_id, good_id) = setup_barkd_with_rejected_vtxo(&ctx, &srv, None).await;
	barkd.refresh_vtxos(vec![bad_id.to_string(), good_id.to_string()]).await;
	srv.trigger_round().await;
	wait_for_rounds_complete(&ctx, &barkd).await;

	let ids = barkd.vtxos(None).await.into_iter().map(|v| v.vtxo.id).collect::<Vec<_>>();
	assert!(ids.contains(&bad_id), "rejected vtxo should remain after the failed refresh");
	assert!(
		ids.contains(&good_id),
		"healthy vtxo must NOT be silently refreshed when explicitly batched with a \
		rejected one; got {ids:?}",
	);
}

/// Non-delegated (interactive) maintenance must make forward progress around a
/// VTXO the server rejects as unusable.
#[tokio::test]
async fn maintenance_refresh_skips_rejected_vtxo_barkd() {
	require_bark_version!(> "0.5.0");

	let ctx = TestContext::new("barkd/maintenance_refresh_skips_rejected_vtxo_barkd").await;
	// Short vtxo lifetime so the test doesn't have to mine 432 blocks
	// to age vtxos into the maintenance refresh window.
	let srv = ctx.captaind("server").funded(btc(1))
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(96))
		.create().await;

	let (barkd, _proxy, bad_id, good_id) = setup_barkd_with_rejected_vtxo(&ctx, &srv, Some(BlockDelta::new(64))).await;

	let principal = barkd.vtxos(None).await.iter().map(|v| v.vtxo.amount).sum::<Amount>();
	let protection_refresh_by = age_boards_for_maintenance(&ctx, &barkd, &[bad_id, good_id]).await;

	// A SINGLE round must refresh the healthy vtxo: the daemon submits
	// [bad, good], the server rejects `bad`, and the daemon re-submits just
	// `good` to the SAME round (the submit window stays open after a rejection
	// and the in-flight attempt is replayed to the new participation). We trigger
	// exactly one round, then only mine blocks to confirm its funding tx — we
	// never trigger a second round, so a pass proves same-round recovery.
	let mut log_round_finished = srv.subscribe_log::<RoundFinished>();
	srv.trigger_round().await;
	let finished = log_round_finished.recv().wait(Duration::from_secs(30)).await.unwrap();
	assert_eq!(finished.nb_input_vtxos, 1, "only the healthy input may complete this round");
	confirm_maintenance_round(&ctx, &barkd, protection_refresh_by, finished.txid).await;

	let final_vtxos = barkd.vtxos(None).await;
	assert!(final_vtxos.iter().all(|v| v.state == VtxoStateInfo::Spendable));
	assert_eq!(final_vtxos.iter().map(|v| v.vtxo.amount).sum::<Amount>(), principal, "refresh must preserve both boards' principal");
	let final_ids = final_vtxos.iter().map(|v| v.vtxo.id).collect::<Vec<_>>();
	assert!(
		final_ids.contains(&bad_id),
		"the rejected vtxo should be left untouched (still spendable locally); got {final_ids:?}",
	);
	assert_eq!(final_ids.len(), 2, "should have the old VTXO and a new one");
	assert!(!final_ids.contains(&good_id), "the good vtxo should have been refreshed");

	// The recovery is recorded as two distinct refresh movements — a failed
	// attempt that included the rejected input, and a successful one that
	// dropped it — rather than a single movement mutated in place.
	let refreshes = barkd.history(None, None).await.into_iter()
		.filter(|m| m.subsystem.name == "bark.round" && m.subsystem.kind == "refresh")
		.collect::<Vec<_>>();

	let failed = refreshes.iter()
		.find(|m| m.status == MovementStatus::Failed)
		.expect("expected a failed refresh movement for the rejected batch");
	assert!(
		failed.input_vtxos.contains(&bad_id) && failed.input_vtxos.contains(&good_id),
		"the failed refresh movement should have attempted both inputs; got {:?}",
		failed.input_vtxos,
	);

	let succeeded = refreshes.iter()
		.find(|m| m.status == MovementStatus::Successful)
		.expect("expected a successful refresh movement for the healthy input");
	assert!(
		succeeded.input_vtxos.contains(&good_id) && !succeeded.input_vtxos.contains(&bad_id),
		"the successful refresh movement should have dropped the rejected input; got {:?}",
		succeeded.input_vtxos,
	);

	assert_eq!(succeeded.output_vtxos.len(), 1);
	assert!(final_ids.contains(&succeeded.output_vtxos[0]));
	assert!(![bad_id, good_id].contains(&succeeded.output_vtxos[0]));
	assert_eq!(succeeded.offchain_fee, Amount::ZERO);
	assert_ne!(failed.id, succeeded.id, "the two refreshes should be distinct movements");
	assert_ne!(
		failed.input_vtxos, succeeded.input_vtxos,
		"the failed and successful refresh movements should have different inputs",
	);
}
