//! The default daemon exits an unpaid fallback source while Ark history is unavailable.
//! The app then signs and broadcasts the claim through the public exit API.

use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

use ark::{ProtocolEncoding, VtxoId};
use ark_testing::{sat, TestContext};
use ark_testing::daemon::captaind::{self, ArkClient};
use bark::{Balance, ImportVtxoArgs, OpenWalletArgs, Wallet, WalletSeed};
use bark::movement::MovementStatus;
use bark::exit::ExitState;
use bark::onchain::OnchainWallet;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
use bitcoin::{FeeRate, Network};
use bitcoin_ext::{BlockDelta, rpc::{BitcoinRpcExt, RpcApi}};
use server::database::Db;
use server_rpc::protos;

#[derive(Clone, Default)]
struct HistoryUnavailable {
	refuse: Arc<AtomicBool>,
	hits: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for HistoryUnavailable {
	async fn get_vtxo_spend_proof(&self, upstream: &mut ArkClient, request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		if self.refuse.load(Ordering::SeqCst) {
			self.hits.fetch_add(1, Ordering::SeqCst);
			return Err(tonic::Status::unavailable("owned fixture: authenticated history unavailable"));
		}
		Ok(upstream.get_vtxo_spend_proof(request).await?.into_inner())
	}
}

fn configure(config: &mut bark::Config) {
	config.daemon_manual_sync = true;
	config.daemon_sync_interval_secs = 1;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
}

fn exact_principal(balance: Balance, amount: bitcoin::Amount, awaiting_only: bool) {
	assert_eq!(balance.spendable, sat(0));
	assert_eq!(balance.needs_refresh, sat(0));
	assert_eq!(balance.pending_board, sat(0));
	assert_eq!(balance.pending_in_round, sat(0));
	assert_eq!(balance.pending_arkoor_send, sat(0));
	assert_eq!(balance.pending_offboard, sat(0));
	assert_eq!(balance.pending_lightning_send, sat(0));
	assert_eq!(balance.claimable_lightning_receive, sat(0));
	assert_eq!(balance.awaiting_settlement + balance.pending_exit, amount);
	if awaiting_only { assert_eq!(balance.awaiting_settlement, amount); assert_eq!(balance.pending_exit, sat(0)); }
	assert_eq!(balance.total(), amount, "historic spent material is not additional principal");
}

async fn open_cached(ctx: &TestContext, mnemonic: &bip39::Mnemonic, config: bark::Config) -> Wallet {
	let db = Arc::new(SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap());
	let onchain = OnchainWallet::load_or_create(Network::Regtest, mnemonic.to_seed(""), db.clone()).await.unwrap();
	let fallback_spk = onchain.fallback_script();
	Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, mnemonic), config,
		OpenWalletArgs { datadir: Some(ctx.datadir.join("owner")), persister: Some(db),
			onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))), fallback_spk: Some(fallback_spk),
			run_daemon: true, ..Default::default() }).await.unwrap()
}

async fn historic_is_blocked(wallet: &Wallet, db: &SqliteClient, id: VtxoId, body: &[u8]) {
	assert_eq!(db.get_wallet_vtxo(id).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert!(db.get_fallback_snapshot().await.unwrap().unproven_spent_vtxos.contains(&id));
	assert_eq!(wallet.get_full_vtxo(id).await.unwrap().serialize().as_slice(), body);
	assert!(wallet.exit_mgr().get_exit_vtxo(id).await.is_none());
}

#[tokio::test]
async fn default_daemon_protects_owned_unpaid_principal_without_exiting_an_ordinary_spent_import() {
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run())).await
		.expect("the one bounded daemon/reopen/explicit-drain scenario must finish");
}

