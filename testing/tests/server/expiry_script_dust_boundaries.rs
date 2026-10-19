//! Script-sensitive dust boundaries through original registered SDK accounts.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ark::attestations::VtxoStatusAttestation;
use ark::mailbox::MailboxIdentifier;
use ark::{ProtocolEncoding, Vtxo, VtxoId, SECP};
use ark::vtxo::Full;
use ark_testing::{require_bitcoind_chain_source, sat, Captaind, TestContext};
use ark_testing::daemon::captaind::WalletAdminClient;
use bark::{FallbackDisposition, FallbackEntitlement, FallbackPayoutStage, OpenWalletArgs, Wallet, WalletSeed};
use bark::lock_manager::memory::MemoryLockManager;
use bark::persist::sqlite::SqliteClient;
use bark::movement::{Movement, MovementStatus, PaymentMethod};
use bark::vtxo::VtxoState;
use bitcoin::{Address, Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, Txid, TxOut};
use bitcoin::consensus::deserialize;
use bitcoin::script::Builder;
use bitcoin::opcodes::all::OP_CHECKSIG;
use bitcoin::secp256k1::Keypair;
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
		"no live or finished unilateral exit may claim already settled principal");
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
				Ok(view.captaind == point && view.watchmand == point && tx.expiry_view_is_rebuilt(view).await?)
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
	group_payout: u64,
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
		let ledger = db.read(async |tx| Ok(tx.query_one("SELECT a.status,a.confirmed_hash,a.confirmed_height,c.paying_txid, \
			c.paying_hash,c.paying_height,p.output_index,p.amount_sat FROM expiry_allocation_coin c \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
			LEFT JOIN expiry_allocation_payout p ON p.allocation_id=c.allocation_id AND p.spk=c.spk \
			WHERE e.vtxo_id=$1 AND c.allocation_id=$2", &[&id.to_string(), &allocation],
		).await?)).await.unwrap();
		assert_eq!(ledger.get::<_, String>("status"), "confirmed");
		assert_eq!(ledger.get::<_, String>("confirmed_hash"), hash.to_string());
		assert_eq!(ledger.get::<_, i64>("confirmed_height"), i64::from(height.to_u32()));
		assert_eq!(ledger.get::<_, String>("paying_txid"), txid.to_string());
		assert_eq!(ledger.get::<_, String>("paying_hash"), hash.to_string());
		assert_eq!(ledger.get::<_, i64>("paying_height"), i64::from(height.to_u32()));
		let index = ledger.get::<_, Option<i32>>("output_index").map(|index| u32::try_from(index).unwrap());
		if leaf.payout == 0 {
			assert!(index.is_none());
			assert!(ledger.get::<_, Option<i64>>("amount_sat").is_none());
			assert!(parent.output.iter().all(|output| output.script_pubkey != leaf.script));
		} else {
			let index = index.unwrap();
			assert_eq!(ledger.get::<_, Option<i64>>("amount_sat"), Some(leaf.group_payout as i64));
			assert_eq!(parent.output[index as usize], TxOut { value: sat(leaf.group_payout), script_pubkey: leaf.script.clone() });
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
		let stored = db.read(async |tx| Ok(tx.query_one("SELECT raw_tx,fee_sat,confirmed_hash FROM expiry_fee_child WHERE txid=$1",
			&[&child.compute_txid().to_string()]).await?)).await.unwrap();
		assert_eq!(deserialize::<Transaction>(stored.get("raw_tx")).unwrap(), *child);
		assert_eq!(stored.get::<_, i64>("fee_sat"), fee.to_sat() as i64);
		assert_eq!(stored.get::<_, String>("confirmed_hash"), hash.to_string());
		result.packages.push(json!({"parent_txid": txid, "child_txid": child.compute_txid(),
			"canonical_hash": hash, "canonical_height": result.heights[txid].to_u32(),
			"parent_miner_fee_sat": 0, "actual_child_miner_fee_sat": fee.to_sat()}));
		result.children.insert(child.compute_txid(), child.clone());
	}
	assert!(ctx.bitcoind().get_block_count().await - start <= 96);
	result
}

