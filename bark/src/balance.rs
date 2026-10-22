//! The wallet balance: which sats can be spent right now and which are held
//! by an operation in progress.
//!
//! [Wallet::balance] sums the spendable VTXOs, setting aside the ones that
//! have to be refreshed before they can be sent again, and then asks every
//! operation in progress which VTXOs it holds, so a VTXO is counted once, in
//! the field of the operation that holds it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use anyhow::Context;
use bitcoin::Amount;
use bitcoin_ext::BlockHeight;
use log::{debug, warn};

use ark::{ProtocolEncoding, VtxoId};

use crate::{Wallet, WalletVtxo};
use crate::actions::{WalletAction, WalletActionCheckpoint};
use crate::exit::ExitStateKind;
use crate::fallback::payouts::FallbackSnapshot;
use crate::movement::{Movement, MovementStatus};
use crate::persist::models::StoredExit;
use crate::subsystem::Subsystem;
use crate::vtxo::{VtxoLockHolder, VtxoState, VtxoStateKind};

/// The different balances of a Bark wallet.
///
/// Each field counts different VTXOs, so the fields never overlap. See
/// [Wallet::balance] for how the fields are computed.
///
/// Show [Balance::spendable] as what a user can pay with and [Balance::total]
/// as what a user owns, or use [Balance::summary] for exactly those numbers.
#[derive(Debug, Clone)]
pub struct Balance {
	/// Owned coins awaiting an authoritative settlement result. They cannot
	/// be spent or promised refreshable while that result is unknown.
	pub awaiting_settlement: Amount,
	/// Sats in VTXOs that can be spent right now, in an arkoor or lightning
	/// payment, an offboard or a round.
	pub spendable: Amount,
	/// Sats in VTXOs that can no longer be sent in an arkoor payment because
	/// they have expired or their exit depth has reached the server's limit.
	/// Nothing holds them, and they can still be offboarded, exited or
	/// refreshed, but they are kept out of [Balance::spendable] until
	/// maintenance has refreshed them. See [crate::WalletVtxo::needs_refresh].
	pub needs_refresh: Amount,
	/// Sats locked in an outgoing lightning payment that has not settled yet.
	/// They come back as spendable VTXOs if the payment fails and leave the
	/// wallet once the payment succeeds.
	pub pending_lightning_send: Amount,
	/// Sats in HTLC VTXOs of an incoming lightning payment whose preimage has
	/// been revealed but which have not been swapped for spendable VTXOs yet.
	/// An incoming payment that can still be cancelled is not counted.
	pub claimable_lightning_receive: Amount,
	/// Sats locked in VTXOs used as inputs to a round that has not completed
	/// yet. A participation still awaiting its round holds no lock, so its
	/// inputs stay in [Balance::spendable]: an interactive participation locks
	/// them when a round attempt starts, a delegated one once the server has
	/// issued the round and its funding transaction has been seen.
	pub pending_in_round: Amount,
	/// Sats reserved by a native safety exit, or committed to an on-chain
	/// unilateral exit and not yet claimed. An abortable safety exit owns an
	/// exact Movement holder and still counts here while unavailable for Ark
	/// spending. A manual exit intent that reserves nothing does not: its
	/// Spendable coin remains in the ordinary balance until commitment.
	pub pending_exit: Amount,
	/// Sats in board VTXOs that are waiting for enough on-chain confirmations
	/// to be registered with the Ark server. A board whose funding transaction
	/// has not reached the network is not counted anywhere yet.
	pub pending_board: Amount,
	/// Sats locked in an outgoing arkoor payment that has not completed yet:
	/// the whole input amount, until the send finalizes and the change comes
	/// back as spendable.
	pub pending_arkoor_send: Amount,
	/// Sats locked in an offboard whose transaction has not been broadcast
	/// yet, including any change that comes back. Once the transaction is on
	/// the network the sats belong to the on-chain wallet.
	pub pending_offboard: Amount,
}

impl Balance {
	/// The sum of every field: all sats that belong to the wallet.
	pub fn total(&self) -> Amount {
		self.spendable + self.needs_refresh + self.pending()
	}

