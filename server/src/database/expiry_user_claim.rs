//! Retain a canonical user's existing timeout authorization independently of
//! its current confirmation. These rows never create an entitlement or payment.

use anyhow::Context;
use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoId, VtxoPolicy};
use ark::vtxo::TapScriptClause;
use bitcoin::{Transaction, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::taproot::LeafVersion;
use bitcoin_ext::BlockRef;

use super::Tx;
use super::expiry_chain::ExpiryViewToken;
use super::expiry_internal_refund::InternalOriginalClaimScope;

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

/// The entire raw input set must be inside one actual original-attempt
/// membership. Distinct/size checks precede any per-source canonical lookup.
fn complete_original_batch_inputs(originals: &[VtxoId], claim: &Transaction) -> bool {
	claim.input.len()>1 && replay_shape(claim)
		&& claim.input.iter().all(|input|originals.contains(&VtxoId::from(input.previous_output)))
}

/// A prior financial reservation is not authority to create or alter a user
/// spend. Only the exact retained DEFAULT authorization may use its private
/// joint-refund scope; the ordinary financial admission fence stays in place.
fn retained_terminal_omission_shape(source: &ServerVtxo, claim: &Transaction, retained: &[u8]) -> bool {
	terminal_omission_shape(source, claim) && serialize(claim) == retained
}

#[derive(Clone, Copy)]
enum TerminalProofScope { Unfenced, InternalReturn, InternalPartial, RebuildingUnfenced, RebuildingPartial }

impl TerminalProofScope {
	fn requires_completed_rebuild(self)->bool {
		!matches!(self,Self::RebuildingUnfenced|Self::RebuildingPartial)
	}
}

/// One closed epoch covers only database row IDs already allocated when it
/// starts. Later arrivals cannot keep its tail growing; an older row becoming
/// eligible behind the cursor is revisited in the next epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BackfillWindow {
	after: i64,
	through: i64,
}

impl BackfillWindow {
	fn begin(self, current_max: i64) -> Self {
		if self.after>=self.through { Self {after:0,through:current_max} } else { self }
	}

	fn advanced(self, last: Option<i64>) -> anyhow::Result<Self> {
		let after=last.unwrap_or(self.through);
		ensure!(after>=self.after && after<=self.through
			&& (last.is_none() || after>self.after),"invalid user claim backfill position");
		Ok(Self {after,..self})
	}
}

pub(crate) struct UserClaimBackfill {
	pub source_id: String,
	pub body: Vec<u8>,
	pub claim_txid: String,
	pub observed_height: i32,
}

