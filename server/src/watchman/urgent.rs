//! Direct-mode urgent reactions reuse the existing builders and durable nursery.
//! No legacy expiry sweep is admitted. Every package retains its original debit.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{Amount, FeeRate, OutPoint, Transaction, TxOut, Txid};
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::{BlockRef, TxStatus, TransactionExt, cpfp::MakeCpfpFees};
use bitcoin_ext::bdk::WalletExt;
use bitcoind_async_client::Client;
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;

use ark::ServerVtxo;
use crate::bitcoind as bcd;
use crate::database::{Db, carrier_snapshot::BackingSnapshotLimits, urgent_publication::UrgentPublication};
use crate::utils::InstrumentedLock;
use crate::wallet::{BdkWalletExt, PersistedWallet, WalletKind, WalletUtxosGuard};
use super::{Watchman, expiry_runtime::DirectExpiryRuntime, ordinary_fee::OrdinaryFeeBudget};

#[derive(Default)]
pub(super) struct UrgentWork {
	pending: Option<(UrgentPublication, Vec<Transaction>)>,
	pub(super) reservations: BTreeMap<Txid, WalletUtxosGuard>,
}

/// Restore the original wallet/outbox stage before ordinary coin selection.
/// The carrier lock waits for any still-running earlier COMMIT to finish.
pub(super) async fn hydrate_urgent_publications(
	db: &Db, wallet: &InstrumentedLock<PersistedWallet>, ordinary: &Arc<OrdinaryFeeBudget>, budget: BackingSnapshotLimits,
	node: &Client, reservations: &mut BTreeMap<Txid, WalletUtxosGuard>,
) -> anyhow::Result<Vec<(UrgentPublication, Vec<Transaction>)>> {
	let observed = db.write(async |t| {
		t.lock_htlc_settlements().await?; t.lock_expiry_carriers().await?;
		t.urgent_publications(budget).await
	}).await?;
	// All Core observation precedes the wallet/source money locks.
	let winners = confirmed_urgent_transactions(node, &observed).await?;
	let mut wallet = wallet.lock().await;
	for (meta, txs) in &observed {
		if txs.len() != 2 { continue; }
		let group = meta.txids[0];
		let known_winner = observed.iter().filter(|(_, p)| p.len() == 2 && p[0].compute_txid() == group)
			.filter_map(|(m, _)| winners.get(m.txids.last()?).map(|block| (*m.txids.last().expect("package checked"), *block)))
			.find(|(txid, block)| wallet.require_confirmed_fee_winner(*txid, *block, true).is_ok());
		if known_winner.is_some() { reservations.remove(&group); continue; }
		let points = observed.iter().filter(|(_, p)| p.len() == 2 && p[0].compute_txid() == group)
			.flat_map(|(_, p)| p[1].input.iter().filter(move |i| i.previous_output.txid != group).map(|i| i.previous_output))
			.collect::<BTreeSet<_>>();
		restore_fee_union(&wallet, reservations, group, points)?;
	}
	let publications = db.write(async |t| {
		t.lock_htlc_settlements().await?; t.lock_expiry_carriers().await?;
		let publications = t.urgent_publications(budget).await?;
		ensure!(publications == observed, "urgent durable inventory changed during winner observation");
		for (meta, txs) in &publications {
			if txs.len() == 2 {
				let anchor = txs[0].fee_anchor().context("urgent parent has no fee anchor")?.0;
				for (input, prevout) in txs[1].input.iter().zip(&meta.prevouts[1]) {
					if input.previous_output == anchor {
						ensure!(txs[0].output.get(anchor.vout as usize) == Some(prevout), "urgent anchor prevout differs");
					} else {
						let actual = wallet.tx_graph().get_txout(input.previous_output).context("urgent fee input missing from owning wallet")?;
						ensure!(actual == prevout && wallet.is_mine(actual.script_pubkey.clone()), "urgent fee input ownership differs");
					}
				}
				for tx in txs { wallet.commit_tx(tx); }
			}
		}
		if let Some(stage) = wallet.staged() { t.store_changeset(WalletKind::Watchman, stage).await?; }
		Ok(publications)
	}).await.context("urgent retained publication outcome unknown; no new signing")?;
	wallet.take_staged();
	// DB-first has completed and the metadata/bytes are now authoritative.
	for (meta, txs) in &publications {
		let fee = meta.validate(txs)?;
		ordinary.bind_urgent_signed(meta.attempt(), txs, fee, sha256::Hash::hash(&serde_json::to_vec(meta)?))?;
		db.write(async |t| t.permit_bound_urgent_publication(meta).await).await?;
	}
	Ok(publications)
}

