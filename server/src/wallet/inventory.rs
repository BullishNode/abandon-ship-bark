//! Public source inventory from the owning wallet, without wallet mutation.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Context;
use bdk_wallet::chain::ChainPosition;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{OutPoint, Transaction, Witness};
use bitcoin::consensus::serialize;
use prost::Message;
use server_rpc::protos;

use bitcoin_ext::bdk::{TrustedCanonicalization, KEYCHAIN};

use super::{PersistedWallet, WalletKind};

pub const MAX_WALLET_INVENTORY_ROWS: usize = 2_000;
pub const MAX_WALLET_INVENTORY_BYTES: usize = 2 * 1024 * 1024;

impl PersistedWallet {
	/// Caller holds the existing owning wallet guard. This observes its current
	/// canonical graph and reservation index, not future spend authorization.
	/// No address reveal, sync, commit, persistence or private-key export occurs.
	pub fn inventory(&self) -> anyhow::Result<protos::WalletInventory> {
		let locked = self.locked_outputs.utxos();
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
	let mut serialized_bytes = inventory.encoded_len();
	for output in canon.list_unspent() {
		ensure!(inventory.utxos.len() < max_rows, "wallet inventory UTXO row budget exceeded");
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
		let row_bytes = utxo.encoded_len();
		// Field 7's tag is one byte; use the exact protobuf length prefix.
		serialized_bytes += 1 + prost::length_delimiter_len(row_bytes) + row_bytes;
		ensure!(serialized_bytes <= max_bytes, "wallet inventory serialized byte budget exceeded");
		inventory.utxos.push(utxo);
	}
	inventory.utxos.sort_by(|a, b| a.outpoint.cmp(&b.outpoint));
	// A canonical pending spend hides its still-confirmed inputs from list_unspent.
	// Retain actual bodies/parents so an independent Core reader can reconstruct
	// the confirmed-tip view, without treating pending change as a second asset.
	let mut parents = BTreeMap::new();
	let mut unavailable = BTreeSet::new();
	for pending in wallet.transactions().filter(|tx| !tx.chain_position.is_confirmed()) {
		ensure!(inventory.pending_transactions.len() < max_rows, "wallet inventory pending row budget exceeded");
		let tx = pending.tx_node.tx;
		let row = public_transaction(&tx, max_bytes)?;
		add_row_bytes(&mut serialized_bytes, row.encoded_len(), max_bytes)?;
		inventory.pending_transactions.push(row);
		for input in &tx.input {
			let id = input.previous_output.txid;
			if parents.contains_key(&id) || unavailable.contains(&id) { continue; }
			ensure!(parents.len() + unavailable.len() < max_rows, "wallet inventory parent row budget exceeded");
			match wallet.tx_graph().get_tx(id) {
				Some(parent) if parent.input.iter().all(|input| input.script_sig.is_empty()) => {
					let row = public_transaction(&parent, max_bytes)?;
					add_row_bytes(&mut serialized_bytes, row.encoded_len(), max_bytes)?;
					parents.insert(id, row);
				},
				// A legacy scriptSig can contain a preimage; retain only its txid gap.
				_ => {
					add_row_bytes(&mut serialized_bytes, id.to_string().len(), max_bytes)?;
					unavailable.insert(id);
				},
			}
		}
	}
	inventory.pending_transactions.sort_by(|a, b| a.txid.cmp(&b.txid));
	inventory.pending_parents = parents.into_values().collect();
	inventory.unavailable_parent_txids = unavailable.into_iter().map(|id| id.to_string()).collect();
	let rows = inventory.utxos.len() + inventory.reserved_outpoints.len()
		+ inventory.pending_transactions.len() + inventory.pending_parents.len()
		+ inventory.unavailable_parent_txids.len();
	ensure!(rows <= max_rows, "wallet inventory aggregate row budget exceeded");
	inventory.complete = inventory.unavailable_parent_txids.is_empty();
	ensure!(inventory.encoded_len() <= max_bytes, "wallet inventory serialized byte budget exceeded");
	Ok(inventory)
}


fn add_row_bytes(bytes: &mut usize, row: usize, maximum: usize) -> anyhow::Result<()> {
	// The repeated inventory fields have one-byte tags (field numbers <16).
	*bytes = bytes.checked_add(1 + prost::length_delimiter_len(row) + row)
		.context("wallet inventory byte count overflow")?;
	ensure!(*bytes <= maximum, "wallet inventory serialized byte budget exceeded");
	Ok(())
}

fn public_transaction(tx: &Transaction, maximum: usize) -> anyhow::Result<protos::WalletInventoryTransaction> {
	// Removing scriptSig would alter the txid; exporting it could reveal secrets.
	ensure!(tx.input.iter().all(|input| input.script_sig.is_empty()),
		"wallet inventory transaction has nonempty scriptSig; public body export refused");
	ensure!(tx.base_size() <= maximum, "wallet inventory transaction byte budget exceeded");
	let mut body = tx.clone();
	for input in &mut body.input { input.witness = Witness::default(); }
	Ok(protos::WalletInventoryTransaction {
		txid: tx.compute_txid().to_string(), transaction_body: serialize(&body),
		signed_weight_wu: tx.weight().to_wu(),
	})
}


#[cfg(test)]
mod test {
	use super::*;
	use super::super::{LockedWalletUtxosIndex, WalletUtxoGuard};
	use bdk_wallet::chain::{BlockId, ConfirmationBlockTime};
	use bdk_wallet::test_utils::{get_test_tr_single_sig_xprv, insert_anchor, insert_checkpoint, insert_tx, receive_output_in_latest_block};
	use bitcoin::{Amount, BlockHash, FeeRate, Network, ScriptBuf, Sequence, TxIn, TxOut};
	use bitcoin::hashes::Hash;
	use bitcoin::consensus::deserialize;
	use bitcoin::script::Builder;

