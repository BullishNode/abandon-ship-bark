//! One inaccessible native coin record must not suppress another coin's exit.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ark::{ProtocolEncoding, VtxoId};
use ark_testing::{sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use bark::{ExitProtectionIncomplete, OpenWalletArgs, Wallet, WalletSeed};
use bark::onchain::OnchainWallet;
use bark::persist::BarkPersister;
use bark::persist::adaptor::{partition, Query, QueryRange, Record, StorageAdaptor, StorageAdaptorWrapper};
use bark::persist::adaptor::filestore::FileStorageAdaptor;
use bark::vtxo::VtxoState;
use bitcoin::{FeeRate, Network};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};

#[derive(Clone, Default)]
struct ReadFault {
	coin: Arc<Mutex<Option<VtxoId>>>,
	hits: Arc<AtomicUsize>,
}

struct CoinReadGate {
	inner: FileStorageAdaptor,
	fault: ReadFault,
}

#[async_trait::async_trait]
impl StorageAdaptor for CoinReadGate {
	async fn put(&mut self, record: Record) -> anyhow::Result<()> {
		self.inner.put(record).await
	}

	async fn get(&self, partition: u8, pk: &[u8]) -> anyhow::Result<Option<Record>> {
		let unavailable = *self.fault.coin.lock().unwrap();
		if partition == partition::VTXO && unavailable.is_some_and(|coin| pk == coin.to_bytes()) {
			self.fault.hits.fetch_add(1, Ordering::SeqCst);
			return Err(std::io::Error::other("owned fixture: native coin point read unavailable").into());
		}
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

#[tokio::test]
async fn missing_coin_body_does_not_starve_a_healthy_coins_protective_exit() {
	tokio::time::timeout(Duration::from_secs(180), protect_with_one_unavailable_record()).await
		.expect("healthy coin protection and native claim must finish while another record is unavailable");
}

async fn protect_with_one_unavailable_record() {
	let ctx = TestContext::new("bark_sdk/qualification_protection_isolation").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(48);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).create().await;
	let first_height = ctx.bitcoind().get_block_count().await;
	let fault = ReadFault::default();
	let storage_path = ctx.datadir.join("owner.json");
	let database = Arc::new(StorageAdaptorWrapper::new(CoinReadGate {
		inner: FileStorageAdaptor::open(&storage_path).await.unwrap(), fault: fault.clone(),
	}));
	let network = Network::Regtest;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let seed = mnemonic.to_seed("");
	let onchain = OnchainWallet::load_or_create(network, seed, database.clone()).await.unwrap();
	let destination = onchain.fallback_script();
	let mut config = ctx.bark_default_cfg(&*srv, Some(ctx.bitcoind()));
	config.daemon_manual_sync = true;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
	let wallet = Wallet::open(network, WalletSeed::new_from_seed(network, &seed), config, OpenWalletArgs {
		persister: Some(database.clone()), onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))),
		fallback_spk: Some(destination), run_daemon: false, ..Default::default()
	}).await.unwrap();
	let address = wallet.onchain().unwrap().write().await.address().await.unwrap();
	ctx.bitcoind().fund_addr(address, sat(500_000)).await;
	ctx.generate_blocks(1).await;
	wallet.sync_onchain().await.unwrap();
	let mut originals = Vec::new();
	for _ in 0..2 {
		let board = wallet.board_amount(sat(100_000)).await.unwrap();
		assert_eq!(board.vtxos.len(), 1);
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
		wallet.chain().invalidate_caches().await;
		wallet.sync_pending_boards().await.unwrap();
		wallet.sync_onchain().await.unwrap();
		originals.push(wallet.get_full_vtxo(board.vtxos[0]).await.unwrap());
	}
	let healthy = &originals[0];
	let unavailable = &originals[1];
	assert_eq!(healthy.amount(), sat(100_000));
	assert_eq!(unavailable.amount(), sat(100_000));
	assert_ne!(healthy.chain_anchor(), unavailable.chain_anchor());
	let healthy_backing = ctx.bitcoind().await_transaction(healthy.chain_anchor().txid).await;
	assert_eq!(healthy_backing.output[healthy.chain_anchor().vout as usize].value, healthy.amount());
	let backing = ctx.bitcoind().await_transaction(unavailable.chain_anchor().txid).await
		.output[unavailable.chain_anchor().vout as usize].clone();
	assert_eq!(backing.value, unavailable.amount());
	let underlying = FileStorageAdaptor::open(&storage_path).await.unwrap();
	let retained = underlying.get(partition::VTXO, &unavailable.id().to_bytes()).await.unwrap().unwrap();
	let schedule = wallet.exit_protection_schedule().await.unwrap().into_iter()
		.find(|schedule| schedule.vtxo_id == healthy.id()).unwrap();
	assert!(schedule.act_by < healthy.expiry_height());
	assert!(wallet.onchain().unwrap().read().await.balance().await > sat(100_000),
		"the healthy exit must have genuine separate fee liquidity");
	srv.stop().await.unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	assert!(tip < schedule.act_by.to_u32());
	ctx.generate_blocks(schedule.act_by.to_u32() - tip).await;
	wallet.chain().invalidate_caches().await;
	*fault.coin.lock().unwrap() = Some(unavailable.id());
	let report = wallet.exit_protection_report().await.unwrap();
	assert!(report.schedules.iter().any(|schedule| schedule.vtxo_id == healthy.id()));
	assert!(!report.schedules.iter().any(|schedule| schedule.vtxo_id == unavailable.id()));
	assert!(report.issues.iter().any(|issue| issue.vtxo_id == Some(unavailable.id())
		&& issue.operation == "derive_schedule" && !issue.detail.is_empty()));
	let strict = wallet.exit_protection_schedule().await.unwrap_err();
	assert!(strict.downcast_ref::<ExitProtectionIncomplete>().is_some(), "strict query cannot hide partial scheduling");
	for attempt in 0..2 {
		let error = wallet.protect_expiring_vtxos().await.expect_err("B's failure must remain actionable");
		let partial = error.downcast_ref::<ExitProtectionIncomplete>().expect("structured partial progress");
		assert!(partial.report.issues.iter().any(|issue| issue.vtxo_id == Some(unavailable.id())
			&& issue.operation == "derive_schedule" && !issue.detail.is_empty()));
		if attempt == 0 { assert!(partial.outcome.exits_started.contains(&healthy.id())); }
	}
	assert!(fault.hits.load(Ordering::SeqCst) >= 2, "the repeated per-coin read fault must actually be reached");
	let exit = wallet.exit_mgr().get_exit_vtxo(healthy.id()).await
		.expect("B's body read failure starved A's public protection path");
	assert_eq!(database.get_wallet_vtxo(healthy.id()).await.unwrap().unwrap().state,
		VtxoState::Locked { holder: Some(exit.movement_id().unwrap().into()) });
	assert_eq!(wallet.get_full_vtxo(healthy.id()).await.unwrap().serialize(), healthy.serialize());
	let root_step = healthy.transactions().find(|step|
		step.tx.input.iter().any(|input| input.previous_output == healthy.chain_anchor())).unwrap();
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
			if ctx.bitcoind().sync_client().custom_get_raw_transaction_info(root_step.tx.compute_txid(), None)
				.unwrap().is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("A's signed protective path did not actually reach Core");
	assert_eq!(ctx.bitcoind().await_transaction(root_step.tx.compute_txid()).await, root_step.tx);
	assert!(ctx.bitcoind().get_block_count().await < u64::from(healthy.expiry_height().to_u32()),
		"A protection must reach Core before its original expiry");
	let mut ready = None;
	for _ in 0..16 {
		wallet.sync_onchain().await.unwrap();
		wallet.exit_mgr().progress_exits_with_cpfp(&wallet, None).await.unwrap();
		let claimable = wallet.exit_mgr().list_claimable().await;
		assert!(claimable.iter().all(|coin| coin.id() == healthy.id()));
		if let Some(coin) = claimable.into_iter().next() { ready = Some(coin); break; }
		ctx.generate_blocks(1).await;
		wallet.chain().invalidate_caches().await;
	}
	let ready = ready.expect("the healthy actual signed exit never became claimable");
	let claim_destination = ctx.bitcoind().get_new_address();
	let claim = wallet.exit_mgr().drain_exits(&[ready], &wallet, claim_destination.clone(),
		Some(FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert_eq!(claim.input.len(), 1);
	assert_eq!(claim.input[0].previous_output, healthy.point());
	assert_eq!(claim.output.len(), 1);
	assert_eq!(claim.output[0].script_pubkey, claim_destination.script_pubkey());
	let previous = ctx.bitcoind().await_transaction(healthy.point().txid).await;
	let input_value = previous.output[healthy.point().vout as usize].value;
	assert_eq!(input_value, healthy.amount());
	let claim_fee = input_value.checked_sub(claim.output[0].value).unwrap();
	assert!(claim_fee > sat(0) && claim.output[0].value > sat(0));
	assert_eq!(healthy.amount(), claim.output[0].value + claim_fee);
	ctx.bitcoind().sync_client().send_raw_transaction(&claim).unwrap();
	ctx.generate_blocks(6).await;
	let core = ctx.bitcoind().sync_client();
	let info = core.custom_get_raw_transaction_info(claim.compute_txid(), None).unwrap().unwrap();
	assert!(info.confirmations.unwrap_or(0) >= 6);
	let hash = info.blockhash.unwrap();
	assert_eq!(core.get_block_hash(core.get_block_header_info(&hash).unwrap().height as u64).unwrap(), hash);
	let paid = core.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(paid.confirmations >= 6);
	assert_eq!(paid.value, claim.output[0].value);
	assert_eq!(paid.script_pub_key.script().unwrap(), claim_destination.script_pubkey());
	// B's read error cannot be mistaken for a terminal payment or lost principal.
	assert!(wallet.get_full_vtxo(unavailable.id()).await.is_err());
	let debt = wallet.spendable_vtxos().await.unwrap();
	let debt = debt.iter().find(|coin| coin.id() == unavailable.id()).unwrap();
	assert_eq!(debt.amount(), sat(100_000));
	assert_eq!(debt.state, VtxoState::Spendable);
	assert!(wallet.exit_mgr().get_exit_vtxo(unavailable.id()).await.is_none());
	let reopened = FileStorageAdaptor::open(&storage_path).await.unwrap();
	assert_eq!(reopened.get(partition::VTXO, &unavailable.id().to_bytes()).await.unwrap().unwrap(), retained);
	let independent = StorageAdaptorWrapper::new(FileStorageAdaptor::open(&storage_path).await.unwrap());
	assert_eq!(independent.get_full_vtxo(unavailable.id()).await.unwrap().unwrap().serialize(), unavailable.serialize());
	let pending = core.get_tx_out(&unavailable.chain_anchor().txid, unavailable.chain_anchor().vout, Some(true))
		.unwrap().unwrap();
	assert_eq!(pending.value, backing.value);
	assert_eq!(pending.script_pub_key.script().unwrap(), backing.script_pubkey);
	assert!(database.get_fallback_snapshot().await.unwrap().entitlements.is_empty(),
		"no manufactured entitlement or payout may explain preserved principal");
	*fault.coin.lock().unwrap() = None;
	assert_eq!(wallet.get_full_vtxo(unavailable.id()).await.unwrap().serialize(), unavailable.serialize());
	assert_eq!(database.get_wallet_vtxo(unavailable.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
	assert!(ctx.bitcoind().get_block_count().await - first_height <= 96);
}
