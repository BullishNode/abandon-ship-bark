//! Unilateral exit tracking and progression for individual VTXOs.
//!
//! This module defines types that track the lifecycle of a single [Vtxo] exit, including its current
//! state, onchain transaction IDs, and a history of prior states for auditing and troubleshooting.
//!
//! The primary type is [ExitVtxo], which provides an async [`ExitVtxo::progress`] method to advance
//! the unilateral exit state machine until completion or until the next step actionable step such
//! as requiring more onchain funds or waiting for a confirmation.
//!
//! See [ExitModel] for persisting the state machine in a database.

use bitcoin::{Amount, Txid};
use log::{debug, trace};

use ark::{Vtxo, VtxoId};
use ark::vtxo::{Bare, Full};
use bitcoin_ext::BlockHeight;

use crate::exit::models::{ExitError, ExitState};
use crate::exit::progress::{ExitStateProgress, ProgressContext, ProgressStep};
use crate::exit::transaction_manager::ExitTransactionManager;
use crate::movement::MovementId;
use crate::persist::BarkPersister;
use crate::persist::models::StoredExit;
use crate::{Wallet, WalletVtxo};

/// Tracks the exit lifecycle for a single [Vtxo].
///
/// An `ExitVtxo` maintains:
/// - the underlying [Vtxo] being exited,
/// - the set of related onchain transaction IDs in topographical order,
/// - the current state [ExitState],
/// - and a history of prior states for debugging and auditing.
///
/// Use [ExitVtxo::progress] to drive the state machine forward. The method is idempotent and will
/// only persist when a logical state transition occurs.
#[derive(Debug, Clone)]
pub struct ExitVtxo {
	vtxo_id: VtxoId,
	amount: Amount,
	state: ExitState,
	history: Vec<ExitState>,
	txids: Option<Vec<Txid>>,
	movement_id: Option<MovementId>,
}

impl ExitVtxo {
	/// Create a new instance for the given [VtxoId] with an initial state of [ExitState::Start].
	/// The unilateral exit can't progress until [ExitVtxo::initialize] is called.
	///
	/// # Parameters
	/// - `vtxo`: the [Vtxo] being exited.
	/// - `tip`: current chain tip used to initialize the starting state.
	/// - `movement_id`: the [MovementId] of the pending movement that records this exit.
	pub fn new(vtxo: &Vtxo<Bare>, tip: BlockHeight, movement_id: Option<MovementId>) -> Self {
		Self {
			vtxo_id: vtxo.id(),
			amount: vtxo.amount(),
			state: ExitState::new_start(tip),
			history: vec![],
			txids: None,
			movement_id,
		}
	}

	/// Reconstruct an `ExitVtxo` from its parts. This leaves the instance in an uninitialized
	/// state. Useful when loading a tracked exit from storage.
	///
	/// # Parameters
	/// - `entry`: The persisted data to reconstruct this instance from.
	/// - `vtxo`: The [Vtxo] that this exit is tracking.
	pub fn from_entry(entry: StoredExit, vtxo: &WalletVtxo) -> Self {
		assert_eq!(entry.vtxo_id, vtxo.id());
		ExitVtxo {
			vtxo_id: entry.vtxo_id,
			amount: vtxo.amount(),
			state: entry.state,
			history: entry.history,
			txids: None,
			movement_id: entry.movement_id,
		}
	}

	pub(super) fn adopt_persisted_state(&mut self, entry: StoredExit) {
		assert_eq!(entry.vtxo_id, self.vtxo_id);
		assert_eq!(entry.movement_id, self.movement_id);
		self.state = entry.state;
		self.history = entry.history;
	}

	/// Returns the ID of the tracked [Vtxo].
	pub fn id(&self) -> VtxoId {
		self.vtxo_id
	}

	/// Returns the amount being exited.
	pub fn amount(&self) -> Amount {
		self.amount
	}

	/// Returns the current state of the unilateral exit.
	pub fn state(&self) -> &ExitState {
		&self.state
	}

	/// Returns the history of the exit machine in the order that states were observed.
	pub fn history(&self) -> &Vec<ExitState> {
		&self.history
	}

	/// Returns the set of exit-related transaction IDs, these may not be broadcast yet. If the
	/// instance is not yet initialized, None will be returned.
	pub fn txids(&self) -> Option<&Vec<Txid>> {
		self.txids.as_ref()
	}

