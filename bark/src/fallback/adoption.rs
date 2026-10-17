//! A server Spent status is followed by a fresh authenticated history barrier.

use std::collections::HashSet;
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
		self.store_vtxos(coins.iter().copied(), &VtxoState::Locked { holder: None }).await?;
		let mut validated = HashSet::new();
		for coin in coins {
			match self.validate_vtxo(coin).await {
				Ok(()) => { validated.insert(coin.id()); },
				Err(error) => log::warn!("Spent recovery coin {} awaits chain validation: {}", coin.id(), error),
			}
		}
		{
			let _guard = self.inner.lock_manager.lock(
				&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
			).await?;
			let claimed = self.operation_input_ids().await?;
			let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
			for coin in coins {
				snapshot.awaiting_settlement.insert(coin.id());
				if validated.contains(&coin.id()) && !claimed.contains(&coin.id())
					&& self.inner.db.get_wallet_vtxo(coin.id()).await?.is_some_and(|stored|
						stored.state == VtxoState::Locked { holder: None }) {
					snapshot.quarantined_vtxos.insert(coin.id());
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
		let _guard = self.inner.lock_manager.lock(
			&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let claimed = self.operation_input_ids().await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		let mut final_ids = HashSet::new();
		for coin in coins {
			let id = coin.id();
			if !validated.contains(&id) { continue; }
			let entitlement = snapshot.entitlements.iter().find(|(_, entry)| entry.vtxo_id == id);
			let paid = entitlement.is_some_and(|(key, _)| snapshot.is_discharged(*key));
			if entitlement.is_some() && !paid { continue; }
			let Some(stored) = self.inner.db.get_wallet_vtxo(id).await? else { continue; };
			match stored.state {
				VtxoState::Locked { holder: None } if snapshot.quarantined_vtxos.contains(&id) && !claimed.contains(&id) => {
					if paid {
						snapshot.adopted_payout_vtxos.insert(id);
						self.inner.db.store_fallback_snapshot(&snapshot).await?;
					}
					self.inner.db.release_vtxo_lock(id, None).await?;
				},
				VtxoState::Spendable | VtxoState::Spent => {
					if paid {
						snapshot.adopted_payout_vtxos.insert(id);
						self.inner.db.store_fallback_snapshot(&snapshot).await?;
					}
				},
				_ => continue, // Never steal another action's lock or retire its exit.
			}
			// A concurrent native action can still win after release. Its
			// Locked state is deliberately excluded from this checked write.
			if let Err(error) = self.inner.db.update_vtxo_state_checked(id, VtxoState::Spent,
				&[VtxoStateKind::Spendable, VtxoStateKind::Spent]).await {
				log::warn!("Spent adoption deferred to another action: {:#}", error);
				continue;
			}
			snapshot.quarantined_vtxos.remove(&id);
			snapshot.awaiting_settlement.remove(&id);
			final_ids.insert(id);
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(final_ids)
	}

	/// Retry only this recovery path's validated quarantines. A real native
	/// action's persisted inputs always take precedence over a None holder.
	pub(crate) async fn sync_recovery_quarantine(&self) -> anyhow::Result<()> {
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let mut coins = Vec::new();
		let mut keys = Vec::new();
		for id in &snapshot.quarantined_vtxos {
			let Some(stored) = self.inner.db.get_wallet_vtxo(*id).await? else { continue; };
			if stored.state != (VtxoState::Locked { holder: None }) { continue; }
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
			snapshot.awaiting_settlement.remove(&id);
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(())
	}
}
