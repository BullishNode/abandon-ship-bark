//! Unilateral exit management
//!
//! This module coordinates unilateral exits of VTXOs back to on-chain bitcoin without
//! requiring any third-party cooperation. It tracks which VTXOs should be exited, prepares
//! and signs the required transactions, and drives the process forward until the funds are
//! confirmed and claimable.
//!
//! What this module provides
//! - Discovery, tracking, and persistence of the exit state for VTXOs.
//! - Initiation of exits for the entire wallet or a selected set of VTXOs.
//! - Periodic progress of exits (broadcasting, fee-bumping, and state updates).
//! - APIs to inspect the current exit status, history, and related transactions.
//! - Construction and signing of a final claim (drain) transaction once exits become claimable.
//!
//! When to use this module
//! - Whenever VTXOs must be unilaterally moved on-chain, e.g., during counterparty unavailability,
//!   or when the counterparty turns malicious.
//!
//! When not to use this module
//! - If the server is cooperative. You can always offboard or pay onchain in a way that is much
//!   cheaper and faster.
//!
//! Core types
//! - [Exit]: High-level coordinator for the exit workflow. It persists state and advances
//!   unilateral exits until they are claimable.
//! - [ExitVtxo]: A VTXO marked for, and progressing through, unilateral exit. Each instance exposes
//!   its current state and related metadata.
//!
//! Typical lifecycle
//! 1) Choose what to exit
//!    - Mark individual VTXOs for exit with [Exit::start_exit_for_vtxos], or exit everything with
//!      [Exit::start_exit_for_entire_wallet].
//! 2) Drive progress
//!    - Call [Exit::progress_exits] to advance the wallet-agnostic state machine for each exit.
//!    - To create or fee-bump CPFP transactions using an onchain wallet, call
//!      [Exit::exits_needing_cpfp] to get pending requests, provide signed CPFPs via
//!      [Exit::provide_cpfp_tx], then call [Exit::progress_exits] again. Alternatively, use the
//!      [Exit::progress_exits_with_cpfp] if you have an onchain wallet to make CPFP txs directly.
//! 3) Inspect status
//!    - Use [Exit::get_exit_status] for detailed per-VTXO status (optionally including
//!      history and transactions).
//!    - Use [Exit::get_exit_vtxos] or [Exit::list_claimable] to browse tracked exits and locate
//!      those that are fully confirmed onchain.
//! 4) Claim the exited funds (optional)
//!    - Once your transaction is confirmed onchain the funds are fully yours. However, recovery
//!      from seed is not supported. By claiming your VTXO you move them to your onchain wallet.
//!    - Once claimable, construct a PSBT to drain them with [Exit::drain_exits].
//!    - Alternatively, you can use [Exit::sign_exit_claim_inputs] to sign the inputs of a given
//!      PSBT if any are the outputs of a claimable unilateral exit.
//!
//! Fee rates
//! - Suitable fee rates will be calculated based on the current network conditions. To override,
//!   pass your own [FeeRate] to [Exit::progress_exits_with_cpfp] or [Exit::drain_exits].
//!
//! Error handling and persistence
//! - The coordinator surfaces operational errors via [anyhow::Result] and domain-specific errors
//!   via [ExitError] where appropriate. Persistent state is kept via the configured persister and
//!   refreshed against the current chain view provided by the chain source client.
//!
//! Minimal example (high-level):
//! ```no_run
//! # use std::sync::Arc;
//! # use std::str::FromStr;
//! # use std::path::PathBuf;
//! #
//! # use bitcoin::Network;
//! # use tokio::fs;
//! #
//! # use bark::{Config, Wallet, WalletSeed, OpenWalletArgs};
//! # use bark::lock_manager::memory::MemoryLockManager;
//! # use bark::onchain::OnchainWallet;
//! # use bark::persist::sqlite::SqliteClient;
//! #
//! # async fn get_wallets() -> (Wallet, OnchainWallet) {
//! #   let datadir = PathBuf::from("./bark");
//! #   let config = Config::network_default(bitcoin::Network::Bitcoin);
//! #   let db = Arc::new(SqliteClient::open(datadir.join("db.sqlite")).unwrap());
//! #   let mnemonic_str = fs::read_to_string(datadir.join("mnemonic")).await.unwrap();
//! #   let mnemonic = bip39::Mnemonic::from_str(&mnemonic_str).unwrap();
//! #   let seed = WalletSeed::new_from_mnemonic(Network::Signet, &mnemonic);
//! #   let bark_wallet = Wallet::open(Network::Signet, seed, config, OpenWalletArgs {
//! #   	persister: Some(db.clone()),
//! #   	..Default::default()
//! #   }).await.unwrap();
//! #   let seed = mnemonic.to_seed("");
//! #   let onchain_wallet = OnchainWallet::load_or_create(Network::Regtest, seed, db).await.unwrap();
//! #   (bark_wallet, onchain_wallet)
//! # }
//! #
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! let (mut bark_wallet, mut onchain_wallet) = get_wallets().await;
//!
//! // Mark all VTXOs for exit.
//! bark_wallet.exit_mgr().start_exit_for_entire_wallet().await?;
//!
//! // Transactions will be broadcast and require confirmations so keep periodically calling this.
//! bark_wallet.exit_mgr().progress_exits_with_cpfp(&bark_wallet, None).await?;
//!
//! // Once all VTXOs are claimable, construct a PSBT to drain them.
//! let drain_to = bitcoin::Address::from_str("bc1p...")?.assume_checked();
//! let claimable_outputs = bark_wallet.exit_mgr().list_claimable().await;
//! let drain_psbt = bark_wallet.exit_mgr().drain_exits(
//!   &claimable_outputs,
//!   &bark_wallet,
//!   drain_to,
//!   None,
//! ).await?;
//!
//! // Next you should broadcast the PSBT, once it's confirmed the unilateral exit is complete.
//! // broadcast_psbt(drain_psbt).await?;
//! #   Ok(())
//! # }
//! ```

mod models;
mod vtxo;
mod estimate;
pub mod bdk;
pub(crate) mod progress;
pub(crate) mod transaction_manager;

pub use self::models::{
	ExitCpfpRequest, ExitTransactionPackage, FeeInfo, RbfRequirement, TransactionInfo,
	ChildTransactionInfo, ExitError, ExitState, ExitTx, ExitTxStatus, ExitTxOrigin, ExitStartState,
	ExitProcessingState, ExitAwaitingDeltaState, ExitClaimableState, ExitClaimInProgressState,
	ExitClaimedState, ExitVtxoAlreadySpentState, ExitCanceledState, ExitStateKind,
	ExitProgressStatus, ExitTransactionStatus,
};
pub use self::vtxo::ExitVtxo;
pub use self::estimate::{DEFAULT_BROADCAST_FEE_MARGIN, ExitFeeEstimate};

use std::borrow::Borrow;
use std::cmp;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{
	Address, Amount, FeeRate, OutPoint, Psbt, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, sighash
};
use bitcoin::consensus::Params;
use log::{error, info, trace, warn};

use ark::{Vtxo, VtxoId, VtxoPolicy};
use ark::vtxo::{Bare, Full, VtxoTxIterItem};
use ark::vtxo::policy::signing::VtxoSigner;
use bitcoin_ext::{BlockDelta, BlockHeight, P2TR_DUST, TxStatus};

use crate::Wallet;
use crate::chain::ChainSource;
use crate::exit::bdk::should_rbf;
use crate::exit::transaction_manager::ExitTransactionManager;
use crate::onchain::MakeCpfpFees;
use crate::movement::{MovementDestination, MovementId, MovementStatus, PaymentMethod};
use crate::movement::manager::MovementManager;
use crate::movement::update::MovementUpdate;

use crate::persist::BarkPersister;
use crate::persist::models::StoredExit;
use crate::psbtext::PsbtInputExt;
use crate::subsystem::{ExitMovement, Subsystem};
use crate::vtxo::{VtxoState, VtxoStateKind};

/// What is known about the CPFP child of a pending exit transaction.
pub(crate) enum ChildState {
	/// No package for the transaction is in the mempool.
	Missing,
	/// A package is in the mempool; `None` when its committed fee couldn't be determined.
	InMempool(Option<FeeInfo>),
}

impl ChildState {
	/// The child that would make the package commit `fee_rate`: a fresh one, a replacement of a
	/// package paying below the rate, or none when the package already pays enough or can't be
	/// safely replaced because its committed fee is unknown.
	pub(crate) fn required_cpfp_fees(&self, fee_rate: FeeRate) -> Option<MakeCpfpFees> {
		match self {
			ChildState::Missing => Some(MakeCpfpFees::Effective(fee_rate)),
			ChildState::InMempool(Some(fi)) if should_rbf(fee_rate, fi.fee_rate) => {
				Some(MakeCpfpFees::Rbf {
					min_effective_fee_rate: fee_rate,
					current_package_fee: fi.total_fee,
				})
			},
			ChildState::InMempool(_) => None,
		}
	}
}

/// The exit transactions of one VTXO not yet confirmed onchain, in broadcast order, each paired
/// with what is known about its CPFP child.
struct UnconfirmedExitParents {
	vtxo: Vtxo<Full>,
	parents: Vec<(Transaction, ChildState)>,
}

/// Handles the process of ongoing VTXO exits.
pub(crate) struct ExitInner {
	tx_manager: ExitTransactionManager,
	persister: Arc<dyn BarkPersister>,
	chain_source: Arc<ChainSource>,
	movement_manager: Arc<MovementManager>,

	exit_vtxos: Vec<ExitVtxo>,
	/// Nonfinancial fair retry position; signed bodies/checkpoints stay durable.
	claimed_reorg_cursor: Option<VtxoId>,
}

// Signed single-parent exit paths have a confirmed prefix. Probe it in
// logarithmic network work; errors remain errors, never absence proof.
async fn confirmed_path_prefix_len<F, Fut>(len: usize, mut confirmed: F) -> anyhow::Result<usize>
where
	F: FnMut(usize) -> Fut,
	Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
	let (mut first, mut end) = (0, len);
	while first < end {
		let middle = first + (end - first) / 2;
		if confirmed(middle).await? { first = middle + 1; } else { end = middle; }
	}
	Ok(first)
}

fn signed_frontier_output(
	step: &VtxoTxIterItem, next_input: OutPoint, actual: &Transaction,
) -> anyhow::Result<TxOut> {
	let point = OutPoint::new(step.tx.compute_txid(), u32::try_from(step.output_idx)?);
	ensure!(point == next_input, "recovery frontier is not the selected signed path output");
	ensure!(actual == &step.tx, "current recovery frontier differs from its retained signed transaction");
	actual.output.get(step.output_idx).cloned().context("signed recovery frontier output is missing")
}

