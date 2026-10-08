//! Operator-initiated, recipient-funded payments using the native wallet and nursery.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::fs::{self, File};
use std::io::Write;
use std::str::FromStr;

use anyhow::Context;
use ark::{ProtocolEncoding, Vtxo, VtxoId};
use bdk_wallet::coin_selection::LargestFirstCoinSelection;
use bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, Transaction, Txid, Weight};
use bitcoind_async_client::traits::Reader;
use serde::{Deserialize, Serialize};
use bitcoin_ext::bdk::{WalletExt, WithGuaranteedChange};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use tracing::{info, warn};

use crate::database::expiry_settlement::payout_script;
use crate::nursery::NurseryTxKind;
use crate::wallet::{BdkWalletExt, WalletKind};
use crate::Server;

fn deferred(status: Status, reason: impl ToString) -> Payment {
	Payment { status, reason: reason.to_string(), ..Default::default() }
}

/// Largest remainders, tied by sorted coin ID, keep all integer shares within
/// one sat of proportional allocation and make their sum equal the actual fee.
fn fee_shares(amounts: &[u64], fee: u64) -> anyhow::Result<Vec<u64>> {
	let total: u64 = amounts.iter().try_fold(0u64, |a,b| a.checked_add(*b)).context("gross overflow")?;
	ensure!(total > fee, "payout does not cover its fee");
	let mut shares: Vec<u64> = amounts.iter().map(|a| (u128::from(*a)*u128::from(fee)/u128::from(total)) as u64).collect();
	let mut order: Vec<_> = amounts.iter().enumerate().map(|(i,a)|
		(i, u128::from(*a)*u128::from(fee)%u128::from(total))).collect();
	order.sort_by_key(|(i,rem)| (std::cmp::Reverse(*rem),*i));
	let left = fee - shares.iter().sum::<u64>();
	for (i, _) in order.into_iter().take(left as usize) { shares[i] += 1; }
	Ok(shares)
}