fn restore_fee_union(wallet: &PersistedWallet, guards: &mut BTreeMap<Txid, WalletUtxosGuard>, group: Txid, points: BTreeSet<OutPoint>) -> anyhow::Result<()> {
	if guards.get(&group).is_some_and(|guard| guard.utxos().iter().copied().collect::<BTreeSet<_>>() == points) { return Ok(()); }
	// The owning wallet lock spans transfer. No competing builder can select
	// either the old union or the newly admitted complete union in between.
	guards.remove(&group);
	guards.insert(group, wallet.lock_wallet_utxos(points)?);
	Ok(())
}

async fn confirmed_urgent_transactions(node: &Client, publications: &[(UrgentPublication, Vec<Transaction>)]) -> anyhow::Result<BTreeMap<Txid, BlockRef>> {
	let before = bcd::tip(node).await?;
	let mut winners = BTreeMap::new();
	for (_, txs) in publications { for tx in txs {
		if let TxStatus::Confirmed(block) = bcd::tx_status(node, tx.compute_txid()).await? {
			let actual = bcd::custom_get_raw_transaction_info(node, tx.compute_txid(), Some(&block.hash)).await?
				.context("confirmed urgent winner bytes unavailable")?;
			ensure!(bitcoin::consensus::deserialize::<Transaction>(&actual.hex)? == *tx, "urgent winner differs from original signed bytes");
			winners.insert(tx.compute_txid(), block);
		}
	} }
	ensure!(bcd::tip(node).await? == before, "Core tip changed during urgent winner observation");
	Ok(winners)
}

impl Watchman {
	async fn urgent_sources(&self, tx: &Transaction) -> anyhow::Result<Vec<ServerVtxo>> {
		self.db.read(async |t| {
			let mut sources = Vec::new();
			for input in &tx.input { sources.push(t.get_bare_vtxo_by_id(input.previous_output.into()).await?.vtxo); }
			Ok(sources)
		}).await
	}

	async fn authenticate_urgent_outputs(&self, outputs: &[(OutPoint, TxOut)]) -> anyhow::Result<()> {
		ensure!(!outputs.is_empty() && outputs.len() <= crate::payout_witness::DIRECT_HISTORY_LIMITS.max_rows,
			"urgent actual input observations exceed bound");
		let before = bcd::tip(&self.bitcoind).await?;
		for (point, expected) in outputs {
			let actual = self.bitcoind.try_get_tx_out(*point, false).await?.context("urgent input is not unspent on the active chain")?;
			ensure!(actual.confirmations > 0 && actual.value == expected.value &&
				actual.script_pub_key.hex == expected.script_pubkey.as_bytes(), "urgent input differs from confirmed actual Core prevout");
		}
		ensure!(bcd::tip(&self.bitcoind).await? == before, "urgent Core tip changed during input authentication");
		Ok(())
	}

	pub(super) async fn recover_urgent_publications(&self, runtime: &DirectExpiryRuntime, submit: bool) -> anyhow::Result<()> {
		let mut work = runtime.urgent_work.lock().await;
		if let Some((meta, txs)) = &work.pending {
			// Same complete bytes and original reservation only. A COMMIT error
			// never authorizes another attempt or a replacement wallet selection.
			self.db.write(async |t| t.store_urgent_publication(meta, txs, self.sync_height().height).await).await?;
		}
		let publications = hydrate_urgent_publications(&self.db, &self.watchman_wallet, &runtime.ordinary,
			crate::payout_witness::DIRECT_HISTORY_LIMITS, &self.bitcoind, &mut work.reservations).await?;
		if let Some((meta, txs)) = &work.pending {
			ensure!(publications.iter().any(|p| p == &(meta.clone(), txs.clone())), "unknown urgent COMMIT not found exactly; original packet retained");
		}
		work.pending = None;
		drop(work);
		if submit {
			let winners = confirmed_urgent_transactions(&self.bitcoind, &publications).await?;
			for (_, txs) in &publications {
				let obsolete = publications.iter().flat_map(|(_, p)| p).any(|known|
					winners.contains_key(&known.compute_txid()) && known.compute_txid() != txs.last().expect("package checked").compute_txid() &&
					known.input.iter().any(|input| txs.last().expect("package checked").input.iter().any(|old| old.previous_output == input.previous_output)));
				if !obsolete && !winners.contains_key(&txs.last().expect("package checked").compute_txid()) {
					self.submit_urgent_package(txs).await?;
				}
			}
		}
		Ok(())
	}

