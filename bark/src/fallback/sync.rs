//! Reconcile retained source coins against the wallet's own chain backend.

use std::time::Duration;
use crate::Wallet;
use crate::vtxo::VtxoStateKind;

impl Wallet {
	/// The on-chain wallet discovers its registered address normally. This pass
	/// verifies which retained Ark sources the same canonical payment spent.
	pub async fn sync_fallback_payouts(&self) -> anyhow::Result<()> {
		let Some(record) = self.fallback_registration().await? else { return Ok(()); };
		let stored = self.inner.db.get_vtxos_by_state(&[
			VtxoStateKind::Spendable, VtxoStateKind::Locked, VtxoStateKind::Exited, VtxoStateKind::Spent,
		]).await?;
		let snapshot = self.inner.db.get_fallback_snapshot().await?;
		let mut coins = Vec::new();
		for coin in stored {
			if coin.state.kind() == VtxoStateKind::Spent && !snapshot.adopted_payout_vtxos.contains(&coin.id())
				&& !snapshot.quarantined_vtxos.contains(&coin.id()) { continue; }
			coins.push(self.get_full_vtxo(coin.id()).await?);
		}
		let paid = self.confirmed_fallback_vtxos(&coins, &record.spk).await?;
		let tip = self.chain().tip().await?;
		let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
		let mut snapshot = self.inner.db.get_fallback_snapshot().await?;
		let previous = snapshot.adopted_payout_vtxos.clone();
		for coin in &coins {
			if coin.expiry_height() <= tip && !paid.contains(&coin.id())
				&& !snapshot.verified_ordinary_spends.contains(&coin.id()) { snapshot.awaiting_settlement.insert(coin.id()); }
		}
		snapshot.adopted_payout_vtxos = paid;
		let paid = snapshot.adopted_payout_vtxos.clone();
		for id in &paid { snapshot.awaiting_settlement.remove(id); }
		for id in previous.difference(&paid) { snapshot.awaiting_settlement.insert(*id); }
		self.inner.db.store_fallback_snapshot(&snapshot).await?;
		self.apply_fallback_chain_states(&snapshot).await
	}
}
