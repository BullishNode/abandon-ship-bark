//! Immutable zero-fee sweep parents. Fee replacements affect only their child.

use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Transaction, TxIn, TxOut, Txid, ScriptBuf, Sequence, Witness, sighash};
use bitcoin_ext::TxStatus;
use bitcoin_ext::{fee, TransactionExt, TxOutExt};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use bitcoind_async_client::traits::Reader;
use bitcoin_ext::bdk::WalletExt;
use bitcoin_ext::cpfp::MakeCpfpFees;
use tracing::warn;

use crate::bitcoind as bcd;
use crate::bitcoind::BitcoindErrorExt;
use crate::database::expiry_chain::ExpiryViewToken;
use crate::database::expiry_payout::{MAX_PAYOUT_VSIZE, PendingExpiryPayout};
use crate::wallet::BdkWalletExt;

use ark::{ServerVtxo, ServerVtxoPolicy};
use ark::vtxo::policy::signing::VtxoSigner;
use ark::vtxo::policy::clause::VtxoClause;

use super::Watchman;

/// The fee of the transaction being replaced, not a stale historical
/// child or its entire mempool chunk.
async fn mempool_child_fee(client: &bitcoind_async_client::Client, txid: Txid) -> anyhow::Result<Option<Amount>> {
	#[derive(Debug, serde::Deserialize)]
	struct Entry {
		fees: Fees
	}
	#[derive(Debug, serde::Deserialize)]
	struct Fees {
		#[serde(with = "bitcoin::amount::serde::as_btc")]
		base: Amount
	}
	let entry: Entry = match client.call_raw("getmempoolentry", &[bcd::json_arg(txid)?]).await {
		Ok(entry) => entry,
		Err(error) if error.is_not_found() => return Ok(None),
		Err(error) => return Err(error.into()),
	};
	Ok(Some(entry.fees.base))
}

