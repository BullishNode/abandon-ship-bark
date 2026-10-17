//! Derive protection deadlines from the native signed exit path.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use bitcoin_ext::{BlockHeight, TxStatus};
use ark::{VtxoId, VtxoPolicy};
use ark::vtxo::policy::signing::VtxoSigner;

use crate::Wallet;
use crate::vtxo::{VtxoState, VtxoStateKind};

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

fn deadlines(expiry: BlockHeight, path_blocks: u32, claim_csv: u32, margin: u32) -> (BlockHeight, BlockHeight, u32) {
	// The final signed claim also needs a block. Native path progression
	// confirms parents before broadcasting children; this is not depth*CSV.
	let remaining = path_blocks.saturating_add(claim_csv).saturating_add(1);
	let act_by = BlockHeight::new(expiry.to_u32().saturating_sub(remaining.saturating_add(margin)));
	let refresh_by = BlockHeight::new(act_by.to_u32().saturating_sub(margin));
	(act_by, refresh_by, remaining)
}

impl Wallet {
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

		// This path reads cached authentic receipts and our own chain only.
		// Server pagination runs separately and cannot suppress protection.
		if let Some(record) = self.fallback_registration().await? {
			let _history = self.inner.lock_manager.lock(
				&format!("{}.fallback-history", self.fingerprint()), budget,
			).await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			self.verify_fallback_chain_evidence(&mut snapshot, &record.spk).await?;
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
			self.apply_fallback_chain_states(&snapshot).await?;
		}
		let tip = self.chain().tip_ref_uncached().await?.height;
		let schedules = self.exit_protection_schedule().await?;
		let due = schedules.iter().filter(|schedule| tip >= schedule.refresh_by)
			.map(|schedule| schedule.vtxo_id).collect::<HashSet<_>>();
		if due.is_empty() { return Ok(outcome); }

		// Progress an existing attempt first. Each network wait is bounded,
		// and a forfeit transmission checkpoints its replacement material.
		let mut in_round = HashSet::new();
		let progress = async {
			for pending in self.pending_round_states().await? {
				if !pending.state().participation().inputs.iter().any(|coin| due.contains(&coin.id())) { continue; }
				if let Some(mut pending) = self.try_lock_round_state(pending.id()).await? {
					match pending.sync(self).await {
						Ok(status) if status.is_final() => self.inner.db.remove_round_state(&pending).await?,
						Ok(_) => self.inner.db.update_round_state(&pending).await?,
						Err(error) => log::warn!("Protection refresh awaits progress: {:#}", error),
					}
				}
			}
			Ok::<_, anyhow::Error>(())
		};
		// One budget for the whole batch, not one full wait per stuck coin.
		match bark_runtime::timeout(budget, progress).await {
			Ok(result) => result?,
			Err(_) => log::warn!("Protection refresh batch exceeded its sync interval"),
		}
		for pending in self.pending_round_states().await? {
			in_round.extend(pending.state().participation().inputs.iter().map(|coin| coin.id()));
		}
		let fresh = self.spendable_vtxos().await?.into_iter()
			.filter(|coin| due.contains(&coin.id()) && !in_round.contains(&coin.id()))
			.filter(|coin| !schedules.iter().any(|schedule| schedule.vtxo_id == coin.id() && tip >= schedule.act_by))
			.map(|coin| coin.id()).collect::<Vec<_>>();
		if !fresh.is_empty() {
			outcome.refresh_attempted = fresh.clone();
			match bark_runtime::timeout(budget, self.refresh_vtxos(fresh)).await {
				Ok(Ok(_)) => {},
				Ok(Err(error)) => log::warn!("Early protection refresh failed: {:#}", error),
				Err(_) => log::warn!("Early protection refresh exceeded its sync interval"),
			}
		}

		// Re-read after any successful refresh. Its old coin is discharged
		// only by the normal confirmed swap, never a mere round ACK.
		let tip = self.chain().tip_ref_uncached().await?.height;
		for schedule in self.exit_protection_schedule().await? {
			if tip < schedule.act_by || self.exit_mgr().is_exiting(schedule.vtxo_id).await { continue; }
			let mut conditional_exit = false;
			for pending in self.pending_round_states().await? {
				if !pending.state().participation().inputs.iter().any(|coin| coin.id() == schedule.vtxo_id) { continue; }
				if let Some(mut pending) = self.try_lock_round_state(pending.id()).await? {
					if !pending.cancel_for_protection(self).await? {
						match pending.protect_binding_refresh_input(self, schedule.vtxo_id).await {
							Ok(true) => { conditional_exit = true; break; },
							Ok(false) => {},
							Err(error) => log::warn!("Binding refresh protection for {} awaits retry: {:#}", schedule.vtxo_id, error),
						}
					}
				}
			}
			if conditional_exit { outcome.exits_started.push(schedule.vtxo_id); continue; }
			let Some(stored) = self.inner.db.get_wallet_vtxo(schedule.vtxo_id).await? else { continue; };
			if stored.state == (VtxoState::Locked { holder: None }) {
				let snapshot = self.inner.db.get_fallback_snapshot().await?;
				if snapshot.quarantined_vtxos.contains(&stored.id())
					&& !self.operation_input_ids().await?.contains(&stored.id()) {
					// This validated recovery lock has no action owner. The
					// actual exit movement takes its own checked holder next.
					self.inner.db.release_vtxo_lock(stored.id(), None).await?;
				}
			}
			let Some(stored) = self.inner.db.get_wallet_vtxo(schedule.vtxo_id).await? else { continue; };
			if stored.state != VtxoState::Spendable { outcome.blocked.push(schedule.vtxo_id); continue; }
			self.exit_mgr().start_protective_exit(&stored.vtxo, self).await?;
			outcome.exits_started.push(schedule.vtxo_id);
		}
		Ok(outcome)
	}

	/// Actual native-chain deadlines for standard coins, including unavailable
	/// pending claims. A successful server response cannot remove this watch.
	pub async fn exit_protection_schedule(&self) -> anyhow::Result<Vec<ExitProtectionSchedule>> {
		let tip = self.chain().tip_ref_uncached().await?;
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let discharged = snapshot.entitlements.iter()
			.filter(|(id, _)| snapshot.is_discharged(**id))
			.map(|(_, entitlement)| entitlement.vtxo_id).collect::<std::collections::HashSet<_>>();
		let mut result = Vec::new();
		for stored in self.inner.db.get_vtxos_by_state(&[VtxoStateKind::Spendable, VtxoStateKind::Locked]).await? {
			if discharged.contains(&stored.id()) || !matches!(stored.policy(), VtxoPolicy::Pubkey(_)) { continue; }
			let full = self.get_full_vtxo(stored.id()).await?;
			if stored.state == (VtxoState::Locked { holder: None }) {
				if !snapshot.quarantined_vtxos.contains(&stored.id()) { continue; }
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
				csv = csv.saturating_sub(tip.height.checked_blocks_since(block.height).unwrap_or(0));
			}
			let (act_by, refresh_by, remaining_exit_blocks) = deadlines(full.expiry_height(), path_blocks,
				csv, self.config().vtxo_exit_margin.to_u32());
			result.push(ExitProtectionSchedule { vtxo_id: full.id(), refresh_by, act_by, remaining_exit_blocks });
		}
		ensure!(self.chain().block_ref(tip.height).await? == tip, "chain changed while deriving exit deadlines");
		Ok(result)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