impl ExitInner {
	/// Starts exits for the given vtxos.
	/// Used by both [Exit::start_exit_for_vtxos] and [Exit::start_exit_for_entire_wallet].
	async fn start_exit_for_vtxos(
		&mut self,
		vtxos: &[impl Borrow<Vtxo<Bare>>],
		skip_standardness_checks: bool,
		protective: bool,
		protective_round: Option<(u32, MovementId)>,
	) -> anyhow::Result<()> {
		if vtxos.is_empty() {
			return Ok(());
		}
		let tip = self.chain_source.tip().await?;
		let params = Params::new(self.chain_source.network());
		for vtxo in vtxos {
			let vtxo = vtxo.borrow();
			let vtxo_id = vtxo.id();
			if self.exit_vtxos.iter().any(|ev| ev.id() == vtxo_id) {
				warn!("VTXO {} is already in the exit process", vtxo_id);
				continue;
			}

			if !skip_standardness_checks {
				// Pre-flight check: Prevent exiting dust, which causes "zombie" states
				if vtxo.amount() < P2TR_DUST {
					return Err(ExitError::DustLimit {
						vtxo: vtxo_id,
						amount: vtxo.amount(),
						dust: P2TR_DUST,
					}.into());
				}

				// Pre-flight check: refuse to start an exit whose chain is not
				// standardness-compliant. The exit chain is what we'd broadcast to
				// claim the funds; if any tx in it carries a sub-dust or
				// unrecognised-script output the broadcast will be rejected by
				// public-network relay, so committing CPFP budget to it would just
				// burn fees. Fetch the genesis via the persister since the
				// Vtxo<Bare> we get here only carries the leaf info.
				let full_vtxo = self.persister.get_full_vtxo(vtxo_id).await?
					.ok_or_else(|| ExitError::InvalidWalletState {
						error: format!("missing genesis for VTXO {vtxo_id}"),
					})?;
				if let Err(error) = full_vtxo.check_standard() {
					return Err(ExitError::NonStandardVtxo { vtxo: vtxo_id, error }.into());
				}
			}

			// Create the movement in a Pending state. It transitions to Successful once the
			// exit completes (Claimed), or Canceled if we discover the VTXO was already
			// consumed by something else. We don't touch the VTXO's own state here — that
			// happens in `progress_exits` once we've actually broadcast the exit chain.
			let balance = -vtxo.amount().to_signed()?;
			let script_pubkey = vtxo.output_script_pubkey();
			let payment_method = match Address::from_script(&script_pubkey, &params) {
				Ok(addr) => PaymentMethod::Bitcoin(addr.into_unchecked()),
				Err(e) => {
					warn!("Unable to convert script pubkey to address: {:#}", e);
					PaymentMethod::OutputScript(script_pubkey)
				}
			};

			let movement_id = self.movement_manager.new_movement_with_update(
				Subsystem::EXIT,
				ExitMovement::Exit.to_string(),
				MovementUpdate::new()
					.intended_and_effective_balance(balance)
					.consumed_vtxo(vtxo_id)
					.sent_to([MovementDestination::new(payment_method, vtxo.amount())])
					.metadata(protective.then(|| ("protective_exit".into(), serde_json::Value::Bool(true))))
					.metadata(protective_round.into_iter().flat_map(|(round, movement)| [
						("protective_round_state".into(), serde_json::json!(round)),
						("protective_round_movement".into(), serde_json::json!(movement)),
					])),
			).await.context("Failed to register exit movement")?;

			// We avoid composing the TXID vector since that requires access to the onchain wallet,
			// as such the ExitVtxo will be considered uninitialized.
			trace!("Starting exit for VTXO: {}", vtxo_id);
			let exit = ExitVtxo::new(vtxo, tip, Some(movement_id));
			self.persister.store_exit_vtxo_entry(&StoredExit::new(&exit)).await?;
			self.exit_vtxos.push(exit);
			trace!("Exit for VTXO started successfully: {}", vtxo_id);
		}
		Ok(())
	}

	/// Finished rows are not in the live set after a cold open. Revisit at most
	/// sixteen per pass, including failed partial writes, without starving live exits.
	async fn rearm_orphaned_claims(&mut self, wallet: &Wallet) -> anyhow::Result<Vec<ExitProgressStatus>> {
		let mut entries = self.persister.get_exit_vtxo_entries_with_states(&[ExitStateKind::Claimed]).await?;
		entries.sort_by_key(|entry| entry.vtxo_id);
		let start = entries.partition_point(|entry| self.claimed_reorg_cursor.is_some_and(|id| entry.vtxo_id <= id));
		if start < entries.len() { entries.rotate_left(start); }
		let mut errors = Vec::new();
		for entry in entries.into_iter().take(16) {
			self.claimed_reorg_cursor = Some(entry.vtxo_id);
			match self.reconcile_reconfirmed_claim(wallet, entry.vtxo_id).await {
				Ok(true) => continue,
				Ok(false) => {},
				Err(error) => {
					warn!("Completed exit {} could not finish its confirmed claim: {:#}", entry.vtxo_id, error);
					errors.push(ExitProgressStatus { vtxo_id: entry.vtxo_id, state: entry.state,
						error: Some(ExitError::InvalidWalletState { error: format!("claim completion reconciliation: {:#}", error) }) });
					continue;
				},
			}
			if let Err(error) = self.rearm_orphaned_claim(wallet, entry.vtxo_id).await {
				warn!("Completed exit {} could not reconcile its claim: {:#}", entry.vtxo_id, error);
				errors.push(ExitProgressStatus { vtxo_id: entry.vtxo_id, state: entry.state,
					error: Some(ExitError::InvalidWalletState { error: format!("claim reorg reconciliation: {:#}", error) }) });
			}
		}
		Ok(errors)
	}

	async fn rearm_orphaned_claim(&mut self, wallet: &Wallet, id: VtxoId) -> anyhow::Result<()> {
		let Some(entry) = self.persister.get_exit_vtxo_entry(&id).await? else { return Ok(()); };
		let Some(coin) = self.persister.get_wallet_vtxo(id).await? else { return Ok(()); };
		let Some(movement_id) = entry.movement_id else { return Ok(()); };
		let movement = self.persister.get_movement_by_id(movement_id).await?;
		let snapshot = self.persister.get_fallback_snapshot().await?;
		if !orphaned_claim_owns_principal(&entry, &movement, &coin.state, coin.amount(), &snapshot) { return Ok(()); }
		// Match ordinary-proof publication/reservation: Exit.inner -> history.
		let _history = wallet.lock_automatic_exit_ownership(id).await?;
		let mut fresh = self.persister.get_exit_vtxo_entry(&id).await?.context("completed exit disappeared")?;
		ensure!(fresh == entry, "completed exit changed before its reorg lock");
		let coin = self.persister.get_wallet_vtxo(id).await?.context("completed exit source disappeared")?;
		let movement = self.persister.get_movement_by_id(movement_id).await?;
		let snapshot = self.persister.get_fallback_snapshot().await?;
		if !orphaned_claim_owns_principal(&fresh, &movement, &coin.state, coin.amount(), &snapshot) { return Ok(()); }
		let ExitState::Claimed(claim) = fresh.state else { return Ok(()); };
		let tip = self.chain_source.tip_ref_uncached().await?;
		if tip.height >= claim.block.height && self.chain_source.block_ref(claim.block.height).await? == claim.block {
			return Ok(());
		}
		// Reconfirmation of the same transaction is not an unspent entitlement.
		if let TxStatus::Confirmed(block) = self.chain_source.tx_status(claim.txid).await? {
			if tip.height >= block.height && self.chain_source.block_ref(block.height).await? == block { return Ok(()); }
		}
		let full = wallet.get_full_vtxo(id).await?;
		ensure!(matches!(full.policy(), VtxoPolicy::Pubkey(_)), "completed exit is not an ordinary own-key leaf");
		wallet.validate_vtxo(&full).await.context("completed exit lost its valid signed source body")?;
		let clause = wallet.find_signable_clause(&full).await.context("completed exit has no own signable clause")?;
		let retained = snapshot.terminal_observations.values().flatten()
			.find(|backup| backup.transaction.compute_txid() == claim.txid).map(|claim| claim.transaction.clone());
		let actual_claim = match retained {
			Some(transaction) => transaction,
			None => self.chain_source.get_tx(&claim.txid).await?.context("orphaned claim raw is unavailable")?,
		};
		let expected = clause.tapscript();
		ensure!(actual_claim.compute_txid() == claim.txid && actual_claim.input.iter().any(|input|
			input.previous_output == full.point() && input.witness.taproot_leaf_script().is_some_and(|leaf|
				leaf.version == bitcoin::taproot::LeafVersion::TapScript && leaf.script == expected.as_script())),
			"orphaned claimed transaction does not spend this exact own user-clause leaf");
		let sequence = clause.sequence().context("completed exit has no claim delay")?;
		ensure!(sequence.is_relative_lock_time() && sequence.0 & (1 << 22) == 0,
			"completed exit needs a block-based claim delay");
		let delay = BlockDelta::new((sequence.0 & 0xffff) as u16);
		ensure!(delay != BlockDelta::ZERO, "zero-delay completed exit needs separate reconciliation");
		let next = if let TxStatus::Confirmed(leaf_block) = self.chain_source.tx_status(full.point().txid).await? {
			if tip.height < leaf_block.height || self.chain_source.block_ref(leaf_block.height).await? != leaf_block { return Ok(()); }
			let leaf = self.chain_source.get_tx(&full.point().txid).await?.context("confirmed exit leaf is unavailable")?;
			ensure!(leaf.output.get(full.point().vout as usize) == Some(&full.txout()),
				"current confirmed leaf differs from the retained signed exit");
			if !self.chain_source.confirmed_output_is_unspent(full.point(), &full.txout()).await? { return Ok(()); }
			ExitState::new_awaiting_delta(tip.height, leaf_block, delay)
		} else {
			// No leaf means no immediate claim right. Resume the SAME signed native
			// path only from an actually canonical, unspent output on that path.
			let anchor = full.chain_anchor();
			let TxStatus::Confirmed(block) = self.chain_source.tx_status(anchor.txid).await? else { return Ok(()); };
			if tip.height < block.height || self.chain_source.block_ref(block.height).await? != block { return Ok(()); }
			let funding = self.chain_source.get_tx(&anchor.txid).await?.context("canonical exit funding body is unavailable")?;
			ensure!(funding.compute_txid() == anchor.txid, "exit funding transaction identity changed");
			full.validate(&funding).context("signed exit does not match its actual canonical funding output")?;
			let steps = full.transactions().collect::<Vec<_>>();
			ensure!(!steps.is_empty(), "missing exit path cannot be reconstructed from its retained body");
			let root_output = funding.output.get(anchor.vout as usize).context("exit funding output is missing")?;
			if !self.chain_source.confirmed_output_is_unspent(anchor, root_output).await? {
				// Each signed step spends the preceding selected output, so current
				// confirmations form a prefix. Only its exact, positively unspent
				// final output can authorize recovery; unrelated root spends cannot.
				let prefix = confirmed_path_prefix_len(steps.len(), |index| {
					let txid = steps[index].tx.compute_txid();
					let chain = &self.chain_source;
					async move {
						let TxStatus::Confirmed(block) = chain.tx_status(txid).await? else { return Ok(false); };
						Ok(tip.height >= block.height && chain.block_ref(block.height).await? == block)
					}
				}).await?;
				let Some(index) = prefix.checked_sub(1) else { return Ok(()); };
				let step = &steps[index];
				let point = OutPoint::new(step.tx.compute_txid(), u32::try_from(step.output_idx)?);
				let next = match steps.get(index + 1) {
					Some(next) => {
						ensure!(next.tx.input.len() == 1, "recovery path is not a single-parent signed transition");
						next.tx.input[0].previous_output
					},
					None => full.point(),
				};
				let actual = self.chain_source.get_tx(&point.txid).await?.context("canonical recovery frontier body is unavailable")?;
				let output = signed_frontier_output(step, next, &actual)?;
				if !self.chain_source.confirmed_output_is_unspent(point, &output).await? { return Ok(()); }
			}
			ExitState::new_processing(tip.height, steps.iter().map(|step| step.tx.compute_txid()))
		};
		ensure!(self.chain_source.tip_ref_uncached().await? == tip, "chain changed during completed-claim reconciliation");
		// Pending commits first. A crash or failed exit write leaves Claimed+Pending
		// eligible for this same scan; it never creates a second principal debit.
		self.movement_manager.rearm_chain_recovery(movement_id).await?;
		fresh.history.push(fresh.state.clone());
		// Preserve Exited and the old Claimed history. A frontier-backed Processing
		// retry is authenticated again before any package replay or holder check.
		fresh.state = next;
		self.persister.store_exit_vtxo_entry(&fresh).await?;
		let old_txids = self.exit_vtxos.iter().position(|exit| exit.id() == id)
			.and_then(|index| self.exit_vtxos.remove(index).txids().cloned());
		// Install synchronously after persistence, before any tracking await.
		// refresh_tx_state initializes/retries this LIVE entry before sync and
		// progress; cold reload uses the existing LIVE_STATES path.
		self.exit_vtxos.push(ExitVtxo::from_entry(fresh, &coin));
		if let Some(txids) = old_txids { self.tx_manager.untrack_vtxo_exits(&txids).await; }
		Ok(())
	}

