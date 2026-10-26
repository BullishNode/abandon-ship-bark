//! Shared canonical-chain assertions for expiry payout scenarios.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use ark::VtxoId;
use ark_testing::{sat, Captaind, TestContext};
use ark_testing::daemon::captaind::WalletAdminClient;
use bitcoin::{Amount, BlockHash, OutPoint, ScriptBuf, Transaction, Txid, TxOut};
use bitcoin::hex::DisplayHex;
use bitcoin_ext::BlockRef;
use bitcoin_ext::rpc::RpcApi;
use serde_json::json;
use server::database::Db;
use server_rpc::protos;

pub(super) fn fields(balance: &bark::Balance) -> [Amount; 10] {
	[balance.spendable, balance.needs_refresh, balance.pending_lightning_send,
		balance.claimable_lightning_receive, balance.pending_in_round, balance.pending_exit,
		balance.pending_board, balance.pending_arkoor_send, balance.pending_offboard,
		balance.awaiting_settlement]
}

pub(super) async fn rebuilt(ctx: &TestContext, srv: &Captaind, db: &Db) {
	let height = bitcoin_ext::BlockHeight::new(u32::try_from(ctx.bitcoind().get_block_count().await).unwrap());
	srv.wait_for_sync_height(height).await;
	srv.watchmand().wait_for_sync_height(height).await;
	let point = BlockRef { height, hash: ctx.bitcoind().sync_client().get_best_block_hash().unwrap() };
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			if db.read(async |tx| {
				let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
				Ok(view.captaind == point && view.watchmand == point && super::expiry_view_is_rebuilt(tx, view).await?)
			}).await.unwrap() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both completed expiry views must bind the real Core tip");
}

pub(super) async fn actual_fee(ctx: &TestContext, transaction: &Transaction) -> Amount {
	let mut total = Amount::ZERO;
	let mut seen = BTreeSet::new();
	for input in &transaction.input {
		assert!(seen.insert(input.previous_output));
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		total += previous.output[input.previous_output.vout as usize].value;
	}
	total.checked_sub(transaction.output.iter().map(|output| output.value).sum::<Amount>()).unwrap()
}

pub(super) async fn watch_coins(srv: &Captaind) -> BTreeSet<OutPoint> {
	let address = srv.watchmand().config().admin_address.unwrap();
	let mut rpc = WalletAdminClient::connect(format!("http://{address}")).await.unwrap();
	rpc.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap()
		.confirmed_utxos.into_iter().map(|point| OutPoint::from_str(&point).unwrap()).collect()
}

pub(super) struct Epoch {
	pub(super) parents: BTreeMap<Txid, Transaction>,
	pub(super) children: BTreeMap<Txid, Transaction>,
	pub(super) allocations: BTreeMap<VtxoId, i64>,
	pub(super) outputs: BTreeMap<VtxoId, Option<OutPoint>>,
	pub(super) hashes: BTreeMap<Txid, BlockHash>,
	pub(super) heights: BTreeMap<Txid, bitcoin_ext::BlockHeight>,
	pub(super) packages: Vec<serde_json::Value>,
}

