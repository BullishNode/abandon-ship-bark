//! Native intra-Ark source arbitration without a receive grant.

use std::sync::Arc;
use std::time::Duration;

use ark::{ProtocolEncoding, Vtxo, VtxoId, VtxoPolicy};
use ark::arkoor::{ArkoorCosignRequest, ArkoorDestination};
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignRequest};
use ark::vtxo::Full;
use ark::attestations::ArkoorCosignAttestation;
use ark::lightning::PaymentHash;
use bark::vtxo::VtxoState;
use bitcoin_ext::BlockDelta;
use bitcoincore_rpc::RpcApi;
use server::database::Db;
use server::database::carrier_snapshot::BackingSnapshotLimits;
use server::database::ln::LightningHtlcSubscriptionStatus;
use server_rpc::protos;

use ark_testing::{btc, sat, TestContext};

#[tokio::test]
async fn zero_grant_actual_timeout_cancels_before_sender_freeze() {
	zero_grant_timeout_case(false).await;
}

#[tokio::test]
async fn zero_grant_actual_unavailable_incoming_node_cannot_refund() {
	zero_grant_timeout_case(true).await;
}

async fn zero_grant_timeout_case(stop_incoming_node: bool) {
	let name = if stop_incoming_node { "zero_grant_actual_unavailable_incoming_node_cannot_refund" }
		else { "zero_grant_actual_timeout_cancels_before_sender_freeze" };
	let ctx = TestContext::new(format!("expiry_zero_grant/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(512); c.vtxopool.vtxo_lifetime = BlockDelta::new(512);
			c.expiry_payout.grace_blocks = 0;
			// Expose the source API's arbitration, rather than letting the
			// periodic hold monitor cancel this native fixture first.
			c.invoice_check_interval = Duration::from_secs(3600);
			c.receive_htlc_forward_timeout = Duration::from_secs(3);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("absent_sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let receiver = ctx.bark_sdk("returning_receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let invoice = receiver.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	sender.pay_lightning_invoice(invoice.to_string(), None, false).await.unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert_eq!(sub.status, LightningHtlcSubscriptionStatus::Accepted);
	assert!(sub.htlc_vtxos.is_empty());
	assert!(sub.accepted_at.is_some());
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert!(attempt.is_self_payment() && !attempt.status.is_final());
	let (key, _) = receiver.derive_store_next_keypair().await.unwrap();
	receiver.fallback_key_link(key.public_key()).await.unwrap();
	let stale_prepare = protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: sub.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta).into(),
		lightning_receive_anti_dos: None,
	};
	let coins = sender.all_vtxos().await.unwrap().into_iter().filter(|w| !matches!(w.state, VtxoState::Spent)).collect::<Vec<_>>();
	let terminals = coins.iter().map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let roots = coins.iter().map(|w| VtxoId::from(w.vtxo.chain_anchor())).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
	let send_ids = coins.iter().filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	assert!(!send_ids.is_empty());
	let expiry = coins.iter().map(|w| w.vtxo.expiry_height().to_u32()).max().unwrap();
	drop(sender);
	let config = srv.config().clone(); srv.stop().await.unwrap();
	let server = server::Server::start(config).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		while server.chain_tip().height.to_u32() != core.get_block_count().unwrap() as u32 {
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	tokio::time::sleep(Duration::from_secs(3)).await;
	assert_eq!(db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().status,
		LightningHtlcSubscriptionStatus::Accepted, "periodic monitor must not supply this fixture's cancellation");
	let budget = BackingSnapshotLimits { max_rows: 1000, max_serialized_bytes: 10_000_000 };
	if stop_incoming_node {
		// Stop the actual owned CLN process. No invoice/subscription/attempt
		// status is replaced; prior intra-Ark setup does not stand in for a
		// current authoritative incoming cancellation observation.
		lightning.internal.stop().await.unwrap();
		let refused = tokio::time::timeout(Duration::from_secs(15),
			server.freeze_current_expiry_backing(&roots, &terminals, budget)).await.unwrap().err().unwrap();
		assert!(format!("{refused:#}").contains("incoming hold node"), "{refused:#}");
		assert_eq!(db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().status,
			LightningHtlcSubscriptionStatus::Accepted);
		assert!(!db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status.is_final());
		let count: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_allocation", &[]).await?.get(0))).await.unwrap();
		assert_eq!(count, 0);
		assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
		return;
	}
	// This existing SQL settlement barrier lets the freeze acquire the payment
	// guard first while a real returning recipient queues preparation behind it.
	let entered = Arc::new(tokio::sync::Notify::new());
	let release = Arc::new(tokio::sync::Notify::new());
	let barrier_db = db.clone(); let enter = entered.clone(); let leave = release.clone();
	let barrier = tokio::spawn(async move { barrier_db.write(async |t| {
		t.lock_htlc_settlements().await?; enter.notify_one(); leave.notified().await; Ok(())
	}).await });
	entered.notified().await;
	let freeze_server = server.clone(); let freeze_roots = roots.clone(); let freeze_terminals = terminals.clone();
	let freezing = tokio::spawn(async move { freeze_server.freeze_current_expiry_backing(&freeze_roots, &freeze_terminals, budget).await });
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let waiting: bool = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory')", &[]).await?.get(0))).await.unwrap();
			if waiting { break; }
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}).await.unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let mut late_prepare = tokio::spawn(async move { rpc.prepare_lightning_receive_claim(stale_prepare).await });
	assert!(tokio::time::timeout(Duration::from_millis(200), &mut late_prepare).await.is_err(),
		"actual returning preparation must wait behind the freeze payment guard");
	assert!(!freezing.is_finished());
	release.notify_one(); barrier.await.unwrap().unwrap();
	let frozen = freezing.await.unwrap().unwrap_or_else(|e|
		panic!("actual zero-grant timeout must make progress through guarded freeze: {e:#}"));
	assert_eq!(db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().status,
		LightningHtlcSubscriptionStatus::Canceled);
	let rows = db.read(async |t| t.query("SELECT id,source,disposition FROM expiry_allocation WHERE freeze_set=$1", &[&frozen.id]).await.map_err(Into::into)).await.unwrap();
	for id in &send_ids {
		let row = rows.iter().find(|r| r.get::<_, &str>("id") == id.to_string()).unwrap();
		assert_eq!(row.get::<_, &str>("source"), format!("lightning-send:{hash}"));
		assert_eq!(row.get::<_, &str>("disposition"), "pay");
	}
	assert_eq!(server.freeze_current_expiry_backing(&roots, &terminals, budget).await.unwrap().id, frozen.id);
	let err = tokio::time::timeout(Duration::from_secs(10), late_prepare).await.unwrap().unwrap().unwrap_err();
	assert!(err.message().contains("canceled"), "{err}");
	assert!(db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().htlc_vtxos.is_empty());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	// A zero-grant recipient has no valid historical claim input. Still try
	// revealing its real saved preimage with a real (unrelated) input and a
	// destination signed by the recipient; the canceled-state fence must run
	// before storing any preimage or attempting a grant lookup/cosignature.
	let preimage = receiver.lightning_receive_checkpoint(hash).await.unwrap().unwrap().payment_preimage;
	let destination = ArkoorDestination { total_amount: sat(100_000), policy: VtxoPolicy::new_pubkey(key.public_key()) };
	let request = ArkoorPackageCosignRequest { requests: vec![ArkoorCosignRequest {
		input: send_ids[0], outputs: vec![destination.clone()], isolated_outputs: vec![], user_pub_nonces: vec![], use_checkpoint: true,
		attestation: ArkoorCosignAttestation::new(send_ids[0], &[&destination], &key),
	}] };
	let refused = server.claim_lightning_receive(hash, preimage, request, 1).await.unwrap_err();
	assert!(format!("{refused:#}").contains("incorrect state: canceled"), "{refused:#}");
	// Returning SDK drives the actual canceled invoice; it cannot prepare a
	// grant or collect the sender's already frozen value.
	let _ = receiver.try_claim_lightning_receive(hash, false).await;
	assert!(receiver.spendable_vtxos().await.unwrap().is_empty());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
}