	/// Returns the [MovementId] of the pending movement that records this exit, if any.
	/// Older exits created before movement tracking was wired up may not have an associated
	/// movement here; callers should handle that case gracefully.
	pub fn movement_id(&self) -> Option<MovementId> {
		self.movement_id
	}

	/// True if the exit is currently [ExitState::Claimable] and can be claimed/spent.
	pub fn is_claimable(&self) -> bool {
		matches!(self.state, ExitState::Claimable(..))
	}

	/// A replay of a previously completed claim retains its existing debit even
	/// while its missing path is back in Processing's ordinary abortable states.
	pub(crate) fn is_cancelable(&self) -> bool {
		self.state.is_cancelable() && !self.history.iter().any(|state| matches!(state, ExitState::Claimed(_)))
	}

	/// True if [ExitVtxo::initialize] has been called and the exit is ready to progress.
	pub fn is_initialized(&self) -> bool {
		self.txids.is_some()
	}

	/// Prepares an [ExitVtxo] for progression by querying the list of transactions required to
	/// process the unilateral exit and adds them to the exit transaction manager.
	pub async fn initialize(
		&mut self,
		tx_manager: &mut ExitTransactionManager,
		persister: &dyn BarkPersister,
	) -> anyhow::Result<(), ExitError> {
		trace!("Initializing VTXO for exit {}", self.vtxo_id);
		let vtxo = self.get_full_vtxo(persister).await?;
		self.txids = Some(tx_manager.track_vtxo_exits(&vtxo).await?);
		Ok(())
	}

	/// Advances the exit state machine for this [Vtxo].
	///
	/// The method:
	/// - Attempts to transition the unilateral exit state machine.
	/// - Persists only when a logical state change occurs.
	///
	/// Returns:
	/// - `Ok(())` when no more immediate work is required, such as when we're waiting for a
	///   confirmation or when the exit is complete.
	/// - `Err(ExitError)` when an unrecoverable issue occurs, such as requiring more onchain funds
	///   or if an exit transaction fails to broadcast; if the error includes a newer state, it will
	///   be committed before returning.
	///
	pub async fn progress(
		&mut self,
		wallet: &Wallet,
		tx_manager: &mut ExitTransactionManager,
		continue_until_finished: bool,
	) -> anyhow::Result<(), ExitError> {
		if self.txids.is_none() {
			return Err(ExitError::InternalError {
				error: String::from("Unilateral exit not yet initialized"),
			});
		}

		let mut vtxo = self.get_vtxo(&*wallet.inner.db).await?;
		// The caller holds Exit.inner throughout this progression. New complete
		// ordinary facts need that same guard before publication. A resumed
		// intent must never reacquire/broadcast over an already durable fact.
		let snapshot = wallet.inner.db.get_fallback_snapshot().await
			.map_err(|error| ExitError::InternalError { error: error.to_string() })?;
		if !matches!(self.state, ExitState::Claimed(_)) && !super::exit_rebroadcast_allowed(self, wallet).await
			.map_err(|error| ExitError::InvalidWalletState { error: error.to_string() })? {
			return Err(ExitError::InvalidWalletState {
				error: format!("completed exit {} no longer owns this exact resumed intent", self.id()),
			});
		}
		let resumed_claim = matches!(self.state, ExitState::Processing(_))
			&& super::completed_claim_recovery_lineage(&StoredExit::new(self))
			&& vtxo.state == crate::vtxo::VtxoState::Exited;
		let ordinary_spent = snapshot.verified_ordinary_spends.contains(&self.id());
		if ordinary_spent && vtxo.state.kind() != crate::vtxo::VtxoStateKind::Spent {
			return Err(ExitError::InvalidWalletState {
				error: format!("verified ordinary spend of {} awaits exact holder retirement; exit is paused", self.id()),
			});
		}
		if !resumed_claim && !self.state.warrants_exited_vtxo() && vtxo.state.kind() != crate::vtxo::VtxoStateKind::Spent {
			if let Some(movement_id) = self.movement_id {
				let movement = wallet.inner.db.get_movement_by_id(movement_id).await
					.map_err(|error| ExitError::InternalError { error: error.to_string() })?;
				if movement.metadata.get("protective_exit") == Some(&serde_json::Value::Bool(true)) {
					let holder = wallet.protective_exit_holder(&movement, self.id()).await
						.map_err(|error| ExitError::InternalError { error: error.to_string() })?;
					wallet.lock_vtxos([self.id()], Some(holder)).await
						.map_err(|error| ExitError::InternalError { error: error.to_string() })?;
					vtxo = self.get_vtxo(&*wallet.inner.db).await?;
				}
			}
		}
		const MAX_ITERATIONS: usize = 100;
		for _ in 0..MAX_ITERATIONS {
			let mut context = ProgressContext {
				vtxo: &vtxo,
				exit_txids: self.txids.as_ref().unwrap(),
				wallet,
				tx_manager,
			};
			// Attempt to move to the next state, which may or may not generate a new state
			if log::log_enabled!(log::Level::Trace) {
				match wallet.inner.chain.tip().await {
					Ok(h) => trace!("Progressing VTXO {} at height {}", self.id(), h),
					Err(_) => trace!("Progressing VTXO {}", self.id()),
				}
			}
			match self.state.clone().progress(&mut context).await {
				Ok(new_state) => {
					self.update_state_if_newer(new_state, &*wallet.inner.db).await?;
					if !continue_until_finished {
						return Ok(());
					}
					match ProgressStep::from_exit_state(&self.state) {
						ProgressStep::Continue => debug!("VTXO {} can continue", self.id()),
						ProgressStep::Done => return Ok(())
					}
				},
				Err(e) => {
					// We may need to commit a new state before returning an error
					if let Some(new_state) = e.state {
						self.update_state_if_newer(new_state, &*wallet.inner.db).await?;
					}
					return Err(e.error);
				}
			}
		}
		debug_assert!(false, "Exceeded maximum iterations for progressing VTXO {}", self.id());
		Ok(())
	}

