//! Native outgoing evidence with a real failed external attempt and prepared local retry.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bitcoincore_rpc::RpcApi;
use cln_rpc::plugins::hold;
use tonic::body::Body;
use tonic::codegen::{http, Service};
use tonic::server::NamedService;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

use ark::{ProtocolEncoding, ServerVtxo, Vtxo, VtxoPolicy};
use ark::arkoor::package::ArkoorPackageBuilder;
use ark::lightning::Preimage;
use ark::vtxo::Full;
use bitcoin_ext::BlockDelta;
use server::database::{Db, SpendState};
use server::database::carrier_snapshot::BackingSnapshotLimits;
use server::database::expiry_ownership::{ExpirySource, OwnershipContext, OwnershipDisposition, RetirementRule};
use server::database::ln::LightningPaymentStatus;
use server::ln::SendRefundEvidence;
use server_rpc::protos;

use ark_testing::{btc, sat, TestContext};

/// Declared observation faults only. Xpay, invoice and sendpay calls still
/// reach the actual owned node; no attempt or payment row is fabricated.
#[derive(Clone)]
struct ObservationRelay {
	upstream: Channel,
	unknown: Arc<AtomicBool>,
	silent: Arc<AtomicBool>,
}

impl NamedService for ObservationRelay {
	const NAME: &'static str = "cln.Node";
}

impl Service<http::Request<Body>> for ObservationRelay {
	type Response = http::Response<Body>;
	type Error = Infallible;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
	fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { Poll::Ready(Ok(())) }
	fn call(&mut self, request: http::Request<Body>) -> Self::Future {
		let relay = self.clone();
		Box::pin(async move {
			if request.uri().path() == "/cln.Node/ListPays" {
				if relay.silent.load(Ordering::SeqCst) { std::future::pending::<()>().await; }
				if relay.unknown.load(Ordering::SeqCst) {
					// Empty ListpaysResponse: a real gRPC Unknown observation, not
					// transport failure. Five zero bytes are its empty message frame.
					return Ok(http::Response::builder().header("content-type", "application/grpc")
						.header("grpc-status", "0").body(Body::new(axum::body::Body::from(vec![0u8; 5]))).unwrap());
				}
			}
			let mut upstream = relay.upstream;
			poll_fn(|cx| upstream.poll_ready(cx)).await.unwrap();
			Ok(match upstream.call(request).await {
				Ok(response) => response,
				Err(error) => tonic::Status::unavailable(error.to_string()).into_http(),
			})
		})
	}
}

#[tokio::test]
async fn mixed_real_failed_external_then_prepared_local_refund_keeps_all_grants() {
	mixed_case(true).await;
}

#[tokio::test]
async fn mixed_real_unknown_requires_final_failure_and_dispatch_fence() {
	mixed_case(false).await;
}

#[tokio::test]
async fn mixed_real_paid_external_outgoing_is_never_refundable() {
	let ctx = TestContext::new("expiry_mixed/mixed_real_paid_external_outgoing_is_never_refundable").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.no_vtxo_pool().funded(btc(1)).cfg(|c| c.vtxo_lifetime = BlockDelta::new(512)).create().await;
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let invoice = lightning.external.invoice(Some(sat(100_000)), "mixed_paid_observer", "owned native paid observation").await;
	sender.pay_lightning_invoice(invoice, None, true).await.unwrap();
	lightning.external.wait_invoice_paid("mixed_paid_observer").await;
	let rows = db.read(async |t| Ok(t.query("SELECT v.vtxo FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE h.direction='incoming'", &[]).await?)).await.unwrap();
	let htlcs = rows.iter().map(|r| Vtxo::<Full>::deserialize(r.get::<_, &[u8]>("vtxo")).unwrap()).collect::<Vec<_>>();
	assert!(!htlcs.is_empty());
	let hash = htlcs[0].policy().as_server_htlc_send().unwrap().payment_hash;
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_some());
	let attempts = db.read(async |t| t.get_payment_attempts_by_payment_hash(hash).await).await.unwrap();
	assert!(attempts.iter().any(|a| a.status == LightningPaymentStatus::Succeeded));
	let mut roots = Vec::new();
	for coin in &htlcs {
		let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap();
		if !roots.iter().any(|r: &ServerVtxo| r.id() == root.id()) { roots.push(root); }
	}
	let inventory = db.read(async |t| t.backing_snapshot(&roots, BackingSnapshotLimits {
		max_rows: 1000, max_serialized_bytes: 10_000_000,
	}).await).await.unwrap();
	let ids = htlcs.iter().map(Vtxo::id).collect::<Vec<_>>();
	drop(sender);
	let config = srv.config().clone(); srv.stop().await.unwrap();
	let server = server::Server::start(config).await.unwrap();
	// Authoritative Paid dominates an unavailable node; no outcome is inferred
	// from the node's absence or from the expiry clock.
	let mut unavailable = attempts.iter().map(|a| a.lightning_node_id).collect();
	let evidence = server.observe_expiry_send_evidence(&inventory, &ids, &mut unavailable).await.unwrap();
	assert!(matches!(evidence.get(&hash), Some(SendRefundEvidence::Paid(_))), "{evidence:?}");
}

