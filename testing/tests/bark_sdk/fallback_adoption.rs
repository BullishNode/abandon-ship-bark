//! Real server decisions must not discard a restored or imported exit path.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bark::{ExitProtectionIncomplete, ImportVtxoArgs};
use bark::{OpenWalletArgs, Wallet, WalletSeed};
use bark::onchain::OnchainWallet;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
use bitcoin_ext::BlockDelta;
use bitcoin::Network;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server_rpc::protos;

use ark::ProtocolEncoding;
use ark_testing::{sat, TestContext};
use ark_testing::context::builders::BarkSdkBuilder;
use ark_testing::daemon::captaind::{self, ArkClient};

#[derive(Clone, Default)]
struct HistoryGate {
	refuse: Arc<AtomicBool>,
	requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for HistoryGate {
	async fn get_fallback_payouts(&self, upstream: &mut ArkClient, request: protos::GetFallbackPayoutsRequest)
		-> Result<protos::GetFallbackPayoutsResponse, tonic::Status>
	{
		self.requests.fetch_add(1, Ordering::SeqCst);
		if self.refuse.load(Ordering::SeqCst) { return Err(tonic::Status::unavailable("history deliberately unavailable")); }
		Ok(upstream.get_fallback_payouts(request).await?.into_inner())
	}
}

fn config(config: &mut bark::Config) {
	config.daemon_manual_sync = true;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
}

async fn mnemonic(ctx: &TestContext, name: &str) -> bip39::Mnemonic {
	let text = tokio::fs::read_to_string(ctx.datadir.join(name).join("mnemonic")).await.unwrap();
	bip39::Mnemonic::from_str(text.trim()).unwrap()
}

async fn reopen(ctx: &TestContext, name: &str, srv: &captaind::Captaind) -> Wallet {
	// The harness deliberately reallocates listener ports after stop/start.
	// Reopen the actual existing databases against its current endpoint.
	let mnemonic = mnemonic(ctx, name).await;
	let db = Arc::new(SqliteClient::open(ctx.datadir.join(name).join("db.sqlite")).unwrap());
	let onchain = OnchainWallet::load_or_create(Network::Regtest, mnemonic.to_seed(""), db.clone()).await.unwrap();
	let script = onchain.fallback_script();
	let mut cfg = ctx.bark_default_cfg(srv, Some(ctx.bitcoind()));
	config(&mut cfg);
	Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), cfg, OpenWalletArgs {
		persister: Some(db), onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))),
		fallback_spk: Some(script), run_daemon: false, ..Default::default()
	}).await.unwrap()
}

