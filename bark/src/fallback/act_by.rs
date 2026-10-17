//! Derive protection deadlines from the native signed exit path.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use anyhow::Context;
use bitcoin_ext::{BlockHeight, TxStatus};
use ark::{VtxoId, VtxoPolicy};
use ark::vtxo::policy::signing::VtxoSigner;

use crate::Wallet;
use crate::vtxo::{VtxoState, VtxoStateKind};
use super::payouts::{FallbackDisposition, FallbackPayoutStage, FallbackSnapshot};

/// A present wallet must refresh before this deadline, or begin protecting
/// its signed exit path. None of these heights comes from a server spend flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitProtectionSchedule {
	pub vtxo_id: VtxoId,
	pub refresh_by: BlockHeight,
	pub act_by: BlockHeight,
	pub remaining_exit_blocks: u32,
}

/// A scheduled refresh is still pending until its funding is confirmed and
/// the signed replacement was durably adopted. Blocked actions are reported
/// separately: protection never steals their locks.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitProtectionOutcome {
	pub refresh_attempted: Vec<VtxoId>,
	pub exits_started: Vec<VtxoId>,
	pub blocked: Vec<VtxoId>,
}

/// A single coin's failed protection step. Retrying or restoring the named
/// local record must not prevent unrelated coins from reaching their deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitProtectionIssue {
	pub vtxo_id: Option<VtxoId>,
	pub operation: String,
	pub detail: String,
}

/// Partial scheduling is inspectable without presenting a failed coin as safe.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitProtectionReport {
	pub schedules: Vec<ExitProtectionSchedule>,
	pub issues: Vec<ExitProtectionIssue>,
	/// Historical bodies remain watched, but do not authorize automatic exits
	/// without prior positive ownership or a current customer entitlement.
	pub unproven_historical: Vec<VtxoId>,
}

/// Protection made only partial progress. The actual outcome remains available
/// to callers, while the error prevents a failed batch being called complete.
#[derive(Debug)]
pub struct ExitProtectionIncomplete {
	pub outcome: ExitProtectionOutcome,
	pub report: ExitProtectionReport,
}

impl fmt::Display for ExitProtectionIncomplete {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Exit protection incomplete: {} issue(s), {} exit(s) started",
			self.report.issues.len(), self.outcome.exits_started.len())?;
		for issue in &self.report.issues {
			write!(f, "; {:?} {}: {}", issue.vtxo_id, issue.operation, issue.detail)?;
		}
		Ok(())
	}
}

impl std::error::Error for ExitProtectionIncomplete {}

impl ExitProtectionReport {
	fn issue(&mut self, id: Option<VtxoId>, operation: &str, error: impl fmt::Display) {
		let issue = ExitProtectionIssue { vtxo_id: id, operation: operation.to_owned(), detail: error.to_string() };
		if !self.issues.contains(&issue) { self.issues.push(issue); }
	}

	fn collect(&mut self, id: VtxoId, result: anyhow::Result<ExitProtectionSchedule>) {
		match result {
			Ok(schedule) => self.schedules.push(schedule),
			Err(error) => self.issue(Some(id), "derive_schedule", format!("{:#}", error)),
		}
	}

	fn finish(self, outcome: ExitProtectionOutcome) -> anyhow::Result<ExitProtectionOutcome> {
		if self.issues.is_empty() { Ok(outcome) }
		else { Err(ExitProtectionIncomplete { outcome, report: self }.into()) }
	}
}

fn automatic_exit_ownership_known(snapshot: &FallbackSnapshot, id: VtxoId) -> anyhow::Result<bool> {
	if snapshot.verified_ordinary_spends.contains(&id) || snapshot.entitlements.iter().any(|(key, receipt)|
		receipt.vtxo_id == id && snapshot.is_discharged(*key)) { return Ok(false); }
	if !snapshot.unproven_spent_vtxos.contains(&id) { return Ok(true); }
	for (key, entitlement) in &snapshot.entitlements {
		if entitlement.vtxo_id == id && entitlement.chain_view_valid && !snapshot.is_discharged(*key)
			&& entitlement.active_attempt()?.is_some_and(|attempt| attempt.stage != FallbackPayoutStage::Conflicted
				&& matches!(attempt.disposition, FallbackDisposition::CustomerPayout | FallbackDisposition::BelowDust)) {
			return Ok(true);
		}
	}
	Ok(false)
}

