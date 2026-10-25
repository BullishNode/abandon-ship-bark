//! Two full-value sends leave one final owner's fallback claim.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use ark::attestations::VtxoStatusAttestation;
use ark::mailbox::MailboxIdentifier;
use ark::{ProtocolEncoding, Vtxo, VtxoId};
use ark::vtxo::Full;
use ark_testing::{require_bitcoind_chain_source, sat, Captaind, TestContext};
use ark_testing::daemon::captaind::WalletAdminClient;
use bark::{Wallet};
use bark::movement::{Movement, MovementStatus, PaymentMethod};
use bark::vtxo::VtxoState;
use bitcoin::{Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, Txid, TxOut};
use bitcoin::consensus::deserialize;
use bitcoin::hex::DisplayHex;
use bitcoin_ext::{BlockDelta, BlockRef, TxOutExt};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use serde_json::json;
use server::database::Db;
use server_rpc::protos;

fn fields(balance: &bark::Balance) -> [Amount; 10] {
	[balance.spendable, balance.needs_refresh, balance.pending_lightning_send,
		balance.claimable_lightning_receive, balance.pending_in_round, balance.pending_exit,
		balance.pending_board, balance.pending_arkoor_send, balance.pending_offboard,
		balance.awaiting_settlement]
}

async fn balances(wallet: &Wallet, spendable: Amount) {
	let mut expected = [Amount::ZERO; 10];
	expected[0] = spendable;
	assert_eq!(fields(&wallet.balance().await.unwrap()), expected);
	assert!(wallet.exit_mgr().list_all(false, false).await.unwrap().is_empty(),
		"no live or finished unilateral exit may claim the forfeited principal");
}

async fn rebuilt(ctx: &TestContext, srv: &Captaind, db: &Db) {
	let height = bitcoin_ext::BlockHeight::new(u32::try_from(ctx.bitcoind().get_block_count().await).unwrap());
	srv.wait_for_sync_height(height).await;
	srv.watchmand().wait_for_sync_height(height).await;
	let point = BlockRef { height, hash: ctx.bitcoind().sync_client().get_best_block_hash().unwrap() };
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			if db.read(async |tx| {
				let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
				Ok(view.captaind == point && view.watchmand == point && !tx.query_one("SELECT invalid FROM expiry_chain_view WHERE id=1", &[]).await?.get::<_, bool>(0))
			}).await.unwrap() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both completed expiry views must bind the real Core tip");
}

async fn actual_fee(ctx: &TestContext, transaction: &Transaction) -> Amount {
	let mut total = Amount::ZERO;
	let mut seen = BTreeSet::new();
	for input in &transaction.input {
		assert!(seen.insert(input.previous_output));
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		total += previous.output[input.previous_output.vout as usize].value;
	}
	total.checked_sub(transaction.output.iter().map(|output| output.value).sum::<Amount>()).unwrap()
}

async fn watch_coins(srv: &Captaind) -> BTreeSet<OutPoint> {
	let address = srv.watchmand().config().admin_address.unwrap();
	let mut rpc = WalletAdminClient::connect(format!("http://{address}")).await.unwrap();
	rpc.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap()
		.confirmed_utxos.into_iter().map(|point| OutPoint::from_str(&point).unwrap()).collect()
}

#[derive(Clone)]
struct ExpectedLeaf {
	coin: Vtxo<Full>,
	mailbox: MailboxIdentifier,
	script: ScriptBuf,
	fee: u64,
	payout: u64,
	forfeited: u64,
}

struct Epoch {
	parents: BTreeMap<Txid, Transaction>,
	children: BTreeMap<Txid, Transaction>,
	allocations: BTreeMap<VtxoId, i64>,
	outputs: BTreeMap<VtxoId, Option<OutPoint>>,
	hashes: BTreeMap<Txid, BlockHash>,
	heights: BTreeMap<Txid, bitcoin_ext::BlockHeight>,
	packages: Vec<serde_json::Value>,
}

