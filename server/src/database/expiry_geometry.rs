//! Current economic cuts through complete, evidenced expiry backing paths.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::OnceLock;

use anyhow::Context;
use bitcoin::{OutPoint, Transaction, Txid};
use bitcoin_ext::TxOutExt;

use ark::ServerVtxo;
use ark::vtxo::Full;
use super::expiry_inventory::{BackingPartition, BackingPartitionResidual, BackingSnapshot, BackingSnapshotLimits};

struct PathEntry {
	steps: Vec<(Txid, usize)>,
	unsigned_ok: OnceLock<()>,
}

/// Geometry belongs to these exact immutable paths and funding bytes, for one
/// anchor preparation only. No ownership or selected-partition verdict is kept.
pub(crate) struct BackingGeometry {
	snapshot: BackingSnapshot,
	funding: Transaction,
	transitions: BTreeMap<Txid, Transaction>,
	facts: BTreeMap<OutPoint, usize>,
	paths: Option<Vec<Option<PathEntry>>>,
}

pub(super) enum BackingPath<'a> {
	Native(&'a ServerVtxo<Full>),
	Cached(&'a BTreeMap<Txid, Transaction>, &'a [(Txid, usize)]),
}

impl<'a> BackingPath<'a> {
	pub(super) fn uncached(source: &'a ServerVtxo<Full>, funding: &Transaction) -> anyhow::Result<Self> {
		ensure!(funding.compute_txid() == source.chain_anchor().txid,
			"backing path funding transaction differs from its anchor");
		source.validate_unsigned(funding).context("invalid backing source path")?;
		Ok(Self::Native(source))
	}

	pub(super) fn iter(&self) -> Box<dyn Iterator<Item = (Cow<'_, Transaction>, usize)> + '_> {
		match self {
			Self::Native(source) => Box::new(source.transactions().map(|item| (Cow::Owned(item.tx), item.output_idx))),
			Self::Cached(transactions, steps) => Box::new(steps.iter().map(|(txid, output)| (Cow::Borrowed(&transactions[txid]), *output))),
		}
	}
}

impl BackingGeometry {
	pub(crate) fn new(snapshot: BackingSnapshot, funding: Transaction, limits: BackingSnapshotLimits) -> anyhow::Result<Self> {
		let mut paths = Vec::new();
		let slots_fit = snapshot.regular.len() <= limits.max_rows
			&& snapshot.regular.len().checked_mul(size_of::<Option<PathEntry>>())
				.is_some_and(|bytes| bytes <= limits.max_serialized_bytes)
			&& paths.try_reserve_exact(snapshot.regular.len()).is_ok();
		let mut cache_bytes = paths.capacity().checked_mul(size_of::<Option<PathEntry>>());
		let mut paths = if slots_fit && cache_bytes.is_some_and(|bytes| bytes <= limits.max_serialized_bytes) {
			paths.resize_with(snapshot.regular.len(), || None);
			Some(paths)
		} else { None };
		let transitions = collect_source_transitions(&snapshot, &funding, paths.as_deref_mut(), &mut cache_bytes, limits)?;
		let facts = snapshot.regular.iter().enumerate().map(|(index, fact)| (fact.state.vtxo.point(), index)).collect();
		Ok(Self { snapshot, funding, transitions, facts, paths })
	}

	pub(crate) fn snapshot(&self) -> &BackingSnapshot { &self.snapshot }
	pub(crate) fn funding(&self) -> &Transaction { &self.funding }

	pub(crate) fn economic_cut(&self, root: &ServerVtxo, limits: BackingSnapshotLimits) -> anyhow::Result<EconomicCut> {
		economic_cut_inner(&self.snapshot, root, &self.funding, &self.transitions, &self.facts, Some(self), limits)
	}

	pub(crate) fn check_partition(
		&self, roots: &[ServerVtxo], terminals: &[usize], residuals: &[BackingPartitionResidual], limits: BackingSnapshotLimits,
	) -> anyhow::Result<BackingPartition> {
		let sources = terminals.iter().map(|index| self.snapshot.regular[*index].full_vtxo.as_ref()
			.context("expiry current terminal lacks its complete source path")).collect::<anyhow::Result<Vec<_>>>()?;
		let selected = BackingSnapshot { roots: roots.to_vec(), regular: Vec::new(), serialized_bytes: 0 };
		selected.check_partition_inner(&sources, std::slice::from_ref(&self.funding), residuals, limits, Some((self, terminals)))
	}

