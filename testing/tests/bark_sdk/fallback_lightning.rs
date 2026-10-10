use std::collections::BTreeSet;
use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::Notify;

use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;
use ark::lightning::{PaymentHash, Preimage};
use ark_testing::{Captaind, TestContext, btc, sat};
use ark_testing::context::LightningPaymentSetup;
use ark_testing::daemon::captaind::{ArkClient, MailboxClient, proxy::{ArkRpcProxy, MailboxRpcProxy}};
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use bdk_wallet::{KeychainKind, SignOptions};
use bdk_wallet::template::Bip84;
use bitcoin::{Address, FeeRate, Network, OutPoint, ScriptBuf, Transaction, Txid};
use bitcoin::bip32::Xpriv;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use cln_rpc::plugins::hold;
use cln_rpc::plugins::hold::hold_client::HoldClient;
use server_rpc::protos;
use tonic::body::Body;
use tonic::codegen::{http, Service};
use tonic::server::NamedService;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity, Server};

#[derive(Clone)]
struct InterruptedReceiveClaim {
	reached: Arc<Notify>,
	request: Arc<Mutex<Option<protos::ClaimLightningReceiveRequest>>>,
	reveal_preimage: bool,
	/// The server's own hold plugin, to fail the incoming HTLCs back to the
	/// payer just before the claim arrives.
	fail_incoming: Option<HoldClient<Channel>>,
}

