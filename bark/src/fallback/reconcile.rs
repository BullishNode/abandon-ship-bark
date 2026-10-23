//! Expired inputs are checked against server decisions before a new refresh.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use log::warn;

use ark::VtxoId;
use ark::vtxo::VtxoRef;
use server_rpc::protos::VtxoSpendState;

use crate::Wallet;
use crate::vtxo::VtxoStateKind;

impl Wallet {
	/// Wallet/UI projection of expired coins whose owner or in-flight operation
	/// is still being settled. No absent or failed status response is treated
	/// as either spendable or already paid.
	pub async fn awaiting_settlement_vtxos(&self) -> anyhow::Result<HashSet<VtxoId>> {
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let pending = self.fallback_pending_principal(&snapshot).await?;
		Ok(snapshot.awaiting_settlement.into_iter().chain(pending.into_keys()).collect())
	}


	pub(crate) async fn reconcile_expired_inputs(&self, ids: &[VtxoId]) -> anyhow::Result<HashSet<VtxoId>> {
		if ids.is_empty() { return Ok(HashSet::new()); }
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let principal = self.fallback_pending_principal(&snapshot).await?;
		let mut signed_parents = HashSet::new();
		let mut keyed = Vec::new();
		for id in ids {
			let Some(stored) = self.inner.db.get_wallet_vtxo(*id).await? else { continue; };
			if stored.state.kind() != VtxoStateKind::Spendable { continue; }
			if principal.get(id).is_some_and(|amount| *amount < stored.amount()) { signed_parents.insert(*id); }
			let key = self.get_vtxo_key(&stored.vtxo).await?;
			keyed.push((*id, key));
		}
		let queries = keyed.iter().map(|(id, key)| (*id, key)).collect::<Vec<_>>();
		let statuses = self.fetch_vtxo_spend_states(&queries).await;
		// Remote status must not monopolize the local protection lock.
		let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		let statuses = match statuses {
			Ok(statuses) => statuses,
			Err(error) => {
				snapshot.awaiting_settlement.extend(ids.iter().copied());
				self.inner.db.store_fallback_snapshot(&snapshot).await?;
				return Err(error).context("expired coins await an authoritative server status");
			},
		};
		let decided = snapshot.adopted_payout_vtxos.clone();
		let mut eligible = HashSet::new();

		for id in ids {
			match statuses.get(id) {
				Some(VtxoSpendState::Spendable) if !decided.contains(id) && !signed_parents.contains(id) => {
					snapshot.awaiting_settlement.remove(id);
					eligible.insert(*id);
				},
				// The admission fence says nothing about financial finality.
				// Adopt Spent only after independent chain verification.
				Some(VtxoSpendState::Spent) if decided.contains(id) => {
					snapshot.awaiting_settlement.insert(*id);
				},
				_ => { snapshot.awaiting_settlement.insert(*id); },
			}
		}
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		Ok(eligible)
	}

	pub(crate) async fn filter_expired_refresh_inputs<V: VtxoRef>(
		&self, inputs: Vec<V>,
	) -> anyhow::Result<Vec<V>> {
		let tip = self.inner.chain.tip().await?;
		let pending = self.awaiting_settlement_vtxos().await?;
		let mut expired = HashSet::new();
		for input in &inputs {
			if let Some(stored) = self.inner.db.get_wallet_vtxo(input.vtxo_id()).await? {
				if stored.expiry_height() <= tip || pending.contains(&input.vtxo_id()) { expired.insert(input.vtxo_id()); }
			}
		}
		if expired.is_empty() { return Ok(inputs); }
		if let Err(error) = self.sync_fallback_payouts().await {
			warn!("Could not verify fallback outputs before refresh: {:#}", error);
		}
		let ids = expired.iter().copied().collect::<Vec<_>>();
		let eligible = self.reconcile_expired_inputs(&ids).await?;
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let pending = self.fallback_pending_principal(&snapshot).await?;
		Ok(inputs.into_iter().filter(|input| !pending.contains_key(&input.vtxo_id())
			&& (!expired.contains(&input.vtxo_id()) || eligible.contains(&input.vtxo_id()))).collect())
	}

	pub(crate) async fn reconcile_all_expired_vtxos(&self) -> anyhow::Result<()> {
		let tip = self.inner.chain.tip().await?;
		let pending = self.awaiting_settlement_vtxos().await?;
		let ids = self.inner.db.get_vtxos_by_state(&[VtxoStateKind::Spendable]).await?
			.into_iter().filter(|vtxo| vtxo.expiry_height() <= tip || pending.contains(&vtxo.id())).map(|vtxo| vtxo.id()).collect::<Vec<_>>();
		self.reconcile_expired_inputs(&ids).await?;
		Ok(())
	}
}