	/// Transitions this exit to the terminal-but-resumable [ExitState::Canceled] and persists it.
	///
	/// This only updates the exit's own state and history. The caller
	/// ([crate::exit::Exit::cancel_exit]) is responsible for dropping the exit's transactions from
	/// the transaction manager, finalizing the movement, and removing the exit from active
	/// tracking.
	pub async fn cancel(
		&mut self,
		tip: BlockHeight,
		persister: &dyn BarkPersister,
	) -> anyhow::Result<(), ExitError> {
		if matches!(self.state, ExitState::Canceled(_)) { return Ok(()); }
		if !self.is_cancelable() {
			return Err(ExitError::CannotCancelExit { vtxo: self.id(), state: self.state.kind() });
		}
		self.update_state_if_newer(ExitState::new_canceled(tip), persister).await
	}

	pub async fn get_vtxo(&self, persister: &dyn BarkPersister) -> anyhow::Result<WalletVtxo, ExitError> {
		persister.get_wallet_vtxo(self.vtxo_id).await
			.map_err(|e| ExitError::InvalidWalletState { error: e.to_string() })?
			.ok_or_else(|| ExitError::InternalError {
				error: format!("VTXO for exit couldn't be found: {}", self.vtxo_id)
			})
	}

	/// Hydrate the underlying [Vtxo] including its full unilateral exit
	/// chain. Used by exit-specific code paths that need to construct or
	/// inspect the actual exit transactions.
	pub async fn get_full_vtxo(
		&self,
		persister: &dyn BarkPersister,
	) -> anyhow::Result<Vtxo<Full>, ExitError> {
		persister.get_full_vtxo(self.vtxo_id).await
			.map_err(|e| ExitError::InvalidWalletState { error: e.to_string() })?
			.ok_or_else(|| ExitError::InternalError {
				error: format!("VTXO for exit couldn't be found: {}", self.vtxo_id)
			})
	}

	async fn update_state_if_newer(
		&mut self,
		new: ExitState,
		persister: &dyn BarkPersister,
	) -> anyhow::Result<(), ExitError> {
		// We don't want to push a new history item unless the state has changed logically
		if new != self.state {
			let mut entry = StoredExit::new(self);
			entry.history.push(self.state.clone());
			entry.state = new;
			// Publish memory only after the durable checkpoint. Otherwise a
			// failed write makes exact persisted-intent revalidation reject
			// this engine forever, even though cold reload could still retry.
			persister.store_exit_vtxo_entry(&entry).await
				.map_err(|e| ExitError::DatabaseVtxoStoreFailure { vtxo_id: self.id(), error: e.to_string() })?;
			self.history = entry.history;
			self.state = entry.state;
			Ok(())
		} else {
			Ok(())
		}
	}

}

