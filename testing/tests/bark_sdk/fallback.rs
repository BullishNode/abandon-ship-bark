use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use bitcoin::{Address, FeeRate, Network, OutPoint, Transaction, Txid};
use bitcoin::constants::ChainHash;
use bitcoin::bip32::Xpriv;
use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use bark::lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use bdk_wallet::{KeychainKind, SignOptions};
use bdk_wallet::template::Bip84;

use ark_testing::{TestContext, btc, sat};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::captaind::{ArkClient, proxy::ArkRpcProxy};
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use ark::ProtocolEncoding;
use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::attestations::FallbackRecordAttestation;
use server::database::Db;
use server_rpc::{protos, StatusExt};

#[tokio::test]
async fn fallback_register_board_rotate_and_offline_pool() {
	let ctx = TestContext::new("bark_sdk/fallback_register_board_rotate_and_offline_pool").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let wallet = ctx.bark_sdk("wallet", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	assert!(record.spk.is_p2wpkh());
	let mailbox = wallet.mailbox_keypair().public_key().serialize().to_vec();
	let stored = db.read(async |t| Ok(t.query_one(
		"SELECT spk, seq FROM fallback_record WHERE mailbox_pk = $1", &[&mailbox],
	).await?)).await.unwrap();
	assert_eq!(stored.get::<_, Vec<u8>>(0), record.spk.as_bytes());
	assert_eq!(stored.get::<_, i64>(1), record.seq as i64);
	for vtxo in wallet.spendable_vtxos().await.unwrap() {
		let pk = vtxo.user_pubkey().serialize().to_vec();
		let linked = db.read(async |t| Ok(t.query_one(
			"SELECT mailbox_pk FROM key_link WHERE user_pubkey = $1", &[&pk],
		).await?)).await.unwrap();
		assert_eq!(linked.get::<_, Vec<u8>>(0), mailbox);
	}

	// The rotation trigger is an actual chain receive. S5 separately proves
	// the automatic expiry transaction; this test isolates client registration.
	let address = Address::from_script(&record.spk, Network::Regtest).unwrap();
	ctx.bitcoind().fund_addr(address, sat(20_000)).await;
	ctx.generate_blocks(1).await;
	wallet.sync_onchain().await.unwrap();
	let rotated = wallet.fallback_destination().await.unwrap();
	assert_ne!(rotated.spk, record.spk);
	assert!(rotated.seq > record.seq);
	let foreign = ctx.bitcoind().get_new_address().script_pubkey();
	assert!(wallet.set_fallback_destination(foreign).await.is_err());
	assert_eq!(wallet.fallback_destination().await.unwrap(), rotated);

	wallet.sync().await;
	srv.stop().await.unwrap();
	let mut keys = Vec::<PublicKey>::new();
	for _ in 0..8 {
		let (address, _) = wallet.new_address_with_index().await.unwrap();
		let pk = address.policy().user_pubkey();
		assert!(!keys.contains(&pk));
		keys.push(pk);
	}
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.new_address()).await
		.expect("offline exhaustion returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"));
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.board_amount(sat(10_000))).await
		.expect("offline board returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"));
	assert!(wallet.pending_boards().await.unwrap().is_empty());

	// An empty pool must fail before Lightning send locks any input. No LN
	// node is needed: the valid invoice must never reach an HTLC cosign step.
	let secp = Secp256k1::new();
	let key = SecretKey::from_slice(&[42; 32]).unwrap();
	let invoice = InvoiceBuilder::new(Currency::Regtest)
		.description("offline linking failure".into())
		.payment_hash(sha256::Hash::hash(b"fallback offline send"))
		.payment_secret(PaymentSecret([43; 32]))
		.current_timestamp().min_final_cltv_expiry_delta(144)
		.amount_milli_satoshis(10_000_000)
		.build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &key)).unwrap();
	let before = wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<Vec<_>>();
	assert!(!before.is_empty());
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.pay_lightning_invoice(invoice, None, false))
		.await.expect("offline LN send returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"), "{err:#}");
	assert_eq!(wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<Vec<_>>(), before);
	assert!(wallet.pending_lightning_sends().await.unwrap().is_empty());
}

/// A wallet whose stored record predates the chain and server binding signs a
/// bound one at its next sync. The server never drops the old row by itself.
#[tokio::test]
async fn fallback_stored_legacy_record_migrates_on_sync() {
	let ctx = TestContext::new("bark_sdk/fallback_stored_legacy_record_migrates_on_sync").await;
	let srv = ctx.captaind("server").create().await;
	let wallet = ctx.bark_sdk("wallet", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let server_pubkey = srv.ark_info().await.server_pubkey;
	let mailbox = wallet.mailbox_keypair();
	let mailbox_pk = mailbox.public_key().serialize().to_vec();
	let record = wallet.fallback_destination().await.unwrap();

	// The signature a wallet made before the binding, for the same script and sequence.
	let mut engine = sha256::Hash::engine();
	engine.input(b"Ark expiry fallback record      ");
	engine.input(record.spk.as_bytes());
	engine.input(&record.seq.to_le_bytes());
	let msg = Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array());
	let legacy = Secp256k1::new().sign_schnorr_no_aux_rand(&msg, &mailbox);
	let legacy_sig = legacy.as_ref().to_vec();
	db.write(async |t| Ok(t.execute(
		"UPDATE fallback_record SET sig = $2 WHERE mailbox_pk = $1", &[&mailbox_pk, &legacy_sig],
	).await?)).await.unwrap();

	wallet.sync().await;
	let row = db.read(async |t| Ok(t.query_one(
		"SELECT spk, seq, sig FROM fallback_record WHERE mailbox_pk = $1", &[&mailbox_pk],
	).await?)).await.unwrap();
	let seq = row.get::<_, i64>("seq") as u64;
	assert!(seq > record.seq, "the wallet signed a newer record");
	assert_eq!(row.get::<_, Vec<u8>>("spk"), record.spk.as_bytes(), "the destination is unchanged");
	FallbackRecordAttestation::deserialize(&row.get::<_, Vec<u8>>("sig")).unwrap()
		.verify(ChainHash::REGTEST, server_pubkey, &record.spk, seq, mailbox.public_key())
		.expect("the stored record is bound to this chain and server");
	assert_eq!(wallet.fallback_destination().await.unwrap().seq, seq);
}

