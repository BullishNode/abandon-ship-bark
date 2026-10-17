//! Recover a real dispatched payment's receipt after its caller has stopped.

use std::collections::BTreeSet;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::fees::VtxoFeeInfo;
use ark::lightning::{PaymentHash, Preimage};
use ark::{VtxoId, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::lightningd::Lightningd;
use bitcoin::{OutPoint, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use cln_rpc::listpays_pays::ListpaysPaysStatus;
use cln_rpc::plugins::hold;
use server::database::Db;
use server_rpc::protos;

async fn remote_pays(node: &Lightningd, hash: PaymentHash) -> Vec<cln_rpc::ListpaysPays> {
	node.grpc_client().await.list_pays(cln_rpc::ListpaysRequest {
		bolt11: None, payment_hash: Some(hash.to_vec()), status: None,
		index: None, limit: None, start: None,
	}).await.unwrap().into_inner().pays
}

async fn assert_one_source_dispatch(
	db: &Db, hash: PaymentHash, sources: &[VtxoId], attempt: i64, label: &str, succeeded: bool,
) {
	let row = db.read(async |tx| Ok(tx.query_one("SELECT d.label,a.amount_msat,a.user_fee_sat, \
		(SELECT COUNT(*) FROM lightning_payment_attempt other WHERE other.payment_hash=a.payment_hash) AS attempts, \
		(SELECT COUNT(*) FROM expiry_lightning_dispatch other JOIN lightning_payment_attempt p \
		 ON p.id=other.id WHERE p.payment_hash=a.payment_hash) AS dispatches, \
		(SELECT COUNT(*) FROM expiry_lightning_external_success s WHERE s.id=a.id) AS successes, \
		(SELECT COUNT(*) FROM expiry_lightning_terminal_failure f WHERE f.id=a.id) AS failures \
		FROM lightning_payment_attempt a JOIN expiry_lightning_dispatch d ON d.id=a.id \
		WHERE a.id=$1 AND a.payment_hash=$2", &[&attempt, &hash.to_string()],
	).await?)).await.unwrap();
	assert_eq!(row.get::<_, String>("label"), label);
	assert_eq!(row.get::<_, i64>("amount_msat"), 100_000_000);
	assert_eq!(row.get::<_, Option<i64>>("user_fee_sat"), Some(0));
	assert_eq!(row.get::<_, i64>("attempts"), 1, "restart must reconcile the original attempt rather than retrying payment");
	assert_eq!(row.get::<_, i64>("dispatches"), 1);
	assert_eq!(row.get::<_, i64>("successes"), if succeeded { 1 } else { 0 });
	assert_eq!(row.get::<_, i64>("failures"), 0, "an accepted ambiguous dispatch is not terminal failure");
	for (index, source) in sources.iter().enumerate() {
		let count = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) \
			FROM lightning_payment_attempt_htlc_vtxo WHERE vtxo_id=$1", &[&source.to_string()],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(count, if index == 0 { 1 } else { 0 }, "the same-hash sibling never funded this dispatch");
	}
}

#[tokio::test]
async fn expiry_external_success_during_restart_is_bound_to_its_actual_source() {
	tokio::time::timeout(Duration::from_secs(180), external_success_during_restart()).await
		.expect("the bounded real external receipt/restart case must finish");
}

async fn external_success_during_restart() {
	let ctx = TestContext::new("server/qualification_lightning_lost_dispatch_reply").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.no_vtxo_pool().funded(btc(10)).cfg(|config| {
			config.vtxo_lifetime = BlockDelta::new(24);
			config.vtxo_exit_delta = BlockDelta::new(3);
			// One direct channel with final CLTV18 and the native six-block
			// settlement margin; no production-lifetime claim follows.
			config.htlc_send_expiry_delta = BlockDelta::new(40);
			config.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create().await;
	let first_height = ctx.bitcoind().get_block_count().await;
	let wallet = ctx.bark("absent-sender", &srv).cfg(|config| config.daemon_manual_sync = true)
		.funded(sat(500_000)).create().await;
	let first = wallet.board(sat(100_000)).await;
	let second = wallet.board(sat(100_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.sync().await;
	assert_eq!((first.vtxos.len(), second.vtxos.len()), (1, 1));
	let client = wallet.client().await;
	let destination = client.fallback_destination().await.unwrap();
	let preimage = Preimage::random();
	let hash = preimage.compute_payment_hash();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let mut rpc = srv.get_public_rpc().await;
	let mut grants = Vec::new();
	let mut roots = Vec::new();
	for id in [first.vtxos[0], second.vtxos[0]] {
		let input = client.get_full_vtxo(id).await.unwrap();
		assert_eq!(input.amount(), sat(100_000));
		roots.push(input.chain_anchor());
		let key = client.get_vtxo_key(&input).await.unwrap();
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()],
			VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline)).unwrap()
			.generate_user_nonces(&[key]).unwrap();
		let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
			parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
		}).await.unwrap().into_inner();
		let leaves = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
			.unwrap().build_signed_vtxos();
		assert_eq!(leaves.len(), 1);
		assert_eq!(leaves[0].amount(), sat(100_000));
		assert_eq!(leaves[0].chain_anchor(), input.chain_anchor());
		client.register_vtxo_transactions_with_server(&leaves).await.unwrap();
		grants.push(leaves[0].clone());
	}
	assert_ne!(roots[0], roots[1]);
	assert_eq!(grants[0].expiry_height(), grants[1].expiry_height());
	assert!(deadline > grants[0].expiry_height());
	// Use the fixture's ACTUAL existing fee schedule. Do not overfund a90k
	// invoice with a100k source and silently discard the10k excess as a fee.
	let fee = srv.config().fees.lightning_send.calculate(sat(100_000),
		[VtxoFeeInfo::from_vtxo_and_tip(&grants[0], tip)]).unwrap();
	assert_eq!(fee, sat(0), "this direct-channel fixture requires its declared native zero-fee schedule");
	let sources = grants.iter().map(|grant| grant.id()).collect::<Vec<_>>();
	let mut receiver = lightning.external.hold_client().await;
	let invoice = receiver.invoice(hold::InvoiceRequest {
		payment_hash: hash.to_vec(), amount_msat: 100_000_000,
		description: Some(hold::invoice_request::Description::Memo("native lost dispatch reply".into())),
		min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice, payment_amount_sat: 100_000, htlc_vtxo_ids: vec![sources[0].to_bytes().to_vec()],
		mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	lightning.external.wait_for_hold_invoice_accepted(hash).await;
	drop(client);
	drop(wallet);
	// The accepted sender is now fenced: no later client open/sync,
	// revocation or resubmission recovers this payment or the unused sibling.
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (attempt, label) = db.read(async |tx| Ok(tx.query_one("SELECT a.id,d.label \
		FROM lightning_payment_attempt a JOIN expiry_lightning_dispatch d ON d.id=a.id \
		WHERE a.payment_hash=$1", &[&hash.to_string()],
	).await.map(|row| (row.get::<_, i64>("id"), row.get::<_, String>("label")))?)).await.unwrap();
	assert_one_source_dispatch(&db, hash, &sources, attempt, &label, false).await;
	assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let pending = remote_pays(&lightning.internal, hash).await;
	assert_eq!(pending.len(), 1);
	assert_eq!(pending[0].label.as_deref(), Some(label.as_str()));
	assert_eq!(pending[0].status(), ListpaysPaysStatus::Pending);
	assert!(pending[0].preimage.is_none());
	// The external payment survives loss of the local xpay task/response.
	// Both product processes are stopped, while the two owned CLN peers stay
	// running. This is an authentic unresolved dispatch, not a forged status.
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	receiver.settle(hold::SettleRequest { payment_preimage: preimage.to_vec() }).await.unwrap();
	let remote = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let pays = remote_pays(&lightning.internal, hash).await;
			assert_eq!(pays.len(), 1, "a single native dispatch must not create another external payment group");
			if pays[0].status() == ListpaysPaysStatus::Complete { break pays.into_iter().next().unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the actual external receiver did not settle while the product was down");
	assert_eq!(remote.label.as_deref(), Some(label.as_str()));
	assert_eq!(remote.payment_hash, hash.to_vec());
	assert_eq!(remote.preimage.as_deref(), Some(preimage.as_ref()));
	assert_eq!(remote.amount_msat.as_ref().unwrap().msat, 100_000_000);
	assert_eq!(remote.amount_sent_msat.as_ref().unwrap().msat, 100_000_000,
		"the single direct channel must not hide routing cost or change in the paid principal");
	assert!(remote.created_index.unwrap() > 0);
	assert_one_source_dispatch(&db, hash, &sources, attempt, &label, false).await;
	assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none(),
		"the real remote result must not appear in product state before its observer restarts");
	srv.start().await.unwrap();
	srv.watchmand().start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(45), async {
		loop {
			let receipt = db.read(async |tx| Ok(tx.query_opt("SELECT created_index \
				FROM expiry_lightning_external_success WHERE id=$1", &[&attempt],
			).await?)).await.unwrap();
			if let Some(receipt) = receipt {
				assert_eq!(receipt.get::<_, i64>(0), i64::try_from(remote.created_index.unwrap()).unwrap());
				assert_eq!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap(), Some(preimage));
				break;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("restart must reconcile the ORIGINAL source-bound CLN receipt and actual preimage");
	assert_one_source_dispatch(&db, hash, &sources, attempt, &label, true).await;
	let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
		hash: hash.to_vec(), wait: false,
	}).await.unwrap().into_inner();
	assert!(matches!(status.payment_status,
		Some(protos::lightning_payment_status::PaymentStatus::Success(ref success))
		if success.preimage == preimage.to_vec()));
	let fee_address = srv.watchmand().wait_wallet_address().await;
	for _ in 0..2 { ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await; }
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	assert!(height < deadline.to_u32());
	let expired = ctx.generate_blocks(deadline.to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let parent = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |tx| Ok(tx.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL",
				&[&sources[1].to_string()],
			).await?)).await.unwrap();
			if let Some(row) = row { break deserialize::<Transaction>(row.get("sweep_tx")).unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the UNUSED same-hash source must automatically refund its absent owner after actual deadline");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let payouts = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk)
		.collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	let (index, output) = payouts[0];
	assert_eq!(output.value, sat(99_000));
	let mut backing = 0;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		backing += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent.output.iter().map(|output| output.value.to_sat()).sum::<u64>(), backing);
	let confirmed = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	let actual = ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), index as u32, Some(true))
		.unwrap().unwrap();
	assert!(actual.confirmations >= 6);
	assert_eq!(actual.value, sat(99_000));
	assert_eq!(actual.script_pub_key.script().unwrap(), destination.spk);
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |tx| Ok(tx.query_one("SELECT \
				COUNT(*) FILTER (WHERE c.active) AS active, \
				COUNT(DISTINCT a.sweep_txid) FILTER (WHERE a.status='confirmed') AS paid, \
				MAX(a.sweep_txid) FILTER (WHERE a.status='confirmed') AS payer, \
				MAX(c.fee_sat) FILTER (WHERE c.active) AS fee, \
				MAX(c.payout_sat) FILTER (WHERE c.active) AS payout \
				FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id \
				JOIN expiry_allocation a ON a.id=c.allocation_id \
				WHERE e.vtxo_id=$1 AND c.disposition='customer'", &[&sources[1].to_string()],
			).await?)).await.unwrap();
			assert!(row.get::<_, i64>("active") <= 1);
			assert!(row.get::<_, i64>("paid") <= 1, "the unused source cannot receive a second economic payout");
			if row.get::<_, i64>("paid") == 1 {
				assert_eq!(row.get::<_, i64>("active"), 1);
				assert_eq!(row.get::<_, Option<String>>("payer"), Some(parent.compute_txid().to_string()));
				assert_eq!(row.get::<_, Option<i64>>("fee"), Some(1_000));
				assert_eq!(row.get::<_, Option<i64>>("payout"), Some(99_000));
				break;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the unused source's unique canonical refund must reconcile in the durable ledger");
	let paid_customer_rows = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN vtxo v ON v.vtxo_id=e.vtxo_id \
		WHERE v.anchor_point=$1 AND c.disposition='customer'", &[&roots[0].to_string()],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(paid_customer_rows, 0,
		"no predecessor or descendant under the actual paid backing root may receive a second customer refund");
	// Reconcile ALL canonical customer outputs from both original roots,
	// including historical predecessor entitlements. Counting only the chosen
	// grant leaf/output could hide a second refund elsewhere in that lineage.
	let root_ids = roots.iter().map(ToString::to_string).collect::<Vec<_>>();
	let allocations = db.read(async |tx| Ok(tx.query("SELECT DISTINCT a.sweep_tx FROM expiry_allocation a \
		JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
		JOIN vtxo v ON v.vtxo_id=e.vtxo_id WHERE v.anchor_point=ANY($1) AND a.sweep_tx IS NOT NULL",
		&[&root_ids],
	).await?)).await.unwrap();
	let mut customer_outputs = BTreeSet::new();
	let mut customer_total = sat(0);
	for allocation in allocations {
		let recorded = deserialize::<Transaction>(allocation.get("sweep_tx")).unwrap();
		let txid = recorded.compute_txid();
		let Some(info) = ctx.bitcoind().sync_client().custom_get_raw_transaction_info(txid, None).unwrap() else { continue; };
		if info.confirmations.unwrap_or(0) == 0 { continue; }
		assert!(info.confirmations.unwrap() >= 6);
		assert_eq!(ctx.bitcoind().await_transaction(txid).await, recorded);
		for (vout, output) in recorded.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk) {
			assert!(customer_outputs.insert(OutPoint::new(txid, vout as u32)));
			customer_total += output.value;
		}
	}
	assert_eq!(customer_outputs, BTreeSet::from([OutPoint::new(parent.compute_txid(), index as u32)]));
	assert_eq!(customer_total, sat(99_000), "no other canonical customer output may hide an extra same-root refund");
	assert_eq!(sat(100_000) + customer_total + sat(1_000), grants.iter().map(|grant| grant.amount()).sum::<bitcoin::Amount>(),
		"actual external principal plus ALL canonical customer outputs and their fee conserve both grants");
	assert_one_source_dispatch(&db, hash, &sources, attempt, &label, true).await;
	let final_pays = remote_pays(&lightning.internal, hash).await;
	assert_eq!(final_pays.len(), 1);
	assert_eq!(final_pays[0].created_index, remote.created_index);
	assert_eq!(final_pays[0].label, remote.label);
	assert_eq!(final_pays[0].status(), ListpaysPaysStatus::Complete);
	assert!(ctx.bitcoind().get_block_count().await - first_height <= 96,
		"this direct-channel fixture must remain a bounded native case");
}
