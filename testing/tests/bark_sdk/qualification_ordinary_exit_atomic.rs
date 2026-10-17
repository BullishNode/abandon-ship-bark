//! A genuine complete ordinary-spend proof must publish before a concurrent
//! automatic exit can reserve its source. No exit transaction is broadcast.

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::Duration;

use ark::{ProtocolEncoding, VtxoId};
use ark_testing::{sat, TestContext};
use ark_testing::daemon::captaind::{self, ArkClient};
use bark::{ExitProtectionIncomplete, OpenWalletArgs, Wallet, WalletSeed};
use bark::lock_manager::{LockGuard, LockManager, memory::MemoryLockManager};
use bark::onchain::OnchainWallet;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::persist::adaptor::{partition, Query, QueryRange, Record, StorageAdaptor, StorageAdaptorWrapper};
use bark::persist::adaptor::filestore::FileStorageAdaptor;
use bark::persist::models::SerdeVtxo;
use bark::exit::{ExitState, ExitTxStatus};
use bark::movement::MovementStatus;
use bark::vtxo::VtxoState;
use bitcoin::Network;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::BitcoinRpcExt;
use server_rpc::protos;

#[derive(Debug, Default)]
struct PublicationPause {
	armed: AtomicBool,
	entered: tokio::sync::Notify,
	release: tokio::sync::Notify,
}

#[derive(Debug)]
struct PublicationLockManager {
	inner: MemoryLockManager,
	pause: Arc<PublicationPause>,
}

#[async_trait::async_trait]
impl LockManager for PublicationLockManager {
	async fn try_lock(&self, key: &str) -> Option<Box<dyn LockGuard>> {
		self.inner.try_lock(key).await
	}

	async fn lock(&self, key: &str, timeout: Duration) -> anyhow::Result<Box<dyn LockGuard>> {
		if key.ends_with(".fallback-history") && self.pause.armed.swap(false, Ordering::SeqCst) {
			// The real complete proof was returned and validated before this
			// publication lock request. Pause BEFORE the named lock acquisition,
			// leaving it available to the other task's normal protection reads.
			self.pause.entered.notify_one();
			self.pause.release.notified().await;
		}
		self.inner.lock(key, timeout).await
	}
}

