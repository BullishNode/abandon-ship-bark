//! A separately seeded app wallet crosses the real public callback boundary.
//! This is registration/restore/onchain receipt coverage; an actual fallback
//! payout test must also run once the backend payout path is available.

use std::sync::Arc;
use std::time::Duration;

use bark::chain::ChainSource;
use bark::onchain::{CpfpError, MakeCpfpFees, OnchainWallet, OnchainWalletTrait};
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::{OpenWalletArgs, Wallet, WalletSeed};
use bitcoin::{Address, Amount, FeeRate, Network, OutPoint, Psbt, Script, Transaction, Txid};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use tokio::sync::RwLock;

use ark_testing::{sat, TestContext};

/// The application owns its BDK keys and persistence. The SDK receives only
/// this trait object, whose concrete type is deliberately not OnchainWallet.
struct AppBip84Wallet(OnchainWallet);

#[async_trait::async_trait]
impl OnchainWalletTrait for AppBip84Wallet {
	async fn balance(&self) -> Amount { OnchainWalletTrait::balance(&self.0).await }
	async fn address(&mut self) -> anyhow::Result<Address> { OnchainWalletTrait::address(&mut self.0).await }
	async fn sync(&mut self, chain: &ChainSource) -> anyhow::Result<()> { OnchainWalletTrait::sync(&mut self.0, chain).await }
	async fn is_mine(&self, spk: &Script) -> anyhow::Result<bool> { OnchainWalletTrait::is_mine(&self.0, spk).await }
	async fn register_tx(&mut self, tx: &Transaction) -> anyhow::Result<()> { OnchainWalletTrait::register_tx(&mut self.0, tx).await }
	async fn evict_tx(&mut self, txid: Txid) -> anyhow::Result<()> { OnchainWalletTrait::evict_tx(&mut self.0, txid).await }
	async fn prepare_tx(&mut self, destinations: &[(Address, Amount)], fee_rate: FeeRate) -> anyhow::Result<Psbt> {
		OnchainWalletTrait::prepare_tx(&mut self.0, destinations, fee_rate).await
	}
	async fn prepare_drain_tx(&mut self, destination: Address, fee_rate: FeeRate) -> anyhow::Result<Psbt> {
		OnchainWalletTrait::prepare_drain_tx(&mut self.0, destination, fee_rate).await
	}
	async fn sign_psbt(&mut self, psbt: Psbt) -> anyhow::Result<Psbt> { OnchainWalletTrait::sign_psbt(&mut self.0, psbt).await }
	async fn finish_psbt(&mut self, psbt: Psbt) -> anyhow::Result<Psbt> { OnchainWalletTrait::finish_psbt(&mut self.0, psbt).await }
	async fn make_signed_p2a_cpfp(&mut self, tx: &Transaction, fees: MakeCpfpFees) -> Result<Transaction, CpfpError> {
		OnchainWalletTrait::make_signed_p2a_cpfp(&mut self.0, tx, fees).await
	}
	async fn store_signed_p2a_cpfp(&mut self, tx: &Transaction) -> Result<(), CpfpError> {
		OnchainWalletTrait::store_signed_p2a_cpfp(&mut self.0, tx).await
	}
}

