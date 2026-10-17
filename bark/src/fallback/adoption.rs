//! A server Spent status requires actual discharge or a complete signed ordinary transfer.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ark::{Vtxo, VtxoId};
use ark::vtxo::Full;
use server_rpc::protos::VtxoSpendState;

use crate::Wallet;
use crate::actions::{WalletAction, WalletActionCheckpoint};
use crate::vtxo::{VtxoState, VtxoStateKind};

impl Wallet {
	/// Real action/round records take priority over recovery quarantine. In
	/// particular a preparatory offboard's temporary None holder is not ours.
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

	/// Called only AFTER the caller has fetched these coins' Spent statuses.
	/// A successful complete history obtained before that fetch is insufficient:
	/// a decision can commit in between. Preserve signed bodies before asking
	/// another server endpoint, and never invent a historical Ark debit.
	pub(crate) async fn adopt_reported_spent(&self, coins: &[&Vtxo<Full>]) -> anyhow::Result<HashSet<VtxoId>> {
		if coins.is_empty() { return Ok(HashSet::new()); }
		let mut prior_states = HashMap::new();
		for coin in coins {
			if let Some(stored) = self.inner.db.get_wallet_vtxo(coin.id()).await? {
				prior_states.insert(coin.id(), stored.state);
			}
		}
		self.store_vtxos(coins.iter().copied(), &VtxoState::Locked { holder: None }).await?;
		let mut validated = HashSet::new();
		for coin in coins {
			match self.validate_vtxo(coin).await {
				Ok(()) => { validated.insert(coin.id()); },
				Err(error) => log::warn!("Spent recovery coin {} awaits chain validation: {}", coin.id(), error),
			}
		}
		{
			let _exit = self.exit_mgr().lock_ordinary_spend_publication().await;
			let _guard = self.inner.lock_manager.lock(
				&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
			).await?;
			let claimed = self.operation_input_ids().await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			for coin in coins {
				if snapshot.verified_ordinary_spends.contains(&coin.id()) {
					if !claimed.contains(&coin.id()) && self.retire_verified_ordinary_source(coin.id(), &snapshot).await? {
						snapshot.quarantined_vtxos.remove(&coin.id());
						snapshot.unproven_spent_vtxos.remove(&coin.id());
						snapshot.awaiting_settlement.remove(&coin.id());
					}
					continue;
				}
				snapshot.awaiting_settlement.insert(coin.id());
				if !snapshot.entitlements.values().any(|entry| entry.vtxo_id == coin.id()) {
					// Mere restore/import presence cannot invent ownership. An
					// already locally held positive state, however, retains its
					// pending principal until an actual discharge is established.
					let locally_owned = !snapshot.unproven_spent_vtxos.contains(&coin.id())
						&& prior_states.get(&coin.id()).is_some_and(|state| matches!(state,
							VtxoState::Spendable | VtxoState::Exited | VtxoState::Locked { holder: Some(_) })
							|| (*state == VtxoState::Locked { holder: None } && claimed.contains(&coin.id())));
					if !locally_owned { snapshot.unproven_spent_vtxos.insert(coin.id()); }
				}
				if !claimed.contains(&coin.id()) {
					let Some(stored) = self.inner.db.get_wallet_vtxo(coin.id()).await? else { continue; };
					if stored.state == (VtxoState::Locked { holder: None }) {
						snapshot.quarantined_vtxos.insert(coin.id());
					} else if stored.state == VtxoState::Spent {
						// Checkpoint this exact quarantine before rearming our retired
						// historical body. No foreign action holder can be changed.
						snapshot.quarantined_vtxos.insert(coin.id());
						self.inner.db.store_fallback_snapshot(&snapshot).await?;
						if let Err(error) = self.inner.db.update_vtxo_state_checked(coin.id(),
							VtxoState::Locked { holder: None }, &[VtxoStateKind::Spent]).await {
							log::debug!("Historical Spent quarantine deferred: {:#}", error);
						}
					}
				}
			}
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
		}
		let barrier = async {
			// Read-only wallets without an authenticated registration cannot
			// interpret an empty history as proof of an ordinary historic spend.
			self.fallback_destination().await?;
			self.sync_fallback_payouts().await
		}.await;
		if let Err(error) = barrier {
			log::warn!("Spent status has no complete post-status history; retaining quarantine: {:#}", error);
			return Ok(HashSet::new());
		}
		// Never hold the history/action lock across proof RPC or chain I/O.
		// A bounded pass preserves all remaining bodies for a subsequent sync.
		let history = self.inner.db.get_fallback_snapshot().await?;
		let mut ordinary = coins.iter().map(|coin| coin.id())
			.filter(|id| history.verified_ordinary_spends.contains(id)).collect::<HashSet<_>>();
		let mut candidates = coins.iter().copied().filter(|coin| validated.contains(&coin.id())
			&& !ordinary.contains(&coin.id())
			&& !history.entitlements.values().any(|entry| entry.vtxo_id == coin.id())).collect::<Vec<_>>();
		candidates.sort_unstable_by_key(|coin| coin.id());
		if let Some(cursor) = history.ordinary_spend_cursor {
			let offset = candidates.partition_point(|coin| coin.id() <= cursor);
			candidates.rotate_left(offset);
		}
		let started = tokio::time::Instant::now();
		let mut last_attempt = None;
		for coin in candidates.into_iter().take(32) {
			if started.elapsed() >= Duration::from_secs(10) { break; }
			last_attempt = Some(coin.id());
			match tokio::time::timeout(Duration::from_secs(3), self.verify_reported_ordinary_spend(coin)).await {
				Ok(Ok(())) => { ordinary.insert(coin.id()); },
				Ok(Err(error)) => log::debug!("Spent coin {} has no complete ordinary proof: {:#}", coin.id(), error),
				Err(_) => log::debug!("Ordinary proof lookup timed out for {}", coin.id()),
			}
		}
		let _exit = self.exit_mgr().lock_ordinary_spend_publication().await;
		let _guard = self.inner.lock_manager.lock(
			&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let claimed = self.operation_input_ids().await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		if let Some(cursor) = last_attempt { snapshot.ordinary_spend_cursor = Some(cursor); }
		// This off-chain signature fact survives both process failure and a
		// chain reorg. Checkpoint it BEFORE retiring its exact local holder.
		snapshot.verified_ordinary_spends.extend(ordinary.iter().copied());
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		let mut final_ids = HashSet::new();
		for coin in coins {
			let id = coin.id();
			if !validated.contains(&id) { continue; }
			let entitlement = snapshot.entitlements.iter().find(|(_, entry)| entry.vtxo_id == id);
			let paid = entitlement.is_some_and(|(key, _)| snapshot.is_discharged(*key));
			if !paid && (entitlement.is_some() || !ordinary.contains(&id)) {
				// Empty history cannot authorize retirement. A historical local
				// Spent row also needs its signed body protected without claiming
				// that its old face value is newly owned or spendable.
				continue;
			}
			if ordinary.contains(&id) {
				if !claimed.contains(&id) && self.retire_verified_ordinary_source(id, &snapshot).await? {
					snapshot.quarantined_vtxos.remove(&id);
					snapshot.unproven_spent_vtxos.remove(&id);
					snapshot.awaiting_settlement.remove(&id);
					final_ids.insert(id);
				}
				continue;
			}
			let Some(stored) = self.inner.db.get_wallet_vtxo(id).await? else { continue; };
			let mut retired = false;
			match stored.state {
				VtxoState::Locked { holder: None } if snapshot.quarantined_vtxos.contains(&id) && !claimed.contains(&id) => {
					if paid {
						snapshot.adopted_payout_vtxos.insert(id);
						self.inner.db.store_fallback_snapshot(&snapshot).await?;
					}
					if !self.inner.db.retire_vtxo_lock(id, None).await? { continue; }
					retired = true;
				},
				VtxoState::Spendable | VtxoState::Spent => {
					if paid {
						snapshot.adopted_payout_vtxos.insert(id);
						self.inner.db.store_fallback_snapshot(&snapshot).await?;
					}
				},
				_ => continue, // Never steal another action's lock or retire its exit.
			}
			// A concurrent native action can still win a Spendable input. Its
			// Locked state is deliberately excluded from this checked write.
			if !retired && let Err(error) = self.inner.db.update_vtxo_state_checked(id, VtxoState::Spent,
				&[VtxoStateKind::Spendable, VtxoStateKind::Spent]).await {
				log::warn!("Spent adoption deferred to another action: {:#}", error);
				continue;
			}
			snapshot.quarantined_vtxos.remove(&id);
			snapshot.unproven_spent_vtxos.remove(&id);
			snapshot.awaiting_settlement.remove(&id);
			final_ids.insert(id);
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(final_ids)
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

	/// Retry only this recovery path's quarantines. Chain validation remains
	/// mandatory before ordinary proof adoption or protective exit. A real native
	/// action's persisted inputs always take precedence over a None holder.
	pub(crate) async fn sync_recovery_quarantine(&self) -> anyhow::Result<()> {
		// A crash may follow the immutable proof checkpoint but precede its
		// exact-holder retirement. Finish that local write without any server.
		{
			let _exit = self.exit_mgr().lock_ordinary_spend_publication().await;
			let _guard = self.inner.lock_manager.lock(
				&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
			).await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			let claimed = self.operation_input_ids().await?;
			let pending = snapshot.quarantined_vtxos.iter().chain(&snapshot.awaiting_settlement)
				.chain(&snapshot.unproven_spent_vtxos).copied()
				.filter(|id| snapshot.verified_ordinary_spends.contains(id)).collect::<HashSet<_>>();
			for id in pending {
				if !claimed.contains(&id) && self.retire_verified_ordinary_source(id, &snapshot).await? {
					snapshot.quarantined_vtxos.remove(&id);
					snapshot.unproven_spent_vtxos.remove(&id);
					snapshot.awaiting_settlement.remove(&id);
				}
			}
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
		}
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let mut coins = Vec::new();
		let mut keys = Vec::new();
		let pending = snapshot.quarantined_vtxos.iter().chain(&snapshot.awaiting_settlement).copied()
			.collect::<HashSet<_>>();
		for id in &pending {
			let Some(stored) = self.inner.db.get_wallet_vtxo(*id).await? else { continue; };
			let recoverable = (stored.state == (VtxoState::Locked { holder: None })
				&& snapshot.quarantined_vtxos.contains(id)) || (stored.state == VtxoState::Spendable
				&& !snapshot.entitlements.values().any(|entry| entry.vtxo_id == *id));
			if !recoverable { continue; }
			let coin = self.get_full_vtxo(*id).await?;
			keys.push((*id, self.get_vtxo_key(&coin).await?));
			coins.push(coin);
		}
		if coins.is_empty() { return Ok(()); }
		let queries = keys.iter().map(|(id, key)| (*id, key)).collect::<Vec<_>>();
		let states = self.fetch_vtxo_spend_states(&queries).await?;
		let spent = coins.iter().filter(|coin| states.get(&coin.id()) == Some(&VtxoSpendState::Spent)).collect::<Vec<_>>();
		self.adopt_reported_spent(&spent).await?;
		let free = coins.iter().filter(|coin| states.get(&coin.id()) == Some(&VtxoSpendState::Spendable)).collect::<Vec<_>>();
		if free.is_empty() { return Ok(()); }
		// The post-status barrier applies to a previously uncertain coin even
		// when the latest status is Spendable. Never use a stale empty cache.
		self.fallback_destination().await?;
		self.sync_fallback_payouts().await?;
		let _guard = self.inner.lock_manager.lock(
			&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let claimed = self.operation_input_ids().await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		for coin in free {
			let id = coin.id();
			if claimed.contains(&id) || !snapshot.quarantined_vtxos.contains(&id)
				|| snapshot.entitlements.values().any(|entry| entry.vtxo_id == id) { continue; }
			self.inner.db.release_vtxo_lock(id, None).await?;
			snapshot.quarantined_vtxos.remove(&id);
			snapshot.unproven_spent_vtxos.remove(&id);
			snapshot.awaiting_settlement.remove(&id);
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(())
	}
}
