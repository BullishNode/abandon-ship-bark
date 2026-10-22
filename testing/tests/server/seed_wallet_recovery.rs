use std::fs::{self, File};
use std::path::Path;
use std::process::Stdio;
use std::str::FromStr;

use bitcoin::{Address, Network, OutPoint, Transaction, Txid};
use bitcoin::bip32::Xpriv;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::io::AsyncWriteExt;

use ark::SECP;
use ark_testing::{sat, Bitcoind, TestContext};
use bitcoin_ext::rpc::{BitcoinRpcClient, RpcApi};
use server::wallet::WalletKind;

// The RPC library logs request parameters at debug level. Use Core's stdin
// interface for the only request that contains a private descriptor.
async fn import_seed(
	bitcoind: &Bitcoind, wallet: &str, seed: &Path, kind: WalletKind, next_index: u32,
) {
	let helper = ark_testing::util::get_cargo_workspace().join("contrib/release/server-seed-descriptors.py");
	let output = Command::new("python3").arg(helper)
		.args(["--network", "regtest", "--wallet", kind.name(), "--range-end", "1023", "--next-index"])
		.arg(next_index.to_string()).arg("--stdout").stdin(Stdio::from(File::open(seed).unwrap())).output().await.unwrap();
	assert!(output.status.success(), "private descriptor helper failed");
	let descriptor = output.stdout;
	let mut importer = Command::new(Bitcoind::exec().with_file_name("bitcoin-cli"))
		.args(["-regtest", "-rpcconnect=127.0.0.1"])
		.arg(format!("-datadir={}", bitcoind.datadir().display()))
		.arg(format!("-rpcport={}", bitcoind.rpc_port()))
		.arg(format!("-rpccookiefile={}", bitcoind.rpc_cookie().display()))
		.arg(format!("-rpcwallet={wallet}")).args(["-stdin", "importdescriptors"])
		.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
	let mut stdin = importer.stdin.take().unwrap();
	stdin.write_all(&descriptor).await.unwrap();
	drop(stdin);
	let output = importer.wait_with_output().await.unwrap();
	assert!(output.status.success(), "Core rejected the private descriptor import");
	let response: Value = serde_json::from_slice(&output.stdout).expect("Core import response is JSON");
	assert!(response[0]["success"] == true, "Core did not import the seed descriptor");
}

fn fresh_core_wallet(bitcoind: &Bitcoind, name: &str) -> BitcoinRpcClient {
	let result: Value = bitcoind.sync_client().call("createwallet", &[
		json!(name), json!(false), json!(true), json!(""), json!(false), json!(true),
	]).unwrap();
	assert_eq!(result["name"], name);
	BitcoinRpcClient::new(&format!("{}/wallet/{name}", bitcoind.rpc_url()), bitcoind.auth()).unwrap()
}

fn seed_address(master: &Xpriv, kind: WalletKind, index: u32) -> Address {
	let key = master.derive_priv(&SECP, &[kind.child_number(), 0.into(), index.into()]).unwrap();
	Address::p2tr(&SECP, key.to_keypair(&SECP).x_only_public_key().0, None, Network::Regtest)
}

fn assert_unspent(bitcoind: &Bitcoind, point: OutPoint, amount: bitcoin::Amount) {
	let coin = bitcoind.sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap()
		.expect("the recovered output must be unspent on the canonical chain");
	assert!(coin.confirmations >= 6);
	assert_eq!(coin.value, amount);
}

async fn spend_with_change(
	bitcoind: &Bitcoind, wallet: &BitcoinRpcClient, inputs: &[OutPoint],
	destination: &Address, change: &Address, amount: bitcoin::Amount,
) -> (Transaction, bitcoin::Amount) {
	let funded: Value = wallet.call("walletcreatefundedpsbt", &[
		json!(inputs.iter().map(|p| json!({"txid": p.txid, "vout": p.vout})).collect::<Vec<_>>()),
		json!([{destination.to_string(): amount.to_btc()}]), json!(0),
		json!({"add_inputs": false, "changeAddress": change.to_string(), "fee_rate": 2}),
	]).unwrap();
	let signed: Value = wallet.call("walletprocesspsbt", &[funded["psbt"].clone()]).unwrap();
	assert_eq!(signed["complete"], true);
	let finalized: Value = wallet.call("finalizepsbt", &[signed["psbt"].clone()]).unwrap();
	assert_eq!(finalized["complete"], true);
	let txid: Txid = wallet.call("sendrawtransaction", &[finalized["hex"].clone()]).unwrap();
	let transaction = bitcoind.await_transaction(txid).await;
	let mut actual_inputs = transaction.input.iter().map(|i| i.previous_output).collect::<Vec<_>>();
	let mut expected_inputs = inputs.to_vec();
	actual_inputs.sort();
	expected_inputs.sort();
	assert_eq!(actual_inputs, expected_inputs);
	assert_eq!(transaction.output.len(), 2);
	let mut total_input = sat(0);
	for input in inputs {
		let previous = bitcoind.await_transaction(input.txid).await;
		total_input += previous.output[input.vout as usize].value;
	}
	let actual_fee = total_input - transaction.output.iter().map(|o| o.value).sum::<bitcoin::Amount>();
	assert!(actual_fee > sat(0));
	assert_eq!(actual_fee, bitcoin::Amount::from_btc(funded["fee"].as_f64().unwrap()).unwrap());
	let payment = transaction.output.iter().find(|o| o.script_pubkey == destination.script_pubkey()).unwrap();
	assert_eq!(payment.value, amount);
	bitcoind.generate(6).await;
	let payment_index = transaction.output.iter().position(|o| o == payment).unwrap();
	assert_unspent(bitcoind, OutPoint::new(txid, payment_index as u32), amount);
	for input in inputs {
		assert!(bitcoind.sync_client().get_tx_out(&input.txid, input.vout, Some(true)).unwrap().is_none());
	}
	(transaction, actual_fee)
}

