//! Account-owned fallback receipts and their current output projection.
//!
//! An entitlement is permanent. Output groups may merge, split or disappear
//! after an exit or reorg, so historical attempts never create extra debits.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Context;
use bitcoin::{Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, Txid};
use bitcoin::hex::DisplayHex;
use chrono::{DateTime, Local};

use ark::VtxoId;
use bitcoin_ext::BlockHeight;

use crate::movement::{Movement, MovementDestination, MovementId, MovementStatus, MovementSubsystem};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackBlock {
	pub height: BlockHeight,
	pub hash: BlockHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackPayoutStage { Committed, Broadcast, Confirmed, Conflicted }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackDisposition { CustomerPayout, BelowDust, Operator }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackAttempt {
	pub allocation_id: i64,
	pub active: bool,
	pub stage: FallbackPayoutStage,
	pub disposition: FallbackDisposition,
	pub output_index: Option<u32>,
	pub script: Option<ScriptBuf>,
	pub gross: Amount,
	pub fee: Amount,
	pub payout: Amount,
	pub forfeited: Amount,
	pub output_amount: Option<Amount>,
	pub parent: Option<Transaction>,
	pub confirmation: Option<FallbackBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackTerminalClaim {
	pub txid: Txid,
	pub confirmation: FallbackBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackEntitlement {
	pub entitlement_id: i64,
	pub vtxo_id: VtxoId,
	pub gross: Amount,
	pub chain_view_valid: bool,
	pub attempts: Vec<FallbackAttempt>,
	pub terminal_claim: Option<FallbackTerminalClaim>,
	pub first_seen: DateTime<Local>,
	pub last_updated: DateTime<Local>,
}

/// Evidence obtained independently from the wallet's own chain backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackVerifiedPayout {
	pub allocation_id: i64,
	pub attempt: FallbackAttempt,
	pub outpoint: OutPoint,
	pub confirmation: FallbackBlock,
}

/// A non-payout disposition is final only after this wallet observes its
/// actual transaction on the canonical chain. The full transaction is retained
/// separately from current verification so reorgs and server outages are safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTerminalDisposition {
	BelowDust(FallbackAttempt),
	Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackVerifiedTerminal {
	pub disposition: FallbackTerminalDisposition,
	pub transaction: Transaction,
	pub confirmation: FallbackBlock,
}

/// This one persisted record is the visible history generation. IDs reserved
/// for output movements are not visible until the complete account snapshot
/// commits. A crash during pagination or ID reservation preserves the prior
/// complete projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[doc(hidden)]
pub struct FallbackSnapshot {
	pub entitlements: BTreeMap<i64, FallbackEntitlement>,
	/// Only actual locally known coin IDs create Ark balance debits. An
	/// unknown C4 descendant does not repeat its parent's outgoing debit.
	pub local_debits: BTreeMap<i64, Amount>,
	pub movement_ids: BTreeMap<String, MovementId>,
	pub view_token: Vec<u8>,
	pub awaiting_sync: bool,
	pub awaiting_settlement: BTreeSet<VtxoId>,
	/// A server confirmation is never copied here without a chain lookup.
	#[serde(default)]
	pub verified_payouts: BTreeMap<i64, FallbackVerifiedPayout>,
	/// Previously observed outputs remain reserved to the same claims even
	/// while reorged out. A later server regroup cannot reuse their value.
	#[serde(default)]
	pub payout_observations: BTreeMap<i64, Vec<FallbackVerifiedPayout>>,
	/// Dust forfeiture and actual leaf claims have no payout outpoint.
	#[serde(default)]
	pub verified_terminals: BTreeMap<i64, FallbackVerifiedTerminal>,
	#[serde(default)]
	pub terminal_observations: BTreeMap<i64, Vec<FallbackVerifiedTerminal>>,
	/// Only coins this client itself adopted may be restored after a reorg.
	/// Persisted before changing their local state, so interruption is safe.
	#[serde(default)]
	pub adopted_payout_vtxos: BTreeSet<VtxoId>,
	/// Validated recovery bodies kept unavailable while post-status history
	/// or their independently confirmed payout is missing. The marker survives
	/// a later protective exit or payout adoption to avoid inventing a prior
	/// Ark debit after fresh recovery. Only orphan None holders may be released.
	#[serde(default)]
	pub quarantined_vtxos: BTreeSet<VtxoId>,
}

pub(crate) const FALLBACK_SUBSYSTEM: &str = "abandon.fallback_payout";

impl FallbackEntitlement {
	pub(crate) fn active_attempt(&self) -> anyhow::Result<Option<&FallbackAttempt>> {
		let mut active = self.attempts.iter().filter(|attempt| attempt.active);
		let first = active.next();
		ensure!(active.next().is_none(), "entitlement has multiple active allocation associations");
		Ok(first)
	}

	pub(crate) fn verify(&self, expected: &bitcoin::Script) -> anyhow::Result<()> {
		ensure!(self.entitlement_id > 0 && self.gross > Amount::ZERO, "invalid fallback entitlement identity/value");
		let _ = self.active_attempt()?;
		for attempt in &self.attempts {
			ensure!(attempt.allocation_id > 0 && attempt.gross == self.gross, "fallback attempt differs from permanent entitlement");
			if attempt.disposition == FallbackDisposition::Operator {
				ensure!(attempt.fee == Amount::ZERO && attempt.payout == Amount::ZERO
					&& attempt.forfeited == Amount::ZERO && attempt.output_index.is_none()
					&& attempt.output_amount.is_none() && attempt.script.is_none(),
					"operator receipt falsely names customer value or a payout");
				continue;
			}
			ensure!(attempt.fee.checked_add(attempt.payout).and_then(|a| a.checked_add(attempt.forfeited)) == Some(attempt.gross),
				"fallback attempt does not conserve entitlement value");
			ensure!(attempt.script.as_deref() == Some(expected), "fallback receipt names a different registered payout address");
			if attempt.disposition == FallbackDisposition::BelowDust {
				ensure!(attempt.payout == Amount::ZERO && attempt.fee == Amount::ZERO
					&& attempt.forfeited == attempt.gross && attempt.output_index.is_none()
					&& attempt.output_amount.is_none(), "dust receipt falsely names a payout or charges a fee");
				continue;
			}
			let index = attempt.output_index.context("customer receipt lacks output index")?;
			let amount = attempt.output_amount.context("customer receipt lacks output value")?;
			ensure!(attempt.payout <= amount, "entitlement payout exceeds its group output");
			if let Some(parent) = &attempt.parent {
				let output = parent.output.get(index as usize).context("fallback parent lacks committed output")?;
				ensure!(output.script_pubkey.as_script() == expected && output.value == amount,
					"fallback parent differs from committed output");
			} else {
				ensure!(matches!(attempt.stage, FallbackPayoutStage::Committed | FallbackPayoutStage::Conflicted),
					"payout stage requires actual parent transaction");
			}
			if attempt.stage == FallbackPayoutStage::Confirmed {
				ensure!(self.chain_view_valid && attempt.confirmation.is_some(), "confirmed receipt lacks valid chain evidence");
			}
		}
		ensure!(self.terminal_claim.is_none() || self.chain_view_valid, "terminal exit lacks a valid chain view");
		Ok(())
	}

	fn group_key(&self) -> anyhow::Result<Option<String>> {
		// Another device may have exited this coin. Preserve the receipt until
		// projection can check whether this device has its native exit movement.
		if self.terminal_claim.is_some() { return Ok(Some(format!("pending/{}", self.entitlement_id))); }
		let Some(attempt) = self.active_attempt()? else {
			return Ok(Some(format!("pending/{}", self.entitlement_id)));
		};
		match attempt.disposition {
			FallbackDisposition::Operator => Ok(None),
			FallbackDisposition::CustomerPayout => {
				let index = attempt.output_index.context("customer allocation lacks output index")?;
				Ok(Some(format!("allocation/{}/output/{}", attempt.allocation_id, index)))
			},
			FallbackDisposition::BelowDust => {
				let script = attempt.script.as_ref().context("dust allocation lacks payout script")?;
				Ok(Some(format!("allocation/{}/dust/{}", attempt.allocation_id, script.as_bytes().as_hex())))
			},
		}
	}
}

impl FallbackSnapshot {
	pub(crate) fn is_discharged(&self, id: i64) -> bool {
		self.verified_payouts.contains_key(&id) || self.verified_terminals.contains_key(&id)
	}

	pub(crate) fn groups(&self) -> anyhow::Result<BTreeMap<String, Vec<&FallbackEntitlement>>> {
		let mut groups = BTreeMap::<String, Vec<&FallbackEntitlement>>::new();
		for entitlement in self.entitlements.values() {
			let key = if self.verified_terminals.get(&entitlement.entitlement_id).is_some_and(|proof|
				matches!(proof.disposition, FallbackTerminalDisposition::Exited)) {
				Some(format!("pending/{}", entitlement.entitlement_id))
			} else { entitlement.group_key()? };
			if let Some(key) = key { groups.entry(key).or_default().push(entitlement); }
		}
		Ok(groups)
	}

	/// Keep regular movements and replace placeholder fallback movements with
	/// exactly the current account projection. Historical allocation attempts
	/// stay in receipt metadata, not as additional outgoing movements.
	pub(crate) fn project(&self, stored: Vec<Movement>, network: Network) -> anyhow::Result<Vec<Movement>> {
		let mut known = HashMap::new();
		let mut result = Vec::new();
		let verified_coins = self.entitlements.iter().filter(|(id, _)| self.verified_payouts.contains_key(*id)
			|| self.verified_terminals.get(*id).is_some_and(|proof|
				matches!(proof.disposition, FallbackTerminalDisposition::BelowDust(_))))
			.map(|(_, entry)| entry.vtxo_id).collect::<BTreeSet<_>>();
		let mut reserved_by_exit = BTreeSet::new();
		let mut native_exit_principal = BTreeSet::new();
		for mut movement in stored {
			if movement.subsystem.name == FALLBACK_SUBSYSTEM { known.insert(movement.id, movement); }
			else {
				if movement.subsystem.name == "bark.exit" && movement.effective_balance < bitcoin::SignedAmount::ZERO
					&& !matches!(movement.status, MovementStatus::Canceled | MovementStatus::Failed) {
					native_exit_principal.extend(movement.input_vtxos.iter().copied());
				}
				if movement.metadata.get("protective_exit") == Some(&serde_json::Value::Bool(true))
					&& !movement.input_vtxos.is_empty() {
					let paid = movement.input_vtxos.iter().all(|id| verified_coins.contains(id));
					if paid || matches!(movement.status, MovementStatus::Canceled | MovementStatus::Failed) {
						// The fallback, rather than this protective intent, paid
						// the principal. Reorgs rebuild this projection from the
						// retained exit movement and actual current chain proof.
						movement.effective_balance = bitcoin::SignedAmount::ZERO;
						if paid { movement.status = MovementStatus::Canceled; }
					} else if movement.effective_balance < bitcoin::SignedAmount::ZERO {
						reserved_by_exit.extend(movement.input_vtxos.iter().copied());
					}
				}
				result.push(movement);
			}
		}
		for (key, entitlements) in self.groups()? {
			let external_exit = entitlements.iter().all(|entry| entry.terminal_claim.is_some()
				|| self.verified_terminals.get(&entry.entitlement_id).is_some_and(|proof|
					matches!(proof.disposition, FallbackTerminalDisposition::Exited)));
			if external_exit && entitlements.iter().all(|entry| native_exit_principal.contains(&entry.vtxo_id)) {
				continue;
			}
			let id = *self.movement_ids.get(&key).context("fallback group lacks reserved history ID")?;
			let first = entitlements[0];
			let time = entitlements.iter().map(|e| e.first_seen).min().expect("nonempty group");
			let mut movement = known.remove(&id).unwrap_or_else(|| Movement::new(id, MovementStatus::Pending,
				&MovementSubsystem { name: FALLBACK_SUBSYSTEM.to_owned(), kind: "payout".to_owned() }, time));
			movement.subsystem.kind = if external_exit { "exit" } else { "payout" }.to_owned();
			movement.time.created_at = time;
			movement.time.updated_at = entitlements.iter().map(|e| e.last_updated).max().expect("nonempty group");
			movement.input_vtxos = entitlements.iter().map(|e| e.vtxo_id).collect();
			let mut debit = Amount::ZERO;
			let mut fee = Amount::ZERO;
			let mut payout = Amount::ZERO;
			let mut forfeited = Amount::ZERO;
			let mut valid = true;
			let reference = if external_exit { None } else { first.active_attempt()? };
			for entitlement in &entitlements {
				if !reserved_by_exit.contains(&entitlement.vtxo_id) {
					debit = debit.checked_add(self.local_debits.get(&entitlement.entitlement_id).copied().unwrap_or(Amount::ZERO))
						.context("fallback history debit overflow")?;
				}
				valid &= entitlement.chain_view_valid;
				if let Some(attempt) = entitlement.active_attempt()?.filter(|_| !external_exit) {
					let reference = reference.context("mixed pending and allocated fallback group")?;
					ensure!(attempt.script == reference.script && attempt.parent == reference.parent
						&& attempt.output_amount == reference.output_amount && attempt.stage == reference.stage
						&& attempt.confirmation == reference.confirmation, "inconsistent current payout output associations");
					fee = fee.checked_add(attempt.fee).context("fallback history fee overflow")?;
					payout = payout.checked_add(attempt.payout).context("fallback history payout overflow")?;
					forfeited = forfeited.checked_add(attempt.forfeited).context("fallback history dust overflow")?;
				}
			}
			movement.intended_balance = -debit.to_signed().context("fallback debit out of range")?;
			movement.effective_balance = movement.intended_balance;
			movement.offchain_fee = fee;
			movement.sent_to.clear();
			let mut stage = "committed";
			movement.status = MovementStatus::Pending;
			if let Some(attempt) = reference {
				stage = match attempt.stage {
					FallbackPayoutStage::Committed => "committed", FallbackPayoutStage::Broadcast => "broadcast",
					FallbackPayoutStage::Confirmed => "confirmed", FallbackPayoutStage::Conflicted => "conflicted",
				};
				if attempt.disposition == FallbackDisposition::BelowDust {
					stage = "awaiting_settlement";
					if entitlements.iter().all(|entry| self.verified_terminals.get(&entry.entitlement_id)
						.is_some_and(|proof| matches!(&proof.disposition,
							FallbackTerminalDisposition::BelowDust(verified) if entry.active_attempt().ok().flatten() == Some(verified))
							&& entry.active_attempt().ok().flatten().and_then(|current| current.parent.as_ref()) == Some(&proof.transaction))) {
						stage = "below_dust";
						movement.status = MovementStatus::Successful;
					}
				} else if let Some(script) = &attempt.script {
					let address = bitcoin::Address::from_script(script, network)?;
					movement.sent_to.push(MovementDestination::bitcoin(address, payout));
					if entitlements.iter().all(|e| self.verified_payouts.get(&e.entitlement_id).is_some_and(|proof|
						proof.allocation_id == attempt.allocation_id
						&& Some(proof.outpoint.vout) == attempt.output_index
						&& attempt.parent.as_ref().is_some_and(|parent| parent.compute_txid() == proof.outpoint.txid))) {
						stage = "confirmed";
						movement.status = MovementStatus::Successful;
					}
				}
			}
			if external_exit {
				stage = "awaiting_settlement";
				if entitlements.iter().all(|entry| self.verified_terminals.get(&entry.entitlement_id)
					.is_some_and(|proof| matches!(proof.disposition, FallbackTerminalDisposition::Exited))) {
					stage = "exited";
					movement.status = MovementStatus::Successful;
				}
			}
			if movement.status == MovementStatus::Pending && stage == "confirmed" { stage = "awaiting_settlement"; }
			if !valid && movement.status != MovementStatus::Successful { stage = "awaiting_settlement"; }
			movement.time.completed_at = if movement.status == MovementStatus::Successful { Some(movement.time.updated_at) } else { None };
			movement.metadata.insert("fallback".to_owned(), serde_json::json!({
				"txid": if external_exit {
					self.verified_terminals.get(&first.entitlement_id).map(|proof| proof.transaction.compute_txid())
						.or_else(|| first.terminal_claim.as_ref().map(|claim| claim.txid))
				} else { reference.and_then(|a| a.parent.as_ref()).map(Transaction::compute_txid) },
				"disposition": if external_exit { "exited" } else { match reference.map(|attempt| attempt.disposition) {
					Some(FallbackDisposition::BelowDust) => "below_dust", Some(FallbackDisposition::Operator) => "operator",
					_ => "customer_payout",
				}},
				"output_index": reference.and_then(|a| a.output_index),
				"allocation_id": reference.map(|a| a.allocation_id),
				"stage": stage, "payout_sat": payout.to_sat(), "forfeited_sat": forfeited.to_sat(),
				"chain_view_valid": valid, "chain_verified": movement.status == MovementStatus::Successful, "entitlements": entitlements,
			}));
			result.push(movement);
		}
		result.sort_by(|a, b| b.time.created_at.cmp(&a.time.created_at).then_with(|| b.id.cmp(&a.id)));
		Ok(result)
	}
}

#[cfg(test)]
mod tests {
	use bitcoin::hashes::Hash;
	use super::*;
	use std::str::FromStr;

	fn entry(id: i64, allocation: i64, payout: u64, fee: u64) -> FallbackEntitlement {
		FallbackEntitlement {
			entitlement_id: id,
			vtxo_id: VtxoId::from_str(&format!("{:064x}:0", id)).unwrap(),
			gross: Amount::from_sat(5_000), chain_view_valid: true,
			attempts: vec![FallbackAttempt {
				allocation_id: allocation, active: true, stage: FallbackPayoutStage::Committed,
				disposition: FallbackDisposition::CustomerPayout, output_index: Some(0),
				script: Some(ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap()),
				gross: Amount::from_sat(5_000), fee: Amount::from_sat(fee), payout: Amount::from_sat(payout),
				forfeited: Amount::ZERO, output_amount: Some(Amount::from_sat(9_000)), parent: None, confirmation: None,
			}], terminal_claim: None, first_seen: Local::now(), last_updated: Local::now(),
		}
	}

	fn reserve(snapshot: &mut FallbackSnapshot) {
		let keys = snapshot.groups().unwrap().into_keys().collect::<Vec<_>>();
		for key in keys {
			let id = MovementId::new(snapshot.movement_ids.len() as u32 + 1);
			snapshot.movement_ids.entry(key).or_insert(id);
		}
	}

	fn total_debit(snapshot: &FallbackSnapshot) -> i64 {
		snapshot.project(vec![], Network::Regtest).unwrap().iter().map(|m| m.effective_balance.to_sat()).sum()
	}

	#[test]
	fn regroup_split_and_reorg_never_repeat_entitlement_debits() {
		let mut snapshot = FallbackSnapshot::default();
		for id in [1, 2] {
			snapshot.entitlements.insert(id, entry(id, 10, 4_500, 500));
			snapshot.local_debits.insert(id, Amount::from_sat(5_000));
		}
		reserve(&mut snapshot);
		assert_eq!(snapshot.project(vec![], Network::Regtest).unwrap().len(), 1);
		assert_eq!(total_debit(&snapshot), -10_000);
		for (id, entitlement) in &mut snapshot.entitlements {
			let mut new = entitlement.attempts[0].clone();
			entitlement.attempts[0].active = false;
			entitlement.attempts[0].stage = FallbackPayoutStage::Conflicted;
			new.allocation_id = 20 + *id;
			new.output_amount = Some(Amount::from_sat(4_000));
			new.fee = Amount::from_sat(1_000);
			new.payout = Amount::from_sat(4_000);
			entitlement.attempts.push(new);
		}
		reserve(&mut snapshot);
		assert_eq!(snapshot.project(vec![], Network::Regtest).unwrap().len(), 2);
		assert_eq!(total_debit(&snapshot), -10_000);
		for entitlement in snapshot.entitlements.values_mut() { entitlement.chain_view_valid = false; }
		let history = snapshot.project(vec![], Network::Regtest).unwrap();
		assert!(history.iter().all(|m| m.status == MovementStatus::Pending && m.metadata["fallback"]["stage"] == "awaiting_settlement"));
		assert_eq!(total_debit(&snapshot), -10_000);
		assert!(history.iter().all(|m| m.metadata["fallback"]["entitlements"][0]["attempts"].as_array().unwrap().len() == 2));
	}

	#[test]
	fn server_confirmed_without_independent_chain_observation_stays_pending() {
		let mut snapshot = FallbackSnapshot::default();
		let mut entitlement = entry(1, 10, 4_000, 1_000);
		let attempt = &mut entitlement.attempts[0];
		attempt.stage = FallbackPayoutStage::Confirmed;
		attempt.confirmation = Some(FallbackBlock { height: 123.into(), hash: BlockHash::all_zeros() });
		snapshot.entitlements.insert(1, entitlement);
		reserve(&mut snapshot);
		let history = snapshot.project(vec![], Network::Regtest).unwrap();
		assert_eq!(history[0].status, MovementStatus::Pending);
		assert_eq!(history[0].metadata["fallback"]["stage"], "awaiting_settlement");
		assert_eq!(history[0].metadata["fallback"]["chain_verified"], false);
	}

	#[test]
	fn protective_exit_and_fallback_reserve_the_same_principal_once_across_reorg() {
		let mut snapshot = FallbackSnapshot::default();
		let entitlement = entry(1, 10, 4_000, 1_000);
		let coin = entitlement.vtxo_id;
		snapshot.entitlements.insert(1, entitlement.clone());
		snapshot.local_debits.insert(1, Amount::from_sat(5_000));
		reserve(&mut snapshot);
		let mut exit = Movement::new(MovementId::new(20), MovementStatus::Pending,
			&MovementSubsystem { name: "bark.exit".into(), kind: "exit".into() }, Local::now());
		exit.input_vtxos = vec![coin];
		exit.effective_balance = bitcoin::SignedAmount::from_sat(-5_000);
		exit.metadata.insert("protective_exit".into(), serde_json::Value::Bool(true));
		let total = |snapshot: &FallbackSnapshot| snapshot.project(vec![exit.clone()], Network::Regtest).unwrap()
			.iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>();
		assert_eq!(total(&snapshot), -5_000);
		snapshot.verified_payouts.insert(1, FallbackVerifiedPayout { allocation_id: 10,
			outpoint: OutPoint::null(), attempt: entitlement.attempts[0].clone(),
			confirmation: FallbackBlock { height: 123.into(), hash: BlockHash::all_zeros() } });
		assert_eq!(total(&snapshot), -5_000);
		let history = snapshot.project(vec![exit.clone()], Network::Regtest).unwrap();
		assert_eq!(history.iter().find(|entry| entry.id == exit.id).unwrap().status, MovementStatus::Canceled);
		snapshot.verified_payouts.clear();
		assert_eq!(total(&snapshot), -5_000);
		assert_eq!(snapshot.project(vec![exit.clone()], Network::Regtest).unwrap().iter()
			.find(|entry| entry.id == exit.id).unwrap().status, MovementStatus::Pending);
	}

	#[test]
	fn unknown_descendant_has_receipt_without_inventing_local_ark_debit() {
		let mut snapshot = FallbackSnapshot::default();
		snapshot.entitlements.insert(1, entry(1, 10, 4_500, 500));
		reserve(&mut snapshot);
		let history = snapshot.project(vec![], Network::Regtest).unwrap();
		assert_eq!(history.len(), 1);
		assert_eq!(history[0].effective_balance.to_sat(), 0);
		assert_eq!(history[0].sent_to[0].amount.to_sat(), 4_500);
		assert_eq!(history[0].status, MovementStatus::Pending, "a decision without broadcast is not paid");
	}
	#[test]
	fn dust_projection_requires_chain_proof_and_rearms_on_reorg_without_repeating_principal() {
		let mut snapshot = FallbackSnapshot::default();
		let mut entitlement = entry(1, 10, 0, 0);
		entitlement.gross = Amount::from_sat(1_200);
		let attempt = &mut entitlement.attempts[0];
		attempt.gross = entitlement.gross;
		attempt.disposition = FallbackDisposition::BelowDust;
		attempt.forfeited = entitlement.gross;
		attempt.output_index = None;
		attempt.output_amount = None;
		let parent = Transaction { version: bitcoin::transaction::Version::non_standard(3),
			lock_time: bitcoin::absolute::LockTime::ZERO, input: vec![], output: vec![] };
		attempt.parent = Some(parent.clone());
		attempt.stage = FallbackPayoutStage::Confirmed;
		let confirmation = FallbackBlock { height: 123.into(), hash: BlockHash::all_zeros() };
		attempt.confirmation = Some(confirmation.clone());
		let proof = FallbackVerifiedTerminal { disposition: FallbackTerminalDisposition::BelowDust(attempt.clone()),
			transaction: parent, confirmation };
		snapshot.entitlements.insert(1, entitlement);
		snapshot.local_debits.insert(1, Amount::from_sat(1_200));
		reserve(&mut snapshot);
		assert_eq!(snapshot.project(vec![], Network::Regtest).unwrap()[0].status, MovementStatus::Pending);
		assert!(!snapshot.is_discharged(1));
		snapshot.terminal_observations.insert(1, vec![proof.clone()]);
		snapshot.verified_terminals.insert(1, proof);
		let history = snapshot.project(vec![], Network::Regtest).unwrap();
		assert_eq!(history.len(), 1);
		assert_eq!(history[0].status, MovementStatus::Successful);
		assert_eq!(history[0].metadata["fallback"]["stage"], "below_dust");
		assert_eq!(history[0].metadata["fallback"]["forfeited_sat"], 1_200);
		assert!(history[0].sent_to.is_empty(), "dust was not paid to an onchain destination");
		assert_eq!(history[0].offchain_fee, Amount::ZERO);
		assert_eq!(total_debit(&snapshot), -1_200);
		assert!(snapshot.is_discharged(1));
		// Chain verification clears current proof, while retaining the actual
		// prior transaction to recheck even with the Ark server unavailable.
		snapshot.verified_terminals.clear();
		assert!(!snapshot.is_discharged(1));
		assert_eq!(snapshot.terminal_observations[&1].len(), 1);
		assert_eq!(snapshot.project(vec![], Network::Regtest).unwrap()[0].status, MovementStatus::Pending);
		assert_eq!(total_debit(&snapshot), -1_200);
	}

	#[test]
	fn operator_and_exit_receipts_never_invent_an_additional_fallback_payout() {
		let mut snapshot = FallbackSnapshot::default();
		let mut operator = entry(1, 10, 0, 0);
		let attempt = &mut operator.attempts[0];
		attempt.disposition = FallbackDisposition::Operator;
		attempt.output_index = None;
		attempt.output_amount = None;
		attempt.script = None;
		assert!(operator.verify(&ScriptBuf::new()).is_ok());
		let mut exited = entry(2, 20, 4_000, 1_000);
		exited.terminal_claim = Some(FallbackTerminalClaim { txid: Txid::all_zeros(),
			confirmation: FallbackBlock { height: 123.into(), hash: BlockHash::all_zeros() } });
		snapshot.awaiting_settlement.extend([operator.vtxo_id, exited.vtxo_id]);
		for entitlement in [operator, exited] {
			snapshot.local_debits.insert(entitlement.entitlement_id, entitlement.gross);
			snapshot.entitlements.insert(entitlement.entitlement_id, entitlement);
		}
		reserve(&mut snapshot);
		let exited_coin = snapshot.entitlements[&2].vtxo_id;
		let mut native_exit = Movement::new(MovementId::new(50), MovementStatus::Pending,
			&MovementSubsystem { name: "bark.exit".into(), kind: "exit".into() }, Local::now());
		native_exit.effective_balance = bitcoin::SignedAmount::from_sat(-5_000);
		native_exit.input_vtxos = vec![exited_coin];
		let history = snapshot.project(vec![native_exit], Network::Regtest).unwrap();
		assert_eq!(history.len(), 1, "native exit remains the only outgoing history entry");
		assert_eq!(history[0].effective_balance.to_sat(), -5_000);
		assert_eq!(snapshot.entitlements.len(), 2, "account receipts remain available through the API");
		assert_eq!(snapshot.awaiting_settlement.len(), 2);
		assert!(!snapshot.is_discharged(1) && !snapshot.is_discharged(2), "server labels alone do not end protection");
	}

	#[test]
	fn old_snapshots_default_new_terminal_observations_without_losing_exit_material_markers() {
		let mut snapshot = FallbackSnapshot::default();
		let coin = entry(1, 10, 4_000, 1_000).vtxo_id;
		snapshot.adopted_payout_vtxos.insert(coin);
		let mut json = serde_json::to_value(snapshot).unwrap();
		json.as_object_mut().unwrap().remove("verified_terminals");
		json.as_object_mut().unwrap().remove("terminal_observations");
		let restored: FallbackSnapshot = serde_json::from_value(json).unwrap();
		assert!(restored.verified_terminals.is_empty() && restored.terminal_observations.is_empty());
		assert!(restored.adopted_payout_vtxos.contains(&coin));
	}

	#[test]
	fn another_device_exit_retains_one_principal_debit_without_inventing_a_payout() {
		let mut snapshot = FallbackSnapshot::default();
		let mut entitlement = entry(1, 10, 4_000, 1_000);
		let confirmation = FallbackBlock { height: 123.into(), hash: BlockHash::all_zeros() };
		let transaction = Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO, input: vec![], output: vec![] };
		entitlement.terminal_claim = Some(FallbackTerminalClaim { txid: transaction.compute_txid(), confirmation: confirmation.clone() });
		snapshot.local_debits.insert(1, entitlement.gross);
		snapshot.entitlements.insert(1, entitlement);
		reserve(&mut snapshot);
		let pending = snapshot.project(vec![], Network::Regtest).unwrap();
		assert_eq!(pending.len(), 1);
		assert_eq!(pending[0].effective_balance.to_sat(), -5_000);
		assert_eq!(pending[0].status, MovementStatus::Pending);
		assert_eq!(pending[0].metadata["fallback"]["disposition"], "exited");
		assert!(pending[0].sent_to.is_empty());
		snapshot.verified_terminals.insert(1, FallbackVerifiedTerminal {
			disposition: FallbackTerminalDisposition::Exited, transaction, confirmation });
		let final_history = snapshot.project(vec![], Network::Regtest).unwrap();
		assert_eq!(final_history.len(), 1);
		assert_eq!(final_history[0].effective_balance.to_sat(), -5_000);
		assert_eq!(final_history[0].status, MovementStatus::Successful);
		assert_eq!(final_history[0].subsystem.kind, "exit");
		assert_eq!(final_history[0].metadata["fallback"]["stage"], "exited");
		assert_eq!(final_history[0].metadata["fallback"]["payout_sat"], 0);
		assert_eq!(final_history[0].offchain_fee, Amount::ZERO);
		assert!(final_history[0].sent_to.is_empty());
		snapshot.verified_terminals.clear();
		let reorged = snapshot.project(vec![], Network::Regtest).unwrap();
		assert_eq!(reorged.len(), 1);
		assert_eq!(reorged[0].effective_balance.to_sat(), -5_000);
		assert_eq!(reorged[0].status, MovementStatus::Pending);
	}

}