async fn await_decision(ctx: &TestContext, srv: &captaind::Captaind, coin: &bark::WalletVtxo) {
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(coin.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let database = Db::connect(&srv.config().postgres).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let exists = database.read(async |tx| Ok(tx.query_opt(
				"SELECT entitlement_id FROM expiry_decided_coin WHERE vtxo_id=$1", &[&coin.id().to_string()],
			).await?)).await.unwrap().is_some();
			if exists { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual unfunded background allocation did not decide the coin");
}

#[tokio::test]
async fn fallback_adoption_fresh_seed_retains_unfunded_decision_and_protects_offline() {
	let ctx = TestContext::new("bark_sdk/fallback_adoption_seed_pending").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(24);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let original = ctx.bark_sdk("original", &srv).cfg(config)
		.funded(sat(500_000)).boarded(sat(100_000)).create().await;
	let coin = original.spendable_vtxos().await.unwrap().remove(0);
	let signed = original.get_full_vtxo(coin.id()).await.unwrap();
	await_decision(&ctx, &srv, &coin).await;
	let restored = ctx.bark_sdk("restored", &srv).cfg(config)
		.mnemonic(mnemonic(&ctx, "original").await).create().await;
	let db = SqliteClient::open(ctx.datadir.join("restored/db.sqlite")).unwrap();
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(restored.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(restored.spendable_vtxos().await.unwrap().is_empty());
	assert!(restored.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert!(db.get_fallback_snapshot().await.unwrap().verified_payouts.is_empty());
	assert!(restored.history().await.unwrap().iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout")
		.all(|movement| movement.effective_balance.to_sat() == 0), "fresh restore must not invent an earlier Ark debit");
	let paid = ctx.bark_sdk("paid", &srv).cfg(config)
		.mnemonic(mnemonic(&ctx, "original").await).create().await;
	srv.stop().await.unwrap();
	let protection = restored.protect_expiring_vtxos().await.unwrap();
	assert_eq!(protection.exits_started, vec![coin.id()]);
	let exit = restored.exit_mgr().get_exit_vtxo(coin.id()).await.unwrap();
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state,
		VtxoState::Locked { holder: Some(exit.movement_id().unwrap().into()) });

	// A second independently restored database stays quarantined while the
	// first has its own protective movement. Fund the actual fee wallet and
	// cache the real signed payout before taking Ark offline at confirmation.
	srv.start().await.unwrap();
	drop(paid);
	drop(restored);
	let paid = reopen(&ctx, "paid", &srv).await;
	let restored = reopen(&ctx, "restored", &srv).await;
	ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let script = paid.fallback_destination().await.unwrap().spk;
	let payout = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				let tx = ctx.bitcoind().await_transaction(txid).await;
				if tx.output.iter().any(|output| output.script_pubkey == script && output.value == sat(99_000)) { return txid; }
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual payout did not broadcast after funding");
	paid.sync_fallback_payouts().await.unwrap();
	restored.sync_fallback_payouts().await.unwrap();
	assert!(restored.history().await.unwrap().iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout")
		.all(|movement| movement.effective_balance.to_sat() == 0), "a later protective holder must not invent a restore debit");
	ctx.generate_blocks(6).await;
	srv.stop().await.unwrap();
	paid.chain().invalidate_caches().await;
	assert!(paid.sync_fallback_payouts().await.is_err());
	let paid_db = SqliteClient::open(ctx.datadir.join("paid/db.sqlite")).unwrap();
	assert_eq!(paid_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(paid_db.get_fallback_snapshot().await.unwrap().verified_payouts.values().any(|proof| proof.outpoint.txid == payout));
	assert!(paid.exit_protection_schedule().await.unwrap().is_empty());
}

#[tokio::test]
async fn fallback_adoption_history_failure_quarantines_then_reconciles_an_ordinary_spend() {
	let ctx = TestContext::new("bark_sdk/fallback_adoption_history_failure").await;
	let srv = ctx.captaind("server").create().await;
	let gate = HistoryGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let owner = ctx.bark_sdk("owner", &proxy).cfg(config).boarded(sat(100_000)).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv).create().await;
	let coin = owner.spendable_vtxos().await.unwrap().remove(0);
	let signed = owner.get_full_vtxo(coin.id()).await.unwrap();
	owner.send_arkoor_payment(&recipient.new_address().await.unwrap(), coin.amount()).await.unwrap();
	owner.dangerous_drop_vtxo(coin.id()).await.unwrap();
	gate.refuse.store(true, Ordering::SeqCst);
	owner.import_vtxo(&signed, ImportVtxoArgs::default()).await.unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(owner.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert!(owner.exit_protection_schedule().await.unwrap().iter().any(|entry| entry.vtxo_id == coin.id()),
		"missing evidence retains the historical signed protection material");
	let watch = owner.exit_protection_report().await.unwrap();
	assert!(watch.unproven_historical.contains(&coin.id()),
		"watching a completed historic send must not authorize an automatic claim");
	let schedule = watch.schedules.iter().find(|schedule| schedule.vtxo_id == coin.id()).unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	if schedule.act_by.to_u32() > tip { ctx.generate_blocks(schedule.act_by.to_u32() - tip).await; }
	owner.chain().invalidate_caches().await;
	assert!(ctx.bitcoind().get_block_count().await >= u64::from(schedule.act_by.to_u32()));
	let refused = owner.protect_expiring_vtxos().await.expect_err("unknown historic ownership blocks automatic exit at act-by");
	let partial = refused.downcast_ref::<ExitProtectionIncomplete>().expect("structured ownership refusal");
	assert!(partial.outcome.blocked.contains(&coin.id()));
	assert!(partial.outcome.exits_started.is_empty());
	assert!(partial.report.issues.iter().any(|issue| issue.vtxo_id == Some(coin.id())
		&& issue.operation == "automatic_exit_ownership"));
	assert!(owner.exit_mgr().get_exit_vtxo(coin.id()).await.is_none());
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(owner.spendable_vtxos().await.unwrap().is_empty());
	let balance = owner.balance().await.unwrap();
	assert_eq!(balance.spendable, sat(0));
	assert_eq!(balance.awaiting_settlement, sat(0), "historic import cannot invent principal from a completed payment");
	assert_eq!(balance.total(), sat(0));
	assert!(db.get_fallback_snapshot().await.unwrap().entitlements.is_empty());
	gate.refuse.store(false, Ordering::SeqCst);
	owner.sync().await;
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(owner.exit_protection_schedule().await.unwrap().is_empty(), "ordinary historic outgoing is not resurrected");
	assert_eq!(owner.balance().await.unwrap().awaiting_settlement, sat(0));
}

#[tokio::test]
async fn fallback_adoption_status_uses_history_newer_than_the_decision() {
	let ctx = TestContext::new("bark_sdk/fallback_adoption_post_status_barrier").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(24);
	}).watchmand().create().await;
	let gate = HistoryGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let owner = ctx.bark_sdk("owner", &proxy).cfg(config).boarded(sat(100_000)).create().await;
	let coin = owner.spendable_vtxos().await.unwrap().remove(0);
	owner.sync_fallback_payouts().await.unwrap();
	assert!(owner.fallback_payouts().await.unwrap().is_empty());
	let before = gate.requests.load(Ordering::SeqCst);
	await_decision(&ctx, &srv, &coin).await;
	assert_eq!(owner.trust_and_adopt_server_vtxo_status(coin.id()).await.unwrap(), None);
	assert!(gate.requests.load(Ordering::SeqCst) > before, "the cached empty history predates the decision");
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
	assert!(owner.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert!(owner.spendable_vtxos().await.unwrap().is_empty());
}

/// ML181: authentic historical transfers must remain restorable without
/// treating unavailable/partial evidence as a financial discharge.
#[derive(Clone, Default)]
struct OrdinaryProofGate {
	mode: Arc<AtomicUsize>,
	requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for OrdinaryProofGate {
	async fn get_vtxo_spend_proof(&self, upstream: &mut ArkClient, request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		self.requests.fetch_add(1, Ordering::SeqCst);
		if self.mode.load(Ordering::SeqCst) == 0 {
			return Err(tonic::Status::unavailable("ordinary proof deliberately unavailable"));
		}
		let mut proof = upstream.get_vtxo_spend_proof(request).await?.into_inner();
		if self.mode.load(Ordering::SeqCst) == 1 {
			assert!(proof.vtxos.len() >= 2, "genuine partial payment must have payment and all change branches");
			proof.vtxos.pop();
		}
		Ok(proof)
	}
}

// Keep this long, multi-device fixture's debug poll frame small. Construct
// repeated SDK create/sync futures on a separate frame and retain only their
// boxed pointer in the test; production stack limits and timers are unchanged.
#[inline(never)]
fn create_proof_wallet<'a>(builder: BarkSdkBuilder<'a>) -> Pin<Box<dyn Future<Output = Wallet> + 'a>> {
	Box::pin(builder.create())
}

#[inline(never)]
fn sync_proof_wallet(wallet: &Wallet) -> Pin<Box<dyn Future<Output = ()> + '_>> {
	Box::pin(wallet.sync())
}

#[tokio::test]
async fn fallback_ml181_spent_without_complete_signed_proof_retains_body_without_inventing_balance() {
	let ctx = TestContext::new("bark_sdk/fallback_ml181_ordinary_spend_proof").await;
	let srv = ctx.captaind("server").create().await;
	let source = create_proof_wallet(ctx.bark_sdk("source", &srv).cfg(config).boarded(sat(200_000))).await;
	let target = create_proof_wallet(ctx.bark_sdk("target", &srv).cfg(config)).await;
	source.send_arkoor_payment(&target.new_address().await.unwrap(), sat(100_000)).await.unwrap();
	sync_proof_wallet(&target).await;
	let original = target.spendable_vtxos().await.unwrap().remove(0);
	let signed = target.get_full_vtxo(original.id()).await.unwrap();
	let seed = mnemonic(&ctx, "target").await;
	let gate = OrdinaryProofGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	// This real second device already positively owned the input before the
	// other device paid. Its original principal must neither vanish nor be
	// counted again alongside the actual signed change it recovers later.
	let older = create_proof_wallet(ctx.bark_sdk("older-device", &proxy).cfg(config).mnemonic(seed.clone())).await;
	assert_eq!(older.balance().await.unwrap().total(), sat(100_000));
	target.send_arkoor_payment(&source.new_address().await.unwrap(), sat(60_000)).await.unwrap();
	assert_eq!(target.balance().await.unwrap().total(), sat(40_000));
	let intermediate = target.spendable_vtxos().await.unwrap();
	assert!(!intermediate.is_empty());
	assert_eq!(intermediate.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(40_000));
	older.recover_vtxos(std::iter::once(original.id()).chain(intermediate.iter().map(|coin| coin.id())), None).await.unwrap();
	let older_pending = older.balance().await.unwrap();
	assert_eq!(older_pending.spendable, sat(40_000));
	assert_eq!(older_pending.awaiting_settlement, sat(60_000));
	assert_eq!(older_pending.total(), sat(100_000), "old source plus its known signed change cannot become140k");
	// A real second payment creates a third generation. This device learns
	// only the newest change, so its intermediate remains locally Spendable.
	// Availability must detect that stale parent before any balance call.
	target.send_arkoor_payment(&source.new_address().await.unwrap(), sat(10_000)).await.unwrap();
	let newest = target.spendable_vtxos().await.unwrap();
	assert_eq!(newest.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(30_000));
	let current_ids = newest.iter().map(|coin| coin.id()).collect::<BTreeSet<_>>();
	let stale = intermediate.iter().filter(|coin| !current_ids.contains(&coin.id())).collect::<Vec<_>>();
	assert!(!stale.is_empty(), "the real second payment must consume a previously retained intermediate");
	let mut current_paths = Vec::new();
	for coin in &newest {
		let full = target.get_full_vtxo(coin.id()).await.unwrap();
		let path = full.ancestor_ids();
		assert!(path.contains(&original.id()), "every current change is on the actual original signed path");
		current_paths.push(path);
	}
	assert!(stale.iter().any(|coin| current_paths.iter().any(|path| path.contains(&coin.id()))),
		"actual change must exercise the stale middle ancestor, not an unrelated source");
	drop(target);
	older.recover_vtxos(newest.iter().map(|coin| coin.id()), None).await.unwrap();
	let older_db = SqliteClient::open(ctx.datadir.join("older-device/db.sqlite")).unwrap();
	for coin in &stale {
		assert_eq!(older_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
		assert!(!older_db.get_fallback_snapshot().await.unwrap().awaiting_settlement.contains(&coin.id()));
	}
	assert_eq!(older.spendable_vtxos().await.unwrap().iter().map(|coin| coin.id()).collect::<BTreeSet<_>>(), current_ids);
	let fenced = older_db.get_fallback_snapshot().await.unwrap().awaiting_settlement;
	for coin in &stale {
		assert!(fenced.contains(&coin.id()), "inferred stale parent remains fenced after the original source resolves");
	}
	assert!(current_ids.iter().all(|id| !fenced.contains(id)), "current unchanged and new change outputs remain available");
	let transitive = older.balance().await.unwrap();
	assert_eq!(transitive.spendable, sat(30_000));
	assert_eq!(transitive.awaiting_settlement, sat(70_000));
	assert_eq!(transitive.total(), sat(100_000), "stale middle plus newest change cannot inflate principal");
	let restored = create_proof_wallet(ctx.bark_sdk("restored", &proxy).cfg(config).mnemonic(seed)).await;
	let db = SqliteClient::open(ctx.datadir.join("restored/db.sqlite")).unwrap();
	let before = gate.requests.load(Ordering::SeqCst);
	assert!(before > 0, "recovery must request actual signed-spender evidence");
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(restored.get_full_vtxo(original.id()).await.unwrap().serialize(), signed.serialize());
	assert!(restored.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000), "retained old input is not additional owned principal");
	assert!(db.get_fallback_snapshot().await.unwrap().entitlements.is_empty());
	assert!(restored.history().await.unwrap().iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout"));

	gate.mode.store(1, Ordering::SeqCst);
	sync_proof_wallet(&restored).await;
	assert!(gate.requests.load(Ordering::SeqCst) > before);
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None },
		"one signed branch cannot retire the whole source");
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000));
	sync_proof_wallet(&older).await;
	assert_eq!(older.balance().await.unwrap().total(), sat(100_000));

	gate.mode.store(2, Ordering::SeqCst);
	sync_proof_wallet(&restored).await;
	sync_proof_wallet(&older).await;
	assert_eq!(older.balance().await.unwrap().total(), sat(30_000));
	assert!(!older.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(!restored.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert!(!db.get_fallback_snapshot().await.unwrap().quarantined_vtxos.contains(&original.id()));
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000));
	assert_eq!(restored.get_full_vtxo(original.id()).await.unwrap().serialize(), signed.serialize(),
		"verified retirement must retain signed recovery material");
	// Restore only the LOCAL checkpoint boundary before atomic retirement.
	// The signature fact is genuine from the completed transfer above; no
	// server financial table or transaction is modified by this crash model.
	let mut interrupted = db.get_fallback_snapshot().await.unwrap();
	assert!(interrupted.verified_ordinary_spends.contains(&original.id()));
	interrupted.quarantined_vtxos.insert(original.id());
	interrupted.unproven_spent_vtxos.insert(original.id());
	interrupted.awaiting_settlement.insert(original.id());
	db.store_fallback_snapshot(&interrupted).await.unwrap();
	db.update_vtxo_state_checked(original.id(), VtxoState::Locked { holder: None },
		&[bark::vtxo::VtxoStateKind::Spent]).await.unwrap();
	srv.stop().await.unwrap();
	sync_proof_wallet(&restored).await;
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Spent,
		"immutable signed-spend checkpoint must finish without the server");
	assert!(!restored.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert!(!restored.exit_protection_schedule().await.unwrap().iter().any(|entry| entry.vtxo_id == original.id()),
		"a crash cannot rearm an already owner-authorized ordinary transfer");
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000));
}