	pub(super) fn path(&self, index: usize) -> anyhow::Result<BackingPath<'_>> {
		let full = self.snapshot.regular[index].full_vtxo.as_ref().context("expiry current terminal lacks its complete source path")?;
		ensure!(self.funding.compute_txid() == full.chain_anchor().txid,
			"backing path funding transaction differs from its anchor");
		let Some(entry) = self.paths.as_ref().and_then(|paths| paths[index].as_ref()) else {
			return BackingPath::uncached(full, &self.funding);
		};
		if entry.unsigned_ok.get().is_none() {
			full.validate_unsigned(&self.funding).context("invalid backing source path")?;
			let _ = entry.unsigned_ok.set(());
		}
		Ok(BackingPath::Cached(&self.transitions, &entry.steps))
	}
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EconomicCut {
	pub visited: Vec<usize>,
	pub terminals: Vec<usize>,
	pub residuals: Vec<BackingPartitionResidual>,
}

/// Use only the server's recorded current transitions. A valid signed path
/// alone does not prove that it is the current economic branch. In particular,
/// an old offboard or round input is a settlement obligation, not an ordinary
/// OOR branch: its forfeit transaction can have additional connector inputs.
#[cfg(test)]
pub(crate) fn economic_cut(
	snapshot: &BackingSnapshot,
	root: &ServerVtxo,
	funding: &Transaction,
	transitions: &BTreeMap<Txid, Transaction>,
	limits: BackingSnapshotLimits,
) -> anyhow::Result<EconomicCut> {
	let facts = snapshot.regular.iter().enumerate()
		.map(|(index, fact)| (fact.state.vtxo.point(), index)).collect::<BTreeMap<_, _>>();
	economic_cut_inner(snapshot, root, funding, transitions, &facts, None, limits)
}

fn economic_cut_inner(
	snapshot: &BackingSnapshot, root: &ServerVtxo, funding: &Transaction,
	transitions: &BTreeMap<Txid, Transaction>, facts: &BTreeMap<OutPoint, usize>,
	geometry: Option<&BackingGeometry>, limits: BackingSnapshotLimits,
) -> anyhow::Result<EconomicCut> {
	ensure!(facts.len() == snapshot.regular.len(), "expiry inventory has duplicate source outputs");
	let mut cut = EconomicCut { visited: Vec::new(), terminals: Vec::new(), residuals: Vec::new() };
	let mut pending = vec![root.point()];
	let mut visited = BTreeSet::new();
	while let Some(point) = pending.pop() {
		ensure!(visited.insert(point), "expiry economic branch overlaps or cycles");
		ensure!(visited.len() <= limits.max_rows, "expiry economic branch exceeds row budget");
		let index = *facts.get(&point).context("expiry economic branch has an unstored child")?;
		let fact = &snapshot.regular[index];
		cut.visited.push(index);
		if fact.state.offboarded_in.is_some() || fact.state.spent_in_round.is_some()
			|| fact.state.oor_spent_txid.is_none() {
			cut.terminals.push(index);
			continue;
		}
		let txid = fact.state.oor_spent_txid.expect("checked above");
		let tx = transitions.get(&txid).context("expiry recorded transition lacks an actual source path")?;
		ensure!(tx.input.len() == 1 && tx.input[0].previous_output == point,
			"expiry ordinary branch is not a one-input transition on its own backing");
		let value = tx.output.iter().try_fold(bitcoin::Amount::ZERO, |sum, output|
			sum.checked_add(output.value).context("expiry transition amount overflow"),
		)?;
		ensure!(value == fact.state.vtxo.amount(), "expiry transition does not conserve its backing");
		for (vout, output) in tx.output.iter().enumerate() {
			let outpoint = OutPoint::new(txid, u32::try_from(vout)?);
			if output.is_p2a_fee_anchor() {
				cut.residuals.push(BackingPartitionResidual { outpoint, txout: output.clone() });
			} else {
				let child = &snapshot.regular[*facts.get(&outpoint)
					.context("expiry transition has an unrepresented non-anchor output")?];
				ensure!(child.state.vtxo.txout() == *output, "expiry child metadata differs from its transaction output");
				pending.push(outpoint);
			}
		}
	}
	ensure!(!cut.terminals.is_empty(), "expiry backing has no economic terminals");
	// Transition lookup is only a candidate graph. Validate the complete current
	// terminal paths and their conserving partition before attribution, splitting
	// or any financial decision. Historical descendants beyond a binding round or
	// offboard are not evidence for this cut and cannot poison its preparation.
	let terminals = cut.terminals.iter().map(|index| snapshot.regular[*index].full_vtxo.clone()
		.context("expiry current terminal lacks its complete source path"))
		.collect::<anyhow::Result<Vec<_>>>()?;
	let selected = BackingSnapshot { roots: vec![root.clone()], regular: Vec::new(), serialized_bytes: 0 };
	match geometry {
		Some(geometry) => geometry.check_partition(&selected.roots, &cut.terminals, &cut.residuals, limits),
		None => selected.check_partition(&terminals, std::slice::from_ref(funding), &cut.residuals, limits),
	}.context("expiry current economic cut is invalid")?;
	Ok(cut)
}

#[cfg(test)]
pub(crate) fn source_transitions(
	snapshot: &BackingSnapshot, funding: &Transaction,
) -> anyhow::Result<BTreeMap<Txid, Transaction>> {
	collect_source_transitions(snapshot, funding, None, &mut None,
		BackingSnapshotLimits { max_rows: 0, max_serialized_bytes: 0 })
}

fn collect_source_transitions(
	snapshot: &BackingSnapshot, funding: &Transaction, mut paths: Option<&mut [Option<PathEntry>]>,
	cache_bytes: &mut Option<usize>, limits: BackingSnapshotLimits,
) -> anyhow::Result<BTreeMap<Txid, Transaction>> {
	let mut transactions = BTreeMap::new();
	let funding_txid = funding.compute_txid();
	let mut cached_steps = 0usize;
	for (index, fact) in snapshot.regular.iter().enumerate() {
		let Some(full) = &fact.full_vtxo else { continue; };
		ensure!(full.chain_anchor().txid == funding_txid, "expiry source has another funding anchor");
		let depth = usize::from(full.exit_depth());
		let mut steps = Vec::new();
		let fits = paths.is_some() && cached_steps.checked_add(depth).is_some_and(|count| count <= limits.max_rows)
			&& depth.checked_mul(size_of::<(Txid, usize)>()).and_then(|bytes| cache_bytes.and_then(|base| base.checked_add(bytes)))
				.is_some_and(|bytes| bytes <= limits.max_serialized_bytes)
			&& steps.try_reserve_exact(depth).is_ok();
		let next_bytes = steps.capacity().checked_mul(size_of::<(Txid, usize)>())
			.and_then(|bytes| cache_bytes.and_then(|base| base.checked_add(bytes)));
		let mut steps = if fits && next_bytes.is_some_and(|bytes| bytes <= limits.max_serialized_bytes) { Some(steps) } else { None };
		// These are candidate links only. economic_cut validates every selected
		// complete path against funding, including before requesting progression.
		// Eagerly validating historical forfeit descendants would reject an
		// unrelated customer's valid path before the current cut can be selected.
		for item in full.transactions() {
			let mut tx = item.tx;
			for input in &mut tx.input { input.witness.clear(); }
			let txid = tx.compute_txid();
			if let Some(steps) = &mut steps { steps.push((txid, item.output_idx)); }
			if let Some(previous) = transactions.insert(txid, tx.clone()) {
				ensure!(previous == tx, "expiry source has conflicting transaction geometry");
			}
		}
		if let (Some(paths), Some(steps)) = (&mut paths, steps) {
			cached_steps += depth;
			*cache_bytes = next_bytes;
			paths[index] = Some(PathEntry { steps, unsigned_ok: OnceLock::new() });
		}
	}
	Ok(transactions)
}


#[cfg(test)]
mod tests {
	use super::*;
	use std::str::FromStr;
	use ark::board::BoardBuilder;
	use ark::ProtocolEncoding;
	use ark::test_util::VTXO_VECTORS;
	use ark::vtxo::Full;
	use ark::vtxo::raw::RawVtxo;
	use crate::database::{SpendState, VtxoState};
	use crate::database::expiry_inventory::{BackingVtxoFact, SnapshotGenesis};

