//! Protect interrupted binding refreshes while preserving input locks.

use std::collections::HashSet;

use anyhow::Context;
use ark::{VtxoId, Vtxo, forfeit};
use ark::vtxo::Full;
use ark::vtxo::policy::clause::{HashDelaySignClause, TapScriptClause};
use ark::tree::signed::{UnlockHash, UnlockPreimage};
use ark::lightning::PaymentHash;
use bitcoin::{OutPoint, SignedAmount};
use bitcoin_ext::TxStatus;

use crate::Wallet;
use crate::movement::{Movement, MovementId, MovementStatus};
use crate::movement::update::MovementUpdate;
use crate::persist::models::{RoundStateId, StoredRoundState};
use crate::vtxo::{VtxoLockHolder, VtxoState, VtxoStateKind};

use super::{RoundFlowState, RoundState, RoundStatus, check_vtxo_fails_hash_lock};

impl StoredRoundState {
	pub(crate) async fn protect_binding_refresh_input(&self, wallet: &Wallet, id: VtxoId) -> anyhow::Result<bool> {
		let state = self.state();
		if state.done || !state.sent_forfeit_sigs || state.new_vtxos.is_empty()
			|| unlocked_replacements(state) || !state.is_self_refresh(wallet).await? { return Ok(false); }
		let RoundFlowState::Finished { funding_tx, .. } = &state.flow else { return Ok(false); };
		let Some(input) = state.participation.inputs.iter().find(|coin| coin.id() == id) else { return Ok(false); };
		// The checkpoint must retain signed inputs and replacements before
		// an exit can broadcast the original input's path.
		for replacement in &state.new_vtxos {
			if replacement.validate(funding_tx).is_err() {
				check_vtxo_fails_hash_lock(funding_tx, replacement)
					.context("binding refresh lost its signed replacement material")?;
			}
		}
		let holder = state.movement_id.context("binding refresh has no native holder")?;
		wallet.exit_mgr().start_protective_round_exit(&input.to_bare(), wallet, self.id().0, holder).await?;
		// Until the replacement is unlocked, the old exit carries the entire
		// principal. A round fee must not debit that same principal again.
		wallet.inner.movements.update_movement(holder, MovementUpdate::new()
			.effective_balance(SignedAmount::ZERO).fee(bitcoin::Amount::ZERO)).await?;
		Ok(true)
	}
}

impl Wallet {
	/// Read without the round lock to preserve the round/exit lock order.
	/// The later checked lock operation still rejects another action's holder.
	pub(crate) async fn protective_exit_holder(&self, movement: &Movement, coin: VtxoId) -> anyhow::Result<VtxoLockHolder> {
		let Some(round) = movement.metadata.get("protective_round_state") else { return Ok(movement.id.into()); };
		let id = u32::try_from(round.as_u64().context("invalid protective round identity")?)?;
		let expected: MovementId = serde_json::from_value(movement.metadata.get("protective_round_movement")
			.context("missing protective round holder")?.clone())?;
		let stored = self.inner.db.get_round_state_by_id(RoundStateId(id)).await?
			.context("protective round checkpoint is unavailable")?;
		let state = stored.state();
		ensure!((!state.done || state.onchain_recovery) && state.sent_forfeit_sigs && !state.new_vtxos.is_empty()
			&& state.movement_id == Some(expected) && (state.participation.inputs.iter().any(|input| input.id() == coin)
				|| (unlocked_replacements(state) && state.new_vtxos.iter().any(|output| output.id() == coin))),
			"protective exit no longer matches its binding round checkpoint");
		Ok(expected.into())
	}
}

/// The signed genesis retains the preimage once replacement outputs are
/// unlocked. Recovery does not need a separate preimage record.
fn unlocked_replacements(state: &RoundState) -> bool {
	let RoundFlowState::Finished { funding_tx, .. } = &state.flow else { return false; };
	!state.new_vtxos.is_empty() && state.new_vtxos.iter().all(|coin| coin.validate(funding_tx).is_ok())
}

