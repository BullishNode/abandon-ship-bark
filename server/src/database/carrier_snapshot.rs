//! Bounded source inventory for a supplied physical backing set.
//! This is an anchor superset, never an ownership or covered-leaf decision.

use std::collections::BTreeSet;
use std::str::FromStr;

use anyhow::Context;
use bitcoin::{OutPoint, Transaction, Txid};
use bitcoin_ext::BlockHeight;
use chrono::{DateTime, Utc};
use tokio_postgres::Row;

use ark::{ProtocolDecodingError, ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, Vtxo, VtxoId};
use ark::vtxo::{Bare, Full, VtxoValidationError};

use super::{Tx, VtxoState};

#[derive(Debug, Clone, Copy)]
pub struct BackingSnapshotLimits {
	pub max_rows: usize,
	/// Sum of the stored VTXO encodings (including pending Board encodings).
	pub max_serialized_bytes: usize,
}

/// Availability of encoded genesis, not validity or physical containment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotGenesis {
	/// The row represents its own chain anchor; no intermediate path is needed.
	Anchor,
	/// An encoded path is available for the caller to validate against funding.
	EncodedPath,
	/// Bare intermediate metadata cannot prove which descendants it consumes.
	MissingPath,
}

pub struct BackingVtxoFact {
	pub state: VtxoState<Bare, ServerVtxoPolicy>,
	/// Present only when the actual stored encoding supplies a complete path.
	/// Availability still does not prove signatures or physical containment.
	pub full_vtxo: Option<ServerVtxo<Full>>,
	pub genesis: SnapshotGenesis,
	pub frontier_at: Option<DateTime<Utc>>,
	pub onchain_spent_height: Option<BlockHeight>,
	pub onchain_spent_txid: Option<Txid>,
	pub lightning_htlc_subscription_id: Option<i64>,
}

pub struct BackingPendingBoardFact {
	pub anchor: OutPoint,
	pub vtxo: Vtxo,
	pub genesis: SnapshotGenesis,
}

/// Complete known rows for the queried anchors, including spent and intermediate
/// representations. A progressed backing's anchor lookup is only a superset:
/// the caller must validate actual full genesis/path containment before treating
/// any row as a financially covered leaf. MissingPath is never operator evidence.
/// Frozen allocations take precedence over this source inventory in the resolver.
pub struct BackingSnapshot {
	pub roots: Vec<ServerVtxo>,
	pub regular: Vec<BackingVtxoFact>,
	pub pending_boards: Vec<BackingPendingBoardFact>,
	pub serialized_bytes: usize,
}

impl BackingSnapshot {
	/// Match one complete source path to its supplied physical root. This proves
	/// geometry and amounts only: ownership, signatures, maturity, current spends
	/// and complete economic retirement remain the resolver's responsibility.
	/// An incomplete source must not be passed as an invented full VTXO.
	pub fn path_root(
		&self, source: &ServerVtxo<Full>, funding: &Transaction,
	) -> anyhow::Result<Option<VtxoId>> {
		ensure!(funding.compute_txid() == source.chain_anchor().txid,
			"backing path funding transaction differs from its anchor");
		source.validate_unsigned(funding).context("invalid backing source path")?;
		let path = std::iter::once((source.chain_anchor(),
			funding.output.get(source.chain_anchor().vout as usize)
				.context("backing anchor output is missing")?.clone()))
			.chain(source.transactions().map(|item| (
				OutPoint::new(item.tx.compute_txid(), item.output_idx as u32),
				item.tx.output[item.output_idx].clone(),
			))).collect::<Vec<_>>();
		let mut matched = None;
		for root in &self.roots {
			let Some((_, output)) = path.iter().find(|(point, _)| *point == root.point()) else { continue; };
			ensure!(root.chain_anchor() == source.chain_anchor() && root.txout() == *output,
				"backing root metadata differs from the actual source path");
			ensure!(matched.replace(root.id()).is_none(),
				"backing set contains overlapping roots on the same source path");
		}
		Ok(matched)
	}
}