#[async_trait::async_trait]
impl ArkRpcProxy for InterruptedReceiveClaim {
	async fn claim_lightning_receive(
		&self, upstream: &mut ArkClient, request: protos::ClaimLightningReceiveRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		*self.request.lock().unwrap() = Some(request.clone());
		if let Some(hold) = &self.fail_incoming {
			hold.clone().cancel(hold::CancelRequest { payment_hash: request.payment_hash.clone() }).await
				.expect("the server's node must fail the held incoming HTLCs back");
		}
		if self.reveal_preimage {
			upstream.claim_lightning_receive(request).await
				.expect_err("the claim must not complete");
		}
		self.reached.notify_one();
		// Keep the client at its real claim checkpoint until the test drops it.
		std::future::pending().await
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveOutcome {
	/// The server collects the external payment; the Ark claim rolls back.
	Collected,
	/// The recipient disappears before disclosing the preimage.
	Undisclosed,
	/// The recipient prepares the claim and disappears before disclosing
	/// the preimage. Nobody cancels: the hold plugin's own HTLC deadline
	/// fails the payment back to the external payer.
	UndisclosedUntilHoldDeadline,
	/// The recipient discloses the preimage, but the incoming HTLCs were
	/// already failed back, so the server never collects the payment.
	Uncollected,
	/// The server collects the external payment, but cannot record that
	/// until later.
	CollectedStatusLost,
}

/// External Lightning has paid, but the Ark claim transaction rolls back.
/// The recipient disappears with only the original HTLC-receive entitlement.
#[tokio::test]
async fn fallback_settled_lightning_receive_without_claim_commit() {
	Box::pin(interrupted_receive(ReceiveOutcome::Collected)).await;
}

#[tokio::test]
async fn fallback_unsettled_lightning_receive_is_not_paid() {
	Box::pin(interrupted_receive(ReceiveOutcome::Undisclosed)).await;
}

/// The external counterpart of an abandoned intra-Ark receive: the payer is
/// refunded by the hold plugin's own deadline, and the recipient is not paid.
#[tokio::test]
async fn fallback_prepared_receive_refunds_external_payer_at_hold_deadline() {
	Box::pin(interrupted_receive(ReceiveOutcome::UndisclosedUntilHoldDeadline)).await;
}

/// A recorded preimage alone is not a collected payment. The rounds wallet
/// must not pay out a receive whose payer was refunded.
#[tokio::test]
async fn fallback_uncollected_lightning_receive_is_held() {
	Box::pin(interrupted_receive(ReceiveOutcome::Uncollected)).await;
}

/// The hold is not a refusal: once the server records the collection it
/// already made, the recipient is paid.
#[tokio::test]
async fn fallback_collected_lightning_receive_pays_after_status_recovers() {
	Box::pin(interrupted_receive(ReceiveOutcome::CollectedStatusLost)).await;
}

async fn interrupted_receive(outcome: ReceiveOutcome) {
	let name = match outcome {
		ReceiveOutcome::Collected => "fallback_settled_lightning_receive_without_claim_commit",
		ReceiveOutcome::Undisclosed => "fallback_unsettled_lightning_receive_is_not_paid",
		ReceiveOutcome::UndisclosedUntilHoldDeadline => "fallback_prepared_receive_refunds_external_payer_at_hold_deadline",
		ReceiveOutcome::Uncollected => "fallback_uncollected_lightning_receive_is_held",
		ReceiveOutcome::CollectedStatusLost => "fallback_collected_lightning_receive_pays_after_status_recovers",
	};
	let disclosed = !matches!(outcome, ReceiveOutcome::Undisclosed | ReceiveOutcome::UndisclosedUntilHoldDeadline);
	let collected = matches!(outcome, ReceiveOutcome::Collected | ReceiveOutcome::CollectedStatusLost);
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(512);
			c.vtxopool.vtxo_lifetime = BlockDelta::new(512);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let reached = Arc::new(Notify::new());
	let claim_request = Arc::new(Mutex::new(None));
	let fail_incoming = match outcome {
		ReceiveOutcome::Uncollected => Some(lightning.internal.hold_client().await),
		_ => None,
	};
	let proxy = srv.start_proxy_no_mailbox(InterruptedReceiveClaim {
		reached: reached.clone(), request: claim_request.clone(), reveal_preimage: disclosed, fail_incoming,
	}).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("recipient", &proxy.address).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	let invoice = wallet.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let payment_hash = PaymentHash::from(&invoice);
	let external = lightning.external;
	let paying = tokio::spawn(async move { external.try_pay_bolt11(invoice.to_string()).await });
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
				.await.unwrap().unwrap();
			if sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Accepted { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("external Lightning must fund the receive");

	// Fail only the cooperative spend of the granted receive, after the
	// preimage and external payment have committed. No entitlement, payment
	// status or sweep evidence is invented by this fault injection.
	db.write(async |t| {
		t.batch_execute("CREATE FUNCTION interrupt_receive_claim() RETURNS trigger AS $$
			BEGIN
				IF OLD.spend_state='htlc-recv-unclaimed' AND OLD.oor_spent_txid IS NULL
					AND NEW.oor_spent_txid IS NOT NULL THEN
					RAISE EXCEPTION 'test interrupted receive claim commit';
				END IF;
				RETURN NEW;
			END; $$ LANGUAGE plpgsql;
			CREATE TRIGGER interrupt_receive_claim BEFORE UPDATE ON vtxo
			FOR EACH ROW EXECUTE FUNCTION interrupt_receive_claim();").await?;
		Ok(())
	}).await.unwrap();
	if outcome == ReceiveOutcome::CollectedStatusLost {
		// Test-only fault: the hold plugin really settles, but the server
		// cannot record the settled status until the trigger is dropped.
		db.write(async |t| {
			t.batch_execute("CREATE FUNCTION interrupt_receive_status() RETURNS trigger AS $$
				BEGIN
					IF NEW.status='settled' THEN
						RAISE EXCEPTION 'test interrupted receive status write';
					END IF;
					RETURN NEW;
				END; $$ LANGUAGE plpgsql;
				CREATE TRIGGER interrupt_receive_status BEFORE UPDATE ON lightning_htlc_subscription
				FOR EACH ROW EXECUTE FUNCTION interrupt_receive_status();").await?;
			Ok(())
		}).await.unwrap();
	}
	tokio::time::timeout(Duration::from_secs(30), async {
		tokio::select! {
			r = wallet.try_claim_lightning_receive(payment_hash, true) =>
				panic!("claim completed before interruption: {r:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("claim must reach the interrupted commit");
	if outcome == ReceiveOutcome::Undisclosed {
		// The user disappears before disclosing the preimage. Cancel the
		// real held invoice, so the external payer keeps its funds.
		lightning.internal.hold_client().await.cancel(cln_rpc::plugins::hold::CancelRequest {
			payment_hash: payment_hash.to_vec(),
		}).await.unwrap();
	}
	let paid = if outcome == ReceiveOutcome::UndisclosedUntilHoldDeadline {
		// Only blocks pass. The hold plugin fails the incoming HTLCs back
		// before they expire, which bounds the payer's wait.
		let mut paying = paying;
		let paid = tokio::time::timeout(Duration::from_secs(180), async {
			loop {
				tokio::select! {
					paid = &mut paying => break paid,
					_ = tokio::time::sleep(Duration::from_secs(1)) => { ctx.generate_blocks(1).await; },
				}
			}
		}).await.expect("the hold plugin must fail the payment back by its deadline").unwrap();
		let invoices = lightning.internal.hold_client().await.list(hold::ListRequest {
			constraint: Some(hold::list_request::Constraint::PaymentHash(payment_hash.to_vec())),
		}).await.unwrap().into_inner().invoices;
		assert_eq!(invoices[0].state(), hold::InvoiceState::Cancelled);
		println!("hold deadline: external payment failed back at height {}",
			ctx.bitcoind().sync_client().get_block_count().unwrap());
		paid
	} else {
		tokio::time::timeout(Duration::from_secs(30), paying).await
			.expect("external payment must finish").unwrap()
	};
	if collected { paid.expect("external payment must succeed"); }
	else { paid.expect_err("external canceled payment must fail"); }
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Settled,
		outcome == ReceiveOutcome::Collected, "subscription status {}", sub.status);
	assert_eq!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
		.await.unwrap().is_some(), disclosed);
	let ids = sub.htlc_vtxos.iter().map(ToString::to_string).collect::<Vec<_>>();
	assert!(!ids.is_empty());
	let coins = db.read(async |t| t.get_user_vtxos_by_id(&sub.htlc_vtxos).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	let principal = coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert_eq!(principal, 100_000);
	let unresolved = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.direction='outgoing'
		 AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL
		 AND v.oor_spent_txid IS NULL", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(unresolved as usize, coins.len());
	assert!(wallet.lightning_receive_checkpoint(payment_hash).await.unwrap().is_some());
	drop(wallet);
	drop(proxy);
	db.write(async |t| {
		t.batch_execute("DROP TRIGGER interrupt_receive_claim ON vtxo; DROP FUNCTION interrupt_receive_claim();").await?;
		Ok(())
	}).await.unwrap();
	println!("interrupted receive: outcome={outcome:?}, external_paid={collected}, preimage_recorded={disclosed}, \
		granted_sat={principal}, unresolved_htlcs={}", coins.len());

	expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	enable_payouts(&ctx, &srv).await;
	match outcome {
		ReceiveOutcome::Undisclosed | ReceiveOutcome::UndisclosedUntilHoldDeadline => {
			assert_no_payout(&ctx, &db, &ids, &record.spk).await;
			println!("unsettled receive: external payer refunded, no recorded preimage, no expiry payout");
			return;
		},
		ReceiveOutcome::Uncollected => {
			assert_no_payout(&ctx, &db, &ids, &record.spk).await;
			let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
				.await.unwrap().unwrap();
			assert_ne!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Settled);
			let held = db.read(async |t| Ok(t.query_one(
				"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
				 WHERE v.vtxo_id=ANY($1) AND v.spend_state='htlc-recv-unclaimed'
				 AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL", &[&ids],
			).await?.get::<_, i64>(0))).await.unwrap();
			assert_eq!(held as usize, coins.len(), "the receive stays held, not resolved");
			println!("uncollected receive: preimage recorded, external payer refunded, subscription {}, \
				no expiry payout", sub.status);
			return;
		},
		ReceiveOutcome::CollectedStatusLost => {
			// While the collection is unrecorded the receive is held.
			assert_no_payout(&ctx, &db, &ids, &record.spk).await;
			db.write(async |t| {
				t.batch_execute("DROP TRIGGER interrupt_receive_status ON lightning_htlc_subscription;
					DROP FUNCTION interrupt_receive_status();").await?;
				Ok(())
			}).await.unwrap();
			// The server's own hold settler records the collection it made.
			tokio::time::timeout(Duration::from_secs(60), async {
				loop {
					let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
						.await.unwrap().unwrap();
					if sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Settled { break; }
					tokio::time::sleep(Duration::from_millis(500)).await;
				}
			}).await.expect("the server must record the collected payment");
			println!("collected receive: held while unrecorded, settled status recovered");
		},
		ReceiveOutcome::Collected => {},
	}
	let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &ids, &record.spk, principal).await;
	let fulfilled = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='fulfilled'", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(fulfilled as usize, coins.len());
	let request = claim_request.lock().unwrap().take().unwrap();
	srv.get_public_rpc().await.claim_lightning_receive(request).await
		.expect_err("a late cooperative claim cannot spend the paid HTLC a second time");
	srv.stop().await.unwrap();
	let spend_txid = restore_and_spend(&ctx, &mnemonic, payout).await;
	println!("interrupted receive recovered: payout={payout}, principal_sat={principal}, fee_sat={fee}, confirmed_seed_spend={spend_txid}");
}

#[derive(Clone)]
struct AbsentSender {
	reached: Arc<Notify>,
	request: Arc<Mutex<Option<protos::ArkoorPackageCosignRequest>>>,
}

#[async_trait::async_trait]
impl ArkRpcProxy for AbsentSender {
	async fn request_lightning_pay_htlc_revocation(
		&self, _upstream: &mut ArkClient, request: protos::ArkoorPackageCosignRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		*self.request.lock().unwrap() = Some(request);
		self.reached.notify_one();
		// The sender disappears at its real refund request.
		std::future::pending().await
	}
}

/// The payee cancels, so the server's node fails the payment. The sender
/// disappears before its own refund request reaches the server.
#[tokio::test]
async fn fallback_failed_lightning_send_refunds_absent_sender() {
	Box::pin(absent_sender(false)).await;
}

/// The payee settles, so the server's node completed the payment, but the
/// server could not record the preimage. Neither the ordinary refund request
/// nor the automatic payout may refund the sender.
#[tokio::test]
async fn fallback_completed_lightning_send_without_recorded_preimage_is_not_refunded() {
	Box::pin(absent_sender(true)).await;
}

async fn absent_sender(completed: bool) {
	let name = if completed { "fallback_completed_lightning_send_without_recorded_preimage_is_not_refunded" }
		else { "fallback_failed_lightning_send_refunds_absent_sender" };
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().create().await;
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let reached = Arc::new(Notify::new());
	let revocation = Arc::new(Mutex::new(None));
	let proxy = srv.start_proxy_no_mailbox(AbsentSender {
		reached: reached.clone(), request: revocation.clone(),
	}).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("sender", &proxy.address).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();

	// A real hold invoice on the external node: the payee decides the outcome.
	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let mut payee = lightning.external.hold_client().await;
	let invoice = payee.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
		amount_msat: 100_000 * 1_000,
		description: Some(hold::invoice_request::Description::Memo(name.into())),
		min_final_cltv_expiry: Some(18),
		expiry: Some(3600),
		routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	// Boarding just mined blocks. A node paying from a stale tip sets an
	// HTLC expiry the payee rejects, which would fail the payment for an
	// unrelated reason.
	lightning.sync().await;
	wallet.pay_lightning_invoice(invoice, None, false).await.unwrap();
	lightning.external.wait_for_hold_invoice_accepted(payment_hash).await;

	if completed {
		// Model a server that cannot persist the preimage its node learned.
		// The node's payment, its preimage and all coin state stay real.
		db.write(async |t| {
			t.batch_execute("CREATE FUNCTION interrupt_send_settlement() RETURNS trigger AS $$
				BEGIN RAISE EXCEPTION 'test interrupted send settlement'; END; $$ LANGUAGE plpgsql;
				CREATE TRIGGER interrupt_send_settlement BEFORE INSERT ON htlc_settlement
				FOR EACH ROW EXECUTE FUNCTION interrupt_send_settlement();").await?;
			Ok(())
		}).await.unwrap();
		payee.settle(hold::SettleRequest { payment_preimage: preimage.as_ref().to_vec() }).await.unwrap();
		// The server's own node reports the completed payment and its preimage.
		let mut node = lightning.internal.grpc_client().await;
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let pays = node.list_pays(cln_rpc::ListpaysRequest {
					bolt11: None, payment_hash: Some(payment_hash.to_vec()), status: None,
					index: None, limit: None, start: None,
				}).await.unwrap().into_inner().pays;
				if pays.iter().any(|p| p.status() == cln_rpc::listpays_pays::ListpaysPaysStatus::Complete
					&& p.preimage.as_deref() == Some(preimage.as_ref())) { break; }
				tokio::time::sleep(Duration::from_millis(200)).await;
			}
		}).await.expect("the server's node must complete the payment");
		tokio::time::sleep(Duration::from_secs(3)).await;
		assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
			.await.unwrap().is_none(), "the fault must keep the preimage out of the database");
	} else {
		payee.cancel(hold::CancelRequest { payment_hash: payment_hash.as_ref().to_vec() }).await.unwrap();
		tokio::time::timeout(Duration::from_secs(60), async {
			loop {
				let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
					.await.unwrap().unwrap();
				if attempt.status == server::database::ln::LightningPaymentStatus::Failed { break; }
				tokio::time::sleep(Duration::from_millis(200)).await;
			}
		}).await.expect("the server must record the failed payment");
		tokio::time::timeout(Duration::from_secs(60), async {
			tokio::select! {
				r = wallet.check_lightning_payment(payment_hash, true) =>
					panic!("refund completed before the sender disappeared: {r:?}"),
				_ = reached.notified() => {},
			}
		}).await.expect("the sender must reach its refund request");
	}

	let held = wallet.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == payment_hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	assert!(!held.is_empty());
	let coins = db.read(async |t| t.get_user_vtxos_by_id(&held).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let principal = coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	let unresolved = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.direction='incoming' AND v.spend_state='spendable'
		 AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(unresolved as usize, coins.len());

	if completed {
		// Past the HTLC expiry the ordinary refund request is the sender's own
		// path. A completed payment must not be refunded through it either.
		let htlc_expiry = coins[0].policy().as_server_htlc_send().unwrap().htlc_expiry.to_u32();
		let core = ctx.bitcoind().sync_client();
		ctx.generate_blocks(htlc_expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 2).await;
		let mut keypairs = Vec::new();
		let mut full = Vec::new();
		for coin in &coins {
			let vtxo = wallet.get_full_vtxo(coin.id()).await.unwrap();
			keypairs.push(wallet.get_vtxo_key(&vtxo).await.unwrap());
			full.push(vtxo);
		}
		let output = wallet.derive_store_next_keypair().await.unwrap().0.public_key();
		let builder = ark::arkoor::package::ArkoorPackageBuilder::new_claim_all_with_checkpoints(
			full.into_iter(), ark::VtxoPolicy::new_pubkey(output),
		).unwrap().generate_user_nonces(&keypairs).unwrap();
		let status = srv.get_public_rpc().await
			.request_lightning_pay_htlc_revocation(protos::ArkoorPackageCosignRequest::from(builder.cosign_request()))
			.await.expect_err("the server refunded a payment its node completed");
		println!("completed send: ordinary refund refused: {}", status.message());
	}
	drop(wallet);
	drop(proxy);
	println!("absent sender: completed={completed}, htlc_sat={principal}, coins={}", coins.len());

	expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	enable_payouts(&ctx, &srv).await;
	if completed {
		assert_no_payout(&ctx, &db, &ids, &record.spk).await;
		println!("completed send: no recorded preimage, node reports success, no expiry refund");
		return;
	}
	let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &ids, &record.spk, principal).await;
	let revoked = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='revoked'", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(revoked as usize, coins.len());
	let request = revocation.lock().unwrap().take().unwrap();
	srv.get_public_rpc().await.request_lightning_pay_htlc_revocation(request).await
		.expect_err("a late refund request cannot spend the refunded HTLC a second time");
	srv.stop().await.unwrap();
	let spend_txid = restore_and_spend(&ctx, &mnemonic, payout).await;
	println!("failed send refunded: payout={payout}, principal_sat={principal}, fee_sat={fee}, confirmed_seed_spend={spend_txid}");
}

/// Relays the server's calls to its own CLN node. The first xpay call fails
/// at once with a transport error, while its request reaches the node only
/// when the test releases it: the server cannot tell whether its node pays.
#[derive(Clone)]
struct DroppedXpayRelay {
	upstream: Channel,
	intercepted: Arc<AtomicBool>,
	/// The xpay call was answered with the error.
	dropped: Arc<Notify>,
	/// The server asked its node about payments after the error.
	listed: Arc<Notify>,
	release: Arc<Notify>,
}

impl NamedService for DroppedXpayRelay {
	const NAME: &'static str = "cln.Node";
}

impl Service<http::Request<Body>> for DroppedXpayRelay {
	type Response = http::Response<Body>;
	type Error = Infallible;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

	fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, request: http::Request<Body>) -> Self::Future {
		let relay = self.clone();
		Box::pin(async move {
			let path = request.uri().path().to_owned();
			let mut upstream = relay.upstream.clone();
			if path == "/cln.Node/Xpay" && !relay.intercepted.swap(true, Ordering::SeqCst) {
				// Read the whole request before failing the call, so the node
				// gets exactly what the server sent.
				let (parts, body) = request.into_parts();
				let bytes = axum::body::to_bytes(axum::body::Body::new(body), usize::MAX).await.unwrap();
				let held = http::Request::from_parts(parts, Body::new(axum::body::Body::from(bytes)));
				let release = relay.release.clone();
				tokio::spawn(async move {
					release.notified().await;
					poll_fn(|cx| upstream.poll_ready(cx)).await.unwrap();
					// Keep the call open until xpay answers, so the node
					// finishes the payment.
					let response = upstream.call(held).await.unwrap();
					let _ = axum::body::to_bytes(axum::body::Body::new(response.into_body()), usize::MAX).await;
				});
				relay.dropped.notify_one();
				return Ok(tonic::Status::unavailable("connection reset by peer").into_http());
			}
			let listing = path == "/cln.Node/ListPays" && relay.intercepted.load(Ordering::SeqCst);
			poll_fn(|cx| upstream.poll_ready(cx)).await.unwrap();
			let response = match upstream.call(request).await {
				Ok(response) => response,
				Err(e) => tonic::Status::unavailable(e.to_string()).into_http(),
			};
			if listing {
				relay.listed.notify_one();
			}
			Ok(response)
		})
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum XpayDelay {
	/// The request reaches the node a second after the error.
	Brief,
	/// The request reaches the node only after the monitor's failure
	/// horizon: the attempt's retry time plus the server's buffer.
	PastRetries,
	/// The request reaches the node only after the invoice expired, so the
	/// node refuses it before sending any HTLC.
	PastInvoiceExpiry,
}

/// The server's xpay call fails with a transport error while its node may
/// still be paying. That is no evidence the payment failed: the sender's
/// refund waits, and the payment the node then completes is recorded.
#[tokio::test]
async fn fallback_dropped_xpay_call_is_not_refunded() {
	Box::pin(dropped_xpay(XpayDelay::Brief)).await;
}

/// The same, with the request held past the time the node would have
/// stopped retrying a request it received at once. The request can still
/// start when it arrives, so the server must not conclude it failed.
#[tokio::test]
async fn fallback_long_delayed_xpay_dispatch_is_not_refunded() {
	Box::pin(dropped_xpay(XpayDelay::PastRetries)).await;
}

/// A request held past the invoice expiry can no longer start: the node
/// refuses an expired invoice before sending any HTLC. The sender is then
/// refunded.
#[tokio::test]
async fn fallback_delayed_xpay_after_invoice_expiry_is_refunded() {
	Box::pin(dropped_xpay(XpayDelay::PastInvoiceExpiry)).await;
}

async fn dropped_xpay(delay: XpayDelay) {
	let name = match delay {
		XpayDelay::Brief => "fallback_dropped_xpay_call_is_not_refunded",
		XpayDelay::PastRetries => "fallback_long_delayed_xpay_dispatch_is_not_refunded",
		XpayDelay::PastInvoiceExpiry => "fallback_delayed_xpay_after_invoice_expiry_is_refunded",
	};
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;

	// Test-only fault: the server reaches its own node through this relay,
	// in plain text on loopback. It changes no payment, coin or attempt state.
	let details = lightning.internal.grpc_details().await;
	let upstream = Channel::builder(details.uri.parse().unwrap()).tls_config(ClientTlsConfig::new()
		.ca_certificate(Certificate::from_pem(std::fs::read_to_string(&details.server_cert_path).unwrap()))
		.identity(Identity::from_pem(
			std::fs::read_to_string(&details.client_cert_path).unwrap(),
			std::fs::read_to_string(&details.client_key_path).unwrap(),
		))
	).unwrap().connect().await.unwrap();
	let relay = DroppedXpayRelay {
		upstream,
		intercepted: Arc::new(AtomicBool::new(false)),
		dropped: Arc::new(Notify::new()),
		listed: Arc::new(Notify::new()),
		release: Arc::new(Notify::new()),
	};
	let port = ark_testing::ports::pick_port();
	tokio::spawn(Server::builder().add_service(relay.clone()).serve(([127, 0, 0, 1], port).into()));
	let relay_uri = format!("http://127.0.0.1:{port}");

	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10))
		.cfg(move |c| c.cln_array[0].uri = relay_uri.parse().unwrap())
		.create().await;
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv)
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();

	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let mut payee = lightning.external.hold_client().await;
	let invoice_expiry = if delay == XpayDelay::PastInvoiceExpiry { 40 } else { 3600 };
	let invoice = payee.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
		amount_msat: 100_000 * 1_000,
		description: Some(hold::invoice_request::Description::Memo(name.into())),
		min_final_cltv_expiry: Some(18),
		expiry: Some(invoice_expiry),
		routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	let invoice_created = std::time::Instant::now();
	// Boarding just mined blocks; pay from a synced tip.
	lightning.sync().await;
	sender.pay_lightning_invoice(invoice, None, false).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), relay.dropped.notified()).await
		.expect("the server must call xpay");
	// The server reconciles with its node right after the error. The node
	// has no record of the payment yet.
	tokio::time::timeout(Duration::from_secs(30), relay.listed.notified()).await
		.expect("the server must ask its node about the payment");
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	let retry_for = attempt.retry_for.unwrap_or(srv.config().cln_xpay_timeout);
	let hold = match delay {
		XpayDelay::Brief => Duration::from_secs(1),
		// Several monitor checks past its horizon; the server adds a buffer
		// of 15 seconds to the retry time.
		XpayDelay::PastRetries => retry_for + Duration::from_secs(15)
			+ 3 * srv.config().invoice_check_interval + Duration::from_secs(10),
		XpayDelay::PastInvoiceExpiry => Duration::from_secs(invoice_expiry + 5)
			.saturating_sub(invoice_created.elapsed()),
	};
	tokio::time::sleep(hold).await;
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	println!("dropped xpay call: delay={delay:?}, held {hold:?} (retry_for {retry_for:?}); attempt status: {}",
		attempt.status);

