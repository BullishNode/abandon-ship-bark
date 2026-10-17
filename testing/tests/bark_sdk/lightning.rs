use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;

use ark::lightning::{PaymentHash, Preimage};
use ark::attestations::VtxoStatusAttestation;
use ark::ProtocolEncoding;
use bitcoin_ext::{BlockDelta, TxOutExt};
use ark_testing::{TestContext, btc, util::{FutureExt, poll_interval}};
use ark_testing::balance::assert_balance_consistent;

use bark::actions::lightning::pay::LightningSendState;
use bark::actions::lightning::receive::LightningReceiveState;
use bark::movement::MovementStatus;
use bark::subsystem::Subsystem;
use bark::{LightningSendOptions, OpenWalletArgs, Wallet, WalletSeed};
use bark::onchain::OnchainWallet;
use bark::persist::sqlite::SqliteClient;
use bitcoin::Network;
use bitcoin::{FeeRate, OutPoint, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::rpc::RpcApi;
use cln_rpc::plugins::hold;
use server::database::Db;
use server_rpc::protos;
use server_rpc::protos::mailbox_server::mailbox_message::Message as MailboxMsg;
use ark_testing::daemon::captaind::{self, ArkClient};

/// Wait until the hold plugin holds the payment's HTLCs. The invoice turns
/// ACCEPTED only once every HTLC is irrevocably committed and handed to the
/// plugin; a settle before that fails with "no HTLCs to settle".
async fn wait_for_hold_invoice_accepted(
	hold_client: &mut hold::hold_client::HoldClient<tonic::transport::Channel>,
	payment_hash: PaymentHash,
) {
	async {
		loop {
			let invoices = hold_client.list(hold::ListRequest {
				constraint: Some(hold::list_request::Constraint::PaymentHash(
					payment_hash.as_ref().to_vec(),
				)),
			}).await.expect("list hold invoices").into_inner().invoices;
			let invoice = invoices.first().expect("hold invoice should exist");
			if invoice.state() == hold::InvoiceState::Accepted {
				break;
			}
			tokio::time::sleep(poll_interval()).await;
		}
	}.wait_millis(30_000).await
}

#[tokio::test]
async fn pay_hold_succeeds() {
	let ctx = TestContext::new("bark_sdk/pay_hold_succeds").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;

	let board_amount = btc(2);
	let wallet = ctx.bark_sdk("bark", &srv)
		.boarded(board_amount)
		.create().await;

	lightning.sync().await;

	// Build a hold invoice on the external CLN node. The HTLC won't be
	// settled until we hand the preimage to the hold plugin.
	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let invoice_amount = btc(0.5);

	let mut hold_client = lightning.external.hold_client().await;
	let invoice = hold_client.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
		amount_msat: invoice_amount.to_sat() * 1_000,
		description: Some(hold::invoice_request::Description::Memo(
			"sdk_pay_lightning_hold_invoice".into(),
		)),
		min_final_cltv_expiry: Some(18),
		expiry: Some(3600),
		routing_hints: vec![],
	}).await.expect("create hold invoice").into_inner().bolt11;

	// Initiates the payment; HTLCs are now in flight but the external
	// node holds them. pay_lightning_invoice returns once the HTLCs are
	// sent, not once they settle.
	wallet.pay_lightning_invoice(invoice, None, false).await
		.expect("pay_lightning_invoice failed");

	wait_for_hold_invoice_accepted(&mut hold_client, payment_hash).await;

	// The payment is pending because the receiver hasn't claimed it yet
	// We report this correctly
	let status = wallet.check_lightning_payment(payment_hash, false).await .expect("no-wait check errored");
	match status {
		LightningSendState::InProgress(pending) => {
			let pending_balance = pending.payment_amount + pending.fee;
			let balance = assert_balance_consistent(&wallet, false).await;
			assert_eq!(balance.spendable, board_amount - pending_balance);
			assert_eq!(balance.pending_lightning_send, pending_balance);
		}
		other => panic!("Payment should be pending was {:?}", other),
	};

	// Wait for the payment to get completed
	let waiter = {
		let wallet = wallet.clone();
		tokio::spawn(async move {
			wallet.check_lightning_payment(payment_hash, true).await
		})
	};

	// We should wait if the invoice isn't settled
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(!waiter.is_finished(), "waiting check resolved before settlement");

	// Settle the invoice
	hold_client.settle(hold::SettleRequest {
		payment_preimage: preimage.as_ref().to_vec(),
	}).await.expect("settle hold invoice");

	// ready()'s 2s bound is too tight under BARK_DOUBLE_DRIVE_ACTIONS: steps run twice and the send re-polls every 2s.
	let status = waiter.wait_millis(10_000).await.expect("join waiter");
	match &status {
		Ok(LightningSendState::Paid(invoice)) => {
			assert_eq!(invoice.payment_hash, payment_hash);
			assert_eq!(invoice.preimage, preimage);
		},
		err => panic!("Payment did not succeed: {:?}", err),
	}

	// A subsequent check should return the same cached result
	let new_status =  wallet.check_lightning_payment(payment_hash, false).await.expect("Failed to query lightning send");
	assert_eq!(new_status, status.unwrap());

	let balance = assert_balance_consistent(&wallet, false).await;
	assert_eq!(balance.spendable, board_amount - invoice_amount);
}

