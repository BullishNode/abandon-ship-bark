//! Retain signed bodies until the actual spend can be verified.

use std::collections::{HashMap, HashSet};
use std::time::Duration;
use ark::{Vtxo, VtxoId};
use ark::vtxo::Full;
use server_rpc::protos::VtxoSpendState;
use crate::Wallet;
use crate::actions::{WalletAction, WalletActionCheckpoint};
use crate::vtxo::{VtxoState, VtxoStateKind};

impl Wallet {
	pub(crate) async fn operation_input_ids(&self) -> anyhow::Result<HashSet<VtxoId>> {
		let mut ids = HashSet::new();
		for round in self.pending_round_states().await? {
			ids.extend(round.state().participation().inputs.iter().map(|coin| coin.id()));
		}
		for action in self.inner.db.get_all_wallet_action_checkpoints().await? {
			ids.extend(match action {
				WalletActionCheckpoint::LightningSend(action) => action.pending_balance_vtxo_ids(),
				WalletActionCheckpoint::LightningReceive(action) => action.pending_balance_vtxo_ids(),
				WalletActionCheckpoint::ArkoorSend(action) => action.pending_balance_vtxo_ids(),
				WalletActionCheckpoint::Board(action) => action.pending_balance_vtxo_ids(),
				WalletActionCheckpoint::Offboard(action) => action.pending_balance_vtxo_ids(),
			});
		}
		Ok(ids)
	}


