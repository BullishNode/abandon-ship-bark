//! Operator-initiated, recipient-funded payments using the native wallet and nursery.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::str::FromStr;

use anyhow::Context;
use ark::{ProtocolEncoding, Vtxo, VtxoId, VtxoPolicy};
use bdk_wallet::coin_selection::LargestFirstCoinSelection;
use bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, Transaction, Txid, Weight};
use serde::{Deserialize, Serialize};
use bitcoin_ext::bdk::{WalletExt, WithGuaranteedChange};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use bitcoin_ext::FeeRateExt;
use tracing::{error, info, warn};

use crate::database::expiry_settlement::{payout_script, ExpiryInput, ExpirySource};
use crate::database::ln::LightningNodeId;
use crate::ln::SendRefund;
use crate::nursery::NurseryTxKind;
use crate::wallet::{BdkWalletExt, WalletKind};
use crate::Server;

fn deferred<T>(reason: impl std::fmt::Display) -> Option<T> {
	warn!("expiry batch deferred: {reason}");
	None
}

/// Largest remainders, tied by sorted destination script, keep all integer shares within
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

#[derive(Clone)]
struct PayoutGroup {
	script: ScriptBuf,
	inputs: Vec<ExpiryInput>,
	gross: u64,
}

/// Compute fees after grouping. Every recipient pays its proportional share of
/// the actual miner fee, and the minimum applies to its remaining output.
fn group_fee_shares(
	gross: &BTreeMap<ScriptBuf, u64>, fee: u64, minimum: u64,
) -> anyhow::Result<BTreeMap<ScriptBuf, u64>> {
	let amounts = gross.values().copied().collect::<Vec<_>>();
	let shares = fee_shares(&amounts, fee)?;
	gross.iter().zip(shares).map(|((script, amount), share)| {
		let net = amount.checked_sub(share).context("fee exceeds payout")?;
		ensure!(net >= minimum && net >= script.minimal_non_dust().to_sat(),
			"net wallet payout is below the minimum");
		Ok((script.clone(), share))
	}).collect()
}