/// A send from two inputs with different expiries stalls in registration
/// while the first input's outputs settle, which refunds them to the sender.
/// The returning sender delivers the other outputs through the mailbox. The
/// post registers them, so the recipient is paid for those.
#[tokio::test]
async fn fallback_multi_input_send_one_input_settled_pays_recipient() {
	let ctx = TestContext::new("bark_sdk/fallback_multi_input_send_one_input_settled_pays_recipient").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender_mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let sender = ctx.bark_sdk("sender", &srv).mnemonic(sender_mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).funded(sat(1_000_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let first = sender.board_amount(sat(30_000)).await.unwrap();
	ctx.await_transaction(first.funding_tx.compute_txid()).await;
	ctx.generate_blocks(40).await;
	let second = sender.board_amount(sat(30_000)).await.unwrap();
	ctx.await_transaction(second.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let first_anchor = first.funding_tx.compute_txid();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			sender.sync().await;
			if sender.spendable_vtxos().await.unwrap().len() == 2 { break; }
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("both boards must become spendable");
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	let recipient_spk = recipient.fallback_destination().await.unwrap().spk;
	let sender_spk = sender.fallback_destination().await.unwrap().spk;
	drop(recipient);

	// The registration request of the send never reaches the server.
	let reached = Arc::new(Notify::new());
	let proxy = srv.start_proxy_no_mailbox(InterruptArkoorRegistration {
		reached: reached.clone(), commit: false, recipient: address.policy().user_pubkey(),
	}).await;
	let mut config = sender.config().clone();
	config.server_address = proxy.address.clone();
	drop(sender);
	let open = |config| bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &sender_mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("sender")), run_daemon: false, ..Default::default() },
	);
	let sender = open(config.clone()).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		tokio::select! {
			// The second board's change is above the payout minimum.
			r = sender.send_arkoor_payment(&address, sat(45_000)) => panic!("send completed before interruption: {r:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("registration request must be reached");
	let pending = sender.pending_arkoor_sends().await.unwrap();
	assert_eq!(pending.len(), 1);
	let bark::actions::arkoor_send::Progress::Registration {
		signed_destination_vtxos, signed_change_vtxos, ..
	} = &pending[0].progress else { panic!("the send must stall in registration"); };
	let outputs = signed_destination_vtxos.iter().map(|v| (v.clone(), true))
		.chain(signed_change_vtxos.iter().map(|v| (v.clone(), false))).collect::<Vec<_>>();
	drop(sender);
	drop(proxy);
	let (first_outputs, second_outputs): (Vec<_>, Vec<_>) = outputs.into_iter()
		.partition(|(v, _)| v.chain_anchor().txid == first_anchor);
	assert!(!first_outputs.is_empty() && !second_outputs.is_empty(), "the send spends both boards");
	assert!(second_outputs.iter().any(|(_, to_recipient)| *to_recipient));
	let ids = |outputs: &[(ark::Vtxo<ark::vtxo::Full>, bool)]| outputs.iter().map(|(v, _)| v.id().to_string()).collect::<Vec<_>>();
	let first_coins = first_outputs.iter().map(|(v, _)| v.clone()).collect::<Vec<ark::Vtxo>>();
	let second_coins = second_outputs.iter().map(|(v, _)| v.clone()).collect::<Vec<ark::Vtxo>>();

	// The first board's outputs settle while the sender is away: unregistered,
	// so they are refunded to the sender.
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &first_coins).await;
	assert!((ctx.bitcoind().get_block_count().await as u32) < second_coins[0].expiry_height().to_u32());
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	for row in wait_settled(&ctx, &db, &ids(&first_outputs)).await {
		assert_eq!(row.get::<_, Vec<u8>>("spk"), sender_spk.as_bytes(), "unregistered outputs refund the sender");
	}

	// The sender returns: registration is refused as settled, and delivery
	// posts the second board's outputs to the recipient's mailbox.
	config.server_address = srv.ark_url();
	let sender = open(config).await.unwrap();
	tokio::time::timeout(Duration::from_secs(60), async {
		while !sender.pending_arkoor_sends().await.unwrap().is_empty() {
			sender.sync_pending_arkoor_sends().await.unwrap();
		}
	}).await.expect("the returning send must finish");
	let destination_ids = second_outputs.iter().filter(|(_, r)| *r).map(|(v, _)| v.id().to_string()).collect::<Vec<_>>();
	let spendable = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND spend_state='spendable'", &[&destination_ids],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(spendable as usize, destination_ids.len(), "the post registered the delivered outputs");
	drop(sender);

	// The second board's outputs settle: the delivered ones are the recipient's.
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &second_coins).await;
	let rows = wait_settled(&ctx, &db, &ids(&second_outputs)).await;
	for (vtxo, to_recipient) in &second_outputs {
		let row = rows.iter().find(|r| r.get::<_, String>("id") == vtxo.id().to_string()).unwrap();
		let expected = if *to_recipient { &recipient_spk } else { &sender_spk };
		assert_eq!(row.get::<_, Vec<u8>>("spk"), expected.as_bytes(),
			"output {} (to recipient: {to_recipient})", vtxo.id());
	}
	let all = ids(&first_outputs).into_iter().chain(ids(&second_outputs)).collect::<Vec<_>>();
	let settlements = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&all],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(settlements as usize, all.len(), "every output is settled once");
	println!("multi-input send: first board outputs refunded to the sender, delivered second board outputs paid to the recipient");
}

/// Real funded boards, sweep, expiry task and nursery. The absent wallet's
/// individual coins are all below the configured minimum; their group is not.
#[tokio::test]
async fn fallback_grouped_expiry_without_client() {
	Box::pin(grouped_expiry_without_client("fallback_grouped_expiry_without_client", 3, 5_000, 100, ExpiryCase::Registered)).await;
}

#[tokio::test]
async fn fallback_grouped_200_small_coins() {
	Box::pin(grouped_expiry_without_client("fallback_grouped_200_small_coins", 200, 600, 200, ExpiryCase::Registered)).await;
}

/// One wallet group is one payout output, however many coins it holds. With
/// the default configuration, the 101st coin must not hold up the wallet.
#[tokio::test]
async fn fallback_grouped_101_coins_default_batch() {
	let max_batch = server::expiry_payout::Config::default().max_batch;
	Box::pin(grouped_expiry_without_client("fallback_grouped_101_coins_default_batch", 101, 600, max_batch, ExpiryCase::Registered)).await;
}

/// A funded board must survive the owner disappearing before registration.
#[tokio::test]
async fn fallback_abandoned_board_without_registration() {
	Box::pin(grouped_expiry_without_client("fallback_abandoned_board_without_registration", 1, 25_000, 100, ExpiryCase::AbandonedBoard)).await;
}

#[tokio::test]
async fn fallback_abandoned_board_return_clears_pending() {
	Box::pin(grouped_expiry_without_client("fallback_abandoned_board_return_clears_pending", 1, 25_000, 100, ExpiryCase::ReturningBoard)).await;
}

#[tokio::test]
async fn fallback_unregistered_arkoor_pays_input_owner() {
	Box::pin(grouped_expiry_without_client("fallback_unregistered_arkoor_pays_input_owner",
		2, 25_000, 100, ExpiryCase::UnregisteredArkoor)).await;
}

#[tokio::test]
async fn fallback_arkoor_registration_wins_payout_race() {
	Box::pin(grouped_expiry_without_client("fallback_arkoor_registration_wins_payout_race",
		2, 25_000, 100, ExpiryCase::RegistrationWins)).await;
}

#[tokio::test]
async fn fallback_arkoor_payout_wins_registration_race() {
	Box::pin(grouped_expiry_without_client("fallback_arkoor_payout_wins_registration_race",
		2, 25_000, 100, ExpiryCase::PayoutWins)).await;
}

