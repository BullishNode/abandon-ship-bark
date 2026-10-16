//! Bounded source inventory and geometry for supplied physical backing.
//! Source rows are an anchor superset. A geometric cut confers no ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Transaction, TxOut, Txid};
use bitcoin_ext::{BlockHeight, TxOutExt};
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

/// A caller-supplied transition anchor. Its exact output must be evidenced by
/// the full paths. Even zero-valued anchors must be explicit. This is no payee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackingPartitionResidual {
	pub outpoint: OutPoint,
	pub txout: TxOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackingRootPartition {
	pub root: VtxoId,
	pub terminals: Vec<VtxoId>,
	pub terminal_amount: Amount,
	pub anchor_residual_amount: Amount,
}

/// A complete geometric cut, not proof that the selected members are current
/// economic terminals. No ownership, signature, chain or retirement decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackingPartition {
	pub roots: Vec<BackingRootPartition>,
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

	/// Prove one disjoint, complete geometric cut through the supplied roots.
	/// Every evidenced sibling must continue to another evidenced transition or
	/// be one supplied terminal. Every P2A output must be an exact explicit
	/// residual. Current transitions conserve their whole input in outputs;
	/// unrepresented miner fees and unproved alternatives are refused.
	///
	/// A selected full ancestor alone can be a geometric cut. This does not
	/// prove that later economic claims do not exist. The caller must resolve
	/// current economic terminals independently before financial authorization.
	/// Row work counts roots, terminals, funding transactions, residuals and all
	/// terminal genesis items, including shared paths. Byte work counts terminal
	/// encodings, funding transactions and residual TxOut encodings.
	pub fn check_partition(
		&self,
		terminals: &[ServerVtxo<Full>],
		funding: &[Transaction],
		residuals: &[BackingPartitionResidual],
		limits: BackingSnapshotLimits,
	) -> anyhow::Result<BackingPartition> {
		ensure!(!self.roots.is_empty() && !terminals.is_empty(), "backing partition requires roots and terminals");
		ensure!(limits.max_rows > 0 && limits.max_serialized_bytes > 0, "backing partition budgets must be positive");
		let mut rows = self.roots.len().checked_add(terminals.len())
			.and_then(|n| n.checked_add(funding.len())).and_then(|n| n.checked_add(residuals.len()))
			.context("backing partition row budget overflow")?;
		let mut bytes = 0usize;
		for source in terminals {
			rows = rows.checked_add(usize::from(source.exit_depth())).context("backing partition row budget overflow")?;
			ensure!(rows <= limits.max_rows, "complete backing partition exceeds row budget");
			bytes = bytes.checked_add(source.serialize().len()).context("backing partition byte budget overflow")?;
			ensure!(bytes <= limits.max_serialized_bytes, "complete backing partition exceeds serialized byte budget");
		}
		for tx in funding {
			bytes = bytes.checked_add(bitcoin::consensus::serialize(tx).len()).context("backing partition byte budget overflow")?;
			ensure!(bytes <= limits.max_serialized_bytes, "complete backing partition exceeds serialized byte budget");
		}
		for residual in residuals {
			bytes = bytes.checked_add(bitcoin::consensus::serialize(&residual.txout).len())
				.context("backing partition byte budget overflow")?;
			ensure!(bytes <= limits.max_serialized_bytes, "complete backing partition exceeds serialized byte budget");
		}
		let anchors = funding.iter().map(|tx| (tx.compute_txid(), tx)).collect::<BTreeMap<_, _>>();
		ensure!(anchors.len() == funding.len(), "backing partition has duplicate funding transactions");
		let supplied = terminals.iter().map(|v| (v.point(), v)).collect::<BTreeMap<_, _>>();
		ensure!(supplied.len() == terminals.len(), "backing partition has duplicate terminals");
		ensure!(self.roots.iter().map(|r| r.point()).collect::<BTreeSet<_>>().len() == self.roots.len(),
			"backing partition has duplicate roots");
		let residual_map = residuals.iter().map(|r| (r.outpoint, &r.txout)).collect::<BTreeMap<_, _>>();
		ensure!(residual_map.len() == residuals.len(), "backing partition has duplicate residuals");
		ensure!(residuals.iter().all(|r| r.txout.is_p2a_fee_anchor()), "backing partition residual must be an exact P2A anchor");
		let mut used_funding = BTreeSet::new();
		let mut transitions = BTreeMap::<OutPoint, (TxOut, Transaction)>::new();
		for source in terminals {
			let anchor = anchors.get(&source.chain_anchor().txid).context("backing partition funding is missing")?;
			let root_id = self.path_root(source, anchor)?.context("backing partition terminal is outside supplied roots")?;
			let root = self.roots.iter().find(|r| r.id() == root_id).context("backing partition root is missing")?;
			used_funding.insert(source.chain_anchor().txid);
			let mut point = source.chain_anchor();
			let mut output = anchor.output[point.vout as usize].clone();
			let mut inside = false;
			for item in source.transactions() {
				inside |= point == root.point();
				if inside {
					let mut tx = item.tx.clone();
					// Witnesses do not determine partition geometry. They require
					// separate authorization and signature validation by the caller.
					for input in &mut tx.input { input.witness.clear(); }
					if let Some(previous) = transitions.get(&point) {
						ensure!(previous == &(output.clone(), tx), "backing partition has alternative transitions");
					} else {
						transitions.insert(point, (output.clone(), tx));
					}
				}
				point = OutPoint::new(item.tx.compute_txid(), u32::try_from(item.output_idx)?);
				output = item.tx.output[item.output_idx].clone();
			}
		}
		ensure!(used_funding.len() == anchors.len(), "backing partition has extra funding transactions");
		let mut visited = BTreeSet::new();
		let mut used_terminals = BTreeSet::new();
		let mut used_residuals = BTreeSet::new();
		let mut used_transitions = BTreeSet::new();
		let mut partitions = Vec::with_capacity(self.roots.len());
		for root in &self.roots {
			let mut partition = BackingRootPartition { root: root.id(), terminals: Vec::new(),
				terminal_amount: Amount::ZERO, anchor_residual_amount: Amount::ZERO };
			let mut pending = vec![(root.point(), root.txout())];
			while let Some((point, output)) = pending.pop() {
				ensure!(visited.insert(point), "backing partition roots or branches overlap");
				if let Some(source) = supplied.get(&point) {
					ensure!(!transitions.contains_key(&point), "backing partition has ancestor and descendant terminals");
					ensure!(source.txout() == output, "backing partition terminal output differs from path");
					used_terminals.insert(point);
					partition.terminals.push(source.id());
					partition.terminal_amount = partition.terminal_amount.checked_add(output.value)
						.context("backing partition terminal amount overflow")?;
				} else if output.is_p2a_fee_anchor() {
					ensure!(residual_map.get(&point).is_some_and(|supplied| **supplied == output),
						"backing partition has a missing or mismatched P2A residual");
					used_residuals.insert(point);
					partition.anchor_residual_amount = partition.anchor_residual_amount.checked_add(output.value)
						.context("backing partition anchor amount overflow")?;
				} else {
					let (prevout, tx) = transitions.get(&point).context("backing partition has an uncovered output")?;
					ensure!(*prevout == output && tx.input.len() == 1 && tx.input[0].previous_output == point,
						"backing partition transition input differs from path");
					let sum = tx.output.iter().try_fold(Amount::ZERO, |sum, out| sum.checked_add(out.value))
						.context("backing partition transition output amount overflow")?;
					ensure!(sum == output.value, "backing partition transition has an unrepresented fee or amount mismatch");
					used_transitions.insert(point);
					let txid = tx.compute_txid();
					for (vout, out) in tx.output.iter().enumerate() {
						pending.push((OutPoint::new(txid, u32::try_from(vout)?), out.clone()));
					}
				}
			}
			ensure!(!partition.terminals.is_empty(), "backing partition has an unmapped root");
			ensure!(partition.terminal_amount.checked_add(partition.anchor_residual_amount) == Some(root.amount()),
				"backing partition root amount is not conserved");
			partition.terminals.sort();
			partitions.push(partition);
		}
		ensure!(used_terminals.len() == supplied.len() && used_transitions.len() == transitions.len(),
			"backing partition has extra terminals or transitions");
		ensure!(used_residuals.len() == residual_map.len(), "backing partition has extra P2A residuals");
		partitions.sort_by_key(|p| p.root);
		Ok(BackingPartition { roots: partitions })
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

/// Original private native bodies and their exact original funding transaction.
/// Missing Full paths are errors, never reconstructed from bare user metadata.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedBoardPath {
	pub user_vtxo: Vec<u8>,
	pub funding_transaction: Vec<u8>,
	pub physical_root: Vec<u8>,
}

