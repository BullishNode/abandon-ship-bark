//! Dust forfeiture cannot become principal in a later receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use ark::arkoor::{ArkoorDestination, package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse}};
use ark::mailbox::MailboxIdentifier;
use ark::{ProtocolEncoding, ServerVtxoPolicy, Vtxo, VtxoId, VtxoPolicy};
use ark::vtxo::Full;
use ark_testing::{require_bitcoind_chain_source, sat, Captaind, TestContext};
use ark_testing::daemon::captaind::WalletAdminClient;
use bark::{FallbackDisposition, FallbackEntitlement, FallbackPayoutStage, Wallet};
use bark::movement::{Movement, MovementStatus, PaymentMethod};
use bark::vtxo::VtxoState;
use bitcoin::{Address, Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, Txid, TxOut};
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

/// Use the production cosign, signed-chain registration and mailbox paths.
async fn transfer(
	ctx: &TestContext, srv: &Captaind, sender: &Wallet, recipient: &Wallet,
	source: &Vtxo<Full>, amount: Amount,
) -> Vec<Vtxo<Full>> {
	let recipient_key = recipient.derive_store_next_keypair().await.unwrap().0;
	let mut keys = vec![recipient_key];
	let mut owners = vec![recipient.mailbox_identifier()];
	let mut amounts = vec![amount];
	if source.amount() > amount {
		keys.push(sender.derive_store_next_keypair().await.unwrap().0);
		owners.push(sender.mailbox_identifier());
		amounts.push(source.amount() - amount);
	}
	let destinations = keys.iter().zip(&amounts).map(|(key, amount)| ArkoorDestination {
		total_amount: *amount, policy: VtxoPolicy::new_pubkey(key.public_key()),
	}).collect::<Vec<_>>();
	let key = sender.get_vtxo_key(source).await.unwrap();
	sender.register_vtxo_transactions_with_server([source]).await.unwrap();
	let builder = ArkoorPackageBuilder::new_with_checkpoints([source.clone()], destinations).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_arkoor_cosign(protos::ArkoorPackageCosignRequest::from(builder.cosign_request()))
		.await.unwrap().into_inner();
	let signed = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap();
	assert_eq!(signed.build_signed_internal_vtxos().iter()
		.filter(|(coin, _)| matches!(coin.policy(), ServerVtxoPolicy::Checkpoint(_))).count(), amounts.len());
	assert_eq!(signed.signed_virtual_transactions().len(), amounts.len() + 1);
	let leaves = signed.build_signed_vtxos();
	assert_eq!(leaves.len(), amounts.len());
	let board = ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
	for (index, leaf) in leaves.iter().enumerate() {
		assert_eq!(leaf.amount(), amounts[index]);
		assert_eq!(leaf.policy(), &VtxoPolicy::new_pubkey(keys[index].public_key()));
		assert_eq!(leaf.chain_anchor(), source.chain_anchor());
		assert_eq!(leaf.expiry_height(), source.expiry_height());
		leaf.validate(&board).unwrap();
	}
	sender.register_vtxo_transactions_with_server(&leaves).await.unwrap();
	let mut mailbox = srv.get_mailbox_public_rpc().await;
	let mailbox_key = srv.ark_info().await.mailbox_pubkey;
	for (index, leaf) in leaves.iter().enumerate() {
		let blinded = owners[index].to_blinded(mailbox_key, &keys[index]).unwrap();
		mailbox.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
			blinded_id: blinded.as_ref().to_vec(), vtxos: vec![leaf.serialize()],
		}).await.unwrap();
	}
	recipient.sync_mailbox().await.unwrap();
	sender.sync_mailbox().await.unwrap();
	// A manual signed package is not an SDK send action. Retire the source
	// only through the actual complete ordinary-spend proof, never SQL.
	sender.recover_vtxos([source.id()], None).await.unwrap();
	assert_eq!(sender.get_vtxo_by_id(source.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(sender.get_full_vtxo(source.id()).await.unwrap().serialize(), source.serialize());
	for (index, leaf) in leaves.iter().enumerate() {
		let wallet = if index == 0 { recipient } else { sender };
		assert_eq!(wallet.get_full_vtxo(leaf.id()).await.unwrap().serialize(), leaf.serialize());
		assert_eq!(wallet.get_vtxo_by_id(leaf.id()).await.unwrap().state, VtxoState::Spendable);
	}
	assert_eq!(recipient.spendable_vtxos().await.unwrap().iter().map(|coin| coin.id()).collect::<BTreeSet<_>>(),
		BTreeSet::from([leaves[0].id()]));
	assert_eq!(sender.spendable_vtxos().await.unwrap().iter().map(|coin| coin.id()).collect::<BTreeSet<_>>(),
		leaves.iter().skip(1).map(|leaf| leaf.id()).collect());
	leaves
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
			assert!(ctx.bitcoind().get_block_count().await - start <= 122);
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
			p.output_index,p.amount_sat FROM expiry_allocation_coin c \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
			LEFT JOIN expiry_allocation_payout p ON p.allocation_id=c.allocation_id AND p.spk=c.spk \
			WHERE e.vtxo_id=$1 AND c.allocation_id=$2", &[&id.to_string(), &allocation],
		).await?)).await.unwrap();
		assert_eq!(ledger.get::<_, String>("status"), "confirmed");
		assert_eq!(ledger.get::<_, String>("confirmed_hash"), hash.to_string());
		assert_eq!(ledger.get::<_, i64>("confirmed_height"), i64::from(height.to_u32()));
		assert_eq!(ledger.get::<_, String>("paying_txid"), txid.to_string());
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
	assert!(ctx.bitcoind().get_block_count().await - start <= 128);
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
	assert_eq!(attempt.output_amount, (leaf.payout > 0).then_some(sat(leaf.payout)));
	assert_eq!(wallet.get_vtxo_by_id(leaf.coin.id()).await.unwrap().state, VtxoState::Spent);
	assert_eq!(wallet.get_full_vtxo(leaf.coin.id()).await.unwrap().serialize(), leaf.coin.serialize());
	receipt
}