#[cfg(test)]
mod completed_recovery_tests {
	use super::*;
	use bitcoin::hashes::Hash;

	#[test]
	fn previously_claimed_processing_cannot_cancel_the_existing_principal_debit() {
		let tip = BlockHeight::new(20);
		let mut exit = ExitVtxo {
			vtxo_id: VtxoId::from_slice(&[13; 36]).unwrap(), amount: Amount::from_sat(100_000),
			state: ExitState::new_processing(tip, [Txid::all_zeros()]), history: vec![],
			txids: None, movement_id: Some(MovementId(13)),
		};
		assert!(exit.is_cancelable(), "normal unbroadcast exits retain their abortable window");
		exit.history.push(ExitState::new_awaiting_delta(tip,
			bitcoin_ext::BlockRef { height: BlockHeight::new(10), hash: bitcoin::BlockHash::all_zeros() }, bitcoin_ext::BlockDelta::new(3)));
		assert!(exit.is_cancelable(), "an ordinary uncompleted reorg is not a completed-claim retry");
		exit.history.push(ExitState::new_claimed(tip, Txid::all_zeros(),
			bitcoin_ext::BlockRef { height: BlockHeight::new(10), hash: bitcoin::BlockHash::all_zeros() }));
		assert!(exit.state().is_cancelable(), "Processing by itself does not record the prior commitment");
		assert!(!exit.is_cancelable(), "the exact persisted Claimed lineage prevents manual debt erasure");
	}
}

#[cfg(all(test, feature = "sqlite", not(target_arch = "wasm32")))]
mod checkpoint_failure_tests {
	use super::*;
	use crate::persist::sqlite::SqliteClient;
	use bitcoin::hashes::Hash;

	#[tokio::test]
	async fn failed_recovery_checkpoint_preserves_durable_and_memory_intent_then_retries() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("checkpoint.sqlite");
		let db = SqliteClient::open(&path).unwrap();
		let tip = BlockHeight::new(30);
		let movement = db.create_new_movement(crate::movement::MovementStatus::Pending,
			&crate::movement::MovementSubsystem { name: crate::subsystem::Subsystem::EXIT.as_name().into(), kind: "exit".into() },
			chrono::Local::now(), None).await.unwrap();
		let mut exit = ExitVtxo {
			vtxo_id: VtxoId::from_slice(&[14; 36]).unwrap(), amount: Amount::from_sat(100_000),
			state: ExitState::new_processing(tip, [Txid::all_zeros()]),
			history: vec![ExitState::new_claimed(tip, Txid::all_zeros(),
				bitcoin_ext::BlockRef { height: BlockHeight::new(10), hash: bitcoin::BlockHash::all_zeros() })],
			txids: None, movement_id: Some(movement),
		};
		db.store_exit_vtxo_entry(&StoredExit::new(&exit)).await.unwrap();
		let before = StoredExit::new(&exit);
		let conn = rusqlite::Connection::open(&path).unwrap();
		conn.execute_batch("CREATE TRIGGER reject_exit_checkpoint BEFORE UPDATE ON bark_exit_states BEGIN SELECT RAISE(FAIL, 'injected checkpoint failure'); END;").unwrap();
		let next = ExitState::new_awaiting_delta(tip,
			bitcoin_ext::BlockRef { height: BlockHeight::new(25), hash: bitcoin::BlockHash::all_zeros() }, bitcoin_ext::BlockDelta::new(3));
		assert!(matches!(exit.update_state_if_newer(next.clone(), &db).await, Err(ExitError::DatabaseVtxoStoreFailure { .. })));
		assert_eq!(StoredExit::new(&exit), before);
		assert_eq!(db.get_exit_vtxo_entry(&exit.id()).await.unwrap(), Some(before));
		conn.execute_batch("DROP TRIGGER reject_exit_checkpoint;").unwrap();
		exit.update_state_if_newer(next.clone(), &db).await.unwrap();
		let reopened = db.get_exit_vtxo_entry(&exit.id()).await.unwrap().unwrap();
		assert_eq!(StoredExit::new(&exit), reopened);
		assert_eq!(reopened.state, next);
		assert_eq!(reopened.movement_id, Some(movement));
		assert_eq!(reopened.history.len(), 2, "one committed transition after the same-process retry");
		assert!(!exit.is_cancelable());
	}
}