async fn receipt(wallet: &Wallet, leaf: &ExpectedLeaf, epoch: &Epoch) -> FallbackEntitlement {
	let receipts = wallet.fallback_payouts().await.unwrap();
	let receipt = receipts.iter().find(|entry| entry.vtxo_id == leaf.coin.id()).unwrap().clone();
	assert_eq!(receipt.gross, leaf.coin.amount());
	assert!(receipt.chain_view_valid && receipt.terminal_claim.is_none());
	assert_eq!(receipt.attempts.len(), 1);
	let attempt = &receipt.attempts[0];
	assert!(attempt.active);
	assert_eq!(attempt.allocation_id, epoch.allocations[&leaf.coin.id()]);
	assert_eq!(attempt.stage, FallbackPayoutStage::Confirmed);
	assert_eq!(attempt.disposition, if leaf.payout == 0 { FallbackDisposition::BelowDust } else { FallbackDisposition::CustomerPayout });
	assert_eq!(attempt.gross, leaf.coin.amount());
	assert_eq!(attempt.fee, sat(leaf.fee));
	assert_eq!(attempt.payout, sat(leaf.payout));
	assert_eq!(attempt.forfeited, sat(leaf.forfeited));
	assert_eq!(attempt.script.as_ref(), Some(&leaf.script));
	let parent = attempt.parent.as_ref().unwrap();
	assert_eq!(epoch.parents.get(&parent.compute_txid()), Some(parent));
	assert_eq!(attempt.confirmation.as_ref().unwrap().hash, epoch.hashes[&parent.compute_txid()]);
	assert_eq!(attempt.confirmation.as_ref().unwrap().height, epoch.heights[&parent.compute_txid()]);
	assert_eq!(attempt.output_index, epoch.outputs[&leaf.coin.id()].map(|point| point.vout));
	assert_eq!(attempt.output_amount, (leaf.group_payout > 0).then_some(sat(leaf.group_payout)));
	assert_eq!(wallet.get_vtxo_by_id(leaf.coin.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(wallet.get_full_vtxo(leaf.coin.id()).await.unwrap().serialize(), leaf.coin.serialize());
	receipt
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

#[derive(Clone, Copy)]
enum Family { P2pkh, P2sh, P2wpkh, P2wsh, P2tr }

impl Family {
	fn label(self) -> &'static str {
		match self { Self::P2pkh => "p2pkh", Self::P2sh => "p2sh", Self::P2wpkh => "p2wpkh", Self::P2wsh => "p2wsh", Self::P2tr => "p2tr" }
	}

	fn dust(self) -> u64 {
		match self { Self::P2pkh => 546, Self::P2sh => 540, Self::P2wpkh => 294, Self::P2wsh | Self::P2tr => 330 }
	}

	fn script(self, key: &Keypair) -> ScriptBuf {
		let public = bitcoin::PublicKey::new(key.public_key());
		match self {
			Self::P2pkh => ScriptBuf::new_p2pkh(&public.pubkey_hash()),
			Self::P2sh => ScriptBuf::new_p2wpkh(&public.wpubkey_hash().unwrap()).to_p2sh(),
			Self::P2wpkh => ScriptBuf::new_p2wpkh(&public.wpubkey_hash().unwrap()),
			Self::P2wsh => Builder::new().push_key(&public).push_opcode(OP_CHECKSIG).into_script().to_p2wsh(),
			Self::P2tr => ScriptBuf::new_p2tr(&SECP, key.x_only_public_key().0, None),
		}
	}
}

/// App secrets stay in memory. Only the public payout binding is registered;
/// this fixture never claims spending or custody of these external outputs.
struct Application {
	seed: [u8; 64],
	database: PathBuf,
	script: ScriptBuf,
}

impl Application {
	fn new(ctx: &TestContext, family: Family, index: usize) -> Self {
		let seed = bip39::Mnemonic::generate(12).unwrap().to_seed("");
		let spend_key = Keypair::new(&SECP, &mut bitcoin::secp256k1::rand::thread_rng());
		let script = family.script(&spend_key);
		let address = Address::from_script(&script, Network::Regtest).unwrap();
		assert_eq!(address.script_pubkey(), script);
		assert_eq!(script.minimal_non_dust(), sat(family.dust()));
		Self { seed, script, database: ctx.datadir.join(format!("recipient_{index}.sqlite")) }
	}

	async fn open(&self, ctx: &TestContext, srv: &Captaind, create: bool) -> Wallet {
		assert_eq!(self.database.exists(), !create, "reopen must load the original database");
		let mut config = ctx.bark_default_cfg(&*srv, Some(ctx.bitcoind()));
		config.daemon_manual_sync = true;
		assert_eq!(config.change_vtxo_split_factor, 2);
		Wallet::open(Network::Regtest, WalletSeed::new_from_seed(Network::Regtest, &self.seed), config,
			OpenWalletArgs {
				persister: Some(Arc::new(SqliteClient::open(&self.database).unwrap())),
				lock_manager: Some(Box::new(MemoryLockManager::new())),
				fallback_spk: Some(self.script.clone()), onchain: None,
				create_if_not_exists: create, run_daemon: false, ..Default::default()
			},
		).await.unwrap()
	}
}

fn group_history(
	current: Vec<Movement>, baseline: &[Movement], leaves: &[&ExpectedLeaf],
) -> Movement {
	let ids = leaves.iter().map(|leaf| leaf.coin.id()).collect::<BTreeSet<_>>();
	let gross = leaves.iter().map(|leaf| leaf.coin.amount().to_sat()).sum::<u64>();
	let fee = leaves.iter().map(|leaf| leaf.fee).sum::<u64>();
	let payout = leaves.iter().map(|leaf| leaf.payout).sum::<u64>();
	let forfeited = leaves.iter().map(|leaf| leaf.forfeited).sum::<u64>();
	assert!(leaves.iter().all(|leaf| leaf.script == leaves[0].script && leaf.group_payout == payout));
	let ordinary = current.iter().filter(|movement| movement.subsystem.name != "abandon.fallback_payout").cloned().collect::<Vec<_>>();
	assert_eq!(ordinary, baseline, "returning SDK must retain actual ordinary receive/send history");
	let other_debits = |items: &[Movement]| items.iter().filter(|movement|
		movement.subsystem.name != "abandon.fallback_payout" && movement.effective_balance.to_sat() < 0
		&& movement.input_vtxos.iter().any(|id| ids.contains(id)))
		.map(|movement| (movement.id, movement.effective_balance.to_sat())).collect::<BTreeMap<_, _>>();
	assert_eq!(other_debits(&current), other_debits(baseline));
	let debits = current.iter().filter(|movement| movement.effective_balance.to_sat() < 0
		&& movement.input_vtxos.iter().any(|id| ids.contains(id))).map(|movement| movement.effective_balance.to_sat()).sum::<i64>();
	assert_eq!(debits, other_debits(baseline).values().sum::<i64>() - gross as i64);
	let fallback = current.into_iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout").collect::<Vec<_>>();
	assert_eq!(fallback.len(), 1);
	let movement = fallback.into_iter().next().unwrap();
	assert_eq!(movement.input_vtxos.iter().copied().collect::<BTreeSet<_>>(), ids);
	assert_eq!(movement.input_vtxos.len(), ids.len());
	assert_eq!(movement.status, MovementStatus::Successful);
	assert_eq!(movement.intended_balance.to_sat(), -(gross as i64));
	assert_eq!(movement.effective_balance, movement.intended_balance);
	assert_eq!(movement.offchain_fee, sat(fee));
	assert_eq!(movement.metadata["fallback"]["chain_verified"], true);
	assert_eq!(movement.metadata["fallback"]["forfeited_sat"], forfeited);
	assert_eq!(movement.metadata["fallback"]["payout_sat"], payout);
	if payout == 0 {
		assert_eq!(movement.metadata["fallback"]["stage"], "below_dust");
		assert_eq!(movement.metadata["fallback"]["disposition"], "below_dust");
		assert!(movement.metadata["fallback"]["output_index"].is_null());
		assert!(movement.sent_to.is_empty());
	} else {
		assert_eq!(movement.metadata["fallback"]["stage"], "confirmed");
		assert_eq!(movement.metadata["fallback"]["disposition"], "customer_payout");
		assert_eq!(movement.sent_to.len(), 1);
		assert_eq!(movement.sent_to[0].amount, sat(payout));
		assert_eq!(movement.sent_to[0].destination,
			PaymentMethod::Bitcoin(Address::from_script(&leaves[0].script, Network::Regtest).unwrap().into_unchecked()));
	}
	movement
}

/// Normal SDK sends, mailbox hydration, and a complete signed ordinary proof
/// authenticate every spent source. No manually fabricated registrations.
async fn receive_boundary(
	srv: &Captaind, db: &Db, sender: &Wallet, recipient: &Wallet,
	amount: Amount, board: &Transaction,
) -> (Vtxo<Full>, Vec<VtxoId>) {
	balances(recipient, Amount::ZERO).await;
	let before = sender.spendable_vtxos().await.unwrap();
	let before = {
		let mut full = BTreeMap::new();
		for coin in before { full.insert(coin.id(), sender.get_full_vtxo(coin.id()).await.unwrap()); }
		full
	};
	let total_before = before.values().map(|coin| coin.amount()).sum::<Amount>();
	let address = recipient.new_address().await.unwrap();
	Box::pin(sender.send_arkoor_payment(&address, amount)).await.unwrap();
	assert!(sender.pending_arkoor_sends().await.unwrap().is_empty());
	recipient.sync_mailbox().await.unwrap();
	let received = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(received.len(), 1, "a successful mailbox call alone is insufficient");
	let leaf = recipient.get_full_vtxo(received[0].id()).await.unwrap();
	assert_eq!(leaf.amount(), amount);
	assert_eq!(leaf.policy(), address.policy());
	leaf.validate(board).unwrap();
	let _real_leaf_key = recipient.get_vtxo_key(&leaf).await.unwrap();
	balances(recipient, amount).await;
	let mut after = BTreeMap::new();
	for coin in sender.spendable_vtxos().await.unwrap() {
		let full = sender.get_full_vtxo(coin.id()).await.unwrap();
		full.validate(board).unwrap();
		after.insert(full.id(), full);
	}
	assert_eq!(after.values().map(|coin| coin.amount()).sum::<Amount>() + amount, total_before);
	let consumed = before.values().filter(|coin| !after.contains_key(&coin.id())).collect::<Vec<_>>();
	assert_eq!(consumed.len(), 1, "all genuine change pieces here exceed the next small payment");
	let source = consumed[0];
	assert_eq!(sender.get_vtxo_by_id(source.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(sender.get_full_vtxo(source.id()).await.unwrap().serialize(), source.serialize());
	assert_eq!(leaf.chain_anchor(), source.chain_anchor());
	assert_eq!(leaf.expiry_height(), source.expiry_height());
	assert_eq!(leaf.exit_delta(), source.exit_delta());
	assert_eq!(leaf.server_pubkey(), source.server_pubkey());
	let new_change = after.values().filter(|coin| !before.contains_key(&coin.id())).collect::<Vec<_>>();
	let expected = std::iter::once(&leaf).chain(new_change.iter().copied()).collect::<Vec<_>>();
	assert_eq!(new_change.len(), 2, "retain the real default split factor2");
	assert_eq!(expected.iter().map(|coin| coin.amount()).sum::<Amount>(), source.amount());
	let key = sender.get_vtxo_key(source).await.unwrap();
	let proof = srv.get_public_rpc().await.get_vtxo_spend_proof(protos::GetVtxoStatusRequest {
		vtxo_id: source.id().to_bytes().to_vec(),
		attestation: VtxoStatusAttestation::new(source.id(), &key).serialize(),
	}).await.unwrap().into_inner();
	assert_eq!(proof.vtxos.iter().cloned().collect::<BTreeSet<_>>(),
		expected.iter().map(|coin| coin.serialize()).collect::<BTreeSet<_>>());
	assert_eq!(proof.vtxos.len(), expected.len());
	let prefix = source.transactions().collect::<Vec<_>>();
	for coin in expected {
		let path = coin.transactions().collect::<Vec<_>>();
		assert_eq!(path.len(), prefix.len() + 2);
		assert_eq!(&path[..prefix.len()], prefix.as_slice());
		let checkpoint = &path[prefix.len()].tx;
		assert_eq!(checkpoint.input.len(), 1);
		assert_eq!(checkpoint.input[0].previous_output, source.point());
		assert_eq!(checkpoint.output.len(), 4);
		assert!(checkpoint.output.last().unwrap().is_p2a_fee_anchor());
		assert_eq!(checkpoint.output.iter().map(|output| output.value).sum::<Amount>(), source.amount());
		let transaction = &path[prefix.len() + 1].tx;
		assert_eq!(transaction.input.len(), 1);
		assert_eq!(transaction.input[0].previous_output.txid, checkpoint.compute_txid());
		assert_eq!(transaction.output.len(), 2);
		assert_eq!(transaction.output[0], coin.txout());
		assert!(transaction.output[1].is_p2a_fee_anchor());
		let stored = db.read(async |tx| tx.get_user_vtxo_by_id(coin.id()).await).await.unwrap();
		assert_eq!(stored.vtxo.serialize(), coin.serialize());
	}
	let arrivals = recipient.history().await.unwrap();
	let arrivals = arrivals.iter().filter(|movement| movement.subsystem.name == "bark.arkoor"
		&& movement.subsystem.kind == "receive" && movement.output_vtxos.contains(&leaf.id())).collect::<Vec<_>>();
	assert_eq!(arrivals.len(), 1);
	assert_eq!(arrivals[0].status, MovementStatus::Successful);
	assert_eq!(arrivals[0].effective_balance.to_sat(), amount.to_sat() as i64);
	(leaf, vec![source.id()])
}

async fn verify_return(
	wallet: &Wallet, leaves: &[&ExpectedLeaf], epoch: &Epoch, baseline: &[Movement],
) -> (Vec<FallbackEntitlement>, Movement) {
	wallet.sync_fallback_payouts().await.unwrap();
	let mut receipts = Vec::new();
	for leaf in leaves { receipts.push(receipt(wallet, leaf, epoch).await); }
	balances(wallet, Amount::ZERO).await;
	assert!(wallet.pending_arkoor_sends().await.unwrap().is_empty());
	(receipts, group_history(wallet.history().await.unwrap(), baseline, leaves))
}

async fn boundaries(family: Family) {
	let ctx = TestContext::new(format!("server/expiry_script_dust_boundaries_{}", family.label())).await;
	let start = ctx.bitcoind().get_block_count().await;
	let srv = Box::pin(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(2);
	}).watchmand().create()).await;
	let donor_cli = Box::pin(ctx.bark("original_sender", &srv)
		.cfg(|config| config.daemon_manual_sync = true).funded(sat(200_000)).create()).await;
	let ids = Box::pin(donor_cli.board_and_confirm_and_register(&ctx, sat(100_000))).await;
	assert_eq!(ids.len(), 1);
	let sender = Box::pin(donor_cli.client()).await;
	assert_eq!(sender.config().change_vtxo_split_factor, 2);
	sender.stop_daemon_wait().await.unwrap();
	let source = sender.get_full_vtxo(ids[0]).await.unwrap();
	assert_eq!(source.amount(), sat(100_000));
	let root = source.chain_anchor();
	let board = ctx.bitcoind().await_transaction(root.txid).await;
	source.validate(&board).unwrap();
	let ordinary = donor_cli.onchain_utxos().await.into_iter().map(|coin| (coin.outpoint, coin.amount)).collect::<BTreeMap<_, _>>();
	assert!(!ordinary.is_empty() && ordinary.keys().all(|point| point.txid == root.txid && *point != root));
	let board_fee = actual_fee(&ctx, &board).await;
	assert_eq!(sat(100_000) + ordinary.values().copied().sum::<Amount>() + board_fee, sat(200_000));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let apps = (0..3).map(|index| Application::new(&ctx, family, index)).collect::<Vec<_>>();
	let mut recipients = Vec::new();
	for app in &apps { recipients.push(Box::pin(app.open(&ctx, &srv, true)).await); }
	let sender_binding = sender.fallback_destination().await.unwrap();
	let mut bindings = vec![sender_binding.clone()];
	let mut mailboxes = vec![sender.mailbox_identifier()];
	for (wallet, app) in recipients.iter().zip(&apps) {
		assert!(wallet.onchain().is_none());
		let binding = wallet.fallback_destination().await.unwrap();
		assert_eq!(binding.spk, app.script);
		assert_eq!(binding.account_xpub, wallet.vtxo_xpub());
		bindings.push(binding);
		mailboxes.push(wallet.mailbox_identifier());
	}
	assert_eq!(bindings.iter().map(|binding| binding.spk.clone()).collect::<BTreeSet<_>>().len(), 4);
	assert_eq!(bindings.iter().map(|binding| binding.account_xpub).collect::<BTreeSet<_>>().len(), 4);
	assert_eq!(mailboxes.iter().map(|mailbox| mailbox.as_pubkey().to_string()).collect::<BTreeSet<_>>().len(), 4);
	for (binding, mailbox) in bindings.iter().zip(&mailboxes) {
		binding.verify(Network::Regtest, mailbox.as_pubkey()).unwrap();
		let stored = db.read(async |tx| tx.get_fallback(mailbox.as_pubkey()).await).await.unwrap().unwrap();
		assert!(stored.same_binding(binding));
		for input in &board.input {
			let funding = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
			assert!(funding.output.iter().all(|output| output.script_pubkey != binding.spk));
		}
	}
	let dust = family.dust();
	let amounts = [999 + dust, 1_000 + dust, 1_001 + dust];
	let mut leaves = Vec::new();
	let mut old = BTreeSet::from([source.id()]);
	for (index, wallet) in recipients.iter().enumerate() {
		let (coin, consumed) = Box::pin(receive_boundary(&srv, &db, &sender, wallet, sat(amounts[index]), &board)).await;
		old.extend(consumed);
		let pays = index != 0;
		leaves.push(ExpectedLeaf { coin, mailbox: mailboxes[index + 1], script: apps[index].script.clone(),
			fee: if pays { 1_000 } else { 0 }, payout: if pays { dust + index as u64 - 1 } else { 0 },
			forfeited: if pays { 0 } else { amounts[index] }, group_payout: if pays { dust + index as u64 - 1 } else { 0 } });
	}
	let mut change = Vec::new();
	for coin in sender.spendable_vtxos().await.unwrap() { change.push(sender.get_full_vtxo(coin.id()).await.unwrap()); }
	change.sort_by_key(|coin| coin.id());
	assert_eq!(change.len(), 4);
	let change_gross = change.iter().map(|coin| coin.amount().to_sat()).sum::<u64>();
	assert_eq!(change_gross + amounts.iter().sum::<u64>(), 100_000);
	let mut fee = 1_000;
	for coin in change {
		let charged = coin.amount().to_sat().min(fee);
		fee -= charged;
		let payout = coin.amount().to_sat() - charged;
		leaves.push(ExpectedLeaf { coin, mailbox: mailboxes[0], script: sender_binding.spk.clone(), fee: charged,
			payout, forfeited: 0, group_payout: change_gross - 1_000 });
	}
	assert_eq!(fee, 0);
	assert_eq!(leaves.iter().map(|leaf| leaf.coin.amount()).sum::<Amount>(), sat(100_000));
	assert_eq!(leaves.iter().map(|leaf| leaf.fee).sum::<u64>(), 3_000);
	assert_eq!(leaves.iter().map(|leaf| leaf.forfeited).sum::<u64>(), amounts[0]);
	assert_eq!(leaves.iter().map(|leaf| leaf.payout).sum::<u64>(), 96_001 - dust);
	balances(&sender, sat(change_gross)).await;
	for leaf in &leaves {
		assert_eq!(leaf.coin.chain_anchor(), root);
		leaf.coin.validate(&board).unwrap();
		assert!(leaf.coin.transactions().len() <= 8);
		old.extend(leaf.coin.ancestor_ids());
	}
	let current = leaves.iter().map(|leaf| leaf.coin.id()).collect::<BTreeSet<_>>();
	old.retain(|id| !current.contains(id));
	let baselines = std::iter::once(&sender).chain(&recipients).map(|wallet| wallet.history());
	let mut history_before = Vec::new();
	for pending in baselines { history_before.push(pending.await.unwrap()); }
	assert!(history_before.iter().all(|history| history.iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout")));
	for wallet in std::iter::once(&sender).chain(&recipients) { wallet.stop_daemon_wait().await.unwrap(); }
	drop(sender);
	drop(recipients);
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
	let scan = core.get_block_count().unwrap();
	let epoch = Box::pin(confirm_epoch(&ctx, &srv, &db, &leaves, start)).await;
	assert_eq!(epoch.parents.len(), 1);
	assert_eq!(epoch.children.len(), 1);
	let parent = epoch.parents.values().next().unwrap();
	let payees = vec![(apps[0].script.clone(), None), (apps[1].script.clone(), Some(dust)),
		(apps[2].script.clone(), Some(dust + 1)), (sender_binding.spk.clone(), Some(change_gross - 1_000))];
	let operator = {
		let config = srv.watchmand().config();
		config.sweep_address.clone().unwrap().require_network(config.network).unwrap().script_pubkey()
	};
	assert!(payees.iter().all(|(script, _)| script != &operator));
	let remainder = parent.output.iter().filter(|output| payees.iter().all(|(script, _)| output.script_pubkey != *script)).collect::<Vec<_>>();
	assert_eq!(parent.output.iter().map(|output| output.value).sum::<Amount>(), sat(100_000));
	assert_eq!(remainder.iter().map(|output| output.value).sum::<Amount>(), sat(3_000 + amounts[0]));
	assert!(remainder.iter().all(|output| output.script_pubkey == operator || output.is_p2a_fee_anchor()));
	let roots = BTreeMap::from([(root, board.output[root.vout as usize].clone())]);
	let cut = Box::pin(financial_cut(&ctx, &srv, &db, scan, &roots, &sponsors, &[&epoch], &payees, &current)).await;
	assert_eq!(cut["selected_input_sat"], 300_000);
	let forbidden = old.iter().map(ToString::to_string).collect::<Vec<_>>();
	assert!(db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=ANY($1) AND c.disposition='customer'",
		&[&forbidden]).await?)).await.unwrap().is_empty());
	let returned_sender = Box::pin(donor_cli.client()).await;
	let sender_leaves = leaves.iter().filter(|leaf| leaf.mailbox == mailboxes[0]).collect::<Vec<_>>();
	let sender_result = verify_return(&returned_sender, &sender_leaves, &epoch, &history_before[0]).await;
	let mut results = Vec::new();
	let mut returned = Vec::new();
	for (index, app) in apps.iter().enumerate() {
		let wallet = Box::pin(app.open(&ctx, &srv, false)).await;
		assert!(wallet.fallback_destination().await.unwrap().same_binding(&bindings[index + 1]));
		assert_eq!(wallet.mailbox_identifier(), mailboxes[index + 1]);
		results.push(verify_return(&wallet, &[&leaves[index]], &epoch, &history_before[index + 1]).await);
		returned.push(wallet);
	}
	for _ in 0..2 {
		assert_eq!(verify_return(&returned_sender, &sender_leaves, &epoch, &history_before[0]).await, sender_result);
		for (index, wallet) in returned.iter().enumerate() {
			assert_eq!(verify_return(wallet, &[&leaves[index]], &epoch, &history_before[index + 1]).await, results[index]);
		}
	}
	for wallet in std::iter::once(&returned_sender).chain(&returned) { wallet.stop_daemon_wait().await.unwrap(); }
	drop(returned_sender);
	drop(returned);
	ctx.generate_blocks(1).await;
	rebuilt(&ctx, &srv, &db).await;
	assert_eq!(Box::pin(financial_cut(&ctx, &srv, &db, scan, &roots, &sponsors, &[&epoch], &payees, &current)).await,
		cut);
	let reopened_sender = Box::pin(donor_cli.client()).await;
	assert_eq!(verify_return(&reopened_sender, &sender_leaves, &epoch, &history_before[0]).await, sender_result);
	for (index, app) in apps.iter().enumerate() {
		let wallet = Box::pin(app.open(&ctx, &srv, false)).await;
		assert!(wallet.fallback_destination().await.unwrap().same_binding(&bindings[index + 1]));
		assert_eq!(verify_return(&wallet, &[&leaves[index]], &epoch, &history_before[index + 1]).await, results[index]);
		wallet.stop_daemon_wait().await.unwrap();
	}
	reopened_sender.stop_daemon_wait().await.unwrap();
	assert!(ctx.bitcoind().get_block_count().await - start <= 96);
}

async fn bounded(family: Family) {
	assert!(std::env::var_os("BARK_DOUBLE_DRIVE_ACTIONS").is_none());
	tokio::time::timeout(Duration::from_secs(180), Box::pin(boundaries(family))).await
		.expect("one family's three genuine boundaries must fit180seconds without retries");
}

#[tokio::test]
async fn p2pkh_exact_dust_boundary_is_paid_and_one_sat_below_is_forfeited() {
	require_bitcoind_chain_source!();
	bounded(Family::P2pkh).await;
}

#[tokio::test]
async fn p2sh_exact_dust_boundary_is_paid_and_one_sat_below_is_forfeited() {
	require_bitcoind_chain_source!();
	bounded(Family::P2sh).await;
}

#[tokio::test]
async fn p2wpkh_exact_dust_boundary_is_paid_and_one_sat_below_is_forfeited() {
	require_bitcoind_chain_source!();
	bounded(Family::P2wpkh).await;
}

#[tokio::test]
async fn p2wsh_exact_dust_boundary_is_paid_and_one_sat_below_is_forfeited() {
	require_bitcoind_chain_source!();
	bounded(Family::P2wsh).await;
}

#[tokio::test]
async fn p2tr_exact_dust_boundary_is_paid_and_one_sat_below_is_forfeited() {
	require_bitcoind_chain_source!();
	bounded(Family::P2tr).await;
}
