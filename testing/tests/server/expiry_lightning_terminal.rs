use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::{PaymentHash, Preimage};
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark::vtxo::{Full, TapScriptClause};
use ark_testing::{btc, sat, TestContext};
use bitcoin::consensus::deserialize;
use bitcoin::Transaction;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use cln_rpc::plugins::hold;
use server::database::Db;
use server_rpc::protos;


/// Grant delivery must leave every authentic signed ancestor durable on the
/// server, even when neither wallet ever returns to register or claim it.
async fn assert_stored_grant_ancestry(db: &Db, grants: &[Vtxo<Full>]) {
	let mut expected = std::collections::BTreeMap::new();
	for grant in grants {
		for step in grant.transactions() {
			let raw = bitcoin::consensus::serialize(&step.tx);
			if let Some(old) = expected.insert(step.tx.compute_txid().to_string(), raw.clone()) {
				assert_eq!(old, raw, "shared signed grant ancestry must be exact");
			}
		}
	}
	let ids = expected.keys().cloned().collect::<Vec<_>>();
	let actual = db.read(async |t| Ok(t.query("SELECT txid,signed_tx FROM virtual_transaction WHERE txid=ANY($1)",
		&[&ids]).await?)).await.unwrap().into_iter().map(|row|
		(row.get::<_, String>("txid"), row.get::<_, Option<Vec<u8>>>("signed_tx")))
		.collect::<std::collections::BTreeMap<_,_>>();
	assert_eq!(actual.len(), expected.len(), "server is missing a granted receive's ancestor");
	for (txid, raw) in expected {
		assert_eq!(actual.get(&txid).and_then(Option::as_ref), Some(&raw),
			"server lost the actual signature/preimage for receive ancestor {txid}");
	}
}

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
	internal_zero_grant_timeout(false).await;
}

/// An actually funded incoming attempt must still refund its absent owner
/// when the existing pool can only create a one-satoshi final receive grant.
#[tokio::test]
async fn expiry_internal_unsafe_fragmented_pool_leaves_zero_grants_and_refunds_owner() {
	internal_zero_grant_timeout(true).await;
}

