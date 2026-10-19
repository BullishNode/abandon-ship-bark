use std::time::Duration;

use ark::arkoor::package::ArkoorPackageBuilder;
use ark::lightning::{Invoice, PaymentHash, Preimage};
use ark::VtxoPolicy;
use bitcoin_ext::{BlockDelta, BlockHeight};
use server::database::ln::LightningPaymentStatus;
use server_rpc::protos;
use cln_rpc::plugins::hold;

use ark_testing::{btc, sat, TestContext};
use ark_testing::util::{FutureExt, poll_interval};


/// Exercise the actual pre-dispatch database boundary with a natively
/// registered signed HTLC. The external xpay task is deliberately not run:
/// this proves admission fencing, not a complete payment-vs-payout RPC race.
#[tokio::test]
async fn expiry_dispatch_rechecks_registered_sources_and_closed_attempts() {
	let ctx = TestContext::new("server/expiry_dispatch_execution_boundary").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24))
		.watchmand_cfg(|cfg| cfg.watchman.sweep_interval = std::time::Duration::from_secs(3600))
		.create().await;
	let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let wallet = sender.client().await;
	let input = wallet.get_full_vtxo(ids[0]).await.unwrap();
	let key = wallet.get_vtxo_key(&input).await.unwrap();
	let preimage_bytes = [12;32];
	let invoice = lightning.external.invoice_with_preimage(Some(sat(100_000)), "dispatch-boundary", "Registered source dispatch fence", preimage_bytes).await;
	let parsed: bark::lightning_invoice::Bolt11Invoice = invoice.parse().unwrap();
	let hash = PaymentHash::from(&parsed);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		[input.clone()], VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline),
	).unwrap().generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let response = ark::arkoor::package::ArkoorPackageCosignResponse::try_from(response).unwrap();
	let htlcs = builder.user_cosign(&[key], response).unwrap().build_signed_vtxos();
	wallet.register_vtxo_transactions_with_server(&htlcs).await.unwrap();
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let htlc = &htlcs[0];
	let invoice = Invoice::Bolt11(parsed);
	let node_id = db.read(async |t| Ok(t.query_one("
		SELECT id
		FROM lightning_node
		ORDER BY id
		LIMIT 1
	", &[]).await?.get::<_, i64>("id"))).await.unwrap();
	let attempt = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, sat(100_000), None,
			&[htlc.id()], None, tip, sat(0), None, Duration::from_secs(1)).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else( || anyhow::anyhow!("attempt disappeared"))
	}).await.unwrap();
	assert!(db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap());
	let separate_dispatcher = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	assert!(!separate_dispatcher.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap(),
		"a separate connection cannot dispatch the same durable attempt twice");
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&attempt,
		LightningPaymentStatus::Failed, Some("closed before queued dispatch")).await).await.unwrap().is_some());
	let replacement = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, sat(100_000), None,
			&[htlc.id()], None, tip, sat(0), None, Duration::from_secs(1)).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else( || anyhow::anyhow!("replacement disappeared"))
	}).await.unwrap();
	assert_ne!(replacement.id, attempt.id);
	assert!(!db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap(),
		"an obsolete queued task cannot dispatch a replacement attempt");
	assert!(db.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap());
	let expired = ctx.generate_blocks(input.expiry_height().to_u32() + 1 - tip.to_u32()).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let error = db.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap_err();
	let message = format!("{error:#}");
	assert!(message.contains("expired backing") && message.contains(&htlc.id().to_string()), "{message}");
	assert_eq!(db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().id,
		replacement.id, "refused dispatch does not create or replace an attempt");
}

/// A real external receiver settles after the coarse attempt row was made
/// stale. The actual preimage must still reach the monetary settlement WAL
/// and the public payment status must reflect it.
#[tokio::test]
async fn expiry_xpay_preserves_late_preimage_after_coarse_failure() {
	let ctx = TestContext::new("server/expiry_xpay_late_preimage").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	let wallet = ctx.bark_sdk("sender", &srv).boarded(btc(2)).create().await;
	lightning.sync().await;
	let preimage = Preimage::random();
	let hash = preimage.compute_payment_hash();
	let mut receiver = lightning.external.hold_client().await;
	let invoice = receiver.invoice(hold::InvoiceRequest {
		payment_hash: hash.to_vec(), amount_msat: 100_000_000,
		description: Some(hold::invoice_request::Description::Memo("late-preimage-after-stale-status".into())),
		min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	wallet.pay_lightning_invoice(invoice, None, false).await.unwrap();
	lightning.external.wait_for_hold_invoice_accepted(hash).await;
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let attempt = db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
	// Inject the stale status at the actual database writer; neither the
	// receiver's HTLC nor the real xpay call is synthesized or cancelled.
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&attempt,
		LightningPaymentStatus::Failed, Some("injected stale coarse reconciliation")).await).await.unwrap().is_some());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	receiver.settle(hold::SettleRequest { payment_preimage: preimage.to_vec() }).await.unwrap();
	async {
		loop {
			if db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap() == Some(preimage) { break; }
			tokio::time::sleep(poll_interval()).await;
		}
	}.wait_millis(30_000).await;
	let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
		hash: hash.to_vec(), wait: false,
	}).await.unwrap().into_inner();
	assert!(matches!(status.payment_status,
		Some(protos::lightning_payment_status::PaymentStatus::Success(ref success))
		if success.preimage == preimage.to_vec()), "{status:?}");
	assert_eq!(db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status,
		LightningPaymentStatus::Failed, "the public result must use actual settlement rather than trusting this stale row");
}
