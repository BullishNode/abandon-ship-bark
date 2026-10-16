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
		Ok(UnsignedFundingTx { psbt, txid, max_fee_rate: self.max_fee_rate, pinned_input, _extra_inputs })
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
}

impl UnsignedFundingTx {
	pub fn psbt(&self) -> &Psbt {
		&self.psbt
	}

	pub fn fee(&self) -> Amount {
		self.psbt.fee().expect("freshly built psbt has a valid fee")
	}

	pub fn sign(self, wallet: &mut PersistedWallet) -> anyhow::Result<SignedFundingTx> {
		let fee = self.fee();
		let tx = wallet.finish_tx_with_max_fee_rate(self.psbt().clone(), self.max_fee_rate)
			.context("failed to sign funding transaction")?;
		ensure!(tx.compute_txid() == self.txid, "signed funding body differs from fixed PSBT");
		Ok(SignedFundingTx {
			tx, psbt: self.psbt, txid: self.txid, fee,
			_pinned_input: self.pinned_input, _extra_inputs: self._extra_inputs,
		})
	}
}

pub struct SignedFundingTx {
	psbt: Psbt,
	pub tx: Transaction,
	pub txid: Txid,
	pub fee: Amount,
	/// Held only for its Drop impl: releases the locks on theinputs
	/// when this `SignedFundingTx` is dropped (e.g. on round restart).
	pub _pinned_input: WalletUtxoGuard,
	pub _extra_inputs: WalletUtxosGuard,
}






impl SignedFundingTx {
	pub fn unsigned_tx(&self) -> &Transaction { &self.psbt.unsigned_tx }
	pub fn txid(&self) -> Txid { self.txid }
	pub fn tree_outpoint(&self) -> OutPoint {
		OutPoint::new(self.txid, ROUND_TX_VTXO_TREE_VOUT)
	}
	pub fn into_pinned_input(self) -> WalletUtxoGuard { self._pinned_input }
	pub fn validate_current_recovery(&self, wallet: &PersistedWallet) -> anyhow::Result<()> {
		ensure!(self.tx.compute_txid() == self.psbt.unsigned_tx.compute_txid(),
			"retained signed funding differs from fixed PSBT");
		wallet.validate_private_funding_psbt(&self.psbt)
	}
}
