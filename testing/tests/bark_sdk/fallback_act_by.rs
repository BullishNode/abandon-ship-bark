//! Native protective refresh and exit, without fabricated financial records.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
use bitcoin::FeeRate;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use server_rpc::protos;

use ark::ProtocolEncoding;
use ark::tree::signed::UnlockPreimage;
use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::ROUND_CONFIRMATIONS;
use ark_testing::daemon::captaind::{self, ArkClient};
use ark_testing::util::FutureExt;

fn protection_config(config: &mut bark::Config) {
	config.daemon_manual_sync = true;
	config.daemon_sync_interval_secs = 10;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
}

#[tokio::test]
async fn fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable() {
	let ctx = TestContext::new("bark_sdk/fallback_protection_default_daemon").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let wallet = ctx.bark_sdk("owner", &srv).cfg(|config| {
		protection_config(config);
		config.daemon_manual_sync = false;
		config.daemon_sync_interval_secs = 1;
	}).funded(sat(500_000)).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let schedule = wallet.exit_protection_schedule().await.unwrap().remove(0);
	advance_to(&ctx, schedule.refresh_by).await;
	tokio::time::timeout(Duration::from_secs(20), async {
		while wallet.pending_round_state_ids().await.unwrap().is_empty() {
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
	}).await.expect("default daemon did not initiate its own early refresh");
	srv.trigger_round().await;
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let pool = ctx.bitcoind().sync_client().get_raw_mempool().unwrap();
			for pending in wallet.pending_round_states().await.unwrap() {
				if pending.state().output_vtxos().is_some_and(|coins|
					coins.iter().any(|coin| pool.contains(&coin.chain_anchor().txid))) { return; }
			}
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
	}).await.expect("actual early-refresh funding transaction did not reach Core");
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	let replacement = tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			if let Some(coin) = wallet.spendable_vtxos().await.unwrap().into_iter()
				.find(|coin| coin.id() != original.id()) { return coin; }
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
	}).await.expect("default daemon did not adopt its actually confirmed refresh");
	advance_to(&ctx, schedule.act_by).await;
	assert!(wallet.exit_mgr().get_exit_vtxo_ids().await.is_empty(), "healthy confirmed replacement must not be exited");
	let replacement_schedule = wallet.exit_protection_schedule().await.unwrap().into_iter()
		.find(|schedule| schedule.vtxo_id == replacement.id()).unwrap();
	let signed = wallet.get_full_vtxo(replacement.id()).await.unwrap();
	wallet.sync_onchain().await.unwrap();
	srv.stop().await.unwrap();
	advance_to(&ctx, replacement_schedule.act_by).await;
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let pool = ctx.bitcoind().sync_client().get_raw_mempool().unwrap();
			if signed.transactions().any(|step| pool.contains(&step.tx.compute_txid())) { break; }
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
	}).await.expect("default daemon did not broadcast the actual signed protective path with Ark offline");
	assert!(wallet.exit_mgr().get_exit_vtxo_ids().await.contains(&replacement.id()));
}

async fn advance_to(ctx: &TestContext, height: BlockHeight) {
	let tip = ctx.bitcoind().get_block_count().await as u32;
	if height.to_u32() > tip { ctx.generate_blocks(height.to_u32() - tip).await; }
}

#[tokio::test]
async fn fallback_protection_refreshes_a_healthy_wallet_without_a_spurious_exit() {
	let ctx = TestContext::new("bark_sdk/fallback_protection_healthy_refresh").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let wallet = ctx.bark_sdk("owner", &srv).cfg(protection_config).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let schedule = wallet.exit_protection_schedule().await.unwrap().remove(0);
	assert!(schedule.refresh_by < original.expiry_height().saturating_sub(BlockDelta::new(4)),
		"the actual signed exit path requires refresh before the normal threshold");
	advance_to(&ctx, schedule.refresh_by).await;
	let (result, _) = tokio::join!(wallet.protect_expiring_vtxos(), async {
		tokio::time::sleep(Duration::from_millis(300)).await;
		srv.trigger_round().await;
	});
	let result = result.unwrap();
	assert_eq!(result.refresh_attempted, vec![original.id()]);
	assert!(result.exits_started.is_empty());
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.chain().invalidate_caches().await;
	wallet.sync_pending_rounds().await.unwrap();
	let replacement = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(replacement.len(), 1);
	assert_ne!(replacement[0].id(), original.id());
	assert!(replacement[0].expiry_height() > original.expiry_height());
	advance_to(&ctx, schedule.act_by).await;
	let result = wallet.protect_expiring_vtxos().await.unwrap();
	assert!(result.exits_started.is_empty(), "confirmed healthy refresh must prevent a needless exit");
	assert!(wallet.exit_mgr().get_exit_vtxo_ids().await.is_empty());
}

