//! Public source inventory from the owning wallet, without wallet mutation.

use std::collections::{BTreeSet, HashSet};

use anyhow::Context;
use bdk_wallet::chain::ChainPosition;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{OutPoint, Transaction, Witness};
use prost::Message;
use server_rpc::protos;

use bitcoin_ext::bdk::{TrustedCanonicalization, KEYCHAIN};

use super::{PersistedWallet, WalletKind};

pub const MAX_WALLET_INVENTORY_ROWS: usize = 2_000;
pub const MAX_WALLET_INVENTORY_BYTES: usize = 2 * 1024 * 1024;

fn public_transaction(tx: &Transaction) -> anyhow::Result<protos::WalletInventoryTransaction> {
	// A legacy scriptSig can carry a preimage. Redacting it would change txid.
	ensure!(tx.input.iter().all(|i| i.script_sig.is_empty()), "wallet inventory refuses unsafe scriptSig body");
	let mut body = tx.clone();
	for input in &mut body.input { input.witness = Witness::default(); }
	Ok(protos::WalletInventoryTransaction {
		txid: tx.compute_txid().to_string(), signed_weight_wu: tx.weight().to_wu(),
		transaction_body: bitcoin::consensus::serialize(&body),
	})
}

fn account_row(
	rows: &mut usize, bytes: &mut usize, row_bytes: usize, max_rows: usize, max_bytes: usize,
) -> anyhow::Result<()> {
	ensure!(*rows < max_rows, "wallet inventory aggregate row budget exceeded");
	*rows += 1;
	// All repeated source fields have a one-byte protobuf tag.
	*bytes += 1 + prost::length_delimiter_len(row_bytes) + row_bytes;
	ensure!(*bytes <= max_bytes, "wallet inventory serialized byte budget exceeded");
	Ok(())
}

impl PersistedWallet {
	/// Caller holds the existing owning wallet guard. This observes its current
	/// canonical graph and reservation index, not future spend authorization.
	/// No address reveal, sync, commit, persistence or private-key export occurs.
	pub fn inventory(&self) -> anyhow::Result<protos::WalletInventory> {
		let locked = self.reserved_outputs();
		read_inventory(&self.wallet, self.kind, self.min_trusted_confs, &locked,
			MAX_WALLET_INVENTORY_ROWS, MAX_WALLET_INVENTORY_BYTES)
	}
}

