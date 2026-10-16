//! A separately seeded app wallet crosses the real public callback boundary.
//! This is registration/restore/onchain receipt coverage; an actual fallback
//! payout test must also run once the backend payout path is available.

use std::sync::Arc;

use bark::chain::ChainSource;
use bark::onchain::{CpfpError, MakeCpfpFees, OnchainWallet, OnchainWalletTrait};
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::{OpenWalletArgs, Wallet, WalletSeed};
use bitcoin::{Address, Amount, FeeRate, Network, Psbt, Script, Transaction, Txid};
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
