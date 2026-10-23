//! Local ownership retained while a server spend is reconciled.

use std::collections::{BTreeMap, BTreeSet};
use bitcoin::Transaction;
use ark::VtxoId;

/// Signed bodies stay in the ordinary wallet store until their spend is verified.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[doc(hidden)]
pub struct FallbackSnapshot {
	/// Signed own claims are recovery material, not server payout receipts.
	pub terminal_observations: BTreeMap<i64, Vec<OwnClaim>>,
	pub awaiting_settlement: BTreeSet<VtxoId>,
	pub adopted_payout_vtxos: BTreeSet<VtxoId>,
	pub quarantined_vtxos: BTreeSet<VtxoId>,
	pub unproven_spent_vtxos: BTreeSet<VtxoId>,
	pub ordinary_spend_cursor: Option<VtxoId>,
	pub verified_ordinary_spends: BTreeSet<VtxoId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnClaim {
	pub transaction: Transaction,
}