	/// Reconcile an independently paid own claim without authorizing any replay.
	/// The current proof map is only a lookup hint; canonical/raw/user-clause and
	/// stable-tip checks are repeated while holding Exit.inner -> history.
	async fn reconcile_reconfirmed_claim(&mut self, wallet: &Wallet, id: VtxoId) -> anyhow::Result<bool> {
		let _history = wallet.inner.lock_manager.lock(
			&format!("{}.fallback-history", wallet.fingerprint()), std::time::Duration::from_secs(30),
		).await?;
		let Some(mut entry) = self.persister.get_exit_vtxo_entry(&id).await? else { return Ok(false); };
		let Some(coin) = self.persister.get_wallet_vtxo(id).await? else { return Ok(false); };
		let Some(movement_id) = entry.movement_id else { return Ok(false); };
		let movement = self.persister.get_movement_by_id(movement_id).await?;
		let snapshot = self.persister.get_fallback_snapshot().await?;
		if !reconfirmed_claim_candidate(&entry, &movement, &coin.state, coin.amount(), &snapshot) { return Ok(false); }
		let full = wallet.get_full_vtxo(id).await?;
		let tip = self.chain_source.tip_ref_uncached().await?;
		let TxStatus::Confirmed(source_block) = self.chain_source.tx_status(full.point().txid).await? else { return Ok(false); };
		#[allow(deprecated)]
		let spends = self.chain_source.txs_spending_inputs([full.point()], source_block.height).await?;
		let Some((txid, TxStatus::Confirmed(block))) = spends.get(&full.point()) else { return Ok(false); };
		let (txid, block) = (*txid, *block);
		if tip.height.checked_blocks_since(block.height).is_none_or(|depth| depth.saturating_add(1) < 6)
			|| self.chain_source.block_ref(block.height).await? != block { return Ok(false); }
		let actual = self.chain_source.get_tx(&txid).await?.context("confirmed own claim body is unavailable")?;
		ensure!(matches!(full.policy(), VtxoPolicy::Pubkey(_)), "confirmed recovery is not an own-key leaf");
		wallet.validate_vtxo(&full).await.context("confirmed claim lost its valid signed source")?;
		let clause = wallet.find_signable_clause(&full).await.context("confirmed claim has no own signable clause")?;
		let expected = clause.tapscript();
		ensure!(actual.input.iter().any(|input| input.previous_output == full.point()
			&& input.witness.taproot_leaf_script().is_some_and(|leaf|
				leaf.version == bitcoin::taproot::LeafVersion::TapScript && leaf.script == expected.as_script())),
			"confirmed receipt does not spend this exact own user-clause leaf");
		ensure!(self.chain_source.tip_ref_uncached().await? == tip, "chain changed during own claim completion");
		let mut retained = self.persister.get_fallback_snapshot().await?;
		if !retained.terminal_observations.values().flatten().any(|claim| claim.transaction == actual) {
			retained.terminal_observations.entry(0).or_default().push(crate::fallback::payouts::OwnClaim { transaction: actual.clone() });
			self.persister.store_fallback_snapshot(&retained).await?;
		}

		let index = self.exit_vtxos.iter().position(|exit| exit.id() == id);
		if let Some(index) = index {
			ensure!(StoredExit::new(&self.exit_vtxos[index]) == entry, "live exit differs from its durable completion intent");
		}
		if !matches!(entry.state, ExitState::Claimed(claim) if claim.txid == txid && claim.block == block) {
			entry.history.push(entry.state.clone());
			entry.state = ExitState::new_claimed(tip.height, txid, block);
			self.persister.store_exit_vtxo_entry(&entry).await?;
			// Install immediately after commit, before the movement write awaits.
			// Claimed+Pending is retried by the bounded finished-row scan above.
			if let Some(index) = index { self.exit_vtxos[index].adopt_persisted_state(entry); }
		}
		self.movement_manager.finish_movement(movement_id, MovementStatus::Successful).await?;
		Ok(true)
	}

	/// Initializes pending exits and refreshes the chain view of their transaction packages.
	async fn refresh_tx_state(&mut self, wallet: &Wallet) -> anyhow::Result<()> {
		let recovering = self.exit_vtxos.iter().filter(|exit|
			completed_claim_recovery_lineage(&StoredExit::new(exit))).map(ExitVtxo::id).collect::<Vec<_>>();
		for id in recovering {
			if let Err(error) = self.reconcile_reconfirmed_claim(wallet, id).await {
				warn!("Exit {} could not reconcile its actual completed claim: {:#}", id, error);
			}
		}
		let snapshot = self.persister.get_fallback_snapshot().await?;
		let mut blocked = HashSet::new();
		let mut exit_vtxos = std::mem::take(&mut self.exit_vtxos);
		for exit in &mut exit_vtxos {
			match exit_rebroadcast_allowed(exit, wallet).await {
				Ok(true) => {},
				result => {
					warn!("Exit {} is not authorized to resume its completed intent: {:?}", exit.id(), result);
					blocked.insert(exit.id());
					continue;
				},
			}
			if !exit.is_initialized() {
				match exit.initialize(&mut self.tx_manager, &*self.persister).await {
					Ok(()) => continue,
					Err(e) => {
						error!("Error initializing exit for VTXO {}: {:#}", exit.id(), e);
					}
				}
			}
		}
		self.exit_vtxos = exit_vtxos;
		let mut candidates = HashSet::<Txid>::new();
		let mut live = HashSet::<Txid>::new();
		for exit in &self.exit_vtxos {
			if let Some(txids) = exit.txids() {
				if snapshot.verified_ordinary_spends.contains(&exit.id()) || blocked.contains(&exit.id()) { candidates.extend(txids.iter().copied()); }
				else { live.extend(txids.iter().copied()); }
			}
		}
		// Status synchronization itself can rebroadcast a saved fee child.
		// Suppress spent-only packages before that pass; a shared ancestor
		// needed by another exit remains eligible.
		let no_rebroadcast = candidates.difference(&live).copied().collect();
		self.tx_manager.sync(&no_rebroadcast).await?;
		Ok(())
	}

	/// Signs exit claim inputs on a PSBT.
	/// Used by both [Exit::sign_exit_claim_inputs] and [Exit::drain_exits].
	async fn sign_exit_claim_inputs(
		&self,
		psbt: &mut Psbt,
		wallet: &Wallet,
	) -> anyhow::Result<()> {
		let prevouts = psbt.inputs.iter()
			.map(|i| i.witness_utxo.clone().unwrap())
			.collect::<Vec<_>>();

		let prevouts = sighash::Prevouts::All(&prevouts);
		let mut shc = sighash::SighashCache::new(&psbt.unsigned_tx);

		let claimable = self.exit_vtxos.iter()
			.filter(|ev| ev.is_claimable())
			.map(|e| (e.id(), e))
			.collect::<HashMap<_, _>>();

		for (i, input) in psbt.inputs.iter_mut().enumerate() {
			let vtxo = input.get_exit_claim_input();

			if let Some(vtxo) = vtxo {
				let exit_vtxo = claimable.get(&vtxo.id()).context("vtxo is not claimable yet")?;

				if let Some(id) = exit_vtxo.movement_id() {
					let movement = wallet.inner.db.get_movement_by_id(id).await?;
					ensure!(movement.metadata.get("recovered_through_round") != Some(&serde_json::Value::Bool(true)),
						"this refresh is recovering through its replacement coin; do not claim the original as well");
				}
				let witness = wallet.sign_input(&vtxo, i, &mut shc, &prevouts).await
					.map_err(|e| ExitError::ClaimSigningError { error: e.to_string() })?;

				input.final_script_witness = Some(witness);
				let _ = exit_vtxo;
			}
		}

		Ok(())
	}

	/// Builds the status for a stored exit. Transactions are taken from the transaction manager
	/// when tracked (which includes fee data), falling back to the stored child transactions.
	async fn exit_status(
		&self,
		entry: StoredExit,
		include_history: bool,
		include_transactions: bool,
	) -> anyhow::Result<ExitTransactionStatus> {
		let mut transactions = Vec::new();
		if include_transactions {
			let vtxo = self.persister.get_full_vtxo(entry.vtxo_id).await
				.context("failed to retrieve VTXO for exit")?
				.with_context(|| format!("failed to retrieve VTXO for exit {}", entry.vtxo_id))?;
			for tx in vtxo.transactions() {
				let txid = tx.tx.compute_txid();
				if let Some(package) = self.tx_manager.try_get_package(txid) {
					transactions.push(package.read().await.clone());
					continue;
				}
				let child = self.persister.get_exit_child_tx(txid).await
					.context("failed to retrieve child tx for exit")?;
				transactions.push(ExitTransactionPackage {
					exit: TransactionInfo {
						txid,
						tx: tx.tx,
					},
					child: child.map(|(tx, origin)| ChildTransactionInfo {
						origin,
						info: TransactionInfo {
							txid: tx.compute_txid(),
							tx,
						},
						fee_info: None,
					}),
				});
			}
		}
		Ok(ExitTransactionStatus {
			vtxo_id: entry.vtxo_id,
			state: entry.state,
			history: if include_history { Some(entry.history) } else { None },
			transactions,
		})
	}

	/// Builds statuses for the given stored exits.
	async fn exit_statuses(
		&self,
		entries: Vec<StoredExit>,
		include_history: bool,
		include_transactions: bool,
	) -> anyhow::Result<Vec<ExitTransactionStatus>> {
		let mut statuses = Vec::with_capacity(entries.len());
		for entry in entries {
			statuses.push(self.exit_status(entry, include_history, include_transactions).await?);
		}
		Ok(statuses)
	}
}

/// Public handle to the exit subsystem. Wraps `ExitInner` in an `Arc<RwLock>` so all
/// locking is internal — callers never need to acquire the lock directly.
pub struct Exit {
	inner: Arc<tokio::sync::RwLock<ExitInner>>,
}

impl Exit {
	pub(crate) async fn new(
		persister: Arc<dyn BarkPersister>,
		chain_source: Arc<ChainSource>,
		movement_manager: Arc<MovementManager>,
	) -> anyhow::Result<Exit> {
		let tx_manager = ExitTransactionManager::new(persister.clone(), chain_source.clone())?;
		let inner = ExitInner {
			exit_vtxos: Vec::new(),
			claimed_reorg_cursor: None,
			tx_manager,
			persister,
			chain_source,
			movement_manager,
		};
		Ok(Exit { inner: Arc::new(tokio::sync::RwLock::new(inner)) })
	}

	pub(crate) async fn load(&self) -> anyhow::Result<()> {
		let mut guard = self.inner.write().await;
		let inner = &mut *guard;

		// Finished exits never progress again, so don't track them.
		let exit_vtxo_entries = inner.persister
			.get_exit_vtxo_entries_with_states(ExitStateKind::LIVE_STATES).await?;
		inner.exit_vtxos.reserve(exit_vtxo_entries.len());

		for entry in exit_vtxo_entries {
			if let Some(vtxo) = inner.persister.get_wallet_vtxo(entry.vtxo_id).await? {
				let mut exit = ExitVtxo::from_entry(entry, &vtxo);
				exit.initialize(&mut inner.tx_manager, &*inner.persister).await?;
				inner.exit_vtxos.push(exit);
			} else {
				error!("VTXO {} is marked for exit but it's missing from the database", entry.vtxo_id);
			}
		}
		Ok(())
	}

	/// The default fee rate for broadcasting and CPFP-bumping unilateral exit transactions.
	///
	/// Exits are time-critical, so this targets fast (~1 block) confirmation.
	pub async fn default_exit_fee_rate(&self) -> FeeRate {
		self.inner.read().await.chain_source.fee_rates().await.fast
	}

