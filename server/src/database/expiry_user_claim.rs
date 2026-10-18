//! Retain a canonical user's existing timeout authorization independently of
//! its current confirmation. These rows never create an entitlement or payment.

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoId, VtxoPolicy};
use ark::vtxo::TapScriptClause;
use bitcoin::{Transaction, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::taproot::LeafVersion;
use bitcoin_ext::BlockRef;

use super::Tx;
use super::expiry_chain::ExpiryViewToken;

/// Replay is deliberately bounded to standard-sized existing transactions.
/// Unsupported bodies are still retained; they do not authorize dropping a
/// source from a joint refund until a supported replay path is available.
pub(crate) const MAX_REPLAY_INPUTS: usize = 128;
pub(crate) const MAX_REPLAY_BYTES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplayableTerminalClaim {
	pub source: VtxoId,
	pub claim: Transaction,
	pub confirmed: BlockRef,
}

impl ReplayableTerminalClaim {
	pub(crate) fn evidence_json(&self) -> serde_json::Value {
		serde_json::json!({"source":self.source.to_string(),
			"claim_txid":self.claim.compute_txid().to_string(),
			"height":self.confirmed.height.to_u32(),"hash":self.confirmed.hash.to_string()})
	}
}

/// The caller must first establish actual canonical inclusion. Matching script
/// bytes alone is not independent signature validation. Restrict retained replay
/// to the existing full-transaction DEFAULT user authorization (no ANYONECANPAY).
fn user_timeout_clause(source: &ServerVtxo, claim: &Transaction) -> bool {
	let Some(input) = claim.input.iter().find(|input| input.previous_output == source.point()) else { return false; };
	let Some(leaf) = input.witness.taproot_leaf_script() else { return false; };
	let script = match source.policy() {
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy)) =>
			policy.user_claim_after_expiry_clause(source.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend_v0(policy)) =>
			policy.user_claim_after_expiry_clause(source.exit_delta()).tapscript(),
		_ => return false,
	};
	leaf.version == LeafVersion::TapScript && leaf.script == script.as_script()
		&& input.script_sig.is_empty()
}

pub(crate) fn user_timeout_input(source: &ServerVtxo, claim: &Transaction) -> bool {
	user_timeout_clause(source,claim) && claim.input.iter().find(|input|input.previous_output==source.point())
		.is_some_and(|input|input.witness.len()==3
			&& input.witness.iter().next().is_some_and(|signature|signature.len()==64))
}

fn replay_shape(claim: &Transaction) -> bool {
	!claim.input.is_empty() && claim.input.len() <= MAX_REPLAY_INPUTS
		&& serialize(claim).len() <= MAX_REPLAY_BYTES && claim.weight().to_wu() <= 400_000
		&& claim.input.iter().map(|input| input.previous_output).collect::<std::collections::BTreeSet<_>>().len() == claim.input.len()
}

fn terminal_omission_shape(source: &ServerVtxo, claim: &Transaction) -> bool {
	claim.input.len()==1 && replay_shape(claim) && user_timeout_input(source,claim)
}