#[tokio::test]
async fn pay_hold_with_near_expiry_inputs_succeeds() {
	let ctx = TestContext::new("bark_sdk/pay_hold_with_near_expiry_inputs_succeeds").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	// Short vtxo lifetime keeps the block-advance cheap; absurdly long
	// round interval keeps any background refresh out of the picture.
	let srv = ctx.captaind("server").lightningd(&lightning.internal).cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(100);
		cfg.round_interval = Duration::from_secs(86400);
	}).funded(btc(10)).create().await;

	let board_amount = btc(2);
	let wallet = ctx.bark_sdk("bark", &srv)
		.boarded(board_amount)
		.create().await;

	// Push the boarded VTXO past its expiry. An expired VTXO can still fund
	// a lightning payment; it and its change count as needs_refresh. The
	// lightning send used to spuriously fail this scenario because it
	// conflated input-VTXO expiry with HTLC expiry; the fresh HTLC's own
	// CLTV (htlc_send_expiry_delta blocks ahead) is still comfortably far
	// out, so the payment should still settle normally.
	ctx.generate_blocks(98).await;

	lightning.sync().await;

	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let invoice_amount = btc(0.5);

	let mut hold_client = lightning.external.hold_client().await;
	let invoice = hold_client.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
		amount_msat: invoice_amount.to_sat() * 1_000,
		description: Some(hold::invoice_request::Description::Memo(
			"pay_hold_with_expired_inputs_succeeds".into(),
		)),
		min_final_cltv_expiry: Some(18),
		expiry: Some(3600),
		routing_hints: vec![],
	}).await.expect("create hold invoice").into_inner().bolt11;

	wallet.pay_lightning_invoice(invoice, None, false).await
		.expect("pay_lightning_invoice failed");

	wait_for_hold_invoice_accepted(&mut hold_client, payment_hash).await;

	// The payment is pending because the receiver hasn't claimed it yet
	// We report this correctly
	let status = wallet.check_lightning_payment(payment_hash, false).await .expect("no-wait check errored");
	match status {
		LightningSendState::InProgress(pending) => {
			let pending_balance = pending.payment_amount + pending.fee;
			let balance = assert_balance_consistent(&wallet, false).await;
			assert_eq!(balance.needs_refresh, board_amount - pending_balance);
			assert_eq!(balance.pending_lightning_send, pending_balance);
		}
		other => panic!("Payment should be pending was {:?}", other),
	};

	// Wait for the payment to get completed
	let waiter = {
		let wallet = wallet.clone();
		tokio::spawn(async move {
			wallet.check_lightning_payment(payment_hash, true).await
		})
	};

	// We should wait if the invoice isn't settled
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(!waiter.is_finished(), "waiting check resolved before settlement");

	// Settle the invoice
	hold_client.settle(hold::SettleRequest {
		payment_preimage: preimage.as_ref().to_vec(),
	}).await.expect("settle hold invoice");

	// ready()'s 2s bound is too tight under BARK_DOUBLE_DRIVE_ACTIONS: steps run twice and the send re-polls every 2s.
	let status = waiter.wait_millis(10_000).await.expect("join waiter");
	match &status {
		Ok(LightningSendState::Paid(invoice)) => {
			assert_eq!(invoice.payment_hash, payment_hash);
			assert_eq!(invoice.preimage, preimage);
		},
		err => panic!("Payment did not succeed: {:?}", err),
	}

	// A subsequent check should return the same cached result
	let new_status =  wallet.check_lightning_payment(payment_hash, false).await.expect("Failed to query lightning send");
	assert_eq!(new_status, status.unwrap());

	let balance = assert_balance_consistent(&wallet, false).await;
	assert_eq!(balance.needs_refresh, board_amount - invoice_amount);
}

