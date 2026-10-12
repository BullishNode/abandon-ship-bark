//! Required absent delegated/scheduled refresh and measured replacement delay.
use std::time::{Duration, Instant};
use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoincore_rpc::RpcApi;
use server::database::Db;
use server_log::RoundFinished;
use ark_testing::{btc, sat, TestContext};
use ark_testing::util::FutureExt;

#[tokio::test]
async fn fallback_delegated_never_returns_delay_and_recovery() {
	Box::pin(absence("fallback_delegated_never_returns_delay_and_recovery", false, false, 0)).await;
}
#[tokio::test]
async fn fallback_scheduled_never_returns_delay_and_recovery() {
	Box::pin(absence("fallback_scheduled_never_returns_delay_and_recovery", true, false, 0)).await;
}
#[tokio::test]
async fn fallback_repeated_scheduled_requests_pay_once() {
	Box::pin(absence("fallback_repeated_scheduled_requests_pay_once", true, true, 0)).await;
}
#[tokio::test]
async fn fallback_delegated_signed_refresh_fee_is_reconciled() {
	Box::pin(absence("fallback_delegated_signed_refresh_fee_is_reconciled", false, false, 1000)).await;
}

async fn absence(name: &str, scheduled: bool, repeated: bool, refresh_fee: u64) {
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).watchmand().cfg(move |c| {
		c.vtxo_lifetime = BlockDelta::new(300);
		c.round_interval = Duration::from_secs(3600);
		c.fees.refresh.base_fee = sat(refresh_fee);
	}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coin = wallet.spendable_vtxos().await.unwrap()[0].id();
	let original = wallet.get_full_vtxo(coin).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	let control_seed = bip39::Mnemonic::generate(12).unwrap();
	let control = ctx.bark_sdk("ordinary-control", &srv).mnemonic(control_seed.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	control.stop_daemon_wait().await.unwrap();
	let control_coin = control.get_full_vtxo(control.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let control_record = control.fallback_destination().await.unwrap();
	drop(control);
	let core = ctx.bitcoind().sync_client();
	let booked_height = core.get_block_count().unwrap() as u32 + 140;
	let mut hashes = Vec::new();
	if scheduled {
		for i in 0..if repeated { 3 } else { 1 } {
			let request = wallet.refresh_vtxos_scheduled(vec![coin], BlockHeight::new(booked_height + i)).await.unwrap().unwrap();
			hashes.push(request.state().unlock_hash().unwrap().to_string());
			drop(request);
			// A real intervening round must not issue the future request yet.
			if repeated {
				srv.trigger_round().await;
				tokio::time::sleep(Duration::from_secs(1)).await;
			}
		}
		let count: i64 = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id WHERE i.vtxo_id=$1", &[&coin.to_string()],
		).await?.get(0))).await.unwrap();
		assert_eq!(count, 1, "future requests must supersede, not multiply original entitlements");
		drop(wallet); // no original wallet operation after this point
		let tip = core.get_block_count().unwrap() as u32;
		ctx.generate_blocks(booked_height + if repeated { 2 } else { 0 } - tip).await;
	} else {
		let tip = core.get_block_count().unwrap() as u32;
		ctx.generate_blocks(booked_height - tip).await;
		let request = wallet.refresh_vtxos_delegated(vec![coin]).await.unwrap().unwrap();
		hashes.push(request.state().unlock_hash().unwrap().to_string());
		drop(request);
		drop(wallet); // no original wallet operation after request acceptance
	}
	let unlock = hashes.last().unwrap().clone();
	let mut finished = srv.subscribe_log::<RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(60)).await.unwrap();
	let rows = db.read(async |t| Ok(t.query("SELECT vtxo FROM vtxo WHERE spend_state='unclaimed'", &[]).await?)).await.unwrap();
	let replacements = rows.iter().map(|r| Vtxo::<Full>::deserialize(&r.get::<_, Vec<u8>>(0)).unwrap())
		.filter(|v| v.unlock_hash().is_some_and(|h| h.to_string() == unlock)).collect::<Vec<_>>();
	assert!(!replacements.is_empty());
	let principal = replacements.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert_eq!(original.amount().to_sat(), principal + refresh_fee,
		"original principal must account for the exact accepted refresh fee");
	let ids = replacements.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let replacement_expiry = replacements.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	assert!(replacement_expiry > original.expiry_height().to_u32() + 100,
		"fixture must expose a substantial replacement-versus-original expiry gap");
	let started = Instant::now();
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &[original.clone(), control_coin.clone()]).await;
	let original_swept_height = core.get_block_count().unwrap();
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let (control_payout, _) = super::fallback_lightning::wait_and_reconcile_payout(&ctx, &db,
		&[control_coin.id().to_string()], &control_record.spk, control_coin.amount().to_sat()).await;
	let control_height = core.get_block_count().unwrap();
	let control_confirmed_height = control_height - core.get_raw_transaction_info(&control_payout.txid, None).unwrap().confirmations.unwrap() as u64 + 1;
	assert!(control_height < replacement_expiry as u64, "control must settle before replacement expiry");
	let premature: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&ids]).await?.get(0))).await.unwrap();
	assert_eq!(premature, 0, "current replacement-expiry eligibility must be measured, never guessed");
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &replacements).await;
	let replacement_swept_height = core.get_block_count().unwrap();
	super::fallback_lightning::train_fee_estimator(&ctx).await;
	let (payout, _) = super::fallback_lightning::wait_and_reconcile_payout(&ctx, &db, &ids, &record.spk, principal).await;
	let payout_height = core.get_block_count().unwrap();
	let payout_confirmed_height = payout_height - core.get_raw_transaction_info(&payout.txid, None).unwrap().confirmations.unwrap() as u64 + 1;
	let timings = db.read(async |t| Ok(t.query_one("SELECT extract(epoch FROM min(created_at))::float8 FROM expiry_settlement WHERE id=ANY($1)", &[&ids]).await?.get::<_, f64>(0))).await.unwrap();
	let original_expiry_chain_time = core.get_block(&core.get_block_hash(original.expiry_height().to_u32() as u64).unwrap()).unwrap().header.time;
	let original_paid: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement WHERE id=$1", &[&coin.to_string()]).await?.get(0))).await.unwrap();
	assert_eq!(original_paid, 0, "originals and replacements must not both be paid");
	let pending: bool = db.read(async |t| Ok(t.query_one("SELECT forfeited_at IS NULL FROM round_participation WHERE unlock_hash=$1", &[&unlock]).await?.get(0))).await.unwrap();
	assert!(pending, "absent user never completed the swap");
	let delay = serde_json::json!({"regtest_accelerated":true,"grace_blocks":0,"sweep_depth":1,"interval_seconds":1,
		"original_expiry":original.expiry_height().to_u32(),"replacement_expiry":replacement_expiry,
		"original_swept_observed_height":original_swept_height,"replacement_swept_observed_height":replacement_swept_height,
		"control_expiry":control_coin.expiry_height().to_u32(),"control_payout_observed_height":control_height,
		"delegated_payout_observed_height":payout_height,"delegated_payout_confirmed_height":payout_confirmed_height,"control_payout_confirmed_height":control_confirmed_height,"original_expiry_chain_time":original_expiry_chain_time,"settlement_created_epoch":timings,"observed_controller_seconds":started.elapsed().as_secs_f64(),
		"request_hashes":hashes,"original_principal_sat":original.amount().to_sat(),"signed_refresh_fee_sat":refresh_fee,
		"replacement_principal_sat":principal,"control_payout":control_payout.to_string(),"delegated_payout":payout.to_string(),
		"extra_observed_blocks":(payout_confirmed_height-original.expiry_height().to_u32() as u64).saturating_sub(control_confirmed_height-control_coin.expiry_height().to_u32() as u64)});
	std::fs::write(ctx.datadir.join("delegated-delay.json"), serde_json::to_vec_pretty(&delay).unwrap()).unwrap();
	println!("delegated absence evidence: {delay}");
	super::fallback_lightning::restore_and_spend(&ctx, &mnemonic, payout).await;
	super::fallback_lightning::restore_and_spend(&ctx, &control_seed, control_payout).await;
}

