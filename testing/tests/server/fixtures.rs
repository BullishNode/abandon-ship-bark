use bitcoincore_rpc::RpcApi;

use ark_testing::TestContext;

#[tokio::test]
async fn loaded_funding_wallet_can_be_opened_again() {
	let ctx = TestContext::new("server/loaded_funding_wallet_can_be_opened_again").await;
	let core = ctx.bitcoind().sync_client();
	let before = core.call::<serde_json::Value>("getwalletinfo", &[]).unwrap();
	let balance = core.call::<serde_json::Value>("getbalances", &[]).unwrap();
	ctx.bitcoind().load_wallet("central").await;
	let after = core.call::<serde_json::Value>("getwalletinfo", &[]).unwrap();
	assert_eq!(before["walletname"], "central");
	assert_eq!(before["walletname"], after["walletname"]);
	assert_eq!(balance, core.call::<serde_json::Value>("getbalances", &[]).unwrap());
}