fn history(
	current: Vec<Movement>, baseline: &[Movement], leaves: &[&ExpectedLeaf],
) -> BTreeMap<VtxoId, Movement> {
	let ids = leaves.iter().map(|leaf| leaf.coin.id()).collect::<BTreeSet<_>>();
	let other_debits = |items: &[Movement]| items.iter().filter(|movement|
		movement.subsystem.name != "abandon.fallback_payout" && movement.effective_balance.to_sat() < 0
		&& movement.input_vtxos.iter().any(|id| ids.contains(id)))
		.map(|movement| (movement.id, movement.effective_balance.to_sat())).collect::<BTreeMap<_, _>>();
	let previous = other_debits(baseline);
	assert_eq!(other_debits(&current), previous);
	let gross = leaves.iter().map(|leaf| leaf.coin.amount().to_sat() as i64).sum::<i64>();
	let debit = current.iter().filter(|movement| movement.effective_balance.to_sat() < 0
		&& movement.input_vtxos.iter().any(|id| ids.contains(id)))
		.map(|movement| movement.effective_balance.to_sat()).sum::<i64>();
	assert_eq!(debit, previous.values().sum::<i64>() - gross);
	let mut result = BTreeMap::new();
	for movement in current.into_iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout") {
		assert_eq!(movement.input_vtxos.len(), 1);
		let id = movement.input_vtxos[0];
		let leaf = leaves.iter().find(|leaf| leaf.coin.id() == id).unwrap();
		assert_eq!(movement.status, MovementStatus::Successful);
		assert_eq!(movement.intended_balance.to_sat(), -(leaf.coin.amount().to_sat() as i64));
		assert_eq!(movement.effective_balance, movement.intended_balance);
		assert_eq!(movement.offchain_fee, sat(leaf.fee));
		assert_eq!(movement.metadata["fallback"]["chain_verified"], true);
		assert_eq!(movement.metadata["fallback"]["forfeited_sat"], leaf.forfeited);
		assert_eq!(movement.metadata["fallback"]["payout_sat"], leaf.payout);
		if leaf.payout == 0 {
			assert_eq!(movement.metadata["fallback"]["stage"], "below_dust");
			assert_eq!(movement.metadata["fallback"]["disposition"], "below_dust");
			assert!(movement.metadata["fallback"]["output_index"].is_null());
			assert!(movement.sent_to.is_empty());
		} else {
			assert_eq!(movement.metadata["fallback"]["stage"], "confirmed");
			assert_eq!(movement.metadata["fallback"]["disposition"], "customer_payout");
			assert_eq!(movement.sent_to.len(), 1);
			assert_eq!(movement.sent_to[0].amount, sat(leaf.payout));
			assert_eq!(movement.sent_to[0].destination,
				PaymentMethod::Bitcoin(Address::from_script(&leaf.script, Network::Regtest).unwrap().into_unchecked()));
		}
		assert!(result.insert(id, movement).is_none());
	}
	assert_eq!(result.keys().copied().collect::<BTreeSet<_>>(), ids);
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
		JOIN vtxo v ON v.vtxo_id=e.vtxo_id WHERE v.anchor_point=ANY($1) AND c.disposition='customer' AND a.status='confirmed'",
		&[&anchors]).await?)).await.unwrap();
	assert_eq!(rows.len(), expected_ids.len());
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