	// The sender asks for its refund while the request is still held.
	let held = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == payment_hash))
		.map(|w| w.vtxo).collect::<Vec<_>>();
	assert!(!held.is_empty());
	let ids = held.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let mut keypairs = Vec::new();
	let mut full = Vec::new();
	for vtxo in &held {
		let vtxo = sender.get_full_vtxo(vtxo.id()).await.unwrap();
		keypairs.push(sender.get_vtxo_key(&vtxo).await.unwrap());
		full.push(vtxo);
	}
	let output = sender.derive_store_next_keypair().await.unwrap().0.public_key();
	let builder = ark::arkoor::package::ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		full.into_iter(), ark::VtxoPolicy::new_pubkey(output),
	).unwrap().generate_user_nonces(&keypairs).unwrap();
	let revocation = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());

	if delay == XpayDelay::PastInvoiceExpiry {
		// The request reaches the node after the invoice expired. The node
		// refuses it, so the payment can no longer start or complete.
		relay.release.notify_one();
		tokio::time::timeout(Duration::from_secs(90), async {
			loop {
				let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
					.await.unwrap().unwrap();
				if attempt.status == server::database::ln::LightningPaymentStatus::Failed { break; }
				tokio::time::sleep(Duration::from_millis(500)).await;
			}
		}).await.expect("a request that can no longer start must be failed");
		srv.get_public_rpc().await.request_lightning_pay_htlc_revocation(revocation).await
			.expect("the sender of a payment that can no longer start is refunded");
		let invoices = payee.list(hold::ListRequest {
			constraint: Some(hold::list_request::Constraint::PaymentHash(payment_hash.to_vec())),
		}).await.unwrap().into_inner().invoices;
		assert!(!matches!(invoices[0].state(), hold::InvoiceState::Accepted | hold::InvoiceState::Paid),
			"the expired invoice was never paid: {:?}", invoices[0].state());
		let mut node = lightning.internal.grpc_client().await;
		let pays = node.list_pays(cln_rpc::ListpaysRequest {
			bolt11: None, payment_hash: Some(payment_hash.to_vec()), status: None,
			index: None, limit: None, start: None,
		}).await.unwrap().into_inner().pays;
		assert!(pays.is_empty(), "the late request must not start a payment: {pays:?}");
		println!("dropped xpay call past invoice expiry: node refused it, sender refunded, payee unpaid");
		return;
	}

	let refund = srv.get_public_rpc().await.request_lightning_pay_htlc_revocation(revocation).await;
	println!("dropped xpay call: refund while the request is held: {:?}", refund.as_ref().map(|_| ()).map_err(|e| e.message().to_owned()));

	// The request reaches the node, which pays.
	relay.release.notify_one();
	lightning.external.wait_for_hold_invoice_accepted(payment_hash).await;
	payee.settle(hold::SettleRequest { payment_preimage: preimage.as_ref().to_vec() }).await.unwrap();
	let mut node = lightning.internal.grpc_client().await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let pays = node.list_pays(cln_rpc::ListpaysRequest {
				bolt11: None, payment_hash: Some(payment_hash.to_vec()), status: None,
				index: None, limit: None, start: None,
			}).await.unwrap().into_inner().pays;
			if pays.iter().any(|p| p.status() == cln_rpc::listpays_pays::ListpaysPaysStatus::Complete) { break; }
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	}).await.expect("the server's node must complete the late request");
	println!("dropped xpay call: the node completed the payment after the delay; refunded before={}", refund.is_ok());
	assert!(refund.is_err(), "the server refunded a payment its node then completed: paid twice");
	assert_ne!(attempt.status, server::database::ln::LightningPaymentStatus::Failed,
		"the server failed a payment whose request had not reached its node");
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
				.await.unwrap().unwrap();
			if attempt.status == server::database::ln::LightningPaymentStatus::Succeeded { break; }
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("the server must record the payment its node completed");
	assert_eq!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
		.await.unwrap(), Some(preimage));
	let revoked = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='revoked'", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(revoked, 0);
	let state = sender.check_lightning_payment(payment_hash, true).await.unwrap();
	assert!(matches!(state, bark::actions::lightning::pay::LightningSendState::Paid(_)), "{state:?}");
	println!("dropped xpay call: node completed the payment; server recorded it; sender not refunded");
}