	async fn submit_urgent_package(&self, txs: &[Transaction]) -> anyhow::Result<()> {
		if txs.len() == 1 { bcd::broadcast_tx(&self.bitcoind, &txs[0]).await?; }
		else { bcd::submit_package(&self.bitcoind, txs).await?; }
		Ok(())
	}

	pub(super) async fn publish_urgent_claim(&self, runtime: &DirectExpiryRuntime, sources: &[ServerVtxo], rate: FeeRate) -> anyhow::Result<()> {
		ensure!(rate <= runtime.policy.max_fee_rate, "urgent claim fee rate exceeds configured maximum");
		let mut work = runtime.urgent_work.lock().await;
		ensure!(work.pending.is_none(), "urgent publication unresolved; original packet retained");
		let points = sources.iter().map(|v| (v.point(), v.txout())).collect::<Vec<_>>();
		self.authenticate_urgent_outputs(&points).await?;
		// Preliminary source fence, then sign without a DB/money lock. Publication
		// repeats this fence atomically with the durable nursery insertion.
		self.db.write(async |t| t.lock_unfrozen_urgent_sources(sources).await).await?;
		let ceiling = runtime.ordinary.urgent_ceiling_limit(runtime.policy.max_miner_fee)?;
		let attempt = runtime.ordinary.reserve_urgent_ceiling(ceiling)?;
		let tx = self.build_claim_tx(sources, rate).await?;
		let meta = UrgentPublication { version: 1, policy_sha256: attempt.policy_sha256, attempt_index: attempt.index,
			ceiling_sat: ceiling.to_sat(), source_anchors: sources.iter().map(|v| v.chain_anchor()).collect::<BTreeSet<_>>().into_iter().collect(),
			txids: vec![tx.compute_txid()], prevouts: vec![points.into_iter().map(|(_, output)| output).collect()] };
		let fee = meta.validate(std::slice::from_ref(&tx))?;
		ensure!(tx.weight() <= runtime.policy.max_weight && rate.fee_wu(tx.weight()).is_some_and(|required| fee >= required),
			"urgent claim signed weight/fee violates existing exact policy");
		work.pending = Some((meta.clone(), vec![tx.clone()]));
		self.db.write(async |t| t.store_urgent_publication(&meta, std::slice::from_ref(&tx), self.sync_height().height).await).await?;
		runtime.ordinary.bind_urgent_signed(attempt, std::slice::from_ref(&tx), fee, sha256::Hash::hash(&serde_json::to_vec(&meta)?))?;
		self.db.write(async |t| t.permit_bound_urgent_publication(&meta).await).await?;
		work.pending = None; drop(work);
		self.submit_urgent_package(std::slice::from_ref(&tx)).await?;
		Ok(())
	}

