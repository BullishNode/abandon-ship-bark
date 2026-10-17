//! A server admission fence cannot discharge the client's financial claim.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Script, Transaction};
use bitcoin_ext::{BlockDelta, TxStatus};

use crate::Wallet;
use crate::vtxo::{VtxoState, VtxoStateKind};
use super::payouts::{FallbackAttempt, FallbackBlock, FallbackDisposition, FallbackSnapshot,
	FallbackVerifiedPayout};

const PAYOUT_CONFIRMATIONS: BlockDelta = BlockDelta::new(6);
const PAYOUT_OUTPUT_FEE: Amount = Amount::from_sat(1_000);

/// All claims assigned to one actual output share its finite value. Server
/// allocation IDs cannot turn the same outpoint into multiple payments.
fn verify_output_group(
	attempts: &[&FallbackAttempt], actual: &Transaction, point: OutPoint, expected: &Script,
) -> anyhow::Result<()> {
	ensure!(!attempts.is_empty(), "empty payout evidence group");
	ensure!(actual.compute_txid() == point.txid, "chain returned another payout transaction");
	let output = actual.output.get(point.vout as usize).context("chain payout lacks expected output")?;
	ensure!(output.script_pubkey.as_script() == expected, "chain payout pays another script");
	let mut gross = Amount::ZERO;
	let mut fee = Amount::ZERO;
	let mut payout = Amount::ZERO;
	for attempt in attempts {
		ensure!(attempt.disposition == FallbackDisposition::CustomerPayout,
			"non-customer receipt cannot discharge a payout claim");
		ensure!(attempt.output_index == Some(point.vout) && attempt.output_amount == Some(output.value),
			"chain payout differs from the stated output/value");
		ensure!(attempt.script.as_deref() == Some(expected), "receipt pays another registered script");
		ensure!(attempt.forfeited == Amount::ZERO && attempt.fee.checked_add(attempt.payout) == Some(attempt.gross),
			"customer payout does not conserve its principal");
		gross = gross.checked_add(attempt.gross).context("payout principal overflow")?;
		fee = fee.checked_add(attempt.fee).context("payout fee overflow")?;
		payout = payout.checked_add(attempt.payout).context("payout value overflow")?;
	}
	// Other registered accounts may deliberately share this same destination;
	// this account's authenticated history need not enumerate their claims.
	ensure!(fee <= PAYOUT_OUTPUT_FEE && gross.checked_sub(fee) == Some(payout),
		"payout output charges more than the agreed fee");
	ensure!(payout > Amount::ZERO && payout <= output.value, "one chain output cannot cover these claims");
	Ok(())
}

impl Wallet {
	pub(crate) async fn verify_fallback_chain_evidence(
		&self, snapshot: &mut FallbackSnapshot, expected: &Script,
	) -> anyhow::Result<()> {
		let previous_verified = std::mem::take(&mut snapshot.verified_payouts);
		snapshot.awaiting_settlement.extend(snapshot.entitlements.values().map(|entitlement| entitlement.vtxo_id));
		let tip = self.chain().tip_ref().await?;
		let mut candidates = BTreeMap::<OutPoint, Vec<(i64, FallbackAttempt)>>::new();
		for (id, entitlement) in &snapshot.entitlements {
			if let Some(coin) = self.inner.db.get_wallet_vtxo(entitlement.vtxo_id).await? {
				ensure!(coin.amount() == entitlement.gross, "server entitlement differs from the client's signed coin value");
			}
			// Real earlier payments retain their association even if the
			// server changes its current projection or a reorg undoes them.
			let mut attempts = snapshot.payout_observations.get(id).into_iter().flatten()
				.map(|proof| proof.attempt.clone()).collect::<Vec<_>>();
			attempts.extend(entitlement.attempts.iter().cloned());
			let mut seen = BTreeSet::new();
			for attempt in attempts {
				if attempt.disposition != FallbackDisposition::CustomerPayout { continue; }
				let (Some(parent), Some(index)) = (&attempt.parent, attempt.output_index) else { continue; };
				let point = OutPoint::new(parent.compute_txid(), index);
				if seen.insert(point) { candidates.entry(point).or_default().push((*id, attempt)); }
			}
		}
		let mut verified = BTreeMap::new();
		for (point, group) in candidates {
			let TxStatus::Confirmed(block) = self.chain().tx_status(point.txid).await? else { continue; };
			if tip.height.checked_blocks_since(block.height).is_none_or(|depth| depth.saturating_add(1) < PAYOUT_CONFIRMATIONS.to_u32()) {
				continue;
			}
			// Check the block against the current chain, not the server's hash.
			if self.chain().block_ref(block.height).await? != block { continue; }
			let Some(actual) = self.chain().get_tx(&point.txid).await? else { continue; };
			let attempts = group.iter().map(|(_, attempt)| attempt).collect::<Vec<_>>();
			if let Err(error) = verify_output_group(&attempts, &actual, point, expected) {
				log::warn!("Fallback payout is not independently discharged: {:#}", error);
				continue;
			}
			for (id, attempt) in group {
				if let Some(coin) = self.inner.db.get_full_vtxo(snapshot.entitlements[&id].vtxo_id).await? {
					let path = std::iter::once(coin.chain_anchor()).chain(coin.transactions().map(|step|
						OutPoint::new(step.tx.compute_txid(), step.output_idx as u32))).collect::<BTreeSet<_>>();
					if !actual.input.iter().any(|input| path.contains(&input.previous_output)) {
						log::warn!("Fallback output does not spend this client's signed coin path");
						continue;
					}
				}
				let proof = FallbackVerifiedPayout { allocation_id: attempt.allocation_id,
					outpoint: point, attempt, confirmation: FallbackBlock { height: block.height, hash: block.hash } };
				verified.entry(id).or_insert(proof);
			}
		}
		// Do not publish observations collected across a reorg.
		ensure!(self.chain().block_ref(tip.height).await? == tip, "chain changed while verifying payout evidence");
		for (id, proof) in &verified {
			if previous_verified.get(id) != Some(proof) {
				if let Some(entitlement) = snapshot.entitlements.get_mut(id) { entitlement.last_updated = chrono::Local::now(); }
			}
			let observations = snapshot.payout_observations.entry(*id).or_default();
			if !observations.iter().any(|prior| prior.outpoint == proof.outpoint) { observations.push(proof.clone()); }
		}
		for id in previous_verified.keys().filter(|id| !verified.contains_key(*id)) {
			if let Some(entitlement) = snapshot.entitlements.get_mut(id) { entitlement.last_updated = chrono::Local::now(); }
		}
		snapshot.verified_payouts = verified;
		for (id, entitlement) in &snapshot.entitlements {
			if snapshot.verified_payouts.contains_key(id) {
				snapshot.awaiting_settlement.remove(&entitlement.vtxo_id);
				if self.inner.db.get_wallet_vtxo(entitlement.vtxo_id).await?.is_some_and(|coin|
					coin.state.kind() == VtxoStateKind::Spendable) {
					snapshot.adopted_payout_vtxos.insert(entitlement.vtxo_id);
				}
			} else {
				snapshot.awaiting_settlement.insert(entitlement.vtxo_id);
			}
		}
		Ok(())
	}

