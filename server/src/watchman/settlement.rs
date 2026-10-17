//! Recipient amounts for a direct expiry claim. Ownership is resolved before this step.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context};
use bitcoin::{Amount, ScriptBuf, SignedAmount, Transaction, TxOut};

use ark::VtxoId;

const OUTPUT_FEE: Amount = Amount::from_sat(1_000);

/// A customer entitlement whose ownership and backing have been resolved.
pub(crate) struct CustomerLeaf {
	pub id: VtxoId,
	pub script: ScriptBuf,
	pub amount: Amount,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LeafAllocation {
	pub id: VtxoId,
	pub gross: Amount,
	pub fee: Amount,
	pub payout: Amount,
	pub forfeited: Amount,
}

pub(crate) struct Recipient {
	pub output: TxOut,
	pub leaves: Vec<LeafAllocation>,
}

pub(crate) struct SettlementPlan {
	pub recipients: Vec<Recipient>,
	pub forfeits: Vec<LeafAllocation>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PayoutFeeAccounting {
	pub operator_remainder: Amount,
	pub miner_fee: Amount,
	pub subsidy: Amount,
	pub net_operator_result: SignedAmount,
}

/// Customer outputs are the committed prefix. Everything remaining belongs to
/// the operator, whether paid directly to its drain or through a nonzero P2A.
/// Apply this to the one canonically confirmed fee child, not all RBF versions.
pub(crate) fn payout_fee_accounting(
	parent: &Transaction, customer_outputs: usize, miner_fee: Amount,
) -> anyhow::Result<PayoutFeeAccounting> {
	ensure!(customer_outputs <= parent.output.len(), "payout accounting omits customer outputs");
	let operator_remainder = parent.output[customer_outputs..].iter().try_fold(Amount::ZERO, |sum, output|
		sum.checked_add(output.value).context("operator payout remainder overflow"),
	)?;
	let net_operator_result = operator_remainder.to_signed()?.checked_sub(miner_fee.to_signed()?)
		.context("net operator payout result overflow")?;
	let subsidy = miner_fee.checked_sub(operator_remainder).unwrap_or(Amount::ZERO);
	Ok(PayoutFeeAccounting { operator_remainder, miner_fee, subsidy, net_operator_result })
}

/// Group by actual output, with stable per-leaf amounts for persisted attribution.
/// Mining cost is independent: the watchman wallet funds the fee child,
/// and the sweep remainder returns to the operator.
pub(crate) fn allocate(leaves: Vec<CustomerLeaf>) -> anyhow::Result<SettlementPlan> {
	let mut ids = BTreeSet::new();
	let mut groups = BTreeMap::<ScriptBuf, Vec<CustomerLeaf>>::new();
	for leaf in leaves {
		ensure!(ids.insert(leaf.id), "duplicate expiry entitlement {}", leaf.id);
		ensure!(leaf.amount > Amount::ZERO, "empty expiry entitlement {}", leaf.id);
		groups.entry(leaf.script.clone()).or_default().push(leaf);
	}

	let mut plan = SettlementPlan { recipients: Vec::new(), forfeits: Vec::new() };
	for (script, mut group) in groups {
		group.sort_by_key(|leaf| leaf.id);
		let gross = group.iter().try_fold(Amount::ZERO, |sum, leaf|
			sum.checked_add(leaf.amount).context("expiry group amount overflow"),
		)?;
		let group_fee = gross.min(OUTPUT_FEE);
		let net = gross.checked_sub(group_fee).context("expiry fee exceeds principal")?;
		let pays = net > Amount::ZERO && net >= script.minimal_non_dust();
		// There is no output fee when the entire group is forfeited.
		let mut remaining_fee = if pays { group_fee } else { Amount::ZERO };
		let mut allocations = Vec::with_capacity(group.len());
		for leaf in group {
			let fee = leaf.amount.min(remaining_fee);
			remaining_fee = remaining_fee.checked_sub(fee).context("expiry fee allocation overflow")?;
			let remainder = leaf.amount.checked_sub(fee).context("expiry leaf fee exceeds principal")?;
			allocations.push(LeafAllocation {
				id: leaf.id, gross: leaf.amount, fee,
				payout: if pays { remainder } else { Amount::ZERO },
				forfeited: if pays { Amount::ZERO } else { remainder },
			});
		}
		ensure!(remaining_fee == Amount::ZERO, "expiry group fee was not allocated");
		if pays {
			plan.recipients.push(Recipient {
				output: TxOut { script_pubkey: script, value: net }, leaves: allocations,
			});
		} else {
			plan.forfeits.extend(allocations);
		}
	}
	Ok(plan)
}

#[cfg(test)]
mod tests {
	use bitcoin::hashes::Hash;
	use bitcoin::{PubkeyHash, ScriptHash, WPubkeyHash, WScriptHash};
	use bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey};

	use super::*;

	fn leaf(id: u8, amount: u64, script: &ScriptBuf) -> CustomerLeaf {
		let mut bytes = [0; VtxoId::ENCODE_SIZE];
		bytes[0] = id;
		CustomerLeaf { id: VtxoId::from_slice(&bytes).unwrap(), amount: Amount::from_sat(amount), script: script.clone() }
	}

	fn wpkh() -> ScriptBuf {
		ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([1; 20]))
	}

