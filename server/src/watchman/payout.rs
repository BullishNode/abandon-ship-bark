//! Immutable zero-fee sweep parents. Fee replacements affect only their child.

use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Amount, Transaction, TxOut};
use bitcoin_ext::{fee, TxOutExt};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use bitcoind_async_client::traits::Reader;
use bitcoin_ext::bdk::WalletExt;
use bitcoin_ext::cpfp::MakeCpfpFees;
use tracing::warn;

use crate::bitcoind as bcd;
use crate::database::expiry_chain::ExpiryViewToken;
use crate::database::expiry_payout::PendingExpiryPayout;
use crate::wallet::BdkWalletExt;

use ark::ServerVtxo;

use super::Watchman;

const MAX_PAYOUT_VSIZE: usize = 10_000;

impl Watchman {
	/// Build the complete group as one transaction; callers must never feed a
	/// committed group through process_claims, which retries individual inputs.
	pub(crate) async fn build_expiry_payout(
		&self,
		vtxos: &[ServerVtxo],
		payouts: &[TxOut],
	) -> anyhow::Result<Transaction> {
		ensure!(!vtxos.is_empty(), "expiry payout has no backing inputs");
		ensure!(vtxos.iter().map(|v| v.id()).collect::<BTreeSet<_>>().len() == vtxos.len(),
			"expiry payout repeats a backing input");
		for vtxo in vtxos {
			let output = self.bitcoind.try_get_tx_out(vtxo.point(), true).await?
				.context("expiry payout input is already consumed")?;
			ensure!(output.confirmations > 0 && output.value == vtxo.amount()
				&& output.script_pub_key.script().context("invalid backing script")? == vtxo.txout().script_pubkey,
				"expiry payout backing is not the exact confirmed output");
		}
		ensure!(payouts.iter().all(|out| out.is_standard()), "expiry payout contains nonstandard or dust payee output");
		let (inputs, clauses, input_amount, _) = self.claim_inputs(vtxos).await?;
		let paid = payouts.iter().try_fold(Amount::ZERO, |sum, out|
			sum.checked_add(out.value).context("expiry payout amount overflow"),
		)?;
		let remainder = input_amount.checked_sub(paid).context("expiry payout exceeds backing")?;
		let mut outputs = payouts.to_vec();
		if remainder >= self.drain_spk.minimal_non_dust() {
			outputs.push(TxOut { value: remainder, script_pubkey: self.drain_spk.clone() });
			outputs.push(fee::fee_anchor());
		} else {
			// One ephemeral dust output, consumed by the fee child, returns a
			// sub-dust operator remainder without a nonstandard drain output.
			outputs.push(fee::fee_anchor_with_amount(remainder));
		}
		let lock_time = self.sync_height().height.to_locktime().context("invalid expiry payout locktime")?;
		let tx = Transaction {
			version: bitcoin::transaction::Version::non_standard(3),
			lock_time, input: inputs, output: outputs,
		};
		let tx = self.sign_claim_inputs(vtxos, &clauses, tx).await?;
		ensure!(tx.vsize() <= MAX_PAYOUT_VSIZE, "expiry payout requires splitting: {} vbytes", tx.vsize());
		let outputs = tx.output.iter().try_fold(Amount::ZERO, |sum, out|
			sum.checked_add(out.value).context("expiry payout output overflow"),
		)?;
		ensure!(outputs == input_amount, "expiry sweep parent must have exactly zero fee");
		Ok(tx)
	}

	/// Publish only already committed groups. Parent bytes are durable before
	/// fee funding or broadcast, and every retry keeps those exact bytes.
	pub(crate) async fn process_expiry_payouts(&self, view: ExpiryViewToken) -> anyhow::Result<()> {
		let groups = self.db.read(async |t| t.pending_expiry_payouts().await).await?;
		for group in groups {
			let id = group.id;
			if let Err(error) = self.process_expiry_payout(view, group).await {
				warn!(allocation_id = id, %error, "Expiry payout remains pending");
			}
		}
		Ok(())
	}

