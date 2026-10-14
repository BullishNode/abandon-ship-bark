//! Scheduled discovery of complete source cuts. Freezing is not payment.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use ark::{ProtocolEncoding, ServerVtxo, VtxoId};
use crate::bitcoind;
use crate::database::carrier_snapshot::{BackingSnapshot, BackingSnapshotLimits};
use crate::Server;

const LIMITS: BackingSnapshotLimits = BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 16 * 1024 * 1024 };

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
	pub enabled: bool,
	#[serde(with = "crate::utils::serde::duration")]
	pub interval: Duration,
	pub grace_blocks: u32,
	pub max_groups_per_tick: usize,
}

impl Default for Config {
	fn default() -> Self {
		Self { enabled: false, interval: Duration::from_secs(30), grace_blocks: 6, max_groups_per_tick: 32 }
	}
}

impl Config {
	pub(crate) fn validate(&self, legacy_enabled: bool) -> anyhow::Result<()> {
		if !self.enabled { return Ok(()); }
		ensure!(!legacy_enabled, "direct_expiry cannot run concurrently with legacy expiry_payout");
		ensure!(!self.interval.is_zero() && self.max_groups_per_tick > 0 && self.max_groups_per_tick <= LIMITS.max_rows,
			"direct_expiry requires a positive interval and bounded positive group count");
		Ok(())
	}
}

// Same physical frontier predicate as the upstream watchman, including its
// unfrontiered funding records. Only the scheduling page is bounded: each
// chosen anchor must pass a complete physical/source preflight independently.
const INVENTORY: &str = "SELECT v.vtxo_id, v.anchor_point, v.expiry, v.vtxo FROM vtxo v
	WHERE v.onchain_spent_height IS NULL
	AND (v.frontier_at IS NOT NULL OR EXISTS (SELECT 1 FROM virtual_transaction t
		WHERE t.txid=v.vtxo_txid AND t.is_funding))
	AND NOT EXISTS (SELECT 1 FROM expiry_allocation a
		WHERE a.id=v.vtxo_id OR v.vtxo_id=ANY(a.retirement_ids))";

/// Legacy rows can still be recovered by the nursery with its producer disabled.
/// Refuse that retained route before starting any worker in direct source mode.
pub(crate) async fn require_no_legacy_payout(db: &crate::database::Db, enabled: bool) -> anyhow::Result<()> {
	if !enabled { return Ok(()); }
	let retained: bool = db.read(async |t| Ok(t.query_one(
		"SELECT EXISTS(SELECT 1 FROM expiry_settlement)
			OR EXISTS(SELECT 1 FROM nursery_tx WHERE kind::text='expiry-payout')", &[],
	).await?.get(0))).await?;
	ensure!(!retained, "direct_expiry refuses retained legacy expiry_payout rows; reconcile the existing route before enabling direct source freezing");
	Ok(())
}

impl Server {
	pub(crate) fn start_direct_expiry_task(self: &Arc<Self>) {
		if !self.config.direct_expiry.enabled { return; }
		let server = self.clone();
		let worker = self.rtmgr.spawn("DirectExpiryFreeze");
		tokio::spawn(async move {
			let _worker = worker;
			let mut cursor = None;
			loop {
				// Do not cancel a guarded financial commit on timer/shutdown.
				// The API retains payment/source guards through its DB commit.
				if let Err(error) = server.direct_expiry_tick(&mut cursor).await {
					warn!("direct expiry complete preflight deferred: {error:#}");
				}
				tokio::select! {
					_ = server.rtmgr.shutdown_signal() => break,
					_ = tokio::time::sleep(server.config.direct_expiry.interval) => {},
				}
			}
		});
	}

