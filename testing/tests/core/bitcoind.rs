mod spending;

use bitcoincore_rpc::RpcApi;

use ark_testing::TestContext;

#[tokio::test]
async fn backing_spends_require_a_synchronized_reverse_index() {
	let ctx = TestContext::new("bitcoind/backing_spends_require_a_synchronized_reverse_index").await;
	let error = self::spending::require_spender_index(&ctx.bitcoind().async_client()).await
		.expect_err("txindex alone must never establish confirmed-spender coverage");
	assert!(format!("{error:#}").contains("txospenderindex"));
}

#[tokio::test]
async fn backing_spends_find_confirmed_transactions_with_empty_mempool() {
	use std::time::Duration;
	use ark_testing::sat;
	use bitcoin::OutPoint;
	use bitcoind_async_client::traits::Reader;
	use self::spending::{backing_spends, require_spender_index};

	let ctx = TestContext::new("bitcoind/backing_spends_find_confirmed_transactions_with_empty_mempool").await;
	let mut config = ctx.bitcoind_default_cfg("indexed");
	config.wallet = true;
	config.txospenderindex = true;
	let indexed = ctx.new_bitcoind_with_cfg("indexed", config).await;
	indexed.prepare_funds().await;
	let destination = indexed.get_new_address();
	let txid = indexed.fund_addr(destination, sat(10_000)).await;
	let transaction = indexed.await_transaction(txid).await;
	let points = transaction.input.iter().map(|input| input.previous_output).collect::<Vec<_>>();
	assert!(!points.is_empty());
	let client = indexed.async_client();
	let wait_for_index = async {
		while require_spender_index(&client).await.is_err() {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	};
	tokio::time::timeout(Duration::from_secs(60), wait_for_index).await.unwrap();
	let in_flight = backing_spends(&client, &points).await.unwrap();
	assert!(in_flight.iter().all(|spend| spend.as_ref().is_some_and(|spend|
		spend.transaction.compute_txid() == txid && spend.block_hash.is_none())));

	indexed.generate(1).await;
	tokio::time::timeout(Duration::from_secs(60), async {
		while require_spender_index(&client).await.is_err() {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
	assert!(client.get_raw_mempool().await.unwrap().0.is_empty());
	let confirmed = backing_spends(&client, &points).await.unwrap();
	assert!(confirmed.iter().all(|spend| spend.as_ref().is_some_and(|spend|
		spend.transaction == transaction && spend.block_hash.is_some())));
	assert_eq!(server::bitcoind::get_mempool_spending_tx(&client, points[0]).await.unwrap(), None,
		"the previous mempool-only helper cannot recover this spend");
	let unspent = transaction.output.iter().enumerate().find(|(_, output)| output.value > sat(0)).unwrap().0;
	assert!(backing_spends(&client, &[OutPoint::new(txid, unspent as u32)]).await.unwrap()[0].is_none());
}

#[tokio::test]
async fn start_bitcoind()  {
	let ctx = TestContext::new("bitcoind/start_bitcoind").await;
	let bitcoind = ctx.new_bitcoind("bitcoind-1").await;

	let client = bitcoind.sync_client();
	let info = client.get_blockchain_info().unwrap();
	assert_eq!(info.chain.to_string(), "regtest");
}

#[tokio::test]
async fn fund_bitcoind() {
	let ctx = TestContext::new("bitcoind/fund_bitcoind").await;

	// Check the balance
	let client = ctx.bitcoind().sync_client();
	let amount = client.get_balance(Some(6), None).unwrap();
	assert!(amount.to_sat() > 100_000_000);
}