	/// The sats awaiting settlement or held by operations: everything except
	/// [Balance::spendable] and [Balance::needs_refresh].
	pub fn pending(&self) -> Amount {
		self.awaiting_settlement
			+ self.pending_in_round
			+ self.pending_board
			+ self.pending_arkoor_send
			+ self.pending_lightning_send
			+ self.claimable_lightning_receive
			+ self.pending_offboard
			+ self.pending_exit
	}

	/// Whether any sats await settlement or an operation in progress.
	pub fn has_pending(&self) -> bool {
		self.pending() > Amount::ZERO
	}

	/// The numbers to show a user, without the breakdown.
	pub fn summary(&self) -> BalanceSummary {
		BalanceSummary {
			awaiting_settlement: self.awaiting_settlement,
			spendable: self.spendable,
			needs_refresh: self.needs_refresh,
			pending: self.pending(),
			total: self.total(),
		}
	}
}

/// The wallet balance without the breakdown of [Balance]: what a user can pay
/// with, what needs a refresh first, what is held by operations in progress
/// and what the user owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceSummary {
	/// The awaiting-settlement subset of pending, not an additional total.
	pub awaiting_settlement: Amount,
	/// Sats that can be spent right now. See [Balance::spendable].
	pub spendable: Amount,
	/// Sats that need a refresh before they can be sent again. See
	/// [Balance::needs_refresh].
	pub needs_refresh: Amount,
	/// Sats held by operations in progress. They either come back as
	/// spendable or leave the wallet, for example to the on-chain wallet.
	pub pending: Amount,
	/// All sats that belong to the wallet: `spendable + needs_refresh + pending`.
	pub total: Amount,
}

impl Wallet {
	/// Only pending positive sources need this exceptional overlap check.
	/// Previously validated retained bodies supply their signed paths;
	/// no server call or chain lookup can block ordinary balance reporting.
	async fn retained_pending_principal(
		&self, owned: &HashMap<VtxoId, WalletVtxo>, pending: &BTreeSet<VtxoId>,
	) -> anyhow::Result<HashMap<VtxoId, Amount>> {
		let result = owned.iter().filter(|(id, _)| pending.contains(*id))
			.map(|(id, coin)| (*id, coin.amount())).collect::<HashMap<_, _>>();
		if result.is_empty() { return Ok(result); }
		if owned.len() <= 1 { return Ok(result); }
		ensure!(owned.len() <= 256, "pending balance overlap exceeds the 256-body verification budget");
		let ids = owned.keys().copied().collect::<Vec<_>>();
		let mut bytes = 0usize;
		let mut steps = 0usize;
		let mut paths = Vec::new();
		for id in ids {
			let body = self.get_full_vtxo(id).await?;
			bytes = bytes.checked_add(body.serialize().len()).context("pending balance path size overflow")?;
			steps = steps.checked_add(usize::from(body.exit_depth())).context("pending balance path work overflow")?;
			ensure!(bytes <= 1_048_576 && steps <= 8_192,
				"pending balance overlap exceeds its signed-path verification budget");
			paths.push((id, body.ancestor_ids()));
		}
		let amounts = owned.iter().map(|(id, coin)| (*id, coin.amount())).collect();
		let cut = owned_pending_cut(&amounts, pending, &paths)?;
		let inferred = cut.keys().filter(|id| !pending.contains(*id)).copied().collect::<Vec<_>>();
		if !inferred.is_empty() {
			// Keep this fence after the original pending ancestor is resolved.
			// Proof derivation above is local and outside the short history lock;
			// never replace a concurrently refreshed account snapshot.
			let _guard = self.lock_fallback_history(Duration::from_secs(30)).await?;
			let mut latest = self.inner.db.get_fallback_snapshot().await?;
			for id in inferred {
				if latest.verified_ordinary_spends.contains(&id) { continue; }
				if self.inner.db.get_wallet_vtxo(id).await?.is_some_and(|coin| coin.state != VtxoState::Spent) {
					latest.awaiting_settlement.insert(id);
				}
			}
			self.inner.db.store_fallback_snapshot(&latest).await?;
		}
		Ok(cut)
	}