/// A receive the external node pays into the server's hold invoice, driven
/// at the RPC layer so the test controls each step of the claim.
struct HeldReceive {
	preimage: Preimage,
	payment_hash: PaymentHash,
	paying: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn held_receive(
	lightning: &LightningPaymentSetup, srv: &Captaind, db: &Db,
) -> HeldReceive {
	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let invoice = srv.get_public_rpc().await.start_lightning_receive(protos::StartLightningReceiveRequest {
		payment_hash: payment_hash.to_vec(),
		amount_sat: 100_000,
		min_cltv_delta: 18,
		mailbox_id: None,
		description: None,
	}).await.unwrap().into_inner().bolt11;
	// Pay from a synced tip, as `try_pay_bolt11` does.
	lightning.external.wait_for_block_sync().await;
	let mut payer = lightning.external.grpc_client().await;
	let paying = tokio::spawn(async move {
		payer.xpay(cln_rpc::XpayRequest {
			invstring: invoice, amount_msat: None, maxfee: None, layers: vec![], retry_for: None,
			partial_msat: None, maxdelay: None, payer_note: None, label: None, localinvreqid: None,
			dev_use_shadow: None,
		}).await.map(|_| ()).map_err(anyhow::Error::from)
	});
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
				.await.unwrap().unwrap();
			if sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Accepted { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("external Lightning must fund the receive");
	HeldReceive { preimage, payment_hash, paying }
}

/// Ask for the HTLC-recv vtxos of a held receive, for a key linked to the
/// wallet's fallback record.
async fn prepare_claim(
	srv: &Captaind, wallet: &bark::Wallet, receive: &HeldReceive,
) -> Result<(bitcoin::secp256k1::Keypair, Vec<Vtxo<Full>>), tonic::Status> {
	let (keypair, _) = wallet.derive_store_next_keypair().await.unwrap();
	let sub_expiry = Db::connect(&srv.config().postgres).await.unwrap()
		.read(async |t| t.get_htlc_subscription_by_payment_hash(receive.payment_hash).await)
		.await.unwrap().unwrap().lowest_incoming_htlc_expiry.unwrap();
	let granted = srv.get_public_rpc().await.prepare_lightning_receive_claim(
		protos::PrepareLightningReceiveClaimRequest {
			payment_hash: receive.payment_hash.to_vec(),
			user_pubkey: keypair.public_key().serialize().to_vec(),
			htlc_recv_expiry: sub_expiry.saturating_sub(srv.config().htlc_expiry_delta).into(),
			lightning_receive_anti_dos: None,
		},
	).await?.into_inner().htlc_vtxos.into_iter()
		.map(|b| Vtxo::<Full>::deserialize(&b).unwrap()).collect();
	Ok((keypair, granted))
}

/// Disclose the preimage and claim the granted vtxos cooperatively.
async fn claim(
	srv: &Captaind, wallet: &bark::Wallet, receive: &HeldReceive,
	keypair: bitcoin::secp256k1::Keypair, granted: Vec<Vtxo<Full>>,
) -> Result<(), tonic::Status> {
	let output = wallet.derive_store_next_keypair().await.unwrap().0.public_key();
	let builder = ark::arkoor::package::ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		granted.into_iter(), ark::VtxoPolicy::new_pubkey(output),
	).unwrap().generate_user_nonces(&[keypair]).unwrap();
	srv.get_public_rpc().await.claim_lightning_receive(protos::ClaimLightningReceiveRequest {
		payment_hash: receive.payment_hash.to_vec(),
		payment_preimage: receive.preimage.as_ref().to_vec(),
		cosign_request: Some(protos::ArkoorPackageCosignRequest::from(builder.cosign_request())),
	}).await.map(|_| ())
}

async fn hold_state(lightning: &LightningPaymentSetup, payment_hash: PaymentHash) -> hold::InvoiceState {
	let invoices = lightning.internal.hold_client().await.list(hold::ListRequest {
		constraint: Some(hold::list_request::Constraint::PaymentHash(payment_hash.to_vec())),
	}).await.unwrap().into_inner().invoices;
	invoices[0].state()
}

/// The server granted HTLC-recv vtxos for a receive, so it must collect the
/// incoming HTLCs. The invoice expiring before the recipient claims must not
/// make the server fail them back.
#[tokio::test]
async fn fallback_prepared_receive_is_collected_after_invoice_expiry() {
	let name = "fallback_prepared_receive_is_collected_after_invoice_expiry";
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let invoice_expiry = Duration::from_secs(20);
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10))
		.cfg(move |c| c.invoice_expiry = invoice_expiry).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	wallet.stop_daemon_wait().await.unwrap();

	let started = std::time::Instant::now();
	let receive = held_receive(&lightning, &srv, &db).await;
	let (keypair, granted) = prepare_claim(&srv, &wallet, &receive).await.unwrap();
	assert!(!granted.is_empty());

	// The invoice expires while the claim is prepared. Give the server's
	// subscription checks, every 3 seconds, several runs past the expiry.
	let checks_done = invoice_expiry + Duration::from_secs(10);
	tokio::time::timeout(checks_done.saturating_sub(started.elapsed()), async {
		loop {
			let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(receive.payment_hash).await)
				.await.unwrap().unwrap();
			if sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Canceled { break; }
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.err();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(receive.payment_hash).await)
		.await.unwrap().unwrap();
	println!("prepared receive after invoice expiry: subscription {}, hold invoice {:?}",
		sub.status, hold_state(&lightning, receive.payment_hash).await);

	claim(&srv, &wallet, &receive, keypair, granted).await
		.expect("the recipient's claim must still be collected");
	let paid = tokio::time::timeout(Duration::from_secs(30), receive.paying).await
		.expect("external payment must finish").unwrap();
	paid.expect("the server must collect the external payment");
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(receive.payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Settled);
	assert_eq!(hold_state(&lightning, receive.payment_hash).await, hold::InvoiceState::Paid);
	println!("prepared receive collected after invoice expiry; external payer paid");
}

/// A receive whose incoming HTLCs are gone can never be collected, even
/// with its preimage. It must not hold up collecting the receives behind it.
#[tokio::test]
async fn fallback_uncollectable_receive_does_not_block_later_collection() {
	let name = "fallback_uncollectable_receive_does_not_block_later_collection";
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	wallet.stop_daemon_wait().await.unwrap();

	// First receive: the claim is prepared, then the server's own hold
	// plugin fails the incoming HTLCs back, as its expiry deadline does.
	// Test-only fault: a direct plugin cancel; no server state is changed.
	let lost = held_receive(&lightning, &srv, &db).await;
	let (keypair, granted) = prepare_claim(&srv, &wallet, &lost).await.unwrap();
	lightning.internal.hold_client().await.cancel(hold::CancelRequest {
		payment_hash: lost.payment_hash.to_vec(),
	}).await.unwrap();
	let err = claim(&srv, &wallet, &lost, keypair, granted).await
		.expect_err("a claim the server cannot collect must be refused");
	println!("uncollectable receive: claim refused: {}", err.message());
	tokio::time::timeout(Duration::from_secs(30), lost.paying).await
		.expect("external payment must finish").unwrap()
		.expect_err("the failed-back external payment must fail");
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(lost.payment_hash).await)
		.await.unwrap().is_some(), "the disclosed preimage is recorded");

	// Second receive: the hold invoice settles, but the server cannot record
	// that until the test-only trigger is dropped. Only the hold settler
	// records it afterwards.
	let collected = held_receive(&lightning, &srv, &db).await;
	let (keypair, granted) = prepare_claim(&srv, &wallet, &collected).await.unwrap();
	let sub_id = db.read(async |t| t.get_htlc_subscription_by_payment_hash(collected.payment_hash).await)
		.await.unwrap().unwrap().id;
	db.write(async |t| {
		t.batch_execute(&format!("CREATE FUNCTION interrupt_receive_status() RETURNS trigger AS $$
			BEGIN
				IF NEW.status='settled' AND NEW.id={sub_id} THEN
					RAISE EXCEPTION 'test interrupted receive status write';
				END IF;
				RETURN NEW;
			END; $$ LANGUAGE plpgsql;
			CREATE TRIGGER interrupt_receive_status BEFORE UPDATE ON lightning_htlc_subscription
			FOR EACH ROW EXECUTE FUNCTION interrupt_receive_status();")).await?;
		Ok(())
	}).await.unwrap();
	claim(&srv, &wallet, &collected, keypair, granted).await
		.expect_err("the claim cannot record the settled status");
	tokio::time::timeout(Duration::from_secs(30), collected.paying).await
		.expect("external payment must finish").unwrap()
		.expect("the server must collect the external payment");
	db.write(async |t| {
		t.batch_execute("DROP TRIGGER interrupt_receive_status ON lightning_htlc_subscription;
			DROP FUNCTION interrupt_receive_status();").await?;
		Ok(())
	}).await.unwrap();
	tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(collected.payment_hash).await)
				.await.unwrap().unwrap();
			if sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Settled { break; }
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("the hold settler must record the later collection");

	// The uncollectable receive stays held: its payment was never collected.
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(lost.payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::HtlcsReady);
	assert_eq!(hold_state(&lightning, lost.payment_hash).await, hold::InvoiceState::Cancelled);
	println!("uncollectable receive held ({}); later receive collected and recorded", sub.status);
}

/// The hold plugin already failed the incoming HTLCs back while the server's
/// status still says they are held. Granting HTLC-recv vtxos now would commit
/// the server to a payment it can never collect.
#[tokio::test]
async fn fallback_receive_not_granted_after_incoming_htlcs_failed_back() {
	let name = "fallback_receive_not_granted_after_incoming_htlcs_failed_back";
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	wallet.stop_daemon_wait().await.unwrap();

	let mut receive = held_receive(&lightning, &srv, &db).await;
	// Test-only fault: a direct cancel at the server's own hold plugin; the
	// server's subscription is left as it was.
	lightning.internal.hold_client().await.cancel(hold::CancelRequest {
		payment_hash: receive.payment_hash.to_vec(),
	}).await.unwrap();
	let paying = tokio::time::timeout(Duration::from_secs(30), &mut receive.paying).await
		.expect("external payment must finish").unwrap();
	paying.expect_err("the failed-back external payment must fail");
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(receive.payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Accepted);

	let err = prepare_claim(&srv, &wallet, &receive).await
		.expect_err("the server granted HTLC-recv vtxos it can never collect");
	assert_eq!(err.code(), tonic::Code::InvalidArgument, "{err:?}");
	println!("failed-back receive: grant refused: {}", err.message());
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(receive.payment_hash).await)
		.await.unwrap().unwrap();
	assert!(sub.htlc_vtxos.is_empty());
	assert_ne!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::HtlcsReady);
}