	// Synthetic fixture bytes only: never an actual exchange secret or preimage.
	const SCRIPT_SIG_SENTINEL: [u8; 32] = [0x5a; 32];

	fn script_sig_parent() -> (Wallet, Transaction) {
		let mut wallet = Wallet::create_single(get_test_tr_single_sig_xprv())
			.network(Network::Regtest).create_wallet_no_persist().unwrap();
		insert_checkpoint(&mut wallet, BlockId { height: 1_000, hash: BlockHash::all_zeros() });
		let destination = wallet.next_unused_address(KEYCHAIN).script_pubkey();
		let parent = Transaction {
			version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![TxIn {
				previous_output: OutPoint { txid: bitcoin::Txid::from_byte_array([3; 32]), vout: 0 },
				script_sig: Builder::new().push_slice(SCRIPT_SIG_SENTINEL).into_script(),
				sequence: Sequence::MAX, witness: Witness::default(),
			}],
			output: vec![TxOut { value: Amount::from_sat(20_000), script_pubkey: destination }],
		};
		(wallet, parent)
	}

	fn wallet() -> (Wallet, OutPoint) {
		let mut wallet = Wallet::create_single(get_test_tr_single_sig_xprv())
			.network(Network::Regtest).create_wallet_no_persist().unwrap();
		insert_checkpoint(&mut wallet, BlockId { height: 1_000, hash: BlockHash::all_zeros() });
		let point = receive_output_in_latest_block(&mut wallet, Amount::from_sat(20_000));
		(wallet, point)
	}

