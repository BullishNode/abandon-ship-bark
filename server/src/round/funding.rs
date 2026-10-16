//! Construction of a round's funding transaction.
//!
//! When a round needs multiple attempts, every attempt's funding tx must
//! share at least one input — the *pinned input* — with the previous
//! attempt. Once the last attempt confirms, clients who signed forfeits
//! on previous attempts have proof that those stale attempts can no
//! longer go on-chain.
//!
//! [`FundingTx`] keeps locks on every input it picked: the pinned input
//! is held across attempts, the extras are released when the
//! [`FundingTx`] is dropped.

use anyhow::Context;
use bdk_wallet::coin_selection::DefaultCoinSelectionAlgorithm;
use bitcoin::{Amount, FeeRate, OutPoint, Psbt, Transaction, TxOut, Txid};

use ark::rounds::ROUND_TX_VTXO_TREE_VOUT;
use bitcoin_ext::bdk::{PreferConfirmedCoinSelection, WithGuaranteedChange};

use crate::utils::InstrumentedLock;
use crate::wallet::{PersistedWallet, WalletUtxoGuard, WalletUtxosGuard};

/// Inputs needed to build a funding tx. Pass to [`FundingTxSpec::build`]
/// to produce a [`FundingTx`].
pub struct FundingTxSpec {
	/// The output in which the vtxo tree will be rooted.
	pub tree_output: TxOut,
	/// Feerate for the funding tx.
	pub fee_rate: FeeRate,
	/// Existing configured ceiling, applied to the actual signed funding tx.
	pub max_fee_rate: Option<FeeRate>,
	/// If `Some`, the wallet is forced to include this input in the new tx.
	/// `None` on the first attempt.
	pub pinned_input: Option<WalletUtxoGuard>,
}

impl FundingTxSpec {
	/// Build the funding tx, offloading the blocking work to the tokio
	/// blocking pool. Takes the wallet lock internally and releases it
	/// before it returns.
	pub async fn build(
		self,
		wallet: &InstrumentedLock<PersistedWallet>,
	) -> anyhow::Result<UnsignedFundingTx> {
		let (_, funding_tx) = wallet.build_blocking(move |w| self.compute_build(w)).await?;
		Ok(funding_tx)
	}

	/// Build the funding tx synchronously. Blocks the calling thread on
	/// bdk's coin selection; in an async context prefer
	/// [`FundingTxSpec::build`].
	fn compute_build(self, wallet: &mut PersistedWallet) -> anyhow::Result<UnsignedFundingTx> {
		let start = std::time::Instant::now();

		// Confirmed inputs first: they cost no extra fee.
		let selection = WithGuaranteedChange(
			PreferConfirmedCoinSelection(DefaultCoinSelectionAlgorithm::default()),
		);
		let psbt = wallet.build_quiet_tx_at_chunk_feerate(selection, self.fee_rate, self.max_fee_rate, |b| {
			// `Untouched` keeps insertion order: `tree_output` is added first, so
			// it lands at vout `ROUND_TX_VTXO_TREE_VOUT` (0), as the round tx requires.
			b.ordering(bdk_wallet::TxOrdering::Untouched);
			// NB: manual selection overrides the wallet's unspendable outputs
			if let Some(ref pinned) = self.pinned_input {
				b.add_utxo(pinned.utxo()).context("pinned input not in wallet")?;
			}
			b.add_recipient(self.tree_output.script_pubkey.clone(), self.tree_output.value);
			Ok(())
		}).context("failed to build funding tx")?;

		// Determine the pinned input and collect the extras.
		let (pinned_input, extra_outpoints) = match self.pinned_input {
			Some(pinned) => {
				let extras = psbt.unsigned_tx.input.iter()
					.map(|i| i.previous_output)
					.filter(|o| *o != pinned.utxo())
					.collect::<Vec<_>>();
				(pinned, extras)
			},
			None => {
				let mut iter = psbt.unsigned_tx.input.iter().map(|i| i.previous_output);
				let pinned = iter.next()
					.expect("funded tx should have at least one input");
				let pinned = wallet.lock_wallet_utxo(pinned)
					.context("failed to lock pinned input")?;
				(pinned, iter.collect::<Vec<_>>())
			},
		};

		let _extra_inputs = wallet.lock_wallet_utxos(extra_outpoints)
			.context("failed to lock extra inputs")?;

		let txid = psbt.unsigned_tx.compute_txid();
		let total_output_amount = psbt.unsigned_tx.output.iter()
			.map(|o| o.value)
			.sum();
		slog!(RoundFundingTxBuilt,
			total_output_amount,
			pinned_input: pinned_input.utxo(),
			build_time: start.elapsed(),
		);
		Ok(UnsignedFundingTx { psbt, txid, max_fee_rate: self.max_fee_rate, pinned_input, _extra_inputs,
			prepared: None, native_permit: None })
	}
}