	/// Returns the unilateral exit status for a given VTXO, live or finished, if any.
	///
	/// # Parameters
	/// - vtxo_id: The ID of the VTXO to check.
	/// - include_history: Whether to include the full state machine history of the exit
	/// - include_transactions: Whether to include the full set of transactions related to the exit.
	pub async fn get_exit_status(
		&self,
		vtxo_id: VtxoId,
		include_history: bool,
		include_transactions: bool,
	) -> anyhow::Result<Option<ExitTransactionStatus>> {
		let guard = self.inner.read().await;
		match guard.persister.get_exit_vtxo_entry(&vtxo_id).await? {
			None => Ok(None),
			Some(entry) => {
				Ok(Some(guard.exit_status(entry, include_history, include_transactions).await?))
			},
		}
	}

	/// Returns a clone of the tracked [ExitVtxo] if it exists.
	pub async fn get_exit_vtxo(&self, vtxo_id: VtxoId) -> Option<ExitVtxo> {
		let guard = self.inner.read().await;
		guard.exit_vtxos.iter().find(|ev| ev.id() == vtxo_id).cloned()
	}

	/// Returns the IDs of all active unilateral exits in this wallet.
	pub async fn get_exit_vtxo_ids(&self) -> Vec<VtxoId> {
		let guard = self.inner.read().await;
		guard.exit_vtxos.iter().map(|ev| ev.id()).collect()
	}

	/// Returns clones of all known unilateral exits in this wallet.
	pub async fn get_exit_vtxos(&self) -> Vec<ExitVtxo> {
		let guard = self.inner.read().await;
		guard.exit_vtxos.clone()
	}

	/// Returns statuses for every exit, live and finished.
	pub async fn list_all(
		&self,
		include_history: bool,
		include_transactions: bool,
	) -> anyhow::Result<Vec<ExitTransactionStatus>> {
		let guard = self.inner.read().await;
		let entries = guard.persister.get_exit_vtxo_entries().await?;
		guard.exit_statuses(entries, include_history, include_transactions).await
	}

	/// Returns statuses for exits that are still progressing.
	pub async fn list_live(
		&self,
		include_history: bool,
		include_transactions: bool,
	) -> anyhow::Result<Vec<ExitTransactionStatus>> {
		let guard = self.inner.read().await;
		let entries = guard.persister
			.get_exit_vtxo_entries_with_states(ExitStateKind::LIVE_STATES).await?;
		guard.exit_statuses(entries, include_history, include_transactions).await
	}

	/// Returns statuses for exits in a terminal state: claimed, aborted because the VTXO was
	/// already spent, or canceled.
	pub async fn list_finished(
		&self,
		include_history: bool,
		include_transactions: bool,
	) -> anyhow::Result<Vec<ExitTransactionStatus>> {
		let guard = self.inner.read().await;
		let entries = guard.persister
			.get_exit_vtxo_entries_with_states(ExitStateKind::FINISHED_STATES).await?;
		guard.exit_statuses(entries, include_history, include_transactions).await
	}

	/// Returns whether a VTXO has an active or completed unilateral exit.
	pub async fn is_exiting(&self, vtxo_id: VtxoId) -> bool {
		let guard = self.inner.read().await;
		let state = guard.exit_vtxos.iter().find(|ev| ev.id() == vtxo_id).map(|ev| ev.state());
		match state {
			Some(ExitState::Start(_)) => true,
			Some(ExitState::Processing(_)) => true,
			Some(ExitState::AwaitingDelta(_)) => true,
			Some(ExitState::Claimable(_)) => true,
			Some(ExitState::ClaimInProgress(_)) => true,
			Some(ExitState::Claimed(_)) => true,
			Some(ExitState::VtxoAlreadySpent(_)) => false,
			Some(ExitState::VtxoSwept(_)) => false,
			Some(ExitState::Canceled(_)) => false,
			None => false,
		}
	}

	/// True if there are any unilateral exits which have been started but are not yet claimable.
	pub async fn has_pending_exits(&self) -> bool {
		let guard = self.inner.read().await;
		guard.exit_vtxos.iter().any(|ev| ev.state().is_pending())
	}

	/// Returns the earliest block height at which all tracked exits will be claimable
	pub async fn all_claimable_at_height(&self) -> Option<BlockHeight> {
		let guard = self.inner.read().await;
		let mut highest_claimable_height = None;
		for exit in &guard.exit_vtxos {
			match exit.state().claimable_height() {
				Some(h) => highest_claimable_height = cmp::max(highest_claimable_height, Some(h)),
				None => continue,
			}
		}
		highest_claimable_height
	}

	/// Starts the unilateral exit process for the entire wallet (all eligible VTXOs).
	///
	/// It does not block until completion, you must use [Exit::progress_exits] to advance each exit.
	///
	/// It's recommended to sync the wallet, by using something like [Wallet::maintenance] being
	/// doing this.
	pub async fn start_exit_for_entire_wallet(&self) -> anyhow::Result<()> {
		let mut guard = self.inner.write().await;
		let all_vtxos = guard.persister.get_vtxos_by_state(&VtxoStateKind::UNSPENT_STATES).await?
			.into_iter();

		// Partition: separate eligible VTXOs from dust
		let total_vtxos = all_vtxos.len();
		let mut eligible = Vec::with_capacity(total_vtxos);
		for v in all_vtxos {
			// Skip non-standard VTXOs
			match guard.persister.get_full_vtxo(v.id()).await {
				Ok(Some(full)) => match full.check_standard() {
					Ok(()) => eligible.push(v.vtxo),
					Err(e) => warn!("Skipping non-standard VTXO {}: {:#}", v.id(), e),
				},
				Ok(None) => error!("Failed to retrieve full VTXO: {}", v.id()),
				Err(e) => error!("Failed to retrieve full VTXO {}: {:#}", v.id(), e),
			}
		}

		// If everything is dust.
		let ineligible = total_vtxos - eligible.len();
		if eligible.is_empty() && ineligible > 0 {
			warn!(
				"Exit not started: all {} VTXOs are non-standard. To exit and consolidate you \
				should try refreshing your VTXOs first",
				ineligible,
			);
			return Ok(());
		}

		guard.start_exit_for_vtxos(&eligible, false, false, None).await
	}

	/// Starts the unilateral exit process for the given VTXOs.
	///
	/// It does not block until completion, you must use [Exit::progress_exits] to advance each exit.
	///
	/// It's recommended to sync the wallet, by using something like [Wallet::maintenance] being
	/// doing this.
	pub async fn start_exit_for_vtxos(
		&self,
		vtxos: &[impl Borrow<Vtxo<Bare>>],
	) -> anyhow::Result<()> {
		let mut guard = self.inner.write().await;
		guard.start_exit_for_vtxos(vtxos, false, false, None).await
	}