	#[test]
	fn direct_fee_many_small_leaves_pay_one_non_dust_output() {
		let script = wpkh();
		let plan = allocate((0..20).map(|i| leaf(i, 100, &script)).collect()).unwrap();
		assert!(plan.forfeits.is_empty());
		assert_eq!(plan.recipients.len(), 1);
		let recipient = &plan.recipients[0];
		assert_eq!(recipient.output.value, Amount::from_sat(1_000));
		assert_eq!(recipient.leaves.iter().map(|a| a.fee.to_sat()).sum::<u64>(), 1_000);
		assert!(recipient.leaves.iter().all(|a| a.fee <= a.gross));
	}

	#[test]
	fn direct_fee_script_dust_equality_is_paid() {
		let secp = Secp256k1::new();
		let key = Keypair::from_secret_key(&secp, &SecretKey::from_slice(&[3; 32]).unwrap());
		for script in [
			wpkh(),
			ScriptBuf::new_p2pkh(&PubkeyHash::from_byte_array([2; 20])),
			ScriptBuf::new_p2sh(&ScriptHash::from_byte_array([4; 20])),
			ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([5; 32])),
			ScriptBuf::new_p2tr(&secp, key.x_only_public_key().0, None),
		] {
			let dust = script.minimal_non_dust().to_sat();
			let paid = allocate(vec![leaf(1, 1_000 + dust, &script)]).unwrap();
			assert_eq!(paid.recipients[0].output.value.to_sat(), dust);
			let forfeited = allocate(vec![leaf(1, 999 + dust, &script)]).unwrap();
			assert!(forfeited.recipients.is_empty());
			assert_eq!(forfeited.forfeits[0].forfeited.to_sat(), 999 + dust);
			assert_eq!(forfeited.forfeits[0].fee, Amount::ZERO);
		}
	}

	#[test]
	fn direct_fee_order_does_not_change_leaf_attribution() {
		let script = wpkh();
		let a = allocate(vec![leaf(2, 2_000, &script), leaf(1, 400, &script)]).unwrap();
		let b = allocate(vec![leaf(1, 400, &script), leaf(2, 2_000, &script)]).unwrap();
		assert_eq!(a.recipients[0].leaves, b.recipients[0].leaves);
		assert_eq!(a.recipients[0].leaves[0].fee.to_sat(), 400);
		assert_eq!(a.recipients[0].leaves[1].fee.to_sat(), 600);
	}

	#[test]
	fn direct_fee_rejects_duplicate_entitlement() {
		let script = wpkh();
		assert!(allocate(vec![leaf(1, 2_000, &script), leaf(1, 2_000, &script)]).is_err());
	}

	#[test]
	fn direct_fee_conserves_each_leaf_across_policy_boundary() {
		let script = wpkh();
		for total in 1..4_000 {
			let plan = allocate(vec![leaf(1, total, &script)]).unwrap();
			let allocations = plan.forfeits.iter().chain(plan.recipients.iter().flat_map(|r| &r.leaves));
			for a in allocations {
				assert_eq!(a.gross.to_sat(), a.fee.to_sat() + a.payout.to_sat() + a.forfeited.to_sat());
				assert!(a.payout == Amount::ZERO || a.forfeited == Amount::ZERO);
			}
		}
	}
	#[test]
	fn direct_fee_separate_outputs_forfeit_what_a_merged_output_pays() {
		let script = wpkh();
		let combined = allocate(vec![leaf(1, 1_200, &script), leaf(2, 1_200, &script)]).unwrap();
		assert_eq!(combined.recipients.len(), 1);
		assert_eq!(combined.recipients[0].output.value.to_sat(), 1_400);
		for id in [1, 2] {
			let separate = allocate(vec![leaf(id, 1_200, &script)]).unwrap();
			assert!(separate.recipients.is_empty());
			assert_eq!(separate.forfeits[0].forfeited.to_sat(), 1_200);
		}
	}

	#[test]
	fn expiry_fee_accounting_includes_the_drain_and_nonzero_anchor() {
		let mut parent = Transaction {
			version: bitcoin::transaction::Version::non_standard(3),
			lock_time: bitcoin::absolute::LockTime::ZERO, input: vec![],
			output: vec![
				TxOut { value: Amount::from_sat(9_000), script_pubkey: wpkh() },
				TxOut { value: Amount::from_sat(1_000), script_pubkey: wpkh() },
				bitcoin_ext::fee::fee_anchor(),
			],
		};
		let ordinary = payout_fee_accounting(&parent,1,Amount::from_sat(250)).unwrap();
		assert_eq!(ordinary.operator_remainder.to_sat(),1_000);
		assert_eq!(ordinary.subsidy,Amount::ZERO);
		assert_eq!(ordinary.net_operator_result.to_sat(),750);
		assert_eq!(9_000+ordinary.net_operator_result.to_sat()+250,10_000);
		parent.output.truncate(1);
		parent.output.push(bitcoin_ext::fee::fee_anchor_with_amount(Amount::from_sat(200)));
		let small = payout_fee_accounting(&parent,1,Amount::from_sat(300)).unwrap();
		assert_eq!(small.operator_remainder.to_sat(),200);
		assert_eq!(small.subsidy.to_sat(),100);
		assert_eq!(small.net_operator_result.to_sat(),-100);
		assert_eq!(9_000+small.net_operator_result.to_sat()+300,9_200);
		// The final version replaces the earlier fee, it is never added to it.
		let replacement = payout_fee_accounting(&parent,1,Amount::from_sat(450)).unwrap();
		assert_eq!(replacement.miner_fee.to_sat(),450);
		assert_eq!(replacement.subsidy.to_sat(),250);
	}

}