/// An intra-Ark payment whose recipient prepared the claim, got its HTLC-recv
/// vtxos and disappeared without disclosing the preimage. The sender
/// disappears too. Once the recipient can no longer claim, the expiry payout
/// refunds the sender, and only the sender.
#[tokio::test]
async fn fallback_abandoned_intra_ark_receive_refunds_absent_sender() {
	Box::pin(abandoned_intra_ark_receive(false)).await;
}

/// As above, but the sender returns and asks for its refund itself. It is
/// refused while the granted receive can still be claimed, and granted once
/// it no longer can.
#[tokio::test]
async fn fallback_abandoned_intra_ark_receive_allows_revocation() {
	Box::pin(abandoned_intra_ark_receive(true)).await;
}

async fn abandoned_intra_ark_receive(returning_sender: bool) {
	let name = if returning_sender { "fallback_abandoned_intra_ark_receive_allows_revocation" }
		else { "fallback_abandoned_intra_ark_receive_refunds_absent_sender" };
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	// The granted HTLC-recv vtxos come from the pool and expire well before
	// the sender's boards, so a returning sender's refund is a live coin.
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(1000);
			c.vtxopool.vtxo_lifetime = BlockDelta::new(400);
			// The refund decision requires this sweep depth whether or not
			// payouts are enabled.
			c.expiry_payout.sweep_min_confs = 1;
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let reached = Arc::new(Notify::new());
	let claim_request = Arc::new(Mutex::new(None));
	let proxy = srv.start_proxy_no_mailbox(InterruptedReceiveClaim {
		reached: reached.clone(), request: claim_request.clone(), reveal_preimage: false, fail_incoming: None,
	}).await;
	let recipient = ctx.bark_sdk("recipient", &proxy.address)
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let recipient_record = recipient.fallback_destination().await.unwrap();
	let sender_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let sender = ctx.bark_sdk("sender", &srv).mnemonic(sender_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let sender_record = sender.fallback_destination().await.unwrap();
	let sender_total = sender.balance().await.unwrap().total();

	let invoice = recipient.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let payment_hash = PaymentHash::from(&invoice);
	sender.pay_lightning_invoice(invoice.to_string(), None, false).await.unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Accepted);
	// The recipient prepares the claim and disappears at its real claim
	// request, before the preimage reaches the server.
	tokio::time::timeout(Duration::from_secs(30), async {
		tokio::select! {
			r = recipient.try_claim_lightning_receive(payment_hash, true) =>
				panic!("claim completed before interruption: {r:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("the recipient must reach its claim request");
	drop(recipient);
	drop(proxy);
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::HtlcsReady);
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
		.await.unwrap().is_none(), "the preimage was never disclosed");
	let granted = db.read(async |t| t.get_user_vtxos_by_id(&sub.htlc_vtxos).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	assert!(!granted.is_empty());
	let granted_ids = granted.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let held = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == payment_hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&held).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	assert!(!htlcs.is_empty());
	let htlc_ids = htlcs.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let htlc_principal = htlcs.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert!(attempt.is_self_payment() && !attempt.status.is_final());
	let recv_deadline = granted.iter().map(|v| v.policy().as_server_htlc_recv().unwrap().htlc_expiry.to_u32())
		.max().unwrap();
	let send_expiry = htlcs[0].policy().as_server_htlc_send().unwrap().htlc_expiry.to_u32();
	println!("abandoned intra-Ark receive: granted_sat={}, recv_deadline={recv_deadline}, \
		send_expiry={send_expiry}, sender_htlc_sat={htlc_principal}",
		granted.iter().map(|v| v.amount().to_sat()).sum::<u64>());

	let core = ctx.bitcoind().sync_client();
	if returning_sender {
		// Past both HTLC deadlines, but the granted vtxos are not swept: the
		// recipient can still exit them and claim with the preimage.
		ctx.generate_blocks(send_expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 2).await;
		// The client revokes past the HTLC expiry; a refusal parks its action.
		let refused = sender.check_lightning_payment(payment_hash, false).await;
		let state = sender.lightning_send_state(payment_hash).await.unwrap();
		assert!(matches!(state, bark::actions::lightning::pay::LightningSendState::InProgress(_)), "{state:?}");
		assert_eq!(db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
			.await.unwrap().unwrap().status, server::database::ln::LightningHtlcSubscriptionStatus::HtlcsReady,
			"a refund before the granted receive is swept would let both parties keep the value");
		println!("sender refund refused while the granted receive can still be claimed: {:?}", refused.err());

		expire_and_confirm_sweeps(&ctx, &db, &granted).await;
		assert!((core.get_block_count().unwrap() as u32) < htlcs[0].expiry_height().to_u32());
		let revoked = sender.check_lightning_payment(payment_hash, false).await;
		let state = sender.lightning_send_state(payment_hash).await.unwrap();
		assert!(matches!(state, bark::actions::lightning::pay::LightningSendState::Unknown),
			"the sender's revocation must complete: {revoked:?}, {state:?}");
		assert!(sender.pending_lightning_sends().await.unwrap().is_empty());
		let balance = sender.balance().await.unwrap();
		assert_eq!(balance.pending_lightning_send, sat(0));
		assert_eq!(balance.total(), sender_total, "the sender's HTLC value is back in Ark");
		println!("sender refunded by its own revocation: total={}", balance.total());
	} else {
		drop(sender);
		let mut coins = htlcs.clone();
		coins.extend(granted.iter().cloned());
		expire_and_confirm_sweeps(&ctx, &db, &coins).await;
		enable_payouts(&ctx, &srv).await;
		let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &htlc_ids, &sender_record.spk, htlc_principal).await;
		srv.stop().await.unwrap();
		let spend_txid = restore_and_spend(&ctx, &sender_mnemonic, payout).await;
		srv.start().await.unwrap();
		println!("absent sender refunded: payout={payout}, fee_sat={fee}, confirmed_seed_spend={spend_txid}");
	}

	// Exactly one party has the value: the receive is canceled, the
	// sender's HTLCs are revoked once, and the recipient gets nothing.
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Canceled);
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(attempt.status, server::database::ln::LightningPaymentStatus::Failed);
	let revoked = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='revoked'", &[&htlc_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(revoked as usize, htlc_ids.len());
	let request = claim_request.lock().unwrap().take().unwrap();
	let err = srv.get_public_rpc().await.claim_lightning_receive(request).await
		.expect_err("a late claim must not pay the recipient after the sender's refund");
	println!("late recipient claim refused: {}", err.message());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
		.await.unwrap().is_none());
	if returning_sender { enable_payouts(&ctx, &srv).await; }
	assert_no_payout(&ctx, &db, &granted_ids, &recipient_record.spk).await;
	let recipient_coins = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND spend_state='htlc-recv-unclaimed'
		 AND oor_spent_txid IS NULL", &[&granted_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(recipient_coins as usize, granted_ids.len(), "the granted receive was never claimed");
	println!("abandoned intra-Ark receive: returning_sender={returning_sender}; sender refunded once, recipient unpaid");
}

/// Fails every arkoor mailbox post, as an unreachable mailbox would.
#[derive(Clone)]
struct FailPost;

#[async_trait::async_trait]
impl MailboxRpcProxy for FailPost {
	async fn post_arkoor_message(
		&self, _upstream: &mut MailboxClient, _req: protos::mailbox_server::PostArkoorMessageRequest,
	) -> Result<protos::core::Empty, tonic::Status> {
		Err(tonic::Status::unavailable("test drops the mailbox post"))
	}
}