// One statement sees one PostgreSQL snapshot even inside a READ COMMITTED Tx.
// Limit first, then count VTXO encoding lengths. Overbudget results omit blobs;
// a single oversized bytea must not be copied into the client before refusal.
const SNAPSHOT: &str = "
WITH inventory AS (
	SELECT false AS pending, vtxo_id, anchor_point, id, vtxo, expiry,
		oor_spent_txid, spent_in_round, offboarded_in, banned_until_height,
		confirmed_height, spend_state::text AS spend_state, created_at, updated_at,
		frontier_at, onchain_spent_height, onchain_spent_txid,
		lightning_htlc_subscription_id, vtxo_txid, exit_delta, policy_type, policy, server_pubkey, amount
	FROM vtxo WHERE anchor_point = ANY($1)
	UNION ALL
	SELECT true, p.vtxo_id, p.id, NULL::bigint, p.vtxo, p.expiry,
		NULL::text, NULL::bigint, NULL::text, NULL::int4,
		NULL::int4, NULL::text, NULL::timestamptz, NULL::timestamptz,
		NULL::timestamptz, NULL::int4, NULL::text, NULL::bigint,
		NULL::text, NULL::int4, NULL::text, NULL::bytea, NULL::text, NULL::bigint
	FROM pending_board p WHERE p.id = ANY($1)
	AND NOT EXISTS (SELECT 1 FROM vtxo v WHERE v.vtxo_id = p.vtxo_id)
), bounded AS (
	SELECT pending, vtxo_id, anchor_point, id, vtxo, expiry,
		oor_spent_txid, spent_in_round, offboarded_in, banned_until_height,
		confirmed_height, spend_state, created_at, updated_at, frontier_at,
		onchain_spent_height, onchain_spent_txid, lightning_htlc_subscription_id,
		vtxo_txid, exit_delta, policy_type, policy, server_pubkey, amount
	FROM inventory ORDER BY anchor_point, vtxo_id, pending LIMIT $2
), sized AS (
	SELECT pending, vtxo_id, anchor_point, id, vtxo, expiry,
		oor_spent_txid, spent_in_round, offboarded_in, banned_until_height,
		confirmed_height, spend_state, created_at, updated_at, frontier_at,
		onchain_spent_height, onchain_spent_txid, lightning_htlc_subscription_id,
		vtxo_txid, exit_delta, policy_type, policy, server_pubkey, amount,
		count(*) OVER () AS row_count,
		sum(octet_length(vtxo)::bigint) OVER ()::bigint AS byte_count FROM bounded
)
SELECT pending, vtxo_id, anchor_point, id,
	CASE WHEN row_count < $2 AND byte_count <= $3 THEN vtxo ELSE NULL END AS vtxo,
	expiry, oor_spent_txid, spent_in_round, offboarded_in, banned_until_height,
	confirmed_height, spend_state, created_at, updated_at, frontier_at,
	onchain_spent_height, onchain_spent_txid, lightning_htlc_subscription_id,
	vtxo_txid, exit_delta, policy_type,
	CASE WHEN row_count < $2 AND byte_count <= $3 THEN policy ELSE NULL END AS policy,
	server_pubkey, amount, row_count, byte_count
FROM sized ORDER BY anchor_point, vtxo_id, pending";

impl Tx<'_> {
	/// Read the whole bounded anchor inventory or refuse it. No partial payable
	/// prefix, ownership inference, row locks or source-state mutation occurs.
	/// The root resolver must expand cross-anchor dependencies and revalidate
	/// under its decisive guards before authorizing a carrier.
	pub async fn backing_snapshot(
		&self, roots: &[ServerVtxo], limits: BackingSnapshotLimits,
	) -> anyhow::Result<BackingSnapshot> {
		ensure!(!roots.is_empty(), "backing snapshot requires physical roots");
		ensure!(limits.max_rows > 0 && limits.max_serialized_bytes > 0, "backing snapshot budgets must be positive");
		ensure!(roots.len() <= limits.max_rows, "backing snapshot roots exceed row budget");
		let points = roots.iter().map(|r| r.point()).collect::<BTreeSet<_>>();
		ensure!(points.len() == roots.len(), "backing snapshot has duplicate physical roots");
		let anchors = roots.iter().map(|r| r.chain_anchor().to_string()).collect::<BTreeSet<_>>()
			.into_iter().collect::<Vec<_>>();
		let limit = i64::try_from(limits.max_rows)?.checked_add(1).context("backing snapshot row budget overflow")?;
		let bytes = i64::try_from(limits.max_serialized_bytes)?;
		let rows = self.query(SNAPSHOT, &[&anchors, &limit, &bytes]).await?;
		let first = rows.first().context("backing snapshot is incomplete: no source inventory")?;
		let count = usize::try_from(first.get::<_, i64>("row_count"))?;
		let serialized_bytes = usize::try_from(first.get::<_, i64>("byte_count"))?;
		ensure!(count <= limits.max_rows, "complete backing snapshot exceeds row budget");
		ensure!(serialized_bytes <= limits.max_serialized_bytes, "complete backing snapshot exceeds serialized byte budget");
		let mut snapshot = BackingSnapshot {
			roots: roots.to_vec(), regular: Vec::new(), pending_boards: Vec::new(), serialized_bytes,
		};
		for row in rows {
			let id = VtxoId::from_str(row.get::<_, &str>("vtxo_id"))?;
			let anchor = OutPoint::from_str(row.get::<_, &str>("anchor_point"))?;
			if row.get::<_, bool>("pending") {
				let vtxo = Vtxo::deserialize(row.get::<_, &[u8]>("vtxo"))?;
				ensure!(vtxo.id() == id && vtxo.chain_anchor() == anchor &&
					vtxo.expiry_height().to_u32() == u32::try_from(row.get::<_, i32>("expiry"))?,
					"backing snapshot pending Board facts disagree");
				let genesis = genesis_availability(&vtxo);
				snapshot.pending_boards.push(BackingPendingBoardFact { anchor, vtxo, genesis });
			} else {
				let frontier_at = row.get("frontier_at");
				let onchain_spent_height = optional_height(&row, "onchain_spent_height")?;
				let onchain_spent_txid = row.get::<_, Option<&str>>("onchain_spent_txid").map(Txid::from_str).transpose()?;
				let lightning_htlc_subscription_id = row.get("lightning_htlc_subscription_id");
				let data = row.get::<_, &[u8]>("vtxo");
				let encoded: ServerVtxo = ServerVtxo::deserialize(data)?;
				let (full_vtxo, genesis) = decode_genesis(data)?;
				ensure!(encoded.id() == id && encoded.chain_anchor() == anchor &&
					encoded.expiry_height().to_u32() == u32::try_from(row.get::<_, i32>("expiry"))? &&
					encoded.point().txid.to_string() == row.get::<_, &str>("vtxo_txid") &&
					i32::from(encoded.exit_delta().to_u16()) == row.get::<_, i32>("exit_delta") &&
					encoded.policy_type().to_string() == row.get::<_, &str>("policy_type") &&
					encoded.policy().serialize() == row.get::<_, &[u8]>("policy") &&
					encoded.server_pubkey().to_string() == row.get::<_, &str>("server_pubkey") &&
					encoded.amount().to_sat() == u64::try_from(row.get::<_, i64>("amount"))?,
					"backing snapshot source metadata disagrees with encoded VTXO");
				let state = VtxoState::<Bare, ServerVtxoPolicy>::try_from(row)?;
				ensure!(state.vtxo.id() == id && state.vtxo.chain_anchor() == anchor,
					"backing snapshot source facts disagree");
				snapshot.regular.push(BackingVtxoFact { state, full_vtxo, genesis, frontier_at,
					onchain_spent_height, onchain_spent_txid, lightning_htlc_subscription_id });
			}
		}
		for root in roots {
			let fact = snapshot.regular.iter().find(|f| f.state.vtxo.id() == root.id())
				.context("backing snapshot is incomplete: physical root has no source row")?;
			ensure!(fact.state.vtxo.to_bare().serialize() == root.serialize(),
				"backing snapshot supplied physical root disagrees with source");
		}
		Ok(snapshot)
	}
}