	async fn retained_owned_principal(&self, fallback: &FallbackSnapshot) -> anyhow::Result<HashMap<VtxoId, WalletVtxo>> {
		let retained = self.inner.db.get_vtxos_by_state(
			&[VtxoStateKind::Spendable, VtxoStateKind::Locked, VtxoStateKind::Exited],
		).await?.into_iter().map(|v| (v.id(), v)).collect::<HashMap<_, _>>();
		// A signed body retained for historic recovery is not another owned
		// claim. Keep only current positive local ownership and customer
		// receipt principal; an Operator label cannot synthesize either.
		let mut owned = retained.iter().filter(|(id, _)| !fallback.unproven_spent_vtxos.contains(id)
			&& !fallback.verified_ordinary_spends.contains(id)).map(|(id, coin)| (*id, coin.clone()))
			.collect::<HashMap<_, _>>();
		for (id, receipt) in &fallback.entitlements {
			let customer = receipt.active_attempt()?.is_some_and(|attempt| matches!(attempt.disposition,
				crate::FallbackDisposition::CustomerPayout | crate::FallbackDisposition::BelowDust));
			if !customer || fallback.is_discharged(*id) || fallback.verified_ordinary_spends.contains(&receipt.vtxo_id) {
				owned.remove(&receipt.vtxo_id);
				continue;
			}
			if let Some(coin) = retained.get(&receipt.vtxo_id) {
				owned.insert(coin.id(), coin.clone());
			} else if !owned.contains_key(&receipt.vtxo_id) && fallback.adopted_payout_vtxos.contains(&receipt.vtxo_id) {
				if let Some(coin) = self.inner.db.get_wallet_vtxo(receipt.vtxo_id).await? {
					if coin.state == VtxoState::Spent { owned.insert(coin.id(), coin); }
				}
			}
		}
		Ok(owned)
	}

	/// Availability uses the same bounded ancestry cut as the balance. A known
	/// signed descendant also fences a stale intermediate on another device.
	pub(crate) async fn fallback_pending_principal(
		&self, fallback: &FallbackSnapshot,
	) -> anyhow::Result<HashMap<VtxoId, Amount>> {
		if fallback.awaiting_settlement.is_empty() { return Ok(HashMap::new()); }
		let owned = self.retained_owned_principal(fallback).await?;
		self.retained_pending_principal(&owned, &fallback.awaiting_settlement).await
	}