/// A Lightning receive forwarding to another wallet stalls in delivery until
/// its claim outputs settle. They were registered before delivery, so they
/// were paid to the destination's own record. The returning receiver treats
/// the settled refusal as delivered instead of retrying forever.
#[tokio::test]
async fn fallback_ln_receive_external_delivery_after_settlement_terminates() {
	let ctx = TestContext::new("bark_sdk/fallback_ln_receive_external_delivery_after_settlement_terminates").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(1000);
			c.vtxopool.vtxo_lifetime = BlockDelta::new(400);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let destination = ctx.bark_sdk("destination", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	destination.stop_daemon_wait().await.unwrap();
	let destination_address = destination.new_address().await.unwrap();
	let destination_spk = destination.fallback_destination().await.unwrap().spk;
	drop(destination);

	let proxy = srv.start_proxy_with_mailbox((), FailPost).await;
	let receiver_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let receiver = ctx.bark_sdk("receiver", &proxy.address).mnemonic(receiver_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let mut config = receiver.config().clone();
	let invoice = receiver.bolt11_invoice_for_address(sat(100_000), destination_address.clone(), None, None)
		.await.unwrap();
	let payment_hash = PaymentHash::from(&invoice);
	lightning.external.wait_for_block_sync().await;
	let mut payer = lightning.external.grpc_client().await;
	let paying = tokio::spawn(async move {
		payer.xpay(cln_rpc::XpayRequest {
			invstring: invoice.to_string(), amount_msat: None, maxfee: None, layers: vec![], retry_for: None,
			partial_msat: None, maxdelay: None, payer_note: None, label: None, localinvreqid: None,
			dev_use_shadow: None,
		}).await.map(|_| ()).map_err(anyhow::Error::from)
	});
	let delivery = tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			let _ = receiver.try_claim_lightning_receive(payment_hash, false).await;
			let recv = receiver.lightning_receive_checkpoint(payment_hash).await.unwrap().unwrap();
			if let bark::actions::lightning::receive::Progress::Delivering(delivery) = recv.progress {
				break delivery;
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("the receive must claim and reach delivery");
	tokio::time::timeout(Duration::from_secs(30), paying).await.unwrap().unwrap()
		.expect("the external payment completes once the receive is claimed");
	drop(receiver);
	drop(proxy);
	let ids = delivery.vtxos.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();

	// The claim outputs settle while the receiver is away.
	expire_and_confirm_sweeps(&ctx, &db, &delivery.vtxos).await;
	enable_payouts(&ctx, &srv).await;
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT spk FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() {
				assert!(rows.iter().all(|r| r.get::<_, Vec<u8>>("spk") == destination_spk.as_bytes()),
					"registered claim outputs are paid to the destination's record");
				break;
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("the claim outputs must be paid");

	// The receiver returns, with its mailbox reachable again.
	config.server_address = srv.ark_url();
	let receiver = bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &receiver_mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("receiver")), run_daemon: false, ..Default::default() },
	).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let _ = receiver.try_claim_lightning_receive(payment_hash, false).await;
			if matches!(receiver.lightning_receive_state(payment_hash).await.unwrap(),
				bark::actions::lightning::receive::LightningReceiveState::Settled(_))
			{ break; }
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("the delivery of settled outputs must finish");
	assert!(receiver.pending_lightning_receives().await.unwrap().is_empty());
	let history = receiver.history().await.unwrap();
	let movement = history.iter().find(|m| m.id == delivery.movement_id).unwrap();
	assert_eq!(movement.status, bark::movement::MovementStatus::Successful);
	assert!(!movement.sent_to.is_empty(), "the receive was forwarded to the destination");
	let posts = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM mailbox WHERE mailbox_type='arkoor-receive' AND vtxo_id=ANY($1)", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(posts, 0, "settled outputs are not delivered as Ark coins");
	println!("external delivery after settlement: receive settled, outputs paid to the destination record");
}

/// An intra-Ark payment whose recipient prepared the claim, then exited its
/// granted HTLC-recv vtxos on-chain without disclosing the preimage, and
/// disappeared; so did the sender. Watchmand spends the exited outputs
/// through the server's timeout clause. At the sweep depth, the recipient
/// can no longer claim, and the expiry payout refunds the sender.
#[tokio::test]
async fn fallback_exited_granted_receive_refunds_absent_sender() {
	let name = "fallback_exited_granted_receive_refunds_absent_sender";
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(1000);
			c.vtxopool.vtxo_lifetime = BlockDelta::new(400);
			c.expiry_payout.sweep_min_confs = 1;
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await
		.expect("the initially funded pool must become ready");
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let reached = Arc::new(Notify::new());
	let claim_request = Arc::new(Mutex::new(None));
	let proxy = srv.start_proxy_no_mailbox(InterruptedReceiveClaim {
		reached: reached.clone(), request: claim_request.clone(), reveal_preimage: false, fail_incoming: None,
	}).await;
	let recipient = ctx.bark_sdk("recipient", &proxy.address)
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let recipient_record = recipient.fallback_destination().await.unwrap();
	let sender_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let sender = ctx.bark_sdk("sender", &srv).mnemonic(sender_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let sender_record = sender.fallback_destination().await.unwrap();

	let invoice = recipient.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let payment_hash = PaymentHash::from(&invoice);
	sender.pay_lightning_invoice(invoice.to_string(), None, false).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		tokio::select! {
			r = recipient.try_claim_lightning_receive(payment_hash, true) =>
				panic!("claim completed before interruption: {r:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("the recipient must reach its claim request");
	drop(recipient);
	drop(proxy);
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::HtlcsReady);
	let granted = db.read(async |t| t.get_user_vtxos_by_id(&sub.htlc_vtxos).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	assert!(!granted.is_empty());
	let granted_ids = granted.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let held = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == payment_hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&held).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	assert!(!htlcs.is_empty());
	let htlc_ids = htlcs.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let htlc_principal = htlcs.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	drop(sender);

	// The recipient broadcasts the signed exit chain of its granted vtxos.
	let core = ctx.bitcoind().sync_client();
	for vtxo in &granted {
		for item in vtxo.transactions() {
			let txid = item.tx.compute_txid();
			if core.get_raw_transaction_info(&txid, None).is_ok() { continue; }
			ctx.broadcast_cpfp(&item.tx).await;
			ctx.generate_blocks(1).await;
		}
	}
	tokio::time::timeout(Duration::from_secs(120), async {
		loop {
			let exited = db.read(async |t| Ok(t.query_one(
				"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND confirmed_height IS NOT NULL", &[&granted_ids],
			).await?.get::<_, i64>(0))).await.unwrap();
			if exited as usize == granted_ids.len() { break; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("the granted vtxos must be exited");
	// Watchmand claims each exited output through the timeout clause, once
	// past its HTLC expiry and exit delta.
	tokio::time::timeout(Duration::from_secs(180), async {
		loop {
			let revoked = db.read(async |t| Ok(t.query_one(
				"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
				 WHERE v.vtxo_id=ANY($1) AND h.chain_resolution='revoked'", &[&granted_ids],
			).await?.get::<_, i64>(0))).await.unwrap();
			if revoked as usize == granted_ids.len() { break; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("watchmand must spend the exited outputs through the timeout clause");
	ctx.generate_blocks(1).await;

	expire_and_confirm_sweeps(&ctx, &db, &htlcs).await;
	enable_payouts(&ctx, &srv).await;
	let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &htlc_ids, &sender_record.spk, htlc_principal).await;

	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Canceled);
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(attempt.status, server::database::ln::LightningPaymentStatus::Failed);
	let revoked = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='revoked'", &[&htlc_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(revoked as usize, htlc_ids.len());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
		.await.unwrap().is_none(), "the preimage was never disclosed");
	assert_no_payout(&ctx, &db, &granted_ids, &recipient_record.spk).await;
	println!("exited granted receive: sender refunded once, payout={payout}, fee_sat={fee}");
}

/// A sender's late refund request holds its payment guard while the database
/// keeps it waiting. Another wallet whose expired coins share the payout batch
/// must still be paid, and the sender's coins must settle exactly once.
#[tokio::test]
async fn fallback_busy_lightning_payment_does_not_stall_other_wallets() {
	let name = "fallback_busy_lightning_payment_does_not_stall_other_wallets";
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().create().await;
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let reached = Arc::new(Notify::new());
	let revocation = Arc::new(Mutex::new(None));
	let proxy = srv.start_proxy_no_mailbox(AbsentSender {
		reached: reached.clone(), request: revocation.clone(),
	}).await;
	let sender = ctx.bark_sdk("sender", &proxy.address)
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let bystander_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let bystander = ctx.bark_sdk("bystander", &srv).mnemonic(bystander_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(50_000)).create().await;
	bystander.stop_daemon_wait().await.unwrap();
	let bystander_record = bystander.fallback_destination().await.unwrap();

	// A real failed payment: the external payee cancels its hold invoice.
	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let mut payee = lightning.external.hold_client().await;
	let invoice = payee.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
		amount_msat: 100_000 * 1_000,
		description: Some(hold::invoice_request::Description::Memo(name.into())),
		min_final_cltv_expiry: Some(18),
		expiry: Some(3600),
		routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	// Boarding just mined blocks. A node paying from a stale tip sets an
	// HTLC expiry the payee rejects, which would fail the payment for an
	// unrelated reason.
	lightning.sync().await;
	sender.pay_lightning_invoice(invoice, None, false).await.unwrap();
	lightning.external.wait_for_hold_invoice_accepted(payment_hash).await;
	payee.cancel(hold::CancelRequest { payment_hash: payment_hash.as_ref().to_vec() }).await.unwrap();
	tokio::time::timeout(Duration::from_secs(60), async {
		tokio::select! {
			r = sender.check_lightning_payment(payment_hash, true) =>
				panic!("refund completed before the sender disappeared: {r:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("the sender must reach its refund request");

	let held = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == payment_hash))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&held).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	let htlc_ids = htlcs.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	assert!(!htlc_ids.is_empty());
	let others = bystander.all_vtxos().await.unwrap().into_iter().map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let other_coins = db.read(async |t| t.get_user_vtxos_by_id(&others).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	let other_ids = other_coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let other_principal = other_coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert_eq!(other_principal, 50_000);
	drop(sender);
	drop(bystander);
	drop(proxy);

	// Payouts run from before expiry, so the guard is busy before either
	// wallet becomes payable.
	restart_with_payouts(&srv).await;

	// Test-only fault: a database session holds the sender's HTLC coin rows,
	// so the refund request waits while it holds its payment guard. No
	// payment, coin, attempt or sweep state is changed by this lock.
	let locked = Arc::new(Notify::new());
	let release = Arc::new(Notify::new());
	let lock_task = tokio::spawn({
		let (db, ids, locked, release) = (db.clone(), htlc_ids.clone(), locked.clone(), release.clone());
		async move {
			db.write(async |t| {
				t.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) FOR UPDATE", &[&ids]).await?;
				locked.notify_one();
				release.notified().await;
				Ok(())
			}).await
		}
	});
	tokio::time::timeout(Duration::from_secs(30), locked.notified()).await
		.expect("the test session must lock the HTLC rows");
	let request = revocation.lock().unwrap().take().unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let refund = tokio::spawn(async move { rpc.request_lightning_pay_htlc_revocation(request).await });
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let waiting = db.read(async |t| Ok(t.query_one(
				"SELECT count(*) FROM pg_locks WHERE NOT granted", &[],
			).await?.get::<_, i64>(0))).await.unwrap();
			if waiting > 0 { break; }
			assert!(!refund.is_finished(), "the refund request must wait on the locked rows");
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	}).await.expect("the refund request must wait while holding its payment guard");

	let mut coins = htlcs.clone();
	coins.extend(other_coins.iter().cloned());
	expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	train_fee_estimator(&ctx).await;
	let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &other_ids, &bystander_record.spk, other_principal).await;
	assert!(!refund.is_finished(), "the refund request still holds its payment guard");
	let early = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&htlc_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(early, 0, "the busy payment's coins wait");
	let logs = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
	assert!(logs.contains("expiry wallet group deferred: Lightning payment in progress"));
	println!("busy payment guard: other wallet paid {payout} (fee_sat={fee}) while the refund request waited");

	// The refund request continues. The sender's HTLC coins settle once:
	// either through that refund or through the expiry payout.
	release.notify_one();
	lock_task.await.unwrap().unwrap();
	let refunded = tokio::time::timeout(Duration::from_secs(60), refund).await
		.expect("the refund request must finish").unwrap();
	for _ in 0..10 {
		tokio::time::sleep(Duration::from_secs(1)).await;
		ctx.generate_blocks(1).await;
	}
	let state = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FILTER (WHERE v.oor_spent_txid IS NOT NULL),
			count(*) FILTER (WHERE s.id IS NOT NULL),
			count(*) FILTER (WHERE h.offchain_resolution='revoked')
		 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id LEFT JOIN expiry_settlement s ON s.id=v.vtxo_id
		 WHERE v.vtxo_id=ANY($1)", &[&htlc_ids],
	).await?)).await.unwrap();
	let (refunded_in_ark, paid_on_chain, revoked) =
		(state.get::<_, i64>(0) as usize, state.get::<_, i64>(1) as usize, state.get::<_, i64>(2) as usize);
	match &refunded {
		Ok(_) => assert_eq!((refunded_in_ark, paid_on_chain), (htlc_ids.len(), 0)),
		Err(_) => assert_eq!((refunded_in_ark, paid_on_chain), (0, htlc_ids.len())),
	}
	assert_eq!(revoked, htlc_ids.len());
	println!("busy payment released: refund_request_ok={}, refunded_in_ark={refunded_in_ark}, \
		paid_on_chain={paid_on_chain}", refunded.is_ok());
	srv.stop().await.unwrap();
	let spend_txid = restore_and_spend(&ctx, &bystander_mnemonic, payout).await;
	println!("other wallet recovered: payout={payout}, confirmed_seed_spend={spend_txid}");
}

/// The server's own node stops answering while a failed send's coins are
/// payable. That is no evidence the payment failed: the sender's coins wait,
/// and other wallets are still paid. Once the node answers, the sender is
/// refunded.
#[tokio::test]
async fn fallback_unresponsive_node_holds_send_refund_only() {
	Box::pin(unresponsive_node(1)).await;
}

/// Several failed sends wait on the same silent node. The tick asks that
/// node once, not once per payment, so other wallets are paid promptly.
#[tokio::test]
async fn fallback_unresponsive_node_does_not_stall_tick() {
	Box::pin(unresponsive_node(6)).await;
}

async fn unresponsive_node(sends: usize) {
	let name = if sends == 1 { "fallback_unresponsive_node_holds_send_refund_only" }
		else { "fallback_unresponsive_node_does_not_stall_tick" };
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.lightningd(&lightning.internal).funded(btc(10)).watchmand().create().await;
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let reached = Arc::new(Notify::new());
	let revocation = Arc::new(Mutex::new(None));
	let proxy = srv.start_proxy_no_mailbox(AbsentSender {
		reached: reached.clone(), request: revocation.clone(),
	}).await;
	let sender_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let sender = ctx.bark_sdk("sender", &proxy.address).mnemonic(sender_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let sender_record = sender.fallback_destination().await.unwrap();
	let bystander_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let bystander = ctx.bark_sdk("bystander", &srv).mnemonic(bystander_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(50_000)).create().await;
	bystander.stop_daemon_wait().await.unwrap();
	let bystander_record = bystander.fallback_destination().await.unwrap();

	// Real failed payments: the external payee cancels each hold invoice.
	let amount = if sends == 1 { 100_000 } else { 20_000 };
	let mut payee = lightning.external.hold_client().await;
	let mut payment_hashes = Vec::new();
	for i in 0..sends {
		let preimage = Preimage::random();
		let payment_hash = preimage.compute_payment_hash();
		let invoice = payee.invoice(hold::InvoiceRequest {
			payment_hash: payment_hash.as_ref().to_vec(),
			amount_msat: amount * 1_000,
			description: Some(hold::invoice_request::Description::Memo(format!("{name} {i}"))),
			min_final_cltv_expiry: Some(18),
			expiry: Some(3600),
			routing_hints: vec![],
		}).await.unwrap().into_inner().bolt11;
		// Boarding just mined blocks. A node paying from a stale tip sets an
		// HTLC expiry the payee rejects, which would fail the payment for an
		// unrelated reason.
		lightning.sync().await;
		sender.pay_lightning_invoice(invoice, None, false).await.unwrap();
		lightning.external.wait_for_hold_invoice_accepted(payment_hash).await;
		payee.cancel(hold::CancelRequest { payment_hash: payment_hash.as_ref().to_vec() }).await.unwrap();
		tokio::time::timeout(Duration::from_secs(60), async {
			tokio::select! {
				r = sender.check_lightning_payment(payment_hash, true) =>
					panic!("refund completed before the sender disappeared: {r:?}"),
				_ = reached.notified() => {},
			}
		}).await.expect("the sender must reach its refund request");
		payment_hashes.push(payment_hash);
	}

	let held = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| payment_hashes.contains(&p.payment_hash)))
		.map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&held).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	let htlc_ids = htlcs.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let htlc_principal = htlcs.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert_eq!(htlcs.iter().map(|v| v.policy().as_server_htlc_send().unwrap().payment_hash)
		.collect::<BTreeSet<_>>().len(), sends);
	let others = bystander.all_vtxos().await.unwrap().into_iter().map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let other_coins = db.read(async |t| t.get_user_vtxos_by_id(&others).await).await.unwrap()
		.into_iter().map(|v| v.vtxo).collect::<Vec<Vtxo>>();
	let other_ids = other_coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let other_principal = other_coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	drop(sender);
	drop(bystander);
	drop(proxy);

	// Payouts run from before expiry, so the node is already silent when
	// either wallet becomes payable.
	restart_with_payouts(&srv).await;

	// A real outage of the server's own node: its container is frozen, so it
	// accepts connections but answers nothing. Nothing else is changed.
	let container = lightning.internal.container_name().to_owned();
	let docker = |action: &str| {
		let status = std::process::Command::new("docker").args([action, &container]).status().unwrap();
		assert!(status.success(), "docker {action} {container}");
	};
	docker("pause");
	let mut coins = htlcs.clone();
	coins.extend(other_coins.iter().cloned());
	expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	train_fee_estimator(&ctx).await;
	// Each query of the silent node gives up after ten seconds. Asked once
	// per payment, a tick would scan for at least `sends` times as long
	// before it pays anyone, so the tick durations are checked below.
	let payable = std::time::Instant::now();
	let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &other_ids, &bystander_record.spk, other_principal).await;
	let waited = payable.elapsed();
	println!("unresponsive node: {sends} held sends; other wallet paid {payout} (fee_sat={fee}) {waited:?} after it became payable");
	let early = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&htlc_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(early, 0, "an unanswered node query must not refund the sender");
	let logs = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
	let summaries = logs.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
		.filter(|e| e["message"] == "expiry payout tick summary" && e["candidates"].as_u64() > Some(0))
		// The log writes this u128 field as a string.
		.map(|e| e["duration_ms"].as_str().unwrap().parse::<u64>().unwrap()).collect::<Vec<_>>();
	println!("tick durations with candidates (ms): {summaries:?}");
	if sends > 1 {
		assert!(summaries.iter().all(|ms| *ms < 35_000),
			"a tick waited behind {sends} queries of the silent node: {summaries:?}");
	}

	docker("unpause");
	let (refund, refund_fee) = wait_and_reconcile_payout(&ctx, &db, &htlc_ids, &sender_record.spk, htlc_principal).await;
	let revoked = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='revoked'", &[&htlc_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(revoked as usize, htlc_ids.len());
	println!("node answers again: sender refunded {refund} (fee_sat={refund_fee})");
	srv.stop().await.unwrap();
	let other_spend = restore_and_spend(&ctx, &bystander_mnemonic, payout).await;
	let sender_spend = restore_and_spend(&ctx, &sender_mnemonic, refund).await;
	println!("seed-only spends confirmed: other={other_spend}, sender={sender_spend}");
}

