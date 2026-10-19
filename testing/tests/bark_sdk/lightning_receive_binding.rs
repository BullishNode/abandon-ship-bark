//! Normal successful receive-for-address retains the intended recipient in the
//! server's durable receive binding, independently of the provider's key.

use std::time::Duration;
use std::sync::Arc;

use ark::{ProtocolEncoding, ServerVtxo, Vtxo};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;
use ark_testing::{btc, sat, TestContext};
use bark::actions::lightning::receive::LightningReceiveState;
use server::database::Db;
use ark_testing::daemon::captaind::{self, ArkClient};
use server_rpc::protos;

/// Pause at the normal completed-grant reply boundary. The grants themselves
/// are real and committed; this controller never changes a financial row.
#[derive(Clone)]
struct PausePreparedInternalReceive {
	arrived: Arc<tokio::sync::Notify>,
	resume: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for PausePreparedInternalReceive {
	async fn prepare_lightning_receive_claim(
		&self, upstream: &mut ArkClient, request: protos::PrepareLightningReceiveClaimRequest,
	) -> Result<protos::PrepareLightningReceiveClaimResponse, tonic::Status> {
		let response = upstream.prepare_lightning_receive_claim(request).await?.into_inner();
		self.arrived.notify_one();
		self.resume.notified().await;
		Ok(response)
	}
}

/// An admitted receive claim request survives a restart while the tree writer is pending,
/// and completes without either wallet returning.
#[tokio::test]
async fn receive_binding_internal_original_request_resumes_without_either_wallet() {
	let ctx = TestContext::new("bark_sdk/receive_binding_internal_callerless_restart").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let pause = PausePreparedInternalReceive {
		arrived: Arc::new(tokio::sync::Notify::new()),
		resume: Arc::new(tokio::sync::Notify::new()),
	};
	let proxy = srv.start_proxy_no_mailbox(pause.clone()).await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &proxy)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(50_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (original, protocol, subscription, pending_pid) = {
		let operation = async {
			tokio::join!(sender.pay_lightning_invoice(invoice, None, true),
				recipient.try_claim_lightning_receive(hash, true))
		};
		tokio::pin!(operation);
		tokio::select! {
			_ = pause.arrived.notified() => {},
			result = &mut operation => panic!("internal operation finished before the normal grant pause: {result:?}"),
			_ = tokio::time::sleep(Duration::from_secs(60)) => panic!("normal internal grants were not prepared"),
		}
		// Lock only the existing infrastructure barrier, after real grants
		// commit. No subscription, attempt, coin, or settlement is fabricated.
		db.write(async |t| {
			t.execute("SELECT pg_advisory_xact_lock(2)", &[]).await?;
			pause.resume.notify_one();
			let admitted = async {
				loop {
					let row = db.read(async |tx| Ok(tx.query_opt("SELECT r.request,r.protocol_version,r.subscription_id, \
						(SELECT l.pid FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid \
						WHERE l.locktype='advisory' AND l.classid=0 AND l.objid=2 AND NOT l.granted \
						AND a.datname=current_database() AND a.query LIKE '%INSERT INTO htlc_settlement%' LIMIT 1) AS pending_pid \
						FROM expiry_lightning_receive_replay r WHERE r.payment_hash=$1", &[&hash.to_string()]).await?)).await?;
					if let Some(row) = row {
						if let Some(pid) = row.get::<_, Option<i32>>("pending_pid") {
							break Ok::<_, anyhow::Error>((row.get::<_, Vec<u8>>("request"),
								row.get::<_, i64>("protocol_version"), row.get::<_, i64>("subscription_id"), pid));
						}
					}
					tokio::time::sleep(Duration::from_millis(20)).await;
				}
			};
			let admitted = tokio::select! {
				result = tokio::time::timeout(Duration::from_secs(30), admitted) => result.expect("original claim never reached its actual pending WAL writer")?,
				result = &mut operation => panic!("claim finished while its actual WAL writer was held: {result:?}"),
			};
			let before = t.query_one("SELECT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal, \
				EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE payment_hash=$1 AND materialized) AS materialized", &[&hash.to_string()]).await?;
			assert!(!before.get::<_, bool>("wal") && !before.get::<_, bool>("materialized"),
				"recipient materialization and preimage must remain invisible together before commit");
			srv.stop().await?;
			Ok(admitted)
		}).await.unwrap()
	}; // Both original client futures end before the server restarts.
	drop(sender);
	drop(recipient);
	drop(proxy);
	tokio::time::timeout(Duration::from_secs(10), async {
		while db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pending_pid]).await?.get::<_, bool>(0))).await.unwrap() {
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}).await.expect("interrupted server writer did not close after its infrastructure gate was released");
	let rolled_back = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal, \
		EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE payment_hash=$1 AND materialized) AS materialized", &[&hash.to_string()]).await?)).await.unwrap();
	assert!(!rolled_back.get::<_, bool>("wal") && !rolled_back.get::<_, bool>("materialized"));
	srv.start().await.unwrap();
	let recovered = tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			let row = db.read(async |t| Ok(t.query_one("SELECT r.request,r.protocol_version,s.status::TEXT AS receive_status, \
				a.status::TEXT AS attempt_status,COUNT(c.source_vtxo_id) AS bindings,BOOL_AND(c.materialized) AS materialized, \
				BOOL_AND(h.offchain_resolution::TEXT='fulfilled') AS fulfilled, \
				COALESCE(SUM(v.amount),0)::BIGINT AS amount, \
				EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal \
				FROM expiry_lightning_receive_replay r JOIN lightning_htlc_subscription s ON s.id=r.subscription_id \
				JOIN expiry_lightning_internal_funding f ON f.subscription_id=s.id \
				JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=s.id \
				JOIN expiry_lightning_receive_claim c ON c.subscription_id=s.id \
				JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id JOIN htlc_vtxo h ON h.id=source.id \
				LEFT JOIN vtxo v ON v.vtxo_id=c.output_vtxo_id \
				WHERE r.subscription_id=$2 GROUP BY r.subscription_id,r.request,r.protocol_version,s.status,a.status",
				&[&hash.to_string(),&subscription]).await?)).await.unwrap();
			if row.get::<_, &str>("receive_status") == "settled" && row.get::<_, &str>("attempt_status") == "succeeded" { break row; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("original admitted internal receive required a returning wallet");
	assert_eq!(recovered.get::<_, Vec<u8>>("request"), original);
	assert_eq!(recovered.get::<_, i64>("protocol_version"), protocol);
	assert!(recovered.get::<_, i64>("bindings") > 0);
	assert!(recovered.get::<_, bool>("materialized") && recovered.get::<_, bool>("fulfilled") && recovered.get::<_, bool>("wal"));
	assert_eq!(recovered.get::<_, i64>("amount"), 50_000);
	let actual = db.read(async |t| Ok(t.query("SELECT c.output_vtxo,v.vtxo FROM expiry_lightning_receive_claim c \
		JOIN vtxo v ON v.vtxo_id=c.output_vtxo_id WHERE c.subscription_id=$1", &[&subscription]).await?)).await.unwrap();
	assert_eq!(actual.len() as i64, recovered.get::<_, i64>("bindings"));
	for row in actual {
		let promised = Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
		let committed = Vtxo::<Full>::deserialize(row.get("vtxo")).unwrap();
		assert_eq!(committed.id(), promised.id());
		assert_eq!(committed.user_pubkey(), promised.user_pubkey());
		assert_eq!(committed.amount(), promised.amount());
	}
}

