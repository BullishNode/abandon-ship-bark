//! Resolve only the original appended leaf of a round with creation provenance.

use bitcoin::{Transaction, Txid};
use bitcoin::consensus::deserialize;

use ark::{ProtocolEncoding, ServerVtxo};
use ark::tree::signed::SignedVtxoTreeSpec;

use crate::database::expiry_inventory::BackingVtxoFact;
use crate::database::{SpendState, Tx};
use crate::round::padding::{PaddingIssuance, MAX_ISSUANCE_BYTES};

impl Tx<'_> {
	/// The caller must prove canonical confirmed physical backing separately.
	/// Commit calls this again under view and sorted source locks. Lock the round
	/// last, as UndoRound does, and retain that lock until the decision commits.
	pub(crate) async fn round_padding_issuance(
		&self, fact: &BackingVtxoFact, funding: &Transaction,
	) -> anyhow::Result<Option<(Txid, u32)>> {
		if !matches!(fact.state.spend_state, SpendState::Unclaimed | SpendState::Spent)
			|| fact.state.oor_spent_txid.is_some() || fact.state.spent_in_round.is_some()
			|| fact.state.offboarded_in.is_some() || fact.lightning_htlc_subscription_id.is_some()
			|| fact.onchain_spent_height.is_some() || fact.onchain_spent_txid.is_some()
		{ return Ok(None); }
		let Some(full) = &fact.full_vtxo else { return Ok(None); };
		let Some(row) = self.query_opt("
			SELECT signed_tree,funding_tx,padding_issuance::TEXT AS issuance
			FROM round WHERE funding_txid=$1 FOR SHARE
		", &[&full.chain_anchor().txid.to_string()]).await? else { return Ok(None); };
		let Some(encoded) = row.get::<_, Option<&str>>("issuance") else { return Ok(None); };
		ensure!(encoded.len() <= MAX_ISSUANCE_BYTES, "stored padding issuance exceeds its limit");
		let issuance: PaddingIssuance = serde_json::from_str(encoded)?;
		let stored_funding: Transaction = deserialize(row.get("funding_tx"))?;
		// The first commit deliberately has no funding signatures. Its metadata
		// is not funded authority. Require the actual signed canonical body.
		if stored_funding.input.iter().any(|input| input.witness.is_empty())
			|| stored_funding != *funding { return Ok(None); }
		let tree = SignedVtxoTreeSpec::deserialize(row.get("signed_tree"))?
			.with_detected_hashlock_version()?.into_cached_tree();
		// Locate the exact origin before expensive funding signature checks.
		// This prefilter is not an authorization and does not accept descendants.
		let Some(index) = issuance.appended_leaf(&tree, fact.state.vtxo_id) else { return Ok(None); };
		issuance.validate(&tree, funding)?;
		issuance.validate_signed_funding(funding)?;
		let leaf = tree.output_vtxos().nth(index as usize).expect("validated leaf index");
		if full.chain_anchor() != leaf.chain_anchor()
			|| full.serialize() != ServerVtxo::from(leaf.clone()).serialize()
		{ return Ok(None); }
		let hash = leaf.unlock_hash().expect("round leaf has an unlock hash");
		if self.query_opt("
			SELECT 1 WHERE
				EXISTS(SELECT 1 FROM round_participation WHERE unlock_hash=$1)
				OR EXISTS(SELECT 1 FROM expiry_cancelled_participation WHERE unlock_hash=$1)
				OR EXISTS(SELECT 1 FROM round_part_input WHERE vtxo_id=$2)
				OR EXISTS(SELECT 1 FROM expiry_owner_key WHERE user_pubkey=$3)
				OR EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE source_vtxo_id=$2 OR output_vtxo_id=$2)
				OR EXISTS(SELECT 1 FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id
					WHERE e.vtxo_id=$2 AND c.disposition='customer')
				OR EXISTS(SELECT 1 FROM expiry_entitlement e JOIN expiry_terminal_claim c ON c.entitlement_id=e.id
					WHERE e.vtxo_id=$2)
		", &[&hash.to_string(), &fact.state.vtxo_id.to_string(), &leaf.user_pubkey().to_string()]).await?.is_some()
		{ return Ok(None); }
		let entitlement = self.query_opt("
			SELECT irreversible_facts::TEXT AS facts FROM expiry_entitlement WHERE vtxo_id=$1
		", &[&fact.state.vtxo_id.to_string()]).await?;
		if fact.state.spend_state == SpendState::Spent && entitlement.is_none() { return Ok(None); }
		if let Some(entitlement) = entitlement {
			let facts: serde_json::Value = serde_json::from_str(entitlement.get("facts"))?;
			if facts.get("kind").and_then(serde_json::Value::as_str) != Some("round_padding_issuance")
				|| facts.get("funding_txid").and_then(serde_json::Value::as_str) != Some(funding.compute_txid().to_string().as_str())
				|| facts.get("leaf_index").and_then(serde_json::Value::as_u64) != Some(u64::from(index))
				|| facts.get("operator").and_then(serde_json::Value::as_bool) != Some(true)
				|| facts.get("customer_key").is_some()
			{ return Ok(None); }
		}
		Ok(Some((funding.compute_txid(), index)))
	}
}