fn deadlines(expiry: BlockHeight, path_blocks: u32, claim_csv: u32, margin: u32) -> (BlockHeight, BlockHeight, u32) {
	// The final signed claim also needs a block. Native path progression
	// confirms parents before broadcasting children; this is not depth*CSV.
	let remaining = path_blocks.saturating_add(claim_csv).saturating_add(1);
	let act_by = BlockHeight::new(expiry.to_u32().saturating_sub(remaining.saturating_add(margin)));
	let refresh_by = BlockHeight::new(act_by.to_u32().saturating_sub(margin));
	(act_by, refresh_by, remaining)
}

impl Wallet {
	/// Called only while the exit manager's write guard is held. Ordinary
	/// proof publication uses the same exit -> history order, so its durable
	/// checkpoint cannot fall between this check and the native reservation.
	pub(crate) async fn lock_automatic_exit_ownership(
		&self, id: VtxoId,
	) -> anyhow::Result<Box<dyn crate::lock_manager::LockGuard>> {
		let guard = self.inner.lock_manager.lock(
			&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		ensure!(automatic_exit_ownership_known(&snapshot, id)?,
			"current ownership or discharge evidence does not authorize this automatic exit");
		Ok(guard)
	}

	/// Refresh early enough to retain the native unilateral-exit window. A
	/// bounded refresh request is only an attempt; without an adopted usable
	/// replacement or independently confirmed payout, act-by starts an exit.
	/// Apps in manual-sync mode must call this alongside their sync loop.
	pub async fn protect_expiring_vtxos(&self) -> anyhow::Result<ExitProtectionOutcome> {
		let mut outcome = ExitProtectionOutcome::default();
		let Some(_guard) = self.inner.lock_manager.try_lock(
			&format!("{}.exit-protection", self.fingerprint()),
		).await else { return Ok(outcome); };
		let budget = Duration::from_secs(self.config().daemon_sync_interval_secs.max(1));
		let mut report = ExitProtectionReport::default();

		// A failed receipt proof cannot become finality, but must not suppress
		// unrelated native paths. Persist the verifier's cleared proof view and
		// surface the incomplete evidence after the healthy work has progressed.
		if let Some(record) = self.fallback_registration().await? {
			let _history = self.inner.lock_manager.lock(
				&format!("{}.fallback-history", self.fingerprint()), budget,
			).await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			if let Err(error) = self.verify_fallback_chain_evidence(&mut snapshot, &record.spk).await {
				snapshot.verified_payouts.clear();
				snapshot.verified_terminals.clear();
				report.issue(None, "verify_receipts", format!("{:#}", error));
			}
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
			if let Err(error) = self.apply_fallback_chain_states(&snapshot).await {
				report.issue(None, "apply_receipts", format!("{:#}", error));
			}
		}
		let tip = self.chain().tip_ref_uncached().await?.height;
		let initial = self.exit_protection_report().await?;
		for issue in initial.issues { report.issue(issue.vtxo_id, &issue.operation, issue.detail); }
		report.schedules = initial.schedules;
		report.unproven_historical = initial.unproven_historical;
		let due = report.schedules.iter().filter(|schedule| tip >= schedule.refresh_by)
			.map(|schedule| schedule.vtxo_id).collect::<HashSet<_>>();
		if due.is_empty() { return report.finish(outcome); }

		let mut in_round = HashSet::new();
		// Failure to enumerate holders is common state and remains fatal. Once a
		// named round is known, its lock/write failure is local to its inputs.
		let rounds = self.pending_round_states().await?;
		let unproven = report.unproven_historical.iter().copied().collect::<HashSet<_>>();
		let progress = async {
			for pending in rounds {
				let affected = pending.state().participation().inputs.iter().map(|coin| coin.id())
					.filter(|id| due.contains(id) && !unproven.contains(id)).collect::<Vec<_>>();
				if affected.is_empty() { continue; }
				let operation = format!("progress_round_{}", pending.id());
				let step = async {
					if let Some(mut pending) = self.try_lock_round_state(pending.id()).await? {
						match pending.sync(self).await {
							Ok(status) if status.is_final() => self.inner.db.remove_round_state(&pending).await?,
							Ok(_) => self.inner.db.update_round_state(&pending).await?,
							// A cooperative attempt may normally fail offline; the
							// subsequent native fallback remains the actual protection.
							Err(error) => log::warn!("Protection refresh awaits progress: {:#}", error),
						}
					}
					Ok::<_, anyhow::Error>(())
				};
				if let Err(error) = step.await {
					for id in affected { report.issue(Some(id), &operation, format!("{:#}", error)); }
				}
			}
		};
		if bark_runtime::timeout(budget, progress).await.is_err() {
			log::warn!("Protection refresh batch exceeded its sync interval");
		}
		for pending in self.pending_round_states().await? {
			in_round.extend(pending.state().participation().inputs.iter().map(|coin| coin.id()));
		}
		let refreshable = report.schedules.iter().filter(|schedule| due.contains(&schedule.vtxo_id)
			&& tip < schedule.act_by && !in_round.contains(&schedule.vtxo_id)
			&& !report.unproven_historical.contains(&schedule.vtxo_id))
			.map(|schedule| schedule.vtxo_id).collect::<HashSet<_>>();
		if !refreshable.is_empty() {
			match self.spendable_vtxos().await {
				Ok(coins) => {
					let fresh = coins.iter().map(|coin| coin.id()).filter(|id| refreshable.contains(id)).collect::<Vec<_>>();
					if !fresh.is_empty() {
						outcome.refresh_attempted = fresh.clone();
						match bark_runtime::timeout(budget, self.refresh_vtxos(fresh)).await {
							Ok(Ok(_)) => {},
							Ok(Err(error)) => log::warn!("Early protection refresh failed: {:#}", error),
							Err(_) => log::warn!("Early protection refresh exceeded its sync interval"),
						}
					}
				},
				Err(error) => report.issue(None, "select_refresh", format!("{:#}", error)),
			}
		}

		// Recollect after refresh: only the normal completed swap or independent
		// chain discharge removes a native watch. A broken other record is partial.
		let tip = self.chain().tip_ref_uncached().await?.height;
		let current = self.exit_protection_report().await?;
		for issue in current.issues { report.issue(issue.vtxo_id, &issue.operation, issue.detail); }
		report.schedules = current.schedules;
		report.unproven_historical = current.unproven_historical;
		for schedule in report.schedules.clone() {
			if tip < schedule.act_by || self.exit_mgr().is_exiting(schedule.vtxo_id).await { continue; }
			if report.unproven_historical.contains(&schedule.vtxo_id) {
				outcome.blocked.push(schedule.vtxo_id);
				report.issue(Some(schedule.vtxo_id), "automatic_exit_ownership",
					"historical signed material has no established current ownership; retain it and reconcile the original spend");
				continue;
			}
			match self.protect_scheduled_coin(schedule.vtxo_id).await {
				Ok(true) => outcome.exits_started.push(schedule.vtxo_id),
				Ok(false) => outcome.blocked.push(schedule.vtxo_id),
				Err(error) => {
					outcome.blocked.push(schedule.vtxo_id);
					report.issue(Some(schedule.vtxo_id), "start_exit", format!("{:#}", error));
				},
			}
		}
		report.finish(outcome)
	}

	async fn protect_scheduled_coin(&self, id: VtxoId) -> anyhow::Result<bool> {
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		ensure!(automatic_exit_ownership_known(&snapshot, id)?, "current ownership or discharge evidence does not authorize this automatic exit");
		for pending in self.pending_round_states().await? {
			if !pending.state().participation().inputs.iter().any(|coin| coin.id() == id) { continue; }
			if let Some(mut pending) = self.try_lock_round_state(pending.id()).await? {
				if !pending.cancel_for_protection(self).await?
					&& pending.protect_binding_refresh_input(self, id).await? { return Ok(true); }
			}
		}
		let Some(stored) = self.inner.db.get_wallet_vtxo(id).await? else { return Ok(false); };
		self.exit_mgr().start_protective_exit(&stored.vtxo, self).await
	}

	/// Actual native-chain deadlines for standard coins, including unavailable
	/// pending claims. A successful server response cannot remove this watch.
	pub async fn exit_protection_schedule(&self) -> anyhow::Result<Vec<ExitProtectionSchedule>> {
		let report = self.exit_protection_report().await?;
		if report.issues.is_empty() { Ok(report.schedules) }
		else { Err(ExitProtectionIncomplete { outcome: ExitProtectionOutcome::default(), report }.into()) }
	}

	/// Return healthy schedules together with explicit per-coin failures.
	/// Whole-wallet enumeration and chain-view errors still return Err: no
	/// coherent healthy subset can be asserted without those common facts.
	pub async fn exit_protection_report(&self) -> anyhow::Result<ExitProtectionReport> {
		let tip = self.chain().tip_ref_uncached().await?;
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let discharged = snapshot.entitlements.iter().filter(|(id, _)| snapshot.is_discharged(**id))
			.map(|(_, entitlement)| entitlement.vtxo_id).collect::<HashSet<_>>();
		let mut report = ExitProtectionReport::default();
		for stored in self.inner.db.get_vtxos_by_state(&[VtxoStateKind::Spendable, VtxoStateKind::Locked]).await? {
			if discharged.contains(&stored.id()) || snapshot.verified_ordinary_spends.contains(&stored.id())
				|| !matches!(stored.policy(), VtxoPolicy::Pubkey(_)) { continue; }
			if stored.state == (VtxoState::Locked { holder: None }) && !snapshot.quarantined_vtxos.contains(&stored.id()) { continue; }
			match automatic_exit_ownership_known(&snapshot, stored.id()) {
				Ok(false) => report.unproven_historical.push(stored.id()),
				Ok(true) => {},
				Err(error) => { report.issue(Some(stored.id()), "ownership", format!("{:#}", error)); continue; },
			}
			report.collect(stored.id(), self.coin_exit_schedule(&stored, tip.height).await);
		}
		ensure!(self.chain().block_ref(tip.height).await? == tip, "chain changed while deriving exit deadlines");
		Ok(report)
	}

	async fn coin_exit_schedule(&self, stored: &crate::WalletVtxo, tip: BlockHeight) -> anyhow::Result<ExitProtectionSchedule> {
		let full = self.get_full_vtxo(stored.id()).await?;
		if stored.state == (VtxoState::Locked { holder: None }) {
			self.validate_vtxo(&full).await.context("recovery exit body is not validated")?;
		}
		let clause = self.find_signable_clause(&full).await.context("coin has no locally signable exit clause")?;
		let sequence = clause.sequence().context("standard coin has no delayed claim clause")?;
		ensure!(sequence.0 & (1 << 22) == 0, "time-based claim delay needs a separate protection schedule");
		let mut csv = sequence.0 & 0xffff;
		let mut path_blocks = 0u32;
		for step in full.transactions() {
			if matches!(self.chain().tx_status(step.tx.compute_txid()).await?, TxStatus::Confirmed(_)) { continue; }
			let mut wait = 1;
			for input in &step.tx.input {
				if input.sequence.is_relative_lock_time() {
					ensure!(input.sequence.0 & (1 << 22) == 0, "time-based signed path delay needs a separate schedule");
					wait = wait.max(input.sequence.0 & 0xffff);
				}
			}
			path_blocks = path_blocks.saturating_add(wait);
		}
		if let TxStatus::Confirmed(block) = self.chain().tx_status(full.point().txid).await? {
			csv = csv.saturating_sub(tip.checked_blocks_since(block.height).unwrap_or(0));
		}
		let (act_by, refresh_by, remaining_exit_blocks) = deadlines(full.expiry_height(), path_blocks,
			csv, self.config().vtxo_exit_margin.to_u32());
		Ok(ExitProtectionSchedule { vtxo_id: full.id(), refresh_by, act_by, remaining_exit_blocks })
	}

}

#[cfg(test)]
mod tests {
	use bitcoin::Amount;
	use chrono::Local;
	use crate::{FallbackAttempt, FallbackEntitlement};
	use super::*;