	pub(crate) async fn adopt_reported_spent(&self, coins: &[&Vtxo<Full>]) -> anyhow::Result<HashSet<VtxoId>> {
		if coins.is_empty() { return Ok(HashSet::new()); }
		let mut prior = HashMap::new();
		for coin in coins {
			if let Some(stored) = self.inner.db.get_wallet_vtxo(coin.id()).await? { prior.insert(coin.id(), stored.state); }
		}
		self.store_vtxos(coins.iter().copied(), &VtxoState::Locked { holder: None }).await?;
		let mut validated = HashSet::new();
		for coin in coins {
			if self.validate_vtxo(coin).await.is_ok() { validated.insert(coin.id()); }
		}
		{
			let _exit = self.exit_mgr().lock_ordinary_spend_publication().await;
			let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
			let claimed = self.operation_input_ids().await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			for coin in coins {
				let id = coin.id();
				if snapshot.verified_ordinary_spends.contains(&id) { continue; }
				snapshot.awaiting_settlement.insert(id);
				let owned = prior.get(&id).is_some_and(|state| matches!(state,
					VtxoState::Spendable | VtxoState::Exited | VtxoState::Locked { holder: Some(_) })
					|| (*state == VtxoState::Locked { holder: None } && claimed.contains(&id)));
				if !owned { snapshot.unproven_spent_vtxos.insert(id); }
				if !claimed.contains(&id) {
					if let Some(stored) = self.inner.db.get_wallet_vtxo(id).await? {
						if stored.state == (VtxoState::Locked { holder: None }) { snapshot.quarantined_vtxos.insert(id); }
						else if stored.state == VtxoState::Spent {
							snapshot.quarantined_vtxos.insert(id);
							self.inner.db.store_fallback_snapshot(&snapshot).await?;
							self.inner.db.update_vtxo_state_checked(id, VtxoState::Locked { holder: None }, &[VtxoStateKind::Spent]).await?;
						}
					}
				}
			}
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
		}
		let paid = if self.sync_fallback_payouts().await.is_ok() {
			self.inner.db.get_fallback_snapshot().await?.adopted_payout_vtxos
		} else { Default::default() };
		let mut ordinary = HashSet::new();
		let previous = self.inner.db.get_fallback_snapshot().await?;
		for coin in coins.iter().filter(|coin| validated.contains(&coin.id())).take(32) {
			if previous.verified_ordinary_spends.contains(&coin.id())
				|| matches!(tokio::time::timeout(Duration::from_secs(3), self.verify_reported_ordinary_spend(coin)).await, Ok(Ok(()))) {
				ordinary.insert(coin.id());
			}
		}
		let _exit = self.exit_mgr().lock_ordinary_spend_publication().await;
		let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
		let claimed = self.operation_input_ids().await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		snapshot.verified_ordinary_spends.extend(ordinary.iter().copied());
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		let mut retired = HashSet::new();
		for coin in coins {
			let id = coin.id();
			if !validated.contains(&id) || claimed.contains(&id) { continue; }
			if ordinary.contains(&id) {
				if !self.retire_verified_ordinary_source(id, &snapshot).await? { continue; }
			} else if paid.contains(&id) {
				let Some(stored) = self.inner.db.get_wallet_vtxo(id).await? else { continue; };
				if stored.state == (VtxoState::Locked { holder: None }) && snapshot.quarantined_vtxos.contains(&id) {
					if !self.inner.db.retire_vtxo_lock(id, None).await? { continue; }
				} else if matches!(stored.state, VtxoState::Spendable | VtxoState::Spent) {
					self.inner.db.update_vtxo_state_checked(id, VtxoState::Spent,
						&[VtxoStateKind::Spendable, VtxoStateKind::Spent]).await?;
				} else { continue; }
			} else { continue; }
			snapshot.quarantined_vtxos.remove(&id);
			snapshot.unproven_spent_vtxos.remove(&id);
			snapshot.awaiting_settlement.remove(&id);
			retired.insert(id);
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(retired)
	}

	async fn retire_verified_ordinary_source(
		&self, id: VtxoId, snapshot: &super::payouts::FallbackSnapshot,
	) -> anyhow::Result<bool> {
		let Some(coin) = self.inner.db.get_wallet_vtxo(id).await? else { return Ok(false); };
		ensure!(snapshot.verified_ordinary_spends.contains(&id), "ordinary retirement lacks its durable signed proof");
		match coin.state {
			VtxoState::Locked { holder: Some(ref holder) } => {
				let Some(entry) = self.inner.db.get_exit_vtxo_entry(&id).await? else { return Ok(false); };
				let Some(movement_id) = entry.movement_id else { return Ok(false); };
				let movement = self.inner.db.get_movement_by_id(movement_id).await?;
				if !crate::exit::ordinary_proof_retires_protective_exit(&entry, &movement, holder) { return Ok(false); }
				if !self.inner.db.retire_vtxo_lock(id, Some(holder)).await? { return Ok(false); }
				self.inner.movements.finish_movement_with_update(movement_id,
					crate::movement::MovementStatus::Canceled, crate::movement::update::MovementUpdate::new()
						.effective_balance(bitcoin::SignedAmount::ZERO)).await?;
				Ok(true)
			},
			VtxoState::Locked { holder: None } if snapshot.quarantined_vtxos.contains(&id) =>
				self.inner.db.retire_vtxo_lock(id, None).await,
			VtxoState::Spendable | VtxoState::Spent => {
				self.inner.db.update_vtxo_state_checked(id, VtxoState::Spent,
					&[VtxoStateKind::Spendable, VtxoStateKind::Spent]).await?;
				Ok(true)
			},
			_ => Ok(false),
		}
	}


	pub(crate) async fn sync_recovery_quarantine(&self) -> anyhow::Result<()> {
		// Finish a locally committed signed-spend proof before any server call.
		{
			let _exit = self.exit_mgr().lock_ordinary_spend_publication().await;
			let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			let claimed = self.operation_input_ids().await?;
			let ready = snapshot.quarantined_vtxos.iter().chain(&snapshot.awaiting_settlement)
				.chain(&snapshot.unproven_spent_vtxos).copied().filter(|id|
					snapshot.verified_ordinary_spends.contains(id)).collect::<HashSet<_>>();
			for id in ready {
				if !claimed.contains(&id) && self.retire_verified_ordinary_source(id, &snapshot).await? {
					snapshot.quarantined_vtxos.remove(&id);
					snapshot.unproven_spent_vtxos.remove(&id);
					snapshot.awaiting_settlement.remove(&id);
				}
			}
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
		}
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let pending = snapshot.quarantined_vtxos.iter().chain(&snapshot.awaiting_settlement).copied().collect::<HashSet<_>>();
		let claimed = self.operation_input_ids().await?;
		let mut coins = Vec::new();
		let mut keys = Vec::new();
		for id in pending {
			if claimed.contains(&id) { continue; }
			let Some(stored) = self.inner.db.get_wallet_vtxo(id).await? else { continue; };
			if stored.state != VtxoState::Spendable
				&& !(stored.state == (VtxoState::Locked { holder: None }) && snapshot.quarantined_vtxos.contains(&id)) { continue; }
			let full = self.get_full_vtxo(id).await?;
			keys.push((id, self.get_vtxo_key(&full).await?));
			coins.push(full);
		}
		if coins.is_empty() { return Ok(()); }
		let queries = keys.iter().map(|(id, key)| (*id, key)).collect::<Vec<_>>();
		let states = self.fetch_vtxo_spend_states(&queries).await?;
		let spent = coins.iter().filter(|coin| states.get(&coin.id()) == Some(&VtxoSpendState::Spent)).collect::<Vec<_>>();
		self.adopt_reported_spent(&spent).await?;
		let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
		let claimed = self.operation_input_ids().await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		for coin in &coins {
			let id = coin.id();
			if states.get(&id) != Some(&VtxoSpendState::Spendable) || claimed.contains(&id)
				|| !snapshot.quarantined_vtxos.contains(&id) || snapshot.adopted_payout_vtxos.contains(&id) { continue; }
			self.inner.db.release_vtxo_lock(id, None).await?;
			snapshot.quarantined_vtxos.remove(&id);
			snapshot.unproven_spent_vtxos.remove(&id);
			snapshot.awaiting_settlement.remove(&id);
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(())
	}
}