	/// Return the [Balance] of the wallet.
	///
	/// When not running the daemon, make sure you sync before calling this method.
	pub async fn balance(&self) -> anyhow::Result<Balance> {
		let tip = match self.inner.chain.tip().await {
			Ok(tip) => Some(tip),
			Err(e) => {
				debug!("Chain tip unavailable for the balance, using the last one seen: {:#}", e);
				self.inner.chain.last_observed_tip().await
			},
		};
		let max_exit_depth = match self.ark_info().await {
			Ok(info) => info.map(|i| i.max_vtxo_exit_depth),
			Err(e) => {
				// TODO(pc): Cache this value in the wallet to solve the case where the server is
				//   unreachable and the users balance is miscategorized as a result.
				debug!("Server info unavailable for the balance, judging expiry only: {:#}", e);
				None
			},
		};

		let fallback = self.inner.db.get_fallback_snapshot().await?;
		let decided = fallback.entitlements.values().map(|receipt| receipt.vtxo_id).collect::<HashSet<_>>();

		// A permanent decision removes ordinary Ark availability, not the
		// owned claim while its settlement is still unconfirmed.
		let owned = self.retained_owned_principal(&fallback).await?;
		let pending_principal = self.retained_pending_principal(&owned, &fallback.awaiting_settlement).await?;
		let mut awaiting_decided = Amount::ZERO;
		let mut pending_ids = HashSet::new();
		for receipt in fallback.entitlements.values() {
			if !pending_ids.insert(receipt.vtxo_id) { continue; }
			if let Some(coin) = owned.get(&receipt.vtxo_id) {
				awaiting_decided += principal(coin, &pending_principal);
			}
		}
		let vtxos = owned.into_iter().filter(|(id, _)| !decided.contains(id)).collect::<HashMap<_, _>>();

		let mut balance = Balance {
			awaiting_settlement: awaiting_decided,
			spendable: Amount::ZERO,
			needs_refresh: Amount::ZERO,
			pending_in_round: Amount::ZERO,
			pending_lightning_send: Amount::ZERO,
			claimable_lightning_receive: Amount::ZERO,
			pending_exit: Amount::ZERO,
			pending_board: Amount::ZERO,
			pending_arkoor_send: Amount::ZERO,
			pending_offboard: Amount::ZERO,
		};

		for vtxo in vtxos.values().filter(|v| v.state == VtxoState::Spendable) {
			// Without a tip only the depth can be judged; a tip of zero
			// expires nothing.
			if pending_principal.contains_key(&vtxo.id()) {
				balance.awaiting_settlement += principal(vtxo, &pending_principal);
			} else if vtxo.needs_refresh(tip.unwrap_or(BlockHeight::ZERO), max_exit_depth) {
				balance.needs_refresh += vtxo.amount();
			} else {
				balance.spendable += vtxo.amount();
			}
		}

		let mut counted = HashSet::with_capacity(vtxos.len());
		for round in self.pending_round_states().await? {
			let holder = round.state().lock_holder();
			for input in round.state().locked_pending_inputs() {
				let Some(vtxo) = vtxos.get(&input.id()) else { continue };
				let held = matches!(&vtxo.state, VtxoState::Locked { holder: h } if *h == holder);
				if held && counted.insert(vtxo.id()) {
					balance.pending_in_round += principal(vtxo, &pending_principal);
				}
			}
		}

		for action in self.inner.db.get_all_wallet_action_checkpoints().await? {
			match &action {
				WalletActionCheckpoint::LightningSend(a) => {
					balance.pending_lightning_send += held_by(a, &vtxos, &mut counted, &pending_principal);
				},
				WalletActionCheckpoint::LightningReceive(a) => {
					balance.claimable_lightning_receive += held_by(a, &vtxos, &mut counted, &pending_principal);
				},
				WalletActionCheckpoint::ArkoorSend(a) => {
					balance.pending_arkoor_send += held_by(a, &vtxos, &mut counted, &pending_principal);
				},
				WalletActionCheckpoint::Board(a) => {
					balance.pending_board += held_by(a, &vtxos, &mut counted, &pending_principal);
				},
				WalletActionCheckpoint::Offboard(a) => {
					balance.pending_offboard += held_by(a, &vtxos, &mut counted, &pending_principal);
				},
			}
		}

		// Validated recovered bodies without an action holder remain owned,
		// unavailable value. A preparatory offboard's None lock is not ours.
		let claimed = self.operation_input_ids().await?;
		for id in &fallback.quarantined_vtxos {
			let Some(vtxo) = vtxos.get(id) else { continue; };
			if vtxo.state == (VtxoState::Locked { holder: None }) && !claimed.contains(id)
				&& counted.insert(*id) {
				balance.awaiting_settlement += principal(vtxo, &pending_principal);
			}
		}

		// Read the exits from the database rather than the exit manager, whose
		// lock may be held for a while by an exit in progress.
		let exits = self.inner.db
			.get_exit_vtxo_entries_with_states(ExitStateKind::LIVE_STATES).await?;
		for exit in exits {
			if decided.contains(&exit.vtxo_id) { continue; }
			if let Some(coin) = vtxos.get(&exit.vtxo_id)
				&& let VtxoState::Locked { holder: Some(VtxoLockHolder::Movement { id }) } = &coin.state
				&& exit.movement_id == Some(*id) && !counted.contains(&coin.id()) {
				let movement = self.inner.db.get_movement_by_id(*id).await?;
				if owns_protective_exit_reservation(&exit, &movement, coin) && counted.insert(coin.id()) {
					// Start/Processing safety exits already reserve real positive
					// principal, even though their final leaf is not on chain yet.
					balance.pending_exit += principal(coin, &pending_principal);
					continue;
				}
			}
			if !exit.state.warrants_exited_vtxo() {
				continue;
			}
			match vtxos.get(&exit.vtxo_id) {
				Some(v) if v.state == VtxoState::Exited => {
					if counted.insert(v.id()) { balance.pending_exit += principal(v, &pending_principal); }
				},
				Some(v) => warn!("Exit of VTXO {} has committed but the VTXO is {:?}",
					exit.vtxo_id, v.state,
				),
				None => warn!("Exit of VTXO {} has committed but the VTXO is spent or unknown",
					exit.vtxo_id,
				),
			}
		}

		Ok(balance)
	}
}

