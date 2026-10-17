use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::{PaymentHash, Preimage};
use ark::VtxoPolicy;
use ark_testing::{btc, sat, TestContext};
use bitcoin::consensus::deserialize;
use bitcoin::Transaction;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use cln_rpc::plugins::hold;
use server::database::Db;
use server_rpc::protos;


/// A real external HTLC is accepted and then cancelled by its receiver.
/// No test row supplies a failure proof, entitlement, spend fence or payout.
/// The abandoned sender never requests a cooperative HTLC revocation.
async fn failed_external_payout(no_channel: bool) {
	let name = if no_channel { "server/expiry_no_part_external_offline_owner" } else { "server/expiry_failed_external_offline_owner" };
	let ctx = TestContext::new(name).await;
	let lightning = if no_channel { ctx.new_lightning_setup_no_channel("lightningd").await }
		else { ctx.new_lightning_setup("lightningd").await };
	let srv = ctx.captaind("server").lightningd(&lightning.internal).no_vtxo_pool()
		.funded(btc(10)).cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24)).watchmand().create().await;
	let wallet = ctx.bark("abandoned", &srv).funded(sat(200_000)).create().await;
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let client = wallet.client().await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	let key = client.get_vtxo_key(&input).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	let preimage = Preimage::random();
	let hash = preimage.compute_payment_hash();
	let mut receiver = lightning.external.hold_client().await;
	let invoice = receiver.invoice(hold::InvoiceRequest {
		payment_hash: hash.to_vec(), amount_msat: 90_000_000,
		description: Some(hold::invoice_request::Description::Memo("failed external offline owner".into())),
		min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()],
		VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline)).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let leaves = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	client.register_vtxo_transactions_with_server(&leaves).await.unwrap();
	let id = leaves[0].id();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice, payment_amount_sat: 90_000, htlc_vtxo_ids: vec![id.to_bytes().to_vec()],
		mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	if !no_channel {
		lightning.external.wait_for_hold_invoice_accepted(hash).await;
		receiver.cancel(hold::CancelRequest { payment_hash: hash.to_vec() }).await.unwrap();
	}
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let attempt = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.id FROM lightning_payment_attempt a \
				JOIN expiry_lightning_terminal_failure f ON f.id=a.id WHERE a.payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
			if let Some(row) = row { break row.get::<_, i64>("id"); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual CLN dispatch never produced its own terminal failure proof");
	let proof = db.read(async |t| Ok(t.query_one("SELECT created_index,jsonrpc_error_code FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt]).await?)).await.unwrap();
	if no_channel {
		assert_eq!(proof.get::<_, Option<i64>>("created_index"), None);
		assert_eq!(proof.get::<_, Option<i32>>("jsonrpc_error_code"), Some(205));
	} else {
		assert!(proof.get::<_, Option<i64>>("created_index").is_some());
		assert_eq!(proof.get::<_, Option<i32>>("jsonrpc_error_code"), None);
	}
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	drop(client);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(deadline.to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let parent: Transaction = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND a.broadcast_at IS NOT NULL", &[&id.to_string()]).await?)).await.unwrap();
			if let Some(row) = row { break deserialize(row.get("sweep_tx")).unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("failed send still requires an online client to refund it");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let (index, output) = parent.output.iter().enumerate().find(|(_, output)| output.script_pubkey == destination.spk).unwrap();
	assert_eq!(output.value, sat(99_000));
	let mut total = 0;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		total += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent.output.iter().map(|output| output.value.to_sat()).sum::<u64>(), total);
	ctx.generate_blocks(6).await;
	assert!(ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), index as u32, Some(true)).unwrap().unwrap().confirmations >= 6);
	let upstream = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, id).await).await.unwrap().unwrap();
	assert_eq!(upstream.htlc.resolution(), None, "fallback must not manufacture a cooperative user revocation");
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT COUNT(*) FROM expiry_lightning_dispatch WHERE id=$1", &[&attempt]).await?.get::<_, i64>(0))).await.unwrap(), 1);
}

#[tokio::test]
async fn expiry_actual_failed_external_send_pays_offline_owner() {
	failed_external_payout(false).await;
}

#[tokio::test]
async fn expiry_actual_no_part_external_send_pays_offline_owner() {
	failed_external_payout(true).await;
}