#[tokio::test]
async fn pay_hold_refused() {
	let ctx = TestContext::new("bark_sdk/pay_hold_refused").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;

	let board_amount = btc(2);
	let wallet = ctx.bark_sdk("bark", &srv)
		.boarded(board_amount)
		.create().await;

	lightning.sync().await;

	// Build a hold invoice on the external CLN node. We never settle it
	// — instead we cancel, which makes the held HTLC fail back through
	// the route.
	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let invoice_amount = btc(0.5);

	let mut hold_client = lightning.external.hold_client().await;
	let invoice = hold_client.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
		amount_msat: invoice_amount.to_sat() * 1_000,
		description: Some(hold::invoice_request::Description::Memo(
			"sdk_pay_lightning_hold_invoice_fail".into(),
		)),
		min_final_cltv_expiry: Some(18),
		expiry: Some(3600),
		routing_hints: vec![],
	}).await.expect("create hold invoice").into_inner().bolt11;

	wallet.pay_lightning_invoice(invoice, None, false).await
		.expect("pay_lightning_invoice failed");

	// The payment is pending because the receiver hasn't claimed or
	// cancelled it yet; while the HTLC is in flight the locked amount
	// should be tracked as pending lightning send and excluded from
	// spendable.
	let status = wallet.check_lightning_payment(payment_hash, false).await.expect("no-wait check errored");
	match status {
		LightningSendState::InProgress(pending) => {
			let pending_balance = pending.payment_amount + pending.fee;
			let balance = assert_balance_consistent(&wallet, false).await;
			assert_eq!(balance.spendable, board_amount - pending_balance);
			assert_eq!(balance.pending_lightning_send, pending_balance);
		}
		other => panic!("Payment should be pending was {:?}", other),
	};

	// Wait for the payment to resolve
	let waiter = {
		let wallet = wallet.clone();
		tokio::spawn(async move {
			wallet.check_lightning_payment(payment_hash, true).await
		})
	};

	// We should wait if the invoice isn't cancelled
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(!waiter.is_finished(), "waiting check resolved before cancel");

	// Cancel the hold invoice — the HTLC fails back to the server and
	// bark revokes the HTLC vtxos, restoring the funds.
	hold_client.cancel(hold::CancelRequest {
		payment_hash: payment_hash.as_ref().to_vec(),
	}).await.expect("cancel hold invoice");

	// On failure the LightningSend record is removed once revocation completes, so the state
	// is Unknown. With CLN 26.06, xpay reports the destination failure before CLN's own sendpay
	// row transitions to failed, so captaind's immediate listpays reconciliation still sees
	// Pending and marks the attempt Submitted. It only flips to Failed on the next
	// process_payment_attempts tick past cln_xpay_timeout + XPAY_TIMEOUT_BUFFER (5s + 15s)
	// after the attempt was created. Wait long enough to clear that gate under
	// BARK_DOUBLE_DRIVE_ACTIONS + a couple of bark re-poll intervals.
	let status = waiter.wait_millis(40_000).await.expect("join waiter")
		.expect("waiting check errored");
	assert_eq!(status, LightningSendState::Unknown);

	// A subsequent check should also report Unknown
	let new_status = wallet.check_lightning_payment(payment_hash, false).await.expect("Failed to query lightning send");
	assert_eq!(new_status, LightningSendState::Unknown);

	// After revocation the HTLC vtxos come back as spendable
	let balance = assert_balance_consistent(&wallet, false).await;
	assert_eq!(balance.spendable, board_amount);
	assert_eq!(balance.pending_lightning_send, btc(0));
}