#[tokio::test]
async fn forfeited_small_receipt_does_not_revive_when_same_customer_receives_again() {
	require_bitcoind_chain_source!();
	assert!(std::env::var_os("BARK_DOUBLE_DRIVE_ACTIONS").is_none());
	tokio::time::timeout(Duration::from_secs(180), Box::pin(two_receipts())).await
		.expect("two expiry epochs exceeded 180 seconds");
}

async fn two_receipts() {
	let ctx = TestContext::new("server/expiry_dust_later_receipt").await;
	let start = ctx.bitcoind().get_block_count().await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(40);
		config.vtxo_exit_delta = BlockDelta::new(2);
	}).watchmand().create().await;
	let recipient_cli = ctx.bark("recipient", &srv).cfg(|config| config.daemon_manual_sync = true).create().await;
	let donor_cli = ctx.bark("first_donor", &srv).cfg(|config| config.daemon_manual_sync = true)
		.funded(sat(200_000)).create().await;
	let ids = donor_cli.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	assert_eq!(ids.len(), 1);
	let donor = donor_cli.client().await;
	let recipient = recipient_cli.client().await;
	donor.stop_daemon_wait().await.unwrap();
	recipient.stop_daemon_wait().await.unwrap();
	let source = donor.get_full_vtxo(ids[0]).await.unwrap();
	assert_eq!(source.amount(), sat(100_000));
	let binding = recipient.fallback_destination().await.unwrap();
	let donor_binding = donor.fallback_destination().await.unwrap();
	binding.verify(Network::Regtest, recipient.mailbox_identifier().as_pubkey()).unwrap();
	donor_binding.verify(Network::Regtest, donor.mailbox_identifier().as_pubkey()).unwrap();
	assert_ne!(binding.spk, donor_binding.spk);
	assert!(sat(1_200) > sat(1_000) && sat(200) > Amount::ZERO && sat(200) < binding.spk.minimal_non_dust());
	let first_board = ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
	source.validate(&first_board).unwrap();
	for input in &first_board.input {
		let funding = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		assert!(funding.output.iter().all(|output| output.script_pubkey != binding.spk && output.script_pubkey != donor_binding.spk),
			"ordinary funding addresses must differ from the fresh registered fallback scripts");
	}
	let first_ordinary = donor_cli.onchain_utxos().await.into_iter().map(|coin| (coin.outpoint, coin.amount)).collect::<BTreeMap<_, _>>();
	assert!(!first_ordinary.is_empty());
	assert!(first_ordinary.keys().all(|point| point.txid == source.chain_anchor().txid && *point != source.chain_anchor()));
	let first_board_fee = actual_fee(&ctx, &first_board).await;
	assert_eq!(sat(100_000) + first_ordinary.values().copied().sum::<Amount>() + first_board_fee, sat(200_000));
	let leaves = transfer(&ctx, &srv, &donor, &recipient, &source, sat(1_200)).await;
	assert_eq!(leaves.len(), 2);
	let old = ExpectedLeaf { coin: leaves[0].clone(), mailbox: recipient.mailbox_identifier(),
		script: binding.spk.clone(), fee: 0, payout: 0, forfeited: 1_200 };
	let change = ExpectedLeaf { coin: leaves[1].clone(), mailbox: donor.mailbox_identifier(),
		script: donor_binding.spk.clone(), fee: 1_000, payout: 97_800, forfeited: 0 };
	balances(&recipient, sat(1_200)).await;
	balances(&donor, sat(98_800)).await;
	let baseline = recipient.history().await.unwrap();
	drop(donor);
	drop(donor_cli);
	drop(recipient);

	let db = Db::connect(&srv.config().postgres).await.unwrap();
	for (mailbox, record) in [(old.mailbox, &binding), (change.mailbox, &donor_binding)] {
		let actual = db.read(async |tx| tx.get_fallback(mailbox.as_pubkey()).await).await.unwrap().unwrap();
		assert!(actual.same_binding(record));
	}
	let core = ctx.bitcoind().sync_client();
	let mut sponsors = BTreeMap::new();
	for _ in 0..4 {
		let address = srv.watchmand().wait_wallet_address().await;
		let txid = ctx.bitcoind().fund_addr(address.clone(), sat(100_000)).await;
		let tx = ctx.bitcoind().await_transaction(txid).await;
		let points = tx.output.iter().enumerate().filter(|(_, output)| output.value == sat(100_000)
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
	let scan_height = core.get_block_count().unwrap();
	let mut roots = BTreeMap::from([(source.chain_anchor(), first_board.output[source.chain_anchor().vout as usize].clone())]);
	let first = confirm_epoch(&ctx, &srv, &db, &[old.clone(), change], start).await;
	let change_id = leaves[1].id();
	let first_cut = financial_cut(&ctx, &srv, &db, scan_height, &roots, &sponsors, &[&first],
		&[(binding.spk.clone(), None), (donor_binding.spk.clone(), Some(97_800))], &BTreeSet::from([old.coin.id(), change_id])).await;
	assert_eq!(first_cut["selected_input_sat"], 500_000);
	let returned = recipient_cli.client().await;
	returned.sync_fallback_payouts().await.unwrap();
	assert_eq!(returned.fallback_payouts().await.unwrap().len(), 1);
	let old_receipt = receipt(&returned, &old, &first).await;
	let first_history = history(returned.history().await.unwrap(), &baseline, &[&old]);
	balances(&returned, Amount::ZERO).await;
	for _ in 0..2 {
		returned.sync_fallback_payouts().await.unwrap();
		assert_eq!(receipt(&returned, &old, &first).await, old_receipt);
		assert_eq!(history(returned.history().await.unwrap(), &baseline, &[&old]), first_history);
	}
	returned.stop_daemon_wait().await.unwrap();
	drop(returned);
	let recipient = recipient_cli.client().await;
	recipient.sync_fallback_payouts().await.unwrap();
	assert_eq!(receipt(&recipient, &old, &first).await, old_receipt);
	assert_eq!(history(recipient.history().await.unwrap(), &baseline, &[&old]), first_history);
	balances(&recipient, Amount::ZERO).await;

	// Only after genuine terminal dust proof AND SDK acceptance create a
	// disjoint later donor. Default operator sweep remains the captaind rounds
	// wallet, not the Core funding wallet, and no pool can recycle that return.
	let later_cli = ctx.bark("later_donor", &srv).cfg(|config| config.daemon_manual_sync = true)
		.funded(sat(200_000)).create().await;
	let later_ids = later_cli.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	assert_eq!(later_ids.len(), 1);
	let later = later_cli.client().await;
	later.stop_daemon_wait().await.unwrap();
	let later_source = later.get_full_vtxo(later_ids[0]).await.unwrap();
	assert_eq!(later_source.amount(), sat(100_000));
	assert_ne!(later_source.chain_anchor(), source.chain_anchor());
	assert!(later_source.expiry_height() > source.expiry_height());
	let later_binding = later.fallback_destination().await.unwrap();
	later_binding.verify(Network::Regtest, later.mailbox_identifier().as_pubkey()).unwrap();
	let actual_later = db.read(async |tx| tx.get_fallback(later.mailbox_identifier().as_pubkey()).await).await.unwrap().unwrap();
	assert!(actual_later.same_binding(&later_binding));
	assert_ne!(later_binding.spk, binding.spk);
	assert_ne!(later_binding.spk, donor_binding.spk);
	let second_board = ctx.bitcoind().await_transaction(later_source.chain_anchor().txid).await;
	let forbidden = first_cut["all_graph_outpoints"].as_array().unwrap().iter()
		.map(|point| point.as_str().unwrap().parse::<OutPoint>().unwrap()).collect::<BTreeSet<_>>();
	for input in &second_board.input {
		assert!(!forbidden.contains(&input.previous_output));
		let funding = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		assert!(funding.output.iter().all(|output| output.script_pubkey != binding.spk
			&& output.script_pubkey != donor_binding.spk && output.script_pubkey != later_binding.spk),
			"later ordinary funding must not create an apparent fallback payment");
		assert!(funding.input.iter().all(|input| !forbidden.contains(&input.previous_output)),
			"later source funding must not recycle first-cut operator/customer/watch returns");
	}
	let later_ordinary = later_cli.onchain_utxos().await.into_iter().map(|coin| (coin.outpoint, coin.amount)).collect::<BTreeMap<_, _>>();
	assert!(!later_ordinary.is_empty());
	assert!(later_ordinary.keys().all(|point| point.txid == later_source.chain_anchor().txid && *point != later_source.chain_anchor()));
	let later_board_fee = actual_fee(&ctx, &second_board).await;
	assert_eq!(sat(100_000) + later_ordinary.values().copied().sum::<Amount>() + later_board_fee, sat(200_000));
	assert!(roots.insert(later_source.chain_anchor(), second_board.output[later_source.chain_anchor().vout as usize].clone()).is_none());
	let later_leaves = transfer(&ctx, &srv, &later, &recipient, &later_source, sat(100_000)).await;
	assert_eq!(later_leaves.len(), 1);
	let new = ExpectedLeaf { coin: later_leaves[0].clone(), mailbox: recipient.mailbox_identifier(),
		script: binding.spk.clone(), fee: 1_000, payout: 99_000, forfeited: 0 };
	assert_ne!(new.coin.id(), old.coin.id());
	assert_ne!(new.coin.user_pubkey(), old.coin.user_pubkey());
	assert!(recipient.fallback_destination().await.unwrap().same_binding(&binding));
	assert_eq!(receipt(&recipient, &old, &first).await, old_receipt);
	assert_eq!(history(recipient.history().await.unwrap(), &baseline, &[&old]), first_history);
	balances(&recipient, sat(100_000)).await;
	balances(&later, Amount::ZERO).await;
	recipient.stop_daemon_wait().await.unwrap();
	drop(recipient);
	drop(later);
	drop(later_cli);
	let second = confirm_epoch(&ctx, &srv, &db, std::slice::from_ref(&new), start).await;
	assert_ne!(second.allocations[&new.coin.id()], first.allocations[&old.coin.id()]);
	let expected_ids = BTreeSet::from([old.coin.id(), change_id, new.coin.id()]);
	let payees = [(binding.spk.clone(), Some(99_000)), (donor_binding.spk.clone(), Some(97_800)), (later_binding.spk.clone(), None)];
	let final_cut = financial_cut(&ctx, &srv, &db, scan_height, &roots, &sponsors, &[&first, &second], &payees, &expected_ids).await;
	assert_eq!(final_cut["selected_input_sat"], 600_000);
	let final_wallet = recipient_cli.client().await;
	final_wallet.sync_fallback_payouts().await.unwrap();
	assert_eq!(final_wallet.fallback_payouts().await.unwrap().len(), 2);
	assert_eq!(receipt(&final_wallet, &old, &first).await, old_receipt);
	let new_receipt = receipt(&final_wallet, &new, &second).await;
	let final_history = history(final_wallet.history().await.unwrap(), &baseline, &[&old, &new]);
	assert_eq!(final_history[&old.coin.id()], first_history[&old.coin.id()]);
	balances(&final_wallet, Amount::ZERO).await;
	for _ in 0..2 {
		final_wallet.sync_fallback_payouts().await.unwrap();
		assert_eq!(receipt(&final_wallet, &old, &first).await, old_receipt);
		assert_eq!(receipt(&final_wallet, &new, &second).await, new_receipt);
		assert_eq!(history(final_wallet.history().await.unwrap(), &baseline, &[&old, &new]), final_history);
	}
	final_wallet.stop_daemon_wait().await.unwrap();
	drop(final_wallet);
	ctx.generate_blocks(1).await;
	rebuilt(&ctx, &srv, &db).await;
	let repeated_cut = financial_cut(&ctx, &srv, &db, scan_height, &roots, &sponsors, &[&first, &second], &payees, &expected_ids).await;
	assert_eq!(repeated_cut, final_cut);
	let reopened = recipient_cli.client().await;
	reopened.sync_fallback_payouts().await.unwrap();
	assert!(reopened.fallback_destination().await.unwrap().same_binding(&binding));
	assert_eq!(receipt(&reopened, &old, &first).await, old_receipt);
	assert_eq!(receipt(&reopened, &new, &second).await, new_receipt);
	assert_eq!(history(reopened.history().await.unwrap(), &baseline, &[&old, &new]), final_history);
	balances(&reopened, Amount::ZERO).await;
	assert!(ctx.bitcoind().get_block_count().await - start <= 128);
	reopened.stop_daemon_wait().await.unwrap();
}