impl Server {
	async fn claim_and_pay(&self, ids: Vec<VtxoId>, fee_rate: FeeRate) -> anyhow::Result<Payment> {
		let cfg = self.config.expiry_payout.clone();
		ensure!((1..=100).contains(&ids.len()), "expected 1..100 expiry coins");
		let keys: Vec<String> = ids.iter().map(ToString::to_string).collect();
		ensure!(keys.iter().collect::<BTreeSet<_>>().len() == keys.len(), "duplicate expiry coin ID");
		let Ok(_flux) = self.vtxos_in_flux.try_lock(&ids) else { return Ok(deferred(Status::Busy, "coin in flux")) };
		if let Some(payment) = self.db.read(async |t| t.expiry_replay(&keys).await).await? { return Ok(payment) }
		let tip = self.chain_tip().height.to_u32();
		let coins = self.db.read(async |t| t.expiry_inputs(&keys, tip, cfg.grace_blocks, cfg.min_payout_sat).await).await?;
		if coins.len() != ids.len() { return Ok(deferred(Status::Ineligible, "coin is spent, exited, too small, in grace or participating")) }
		let mut expected = BTreeMap::<ScriptBuf,u64>::new();
		for v in &coins { *expected.entry(payout_script(v)).or_default() += v.amount().to_sat(); }
		let pct = u64::from(cfg.max_fee_pct);
		let expected_build = expected.clone();
		let amounts: Vec<_> = coins.iter().map(|v| v.amount().to_sat()).collect();
		let scripts: Vec<_> = coins.iter().map(payout_script).collect();
		let built = self.rounds_wallet.build_blocking(move |wallet| {
			// Confirmed funds avoid charging recipients for unrelated ancestor fees.
			let unconfirmed: Vec<_> = wallet.list_unspent().filter(|u| !u.chain_position.is_confirmed()).map(|u| u.outpoint).collect();
			let configure = |b: &mut bdk_wallet::TxBuilder<'_, _>| {
				b.ordering(bdk_wallet::TxOrdering::Untouched);
				for op in &unconfirmed { b.add_unspendable(*op); }
				for (spk, amount) in &expected_build { b.add_recipient(spk.clone(), Amount::from_sat(*amount)); }
				Ok(())
			};
			// Select the gross debit with zero initial fee, then deduct the final
			// exact fee from recipients. This also supports an exactly-funded wallet.
			let mut psbt = wallet.build_tx_at_chunk_feerate(LargestFirstCoinSelection, FeeRate::ZERO, configure)?;
			if psbt.fee()? != Amount::ZERO {
				wallet.mark_output_keys_unused(&psbt.unsigned_tx);
				psbt = wallet.build_tx_at_chunk_feerate(WithGuaranteedChange(LargestFirstCoinSelection), FeeRate::ZERO, |b| {
					b.ordering(bdk_wallet::TxOrdering::Untouched);
					for op in &unconfirmed { b.add_unspendable(*op); }
					for (spk, amount) in &expected_build { b.add_recipient(spk.clone(), Amount::from_sat(*amount)); }
					Ok(())
				})?;
			}
			ensure!(psbt.fee()? == Amount::ZERO, "change must not charge an operator fee");
			let unused = psbt.unsigned_tx.clone();
			let result = (|| {
				let weight = psbt.unsigned_tx.weight() + Weight::from_wu(2 + 66 * psbt.inputs.len() as u64);
				ensure!(weight.to_wu() <= 400_000, "payout exceeds maximum transaction weight");
				let fee = fee_rate.fee_wu(weight).context("fee overflow")?.to_sat();
				let shares = fee_shares(&amounts, fee)?;
				let mut per_script = BTreeMap::<ScriptBuf,u64>::new();
				for ((amount, share), spk) in amounts.iter().zip(&shares).zip(&scripts) {
					ensure!(u128::from(*share)*100 <= u128::from(*amount)*u128::from(pct)
						&& share < amount, "actual per-coin fee exceeds cap");
					*per_script.entry(spk.clone()).or_default() += share;
				}
				for out in &mut psbt.unsigned_tx.output {
					if let Some(share) = per_script.remove(&out.script_pubkey) {
						out.value -= Amount::from_sat(share);
						ensure!(out.value >= out.script_pubkey.minimal_non_dust(), "recipient output would be dust");
					}
				}
				ensure!(per_script.is_empty() && psbt.fee()?.to_sat() == fee, "payout deductions do not equal its full mining fee");
				let tx = wallet.finish_tx(psbt)?;
				ensure!(tx.weight() == weight, "unexpected signed payout weight");
				Ok((tx, fee))
			})();
			if result.is_err() { wallet.mark_output_keys_unused(&unused); }
			result
		}).await;
		let (mut wallet, (tx, fee)) = match built {
			Ok(b) => b,
			Err(e) => return Ok(deferred(Status::Busy, format!("payout funding/fee deferred: {e:#}"))),
		};
		let accepted = crate::bitcoind::test_mempool_accept(&self.bitcoind, &[&tx]).await?;
		if !accepted[0].allowed {
			wallet.mark_output_keys_unused(&tx);
			return Ok(deferred(Status::Busy, format!("payout not accepted: {:?}", accepted[0].reject_reason)));
		}
		// The raw transaction alone cannot recover a change key beyond the
		// restored wallet's lookahead. Commit its derivation metadata with it.
		let wallet_metadata = wallet.staged().cloned();
		let target = self.nursery_confirm_target();
		let db = self.db.clone();
		let nursery = self.tx_nursery.clone();
		let flux = _flux.into_owned();
		let worker = self.rtmgr.spawn("ExpiryPayoutCommit");
		// Cancelling a tick must not drop its locks while COMMIT can still
		// succeed. The short durable handoff finishes even if its caller leaves.
		tokio::spawn(async move {
			let (_flux, _worker) = (flux, worker);
			let stored = db.write(async |t| {
				if let Some(change) = &wallet_metadata {
					t.store_changeset(WalletKind::Rounds, change).await?;
				}
				t.store_expiry_payment(&keys, &tx, fee, tip,
					cfg.grace_blocks, cfg.min_payout_sat, target).await
			}).await;
			if let Err(error) = stored {
				loop {
					// Wait for the original transaction's row locks before reading
					// its result: a lost COMMIT response alone does not mean rollback.
					let outcome = db.write(async |t| {
						t.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) FOR UPDATE", &[&keys]).await?;
						t.expiry_replay(&keys).await
					}).await;
					match outcome {
						Ok(None) => { wallet.mark_output_keys_unused(&tx); return Err(error); },
						Ok(Some(_)) => break,
						Err(e) => {
							warn!("expiry commit outcome unavailable; retaining wallet inputs until database returns: {e:#}");
							tokio::time::sleep(std::time::Duration::from_secs(1)).await;
						},
					}
				}
			}
			wallet.take_staged();
			wallet.commit_tx(&tx);
			if let Err(e) = wallet.persist().await { warn!("expiry wallet persist deferred to restart: {e:#}"); }
			drop(wallet);
			nursery.broadcast_tx(tx.clone(), NurseryTxKind::ExpiryPayout, target).await?;
			db.read(async |t| t.expiry_receipt(&tx.compute_txid().to_string()).await).await
		}).await?

	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn config_preserves_mainnet_floors_and_explicit_test_timing() {
		let mut cfg = Config::default();
		assert!(!cfg.enabled);
		cfg.validate(bitcoin::Network::Bitcoin).unwrap();
		cfg.enabled = true;
		cfg.validate(bitcoin::Network::Bitcoin).unwrap();
		cfg.grace_blocks = 0;
		cfg.sweep_min_confs = 1;
		assert!(cfg.validate(bitcoin::Network::Bitcoin).is_err());
		cfg.validate(bitcoin::Network::Regtest).unwrap();
		cfg.interval = Duration::ZERO;
		assert!(cfg.validate(bitcoin::Network::Regtest).is_err());
		cfg.interval = Duration::from_secs(1);
		cfg.max_batch = 101;
		assert!(cfg.validate(bitcoin::Network::Regtest).is_err());
		cfg.max_batch = 1;
		cfg.max_fee_pct = 100;
		assert!(cfg.validate(bitcoin::Network::Regtest).is_err());
		cfg.max_fee_pct = 20;
		cfg.conf_target_blocks = 2;
		assert!(cfg.validate(bitcoin::Network::Regtest).is_err());
	}

	#[test]
	fn exact_proportional_fees_and_rounding() {
		assert_eq!(fee_shares(&[10_000,20_000,10_000], 3).unwrap(), [1,1,1]);
		assert_eq!(fee_shares(&[10_000,20_000], 100).unwrap(), [33,67]);
		assert!(fee_shares(&[330],330).is_err());
		assert_eq!(fee_shares(&[10_000;100], 749).unwrap().iter().sum::<u64>(), 749);
	}
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
	pub enabled: bool,
	#[serde(with = "crate::utils::serde::duration")]
	pub interval: Duration,
	pub grace_blocks: u32,
	pub sweep_min_confs: u32,
	pub min_payout_sat: u64,
	pub max_fee_pct: u32,
	pub max_batch: usize,
	/// Existing estimator targets: 1, 3 or 6 blocks.
	pub conf_target_blocks: u16,
	pub receipt_dir: PathBuf,
}