/// Any integrator should be able to claim a lightning receive off the raw mailbox stream
/// alone, without depending on bark's own background sync loop or any particular sync method:
/// create an invoice, wait on the mailbox stream for the incoming-HTLC notification, then call
/// `try_claim_lightning_receive` once. No daemon and no background syncing runs, so that single
/// claim call has to complete the receive on its own.
#[tokio::test]
async fn receive_claim_on_mailbox_notification() {
	let ctx = TestContext::new("bark_sdk/receive_claim_on_mailbox_notification").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;

	let wallet = ctx.bark_sdk("bark", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true)
		.create().await;

	lightning.sync().await;

	let invoice_amount = btc(0.5);
	let invoice = wallet.bolt11_invoice(invoice_amount, None, None).await
		.expect("creating invoice");
	let payment_hash = PaymentHash::from(&invoice);

	// Subscribe to the mailbox stream before the payment arrives.
	let mut mailbox = wallet.subscribe_mailbox_messages(None).await
		.expect("subscribing to mailbox stream");

	let (pay_result, claim_result) = tokio::join!(
		lightning.external.try_pay_bolt11(invoice.to_string()),
		async {
			// Wait for the server's incoming-payment notification.
			loop {
				let msg = mailbox.next().wait_millis(10_000).await
					.expect("mailbox stream ended before notification")
					.expect("mailbox stream error");
				if let Some(MailboxMsg::IncomingLightningPayment(m)) = msg.message {
					assert_eq!(m.payment_hash, payment_hash.to_vec(),
						"notification for unexpected payment");
					break;
				}
			}
			wallet.try_claim_lightning_receive(payment_hash, false).await
		},
	);

	let state = claim_result.expect("try_claim_lightning_receive errored");
	assert!(matches!(state, LightningReceiveState::Settled(_)),
		"receive should be settled after the claim, got {:?}", state);

	pay_result.expect("lightning payment failed");

	let movements = wallet.history().await.expect("movements");
	let recv_mvt = movements.iter()
		.find(|m| m.subsystem.is_subsystem(Subsystem::LIGHTNING_RECEIVE))
		.expect("no lightning receive movement");
	assert_eq!(recv_mvt.status, MovementStatus::Successful,
		"lightning receive movement should be successful, got {:?}", recv_mvt.status);

	let balance = assert_balance_consistent(&wallet, false).await;
	assert_eq!(balance.spendable, invoice_amount);
}

/// The cooperative claim and the server's hold settler both settle the hold
/// invoice off the same preimage, so either can get there first. Settling the
/// invoice out of band before the claim forces the losing case: the claim
/// must still hand over the claim vtxos rather than fail on the hold plugin's
/// "no HTLCs to settle".
#[tokio::test]
async fn receive_claim_after_hold_invoice_already_settled() {
	let ctx = TestContext::new("bark_sdk/receive_claim_after_hold_invoice_already_settled").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;

	let wallet = ctx.bark_sdk("bark", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true)
		.create().await;

	lightning.sync().await;

	let invoice_amount = btc(0.5);
	let invoice = wallet.bolt11_invoice(invoice_amount, None, None).await
		.expect("creating invoice");
	let payment_hash = PaymentHash::from(&invoice);
	let preimage = wallet.lightning_receive_checkpoint(payment_hash).await
		.expect("fetching receive checkpoint")
		.expect("receive checkpoint should exist")
		.payment_preimage;

	let mut hold_client = lightning.internal.hold_client().await;

	// Subscribe before the payment arrives.
	let mut mailbox = wallet.subscribe_mailbox_messages(None).await
		.expect("subscribing to mailbox stream");

	let (pay_result, claim_result) = tokio::join!(
		lightning.external.try_pay_bolt11(invoice.to_string()),
		async {
			// The server posts the incoming-payment notification only after
			// it has registered the acceptance, so settling after it keeps
			// the server able to hand over the claim vtxos.
			loop {
				let msg = mailbox.next().wait_millis(10_000).await
					.expect("mailbox stream ended before notification")
					.expect("mailbox stream error");
				if let Some(MailboxMsg::IncomingLightningPayment(_)) = msg.message {
					break;
				}
			}

			// Stand in for the hold settler getting there first.
			hold_client.settle(hold::SettleRequest {
				payment_preimage: preimage.as_ref().to_vec(),
			}).await.expect("settling the hold invoice out of band");

			wallet.try_claim_lightning_receive(payment_hash, true).await
		},
	);

	let state = claim_result.expect("try_claim_lightning_receive errored");
	assert!(matches!(state, LightningReceiveState::Settled(_)),
		"receive should be settled after the claim, got {:?}", state);

	pay_result.expect("lightning payment failed");

	let balance = assert_balance_consistent(&wallet, false).await;
	assert_eq!(balance.spendable, invoice_amount);
}