/// The original inputs are expired/swept, while replacements remain live.
/// A returning phone completes and actually spends them; only the new payee
/// gets a later payout, never the spent original/replacement entitlement.
#[tokio::test]
async fn fallback_delegated_return_before_payout_spends_replacements() {
	Box::pin(return_before_payout()).await;
}
async fn return_before_payout() {
	let ctx = TestContext::new("bark_sdk/fallback_delegated_return_before_payout_spends_replacements").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).watchmand().cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(300);
		c.round_interval = Duration::from_secs(3600);
	}).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("phone", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let original = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(140).await;
	let request = wallet.refresh_vtxos_delegated(vec![original.id()]).await.unwrap().unwrap();
	let hash = request.state().unlock_hash().unwrap().to_string();
	drop(request);
	let mut config = wallet.config().clone();
	drop(wallet);
	let mut finished = srv.subscribe_log::<RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(60)).await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let rows = db.read(async |t| Ok(t.query("SELECT vtxo FROM vtxo WHERE spend_state='unclaimed'", &[]).await?)).await.unwrap();
	let replacements = rows.iter().map(|r| Vtxo::<Full>::deserialize(&r.get::<_, Vec<u8>>(0)).unwrap())
		.filter(|v| v.unlock_hash().is_some_and(|h| h.to_string() == hash)).collect::<Vec<_>>();
	assert!(!replacements.is_empty());
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &[original.clone()]).await;
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	assert!(replacements.iter().all(|v| v.expiry_height().to_u32() > core.get_block_count().unwrap() as u32 + 24));
	config.server_address = srv.ark_url();
	let wallet = bark::Wallet::open(bitcoin::Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(bitcoin::Network::Regtest, &mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("phone")), run_daemon: false, ..Default::default() },
	).await.unwrap();
	wallet.sync_pending_rounds().await.unwrap();
	assert!(wallet.pending_round_states().await.unwrap().is_empty());
	assert_eq!(wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.vtxo.amount().to_sat()).sum::<u64>(), 300_000);
	let recipient_seed = bip39::Mnemonic::generate(12).unwrap();
	let recipient = ctx.bark_sdk("new-payee", &srv).mnemonic(recipient_seed.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	let record = recipient.fallback_destination().await.unwrap();
	wallet.send_arkoor_payment(&address, sat(300_000)).await.unwrap();
	recipient.sync().await;
	let paid_to_recipient = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(paid_to_recipient.iter().map(|v| v.vtxo.amount().to_sat()).sum::<u64>(), 300_000);
	let mut received = Vec::new();
	for v in paid_to_recipient { received.push(recipient.get_full_vtxo(v.id()).await.unwrap()); }
	drop((wallet, recipient));
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &received).await;
	super::fallback_lightning::train_fee_estimator(&ctx).await;
	let ids = received.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let (payout, _) = super::fallback_lightning::wait_and_reconcile_payout(&ctx, &db, &ids, &record.spk, 300_000).await;
	let old_ids = std::iter::once(original.id().to_string()).chain(replacements.iter().map(|v| v.id().to_string())).collect::<Vec<_>>();
	let old_paid: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&old_ids]).await?.get(0))).await.unwrap();
	assert_eq!(old_paid, 0, "completed exchange and later transfer cannot also pay the old entitlement");
	let forfeited: bool = db.read(async |t| Ok(t.query_one("SELECT forfeited_at IS NOT NULL FROM round_participation WHERE unlock_hash=$1", &[&hash]).await?.get(0))).await.unwrap();
	assert!(forfeited);
	super::fallback_lightning::restore_and_spend(&ctx, &recipient_seed, payout).await;
}
