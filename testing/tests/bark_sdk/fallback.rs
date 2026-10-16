use ark_testing::TestContext;
use bitcoin::Network;

#[tokio::test]
async fn fallback_native_registration_survives_seed_only_second_device_and_reopen() {
	let ctx = TestContext::new("bark_sdk/fallback_native_registration_survives_seed_only_second_device_and_reopen").await;
	let srv = ctx.captaind("server").create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let first = ctx.bark_sdk("first", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	first.stop_daemon_wait().await.unwrap();
	let record = first.fallback_destination().await.unwrap();
	assert!(record.spk.is_p2wpkh());
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