impl Default for Config {
	fn default() -> Self {
		Self { enabled: false, interval: Duration::from_secs(60), grace_blocks: 1008,
			sweep_min_confs: 100, min_payout_sat: 10_000, max_fee_pct: 20,
			max_batch: 100, conf_target_blocks: 6, receipt_dir: PathBuf::new() }
	}
}

impl Config {
	pub fn validate(&self, network: bitcoin::Network) -> anyhow::Result<()> {
		if !self.enabled { return Ok(()); }
		ensure!(!self.interval.is_zero(), "expiry_payout.interval must be positive");
		ensure!((1..=100).contains(&self.max_batch), "expiry_payout.max_batch must be 1..100");
		ensure!((1..=99).contains(&self.max_fee_pct), "expiry_payout.max_fee_pct must be 1..99");
		ensure!(self.min_payout_sat >= 330 && self.min_payout_sat <= i64::MAX as u64,
			"expiry_payout.min_payout_sat must fit the ledger and be at least 330");
		ensure!(matches!(self.conf_target_blocks, 1 | 3 | 6), "expiry_payout.conf_target_blocks must be 1, 3 or 6");
		ensure!(self.sweep_min_confs > 0, "expiry_payout.sweep_min_confs must be positive");
		if network == bitcoin::Network::Bitcoin {
			ensure!(self.grace_blocks >= 144 && self.sweep_min_confs >= 100,
				"mainnet expiry payouts require grace >=144 and sweep depth >=100");
		}
		Ok(())
	}
}