/// Validate genuine signed Board paths BEFORE using native Board construction
/// or geometric helpers. The result is geometry/amounts ONLY: it does not prove
/// source completeness, current liabilities, maturity, ownership or retirement.
pub(crate) fn validate_captured_board_geometry(paths: &[CapturedBoardPath],
	server: bitcoin::secp256k1::PublicKey, limits: BackingSnapshotLimits,
) -> anyhow::Result<BackingPartition> {
	use bitcoin::consensus::{deserialize, serialize};
	ensure!(!paths.is_empty() && limits.max_rows > 0 && limits.max_serialized_bytes > 0
		&& limits.max_rows <= crate::payout_witness::DIRECT_HISTORY_LIMITS.max_rows
		&& limits.max_serialized_bytes <= crate::payout_witness::DIRECT_HISTORY_LIMITS.max_serialized_bytes,
		"captured Board geometry requires finite native bounds");
	let mut bytes = 0usize;
	ensure!(paths.len() <= limits.max_rows, "captured Board paths exceed row bound");
	for path in paths {
		bytes = bytes.checked_add(path.user_vtxo.len()).and_then(|n| n.checked_add(path.funding_transaction.len()))
			.and_then(|n| n.checked_add(path.physical_root.len())).context("captured Board byte overflow")?;
		ensure!(bytes <= limits.max_serialized_bytes, "complete captured Board bodies exceed native bound");
	}
	let mut roots = Vec::new(); let mut terminals = Vec::new();
	let mut funding = BTreeMap::new(); let mut residuals = Vec::new();
	for path in paths {
		let full = ServerVtxo::<Full>::deserialize(&path.user_vtxo)?;
		ensure!(full.serialize() == path.user_vtxo && full.server_pubkey() == server,
			"captured Board native Full body or server differs");
		let anchor: Transaction = deserialize(&path.funding_transaction)?;
		ensure!(serialize(&anchor) == path.funding_transaction && anchor.compute_txid() == full.chain_anchor().txid,
			"captured Board original funding bytes differ");
		full.validate(&anchor).context("captured Board full signed path is invalid or incomplete")?;
		let user = full.clone().try_into_user_vtxo().map_err(|_| anyhow::anyhow!("captured Board path is not a native user VTXO"))?;
		let constructed = ark::board::BoardBuilder::new_from_vtxo(&user, &anchor, server)?
			.build_server_vtxos().into_iter().next().context("native Board physical root is missing")?;
		let root = ServerVtxo::<Full>::deserialize(&path.physical_root)?;
		ensure!(root.serialize() == path.physical_root && matches!(root.policy(), ServerVtxoPolicy::Expiry(_))
			&& root.to_bare().serialize() == constructed.to_bare().serialize(),
			"captured physical Board expiry root differs from actual native construction");
		root.validate(&anchor).context("captured physical Board root is invalid")?;
		if let Some(previous) = funding.insert(anchor.compute_txid(), anchor.clone()) {
			ensure!(serialize(&previous) == serialize(&anchor), "captured Board funding witness differs between paths");
		}
		for item in full.transactions() {
			let txid = item.tx.compute_txid();
			for (vout, txout) in item.tx.output.into_iter().enumerate().filter(|(_,out)| out.is_p2a_fee_anchor()) {
				residuals.push(BackingPartitionResidual { outpoint: OutPoint::new(txid,u32::try_from(vout)?), txout });
			}
		}
		roots.push(root.to_bare()); terminals.push(full);
	}
	let snapshot = BackingSnapshot { roots, regular:Vec::new(),pending_boards:Vec::new(),serialized_bytes:bytes };
	let funding = funding.into_values().collect::<Vec<_>>();
	for source in &terminals {
		let anchor = funding.iter().find(|tx| tx.compute_txid() == source.chain_anchor().txid).context("captured Board anchor missing")?;
		ensure!(snapshot.path_root(source,anchor)?.is_some(), "captured Board path is outside physical root");
	}
	snapshot.check_partition(&terminals,&funding,&residuals,limits)
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
	use ark::board::BoardBuilder;

	fn captured_board_path() -> CapturedBoardPath {
		let vector = &*VTXO_VECTORS; let board = &vector.board_vtxo;
		let root = BoardBuilder::new_from_vtxo(board,&vector.anchor_tx,board.server_pubkey())
			.unwrap().build_server_vtxos().remove(0);
		CapturedBoardPath { user_vtxo:ServerVtxo::from(board.clone()).serialize(),
			funding_transaction:bitcoin::consensus::serialize(&vector.anchor_tx),physical_root:root.serialize() }
	}
	#[test]
	fn captured_board_geometry_uses_validated_native_full_and_exact_physical_padding() {
		let path = captured_board_path(); let board = &VTXO_VECTORS.board_vtxo;
		let limits = BackingSnapshotLimits {max_rows:100,max_serialized_bytes:1_000_000};
		let partition = validate_captured_board_geometry(&[path.clone()],board.server_pubkey(),limits).unwrap();
		assert_eq!(partition.roots.len(),1);
		assert_eq!(partition.roots[0].root,ServerVtxo::<Full>::deserialize(&path.physical_root).unwrap().id());
		assert_eq!(partition.roots[0].terminal_amount,board.amount());
		assert_eq!(partition.roots[0].anchor_residual_amount,Amount::from_sat(330));
		let full = ServerVtxo::<Full>::deserialize(&path.physical_root).unwrap();
		assert_eq!(full.amount(),board.amount()+Amount::from_sat(330));
		assert!(validate_captured_board_geometry(&[path.clone(),path],board.server_pubkey(),limits).is_err());
	}
	#[test]
	fn captured_board_geometry_refuses_missing_full_wrong_root_funding_and_server() {
		let board = &VTXO_VECTORS.board_vtxo;
		let limits = BackingSnapshotLimits {max_rows:100,max_serialized_bytes:1_000_000};
		for which in 0..4 {
			let mut path = captured_board_path();
			match which {
				0 => path.user_vtxo = ServerVtxo::from(board.to_bare()).serialize(),
				1 => path.physical_root = path.user_vtxo.clone(),
				2 => { let mut funding = VTXO_VECTORS.anchor_tx.clone();funding.output[0].value+=Amount::from_sat(1);
					path.funding_transaction=bitcoin::consensus::serialize(&funding); },
				_ => { path.user_vtxo.pop(); },
			}
			assert!(validate_captured_board_geometry(&[path],board.server_pubkey(),limits).is_err());
		}
		let other = bitcoin::secp256k1::Keypair::new(&crate::SECP,&mut bitcoin::secp256k1::rand::thread_rng()).public_key();
		assert!(validate_captured_board_geometry(&[captured_board_path()],other,limits).is_err());
		let mut changed = captured_board_path();
		let exit = board.transactions().next().unwrap().tx;
		let signature = exit.input[0].witness.iter().next().unwrap();
		assert_eq!(signature.len(),64);
		let start = changed.user_vtxo.windows(signature.len()).position(|bytes|bytes==signature).unwrap();
		changed.user_vtxo[start+63]^=1;
		let corrupt = ServerVtxo::<Full>::deserialize(&changed.user_vtxo).unwrap();
		corrupt.validate_unsigned(&VTXO_VECTORS.anchor_tx).unwrap();
		assert!(corrupt.validate(&VTXO_VECTORS.anchor_tx).is_err());
		let error = validate_captured_board_geometry(&[changed],board.server_pubkey(),limits).unwrap_err();
		assert!(format!("{error:#}").contains("full signed path is invalid"));
	}

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

	fn board_partition_fixture() -> (BackingSnapshot, ServerVtxo<Full>, Transaction, Vec<BackingPartitionResidual>) {
		let vector = &*VTXO_VECTORS;
		let board = &vector.board_vtxo;
		let root = BoardBuilder::new_from_vtxo(board, &vector.anchor_tx, board.server_pubkey())
			.unwrap().build_server_vtxos().remove(0).into_bare();
		let source = ServerVtxo::from(board.clone());
		let residuals = source.transactions().flat_map(|item| {
			let txid = item.tx.compute_txid();
			item.tx.output.into_iter().enumerate().filter(|(_, out)| out.is_p2a_fee_anchor())
				.map(move |(vout, txout)| BackingPartitionResidual { outpoint: OutPoint::new(txid, vout as u32), txout })
		}).collect();
		(BackingSnapshot { roots: vec![root], regular: vec![], pending_boards: vec![], serialized_bytes: 0 },
			source, vector.anchor_tx.clone(), residuals)
	}

	#[test]
	fn backing_partition_exact_board_anchor_conservation_and_refusal() {
		let (snapshot, source, funding, residuals) = board_partition_fixture();
		let limits = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
		let partition = snapshot.check_partition(&[source.clone()], &[funding.clone()], &residuals, limits).unwrap();
		assert_eq!(partition.roots[0].terminal_amount, source.amount());
		assert_eq!(partition.roots[0].anchor_residual_amount, Amount::from_sat(330));
		assert_eq!(source.amount() + Amount::from_sat(330), snapshot.roots[0].amount());
		assert!(snapshot.check_partition(&[source.clone()], &[funding.clone()], &[], limits).is_err());
		let mut wrong = residuals.clone();
		wrong[0].txout.value += Amount::from_sat(1);
		assert!(snapshot.check_partition(&[source.clone()], &[funding.clone()], &wrong, limits).is_err());
		wrong = residuals.clone();
		wrong[0].outpoint.vout += 1;
		assert!(snapshot.check_partition(&[source.clone()], &[funding.clone()], &wrong, limits).is_err());
		wrong = residuals.clone(); wrong.extend(residuals.clone());
		assert!(snapshot.check_partition(&[source.clone()], &[funding.clone()], &wrong, limits).is_err());
		let mut wrong_funding = funding.clone(); wrong_funding.output[0].value += Amount::from_sat(1);
		assert!(snapshot.check_partition(&[source.clone()], &[wrong_funding], &residuals, limits).is_err());
		let board = &VTXO_VECTORS.board_vtxo;
		let user = BoardBuilder::new(board.user_pubkey(), board.expiry_height(), board.server_pubkey(), board.exit_delta())
			.set_funding_details(snapshot.roots[0].amount(), Amount::from_sat(330), board.chain_anchor()).unwrap()
			.generate_user_nonces();
		let bad_root = BoardBuilder::new_for_cosign(board.user_pubkey(), board.expiry_height(), board.server_pubkey(),
			board.exit_delta(), snapshot.roots[0].amount() + Amount::from_sat(1), Amount::from_sat(330),
			board.chain_anchor(), *user.user_pub_nonce()).build_unsigned_vtxos().unwrap().0.into_bare();
		assert_eq!(bad_root.point(), snapshot.roots[0].point());
		let wrong_metadata = BackingSnapshot { roots: vec![bad_root], regular: vec![], pending_boards: vec![], serialized_bytes: 0 };
		let error = wrong_metadata.check_partition(&[source.clone()], &[funding.clone()], &residuals, limits).unwrap_err();
		assert!(format!("{error:#}").contains("root metadata differs"), "actual funding identity alone cannot authorize different TxOut metadata");

		assert!(snapshot.check_partition(&[source.clone(), source.clone()], &[funding.clone()], &residuals, limits).is_err());
		let rows = BackingSnapshotLimits { max_rows: 4, ..limits };
		assert!(snapshot.check_partition(&[source.clone()], &[funding.clone()], &residuals, rows).is_err());
		let bytes = source.serialize().len() + bitcoin::consensus::serialize(&funding).len() +
			residuals.iter().map(|r| bitcoin::consensus::serialize(&r.txout).len()).sum::<usize>();
		assert!(snapshot.check_partition(&[source.clone()], &[funding.clone()], &residuals,
			BackingSnapshotLimits { max_rows: 5, max_serialized_bytes: bytes }).is_ok());
		assert!(snapshot.check_partition(&[source], &[funding], &residuals,
			BackingSnapshotLimits { max_serialized_bytes: bytes - 1, ..limits }).is_err());
	}

	#[test]
	fn backing_partition_rejects_actual_unsigned_alternative_board_shapes() {
		let (snapshot, source, funding, residuals) = board_partition_fixture();
		let board = &VTXO_VECTORS.board_vtxo;
		let user = BoardBuilder::new(board.user_pubkey(), board.expiry_height(), board.server_pubkey(), board.exit_delta())
			.set_funding_details(snapshot.roots[0].amount(), Amount::from_sat(331), board.chain_anchor()).unwrap()
			.generate_user_nonces();
		let alternative = BoardBuilder::new_for_cosign(board.user_pubkey(), board.expiry_height(), board.server_pubkey(),
			board.exit_delta(), snapshot.roots[0].amount(), Amount::from_sat(331), board.chain_anchor(), *user.user_pub_nonce())
			.build_unsigned_vtxos().unwrap().1;
		alternative.validate_unsigned(&funding).unwrap();
		let error = snapshot.check_partition(&[source, ServerVtxo::from(alternative)], &[funding], &residuals,
			BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 }).unwrap_err();
		assert!(format!("{error:#}").contains("alternative transitions"));
	}

}