impl Server {
	/// Pay the groups whose Lightning payments are free. Returns the payment's
	/// txid and fee, and the number of coins it settled.
	async fn claim_and_pay(
		&self, groups: Vec<PayoutGroup>, fee_rate: FeeRate, unavailable_nodes: &mut BTreeSet<LightningNodeId>,
	) -> anyhow::Result<Option<(Txid, u64, usize)>> {
		let cfg = self.config.expiry_payout.clone();
		// Cooperative Lightning claims and refund requests take the payment
		// guard before coin locks and can hold it while they wait on a node.
		// Take the guards in that same order, but without waiting: a busy
		// payment defers only its own wallet group, not the other wallets in
		// the batch. The guards are held until this returns, after COMMIT,
		// as are the wallet and coin locks below.
		let mut payment_guards = BTreeMap::new();
		let mut payable = Vec::with_capacity(groups.len());
		for group in groups {
			let hashes = group.inputs.iter().filter_map(|i| match i.source {
				ExpirySource::LightningReceive(hash) | ExpirySource::LightningSend(hash) => Some(hash),
				_ => None,
			}).filter(|hash| !payment_guards.contains_key(hash)).collect::<BTreeSet<_>>();
			let taken = hashes.iter().map_while(|hash| self.payment_guards.try_lock(*hash)).collect::<Vec<_>>();
			if taken.len() < hashes.len() {
				warn!(script = ?group.script, "expiry wallet group deferred: Lightning payment in progress");
				continue;
			}
			payment_guards.extend(taken.into_iter().map(|guard| (guard.payment_hash(), guard)));
			payable.push(group);
		}
		if payable.is_empty() { return Ok(deferred("Lightning payment in progress")); }
		let groups = payable;
		let inputs = groups.iter().flat_map(|g| g.inputs.iter().copied()).collect::<Vec<_>>();
		let ids = inputs.iter().map(|i| i.id).collect::<Vec<_>>();
		// A wallet group larger than the limit is paid alone.
		ensure!(!ids.is_empty() && (groups.len() == 1 || ids.len() <= cfg.max_batch),
			"expiry batch exceeds coin limit");
		let keys: Vec<String> = ids.iter().map(ToString::to_string).collect();
		let scripts = groups.iter().flat_map(|g| g.inputs.iter().map(|_| g.script.as_bytes().to_vec()))
			.collect::<Vec<_>>();
		let destinations = ids.iter().copied().zip(scripts.iter().cloned().map(ScriptBuf::from))
			.collect::<BTreeMap<_, _>>();
		ensure!(keys.iter().collect::<BTreeSet<_>>().len() == keys.len(), "duplicate expiry coin ID");
		let Ok(_flux) = self.vtxos_in_flux.try_lock(&ids) else { return Ok(deferred("coin in flux")) };
		let tip = self.chain_tip().height.to_u32();
		let coins = self.db.read(async |t| t.expiry_inputs(&inputs, tip, cfg.grace_blocks).await).await?;
		if coins.len() != ids.len() { return Ok(deferred("coin is spent, exited, in grace or participating")) }
		// Decide failed-send refunds again under the payment guards.
		let sends = inputs.iter().filter_map(|i| match i.source {
			ExpirySource::LightningSend(hash) => Some((i.id, hash)),
			_ => None,
		}).collect::<BTreeMap<_, _>>();
		let mut decided = BTreeSet::new();
		for coin in &coins {
			let Some(hash) = sends.get(&coin.id()) else { continue; };
			if !decided.insert(*hash) { continue; }
			let policy = coin.policy().as_server_htlc_send().context("expiry send is not an HTLC send")?;
			match self.lightning_send_refund(*hash, policy.htlc_expiry, unavailable_nodes).await? {
				SendRefund::Allowed => {},
				SendRefund::Refused(reason) | SendRefund::Undecided(reason) =>
					return Ok(deferred(format!("lightning send {hash} not refundable: {reason}"))),
			}
		}
		let mut expected = BTreeMap::<ScriptBuf,u64>::new();
		for v in &coins {
			let script = destinations.get(&v.id()).context("expiry destination missing")?;
			let amount = expected.entry(script.clone()).or_default();
			*amount = amount.checked_add(v.amount().to_sat()).context("expiry gross overflow")?;
		}
		for script in expected.keys() {
			if let Some(list) = &self.bitcoin_address_blocklist {
				if list.check_spk(script).await { return Ok(deferred("destination is blocklisted")); }
			}
		}
		let expected_build = expected.clone();
		let minimum = cfg.min_payout_sat;
		let built = self.rounds_wallet.build_blocking(move |wallet| {
			ensure!(expected_build.keys().all(|spk| !wallet.is_mine(spk.clone())),
				"expiry destination belongs to the rounds wallet");
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
				let mut per_script = group_fee_shares(&expected_build, fee, minimum)?;
				for out in &mut psbt.unsigned_tx.output {
					if let Some(share) = per_script.remove(&out.script_pubkey) {
						ensure!(Some(&out.value.to_sat()) == expected_build.get(&out.script_pubkey),
							"payout gross does not match destination");
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
			Err(e) => return Ok(deferred(format!("payout funding/fee deferred: {e:#}"))),
		};
		let accepted = crate::bitcoind::test_mempool_accept(&self.bitcoind, &[&tx]).await?;
		if !accepted[0].allowed {
			wallet.mark_output_keys_unused(&tx);
			return Ok(deferred(format!("payout not accepted: {:?}", accepted[0].reject_reason)));
		}
		// The raw transaction alone cannot recover a change key beyond the
		// restored wallet's lookahead. Commit its derivation metadata with it.
		let wallet_metadata = wallet.staged().cloned();
		let target = self.nursery_confirm_target();
		// The payment guards, coin and wallet locks are held through COMMIT.
		let stored = self.db.write(async |t| {
			if let Some(change) = &wallet_metadata {
				t.store_changeset(WalletKind::Rounds, change).await?;
			}
			t.store_expiry_payment(&inputs, &scripts, &tx, fee, tip, cfg.grace_blocks, target).await
		}).await;
		if let Err(error) = stored {
			// A lost COMMIT response does not mean rollback. Wait for the original
			// transaction's row locks, then read its result. If the database does
			// not answer, exit: startup waits for that COMMIT and reapplies it.
			let committed = self.db.write(async |t| {
				t.lock_expiry_inputs(&keys).await?;
				Ok(t.get_nursery_raw_tx(tx.compute_txid()).await?.is_some())
			}).await.unwrap_or_else(|e| {
				error!("expiry commit outcome unknown; exiting: {e:#}");
				std::process::exit(1);
			});
			if !committed { wallet.mark_output_keys_unused(&tx); return Err(error); }
		}
		wallet.take_staged();
		wallet.commit_tx(&tx);
		if let Err(e) = wallet.persist().await { warn!("expiry wallet persist deferred to restart: {e:#}"); }
		drop(wallet);
		self.tx_nursery.broadcast_tx(tx.clone(), NurseryTxKind::ExpiryPayout, target).await?;
		Ok(Some((tx.compute_txid(), fee, ids.len())))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn config_preserves_mainnet_floors_and_explicit_test_timing() {
		let mut cfg = Config::default();
		assert!(!cfg.enabled);
		assert_eq!(cfg.max_batch, 10_000);
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
		cfg.max_batch = 200;
		cfg.validate(bitcoin::Network::Regtest).unwrap();
		cfg.max_batch = 0;
		assert!(cfg.validate(bitcoin::Network::Regtest).is_err());
		cfg.max_batch = 1;
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

	#[test]
	fn grouped_minimum_is_net_and_has_no_percentage_cap() {
		let a = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let b = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
		let gross = BTreeMap::from([(a.clone(), 200 * 600)]);
		assert_eq!(group_fee_shares(&gross, 110_000, 10_000).unwrap()[&a], 110_000);
		assert!(group_fee_shares(&gross, 110_001, 10_000).is_err());
		assert!(group_fee_shares(&gross, 120_000, 0).is_err());
		assert!(group_fee_shares(&gross, 119_999, 0).is_err(), "dust still waits");
		let gross = BTreeMap::from([(a.clone(), 10_001), (b.clone(), 20_002)]);
		assert_eq!(group_fee_shares(&gross, 3, 10_000).unwrap(),
			BTreeMap::from([(a, 1), (b, 2)]));
		assert!(group_fee_shares(&gross, 5, 10_000).is_err());
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
	pub max_batch: usize,
	/// Existing estimator targets: 1, 3 or 6 blocks.
	pub conf_target_blocks: u16,
	/// The same configuration file mounted into the watchmand process.
	pub watchman_config: Option<PathBuf>,
}

impl Default for Config {
	fn default() -> Self {
		Self { enabled: false, interval: Duration::from_secs(60), grace_blocks: 1008,
			sweep_min_confs: 100, min_payout_sat: 10_000,
			max_batch: 10_000, conf_target_blocks: 6, watchman_config: None }
	}
}

impl Config {
	pub fn validate(&self, network: bitcoin::Network) -> anyhow::Result<()> {
		if !self.enabled { return Ok(()); }
		ensure!(!self.interval.is_zero(), "expiry_payout.interval must be positive");
		ensure!(self.max_batch > 0, "expiry_payout.max_batch must be positive");
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
		let cfg = &self.config.expiry_payout;
		let estimate: bitcoin_ext::rpc::json::EstimateSmartFeeResult = self.bitcoind.call_raw(
			"estimatesmartfee", &[cfg.conf_target_blocks.into(), "economical".into()],
		).await?;
		let Some(rate) = estimate.fee_rate else {
			warn!("no real fee estimate; expiry payouts wait"); return Ok(());
		};
		let rate = FeeRate::from_amount_per_kvb_ceil(rate);
		ensure!(rate > FeeRate::ZERO, "zero expiry fee estimate");
		let tip = self.chain_tip().height.to_u32();
		let mut cursor = (0, String::new());
		let mut groups = Vec::<PayoutGroup>::new();
		let mut group_index = BTreeMap::<ScriptBuf, usize>::new();
		let mut destination_allowed = BTreeMap::<ScriptBuf, bool>::new();
		let mut cancellation_attempts = BTreeSet::new();
		let mut send_refunds = BTreeMap::new();
		// A node that does not answer once is not asked again this tick, so
		// it holds up only the payments it made, not every other wallet.
		let mut unavailable_nodes = BTreeSet::new();
		loop {
			let page = self.db.read(async |t| t.expiry_settlement_page(tip,
				cfg.grace_blocks, cursor.clone(), 256).await).await?;
			let Some(last) = page.last() else { break; };
			cursor = (last.expiry, last.id.to_string());
			let vtxos = page.iter().map(|coin| Vtxo::deserialize(&coin.vtxo))
				.collect::<Result<Vec<_>, _>>()?;
			let owners = page.iter().zip(&vtxos).map(|(coin, vtxo)| {
				if coin.source == ExpirySource::Unregistered { coin.input_owner }
				else { Some(vtxo.user_pubkey()) }
			}).collect::<Vec<_>>();
			let user_keys = owners.iter().flatten().copied().collect::<Vec<_>>();
			let fallback = self.db.read(async |t| t.fallback_scripts(&user_keys).await).await?;
			for ((coin, vtxo), owner) in page.into_iter().zip(vtxos).zip(owners) {
				// Padding leaves have no participation or owner entitlement.
				if coin.unclaimed && coin.predecessors.is_empty() { continue; }
				stats.candidates += 1;
				let Some(owner) = owner else {
					warn!(id = %coin.id, "unregistered expiry input owner unavailable");
					stats.waiting += 1;
					continue;
				};
				let script = fallback.get(&owner).cloned().unwrap_or_else(|| payout_script(owner));
				let allowed = match destination_allowed.get(&script) {
					Some(allowed) => *allowed,
					None => {
						let blocked = match &self.bitcoin_address_blocklist {
							Some(list) => list.check_spk(&script).await,
							None => false,
						};
						let ours = self.rounds_wallet.lock().await.is_mine(script.clone());
						let allowed = !blocked && !ours;
						if !allowed { warn!(?script, blocked, ours, "expiry destination held"); }
						destination_allowed.insert(script.clone(), allowed);
						allowed
					},
				};
				if !allowed { stats.waiting += 1; continue; }
				if !self.expiry_path_swept(&vtxo).await? { stats.waiting += 1; continue; }
				if let ExpirySource::LightningSend(hash) = coin.source {
					let refundable = match send_refunds.get(&hash) {
						Some(refundable) => *refundable,
						None => {
							let policy = vtxo.policy().as_server_htlc_send()
								.context("expiry send is not an HTLC send")?;
							let refundable = match self.lightning_send_refund(
								hash, policy.htlc_expiry, &mut unavailable_nodes,
							).await? {
								SendRefund::Allowed => true,
								SendRefund::Refused(reason) | SendRefund::Undecided(reason) => {
									warn!(%hash, %reason, "expiry send refund held");
									false
								},
							};
							send_refunds.insert(hash, refundable);
							refundable
						},
					};
					if !refundable { stats.waiting += 1; continue; }
				}
				if coin.unclaimed {
					let mut safe = !coin.predecessors.is_empty();
					for bytes in &coin.predecessors {
						if !self.expiry_path_swept(&Vtxo::deserialize(bytes)?).await? { safe = false; break; }
					}
					if !safe {
						if let Some(hash) = vtxo.unlock_hash() {
							if cancellation_attempts.insert(hash) {
								self.cancel_failed_expiry_refresh(&vtxo).await?;
							}
						}
						stats.waiting += 1;
						continue;
					}
				}
				let index = *group_index.entry(script.clone()).or_insert_with(|| {
					groups.push(PayoutGroup { script, inputs: Vec::new(), gross: 0 });
					groups.len() - 1
				});
				let group = &mut groups[index];
				group.inputs.push(ExpiryInput { id: coin.id, source: coin.source, owner });
				group.gross = group.gross.checked_add(vtxo.amount().to_sat()).context("expiry group overflow")?;
			}
		}
		// Only batch after scanning every page, so a page boundary cannot turn
		// one payable wallet into several individually sub-minimum fragments.
		let mut batch = Vec::new();
		let mut batch_coins = 0;
		for group in groups {
			if group.gross <= cfg.min_payout_sat {
				stats.waiting += group.inputs.len();
				continue;
			}
			// The payout pays from the rounds wallet with one output per wallet
			// group, so its coin count does not size the transaction. A group
			// larger than the limit is paid alone instead of never.
			if group.inputs.len() > cfg.max_batch {
				info!(coins = group.inputs.len(), max_batch = cfg.max_batch,
					"expiry wallet group exceeds max_batch; paying it alone");
				let coins = group.inputs.len();
				let paid = self.pay_expiry_batch(vec![group], rate, &mut unavailable_nodes).await?;
				if paid > 0 { stats.paid = paid; return Ok(()); }
				stats.waiting += coins;
				continue;
			}
			if batch_coins + group.inputs.len() > cfg.max_batch {
				let paid = self.pay_expiry_batch(std::mem::take(&mut batch), rate, &mut unavailable_nodes).await?;
				if paid > 0 { stats.paid = paid; return Ok(()); }
				batch_coins = 0;
			}
			batch_coins += group.inputs.len();
			batch.push(group);
		}
		if !batch.is_empty() { stats.paid = self.pay_expiry_batch(batch, rate, &mut unavailable_nodes).await?; }
		Ok(())
	}

	/// When an original exited, an unfinished delegated exchange can no longer
	/// complete. After every replacement is swept, preserve the original owners'
	/// claims instead of inventing a partial allocation across replacement keys.
	async fn cancel_failed_expiry_refresh(&self, vtxo: &Vtxo) -> anyhow::Result<()> {
		let Some(hash) = vtxo.unlock_hash() else { return Ok(()); };
		let Some(part) = self.db.read(async |t| t.get_round_participation_by_unlock_hash(hash).await).await?
			else { return Ok(()); };
		let Some(round_id) = part.round_id else { return Ok(()); };
		if round_id.as_round_txid() != vtxo.chain_anchor().txid || part.forfeited_at.is_some()
			|| part.inputs.is_empty() || part.inputs.iter().any(|i| i.is_forfeited()) { return Ok(()); }
		let Some(round) = self.db.read(async |t| t.get_round(round_id).await).await? else { return Ok(()); };
		let db_id = round.id;
		let tree = round.into_cached_tree()?;
		let indexes = tree.spec.spec.leaf_idxs_for_participation(hash, part.outputs.iter().map(|o| &o.vtxo_request))
			.context("expired participation leaves missing")?;
		if indexes.is_empty() || indexes.iter().any(|i| tree.spec.spec.vtxos[*i].cosign_pubkey.is_some()) { return Ok(()); }
		let outputs = indexes.into_iter().map(|i| tree.build_vtxo(i)).collect::<Vec<_>>();
		let output_ids = outputs.iter().map(Vtxo::id).collect::<Vec<_>>();
		let input_ids = part.inputs.iter().map(|i| i.vtxo_id).collect::<Vec<_>>();
		let ids = input_ids.iter().chain(&output_ids).copied().collect::<Vec<_>>();
		let Ok(_flux) = self.vtxos_in_flux.try_lock(&ids) else { return Ok(()); };
		let inputs = self.db.read(async |t| t.get_user_vtxos_by_id(&input_ids).await).await?;
		// HTLC ownership resolution is outside this pubkey payout policy.
		if outputs.iter().chain(inputs.iter().map(|v| &v.vtxo))
			.any(|v| !matches!(v.policy(), VtxoPolicy::Pubkey(..))) { return Ok(()); }
		for output in &outputs {
			if !self.expiry_path_swept(output).await? { return Ok(()); }
		}
		let mut exited = Vec::new();
		for input in &inputs {
			let leaf = input.vtxo_id.to_point();
			if let Some(tx) = crate::bitcoind::custom_get_raw_transaction_info(&self.bitcoind, leaf.txid, None).await? {
				if tx.confirmations.unwrap_or(0) > 0 { exited.push(input.vtxo_id); continue; }
			}
			// A waiting/live path remains in the original exchange. Only swept
			// originals are released here, so none can also exit after release.
			if !self.expiry_path_swept(&input.vtxo).await? { return Ok(()); }
		}
		if exited.is_empty() { return Ok(()); }
		if self.db.write(async |t| t.cancel_expired_participation(hash, db_id, &input_ids, &output_ids, &exited).await).await? {
			info!(unlock_hash = %hash, round = %round_id, inputs = input_ids.len(), exited = exited.len(),
				"cancelled swept delegated exchange; original swept entitlements released");
		}
		Ok(())
	}

	/// Whether a confirmed sweep spent this coin's own backing path into the
	/// rounds wallet, at the configured depth.
	pub(crate) async fn expiry_path_swept(&self, vtxo: &Vtxo) -> anyhow::Result<bool> {
		let anchor = vtxo.chain_anchor();
		if self.bitcoind.try_get_tx_out(anchor, true).await?.is_some() { return Ok(false); }
		// A crash can leave a persisted unsigned round that Core never saw.
		// Keep that candidate waiting without starving later funded coins.
		let Some(anchor_info) = crate::bitcoind::custom_get_raw_transaction_info(
			&self.bitcoind, anchor.txid, None,
		).await? else { return Ok(false); };
		let anchor_tx = bitcoin::consensus::deserialize(&anchor_info.hex)?;
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

	async fn pay_expiry_batch(
		&self, batch: Vec<PayoutGroup>, rate: FeeRate, unavailable_nodes: &mut BTreeSet<LightningNodeId>,
	) -> anyhow::Result<usize> {
		let mut pending = VecDeque::from([batch]);
		while let Some(mut batch) = pending.pop_front() {
			if let Some((txid, fee_sat, coins)) = self.claim_and_pay(batch.clone(), rate, unavailable_nodes).await? {
				info!(%txid, fee_sat, coins, "expiry payment committed");
				return Ok(coins);
			}
			if batch.len() > 1 {
				let right = batch.split_off(batch.len()/2);
				pending.push_back(batch); pending.push_back(right);
			}
		}
		Ok(0)
	}
}