#[derive(Clone)]
struct ArmPublication {
	pause: Arc<PublicationPause>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for ArmPublication {
	async fn get_vtxo_spend_proof(&self, upstream: &mut ArkClient, request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		let response = upstream.get_vtxo_spend_proof(request).await?.into_inner();
		assert!(!response.vtxos.is_empty(), "only authentic complete stored branches arm this gate");
		self.pause.armed.store(true, Ordering::SeqCst);
		Ok(response)
	}
}

fn manual(config: &mut bark::Config) {
	config.daemon_manual_sync = true;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
}

#[tokio::test]
async fn verified_ordinary_checkpoint_serializes_with_automatic_exit_reservation() {
	tokio::time::timeout(Duration::from_secs(180), ordinary_checkpoint_serializes()).await
		.expect("ordinary proof and automatic reservation must complete without a lock inversion");
}

async fn ordinary_checkpoint_serializes() {
	let ctx = TestContext::new("bark_sdk/qualification_ordinary_exit_atomic").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(48);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let pause = Arc::new(PublicationPause::default());
	let proxy = srv.start_proxy_no_mailbox(ArmPublication { pause: pause.clone() }).await;
	let sender = ctx.bark_sdk("sender", &srv).cfg(manual).boarded(sat(100_000)).create().await;
	let mnemonic = bip39::Mnemonic::from_str(tokio::fs::read_to_string(ctx.datadir.join("sender/mnemonic"))
		.await.unwrap().trim()).unwrap();
	let original = sender.spendable_vtxos().await.unwrap().remove(0);
	let full = sender.get_full_vtxo(original.id()).await.unwrap();
	let stale = ctx.bark_sdk("stale", &proxy).cfg(manual).mnemonic(mnemonic.clone()).create().await;
	assert_eq!(stale.spendable_vtxos().await.unwrap()[0].id(), original.id());
	drop(stale);
	let db = Arc::new(SqliteClient::open(ctx.datadir.join("stale/db.sqlite")).unwrap());
	let onchain = OnchainWallet::load_or_create(Network::Regtest, mnemonic.to_seed(""), db.clone()).await.unwrap();
	let script = onchain.fallback_script();
	let mut cfg = ctx.bark_default_cfg(&proxy, Some(ctx.bitcoind()));
	manual(&mut cfg);
	let stale = Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), cfg,
		OpenWalletArgs { persister: Some(db.clone()), lock_manager: Some(Box::new(PublicationLockManager {
			inner: MemoryLockManager::new(), pause: pause.clone(),
		})), onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))),
			fallback_spk: Some(script), run_daemon: false, ..Default::default() }).await.unwrap();
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(manual).create().await;
	sender.send_arkoor_payment(&recipient.new_address().await.unwrap(), sat(100_000)).await.unwrap();
	recipient.sync().await;
	assert_eq!(recipient.spendable_vtxos().await.unwrap().iter().map(|coin| coin.amount())
		.sum::<bitcoin::Amount>(), sat(100_000), "the ordinary transfer actually completed");
	let schedule = stale.exit_protection_schedule().await.unwrap().into_iter()
		.find(|schedule| schedule.vtxo_id == original.id()).unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	assert!(tip < schedule.act_by.to_u32());
	ctx.generate_blocks(schedule.act_by.to_u32() - tip).await;
	stale.chain().invalidate_caches().await;
	let id = original.id();
	let adopter = stale.clone();
	let adoption = tokio::spawn(async move { adopter.recover_vtxos([id], None).await });
	tokio::time::timeout(Duration::from_secs(10), pause.entered.notified()).await
		.expect("actual signed-spend proof did not reach its publication checkpoint");
	let protector = stale.clone();
	let mut protection = tokio::spawn(async move { protector.protect_expiring_vtxos().await });
	assert!(tokio::time::timeout(Duration::from_millis(250), &mut protection).await.is_err(),
		"automatic reservation must wait for the in-flight genuine proof publication");
	pause.release.notify_one();
	tokio::time::timeout(Duration::from_secs(10), adoption).await.unwrap().unwrap().unwrap();
	let outcome = tokio::time::timeout(Duration::from_secs(10), protection).await.unwrap().unwrap();
	match outcome {
		Ok(outcome) => assert!(outcome.exits_started.is_empty()),
		Err(error) => assert!(error.downcast_ref::<ExitProtectionIncomplete>().unwrap().outcome.exits_started.is_empty()),
	}
	assert!(db.get_fallback_snapshot().await.unwrap().verified_ordinary_spends.contains(&id));
	assert_eq!(db.get_wallet_vtxo(id).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(db.get_exit_vtxo_entry(&id).await.unwrap().is_none(), "no native exit intent/holder may be created after the proof");
	assert!(stale.history().await.unwrap().iter().all(|movement| movement.subsystem.name != "bark.exit"));
	assert_eq!(stale.get_full_vtxo(id).await.unwrap().serialize(), full.serialize());
	assert_eq!(stale.balance().await.unwrap().total(), sat(0));
}

/// A local storage fault rejects only the exact source's Spent write AFTER
/// genuine immutable proof storage. No substitute record or status is written.
#[derive(Clone, Default)]
struct RetirementFault {
	coin: Arc<Mutex<Option<VtxoId>>>,
	hits: Arc<AtomicUsize>,
}

struct RetirementWriteGate {
	inner: FileStorageAdaptor,
	fault: RetirementFault,
}

#[async_trait::async_trait]
impl StorageAdaptor for RetirementWriteGate {
	async fn put(&mut self, record: Record) -> anyhow::Result<()> {
		if record.partition == partition::VTXO {
			let coin: SerdeVtxo = serde_json::from_slice(&record.data)?;
			let mut fault = self.fault.coin.lock().unwrap();
			if *fault == Some(coin.vtxo.id()) && coin.current_state() == Some(&VtxoState::Spent) {
				*fault = None;
				self.fault.hits.fetch_add(1, Ordering::SeqCst);
				return Err(std::io::Error::other("owned fixture: exact ordinary retirement write unavailable").into());
			}
		}
		self.inner.put(record).await
	}