async fn confirm_epoch(
	ctx: &TestContext, srv: &Captaind, db: &Db, leaves: &[ExpectedLeaf], start: u64,
) -> Epoch {
	let expiry = leaves[0].coin.expiry_height().to_u32();
	assert!(leaves.iter().all(|leaf| leaf.coin.expiry_height().to_u32() == expiry));
	let height = u32::try_from(ctx.bitcoind().get_block_count().await).unwrap();
	assert!(height < expiry);
	ctx.generate_blocks(expiry + 1 - height).await;
	rebuilt(ctx, srv, db).await;
	let ids = leaves.iter().map(|leaf| leaf.coin.id().to_string()).collect::<Vec<_>>();
	let core = ctx.bitcoind().sync_client();
	let rows = tokio::time::timeout(Duration::from_secs(40), async {
		for _ in 0..20 {
			let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,c.gross_sat,c.fee_sat,c.payout_sat, \
				c.forfeited_sat,c.spk,c.owner_mailbox,c.user_pubkey,a.id,a.sweep_tx,a.sweep_txid \
				FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id \
				JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=ANY($1) \
				AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL", &[&ids],
			).await?)).await.unwrap();
			assert!(rows.len() <= leaves.len());
			if rows.len() == leaves.len() && rows.iter().all(|row|
				core.custom_get_raw_transaction_info(row.get::<_, &str>("sweep_txid").parse().unwrap(), None).unwrap().is_some()) {
				return rows;
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
			ctx.generate_blocks(1).await;
			rebuilt(ctx, srv, db).await;
			assert!(ctx.bitcoind().get_block_count().await - start <= 90);
		}
		panic!("native allocations did not publish within20 bounded progress blocks");
	}).await.expect("bounded automatic expiry publication");
	ctx.generate_blocks(6).await;
	rebuilt(ctx, srv, db).await;
	let mut result = Epoch { parents: BTreeMap::new(), children: BTreeMap::new(),
		allocations: BTreeMap::new(), outputs: BTreeMap::new(), hashes: BTreeMap::new(), heights: BTreeMap::new(), packages: Vec::new() };
	for row in rows {
		let id = row.get::<_, &str>("vtxo_id").parse::<VtxoId>().unwrap();
		let leaf = leaves.iter().find(|leaf| leaf.coin.id() == id).unwrap();
		assert_eq!(row.get::<_, i64>("gross_sat"), leaf.coin.amount().to_sat() as i64);
		assert_eq!(row.get::<_, i64>("fee_sat"), leaf.fee as i64);
		assert_eq!(row.get::<_, i64>("payout_sat"), leaf.payout as i64);
		assert_eq!(row.get::<_, i64>("forfeited_sat"), leaf.forfeited as i64);
		assert_eq!(row.get::<_, String>("spk"), leaf.script.as_bytes().to_lower_hex_string());
		assert_eq!(row.get::<_, String>("owner_mailbox"), leaf.mailbox.as_pubkey().to_string());
		assert_eq!(row.get::<_, String>("user_pubkey"), leaf.coin.user_pubkey().to_string());
		let parent = deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
		let txid = parent.compute_txid();
		assert_eq!(row.get::<_, String>("sweep_txid"), txid.to_string());
		assert_eq!(ctx.bitcoind().await_transaction(txid).await, parent);
		let info = core.custom_get_raw_transaction_info(txid, None).unwrap().unwrap();
		assert!(info.confirmations.unwrap_or(0) >= 6);
		let hash = info.blockhash.unwrap();
		let height = bitcoin_ext::BlockHeight::new(u32::try_from(core.get_block_header_info(&hash).unwrap().height).unwrap());
		assert_eq!(core.get_block_hash(u64::from(height.to_u32())).unwrap(), hash);
		let path = std::iter::once(leaf.coin.chain_anchor()).chain(leaf.coin.transactions().map(|step|
			OutPoint::new(step.tx.compute_txid(), step.output_idx as u32))).collect::<BTreeSet<_>>();
		let clause = ark::tree::signed::expiry_clause(leaf.coin.server_pubkey(), leaf.coin.expiry_height());
		assert!(parent.input.iter().any(|input| path.contains(&input.previous_output)
			&& input.witness.taproot_leaf_script().is_some_and(|script|
				script.version == bitcoin::taproot::LeafVersion::TapScript && script.script == clause.as_script())));
		let allocation = row.get::<_, i64>("id");
		// A real dust allocation has NO payout association. Never replace
		// this LEFT JOIN by an inner join which silently drops the dust leaf.
		let ledger = db.read(async |tx| Ok(tx.query_one("SELECT a.status,a.confirmed_hash,a.confirmed_height,a.sweep_txid, \
			p.output_index,p.amount_sat FROM expiry_allocation_coin c \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
			LEFT JOIN expiry_allocation_payout p ON p.allocation_id=c.allocation_id AND p.spk=c.spk \
			WHERE e.vtxo_id=$1 AND c.allocation_id=$2", &[&id.to_string(), &allocation],
		).await?)).await.unwrap();
		assert_eq!(ledger.get::<_, String>("status"), "confirmed");
		assert_eq!(ledger.get::<_, String>("confirmed_hash"), hash.to_string());
		assert_eq!(ledger.get::<_, i64>("confirmed_height"), i64::from(height.to_u32()));
		assert_eq!(ledger.get::<_, String>("sweep_txid"), txid.to_string());
		let index = ledger.get::<_, Option<i32>>("output_index").map(|index| u32::try_from(index).unwrap());
		if leaf.payout == 0 {
			assert!(index.is_none());
			assert!(ledger.get::<_, Option<i64>>("amount_sat").is_none());
			assert!(parent.output.iter().all(|output| output.script_pubkey != leaf.script));
		} else {
			let index = index.unwrap();
			assert_eq!(ledger.get::<_, Option<i64>>("amount_sat"), Some(leaf.payout as i64));
			assert_eq!(parent.output[index as usize], TxOut { value: sat(leaf.payout), script_pubkey: leaf.script.clone() });
		}
		result.allocations.insert(id, allocation);
		result.outputs.insert(id, index.map(|index| OutPoint::new(txid, index)));
		result.hashes.insert(txid, hash);
		result.heights.insert(txid, height);
		result.parents.insert(txid, parent);
	}
	for (txid, parent) in &result.parents {
		assert_eq!(parent.version.0, 3);
		assert_eq!(actual_fee(ctx, parent).await, Amount::ZERO);
		let hash = result.hashes[txid];
		let block = core.get_block(&hash).unwrap();
		assert!(block.txdata.iter().any(|transaction| transaction == parent));
		let anchors = parent.output.iter().enumerate().filter(|(_, output)| output.is_p2a_fee_anchor())
			.map(|(vout, _)| OutPoint::new(*txid, vout as u32)).collect::<Vec<_>>();
		assert_eq!(anchors.len(), 1);
		let children = block.txdata.iter().filter(|transaction| transaction.input.iter()
			.any(|input| input.previous_output == anchors[0])).collect::<Vec<_>>();
		assert_eq!(children.len(), 1);
		let child = children[0];
		assert_eq!(child.version.0, 3);
		let info = core.custom_get_raw_transaction_info(child.compute_txid(), None).unwrap().unwrap();
		assert_eq!(info.blockhash, Some(hash));
		assert!(info.confirmations.unwrap_or(0) >= 6);
		let fee = actual_fee(ctx, child).await;
		assert!(fee > Amount::ZERO);
		result.packages.push(json!({"parent_txid": txid, "child_txid": child.compute_txid(),
			"canonical_hash": hash, "canonical_height": result.heights[txid].to_u32(),
			"parent_miner_fee_sat": 0, "actual_child_miner_fee_sat": fee.to_sat()}));
		result.children.insert(child.compute_txid(), child.clone());
	}
	assert!(ctx.bitcoind().get_block_count().await - start <= 96);
	result
}