pub(super) async fn financial_cut(
	ctx: &TestContext, srv: &Captaind, db: &Db, scan_height: u64,
	roots: &BTreeMap<OutPoint, TxOut>, sponsors: &BTreeMap<OutPoint, TxOut>,
	epochs: &[&Epoch], expected_payees: &[(ScriptBuf, Option<u64>)], expected_ids: &BTreeSet<VtxoId>,
) -> serde_json::Value {
	let core = ctx.bitcoind().sync_client();
	let mut cut = sponsors.clone();
	for (point, output) in roots { assert!(cut.insert(*point, output.clone()).is_none()); }
	let initial = cut.values().map(|output| output.value).sum::<Amount>();
	let mut graph = BTreeMap::new();
	let mut spent = BTreeSet::new();
	let mut all_points = cut.keys().copied().collect::<BTreeSet<_>>();
	let mut fees = Amount::ZERO;
	let mut canonical_payees = expected_payees.iter().map(|(script, _)|
		(script.clone(), Vec::<(OutPoint, TxOut)>::new())).collect::<BTreeMap<_, _>>();
	assert_eq!(canonical_payees.len(), expected_payees.len());
	for height in scan_height + 1..=core.get_block_count().unwrap() {
		for tx in core.get_block(&core.get_block_hash(height).unwrap()).unwrap().txdata {
			// The selected cut alone cannot exclude an extra payment funded
			// from elsewhere. Observe every canonical transaction first.
			for (vout, output) in tx.output.iter().enumerate() {
				if let Some(found) = canonical_payees.get_mut(&output.script_pubkey) {
					found.push((OutPoint::new(tx.compute_txid(), vout as u32), output.clone()));
				}
			}
			if !tx.input.iter().any(|input| cut.contains_key(&input.previous_output)) { continue; }
			let mut amount = Amount::ZERO;
			for input in &tx.input {
				assert!(spent.insert(input.previous_output));
				amount += cut.remove(&input.previous_output).expect("selected graph consumed undeclared outside funds").value;
			}
			let txid = tx.compute_txid();
			fees += amount.checked_sub(tx.output.iter().map(|output| output.value).sum::<Amount>()).unwrap();
			for (vout, output) in tx.output.iter().enumerate() {
				let point = OutPoint::new(txid, vout as u32);
				assert!(all_points.insert(point));
				assert!(cut.insert(point, output.clone()).is_none());
			}
			assert!(graph.insert(txid, tx).is_none());
		}
	}
	assert!(roots.keys().all(|point| spent.contains(point)));
	for epoch in epochs {
		for (txid, tx) in epoch.parents.iter().chain(&epoch.children) { assert_eq!(graph.get(txid), Some(tx)); }
	}
	for (script, expected) in expected_payees {
		let outputs = graph.values().flat_map(|tx| tx.output.iter().enumerate().filter_map(move |(vout, output)|
			(&output.script_pubkey == script).then_some((OutPoint::new(tx.compute_txid(), vout as u32), output))
		)).collect::<Vec<_>>();
		if let Some(value) = expected {
			assert_eq!(outputs.len(), 1);
			assert_eq!(outputs[0].1.value, sat(*value));
			assert!(cut.contains_key(&outputs[0].0));
		} else { assert!(outputs.is_empty(), "a forfeited or transferred source cannot obtain a payout"); }
		let global = &canonical_payees[script];
		assert_eq!(global.len(), outputs.len(), "whole-chain payee outputs must equal the selected economic cut");
		if !global.is_empty() {
			assert_eq!(global[0].0, outputs[0].0);
			assert_eq!(&global[0].1, outputs[0].1);
		}
	}
	let operator = {
		let config = srv.watchmand().config();
		config.sweep_address.clone().unwrap().require_network(config.network).unwrap().script_pubkey()
	};
	let needed_watch = cut.iter().filter(|(_, output)| output.value > Amount::ZERO
		&& output.script_pubkey != operator && expected_payees.iter().all(|(script, _)| output.script_pubkey != *script))
		.map(|(point, _)| *point).collect::<BTreeSet<_>>();
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			if needed_watch.is_subset(&watch_coins(srv).await) { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual child change and unused sponsors remain in the real watchman wallet");
	let mut retained = Amount::ZERO;
	for (point, expected) in &cut {
		if expected.value == Amount::ZERO { continue; }
		let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, expected.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), expected.script_pubkey);
		retained += actual.value;
	}
	assert_eq!(retained + fees, initial);
	for txid in core.get_raw_mempool().unwrap() {
		let tx = core.get_raw_transaction(&txid, None).unwrap();
		assert!(tx.input.iter().all(|input| !cut.contains_key(&input.previous_output)), "no selected terminal value may be pending-spent");
		assert!(tx.output.iter().all(|output| expected_payees.iter().all(|(script, _)| output.script_pubkey != *script)),
			"no old, revived or duplicate customer output may await confirmation");
	}
	let anchors = roots.keys().map(ToString::to_string).collect::<Vec<_>>();
	let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
		JOIN vtxo v ON v.vtxo_id=e.vtxo_id WHERE v.anchor_point=ANY($1) AND c.disposition='customer'",
		&[&anchors]).await?)).await.unwrap();
	assert!(!rows.is_empty(), "the final owner must have a non-vacuous customer allocation");
	assert_eq!(rows.iter().map(|row| row.get::<_, &str>("vtxo_id").parse::<VtxoId>().unwrap()).collect::<BTreeSet<_>>(), *expected_ids);
	json!({"selected_input_sat": initial.to_sat(), "remaining_sat": retained.to_sat(), "actual_graph_miner_fee_sat": fees.to_sat(),
		"roots": roots.iter().map(|(point, output)| json!({"outpoint": point, "amount_sat": output.value.to_sat(),
			"script": output.script_pubkey.as_bytes().to_lower_hex_string()})).collect::<Vec<_>>(),
		"sponsors": sponsors.iter().map(|(point, output)| json!({"outpoint": point, "amount_sat": output.value.to_sat(),
			"script": output.script_pubkey.as_bytes().to_lower_hex_string()})).collect::<Vec<_>>(),
		"expected_payees": expected_payees.iter().map(|(script, amount)| json!({"script": script.as_bytes().to_lower_hex_string(),
			"expected_amount_sat": amount})).collect::<Vec<_>>(),
		"canonical_packages": epochs.iter().flat_map(|epoch| epoch.packages.iter()).collect::<Vec<_>>(),
		"canonical_payee_outputs": canonical_payees.iter().flat_map(|(script, outputs)| outputs.iter().map(move |(point, output)|
			json!({"outpoint": point, "amount_sat": output.value.to_sat(), "script": script.as_bytes().to_lower_hex_string()}))).collect::<Vec<_>>(),
		"terminal_utxos": cut.iter().filter(|(_, output)| output.value > Amount::ZERO).map(|(point, output)|
			json!({"outpoint": point, "amount_sat": output.value.to_sat(), "script": output.script_pubkey.as_bytes().to_lower_hex_string()})).collect::<Vec<_>>(),
		"graph_txids": graph.keys().map(ToString::to_string).collect::<Vec<_>>(),
		"all_graph_outpoints": all_points.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"operator_configured_script": operator.as_bytes().to_lower_hex_string(), "confirmed_watch_owned_terminal_points": needed_watch})
}
