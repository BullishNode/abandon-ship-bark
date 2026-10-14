//! Isolate actual SDK operations from the large returning-client fixture.

use std::sync::Arc;

use super::*;
use bark::{OpenWalletArgs, WalletSeed};
use bark::onchain::OnchainWallet;
use bark::persist::BarkPersister;
use bark::persist::sqlite::SqliteClient;

#[derive(Clone)]
struct Passthrough;

#[async_trait::async_trait]
impl ark_testing::daemon::captaind::proxy::ArkRpcProxy for Passthrough {}

#[tokio::test]
async fn sdk_actual_open_board_sync_on_default_size_worker() {
	let ctx = TestContext::new("client-stack/sdk-money").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(ark_testing::btc(1)).cfg(|c| c.fees.board.base_fee = sat(1_000)).create().await;
	let proxy = srv.start_proxy_no_mailbox(Passthrough).await;
	let mut cfg = ctx.bark_default_cfg(&proxy.address, Some(ctx.bitcoind()));
	cfg.daemon_manual_sync = true;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let seed = WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic);
	let path = ctx.datadir.join("wallet");
	std::fs::create_dir(&path).unwrap();
	let db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(path.join("db.sqlite")).unwrap());
	let seed_bytes = mnemonic.to_seed("");
	// Match the pinned Tokio default worker stack without changing product runtime settings.
	let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1)
		.thread_stack_size(2 * 1024 * 1024).enable_all().build().unwrap();
	let wallet = runtime.spawn(async move {
		let load = OnchainWallet::load_or_create(Network::Regtest, seed_bytes, db.clone());
		let onchain = Arc::new(tokio::sync::RwLock::new(load.await.unwrap()));
		let open = bark::Wallet::open(Network::Regtest, seed, cfg, OpenWalletArgs {
			persister: Some(db), create_if_not_exists: true, onchain: Some(onchain), ..Default::default()
		});
		let wallet = open.await.unwrap();
		wallet.stop_daemon_wait().await.unwrap();
		wallet
	}).await.unwrap();
	let address = wallet.onchain().unwrap().write().await.address().await.unwrap();
	ctx.bitcoind().fund_addr(address, sat(121_000)).await;
	ctx.generate_blocks(1).await;
	let active = wallet.clone();
	runtime.spawn(async move { active.sync_onchain().await.unwrap(); }).await.unwrap();
	let active = wallet.clone();
	let board = runtime.spawn(async move {
		let board = active.board_amount(sat(21_000));
		let result = board.await.unwrap();
		result
	}).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(ark_testing::constants::BOARD_CONFIRMATIONS).await;
	let active = wallet.clone();
	runtime.spawn(async move {
		let sync = active.sync();
		sync.await;
	}).await.unwrap();
	assert_eq!(wallet.balance().await.unwrap().total(), sat(20_000));
	assert_eq!(wallet.spendable_vtxos().await.unwrap().len(), 1);
	assert!(ctx.bitcoind().sync_client().get_raw_transaction(&board.funding_tx.compute_txid(), None).unwrap().output
		.iter().any(|output| output.value == sat(21_000)));
	runtime.shutdown_background();
}
