//! Economic recognition of an observed fee child. This never proves chain
//! acceptance: callers must independently obtain actual Core presence evidence.

use std::collections::BTreeSet;

use anyhow::{ensure, Context};
use bitcoin::{Amount, OutPoint, Script, Transaction, TxOut};
use bitcoin_ext::{TransactionExt, TxOutExt};

/// Only constructible after the complete raw child and its ordered prevouts
/// satisfy the supported fee-only shape. Unknown scripts are outside this
/// watchman's declared ownership scope, not globally foreign-owned.
pub(crate) struct ObservedFeeChild {
	pub(crate) child: Transaction,
	pub(crate) prevouts: Vec<TxOut>,
	pub(crate) fee: Amount,
	pub(crate) operator_contribution: Amount,
}

pub(crate) fn recognize_fee_only(
	parent: &Transaction, customer_outputs: usize, child: &Transaction,
	prevouts: &[TxOut], known_operator: impl Fn(&Script) -> bool,
) -> anyhow::Result<ObservedFeeChild> {
	ensure!(parent.version.0 == 3 && child.version.0 == 3, "fee-only package is not version 3");
	ensure!(child.vsize() <= 1_000 && !child.input.is_empty(), "unsupported fee-only child size");
	ensure!(customer_outputs <= parent.output.len(), "invalid parent customer prefix");
	ensure!(parent.output.iter().filter(|out| out.is_p2a_fee_anchor()).count() == 1,
		"fee-only parent must have one anchor");
	let (anchor, anchor_output) = parent.fee_anchor().context("fee-only parent has no anchor")?;
	ensure!(anchor.vout as usize >= customer_outputs, "fee anchor overlaps customer prefix");
	ensure!(child.input.len() == prevouts.len(), "fee-only prevout list is incomplete");
	let parent_id = parent.compute_txid();
	let is_operator = |script: &Script| known_operator(script)
		|| parent.output[customer_outputs..].iter().any(|out|
			!out.is_p2a_fee_anchor() && out.script_pubkey.as_script() == script);
	let mut points = BTreeSet::<OutPoint>::new();
	let mut anchor_count = 0;
	let mut input_value = Amount::ZERO;
	for (input, previous) in child.input.iter().zip(prevouts) {
		ensure!(points.insert(input.previous_output), "fee-only child repeats an input");
		if input.previous_output == anchor {
			ensure!(previous == anchor_output, "fee-only anchor prevout differs from parent");
			anchor_count += 1;
		} else {
			ensure!(input.previous_output.txid != parent_id,
				"fee-only child also consumes another parent output");
			ensure!(!previous.is_p2a_fee_anchor(), "fee-only child consumes another anchor");
			ensure!(!is_operator(previous.script_pubkey.as_script()),
				"fee-only child also consumes known operator funds");
		}
		input_value = input_value.checked_add(previous.value).context("fee-only input overflow")?;
	}
	ensure!(anchor_count == 1, "fee-only child must consume its exact anchor once");
	let output_value = child.output.iter().try_fold(Amount::ZERO, |sum, output| {
		ensure!(!output.is_p2a_fee_anchor(), "fee-only child creates another anchor");
		ensure!(!is_operator(output.script_pubkey.as_script()), "fee-only child returns known operator funds");
		sum.checked_add(output.value).context("fee-only output overflow")
	})?;
	let fee = input_value.checked_sub(output_value).context("fee-only child creates money")?;
	Ok(ObservedFeeChild {
		child: child.clone(), prevouts: prevouts.to_vec(), fee,
		operator_contribution: anchor_output.value,
	})
}

#[cfg(test)]
mod tests {
	use bitcoin::{absolute, transaction, ScriptBuf, Sequence, TxIn, Witness};
	use bitcoin::hashes::Hash;
	use bitcoin::WPubkeyHash;
	use bitcoin_ext::fee;
	use super::*;

	fn script(tag: u8) -> ScriptBuf { ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([tag;20])) }
	fn input(point: OutPoint) -> TxIn {
		TxIn { previous_output: point, script_sig: ScriptBuf::new(), sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
			witness: Witness::new() }
	}
	fn package(anchor_value: u64) -> (Transaction, Transaction, Vec<TxOut>) {
		let parent = Transaction {
			version: transaction::Version::non_standard(3), lock_time: absolute::LockTime::ZERO,
			input: vec![input(OutPoint::null())], output: vec![
				TxOut { value: Amount::from_sat(9_000), script_pubkey: script(1) },
				TxOut { value: Amount::from_sat(1_000), script_pubkey: script(2) },
				fee::fee_anchor_with_amount(Amount::from_sat(anchor_value)),
			],
		};
		let prevouts = vec![parent.output[2].clone(), TxOut { value: Amount::from_sat(10_000), script_pubkey: script(3) }];
		let child = Transaction {
			version: transaction::Version::non_standard(3), lock_time: absolute::LockTime::ZERO,
			input: vec![input(OutPoint::new(parent.compute_txid(),2)), input(OutPoint::null())],
			output: vec![TxOut { value: Amount::from_sat(9_500), script_pubkey: script(4) }],
		};
		(parent,child,prevouts)
	}

	#[test]
	fn fee_only_tracks_network_fee_separately_from_anchor_contribution() {
		for value in [0,200] {
			let (parent,child,prevouts) = package(value);
			let observed = recognize_fee_only(&parent,1,&child,&prevouts, |_|false).unwrap();
			assert_eq!(observed.fee.to_sat(),500+value);
			assert_eq!(observed.operator_contribution.to_sat(),value);
		}
	}

	#[test]
	fn fee_only_rejects_additional_parent_or_anchor_input() {
		let (parent,mut child,mut prevouts) = package(200);
		child.input[1].previous_output = OutPoint::new(parent.compute_txid(),0);
		prevouts[1] = parent.output[0].clone();
		assert!(recognize_fee_only(&parent,1,&child,&prevouts, |_|false).is_err());
		child.input[1].previous_output = OutPoint::null();
		prevouts[1] = fee::fee_anchor_with_amount(Amount::from_sat(10_000));
		assert!(recognize_fee_only(&parent,1,&child,&prevouts, |_|false).is_err());
	}

	#[test]
	fn fee_only_rejects_known_operator_input_output_and_extra_anchor_output() {
		let (parent,child,prevouts) = package(0);
		assert!(recognize_fee_only(&parent,1,&child,&prevouts, |spk|spk==script(3).as_script()).is_err());
		assert!(recognize_fee_only(&parent,1,&child,&prevouts, |spk|spk==script(4).as_script()).is_err());
		let mut mixed = child.clone();
		mixed.output[0].script_pubkey = script(2);
		assert!(recognize_fee_only(&parent,1,&mixed,&prevouts, |_|false).is_err());
		mixed.output[0] = fee::fee_anchor();
		assert!(recognize_fee_only(&parent,1,&mixed,&prevouts, |_|false).is_err());
	}

	#[test]
	fn fee_only_rejects_incomplete_duplicate_and_wrong_anchor_prevouts() {
		let (parent,mut child,mut prevouts) = package(200);
		assert!(recognize_fee_only(&parent,1,&child,&prevouts[..1], |_|false).is_err());
		prevouts[0].value = Amount::ZERO;
		assert!(recognize_fee_only(&parent,1,&child,&prevouts, |_|false).is_err());
		prevouts[0] = parent.output[2].clone();
		child.input[1].previous_output = child.input[0].previous_output;
		assert!(recognize_fee_only(&parent,1,&child,&prevouts, |_|false).is_err());
	}
}