fn genesis_availability<P: ark::vtxo::Policy>(vtxo: &ark::Vtxo<Full, P>) -> SnapshotGenesis {
	if vtxo.exit_depth() > 0 { SnapshotGenesis::EncodedPath }
	else if vtxo.point() == vtxo.chain_anchor() { SnapshotGenesis::Anchor }
	else { SnapshotGenesis::MissingPath }
}

/// Missing path is an explicit source representation. Every other decode error
/// refuses the complete inventory; we never substitute a guessed genesis.
fn decode_genesis(bytes: &[u8]) -> anyhow::Result<(Option<ServerVtxo<Full>>, SnapshotGenesis)> {
	match ServerVtxo::<Full>::deserialize(bytes) {
		Ok(full) => {
			let availability = genesis_availability(&full);
			Ok((Some(full), availability))
		},
		Err(ProtocolDecodingError::Invalid { source: Some(source), .. })
			if matches!(source.downcast_ref::<VtxoValidationError>(), Some(VtxoValidationError::MissingGenesisItems)) =>
		{
			Ok((None, SnapshotGenesis::MissingPath))
		},
		Err(error) => Err(error.into()),
	}
}

fn optional_height(row: &Row, column: &str) -> anyhow::Result<Option<BlockHeight>> {
	row.get::<_, Option<i32>>(column).map(BlockHeight::try_from).transpose().context("invalid snapshot chain height")
}


#[cfg(test)]
mod test {
	use super::*;
	use ark::test_util::VTXO_VECTORS;

	#[test]
	fn backing_snapshot_genesis_availability_does_not_invent_bare_paths() {
		let full = ServerVtxo::from(VTXO_VECTORS.arkoor2_vtxo.clone());
		let (decoded, availability) = decode_genesis(&full.serialize()).unwrap();
		assert_eq!(availability, SnapshotGenesis::EncodedPath);
		assert!(decoded.is_some());
		let (decoded, availability) = decode_genesis(&full.to_bare().serialize()).unwrap();
		assert_eq!(availability, SnapshotGenesis::MissingPath);
		assert!(decoded.is_none());
		let board = ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone());
		let (decoded, availability) = decode_genesis(&board.to_bare().serialize()).unwrap();
		assert_eq!(availability, SnapshotGenesis::MissingPath);
		assert!(decoded.is_none(), "bare Board user output is not its funding anchor");
		let encoded = full.serialize();
		assert!(decode_genesis(&encoded[..encoded.len() - 1]).is_err(), "truncation is not missing-path evidence");
	}
}