	/// Serialize immutable ordinary-spend publication with this wallet's
	/// native reservations and progression. Acquire BEFORE fallback-history;
	/// no caller may acquire this write guard while holding that named lock.
	pub(crate) async fn lock_ordinary_spend_publication(&self) -> tokio::sync::RwLockWriteGuard<'_, ExitInner> {
		self.inner.write().await
	}

	/// Reserve a safety exit under its real durable movement. The intent is
	/// stored before the lock, so a crash can resume it; progression must
	/// obtain this same lock before broadcasting and never steals an action.
	pub(crate) async fn start_protective_exit(&self, vtxo: &Vtxo<Bare>, wallet: &Wallet) -> anyhow::Result<bool> {
		let mut inner = self.inner.write().await;
		let _history = wallet.lock_automatic_exit_ownership(vtxo.id()).await?;
		let snapshot = wallet.inner.db.get_fallback_snapshot().await?;
		let Some(stored) = wallet.inner.db.get_wallet_vtxo(vtxo.id()).await? else { return Ok(false); };
		// A recovery quarantine is ours only when no native action claims it.
		// Release it inside the SAME critical section as intent+holder storage.
		if stored.state == (crate::vtxo::VtxoState::Locked { holder: None })
			&& snapshot.quarantined_vtxos.contains(&vtxo.id())
			&& !wallet.operation_input_ids().await?.contains(&vtxo.id()) {
			wallet.inner.db.release_vtxo_lock(vtxo.id(), None).await?;
		}
		let Some(stored) = wallet.inner.db.get_wallet_vtxo(vtxo.id()).await? else { return Ok(false); };
		if stored.state != crate::vtxo::VtxoState::Spendable { return Ok(false); }
		inner.start_exit_for_vtxos(&[vtxo], false, true, None).await?;
		let exit = inner.exit_vtxos.iter().find(|exit| exit.id() == vtxo.id())
			.context("protective exit was not stored")?;
		let holder = exit.movement_id().context("protective exit has no movement")?;
		wallet.lock_vtxos([vtxo.id()], Some(holder.into())).await?;
		Ok(true)
	}

	/// A binding refresh retains its round holder. The movement stores the
	/// exact checkpoint identity atomically with the exit intent, and every
	/// broadcast rechecks that identity without taking a second round lock.
	pub(crate) async fn start_protective_round_exit(
		&self, vtxo: &Vtxo<Bare>, wallet: &Wallet, round: u32, holder: MovementId,
	) -> anyhow::Result<()> {
		let mut inner = self.inner.write().await;
		let _history = wallet.lock_automatic_exit_ownership(vtxo.id()).await?;
		inner.start_exit_for_vtxos(&[vtxo], false, true, Some((round, holder))).await?;
		wallet.lock_vtxos([vtxo.id()], Some(holder.into())).await
	}

	/// Revive this same binding exit if its recorded claim left the canonical
	/// chain. Retain its movement and state history, rather than debit a new exit.
	pub(crate) async fn rearm_binding_claim(
		&self, id: VtxoId, wallet: &Wallet, round: u32, holder: MovementId,
	) -> anyhow::Result<()> {
		let mut inner = self.inner.write().await;
		let mut entry = inner.persister.get_exit_vtxo_entry(&id).await?
			.context("binding recovery exit record disappeared")?;
		let Some(movement_id) = entry.movement_id else { bail!("binding exit has no native movement"); };
		let movement = inner.persister.get_movement_by_id(movement_id).await?;
		ensure!(movement.metadata.get("protective_round_state") == Some(&serde_json::json!(round))
			&& movement.metadata.get("protective_round_movement") == Some(&serde_json::json!(holder)),
			"recovery cannot revive another round's exit");
		inner.movement_manager.rearm_chain_recovery(movement_id).await?;
		let ExitState::Claimed(claim) = &entry.state else { return Ok(()); };
		let tip = wallet.chain().tip_ref_uncached().await?;
		if tip.height >= claim.block.height && wallet.chain().block_ref(claim.block.height).await? == claim.block {
			return Ok(());
		}
		let coin = inner.persister.get_wallet_vtxo(id).await?.context("binding exit body disappeared")?;
		ensure!(coin.state == (VtxoState::Locked { holder: Some(holder.into()) }),
			"binding recovery cannot revive another operation's holder");
		entry.history.push(entry.state.clone());
		entry.state = ExitState::new_start(tip.height);
		inner.persister.store_exit_vtxo_entry(&entry).await?;
		inner.exit_vtxos.retain(|exit| exit.id() != id);
		inner.exit_vtxos.push(ExitVtxo::from_entry(entry, &coin));
		Ok(())
	}

	/// Similar to [Exit::start_exit_for_vtxos], but it skips any dust/standardness checks.
	///
	/// This should only be used when you are sure that the VTXOs are already onchain, or you are
	/// able to broadcast to a node which will accept non-standard transactions.
	pub async fn start_exit_for_vtxos_including_non_standard(
		&self,
		vtxos: &[impl Borrow<Vtxo<Bare>>],
	) -> anyhow::Result<()> {
		let mut guard = self.inner.write().await;
		guard.start_exit_for_vtxos(vtxos, true, false, None).await
	}

	/// Cancels the unilateral exit for the given VTXO.
	///
	/// Only exits still in their abortable window can be canceled — i.e. before the *final* exit
	/// transaction has been broadcast (see [ExitState::is_cancelable]); shared ancestor
	/// transactions may already be on-chain. Because starting an exit never touches the VTXO,
	/// there's nothing to undo on the VTXO side: it stays spendable and a fresh exit can be
	/// started for it later.
	///
	/// Canceling an already-canceled exit is a no-op, so retries are safe.
	///
	/// # Errors
	/// - [ExitError::NotExiting] if the VTXO never had an exit.
	/// - [ExitError::CannotCancelExit] if the exit has progressed past its abortable window.
	/// - [ExitError::ExitTxAlreadyBroadcast] if the final exit tx is already on the network.
	pub async fn cancel_exit(&self, vtxo_id: VtxoId) -> anyhow::Result<(), ExitError> {
		let mut guard = self.inner.write().await;
		let inner = &mut *guard;

		let idx = match inner.exit_vtxos.iter().position(|ev| ev.id() == vtxo_id) {
			Some(idx) => idx,
			None => {
				// Only live exits are in memory; check the store for a finished one.
				let entry = inner.persister.get_exit_vtxo_entry(&vtxo_id).await
					.map_err(|e| ExitError::InternalError { error: e.to_string() })?;
				return match entry.map(|e| e.state.kind()) {
					Some(ExitStateKind::Canceled) => Ok(()),
					Some(kind) => Err(ExitError::CannotCancelExit { vtxo: vtxo_id, state: kind }),
					None => Err(ExitError::NotExiting { vtxo: vtxo_id }),
				};
			},
		};

		if !inner.exit_vtxos[idx].is_cancelable() {
			return Err(ExitError::CannotCancelExit {
				vtxo: vtxo_id,
				state: inner.exit_vtxos[idx].state().kind(),
			});
		}

		// Double check with the network first before canceling the exit.
		let leaf_txid = inner.exit_vtxos[idx].get_vtxo(&*inner.persister).await?.point().txid;
		match inner.tx_manager.sync_exit_tx(leaf_txid).await? {
			TxStatus::NotFound => {},
			TxStatus::Mempool | TxStatus::Confirmed(_) => {
				return Err(ExitError::ExitTxAlreadyBroadcast { vtxo: vtxo_id, txid: leaf_txid });
			},
		}

		let tip = inner.chain_source.tip().await
			.map_err(|e| ExitError::TipRetrievalFailure { error: e.to_string() })?;

		if let Some(movement_id) = inner.exit_vtxos[idx].movement_id() {
			let movement = inner.persister.get_movement_by_id(movement_id).await
				.map_err(|e| ExitError::InternalError { error: e.to_string() })?;
			if movement.metadata.get("protective_exit") == Some(&serde_json::Value::Bool(true)) {
				inner.persister.release_vtxo_lock(vtxo_id, Some(&movement_id.into())).await
					.map_err(|e| ExitError::InternalError { error: e.to_string() })?;
			}
		}

		// Record the cancellation first, so it survives even if a later best-effort step fails.
		inner.exit_vtxos[idx].cancel(tip, &*inner.persister).await?;

		// Stop syncing this exit's transactions. Ancestor txs shared with sibling exits stay.
		if let Some(txids) = inner.exit_vtxos[idx].txids() {
			inner.tx_manager.untrack_vtxo_exits(&txids).await;
		}

		// Finalize the associated movement as Canceled (best-effort, like the other reconcilers).
		if let Some(movement_id) = inner.exit_vtxos[idx].movement_id() {
			if let Err(e) = inner.movement_manager
				.finish_movement(movement_id, MovementStatus::Canceled).await
			{
				error!("Failed to finalize exit movement {} as Canceled: {:#}", movement_id, e);
			}
		}

		// Drop it from the active set; the canceled row remains on disk for auditing.
		inner.exit_vtxos.swap_remove(idx);
		info!("Canceled unilateral exit for VTXO {}", vtxo_id);
		Ok(())
	}

	/// Reset exit to an empty state. Should be called when dropping VTXOs
	///
	/// Note: _This method is **dangerous** and can lead to funds loss. Be cautious._
	pub(crate) async fn dangerous_clear_exit(&self) -> anyhow::Result<()> {
		let mut guard = self.inner.write().await;
		for exit in &guard.exit_vtxos {
			guard.persister.remove_exit_vtxo_entry(&exit.id()).await?;
		}
		guard.exit_vtxos.clear();
		Ok(())
	}

	/// Iterates over each registered VTXO and attempts to progress their unilateral exit.
	///
	/// Initializes any pending exits and refreshes the chain view of exit transactions
	/// before advancing state.
	///
	/// If you need to create CPFP transactions using a BDK-backed wallet, call
	/// [Exit::exits_needing_cpfp] after this, supply the signed CPFPs via [Exit::provide_cpfp_tx],
	/// then call this method again to advance the state past [ExitTxStatus::AwaitingCpfpBroadcast].
	///
	/// # Returns
	///
	/// The exit status of each VTXO being exited which has also not yet been spent
	pub async fn progress_exits(
		&self,
		wallet: &Wallet,
	) -> anyhow::Result<Option<Vec<ExitProgressStatus>>> {
		let mut guard = self.inner.write().await;
		let mut exit_statuses = guard.rearm_orphaned_claims(wallet).await?;
		guard.refresh_tx_state(wallet).await?;
		let mut exit_vtxos = std::mem::take(&mut guard.exit_vtxos);

		for ev in exit_vtxos.iter_mut() {
			if !ev.is_initialized() {
				warn!("Skipping progress of uninitialized unilateral exit {}", ev.id());
				continue;
			}

			info!("Progressing exit for VTXO {}", ev.id());
			let pre_state = ev.state().clone();
			let error = match ev.progress(
				wallet,
				&mut guard.tx_manager,
				true,
			).await {
				Ok(_) => None,
				Err(e) => {
					match &e {
						ExitError::InsufficientConfirmedFunds { .. } => {
							warn!("Can't progress exit for VTXO {} at this time: {}", ev.id(), e);
						},
						_ => {
							error!("Error progressing exit for VTXO {}: {}", ev.id(), e);
						}
					}
					Some(e)
				}
			};

			let state_changed = ev.state() != &pre_state;
			Self::reconcile_vtxo_and_movement(
				wallet, &guard.movement_manager, ev, state_changed,
			).await;

			if !matches!(ev.state(), ExitState::Claimed(..)) {
				exit_statuses.push(ExitProgressStatus {
					vtxo_id: ev.id(),
					state: ev.state().clone(),
					error,
				});
			}
		}

		guard.exit_vtxos = exit_vtxos;
		Ok(Some(exit_statuses))
	}

	/// Maps the current exit state onto the VTXO and movement bookkeeping:
	/// - mark the VTXO `Exited` once every exit transaction has been broadcast (i.e. past
	///   `Start`, with `Processing` having all txs broadcast or beyond),
	/// - finish the movement as `Successful` when we reach `Claimed`,
	/// - finish the movement as `Canceled` when we detect the VTXO was already spent.
	///
	/// All updates are best-effort: failures are logged and don't abort progress. The VTXO
	/// transition is idempotent; the movement transitions only fire on a fresh state change
	/// to avoid notification spam.
	async fn reconcile_vtxo_and_movement(
		wallet: &Wallet,
		movements: &MovementManager,
		ev: &ExitVtxo,
		state_changed: bool,
	) {
		let binding = if let Some(id) = ev.movement_id() {
			match wallet.inner.db.get_movement_by_id(id).await {
				Ok(movement) => movement.metadata.contains_key("protective_round_state"),
				Err(error) => { error!("Exit holder could not be checked: {:#}", error); true },
			}
		} else { false };
		// Binding exchange inputs and replacements retain their original
		// round holder until an independently verified terminal user claim.
		if ev.state().warrants_exited_vtxo() && !binding {
			if let Err(e) = wallet.mark_vtxos_as_exited([ev.id()]).await {
				error!("Failed to mark VTXO {} as Exited: {:#}", ev.id(), e);
			}
		}

		// A native Claimable scan also labels server forfeits as Claimed.
		// The owning binding round verifies the user witness before
		// finalizing either its old intent or replacement exit movement.
		if binding {
			// A normal cooperative round may have completed after the exit
			// intent was created. Its old intent consumed no principal.
			if state_changed && matches!(ev.state(), ExitState::VtxoAlreadySpent(_)) {
				if let Some(id) = ev.movement_id() {
					if let Err(error) = movements.finish_movement_with_update(id, MovementStatus::Canceled,
						MovementUpdate::new().effective_balance(bitcoin::SignedAmount::ZERO),
					).await { error!("Binding exit intent could not be cancelled: {:#}", error); }
				}
			}
			return;
		}
		if !state_changed { return; }
		let Some(movement_id) = ev.movement_id() else { return };
		let new_status = match ev.state() {
			ExitState::Claimed(_) => MovementStatus::Successful,
			ExitState::VtxoAlreadySpent(_) => MovementStatus::Canceled,
			_ => return,
		};
		let ordinary_cancel = if matches!(ev.state(), ExitState::VtxoAlreadySpent(_)) {
			let snapshot = match wallet.inner.db.get_fallback_snapshot().await {
				Ok(snapshot) => snapshot,
				Err(error) => { error!("Exit retirement proof could not be read: {:#}", error); return; },
			};
			if snapshot.verified_ordinary_spends.contains(&ev.id()) {
				let movement = match wallet.inner.db.get_movement_by_id(movement_id).await {
					Ok(movement) => movement,
					Err(error) => { error!("Exit retirement intent could not be read: {:#}", error); return; },
				};
				movement.metadata.get("protective_exit") == Some(&serde_json::Value::Bool(true))
					&& movement.input_vtxos == [ev.id()]
			} else { false }
		} else { false };
		let result = if ordinary_cancel {
			movements.finish_movement_with_update(movement_id, new_status,
				MovementUpdate::new().effective_balance(bitcoin::SignedAmount::ZERO)).await
		} else { movements.finish_movement(movement_id, new_status).await };
		if let Err(e) = result {
			error!(
				"Failed to finalize exit movement {} as {:?}: {:#}",
				movement_id, new_status, e,
			);
		}
	}

	/// For use when syncing. Pending exits will be initialized, the network status of each
	/// [ExitTransactionPackage] will be updated, and finally, any unilateral exits that are waiting
	/// for network updates will be progressed.
	pub async fn sync(
		&self,
		wallet: &Wallet,
	) -> anyhow::Result<()> {
		let mut guard = self.inner.write().await;
		guard.refresh_tx_state(wallet).await?;
		let mut exit_vtxos = std::mem::take(&mut guard.exit_vtxos);
		for exit in &mut exit_vtxos {
			if !exit.is_initialized() {
				warn!("Skipping progress of uninitialized unilateral exit {}", exit.id());
				continue;
			}

			let pre_state = exit.state().clone();
			if let Err(e) = exit.progress(
				wallet, &mut guard.tx_manager, true,
			).await {
				error!("Error syncing exit for VTXO {}: {}", exit.id(), e);
			}
			let state_changed = exit.state() != &pre_state;
			Self::reconcile_vtxo_and_movement(
				wallet, &guard.movement_manager, exit, state_changed,
			).await;
		}
		guard.exit_vtxos = exit_vtxos;
		Ok(())
	}

	/// Returns one [ExitCpfpRequest] for each exit transaction that needs a CPFP child.
	///
	/// A request with `rbf_requirement = None` means no CPFP exists yet. A request with
	/// `rbf_requirement = Some(...)` means a third-party CPFP is already in the mempool;
	/// the caller can optionally provide a replacement with a higher fee rate.
	/// Call [Exit::provide_cpfp_tx] to submit the child.
	pub async fn exits_needing_cpfp(&self) -> Vec<ExitCpfpRequest> {
		let guard = self.inner.read().await;
		let mut requests = Vec::new();
		for ev in &guard.exit_vtxos {
			let ExitState::Processing(s) = ev.state() else { continue };
			for tx in &s.transactions {
				let rbf_requirement = match &tx.status {
					ExitTxStatus::AwaitingCpfpBroadcast => None,
					ExitTxStatus::AwaitingConfirmation {..} => {
						// Read mempool RBF info from the transaction manager; fee info is
						// tracked on the child independently of its origin. If we don't have
						// it yet (e.g. ancestor info call hasn't run), skip this round — the
						// next sync will populate it.
						match guard.tx_manager.get_child_status(tx.txid).await {
							Ok(Some(c)) => match c.fee_info {
								Some(fi) => Some(RbfRequirement {
									min_fee_rate: fi.fee_rate,
									current_package_fee: fi.total_fee,
								}),
								None => continue,
							},
							_ => continue,
						}
					},
					_ => continue,
				};
				let package = match guard.tx_manager.get_package(tx.txid) {
					Ok(p) => p,
					Err(_) => continue,
				};
				let exit_tx = package.read().await.exit.tx.clone();
				requests.push(ExitCpfpRequest {
					vtxo_id: ev.id(),
					exit_tx,
					rbf_requirement,
				});
			}
		}
		requests
	}

	/// Submit a signed CPFP child transaction for a given exit transaction.
	///
	/// The child must spend the P2A anchor output of the parent exit transaction identified by
	/// `exit_txid`. The package is broadcast immediately and the state advances to
	/// [ExitTxStatus::AwaitingConfirmation]. The child is persisted so it survives restarts.
	///
	/// # TODO
	/// `wallet` is required here only because [ExitVtxo::progress] calls `get_vtxo(&wallet.db)`
	/// and `tip_height()` unconditionally, even though neither is needed for the
	/// `AwaitingCpfpBroadcast → AwaitingConfirmation` transition. The fix is to make [ExitVtxo::progress]
	/// take `persister` and `chain_source` separately instead of the full wallet, and call
	/// `tip_height()` lazily only where needed.
	pub async fn provide_cpfp_tx(
		&self,
		wallet: &Wallet,
		exit_txid: Txid,
		child_tx: Transaction,
	) -> anyhow::Result<(), ExitError> {
		let origin = ExitTxOrigin::Wallet { confirmed_in: None };
		let mut guard = self.inner.write().await;
		let inner = &mut *guard;
		let snapshot = inner.persister.get_fallback_snapshot().await
			.map_err(|error| ExitError::InternalError { error: error.to_string() })?;
		// A child may have been prepared before a complete ordinary proof was
		// checkpointed. Check BEFORE the broadcast, not only in later progress.
		// A live sibling may still need this exact shared ancestor.
		let mut owners = Vec::new();
		let mut blocked = snapshot.verified_ordinary_spends.clone();
		for exit in inner.exit_vtxos.iter().filter(|exit|
			exit.txids().is_some_and(|txids| txids.contains(&exit_txid))) {
			owners.push(exit.id());
			match exit_rebroadcast_allowed(exit, wallet).await {
				Ok(true) => {},
				result => {
					warn!("CPFP owner {} cannot authorize resumed replay: {:?}", exit.id(), result);
					blocked.insert(exit.id());
				},
			}
		}
		if ordinary_proof_blocks_cpfp(owners, &blocked) {
			return Err(ExitError::InvalidWalletState {
				error: format!("exit package {exit_txid} has only discharged or unauthorized owners; stale CPFP is not broadcast"),
			});
		}

		// Broadcast the package first and only commit the child if it's accepted
		if !inner.tx_manager.broadcast_and_set_child(exit_txid, child_tx, origin).await? {
			return Ok(());
		}

		for ev in inner.exit_vtxos.iter_mut() {
			let ExitState::Processing(s) = ev.state() else { continue };
			let has_tx = s.transactions.iter().any(|tx| tx.txid == exit_txid);
			if has_tx {
				if let Err(e) = ev.progress(wallet, &mut inner.tx_manager, false).await {
					warn!("Failed to progress exit for {} after CPFP: {}", exit_txid, e);
				}
				break;
			}
		}

		Ok(())
	}

	/// Lists all exits that are claimable
	pub async fn list_claimable(&self) -> Vec<ExitVtxo> {
		let guard = self.inner.read().await;
		guard.exit_vtxos.iter().filter(|ev| ev.is_claimable()).cloned().collect()
	}

	/// Sign any inputs of the PSBT that is an exit claim input
	///
	/// Can take the result PSBT of [`bdk_wallet::TxBuilder::finish`] on which
	/// [`crate::onchain::TxBuilderExt::add_exit_claim_inputs`] has been used
	///
	/// Note: This doesn't mark the exit output as spent, it's up to the caller to
	/// do that, or it will be done once the transaction is seen in the network
	pub async fn sign_exit_claim_inputs(&self, psbt: &mut Psbt, wallet: &Wallet) -> anyhow::Result<()> {
		let guard = self.inner.read().await;
		guard.sign_exit_claim_inputs(psbt, wallet).await
	}

	/// Builds a PSBT that drains the provided claimable unilateral exits to the given address.
	///
	/// - `inputs`: Claimable unilateral exits.
	/// - `wallet`: The bark wallet containing the keys needed to spend the unilateral exits.
	/// - `address`: Destination address for the claim.
	/// - `fee_rate_override`: Optional fee rate to use.
	///
	/// Returns a PSBT ready to be broadcast.
	pub async fn drain_exits(
		&self,
		inputs: &[impl Borrow<ExitVtxo>],
		wallet: &Wallet,
		address: Address,
		fee_rate_override: Option<FeeRate>,
	) -> anyhow::Result<Psbt, ExitError> {
		let guard = self.inner.read().await;

		let tip = guard.chain_source.tip().await
			.map_err(|e| ExitError::TipRetrievalFailure { error: e.to_string() })?;

		if inputs.is_empty() {
			return Err(ExitError::ClaimMissingInputs);
		}
		let mut vtxos = HashMap::with_capacity(inputs.len());
		for input in inputs {
			let i = input.borrow();
			let vtxo = i.get_full_vtxo(&*guard.persister).await?;
			vtxos.insert(i.id(), vtxo);
		}

		let mut tx = {
			let mut output_amount = Amount::ZERO;
			let mut tx_ins = Vec::with_capacity(inputs.len());
			for input in inputs {
				let input = input.borrow();
				let vtxo = &vtxos[&input.id()];
				if !matches!(input.state(), ExitState::Claimable(..)) {
					return Err(ExitError::VtxoNotClaimable { vtxo: input.id() });
				}

				output_amount += vtxo.amount();

				let clause = wallet.find_signable_clause(vtxo).await
					.ok_or(ExitError::ClaimMissingSignableClause { vtxo: vtxo.id() })?;

				tx_ins.push(TxIn {
					previous_output: vtxo.point(),
					script_sig: ScriptBuf::default(),
					sequence: clause.sequence().unwrap_or(Sequence::ZERO),
					witness: Witness::new(),
				});
			}

			let locktime = tip.to_locktime()
				.map_err(|e| ExitError::InvalidLocktime { tip, error: e.to_string() })?;

			Transaction {
				version: bitcoin::transaction::Version::TWO,
				lock_time: locktime,
				input: tx_ins,
				output: vec![
					TxOut {
						script_pubkey: address.script_pubkey(),
						value: output_amount,
					},
				],
			}
		};

		// Create a PSBT to determine the weight of the transaction so we can deduct a tx fee
		let create_psbt = |tx: Transaction| async {
			let mut psbt = Psbt::from_unsigned_tx(tx)
				.map_err(|e| ExitError::InternalError {
					error: format!("Failed to create exit claim PSBT: {}", e),
				})?;
			psbt.inputs.iter_mut().zip(inputs).for_each(|(i, e)| {
				let v = &vtxos[&e.borrow().id()];
				i.set_exit_claim_input(v);
				i.witness_utxo = Some(v.txout())
			});
			guard.sign_exit_claim_inputs(&mut psbt, wallet).await
				.map_err(|e| ExitError::ClaimSigningError { error: e.to_string() })?;
			Ok(psbt)
		};
		let fee_amount = {
			let fee_rate = fee_rate_override
				.unwrap_or(guard.chain_source.fee_rates().await.regular);
			fee_rate * create_psbt(tx.clone()).await?
				.extract_tx()
				.map_err(|e| ExitError::InternalError {
					error: format!("Failed to get tx from signed exit claim PSBT: {}", e),
				})?
				.weight()
		};

		// We adjust the drain output to cover the fee
		let needed = fee_amount + P2TR_DUST;
		if needed > tx.output[0].value {
			return Err(ExitError::ClaimFeeExceedsOutput {
				needed, output: tx.output[0].value,
			});
		}
		tx.output[0].value -= fee_amount;

		// Now create the final signed PSBT
		create_psbt(tx).await
	}

	/// Resolves, per requested VTXO (in input order; duplicates: first occurrence wins), the
	/// exit transactions not yet confirmed onchain and what is known about their CPFP children.
	///
	/// Omitted from the result: transactions already confirmed onchain and transactions
	/// already attributed to an earlier VTXO in the same call (shared ancestry).
	async fn collect_unconfirmed_exit_parents(
		&self,
		vtxos: &[VtxoId],
	) -> Result<Vec<UnconfirmedExitParents>, ExitError> {
		let mut groups = Vec::with_capacity(vtxos.len());
		let mut requested = HashSet::with_capacity(vtxos.len());
		let mut seen = HashSet::new();

		for &vtxo_id in vtxos {
			if !requested.insert(vtxo_id) {
				continue;
			}

			let vtxo = self.inner.read().await.persister.get_full_vtxo(vtxo_id).await
				.map_err(|e| ExitError::InvalidWalletState { error: e.to_string() })?
				.ok_or(ExitError::UnknownVtxo { vtxo: vtxo_id })?;

			if let Err(error) = vtxo.check_standard() {
				return Err(ExitError::NonStandardVtxo { vtxo: vtxo_id, error });
			}

			let mut parents = Vec::new();
			// Txs whose confirmation isn't tracked; queried against the chain below, after
			// the read guard is dropped (the status query needs the write guard).
			let mut pending_status = Vec::new();

			{
				let guard = self.inner.read().await;
				match guard.exit_vtxos.iter().find(|ev| ev.id() == vtxo_id).map(|ev| ev.state()) {
					Some(ExitState::Claimed(_)) => {
						return Err(ExitError::VtxoAlreadyExited { vtxo: vtxo_id });
					},
					// The VTXO was consumed by something other than this exit (e.g. forfeited
					// in a round), so no exit transactions can be broadcast.
					Some(ExitState::VtxoAlreadySpent(_)) => {
						return Err(ExitError::VtxoAlreadySpent { vtxo: vtxo_id });
					},
					// An output the chain needs is gone, so nothing can be broadcast either, but
					// the cause differs from an offchain spend so the caller gets its own error.
					Some(ExitState::VtxoSwept(_)) => {
						return Err(ExitError::VtxoSwept { vtxo: vtxo_id });
					},
					Some(ExitState::Processing(s)) => {
						// An in-progress exit: confirmed transactions are done, a package awaiting
						// confirmation has a child in the mempool, and everything else still needs
						// one. Confirmation is read from tracked state, so no chain query is needed.
						for exit_tx in &s.transactions {
							let child = match &exit_tx.status {
								ExitTxStatus::Confirmed { .. } => continue,
								ExitTxStatus::AwaitingConfirmation { .. } => {
									match guard.tx_manager.get_child_status(exit_tx.txid).await {
										Ok(Some(c)) => ChildState::InMempool(c.fee_info),
										_ => ChildState::InMempool(None),
									}
								},
								ExitTxStatus::VerifyInputs |
								ExitTxStatus::AwaitingCpfpBroadcast |
								ExitTxStatus::AwaitingInputConfirmation { .. } => ChildState::Missing,
							};
							if !seen.insert(exit_tx.txid) {
								continue;
							}
							let package = guard.tx_manager.get_package(exit_tx.txid)?;
							let tx = package.read().await.exit.tx.clone();
							parents.push((tx, child));
						}
					},
					// All exit txs are confirmed, so there's nothing left to broadcast.
					Some(ExitState::AwaitingDelta(_)) |
					Some(ExitState::Claimable(_)) |
					Some(ExitState::ClaimInProgress(_)) => {},
					// Exit not started: consider the whole tree chain, deferring the per-tx
					// confirmation check until after we drop the lock.
					Some(ExitState::Start(_)) | Some(ExitState::Canceled(_)) => {
						for item in vtxo.transactions() {
							pending_status.push(item.tx);
						}
					},
					// Never exited: the wallet state still records a VTXO consumed outside
					// the exit flow (e.g. forfeited in a round, or recovered as exited).
					None => {
						let state = guard.persister.get_wallet_vtxo(vtxo_id).await
							.map_err(|e| ExitError::InvalidWalletState { error: e.to_string() })?
							.ok_or(ExitError::UnknownVtxo { vtxo: vtxo_id })?
							.state;
						match state.kind() {
							VtxoStateKind::Spent => {
								return Err(ExitError::VtxoAlreadySpent { vtxo: vtxo_id });
							},
							VtxoStateKind::Exited => {
								return Err(ExitError::VtxoAlreadyExited { vtxo: vtxo_id });
							},
							VtxoStateKind::Spendable | VtxoStateKind::Locked => {},
						}
						for item in vtxo.transactions() {
							pending_status.push(item.tx);
						}
					},
				}
			}

			// Query the chain for the not-yet-started candidates, skipping any already
			// confirmed onchain and any duplicate shared txid. Statuses are cached and only
			// refresh during exit sync, so results reflect the last sync.
			for tx in pending_status {
				let txid = tx.compute_txid();
				if seen.contains(&txid) {
					continue;
				}
				let mut guard = self.inner.write().await;
				let status = guard.tx_manager.tx_status(txid).await
					.map_err(|e| ExitError::TransactionRetrievalFailure {
						txid, error: e.to_string(),
					})?;

				let child = match status {
					TxStatus::NotFound => ChildState::Missing,
					// A zero-fee exit tx only relays as a package, so some child is paying for it.
					TxStatus::Mempool => {
						match guard.tx_manager.get_child_status(txid).await {
							Ok(Some(c)) => ChildState::InMempool(c.fee_info),
							_ => ChildState::InMempool(None),
						}
					},
					TxStatus::Confirmed(_) => continue,
				};

				seen.insert(txid);
				parents.push((tx, child));
			}

			groups.push(UnconfirmedExitParents { vtxo, parents });
		}
		Ok(groups)
	}
}