/// A recipient holding the signed chain through its mailbox is paid, though
/// the sender never registered it.
#[tokio::test]
async fn fallback_arkoor_post_without_registration_pays_recipient() {
	Box::pin(grouped_expiry_without_client("fallback_arkoor_post_without_registration_pays_recipient",
		2, 25_000, 100, ExpiryCase::PostedArkoor)).await;
}

#[tokio::test]
async fn fallback_arkoor_post_after_settlement_refused() {
	Box::pin(grouped_expiry_without_client("fallback_arkoor_post_after_settlement_refused",
		2, 25_000, 100, ExpiryCase::PostAfterSettlement)).await;
}

#[tokio::test]
async fn fallback_arkoor_post_wins_payout_race() {
	Box::pin(grouped_expiry_without_client("fallback_arkoor_post_wins_payout_race",
		2, 25_000, 100, ExpiryCase::PostWins)).await;
}

#[tokio::test]
async fn fallback_arkoor_payout_wins_post_race() {
	Box::pin(grouped_expiry_without_client("fallback_arkoor_payout_wins_post_race",
		2, 25_000, 100, ExpiryCase::PayoutWinsPost)).await;
}

#[tokio::test]
async fn fallback_unregistered_arkoor_return_does_not_report_sent() {
	Box::pin(grouped_expiry_without_client("fallback_unregistered_arkoor_return_does_not_report_sent",
		2, 25_000, 100, ExpiryCase::ReturningArkoor)).await;
}

#[tokio::test]
async fn fallback_registered_arkoor_lost_reply_return_reports_sent() {
	Box::pin(grouped_expiry_without_client("fallback_registered_arkoor_lost_reply_return_reports_sent",
		2, 25_000, 100, ExpiryCase::RegisteredArkoorReturn)).await;
}

#[tokio::test]
async fn fallback_unregistered_arkoor_return_does_not_restore_paid_change() {
	Box::pin(grouped_expiry_without_client("fallback_unregistered_arkoor_return_does_not_restore_paid_change",
		2, 25_000, 100, ExpiryCase::ReturningArkoorChange)).await;
}

/// A wallet group larger than `max_batch` is paid alone in its own claim.
/// Smaller groups of other wallets are still paid in claims of their own.
#[tokio::test]
async fn fallback_oversized_wallet_group_paid_alone() {
	let ctx = TestContext::new("bark_sdk/fallback_oversized_wallet_group_paid_alone").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let large = ctx.bark_sdk("large", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(5_000)).boarded(sat(5_000)).boarded(sat(5_000)).create().await;
	let small_a = ctx.bark_sdk("small-a", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(25_000)).create().await;
	let small_b = ctx.bark_sdk("small-b", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(25_000)).create().await;
	let mut wallets = Vec::new();
	for wallet in [&large, &small_a, &small_b] {
		wallet.stop_daemon_wait().await.unwrap();
		let spk = wallet.fallback_destination().await.unwrap().spk;
		let coins = wallet.spendable_vtxos().await.unwrap().into_iter().map(|v| v.vtxo).collect::<Vec<_>>();
		wallets.push((spk, coins));
	}
	assert_eq!(wallets[0].1.len(), 3);
	assert_eq!(wallets[1].1.len(), 1);
	assert_eq!(wallets[2].1.len(), 1);
	drop((large, small_a, small_b));

	// Real confirmed sweeps of every backing anchor and a real fee estimate,
	// as in the grouped test; the task is enabled only afterwards.
	let core = ctx.bitcoind().sync_client();
	let coins = wallets.iter().flat_map(|(_, c)| c.iter().cloned()).collect::<Vec<_>>();
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(expiry.saturating_sub(tip) + 3).await;
	let anchors = coins.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT vtxo_id, onchain_spent_txid FROM vtxo WHERE vtxo_id = ANY($1)", &[&anchors],
			).await?)).await.unwrap();
			assert_eq!(rows.len(), anchors.len());
			if rows.iter().all(|row| {
				let Some(txid) = row.get::<_, Option<String>>("onchain_spent_txid") else { return false; };
				let txid: Txid = txid.parse().unwrap();
				core.get_raw_transaction_info(&txid, None).unwrap().confirmations.unwrap_or(0) > 0
			}) { break; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("all board sweeps must confirm");
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
	assert!(core.estimate_smart_fee(6, None).unwrap().fee_rate.is_some());

	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.interval = Duration::from_secs(1);
		config.expiry_payout.grace_blocks = 0;
		config.expiry_payout.sweep_min_confs = 1;
		config.expiry_payout.min_payout_sat = 10_000;
		config.expiry_payout.max_batch = 2;
		config.expiry_payout.watchman_config = Some(
			srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE),
		);
	}
	srv.start().await.unwrap();
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let rows = tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT id, txid, fee_sat, spk FROM expiry_settlement WHERE id = ANY($1)", &[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("every wallet, including the group larger than max_batch, must be paid");
	let txid_of = |id: &str| rows.iter().find(|r| r.get::<_, String>("id") == id).unwrap().get::<_, String>("txid");
	let large_txid = txid_of(&wallets[0].1[0].id().to_string());
	for (i, (spk, coins)) in wallets.iter().enumerate() {
		for coin in coins {
			let row = rows.iter().find(|r| r.get::<_, String>("id") == coin.id().to_string()).unwrap();
			assert_eq!(row.get::<_, Vec<u8>>("spk"), spk.as_bytes());
			assert_eq!(row.get::<_, String>("txid") == large_txid, i == 0,
				"the oversized group has a claim of its own");
		}
	}
	let mut payouts = rows.iter().map(|r| r.get::<_, String>("txid")).collect::<Vec<_>>();
	payouts.sort();
	payouts.dedup();
	for txid in &payouts {
		let txid: Txid = txid.parse().unwrap();
		ctx.await_transaction(txid).await;
		let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
		let fee = rows.iter().find(|r| r.get::<_, String>("txid") == txid.to_string()).unwrap()
			.get::<_, i64>("fee_sat") as u64;
		let input = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		assert_eq!(fee, input - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>());
		for (spk, coins) in &wallets {
			let paid = tx.output.iter().filter(|o| &o.script_pubkey == spk).collect::<Vec<_>>();
			let settled = rows.iter().filter(|r| r.get::<_, String>("txid") == txid.to_string()
				&& r.get::<_, Vec<u8>>("spk") == spk.as_bytes()).count();
			assert_eq!(paid.len(), usize::from(settled > 0), "one output per wallet group");
			if settled > 0 {
				assert_eq!(settled, coins.len(), "a wallet group is never split");
				let gross = coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
				assert!(paid[0].value.to_sat() < gross && paid[0].value >= sat(10_000));
			}
		}
	}
	let large_tx: Transaction = core.get_raw_transaction(&large_txid.parse().unwrap(), None).unwrap();
	println!("oversized group: coins=3, max_batch=2, claim={large_txid}, outputs={}; other claims={:?}",
		large_tx.output.len(), payouts.iter().filter(|t| **t != large_txid).collect::<Vec<_>>());
}

