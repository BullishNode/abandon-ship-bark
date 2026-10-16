//! Authenticated attempt metadata on the existing signed nursery outbox.
//! This is not a second fee ledger; every debit belongs to the original file.

use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Transaction, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::sha256;
use bitcoin_ext::BlockHeight;

use crate::nursery::NurseryTxKind;
use crate::watchman::ordinary_fee::UrgentFeeAttempt;
use super::{Tx, carrier_snapshot::BackingSnapshotLimits};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrgentPublication {
	pub version: u32,
	pub policy_sha256: sha256::Hash,
	pub attempt_index: u64,
	pub ceiling_sat: u64,
	pub source_anchors: Vec<OutPoint>,
	pub txids: Vec<Txid>,
	/// Exact authenticated input outputs in transaction/input order.
	pub prevouts: Vec<Vec<TxOut>>,
}

impl UrgentPublication {
	pub fn attempt(&self) -> UrgentFeeAttempt {
		UrgentFeeAttempt { policy_sha256: self.policy_sha256, index: self.attempt_index }
	}

	pub fn validate(&self, txs: &[Transaction]) -> anyhow::Result<Amount> {
		ensure!(self.version == 1 && self.attempt_index > 0 && self.ceiling_sat > 0 &&
			self.ceiling_sat <= Amount::MAX_MONEY.to_sat(), "invalid urgent publication identity/ceiling");
		ensure!(!txs.is_empty() && txs.len() <= 2 && self.prevouts.len() == txs.len() &&
			self.txids == txs.iter().map(Transaction::compute_txid).collect::<Vec<_>>() &&
			self.txids.iter().collect::<BTreeSet<_>>().len() == txs.len(), "urgent package identity differs");
		ensure!(!self.source_anchors.is_empty() && self.source_anchors.iter().copied().collect::<BTreeSet<_>>()
			.into_iter().collect::<Vec<_>>() == self.source_anchors, "urgent source anchors are not canonical");
		let mut total_fee = Amount::ZERO;
		let mut total_weight = 0_u64;
		for (tx, prevouts) in txs.iter().zip(&self.prevouts) {
			ensure!(!tx.input.is_empty() && tx.input.len() == prevouts.len(), "urgent package prevouts are incomplete");
			let input = prevouts.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
				.filter(|v| *v <= Amount::MAX_MONEY).context("urgent input value overflow")?;
			let output = tx.output.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
				.context("urgent output value overflow")?;
			total_fee = total_fee.checked_add(input.checked_sub(output).context("urgent outputs exceed actual prevouts")?)
				.context("urgent package fee overflow")?;
			total_weight = total_weight.checked_add(tx.weight().to_wu()).context("urgent signed weight overflow")?;
		}
		ensure!(total_weight <= 400_000 && total_fee.to_sat() <= self.ceiling_sat,
			"urgent actual signed weight or prevout fee exceeds ceiling");
		Ok(total_fee)
	}
}

pub(crate) enum UrgentRelay {
	Unrelated,
	Unbound,
	Bound(Vec<Transaction>),
}