#[tokio::test]
async fn zero_grant_actual_collection_wins_and_sender_is_never_refunded() {
	let ctx = TestContext::new("expiry_zero_grant/zero_grant_actual_collection_wins_and_sender_is_never_refunded").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(512); c.vtxopool.vtxo_lifetime = BlockDelta::new(2048);
			c.expiry_payout.grace_blocks = 0;
			// Expose the source API's arbitration, rather than letting the
			// periodic hold monitor cancel this native fixture first.
			c.invoice_check_interval = Duration::from_secs(3600);
			c.receive_htlc_forward_timeout = Duration::from_secs(60);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("absent_sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let receiver = ctx.bark_sdk("returning_receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let invoice = receiver.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	sender.pay_lightning_invoice(invoice.to_string(), None, false).await.unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert_eq!(sub.status, LightningHtlcSubscriptionStatus::Accepted);
	assert!(sub.htlc_vtxos.is_empty());
	assert!(sub.accepted_at.is_some());
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert!(attempt.is_self_payment() && !attempt.status.is_final());
	// Keep the real grant backing live after the sender Board expires. This
	// qualifies the opposite payment-guard order without treating an expired
	// or merely missing grant as already retired.
	let preimage = receiver.lightning_receive_checkpoint(hash).await.unwrap().unwrap().payment_preimage;
	let (key, _) = receiver.derive_store_next_keypair().await.unwrap();
	receiver.fallback_key_link(key.public_key()).await.unwrap();
	let granted = srv.get_public_rpc().await.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: sub.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta).into(),
		lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner().htlc_vtxos.into_iter().map(|bytes| Vtxo::<Full>::deserialize(&bytes).unwrap()).collect::<Vec<_>>();
	assert!(!granted.is_empty());
	let (output, _) = receiver.derive_store_next_keypair().await.unwrap();
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(granted.into_iter(), VtxoPolicy::new_pubkey(output.public_key()))
		.unwrap().generate_user_nonces(&[key]).unwrap();
	let cosign_request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &cosign_request.parts {
		for destination in part.outputs.iter().chain(part.isolated_outputs.iter()) {
			let pubkey = VtxoPolicy::deserialize(&destination.policy).unwrap().user_pubkey();
			receiver.fallback_key_link(pubkey).await.unwrap();
		}
	}

	let coins = sender.all_vtxos().await.unwrap().into_iter().filter(|w| !matches!(w.state, VtxoState::Spent)).collect::<Vec<_>>();
	let terminals = coins.iter().map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let roots = coins.iter().map(|w| VtxoId::from(w.vtxo.chain_anchor())).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
	let send_ids = coins.iter().filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	assert!(!send_ids.is_empty());
	let expiry = coins.iter().map(|w| w.vtxo.expiry_height().to_u32()).max().unwrap();
	drop(sender);
	let config = srv.config().clone(); srv.stop().await.unwrap();
	let server = server::Server::start(config).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		while server.chain_tip().height.to_u32() != core.get_block_count().unwrap() as u32 {
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	let budget = BackingSnapshotLimits { max_rows: 1000, max_serialized_bytes: 10_000_000 };

	assert!(server.freeze_current_expiry_backing(&roots, &terminals, budget).await.is_err(),
		"actual nonempty grants must remain refused before collection/retirement");
	let entered = Arc::new(tokio::sync::Notify::new()); let release = Arc::new(tokio::sync::Notify::new());
	let barrier_db = db.clone(); let enter = entered.clone(); let leave = release.clone();
	let barrier = tokio::spawn(async move { barrier_db.write(async |t| {
		t.lock_htlc_settlements().await?; enter.notify_one(); leave.notified().await; Ok(())
	}).await });
	entered.notified().await;
	let mut rpc = srv.get_public_rpc().await;
	let claiming = tokio::spawn(async move { rpc.claim_lightning_receive(protos::ClaimLightningReceiveRequest {
		payment_hash: hash.to_vec(), payment_preimage: preimage.to_vec(), cosign_request: Some(cosign_request),
	}).await });
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let waiting: bool = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory')", &[]).await?.get(0))).await.unwrap();
			if waiting { break; }
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}).await.unwrap();
	let refused = server.freeze_current_expiry_backing(&roots, &terminals, budget).await.err().unwrap();
	assert!(format!("{refused:#}").contains("payment is in progress"), "{refused:#}");
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	release.notify_one(); barrier.await.unwrap().unwrap();
	tokio::time::timeout(Duration::from_secs(10), claiming).await.unwrap().unwrap().unwrap();
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_some());
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			if db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().status
				== LightningHtlcSubscriptionStatus::Settled { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	let frozen = server.freeze_current_expiry_backing(&roots, &terminals, budget).await.unwrap();
	let rows = db.read(async |t| t.query("SELECT id,source,disposition,payout_sat FROM expiry_allocation WHERE freeze_set=$1", &[&frozen.id]).await.map_err(Into::into)).await.unwrap();
	for id in &send_ids {
		let row = rows.iter().find(|r| r.get::<_, &str>("id") == id.to_string()).unwrap();
		assert_eq!(row.get::<_, &str>("disposition"), "operator");
		assert!(row.get::<_, &str>("source").starts_with("operator:PaidLightningSend"));
		assert_eq!(row.get::<_, i64>("payout_sat"), 0);
	}
	assert_eq!(db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().status,
		LightningHtlcSubscriptionStatus::Settled);
}