#[derive(Clone, Copy)]
enum ExpiryCase {
	Registered, AbandonedBoard, ReturningBoard, UnregisteredArkoor, RegistrationWins, PayoutWins,
	ReturningArkoor, RegisteredArkoorReturn, ReturningArkoorChange,
	PostedArkoor, PostAfterSettlement, PostWins, PayoutWinsPost,
}

async fn grouped_expiry_without_client(
	name: &str, count: usize, amount: u64, max_batch: usize, case: ExpiryCase,
) {
	let abandoned = matches!(case, ExpiryCase::AbandonedBoard | ExpiryCase::ReturningBoard);
	let returning = matches!(case, ExpiryCase::ReturningBoard);
	let registered_return = matches!(case, ExpiryCase::RegisteredArkoorReturn);
	let with_change = matches!(case, ExpiryCase::ReturningArkoorChange);
	let returning_arkoor = matches!(case, ExpiryCase::ReturningArkoor) || registered_return || with_change;
	let registration_wins = matches!(case, ExpiryCase::RegistrationWins | ExpiryCase::PostWins);
	let payout_wins = matches!(case, ExpiryCase::PayoutWins | ExpiryCase::PayoutWinsPost);
	// The racing activation is a mailbox post instead of a registration.
	let race_by_post = matches!(case, ExpiryCase::PostWins | ExpiryCase::PayoutWinsPost);
	let posted = matches!(case, ExpiryCase::PostedArkoor);
	let post_after_settlement = matches!(case, ExpiryCase::PostAfterSettlement);
	let unregistered_case = matches!(case, ExpiryCase::UnregisteredArkoor) || post_after_settlement;
	let arkoor = unregistered_case || posted || registration_wins || payout_wins || returning_arkoor;
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let mut mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let sender_mnemonic = mnemonic.clone();
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().create().await;
	let mut wallet = ctx.bark_sdk("wallet", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(count as u64 * amount + 1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let never_broadcast = if abandoned && !returning {
		let (key, _) = wallet.derive_store_next_keypair().await.unwrap();
		let (address, expiry) = wallet.board_funding_address(&key).await.unwrap();
		let psbt = wallet.onchain().unwrap().write().await.prepare_tx(
			&[(address, sat(40_000))], FeeRate::from_sat_per_vb(3).unwrap(),
		).await.unwrap();
		let funding = &psbt.unsigned_tx;
		let info = srv.ark_info().await;
		let builder = ark::board::BoardBuilder::new(key.public_key(), expiry,
			info.server_pubkey, info.vtxo_exit_delta);
		let vout = funding.output.iter().position(|o| o.script_pubkey == builder.funding_script_pubkey()).unwrap();
		let (_, nonce) = ark::musig::nonce_pair(&key);
		let request = protos::BoardCosignRequest {
			amount: 40_000, utxo: OutPoint::new(funding.compute_txid(), vout as u32).serialize(),
			expiry_height: expiry.into(), user_pubkey: key.public_key().serialize().to_vec(),
			pub_nonce: nonce.serialize().to_vec(), funding_tx: bitcoin::consensus::serialize(funding),
		};
		let mut rpc = srv.get_public_rpc().await;
		let mut wrong_amount = request.clone();
		wrong_amount.amount += 1;
		let err = rpc.request_board_cosign(wrong_amount).await.unwrap_err();
		assert!(err.message().contains("amount or script"), "{err}");
		let mut wrong_script = funding.clone();
		wrong_script.output[vout].script_pubkey = ctx.bitcoind().get_new_address().script_pubkey();
		let mut wrong_request = request.clone();
		wrong_request.utxo = OutPoint::new(wrong_script.compute_txid(), vout as u32).serialize();
		wrong_request.funding_tx = bitcoin::consensus::serialize(&wrong_script);
		let err = rpc.request_board_cosign(wrong_request).await.unwrap_err();
		assert!(err.message().contains("amount or script"), "{err}");
		let pending = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM pending_board", &[])
			.await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(pending, 0, "invalid cosigns cannot install pending candidates");
		rpc.request_board_cosign(request.clone()).await.unwrap();
		rpc.request_board_cosign(request).await.unwrap();
		// A third cosign through the real client uses a new nonce but must
		// identify the same entitlement; the unfinished PSBT is never broadcast.
		Some(wallet.board_psbt(psbt, key, expiry).await.unwrap())
	} else { None };
	let mut board_coins = Vec::new();
	for i in 0..count {
		let board = wallet.board_amount(sat(amount)).await.unwrap();
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		board_coins.push(wallet.get_vtxo_by_id(board.vtxos[0]).await.unwrap());
		// Confirm short chains instead of changing Core's mempool limits.
		if i % 8 == 7 { ctx.generate_blocks(1).await; }
	}
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	if !abandoned {
		// Force watchman to observe funding before the registration RPC. The
		// anchor's reserved exit must still be an idempotent spend in this order.
		let anchors = board_coins.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let confirmed = db.read(async |t| Ok(t.query_one(
					"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND confirmed_height IS NOT NULL",
					&[&anchors],
				).await?.get::<_, i64>(0))).await.unwrap();
				if confirmed as usize == count { break; }
				tokio::time::sleep(Duration::from_millis(100)).await;
			}
		}).await.expect("watchman must confirm funding before client registration");
		wallet.sync().await;
	}
	let mut record = wallet.fallback_destination().await.unwrap();
	let mut mailbox_key = wallet.mailbox_keypair();
	let wallet_coins = if abandoned { board_coins } else { wallet.spendable_vtxos().await.unwrap() };
	let mut coins = Vec::new();
	for coin in wallet_coins { coins.push(wallet.get_full_vtxo(coin.id()).await.unwrap()); }
	let original_inputs = coins.iter().map(|v| v.id()).collect::<Vec<_>>();
	let mut recipient_mailbox = None;
	let other_owner_spk = if arkoor {
		let recipient_mnemonic = bip39::Mnemonic::generate(12).unwrap();
		let recipient = ctx.bark_sdk("recipient", &srv).mnemonic(recipient_mnemonic.clone())
			.cfg(|c| c.daemon_manual_sync = true).create().await;
		recipient.stop_daemon_wait().await.unwrap();
		let address = recipient.new_address().await.unwrap();
		recipient_mailbox = address.delivery().iter().find_map(|d| match d {
			ark::address::VtxoDelivery::ServerMailbox { blinded_id } => Some(blinded_id.as_ref().to_vec()),
			_ => None,
		});
		let spk = recipient.fallback_destination().await.unwrap().spk;
		assert_ne!(spk, record.spk);
		if returning_arkoor {
			let reached = Arc::new(Notify::new());
			let proxy = srv.start_proxy_no_mailbox(InterruptArkoorRegistration {
				reached: reached.clone(), commit: registered_return, recipient: address.policy().user_pubkey(),
			}).await;
			let mut config = wallet.config().clone();
			config.server_address = proxy.address.clone();
			drop(wallet);
			wallet = bark::Wallet::open(Network::Regtest,
				bark::WalletSeed::new_from_mnemonic(Network::Regtest, &sender_mnemonic), config,
				bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("wallet")), run_daemon: false, ..Default::default() },
			).await.unwrap();
			let payment = sat(count as u64 * amount - if with_change { 10_000 } else { 0 });
			tokio::time::timeout(Duration::from_secs(30), async {
				tokio::select! {
					r = wallet.send_arkoor_payment(&address, payment) => panic!("send completed before interruption: {r:?}"),
					_ = reached.notified() => {},
				}
			}).await.expect("registration request must be reached");
			let pending = wallet.pending_arkoor_sends().await.unwrap();
			assert_eq!(pending.len(), 1);
			let bark::actions::arkoor_send::Progress::Registration {
				signed_destination_vtxos, signed_change_vtxos, ..
			} = &pending[0].progress else { panic!("registration checkpoint must survive lost request/reply"); };
			assert_eq!(signed_change_vtxos.iter().map(|v| v.amount()).sum::<bitcoin::Amount>(),
				if with_change { sat(10_000) } else { sat(0) });
			coins = signed_destination_vtxos.clone();
			coins.extend(signed_change_vtxos.clone());
		} else {
			let mut keys = Vec::new();
			for coin in &coins { keys.push(wallet.pubkey_keypair(&coin.user_pubkey()).await.unwrap().unwrap().1); }
			let builder = ArkoorPackageBuilder::new_with_checkpoints(coins, vec![ArkoorDestination {
				total_amount: sat(count as u64 * amount), policy: address.policy().clone(),
			}]).unwrap().generate_user_nonces(&keys).unwrap();
			let response = srv.get_public_rpc().await.request_arkoor_cosign(
				protos::ArkoorPackageCosignRequest::from(builder.cosign_request()),
			).await.unwrap().into_inner();
			coins = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(response).unwrap())
				.unwrap().build_signed_vtxos();
		}
		let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
		let unregistered = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND spend_state='unregistered'", &[&ids],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(unregistered as usize, if registered_return { 0 } else { coins.len() });
		if posted {
			// The sender skips registration and only posts the signed chain.
			activate(&srv, &coins, recipient_mailbox.clone()).await.await.unwrap()
				.expect("a valid signed chain is accepted by the mailbox");
			let spendable = db.read(async |t| Ok(t.query_one(
				"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND spend_state='spendable'", &[&ids],
			).await?.get::<_, i64>(0))).await.unwrap();
			assert_eq!(spendable as usize, coins.len(), "the post registered the signed chain");
		}
		let other_spk = if registration_wins || registered_return || posted {
			let sender_spk = record.spk.clone();
			record = recipient.fallback_destination().await.unwrap();
			mailbox_key = recipient.mailbox_keypair();
			mnemonic = recipient_mnemonic;
			sender_spk
		} else { spk };
		drop(recipient);
		Some(other_spk)
	} else { None };
	let signed_board = if abandoned { Some(coins[0].clone()) } else { None };
	if abandoned {
		let id = coins[0].id().to_string();
		let count = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM vtxo WHERE vtxo_id=$1", &[&id])
			.await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(count, 0, "cosign must not install a registerable user coin");
		let mut rpc = srv.get_public_rpc().await;
		let err = rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
			vtxos: vec![signed_board.as_ref().unwrap().serialize()],
		}).await.unwrap_err();
		assert!(err.message().contains("vtxo not found"), "{err}");
	}
	assert_eq!(coins.len(), count + usize::from(with_change));
	let principal = coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert_eq!(principal, count as u64 * amount);
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	let chain_balance_before = wallet.onchain().unwrap().read().await.balance().await;
	let return_config = if returning || returning_arkoor { Some(wallet.config().clone()) } else { None };
	drop(wallet);

	// Feed Core's real estimator with confirmed transactions. No fee estimate
	// is injected into the server or substituted by a test fallback.
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
	let estimate = core.estimate_smart_fee(6, None).unwrap();
	assert!(estimate.fee_rate.is_some(), "real fee estimate required: {estimate:?}");
	if count > 3 || unregistered_case || posted || returning_arkoor {
		// Test a group whose complete backing paths have already been swept.
		// Sweeps can confirm in several blocks. Keep payouts disabled until
		// every backing path has a confirmed real sweep, without changing
		// any coin state or substituting synthetic sweep history.
		let tip = ctx.bitcoind().get_block_count().await as u32;
		ctx.generate_blocks(expiry.saturating_sub(tip) + 3).await;
		let anchors = coins.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
		tokio::time::timeout(Duration::from_secs(90), async {
			loop {
				let rows = db.read(async |t| Ok(t.query(
					"SELECT vtxo_id, onchain_spent_txid FROM vtxo WHERE vtxo_id = ANY($1)",
					&[&anchors],
				).await?)).await.unwrap();
				assert_eq!(rows.len(), count);
				let all_swept = rows.iter().all(|row| {
					let Some(txid) = row.get::<_, Option<String>>("onchain_spent_txid") else { return false; };
					let txid: Txid = txid.parse().unwrap();
					let info = core.get_raw_transaction_info(&txid, None).unwrap();
					if info.confirmations.unwrap_or(0) == 0 { return false; }
					let tx = core.get_raw_transaction(&txid, None).unwrap();
					let anchor: OutPoint = row.get::<_, String>("vtxo_id").parse().unwrap();
					assert!(tx.input.iter().any(|i| i.previous_output == anchor));
					true
				});
				if all_swept { break; }
				tokio::time::sleep(Duration::from_secs(1)).await;
				ctx.generate_blocks(1).await;
			}
		}).await.expect("all board sweeps must confirm before the grouped test starts");
	}

	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.interval = Duration::from_secs(1);
		config.expiry_payout.grace_blocks = 0;
		config.expiry_payout.sweep_min_confs = 1;
		config.expiry_payout.min_payout_sat = 10_000;
		config.expiry_payout.max_batch = max_batch;
		config.expiry_payout.receipt_dir = ctx.datadir.join("receipts");
		config.expiry_payout.watchman_config = Some(
			srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE),
		);
	}
	let race_gate = if registration_wins || payout_wins {
		Some(hold_expiry_race(&db, registration_wins).await)
	} else { None };
	srv.start().await.unwrap();
	let race_mailbox = if race_by_post { recipient_mailbox.clone() } else { None };
	let mut registration = if registration_wins {
		let task = activate(&srv, &coins, race_mailbox.clone()).await;
		wait_expiry_race_lock(&db, true).await;
		Some(task)
	} else { None };
	let tip = ctx.bitcoind().get_block_count().await as u32;
	generate_blocks_without_payouts(&ctx, &db, expiry.saturating_sub(tip) + 3).await;
	if payout_wins {
		// Advance the real sweeps until payout reaches its coin-state update.
		let waiting = wait_expiry_race_lock(&db, true);
		tokio::pin!(waiting);
		loop {
			tokio::select! {
				_ = &mut waiting => break,
				_ = tokio::time::sleep(Duration::from_secs(1)) => { generate_blocks_without_payouts(&ctx, &db, 1).await; },
			}
		}
		registration = Some(activate(&srv, &coins, race_mailbox.clone()).await);
	}
	if let Some((release, gate)) = race_gate {
		// Observe the loser waiting for the winner's real PostgreSQL row lock.
		// For registration-first, keep confirming sweeps while payout catches up.
		let waiting = wait_expiry_race_lock(&db, false);
		tokio::pin!(waiting);
		loop {
			tokio::select! {
				_ = &mut waiting => break,
				_ = tokio::time::sleep(Duration::from_secs(1)) => { generate_blocks_without_payouts(&ctx, &db, 1).await; },
			}
		}
		release.notify_one();
		gate.await.unwrap();
		let result = tokio::time::timeout(Duration::from_secs(30), registration.unwrap())
			.await.expect("registration race must finish").unwrap();
		if registration_wins { result.expect("registration holding the lock must win"); }
		else { assert!(result.unwrap_err().message().contains("expiry settlement")); }
		db.write(async |t| {
			t.batch_execute("DROP TRIGGER expiry_race_pause ON vtxo; DROP FUNCTION expiry_race_pause();").await?;
			Ok(())
		}).await.unwrap();
		println!("expiry registration race: registration_wins={registration_wins}, losing operation observed blocked on coin row");
	}
	let rows = tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT id, txid, fee_sat, spk FROM expiry_settlement WHERE id = ANY($1) ORDER BY id",
				&[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			generate_blocks_without_payouts(&ctx, &db, 1).await;
		}
	}).await.expect("swept grouped entitlements must be paid within 90 seconds");
	if let Some(never) = never_broadcast {
		let missing_txid = never.funding_tx.compute_txid();
		assert!(core.get_raw_transaction(&missing_txid, None).is_err());
		let never_id = never.vtxos[0].to_string();
		let settled = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM expiry_settlement WHERE id=$1", &[&never_id],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(settled, 0, "a never-broadcast proposal has no payout entitlement");
		let mut rpc = srv.get_public_rpc().await;
		assert!(rpc.register_board_vtxo(protos::BoardVtxoRequest {
			board_vtxo: signed_board.as_ref().unwrap().serialize(),
		}).await.is_err(), "a paid board cannot be registered again");
	}
	let txid: Txid = rows[0].get::<_, String>("txid").parse().unwrap();
	let fee = rows[0].get::<_, i64>("fee_sat") as u64;
	for row in &rows {
		assert_eq!(row.get::<_, String>("txid"), txid.to_string());
		assert_eq!(row.get::<_, Vec<u8>>("spk"), record.spk.as_bytes());
		assert_eq!(row.get::<_, i64>("fee_sat") as u64, fee);
	}
	ctx.await_transaction(txid).await;
	// The coins are settled to the payout, so the operator cannot drop it.
	let err = srv.abandon(txid).await.unwrap_err();
	assert_eq!(err.code(), tonic::Code::NotFound, "{err}");
	let nursery = srv.list_nursery_txs(false, true).await;
	let entry = nursery.iter().find(|t| t.txid == txid.to_string())
		.expect("the unconfirmed payout stays in the nursery");
	assert!(entry.abandoned_at.is_none(), "an expiry payout cannot be abandoned");
	ctx.generate_blocks(1).await;
	let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
	if let Some(other_owner_spk) = other_owner_spk {
		assert!(!tx.output.iter().any(|o| o.script_pubkey == other_owner_spk),
			"payout must go only to the winning entitlement owner");
		let other_address = Address::from_script(&other_owner_spk, Network::Regtest).unwrap();
		let scan: serde_json::Value = core.call("scantxoutset", &[
			"start".into(), serde_json::json!([{"desc": format!("addr({other_address})")}]),
		]).unwrap();
		assert_eq!(scan["success"], true);
		assert!(scan["unspents"].as_array().unwrap().is_empty(),
			"absent losing owner must not have a second payout anywhere in the UTXO set");
		let err = srv.get_public_rpc().await.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
			vtxos: coins.iter().map(|v| v.serialize()).collect(),
		}).await.unwrap_err();
		assert!(err.message().contains("expiry settlement"), "late registration must lose: {err}");
	}
	let posts = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM mailbox WHERE mailbox_type='arkoor-receive'", &[],
	).await?.get::<_, i64>(0))).await.unwrap();
	if post_after_settlement {
		// A replay of the signed chain after the payout cannot claim it.
		let err = activate(&srv, &coins, recipient_mailbox.clone()).await.await.unwrap()
			.expect_err("a post after settlement must be refused");
		assert!(err.is_expiry_settled(), "{err}");
		let after = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM mailbox WHERE mailbox_type='arkoor-receive'", &[],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(after, posts, "a refused post leaves no mailbox entry");
		println!("post after settlement refused: {}", err.message());
	}
	if posted || race_by_post || post_after_settlement {
		let expected = if posted || registration_wins { coins.len() } else { 0 };
		assert_eq!(posts as usize, expected, "only an accepted post is in the mailbox");
	}
	let outputs = tx.output.iter().filter(|o| o.script_pubkey == record.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert!(fee > 0);
	assert_eq!(outputs[0].value.to_sat(), principal - fee);
	assert!(outputs[0].value >= sat(10_000));
	let input = tx.input.iter().map(|i| {
		core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()
	}).sum::<u64>();
	assert_eq!(fee, input - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>());
	let vout = tx.output.iter().position(|o| o.script_pubkey == record.spk).unwrap() as u32;
	let unspent = core.get_tx_out(&txid, vout, Some(true)).unwrap().unwrap();
	assert!(unspent.confirmations >= 1);
	assert_eq!(unspent.value, outputs[0].value);
	let receipt_path = ctx.datadir.join("receipts").join(format!("{txid}.json"));
	tokio::time::timeout(Duration::from_secs(10), async {
		while !receipt_path.exists() { tokio::time::sleep(Duration::from_millis(100)).await; }
	}).await.expect("receipt must be exported");
	let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
	assert_eq!(receipt["outputs"].as_array().unwrap().len(), 1);
	assert_eq!(receipt["outputs"][0]["amount_sat"], principal - fee);
	assert_eq!(receipt["outputs"][0]["fee_sat"], fee);
	println!("grouped expiry payout: txid={txid}, coins={}, principal_sat={principal}, net_sat={}, fee_sat={fee}",
		coins.len(), outputs[0].value.to_sat());

	let mut latest_record_seq = record.seq;
	if returning_arkoor {
		let mut config = return_config.clone().unwrap();
		let unavailable = Arc::new(AtomicBool::new(true));
		let proxy = srv.start_proxy_no_mailbox(InterruptSettlementLookup { unavailable: unavailable.clone() }).await;
		config.server_address = proxy.address.clone();
		let wallet = bark::Wallet::open(Network::Regtest,
			bark::WalletSeed::new_from_mnemonic(Network::Regtest, &sender_mnemonic), config,
			bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("wallet")), run_daemon: false, ..Default::default() },
		).await.unwrap();
		wallet.sync_pending_arkoor_sends().await.unwrap();
		assert_eq!(wallet.pending_arkoor_sends().await.unwrap().len(), 1,
			"uncertain per-output settlement outcome must preserve the action");
		for id in &original_inputs {
			assert_eq!(wallet.get_vtxo_by_id(*id).await.unwrap().state.kind(), bark::vtxo::VtxoStateKind::Locked);
		}
		assert!(wallet.history().await.unwrap().iter().filter(|m| m.subsystem.name == "bark.arkoor")
			.all(|m| m.status == bark::movement::MovementStatus::Pending));
		unavailable.store(false, Ordering::SeqCst);
		for _ in 0..2 {
			wallet.sync_pending_arkoor_sends().await.unwrap();
			wallet.sync_onchain().await.unwrap();
		}
		assert!(wallet.pending_arkoor_sends().await.unwrap().is_empty());
		assert_eq!(wallet.balance().await.unwrap().total(), sat(0));
		for id in &original_inputs {
			assert_eq!(wallet.get_vtxo_by_id(*id).await.unwrap().state.kind(), bark::vtxo::VtxoStateKind::Spent);
		}
		let history = wallet.history().await.unwrap();
		let sends = history.iter().filter(|m| m.subsystem.name == "bark.arkoor").collect::<Vec<_>>();
		assert_eq!(sends.len(), 1);
		assert!(sends[0].output_vtxos.is_empty(), "paid change must not be restored as an Ark output");
		let expected = if registered_return { bark::movement::MovementStatus::Successful }
			else { bark::movement::MovementStatus::Failed };
		assert_eq!(sends[0].status, expected,
			"registration outcome must distinguish a recipient payout from an input-owner refund");
		if !registered_return { assert!(sends[0].sent_to.is_empty(), "refunded payment must not claim a recipient was paid"); }
		if !registered_return {
			assert_eq!(sends[0].metadata["expiry_refunded_principal_sat"], principal - if with_change { 10_000 } else { 0 });
			assert_eq!(sends[0].metadata["expiry_settled_change_sat"], if with_change { 10_000 } else { 0 });
			assert!(sends[0].metadata["attempted_destination"].as_str().unwrap().starts_with("tark"));
		}
		let posts = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM mailbox WHERE mailbox_type='arkoor-receive'", &[],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(posts, 0, "already-settled outputs must not be delivered as Ark coins");
		assert_eq!(history.iter().map(|m| m.effective_balance).sum::<bitcoin::SignedAmount>(), bitcoin::SignedAmount::ZERO);
		assert_eq!(wallet.onchain().unwrap().read().await.balance().await,
			chain_balance_before + if registered_return { sat(0) } else { outputs[0].value });
		if !registered_return { latest_record_seq = wallet.fallback_destination().await.unwrap().seq; }
		println!("returning arkoor: registered_before_disappearance={registered_return}, status={}, pending=0, Ark=0", sends[0].status);
	}
	if let Some(mut config) = return_config.filter(|_| returning) {
		// The test daemon reserves new ports on restart. Reopen the original
		// wallet data using the same server identity at its current test URL.
		config.server_address = srv.ark_url();
		let wallet = bark::Wallet::open(Network::Regtest,
			bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config.clone(),
			bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("wallet")), run_daemon: false, ..Default::default() },
		).await.unwrap();
		wallet.require_ark_info().await.unwrap();
		srv.stop().await.unwrap();
		wallet.sync_pending_boards().await.unwrap();
		assert_eq!(wallet.pending_boards().await.unwrap().len(), 1, "unknown status keeps the entitlement");
		assert_eq!(wallet.get_vtxo_by_id(coins[0].id()).await.unwrap().state.kind(),
			bark::vtxo::VtxoStateKind::Locked);
		assert!(!wallet.exit_mgr().is_exiting(coins[0].id()).await, "offline return must not start an impossible exit");
		drop(wallet);
		srv.start().await.unwrap();
		config.server_address = srv.ark_url();
		let wallet = bark::Wallet::open(Network::Regtest,
			bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
			bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("wallet")), run_daemon: false, ..Default::default() },
		).await.unwrap();
		for _ in 0..2 {
			wallet.sync().await;
			wallet.sync_onchain().await.unwrap();
		}
		assert!(wallet.pending_boards().await.unwrap().is_empty(), "paid board must leave pending state");
		assert_eq!(wallet.balance().await.unwrap().total(), sat(0));
		assert_eq!(wallet.get_vtxo_by_id(coins[0].id()).await.unwrap().state.kind(),
			bark::vtxo::VtxoStateKind::Spent);
		assert!(!wallet.exit_mgr().is_exiting(coins[0].id()).await, "paid board must not start an exit");
		assert_eq!(wallet.onchain().unwrap().read().await.balance().await,
			chain_balance_before + outputs[0].value);
		latest_record_seq = wallet.fallback_destination().await.unwrap().seq;
		let history = wallet.history().await.unwrap();
		let debits = history.iter().filter(|m| m.subsystem.name == "bark.server_spend").collect::<Vec<_>>();
		assert_eq!(debits.len(), 1, "repeated sync must not duplicate the Ark debit");
		assert_eq!(debits[0].effective_balance, -sat(principal).to_signed().unwrap());
		assert!(history.iter().all(|m| m.status == bark::movement::MovementStatus::Successful));
		assert_eq!(history.iter().map(|m| m.effective_balance).sum::<bitcoin::SignedAmount>(),
			bitcoin::SignedAmount::ZERO);
	}

	// Restore from only the mnemonic and Bitcoin blocks. A new standard BDK
	// wallet scans both BIP84 chains with lookahead 20, without Ark databases,
	// coin keys, record scripts, address indexes or a connection to captaind.
	srv.stop().await.unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let mut restored = bdk_wallet::Wallet::create(
		Bip84(master, KeychainKind::External), Bip84(master, KeychainKind::Internal),
	).network(Network::Regtest).lookahead(20).create_wallet_no_persist().unwrap();
	for height in 1..=core.get_block_count().unwrap() {
		let hash = core.get_block_hash(height).unwrap();
		restored.apply_block(&core.get_block(&hash).unwrap(), height as u32).unwrap();
	}
	let payout = OutPoint::new(txid, vout);
	let discovered = restored.list_unspent().find(|u| u.outpoint == payout)
		.expect("mnemonic-only BIP84 restore must discover the payout");
	assert_eq!(discovered.txout.value, outputs[0].value);
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
	assert!(core.get_tx_out(&txid, vout, Some(true)).unwrap().is_none());
	assert_eq!(ctx.bitcoind().get_received_by_address(&destination), spend.output[0].value);
	println!("mnemonic-only BIP84 recovery: lookahead=20, payout={payout}, confirmed_spend={spend_txid}");

	// A later valid record update cannot rewrite the historical receipt.
	srv.start().await.unwrap();
	let next_spk = restored.next_unused_address(KeychainKind::External).script_pubkey();
	assert_ne!(next_spk, record.spk);
	let seq = latest_record_seq + 1;
	let mut signed_record = next_spk.as_bytes().to_vec();
	signed_record.extend_from_slice(&seq.to_le_bytes());
	signed_record.extend_from_slice(&FallbackRecordAttestation::new(
		ChainHash::REGTEST, srv.ark_info().await.server_pubkey, &next_spk, seq, &mailbox_key,
	).serialize());
	assert_eq!(srv.get_public_rpc().await.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox_key.public_key().serialize().to_vec(),
		record: Some(signed_record.clone()), key_links: vec![],
	}).await.unwrap().into_inner().record, signed_record);
	std::fs::remove_file(&receipt_path).unwrap();
	tokio::time::timeout(Duration::from_secs(10), async {
		while !receipt_path.exists() { tokio::time::sleep(Duration::from_millis(100)).await; }
	}).await.expect("receipt must regenerate after record rotation");
	let rebuilt: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
	assert_eq!(rebuilt, receipt);
}