#[derive(Default, Eq, PartialEq)]
pub(crate) enum Status { #[default] Ineligible, Busy, Paid }
#[derive(Default)]
pub(crate) struct Payment {
	pub status: Status,
	pub reason: String,
	pub txid: String,
	pub raw_tx: Vec<u8>,
	pub fee_sat: u64,
	pub outputs: Vec<FeeOutput>,
}
#[derive(Serialize)]
pub(crate) struct FeeOutput { pub vout: u32, pub amount_sat: u64, pub fee_sat: u64 }
#[derive(Default)]
struct TickStats { candidates: usize, waiting: usize, paid: usize }

impl Server {
	pub(crate) fn start_expiry_payout_task(self: &Arc<Self>) {
		if !self.config.expiry_payout.enabled { return; }
		let server = self.clone();
		let worker = self.rtmgr.spawn("ExpiryPayout");
		tokio::spawn(async move {
			let _worker = worker;
			loop {
				let started = Instant::now();
				let mut stats = TickStats::default();
				let result = server.expiry_payout_tick(&mut stats).await;
				info!(tip = server.chain_tip().height.to_u32(), candidates = stats.candidates,
					waiting = stats.waiting, paid = stats.paid, success = result.is_ok(),
					duration_ms = started.elapsed().as_millis(), "expiry payout tick summary");
				if let Err(e) = result { warn!("expiry payout tick failed: {e:#}"); }
				tokio::select! {
					_ = server.rtmgr.shutdown_signal() => break,
					_ = tokio::time::sleep(server.config.expiry_payout.interval) => {},
				}
			}
		});
	}

	async fn expiry_payout_tick(&self, stats: &mut TickStats) -> anyhow::Result<()> {
		if let Err(e) = self.export_expiry_receipts().await {
			warn!("expiry receipt export deferred: {e:#}");
		}
		let cfg = &self.config.expiry_payout;
		let Some(rates) = self.fee_estimator.real_rates() else {
			warn!("no real fee estimate; expiry payouts wait"); return Ok(());
		};
		let rate = match cfg.conf_target_blocks { 1 => rates.fast, 3 => rates.regular, _ => rates.slow };
		ensure!(rate > FeeRate::ZERO, "zero expiry fee estimate");
		let tip = self.chain_tip().height.to_u32();
		let mut cursor = (0, String::new());
		let mut batch = Vec::new();
		loop {
			let page = self.db.read(async |t| t.expiry_settlement_page(false, tip,
				cfg.grace_blocks, cfg.min_payout_sat, cursor.clone(), 256).await).await?;
			let Some(last) = page.last() else { break; };
			cursor = (last.expiry, last.id.to_string());
			for coin in page {
				stats.candidates += 1;
				let vtxo = Vtxo::deserialize(&coin.vtxo)?;
				if !self.expiry_path_swept(&vtxo).await? { stats.waiting += 1; continue; }
				if coin.unclaimed {
					let mut safe = !coin.predecessors.is_empty();
					for bytes in &coin.predecessors {
						if !self.expiry_path_swept(&Vtxo::deserialize(bytes)?).await? { safe = false; break; }
					}
					if !safe { stats.waiting += 1; continue; }
				}
				batch.push(coin.id);
				if batch.len() == cfg.max_batch {
					let paid = self.pay_expiry_batch(std::mem::take(&mut batch), rate).await?;
					if paid > 0 { stats.paid = paid; return Ok(()); }
				}
			}
		}
		if !batch.is_empty() { stats.paid = self.pay_expiry_batch(batch, rate).await?; }
		Ok(())
	}