/// Mine past every coin's expiry, then wait until a confirmed sweep spends
/// each backing anchor. The payout task requires that real chain evidence.
pub(crate) async fn expire_and_confirm_sweeps(ctx: &TestContext, db: &Db, coins: &[Vtxo]) {
	let core = ctx.bitcoind().sync_client();
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let anchors = coins.iter().map(|v| v.chain_anchor().to_string())
		.collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT vtxo_id,onchain_spent_txid FROM vtxo WHERE vtxo_id=ANY($1)", &[&anchors],
			).await?)).await.unwrap();
			assert_eq!(rows.len(), anchors.len());
			if rows.iter().all(|row| {
				let Some(txid) = row.get::<_, Option<String>>("onchain_spent_txid") else { return false; };
				let txid: Txid = txid.parse().unwrap();
				let info = core.get_raw_transaction_info(&txid, None).unwrap();
				let tx = core.get_raw_transaction(&txid, None).unwrap();
				let anchor: OutPoint = row.get::<_, String>("vtxo_id").parse().unwrap();
				info.confirmations.unwrap_or(0) > 0 && tx.input.iter().any(|i| i.previous_output == anchor)
			}) { break; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("backing sweep must confirm before enabling payout");
	println!("all {} backing anchors swept and confirmed", anchors.len());
}