/// Wait until every coin of `ids` is settled to an expiry payout.
async fn wait_settled(ctx: &TestContext, db: &Db, ids: &[String]) -> Vec<tokio_postgres::Row> {
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT id, txid, spk FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("the expired coins must be paid")
}

/// Make unregistered outputs spendable: register their signed chain, or, with
/// a blinded mailbox id, post it to the recipient's mailbox.
async fn activate(
	srv: &ark_testing::Captaind, coins: &[ark::Vtxo<ark::vtxo::Full>], mailbox: Option<Vec<u8>>,
) -> JoinHandle<Result<(), tonic::Status>> {
	let vtxos = coins.iter().map(|v| v.serialize()).collect::<Vec<_>>();
	match mailbox {
		Some(blinded_id) => {
			let mut rpc = srv.get_mailbox_public_rpc().await;
			tokio::spawn(async move {
				rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest { blinded_id, vtxos })
					.await.map(|_| ())
			})
		},
		None => {
			let mut rpc = srv.get_public_rpc().await;
			tokio::spawn(async move {
				rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest { vtxos }).await.map(|_| ())
			})
		},
	}
}

/// Mine blocks with the mempool except expiry payouts and their descendants,
/// so the test decides when a payout confirms. Each template is read before
/// the nursery: a payout is committed there before it is broadcast.
async fn generate_blocks_without_payouts(ctx: &TestContext, db: &Db, count: u32) {
	tokio::time::sleep(Duration::from_secs(1)).await;
	let core = ctx.bitcoind().sync_client();
	let address = ctx.bitcoind().get_new_address();
	for _ in 0..count {
		let template: serde_json::Value = core.call("getblocktemplate", &[
			serde_json::json!({"rules": ["segwit"]}),
		]).unwrap();
		let payouts = db.read(async |t| Ok(t.query(
			"SELECT txid FROM nursery_tx WHERE kind::TEXT='expiry-payout'", &[],
		).await?)).await.unwrap().iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>();
		let mut excluded = Vec::<bool>::new();
		let mut block = Vec::new();
		for tx in template["transactions"].as_array().unwrap() {
			let skip = payouts.iter().any(|t| t == tx["txid"].as_str().unwrap())
				|| tx["depends"].as_array().unwrap().iter().any(|d| excluded[d.as_u64().unwrap() as usize - 1]);
			excluded.push(skip);
			if !skip { block.push(tx["data"].clone()); }
		}
		let _: serde_json::Value = core.call("generateblock", &[
			address.to_string().into(), serde_json::Value::Array(block),
		]).unwrap();
	}
	ctx.await_block_count_sync().await;
}