	async fn process_expiry_payout(&self, view: ExpiryViewToken, group: PendingExpiryPayout) -> anyhow::Result<()> {
		let parent = match group.parent {
			Some(parent) => parent,
			None => self.build_expiry_payout(&group.inputs, &group.payouts).await?,
		};
		self.db.write(async |t| t.store_expiry_parent(view, group.id, &parent).await).await?;
		let mut wallet = self.watchman_wallet.lock().await;
		let target = self.fee_estimator.regular();
		let fees = match &group.fee_child {
			Some((child,fee)) if *fee >= (parent.weight() + child.weight()) * target => None,
			Some((_,fee)) => Some(MakeCpfpFees::Rbf {
				min_effective_fee_rate: target,
				current_package_fee: *fee,
			}),
			None => Some(MakeCpfpFees::Effective(target)),
		};
		let child = if let Some(fees) = fees {
			if !wallet.has_trusted_balance(self.config.min_cpfp_amount) {
				warn!(allocation_id = group.id, "Expiry payouts need confirmed usable watchman funds");
				return Ok(());
			}
			let child = wallet.make_signed_p2a_cpfp(&parent, fees)
				.context("funding expiry payout fee child")?;
			let mut input_value = Amount::ZERO;
			for input in &child.input {
				let value = if input.previous_output.txid == parent.compute_txid() {
					parent.output.get(input.previous_output.vout as usize).context("invalid expiry anchor index")?.value
				} else {
					let output = self.bitcoind.try_get_tx_out(input.previous_output, true).await?
						.context("expiry fee funding input is unavailable")?;
					ensure!(output.confirmations > 0, "expiry fee funding is not confirmed");
					output.value
				};
				input_value = input_value.checked_add(value).context("expiry fee input overflow")?;
			}
			let output_value = child.output.iter().try_fold(Amount::ZERO, |sum,o|
				sum.checked_add(o.value).context("expiry fee output overflow"))?;
			let fee = input_value.checked_sub(output_value).context("expiry fee child creates money")?;
			let accounting = super::settlement::payout_fee_accounting(&parent,group.payouts.len(),fee)?;
			if accounting.subsidy > self.config.expiry_fee_warning {
				warn!(allocation_id = group.id, proposed_subsidy_sat = accounting.subsidy.to_sat(),
					miner_fee_sat = fee.to_sat(), operator_remainder_sat = accounting.operator_remainder.to_sat(),
					net_operator_result_sat = accounting.net_operator_result.to_sat(),
					warning_sat = self.config.expiry_fee_warning.to_sat(),
					"Proposed emergency payout operator subsidy exceeds warning level");
			}
			self.db.write(async |t| t.store_expiry_fee_child(view, group.id, &parent, &child, fee).await).await?;
			child
		} else {
			group.fee_child.context("missing recorded expiry fee child")?.0
		};
		wallet.commit_tx(&child);
		wallet.persist().await?;
		drop(wallet);
		// Re-check the actual completed canonical hashes immediately before
		// publication. Hold the durable barrier shared through the RPC.
		self.verify_expiry_view(view).await?;
		self.db.write(async |t| {
			t.lock_valid_expiry_view(view).await?;
			bcd::submit_package(&self.bitcoind, &[parent.clone(), child.clone()]).await?;
			t.execute("UPDATE expiry_allocation SET broadcast_at=COALESCE(broadcast_at,NOW()) WHERE id=$1", &[&group.id]).await?;
			Ok(())
		}).await?;
		Ok(())
	}

	pub(crate) async fn verify_expiry_view(&self, view: ExpiryViewToken) -> anyhow::Result<()> {
		for point in [view.captaind, view.watchmand] {
			ensure!(self.bitcoind.get_block_hash(point.height.into()).await? == point.hash,
				"expiry completed sync view is no longer canonical");
		}
		Ok(())
	}

}