async fn mixed_case(local_retry: bool) {
	let name = if local_retry { "mixed_real_failed_external_then_prepared_local_refund_keeps_all_grants" }
		else { "mixed_real_unknown_requires_final_failure_and_dispatch_fence" };
	let ctx = TestContext::new(format!("expiry_mixed/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let details = lightning.internal.grpc_details().await;
	let upstream = Channel::builder(details.uri.parse().unwrap()).tls_config(ClientTlsConfig::new()
		.ca_certificate(Certificate::from_pem(std::fs::read(&details.server_cert_path).unwrap()))
		.identity(Identity::from_pem(std::fs::read(&details.client_cert_path).unwrap(), std::fs::read(&details.client_key_path).unwrap()))
	).unwrap().connect().await.unwrap();
	let relay = ObservationRelay { upstream, unknown: Arc::new(AtomicBool::new(false)), silent: Arc::new(AtomicBool::new(false)) };
	let port = ark_testing::ports::pick_port();
	tokio::spawn(tonic::transport::Server::builder().add_service(relay.clone()).serve(([127, 0, 0, 1], port).into()));
	let uri = format!("http://127.0.0.1:{port}");
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(btc(10)).cfg(move |c| {
			c.vtxo_lifetime = BlockDelta::new(512); c.vtxopool.vtxo_lifetime = BlockDelta::new(512);
			c.cln_array[0].uri = uri.parse().unwrap(); c.cln_xpay_timeout = Duration::ZERO;
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let hash = Preimage::random().compute_payment_hash();
	let mut external_hold = lightning.external.hold_client().await;
	let external = external_hold.invoice(hold::InvoiceRequest {
		payment_hash: hash.to_vec(), amount_msat: 100_000_000,
		description: Some(hold::invoice_request::Description::Memo("mixed native external first".into())),
		min_final_cltv_expiry: Some(18), expiry: Some(120), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	lightning.sync().await;
	sender.pay_lightning_invoice(external, None, false).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let list = external_hold.list(hold::ListRequest {
				constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
			}).await.unwrap().into_inner();
			if list.invoices.iter().any(|i| i.state() == hold::InvoiceState::Accepted) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
	external_hold.cancel(hold::CancelRequest { payment_hash: hash.to_vec() }).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
			if attempt.status == LightningPaymentStatus::Failed { assert!(!attempt.is_self_payment()); break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
	let sender_ids = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	assert!(!sender_ids.is_empty());
	if !local_retry {
		let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&sender_ids).await).await.unwrap();
		let mut roots = Vec::new();
		for coin in &htlcs {
			let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.vtxo.chain_anchor().into()).await?.vtxo)).await.unwrap();
			if !roots.iter().any(|r: &ServerVtxo| r.id() == root.id()) { roots.push(root); }
		}
		let inventory = db.read(async |t| t.backing_snapshot(&roots, BackingSnapshotLimits {
			max_rows: 1000, max_serialized_bytes: 10_000_000,
		}).await).await.unwrap();
		let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
		let fenced_after = attempt.invoice_expires_at.unwrap() + attempt.retry_for.unwrap() + Duration::from_secs(15);
		let config = srv.config().clone(); srv.stop().await.unwrap();
		let server = server::Server::start(config).await.unwrap();
		wait_for_node(&server).await;
		assert!(chrono::Local::now() < fenced_after, "native unknown test must reach the unfenced window");
		relay.unknown.store(true, Ordering::SeqCst);
		let unknown = server.observe_expiry_send_evidence(&inventory, &sender_ids, &mut BTreeSet::new()).await.unwrap();
		assert!(matches!(unknown.get(&hash), Some(SendRefundEvidence::Unresolved(_))),
			"unknown plus final failure is still nonrefundable before the dispatch fence: {unknown:?}");
		relay.unknown.store(false, Ordering::SeqCst); relay.silent.store(true, Ordering::SeqCst);
		let mut unavailable = BTreeSet::new();
		let silent = server.observe_expiry_send_evidence(&inventory, &sender_ids, &mut unavailable).await.unwrap();
		assert!(matches!(silent.get(&hash), Some(SendRefundEvidence::Unresolved(_))));
		assert!(unavailable.contains(&attempt.lightning_node_id), "an actual silent read must mark its node unavailable");
		relay.silent.store(false, Ordering::SeqCst); relay.unknown.store(true, Ordering::SeqCst);
		while chrono::Local::now() < fenced_after { tokio::time::sleep(Duration::from_millis(200)).await; }
		let still_unavailable = server.observe_expiry_send_evidence(&inventory, &sender_ids, &mut unavailable).await.unwrap();
		assert!(matches!(still_unavailable.get(&hash), Some(SendRefundEvidence::Unresolved(_))),
			"expiry cannot turn an unavailable node into a final verdict");
		let fenced = server.observe_expiry_send_evidence(&inventory, &sender_ids, &mut BTreeSet::new()).await.unwrap();
		assert!(matches!(fenced.get(&hash), Some(SendRefundEvidence::Refundable)),
			"durable actual final failure plus the explicit fence supplies bounded outgoing progress: {fenced:?}");
		return;
	}
	let local = srv.get_public_rpc().await.start_lightning_receive(protos::StartLightningReceiveRequest {
		payment_hash: hash.to_vec(), amount_sat: 100_000, min_cltv_delta: 18,
		mailbox_id: Some(receiver.mailbox_identifier().serialize()), ..Default::default()
	}).await.unwrap().into_inner().bolt11;
	srv.get_public_rpc().await.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice: local, htlc_vtxo_ids: sender_ids.iter().map(|id| id.to_bytes().to_vec()).collect(),
		payment_amount_sat: 100_000, mailbox_id: None, retry_for_secs: Some(0),
	}).await.unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	let (key, _) = receiver.derive_store_next_keypair().await.unwrap();
	// Require the canonical per-key acknowledgment before preparing the grant.
	receiver.fallback_key_link(key.public_key()).await.unwrap();
	let grants = srv.get_public_rpc().await.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: sub.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta).into(),
		lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner().htlc_vtxos.into_iter().map(|v| Vtxo::<Full>::deserialize(&v).unwrap()).collect::<Vec<_>>();
	assert!(!grants.is_empty());
	let attempts = db.read(async |t| t.get_payment_attempts_by_payment_hash(hash).await).await.unwrap();
	assert_eq!(attempts.len(), 2);
	assert!(attempts.iter().any(|a| !a.is_self_payment() && a.status == LightningPaymentStatus::Failed));
	assert!(attempts.iter().any(|a| a.is_self_payment() && !a.status.is_final()));
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&sender_ids).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<_>>();
	let core = ctx.bitcoind().sync_client();
	let mut full_inputs = Vec::new();
	let mut keys = Vec::new();
	for id in &sender_ids {
		let input = sender.get_full_vtxo(*id).await.unwrap();
		keys.push(sender.get_vtxo_key(&input).await.unwrap());
		full_inputs.push(input);
	}
	let output = sender.derive_store_next_keypair().await.unwrap().0.public_key();
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(full_inputs, VtxoPolicy::new_pubkey(output))
		.unwrap().generate_user_nonces(&keys).unwrap();
	let revocation = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &revocation.parts {
		for requested in part.outputs.iter().chain(part.isolated_outputs.iter()) {
			sender.fallback_key_link(VtxoPolicy::deserialize(&requested.policy).unwrap().user_pubkey()).await.unwrap();
		}
	}
	let deadline = htlcs.iter().map(|v| v.policy().as_server_htlc_send().unwrap().htlc_expiry)
		.chain(grants.iter().map(|v| v.policy().as_server_htlc_recv().unwrap().htlc_expiry)).max().unwrap().to_u32();
	ctx.generate_blocks(deadline.saturating_sub(core.get_block_count().unwrap() as u32) + 1).await;
	let legacy = srv.get_public_rpc().await.request_lightning_pay_htlc_revocation(revocation).await.unwrap_err();
	assert!(legacy.message().contains("can still be exited"),
		"the legacy two-step wrapper must retain the actual grant-path refusal: {legacy}");
	let mut roots = Vec::new();
	let mut funding = BTreeMap::new();
	for coin in htlcs.iter().chain(&grants) {
		let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap();
		if !roots.iter().any(|r: &ServerVtxo| r.id() == root.id()) { roots.push(root); }
		funding.insert(coin.chain_anchor().txid, core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap());
	}
	drop(sender); drop(receiver);
	let expiry = htlcs.iter().chain(&grants).map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32 + 1).await;
	let inventory = db.read(async |t| t.backing_snapshot(&roots, BackingSnapshotLimits {
		max_rows: 1000, max_serialized_bytes: 10_000_000,
	}).await).await.unwrap();
	assert!(inventory.regular.iter().filter(|f| grants.iter().any(|g| g.id() == f.state.vtxo_id))
		.all(|f| f.state.spend_state == SpendState::HtlcRecvUnclaimed && f.state.oor_spent_txid.is_none()));
	let config = srv.config().clone();
	srv.stop().await.unwrap();
	// Restart this same owned native server in-process to call its read-only hook.
	let server = server::Server::start(config).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		while server.chain_tip().height.to_u32() < core.get_block_count().unwrap() as u32 {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
	wait_for_node(&server).await;
	let evidence = server.observe_expiry_send_evidence(&inventory, &sender_ids, &mut BTreeSet::new()).await.unwrap();
	assert!(matches!(evidence.get(&hash), Some(SendRefundEvidence::Refundable)),
		"actual external failure must be separable from the unswept local grants: {evidence:?}");
	let context = OwnershipContext { tip: core.get_block_count().unwrap() as u32, grace: 0,
		max_dependencies: 1000, server_pubkey: htlcs[0].server_pubkey(), funding: &funding, canceled_receives: &BTreeSet::new() };
	let proposals = db.read(async |t| t.expiry_ownership_proposal(&inventory, &sender_ids, &context, &evidence).await).await.unwrap();
	let mut all_grants = grants.iter().map(Vtxo::id).collect::<Vec<_>>(); all_grants.sort();
	for proposal in proposals {
		assert!(matches!(proposal.disposition, OwnershipDisposition::Customer(input) if input.source == ExpirySource::LightningSend(hash)));
		assert_eq!(proposal.dependencies.len(), 1);
		assert_eq!(proposal.dependencies[0].ids, all_grants);
		assert_eq!(proposal.dependencies[0].rule, RetirementRule::ConfirmedOrSameCarrier);
	}
}

async fn wait_for_node(server: &server::Server) {
	// A separate actual hold invoice establishes the configured node is ready.
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			if server.start_lightning_receive(Preimage::random().compute_payment_hash(), sat(1000),
				BlockDelta::new(18), None, None, None).await.is_ok() { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
}