fn read_inventory(
	wallet: &Wallet, kind: WalletKind, min_trusted_confs: u32, locked: &HashSet<OutPoint>,
	max_rows: usize, max_bytes: usize,
) -> anyhow::Result<protos::WalletInventory> {
	let canon = TrustedCanonicalization::from_wallet(wallet, min_trusted_confs);
	ensure!(locked.len() <= max_rows, "wallet inventory reservation row budget exceeded");
	let tip = wallet.latest_checkpoint().block_id();
	let mut inventory = protos::WalletInventory {
		wallet: kind.name().into(), network: wallet.network().to_string(),
		// BDK returns DescriptorPublicKey here, never its private signing keymap.
		public_descriptor: wallet.public_descriptor(KEYCHAIN).to_string(),
		tip_height: tip.height, tip_hash: tip.hash.to_string(),
		min_trusted_confirmations: min_trusted_confs,
		utxos: Vec::new(), complete: false,
		reserved_outpoints: locked.iter().map(|p| p.to_string()).collect(),
		last_revealed_index: wallet.derivation_index(KEYCHAIN),
		pending_transactions: Vec::new(), pending_parents: Vec::new(), unavailable_parent_txids: Vec::new(),
	};
	inventory.reserved_outpoints.sort();
	let mut rows = locked.len();
	let mut serialized_bytes = inventory.encoded_len();
	ensure!(serialized_bytes <= max_bytes, "wallet inventory serialized byte budget exceeded");
	for output in canon.list_unspent() {
		let tx = canon.get(output.outpoint.txid).context("canonical wallet UTXO has no transaction")?;
		let (keychain, index) = wallet.derivation_of_spk(output.txout.script_pubkey.clone())
			.context("canonical wallet UTXO has no derivation")?;
		let (height, hash, first_seen, last_seen, descendant, time) = match output.chain_position {
			ChainPosition::Confirmed { anchor, transitively } => (
				Some(anchor.block_id.height), Some(anchor.block_id.hash.to_string()), None, None,
				transitively.map(|id| id.to_string()), Some(anchor.confirmation_time),
			),
			ChainPosition::Unconfirmed { first_seen, last_seen } => (None, None, *first_seen, *last_seen, None, None),
		};
		ensure!(height.is_none_or(|height| height <= tip.height), "wallet inventory confirmation exceeds canonical tip");
		let confirmations = height.map(|height| tip.height - height + 1).unwrap_or(0);
		let coinbase = tx.tx.is_coinbase();
		let mature = !coinbase || confirmations >= bitcoin::constants::COINBASE_MATURITY;
		let reserved = locked.contains(&output.outpoint);
		let eligible = output.is_trusted && mature && !reserved;
		let utxo = protos::WalletInventoryUtxo {
			outpoint: output.outpoint.to_string(), value_sat: output.txout.value.to_sat(),
			script_pubkey: output.txout.script_pubkey.as_bytes().to_vec(),
			keychain: match keychain { KeychainKind::External => "external", KeychainKind::Internal => "internal" }.into(),
			derivation_index: index, confirmed_height_upper_bound: height, confirmation_block_hash: hash,
			unconfirmed_first_seen: first_seen, unconfirmed_last_seen: last_seen,
			coinbase, confirmations, mature, trusted: output.is_trusted, reserved,
			transaction_eligible: eligible, confirmed_fee_eligible: eligible && height.is_some(),
			confirming_descendant_txid: descendant, confirmation_time: time,
		};
		account_row(&mut rows, &mut serialized_bytes, utxo.encoded_len(), max_rows, max_bytes)?;
		inventory.utxos.push(utxo);
	}
	let mut parents = BTreeSet::new();
	for local in canon.transactions().filter(|t| !t.chain_position.is_confirmed()) {
		let tx = &local.tx;
		let ours = tx.output.iter().any(|o| wallet.is_mine(o.script_pubkey.clone())) ||
			tx.input.iter().any(|i| wallet.tx_graph().get_txout(i.previous_output)
				.is_some_and(|o| wallet.is_mine(o.script_pubkey.clone())));
		if !ours { continue; }
		let row = public_transaction(tx).context("unsafe pending wallet transaction")?;
		account_row(&mut rows, &mut serialized_bytes, row.encoded_len(), max_rows, max_bytes)?;
		inventory.pending_transactions.push(row);
		parents.extend(tx.input.iter().map(|i| i.previous_output.txid));
		ensure!(parents.len() <= max_rows - rows, "wallet inventory aggregate parent row budget exceeded");
	}
	for txid in parents {
		let row = wallet.tx_graph().get_tx(txid).and_then(|tx| public_transaction(&tx).ok());
		if let Some(row) = row {
			account_row(&mut rows, &mut serialized_bytes, row.encoded_len(), max_rows, max_bytes)?;
			inventory.pending_parents.push(row);
		} else {
			let id = txid.to_string();
			account_row(&mut rows, &mut serialized_bytes, id.len(), max_rows, max_bytes)?;
			inventory.unavailable_parent_txids.push(id);
		}
	}
	inventory.complete = inventory.unavailable_parent_txids.is_empty();
	inventory.utxos.sort_by(|a, b| a.outpoint.cmp(&b.outpoint));
	inventory.pending_transactions.sort_by(|a, b| a.txid.cmp(&b.txid));
	inventory.pending_parents.sort_by(|a, b| a.txid.cmp(&b.txid));
	ensure!(inventory.encoded_len() <= max_bytes, "wallet inventory serialized byte budget exceeded");
	Ok(inventory)
}


#[cfg(test)]
mod test {
	use super::*;
	use bdk_wallet::chain::BlockId;
	use bdk_wallet::test_utils::{get_test_tr_single_sig_xprv, insert_anchor, insert_checkpoint, insert_tx, receive_output_in_latest_block};
	use bitcoin::{Amount, BlockHash, Network, ScriptBuf, Sequence, TxIn, TxOut, Txid};
	use bdk_wallet::chain::ConfirmationBlockTime;
	use bitcoin::hashes::Hash;