impl Tx<'_> {
	/// Service metadata is committed before external reads. Unsupported or
	/// temporarily unavailable canonical bodies must not monopolize each pass.
	pub(crate) async fn take_user_claim_backfill(
		&self, view: ExpiryViewToken,
	) -> anyhow::Result<Vec<UserClaimBackfill>> {
		self.query_one("SELECT generation FROM expiry_chain_view WHERE id=1 FOR SHARE", &[]).await?;
		if self.expiry_rebuild_token().await? != Some(view) { return Ok(Vec::new()); }
		let row=self.query_one("SELECT after_id,through_id FROM expiry_user_claim_backfill_service WHERE id=1 FOR UPDATE", &[]).await?;
		let position=BackfillWindow {after:row.get("after_id"),through:row.get("through_id")};
		let high=if position.after>=position.through {
			self.query_one("SELECT COALESCE(MAX(id),0) FROM vtxo", &[]).await?.get(0)
		} else { position.through };
		let window=position.begin(high);
		let rows=self.query("SELECT v.id,v.vtxo_id,v.vtxo,v.onchain_spent_txid,v.onchain_spent_height \
			FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.id>$1 AND v.id<=$2 \
			AND h.direction='incoming' AND h.chain_resolution='revoked' \
			AND h.chain_resolution_height=v.onchain_spent_height AND v.onchain_spent_height<=$3 \
			AND NOT EXISTS(SELECT 1 FROM expiry_user_claim_source s WHERE s.source_vtxo_id=v.vtxo_id \
			AND s.claim_txid=v.onchain_spent_txid) ORDER BY v.id LIMIT 16",
			&[&window.after,&window.through,&i32::try_from(view.captaind.height.min(view.watchmand.height).to_u32())?]).await?;
		let next=window.advanced(rows.last().map(|row|row.get("id")))?;
		self.execute("UPDATE expiry_user_claim_backfill_service SET after_id=$1,through_id=$2 WHERE id=1",
			&[&next.after,&next.through]).await?;
		// Decode each source only after this service position has committed. A
		// malformed legacy body is isolated just like an unavailable Core body.
		Ok(rows.into_iter().map(|row|UserClaimBackfill {
			source_id:row.get("vtxo_id"),body:row.get("vtxo"),claim_txid:row.get("onchain_spent_txid"),
			observed_height:row.get("onchain_spent_height"),
		}).collect())
	}


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
		self.replayable_terminal_claim_with_reservations(source, view, TerminalProofScope::Unfenced).await
	}

	/// Only the complete all-terminal internal grant-return guard may opt into
	/// an exact prior conflicted reservation. Its strict mode does not recognize
	/// dependent partial parents; those use the separate typed scope below.
	pub(crate) async fn replayable_internal_return_terminal_claim(
		&self, source: VtxoId, view: ExpiryViewToken,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		self.replayable_terminal_claim_with_reservations(source, view, TerminalProofScope::InternalReturn).await
	}

	/// Only scoped joint preparation uses this opt-in. It preserves an existing
	/// exact original customer debt; it cannot admit an unrelated allocation.
	pub(crate) async fn replayable_internal_partial_terminal_claim(
		&self,source:VtxoId,view:ExpiryViewToken,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		self.replayable_terminal_claim_with_reservations(source,view,TerminalProofScope::InternalPartial).await
	}

	/// The rebuild owns the exclusive generation lock and has independently
	/// verified Core's completed hashes. It cannot demand invalid=false before
	/// finishing its own reconstruction. All canonical source/body joins remain.
	pub(crate) async fn rebuilding_internal_partial_terminal_claim(
		&self,source:VtxoId,view:ExpiryViewToken,prior_joint:bool,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		if prior_joint {
			self.replayable_terminal_claim_with_reservations(source,view,TerminalProofScope::RebuildingPartial).await
		} else {
			self.replayable_terminal_claim_with_reservations(source,view,TerminalProofScope::RebuildingUnfenced).await
		}
	}

	async fn replayable_terminal_claim_with_reservations(
		&self, source: VtxoId, view: ExpiryViewToken, scope: TerminalProofScope,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		let Some((body,proof,raw))=self.canonical_retained_terminal_claim(source,view,scope.requires_completed_rebuild()).await? else {return Ok(None);};
		let claim=&proof.claim;
		// Generic and reserved omission remain single-input. Only the private
		// complete-original-attempt helper below may classify a fresh batch.
		if !retained_terminal_omission_shape(&body,claim,&raw) { return Ok(None); }
		if self.user_claim_has_obligation_fences(&[source]).await? {
			let allowed=match scope {
				TerminalProofScope::Unfenced|TerminalProofScope::RebuildingUnfenced=>false,
				TerminalProofScope::InternalReturn=>self.conflicted_internal_user_claim_scope(source).await?.is_some(),
				TerminalProofScope::InternalPartial|TerminalProofScope::RebuildingPartial=>self.partial_internal_user_claim_scope(source).await?.is_some(),
			};
			if !allowed {return Ok(None);}
		}
		Ok(Some(proof))
	}

	async fn canonical_retained_terminal_claim(
		&self, source: VtxoId, view: ExpiryViewToken, requires_completed_rebuild: bool,
	) -> anyhow::Result<Option<(ServerVtxo,ReplayableTerminalClaim,Vec<u8>)>> {
		if self.expiry_rebuild_token().await? != Some(view)
			|| (requires_completed_rebuild
				&& self.query_one("SELECT invalid FROM expiry_chain_view WHERE id=1", &[]).await?.get::<_,bool>("invalid")) { return Ok(None); }
		let row=self.query_opt("SELECT v.vtxo,t.raw_tx,t.txid,t.confirmed_height,b.hash \
			FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
			JOIN expiry_user_claim_source s ON s.source_vtxo_id=v.vtxo_id AND s.claim_txid=v.onchain_spent_txid \
			JOIN expiry_user_claim_tx t ON t.txid=s.claim_txid \
			JOIN watchmand_block b ON b.height=t.confirmed_height AND b.hash=t.confirmed_hash \
			WHERE v.vtxo_id=$1 AND v.onchain_spent_height=t.confirmed_height \
			AND t.confirmed_height<=$2 AND h.direction='incoming' AND h.chain_resolution='revoked' \
			AND h.chain_resolution_height=v.onchain_spent_height AND h.offchain_resolution IS NULL \
			AND v.oor_spent_txid IS NULL AND v.spent_in_round IS NULL AND v.offboarded_in IS NULL",
			&[&source.to_string(),&i64::from(view.captaind.height.min(view.watchmand.height))]).await?;
		let Some(row)=row else { return Ok(None); };
		let body=ServerVtxo::deserialize(row.get("vtxo"))?;
		let raw:Vec<u8>=row.get("raw_tx");
		let claim:Transaction=deserialize(&raw)?;
		let txid:Txid=row.get::<_,&str>("txid").parse()?;
		ensure!(claim.compute_txid()==txid && body.id()==source,"retained user claim identity differs");
		let proof=ReplayableTerminalClaim { source,claim,confirmed:BlockRef {
			height:row.get::<_,i64>("confirmed_height").try_into()?,hash:row.get::<_,&str>("hash").parse()?,
		}};
		Ok(Some((body,proof,raw)))
	}

	/// Private fresh same-attempt proof, never a generic or reserved replay
	/// exception. Final component commit rechecks this under its complete sorted
	/// original/grant/physical union, WAL2 and subscription locks. Rebuild owns
	/// the exclusive view and reconstructs this same actual membership.
	pub(crate) async fn replayable_internal_original_batch_claim(
		&self, source: VtxoId, view: ExpiryViewToken, scope: &InternalOriginalClaimScope, rebuilding: bool,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		let Some((_,proof,raw))=self.canonical_retained_terminal_claim(source,view,!rebuilding).await? else {return Ok(None);};
		if !scope.originals().contains(&source) || !complete_original_batch_inputs(scope.originals(),&proof.claim) {return Ok(None);}
		let ids=proof.claim.input.iter().map(|input|VtxoId::from(input.previous_output)).collect::<Vec<_>>();
		if self.user_claim_has_obligation_fences(&ids).await?
			|| (!rebuilding && self.user_claim_has_obligation_fences(scope.originals()).await?) {return Ok(None);}
		for id in ids {
			let Some((body,current,current_raw))=self.canonical_retained_terminal_claim(id,view,!rebuilding).await? else {return Ok(None);};
			if current_raw!=raw || serialize(&current.claim)!=raw || current.confirmed!=proof.confirmed
				|| !user_timeout_input(&body,&current.claim) {return Ok(None);}
			let source=super::htlc_vtxo::get_htlc_vtxo(self,id).await?.context("batch original lost its signed HTLC source")?;
			scope.check_source(&source)?;
		}
		Ok(Some(proof))
	}

	async fn user_claim_has_obligation_fences(&self, ids: &[VtxoId]) -> anyhow::Result<bool> {
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		Ok(self.query_one("SELECT EXISTS(SELECT 1 FROM expiry_decided_coin WHERE vtxo_id=ANY($1)) \
			OR EXISTS(SELECT 1 FROM expiry_allocation_input WHERE vtxo_id=ANY($1)) AS fenced", &[&ids])
			.await?.get("fenced"))
	}

	/// The caller holds the completed view. Choose the exact scoped reservation
	/// BEFORE taking any single-source lock: its full original/grant/physical
	/// union must be locked in one order, followed by allocations and WAL/subs.
	/// A new off-chain obligation must never be undone by replaying stale raw.
	pub(crate) async fn guard_user_claim_replay(&self, claim: &Transaction) -> anyhow::Result<()> {
		ensure!(replay_shape(claim),"retained user claim exceeds replay capacity");
		let retained = self.query_opt("SELECT raw_tx FROM expiry_user_claim_tx WHERE txid=$1",
			&[&claim.compute_txid().to_string()]).await?.context("user claim replay has no retained authorization")?;
		let raw: &[u8] = retained.get("raw_tx");
		ensure!(serialize(claim) == raw, "user claim replay changes the retained authorization");
		let ids=claim.input.iter().map(|input|VtxoId::from(input.previous_output)).collect::<Vec<_>>();
		if self.user_claim_has_obligation_fences(&ids).await? {
			let linked = self.query("SELECT v.vtxo FROM expiry_user_claim_source s \
				JOIN vtxo v ON v.vtxo_id=s.source_vtxo_id WHERE s.claim_txid=$1",
				&[&claim.compute_txid().to_string()]).await?;
			ensure!(linked.len() == 1, "reserved user claim replay requires one exact original source");
			let source = ServerVtxo::deserialize(linked[0].get("vtxo"))?;
			ensure!(retained_terminal_omission_shape(&source, claim, raw),
				"reserved user claim replay requires the retained single-input DEFAULT authorization");
			let scope = self.partial_internal_user_claim_scope(source.id()).await?
				.context("retained user claim crosses an unrelated or live financial reservation")?;
			self.lock_conflicted_internal_user_claim_scope(&scope).await?;
		} else {
			// If a fence is inserted after the initial read, the ordinary guard
			// rejects this pass. Retry may then take the complete scoped union.
			super::expiry_guard::guard_obligation_changes(self,&ids).await?;
		}
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
	fn only_reconstruction_modes_may_validate_premises_before_rebuild_finishes() {
		for scope in [TerminalProofScope::Unfenced,TerminalProofScope::InternalReturn,TerminalProofScope::InternalPartial] {
			assert!(scope.requires_completed_rebuild());
		}
		assert!(!TerminalProofScope::RebuildingUnfenced.requires_completed_rebuild());
		assert!(!TerminalProofScope::RebuildingPartial.requires_completed_rebuild());
	}

	#[test]
	fn backfill_unsupported_prefix_does_not_restart_the_same_batch() {
		let start=BackfillWindow {after:0,through:0}.begin(48);
		let first=start.advanced(Some(16)).unwrap();
		// Even if all first sixteen bodies remain unsupported/unlinked, the
		// next database range starts strictly after their actual row IDs.
		assert_eq!(first,BackfillWindow {after:16,through:48});
		let second=first.advanced(Some(32)).unwrap();
		assert_eq!(second.after,32);
		assert_eq!(second.begin(1000).through,48,"new funded arrivals cannot extend this epoch");
		let exhausted=second.advanced(None).unwrap();
		assert_eq!(exhausted.after,48);
		assert_eq!(exhausted.begin(1000),BackfillWindow {after:0,through:1000});
	}

	#[test]
	fn backfill_late_old_row_and_restart_keep_finite_epoch_position() {
		let persisted=BackfillWindow {after:32,through:48};
		assert_eq!(persisted.begin(1000),persisted,"a process restart does not reset the closed epoch");
		let wrapped=persisted.advanced(None).unwrap().begin(1000);
		assert_eq!(wrapped.after,0,"a newly eligible old row below32 is reconsidered after wrap");
		assert!(wrapped.advanced(Some(10)).is_ok());
		assert!(persisted.advanced(Some(20)).is_err());
		assert!(persisted.advanced(Some(32)).is_err());
		assert!(persisted.advanced(Some(49)).is_err());
	}

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
		assert!(retained_terminal_omission_shape(&source, &claim, &bytes));
		assert!(!retained_terminal_omission_shape(&source, &batched, &serialize(&batched)),
			"a financial reservation does not authorize a batched scope shortcut");
		let mut altered = claim.clone();
		altered.output.push(bitcoin::TxOut {value:bitcoin::Amount::from_sat(1),script_pubkey:Default::default()});
		assert!(!retained_terminal_omission_shape(&source, &altered, &bytes),
			"same source/script is insufficient when the retained user outputs change");
		let mut altered_witness = claim.clone();
		let mut witness = claim.input[0].witness.iter().map(|item| item.to_vec()).collect::<Vec<_>>();
		witness[0][0] = 1;
		altered_witness.input[0].witness = Witness::from_slice(&witness);
		assert_eq!(altered_witness.compute_txid(), claim.compute_txid());
		assert!(user_timeout_input(&source, &altered_witness));
		assert!(!retained_terminal_omission_shape(&source, &altered_witness, &bytes),
			"same txid does not permit replacing the exact retained witness");
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
	fn complete_original_batch_never_accepts_a_foreign_duplicate_or_single_input() {
		let a=bitcoin::OutPoint::new(Txid::from_byte_array([1;32]),0);
		let b=bitcoin::OutPoint::new(Txid::from_byte_array([2;32]),0);
		let live=bitcoin::OutPoint::new(Txid::from_byte_array([3;32]),0);
		let originals=[VtxoId::from(a),VtxoId::from(b),VtxoId::from(live)];
		let mut claim=Transaction {version:transaction::Version::TWO,lock_time:absolute::LockTime::ZERO,
			input:vec![TxIn {previous_output:a,..TxIn::default()},TxIn {previous_output:b,..TxIn::default()}],output:vec![]};
		assert!(complete_original_batch_inputs(&originals,&claim));
		claim.input.reverse();
		assert!(complete_original_batch_inputs(&originals,&claim));
		assert!(!complete_original_batch_inputs(&originals[..1],&claim));
		claim.input[1].previous_output=claim.input[0].previous_output;
		assert!(!complete_original_batch_inputs(&originals,&claim));
		claim.input.pop();
		assert!(!complete_original_batch_inputs(&originals,&claim));
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