#[derive(Clone, Default)]
struct MissingHistoryGate {
	proof_requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for MissingHistoryGate {
	async fn get_fallback_payouts(&self, upstream: &mut ArkClient, request: protos::GetFallbackPayoutsRequest)
		-> Result<protos::GetFallbackPayoutsResponse, tonic::Status>
	{
		// An API-negative missing-history response, never a financial DB edit.
		// Status still comes from the actual admitted unpaid decision.
		let mut response = upstream.get_fallback_payouts(request).await?.into_inner();
		response.entitlements.clear();
		response.next_after_entitlement_id = None;
		Ok(response)
	}

	async fn get_vtxo_spend_proof(&self, _upstream: &mut ArkClient, _request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		self.proof_requests.fetch_add(1, Ordering::SeqCst);
		Err(tonic::Status::unavailable("ordinary proof deliberately unavailable"))
	}
}

/// ML181: an ACTUAL unfunded admission fence with a missing history response
/// cannot erase a locally owned unpaid principal. No Spent flag is forged.
#[tokio::test]
async fn fallback_ml181_locally_owned_unpaid_spent_retains_principal_with_missing_history() {
	let ctx = TestContext::new("bark_sdk/fallback_ml181_owned_missing_history").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let gate = MissingHistoryGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let owner = ctx.bark_sdk("owner", &proxy).cfg(config).boarded(sat(100_000)).create().await;
	let coin = owner.spendable_vtxos().await.unwrap().remove(0);
	let signed = owner.get_full_vtxo(coin.id()).await.unwrap();
	assert_eq!(owner.balance().await.unwrap().total(), sat(100_000));
	await_decision(&ctx, &srv, &coin).await;
	owner.recover_vtxos([coin.id()], None).await.unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let snapshot = db.get_fallback_snapshot().await.unwrap();
	assert!(snapshot.entitlements.is_empty(), "negative API response must reach the actual client");
	assert!(gate.proof_requests.load(Ordering::SeqCst) > 0);
	assert!(!snapshot.unproven_spent_vtxos.contains(&coin.id()), "known local ownership is not a fresh historic import");
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
	assert!(owner.spendable_vtxos().await.unwrap().is_empty());
	assert!(owner.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	let pending = owner.balance().await.unwrap();
	assert_eq!(pending.spendable, sat(0));
	assert_eq!(pending.awaiting_settlement, sat(100_000));
	assert_eq!(pending.total(), sat(100_000));
	assert!(owner.exit_protection_schedule().await.unwrap().iter().any(|schedule| schedule.vtxo_id == coin.id()));
	assert!(owner.history().await.unwrap().iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout"));
}