#[tokio::test]
async fn pay_with_retry_for() {
	let ctx = TestContext::new("bark_sdk/pay_with_retry_for").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;

	let board_amount = btc(2);
	let wallet = ctx.bark_sdk("bark", &srv)
		.boarded(board_amount)
		.create().await;

	lightning.sync().await;

	let max = srv.config().cln_xpay_max_retry_for;

	// The server keeps the time on the attempt, which the xpay monitor waits
	// out before reconciling it. 0 makes a single attempt, and a time above
	// the maximum is capped to it.
	let db = Db::connect(&srv.config().postgres).await.expect("connect to server db");
	let one_sec = Duration::from_secs(1);
	for (retry_for, expected) in [
		(Duration::ZERO, Duration::ZERO),
		(max - one_sec, max - one_sec),
		(max + one_sec, max),
	] {
		let invoice = lightning.external.invoice(Some(btc(0.1)), format!("pay-{:?}", retry_for), "pay").await;
		let opts = LightningSendOptions::default().wait(true).retry_for(Some(retry_for));
		let invoice = wallet.pay_lightning_invoice_with(invoice, None, opts).await
			.expect("pay_lightning_invoice failed");

		let attempt = db.read(async |t|
			t.get_latest_payment_attempt_by_payment_hash(invoice.payment_hash()).await
		).await.expect("db read").expect("payment attempt");
		assert_eq!(attempt.retry_for, Some(expected));
	}
}

