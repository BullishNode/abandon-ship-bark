//! Local ownership retained while a server spend is reconciled.

use std::collections::BTreeSet;
use bitcoin::Transaction;
use ark::VtxoId;

/// Signed bodies stay in the ordinary wallet store until their spend is verified.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[doc(hidden)]
pub struct FallbackSnapshot {
	/// Signed own claims are recovery material, not server payout receipts.
	pub own_claims: Vec<OwnClaim>,
	pub awaiting_settlement: BTreeSet<VtxoId>,
	pub adopted_payout_vtxos: BTreeSet<VtxoId>,
	pub quarantined_vtxos: BTreeSet<VtxoId>,
	pub unproven_spent_vtxos: BTreeSet<VtxoId>,
	pub verified_ordinary_spends: BTreeSet<VtxoId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnClaim {
	pub transaction: Transaction,
}
