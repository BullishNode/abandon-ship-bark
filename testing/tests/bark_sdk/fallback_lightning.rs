use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use ark::Vtxo;
use ark::lightning::{PaymentHash, Preimage};
use ark_testing::{Captaind, TestContext, btc, sat};
use ark_testing::daemon::captaind::{ArkClient, proxy::ArkRpcProxy};
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
use tonic::transport::Channel;

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
		ReceiveOutcome::Uncollected => "fallback_uncollected_lightning_receive_is_held",
		ReceiveOutcome::CollectedStatusLost => "fallback_collected_lightning_receive_pays_after_status_recovers",
	};
	let disclosed = outcome != ReceiveOutcome::Undisclosed;
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
	if !disclosed {
		// The user disappears before disclosing the preimage. Cancel the
		// real held invoice, so the external payer keeps its funds.
		lightning.internal.hold_client().await.cancel(cln_rpc::plugins::hold::CancelRequest {
			payment_hash: payment_hash.to_vec(),
		}).await.unwrap();
	}
	let paid = tokio::time::timeout(Duration::from_secs(30), paying).await
		.expect("external payment must finish").unwrap();
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
		ReceiveOutcome::Undisclosed => {
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
	restart_with_payouts(&ctx, &srv).await;

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
	let name = "fallback_unresponsive_node_holds_send_refund_only";
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
	let htlc_principal = htlcs.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert!(!htlc_ids.is_empty());
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
	restart_with_payouts(&ctx, &srv).await;

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
	let (payout, fee) = wait_and_reconcile_payout(&ctx, &db, &other_ids, &bystander_record.spk, other_principal).await;
	let early = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&htlc_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(early, 0, "an unanswered node query must not refund the sender");
	println!("unresponsive node: other wallet paid {payout} (fee_sat={fee}); sender refund waits");

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
async fn expire_and_confirm_sweeps(ctx: &TestContext, db: &Db, coins: &[Vtxo]) {
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
async fn enable_payouts(ctx: &TestContext, srv: &Captaind) {
	train_fee_estimator(ctx).await;
	restart_with_payouts(ctx, srv).await;
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

async fn restart_with_payouts(ctx: &TestContext, srv: &Captaind) {
	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.interval = Duration::from_secs(1);
		config.expiry_payout.grace_blocks = 0;
		config.expiry_payout.sweep_min_confs = 1;
		config.expiry_payout.min_payout_sat = 10_000;
		config.expiry_payout.max_batch = 100;
		config.expiry_payout.receipt_dir = ctx.datadir.join("receipts");
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
	assert_eq!(fee, total_in - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>());
	assert!(fee > 0);
	(OutPoint::new(txid, vout as u32), fee)
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