	const TEST_LIMITS: BackingSnapshotLimits = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };

	fn fact(full: ServerVtxo<Full>, spent: Option<Txid>) -> BackingVtxoFact {
		BackingVtxoFact {
			state: VtxoState {
				id: 1, vtxo_id: full.id(), vtxo: full.to_bare(), oor_spent_txid: spent,
				spent_in_round: None, offboarded_in: None, banned_until_height: None,
				spend_state: SpendState::Spendable, confirmed_height: None, decided_for_fallback: false,
				created_at: chrono::Local::now(), updated_at: chrono::Local::now(),
			},
			full_vtxo: Some(full), genesis: SnapshotGenesis::EncodedPath,
			frontier_at: None, onchain_spent_height: None, onchain_spent_txid: None,
			lightning_htlc_subscription_id: None,
		}
	}

	fn board() -> (BackingSnapshot, Transaction) {
		let board = &VTXO_VECTORS.board_vtxo;
		let builder = BoardBuilder::new_from_vtxo(board, &VTXO_VECTORS.anchor_tx, board.server_pubkey()).unwrap();
		let mut rows = builder.build_server_vtxos();
		let root = rows.remove(0);
		let source = rows.remove(0);
		let exit = source.point().txid;
		let root_bare = root.to_bare();
		(BackingSnapshot {
			roots: vec![root_bare], regular: vec![fact(root, Some(exit)), fact(source, None)],
			serialized_bytes: 0,
		}, VTXO_VECTORS.anchor_tx.clone())
	}