	#[test]
	fn incomplete_schedule_keeps_healthy_work_and_surfaces_the_bad_coin() {
		let good = VtxoId::from_slice(&[1; 36]).unwrap();
		let bad = VtxoId::from_slice(&[2; 36]).unwrap();
		let schedule = ExitProtectionSchedule { vtxo_id: good, refresh_by: BlockHeight::new(8),
			act_by: BlockHeight::new(10), remaining_exit_blocks: 4 };
		let mut report = ExitProtectionReport::default();
		report.collect(bad, Err(anyhow::anyhow!("restore the missing signed path from backup")));
		report.collect(good, Ok(schedule.clone()));
		assert_eq!(report.schedules, vec![schedule]);
		let failure = report.finish(ExitProtectionOutcome { exits_started: vec![good], ..Default::default() }).unwrap_err();
		let partial = failure.downcast_ref::<ExitProtectionIncomplete>().unwrap();
		assert_eq!(partial.outcome.exits_started, vec![good]);
		assert_eq!(partial.report.issues[0].vtxo_id, Some(bad));
		assert_eq!(partial.report.issues[0].operation, "derive_schedule");
		assert!(partial.to_string().contains("restore the missing signed path"));
	}

	#[test]
	fn unproven_historic_body_does_not_authorize_automatic_exit() {
		let id = VtxoId::from_slice(&[3; 36]).unwrap();
		let mut snapshot = FallbackSnapshot::default();
		assert!(automatic_exit_ownership_known(&snapshot, id).unwrap(), "prior positive local ownership remains protected");
		snapshot.unproven_spent_vtxos.insert(id);
		snapshot.quarantined_vtxos.insert(id);
		snapshot.awaiting_settlement.insert(id);
		assert!(!automatic_exit_ownership_known(&snapshot, id).unwrap(), "retention and waiting are not current ownership evidence");
	}