impl StoredRoundState {
	/// Recover only interrupted self-refreshes. This wallet must not cancel
	/// or adopt outputs for another recipient.
	pub(crate) async fn recover_binding_refresh(&mut self, wallet: &Wallet) -> anyhow::Result<Option<RoundStatus>> {
		if (self.state().done && !self.state().onchain_recovery) || !self.state().sent_forfeit_sigs || self.state().new_vtxos.is_empty()
			|| !self.state().is_self_refresh(wallet).await? { return Ok(None); }
		let RoundFlowState::Finished { funding_tx, unlock_hash } = &self.state().flow else { return Ok(None); };
		let funding_tx = funding_tx.clone();
		let unlock_hash = *unlock_hash;
		let inputs = self.state().participation.inputs.clone();
		// A user who already reclaimed the original path must not automatically
		// claim the replacement as well if a preimage arrives afterwards.
		for original in &inputs {
			ensure!(!confirmed_user_claim(wallet, original).await?,
				"the original refresh input was already claimed; replacement recovery remains quarantined");
		}
		if !unlocked_replacements(self.state()) {
			let Some(preimage) = binding_preimage(wallet, &inputs, unlock_hash).await? else { return Ok(None); };
			let mut replacements = self.state().new_vtxos.clone();
			for coin in &mut replacements {
				ensure!(coin.provide_unlock_preimage(preimage), "chain preimage does not unlock the stored replacement");
				coin.validate(&funding_tx).context("unlocked replacement is not fully signed")?;
			}
			self.state_mut().new_vtxos = replacements;
			// Persist every completed signature before exposing any replacement
			// to the exit manager. Original inputs and their holder stay intact.
			wallet.inner.db.update_round_state(self).await?;
		}
		let was_recovered = self.state().onchain_recovery;
		self.state_mut().onchain_recovery = true;
		wallet.inner.db.update_round_state(self).await?;
		let replacements = self.state().new_vtxos.clone();
		let movement = self.state().movement_id.context("binding refresh has no native holder")?;
		let holder = VtxoLockHolder::from(movement);
		let mut claimed = HashSet::new();
		for coin in &replacements {
			if confirmed_user_claim(wallet, coin).await? { claimed.insert(coin.id()); }
		}
		if claimed.len() != replacements.len() {
			if self.state().done {
				self.state_mut().done = false;
				wallet.inner.db.update_round_state(self).await?;
			}
			wallet.inner.movements.rearm_chain_recovery(movement).await?;
		}
		for coin in &replacements {
			if let Some(stored) = wallet.inner.db.get_wallet_vtxo(coin.id()).await? {
				if stored.state == VtxoState::Spent && !claimed.contains(&coin.id()) && was_recovered {
					let exit = wallet.inner.db.get_exit_vtxo_entry(&coin.id()).await?
						.context("retired replacement has no retained native exit")?;
					let id = exit.movement_id.context("retired replacement has no exit movement")?;
					let intent = wallet.inner.db.get_movement_by_id(id).await?;
					ensure!(intent.input_vtxos.contains(&coin.id())
						&& intent.metadata.get("protective_round_state") == Some(&serde_json::json!(self.id().0))
						&& intent.metadata.get("protective_round_movement") == Some(&serde_json::json!(movement)),
						"retired replacement belongs to another operation's exit");
					// This recovery retired these outputs under the same round
					// holder. Do not replace another action's live holder.
					wallet.inner.db.update_vtxo_state_checked(coin.id(),
						VtxoState::Locked { holder: Some(holder.clone()) }, &[VtxoStateKind::Spent]).await?;
				} else {
					ensure!(stored.state == (VtxoState::Locked { holder: Some(holder.clone()) })
						|| (stored.state == VtxoState::Spent && claimed.contains(&coin.id())),
						"replacement is already held or consumed by another operation");
				}
			} else {
				wallet.inner.db.store_vtxos(&[(coin, &VtxoState::Locked { holder: Some(holder.clone()) })]).await?;
			}
		}
		wallet.inner.movements.update_movement(movement, self.state().participation.to_movement_update()?).await?;
		for original in &inputs {
			if let Some(exit) = wallet.inner.db.get_exit_vtxo_entry(&original.id()).await? {
				if let Some(id) = exit.movement_id {
					let current = wallet.inner.db.get_movement_by_id(id).await?;
					if current.metadata.get("recovered_through_round") != Some(&serde_json::Value::Bool(true)) {
						wallet.inner.movements.finish_movement_with_update(id, MovementStatus::Canceled,
							MovementUpdate::new().effective_balance(SignedAmount::ZERO)
								.metadata([("recovered_through_round".into(), serde_json::Value::Bool(true))]),
						).await?;
					}
				}
			}
		}
		for coin in &replacements {
			if !claimed.contains(&coin.id()) {
				if wallet.inner.db.get_exit_vtxo_entry(&coin.id()).await?.is_some() {
					wallet.exit_mgr().rearm_binding_claim(coin.id(), wallet, self.id().0, movement).await?;
				} else {
					wallet.exit_mgr().start_protective_round_exit(&coin.to_bare(), wallet, self.id().0, movement).await?;
				}
			}
		}
		if claimed.len() != replacements.len() {
			return Ok(Some(RoundStatus::OnchainRecovery { claim_confirmed: false }));
		}
		if self.state().done { return Ok(Some(RoundStatus::OnchainRecovery { claim_confirmed: true })); }
		// Check the delayed-claim witness before recording payment. A
		// `Claimed` state can also refer to a server forfeit, which does
		// not pay the wallet.
		let all = inputs.iter().chain(replacements.iter()).map(|coin| coin.id()).collect::<Vec<_>>();
		for id in &all {
			let coin = wallet.inner.db.get_wallet_vtxo(*id).await?.context("binding recovery coin disappeared")?;
			ensure!(coin.state == (VtxoState::Locked { holder: Some(holder.clone()) })
				|| coin.state == VtxoState::Spent,
				"binding recovery cannot retire another operation's holder");
		}
		// Do not expose these inputs as Spendable between durable writes.
		for id in &all {
			ensure!(wallet.inner.db.retire_vtxo_lock(*id, Some(&holder)).await?,
				"binding recovery cannot retire another operation's holder");
		}
		for coin in &replacements {
			let exit = wallet.inner.db.get_exit_vtxo_entry(&coin.id()).await?.context("replacement claim has no exit record")?;
			if let Some(id) = exit.movement_id {
				wallet.inner.movements.finish_movement(id, MovementStatus::Successful).await?;
			}
		}
		wallet.inner.movements.finish_movement_with_update(movement, MovementStatus::Successful,
			MovementUpdate::new().produced_vtxos(&replacements)
				.metadata([("chain_recovered".into(), serde_json::Value::Bool(true))]),
		).await?;
		self.state_mut().done = true;
		wallet.inner.db.update_round_state(self).await?;
		Ok(Some(RoundStatus::OnchainRecovery { claim_confirmed: true }))
	}
}