	async fn expiry_path_swept(&self, vtxo: &Vtxo) -> anyhow::Result<bool> {
		let anchor = vtxo.chain_anchor();
		if self.bitcoind.try_get_tx_out(anchor, true).await?.is_some() { return Ok(false); }
		let anchor_tx = self.bitcoind.get_raw_transaction_verbosity_zero(&anchor.txid).await?.0;
		vtxo.validate_unsigned(&anchor_tx).context("invalid expiry path")?;
		let path = std::iter::once(anchor).chain(vtxo.transactions()
			.map(|t| OutPoint::new(t.tx.compute_txid(), t.output_idx as u32))).collect::<Vec<_>>();
		let ids = path.iter().map(|op| op.to_string().parse::<VtxoId>()).collect::<Result<Vec<_>,_>>()?;
		let hints = self.db.read(async |t| t.expiry_settlement_spenders(&ids).await).await?;
		for (outpoint, spender) in path.into_iter().zip(hints) {
			if outpoint != anchor && self.bitcoind.try_get_tx_out(outpoint, true).await?.is_some() { break; }
			let Some(spender) = spender else { continue; };
			let spender = Txid::from_str(&spender)?;
			let Some(info) = crate::bitcoind::custom_get_raw_transaction_info(&self.bitcoind, spender, None).await? else { continue; };
			if info.confirmations.unwrap_or(0) < self.config.expiry_payout.sweep_min_confs { continue; }
			let tx: Transaction = bitcoin::consensus::deserialize(&info.hex)?;
			if !tx.input.iter().any(|i| i.previous_output == outpoint) { continue; }
			let wallet = self.rounds_wallet.lock().await;
			let outputs = tx.output.iter().filter(|o| !o.script_pubkey.is_op_return()
				&& o.script_pubkey.as_bytes() != [0x51, 0x02, 0x4e, 0x73]).collect::<Vec<_>>();
			if !outputs.is_empty() && outputs.iter().all(|o| wallet.is_mine(o.script_pubkey.clone())) { return Ok(true); }
		}
		Ok(false)
	}

	async fn pay_expiry_batch(&self, batch: Vec<VtxoId>, rate: FeeRate) -> anyhow::Result<usize> {
		let mut pending = VecDeque::from([batch]);
		while let Some(mut batch) = pending.pop_front() {
			let payment = self.claim_and_pay(batch.clone(), rate).await?;
			if payment.status == Status::Paid {
				if let Err(e) = self.write_expiry_receipt(&payment) { warn!("expiry receipt deferred: {e:#}"); }
				info!(txid = %payment.txid, fee_sat = payment.fee_sat, coins = batch.len(), "expiry payment committed");
				return Ok(batch.len());
			}
			warn!(reason = %payment.reason, coins = batch.len(), "expiry batch deferred");
			if batch.len() > 1 {
				let right = batch.split_off(batch.len()/2);
				pending.push_back(batch); pending.push_back(right);
			}
		}
		Ok(0)
	}

	async fn export_expiry_receipts(&self) -> anyhow::Result<()> {
		if self.config.expiry_payout.receipt_dir.as_os_str().is_empty() { return Ok(()); }
		let mut cursor = (0, String::new());
		loop {
			let page = self.db.read(async |t| t.expiry_settlement_page(true, 0, 0, 0, cursor.clone(), 256).await).await?;
			let Some(last) = page.last() else { return Ok(()); };
			cursor = (0, last.id.to_string());
			for coin in page {
				if self.config.expiry_payout.receipt_dir.join(format!("{}.json", coin.payment_txid)).try_exists()? { continue; }
				let payment = self.db.read(async |t| t.expiry_receipt(&coin.payment_txid).await).await?;
				self.write_expiry_receipt(&payment)?;
			}
		}
	}

	fn write_expiry_receipt(&self, payment: &Payment) -> anyhow::Result<()> {
		let directory = &self.config.expiry_payout.receipt_dir;
		if directory.as_os_str().is_empty() { return Ok(()); }
		ensure!(payment.status == Status::Paid, "receipt requires a committed payment");
		let tx: Transaction = bitcoin::consensus::deserialize(&payment.raw_tx)?;
		ensure!(tx.compute_txid().to_string() == payment.txid, "receipt identity mismatch");
		fs::create_dir_all(directory)?;
		let temporary = directory.join(format!("{}.tmp", payment.txid));
		let mut file = File::create(&temporary)?;
		serde_json::to_writer(&mut file, &serde_json::json!({"txid": payment.txid, "outputs": payment.outputs}))?;
		file.write_all(b"\n")?;
		file.sync_all()?;
		fs::rename(temporary, directory.join(format!("{}.json", payment.txid)))?;
		File::open(directory)?.sync_all()?;
		Ok(())
	}
}
