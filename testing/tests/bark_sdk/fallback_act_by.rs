//! Native protective refresh and exit, without fabricated financial records.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
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
	// Drop the actual client future after the upstream accepted the forfeits,
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
