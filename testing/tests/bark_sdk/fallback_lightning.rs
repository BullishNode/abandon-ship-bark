use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use ark::Vtxo;
use ark::lightning::PaymentHash;
use ark_testing::{TestContext, btc, sat};
use ark_testing::daemon::captaind::{ArkClient, proxy::ArkRpcProxy};
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use bdk_wallet::{KeychainKind, SignOptions};
use bdk_wallet::template::Bip84;
use bitcoin::{Address, FeeRate, Network, OutPoint, Transaction, Txid};
use bitcoin::bip32::Xpriv;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server_rpc::protos;

#[derive(Clone)]
struct InterruptedReceiveClaim {
	reached: Arc<Notify>,
	request: Arc<Mutex<Option<protos::ClaimLightningReceiveRequest>>>,
	reveal_preimage: bool,
}

#[async_trait::async_trait]
impl ArkRpcProxy for InterruptedReceiveClaim {
	async fn claim_lightning_receive(
		&self, upstream: &mut ArkClient, request: protos::ClaimLightningReceiveRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		*self.request.lock().unwrap() = Some(request.clone());
		if self.reveal_preimage {
			upstream.claim_lightning_receive(request).await
				.expect_err("the test database trigger must interrupt claim commit");
		}
		self.reached.notify_one();
		// Keep the client at its real claim checkpoint until the test drops it.
		std::future::pending().await
	}
}

/// External Lightning has paid, but the Ark claim transaction rolls back.
/// The recipient disappears with only the original HTLC-receive entitlement.
#[tokio::test]
async fn fallback_settled_lightning_receive_without_claim_commit() {
	Box::pin(interrupted_receive(true)).await;
}

#[tokio::test]
async fn fallback_unsettled_lightning_receive_is_not_paid() {
	Box::pin(interrupted_receive(false)).await;
}

async fn interrupted_receive(settled: bool) {
	let name = if settled { "fallback_settled_lightning_receive_without_claim_commit" }
		else { "fallback_unsettled_lightning_receive_is_not_paid" };
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
	let proxy = srv.start_proxy_no_mailbox(InterruptedReceiveClaim {
		reached: reached.clone(), request: claim_request.clone(), reveal_preimage: settled,
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
	tokio::time::timeout(Duration::from_secs(30), async {
		tokio::select! {
			r = wallet.try_claim_lightning_receive(payment_hash, true) =>
				panic!("claim completed before interruption: {r:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("claim must reach the interrupted commit");
	if !settled {
		// The user disappears before disclosing the preimage. Cancel the
		// real held invoice, so the external payer keeps its funds.
		lightning.internal.hold_client().await.cancel(cln_rpc::plugins::hold::CancelRequest {
			payment_hash: payment_hash.to_vec(),
		}).await.unwrap();
	}
	let paid = tokio::time::timeout(Duration::from_secs(30), paying).await
		.expect("external payment must finish").unwrap();
	if settled { paid.expect("external payment must succeed"); }
	else { paid.expect_err("external canceled payment must fail"); }
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(payment_hash).await)
		.await.unwrap().unwrap();
	if settled { assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Settled); }
	assert_eq!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(payment_hash).await)
		.await.unwrap().is_some(), settled);
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
	println!("interrupted receive: external_paid={settled}, granted_sat={principal}, unresolved_htlcs={}", coins.len());

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
	println!("interrupted receive: all {} backing anchors swept and confirmed", anchors.len());
	// Train after the large expiry advance, which ages out old fee history.
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
	if !settled {
		// A valid estimate and successful ticks must exercise selection;
		// a disabled task or missing estimate would prove nothing here.
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
		assert_eq!(paid, 0, "an unpaid receive is not a user entitlement");
		let address = Address::from_script(&record.spk, Network::Regtest).unwrap();
		let scan: serde_json::Value = core.call("scantxoutset", &[
			"start".into(), serde_json::json!([{"desc": format!("addr({address})")}]),
		]).unwrap();
		assert_eq!(scan["success"], true);
		assert!(scan["unspents"].as_array().unwrap().is_empty());
		println!("unsettled receive: external payer refunded, no recorded preimage, no expiry payout");
		return;
	}
	let rows = tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT txid,fee_sat,spk FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
			).await?)).await.unwrap();
			if rows.len() == coins.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("settled receive must pay its absent recipient within 90 seconds");
	let txid: Txid = rows[0].get::<_, String>("txid").parse().unwrap();
	let fee = rows[0].get::<_, i64>("fee_sat") as u64;
	for row in &rows {
		assert_eq!(row.get::<_, String>("txid"), txid.to_string());
		assert_eq!(row.get::<_, Vec<u8>>("spk"), record.spk.as_bytes());
	}
	ctx.await_transaction(txid).await;
	ctx.generate_blocks(1).await;
	let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
	let outputs = tx.output.iter().enumerate().filter(|(_, o)| o.script_pubkey == record.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	let (vout, output) = outputs[0];
	assert_eq!(output.value.to_sat(), principal - fee);
	let total_in = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
		.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
	assert_eq!(fee, total_in - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>());
	assert!(fee > 0);
	let fulfilled = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id
		 WHERE v.vtxo_id=ANY($1) AND h.offchain_resolution='fulfilled'", &[&ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(fulfilled as usize, coins.len());
	let request = claim_request.lock().unwrap().take().unwrap();
	srv.get_public_rpc().await.claim_lightning_receive(request).await
		.expect_err("a late cooperative claim cannot spend the paid HTLC a second time");
	srv.stop().await.unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let mut restored = bdk_wallet::Wallet::create(
		Bip84(master, KeychainKind::External), Bip84(master, KeychainKind::Internal),
	).network(Network::Regtest).lookahead(20).create_wallet_no_persist().unwrap();
	for height in 1..=core.get_block_count().unwrap() {
		let hash = core.get_block_hash(height).unwrap();
		restored.apply_block(&core.get_block(&hash).unwrap(), height as u32).unwrap();
	}
	let payout = OutPoint::new(txid, vout as u32);
	assert_eq!(restored.list_unspent().find(|u| u.outpoint == payout)
		.expect("plain mnemonic-only BIP84 restore must discover receive payout").txout.value, output.value);
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
	assert!(core.get_tx_out(&txid, vout as u32, Some(true)).unwrap().is_none());
	assert_eq!(ctx.bitcoind().get_received_by_address(&destination), spend.output[0].value);
	println!("interrupted receive recovered: payout={payout}, principal_sat={principal}, fee_sat={fee}, confirmed_seed_spend={spend_txid}");
}