async fn run() {
	let ctx = TestContext::new("bark_sdk/offline_exit_recovery").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(48);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let gate = HistoryUnavailable::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let owner = Box::pin(ctx.bark_sdk("owner", &proxy).mnemonic(mnemonic.clone()).cfg(configure)
		.funded(sat(600_000)).boarded(sat(100_000)).boarded(sat(100_000)).create()).await;
	owner.stop_daemon_wait().await.unwrap();
	let recipient = Box::pin(ctx.bark_sdk("recipient", &srv).cfg(configure).create()).await;
	let original = owner.spendable_vtxos().await.unwrap();
	assert_eq!(original.len(), 2);
	let first_height = ctx.bitcoind().get_block_count().await;
	owner.send_arkoor_payment(&recipient.new_address().await.unwrap(), sat(100_000)).await.unwrap();
	recipient.sync().await;
	assert_eq!(recipient.balance().await.unwrap().total(), sat(100_000));
	let remaining = owner.spendable_vtxos().await.unwrap();
	assert_eq!(remaining.len(), 1);
	let owned = owner.get_full_vtxo(remaining[0].id()).await.unwrap();
	let historic = owner.get_full_vtxo(original.iter().find(|coin| coin.id() != owned.id()).unwrap().id()).await.unwrap();
	assert_ne!(historic.chain_anchor(), owned.chain_anchor());
	assert_eq!(owned.amount(), sat(100_000));
	assert_eq!(historic.amount(), sat(100_000));
	let owned_bytes = owned.serialize();
	let historic_bytes = historic.serialize();
	for coin in [&owned, &historic] {
		coin.validate(&ctx.bitcoind().await_transaction(coin.chain_anchor().txid).await).unwrap();
	}
	// Re-import a completed transfer's source while its signed spending branch is available.
	// It remains a spent control alongside the unpaid source.
	owner.dangerous_drop_vtxo(historic.id()).await.unwrap();
	gate.refuse.store(true, Ordering::SeqCst);
	owner.import_vtxo(&historic, ImportVtxoArgs::default()).await.unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	historic_is_blocked(&owner, &db, historic.id(), &historic_bytes).await;
	recipient.stop_daemon_wait().await.unwrap();
	drop(recipient);
	// Leave the watchman unfunded so it commits a payout decision without paying.
	// Recovery must retain the customer's original source until a chain payment exists.
	let now = ctx.bitcoind().get_block_count().await as u32;
	assert!(now < owned.expiry_height().to_u32());
	let expired = ctx.generate_blocks(owned.expiry_height().to_u32() + 1 - now).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let server_db = Db::connect(&srv.config().postgres).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let decided = server_db.read(async |tx| Ok(tx.query_opt(
				"SELECT id AS entitlement_id FROM expiry_entitlement WHERE vtxo_id=$1", &[&owned.id().to_string()],
			).await?)).await.unwrap().is_some();
			if decided { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual unpaid admission did not commit");
	owner.recover_vtxos([owned.id()], None).await.unwrap();
	assert!(gate.hits.load(Ordering::SeqCst) > 0);
	let snapshot = db.get_fallback_snapshot().await.unwrap();
	assert!(snapshot.adopted_payout_vtxos.is_empty());
	assert!(snapshot.awaiting_settlement.contains(&owned.id()));
	assert!(!snapshot.unproven_spent_vtxos.contains(&owned.id()));
	assert_eq!(owner.get_full_vtxo(owned.id()).await.unwrap().serialize(), owned_bytes);
	exact_principal(owner.balance().await.unwrap(), sat(100_000), true);
	assert!(ctx.bitcoind().sync_client().get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().is_some());
	let schedule = owner.exit_protection_schedule().await.unwrap().into_iter().find(|entry| entry.vtxo_id == owned.id()).unwrap();
	assert!(schedule.act_by.to_u32() <= expired.to_u32());
	let mut config = owner.config().clone();
	owner.stop_daemon_wait().await.unwrap();
	drop(owner);
	// Stop both operator daemons before testing the wallet's emergency exit.
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	config.daemon_manual_sync = false;
	let wallet = Box::pin(open_cached(&ctx, &mnemonic, config.clone())).await;
	// Advance the exit through the default daemon, supplying only blocks here.
	// Direct protection, refresh and sync calls would bypass the behavior under test.
	let started = Instant::now();
	let mut last_block = Instant::now();
	loop {
		historic_is_blocked(&wallet, &db, historic.id(), &historic_bytes).await;
		exact_principal(wallet.balance().await.unwrap(), sat(100_000), false);
		if let Some(exit) = db.get_exit_vtxo_entry(&owned.id()).await.unwrap() {
			let holder = db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state;
			assert!(holder == VtxoState::Exited || holder == (VtxoState::Locked { holder: Some(exit.movement_id.unwrap().into()) }),
				"only this exact native safety movement may reserve the still-owned source");
		}
		if wallet.exit_mgr().list_claimable().await.iter().any(|entry| entry.id() == owned.id()) { break; }
		assert!(started.elapsed() < Duration::from_secs(35), "true daemon did not make the retained owned claim available");
		assert!(ctx.bitcoind().get_block_count().await - first_height <= 80);
		if last_block.elapsed() >= Duration::from_secs(1) { ctx.generate_blocks(1).await; last_block = Instant::now(); }
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	let exit = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap();
	let movement = exit.movement_id.unwrap();
	assert_eq!(wallet.exit_mgr().get_exit_vtxo_ids().await, vec![owned.id()]);
	assert_eq!(db.get_movement_by_id(movement).await.unwrap().status, MovementStatus::Pending);
	let owned_state = db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state;
	assert_eq!(owned_state, VtxoState::Exited);
	wallet.stop_daemon_wait().await.unwrap();
	drop(wallet);
	let wallet = Box::pin(open_cached(&ctx, &mnemonic, config)).await;
	assert_eq!(db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap().movement_id, Some(movement));
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, owned_state);
	exact_principal(wallet.balance().await.unwrap(), sat(100_000), false);
	historic_is_blocked(&wallet, &db, historic.id(), &historic_bytes).await;
	let claimable = wallet.exit_mgr().list_claimable().await;
	assert_eq!(claimable.len(), 1);
	assert_eq!(claimable[0].id(), owned.id());
	let destination = ctx.bitcoind().get_new_address();
	let claim = wallet.exit_mgr().drain_exits(&claimable, &wallet, destination.clone(),
		Some(FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert_eq!(claim.input.len(), 1);
	assert_eq!(claim.input[0].previous_output, owned.point());
	assert_eq!(claim.output.len(), 1);
	assert_eq!(claim.output[0].script_pubkey, destination.script_pubkey());
	let previous = ctx.bitcoind().await_transaction(owned.point().txid).await;
	assert_eq!(previous.output[owned.point().vout as usize].value, sat(100_000));
	let fee = sat(100_000).checked_sub(claim.output[0].value).unwrap();
	assert!(fee > sat(0) && claim.output[0].value > sat(0));
	assert_eq!(claim.output[0].value + fee, sat(100_000));
	ctx.bitcoind().sync_client().send_raw_transaction(&claim).unwrap();
	ctx.generate_blocks(6).await;
	let core = ctx.bitcoind().sync_client();
	let info = core.custom_get_raw_transaction_info(claim.compute_txid(), None).unwrap().unwrap();
	assert!(info.confirmations.unwrap_or(0) >= 6);
	let block = info.blockhash.unwrap();
	assert_eq!(core.get_block_hash(core.get_block_header_info(&block).unwrap().height as u64).unwrap(), block);
	let paid = core.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(paid.confirmations >= 6);
	assert_eq!(paid.value, claim.output[0].value);
	assert_eq!(paid.script_pub_key.script().unwrap(), destination.script_pubkey());
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let retained = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap();
			if matches!(retained.state, ExitState::Claimed(done)
				if done.txid == claim.compute_txid() && done.block.hash == block
					&& u64::from(done.block.height.to_u32()) == core.get_block_header_info(&block).unwrap().height as u64)
				&& retained.movement_id == Some(movement)
				&& db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state == VtxoState::Exited
				&& db.get_movement_by_id(movement).await.unwrap().status == MovementStatus::Successful { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("default daemon did not reconcile the actual confirmed own claim");
	wallet.stop_daemon_wait().await.unwrap();
	exact_principal(wallet.balance().await.unwrap(), sat(0), false);
	historic_is_blocked(&wallet, &db, historic.id(), &historic_bytes).await;
	assert_eq!(wallet.get_full_vtxo(owned.id()).await.unwrap().serialize(), owned_bytes);
	let history = wallet.history().await.unwrap();
	assert!(history.iter().all(|entry| entry.subsystem.name != "abandon.fallback_payout"));
	let exits = history.iter().filter(|entry| entry.subsystem.name == "bark.exit").collect::<Vec<_>>();
	assert_eq!(exits.len(), 1);
	assert_eq!(exits[0].id, movement);
	assert_eq!(exits[0].status, MovementStatus::Successful);
	assert_eq!(exits[0].effective_balance.to_sat(), -100_000);
	assert!(core.get_tx_out(&historic.chain_anchor().txid, historic.chain_anchor().vout, Some(true)).unwrap().is_some(),
		"the genuine ordinary-spent historical root must not be exited");
	assert!(ctx.bitcoind().get_block_count().await - first_height <= 96);
}
