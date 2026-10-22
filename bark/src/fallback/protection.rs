//! Verify fallback payments independently of server admission decisions.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Script, Transaction};
use bitcoin_ext::{BlockDelta, BlockRef, TxStatus};

use ark::{Vtxo, VtxoPolicy};
use ark::vtxo::{Full, TapScriptClause};

use crate::Wallet;
use crate::exit::ExitState;
use crate::vtxo::{VtxoState, VtxoStateKind};
use super::payouts::{FallbackAttempt, FallbackBlock, FallbackDisposition, FallbackSnapshot,
	FallbackVerifiedPayout, FallbackVerifiedTerminal, FallbackTerminalDisposition};

const PAYOUT_CONFIRMATIONS: BlockDelta = BlockDelta::new(6);
fn payout_output_fee(gross: Amount) -> Amount {
	Amount::from_sat(1_000.max(gross.to_sat().div_ceil(100)))
}

/// Claims assigned to the same outpoint share its value. Check their sum
/// even when the server gives them different allocation IDs.
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
	ensure!(fee <= payout_output_fee(gross) && gross.checked_sub(fee) == Some(payout),
		"payout output charges more than the agreed fee");
	ensure!(payout > Amount::ZERO && payout <= output.value, "one chain output cannot cover these claims");
	Ok(())
}

/// The fee/dust test is per output group, rather than per coin. No output is
/// created for dust, so its entire gross is forfeited and no output fee charged.
fn verify_dust_group(attempts: &[&FallbackAttempt], expected: &Script) -> anyhow::Result<()> {
	ensure!(!attempts.is_empty(), "empty dust evidence group");
	let mut gross = Amount::ZERO;
	let allocation = attempts[0].allocation_id;
	for attempt in attempts {
		ensure!(attempt.disposition == FallbackDisposition::BelowDust && attempt.allocation_id == allocation,
			"dust evidence combines different allocation groups");
		ensure!(attempt.script.as_deref() == Some(expected) && attempt.output_index.is_none()
			&& attempt.output_amount.is_none() && attempt.payout == Amount::ZERO
			&& attempt.fee == Amount::ZERO && attempt.forfeited == attempt.gross,
			"dust receipt names a payout, charges a fee or changes its principal");
		gross = gross.checked_add(attempt.gross).context("dust principal overflow")?;
	}
	let net = gross.checked_sub(payout_output_fee(gross)).unwrap_or(Amount::ZERO);
	ensure!(net < expected.minimal_non_dust(), "dust group is large enough to pay out");
	Ok(())
}

/// An intermediate tree transaction can spend an ancestor without paying
/// its descendant. Also check that the spend uses the server expiry clause.
fn spends_expiry_path(coin: &Vtxo<Full>, actual: &Transaction) -> bool {
	let path = std::iter::once(coin.chain_anchor()).chain(coin.transactions().map(|step|
		OutPoint::new(step.tx.compute_txid(), step.output_idx as u32))).collect::<BTreeSet<_>>();
	let expected = ark::tree::signed::expiry_clause(coin.server_pubkey(), coin.expiry_height());
	actual.input.iter().any(|input| path.contains(&input.previous_output)
		&& input.witness.taproot_leaf_script().is_some_and(|leaf|
			leaf.version == bitcoin::taproot::LeafVersion::TapScript && leaf.script == expected.as_script()))
}

pub(crate) fn spends_user_claim(coin: &Vtxo<Full>, actual: &Transaction) -> bool {
	let VtxoPolicy::Pubkey(policy) = coin.policy() else { return false; };
	let expected = policy.user_pubkey_claim_clause(coin.exit_delta()).tapscript();
	actual.input.iter().any(|input| input.previous_output == coin.point()
		&& input.witness.taproot_leaf_script().is_some_and(|leaf|
			leaf.version == bitcoin::taproot::LeafVersion::TapScript && leaf.script == expected.as_script()))
}

impl FallbackSnapshot {
	fn publish_chain_proofs(
		&mut self, initial_tip: BlockRef, observed_tip: BlockRef,
		payouts: BTreeMap<i64, FallbackVerifiedPayout>, terminals: BTreeMap<i64, FallbackVerifiedTerminal>,
	) -> anyhow::Result<()> {
		// Error handlers persist this snapshot to retry protection. Clear both
		// payout and non-payout proofs when the chain view is rejected.
		self.verified_payouts.clear();
		self.verified_terminals.clear();
		ensure!(initial_tip == observed_tip, "chain changed while verifying fallback evidence");
		for (id, proof) in &payouts {
			let observations = self.payout_observations.entry(*id).or_default();
			if !observations.iter().any(|prior| prior.outpoint == proof.outpoint) { observations.push(proof.clone()); }
		}
		for (id, proof) in &terminals {
			let observations = self.terminal_observations.entry(*id).or_default();
			if !observations.iter().any(|old| old.disposition == proof.disposition
				&& old.transaction.compute_txid() == proof.transaction.compute_txid()) { observations.push(proof.clone()); }
		}
		self.verified_payouts = payouts;
		self.verified_terminals = terminals;
		Ok(())
	}
}