	fn geometry_with_limits(limits: BackingSnapshotLimits) -> BackingGeometry {
		let (snapshot, funding) = board();
		BackingGeometry::new(snapshot, funding, limits).unwrap()
	}

	#[test]
	fn expiry_geometry_reuses_exact_paths_and_falls_back_without_changing_cuts() {
		for cache_limits in [TEST_LIMITS,
			BackingSnapshotLimits { max_rows: 1, ..TEST_LIMITS },
			BackingSnapshotLimits { max_serialized_bytes: 1, ..TEST_LIMITS },
			BackingSnapshotLimits { max_serialized_bytes: 2 * size_of::<Option<PathEntry>>(), ..TEST_LIMITS },
		] {
			let geometry = geometry_with_limits(cache_limits);
			let snapshot = geometry.snapshot();
			for root in [&snapshot.roots[0], &snapshot.regular[1].state.vtxo] {
				let expected = economic_cut(snapshot, root, geometry.funding(), &geometry.transitions, TEST_LIMITS).unwrap();
				assert_eq!(geometry.economic_cut(root, TEST_LIMITS).unwrap(), expected);
				assert_eq!(geometry.economic_cut(root, TEST_LIMITS).unwrap(), expected);
			}
			if let Some(entry) = geometry.paths.as_ref().and_then(|paths| paths[1].as_ref()) {
				assert!(entry.unsigned_ok.get().is_some());
				let source = snapshot.regular[1].full_vtxo.as_ref().unwrap();
				let native = source.transactions().map(|item| {
					let mut tx = item.tx;
					for input in &mut tx.input { input.witness.clear(); }
					(tx, item.output_idx)
				}).collect::<Vec<_>>();
				let cached = geometry.path(1).unwrap();
				assert_eq!(cached.iter().map(|(tx, output)| (tx.into_owned(), output)).collect::<Vec<_>>(), native);
			} else {
				assert!(matches!(geometry.path(1).unwrap(), BackingPath::Native(_)));
				if cache_limits.max_serialized_bytes == 2 * size_of::<Option<PathEntry>>() {
					assert!(geometry.paths.is_some(), "row slots fit but terminal steps must fall back");
				}
			}
		}
	}