/// A native intent alone is not a reservation. Require its pending
/// movement and its exact live holder; a binding round or another operation
/// remains accounted for by its own bucket. Call only for already-filtered
/// positive-owned coins, after decided/unproven/discharged sources are removed.
fn owns_protective_exit_reservation(exit: &StoredExit, movement: &Movement, coin: &WalletVtxo) -> bool {
	exit.vtxo_id == coin.id() && exit.movement_id == Some(movement.id)
		&& coin.state == (VtxoState::Locked { holder: Some(movement.id.into()) })
		&& movement.status == MovementStatus::Pending
		&& movement.subsystem.name == Subsystem::EXIT.as_name()
		&& movement.metadata.get("protective_exit") == Some(&serde_json::Value::Bool(true))
		&& !movement.metadata.contains_key("protective_round_state")
		&& !movement.metadata.contains_key("protective_round_movement")
		&& movement.input_vtxos == [coin.id()]
}

/// A transient projection of existing local principal, not a new ledger.
/// One nearest-owned-ancestor assignment per signed path is linear in the
/// bounded number of path entries, including shared/transitive branches.
fn owned_pending_cut(
	owned: &HashMap<VtxoId, Amount>, pending: &BTreeSet<VtxoId>,
	paths: &[(VtxoId, Vec<VtxoId>)],
) -> anyhow::Result<HashMap<VtxoId, Amount>> {
	let mut result = owned.iter().filter(|(id, _)| pending.contains(*id))
		.map(|(id, amount)| (*id, *amount)).collect::<HashMap<_, _>>();
	let mut seen = HashSet::new();
	for (child, path) in paths {
		ensure!(seen.insert(*child), "pending balance path repeats an owned coin");
		let amount = owned.get(child).context("pending balance path is not locally owned")?;
		if let Some(parent) = path.iter().rev().copied().find(|id| *id != *child && owned.contains_key(id)) {
			let value = result.entry(parent).or_insert(owned[&parent]);
			*value = value.checked_sub(*amount)
				.context("signed owned descendants exceed their retained source principal")?;
		}
	}
	Ok(result)
}

fn principal(coin: &WalletVtxo, pending: &HashMap<VtxoId, Amount>) -> Amount {
	pending.get(&coin.id()).copied().unwrap_or_else(|| coin.amount())
}

/// The sats `action` holds: the locked VTXOs among the ones it reports,
/// leaving out any already `counted` by another operation.
fn held_by<A: WalletAction>(
	action: &A,
	vtxos: &HashMap<VtxoId, WalletVtxo>,
	counted: &mut HashSet<VtxoId>,
	pending_principal: &HashMap<VtxoId, Amount>,
) -> Amount {
	let mut amount = Amount::ZERO;
	for id in action.pending_balance_vtxo_ids() {
		if !counted.insert(id) {
			warn!("VTXO {} is held by action {} but was already counted", id, action.id());
			continue;
		}
		// A spent VTXO is not in the map: the action is done with it.
		let Some(vtxo) = vtxos.get(&id) else { continue };
		match vtxo.state {
			VtxoState::Locked { .. } => amount += principal(vtxo, pending_principal),
			// An exit took it from the action; it counts in pending_exit.
			VtxoState::Exited => {},
			// Released by the action, or not locked by it yet.
			VtxoState::Spendable => {},
			VtxoState::Spent => {},
		}
	}
	amount
}

#[cfg(test)]
mod tests {
	use super::*;
	use ark::test_util::VTXO_VECTORS;
	use crate::exit::ExitState;
	use crate::movement::{MovementId, MovementSubsystem};
	use crate::persist::models::wallet_vtxo_from_full;

	fn id(value: u8) -> VtxoId { VtxoId::from_slice(&[value; 36]).unwrap() }

