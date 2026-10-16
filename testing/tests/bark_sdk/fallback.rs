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