/// The first real upstream claim succeeds, but its reply never reaches the
/// client. This observes the durable checkpoint while the request is in flight.
#[derive(Clone)]
struct HoldSuccessfulReceiveClaim {
	hold: Arc<AtomicBool>,
	arrived: Arc<tokio::sync::Notify>,
	requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for HoldSuccessfulReceiveClaim {
	async fn claim_lightning_receive(
		&self, upstream: &mut ArkClient, request: protos::ClaimLightningReceiveRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		self.requests.fetch_add(1, Ordering::SeqCst);
		let response = upstream.claim_lightning_receive(request).await?.into_inner();
		if self.hold.swap(false, Ordering::SeqCst) {
			self.arrived.notify_one();
			std::future::pending::<()>().await;
		}
		Ok(response)
	}
}

#[tokio::test]
async fn receive_claim_reply_loss_is_durable_before_wire_and_recovers_after_reopen() {
	receive_claim_reply_loss_reopen(false).await;
}

/// The same actually settled, reply-lost claim must also retain its rightful
/// fallback after reopening, registering the signed leaf, and going absent.
#[cfg(feature = "fallback-test-bip84")]
#[tokio::test]
async fn receive_claim_reply_loss_reopen_abandon_fallback_and_seed_recovery() {
	receive_claim_reply_loss_reopen(true).await;
}

async fn receive_claim_reply_loss_reopen(fallback: bool) {
	let ctx = TestContext::new(if fallback {
		"bark_sdk/receive_claim_reply_loss_fallback_seed_recovery"
	} else {
		"bark_sdk/receive_claim_reply_loss_reopen"
	}).await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let mut builder = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10));
	if fallback { builder = builder.bitcoind(ctx.bitcoind_arc()).watchmand(); }
	let srv = builder.create().await;
	if fallback {
		assert_eq!(srv.config().fees.lightning_receive.base_fee, bitcoin::Amount::ZERO);
		assert_eq!(srv.config().fees.lightning_receive.ppm, ark::fees::PpmFeeRate::ZERO);
	}
	srv.wait_for_vtxopool(&ctx).await;
	let gate = HoldSuccessfulReceiveClaim { hold: Arc::new(AtomicBool::new(true)),
		arrived: Arc::new(tokio::sync::Notify::new()), requests: Arc::new(AtomicUsize::new(0)) };
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let wallet = ctx.bark_sdk("receiver", &proxy).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	lightning.sync().await;
	let amount = btc(0.5);
	let invoice = wallet.bolt11_invoice(amount, None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let mut mailbox = wallet.subscribe_mailbox_messages(None).await.unwrap();
	let (paid, checkpoint) = tokio::join!(lightning.external.try_pay_bolt11(invoice.to_string()), async {
		loop {
			let message = mailbox.next().wait_millis(10_000).await.unwrap().unwrap();
			if let Some(MailboxMsg::IncomingLightningPayment(message)) = message.message {
				assert_eq!(message.payment_hash, hash.to_vec());
				break;
			}
		}
		// One attempt: a pre-wire error must end the test path, rather than
		// retrying from a parked state and accidentally concealing this window.
		let mut claim = Box::pin(wallet.try_claim_lightning_receive(hash, false));
		tokio::select! {
			result = &mut claim => panic!("claim completed before its actual upstream reply was released: {result:?}"),
			_ = async { tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified()).await.unwrap(); } => {},
		}
		let checkpoint = wallet.lightning_receive_checkpoint(hash).await.unwrap().unwrap();
		assert!(futures::poll!(&mut claim).is_pending(),
			"the captured checkpoint must precede any client reply or timeout-error handling");
		assert_eq!(gate.requests.load(Ordering::SeqCst), 1);
		drop(claim);
		checkpoint
	});
	paid.expect("the real Lightning payer should settle from the accepted preimage");
	let mut hold = lightning.internal.hold_client().await;
	let invoices = hold.list(hold::ListRequest {
		constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
	}).await.unwrap().into_inner().invoices;
	assert_eq!(invoices.first().unwrap().state(), hold::InvoiceState::Paid,
		"the real receiver hold invoice, not an injected RPC failure, must already be settled");
	// Assert only after the actual external payment and receiver settlement
	// succeed. The snapshot itself was read while the client awaited a reply.
	let phase = match &checkpoint.progress {
		bark::actions::lightning::receive::Progress::AwaitingPayment => "awaiting_payment",
		bark::actions::lightning::receive::Progress::HtlcsReady(_) => "htlcs_ready",
		bark::actions::lightning::receive::Progress::PreimageRevealed(_) => "preimage_revealed",
		bark::actions::lightning::receive::Progress::Delivering(_) => "delivering",
	};
	assert_eq!(phase, "preimage_revealed",
		"a completed payment with its client reply still lost must already be durably irreversible");
	drop(mailbox);
	wallet.stop_daemon_wait().await.unwrap();
	drop(wallet);
	// Use only the original private seed and original app/Ark databases.
	// The controller never injects a preimage, receipt, or action state.
	let mnemonic = bip39::Mnemonic::from_str(tokio::fs::read_to_string(ctx.datadir.join("receiver/mnemonic"))
		.await.unwrap().trim()).unwrap();
	let database = Arc::new(SqliteClient::open(ctx.datadir.join("receiver/db.sqlite")).unwrap());
	let onchain = OnchainWallet::load_or_create(Network::Regtest, mnemonic.to_seed(""), database.clone()).await.unwrap();
	let script = onchain.fallback_script();
	let mut config = ctx.bark_default_cfg(&proxy, Some(ctx.bitcoind()));
	config.daemon_manual_sync = true;
	let reopened = Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
		OpenWalletArgs { persister: Some(database), onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))),
			fallback_spk: Some(script), run_daemon: false, ..Default::default() }).await.unwrap();
	assert_eq!(reopened.lightning_receive_checkpoint(hash).await.unwrap().unwrap(), checkpoint);
	assert!(matches!(reopened.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
	assert!(gate.requests.load(Ordering::SeqCst) >= 2, "reopen must execute the genuine idempotent claim RPC again");
	assert!(reopened.lightning_receive_checkpoint(hash).await.unwrap().is_none());
	for _ in 0..2 {
		assert!(matches!(reopened.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
		let balance = assert_balance_consistent(&reopened, false).await;
		assert_eq!(balance.spendable, amount);
		assert_eq!(balance.claimable_lightning_receive, bitcoin::Amount::ZERO);
		let receives = reopened.history().await.unwrap().into_iter().filter(|entry|
			entry.subsystem.is_subsystem(Subsystem::LIGHTNING_RECEIVE)).collect::<Vec<_>>();
		assert_eq!(receives.len(), 1, "lost replies and restart must credit the Lightning receive once");
		assert_eq!(receives[0].status, MovementStatus::Successful);
		assert_eq!(receives[0].effective_balance, amount.to_signed().unwrap());
	}
	if !fallback { return; }

	// Settled is not sufficient: independently authenticate that the signed
	// receive leaf was registered, validated, and is ordinarily spendable.
	let coins = reopened.spendable_vtxos().await.unwrap();
	assert_eq!(coins.len(), 1);
	let source = reopened.get_full_vtxo(coins[0].id()).await.unwrap();
	reopened.validate_vtxo(&source).await.unwrap();
	assert_eq!(source.amount(), amount);
	let key = reopened.get_vtxo_key(&source).await.unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let status = rpc.get_vtxo_status(protos::GetVtxoStatusRequest {
		vtxo_id: source.id().to_bytes().to_vec(),
		attestation: VtxoStatusAttestation::new(source.id(), &key).serialize(),
	}).await.unwrap().into_inner();
	assert_eq!(status.spend_state, protos::VtxoSpendState::Spendable as i32);
	let registration = reopened.fallback_destination().await.unwrap();
	assert!(registration.spk.is_p2wpkh());
	let expected = source.amount() - ark_testing::sat(1_000);
	let absent_height = ctx.bitcoind().get_block_count().await;
	drop(reopened);

	// Two distinct confirmed sponsor UTXOs also permit unrelated operator
	// pool roots to expire. Neither is a deposit into the absent receiver.
	for _ in 0..2 {
		ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await, ark_testing::sat(100_000)).await;
	}
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	assert!(height < source.expiry_height().to_u32());
	let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;

	// Read the real background journal; the test never inserts a decision,
	// ownership result, registration, or payout transaction into Postgres.
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (parent, child, recorded_child_fee) = tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt(
				"SELECT a.sweep_tx,f.raw_tx,f.fee_sat FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				JOIN expiry_fee_child f ON f.allocation_id=a.id \
				WHERE e.vtxo_id=$1 AND c.active AND a.broadcast_at IS NOT NULL \
				ORDER BY f.id DESC LIMIT 1", &[&source.id().to_string()],
			).await?)).await.unwrap();
			if let Some(row) = row {
				break (deserialize::<Transaction>(row.get("sweep_tx")).unwrap(),
					deserialize::<Transaction>(row.get("raw_tx")).unwrap(), row.get::<_, i64>("fee_sat"));
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the registered received leaf never reached an actual background payout");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	assert_eq!(ctx.bitcoind().await_transaction(child.compute_txid()).await, child);
	assert_eq!(parent.version.0, 3);
	assert!(parent.vsize() <= 10_000);
	let anchors = parent.output.iter().enumerate().filter(|(_, output)| output.is_p2a_fee_anchor()).collect::<Vec<_>>();
	assert_eq!(anchors.len(), 1);
	let anchor = OutPoint::new(parent.compute_txid(), anchors[0].0 as u32);
	assert!(child.input.iter().any(|input| input.previous_output == anchor),
		"the measured fee subsidy must actually spend this payout parent's P2A");
	let outputs = parent.output.iter().enumerate().filter(|(_, out)| out.script_pubkey == registration.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert_eq!(outputs[0].1.value, expected);
	let point = OutPoint::new(parent.compute_txid(), outputs[0].0 as u32);
	let mut parent_input_sat = 0;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		parent_input_sat += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent_input_sat, parent.output.iter().map(|out| out.value.to_sat()).sum::<u64>());
	let mut child_input_sat = 0;
	for input in &child.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		child_input_sat += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	let subsidy = child_input_sat - child.output.iter().map(|out| out.value.to_sat()).sum::<u64>();
	assert!(subsidy > 0);
	assert_eq!(subsidy, u64::try_from(recorded_child_fee).unwrap());
	let confirmed = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	// Core 31 reports P2A as `anchor`; raw JSON avoids the old RPC model's
	// unsupported script-type decoder while checking the actual canonical block.
	let core = ctx.bitcoind().sync_client();
	let parent_chain: serde_json::Value = core.call("getrawtransaction", &[
		serde_json::json!(parent.compute_txid().to_string()), serde_json::json!(true),
	]).unwrap();
	let child_chain: serde_json::Value = core.call("getrawtransaction", &[
		serde_json::json!(child.compute_txid().to_string()), serde_json::json!(true),
	]).unwrap();
	assert!(parent_chain["confirmations"].as_u64().unwrap() >= 6);
	assert!(child_chain["confirmations"].as_u64().unwrap() >= 6);
	let payout_block = parent_chain["blockhash"].as_str().unwrap();
	assert_eq!(child_chain["blockhash"].as_str().unwrap(), payout_block);
	let header: serde_json::Value = core.call("getblockheader", &[
		serde_json::json!(payout_block), serde_json::json!(true),
	]).unwrap();
	assert!(header["confirmations"].as_u64().unwrap() >= 6);
	assert_eq!(core.get_block_hash(header["height"].as_u64().unwrap()).unwrap().to_string(), payout_block);
	let payout = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	assert!(payout.confirmations >= 6);
	assert_eq!(payout.value, expected);
	// The payout script belongs to the absent SDK wallet, not Core's test
	// treasury. Count actual chain outputs without wallet import/accounting.
	let mut received = bitcoin::Amount::ZERO;
	for height in absent_height + 1..=u64::from(confirmed.to_u32()) {
		let block = core.get_block(&core.get_block_hash(height).unwrap()).unwrap();
		for tx in block.txdata {
			received += tx.output.iter().filter(|output| output.script_pubkey == registration.spk)
				.map(|output| output.value).sum::<bitcoin::Amount>();
		}
	}
	assert_eq!(received, expected,
		"the old receive HTLC must not create another payout to the same owner");

	// New Ark and BDK databases, original seed only; no leaf IDs, receipts,
	// or payout transaction are supplied to the restored wallet.
	let restored = ctx.bark_sdk("seed_only", &srv).mnemonic(mnemonic)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	assert_eq!(restored.fallback_destination().await.unwrap(), registration);
	restored.sync_onchain().await.unwrap();
	let onchain = restored.onchain().unwrap();
	assert_eq!(onchain.read().await.balance().await, expected);
	restored.sync_fallback_payouts().await.unwrap();
	let receipts = restored.fallback_payouts().await.unwrap();
	let receipt = receipts.iter().find(|receipt| receipt.vtxo_id == source.id()).unwrap();
	assert!(receipt.attempts.iter().any(|attempt| attempt.active && attempt.payout == expected &&
		attempt.output_index == Some(point.vout) && attempt.parent.as_ref().is_some_and(|tx| tx.compute_txid() == point.txid)));
	let destination = ctx.bitcoind().get_new_address();
	let psbt = onchain.write().await.prepare_drain_tx(destination.clone(), FeeRate::from_sat_per_vb(2).unwrap()).await.unwrap();
	let recovery_fee = psbt.fee().unwrap();
	assert!(recovery_fee > bitcoin::Amount::ZERO);
	let signed = onchain.write().await.sign_psbt(psbt).await.unwrap();
	let recovery = onchain.write().await.finish_psbt(signed).await.unwrap().extract_tx().unwrap();
	assert_eq!(recovery.input.len(), 1, "seed recovery must spend only its actual fallback outpoint");
	assert_eq!(recovery.input[0].previous_output, point);
	assert_eq!(recovery.input[0].witness.len(), 2, "actual BIP84 ownership must authorize the recovery spend");
	assert_eq!(recovery.output.len(), 1);
	assert_eq!(recovery.output[0].script_pubkey, destination.script_pubkey());
	let recovered_value = expected - recovery_fee;
	assert_eq!(recovery.output[0].value, recovered_value);
	assert_eq!(payout.value, recovery.output.iter().map(|output| output.value).sum::<bitcoin::Amount>() + recovery_fee);
	restored.chain().broadcast_tx(&recovery).await.unwrap();
	assert_eq!(ctx.bitcoind().await_transaction(recovery.compute_txid()).await, recovery);
	ctx.generate_blocks(6).await;
	let recovered = core.get_tx_out(&recovery.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(recovered.confirmations >= 6);
	assert_eq!(recovered.value, recovered_value);
	assert_eq!(ctx.bitcoind().get_received_by_address(&destination), recovered_value);
	assert!(ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
	std::fs::write(ctx.datadir.join("actual-lightning-fallback-recovery.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"status": "PASS", "scope": "own_wallet_paid_claim_reply_lost_reopened_registered_abandoned",
		"payment_hash": hash, "received_vtxo": source.id(), "expiry_height": source.expiry_height(),
		"received_sat": source.amount().to_sat(), "payout_fee_sat": 1_000, "payout_sat": expected.to_sat(),
		"watchman_subsidy_sat": subsidy, "fallback_outpoint": point,
		"payout_and_fee_child_confirmations": child_chain["confirmations"], "payout_and_fee_child_block": payout_block,
		"recovery_fee_sat": recovery_fee.to_sat(), "recovered_sat": recovered_value.to_sat(),
		"recovery_confirmations": recovered.confirmations,
		"parent": bitcoin::consensus::encode::serialize_hex(&parent),
		"fee_child": bitcoin::consensus::encode::serialize_hex(&child),
		"recovery": bitcoin::consensus::encode::serialize_hex(&recovery),
		"fresh_ark_and_bdk_databases": true, "original_seed_only": true,
		"distinct_recipient": false, "financial_sql_writes": false,
	})).unwrap()).unwrap();
}
