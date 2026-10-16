//! Account-owned fallback receipts and their current output projection.
//!
//! An entitlement is permanent. Output groups may merge, split or disappear
//! after an exit or reorg, so historical attempts never create extra debits.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Context;
use bitcoin::{Amount, BlockHash, Network, ScriptBuf, Transaction, Txid};
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
			if attempt.disposition == FallbackDisposition::Operator { continue; }
			ensure!(attempt.fee.checked_add(attempt.payout).and_then(|a| a.checked_add(attempt.forfeited)) == Some(attempt.gross),
				"fallback attempt does not conserve entitlement value");
			ensure!(attempt.script.as_deref() == Some(expected), "fallback receipt names a different registered payout address");
			if attempt.disposition == FallbackDisposition::BelowDust {
				ensure!(attempt.payout == Amount::ZERO && attempt.output_index.is_none(), "dust receipt falsely names a payout");
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
		if self.terminal_claim.is_some() { return Ok(None); }
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
	pub(crate) fn groups(&self) -> anyhow::Result<BTreeMap<String, Vec<&FallbackEntitlement>>> {
		let mut groups = BTreeMap::<String, Vec<&FallbackEntitlement>>::new();
		for entitlement in self.entitlements.values() {
			if let Some(key) = entitlement.group_key()? { groups.entry(key).or_default().push(entitlement); }
		}
		Ok(groups)
	}

	/// Keep regular movements and replace placeholder fallback movements with
	/// exactly the current account projection. Historical allocation attempts
	/// stay in receipt metadata, not as additional outgoing movements.
	pub(crate) fn project(&self, stored: Vec<Movement>, network: Network) -> anyhow::Result<Vec<Movement>> {
		let mut known = HashMap::new();
		let mut result = Vec::new();
		for movement in stored {
			if movement.subsystem.name == FALLBACK_SUBSYSTEM { known.insert(movement.id, movement); }
			else { result.push(movement); }
		}
		for (key, entitlements) in self.groups()? {
			let id = *self.movement_ids.get(&key).context("fallback group lacks reserved history ID")?;
			let first = entitlements[0];
			let time = entitlements.iter().map(|e| e.first_seen).min().expect("nonempty group");
			let mut movement = known.remove(&id).unwrap_or_else(|| Movement::new(id, MovementStatus::Pending,
				&MovementSubsystem { name: FALLBACK_SUBSYSTEM.to_owned(), kind: "payout".to_owned() }, time));
			movement.time.created_at = time;
			movement.time.updated_at = entitlements.iter().map(|e| e.last_updated).max().expect("nonempty group");
			movement.input_vtxos = entitlements.iter().map(|e| e.vtxo_id).collect();
			let mut debit = Amount::ZERO;
			let mut fee = Amount::ZERO;
			let mut payout = Amount::ZERO;
			let mut forfeited = Amount::ZERO;
			let mut valid = true;
			let reference = first.active_attempt()?;
			for entitlement in &entitlements {
				debit = debit.checked_add(self.local_debits.get(&entitlement.entitlement_id).copied().unwrap_or(Amount::ZERO))
					.context("fallback history debit overflow")?;
				valid &= entitlement.chain_view_valid;
				if let Some(attempt) = entitlement.active_attempt()? {
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
					stage = "below_dust";
					movement.status = MovementStatus::Successful;
				} else if let Some(script) = &attempt.script {
					let address = bitcoin::Address::from_script(script, network)?;
					movement.sent_to.push(MovementDestination::bitcoin(address, payout));
					if attempt.stage == FallbackPayoutStage::Confirmed && attempt.confirmation.is_some() {
						movement.status = MovementStatus::Successful;
					}
				}
			}
			if !valid { stage = "awaiting_settlement"; movement.status = MovementStatus::Pending; }
			movement.time.completed_at = if movement.status == MovementStatus::Successful { Some(movement.time.updated_at) } else { None };
			movement.metadata.insert("fallback".to_owned(), serde_json::json!({
				"txid": reference.and_then(|a| a.parent.as_ref()).map(Transaction::compute_txid),
				"output_index": reference.and_then(|a| a.output_index),
				"allocation_id": reference.map(|a| a.allocation_id),
				"stage": stage, "payout_sat": payout.to_sat(), "forfeited_sat": forfeited.to_sat(),
				"chain_view_valid": valid, "entitlements": entitlements,
			}));
			result.push(movement);
		}
		result.sort_by(|a, b| b.time.created_at.cmp(&a.time.created_at).then_with(|| b.id.cmp(&a.id)));
		Ok(result)
	}
}

#[cfg(test)]
mod tests {
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
}