	async fn get(&self, partition: u8, pk: &[u8]) -> anyhow::Result<Option<Record>> {
		self.inner.get(partition, pk).await
	}

	async fn delete(&mut self, partition: u8, pk: &[u8]) -> anyhow::Result<Option<Record>> {
		self.inner.delete(partition, pk).await
	}

	async fn query_sorted<R: QueryRange>(&self, query: Query<R>) -> anyhow::Result<Vec<Record>> {
		self.inner.query_sorted(query).await
	}

	async fn get_all(&self, partition: u8) -> anyhow::Result<Vec<Record>> {
		self.inner.get_all(partition).await
	}
}

type RetainedDatabase = StorageAdaptorWrapper<RetirementWriteGate>;

// Keep debug fixture poll frames small without changing production stack size.
#[inline(never)]
fn create_reservation_wallet<'a>(builder: ark_testing::context::builders::BarkSdkBuilder<'a>)
	-> Pin<Box<dyn Future<Output = Wallet> + 'a>>
{
	Box::pin(builder.create())
}

#[inline(never)]
fn open_retained_wallet<'a>(
	mnemonic: &'a bip39::Mnemonic, cfg: bark::Config, database: Arc<RetainedDatabase>,
) -> Pin<Box<dyn Future<Output = Wallet> + 'a>> {
	Box::pin(async move {
		let onchain = OnchainWallet::load_or_create(Network::Regtest, mnemonic.to_seed(""), database.clone()).await.unwrap();
		let script = onchain.fallback_script();
		Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, mnemonic), cfg,
			OpenWalletArgs { persister: Some(database), onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))),
				fallback_spk: Some(script), run_daemon: false, ..Default::default() }).await.unwrap()
	})
}

#[tokio::test]
async fn ordinary_proof_retires_an_earlier_own_safety_reservation_without_touching_healthy_sibling() {
	tokio::time::timeout(Duration::from_secs(180), reserve_before_actual_ordinary_proof(false)).await
		.expect("reserve-first exact-holder retirement must finish without broadcasting");
}

#[tokio::test]
async fn ordinary_proof_checkpoint_reopens_without_relocking_its_retained_safety_intent() {
	tokio::time::timeout(Duration::from_secs(180), reserve_before_actual_ordinary_proof(true)).await
		.expect("durable proof plus failed retirement must recover after reopen without broadcasting");
}