	#[test]
	fn wallet_inventory_observes_real_lock_guard_without_revealing_or_staging() {
		let (wallet, point) = wallet();
		let locks = LockedWalletUtxosIndex::new();
		let guard = WalletUtxoGuard::new(locks.clone(), point).unwrap();
		let before = wallet.staged().cloned();
		let index = wallet.derivation_index(KEYCHAIN);
		let locked = read_inventory(&wallet, WalletKind::Rounds, 1, &locks.utxos(), 2_000, 2_000_000).unwrap();
		assert_eq!(locked.reserved_outpoints, [point.to_string()]);
		assert_eq!(locked.utxos.len(), 1);
		assert!(locked.utxos[0].reserved);
		assert!(!locked.utxos[0].transaction_eligible);
		assert!(!locked.utxos[0].confirmed_fee_eligible);
		assert!(!locked.public_descriptor.contains("tprv"));
		assert!(!locked.public_descriptor.contains("xprv"));
		drop(guard);
		let unlocked = read_inventory(&wallet, WalletKind::Rounds, 1, &locks.utxos(), 2_000, 2_000_000).unwrap();
		assert!(!unlocked.utxos[0].reserved);
		assert!(unlocked.utxos[0].confirmed_fee_eligible);
		assert_eq!(unlocked.last_revealed_index, locked.last_revealed_index);
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
		assert_eq!(wallet.staged(), before.as_ref());
		let bytes = unlocked.encoded_len();
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks.utxos(), 1, bytes).is_ok());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks.utxos(), 1, bytes - 1).is_err());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks.utxos(), 0, bytes).is_err());
		assert_eq!(wallet.staged(), before.as_ref(), "budget refusals must remain read-only");
	}

	#[test]
	fn wallet_inventory_retains_signed_pending_inputs_then_removes_projection_after_confirmation() {
		let (mut wallet, point) = wallet();
		let destination = wallet.next_unused_address(KEYCHAIN).script_pubkey();
		let mut builder = wallet.build_tx();
		builder.add_recipient(destination, Amount::from_sat(5_000));
		builder.fee_rate(FeeRate::from_sat_per_vb(1).unwrap());
		let mut psbt = builder.finish().unwrap();
		assert!(wallet.sign(&mut psbt, Default::default()).unwrap());
		let signed = psbt.extract_tx().unwrap();
		let txid = signed.compute_txid();
		assert!(signed.input.iter().any(|input| !input.witness.is_empty()));
		wallet.apply_unconfirmed_txs([(signed.clone(), 1)]);
		let before = wallet.staged().cloned();
		let index = wallet.derivation_index(KEYCHAIN);
		let inventory = read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2_000, 2_000_000).unwrap();
		assert!(!inventory.utxos.iter().any(|u| u.outpoint == point.to_string()),
			"canonical UTXOs omit an input spent by a pending native transaction");
		assert_eq!(inventory.pending_transactions.len(), 1);
		let public = &inventory.pending_transactions[0];
		let body: Transaction = deserialize(&public.transaction_body).unwrap();
		assert_eq!(public.txid, txid.to_string());
		assert_eq!(body.compute_txid(), txid);
		assert!(body.input.iter().any(|input| input.previous_output == point));
		assert!(body.input.iter().all(|input| input.witness.is_empty()));
		assert_eq!(public.signed_weight_wu, signed.weight().to_wu());
		assert!(public.signed_weight_wu > body.weight().to_wu());
		let parent = inventory.pending_parents.iter().find(|p| p.txid == point.txid.to_string()).unwrap();
		let parent: Transaction = deserialize(&parent.transaction_body).unwrap();
		assert_eq!(parent.compute_txid(), point.txid);
		assert_eq!(parent.output[point.vout as usize].value, Amount::from_sat(20_000));
		assert!(inventory.complete && inventory.unavailable_parent_txids.is_empty());
		assert!(inventory.utxos.iter().all(|u| !u.confirmed_fee_eligible));
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
		assert_eq!(wallet.staged(), before.as_ref());
		let bytes = inventory.encoded_len();
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2_000, bytes).is_ok());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2_000, bytes - 1).is_err());

		let block = BlockId { height: 1_001, hash: BlockHash::from_byte_array([1; 32]) };
		insert_checkpoint(&mut wallet, block);
		insert_anchor(&mut wallet, txid, ConfirmationBlockTime { block_id: block, confirmation_time: 1 });
		let confirmed = read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2_000, 2_000_000).unwrap();
		assert!(confirmed.pending_transactions.is_empty() && confirmed.pending_parents.is_empty());
		assert!(confirmed.utxos.iter().all(|u| u.confirmed_fee_eligible));
		assert_eq!(confirmed.tip_height, 1_001);
	}

	#[test]
	fn wallet_inventory_script_sig_public_body_refuses_nonempty_scripts() {
		let (_, mut tx) = script_sig_parent();
		tx.input[0].script_sig = ScriptBuf::new();
		tx.input[0].witness.push(SCRIPT_SIG_SENTINEL);
		let original = serialize(&tx);
		let public = public_transaction(&tx, MAX_WALLET_INVENTORY_BYTES).unwrap();
		let body: Transaction = deserialize(&public.transaction_body).unwrap();
		assert_eq!(body.compute_txid(), tx.compute_txid());
		assert!(body.input[0].script_sig.is_empty() && body.input[0].witness.is_empty());
		assert_eq!(public.signed_weight_wu, tx.weight().to_wu());
		assert!(!public.transaction_body.windows(SCRIPT_SIG_SENTINEL.len()).any(|part| part == SCRIPT_SIG_SENTINEL));
		assert_eq!(serialize(&tx), original, "public projection must not change the source transaction");

		// Even a valid-looking wrapped witness program or coinbase script is not
		// classified as safe by this deliberately narrow empty-scriptSig seam.
		let mut redeem_script = [0u8; 22];
		redeem_script[1] = 20;
		let wrapped_witness = Builder::new().push_slice(redeem_script).into_script();
		let secret = Builder::new().push_slice(SCRIPT_SIG_SENTINEL).into_script();
		for (script, coinbase) in [(secret.clone(), false), (wrapped_witness, false), (secret, true)] {
			tx.input[0].script_sig = script;
			if coinbase { tx.input[0].previous_output = OutPoint::null(); }
			let before = serialize(&tx);
			let result = public_transaction(&tx, MAX_WALLET_INVENTORY_BYTES);
			assert!(result.is_err(), "nonempty scriptSig must not enter public transaction bodies");
			assert_eq!(serialize(&tx), before, "refusal must not rewrite scriptSig or transaction identity");
		}
	}

	#[test]
	fn wallet_inventory_script_sig_pending_body_refuses_whole_capture() {
		let (mut wallet, parent) = script_sig_parent();
		wallet.apply_unconfirmed_txs([(parent, 1)]);
		let before = wallet.staged().cloned();
		let index = wallet.derivation_index(KEYCHAIN);
		let result = read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2_000, 2_000_000);
		assert!(result.is_err(), "an unsafe canonical pending body must refuse the complete capture");
		assert_eq!(wallet.staged(), before.as_ref());
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
	}

	#[test]
	fn wallet_inventory_script_sig_parent_is_unavailable_without_body_export() {
		let (mut wallet, parent) = script_sig_parent();
		let parent_id = parent.compute_txid();
		insert_tx(&mut wallet, parent);
		let block_id = wallet.latest_checkpoint().block_id();
		insert_anchor(&mut wallet, parent_id, ConfirmationBlockTime {
			block_id, confirmation_time: 1,
		});
		let point = OutPoint { txid: parent_id, vout: 0 };
		let destination = wallet.next_unused_address(KEYCHAIN).script_pubkey();
		let mut builder = wallet.build_tx();
		builder.add_utxo(point).unwrap();
		builder.manually_selected_only();
		builder.add_recipient(destination, Amount::from_sat(5_000));
		builder.fee_rate(FeeRate::from_sat_per_vb(1).unwrap());
		let mut psbt = builder.finish().unwrap();
		assert!(wallet.sign(&mut psbt, Default::default()).unwrap());
		let signed = psbt.extract_tx().unwrap();
		assert!(signed.input.iter().all(|input| input.script_sig.is_empty()));
		assert!(signed.input.iter().any(|input| !input.witness.is_empty()));
		wallet.apply_unconfirmed_txs([(signed.clone(), 2)]);
		let before = wallet.staged().cloned();
		let index = wallet.derivation_index(KEYCHAIN);
		let inventory = read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 2_000, 2_000_000).unwrap();
		assert_eq!(inventory.unavailable_parent_txids, [parent_id.to_string()],
			"unsafe parent must remain an explicit txid gap rather than exporting scriptSig");
		assert!(!inventory.complete);
		assert!(inventory.pending_parents.is_empty());
		assert_eq!(inventory.pending_transactions.len(), 1);
		let pending: Transaction = deserialize(&inventory.pending_transactions[0].transaction_body).unwrap();
		assert_eq!(pending.compute_txid(), signed.compute_txid());
		assert!(pending.input.iter().any(|input| input.previous_output == point));
		assert!(pending.input.iter().all(|input| input.script_sig.is_empty() && input.witness.is_empty()));
		assert!(!inventory.encode_to_vec().windows(SCRIPT_SIG_SENTINEL.len()).any(|part| part == SCRIPT_SIG_SENTINEL));
		assert_eq!(wallet.staged(), before.as_ref());
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
	}

	#[test]
	fn wallet_inventory_aggregate_row_budget_covers_locks_and_utxos() {
		let (wallet, point) = wallet();
		let before = wallet.staged().cloned();
		let locked = HashSet::from([point]);
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locked, 1, 2_000_000).is_err(),
			"one UTXO plus one reservation exceeds a one-row aggregate budget");
		let inventory = read_inventory(&wallet, WalletKind::Rounds, 1, &locked, 2, 2_000_000).unwrap();
		assert_eq!(inventory.utxos.len() + inventory.reserved_outpoints.len(), 2);
		assert_eq!(wallet.staged(), before.as_ref(), "aggregate refusal must remain read-only");
	}
}