	#[test]
	fn expiry_geometry_cache_hits_preserve_selected_partitions_and_budget_refusals() {
		let geometry = geometry_with_limits(TEST_LIMITS);
		let root = &geometry.snapshot().roots[0];
		let cut = geometry.economic_cut(root, TEST_LIMITS).unwrap();
		let compare = |roots: &[ServerVtxo], indices: &[usize], residuals: &[BackingPartitionResidual], limits| {
			let terminals = indices.iter().map(|index| geometry.snapshot().regular[*index].full_vtxo.clone().unwrap()).collect::<Vec<_>>();
			let selected = BackingSnapshot { roots: roots.to_vec(), regular: Vec::new(), serialized_bytes: 0 };
			let ordinary = selected.check_partition(&terminals, std::slice::from_ref(geometry.funding()), residuals, limits)
				.map_err(|error| format!("{error:#}"));
			let cached = geometry.check_partition(roots, indices, residuals, limits).map_err(|error| format!("{error:#}"));
			assert_eq!(cached, ordinary);
			cached
		};
		let roots = std::slice::from_ref(root);
		assert!(compare(roots, &[1], &cut.residuals, TEST_LIMITS).is_ok());
		assert!(compare(roots, &[1], &[], TEST_LIMITS).is_err());
		let mut residuals = cut.residuals.clone();
		residuals[0].txout.value += bitcoin::Amount::from_sat(1);
		assert!(compare(roots, &[1], &residuals, TEST_LIMITS).is_err());
		assert!(compare(roots, &[1, 1], &cut.residuals, TEST_LIMITS).is_err());
		assert!(compare(&[root.clone(), root.clone()], &[1], &cut.residuals, TEST_LIMITS).is_err());
		let leaf = geometry.snapshot().regular[1].state.vtxo.clone();
		assert!(compare(&[root.clone(), leaf.clone()], &[1], &cut.residuals, TEST_LIMITS).is_err());
		assert!(compare(&[leaf], &[1], &[], TEST_LIMITS).is_ok());
		let wrong = ServerVtxo::new(root.point(), root.policy().clone(), root.amount() + bitcoin::Amount::from_sat(1),
			root.expiry_height(), root.server_pubkey(), root.exit_delta(), root.chain_anchor());
		assert!(compare(&[wrong], &[1], &cut.residuals, TEST_LIMITS).is_err());
		let bytes = geometry.snapshot().regular[1].full_vtxo.as_ref().unwrap().serialize().len()
			+ bitcoin::consensus::serialize(geometry.funding()).len()
			+ cut.residuals.iter().map(|r| bitcoin::consensus::serialize(&r.txout).len()).sum::<usize>();
		assert!(compare(roots, &[1], &cut.residuals,
			BackingSnapshotLimits { max_rows: 5, max_serialized_bytes: bytes }).is_ok());
		assert!(compare(roots, &[1], &cut.residuals,
			BackingSnapshotLimits { max_rows: 4, ..TEST_LIMITS }).is_err());
		assert!(compare(roots, &[1], &cut.residuals,
			BackingSnapshotLimits { max_serialized_bytes: bytes - 1, ..TEST_LIMITS }).is_err());
		assert!(compare(roots, &[1], &cut.residuals, TEST_LIMITS).is_ok());
	}