#[tokio::test]
async fn fallback_protection_late_return_with_server_offline_starts_an_actual_exit() {
	let ctx = TestContext::new("bark_sdk/fallback_protection_offline_exit").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let wallet = ctx.bark_sdk("owner", &srv).cfg(protection_config)
		.funded(sat(500_000)).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let signed = wallet.get_full_vtxo(original.id()).await.unwrap();
	let schedule = wallet.exit_protection_schedule().await.unwrap().remove(0);
	srv.stop().await.unwrap();
	advance_to(&ctx, schedule.act_by).await;
	let result = wallet.protect_expiring_vtxos().await.unwrap();
	assert_eq!(result.exits_started, vec![original.id()]);
	assert!(result.refresh_attempted.is_empty(), "a late return has no confirmation runway for a new round");
	let database = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let exit = wallet.exit_mgr().get_exit_vtxo(original.id()).await.unwrap();
	assert_eq!(database.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state,
		VtxoState::Locked { holder: Some(exit.movement_id().unwrap().into()) });
	assert_eq!(wallet.get_full_vtxo(original.id()).await.unwrap().serialize(), signed.serialize());
	wallet.sync_onchain().await.unwrap();
	wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
	let mempool = ctx.bitcoind().sync_client().get_raw_mempool().unwrap();
	assert!(signed.transactions().any(|step| mempool.contains(&step.tx.compute_txid())),
		"the protected signed exit must actually reach Core, not merely create a local flag");
}

