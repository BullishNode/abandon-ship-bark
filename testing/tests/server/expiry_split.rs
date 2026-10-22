use std::time::Duration;

use ark::arkoor::{ArkoorDestination, package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse}};
use ark::lightning::PaymentHash;
use ark::VtxoPolicy;
use ark_testing::{sat, TestContext};
use bitcoin::consensus::deserialize;
use bitcoin::Transaction;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server_rpc::protos;

async fn payout(db: &Db, id: ark::VtxoId) -> Transaction {
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt(
				"
					SELECT a.sweep_tx
					FROM expiry_allocation a
					JOIN expiry_allocation_coin c ON c.allocation_id=a.id
					JOIN expiry_entitlement e ON e.id=c.entitlement_id
					WHERE e.vtxo_id=$1
					AND a.broadcast_at IS NOT NULL
				",
				&[&id.to_string()],
			).await?)).await.unwrap();
			if let Some(row) = row { return deserialize(row.get("sweep_tx")).unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("native watchman did not publish this sibling's payout")
}

/// Exercise the split API on a registered mixed tree, not manufactured
/// vtxo/transaction rows. The test requests the first split explicitly to cover
/// its SQL checks; both daemon progression and subsequent payouts are automatic.
#[tokio::test]
async fn expiry_requested_split_preserves_mixed_siblings_and_eventual_refund() {
	let ctx = TestContext::new("server/expiry_requested_mixed_split").await;
	let srv = ctx.captaind("server").no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24)).watchmand().create().await;
	let wallet = ctx.bark("abandoned", &srv).funded(sat(200_000)).create().await;
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let client = wallet.client().await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	let key = client.get_vtxo_key(&input).await.unwrap();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input.clone()], vec![
		ArkoorDestination { total_amount: sat(50_000), policy: VtxoPolicy::new_pubkey(key.public_key()) },
		ArkoorDestination { total_amount: sat(50_000), policy: VtxoPolicy::new_server_htlc_send(key.public_key(), PaymentHash::from([19; 32]), deadline) },
	]).unwrap().generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let package = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap();
	let checkpoint = package.signed_virtual_transactions().into_iter()
		.find(|tx| tx.input[0].previous_output == input.point()).unwrap();
	let leaves = package.build_signed_vtxos();
	let ordinary = leaves.iter().find(|v| matches!(v.policy(), VtxoPolicy::Pubkey(_))).unwrap().id();
	let htlc = leaves.iter().find(|v| matches!(v.policy(), VtxoPolicy::ServerHtlcSend(_))).unwrap().id();
	client.register_vtxo_transactions_with_server(&leaves).await.unwrap();
	drop(client);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(input.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let root = db.read(async |t| {
		let snapshot = t.backing_snapshot(&[ark::ServerVtxo::from(input.clone()).to_bare()],
			server::database::expiry_inventory::BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 10_000_000 }).await?;
		Ok(snapshot.regular.into_iter().find(|f| f.state.vtxo.point() == input.chain_anchor()).unwrap().state.vtxo)
	}).await.unwrap();
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let result = db.write(async |t| {
				let Some(view) = t.expiry_rebuild_token().await? else { return Ok(false); };
				if !t.expiry_view_is_rebuilt(view).await? { return Ok(false); }
				t.request_expiry_split(view, &root).await
			}).await.unwrap();
			if result { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual signed checkpoint paths should authorize this split");
	ctx.bitcoind().await_transaction(input.point().txid).await;
	// Confirm the board exit, then allow the configured watchtower grace
	// period to pass. Waiting in wall-clock time cannot advance this policy.
	ctx.generate_blocks(1 + srv.watchmand().config().watchman.progress_grace_period.to_u32()).await;
	ctx.bitcoind().await_transaction(checkpoint.compute_txid()).await;
	let progressed = ctx.generate_blocks(1).await;
	srv.wait_for_sync_height(progressed).await;
	srv.watchmand().wait_for_sync_height(progressed).await;
	let first = payout(&db, ordinary).await;
	assert_eq!(ctx.bitcoind().await_transaction(first.compute_txid()).await, first);
	let backing = first.input.iter().map(|i| ark::VtxoId::from(i.previous_output)).collect::<Vec<_>>();
	let mut backing_vtxos = Vec::new();
	for id in &backing {
		backing_vtxos.push(db.read(async |t| t.get_bare_vtxo_by_id(*id).await).await.unwrap().vtxo);
	}
	let reserved = db.write(async |t| t.lock_unallocated_watchman_inputs(&backing_vtxos, false).await).await
		.expect_err("normal watchman progression must respect the actual payout's physical backing reservation");
	let refusal = format!("{reserved:#}");
	assert!(backing.iter().any(|id| refusal.contains(&format!("vtxo {id} is reserved for expiry recovery"))),
		"{refusal}");
	assert_eq!(first.output.iter().find(|o| o.script_pubkey == destination.spk).unwrap().value, sat(49_000));
	let waiting = db.read(async |t| Ok(t.query_one("
		SELECT COUNT(*)
		FROM expiry_entitlement
		WHERE vtxo_id=$1
	", &[&htlc.to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(waiting, 0, "ordinary payout must not invent a terminal HTLC resolution");
	assert!((ctx.bitcoind().get_block_count().await as u32) < deadline.to_u32());
	ctx.generate_blocks(6).await;
	let output_index = first.output.iter().position(|o| o.script_pubkey == destination.spk).unwrap() as u32;
	assert!(ctx.bitcoind().sync_client().get_tx_out(&first.compute_txid(), output_index, Some(true)).unwrap().unwrap().confirmations >= 6);

	// This registered send was never initiated. Once its refund deadline
	// passes, the retained checkpoint can pay it without a user signature or
	// a fabricated upstream off-chain Revoked state.
	let height = ctx.bitcoind().get_block_count().await as u32;
	let final_tip = ctx.generate_blocks(deadline.to_u32() + 1 - height).await;
	srv.wait_for_sync_height(final_tip).await;
	srv.watchmand().wait_for_sync_height(final_tip).await;
	let second = payout(&db, htlc).await;
	assert_ne!(first.compute_txid(), second.compute_txid());
	assert_eq!(ctx.bitcoind().await_transaction(second.compute_txid()).await, second);
	assert_eq!(second.output.iter().find(|o| o.script_pubkey == destination.spk).unwrap().value, sat(50_000));
	assert_eq!(first.output.iter().filter(|o| o.script_pubkey == destination.spk).map(|o| o.value).sum::<bitcoin::Amount>()
		+ second.output.iter().filter(|o| o.script_pubkey == destination.spk).map(|o| o.value).sum::<bitcoin::Amount>(), sat(99_000),
		"both 50,000 sat siblings pay the 1,000 sat floor once at their original expiry height");
	let upstream = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, htlc).await).await.unwrap().unwrap();
	assert_eq!(upstream.htlc.resolution(), None);
	assert!(first.input.iter().all(|a| second.input.iter().all(|b| a.previous_output != b.previous_output)));
	ctx.generate_blocks(6).await;
	let output_index = second.output.iter().position(|o| o.script_pubkey == destination.spk).unwrap() as u32;
	assert!(ctx.bitcoind().sync_client().get_tx_out(&second.compute_txid(), output_index, Some(true)).unwrap().unwrap().confirmations >= 6);
}