#[inline(never)]
fn internal_zero_grant_timeout(refuse_fragmented_pool: bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
	Box::pin(async move {
	let name = if refuse_fragmented_pool { "server/expiry_internal_unsafe_fragmented_pool" }
		else { "server/expiry_internal_zero_grant_timeout" };
	let ctx = TestContext::new(name).await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let builder = ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc());
	let builder = if refuse_fragmented_pool { builder } else { builder.no_vtxo_pool() };
	let srv = builder
		.funded(btc(10)).cfg(move |cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.receive_htlc_forward_timeout = Duration::from_secs(if refuse_fragmented_pool { 15 } else { 2 });
			if refuse_fragmented_pool {
				cfg.rpc_rich_errors = true; // Assert the exact admission error chain in this fixture.
				cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
				cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
				cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(45_000), count: 4 }];
			}
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create().await;
	if refuse_fragmented_pool { srv.wait_for_vtxopool(&ctx).await; }
	let recipient = ctx.bark_sdk("absent-recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice_amount = if refuse_fragmented_pool { 90_001 } else { 90_000 };
	let receiver_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let invoice = recipient.bolt11_invoice(sat(invoice_amount), None, None).await.unwrap();
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
		invoice: invoice.to_string(), payment_amount_sat: invoice_amount,
		htlc_vtxo_ids: vec![source.to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	if refuse_fragmented_pool {
		let db = Db::connect(&srv.config().postgres).await.unwrap();
		let available = db.read(async |t| Ok(t.query_one(
			"SELECT COUNT(*) FROM vtxo_pool WHERE spent_at IS NULL AND amount=45000 AND expiry_height>$1",
			&[&i32::try_from(tip.to_u32()).unwrap()]).await?.get::<_, i64>(0))).await.unwrap();
		assert!(available >= 3, "this is a genuine unsafe cover, not an empty or expired pool");
		let response = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
			payment_hash: hash.to_vec(), user_pubkey: receiver_key.serialize().to_vec(),
			htlc_recv_expiry: (tip + BlockDelta::new(36)).to_u32(), lightning_receive_anti_dos: None,
		}).await;
		let error = response.expect_err("a one-satoshi receive grant must be refused before cosigning");
		assert_eq!(error.code(), tonic::Code::Internal);
		assert!(error.message().contains("vtxo pool has no standard cover within the receive refund budget"),
			"the actual budget refusal must win, not an unrelated deadline, anti-DoS or transport error: {error}");
		let state = Db::connect(&srv.config().postgres).await.unwrap().read(async |t| Ok(t.query_one(
			"SELECT s.status::TEXT,(SELECT COUNT(*) FROM vtxo WHERE lightning_htlc_subscription_id=s.id) AS grants \
			FROM lightning_htlc_subscription s WHERE s.payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
		assert_eq!(state.get::<_, &str>("status"), "accepted");
		assert_eq!(state.get::<_, i64>("grants"), 0);
		let remaining = db.read(async |t| Ok(t.query_one(
			"SELECT COUNT(*) FROM vtxo_pool WHERE spent_at IS NULL AND amount=45000 AND expiry_height>$1",
			&[&i32::try_from(tip.to_u32()).unwrap()]).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(remaining, available, "refusal must not consume a pool input");
	}
	drop(rpc); // No second prepare, cooperative revoke, claim or wallet return.
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
	// The fragmented case also has genuine operator-only pool expiries.
	// Each concurrent parent needs its own CONFIRMED fee input at the held tip.
	let fee_inputs = if refuse_fragmented_pool { 3 } else { 1 };
	for _ in 0..fee_inputs {
		ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
	}
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
	})
}

/// ML030 NEW-state component: both actors disappear after real signed receive
/// grants. Their timeout inputs must be consumed by the SAME Bitcoin parent as
/// the original sender refund, not by an unrelated earlier cold-drain return.
#[tokio::test]
async fn expiry_internal_signed_grants_joint_timeout_pays_absent_sender() {
	internal_safe_pool_grants(false).await;
}

#[tokio::test]
async fn expiry_internal_existing_large_pool_cover_refunds_absent_owner() {
	internal_safe_pool_grants(true).await;
}

#[inline(never)]
fn internal_safe_pool_grants(large_cover: bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
	Box::pin(async move {
	let name = if large_cover { "server/expiry_internal_existing_large_pool_cover" }
		else { "server/expiry_internal_signed_grants_joint_timeout" };
	let ctx = TestContext::new(name).await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(move |cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = if large_cover { vec![
				server::vtxopool::VtxoTarget { amount: sat(45_000), count: 4 },
				server::vtxopool::VtxoTarget { amount: sat(100_000), count: 2 },
			] } else { vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 4 }] };
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let recipient = ctx.bark_sdk("absent-recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice_amount = if large_cover { 90_001 } else { 90_000 };
	let invoice = recipient.bolt11_invoice(sat(invoice_amount), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let receiver_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let receiver_destination = recipient.fallback_destination().await.unwrap();
	let sender = ctx.bark("absent-sender", &srv).funded(sat(200_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let client = sender.client().await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	assert_eq!(input.amount(), sat(100_000));
	let key = client.get_vtxo_key(&input).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	assert_ne!(destination.spk, receiver_destination.spk);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let incoming_deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let outgoing_deadline = tip + BlockDelta::new(36);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
		VtxoPolicy::new_server_htlc_send(key.public_key(), hash, incoming_deadline)).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let incoming = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	client.register_vtxo_transactions_with_server(&incoming).await.unwrap();
	let source = incoming[0].id();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice: invoice.to_string(), payment_amount_sat: invoice_amount,
		htlc_vtxo_ids: vec![source.to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	let granted = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: receiver_key.serialize().to_vec(),
		htlc_recv_expiry: outgoing_deadline.to_u32(), lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner();
	let grants = granted.htlc_vtxos.iter().map(|bytes| Vtxo::<Full>::deserialize(bytes).unwrap()).collect::<Vec<_>>();
	assert_eq!(grants.len(), if large_cover { 1 } else { 2 },
		"admission must retain the standard existing cover, including the normal two-grant control");
	assert_eq!(grants.iter().map(|g| g.amount()).sum::<bitcoin::Amount>(), sat(invoice_amount));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_stored_grant_ancestry(&db, &grants).await;
	drop(client);
	drop(recipient);
	drop(rpc); // No preimage disclosure, cooperative revoke, or returning wallet.
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
	if large_cover { ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await; }
	ctx.generate_blocks(2).await;
	// Normal watchman progress has to reach every actual HTLC leaf. Mine only
	// through completed sync cycles and leave reaction/readiness time each block.
	let parent: Transaction = tokio::time::timeout(Duration::from_secs(150), async {
		for _ in 0..100 {
			let height = ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(height).await;
			srv.watchmand().wait_for_sync_height(height).await;
			tokio::time::sleep(Duration::from_secs(1)).await;
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND a.broadcast_at IS NOT NULL", &[&source.to_string()]).await?)).await.unwrap();
			if let Some(row) = row { return deserialize(row.get("sweep_tx")).unwrap(); }
		}
		panic!("signed receive grants did not close in a joint normal timeout parent");
	}).await.expect("both-absent internal timeout required a returning wallet");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	for grant in &grants {
		let input = parent.input.iter().find(|i| i.previous_output == grant.point())
			.expect("dropping even one signed receive grant severs the Bitcoin dependency");
		let policy = grant.policy().as_server_htlc_recv().unwrap();
		let script = policy.server_claim_after_expiry_clause(grant.server_pubkey(), grant.exit_delta()).tapscript();
		assert_eq!(input.witness.taproot_leaf_script().unwrap().script, script.as_script());
	}
	let (index,paid) = parent.output.iter().enumerate().find(|(_, o)| o.script_pubkey == destination.spk).unwrap();
	assert_eq!(paid.value, sat(99_000));
	assert!(parent.output.iter().all(|o| o.script_pubkey != receiver_destination.spk));
	let mut input_sat = 0;
	for input in &parent.input {
		let prev = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		input_sat += prev.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent.output.iter().map(|o| o.value.to_sat()).sum::<u64>(), input_sat);
	ctx.generate_blocks(6).await;
	assert!(ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), index as u32, Some(true)).unwrap().unwrap().confirmations >= 6);
	let state = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, source).await).await.unwrap().unwrap();
	assert!(state.htlc.offchain_resolution.is_none(), "joint timeout must not manufacture the absent user's revocation");
	})
}

/// Two genuinely different internal payments share one original customer
/// backing. Every grant set must close together; per-attempt readiness cannot
/// permanently hold each one behind the other's unresolved source.
#[tokio::test]
async fn expiry_internal_shared_incoming_root_refunds_both_absent_attempts() {
	let ctx = TestContext::new("server/expiry_internal_shared_incoming_root").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 8 }];
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let recipient = ctx.bark_sdk("absent-recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoices = [recipient.bolt11_invoice(sat(90_000), None, None).await.unwrap(),
		recipient.bolt11_invoice(sat(90_000), None, None).await.unwrap()];
	assert_ne!(PaymentHash::from(&invoices[0]), PaymentHash::from(&invoices[1]));
	let recipient_keys = [recipient.new_address().await.unwrap().policy().user_pubkey(),
		recipient.new_address().await.unwrap().policy().user_pubkey()];
	let recipient_destination = recipient.fallback_destination().await.unwrap();
	let sender = ctx.bark_sdk("absent-sender", &srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let original = sender.spendable_vtxos().await.unwrap().remove(0);
	let original_body = sender.get_full_vtxo(original.id()).await.unwrap();
	let anchor = original_body.chain_anchor();
	let destination = sender.fallback_destination().await.unwrap();
	assert_ne!(destination.spk, recipient_destination.spk);
	// Normal signed self-payment creates two separately owned100k branches
	// under this SAME200k board, before the distinct HTLC operations begin.
	let own_address = sender.new_address().await.unwrap();
	sender.send_arkoor_payment(&own_address, sat(100_000)).await.unwrap();
	sender.sync().await;
	let mut parts = sender.spendable_vtxos().await.unwrap();
	parts.sort_unstable_by_key(|coin| coin.id());
	assert_eq!(parts.len(), 2);
	assert!(parts.iter().all(|coin| coin.amount() == sat(100_000)));
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let incoming_deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let outgoing_deadline = tip + BlockDelta::new(36);
	let mut rpc = srv.get_public_rpc().await;
	let mut sources = Vec::new();
	let mut source_paths = Vec::new();
	let mut grants = Vec::new();
	lightning.sync().await;
	for ((part, invoice), recipient_key) in parts.into_iter().zip(invoices).zip(recipient_keys) {
		let input = sender.get_full_vtxo(part.id()).await.unwrap();
		assert_eq!(input.chain_anchor(), anchor);
		let key = sender.get_vtxo_key(&input).await.unwrap();
		let hash = PaymentHash::from(&invoice);
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
			VtxoPolicy::new_server_htlc_send(key.public_key(), hash, incoming_deadline)).unwrap()
			.generate_user_nonces(&[key]).unwrap();
		let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
			parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
		}).await.unwrap().into_inner();
		let incoming = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
			.unwrap().build_signed_vtxos();
		assert_eq!(incoming.len(), 1);
		sender.register_vtxo_transactions_with_server(&incoming).await.unwrap();
		let source = incoming[0].id();
		sources.push(source);
		source_paths.push(incoming[0].clone());
		rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
			invoice: invoice.to_string(), payment_amount_sat: 90_000,
			htlc_vtxo_ids: vec![source.to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(5),
		}).await.unwrap();
		let response = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
			payment_hash: hash.to_vec(), user_pubkey: recipient_key.serialize().to_vec(),
			htlc_recv_expiry: outgoing_deadline.to_u32(), lightning_receive_anti_dos: None,
		}).await.unwrap().into_inner();
		let granted = response.htlc_vtxos.iter().map(|bytes| Vtxo::<Full>::deserialize(bytes).unwrap()).collect::<Vec<_>>();
		assert!(granted.len() >= 2);
		assert_eq!(granted.iter().map(|grant| grant.amount()).sum::<bitcoin::Amount>(), sat(90_000));
		assert!(granted.iter().all(|grant| grant.policy().as_server_htlc_recv().unwrap().payment_hash == hash));
		grants.extend(granted);
	}
	assert!(ctx.bitcoind().sync_client().get_tx_out(&anchor.txid, anchor.vout, Some(true)).unwrap().is_some(),
		"both native attempts must still share one actual physical incoming root");
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_stored_grant_ancestry(&db, &grants).await;
	drop(sender);
	drop(recipient);
	drop(rpc); // Neither actor reveals a preimage or returns to cancel anything.
	let ids = sources.iter().map(ToString::to_string).collect::<Vec<_>>();
	let actual = db.read(async |t| Ok(t.query_one("SELECT COUNT(DISTINCT f.id) AS attempts,COUNT(DISTINCT f.subscription_id) AS subscriptions \
		FROM expiry_lightning_internal_funding f JOIN lightning_payment_attempt_htlc_vtxo l \
		ON l.lightning_payment_attempt_id=f.id WHERE l.vtxo_id=ANY($1)", &[&ids]).await?)).await.unwrap();
	assert_eq!(actual.get::<_, i64>("attempts"), 2);
	assert_eq!(actual.get::<_, i64>("subscriptions"), 2);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(150_000)).await;
	ctx.generate_blocks(2).await;
	let grant_ids = grants.iter().map(|grant| grant.id().to_string()).collect::<Vec<_>>();
	let mut final_fee_sponsored = false;
	let parent: Transaction = tokio::time::timeout(Duration::from_secs(150), async {
		for _ in 0..100 {
			// Publication can consume the grant inputs between observations.
			// Accept the real parent before re-reading their current UTXOs;
			// the unchanged checks below still prove every timeout and Core6.
			let published = db.read(async |t| Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				WHERE a.broadcast_at IS NOT NULL AND (SELECT COUNT(DISTINCT e.vtxo_id) FROM expiry_allocation_coin c \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE c.allocation_id=a.id AND e.vtxo_id=ANY($1))=2",
				&[&ids]).await?)).await.unwrap();
			if let Some(row) = published { return deserialize(row.get("sweep_tx")).unwrap(); }
			// The initial single sponsor also pays genuine grant progression
			// and other pool expiries. Its change can be unconfirmed when the
			// complete joint decision finally exists. Holding the mature tip
			// then cannot make that change usable for the payout fee child.
			let committed = if final_fee_sponsored { false } else {
				db.read(async |t| Ok(t.query_opt("SELECT a.id FROM expiry_allocation a \
					WHERE a.status='pending' AND a.sweep_tx IS NOT NULL AND a.broadcast_at IS NULL \
					AND (SELECT COUNT(DISTINCT e.vtxo_id) FROM expiry_allocation_coin c \
					JOIN expiry_entitlement e ON e.id=c.entitlement_id \
					WHERE c.allocation_id=a.id AND c.active AND e.vtxo_id=ANY($1))=2",
					&[&ids]).await?.is_some())).await.unwrap()
			};
			if committed {
				final_fee_sponsored = true;
				let address = srv.watchmand().wait_wallet_address().await;
				ctx.bitcoind().fund_addr(address.clone(), sat(150_000)).await;
				ctx.bitcoind().fund_addr(address, sat(150_000)).await;
				let height = ctx.generate_blocks(2).await;
				srv.wait_for_sync_height(height).await;
				srv.watchmand().wait_for_sync_height(height).await;
				continue;
			}
			// Keep automatic watchman progression until ALL actual grant leaves
			// confirm. Then mine only their exact remaining signed CSV/CLTV
			// maturity and hold that tip for readiness. Repeated one-block syncs
			// otherwise consume this same 150s budget before one mature pass.
			let rows = db.read(async |t| Ok(t.query("SELECT vtxo_id,confirmed_height FROM vtxo \
				WHERE vtxo_id=ANY($1) AND onchain_spent_height IS NULL AND onchain_spent_txid IS NULL",
				&[&grant_ids]).await?)).await.unwrap();
			let confirmed = rows.into_iter().filter_map(|row| row.get::<_, Option<i32>>("confirmed_height")
				.map(|height| (row.get::<_, String>("vtxo_id"),BlockHeight::try_from(height).unwrap())))
				.collect::<std::collections::BTreeMap<_,_>>();
			let maturity = if confirmed.len()==grants.len() {
				grants.iter().map(|grant| {
					let output = ctx.bitcoind().sync_client().get_tx_out(&grant.point().txid,grant.point().vout,Some(true))
						.unwrap()?;
					if output.confirmations==0 { return None; }
					assert_eq!(output.value,grant.amount());
					let deadline = grant.policy().as_server_htlc_recv().unwrap().htlc_expiry;
					Some(confirmed[&grant.id().to_string()].checked_add(grant.exit_delta()).unwrap().max(deadline))
				}).collect::<Option<Vec<_>>>().and_then(|heights| heights.into_iter().max())
			} else { None };
			let current = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
			let height = match maturity {
				Some(target) if target>current => ctx.generate_blocks(target.to_u32()-current.to_u32()).await,
				Some(_) => current,
				None => ctx.generate_blocks(1).await,
			};
			srv.wait_for_sync_height(height).await;
			srv.watchmand().wait_for_sync_height(height).await;
			tokio::time::sleep(Duration::from_secs(1)).await;
			if let Some(target) = maturity {
				let mature_view = db.read(async |t| {
					let Some(view) = t.expiry_rebuild_token().await? else { return Ok(false); };
					Ok(view.captaind.height>=target && view.watchmand.height>=target && t.expiry_view_is_rebuilt(view).await?)
				}).await.unwrap();
				if !mature_view { continue; }
			} else { continue; }
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				WHERE a.broadcast_at IS NOT NULL AND (SELECT COUNT(DISTINCT e.vtxo_id) FROM expiry_allocation_coin c \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE c.allocation_id=a.id AND e.vtxo_id=ANY($1))=2",
				&[&ids]).await?)).await.unwrap();
			if let Some(row) = row { return deserialize(row.get("sweep_tx")).unwrap(); }
		}
		panic!("different internal attempts sharing one original backing remain mutually held");
	}).await.expect("connected internal refunds required a returning wallet");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let mut incoming_points = std::collections::BTreeSet::new();
	for source in &source_paths {
		let points = source.ancestor_ids().into_iter().map(|id| id.to_point())
			.chain([source.chain_anchor(), source.point()]).collect::<std::collections::BTreeSet<_>>();
		let matched = parent.input.iter().filter(|input| points.contains(&input.previous_output)).collect::<Vec<_>>();
		assert_eq!(matched.len(), 1, "every original source needs one actual backing input on its signed path");
		incoming_points.insert(matched[0].previous_output);
	}
	assert_eq!(parent.input.len(), incoming_points.len() + grants.len(), "no dependency or unrelated input may be hidden");
	for grant in &grants {
		let inputs = parent.input.iter().filter(|input| input.previous_output == grant.point()).collect::<Vec<_>>();
		assert_eq!(inputs.len(), 1, "every grant of BOTH attempts must be spent exactly once by this parent");
		let policy = grant.policy().as_server_htlc_recv().unwrap();
		let timeout = policy.server_claim_after_expiry_clause(grant.server_pubkey(), grant.exit_delta()).tapscript();
		assert_eq!(inputs[0].witness.taproot_leaf_script().unwrap().script, timeout.as_script());
	}
	let payouts = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	assert_eq!(payouts[0].1.value, sat(199_000), "same-root same-user payout charges one output fee");
	assert!(parent.output.iter().all(|output| output.script_pubkey != recipient_destination.spk));
	let mut input_sat = 0;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		input_sat += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent.output.iter().map(|output| output.value.to_sat()).sum::<u64>(), input_sat);
	ctx.generate_blocks(6).await;
	assert!(ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), payouts[0].0 as u32, Some(true)).unwrap().unwrap().confirmations >= 6);
	for source in sources {
		let original = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, source).await).await.unwrap().unwrap();
		assert!(original.htlc.offchain_resolution.is_none(), "neither absent sender source is fictitiously revoked");
	}
}