#[tokio::test]
async fn receive_binding_normal_external_claim_keeps_the_actual_recipient() {
	let ctx = TestContext::new("bark_sdk/receive_binding_normal_external_claim").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let provider = ctx.bark_sdk("provider", &srv).boarded(sat(100_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let address = recipient.new_address().await.unwrap();
	let recipient_key = address.policy().user_pubkey();
	assert_ne!(provider.fallback_destination().await.unwrap().account_xpub,
		recipient.fallback_destination().await.unwrap().account_xpub);
	let invoice = provider.bolt11_invoice_for_address(sat(50_000), address, None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	lightning.sync().await;
	let (paid, claimed) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(
			lightning.external.try_pay_bolt11(invoice.to_string()),
			provider.try_claim_lightning_receive(hash, true),
		)
	}).await.expect("normal receive did not complete");
	paid.expect("external payment failed");
	assert!(matches!(claimed.unwrap(), LightningReceiveState::Settled(_)));
	recipient.sync().await;
	let coins = recipient.spendable_vtxos().await.unwrap();
	assert!(!coins.is_empty(), "normal delivery must give the recipient its signed coins");
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let original_request = db.read(async |t| Ok(t.query_one("SELECT request,protocol_version FROM expiry_lightning_receive_replay WHERE payment_hash=$1",
		&[&hash.to_string()]).await?)).await.unwrap();
	assert!(!original_request.get::<_, &[u8]>("request").is_empty());
	assert!(original_request.get::<_, i64>("protocol_version") > 0);
	let bindings = db.read(async |t| Ok(t.query("SELECT c.source_vtxo_id,c.output_vtxo_id,c.output_vtxo, \
		c.materialized,source.vtxo AS source_vtxo,actual.vtxo AS actual_vtxo, \
		h.offchain_resolution::TEXT AS resolution,s.status::TEXT AS status \
		FROM expiry_lightning_receive_claim c \
		JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id \
		JOIN htlc_vtxo h ON h.id=source.id \
		JOIN vtxo actual ON actual.vtxo_id=c.output_vtxo_id \
		JOIN lightning_htlc_subscription s ON s.id=c.subscription_id \
		WHERE c.payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
	assert!(!bindings.is_empty());
	let mut received = bitcoin::Amount::ZERO;
	for row in &bindings {
		let original = Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
		let actual = Vtxo::<Full>::deserialize(row.get("actual_vtxo")).unwrap();
		let source = ServerVtxo::<Full>::deserialize(row.get("source_vtxo")).unwrap().try_into_user_vtxo().unwrap();
		assert!(row.get::<_, bool>("materialized"));
		assert_eq!(row.get::<_, &str>("resolution"), "fulfilled");
		assert_eq!(row.get::<_, &str>("status"), "settled");
		assert_eq!(original.id(), actual.id());
		assert_eq!(original.user_pubkey(), recipient_key);
		assert_eq!(actual.user_pubkey(), recipient_key);
		assert_ne!(source.user_pubkey(), recipient_key);
		assert_eq!(source.amount(), original.amount());
		assert!(coins.iter().any(|coin| coin.id() == actual.id()));
		received += actual.amount();
	}
	assert_eq!(coins.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), received);
	// A completed normal action is idempotent; it cannot mint another binding.
	assert!(matches!(provider.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
	let count = db.read(async |t| Ok(t.query_one("SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE payment_hash=$1", &[&hash.to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, bindings.len() as i64);
	let retry_request = db.read(async |t| Ok(t.query_one("SELECT request,protocol_version FROM expiry_lightning_receive_replay WHERE payment_hash=$1",
		&[&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(original_request.get::<_, &[u8]>("request"), retry_request.get::<_, &[u8]>("request"));
	assert_eq!(original_request.get::<_, i64>("protocol_version"), retry_request.get::<_, i64>("protocol_version"));
}

/// A normal intra-Ark receive uses its durable funding link after the payer's
/// attempt completes. Replaying maintenance must not consult the canceled native
/// hold invoice, duplicate the binding, or write another identical status row.
#[tokio::test]
async fn receive_binding_normal_internal_claim_and_reconciliation_are_idempotent() {
	let ctx = TestContext::new("bark_sdk/receive_binding_normal_internal_claim").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(50_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let (paid, claimed) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(
			sender.pay_lightning_invoice(invoice, None, true),
			recipient.try_claim_lightning_receive(hash, true),
		)
	}).await.expect("normal internal receive did not complete");
	paid.unwrap();
	assert!(matches!(claimed.unwrap(), LightningReceiveState::Settled(_)));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let row = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_one("SELECT s.id,s.status::TEXT,s.updated_at,a.status::TEXT AS attempt_status, \
				COUNT(c.source_vtxo_id) AS bindings,BOOL_AND(c.materialized) AS materialized \
				FROM lightning_htlc_subscription s JOIN expiry_lightning_internal_funding f ON f.subscription_id=s.id \
				JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=s.id \
				JOIN expiry_lightning_receive_claim c ON c.subscription_id=s.id AND c.payment_hash=s.payment_hash \
				WHERE s.payment_hash=$1 GROUP BY s.id,a.status", &[&hash.to_string()]).await?)).await.unwrap();
			if row.get::<_, &str>("status") == "settled" { break row; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("internal receive did not reach its actual settled subscription");
	assert_eq!(row.get::<_, &str>("attempt_status"), "succeeded");
	assert!(row.get::<_, i64>("bindings") > 0 && row.get::<_, bool>("materialized"));
	let before: chrono::DateTime<chrono::Local> = row.get("updated_at");
	assert!(db.write(async |t| t.reconcile_completed_internal_receives(128).await).await.unwrap().is_empty());
	let after = db.read(async |t| Ok(t.query_one("SELECT updated_at FROM lightning_htlc_subscription WHERE id=$1", &[&row.get::<_, i64>("id")]).await?.get::<_, chrono::DateTime<chrono::Local>>(0))).await.unwrap();
	assert_eq!(before, after, "identical reconciliation must not change status history");
	let replay = db.read(async |t| Ok(t.query_one("SELECT COUNT(*) AS total,BOOL_AND(octet_length(request)>0) AS complete \
		FROM expiry_lightning_receive_replay WHERE payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(replay.get::<_, i64>("total"), 1);
	assert!(replay.get::<_, bool>("complete"));
	let receipt = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(receipt.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(50_000));
	let mut external = lightning.internal.grpc_client().await;
	let pays = external.list_send_pays(cln_rpc::ListsendpaysRequest {
		payment_hash: Some(hash.to_vec()), ..Default::default()
	}).await.unwrap().into_inner();
	assert!(pays.payments.is_empty(), "actual internal receive must not be sent through external CLN");
}