	pub(super) async fn publish_urgent_progress(&self, runtime: &DirectExpiryRuntime, txid: Txid, _vtxo: &ServerVtxo, rate: FeeRate) -> anyhow::Result<()> {
		ensure!(rate <= runtime.policy.max_fee_rate, "urgent progression fee rate exceeds configured maximum");
		let mut work = runtime.urgent_work.lock().await;
		ensure!(work.pending.is_none(), "urgent publication unresolved; original packet retained");
		let record = self.db.read(async |t| t.get_virtual_transaction_by_txid(txid).await).await?.context("urgent progression missing")?;
		let parent = record.signed_tx.context("urgent progression is unsigned")?.into_owned();
		let sources = self.urgent_sources(&parent).await?;
		let mut points = sources.iter().map(|v| (v.point(), v.txout())).collect::<Vec<_>>();
		let available = {
			let wallet = self.watchman_wallet.lock().await;
			wallet.confirmed_unlocked_utxos().into_iter().map(|point| {
				Ok((point, wallet.get_utxo(point).context("eligible urgent wallet input disappeared")?.txout))
			}).collect::<anyhow::Result<BTreeMap<_, _>>>()?
		};
		points.extend(available.iter().map(|(p,o)| (*p,o.clone())));
		self.authenticate_urgent_outputs(&points).await?;
		let mut wallet = self.watchman_wallet.lock().await;
		ensure!(wallet.confirmed_unlocked_utxos().into_iter().collect::<BTreeSet<_>>() == available.keys().copied().collect(),
			"urgent fee source changed during independent Core observation");
		self.db.write(async |t| t.lock_unfrozen_urgent_sources(&sources).await).await?;
		let ceiling = runtime.ordinary.urgent_ceiling_limit(runtime.policy.max_miner_fee)?;
		let attempt = runtime.ordinary.reserve_urgent_ceiling(ceiling)?;
		let parent_inputs = sources.iter().try_fold(Amount::ZERO, |n, v| n.checked_add(v.amount())).context("urgent parent amount overflow")?;
		let parent_outputs = parent.output.iter().try_fold(Amount::ZERO, |n, o| n.checked_add(o.value)).context("urgent parent output overflow")?;
		let parent_fee = parent_inputs.checked_sub(parent_outputs).context("urgent parent output exceeds authenticated inputs")?;
		let child_ceiling = ceiling.checked_sub(parent_fee).context("urgent parent already exceeds reserved fee")?;
		let allowed = available.keys().copied().collect::<Vec<_>>();
		let child = wallet.make_signed_p2a_cpfp_with_limits(&parent, MakeCpfpFees::Effective(rate), Some(&allowed), Some(child_ceiling))?;
		let (anchor, anchor_output) = parent.fee_anchor().context("urgent parent lacks fee anchor")?;
		let child_prevouts = child.input.iter().map(|input| {
			if input.previous_output == anchor { Ok(anchor_output.clone()) }
			else { available.get(&input.previous_output).cloned().context("urgent child used an unadmitted or reserved whole input") }
		}).collect::<anyhow::Result<Vec<_>>>()?;
		let txs = vec![parent, child];
		let meta = UrgentPublication { version: 1, policy_sha256: attempt.policy_sha256, attempt_index: attempt.index,
			ceiling_sat: ceiling.to_sat(), source_anchors: sources.iter().map(|v| v.chain_anchor()).collect::<BTreeSet<_>>().into_iter().collect(),
			txids: txs.iter().map(Transaction::compute_txid).collect(), prevouts: vec![sources.iter().map(|v| v.txout()).collect(), child_prevouts] };
		let fee = meta.validate(&txs)?;
		let weight = txs[0].weight().checked_add(txs[1].weight()).context("urgent package weight overflow")?;
		ensure!(weight <= runtime.policy.max_weight && rate.fee_wu(weight).is_some_and(|required| fee >= required),
			"urgent actual signed package fee/weight violates policy");
		let mut union = work.reservations.get(&txid).map(|g| g.utxos().iter().copied().collect::<BTreeSet<_>>()).unwrap_or_default();
		union.extend(txs[1].input.iter().filter(|i| i.previous_output != anchor).map(|i| i.previous_output));
		restore_fee_union(&wallet, &mut work.reservations, txid, union)?;
		work.pending = Some((meta.clone(), txs.clone()));
		for tx in &txs { wallet.commit_tx(tx); }
		self.db.write(async |t| {
			t.store_urgent_publication(&meta, &txs, self.sync_height().height).await?;
			if let Some(stage) = wallet.staged() { t.store_changeset(WalletKind::Watchman, stage).await?; }
			Ok(())
		}).await?;
		wallet.take_staged(); drop(wallet);
		runtime.ordinary.bind_urgent_signed(attempt, &txs, fee, sha256::Hash::hash(&serde_json::to_vec(&meta)?))?;
		self.db.write(async |t| t.permit_bound_urgent_publication(&meta).await).await?;
		work.pending = None; drop(work);
		self.submit_urgent_package(&txs).await?;
		Ok(())
	}
}