async fn reserve_before_actual_ordinary_proof(interrupt_retirement: bool) {
	let name = if interrupt_retirement { "bark_sdk/ordinary_exit_checkpoint_reopen" }
		else { "bark_sdk/ordinary_exit_reserve_first" };
	let ctx = TestContext::new(name).await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(48);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let sender = create_reservation_wallet(ctx.bark_sdk("sender", &srv).cfg(manual)
		.boarded(sat(100_000)).boarded(sat(100_000))).await;
	let original = sender.spendable_vtxos().await.unwrap();
	assert_eq!(original.len(), 2);
	let mnemonic = bip39::Mnemonic::from_str(tokio::fs::read_to_string(ctx.datadir.join("sender/mnemonic"))
		.await.unwrap().trim()).unwrap();
	let fault = RetirementFault::default();
	let storage_path = ctx.datadir.join("retained.json");
	let database = Arc::new(StorageAdaptorWrapper::new(RetirementWriteGate {
		inner: FileStorageAdaptor::open(&storage_path).await.unwrap(), fault: fault.clone(),
	}));
	let mut cfg = ctx.bark_default_cfg(&*srv, Some(ctx.bitcoind()));
	manual(&mut cfg);
	let stale = open_retained_wallet(&mnemonic, cfg.clone(), database.clone()).await;
	assert_eq!(stale.spendable_vtxos().await.unwrap().iter().map(|coin| coin.amount())
		.sum::<bitcoin::Amount>(), sat(200_000), "the old device genuinely restored both positive sources before any send");
	let recipient = create_reservation_wallet(ctx.bark_sdk("recipient", &srv).cfg(manual)).await;
	let act_by = stale.exit_protection_schedule().await.unwrap().iter().map(|schedule| schedule.act_by)
		.max().unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	assert!(tip < act_by.to_u32());
	assert!(original.iter().all(|coin| act_by < coin.expiry_height()));
	ctx.generate_blocks(act_by.to_u32() - tip).await;
	stale.chain().invalidate_caches().await;
	let reserved = stale.protect_expiring_vtxos().await.unwrap();
	assert_eq!(reserved.exits_started.len(), 2);
	for coin in &original {
		assert!(reserved.exits_started.contains(&coin.id()));
		let exit = database.get_exit_vtxo_entry(&coin.id()).await.unwrap().unwrap();
		assert!(matches!(exit.state, ExitState::Start(_)));
		assert_eq!(database.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state,
			VtxoState::Locked { holder: Some(exit.movement_id.unwrap().into()) });
	}
	// This device has not broadcast an exit; the other genuine owner device
	// completes a normal authorized transfer while its old copy remains locked.
	sender.send_arkoor_payment(&recipient.new_address().await.unwrap(), sat(100_000)).await.unwrap();
	let remaining = sender.spendable_vtxos().await.unwrap();
	assert_eq!(remaining.len(), 1);
	let healthy = original.iter().find(|coin| coin.id() == remaining[0].id()).unwrap();
	let spent = original.iter().find(|coin| coin.id() != healthy.id()).unwrap();
	let full = stale.get_full_vtxo(spent.id()).await.unwrap();
	let spent_intent = database.get_exit_vtxo_entry(&spent.id()).await.unwrap().unwrap();
	let healthy_intent = database.get_exit_vtxo_entry(&healthy.id()).await.unwrap().unwrap();
	let spent_movement = spent_intent.movement_id.unwrap();
	let healthy_movement = healthy_intent.movement_id.unwrap();
	let healthy_holder = VtxoState::Locked { holder: Some(healthy_movement.into()) };
	recipient.sync().await;
	assert_eq!(recipient.spendable_vtxos().await.unwrap().iter().map(|coin| coin.amount())
		.sum::<bitcoin::Amount>(), sat(100_000));
	if interrupt_retirement { *fault.coin.lock().unwrap() = Some(spent.id()); }
	let adoption = stale.recover_vtxos([spent.id()], None).await;
	if interrupt_retirement {
		assert!(adoption.is_err(), "the exact post-checkpoint retirement fault must be surfaced");
		assert_eq!(fault.hits.load(Ordering::SeqCst), 1);
		assert_eq!(database.get_wallet_vtxo(spent.id()).await.unwrap().unwrap().state,
			VtxoState::Locked { holder: Some(spent_movement.into()) });
		assert_eq!(database.get_movement_by_id(spent_movement).await.unwrap().status, MovementStatus::Pending);
	} else {
		adoption.unwrap();
		assert_eq!(database.get_wallet_vtxo(spent.id()).await.unwrap().unwrap().state, VtxoState::Spent);
		let canceled = database.get_movement_by_id(spent_movement).await.unwrap();
		assert_eq!(canceled.status, MovementStatus::Canceled);
		assert_eq!(canceled.effective_balance.to_sat(), 0);
	}
	assert!(database.get_fallback_snapshot().await.unwrap().verified_ordinary_spends.contains(&spent.id()),
		"only the actual complete signed transfer supplied the durable fact");
	assert_eq!(database.get_wallet_vtxo(healthy.id()).await.unwrap().unwrap().state, healthy_holder);
	assert_eq!(database.get_movement_by_id(healthy_movement).await.unwrap().status, MovementStatus::Pending);
	assert_eq!(database.get_exit_vtxo_entries().await.unwrap().len(), 2);
	drop(stale);
	drop(database);
	let database = Arc::new(StorageAdaptorWrapper::new(RetirementWriteGate {
		inner: FileStorageAdaptor::open(&storage_path).await.unwrap(), fault: RetirementFault::default(),
	}));
	let reopened = open_retained_wallet(&mnemonic, cfg, database.clone()).await;
	srv.stop().await.unwrap();
	let status = reopened.exit_mgr().progress_exits(&reopened).await.unwrap().unwrap();
	let source_status = status.iter().find(|entry| entry.vtxo_id == spent.id()).unwrap();
	if interrupt_retirement {
		assert!(source_status.error.is_some(), "a durable proof with unfinished holder retirement must pause rather than reacquire/progress");
		assert!(matches!(source_status.state, ExitState::Start(_)));
		assert_eq!(database.get_wallet_vtxo(spent.id()).await.unwrap().unwrap().state,
			VtxoState::Locked { holder: Some(spent_movement.into()) });
	} else {
		assert!(source_status.error.is_none());
		assert!(matches!(source_status.state, ExitState::VtxoAlreadySpent(_)));
		assert_eq!(database.get_wallet_vtxo(spent.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	}
	let healthy_status = status.iter().find(|entry| entry.vtxo_id == healthy.id()).unwrap();
	assert!(healthy_status.error.is_none(), "the spent source must not starve its healthy sibling");
	let ExitState::Processing(processing) = &healthy_status.state else { panic!("healthy sibling did not prepare its actual signed exit"); };
	assert!(processing.transactions.iter().any(|tx| matches!(tx.status, ExitTxStatus::AwaitingCpfpBroadcast)));
	assert_eq!(database.get_wallet_vtxo(healthy.id()).await.unwrap().unwrap().state, healthy_holder);
	// Cached genuine proof completes exact-holder retirement even with Ark offline.
	tokio::time::timeout(Duration::from_secs(20), reopened.sync()).await.unwrap();
	reopened.exit_mgr().progress_exits(&reopened).await.unwrap();
	let source = database.get_exit_vtxo_entry(&spent.id()).await.unwrap().unwrap();
	assert_eq!(source.movement_id, Some(spent_movement), "recovery must not create a second intent");
	assert!(matches!(source.state, ExitState::VtxoAlreadySpent(_)));
	assert_eq!(database.get_wallet_vtxo(spent.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	let canceled = database.get_movement_by_id(spent_movement).await.unwrap();
	assert_eq!(canceled.status, MovementStatus::Canceled);
	assert_eq!(canceled.effective_balance.to_sat(), 0);
	let healthy_record = database.get_wallet_vtxo(healthy.id()).await.unwrap().unwrap();
	assert_eq!(healthy_record.state, healthy_holder);
	assert_eq!(healthy_record.amount(), sat(100_000));
	assert_eq!(database.get_exit_vtxo_entry(&healthy.id()).await.unwrap().unwrap().movement_id, Some(healthy_movement));
	assert_eq!(database.get_exit_vtxo_entries().await.unwrap().len(), 2);
	assert_eq!(reopened.get_full_vtxo(spent.id()).await.unwrap().serialize(), full.serialize());
	assert!(reopened.spendable_vtxos().await.unwrap().is_empty());
	assert!(reopened.balance().await.unwrap().total() <= sat(100_000), "retained old principal must not be added beside the actual remaining source");
	let history = reopened.history().await.unwrap();
	let intents = history.iter().filter(|movement| movement.subsystem.name == "bark.exit").collect::<Vec<_>>();
	assert_eq!(intents.len(), 2);
	assert_eq!(intents.iter().map(|movement| movement.effective_balance.to_sat()).sum::<i64>(), -100_000,
		"the canceled old intent contributes no second principal debit");
	for coin in &original {
		let retained = reopened.get_full_vtxo(coin.id()).await.unwrap();
		for step in retained.transactions() {
			if step.tx.compute_txid() == retained.chain_anchor().txid { continue; }
			assert!(ctx.bitcoind().sync_client().custom_get_raw_transaction_info(step.tx.compute_txid(), None)
				.unwrap().is_none(), "this fixture must not publish any exit transaction");
		}
	}
}