impl Watchman {
	async fn payout_inputs(
		&self, vtxos: &[ServerVtxo],
	) -> anyhow::Result<(Vec<TxIn>, Vec<Option<VtxoClause>>, Amount, usize)> {
		let mut inputs = Vec::with_capacity(vtxos.len());
		let mut clauses = Vec::with_capacity(vtxos.len());
		let mut total_input_weight = 0;
		let mut total_input_amount = Amount::ZERO;

		for vtxo in vtxos {
			//TODO(stevenroose) try remove this special case
			if *vtxo.policy() == ServerVtxoPolicy::ServerOwned {
				let input = TxIn {
					previous_output: vtxo.point(),
					script_sig: ScriptBuf::new(),
					sequence: Sequence::ZERO,
					witness: Witness::new(),
				};

				// TxIn base weight (non-witness) + witness weight
				total_input_weight += 4 * input.base_size() + (1 + 1 + 64); // nb items + sig
				total_input_amount = total_input_amount.checked_add(vtxo.amount()).context("claim input amount overflow")?;

				inputs.push(input);
				clauses.push(None);
			} else {
				let clause = self.signer.find_signable_clause(vtxo).await
					.context(vtxo.id())
					.context("no signable clause for vtxo")?;

				let input = TxIn {
					previous_output: vtxo.point(),
					script_sig: ScriptBuf::new(),
					sequence: clause.sequence().unwrap_or(Sequence::ZERO),
					witness: Witness::new(),
				};

				// TxIn base weight (non-witness) + witness weight
				total_input_weight += 4 * input.base_size() + clause.witness_size(vtxo);
				total_input_amount = total_input_amount.checked_add(vtxo.amount()).context("claim input amount overflow")?;

				inputs.push(input);
				clauses.push(Some(clause));
			}

			slog!(PreparingVtxoClaim, vtxo_id: vtxo.id(), policy: vtxo.policy().policy_type(),
				value: vtxo.amount(),
			);
		}

		Ok((inputs, clauses, total_input_amount, total_input_weight))
	}
	async fn sign_payout_inputs(
		&self, vtxos: &[ServerVtxo], clauses: &[Option<VtxoClause>], mut tx: Transaction,
	) -> anyhow::Result<Transaction> {
		// Sign all inputs
		let prevouts = vtxos.iter().map(|vtxo| vtxo.txout()).collect::<Vec<_>>();
		let prevouts = sighash::Prevouts::All(&prevouts);

		let mut sighash_cache = sighash::SighashCache::new(&mut tx);
		for (input_idx, (vtxo, clause)) in vtxos.iter().zip(clauses).enumerate() {
			if let Some(clause) = clause {
				let witness = self.signer.sign_input_with_clause(
					vtxo, clause, input_idx, &mut sighash_cache, &prevouts,
				).await.with_context(|| format!(
					"failed to sign script-spend input {} of tx {}",
					input_idx, sighash_cache.transaction().compute_txid(),
				))?;
				*sighash_cache.witness_mut(input_idx).unwrap() = witness;
			} else {
				// keyspend
				let witness = self.signer.sign_input_with_keyspend(
					vtxo, input_idx, &mut sighash_cache, &prevouts,
				).await.with_context(|| format!(
					"failed to sign key-spend input {} of tx {}",
					input_idx, sighash_cache.transaction().compute_txid(),
				))?;
				*sighash_cache.witness_mut(input_idx).unwrap() = witness;
			}
		}

		Ok(tx)
	}

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
		let (inputs, clauses, input_amount, _) = self.payout_inputs(vtxos).await?;
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
		let tx = self.sign_payout_inputs(vtxos, &clauses, tx).await?;
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
				warn!(allocation_id = id, error = %format_args!("{error:#}"), "Expiry payout remains pending");
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
		let (anchor, _) = parent.fee_anchor().context("expiry parent has no fee anchor")?;
		let active_child = bcd::get_mempool_spending_tx(&self.bitcoind, anchor).await?;
		let active_fee = match active_child {
			Some(txid) => Some(mempool_child_fee(&self.bitcoind, txid).await?
				.context("active expiry fee child changed during observation")?),
			None => None,
		};
		let active_record = match active_child {
			Some(txid) => {
				let saved = self.db.read(async |t| Ok(t.query_opt("
					SELECT raw_tx, fee_sat
					FROM expiry_fee_child
					WHERE allocation_id = $1
					AND txid = $2
				",
					&[&group.id, &txid.to_string()]).await?)).await?;
				let fee = active_fee.context("active expiry fee child has no observed fee")?;
				let child = if let Some(row) = saved {
					let child: Transaction = bitcoin::consensus::deserialize(row.get("raw_tx"))?;
					ensure!(child.compute_txid() == txid && child.version.0 == 3
						&& child.input.iter().filter(|input| input.previous_output == anchor).count() == 1,
						"active expiry fee child differs from retained parent history");
					ensure!(fee.to_sat() == u64::try_from(row.get::<_, i64>("fee_sat"))?,
						"active expiry fee differs from retained economics");
					Some(child)
				} else {
					let child = self.bitcoind.get_raw_transaction_verbosity_zero(&txid).await?.0;
					ensure!(child.compute_txid() == txid, "observed fee child body has wrong id");
					let prevouts = self.observed_fee_prevouts(view, &parent, &child).await?;
					let observed = super::fee_sponsor::recognize_fee_only(&parent, group.payouts.len(), &child, &prevouts,
						|script| script == self.drain_spk.as_script() || wallet.is_mine(script.to_owned()));
					ensure!(mempool_child_fee(&self.bitcoind, txid).await? == Some(fee)
						&& bcd::get_mempool_spending_tx(&self.bitcoind, anchor).await? == Some(txid),
						"observed fee child presence or fee changed");
					match observed {
						Ok(observed) => {
							ensure!(observed.fee == fee, "observed fee child economics differ from Core");
							self.db.write(async |t| t.store_observed_expiry_fee_child(view, group.id, &parent, &observed).await).await?;
							Some(child)
						},
						Err(error) => {
							// An unrecognized anchor spender is a replacement target,
							// never proof of our own funding or settlement. Keep
							// Core's fee for the existing RBF path below.
							warn!(allocation_id = group.id, %txid, %error,
								"Replacing unrecognized expiry fee child");
							None
						},
					}
				};
				child.map(|child| (child, fee))
			},
			None => None,
		};
		// A recognized older child or independent fee-only sponsor can already
		// provide the package's fee. Preserve its signed body unchanged.
		let reusable = active_record.or(group.fee_child);
		let fees = if let Some((child, fee)) = &reusable {
			// A side-chain getrawtransaction result is not mempool membership.
			// Only a real canonical confirmation or gettxspendingprevout entry
			// establishes that the child's own spends explain missing funding.
			if matches!(bcd::tx_status(&self.bitcoind, child.compute_txid()).await?, TxStatus::Confirmed(_)) {
				return Ok(());
			}
			let is_active = active_child == Some(child.compute_txid());
			let funding_usable = if is_active {
				true
			} else {
				let mut usable = true;
				for input in child.input.iter().filter(|input| input.previous_output.txid != parent.compute_txid()) {
					usable &= self.bitcoind.try_get_tx_out(input.previous_output, true).await?
						.is_some_and(|output| output.confirmations > 0);
				}
				usable
			};
			if funding_usable && (active_child.is_none() || is_active)
				&& *fee >= (parent.weight() + child.weight()) * target
			{
				None
			} else if let Some(current_package_fee) = active_fee {
				Some(MakeCpfpFees::Rbf { min_effective_fee_rate: target, current_package_fee })
			} else {
				Some(MakeCpfpFees::Effective(target))
			}
		} else if let Some(current_package_fee) = active_fee {
			Some(MakeCpfpFees::Rbf { min_effective_fee_rate: target, current_package_fee })
		} else {
			Some(MakeCpfpFees::Effective(target))
		};
		let child = if let Some(fees) = fees {
			if !wallet.has_trusted_balance(self.config.min_cpfp_amount) {
				warn!(allocation_id = group.id, "Expiry payouts need confirmed usable watchman funds");
				return Ok(());
			}
			// BDK's retained graph can lag a fork. Constrain selection to
			// confirmed, unspent Core outputs before signing any replacement.
			let candidates = wallet.list_unspent().map(|utxo| utxo.outpoint).collect::<Vec<OutPoint>>();
			let mut excluded = Vec::new();
			for point in candidates {
				if !self.bitcoind.try_get_tx_out(point, true).await?
					.is_some_and(|output| output.confirmations > 0)
				{
					excluded.push(point);
				}
			}
			let child = wallet.make_signed_p2a_cpfp_excluding(&parent, fees, &excluded)
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
			let output_value = child.output.iter().try_fold(Amount::ZERO, |sum, o|
				sum.checked_add(o.value).context("expiry fee output overflow"))?;
			let fee = input_value.checked_sub(output_value).context("expiry fee child creates money")?;
			let accounting = super::settlement::payout_fee_accounting(&parent, group.payouts.len(), fee)?;
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
			reusable.context("missing recorded expiry fee child")?.0
		};
		let own = self.db.read(async |t| Ok(t.query_one("
			SELECT funding_kind
			FROM expiry_fee_child
			WHERE allocation_id = $1
			AND txid = $2
		",
			&[&group.id, &child.compute_txid().to_string()]).await?.get::<_, String>(0) == "watchman")).await?;
		if own {
			wallet.commit_tx(&child);
			wallet.persist().await?;
		}
		drop(wallet);
		// Re-check the completed canonical hashes immediately before
		// publication. Hold the durable barrier shared through the RPC.
		self.verify_expiry_view(view).await?;
		self.db.write(async |t| {
			t.lock_valid_expiry_view(view).await?;
			let row = t.query_one("
				SELECT status, sweep_tx
				FROM expiry_allocation
				WHERE id = $1
				FOR UPDATE
			", &[&group.id]).await?;
			ensure!(row.get::<_, &str>("status") == "pending"
				&& row.get::<_, Option<&[u8]>>("sweep_tx") == Some(bitcoin::consensus::serialize(&parent).as_slice()),
				"expiry allocation changed before package publication");
			let result = bcd::submit_package(&self.bitcoind, &[parent.clone(), child.clone()]).await?;
			bcd::require_package_acceptance(&result, &[&parent, &child])?;
			t.execute("
				UPDATE expiry_allocation
				SET broadcast_at = COALESCE(broadcast_at, NOW())
				WHERE id = $1
			", &[&group.id]).await?;
			Ok(())
		}).await?;
		Ok(())
	}

	/// Use creating transactions and their real active-chain confirmations:
	/// a confirmed child has already consumed its prevouts, so gettxout=false
	/// alone cannot reconstruct this proof. The parent is supplied exactly.
	pub(super) async fn observed_fee_prevouts(
		&self, view: ExpiryViewToken, parent: &Transaction, child: &Transaction,
	) -> anyhow::Result<Vec<TxOut>> {
		let mut prevouts = Vec::with_capacity(child.input.len());
		for input in &child.input {
			let point = input.previous_output;
			let previous = if point.txid == parent.compute_txid() {
				parent.output.get(point.vout as usize).cloned().context("invalid observed parent prevout")?
			} else {
				let TxStatus::Confirmed(at) = bcd::tx_status(&self.bitcoind, point.txid).await? else {
					bail!("observed fee child funding has no canonical confirmation");
				};
				ensure!(at.height <= view.captaind.height.min(view.watchmand.height)
					&& self.bitcoind.get_block_hash(at.height.into()).await? == at.hash,
					"observed fee funding is outside completed canonical view");
				let previous = self.bitcoind.get_raw_transaction_verbosity_zero(&point.txid).await?.0;
				ensure!(previous.compute_txid() == point.txid, "observed funding transaction has wrong id");
				previous.output.get(point.vout as usize).cloned().context("invalid observed funding prevout")?
			};
			prevouts.push(previous);
		}
		Ok(prevouts)
	}

	pub(crate) async fn verify_expiry_view(&self, view: ExpiryViewToken) -> anyhow::Result<()> {
		for point in [view.captaind, view.watchmand] {
			ensure!(self.bitcoind.get_block_hash(point.height.into()).await? == point.hash,
				"expiry completed sync view is no longer canonical");
		}
		Ok(())
	}

}