async fn binding_preimage(
	wallet: &Wallet, originals: &[Vtxo<Full>], unlock: UnlockHash,
) -> anyhow::Result<Option<UnlockPreimage>> {
	let tip = wallet.chain().tip_ref_uncached().await?;
	let mut start = tip.height;
	let mut points = Vec::new();
	for coin in originals {
		let forfeit = forfeit::create_hark_forfeit_tx(coin, unlock, None);
		match wallet.chain().tx_status(forfeit.compute_txid()).await? {
			TxStatus::Confirmed(block) => { start = start.min(block.height); },
			TxStatus::Mempool => {},
			TxStatus::NotFound => continue,
		}
		points.push(OutPoint::new(forfeit.compute_txid(), 0));
	}
	if points.is_empty() { return Ok(None); }
	// Start the scan at the earliest confirmed forfeit. The Core scan
	// also checks the mempool for spends of unconfirmed forfeits.
	#[allow(deprecated)]
	let spends = wallet.chain().txs_spending_inputs(points.clone(), start).await?;
	for point in points {
		let Some((txid, _)) = spends.get(&point) else { continue; };
		let Some(tx) = wallet.chain().get_tx(txid).await? else { continue; };
		for input in tx.input.iter().filter(|input| input.previous_output == point) {
			if let Some(preimage) = HashDelaySignClause::extract_preimage_from_witness(
				&input.witness, PaymentHash::from(unlock),
			) {
				// The decoder checks the committed hash. A disclosed preimage
				// remains usable if the transaction that revealed it is reorged.
				return Ok(Some(preimage.into()));
			}
		}
	}
	Ok(None)
}

async fn confirmed_user_claim(wallet: &Wallet, coin: &Vtxo<Full>) -> anyhow::Result<bool> {
	if !matches!(coin.policy(), ark::VtxoPolicy::Pubkey(_)) { return Ok(false); }
	let TxStatus::Confirmed(leaf) = wallet.chain().tx_status(coin.point().txid).await? else { return Ok(false); };
	#[allow(deprecated)]
	let spends = wallet.chain().txs_spending_inputs([coin.point()], leaf.height).await?;
	let Some((txid, TxStatus::Confirmed(block))) = spends.get(&coin.point()) else { return Ok(false); };
	let tip = wallet.chain().tip_ref_uncached().await?;
	if !wallet.has_recovery_confirmations(*block, tip).await? { return Ok(false); }
	let Some(tx) = wallet.chain().get_tx(txid).await? else { return Ok(false); };
	let own = crate::fallback::spends_user_claim(coin, &tx);
	ensure!(wallet.chain().block_ref(tip.height).await? == tip, "chain changed during binding claim verification");
	Ok(own)
}