#[tokio::test]
async fn fallback_protection_cancels_a_silent_pre_forfeit_refresh_without_stealing_its_lock() {
	let ctx = TestContext::new("bark_sdk/fallback_protection_pending_cancel").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let wallet = ctx.bark_sdk("owner", &srv).cfg(|config| {
		protection_config(config); config.daemon_sync_interval_secs = 2;
	}).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let schedule = wallet.exit_protection_schedule().await.unwrap().remove(0);
	let (refresh, protection) = tokio::join!(wallet.refresh_vtxos([original.id()]), async {
		tokio::time::timeout(Duration::from_secs(10), async {
			while wallet.pending_round_state_ids().await.unwrap().is_empty() {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		}).await.expect("native interactive refresh did not persist its intent");
		advance_to(&ctx, schedule.act_by).await;
		wallet.protect_expiring_vtxos().await.unwrap()
	});
	assert!(refresh.is_err(), "an abandoned pre-forfeit attempt must not pretend to be a completed refresh");
	assert_eq!(protection.exits_started, vec![original.id()]);
	let exit = wallet.exit_mgr().get_exit_vtxo(original.id()).await.unwrap();
	let database = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	wallet.sync_pending_rounds().await.unwrap();
	assert_eq!(database.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state,
		VtxoState::Locked { holder: Some(exit.movement_id().unwrap().into()) },
		"late round cleanup must release only its own holder, never the protective exit");
}

#[derive(Clone)]
struct HoldActualForfeitReply {
	hold: Arc<AtomicBool>,
	seen: Arc<tokio::sync::Notify>,
	preimage: Arc<Mutex<Option<UnlockPreimage>>>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for HoldActualForfeitReply {
	async fn forfeit_vtxos(&self, upstream: &mut ArkClient, request: protos::ForfeitVtxosRequest)
		-> Result<protos::ForfeitVtxosResponse, tonic::Status>
	{
		let response = upstream.forfeit_vtxos(request).await?.into_inner();
		*self.preimage.lock().unwrap() = Some(response.unlock_preimage.as_slice().try_into().unwrap());
		self.seen.notify_one();
		while self.hold.load(Ordering::SeqCst) { tokio::time::sleep(Duration::from_millis(10)).await; }
		Ok(response)
	}
}

#[tokio::test]
async fn fallback_protection_lost_forfeit_reply_keeps_signed_replacements_durably() {
	let ctx = TestContext::new("bark_sdk/fallback_protection_forfeit_checkpoint").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().create().await;
	let held = HoldActualForfeitReply { hold: Arc::new(AtomicBool::new(true)),
		seen: Arc::new(tokio::sync::Notify::new()), preimage: Arc::new(Mutex::new(None)) };
	let proxy = srv.start_proxy_no_mailbox(held.clone()).await;
	let wallet = ctx.bark_sdk("owner", &proxy).cfg(protection_config).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let round = wallet.refresh_vtxos_delegated([original.id()]).await.unwrap().unwrap();
	let mut finished = srv.subscribe_log::<server_log::RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(30)).await.unwrap();
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.chain().invalidate_caches().await;
	// Drop the client future after the upstream accepted the forfeits,
	// while the reply is held. The checkpoint must already be durable.
	let mut syncing = Box::pin(wallet.sync_pending_rounds());
	tokio::select! {
		result = &mut syncing => panic!("forfeit call completed before the held response: {result:?}"),
		_ = async { tokio::time::timeout(Duration::from_secs(30), held.seen.notified()).await.expect("actual upstream never accepted the forfeits"); } => {},
	}
	let database = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let checkpoint = database.get_round_state_by_id(round.id()).await.unwrap().unwrap();
	assert!(checkpoint.state().forfeits_may_be_exposed());
	let funding = checkpoint.state().funding_tx().unwrap();
	let preimage = held.preimage.lock().unwrap().unwrap();
	for output in checkpoint.state().output_vtxos().unwrap() {
		let mut output = output.clone();
		assert!(output.provide_unlock_preimage(preimage));
		output.validate(funding).expect("checkpoint must retain all signed replacement exit material");
	}
	drop(syncing);
	// Retry the same durable state after dropping the cancelled caller.
	held.hold.store(false, Ordering::SeqCst);
	wallet.sync_pending_rounds().await.unwrap();
	assert!(wallet.pending_round_states().await.unwrap().is_empty());
	assert!(!wallet.spendable_vtxos().await.unwrap().iter().any(|coin| coin.id() == original.id()));
}

#[tokio::test]
async fn fallback_protection_binding_refresh_with_ark_offline_recovers_the_original_claim() {
	let ctx = TestContext::new("bark_sdk/fallback_protection_binding_offline").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let held = HoldActualForfeitReply { hold: Arc::new(AtomicBool::new(true)),
		seen: Arc::new(tokio::sync::Notify::new()), preimage: Arc::new(Mutex::new(None)) };
	let proxy = srv.start_proxy_no_mailbox(held.clone()).await;
	let wallet = ctx.bark_sdk("owner", &proxy).cfg(protection_config)
		.funded(sat(500_000)).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let schedule = wallet.exit_protection_schedule().await.unwrap().remove(0);
	let round = wallet.refresh_vtxos_delegated([original.id()]).await.unwrap().unwrap();
	let mut finished = srv.subscribe_log::<server_log::RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(30)).await.unwrap();
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.chain().invalidate_caches().await;
	let mut syncing = Box::pin(wallet.sync_pending_rounds());
	tokio::select! {
		result = &mut syncing => panic!("forfeit call completed before its held response: {result:?}"),
		_ = async { tokio::time::timeout(Duration::from_secs(30), held.seen.notified()).await.unwrap(); } => {},
	}
	drop(syncing);
	let database = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let checkpoint = database.get_round_state_by_id(round.id()).await.unwrap().unwrap();
	assert!(checkpoint.state().forfeits_may_be_exposed());
	let material = checkpoint.state().output_vtxos().unwrap().to_vec();
	let holder = checkpoint.state().lock_holder();
	srv.stop().await.unwrap();
	advance_to(&ctx, schedule.act_by).await;
	let protection = wallet.protect_expiring_vtxos().await.unwrap();
	assert_eq!(protection.exits_started, vec![original.id()]);
	assert_eq!(database.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state,
		VtxoState::Locked { holder: holder.clone() }, "a binding refresh keeps its own holder during protection");
	wallet.exit_mgr().cancel_exit(original.id()).await.unwrap();
	assert_eq!(database.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state,
		VtxoState::Locked { holder }, "cancelling only the protective exit must not release a binding round");
	assert_eq!(database.get_round_state_by_id(round.id()).await.unwrap().unwrap().state().output_vtxos().unwrap(), material);
	assert_eq!(wallet.protect_expiring_vtxos().await.unwrap().exits_started, vec![original.id()]);
	let mut claimable = Vec::new();
	for _ in 0..15 {
		wallet.sync_onchain().await.unwrap();
		wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
		claimable = wallet.exit_mgr().list_claimable().await;
		if !claimable.is_empty() { break; }
		ctx.generate_blocks(1).await;
		wallet.chain().invalidate_caches().await;
	}
	assert!(claimable.iter().any(|exit| exit.id() == original.id()), "original claim never became independently available");
	let address = ctx.bitcoind().get_new_address();
	let claim = wallet.exit_mgr().drain_exits(&claimable, &wallet, address.clone(),
		Some(FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert!(claim.input.iter().any(|input| input.previous_output == original.point()));
	wallet.chain().broadcast_tx(&claim).await.unwrap();
	ctx.generate_blocks(6).await;
	assert_eq!(ctx.bitcoind().get_received_by_address(&address), claim.output[0].value);
	assert!(ctx.bitcoind().sync_client().get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap().confirmations >= 6);
	let retained = database.get_round_state_by_id(round.id()).await.unwrap().unwrap();
	assert!(retained.state().forfeits_may_be_exposed());
	assert_eq!(retained.state().output_vtxos().unwrap(), material);
}

#[tokio::test]
async fn fallback_binding_refresh_recovers_from_actual_chain_preimage_without_ark() {
	let ctx = TestContext::new("bark_sdk/fallback_binding_chain_preimage").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).funded(btc(1)).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(80);
		config.vtxo_exit_delta = BlockDelta::new(6);
	}).watchmand_cfg(|config| {
		config.watchman.reaction_interval = Duration::from_secs(15 * 60);
		config.watchman.sweep_interval = Duration::from_secs(15 * 60);
	}).create().await;
	let held = HoldActualForfeitReply { hold: Arc::new(AtomicBool::new(true)),
		seen: Arc::new(tokio::sync::Notify::new()), preimage: Arc::new(Mutex::new(None)) };
	let proxy = srv.start_proxy_no_mailbox(held.clone()).await;
	let wallet = ctx.bark_sdk("owner", &proxy).cfg(protection_config)
		.funded(sat(500_000)).boarded(sat(100_000)).create().await;
	let original = wallet.spendable_vtxos().await.unwrap().remove(0);
	let original_signed = wallet.get_full_vtxo(original.id()).await.unwrap();
	let funding = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(funding, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	// An ageing coin refreshes into a newer tree. The original and
	// replacement have different expiry windows; no clock or proof is faked.
	advance_to(&ctx, original.expiry_height().saturating_sub(BlockDelta::new(30))).await;
	let round = wallet.refresh_vtxos_delegated([original.id()]).await.unwrap().unwrap();
	let mut finished = srv.subscribe_log::<server_log::RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(30)).await.unwrap();
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.chain().invalidate_caches().await;
	let mut syncing = Box::pin(wallet.sync_pending_rounds());
	tokio::select! {
		result = &mut syncing => panic!("forfeit request returned before its held response: {result:?}"),
		_ = async { tokio::time::timeout(Duration::from_secs(30), held.seen.notified()).await.unwrap(); } => {},
	}
	drop(syncing);
	let database = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let checkpoint = database.get_round_state_by_id(round.id()).await.unwrap().unwrap();
	let locked_replacements = checkpoint.state().output_vtxos().unwrap().to_vec();
	let funding_tx = checkpoint.state().funding_tx().unwrap().clone();
	let holder = checkpoint.state().lock_holder();
	assert_eq!(locked_replacements.len(), 1);
	let replacement_id = locked_replacements[0].id();
	let unlock = checkpoint.state().unlock_hash().unwrap();
	let forfeit = ark::forfeit::create_hark_forfeit_tx(&original_signed, unlock, None);
	let schedule = wallet.exit_protection_schedule().await.unwrap().into_iter()
		.find(|schedule| schedule.vtxo_id == original.id()).unwrap();
	srv.stop().await.unwrap();
	advance_to(&ctx, schedule.act_by).await;
	assert_eq!(wallet.protect_expiring_vtxos().await.unwrap().exits_started, vec![original.id()]);
	wallet.sync_onchain().await.unwrap();
	assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0,
		"the original protective intent carries one principal before the preimage is known");
	for _ in 0..10 {
		wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
		if matches!(wallet.chain().tx_status(original.point().txid).await.unwrap(), bitcoin_ext::TxStatus::Confirmed(_)) { break; }
		ctx.generate_blocks(1).await;
		wallet.chain().invalidate_caches().await;
	}
	let grace = srv.watchmand().config().watchman.progress_grace_period.to_u32();
	let tip = ctx.generate_blocks(grace).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let mut progress = srv.watchmand().subscribe_log::<server_log::ProgressBroadcast>();
	srv.watchmand().trigger_sweep().await;
	let broadcast = progress.recv().wait(Duration::from_secs(20)).await.unwrap();
	assert_eq!(broadcast.txid, forfeit.compute_txid());
	ctx.bitcoind().await_transaction(broadcast.cpfp_txid).await;
	let tip = ctx.generate_blocks(1).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let mut claims = srv.watchmand().subscribe_log::<server_log::ClaimBroadcast>();
	srv.watchmand().trigger_sweep().await;
	let revealed = claims.recv().wait(Duration::from_secs(20)).await.unwrap();
	assert!(revealed.vtxo_ids.iter().any(|id| id.to_point() == bitcoin::OutPoint::new(forfeit.compute_txid(), 0)));
	let reveal_tx = ctx.bitcoind().await_transaction(revealed.txid).await;
	assert!(reveal_tx.input.iter().any(|input| input.previous_output == bitcoin::OutPoint::new(forfeit.compute_txid(), 0)));
	ctx.generate_blocks(1 + original.exit_delta().to_u32()).await;
	wallet.chain().invalidate_caches().await;
	wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
	assert!(forfeit.input.iter().any(|input| input.previous_output == original.point()));
	assert!(matches!(wallet.chain().tx_status(forfeit.compute_txid()).await.unwrap(), bitcoin_ext::TxStatus::Confirmed(_)));
	assert!(ctx.bitcoind().sync_client().get_tx_out(&original.point().txid, original.point().vout, Some(true)).unwrap().is_none(),
		"the original leaf must actually be consumed by the confirmed server forfeit");
	// The native exit label is not a payment proof. Its Claimable scanner
	// starts at claimable_since and can miss an earlier server forfeit; a
	// server forfeit seen later can also be labelled Claimed. Neither pays us.
	// The controller has seen the real response, but never gives its secret
	// to the wallet. Only the existing watchman's chain witness can.
	wallet.sync_pending_rounds().await.unwrap();
	let recovered = database.get_round_state_by_id(round.id()).await.unwrap().unwrap();
	for coin in recovered.state().output_vtxos().unwrap() { coin.validate(&funding_tx).unwrap(); }
	assert!(recovered.state().forfeits_may_be_exposed());
	assert_eq!(database.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: holder.clone() });
	assert_eq!(database.get_wallet_vtxo(replacement_id).await.unwrap().unwrap().state, VtxoState::Locked { holder: holder.clone() });
	assert!(wallet.exit_mgr().get_exit_vtxo_ids().await.contains(&replacement_id));
	assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0,
		"the cancelled original intent, round fee and replacement exit reserve one principal");
	let mut claimable = Vec::new();
	for _ in 0..20 {
		wallet.sync_onchain().await.unwrap();
		wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
		claimable = wallet.exit_mgr().list_claimable().await.into_iter().filter(|exit| exit.id() == replacement_id).collect::<Vec<_>>();
		if !claimable.is_empty() { break; }
		ctx.generate_blocks(1).await;
		wallet.chain().invalidate_caches().await;
	}
	assert_eq!(claimable.len(), 1, "actual signed replacement never became claimable");
	let address = ctx.bitcoind().get_new_address();
	let claim = wallet.exit_mgr().drain_exits(&claimable, &wallet, address.clone(),
		Some(FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert!(claim.input.iter().any(|input| input.previous_output == replacement_id.to_point()));
	wallet.chain().broadcast_tx(&claim).await.unwrap();
	ctx.generate_blocks(6).await;
	wallet.chain().invalidate_caches().await;
	wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
	wallet.sync_pending_rounds().await.unwrap();
	assert_eq!(wallet.pending_round_states().await.unwrap().len(), 1,
		"the signed emergency checkpoint remains watched after its current claim confirms");
	assert!(matches!(wallet.sync_pending_rounds().await.unwrap().get(&round.id()),
		Some(bark::round::RoundStatus::OnchainRecovery { claim_confirmed: true })));
	assert!(ctx.bitcoind().get_received_by_address(&address) > sat(90_000));
	assert!(ctx.bitcoind().sync_client().get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap().confirmations >= 6);
	assert_eq!(database.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert_eq!(database.get_wallet_vtxo(replacement_id).await.unwrap().unwrap().state, VtxoState::Spent);
	assert_eq!(wallet.get_full_vtxo(original.id()).await.unwrap(), original_signed);
	wallet.get_full_vtxo(replacement_id).await.unwrap().validate(&funding_tx).unwrap();
	assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0,
		"original intent and replacement exit must not debit principal twice");
	let before = database.get_exit_vtxo_entry(&replacement_id).await.unwrap().unwrap();
	let exit_movement = before.movement_id.unwrap();
	let signed_replacement = wallet.get_full_vtxo(replacement_id).await.unwrap();
	let history_ids = wallet.history().await.unwrap().iter().map(|entry| entry.id).collect::<Vec<_>>();
	let rpc = ctx.bitcoind().sync_client();
	let confirmed = rpc.get_raw_transaction_info(&claim.compute_txid(), None).unwrap();
	rpc.invalidate_block(&confirmed.blockhash.unwrap()).unwrap();
	wallet.chain().invalidate_caches().await;
	for _ in 0..2 {
		assert!(matches!(wallet.sync_pending_rounds().await.unwrap().get(&round.id()),
			Some(bark::round::RoundStatus::OnchainRecovery { claim_confirmed: false })));
		assert_eq!(database.get_wallet_vtxo(replacement_id).await.unwrap().unwrap().state,
			VtxoState::Locked { holder: holder.clone() }, "only this round's retired replacement may be rearmed");
		assert_eq!(wallet.get_full_vtxo(replacement_id).await.unwrap(), signed_replacement);
		assert_eq!(database.get_exit_vtxo_entry(&replacement_id).await.unwrap().unwrap().movement_id, Some(exit_movement));
		assert_eq!(database.get_movement_by_id(exit_movement).await.unwrap().status, bark::movement::MovementStatus::Pending);
		assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0);
		assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.id).collect::<Vec<_>>(), history_ids,
			"a reorg/retry must not manufacture another debit or exit intent");
	}
	assert!(wallet.exit_mgr().get_exit_vtxo_ids().await.contains(&replacement_id));
	wallet.sync_onchain().await.unwrap();
	wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
	// Reconfirm the very same real signed claim, not a fabricated receipt.
	wallet.chain().broadcast_tx(&claim).await.unwrap();
	ctx.generate_blocks(6).await;
	wallet.chain().invalidate_caches().await;
	wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
	assert!(matches!(wallet.sync_pending_rounds().await.unwrap().get(&round.id()),
		Some(bark::round::RoundStatus::OnchainRecovery { claim_confirmed: true })));
	assert_eq!(database.get_wallet_vtxo(replacement_id).await.unwrap().unwrap().state, VtxoState::Spent);
	assert_eq!(wallet.get_full_vtxo(replacement_id).await.unwrap(), signed_replacement);
	assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0);
	assert_eq!(wallet.history().await.unwrap().iter().map(|entry| entry.id).collect::<Vec<_>>(), history_ids);
}
