//! Actual owning-daemon observations and independent Core projection checks.

use std::{str::FromStr, time::Duration};

use bitcoin::{Address, Network, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::rpc::RpcApi;
use serde_json::json;
use server_log::RoundFinished;
use server_rpc::{admin::WalletAdminServiceClient, protos};

use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::util::FutureExt;

#[tokio::test]
async fn wallet_inventory_is_owning_daemon_only_and_does_not_reveal_addresses() {
	let ctx = TestContext::new("server/wallet_inventory_is_owning_daemon_only_and_does_not_reveal_addresses").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().watchmand().create().await;
	srv.watchmand().wait_wallet_address().await;
	let mut rpc = srv.get_wallet_rpc().await;
	let first = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner();
	let again = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner();
	assert!(!first.complete && !first.missing_sources.is_empty());
	assert!(first.watchman.is_none());
	let rounds = first.rounds.unwrap();
	let again = again.rounds.unwrap();
	assert_eq!(rounds.wallet, "rounds");
	assert_eq!(rounds.network, "regtest");
	assert_eq!(rounds.public_descriptor, again.public_descriptor);
	assert_eq!(rounds.last_revealed_index, again.last_revealed_index);
	assert_eq!(rounds.reserved_outpoints, again.reserved_outpoints);
	assert_eq!(rounds.utxos, again.utxos);
	assert!(!rounds.public_descriptor.contains("tprv"));
	let descriptor: serde_json::Value = srv.bitcoind().sync_client()
		.call("getdescriptorinfo", &[json!(rounds.public_descriptor)]).unwrap();
	assert_eq!(descriptor["hasprivatekeys"], false);

	let endpoint = format!("http://{}", srv.watchmand().config().admin_address.unwrap());
	let mut rpc = WalletAdminServiceClient::connect(endpoint).await.unwrap();
	let response = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner();
	assert!(!response.complete && !response.missing_sources.is_empty());
	assert!(response.rounds.is_none());
	let watchman = response.watchman.unwrap();
	assert_eq!(watchman.wallet, "watchman");
	assert_ne!(rounds.public_descriptor, watchman.public_descriptor);
	let before = watchman.last_revealed_index;
	assert_eq!(rpc.wallet_status(protos::Empty {}).await.unwrap_err().code(), tonic::Code::Unimplemented);
	let response = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner();
	assert_eq!(response.watchman.unwrap().last_revealed_index, before);
}

#[tokio::test]
async fn wallet_inventory_pending_round_preserves_confirmed_tip_inputs() {
	let ctx = TestContext::new("server/wallet_inventory_pending_round_preserves_confirmed_tip_inputs").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().create().await;
	let bark = ctx.bark("bark", &srv).funded(sat(1_000_000)).create().await;
	bark.board(sat(800_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	bark.sync().await;
	let mut finished = srv.subscribe_log::<RoundFinished>();
	ctx.refresh_all(&srv, &[&bark]).await;
	let txid = finished.recv().wait(Duration::from_secs(30)).await.unwrap().txid;
	srv.bitcoind().await_transaction(txid).await;
	let core = srv.bitcoind().sync_client();
	let signed = core.get_raw_transaction(&txid, None).unwrap();
	assert_eq!(core.get_raw_transaction_info(&txid, None).unwrap().confirmations.unwrap_or(0), 0);
	let mut rpc = srv.get_wallet_rpc().await;
	let inventory = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
	let pending = inventory.pending_transactions.iter().find(|tx| tx.txid == txid.to_string()).unwrap();
	let public: Transaction = deserialize(&pending.transaction_body).unwrap();
	assert_eq!(public.compute_txid(), txid);
	assert!(public.input.iter().all(|input| input.witness.is_empty()));
	assert_eq!(pending.signed_weight_wu, signed.weight().to_wu());
	for input in &public.input {
		let point = input.previous_output;
		assert!(!inventory.utxos.iter().any(|u| u.outpoint == point.to_string()));
		let confirmed = core.get_tx_out(&point.txid, point.vout, Some(false)).unwrap().unwrap();
		assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
		let parent = inventory.pending_parents.iter().find(|tx| tx.txid == point.txid.to_string()).unwrap();
		let parent: Transaction = deserialize(&parent.transaction_body).unwrap();
		assert_eq!(parent.compute_txid(), point.txid);
		let actual = core.get_raw_transaction(&point.txid, None).unwrap();
		assert_eq!(parent.output[point.vout as usize], actual.output[point.vout as usize]);
		assert_eq!(parent.output[point.vout as usize].value, confirmed.value);
		let index = (0..=inventory.last_revealed_index.unwrap()).find(|index| {
			let addresses: Vec<String> = core.call("deriveaddresses", &[
				json!(inventory.public_descriptor), json!([index, index]),
			]).unwrap();
			Address::from_str(&addresses[0]).unwrap().require_network(Network::Regtest).unwrap()
				.script_pubkey() == parent.output[point.vout as usize].script_pubkey
		}).expect("confirmed pending input must derive from actual public wallet descriptor");
		assert!(index <= inventory.last_revealed_index.unwrap());
	}
	ctx.generate_blocks(1).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let now = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
			if !now.pending_transactions.iter().any(|tx| tx.txid == txid.to_string()) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("confirmed native round must leave the pending projection");
	for input in &public.input {
		assert!(core.get_tx_out(&input.previous_output.txid, input.previous_output.vout, Some(false)).unwrap().is_none());
	}
}