#[tokio::test]
async fn server_and_watchman_seed_only_recover_receipts_and_change() {
	let ctx = TestContext::new("server/server_and_watchman_seed_only_recover_receipts_and_change").await;
	let core = ctx.bitcoind();
	let central = BitcoinRpcClient::new(&format!("{}/wallet/central", core.rpc_url()), core.auth()).unwrap();
	let chain: Value = core.sync_client().call("getblockchaininfo", &[]).unwrap();
	assert_eq!(chain["chain"], "regtest", "this recovery drill must never use mainnet");
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().watchmand().create().await;
	let seed = srv.config().data_dir.join("mnemonic");
	let phrase = bip39::Mnemonic::from_str(fs::read_to_string(&seed).unwrap().trim()).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &phrase.to_seed("")).unwrap();
	let rounds = srv.get_rounds_funding_address().await;
	let watchman = srv.watchmand().wait_wallet_address().await;
	assert_ne!(rounds, watchman);
	let mut receipts = Vec::new();
	for (kind, advertised) in [(WalletKind::Rounds, rounds), (WalletKind::Watchman, watchman)] {
		assert!((0..10).any(|i| seed_address(&master, kind, i) == advertised), "daemon address differs from seed derivation");
		let mut points = Vec::new();
		for address in [advertised, seed_address(&master, kind, 17)] {
			let txid = core.fund_addr(&address, sat(60_000)).await;
			let tx = core.await_transaction(txid).await;
			let index = tx.output.iter().position(|o| o.script_pubkey == address.script_pubkey()).unwrap();
			points.push(OutPoint::new(txid, index as u32));
		}
		receipts.push((kind, points));
	}
	core.generate(6).await;
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	// No application RPC or database is used after this point. Both fresh
	// Core wallets receive only the original seed and the public scan range.
	for (kind, points) in receipts {
		for point in &points { assert_unspent(core, *point, sat(60_000)); }
		let name = format!("{}-first-recovery", kind.name());
		let wallet = fresh_core_wallet(core, &name);
		import_seed(core, &name, &seed, kind, 1000).await;
		let recovered = wallet.list_unspent(Some(6), None, None, None, None).unwrap();
		assert_eq!(recovered.len(), points.len());
		for point in &points {
			assert!(recovered.iter().any(|u| u.txid == point.txid && u.vout == point.vout && u.amount == sat(60_000)));
		}
		let change = wallet.get_new_address(None, Some(bitcoin_ext::rpc::json::AddressType::Bech32m)).unwrap()
			.require_network(Network::Regtest).unwrap();
		assert_eq!(change, seed_address(&master, kind, 1000));
		let destination = central.get_new_address(None, None).unwrap().require_network(Network::Regtest).unwrap();
		let (first, first_fee) = spend_with_change(core, &wallet, &points, &destination, &change, sat(50_000)).await;
		let change_index = first.output.iter().position(|o| o.script_pubkey == change.script_pubkey()).unwrap();
		let change_point = OutPoint::new(first.compute_txid(), change_index as u32);
		let change_amount = first.output[change_index].value;
		assert_eq!(sat(120_000), sat(50_000) + first_fee + change_amount);
		assert_unspent(core, change_point, change_amount);
		let _: Value = core.sync_client().call("unloadwallet", &[json!(name)]).unwrap();
		drop(wallet);

		let name = format!("{}-change-recovery", kind.name());
		let recovered = fresh_core_wallet(core, &name);
		import_seed(core, &name, &seed, kind, 1001).await;
		let unspent = recovered.list_unspent(Some(6), None, None, None, None).unwrap();
		assert_eq!(unspent.len(), 1);
		assert_eq!(OutPoint::new(unspent[0].txid, unspent[0].vout), change_point);
		assert_eq!(unspent[0].amount, change_amount);
		let next_change = recovered.get_new_address(None, Some(bitcoin_ext::rpc::json::AddressType::Bech32m)).unwrap()
			.require_network(Network::Regtest).unwrap();
		assert_eq!(next_change, seed_address(&master, kind, 1001));
		let destination = central.get_new_address(None, None).unwrap().require_network(Network::Regtest).unwrap();
		let (second, second_fee) = spend_with_change(core, &recovered, &[change_point], &destination, &next_change, sat(20_000)).await;
		let remainder = second.output.iter().find(|o| o.script_pubkey == next_change.script_pubkey()).unwrap();
		assert_eq!(sat(120_000), sat(50_000) + sat(20_000) + first_fee + second_fee + remainder.value);
		let index = second.output.iter().position(|o| o == remainder).unwrap();
		assert_unspent(core, OutPoint::new(second.compute_txid(), index as u32), remainder.value);
		log::info!("{} seed recovery: receipts={:?}, first_spend={}, change={}, second_spend={}, miner_fees={} sat, remainder={} sat",
			kind.name(), points, first.compute_txid(), change_point, second.compute_txid(),
			(first_fee + second_fee).to_sat(), remainder.value.to_sat());
	}
}