	#[test]
	fn expiry_readiness_follows_registered_board_and_conserves_actual_backing() {
		let (snapshot, funding) = board();
		let txs = source_transitions(&snapshot, &funding).unwrap();
		let cut = economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs, TEST_LIMITS).unwrap();
		assert_eq!(cut.terminals, [1]);
		assert_eq!(cut.visited, [0, 1]);
		let leaf = snapshot.regular[1].full_vtxo.clone().unwrap();
		let partition = snapshot.check_partition(&[leaf.clone()], &[funding], &cut.residuals, TEST_LIMITS).unwrap();
		assert_eq!(partition.roots[0].terminal_amount, leaf.amount());
		assert_eq!(partition.roots[0].anchor_residual_amount, bitcoin::Amount::from_sat(330));
		assert_eq!(leaf.amount() + partition.roots[0].anchor_residual_amount, snapshot.roots[0].amount());
	}

	#[test]
	fn expiry_readiness_refuses_unrepresented_siblings_and_wrong_funding() {
		let (mut snapshot, funding) = board();
		let txs = source_transitions(&snapshot, &funding).unwrap();
		assert!(economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs,
			BackingSnapshotLimits { max_rows: 1, ..TEST_LIMITS }).is_err());
		snapshot.regular.pop();
		assert!(economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs, TEST_LIMITS).is_err());
		let (snapshot, mut wrong) = board();
		wrong.output[0].value += bitcoin::Amount::from_sat(1);
		assert!(source_transitions(&snapshot, &wrong).is_err());
	}

	#[test]
	fn expiry_current_cut_ignores_invalid_unused_historical_descendant() {
		let (mut snapshot, funding) = board();
		let mut raw: RawVtxo = snapshot.regular[1].full_vtxo.clone().unwrap().into();
		raw.point = OutPoint::new(bitcoin::Txid::from_str("7777777777777777777777777777777777777777777777777777777777777777").unwrap(), 0);
		let obsolete: ServerVtxo<Full> = raw.into();
		assert!(obsolete.validate_unsigned(&funding).is_err());
		let mut historical = fact(obsolete, None);
		historical.state.spend_state = SpendState::RoundForfeit;
		snapshot.regular.push(historical);
		let txs = source_transitions(&snapshot, &funding).unwrap();
		let cut = economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs, TEST_LIMITS).unwrap();
		assert_eq!(cut.terminals, [1]);
		assert!(!cut.visited.contains(&2));
		let geometry = BackingGeometry::new(snapshot, funding, TEST_LIMITS).unwrap();
		assert_eq!(geometry.economic_cut(&geometry.snapshot().roots[0], TEST_LIMITS).unwrap(), cut);
		assert!(geometry.paths.as_ref().unwrap()[2].as_ref().unwrap().unsigned_ok.get().is_none());
	}

	#[test]
	fn expiry_current_cut_still_rejects_invalid_selected_terminal_path() {
		let (mut snapshot, funding) = board();
		let mut raw: RawVtxo = snapshot.regular[1].full_vtxo.clone().unwrap().into();
		let other: RawVtxo = ServerVtxo::from(VTXO_VECTORS.arkoor2_vtxo.clone()).into();
		raw.genesis = other.genesis;
		let invalid: ServerVtxo<Full> = raw.into();
		assert!(invalid.validate_unsigned(&funding).is_err());
		snapshot.regular[1] = fact(invalid, None);
		let txs = source_transitions(&snapshot, &funding).unwrap();
		assert!(economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs, TEST_LIMITS).is_err());
		let geometry = BackingGeometry::new(snapshot, funding, TEST_LIMITS).unwrap();
		assert!(geometry.economic_cut(&geometry.snapshot().roots[0], TEST_LIMITS).is_err());
		assert!(geometry.economic_cut(&geometry.snapshot().roots[0], TEST_LIMITS).is_err());
		assert!(geometry.paths.as_ref().unwrap()[1].as_ref().unwrap().unsigned_ok.get().is_none());
	}

	#[test]
	fn expiry_readiness_stops_at_offboard_and_round_settlement_boundaries() {
		let (mut snapshot, funding) = board();
		let txs = source_transitions(&snapshot, &funding).unwrap();
		// Offboard and round settlement use separate paths. Following an old
		// OOR field here could incorrectly traverse their forfeited inputs.
		snapshot.regular[0].state.offboarded_in = Some(funding.compute_txid());
		assert_eq!(economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs, TEST_LIMITS).unwrap().terminals, [0]);
		snapshot.regular[0].state.offboarded_in = None;
		snapshot.regular[0].state.spent_in_round = Some(9);
		assert_eq!(economic_cut(&snapshot, &snapshot.roots[0], &funding, &txs, TEST_LIMITS).unwrap().terminals, [0]);
	}
}