impl Wallet {
	pub(crate) async fn has_recovery_confirmations(&self, block: BlockRef, tip: BlockRef) -> anyhow::Result<bool> {
		if tip.height.checked_blocks_since(block.height).is_none_or(|depth|
			depth.saturating_add(1) < PAYOUT_CONFIRMATIONS.to_u32()) { return Ok(false); }
		Ok(self.chain().block_ref(block.height).await? == block)
	}

	pub(crate) async fn verify_fallback_chain_evidence(
		&self, snapshot: &mut FallbackSnapshot, expected: &Script,
	) -> anyhow::Result<()> {
		let previous_verified = std::mem::take(&mut snapshot.verified_payouts);
		let previous_terminals = std::mem::take(&mut snapshot.verified_terminals);
		snapshot.awaiting_settlement.extend(snapshot.entitlements.values().map(|entitlement| entitlement.vtxo_id));
		let tip = self.chain().tip_ref_uncached().await?;
		let mut candidates = BTreeMap::<OutPoint, Vec<(i64, FallbackAttempt)>>::new();
		for (id, entitlement) in &snapshot.entitlements {
			if let Some(coin) = self.inner.db.get_wallet_vtxo(entitlement.vtxo_id).await? {
				ensure!(coin.amount() == entitlement.gross, "server entitlement differs from the client's signed coin value");
			}
			// Retain earlier payout associations if the server regroups
			// outputs or a reorg removes those payments.
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
			if !self.has_recovery_confirmations(block, tip).await? { continue; }
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
		let verified_terminals = self.verify_fallback_terminal_evidence(snapshot, expected, tip).await?;
		// Reject new proofs and observations until the chain view is stable.
		// Retain prior observations, but clear current verification on failure.
		let observed_tip = self.chain().block_ref(tip.height).await?;
		snapshot.publish_chain_proofs(tip, observed_tip, verified, verified_terminals)?;
		let changed = previous_verified.keys().chain(snapshot.verified_payouts.keys()).filter(|id|
			previous_verified.get(*id) != snapshot.verified_payouts.get(*id)).chain(
			previous_terminals.keys().chain(snapshot.verified_terminals.keys()).filter(|id|
				previous_terminals.get(*id) != snapshot.verified_terminals.get(*id)))
			.copied().collect::<BTreeSet<_>>();
		for id in changed {
			if let Some(entitlement) = snapshot.entitlements.get_mut(&id) { entitlement.last_updated = chrono::Local::now(); }
		}
		let claimed = self.operation_input_ids().await?;
		for (id, entitlement) in &snapshot.entitlements {
			if snapshot.is_discharged(*id) {
				snapshot.awaiting_settlement.remove(&entitlement.vtxo_id);
				if self.inner.db.get_wallet_vtxo(entitlement.vtxo_id).await?.is_some_and(|coin|
					coin.state.kind() == VtxoStateKind::Spendable || (coin.state == (VtxoState::Locked { holder: None })
						&& snapshot.quarantined_vtxos.contains(&coin.id()) && !claimed.contains(&coin.id()))) {
					snapshot.adopted_payout_vtxos.insert(entitlement.vtxo_id);
				}
			} else {
				snapshot.awaiting_settlement.insert(entitlement.vtxo_id);
			}
		}
		Ok(())
	}

	/// Retire a locally owned claim only after a verified leaf claim or dust
	/// expiry spend. An operator classification alone is insufficient.
	async fn verify_fallback_terminal_evidence(
		&self, snapshot: &FallbackSnapshot, expected: &Script, tip: BlockRef,
	) -> anyhow::Result<BTreeMap<i64, FallbackVerifiedTerminal>> {
		let mut verified = BTreeMap::new();
		for (id, entitlement) in &snapshot.entitlements {
			let Some(coin) = self.inner.db.get_full_vtxo(entitlement.vtxo_id).await? else { continue; };
			let mut candidates = snapshot.terminal_observations.get(id).into_iter().flatten()
				.map(|proof| (proof.disposition.clone(), proof.transaction.compute_txid())).collect::<Vec<_>>();
			if let Some(claim) = &entitlement.terminal_claim {
				candidates.push((FallbackTerminalDisposition::Exited, claim.txid));
			}
			// Offline recovery can use a retained Claimed record without a fresh
			// server receipt. Still check canonical depth, the transaction and
			// the user-claim clause before accepting it as payment.
			if let Some(exit) = self.inner.db.get_exit_vtxo_entry(&entitlement.vtxo_id).await? {
				if let ExitState::Claimed(claim) = exit.state {
					candidates.push((FallbackTerminalDisposition::Exited, claim.txid));
				}
			}
			if let Some(attempt) = entitlement.active_attempt()? {
				if attempt.disposition == FallbackDisposition::BelowDust {
					if let Some(parent) = &attempt.parent {
						candidates.push((FallbackTerminalDisposition::BelowDust(attempt.clone()), parent.compute_txid()));
					}
				}
			}
			for (disposition, txid) in candidates {
				let TxStatus::Confirmed(block) = self.chain().tx_status(txid).await? else { continue; };
				if !self.has_recovery_confirmations(block, tip).await? { continue; }
				let Some(actual) = self.chain().get_tx(&txid).await? else { continue; };
				ensure!(actual.compute_txid() == txid, "chain returned another terminal transaction");
				match &disposition {
					FallbackTerminalDisposition::Exited => {
						if !spends_user_claim(&coin, &actual) { continue; }
					},
					FallbackTerminalDisposition::BelowDust(attempt) => {
						let mut group = vec![attempt];
						for (other_id, other) in &snapshot.entitlements {
							if other_id == id { continue; }
							if let Some(other_attempt) = other.attempts.iter().find(|prior|
								prior.allocation_id == attempt.allocation_id && prior.script.as_deref() == Some(expected)) {
								group.push(other_attempt);
							}
						}
						if verify_dust_group(&group, expected).is_err() || !spends_expiry_path(&coin, &actual) { continue; }
					},
				}
				verified.insert(*id, FallbackVerifiedTerminal { disposition,
					transaction: actual, confirmation: FallbackBlock { height: block.height, hash: block.hash } });
				break;
			}
		}
		Ok(verified)
	}

	/// The recovery marker is persisted before changing coin states. It is
	/// retained after a reorg so an interrupted restore is retried on next sync.
	pub(crate) async fn apply_fallback_chain_states(&self, snapshot: &FallbackSnapshot) -> anyhow::Result<()> {
		let discharged = snapshot.entitlements.iter().filter(|(id, _)| snapshot.is_discharged(**id))
			.map(|(_, entitlement)| entitlement.vtxo_id).collect::<BTreeSet<_>>();
		let claimed = self.operation_input_ids().await?;
		for id in &snapshot.adopted_payout_vtxos {
			let Some(coin) = self.inner.db.get_wallet_vtxo(*id).await? else { continue; };
			if coin.state == (VtxoState::Locked { holder: None }) && discharged.contains(id)
				&& snapshot.quarantined_vtxos.contains(id) && !claimed.contains(id) {
				// Persist the authenticated proof and recovery marker before
				// releasing this recovery lock with no associated action.
				self.inner.db.release_vtxo_lock(*id, None).await?;
			} else if matches!(coin.state.kind(), VtxoStateKind::Locked | VtxoStateKind::Exited) { continue; }
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
	fn payout_accepts_percentage_fee_and_a_prepaid_later_output() {
		let (script, mut parent, mut attempt) = group();
		attempt.gross = Amount::from_sat(200_000);
		attempt.fee = Amount::from_sat(2_000);
		attempt.payout = Amount::from_sat(198_000);
		parent.output[0].value = attempt.payout;
		attempt.output_amount = Some(attempt.payout);
		let point = OutPoint::new(parent.compute_txid(), 0);
		assert!(verify_output_group(&[&attempt], &parent, point, &script).is_ok());
		attempt.fee += Amount::from_sat(1);
		attempt.payout -= Amount::from_sat(1);
		assert!(verify_output_group(&[&attempt], &parent, point, &script).is_err());
		attempt.gross = Amount::from_sat(1_000);
		attempt.fee = Amount::ZERO;
		attempt.payout = attempt.gross;
		parent.output[0].value = attempt.payout;
		attempt.output_amount = Some(attempt.payout);
		let point = OutPoint::new(parent.compute_txid(), 0);
		assert!(verify_output_group(&[&attempt], &parent, point, &script).is_ok());
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
	#[test]
	fn dust_threshold_is_applied_to_the_whole_output_group() {
		let (script, _, mut dust) = group();
		dust.disposition = FallbackDisposition::BelowDust;
		dust.output_index = None;
		dust.output_amount = None;
		dust.fee = Amount::ZERO;
		dust.payout = Amount::ZERO;
		dust.gross = Amount::from_sat(1_200);
		dust.forfeited = dust.gross;
		assert!(verify_dust_group(&[&dust], &script).is_ok());
		assert!(verify_dust_group(&[&dust, &dust], &script).is_err(),
			"two individually sub-dust coins must pay when the group clears dust");
		dust.gross = Amount::from_sat(1_000) + script.minimal_non_dust();
		dust.forfeited = dust.gross;
		assert!(verify_dust_group(&[&dust], &script).is_err(), "the exact dust threshold must pay");
		dust.gross -= Amount::from_sat(1);
		dust.forfeited = dust.gross;
		assert!(verify_dust_group(&[&dust], &script).is_ok());
		dust.fee = Amount::from_sat(1);
		dust.forfeited -= dust.fee;
		assert!(verify_dust_group(&[&dust], &script).is_err(), "a forfeiture has no payout-output fee");
	}

	#[test]
	fn terminal_script_classifier_rejects_intermediate_progress_and_wrong_leaf() {
		let coin = &ark::test_util::VTXO_VECTORS.round1_vtxo;
		let mut actual = group().1;
		let input = bitcoin::TxIn { previous_output: coin.point(), script_sig: ScriptBuf::new(),
			sequence: bitcoin::Sequence::ZERO, witness: bitcoin::Witness::new() };
		actual.input.push(input);
		assert!(!spends_user_claim(coin, &actual));
		assert!(!spends_expiry_path(coin, &actual));
		let VtxoPolicy::Pubkey(policy) = coin.policy() else { panic!("standard vector expected"); };
		let user_script = policy.user_pubkey_claim_clause(coin.exit_delta()).tapscript();
		let witness = |script: &Script| bitcoin::Witness::from_slice(&[
			vec![0u8; 64], script.as_bytes().to_vec(), vec![0xc0; 33],
		]);
		// This is a script/path classifier test, not a chain-valid signature.
		actual.input[0].witness = witness(&user_script);
		assert!(spends_user_claim(coin, &actual));
		actual.input[0].previous_output = coin.chain_anchor();
		assert!(!spends_user_claim(coin, &actual), "a user-like script on an ancestor is not the leaf's claim");
		let expiry = ark::tree::signed::expiry_clause(coin.server_pubkey(), coin.expiry_height());
		actual.input[0].witness = witness(&expiry);
		assert!(spends_expiry_path(coin, &actual));
		actual.input[0].previous_output = OutPoint::null();
		assert!(!spends_expiry_path(coin, &actual), "another root cannot retire this coin");
	}

	#[test]
	fn rejected_final_chain_barrier_clears_both_proof_classes_before_stale_snapshot_persistence() {
		let (_, parent, attempt) = group();
		let block = FallbackBlock { height: 100.into(), hash: bitcoin::BlockHash::from_byte_array([1; 32]) };
		let payout = FallbackVerifiedPayout { allocation_id: attempt.allocation_id,
			attempt, outpoint: OutPoint::new(parent.compute_txid(), 0), confirmation: block.clone() };
		let terminal = FallbackVerifiedTerminal { disposition: FallbackTerminalDisposition::Exited,
			transaction: parent, confirmation: block.clone() };
		let payouts = BTreeMap::from([(1, payout)]);
		let terminals = BTreeMap::from([(2, terminal)]);
		let mut snapshot = FallbackSnapshot::default();
		snapshot.verified_payouts = payouts.clone();
		snapshot.verified_terminals = terminals.clone();
		let initial = BlockRef { height: block.height, hash: block.hash };
		let changed = BlockRef { height: block.height, hash: bitcoin::BlockHash::from_byte_array([2; 32]) };
		assert!(snapshot.publish_chain_proofs(initial, changed, payouts.clone(), terminals.clone()).is_err());
		assert!(!snapshot.is_discharged(1) && !snapshot.is_discharged(2));
		assert!(snapshot.payout_observations.is_empty() && snapshot.terminal_observations.is_empty(),
			"a rejected view may not add observations either");
		// Match sync's error-handler persistence boundary, including JSON.
		let persisted = serde_json::to_vec(&snapshot).unwrap();
		let mut stale: FallbackSnapshot = serde_json::from_slice(&persisted).unwrap();
		assert!(!stale.is_discharged(1) && !stale.is_discharged(2));
		stale.publish_chain_proofs(initial, initial, payouts, terminals).unwrap();
		assert!(stale.is_discharged(1) && stale.is_discharged(2));
		assert_eq!(stale.payout_observations[&1].len(), 1);
		assert_eq!(stale.terminal_observations[&2].len(), 1);
	}

}