#[tokio::test]
async fn fallback_external_bip84_wallet_registers_and_restores_without_ark_seed_assumptions() {
	let ctx = TestContext::new("bark_sdk/fallback_external_bip84_wallet").await;
	let srv = ctx.captaind("server").create().await;
	let network = Network::Regtest;
	let ark_seed = [33; 64];
	let app_seed = [55; 64];
	let app_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("app.sqlite")).unwrap());
	let app_bdk = OnchainWallet::load_or_create(network, app_seed, app_db).await.unwrap();
	let payout = app_bdk.fallback_script();
	assert!(payout.is_p2wpkh());
	let app: Arc<RwLock<dyn OnchainWalletTrait>> = Arc::new(RwLock::new(AppBip84Wallet(app_bdk)));
	let mut config = ctx.bark_default_cfg(&*srv, Some(ctx.bitcoind()));
	config.daemon_manual_sync = true;
	let ark_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("ark.sqlite")).unwrap());
	let wallet = Wallet::open(network, WalletSeed::new_from_seed(network, &ark_seed), config.clone(), OpenWalletArgs {
		persister: Some(ark_db.clone()), onchain: Some(app.clone()), fallback_spk: Some(payout.clone()),
		run_daemon: false, ..Default::default()
	}).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	assert_eq!(record.spk, payout);
	assert_eq!(record.account_xpub, wallet.vtxo_xpub());
	assert_eq!(wallet.new_address_with_index().await.unwrap().1, 1);
	let address = Address::from_script(&payout, network).unwrap();
	ctx.bitcoind().fund_addr(address, sat(20_000)).await;
	ctx.generate_blocks(1).await;
	app.write().await.sync(wallet.chain()).await.unwrap();
	assert_eq!(app.read().await.balance().await, sat(20_000));

	// A changed explicit app choice never overwrites an authenticated cached
	// binding, even when the new script is another standard address.
	let changed = app.write().await.address().await.unwrap().script_pubkey();
	assert_ne!(changed, payout);
	let mismatch = Wallet::open(network, WalletSeed::new_from_seed(network, &ark_seed), config.clone(), OpenWalletArgs {
		persister: Some(ark_db), onchain: Some(app.clone()), fallback_spk: Some(changed),
		run_daemon: false, ..Default::default()
	}).await;
	assert!(mismatch.is_err());
	drop(wallet);
	drop(app);

	// Both app and Ark databases are new. Neither concrete-wallet detection
	// nor deriving an onchain address from the Ark seed can make this pass.
	let app_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("restored-app.sqlite")).unwrap());
	let restored_bdk = OnchainWallet::load_or_create(network, app_seed, app_db).await.unwrap();
	assert_eq!(restored_bdk.fallback_script(), payout);
	let restored_app: Arc<RwLock<dyn OnchainWalletTrait>> = Arc::new(RwLock::new(AppBip84Wallet(restored_bdk)));
	let restored_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("restored-ark.sqlite")).unwrap());
	let restored = Wallet::open(network, WalletSeed::new_from_seed(network, &ark_seed), config, OpenWalletArgs {
		persister: Some(restored_db), onchain: Some(restored_app.clone()), fallback_spk: Some(payout),
		run_daemon: false, ..Default::default()
	}).await.unwrap();
	assert_eq!(restored.fallback_destination().await.unwrap(), record);
	restored_app.write().await.sync(restored.chain()).await.unwrap();
	assert_eq!(restored_app.read().await.balance().await, sat(20_000));
}