/// Only this exact, still-abortable safety intent can yield its holder to an
/// independently validated complete ordinary transfer. Binding refreshes and
/// all other operations retain their signed material and their own holders.
pub(crate) fn ordinary_proof_retires_protective_exit(
	entry: &crate::persist::models::StoredExit, movement: &crate::movement::Movement,
	holder: &crate::vtxo::VtxoLockHolder,
) -> bool {
	entry.movement_id == Some(movement.id) && *holder == movement.id.into()
		&& movement.status == MovementStatus::Pending && entry.state.is_cancelable()
		&& movement.subsystem.name == Subsystem::EXIT.as_name()
		&& movement.metadata.get("protective_exit") == Some(&serde_json::Value::Bool(true))
		&& !movement.metadata.contains_key("protective_round_state")
		&& !movement.metadata.contains_key("protective_round_movement")
		&& movement.input_vtxos == [entry.vtxo_id]
}

/// A cached Claimed label alone never authorizes a balance or another holder.
/// The caller additionally proves the old block noncanonical and the exact
/// signed own-key leaf or funding root currently canonical and unspent under the history lock.
fn orphaned_claim_owns_principal(
	entry: &StoredExit, movement: &crate::movement::Movement, state: &VtxoState,
	amount: Amount, snapshot: &crate::fallback::payouts::FallbackSnapshot,
) -> bool {
	matches!(entry.state, ExitState::Claimed(_))
		&& own_exit_principal(entry, movement, state, amount, snapshot)
}

