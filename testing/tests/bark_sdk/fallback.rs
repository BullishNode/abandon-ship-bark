use ark_testing::TestContext;
use bitcoin::{Network, ScriptBuf};
use std::sync::Arc;

use bark::persist::{BarkPersister, sqlite::SqliteClient};

#[tokio::test]
async fn fallback_native_registration_survives_seed_only_second_device_and_reopen() {
	let ctx = TestContext::new("bark_sdk/fallback_native_registration_survives_seed_only_second_device_and_reopen").await;
	let srv = ctx.captaind("server").create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let first = ctx.bark_sdk("first", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	first.stop_daemon_wait().await.unwrap();
	let record = first.fallback_destination().await.unwrap();
	let onchain = first.onchain().unwrap();
	let expected = {
		let guard = onchain.read().await;
		(&*guard as &dyn std::any::Any).downcast_ref::<bark::onchain::OnchainWallet>().unwrap().fallback_script()
	};
	assert_eq!(record.spk, expected);
	let identity = first.peek_keypair(0).await.unwrap().public_key();
	assert_eq!(first.new_address_with_index().await.unwrap().1, 1);
	let config = first.config().clone();
	let second = ctx.bark_sdk("second", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	second.stop_daemon_wait().await.unwrap();
	assert_eq!(second.fallback_destination().await.unwrap(), record);
	assert_eq!(second.peek_keypair(0).await.unwrap().public_key(), identity);
	drop(first);
	// A cached identity supports a seed-only read/status open. It does not
	// require another onchain wallet or mutate the immutable destination.
	let reopened = bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("first")), run_daemon: false, ..Default::default() },
	).await.unwrap();
	assert_eq!(reopened.fallback_destination().await.unwrap(), record);
	assert_eq!(reopened.new_address_with_index().await.unwrap().1, 2);
}

#[derive(Clone)]
struct LoseFirstFallbackReply(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[async_trait::async_trait]
impl ark_testing::daemon::captaind::proxy::ArkRpcProxy for LoseFirstFallbackReply {
	async fn set_fallback(
		&self, upstream: &mut ark_testing::daemon::captaind::ArkClient,
		request: server_rpc::protos::SetFallbackRequest,
	) -> Result<server_rpc::protos::SetFallbackResponse, tonic::Status> {
		let response = upstream.set_fallback(request).await?.into_inner();
		if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
			return Err(tonic::Status::unavailable("injected lost registration reply"));
		}
		Ok(response)
	}
}

#[tokio::test]
async fn fallback_native_retries_a_lost_registration_reply() {
	let ctx = TestContext::new("bark_sdk/fallback_native_retries_a_lost_registration_reply").await;
	let srv = ctx.captaind("server").create().await;
	let proxy = srv.start_proxy_no_mailbox(LoseFirstFallbackReply(
		std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
	)).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let first = ctx.bark_sdk("wallet", &proxy.address).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).try_create().await;
	assert!(first.is_err(), "the caller must observe the lost reply");
	let postgres = srv.config().postgres.clone();
	let db = server::database::Db::connect(&postgres).await.unwrap();
	let count = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM fallback_record", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, 1, "the upstream must commit before the proxy loses its reply");
	let wallet = ctx.bark_sdk("wallet", &proxy.address).mnemonic(mnemonic)
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	let canonical = db.read(async |tx| tx.get_fallback(wallet.mailbox_identifier().as_pubkey()).await).await.unwrap().unwrap();
	assert_eq!(record, canonical);
	assert_eq!(wallet.new_address_with_index().await.unwrap().1, 1);
}

/// Offline creation retains an exact local destination, without treating the
/// locally signed record as an authenticated server acknowledgement.
#[tokio::test]
async fn fallback_offline_creation_retains_identity_and_registers_on_online_reopen() {
	let ctx = TestContext::new("bark_sdk/fallback_offline_creation_pending_registration").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let mut config = ctx.bark_default_cfg(&srv, Some(ctx.bitcoind()));
	config.daemon_manual_sync = true;
	srv.stop().await.unwrap();
	let directory = ctx.datadir.join("offline-wallet");
	tokio::fs::create_dir_all(&directory).await.unwrap();
	let db = Arc::new(SqliteClient::open(directory.join("db.sqlite")).unwrap());
	let seed = || bark::WalletSeed::new_from_seed(Network::Regtest, &[23; 64]);
	let onchain = bark::onchain::OnchainWallet::load_or_create(Network::Regtest, [23; 64], db.clone()).await.unwrap();
	let expected = onchain.fallback_script();
	let wallet = bark::Wallet::open(Network::Regtest, seed(), config.clone(), bark::OpenWalletArgs {
		persister: Some(db.clone()), datadir: Some(directory.clone()), fallback_spk: Some(expected.clone()),
		create_without_server: true, run_daemon: false, skip_recovery: true, ..Default::default()
	}).await.unwrap();
	assert!(wallet.fallback_registration().await.unwrap().is_none());
	assert!(wallet.fallback_destination().await.is_err());
	assert!(wallet.peek_address(0).await.is_err());
	assert!(wallet.new_address_with_index().await.is_err());
	assert_eq!(db.get_last_vtxo_key_index().await.unwrap(), None);
	let pending = db.get_pending_fallback_record().await.unwrap().unwrap();
	assert_eq!(pending.spk, expected);
	pending.verify(Network::Regtest, wallet.mailbox_identifier().as_pubkey()).unwrap();
	drop(wallet);
	let offline = bark::Wallet::open(Network::Regtest, seed(), config.clone(), bark::OpenWalletArgs {
		persister: Some(db.clone()), datadir: Some(directory.clone()), run_daemon: false, ..Default::default()
	}).await.unwrap();
	assert!(offline.fallback_registration().await.unwrap().is_none());
	assert_eq!(db.get_pending_fallback_record().await.unwrap(), Some(pending.clone()));
	drop(offline);
	let changed = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
	assert!(bark::Wallet::open(Network::Regtest, seed(), config.clone(), bark::OpenWalletArgs {
		persister: Some(db.clone()), datadir: Some(directory.clone()), fallback_spk: Some(changed),
		run_daemon: false, ..Default::default()
	}).await.is_err());
	assert_eq!(db.get_pending_fallback_record().await.unwrap(), Some(pending));
	srv.start().await.unwrap();
	config.server_address = srv.ark_url();
	let online = bark::Wallet::open(Network::Regtest, seed(), config, bark::OpenWalletArgs {
		persister: Some(db.clone()), datadir: Some(directory), run_daemon: false, ..Default::default()
	}).await.unwrap();
	let registered = online.fallback_destination().await.unwrap();
	assert_eq!(registered.spk, expected);
	registered.verify(Network::Regtest, online.mailbox_identifier().as_pubkey()).unwrap();
	assert!(db.get_pending_fallback_record().await.unwrap().is_none());
	let canonical = server::database::Db::connect(&srv.config().postgres).await.unwrap()
		.read(async |tx| tx.get_fallback(online.mailbox_identifier().as_pubkey()).await).await.unwrap().unwrap();
	assert_eq!(registered, canonical);
	assert_eq!(online.new_address_with_index().await.unwrap().1, 0);
}