/// A round's funding transaction with the lock guards on every input it
/// consumes. Built by [`FundingTxSpec::build`].
pub struct UnsignedFundingTx {
	psbt: Psbt,
	max_fee_rate: Option<FeeRate>,
	txid: Txid,
	pinned_input: WalletUtxoGuard,
	/// Held only for its Drop impl: releases the locks on the extra inputs
	/// when this `UnsignedFundingTx` is dropped (e.g. on round restart).
	_extra_inputs: WalletUtxosGuard,
	prepared: Option<Transaction>,
	native_permit: Option<crate::watchman::ordinary_fee_publication::PublicationRecord>,
}

impl UnsignedFundingTx {
	pub(crate) fn prepare_private(&mut self, wallet: &mut PersistedWallet) -> anyhow::Result<Transaction> {
		if let Some(original) = &self.prepared { return Ok(original.clone()); }
		let signed = wallet.finish_tx_private_with_max_fee_rate(self.psbt.clone(), self.max_fee_rate)?;
		ensure!(signed.compute_txid() == self.txid, "private funding signature changed original txid");
		wallet.retain_native_candidate_inputs(&signed)?;
		self.prepared = Some(signed.clone());
		Ok(signed)
	}
	pub(crate) fn bind_native_permit(&mut self, permit: crate::watchman::ordinary_fee_publication::PublicationRecord) -> anyhow::Result<()> {
		permit.matches(self.prepared.as_ref().context("native funding candidate was not privately materialized")?)?;
		ensure!(self.native_permit.as_ref().is_none_or(|old| *old == permit), "native funding permit changed");
		self.native_permit = Some(permit);
		Ok(())
	}
	pub(crate) fn prepared(&self) -> Option<&Transaction> { self.prepared.as_ref() }
	pub(crate) fn native_permit(&self) -> Option<&crate::watchman::ordinary_fee_publication::PublicationRecord> {
		self.native_permit.as_ref()
	}
	pub fn psbt(&self) -> &Psbt {
		&self.psbt
	}

	/// The unsigned transaction.
	pub fn unsigned_tx(&self) -> &Transaction {
		&self.psbt.unsigned_tx
	}

	pub fn txid(&self) -> Txid {
		self.txid
	}

	/// The outpoint in which the vtxo tree is rooted (vout
	/// [`ROUND_TX_VTXO_TREE_VOUT`] of the funding tx).
	pub fn tree_outpoint(&self) -> OutPoint {
		OutPoint::new(self.txid, ROUND_TX_VTXO_TREE_VOUT)
	}

	pub fn fee(&self) -> Amount {
		self.psbt.fee().expect("freshly built psbt has a valid fee")
	}

	/// Consume `self`, releasing the lock on every extra input and returning
	/// the guard for the pinned one. Used between round attempts: we keep
	/// the pinned input locked, but free the rest so the wallet can pick
	/// again on the next attempt.
	pub fn into_pinned_input(self) -> WalletUtxoGuard {
		self.pinned_input
	}

	pub fn sign(mut self, wallet: &mut PersistedWallet) -> anyhow::Result<SignedFundingTx> {
		let signed = if let Some(original) = self.prepared.take() {
			self.native_permit.as_ref().context("private native candidate lacks its precommit permit")?.matches(&original)?;
			// The caller rechecks the original owner/DB permit before entering
			// this step. No signature is recomputed and no fee debit is renewed.
			wallet.log_signed_funding_tx(&original, self.fee());
			Ok(original)
		} else { wallet.finish_tx_with_max_fee_rate(self.psbt().clone(), self.max_fee_rate) };
		match signed {
			Ok(tx) => Ok(SignedFundingTx {
				tx,
				txid: self.txid,
				fee: self.fee(),
				_pinned_input: self.pinned_input,
				_extra_inputs: self._extra_inputs,
			}),
			Err(e) => bail!("Failed to sign funding transaction: {:?}", e),
		}
	}
}

pub struct SignedFundingTx {
	pub tx: Transaction,
	pub txid: Txid,
	pub fee: Amount,
	/// Held only for its Drop impl: releases the locks on theinputs
	/// when this `SignedFundingTx` is dropped (e.g. on round restart).
	pub _pinned_input: WalletUtxoGuard,
	pub _extra_inputs: WalletUtxosGuard,
}