	#[test]
	fn prepared_exit_balance_requires_its_exact_pending_nonbinding_safety_reservation() {
		let movement_id = MovementId::new(7);
		let mut coin = wallet_vtxo_from_full(&VTXO_VECTORS.board_vtxo,
			VtxoState::Locked { holder: Some(movement_id.into()) }, true);
		let exit = StoredExit { vtxo_id: coin.id(), movement_id: Some(movement_id),
			state: ExitState::new_start(BlockHeight::new(1)), history: vec![] };
		let mut movement = Movement::new(movement_id, MovementStatus::Pending,
			&MovementSubsystem { name: Subsystem::EXIT.as_name().into(), kind: "exit".into() }, chrono::Local::now());
		movement.metadata.insert("protective_exit".into(), serde_json::Value::Bool(true));
		movement.input_vtxos.push(coin.id());
		assert!(owns_protective_exit_reservation(&exit, &movement, &coin));
		coin.state = VtxoState::Locked { holder: Some(MovementId::new(8).into()) };
		assert!(!owns_protective_exit_reservation(&exit, &movement, &coin), "a foreign holder is not this exit's principal");
		coin.state = VtxoState::Spendable;
		assert!(!owns_protective_exit_reservation(&exit, &movement, &coin), "an unreserved manual intent does not add another balance");
		coin.state = VtxoState::Locked { holder: Some(movement_id.into()) };
		movement.metadata.insert("protective_round_state".into(), serde_json::json!(1));
		assert!(!owns_protective_exit_reservation(&exit, &movement, &coin), "binding principal remains in the round bucket");
		movement.metadata.remove("protective_round_state");
		movement.metadata.remove("protective_exit");
		assert!(!owns_protective_exit_reservation(&exit, &movement, &coin));
		movement.metadata.insert("protective_exit".into(), serde_json::Value::Bool(true));
		movement.status = MovementStatus::Canceled;
		assert!(!owns_protective_exit_reservation(&exit, &movement, &coin));
		movement.status = MovementStatus::Pending;
		movement.input_vtxos.push(id(8));
		assert!(!owns_protective_exit_reservation(&exit, &movement, &coin), "a mismatched operation cannot synthesize this amount");
	}

	#[test]
	fn pending_signed_cut_does_not_double_subtract_shared_transitive_descendants() {
		let owned = [(id(1), 100), (id(2), 40), (id(3), 30), (id(4), 60)]
			.into_iter().map(|(id, value)| (id, Amount::from_sat(value))).collect();
		let pending = [id(1), id(2)].into_iter().collect();
		let paths = vec![(id(1), vec![]), (id(2), vec![id(1)]),
			(id(3), vec![id(1), id(2)]), (id(4), vec![id(1)])];
		let cut = owned_pending_cut(&owned, &pending, &paths).unwrap();
		assert_eq!(cut[&id(1)], Amount::ZERO);
		assert_eq!(cut[&id(2)], Amount::from_sat(10));
		assert_eq!(cut.values().copied().sum::<Amount>() + owned[&id(3)] + owned[&id(4)], Amount::from_sat(100));
	}

	#[test]
	fn pending_signed_cut_fences_a_stale_middle_parent_without_an_existing_pending_marker() {
		let owned = [(id(1), 100), (id(2), 40), (id(3), 30)]
			.into_iter().map(|(id, value)| (id, Amount::from_sat(value))).collect();
		let pending = [id(1)].into_iter().collect();
		let paths = vec![(id(1), vec![]), (id(2), vec![id(1)]), (id(3), vec![id(1), id(2)])];
		let cut = owned_pending_cut(&owned, &pending, &paths).unwrap();
		assert_eq!(cut[&id(1)], Amount::from_sat(60));
		assert_eq!(cut[&id(2)], Amount::from_sat(10));
		assert!(!cut.contains_key(&id(3)), "only the current leaf remains available");
		assert_eq!(cut.values().copied().sum::<Amount>() + owned[&id(3)], Amount::from_sat(100));
	}

	#[test]
	fn pending_signed_cut_preserves_local_principal_without_adding_its_known_change_twice() {
		let owned = [(id(1), Amount::from_sat(100)), (id(2), Amount::from_sat(40))].into_iter().collect();
		let pending = [id(1)].into_iter().collect();
		let cut = owned_pending_cut(&owned, &pending, &[(id(1), vec![]), (id(2), vec![id(1)])]).unwrap();
		assert_eq!(cut[&id(1)], Amount::from_sat(60));
		assert_eq!(cut[&id(1)] + owned[&id(2)], Amount::from_sat(100));
	}
}