	#[test]
	fn historic_autoexit_requires_current_customer_claim_instead_of_an_operator_label() {
		let id = VtxoId::from_slice(&[4; 36]).unwrap();
		let mut snapshot = FallbackSnapshot::default();
		snapshot.unproven_spent_vtxos.insert(id);
		// This pure cache-policy test assumes the account snapshot was already
		// authenticated. It does not pretend to validate money or a signature.
		snapshot.entitlements.insert(1, FallbackEntitlement { entitlement_id: 1, vtxo_id: id,
			gross: Amount::from_sat(5_000), chain_view_valid: true, terminal_claim: None,
			first_seen: Local::now(), last_updated: Local::now(), attempts: vec![FallbackAttempt {
				allocation_id: 1, active: true, stage: FallbackPayoutStage::Committed,
				disposition: FallbackDisposition::Operator, output_index: None, script: None,
				gross: Amount::from_sat(5_000), fee: Amount::ZERO, payout: Amount::ZERO,
				forfeited: Amount::ZERO, output_amount: None, parent: None, confirmation: None,
			}],
		});
		assert!(!automatic_exit_ownership_known(&snapshot, id).unwrap());
		snapshot.entitlements.get_mut(&1).unwrap().attempts[0].disposition = FallbackDisposition::CustomerPayout;
		assert!(automatic_exit_ownership_known(&snapshot, id).unwrap(), "current admitted customer debt stays protected");
		snapshot.entitlements.get_mut(&1).unwrap().chain_view_valid = false;
		assert!(!automatic_exit_ownership_known(&snapshot, id).unwrap(), "invalid chain view cannot authorize historic autoexit");
		snapshot.entitlements.get_mut(&1).unwrap().chain_view_valid = true;
		snapshot.entitlements.get_mut(&1).unwrap().attempts[0].stage = FallbackPayoutStage::Conflicted;
		assert!(!automatic_exit_ownership_known(&snapshot, id).unwrap(), "conflicted customer attempt is not current evidence");
		snapshot.entitlements.get_mut(&1).unwrap().attempts[0].stage = FallbackPayoutStage::Committed;
		snapshot.entitlements.get_mut(&1).unwrap().attempts[0].active = false;
		assert!(!automatic_exit_ownership_known(&snapshot, id).unwrap(), "inactive/superseded history alone is insufficient");
		snapshot.entitlements.get_mut(&1).unwrap().attempts[0].active = true;
		snapshot.verified_ordinary_spends.insert(id);
		assert!(!automatic_exit_ownership_known(&snapshot, id).unwrap(), "an exact signed discharge cannot be rearmed");
		snapshot.verified_ordinary_spends.remove(&id);
		snapshot.unproven_spent_vtxos.remove(&id);
		snapshot.entitlements.get_mut(&1).unwrap().chain_view_valid = false;
		snapshot.entitlements.get_mut(&1).unwrap().attempts[0].active = false;
		assert!(automatic_exit_ownership_known(&snapshot, id).unwrap(), "prior positive local ownership is not removed by an invalid receipt view");
	}

	#[test]
	fn signed_path_and_claim_delay_can_require_refresh_before_the_normal_threshold() {
		let expiry = BlockHeight::new(1_000);
		let (act_by, refresh_by, remaining) = deadlines(expiry, 1, 144, 12);
		assert_eq!(remaining, 146);
		assert_eq!(act_by, BlockHeight::new(842));
		assert_eq!(refresh_by, BlockHeight::new(830));
		assert!(refresh_by < BlockHeight::new(856), "waiting for the normal144-block refresh point is too late");
		assert_eq!(deadlines(BlockHeight::new(5), u32::MAX, 144, 12).0, BlockHeight::ZERO);
	}
}