impl Tx<'_> {
	/// Called in the same transaction as the canonical source-spend observation,
	/// or after independently fetching that canonical body during backfill.
	/// Never replace the first body with a different witness variant of its txid.
	pub(crate) async fn retain_user_timeout_claim(
		&self, source: &ServerVtxo, claim: &Transaction, confirmed: BlockRef,
	) -> anyhow::Result<()> {
		if !user_timeout_clause(source,claim) { return Ok(()); }
		let txid=claim.compute_txid().to_string();
		let raw=serialize(claim);
		self.execute("INSERT INTO expiry_user_claim_tx(txid,raw_tx,confirmed_height,confirmed_hash) \
			VALUES($1,$2,$3,$4) ON CONFLICT(txid) DO NOTHING",
			&[&txid,&raw,&i64::from(confirmed.height),&confirmed.hash.to_string()]).await?;
		let retained: Vec<u8>=self.query_one("SELECT raw_tx FROM expiry_user_claim_tx WHERE txid=$1", &[&txid]).await?.get("raw_tx");
		// A different canonical witness must not break unrelated block sync or
		// silently overwrite the exact authorization supporting an older source.
		if retained != raw {
			let retained: Transaction = deserialize(&retained)?;
			if !user_timeout_clause(source,&retained) { return Ok(()); }
		}
		self.execute("INSERT INTO expiry_user_claim_source(source_vtxo_id,claim_txid) VALUES($1,$2) \
			ON CONFLICT DO NOTHING", &[&source.id().to_string(),&txid]).await?;
		self.execute("UPDATE expiry_user_claim_tx SET confirmed_height=$2,confirmed_hash=$3 WHERE txid=$1",
			&[&txid,&i64::from(confirmed.height),&confirmed.hash.to_string()]).await?;
		Ok(())
	}

	/// No entitlement is required: an actual user timeout can precede the first
	/// financial decision. Require matching canonical source/HTLC/body evidence,
	/// not a former terminal table entry or an old stored block observation.
	pub(crate) async fn replayable_terminal_claim(
		&self, source: VtxoId, view: ExpiryViewToken,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		if self.expiry_rebuild_token().await? != Some(view)
			|| self.query_one("SELECT invalid FROM expiry_chain_view WHERE id=1", &[]).await?.get::<_,bool>("invalid") { return Ok(None); }
		let row=self.query_opt("SELECT v.vtxo,t.raw_tx,t.txid,t.confirmed_height,b.hash \
			FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
			JOIN expiry_user_claim_source s ON s.source_vtxo_id=v.vtxo_id AND s.claim_txid=v.onchain_spent_txid \
			JOIN expiry_user_claim_tx t ON t.txid=s.claim_txid \
			JOIN watchmand_block b ON b.height=t.confirmed_height AND b.hash=t.confirmed_hash \
			WHERE v.vtxo_id=$1 AND v.onchain_spent_height=t.confirmed_height \
			AND t.confirmed_height<=$2 AND h.direction='incoming' AND h.chain_resolution='revoked' \
			AND h.chain_resolution_height=v.onchain_spent_height AND h.offchain_resolution IS NULL \
			AND v.oor_spent_txid IS NULL AND v.spent_in_round IS NULL AND v.offboarded_in IS NULL \
			AND NOT EXISTS(SELECT 1 FROM expiry_decided_coin d WHERE d.vtxo_id=v.vtxo_id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_allocation_input i WHERE i.vtxo_id=v.vtxo_id)",
			&[&source.to_string(),&i64::from(view.captaind.height.min(view.watchmand.height))]).await?;
		let Some(row)=row else { return Ok(None); };
		let body=ServerVtxo::deserialize(row.get("vtxo"))?;
		let claim:Transaction=deserialize(row.get("raw_tx"))?;
		let txid:Txid=row.get::<_,&str>("txid").parse()?;
		ensure!(claim.compute_txid()==txid && body.id()==source,"retained user claim identity differs");
		// Only a single-input claim supplies terminal omission today: that
		// exact source is in the final component lock union. A batched claim
		// would need every additional known input in that same final union.
		if !terminal_omission_shape(&body,&claim) { return Ok(None); }
		Ok(Some(ReplayableTerminalClaim { source,claim,confirmed:BlockRef {
			height:row.get::<_,i64>("confirmed_height").try_into()?,hash:row.get::<_,&str>("hash").parse()?,
		}}))
	}

	/// Recheck under the same sorted source locks used by all competing writes.
	/// A new off-chain obligation must never be undone by replaying stale raw.
	pub(crate) async fn guard_user_claim_replay(&self, claim: &Transaction) -> anyhow::Result<()> {
		ensure!(replay_shape(claim),"retained user claim exceeds replay capacity");
		let ids=claim.input.iter().map(|input|VtxoId::from(input.previous_output)).collect::<Vec<_>>();
		super::expiry_guard::guard_obligation_changes(self,&ids).await?;
		let ids=ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let rows=self.query("SELECT v.vtxo_id,v.oor_spent_txid,v.spent_in_round,v.offboarded_in,h.offchain_resolution::TEXT AS offchain_resolution \
			FROM vtxo v LEFT JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=ANY($1)", &[&ids]).await?;
		for row in rows {
			ensure!(row.get::<_,Option<&str>>("oor_spent_txid").is_none()
				&& row.get::<_,Option<i64>>("spent_in_round").is_none()
				&& row.get::<_,Option<&str>>("offboarded_in").is_none()
				&& row.get::<_,Option<&str>>("offchain_resolution").is_none(),
				"retained user claim input {} has an off-chain commitment",row.get::<_,&str>("vtxo_id"));
		}
		let linked=self.query("SELECT v.vtxo FROM expiry_user_claim_source s \
			JOIN vtxo v ON v.vtxo_id=s.source_vtxo_id WHERE s.claim_txid=$1", &[&claim.compute_txid().to_string()]).await?;
		ensure!(!linked.is_empty(),"retained user claim has no original source");
		for row in linked {
			let source=ServerVtxo::deserialize(row.get("vtxo"))?;
			ensure!(user_timeout_input(&source,claim),"retained user claim no longer authorizes its original source");
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::hashes::Hash;
	use ark::test_util::VTXO_VECTORS;
	use bitcoin::{absolute,transaction,Sequence,TxIn,Witness};

	#[test]
	fn ordinary_or_ancestor_spend_is_not_a_retained_timeout_authorization() {
		let source=ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone()).to_bare();
		let claim=Transaction {version:transaction::Version::TWO,lock_time:absolute::LockTime::ZERO,
			input:vec![TxIn {previous_output:source.point(),script_sig:Default::default(),sequence:Sequence::ZERO,witness:Witness::new()}],output:vec![]};
		assert!(!user_timeout_input(&source,&claim));
	}

	/// These are classifier controls, not signature-validity or relay proof.
	#[test]
	fn timeout_classifier_requires_exact_leaf_and_existing_default_authorization() {
		let source=ServerVtxo::from(VTXO_VECTORS.arkoor_htlc_out_vtxo.clone()).to_bare();
		let ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy))=source.policy() else { panic!("expected incoming HTLC vector"); };
		let script=policy.user_claim_after_expiry_clause(source.exit_delta()).tapscript();
		let taproot=source.policy().taproot(source.server_pubkey(),source.exit_delta(),source.expiry_height());
		let control=taproot.control_block(&(script.clone(),LeafVersion::TapScript)).unwrap().serialize();
		let mut claim=Transaction {version:transaction::Version::TWO,lock_time:absolute::LockTime::ZERO,
			input:vec![TxIn {previous_output:source.point(),script_sig:Default::default(),sequence:Sequence::ZERO,
				witness:Witness::from_slice(&[vec![0;64],script.clone().into_bytes(),control.clone()])}],output:vec![]};
		assert!(user_timeout_input(&source,&claim));
		assert!(terminal_omission_shape(&source,&claim));
		let mut batched=claim.clone();
		batched.input.push(TxIn::default());
		assert!(replay_shape(&batched));
		assert!(!terminal_omission_shape(&source,&batched),"extra inputs are not in the terminal original source lock union");
		let bytes=serialize(&claim);
		let retained:Transaction=deserialize(&bytes).unwrap();
		assert_eq!(serialize(&retained),bytes);
		claim.input[0].previous_output=source.chain_anchor();
		assert!(!user_timeout_input(&source,&claim));
		claim.input[0].previous_output=source.point();
		claim.input[0].witness=Witness::from_slice(&[vec![0;65],script.into_bytes(),control]);
		assert!(user_timeout_clause(&source,&claim));
		assert!(!user_timeout_input(&source,&claim));
	}

	#[test]
	fn replay_capacity_rejects_duplicate_inputs_instead_of_truncating() {
		let input=TxIn::default();
		let mut claim=Transaction {version:transaction::Version::TWO,lock_time:absolute::LockTime::ZERO,input:vec![input.clone()],output:vec![]};
		assert!(replay_shape(&claim));
		claim.input.push(input);
		assert!(!replay_shape(&claim));
		claim.input=(0..=MAX_REPLAY_INPUTS).map(|index|TxIn {previous_output:bitcoin::OutPoint::new(Txid::from_byte_array([0;32]),index as u32),..TxIn::default()}).collect();
		assert!(!replay_shape(&claim));
	}
}