fn own_exit_principal(
	entry: &StoredExit, movement: &crate::movement::Movement, state: &VtxoState,
	amount: Amount, snapshot: &crate::fallback::payouts::FallbackSnapshot,
) -> bool {
	owned_exit_identity(entry, movement, state, amount, snapshot)
		&& !snapshot.adopted_payout_vtxos.contains(&entry.vtxo_id)
}

fn owned_exit_identity(
	entry: &StoredExit, movement: &crate::movement::Movement, state: &VtxoState,
	amount: Amount, snapshot: &crate::fallback::payouts::FallbackSnapshot,
) -> bool {
	*state == VtxoState::Exited && entry.movement_id == Some(movement.id)
		&& movement.subsystem.name == Subsystem::EXIT.as_name()
		&& movement.input_vtxos == [entry.vtxo_id]
		&& amount.to_signed().is_ok_and(|gross| gross > bitcoin::SignedAmount::ZERO && movement.effective_balance == -gross)
		&& matches!(movement.status, MovementStatus::Successful | MovementStatus::Pending)
		&& !movement.metadata.contains_key("protective_round_state")
		&& !movement.metadata.contains_key("protective_round_movement")
		&& !snapshot.verified_ordinary_spends.contains(&entry.vtxo_id)
		&& !snapshot.unproven_spent_vtxos.contains(&entry.vtxo_id)
}

/// A durable Claimed -> Processing checkpoint is a continuation, not a new
/// reservation. Terminal/canceled history after that claim cannot authorize it.
fn resumed_claim_owns_principal(
	entry: &StoredExit, movement: &crate::movement::Movement, state: &VtxoState,
	amount: Amount, snapshot: &crate::fallback::payouts::FallbackSnapshot,
) -> bool {
	resumed_claim_shape(entry) && movement.status == MovementStatus::Pending
		&& own_exit_principal(entry, movement, state, amount, snapshot)
}

fn resumed_claim_shape(entry: &StoredExit) -> bool {
	let Some(last_claim) = entry.history.iter().rposition(|state| matches!(state, ExitState::Claimed(_))) else { return false; };
	matches!(entry.state, ExitState::Processing(_) | ExitState::AwaitingDelta(_) | ExitState::Claimable(_) | ExitState::ClaimInProgress(_))
		&& entry.history[last_claim + 1..].iter().all(|state| matches!(state,
			ExitState::Processing(_) | ExitState::AwaitingDelta(_) | ExitState::Claimable(_) | ExitState::ClaimInProgress(_)))
}

/// Candidate selection for read-only completion only. A caller must independently
/// revalidate the canonical transaction, exact owned user leaf and tip.
fn reconfirmed_claim_candidate(
	entry: &StoredExit, movement: &crate::movement::Movement, state: &VtxoState,
	amount: Amount, snapshot: &crate::fallback::payouts::FallbackSnapshot,
) -> bool {
	movement.status == MovementStatus::Pending && owned_exit_identity(entry, movement, state, amount, snapshot)
		&& (matches!(entry.state, ExitState::Claimed(_)) || resumed_claim_shape(entry))
}

fn completed_claim_recovery_lineage(entry: &StoredExit) -> bool {
	ExitStateKind::LIVE_STATES.contains(&entry.state.kind())
		&& entry.history.iter().any(|state| matches!(state, ExitState::Claimed(_)))
}

async fn exit_rebroadcast_allowed(exit: &ExitVtxo, wallet: &Wallet) -> anyhow::Result<bool> {
	if matches!(exit.state(), ExitState::Claimed(_)) && exit.history().iter().any(|state| matches!(state, ExitState::Claimed(_))) {
		return Ok(false);
	}
	if !completed_claim_recovery_lineage(&StoredExit::new(exit)) { return Ok(true); }
	// Exit.inner is already held. Match the admission lock order before
	// rechecking this persisted continuation, including after a cold load.
	let _history = wallet.lock_automatic_exit_ownership(exit.id()).await?;
	let persister = &*wallet.inner.db;
	let snapshot = persister.get_fallback_snapshot().await?;
	let coin = persister.get_wallet_vtxo(exit.id()).await?.context("exit replay source is missing")?;
	let Some(stored) = persister.get_exit_vtxo_entry(&exit.id()).await? else { return Ok(false); };
	if stored != StoredExit::new(exit) { return Ok(false); }
	let Some(id) = stored.movement_id else { return Ok(false); };
	let movement = persister.get_movement_by_id(id).await?;
	Ok(resumed_claim_owns_principal(&stored, &movement, &coin.state, coin.amount(), &snapshot))
}

fn ordinary_proof_blocks_cpfp(
	owners: impl IntoIterator<Item = VtxoId>, proven: &std::collections::BTreeSet<VtxoId>,
) -> bool {
	let mut spent_owner = false;
	for id in owners {
		if !proven.contains(&id) { return false; }
		spent_owner = true;
	}
	spent_owner
}

#[cfg(test)]
mod ordinary_proof_tests {
	use super::*;
	use crate::movement::{Movement, MovementSubsystem};
	use crate::persist::models::StoredExit;
	use bitcoin::hashes::Hash;

	#[tokio::test]
	async fn surviving_signed_prefix_search_is_bounded_and_preserves_lookup_errors() {
		for len in [0, 1, 2, 17, 65, 1024] {
			for prefix in 0..=len {
				let mut probes = 0;
				let found = confirmed_path_prefix_len(len, |index| {
					probes += 1;
					std::future::ready(Ok(index < prefix))
				}).await.unwrap();
				assert_eq!(found, prefix);
				assert!(probes <= usize::BITS - len.leading_zeros());
			}
		}
		let error = confirmed_path_prefix_len(17, |_| std::future::ready(
			Err(anyhow::anyhow!("backend unavailable")),
		)).await.unwrap_err();
		assert_eq!(error.to_string(), "backend unavailable", "failed lookup cannot become absence evidence");
	}

	#[test]
	fn recovery_frontier_binds_actual_signed_body_and_selected_path_output() {
		let vectors = &*ark::test_util::VTXO_VECTORS;
		let full = &vectors.arkoor3_vtxo;
		let funding = &vectors.round_tx;
		assert_eq!(full.chain_anchor().txid, funding.compute_txid());
		full.validate(funding).unwrap();
		let steps = full.transactions().collect::<Vec<_>>();
		assert!(steps.len() > 1);
		for (index, step) in steps.iter().enumerate() {
			let next = steps.get(index + 1).map(|next| {
				assert_eq!(next.tx.input.len(), 1);
				next.tx.input[0].previous_output
			}).unwrap_or(full.point());
			assert_eq!(signed_frontier_output(step, next, &step.tx).unwrap(), step.tx.output[step.output_idx]);
			let sibling = OutPoint::new(next.txid, next.vout ^ 1);
			assert!(signed_frontier_output(step, sibling, &step.tx).is_err(), "sibling output does not carry this signed path");
			let mut substituted = step.tx.clone();
			substituted.input[0].witness.push([1]);
			assert_eq!(substituted.compute_txid(), step.tx.compute_txid());
			assert!(signed_frontier_output(step, next, &substituted).is_err(), "same txid does not waive actual signed-body identity");
			let mut changed_output = step.tx.clone();
			changed_output.output[step.output_idx].value += Amount::from_sat(1);
			assert!(signed_frontier_output(step, next, &changed_output).is_err(), "actual amount or body substitution stays blocked");
			let mut invalid = step.clone();
			invalid.output_idx = invalid.tx.output.len();
			let missing = OutPoint::new(next.txid, u32::try_from(invalid.output_idx).unwrap());
			assert!(signed_frontier_output(&invalid, missing, &invalid.tx).is_err());
		}
	}

