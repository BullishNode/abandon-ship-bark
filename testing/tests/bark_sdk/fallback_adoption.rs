//! Real server decisions must not discard a restored or imported exit path.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bark::ImportVtxoArgs;
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
	let balance = owner.balance().await.unwrap();
	assert_eq!(balance.spendable, sat(0));
	assert_eq!(balance.awaiting_settlement, coin.amount(), "unknown ordinary spend must not disappear or be available");
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