impl Tx<'_> {
	/// Called only after the SAME complete independent history binding is durable.
	pub(crate) async fn permit_bound_urgent_publication(&self, meta: &UrgentPublication) -> anyhow::Result<()> {
		let publication = meta.txids.last().context("urgent publication has no transaction")?;
		let expected = serde_json::to_value(meta)?;
		let row = self.query_one("SELECT urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE txid=$1 FOR UPDATE", &[&publication.to_string()]).await?;
		ensure!(serde_json::from_str::<serde_json::Value>(row.get::<_, Option<&str>>("attempt").context("urgent metadata missing")?)? == expected,
			"urgent permission differs from original durable attempt");
		self.execute("UPDATE nursery_tx SET urgent_fee_bound=TRUE WHERE txid=$1", &[&publication.to_string()]).await?;
		Ok(())
	}

	pub(crate) async fn urgent_publication_bound(&self, meta: &UrgentPublication) -> anyhow::Result<bool> {
		Ok(self.query_one("SELECT urgent_fee_bound FROM nursery_tx WHERE txid=$1", &[&meta.txids.last()
			.context("urgent package empty")?.to_string()]).await?.get("urgent_fee_bound"))
	}

	/// Every member, including a NULL-metadata shared parent, is gated through
	/// the immutable publication's package IDs. An older bound package may still
	/// relay its original parent while a new replacement remains unbound.
	pub(crate) async fn urgent_relay(&self, txid: Txid) -> anyhow::Result<UrgentRelay> {
		let bounds = crate::payout_witness::DIRECT_HISTORY_LIMITS;
		let size = self.query_one("SELECT count(*)::bigint AS rows, COALESCE(sum(octet_length(urgent_fee_attempt::text)),0)::bigint AS bytes FROM nursery_tx WHERE urgent_fee_attempt->'txids' ? $1", &[&txid.to_string()]).await?;
		ensure!(usize::try_from(size.get::<_, i64>("rows"))? <= bounds.max_rows &&
			usize::try_from(size.get::<_, i64>("bytes"))? <= bounds.max_serialized_bytes, "urgent relay metadata exceeds read bounds");
		let rows = self.query("SELECT urgent_fee_attempt::text AS attempt,urgent_fee_bound FROM nursery_tx
			WHERE urgent_fee_attempt->'txids' ? $1 ORDER BY id DESC LIMIT 10001", &[&txid.to_string()]).await?;
		ensure!(rows.len() <= 10000, "urgent relay membership exceeds bound");
		if rows.is_empty() { return Ok(UrgentRelay::Unrelated); }
		for row in rows {
			if !row.get::<_, bool>("urgent_fee_bound") { continue; }
			let meta: UrgentPublication = serde_json::from_str(row.get("attempt"))?;
			let mut txs = Vec::new();
			for id in &meta.txids {
				let raw = self.get_nursery_raw_tx(*id).await?.context("bound urgent package member missing")?;
				txs.push(raw);
			}
			meta.validate(&txs)?;
			return Ok(UrgentRelay::Bound(txs));
		}
		Ok(UrgentRelay::Unbound)
	}


	pub(crate) async fn store_urgent_publication(&self, meta: &UrgentPublication, txs: &[Transaction], target: BlockHeight) -> anyhow::Result<()> {
		meta.validate(txs)?;
		let mut sources = Vec::new();
		for (input, expected) in txs[0].input.iter().zip(&meta.prevouts[0]) {
			let source = self.get_bare_vtxo_by_id(input.previous_output.into()).await?.vtxo;
			ensure!(source.txout() == *expected, "urgent source prevout differs from authenticated original row");
			sources.push(source);
		}
		ensure!(sources.iter().map(|v| v.chain_anchor()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>() == meta.source_anchors,
			"urgent physical source association differs");
		self.lock_unfrozen_urgent_sources(&sources).await?;
		let value = serde_json::to_value(meta)?;
		for (position, tx) in txs.iter().enumerate() {
			let publication = position + 1 == txs.len();
			self.upsert_nursery_tx_with_attempt(tx, NurseryTxKind::Internal, target, publication.then_some(&value)).await?;
			let row = self.query_one("SELECT tx,kind::text,urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE txid=$1",
				&[&tx.compute_txid().to_string()]).await?;
			ensure!(row.get::<_, Vec<u8>>("tx") == serialize(tx) && row.get::<_, &str>("kind") == "internal" &&
				if publication {
					serde_json::from_str::<serde_json::Value>(row.get::<_, Option<&str>>("attempt").context("original nursery publication has no urgent metadata; relabel refused")?)? == value
				} else { row.get::<_, Option<&str>>("attempt").is_none() }, "urgent retained outbox conflicts with original attempt");
		}
		Ok(())
	}

	pub(crate) async fn urgent_publications(&self, budget: BackingSnapshotLimits) -> anyhow::Result<Vec<(UrgentPublication, Vec<Transaction>)>> {
		let size = self.query_one("SELECT count(*)::bigint AS rows, COALESCE(sum(octet_length(tx)+octet_length(urgent_fee_attempt::text)),0)::bigint AS bytes
			FROM nursery_tx WHERE urgent_fee_attempt IS NOT NULL", &[]).await?;
		ensure!(usize::try_from(size.get::<_, i64>("rows"))? <= budget.max_rows &&
			usize::try_from(size.get::<_, i64>("bytes"))? <= budget.max_serialized_bytes, "urgent publication inventory exceeds read bounds");
		let mut result = Vec::new();
		for row in self.query("SELECT DISTINCT urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE urgent_fee_attempt IS NOT NULL ORDER BY attempt", &[]).await? {
			let text: &str = row.get("attempt"); let meta: UrgentPublication = serde_json::from_str(text)?;
			let mut txs = Vec::new();
			for (position, txid) in meta.txids.iter().enumerate() {
				let stored = self.query_opt("SELECT tx,kind::text,urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE txid=$1", &[&txid.to_string()]).await?
					.context("urgent signed package member missing")?;
				ensure!(stored.get::<_, &str>("kind") == "internal" && if position + 1 == meta.txids.len() {
						serde_json::from_str::<UrgentPublication>(stored.get::<_, Option<&str>>("attempt").context("urgent publication metadata missing")?)? == meta
					} else { stored.get::<_, Option<&str>>("attempt").is_none() },
					"urgent package members have conflicting attempt identity");
				txs.push(deserialize(&stored.get::<_, Vec<u8>>("tx"))?);
			}
			meta.validate(&txs)?;
			let mut anchors = BTreeSet::new();
			for (input, prevout) in txs[0].input.iter().zip(&meta.prevouts[0]) {
				let source = self.get_bare_vtxo_by_id(input.previous_output.into()).await?.vtxo;
				ensure!(source.txout() == *prevout, "retained urgent source prevout differs");
				anchors.insert(source.chain_anchor());
			}
			ensure!(anchors.into_iter().collect::<Vec<_>>() == meta.source_anchors, "retained urgent source anchors differ");
			result.push((meta, txs));
		}
		Ok(result)
	}
}