/// ML029: normal internal setup timeout closes a receive which never granted
/// any coin. Both actors stay absent; only real services decide and pay back
/// the original 100k source less its single 1k output fee.
#[tokio::test]
async fn expiry_internal_zero_grant_timeout_pays_absent_sender() {
	let ctx = TestContext::new("server/expiry_internal_zero_grant_timeout").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).no_vtxo_pool()
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.receive_htlc_forward_timeout = Duration::from_secs(2);
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create().await;
	let recipient = ctx.bark_sdk("absent-recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(90_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	drop(recipient); // No prepare-claim, claim, revoke or later wallet call.
	let sender = ctx.bark("absent-sender", &srv).funded(sat(200_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let client = sender.client().await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	assert_eq!(input.amount(), sat(100_000));
	let key = client.get_vtxo_key(&input).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
		VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline)).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let leaves = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	client.register_vtxo_transactions_with_server(&leaves).await.unwrap();
	let source = leaves[0].id();
	drop(client);
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice: invoice.to_string(), payment_amount_sat: 90_000,
		htlc_vtxo_ids: vec![source.to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	drop(rpc); // The sender does not poll or ask for cooperative revocation.
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (attempt, subscription) = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.id,f.subscription_id FROM expiry_lightning_internal_funding f \
				JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
				JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash \
				WHERE a.payment_hash=$1 AND s.status='canceled'", &[&hash.to_string()]).await?)).await.unwrap();
			if let Some(row) = row { break (row.get::<_, i64>("id"), row.get::<_, i64>("subscription_id")); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("normal accepted-setup timeout did not cancel the actual internal subscription");
	let mut hold = lightning.internal.hold_client().await;
	let actual = hold.list(hold::ListRequest {
		constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
	}).await.unwrap().into_inner();
	assert!(actual.invoices.iter().any(|i| i.payment_hash == hash.to_vec()
		&& i.invoice == invoice.to_string() && i.state == hold::InvoiceState::Cancelled as i32));
	let mut node = lightning.internal.grpc_client().await;
	assert!(node.list_send_pays(cln_rpc::ListsendpaysRequest {
		payment_hash: Some(hash.to_vec()), ..Default::default()
	}).await.unwrap().into_inner().payments.is_empty());
	let unclaimed = db.read(async |t| Ok(t.query_one("SELECT \
		(SELECT COUNT(*) FROM vtxo WHERE lightning_htlc_subscription_id=$1) AS grants, \
		(SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE subscription_id=$1) AS bindings, \
		EXISTS(SELECT 1 FROM expiry_lightning_dispatch WHERE id=$2) AS dispatched, \
		EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$3) AS wal",
		&[&subscription,&attempt,&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(unclaimed.get::<_, i64>("grants"), 0);
	assert_eq!(unclaimed.get::<_, i64>("bindings"), 0);
	assert!(!unclaimed.get::<_, bool>("dispatched") && !unclaimed.get::<_, bool>("wal"));
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(deadline.to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let parent: Transaction = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND a.broadcast_at IS NOT NULL \
				AND c.evidence->>'kind'='htlc_internal_zero_grant_cancelled'", &[&source.to_string()]).await?)).await.unwrap();
			if let Some(row) = row { break deserialize(row.get("sweep_tx")).unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("cancelled zero-grant internal funding still requires a returning wallet");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let (index, paid) = parent.output.iter().enumerate().find(|(_, o)| o.script_pubkey == destination.spk).unwrap();
	assert_eq!(paid.value, sat(99_000));
	assert_eq!(parent.output.iter().filter(|o| o.script_pubkey == destination.spk).count(), 1);
	let mut input_sat = 0;
	for input in &parent.input {
		let prev = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		input_sat += prev.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent.output.iter().map(|o| o.value.to_sat()).sum::<u64>(), input_sat);
	ctx.generate_blocks(6).await;
	assert!(ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), index as u32, Some(true)).unwrap().unwrap().confirmations >= 6);
	let kept = db.read(async |t| Ok(t.query_one("SELECT \
		EXISTS(SELECT 1 FROM expiry_lightning_internal_funding WHERE id=$1 AND subscription_id=$2) AS funding, \
		(SELECT COUNT(*) FROM vtxo WHERE lightning_htlc_subscription_id=$2) AS grants, \
		(SELECT COUNT(*) FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			WHERE e.vtxo_id=$3 AND c.active AND c.disposition='customer') AS paid_once",
		&[&attempt,&subscription,&source.to_string()]).await?)).await.unwrap();
	assert!(kept.get::<_, bool>("funding"));
	assert_eq!(kept.get::<_, i64>("grants"), 0);
	assert_eq!(kept.get::<_, i64>("paid_once"), 1);
	let original = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, source).await).await.unwrap().unwrap();
	assert_eq!(original.htlc.resolution(), None, "automatic closure never invents a signed revocation");
}