	async fn direct_expiry_tick(&self, cursor: &mut Option<String>) -> anyhow::Result<()> {
		// Existing frozen ownership wins. Read canonical complete plans rather
		// than resolving them again; confirmed winners are never new work.
		let existing = self.db.frozen_expiry_group_ids(LIMITS.max_rows).await?;
		for id in &existing { self.db.read_frozen_expiry_group(id, self.config.network, LIMITS).await?; }
		let anchors: Vec<String> = self.db.read(async |t| {
			let sql = format!("WITH inventory AS ({INVENTORY})
				SELECT DISTINCT anchor_point, (anchor_point > $1) AS after_cursor FROM inventory
				ORDER BY after_cursor DESC, anchor_point LIMIT $2");
			Ok(t.query(&sql, &[&cursor.as_deref().unwrap_or(""), &i64::try_from(self.config.direct_expiry.max_groups_per_tick)?]).await?
				.into_iter().map(|r| r.get(0)).collect())
		}).await?;
		let tip = bitcoind::tip(&self.bitcoind).await?;
		let mut frozen = 0;
		let mut waiting = 0;
		for anchor in &anchors {
			*cursor = Some(anchor.clone()); // A blocked/oversized cut advances the fair cursor.
			let result = async {
				let roots = self.db.read(async |t| {
					let bound = i64::try_from(LIMITS.max_rows)?.checked_add(1).context("root budget overflow")?;
					let sql = format!("WITH inventory AS ({INVENTORY}), bounded AS (
						SELECT vtxo_id, anchor_point, expiry, vtxo FROM inventory WHERE anchor_point=$1 ORDER BY vtxo_id LIMIT $2
					), sized AS (
						SELECT vtxo_id, anchor_point, expiry, vtxo, count(*) OVER () AS row_count,
							sum(octet_length(vtxo)::bigint) OVER ()::bigint AS byte_count FROM bounded
					) SELECT vtxo_id, anchor_point, expiry, row_count, byte_count,
						CASE WHEN row_count < $2 AND byte_count <= $3 THEN vtxo ELSE NULL END AS vtxo FROM sized");
					let rows = t.query(&sql, &[&anchor, &bound, &i64::try_from(LIMITS.max_serialized_bytes)?]).await?;
					let mut roots = Vec::new();
					for row in rows {
						ensure!(usize::try_from(row.get::<_, i64>("row_count"))? <= LIMITS.max_rows
							&& usize::try_from(row.get::<_, i64>("byte_count"))? <= LIMITS.max_serialized_bytes,
							"complete direct expiry physical group exceeds budget");
						let root = ServerVtxo::deserialize(row.get::<_, &[u8]>("vtxo"))?;
						ensure!(root.id().to_string() == row.get::<_, &str>("vtxo_id")
							&& root.chain_anchor().to_string() == row.get::<_, &str>("anchor_point")
							&& root.expiry_height().to_u32() == u32::try_from(row.get::<_, i32>("expiry"))?,
							"direct expiry physical root metadata differs from encoding");
						roots.push(root);
					}
					ensure!(!roots.is_empty(), "direct expiry physical group changed during enumeration");
					Ok(roots)
				}).await?;
				ensure!(roots.iter().all(|r| u64::from(r.expiry_height().to_u32()) + u64::from(self.config.direct_expiry.grace_blocks)
					<= u64::from(tip.height.to_u32())), "physical source group is not mature");
				let ids = roots.iter().map(ServerVtxo::id).collect::<Vec<_>>();
				let terminals = self.db.read(async |t| {
					t.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[]).await?;
					current_terminals(&t.backing_snapshot(&roots, LIMITS).await?)
				}).await?;
				self.freeze_current_expiry_backing(&ids, &terminals, LIMITS).await
			}.await;
			match result {
				Ok(plan) => { frozen += 1; info!(%anchor, freeze_set = %plan.id, "direct expiry source cut frozen; payment pending"); },
				Err(error) => { waiting += 1; warn!(%anchor, "direct expiry source cut deferred: {error:#}"); },
			}
		}
		info!(existing = existing.len(), physical_groups_visited = anchors.len(), frozen, waiting, "direct expiry source tick");
		Ok(())
	}
}

fn current_terminals(snapshot: &BackingSnapshot) -> anyhow::Result<Vec<VtxoId>> {
	let mut ancestors = BTreeSet::new();
	for source in snapshot.regular.iter().filter_map(|f| f.full_vtxo.as_ref()) {
		for tx in source.transactions() { ancestors.extend(tx.tx.input.into_iter().map(|i| VtxoId::from(i.previous_output))); }
	}
	for source in &snapshot.pending_boards {
		for tx in source.vtxo.transactions() { ancestors.extend(tx.tx.input.into_iter().map(|i| VtxoId::from(i.previous_output))); }
	}
	let mut terminals = BTreeSet::new();
	for source in &snapshot.regular {
		if ancestors.contains(&source.state.vtxo_id) { continue; }
		ensure!(source.full_vtxo.is_some(), "current source {} has no stored full path ({:?}, {:?})",
			source.state.vtxo_id, source.state.spend_state, source.genesis);
		terminals.insert(source.state.vtxo_id);
	}
	for source in &snapshot.pending_boards {
		if !ancestors.contains(&source.vtxo.id()) { terminals.insert(source.vtxo.id()); }
	}
	ensure!(!terminals.is_empty(), "complete current source cut is empty");
	Ok(terminals.into_iter().collect())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn direct_source_mode_requires_exclusive_positive_schedule() {
		let mut config = Config { enabled: true, ..Config::default() };
		assert!(config.validate(true).is_err());
		assert!(config.validate(false).is_ok());
		config.interval = Duration::ZERO;
		assert!(config.validate(false).is_err());
		config.interval = Duration::from_secs(1);
		config.max_groups_per_tick = 0;
		assert!(config.validate(false).is_err());
	}
}