#[derive(Clone)]
struct InterruptArkoorRegistration {
	reached: Arc<Notify>,
	commit: bool,
	recipient: PublicKey,
}

#[derive(Clone)]
struct InterruptSettlementLookup {
	unavailable: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl ArkRpcProxy for InterruptSettlementLookup {
	async fn get_vtxo(
		&self, upstream: &mut ArkClient, request: protos::GetVtxoRequest,
	) -> Result<protos::GetVtxoResponse, tonic::Status> {
		if self.unavailable.load(Ordering::SeqCst) {
			return Err(tonic::Status::unavailable("test settlement lookup outage"));
		}
		Ok(upstream.get_vtxo(request).await?.into_inner())
	}
}

#[async_trait::async_trait]
impl ArkRpcProxy for InterruptArkoorRegistration {
	async fn register_vtxo_transactions(
		&self, upstream: &mut ArkClient, request: protos::RegisterVtxoTransactionsRequest,
	) -> Result<protos::Empty, tonic::Status> {
		let destination = request.vtxos.iter().any(|v| ark::Vtxo::deserialize(v)
			.map(|v: ark::Vtxo| v.user_pubkey() == self.recipient).unwrap_or(false));
		if !destination { return Ok(upstream.register_vtxo_transactions(request).await?.into_inner()); }
		if self.commit { upstream.register_vtxo_transactions(request).await?; }
		self.reached.notify_one();
		Err(tonic::Status::unavailable("test interrupts the registration request or reply"))
	}
}

/// Hold a fixture-only trigger after its operation has acquired the coin locks.
/// The service still uses its ordinary RPC and payout code; no production hook.
async fn hold_expiry_race(db: &Db, registration_wins: bool) -> (Arc<Notify>, JoinHandle<()>) {
	let state = if registration_wins { "spendable" } else { "spent" };
	db.write(async |t| {
		t.batch_execute(&format!("
			CREATE FUNCTION expiry_race_pause() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN PERFORM pg_advisory_xact_lock(727064210); RETURN NEW; END $$;
			CREATE TRIGGER expiry_race_pause BEFORE UPDATE OF spend_state ON vtxo
			FOR EACH ROW WHEN (OLD.spend_state='unregistered' AND NEW.spend_state='{state}')
			EXECUTE FUNCTION expiry_race_pause();
		")).await?;
		Ok(())
	}).await.unwrap();
	let ready = Arc::new(Notify::new());
	let ready_task = ready.clone();
	let release = Arc::new(Notify::new());
	let release_task = release.clone();
	let db = db.clone();
	let gate = tokio::spawn(async move {
		db.write(async |t| {
			t.query_one("SELECT pg_advisory_xact_lock(727064210)", &[]).await?;
			ready_task.notify_one();
			release_task.notified().await;
			Ok(())
		}).await.unwrap();
	});
	ready.notified().await;
	(release, gate)
}

async fn wait_expiry_race_lock(db: &Db, advisory: bool) {
	let query = if advisory {
		"SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype='advisory'
		 AND objid=727064210::oid AND NOT granted
		 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))"
	} else {
		"SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database()
		 AND wait_event_type='Lock'
		 AND query LIKE 'SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY%FOR UPDATE%')"
	};
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let blocked = db.read(async |t| Ok(t.query_one(query, &[]).await?.get::<_, bool>(0)))
				.await.unwrap();
			if blocked { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("operation must block on the declared database lock");
}