/// Train Core's estimator with real transactions after the large expiry
/// advance, which ages out old fee history, then restart with payouts enabled.
pub(crate) async fn enable_payouts(ctx: &TestContext, srv: &Captaind) {
	train_fee_estimator(ctx).await;
	restart_with_payouts(srv).await;
}

async fn train_fee_estimator(ctx: &TestContext) {
	let core = ctx.bitcoind().sync_client();
	for _ in 0..12 {
		for _ in 0..8 {
			let address = ctx.bitcoind().get_new_address();
			let _: Txid = core.call("sendtoaddress", &[
				address.to_string().into(), 0.001.into(), "".into(), "".into(),
				false.into(), true.into(), serde_json::Value::Null, "unset".into(),
				serde_json::Value::Null, 3.into(),
			]).unwrap();
		}
		ctx.generate_blocks(1).await;
	}
	let estimate: serde_json::Value = core.call("estimatesmartfee", &[6.into(), "economical".into()]).unwrap();
	assert!(estimate["feerate"].as_f64().is_some(), "real economical fee estimate required: {estimate}");
}

async fn restart_with_payouts(srv: &Captaind) {
	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.interval = Duration::from_secs(1);
		config.expiry_payout.grace_blocks = 0;
		config.expiry_payout.sweep_min_confs = 1;
		config.expiry_payout.min_payout_sat = 10_000;
		config.expiry_payout.watchman_config = Some(srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE));
	}
	srv.start().await.unwrap();
}

/// A valid estimate and successful ticks must exercise selection; a disabled
/// task or a missing estimate would prove nothing about a refusal.
async fn assert_no_payout(ctx: &TestContext, db: &Db, ids: &[String], destination: &ScriptBuf) {
	tokio::time::sleep(Duration::from_secs(12)).await;
	let logs = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
	let events = logs.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
		.collect::<Vec<_>>();
	assert!(events.iter().filter(|e| e["message"] == "expiry payout tick summary"
		&& e["success"] == true).count() >= 5);
	assert!(!events.iter().any(|e| e["message"] == "no real fee estimate; expiry payouts wait"));
	let paid = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(paid, 0, "these coins are not a user entitlement");
	let address = Address::from_script(destination, Network::Regtest).unwrap();
	let core = ctx.bitcoind().sync_client();
	let scan: serde_json::Value = core.call("scantxoutset", &[
		"start".into(), serde_json::json!([{"desc": format!("addr({address})")}]),
	]).unwrap();
	assert_eq!(scan["success"], true);
	assert!(scan["unspents"].as_array().unwrap().is_empty());
}

/// Wait for the payout, then reconcile its single output and the actual
/// miner fee from the confirmed transaction's real input values.
async fn wait_and_reconcile_payout(
	ctx: &TestContext, db: &Db, ids: &[String], destination: &ScriptBuf, principal: u64,
) -> (OutPoint, u64) {
	let rows = tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT txid,fee_sat,spk FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("the absent user must be paid within 90 seconds");
	let txid: Txid = rows[0].get::<_, String>("txid").parse().unwrap();
	let fee = rows[0].get::<_, i64>("fee_sat") as u64;
	for row in &rows {
		assert_eq!(row.get::<_, String>("txid"), txid.to_string());
		assert_eq!(row.get::<_, Vec<u8>>("spk"), destination.as_bytes());
	}
	ctx.await_transaction(txid).await;
	ctx.generate_blocks(1).await;
	let core = ctx.bitcoind().sync_client();
	let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
	let outputs = tx.output.iter().enumerate().filter(|(_, o)| &o.script_pubkey == destination).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	let (vout, output) = outputs[0];
	// The wallet's other expired coins share its destination and its output.
	let gross = db.read(async |t| Ok(t.query_one(
		"SELECT sum(v.amount)::bigint, count(*) FROM expiry_settlement s JOIN vtxo v ON v.vtxo_id=s.id
		 WHERE s.txid=$1 AND s.spk=$2", &[&txid.to_string(), &destination.as_bytes()],
	).await?)).await.unwrap();
	let (gross, grouped) = (gross.get::<_, i64>(0) as u64, gross.get::<_, i64>(1) as usize);
	assert!(grouped >= ids.len() && gross >= principal);
	// Other wallets can share the transaction. Each destination's output
	// carries its share of the fee, and the shares add up to the whole fee.
	let destinations = db.read(async |t| Ok(t.query(
		"SELECT s.spk, sum(v.amount)::bigint FROM expiry_settlement s JOIN vtxo v ON v.vtxo_id=s.id
		 WHERE s.txid=$1 GROUP BY s.spk", &[&txid.to_string()],
	).await?)).await.unwrap();
	let mut deducted = 0;
	for row in &destinations {
		let spk = ScriptBuf::from(row.get::<_, Vec<u8>>(0));
		let paid = tx.output.iter().filter(|o| o.script_pubkey == spk).collect::<Vec<_>>();
		assert_eq!(paid.len(), 1);
		deducted += (row.get::<_, i64>(1) as u64).checked_sub(paid[0].value.to_sat()).unwrap();
	}
	assert_eq!(deducted, fee);
	let share = gross - output.value.to_sat();
	assert!(share > 0 && share <= fee);
	println!("payout output: gross_sat={gross}, coins={grouped}, entitlement_sat={principal}, fee_share_sat={share}, \
		tx_fee_sat={fee}, destinations={}", destinations.len());
	let total_in = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
		.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
	let spks = destinations.iter().map(|r| ScriptBuf::from(r.get::<_, Vec<u8>>(0))).collect::<Vec<_>>();
	assert_change_pays_own_weight(&tx, total_in - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>(), fee, &spks);
	assert!(fee > 0);
	(OutPoint::new(txid, vout as u32), fee)
}

/// The settlement rows record the recipients' part of a payout's mining fee.
/// Its operator change pays the rest, the fee for its own weight at the same
/// rate: both round up, so `operator / tx_fee` is `change weight / weight`
/// within one sat.
pub(crate) fn assert_change_pays_own_weight(tx: &Transaction, tx_fee: u64, recipient_fee: u64, recipients: &[ScriptBuf]) {
	let change = tx.output.iter().filter(|o| !recipients.contains(&o.script_pubkey)).collect::<Vec<_>>();
	assert_eq!(change.len(), 1, "every payout carries one operator change output");
	let (weight, change_weight) = (tx.weight().to_wu(), change[0].weight().to_wu());
	let operator = tx_fee.checked_sub(recipient_fee).expect("recipients pay at most the whole fee");
	assert!(operator > 0 && operator * weight > (tx_fee - 1) * change_weight
		&& operator * weight < tx_fee * change_weight + weight,
		"operator paid {operator} of {tx_fee} sat for {change_weight} of {weight} WU");
}

/// A plain BIP84 wallet built from the mnemonic alone, with the standard
/// gap of 20 and the chain, discovers the payout and spends it.
async fn restore_and_spend(ctx: &TestContext, mnemonic: &bip39::Mnemonic, payout: OutPoint) -> Txid {
	let core = ctx.bitcoind().sync_client();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let mut restored = bdk_wallet::Wallet::create(
		Bip84(master, KeychainKind::External), Bip84(master, KeychainKind::Internal),
	).network(Network::Regtest).lookahead(20).create_wallet_no_persist().unwrap();
	for height in 1..=core.get_block_count().unwrap() {
		let hash = core.get_block_hash(height).unwrap();
		restored.apply_block(&core.get_block(&hash).unwrap(), height as u32).unwrap();
	}
	let tx: Transaction = core.get_raw_transaction(&payout.txid, None).unwrap();
	assert_eq!(restored.list_unspent().find(|u| u.outpoint == payout)
		.expect("plain mnemonic-only BIP84 restore must discover the payout").txout.value,
		tx.output[payout.vout as usize].value);
	let destination = ctx.bitcoind().get_new_address();
	let mut builder = restored.build_tx();
	builder.add_utxo(payout).unwrap().manually_selected_only()
		.drain_to(destination.script_pubkey()).fee_rate(FeeRate::from_sat_per_vb(3).unwrap());
	let mut psbt = builder.finish().unwrap();
	assert!(restored.sign(&mut psbt, SignOptions::default()).unwrap());
	let spend = psbt.extract_tx().unwrap();
	assert_eq!(spend.input.len(), 1);
	assert_eq!(spend.input[0].previous_output, payout);
	let spend_txid = core.send_raw_transaction(&spend).unwrap();
	ctx.bitcoind().generate(1).await;
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	assert_eq!(ctx.bitcoind().get_received_by_address(&destination), spend.output[0].value);
	spend_txid
}