async fn receipt(wallet: &Wallet, leaf: &ExpectedLeaf, epoch: &Epoch) -> Option<OutPoint> {
	let point = epoch.outputs[&leaf.coin.id()];
	assert_eq!(leaf.coin.amount(), sat(leaf.fee + leaf.payout + leaf.forfeited));
	if let Some(point) = point {
		let parent = &epoch.parents[&point.txid];
		let output = &parent.output[point.vout as usize];
		assert_eq!(output.script_pubkey, leaf.script);
		assert_eq!(output.value, sat(leaf.payout));
		assert_eq!(wallet.verify_fallback_output(point).await.unwrap(), output.value);
	} else {
		assert_eq!(leaf.payout, 0);
		assert!(epoch.parents.values().all(|parent| parent.output.iter().all(|out| out.script_pubkey != leaf.script)));
	}
	assert_eq!(wallet.get_vtxo_by_id(leaf.coin.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(wallet.get_full_vtxo(leaf.coin.id()).await.unwrap().serialize(), leaf.coin.serialize());
	point
}

fn history(current: Vec<Movement>, baseline: &[Movement], leaves: &[&ExpectedLeaf]) -> BTreeMap<VtxoId, Vec<Movement>> {
	for previous in baseline { assert!(current.contains(previous), "ordinary history must survive payout discovery"); }
	let mut result = BTreeMap::new();
	for leaf in leaves {
		let debits = |items: &[Movement]| items.iter().filter(|movement|
			movement.effective_balance.to_sat() < 0 && movement.input_vtxos.contains(&leaf.coin.id()))
			.cloned().collect::<Vec<_>>();
		let actual = debits(&current);
		assert_eq!(actual, debits(baseline), "chain discovery must not add another ordinary debit");
		result.insert(leaf.coin.id(), actual);
	}
	result
}

/// Canonical block-order reconstruction permits only the declared roots and
/// sponsors; historical branches are never added again as external principal.
async fn financial_cut(
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

/// A full-value SDK send must really register, deliver, and finalize before
/// its recipient can forward. The RPC proof is signed proof,
/// independent of the sender's local Spent flag.
async fn send_full(
	srv: &Captaind, db: &Db, sender: &Wallet,
	recipient: &Wallet, source: &Vtxo<Full>, board: &Transaction,
) -> (Vtxo<Full>, Movement) {
	assert_eq!(source.amount(), sat(100_000));
	assert_eq!(sender.spendable_vtxos().await.unwrap().iter().map(|coin| coin.id()).collect::<Vec<_>>(), vec![source.id()]);
	balances(recipient, Amount::ZERO).await;
	let address = recipient.new_address().await.unwrap();
	Box::pin(sender.send_arkoor_payment(&address, source.amount())).await.unwrap();
	assert!(sender.pending_arkoor_sends().await.unwrap().is_empty());
	assert_eq!(sender.get_vtxo_by_id(source.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(sender.get_full_vtxo(source.id()).await.unwrap().serialize(), source.serialize());
	balances(sender, Amount::ZERO).await;
	let movements = sender.history().await.unwrap();
	let sends = movements.iter().filter(|movement| movement.subsystem.name == "bark.arkoor"
		&& movement.subsystem.kind == "send" && movement.input_vtxos.contains(&source.id())).collect::<Vec<_>>();
	assert_eq!(sends.len(), 1);
	let sent = (*sends[0]).clone();
	assert_eq!(sent.status, MovementStatus::Successful);
	assert_eq!(sent.input_vtxos, vec![source.id()]);
	assert!(sent.output_vtxos.is_empty() && sent.exited_vtxos.is_empty(), "a full-value send has no change or exit");
	assert_eq!(sent.intended_balance.to_sat(), -100_000);
	assert_eq!(sent.effective_balance.to_sat(), -100_000);
	assert_eq!(sent.offchain_fee, Amount::ZERO);
	assert_eq!(sent.sent_to.len(), 1);
	assert_eq!(sent.sent_to[0].destination, PaymentMethod::from(address.clone()));
	assert_eq!(sent.sent_to[0].amount, sat(100_000));

	recipient.sync_mailbox().await.unwrap();
	let received = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(received.len(), 1, "mailbox success alone does not prove full signed-body hydration");
	let leaf = recipient.get_full_vtxo(received[0].id()).await.unwrap();
	assert_ne!(leaf.id(), source.id());
	assert_ne!(leaf.user_pubkey(), source.user_pubkey());
	assert_eq!(leaf.amount(), source.amount());
	assert_eq!(leaf.chain_anchor(), source.chain_anchor());
	assert_eq!(leaf.expiry_height(), source.expiry_height());
	assert_eq!(leaf.exit_delta(), source.exit_delta());
	assert_eq!(leaf.server_pubkey(), source.server_pubkey());
	assert_eq!(leaf.policy(), address.policy());
	leaf.validate(board).unwrap();
	let _actual_local_key = recipient.get_vtxo_key(&leaf).await.unwrap();
	assert_eq!(recipient.get_vtxo_by_id(leaf.id()).await.unwrap().state, VtxoState::Spendable);
	balances(recipient, sat(100_000)).await;
	let received_history = recipient.history().await.unwrap();
	let arrivals = received_history.iter().filter(|movement| movement.subsystem.name == "bark.arkoor"
		&& movement.subsystem.kind == "receive" && movement.output_vtxos.contains(&leaf.id())).collect::<Vec<_>>();
	assert_eq!(arrivals.len(), 1);
	assert_eq!(arrivals[0].status, MovementStatus::Successful);
	assert_eq!(arrivals[0].output_vtxos, vec![leaf.id()]);
	assert_eq!(arrivals[0].effective_balance.to_sat(), 100_000);

	let key = sender.get_vtxo_key(source).await.unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let proof = rpc.get_vtxo_spend_proof(protos::GetVtxoStatusRequest {
		vtxo_id: source.id().to_bytes().to_vec(),
		attestation: VtxoStatusAttestation::new(source.id(), &key).serialize(),
	}).await.unwrap().into_inner();
	assert_eq!(proof.vtxos, vec![leaf.serialize()], "one full-value ordinary branch must cover all original principal");
	let prefix = source.transactions().collect::<Vec<_>>();
	let path = leaf.transactions().collect::<Vec<_>>();
	assert_eq!(path.len(), prefix.len() + 2);
	assert_eq!(&path[..prefix.len()], prefix.as_slice());
	let checkpoint = &path[prefix.len()].tx;
	assert_eq!(checkpoint.input.len(), 1);
	assert_eq!(checkpoint.input[0].previous_output, source.point());
	assert_eq!(checkpoint.output.len(), 2);
	assert_eq!(checkpoint.output[0].value, source.amount());
	assert!(checkpoint.output[1].is_p2a_fee_anchor());
	let final_tx = &path[prefix.len() + 1].tx;
	assert_eq!(final_tx.input.len(), 1);
	assert_eq!(final_tx.input[0].previous_output, OutPoint::new(checkpoint.compute_txid(), 0));
	assert_eq!(final_tx.output.len(), 2);
	assert_eq!(final_tx.output[0], leaf.txout());
	assert!(final_tx.output[1].is_p2a_fee_anchor());
	for coin in [source, &leaf] {
		let stored = db.read(async |tx| tx.get_user_vtxo_by_id(coin.id()).await).await.unwrap();
		assert_eq!(stored.vtxo.serialize(), coin.serialize());
	}
	(leaf, sent)
}

async fn predecessor(
	wallet: &Wallet, coin: &Vtxo<Full>, baseline: &[Movement], sent: &Movement,
) {
	wallet.sync_fallback_payouts().await.unwrap();
	balances(wallet, Amount::ZERO).await;
	assert!(wallet.pending_arkoor_sends().await.unwrap().is_empty());
	assert_eq!(wallet.get_vtxo_by_id(coin.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(wallet.get_full_vtxo(coin.id()).await.unwrap().serialize(), coin.serialize());
	let current = wallet.history().await.unwrap();
	assert_eq!(current, baseline, "a predecessor retains its ordinary send history and acquires no fallback debit");
	assert_eq!(current.iter().find(|movement| movement.id == sent.id), Some(sent));
	assert!(current.iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout"));
}

#[tokio::test]
async fn only_final_owner_receives_after_two_full_value_forwards() {
	require_bitcoind_chain_source!();
	assert!(std::env::var_os("BARK_DOUBLE_DRIVE_ACTIONS").is_none());
	tokio::time::timeout(Duration::from_secs(180), Box::pin(three_owners())).await
		.expect("two genuine sends and one expiry must fit the unchanged180-second deadline");
}

async fn three_owners() {
	let ctx = TestContext::new("server/expiry_three_owner_forwarding").await;
	let start = ctx.bitcoind().get_block_count().await;
	let srv = Box::pin(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(2);
	}).watchmand().create()).await;
	let a_cli = Box::pin(ctx.bark("original_a", &srv).cfg(|config| config.daemon_manual_sync = true)
		.funded(sat(200_000)).create()).await;
	let b_cli = Box::pin(ctx.bark("original_b", &srv).cfg(|config| config.daemon_manual_sync = true).create()).await;
	let c_cli = Box::pin(ctx.bark("original_c", &srv).cfg(|config| config.daemon_manual_sync = true).create()).await;
	let ids = Box::pin(a_cli.board_and_confirm_and_register(&ctx, sat(100_000))).await;
	assert_eq!(ids.len(), 1);
	let a = Box::pin(a_cli.client()).await;
	let b = Box::pin(b_cli.client()).await;
	let c = Box::pin(c_cli.client()).await;
	for wallet in [&a, &b, &c] { wallet.stop_daemon_wait().await.unwrap(); }
	let source = a.get_full_vtxo(ids[0]).await.unwrap();
	assert_eq!(source.amount(), sat(100_000));
	let root = source.chain_anchor();
	let board = ctx.bitcoind().await_transaction(root.txid).await;
	source.validate(&board).unwrap();
	let ordinary = a_cli.onchain_utxos().await.into_iter().map(|coin| (coin.outpoint, coin.amount)).collect::<BTreeMap<_, _>>();
	assert!(!ordinary.is_empty());
	assert!(ordinary.keys().all(|point| point.txid == root.txid && *point != root));
	let board_fee = actual_fee(&ctx, &board).await;
	assert_eq!(sat(100_000) + ordinary.values().copied().sum::<Amount>() + board_fee, sat(200_000));
	let a_binding = a.fallback_destination().await.unwrap();
	let b_binding = b.fallback_destination().await.unwrap();
	let c_binding = c.fallback_destination().await.unwrap();
	let mailboxes = [a.mailbox_identifier(), b.mailbox_identifier(), c.mailbox_identifier()];
	let bindings = [&a_binding, &b_binding, &c_binding];
	assert_eq!(mailboxes.iter().map(|mailbox| mailbox.as_pubkey().to_string()).collect::<BTreeSet<_>>().len(), 3);
	assert_eq!(bindings.iter().map(|record| record.spk.clone()).collect::<BTreeSet<_>>().len(), 3);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	for (mailbox, binding) in mailboxes.iter().zip(&bindings) {
		binding.verify(Network::Regtest, mailbox.as_pubkey()).unwrap();
		let stored = db.read(async |tx| tx.get_fallback(mailbox.as_pubkey()).await).await.unwrap().unwrap();
		assert!(stored.same_binding(binding));
		for input in &board.input {
			let funding = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
			assert!(funding.output.iter().all(|output| output.script_pubkey != binding.spk),
				"ordinary funding must not be mistaken for fallback payment");
		}
	}
	let (middle, a_send) = Box::pin(send_full(&srv, &db, &a, &b, &source, &board)).await;
	let (last, b_send) = Box::pin(send_full(&srv, &db, &b, &c, &middle, &board)).await;
	assert_eq!([source.user_pubkey(), middle.user_pubkey(), last.user_pubkey()].into_iter().collect::<BTreeSet<_>>().len(), 3);
	assert_eq!(last.chain_anchor(), root);
	assert_eq!(last.expiry_height(), source.expiry_height());
	let ancestors = last.ancestor_ids().into_iter().collect::<BTreeSet<_>>();
	assert!(ancestors.contains(&source.id()) && ancestors.contains(&middle.id()));
	assert!(last.transactions().len() <= 10);
	let expected = ExpectedLeaf { coin: last.clone(), mailbox: mailboxes[2], script: c_binding.spk.clone(),
		fee: 1_000, payout: 99_000, forfeited: 0 };
	balances(&a, Amount::ZERO).await;
	balances(&b, Amount::ZERO).await;
	balances(&c, sat(100_000)).await;
	let baselines = [a.history().await.unwrap(), b.history().await.unwrap(), c.history().await.unwrap()];
	assert!(baselines.iter().all(|items| items.iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout")));
	for wallet in [&a, &b, &c] {
		assert!(wallet.pending_arkoor_sends().await.unwrap().is_empty());
		wallet.stop_daemon_wait().await.unwrap();
	}
	drop(a);
	drop(b);
	drop(c);

	let core = ctx.bitcoind().sync_client();
	let mut sponsors = BTreeMap::new();
	for _ in 0..2 {
		let address = srv.watchmand().wait_wallet_address().await;
		let txid = ctx.bitcoind().fund_addr(address.clone(), sat(100_000)).await;
		let transaction = ctx.bitcoind().await_transaction(txid).await;
		let points = transaction.output.iter().enumerate().filter(|(_, output)| output.value == sat(100_000)
			&& output.script_pubkey == address.script_pubkey()).collect::<Vec<_>>();
		assert_eq!(points.len(), 1);
		assert!(sponsors.insert(OutPoint::new(txid, points[0].0 as u32), points[0].1.clone()).is_none());
	}
	ctx.generate_blocks(6).await;
	for (point, expected) in &sponsors {
		let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, expected.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), expected.script_pubkey);
	}
	let backing = core.get_tx_out(&root.txid, root.vout, Some(true)).unwrap().unwrap();
	assert!(backing.confirmations >= 6);
	assert_eq!(backing.value, sat(100_000));
	let scan_height = core.get_block_count().unwrap();
	assert!(u32::try_from(scan_height).unwrap() < last.expiry_height().to_u32());
	let epoch = Box::pin(confirm_epoch(&ctx, &srv, &db, std::slice::from_ref(&expected), start)).await;
	assert_eq!(epoch.parents.len(), 1);
	assert_eq!(epoch.children.len(), 1);
	let operator = {
		let config = srv.watchmand().config();
		config.sweep_address.clone().unwrap().require_network(config.network).unwrap().script_pubkey()
	};
	let parent = epoch.parents.values().next().unwrap();
	assert_eq!(parent.output.iter().map(|output| output.value).sum::<Amount>(), sat(100_000));
	let retained = parent.output.iter().filter(|output| output.script_pubkey != c_binding.spk).collect::<Vec<_>>();
	assert_eq!(retained.iter().map(|output| output.value).sum::<Amount>(), sat(1_000));
	assert!(retained.iter().all(|output| output.script_pubkey == operator || output.is_p2a_fee_anchor()));
	let roots = BTreeMap::from([(root, board.output[root.vout as usize].clone())]);
	let payees = [(a_binding.spk.clone(), None), (b_binding.spk.clone(), None), (c_binding.spk.clone(), Some(99_000))];
	let ids = BTreeSet::from([last.id()]);
	let cut = Box::pin(financial_cut(&ctx, &srv, &db, scan_height, &roots, &sponsors, &[&epoch], &payees, &ids)).await;
	assert_eq!(cut["selected_input_sat"], 300_000);
	let forbidden = ancestors.iter().map(ToString::to_string).collect::<Vec<_>>();
	let old_allocations = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=ANY($1) AND c.disposition='customer'",
		&[&forbidden]).await?)).await.unwrap();
	assert!(old_allocations.is_empty(), "no generation may allocate predecessor or signed-path ancestor principal to a customer");

	let returned_a = Box::pin(a_cli.client()).await;
	let returned_b = Box::pin(b_cli.client()).await;
	let returned_c = Box::pin(c_cli.client()).await;
	for (wallet, binding, mailbox) in [(&returned_a, &a_binding, mailboxes[0]), (&returned_b, &b_binding, mailboxes[1]), (&returned_c, &c_binding, mailboxes[2])] {
		assert_eq!(wallet.mailbox_identifier(), mailbox);
		assert!(wallet.fallback_destination().await.unwrap().same_binding(binding));
	}
	predecessor(&returned_a, &source, &baselines[0], &a_send).await;
	predecessor(&returned_b, &middle, &baselines[1], &b_send).await;
	returned_c.sync_fallback_payouts().await.unwrap();
	let final_receipt = receipt(&returned_c, &expected, &epoch).await;
	let final_history = history(returned_c.history().await.unwrap(), &baselines[2], &[&expected]);
	let unchanged_c_history = returned_c.history().await.unwrap().into_iter().filter(|movement|
		movement.subsystem.name != "abandon.fallback_payout").collect::<Vec<_>>();
	assert_eq!(unchanged_c_history, baselines[2]);
	balances(&returned_c, Amount::ZERO).await;
	for _ in 0..2 {
		predecessor(&returned_a, &source, &baselines[0], &a_send).await;
		predecessor(&returned_b, &middle, &baselines[1], &b_send).await;
		returned_c.sync_fallback_payouts().await.unwrap();
		assert_eq!(receipt(&returned_c, &expected, &epoch).await, final_receipt);
		assert_eq!(history(returned_c.history().await.unwrap(), &baselines[2], &[&expected]), final_history);
		balances(&returned_c, Amount::ZERO).await;
	}
	for wallet in [&returned_a, &returned_b, &returned_c] { wallet.stop_daemon_wait().await.unwrap(); }
	drop(returned_a);
	drop(returned_b);
	drop(returned_c);
	ctx.generate_blocks(1).await;
	rebuilt(&ctx, &srv, &db).await;
	let repeat_cut = Box::pin(financial_cut(&ctx, &srv, &db, scan_height, &roots, &sponsors, &[&epoch], &payees, &ids)).await;
	assert_eq!(repeat_cut, cut);
	let reopened_a = Box::pin(a_cli.client()).await;
	let reopened_b = Box::pin(b_cli.client()).await;
	let reopened_c = Box::pin(c_cli.client()).await;
	predecessor(&reopened_a, &source, &baselines[0], &a_send).await;
	predecessor(&reopened_b, &middle, &baselines[1], &b_send).await;
	reopened_c.sync_fallback_payouts().await.unwrap();
	assert!(reopened_c.fallback_destination().await.unwrap().same_binding(&c_binding));
	assert_eq!(receipt(&reopened_c, &expected, &epoch).await, final_receipt);
	assert_eq!(history(reopened_c.history().await.unwrap(), &baselines[2], &[&expected]), final_history);
	balances(&reopened_c, Amount::ZERO).await;
	assert!(ctx.bitcoind().get_block_count().await - start <= 96);
	for wallet in [&reopened_a, &reopened_b, &reopened_c] { wallet.stop_daemon_wait().await.unwrap(); }
}
