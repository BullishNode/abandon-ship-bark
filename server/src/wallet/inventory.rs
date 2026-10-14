//! Public source inventory from the owning wallet, without wallet mutation.

use std::collections::HashSet;

use anyhow::Context;
use bdk_wallet::chain::ChainPosition;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::OutPoint;
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
		utxos: Vec::new(), complete: true,
		reserved_outpoints: locked.iter().map(|p| p.to_string()).collect(),
		last_revealed_index: wallet.derivation_index(KEYCHAIN),
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
	ensure!(inventory.encoded_len() <= max_bytes, "wallet inventory serialized byte budget exceeded");
	Ok(inventory)
}


#[cfg(test)]
mod test {
	use super::*;
	use bdk_wallet::chain::BlockId;
	use bdk_wallet::test_utils::{get_test_tr_single_sig_xprv, insert_checkpoint, receive_output_in_latest_block};
	use bitcoin::{Amount, BlockHash, Network};
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
		let result = read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 1, 2_000_000).unwrap();
		assert!(wallet.staged() == before.as_ref(), "read must not change staged wallet state");
		assert_eq!(wallet.derivation_index(KEYCHAIN), index);
		assert_eq!(result.utxos.len(), 1);
		assert!(result.utxos[0].reserved);
		assert!(!result.utxos[0].transaction_eligible);
		assert!(!result.utxos[0].confirmed_fee_eligible);
		let bytes = result.encoded_len();
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 1, bytes).is_ok());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 1, bytes - 1).is_err());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &locks, 0, bytes).is_err());
		assert!(read_inventory(&wallet, WalletKind::Rounds, 1, &HashSet::new(), 0, bytes).is_err());
		assert!(wallet.staged() == before.as_ref(), "even refusals must be read-only");
		assert_eq!(locks, HashSet::from([point]));
	}
}