	#[test]
	fn wallet_inventory_is_read_only_and_refuses_whole_budget_overruns() {
		let mut wallet = Wallet::create_single(get_test_tr_single_sig_xprv())
			.network(Network::Regtest).create_wallet_no_persist().unwrap();
		insert_checkpoint(&mut wallet, BlockId { height: 1_000, hash: BlockHash::all_zeros() });
		let point = receive_output_in_latest_block(&mut wallet, Amount::from_sat(20_000));
		let before = wallet.staged().cloned();
		let index = wallet.derivation_index(KEYCHAIN);
		let locks = HashSet::from([point]);
		let result = read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 2, 2_000_000).unwrap();
		assert!(wallet.staged() == before.as_ref(), "read must not change staged wallet state");
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
		assert_eq!(result.utxos.len(), 1);
		assert!(result.utxos[0].reserved);
		assert!(!result.utxos[0].transaction_eligible);
		assert!(!result.utxos[0].confirmed_fee_eligible);
		let bytes = result.encoded_len();
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 2, bytes).is_ok());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 2, bytes - 1).is_err());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 1, bytes).is_err());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 0, bytes).is_err());
		assert!(wallet.staged() == before.as_ref(), "even refusals must be read-only");
		assert_eq!(locks, HashSet::from([point]));
	}

	fn transaction(point: OutPoint, script: ScriptBuf) -> Transaction {
		Transaction {
			version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![TxIn { previous_output: point, script_sig: ScriptBuf::new(),
				sequence: Sequence::MAX, witness: Witness::from_slice(&[vec![42; 32], vec![43; 64]]) }],
			output: vec![TxOut { value: Amount::from_sat(10_000), script_pubkey: script }],
		}
	}

	#[test]
	fn public_transaction_strips_witness_without_changing_identity_and_refuses_scriptsig() {
		let mut tx = transaction(OutPoint::new(Txid::all_zeros(), 1), ScriptBuf::new());
		let before = tx.clone();
		let public = public_transaction(&tx).unwrap();
		let stripped: Transaction = bitcoin::consensus::deserialize(&public.transaction_body).unwrap();
		assert!(stripped.input.iter().all(|i| i.witness.is_empty()));
		assert_eq!(stripped.compute_txid().to_string(), public.txid);
		assert_eq!(public.signed_weight_wu, tx.weight().to_wu());
		assert!(tx.weight() > stripped.weight());
		assert!(!public.transaction_body.windows(32).any(|b| b == [42; 32]));
		assert_eq!(tx, before);
		tx.input[0].script_sig = ScriptBuf::from_bytes(vec![32].into_iter().chain([42; 32]).collect());
		assert!(public_transaction(&tx).is_err(), "redacting scriptSig would invent a different transaction");
	}

	#[test]
	fn missing_parent_is_explicit_and_entire_snapshot_budget_is_enforced() {
		let mut wallet = Wallet::create_single(get_test_tr_single_sig_xprv())
			.network(Network::Regtest).create_wallet_no_persist().unwrap();
		insert_checkpoint(&mut wallet, BlockId { height: 1_000, hash: BlockHash::all_zeros() });
		let script = wallet.reveal_next_address(KEYCHAIN).address.script_pubkey();
		let missing = Txid::from_byte_array([7; 32]);
		insert_tx(&mut wallet, transaction(OutPoint::new(missing, 0), script));
		let before = wallet.staged().cloned();
		let index = wallet.derivation_index(KEYCHAIN);
		let public = read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 3, 2_000_000).unwrap();
		assert!(!public.complete);
		assert_eq!(public.utxos.len(), 1);
		assert_eq!(public.pending_transactions.len(), 1);
		assert!(public.pending_parents.is_empty());
		assert_eq!(public.unavailable_parent_txids, vec![missing.to_string()]);
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2, 2_000_000).is_err());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 3, public.encoded_len()).is_ok());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 3, public.encoded_len() - 1).is_err());
		assert!(wallet.staged() == before.as_ref());
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
	}

	#[test]
	fn unsafe_parent_is_a_gap_but_unsafe_pending_refuses_the_whole_snapshot() {
		let mut wallet = Wallet::create_single(get_test_tr_single_sig_xprv())
			.network(Network::Regtest).create_wallet_no_persist().unwrap();
		let block = BlockId { height: 1_000, hash: BlockHash::all_zeros() };
		insert_checkpoint(&mut wallet, block);
		let script = wallet.reveal_next_address(KEYCHAIN).address.script_pubkey();
		let mut parent = transaction(OutPoint::new(Txid::from_byte_array([8; 32]), 0), script.clone());
		parent.input[0].script_sig = ScriptBuf::from_bytes(vec![32].into_iter().chain([42; 32]).collect());
		let id = parent.compute_txid();
		insert_tx(&mut wallet, parent);
		let before = wallet.staged().cloned();
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 10, 2_000_000).is_err());
		assert!(wallet.staged() == before.as_ref());
		insert_anchor(&mut wallet, id, ConfirmationBlockTime { block_id: block, confirmation_time: 0 });
		// A drained outgoing transaction is still included without any wallet change.
		insert_tx(&mut wallet, transaction(OutPoint::new(id, 0), ScriptBuf::new()));
		let before = wallet.staged().cloned();
		let public = read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2, 2_000_000).unwrap();
		assert!(public.utxos.is_empty());
		assert_eq!(public.pending_transactions.len(), 1);
		assert!(public.pending_parents.is_empty());
		assert_eq!(public.unavailable_parent_txids, vec![id.to_string()]);
		assert!(!public.complete);
		assert!(wallet.staged() == before.as_ref());
	}

}