	/// The recovery marker is persisted before changing coin states. It is
	/// retained after a reorg so an interrupted restore is retried on next sync.
	pub(crate) async fn apply_fallback_chain_states(&self, snapshot: &FallbackSnapshot) -> anyhow::Result<()> {
		let discharged = snapshot.entitlements.iter().filter(|(id, _)| snapshot.verified_payouts.contains_key(*id))
			.map(|(_, entitlement)| entitlement.vtxo_id).collect::<BTreeSet<_>>();
		for id in &snapshot.adopted_payout_vtxos {
			let Some(coin) = self.inner.db.get_wallet_vtxo(*id).await? else { continue; };
			if matches!(coin.state.kind(), VtxoStateKind::Locked | VtxoStateKind::Exited) { continue; }
			let state = if discharged.contains(id) { VtxoState::Spent } else { VtxoState::Spendable };
			self.inner.db.update_vtxo_state_checked(*id, state,
				&[VtxoStateKind::Spendable, VtxoStateKind::Spent]).await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use bitcoin::{ScriptBuf, TxOut, WPubkeyHash};
	use bitcoin::hashes::Hash;
	use super::*;
	use crate::FallbackPayoutStage;

	fn group() -> (ScriptBuf, Transaction, FallbackAttempt) {
		let script = ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([7; 20]));
		let parent = Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO, input: vec![],
			output: vec![TxOut { value: Amount::from_sat(99_000), script_pubkey: script.clone() }], };
		let attempt = FallbackAttempt { allocation_id: 1, active: true,
			stage: FallbackPayoutStage::Confirmed, disposition: FallbackDisposition::CustomerPayout,
			output_index: Some(0), script: Some(script.clone()), gross: Amount::from_sat(100_000),
			fee: Amount::from_sat(1_000), payout: Amount::from_sat(99_000), forfeited: Amount::ZERO,
			output_amount: Some(Amount::from_sat(99_000)), parent: Some(parent.clone()), confirmation: None };
		(script, parent, attempt)
	}

	#[test]
	fn chain_output_must_pay_registered_script_and_expected_value() {
		let (script, parent, attempt) = group();
		let point = OutPoint::new(parent.compute_txid(), 0);
		assert!(verify_output_group(&[&attempt], &parent, point, &script).is_ok());
		let wrong_script = ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([8; 20]));
		assert!(verify_output_group(&[&attempt], &parent, point, &wrong_script).is_err());
		let mut false_value = attempt.clone();
		false_value.output_amount = Some(Amount::from_sat(100_000));
		assert!(verify_output_group(&[&false_value], &parent, point, &script).is_err());
		let mut excessive_fee = attempt.clone();
		excessive_fee.fee = Amount::from_sat(2_000);
		excessive_fee.payout = Amount::from_sat(98_000);
		assert!(verify_output_group(&[&excessive_fee], &parent, point, &script).is_err());
	}

	#[test]
	fn regroup_ids_cannot_reuse_one_output_as_two_payments() {
		let (script, parent, attempt) = group();
		let point = OutPoint::new(parent.compute_txid(), 0);
		let mut another = attempt.clone();
		another.allocation_id = 2;
		assert!(verify_output_group(&[&attempt, &another], &parent, point, &script).is_err());
		let mut first = attempt.clone();
		first.gross = Amount::from_sat(50_000);
		first.payout = Amount::from_sat(49_000);
		another.gross = Amount::from_sat(50_000);
		another.payout = Amount::from_sat(50_000);
		another.fee = Amount::ZERO;
		assert!(verify_output_group(&[&first, &another], &parent, point, &script).is_ok());
	}
}