/// Full acceptance uses a real Ark expiry payout, never the ordinary deposit
/// in the registration-only fixture above. Both app databases are new on
/// restore; the only secret inputs are the original distinct Ark/app seeds.
#[tokio::test]
async fn fallback_external_bip84_actual_payout_seed_restore_and_confirmed_spend() {
	let ctx = TestContext::new("bark_sdk/fallback_external_bip84_actual_payout").await;
	let srv = ctx.captaind("server").no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24))
		.watchmand().create().await;
	let network = Network::Regtest;
	let ark_seed = [71; 64];
	let app_seed = [92; 64];
	let app_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("app.sqlite")).unwrap());
	let app_bdk = OnchainWallet::load_or_create(network, app_seed, app_db).await.unwrap();
	let script = app_bdk.fallback_script();
	assert!(script.is_p2wpkh());
	let app: Arc<RwLock<dyn OnchainWalletTrait>> = Arc::new(RwLock::new(AppBip84Wallet(app_bdk)));
	let mut config = ctx.bark_default_cfg(&*srv, Some(ctx.bitcoind()));
	config.daemon_manual_sync = true;
	let ark_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("ark.sqlite")).unwrap());
	let wallet = Wallet::open(network, WalletSeed::new_from_seed(network, &ark_seed), config.clone(), OpenWalletArgs {
		persister: Some(ark_db), onchain: Some(app.clone()), fallback_spk: Some(script.clone()),
		run_daemon: false, ..Default::default()
	}).await.unwrap();
	let registration = wallet.fallback_destination().await.unwrap();
	let funding_address = app.write().await.address().await.unwrap();
	assert_ne!(funding_address.script_pubkey(), script);
	ctx.bitcoind().fund_addr(funding_address, sat(200_000)).await;
	ctx.generate_blocks(1).await;
	app.write().await.sync(wallet.chain()).await.unwrap();
	let board = wallet.board_amount(sat(100_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(ark_testing::constants::BOARD_CONFIRMATIONS).await;
	wallet.sync_pending_boards().await.unwrap();
	assert_eq!(board.vtxos.len(), 1);
	let source = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
	assert_eq!(wallet.spendable_vtxos().await.unwrap().len(), 1);
	let expected = source.amount() - sat(1_000);

	// No wallet opens, syncs or maintenance occur after abandonment.
	drop(wallet);
	drop(app);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	assert!(height < source.expiry_height().to_u32());
	let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;

	// Core, not a synthetic receipt or database insert, identifies the real
	// customer output of the running background payout path.
	let (parent, output_index) = tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				let tx = ctx.bitcoind().await_transaction(txid).await;
				for (index, output) in tx.output.iter().enumerate() {
					if output.script_pubkey == script && output.value == expected {
						return (tx, index as u32);
					}
				}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("native runtime did not publish an actual fallback payout");
	assert_eq!(parent.version.0, 3);
	let point = OutPoint::new(parent.compute_txid(), output_index);
	let confirmed = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	let confirmed_payout = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true))
		.unwrap().unwrap();
	assert!(confirmed_payout.confirmations >= 6, "the external app needs an actually confirmed payout");
	assert_eq!(confirmed_payout.value, expected);

	let restored_app_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("restored-app.sqlite")).unwrap());
	let restored_bdk = OnchainWallet::load_or_create(network, app_seed, restored_app_db).await.unwrap();
	assert_eq!(restored_bdk.fallback_script(), script);
	let restored_impl = Arc::new(RwLock::new(AppBip84Wallet(restored_bdk)));
	let restored_app: Arc<RwLock<dyn OnchainWalletTrait>> = restored_impl.clone();
	let restored_ark_db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(ctx.datadir.join("restored-ark.sqlite")).unwrap());
	let restored = Wallet::open(network, WalletSeed::new_from_seed(network, &ark_seed), config, OpenWalletArgs {
		persister: Some(restored_ark_db), onchain: Some(restored_app.clone()), fallback_spk: Some(script),
		run_daemon: false, ..Default::default()
	}).await.unwrap();
	assert_eq!(restored.fallback_destination().await.unwrap(), registration);
	restored_app.write().await.sync(restored.chain()).await.unwrap();
	assert!(restored_impl.read().await.0.utxos().iter().any(|utxo| matches!(utxo,
		bark::onchain::Utxo::Local(local) if local.outpoint == point && local.amount == expected && local.confirmation_height.is_some())));

	// Discovery is account-authorized, without feeding a spent input ID or
	// transaction into the newly restored Ark wallet.
	restored.sync_fallback_payouts().await.expect("native authenticated account payout history");
	let receipts = restored.fallback_payouts().await.unwrap();
	let receipt = receipts.iter().find(|receipt| receipt.vtxo_id == source.id()).expect("seed-only account history missed its payout entitlement");
	assert!(receipt.attempts.iter().any(|attempt| attempt.active && attempt.payout == expected &&
		attempt.parent.as_ref().is_some_and(|tx| tx.compute_txid() == point.txid) && attempt.output_index == Some(point.vout)));
	let history = restored.history().await.unwrap();
	let payouts = history.iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout").collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	assert_eq!(payouts[0].sent_to[0].amount, expected);
	assert_eq!(payouts[0].offchain_fee, sat(1_000));
	assert_eq!(payouts[0].effective_balance.to_sat(), 0, "a seed-restored unknown Ark leaf must not invent another debit");

	let destination = ctx.bitcoind().get_new_address();
	let psbt = restored_app.write().await.prepare_drain_tx(destination.clone(), FeeRate::from_sat_per_vb(2).unwrap()).await.unwrap();
	let signed = restored_app.write().await.sign_psbt(psbt).await.unwrap();
	let final_psbt = restored_app.write().await.finish_psbt(signed).await.unwrap();
	let recovery = final_psbt.extract_tx().unwrap();
	let input = recovery.input.iter().find(|input| input.previous_output == point).expect("external callback did not spend its actual fallback outpoint");
	assert_eq!(input.witness.len(), 2, "the fallback is actually spent through the app's BIP84 key");
	restored.chain().broadcast_tx(&recovery).await.unwrap();
	assert_eq!(ctx.bitcoind().await_transaction(recovery.compute_txid()).await, recovery);
	ctx.generate_blocks(1).await;
	assert!(ctx.bitcoind().get_received_by_address(&destination) > sat(0));
	assert!(ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
	std::fs::write(ctx.datadir.join("actual-fallback-recovery-proof.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"status": "PASS", "original_vtxo": source.id(), "fallback_outpoint": point,
		"payout_sat": expected.to_sat(), "payout_parent": bitcoin::consensus::encode::serialize_hex(&parent),
		"recovery_tx": bitcoin::consensus::encode::serialize_hex(&recovery),
		"separate_original_seeds_only": true, "fresh_ark_and_app_databases": true,
	})).unwrap()).unwrap();
}