	#[test]
	fn stale_cpfp_is_blocked_without_starving_a_live_shared_ancestor_owner() {
		let spent = VtxoId::from_slice(&[1; 36]).unwrap();
		let healthy = VtxoId::from_slice(&[2; 36]).unwrap();
		let proven = [spent].into_iter().collect();
		assert!(ordinary_proof_blocks_cpfp([spent], &proven));
		assert!(!ordinary_proof_blocks_cpfp([spent, healthy], &proven));
		assert!(!ordinary_proof_blocks_cpfp([healthy, spent], &proven));
		assert!(!ordinary_proof_blocks_cpfp([], &proven));
	}

	#[test]
	fn ordinary_proof_retires_only_its_exact_abortable_nonbinding_safety_holder() {
		let id = VtxoId::from_slice(&[7; 36]).unwrap();
		let mut entry = StoredExit { vtxo_id: id, state: ExitState::new_start(BlockHeight::new(1)),
			history: vec![], movement_id: Some(MovementId(7)) };
		let mut movement = Movement::new(MovementId(7), MovementStatus::Pending,
			&MovementSubsystem { name: Subsystem::EXIT.as_name().into(), kind: "exit".into() }, chrono::Local::now());
		movement.input_vtxos.push(id);
		movement.metadata.insert("protective_exit".into(), serde_json::Value::Bool(true));
		let holder = movement.id.into();
		assert!(ordinary_proof_retires_protective_exit(&entry, &movement, &holder));
		assert!(!ordinary_proof_retires_protective_exit(&entry, &movement, &MovementId(8).into()));
		movement.metadata.insert("protective_round_state".into(), serde_json::json!(1));
		assert!(!ordinary_proof_retires_protective_exit(&entry, &movement, &holder));
		movement.metadata.remove("protective_round_state");
		movement.input_vtxos.push(VtxoId::from_slice(&[8; 36]).unwrap());
		assert!(!ordinary_proof_retires_protective_exit(&entry, &movement, &holder));
		movement.input_vtxos.pop();
		entry.state = ExitState::new_vtxo_already_spent(BlockHeight::new(1));
		assert!(!ordinary_proof_retires_protective_exit(&entry, &movement, &holder));
	}

	#[test]
	fn orphaned_claim_rearm_keeps_only_the_same_prior_owned_principal() {
		let id = VtxoId::from_slice(&[9; 36]).unwrap();
		let block = bitcoin_ext::BlockRef { height: BlockHeight::new(10), hash: bitcoin::BlockHash::all_zeros() };
		let entry = StoredExit { vtxo_id: id, state: ExitState::new_claimed(BlockHeight::new(15),
			bitcoin::Txid::all_zeros(), block), history: vec![], movement_id: Some(MovementId(9)) };
		let mut movement = Movement::new(MovementId(9), MovementStatus::Successful,
			&MovementSubsystem { name: Subsystem::EXIT.as_name().into(), kind: "exit".into() }, chrono::Local::now());
		movement.input_vtxos.push(id);
		movement.effective_balance = bitcoin::SignedAmount::from_sat(-100_000);
		movement.metadata.insert("protective_exit".into(), serde_json::Value::Bool(true));
		let mut snapshot = crate::fallback::payouts::FallbackSnapshot::default();
		let eligible = |movement: &Movement, state: &VtxoState, snapshot: &crate::fallback::payouts::FallbackSnapshot|
			orphaned_claim_owns_principal(&entry, movement, state, Amount::from_sat(100_000), snapshot);
		assert!(eligible(&movement, &VtxoState::Exited, &snapshot));
		// Crash after durable Pending but before the LIVE exit checkpoint must retry.
		movement.status = MovementStatus::Pending;
		assert!(eligible(&movement, &VtxoState::Exited, &snapshot));
		for status in [MovementStatus::Canceled, MovementStatus::Failed] {
			movement.status = status;
			assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		}
		movement.status = MovementStatus::Pending;
		movement.id = MovementId(10);
		assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		movement.id = MovementId(9);
		for state in [VtxoState::Spent, VtxoState::Spendable, VtxoState::Locked { holder: None },
			VtxoState::Locked { holder: Some(MovementId(8).into()) }] {
			assert!(!eligible(&movement, &state, &snapshot));
		}
		movement.metadata.insert("protective_round_state".into(), serde_json::json!(1));
		assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		movement.metadata.remove("protective_round_state");
		movement.effective_balance = bitcoin::SignedAmount::from_sat(-200_000);
		assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		movement.effective_balance = bitcoin::SignedAmount::from_sat(-100_000);
		movement.input_vtxos.push(VtxoId::from_slice(&[8; 36]).unwrap());
		assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		movement.input_vtxos.pop();
		snapshot.verified_ordinary_spends.insert(id);
		assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		snapshot.verified_ordinary_spends.clear();
		snapshot.unproven_spent_vtxos.insert(id);
		assert!(!eligible(&movement, &VtxoState::Exited, &snapshot));
		// The resumed native wait retains Exited rather than reacquiring an Ark
		// holder; ordinary Processing would attempt that protective lock again.
		let waiting = ExitState::new_awaiting_delta(BlockHeight::new(15), block, BlockDelta::new(3));
		assert!(waiting.warrants_exited_vtxo());
		assert!(ExitStateKind::LIVE_STATES.contains(&waiting.kind()));
		assert!(!waiting.is_cancelable());
	}

	#[test]
	fn completed_claim_processing_checkpoint_keeps_one_owned_debit_across_cold_retry() {
		let id = VtxoId::from_slice(&[11; 36]).unwrap();
		let tip = BlockHeight::new(25);
		let block = bitcoin_ext::BlockRef { height: BlockHeight::new(10), hash: bitcoin::BlockHash::all_zeros() };
		let claimed = ExitState::new_claimed(tip, bitcoin::Txid::all_zeros(), block);
		let mut entry = StoredExit { vtxo_id: id, state: claimed.clone(), history: vec![], movement_id: Some(MovementId(11)) };
		let mut movement = Movement::new(MovementId(11), MovementStatus::Pending,
			&MovementSubsystem { name: Subsystem::EXIT.as_name().into(), kind: "exit".into() }, chrono::Local::now());
		movement.input_vtxos.push(id);
		movement.effective_balance = bitcoin::SignedAmount::from_sat(-100_000);
		movement.metadata.insert("protective_exit".into(), serde_json::json!(true));
		let mut snapshot = crate::fallback::payouts::FallbackSnapshot::default();
		// Failure/crash between the two durable writes leaves the SAME Claimed
		// row and Pending movement eligible; no replacement movement is needed.
		let interrupted: StoredExit = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
		assert!(orphaned_claim_owns_principal(&interrupted, &movement, &VtxoState::Exited, Amount::from_sat(100_000), &snapshot));
		entry.history.push(claimed);
		entry.state = ExitState::new_processing(tip, [bitcoin::Txid::all_zeros()]);
		let mut reopened: StoredExit = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
		let eligible = |entry: &StoredExit, movement: &Movement, state: &VtxoState, snapshot: &crate::fallback::payouts::FallbackSnapshot|
			resumed_claim_owns_principal(entry, movement, state, Amount::from_sat(100_000), snapshot);
		assert!(completed_claim_recovery_lineage(&reopened));
		assert!(eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		for state in [ExitState::new_awaiting_delta(tip, block, BlockDelta::new(3)),
			ExitState::new_claimable(tip, block, None),
			ExitState::new_claim_in_progress(tip, block, bitcoin::Txid::all_zeros())] {
			reopened.state = state;
			assert!(completed_claim_recovery_lineage(&reopened));
			assert!(eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
			movement.status = MovementStatus::Canceled;
			assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
			movement.status = MovementStatus::Pending;
		}
		reopened.state = ExitState::new_start(tip);
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		reopened.state = ExitState::new_processing(tip, [bitcoin::Txid::all_zeros()]);
		assert_eq!(reopened.movement_id, Some(movement.id));
		assert_eq!(movement.effective_balance, bitcoin::SignedAmount::from_sat(-100_000));
		for state in [VtxoState::Spendable, VtxoState::Spent, VtxoState::Locked { holder: None },
			VtxoState::Locked { holder: Some(MovementId(12).into()) }] {
			assert!(!eligible(&reopened, &movement, &state, &snapshot));
		}
		for status in [MovementStatus::Successful, MovementStatus::Canceled, MovementStatus::Failed] {
			movement.status = status;
			assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		}
		movement.status = MovementStatus::Pending;
		movement.metadata.insert("protective_round_movement".into(), serde_json::json!(12));
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		movement.metadata.remove("protective_round_movement");
		movement.id = MovementId(12);
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		movement.id = MovementId(11);
		snapshot.verified_ordinary_spends.insert(id);
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		snapshot.verified_ordinary_spends.clear();
		snapshot.unproven_spent_vtxos.insert(id);
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		snapshot.unproven_spent_vtxos.clear();
		reopened.history.push(ExitState::new_canceled(tip));
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
		// Ordinary uncompleted AwaitingDelta -> Processing after a fork has no
		// prior Claimed checkpoint. It must not acquire this special gate.
		reopened.history = vec![ExitState::new_awaiting_delta(tip, block, BlockDelta::new(3))];
		assert!(!completed_claim_recovery_lineage(&reopened));
		assert!(!eligible(&reopened, &movement, &VtxoState::Exited, &snapshot));
	}


	#[test]
	fn reconfirmed_claim_lookup_keeps_exact_local_ownership_checks() {
		let id = VtxoId::from_slice(&[15; 36]).unwrap();
		let tip = BlockHeight::new(30);
		let block = bitcoin_ext::BlockRef { height: BlockHeight::new(20), hash: bitcoin::BlockHash::all_zeros() };
		let mut entry = StoredExit { vtxo_id: id, state: ExitState::new_claimable(tip, block, None),
			history: vec![ExitState::new_claimed(tip, bitcoin::Txid::from_byte_array([1; 32]), block)],
			movement_id: Some(MovementId(15)) };
		let mut movement = Movement::new(MovementId(15), MovementStatus::Pending,
			&MovementSubsystem { name: Subsystem::EXIT.as_name().into(), kind: "exit".into() }, chrono::Local::now());
		movement.input_vtxos.push(id);
		movement.effective_balance = bitcoin::SignedAmount::from_sat(-100_000);
		let mut snapshot = crate::fallback::payouts::FallbackSnapshot::default();
		let candidate = |entry: &StoredExit, movement: &Movement, state: &VtxoState, snapshot: &crate::fallback::payouts::FallbackSnapshot|
			reconfirmed_claim_candidate(entry, movement, state, Amount::from_sat(100_000), snapshot);
		assert!(candidate(&entry, &movement, &VtxoState::Exited, &snapshot),
			"a candidate still needs current raw/user-clause/canonical proof");
		entry.history.push(entry.state.clone());
		entry.state = ExitState::new_claimed(tip, bitcoin::Txid::from_byte_array([2; 32]), block);
		assert!(candidate(&entry, &movement, &VtxoState::Exited, &snapshot));
		for status in [MovementStatus::Successful, MovementStatus::Canceled, MovementStatus::Failed] {
			movement.status = status;
			assert!(!candidate(&entry, &movement, &VtxoState::Exited, &snapshot));
		}
		movement.status = MovementStatus::Pending;
		for state in [VtxoState::Spent, VtxoState::Spendable, VtxoState::Locked { holder: Some(MovementId(16).into()) }] {
			assert!(!candidate(&entry, &movement, &state, &snapshot));
		}
		movement.metadata.insert("protective_round_state".into(), serde_json::json!(1));
		assert!(!candidate(&entry, &movement, &VtxoState::Exited, &snapshot));
		movement.metadata.remove("protective_round_state");
		snapshot.verified_ordinary_spends.insert(id);
		assert!(!candidate(&entry, &movement, &VtxoState::Exited, &snapshot));
		snapshot.verified_ordinary_spends.clear();
		snapshot.unproven_spent_vtxos.insert(id);
		assert!(!candidate(&entry, &movement, &VtxoState::Exited, &snapshot));
	}

}
