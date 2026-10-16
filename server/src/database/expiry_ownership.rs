//! Current source ownership for an already partitioned physical backing set.
//! Proposals are observations. The caller rechecks them under payment, flux,
//! settlement and association locks before freezing; this module never settles.

pub use super::expiry_settlement::{ExpiryInput, ExpirySource};

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::Context;
use bitcoin::{Amount, Transaction, Txid};
use bitcoin::secp256k1::PublicKey;
use futures::FutureExt;

use ark::{ServerVtxoPolicy, Vtxo, VtxoId};
use ark::lightning::PaymentHash;
use ark::tree::signed::UnlockHash;

use crate::ln::SendRefundEvidence;
use crate::database::ln::{LightningNodeId, LightningPaymentStatus};
use crate::Server;
use super::carrier_snapshot::{BackingSnapshot, BackingVtxoFact};
use super::expiry_settlement::CANDIDATES;
use super::{SpendState, Tx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetirementRule {
	/// An unfinished exchange's originals must retire as one complete closure.
	/// Earlier consumption of one original is not proof that the others cannot exit.
	CompleteExchangeClosure,
	/// Each right is already conclusively retired, or this exact carrier retires it.
	ConfirmedOrSameCarrier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
	Exchange(UnlockHash),
	ReceiveGrants(PaymentHash),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetirementDependency {
	pub source: DependencySource,
	pub ids: Vec<VtxoId>,
	pub rule: RetirementRule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorRecovery {
	Padding,
	/// Unused operator pool principal, including authenticated allocation change.
	UnusedPool,
	CompletedExchange(UnlockHash),
	/// Reserved operator principal, retired atomically with the sole replacement payout.
	SameCarrierFreshExchange(UnlockHash),
	PaidLightningSend(PaymentHash),
	UnpaidReceive(PaymentHash),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipPending {
	NotMature,
	MissingPath,
	MissingFunding,
	SourceChanged,
	ExitedOrConsumed,
	MissingInputOwner,
	MissingExchange,
	ExchangeNotIssued(UnlockHash),
	PartialForfeits(UnlockHash),
	ExchangeCancellation(UnlockHash),
	MissingGrantEvidence(PaymentHash),
	ReceiveCollection(PaymentHash),
	PaymentBusy(PaymentHash),
	SendEvidence(PaymentHash),
	LightningOutcome { payment_hash: PaymentHash, detail: String },
	UnsupportedSource,
}

#[derive(Debug)]
pub enum OwnershipDisposition {
	Customer(ExpiryInput),
	Operator(OperatorRecovery),
	/// This remains an exchange obligation until its entire carrier confirms.
	/// It cannot be finalized independently or treated as free operator principal.
	ConditionalOriginalRecovery(UnlockHash),
	/// A prepared intra-Ark grant can be reclaimed only with cancellation and
	/// retirement of its complete grant set, with the preimage fence retained.
	ConditionalUnpaidReceive(PaymentHash),
	Pending(OwnershipPending),
}

#[derive(Debug)]
pub struct OwnershipDecision {
	pub id: VtxoId,
	pub gross: Amount,
	pub disposition: OwnershipDisposition,
	pub dependencies: Vec<RetirementDependency>,
}

pub struct OwnershipContext<'a> {
	pub tip: u32,
	pub grace: u32,
	/// Hard bound on all expanded economic dependency IDs in this proposal.
	pub max_dependencies: usize,
	pub server_pubkey: PublicKey,
	pub funding: &'a BTreeMap<Txid, Transaction>,
	/// Actual terminal hold-node cancellation observations, taken under payment
	/// guards. Subscription status or an unavailable node does not supply these.
	pub canceled_receives: &'a BTreeSet<PaymentHash>,
}

struct ExchangeEvidence {
	hash: UnlockHash,
	issued: bool,
	complete: bool,
	partial_forfeits: bool,
	exited: bool,
	missing: bool,
	inputs: Vec<VtxoId>,
}

struct ExchangeInputFacts { id: VtxoId, forfeited: bool, missing: bool, exited: bool }

fn summarize_exchange(hash: UnlockHash, issued: bool, complete: bool,
	inputs: Vec<ExchangeInputFacts>,
) -> ExchangeEvidence {
	let any_forfeited = inputs.iter().any(|input| input.forfeited);
	let all_forfeited = !inputs.is_empty() && inputs.iter().all(|input| input.forfeited);
	let missing = inputs.is_empty() || inputs.iter().any(|input| input.missing) || (complete && !all_forfeited);
	ExchangeEvidence { hash, issued, complete: complete && all_forfeited && !missing,
		partial_forfeits: any_forfeited && !(complete && all_forfeited),
		exited: inputs.iter().any(|input| input.exited), missing,
		inputs: inputs.into_iter().map(|input| input.id).collect() }
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(raw: D) -> Result<Option<T>,D::Error> {
	<Option<T> as serde::Deserialize>::deserialize(raw)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedExchangeParticipation {
	pub id: i64,
	pub unlock_hash: String,
	#[serde(deserialize_with = "required_nullable")]
	pub round_id: Option<Txid>,
	#[serde(deserialize_with = "required_nullable")]
	pub forfeited_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedExchangeSource {
	pub vtxo_id: VtxoId,
	#[serde(deserialize_with = "required_nullable")]
	pub confirmed_height: Option<i32>,
	#[serde(deserialize_with = "required_nullable")]
	pub offboarded_in: Option<Txid>,
	#[serde(deserialize_with = "required_nullable")]
	pub oor_spent_txid: Option<Txid>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedExchangeInput {
	pub participation_id: i64,
	pub vtxo_id: VtxoId,
	#[serde(deserialize_with = "required_nullable")]
	pub signed_forfeit_tx: Option<Vec<u8>>,
	#[serde(deserialize_with = "required_nullable")]
	pub source: Option<CapturedExchangeSource>,
}

/// Persisted dependency observations only, never Operator/paid/refund/retirement
/// authorization. Even an unfinished exchange with no pending parse error still
/// requires its complete physical retirement; it is not unencumbered principal.
pub(crate) struct CapturedExchangeClosure {
	pub unlock_hash: UnlockHash,
	pub inputs: Vec<VtxoId>,
	pub round_assigned: bool,
	pub completion_fields_present: bool,
	pub requires_complete_exchange_closure: bool,
	pub partial_forfeits: bool,
	pub exited: bool,
	pub missing: bool,
	pub pending: Option<OwnershipPending>,
}

/// Use the SAME native aggregation as exchange_evidence. The captured caller
/// must separately prove the complete original same-RR membership, actual Full
/// originals/forfeit signatures, linked Round rights, chain and current state.
/// No preimage is accepted, returned or inferred to be a signed forfeit.
pub(crate) fn captured_exchange_closure(participation: &CapturedExchangeParticipation,
	inputs: &[CapturedExchangeInput], limit: usize,
) -> anyhow::Result<CapturedExchangeClosure> {
	ensure!(participation.id > 0 && limit > 0 && limit <= crate::payout_witness::DIRECT_HISTORY_LIMITS.max_rows
		&& inputs.len() <= limit, "captured complete exchange exceeds native source bound");
	let mut ids = BTreeSet::new(); let mut bytes = 0usize;
	for input in inputs {
		ensure!(input.participation_id == participation.id && ids.insert(input.vtxo_id),
			"captured exchange input association is foreign or repeated");
		if let Some(raw) = &input.signed_forfeit_tx {
			bytes = bytes.checked_add(raw.len()).context("captured exchange original bytes overflow")?;
			ensure!(raw.len() <= 4 * 1024 * 1024 && bytes <= 16 * 1024 * 1024,
				"complete captured original forfeits exceed native byte bound");
		}
		if let Some(source) = &input.source {
			ensure!(source.vtxo_id == input.vtxo_id && source.confirmed_height.is_none_or(|height| height >= 0),
				"captured exchange original source identity or height differs");
		}
	}
	for input in inputs {
		if let Some(raw) = &input.signed_forfeit_tx {
			let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(raw)?;
			ensure!(bitcoin::consensus::serialize(&tx) == *raw && !tx.input.is_empty()
				&& tx.input.iter().all(|point| !point.witness.is_empty()), "captured signed-forfeit original body is malformed");
		}
	}
	let hash = UnlockHash::from_str(&participation.unlock_hash)?;
	let evidence = summarize_exchange(hash, participation.round_id.is_some(), participation.forfeited_at.is_some(),
		inputs.iter().map(|input| ExchangeInputFacts { id:input.vtxo_id, forfeited:input.signed_forfeit_tx.is_some(),
			missing:input.source.is_none(), exited:input.source.as_ref().is_some_and(|source| source.confirmed_height.is_some()
				|| source.offboarded_in.is_some() || source.oor_spent_txid.is_some()) }).collect());
	let pending = evidence.pending();
	Ok(CapturedExchangeClosure { unlock_hash:evidence.hash,inputs:evidence.inputs,round_assigned:evidence.issued,
		completion_fields_present:evidence.complete,requires_complete_exchange_closure:!evidence.complete,
		partial_forfeits:evidence.partial_forfeits,exited:evidence.exited,missing:evidence.missing,pending })
}

impl ExchangeEvidence {
	fn dependency(&self, original: bool) -> RetirementDependency {
		RetirementDependency { source: DependencySource::Exchange(self.hash), ids: self.inputs.clone(),
			rule: if original { RetirementRule::CompleteExchangeClosure } else { RetirementRule::ConfirmedOrSameCarrier } }
	}

	fn pending(&self) -> Option<OwnershipPending> {
		if self.missing { Some(OwnershipPending::MissingExchange) }
		else if !self.issued { Some(OwnershipPending::ExchangeNotIssued(self.hash)) }
		else if self.partial_forfeits { Some(OwnershipPending::PartialForfeits(self.hash)) }
		else if !self.complete && self.exited { Some(OwnershipPending::ExchangeCancellation(self.hash)) }
		else { None }
	}
}

impl Tx<'_> {
	/// `covered` must be the complete current leaf partition of the supplied roots,
	/// established using actual funding and path containment. Anchor inventory is
	/// only a superset. Frozen allocations take precedence outside this function.
	/// Refund evidence must be observed under its payment guard; even Refundable
	/// never removes physical grant dependencies or the commit-time preimage fence.
	pub async fn expiry_ownership_proposal(
		&self, snapshot: &BackingSnapshot, covered: &[VtxoId], context: &OwnershipContext<'_>,
		send_evidence: &BTreeMap<PaymentHash, SendRefundEvidence>,
	) -> anyhow::Result<Vec<OwnershipDecision>> {
		ensure!(context.max_dependencies > 0, "ownership dependency budget must be positive");
		let ids = covered.iter().copied().collect::<BTreeSet<_>>();
		ensure!(ids.len() == covered.len(), "ownership partition has duplicate leaves");
		ensure!(covered.len() <= snapshot.regular.len() + snapshot.pending_boards.len(), "ownership partition exceeds source inventory");
		let keys = covered.iter().map(ToString::to_string).collect::<Vec<_>>();
		let candidates = self.query(&format!("SELECT c.vtxo_id FROM ({CANDIDATES}) c WHERE c.vtxo_id=ANY($3)"),
			&[&(context.grace as i64), &(context.tip as i64), &keys]).await?
			.into_iter().map(|r| VtxoId::from_str(r.get("vtxo_id"))).collect::<Result<BTreeSet<_>, _>>()?;
		let mut decisions = Vec::with_capacity(covered.len());
		let mut dependencies = BTreeSet::new();
		for id in covered {
			let mut decision = if let Some(fact) = snapshot.regular.iter().find(|f| f.state.vtxo_id == *id) {
				self.regular_ownership(snapshot, fact, context, candidates.contains(id), send_evidence).await?
			} else if let Some(fact) = snapshot.pending_boards.iter().find(|f| f.vtxo.id() == *id) {
				let v = &fact.vtxo;
				let disposition = if !mature(v.expiry_height().to_u32(), context) { pending(OwnershipPending::NotMature) }
					else if !candidates.contains(id) { pending(OwnershipPending::SourceChanged) }
					else if let Some(reason) = validate_source(v, context)? { pending(reason) }
					else { customer(*id, ExpirySource::PendingBoard, v.user_pubkey()) };
				OwnershipDecision { id: *id, gross: v.amount(), disposition, dependencies: Vec::new() }
			} else { bail!("ownership partition leaf {id} has no source evidence") };
			for dependency in &mut decision.dependencies {
				dependency.ids.sort();
				dependency.ids.dedup();
				dependencies.extend(dependency.ids.iter().copied());
			}
			ensure!(dependencies.len() <= context.max_dependencies, "complete ownership proposal exceeds dependency budget");
			decisions.push(decision);
		}
		Ok(decisions)
	}

	async fn regular_ownership(
		&self, snapshot: &BackingSnapshot, fact: &BackingVtxoFact, context: &OwnershipContext<'_>, candidate: bool,
		send_evidence: &BTreeMap<PaymentHash, SendRefundEvidence>,
	) -> anyhow::Result<OwnershipDecision> {
		let state = &fact.state;
		let id = state.vtxo_id;
		let mut decision = OwnershipDecision { id, gross: state.vtxo.amount(),
			disposition: pending(OwnershipPending::UnsupportedSource), dependencies: Vec::new() };
		if !mature(state.vtxo.expiry_height().to_u32(), context) {
			decision.disposition = pending(OwnershipPending::NotMature); return Ok(decision);
		}
		if state.confirmed_height.is_some() || fact.onchain_spent_txid.is_some() || state.offboarded_in.is_some() || state.oor_spent_txid.is_some() {
			decision.disposition = pending(OwnershipPending::ExitedOrConsumed); return Ok(decision);
		}
		let Some(full) = &fact.full_vtxo else {
			decision.disposition = pending(OwnershipPending::MissingPath); return Ok(decision);
		};
		ensure!(full.server_pubkey() == context.server_pubkey, "ownership source uses another server signer");
		let Some(funding) = context.funding.get(&full.chain_anchor().txid) else {
			decision.disposition = pending(OwnershipPending::MissingFunding); return Ok(decision);
		};
		let locked_pool = state.spend_state == SpendState::Pool && full.unlock_hash().is_some() && !full.has_all_witnesses();
		if state.spend_state == SpendState::Unregistered || state.spend_state == SpendState::Unclaimed || locked_pool {
			// Unregistered Arkoor and unclaimed hArk deliberately store an
			// unsigned/unfinalized terminal. Source associations and physical
			// ancestor signing remain mandatory; no recipient signature is invented.
			full.validate_unsigned(funding).context("unfinalized ownership path is invalid")?;
			if state.spend_state == SpendState::Unclaimed || locked_pool {
				// The hArk terminal is intentionally locked/unfinalized. Verify its
				// signed immediate ancestor, rather than asking for a user signature
				// or releasing the server-held unlock material to resolve ownership.
				let parent = full.transactions().last().and_then(|item| item.tx.input.first().map(|i| VtxoId::from(i.previous_output)));
				let signed_parent = parent.and_then(|id| snapshot.regular.iter().find(|f| f.state.vtxo_id == id))
					.and_then(|f| f.full_vtxo.as_ref());
				let Some(signed_parent) = signed_parent else {
					decision.disposition = pending(OwnershipPending::MissingPath); return Ok(decision);
				};
				ensure!(signed_parent.server_pubkey() == context.server_pubkey, "unclaimed source ancestor uses another signer");
				signed_parent.validate(funding).context("unclaimed ownership ancestor signature is invalid")?;
			}
		} else { full.validate(funding).context("ownership source signature or path is invalid")?; }
		if state.spend_state == SpendState::RoundForfeit {
			return self.round_forfeit_ownership(decision, snapshot, full, context).await;
		}
		let user = full.clone().try_into_user_vtxo().ok();
		// These exact padding constants are emitted by the round builder. The
		// participation must also be absent; an owner key alone is insufficient.
		if state.spend_state == SpendState::Unclaimed && is_padding(full) {
			if self.query_opt("SELECT id FROM round_participation WHERE unlock_hash=$1", &[&full.unlock_hash().unwrap().to_string()]).await?.is_none() {
				decision.disposition = OwnershipDisposition::Operator(OperatorRecovery::Padding); return Ok(decision);
			}
		}
		if state.spend_state == SpendState::Pool {
			let unused = self.query_opt("SELECT vtxo_id FROM vtxo_pool WHERE vtxo_id=$1 AND spent_at IS NULL", &[&id.to_string()]).await?.is_some();
			let granted = fact.lightning_htlc_subscription_id.is_some() || self.query_opt("SELECT id FROM htlc_vtxo WHERE id=$1", &[&state.id]).await?.is_some();
			let retirement = if let Some((input, spent)) = self.pool_change_retirement_input(id).await? {
				full.transactions().any(|item| item.tx.compute_txid() == spent &&
					item.tx.input.iter().any(|i| i.previous_output == input.to_point()))
			} else { false };
			if (unused || retirement) && !granted && state.spent_in_round.is_none() {
				decision.disposition = OwnershipDisposition::Operator(OperatorRecovery::UnusedPool);
			}
			return Ok(decision);
		}
		let original = self.exchange_original(id, state.spent_in_round, context.max_dependencies).await?;
		if let Some(exchange) = original {
			if !exchange.complete { decision.dependencies.push(exchange.dependency(true)); }
			decision.disposition = if let Some(reason) = exchange.pending() { pending(reason) }
				else if exchange.complete { OwnershipDisposition::Operator(OperatorRecovery::CompletedExchange(exchange.hash)) }
				else { OwnershipDisposition::ConditionalOriginalRecovery(exchange.hash) };
			return Ok(decision);
		}
		if state.spend_state == SpendState::Unclaimed {
			let Some(vtxo) = user.as_ref() else { return Ok(decision); };
			let Some(hash) = vtxo.unlock_hash() else { decision.disposition = pending(OwnershipPending::MissingExchange); return Ok(decision); };
			let Some(exchange) = self.exchange_replacement(hash, vtxo.chain_anchor().txid, context.max_dependencies).await? else {
				decision.disposition = pending(OwnershipPending::MissingExchange); return Ok(decision);
			};
			if !exchange.complete { decision.dependencies.push(exchange.dependency(false)); }
			decision.disposition = if let Some(reason) = exchange.pending() { pending(reason) }
				else { customer(id, ExpirySource::Registered, vtxo.user_pubkey()) };
			return Ok(decision);
		}
		let Some(vtxo) = user else { return Ok(decision); };
		// Before signed-chain registration, even a cosigned HTLC output belongs
		// to its input owner. Its policy does not create an HTLC source record.
		if state.spend_state == SpendState::Unregistered {
			decision.disposition = if !candidate { pending(OwnershipPending::SourceChanged) }
				else { match self.unregistered_expiry_owner(&vtxo).await? {
					Some(owner) => customer(id, ExpirySource::Unregistered, owner),
					None => pending(OwnershipPending::MissingInputOwner),
				} };
			return Ok(decision);
		}
		if vtxo.policy().as_server_htlc_recv().is_some() {
			return self.receive_ownership(decision, fact, &vtxo, context).await;
		}
		if vtxo.policy().as_server_htlc_send().is_some() {
			return self.send_ownership(decision, fact, &vtxo, context, send_evidence).await;
		}
		if !candidate { decision.disposition = pending(OwnershipPending::SourceChanged); return Ok(decision); }
		decision.disposition = customer(id, ExpirySource::Registered, vtxo.user_pubkey());
		Ok(decision)
	}

	async fn round_forfeit_ownership(
		&self, mut decision: OwnershipDecision, snapshot: &BackingSnapshot,
		full: &ark::ServerVtxo<ark::vtxo::Full>, context: &OwnershipContext<'_>,
	) -> anyhow::Result<OwnershipDecision> {
		let (hash, key) = match full.policy() {
			ServerVtxoPolicy::HarkForfeit(policy) => (policy.unlock_hash, policy.user_pubkey),
			ServerVtxoPolicy::HarkForfeit_v0(policy) => (policy.unlock_hash, policy.user_pubkey),
			_ => return Ok(decision),
		};
		let Some(last) = full.transactions().last() else { return Ok(decision); };
		let [input] = last.tx.input.as_slice() else { return Ok(decision); };
		let parent = snapshot.regular.iter().find(|f| f.state.vtxo_id == VtxoId::from(input.previous_output))
			.and_then(|f| f.full_vtxo.as_ref()).and_then(|v| v.clone().try_into_user_vtxo().ok());
		let Some(parent) = parent else { decision.disposition = pending(OwnershipPending::MissingPath); return Ok(decision); };
		ensure!(parent.server_pubkey() == context.server_pubkey && parent.user_pubkey() == key
			&& parent.chain_anchor() == full.chain_anchor() && parent.expiry_height() == full.expiry_height()
			&& parent.exit_delta() == full.exit_delta() && parent.amount() == full.amount(), "ownership forfeit metadata differs from its actual original");
		parent.validate(context.funding.get(&parent.chain_anchor().txid).context("forfeit original funding missing")?)
			.context("forfeit original source signature is invalid")?;
		let row = self.query_opt("SELECT p.id, p.unlock_hash, true AS issued, p.forfeited_at IS NOT NULL AS complete, i.signed_forfeit_tx
			FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id
			JOIN round r ON r.funding_txid=p.round_id JOIN vtxo original ON original.vtxo_id=i.vtxo_id
			WHERE i.vtxo_id=$1 AND p.unlock_hash=$2 AND original.spent_in_round=r.id
			AND original.oor_spent_txid=$3 AND i.signed_forfeit_tx IS NOT NULL",
			&[&parent.id().to_string(), &hash.to_string(), &full.point().txid.to_string()]).await?;
		let Some(row) = row else { decision.disposition = pending(OwnershipPending::MissingExchange); return Ok(decision); };
		let signed: Transaction = bitcoin::consensus::deserialize(row.get("signed_forfeit_tx"))?;
		ensure!(signed == last.tx && signed.compute_txid() == full.point().txid && full.point().vout == 0
			&& signed.input.len() == 1 && signed.input[0].previous_output == parent.point()
			&& signed.output.len() == 2 && signed.output[0] == full.txout()
			&& signed.output[1] == bitcoin_ext::fee::fee_anchor(), "ownership forfeit differs from the completed exchange");
		let exchange = self.exchange_evidence(&row, context.max_dependencies).await?;
		if !exchange.complete { decision.dependencies.push(exchange.dependency(true)); }
		decision.disposition = if exchange.complete { OwnershipDisposition::Operator(OperatorRecovery::CompletedExchange(hash)) }
			else { pending(exchange.pending().unwrap_or(OwnershipPending::MissingExchange)) };
		Ok(decision)
	}

	async fn exchange_original(&self, id: VtxoId, spent_round: Option<i64>, limit: usize) -> anyhow::Result<Option<ExchangeEvidence>> {
		let rows = self.query("SELECT p.id, p.unlock_hash, p.round_id IS NOT NULL AS issued, p.forfeited_at IS NOT NULL AS complete
			FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id
			LEFT JOIN round r ON r.funding_txid=p.round_id
			WHERE i.vtxo_id=$1 AND (p.forfeited_at IS NULL OR r.id=$2) ORDER BY p.id LIMIT 2",
			&[&id.to_string(), &spent_round]).await?;
		ensure!(rows.len() <= 1, "ownership original has conflicting exchange associations");
		let Some(row) = rows.first() else { return Ok(None); };
		self.exchange_evidence(row, limit).await.map(Some)
	}

	async fn exchange_replacement(&self, hash: UnlockHash, round_txid: Txid, limit: usize) -> anyhow::Result<Option<ExchangeEvidence>> {
		let row = self.query_opt("SELECT p.id, p.unlock_hash, true AS issued, p.forfeited_at IS NOT NULL AS complete
			FROM round_participation p JOIN round r ON r.funding_txid=p.round_id WHERE p.unlock_hash=$1 AND r.funding_txid=$2",
			&[&hash.to_string(), &round_txid.to_string()]).await?;
		let Some(row) = row else { return Ok(None); };
		self.exchange_evidence(&row, limit).await.map(Some)
	}

	async fn exchange_evidence(&self, row: &tokio_postgres::Row, limit: usize) -> anyhow::Result<ExchangeEvidence> {
		let limit_plus_one = i64::try_from(limit)?.checked_add(1).context("dependency budget overflow")?;
		let inputs = self.query("SELECT i.vtxo_id, i.signed_forfeit_tx IS NOT NULL AS forfeited,
			v.id IS NULL AS missing, v.confirmed_height IS NOT NULL OR v.offboarded_in IS NOT NULL OR v.oor_spent_txid IS NOT NULL AS exited
			FROM round_part_input i LEFT JOIN vtxo v ON v.vtxo_id=i.vtxo_id WHERE i.participation_id=$1 ORDER BY i.vtxo_id LIMIT $2",
			&[&row.get::<_, i64>("id"), &limit_plus_one]).await?;
		ensure!(inputs.len() <= limit, "complete exchange closure exceeds dependency budget");
		let facts = inputs.iter().map(|input| Ok(ExchangeInputFacts {
			id:VtxoId::from_str(input.get("vtxo_id"))?,forfeited:input.get("forfeited"),missing:input.get("missing"),
			exited:input.get::<_,Option<bool>>("exited").unwrap_or(false),
		})).collect::<anyhow::Result<Vec<_>>>()?;
		Ok(summarize_exchange(UnlockHash::from_str(row.get("unlock_hash"))?,row.get("issued"),row.get("complete"),facts))
	}

	async fn receive_ownership(&self, mut decision: OwnershipDecision, fact: &BackingVtxoFact, vtxo: &Vtxo, context: &OwnershipContext<'_>) -> anyhow::Result<OwnershipDecision> {
		let hash = vtxo.policy().as_server_htlc_recv().unwrap().payment_hash;
		let evidence = self.query_opt("SELECT h.direction::text, h.offchain_resolution::text, h.chain_resolution::text,
			r.status::text AS status, r.id AS subscription_id, r.id= v.lightning_htlc_subscription_id AS bound,
			EXISTS (SELECT 1 FROM htlc_settlement s WHERE s.payment_hash=h.payment_hash) AS preimage
			FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id LEFT JOIN lightning_htlc_subscription r ON r.payment_hash=h.payment_hash
			WHERE h.id=$1 AND h.payment_hash=$2", &[&fact.state.id, &hash.to_string()]).await?;
		let Some(row) = evidence else { decision.disposition = pending(OwnershipPending::MissingGrantEvidence(hash)); return Ok(decision); };
		let preimage: bool = row.get("preimage");
		let status = row.get::<_, Option<&str>>("status");
		let valid = row.get::<_, &str>("direction") == "outgoing" && row.get::<_, Option<bool>>("bound") == Some(true)
			&& row.get::<_, Option<&str>>("offchain_resolution").is_none() && row.get::<_, Option<&str>>("chain_resolution").is_none()
			&& fact.state.spend_state == SpendState::HtlcRecvUnclaimed;
		let attempts = self.get_payment_attempts_by_payment_hash(hash).await?;
		let subscription_id: Option<i64> = row.get("subscription_id");
		let pure_intra = !attempts.is_empty() && subscription_id.is_some()
			&& attempts.iter().all(|a| a.lightning_htlc_subscription_id == subscription_id);
		let deadline_passed = context.tip > vtxo.policy().as_server_htlc_recv().unwrap().htlc_expiry.to_u32();
		decision.disposition = if !valid { pending(OwnershipPending::MissingGrantEvidence(hash)) }
			else if status == Some("settled") && preimage { customer(decision.id, ExpirySource::LightningReceive(hash), vtxo.user_pubkey()) }
			else if preimage || status == Some("settled") { pending(OwnershipPending::ReceiveCollection(hash)) }
			else if status == Some("canceled") && deadline_passed && context.canceled_receives.contains(&hash) {
				OwnershipDisposition::Operator(OperatorRecovery::UnpaidReceive(hash))
			} else if pure_intra && deadline_passed && matches!(status, Some("htlcs-ready" | "canceled"))
				&& !attempts.iter().any(|a| a.status == LightningPaymentStatus::Succeeded) {
				let grants = self.receive_grant_ids(hash, context.max_dependencies).await?.unwrap_or_default();
				if grants.is_empty() { pending(OwnershipPending::MissingGrantEvidence(hash)) }
				else { decision.dependencies.push(RetirementDependency { source: DependencySource::ReceiveGrants(hash), ids: grants,
					rule: RetirementRule::ConfirmedOrSameCarrier }); OwnershipDisposition::ConditionalUnpaidReceive(hash) }
			} else { pending(OwnershipPending::ReceiveCollection(hash)) };
		Ok(decision)
	}

	async fn send_ownership(&self, mut decision: OwnershipDecision, fact: &BackingVtxoFact, vtxo: &Vtxo, context: &OwnershipContext<'_>, evidence: &BTreeMap<PaymentHash, SendRefundEvidence>) -> anyhow::Result<OwnershipDecision> {
		let hash = vtxo.policy().as_server_htlc_send().unwrap().payment_hash;
		let row = self.query_opt("SELECT direction::text, offchain_resolution::text, chain_resolution::text FROM htlc_vtxo WHERE id=$1 AND payment_hash=$2", &[&fact.state.id, &hash.to_string()]).await?;
		let valid = row.as_ref().is_some_and(|r| r.get::<_, &str>("direction") == "incoming"
			&& r.get::<_, Option<&str>>("offchain_resolution") != Some("revoked") && r.get::<_, Option<&str>>("chain_resolution").is_none());
		if !valid || !matches!(fact.state.spend_state, SpendState::Spendable | SpendState::LnSpent) {
			decision.disposition = pending(OwnershipPending::SourceChanged); return Ok(decision);
		}
		// Re-read the authoritative preimage even when an earlier node observation
		// was Refundable. A node verdict does not authorize the receive principal.
		let preimage = self.get_htlc_settlement_by_payment_hash(hash).await?.is_some();
		let attempts = self.get_payment_attempts_by_payment_hash(hash).await?;
		if preimage || attempts.iter().any(|a| a.status == LightningPaymentStatus::Succeeded)
			|| matches!(evidence.get(&hash), Some(SendRefundEvidence::Paid(_))) {
			decision.disposition = OwnershipDisposition::Operator(OperatorRecovery::PaidLightningSend(hash)); return Ok(decision);
		}
		if row.as_ref().is_some_and(|r| r.get::<_, Option<&str>>("offchain_resolution").is_some()) {
			decision.disposition = pending(OwnershipPending::SendEvidence(hash)); return Ok(decision);
		}
		decision.disposition = match evidence.get(&hash) {
			Some(SendRefundEvidence::Refundable) => customer(decision.id, ExpirySource::LightningSend(hash), vtxo.user_pubkey()),
			Some(SendRefundEvidence::Unresolved(detail)) => pending(OwnershipPending::LightningOutcome { payment_hash: hash, detail: detail.clone() }),
			None => pending(OwnershipPending::SendEvidence(hash)),
			Some(SendRefundEvidence::Paid(_)) => unreachable!(),
		};
		if matches!(decision.disposition, OwnershipDisposition::Customer(_)) {
			let grants = self.receive_grant_ids(hash, context.max_dependencies).await?;
			match grants {
				Some(ids) if !ids.is_empty() => decision.dependencies.push(RetirementDependency { source: DependencySource::ReceiveGrants(hash), ids, rule: RetirementRule::ConfirmedOrSameCarrier }),
				Some(_) => decision.disposition = pending(OwnershipPending::MissingGrantEvidence(hash)),
				None => {},
			}
		}
		Ok(decision)
	}

	async fn receive_grant_ids(&self, hash: PaymentHash, limit: usize) -> anyhow::Result<Option<Vec<VtxoId>>> {
		let attempts = self.get_payment_attempts_by_payment_hash(hash).await?;
		let subscriptions = attempts.iter().filter_map(|a| a.lightning_htlc_subscription_id).collect::<BTreeSet<_>>();
		let row = self.query_opt("SELECT id, status::text FROM lightning_htlc_subscription WHERE payment_hash=$1", &[&hash.to_string()]).await?;
		let Some(row) = row else { return Ok(if subscriptions.is_empty() { None } else { Some(Vec::new()) }); };
		if subscriptions.iter().any(|id| *id != row.get::<_, i64>("id")) { return Ok(Some(Vec::new())); }
		if row.get::<_, &str>("status") == "settled" { return Ok(Some(Vec::new())); }
		let limit_plus_one = i64::try_from(limit)?.checked_add(1).context("dependency budget overflow")?;
		let rows = self.query("SELECT v.vtxo_id FROM vtxo v WHERE v.lightning_htlc_subscription_id=$1 ORDER BY v.vtxo_id LIMIT $2",
			&[&row.get::<_, i64>("id"), &limit_plus_one]).await?;
		ensure!(rows.len() <= limit, "complete grant closure exceeds dependency budget");
		// Created/Accepted subscriptions have never handed out a grant. Their
		// cancellation remains a commit predicate, rather than invented grant data.
		if rows.is_empty() && matches!(row.get::<_, &str>("status"), "created" | "accepted") { return Ok(None); }
		// A canceled prepared history with missing grants is still incomplete.
		// Only the existing durable setup-timeout failure of every matching
		// intra-Ark attempt identifies a canceled, never-prepared receive.
		if rows.is_empty() && row.get::<_, &str>("status") == "canceled" && !attempts.is_empty()
			&& attempts.iter().all(|a| a.lightning_htlc_subscription_id == Some(row.get::<_, i64>("id"))
				&& a.status == LightningPaymentStatus::Failed && a.error.as_deref() == Some("htlc vtxo setup timed out"))
			&& self.get_htlc_settlement_by_payment_hash(hash).await?.is_none()
		{ return Ok(None); }
		Ok(Some(rows.iter().map(|r| VtxoId::from_str(r.get("vtxo_id"))).collect::<Result<_, _>>()?))
	}
}

impl Server {
	/// Read-only per-pass observations under the existing payment guards.
	/// The guards are released before return; freeze must reacquire and re-read.
	pub async fn observe_expiry_send_evidence(
		&self, snapshot: &BackingSnapshot, covered: &[VtxoId], unavailable_nodes: &mut BTreeSet<LightningNodeId>,
	) -> anyhow::Result<BTreeMap<PaymentHash, SendRefundEvidence>> {
		let ids = covered.iter().copied().collect::<BTreeSet<_>>();
		let mut sends = BTreeMap::new();
		for fact in snapshot.regular.iter().filter(|f| ids.contains(&f.state.vtxo_id)) {
			let Some(vtxo) = fact.full_vtxo.as_ref().and_then(|v| v.clone().try_into_user_vtxo().ok()) else { continue; };
			if let Some(policy) = vtxo.policy().as_server_htlc_send() { sends.insert(policy.payment_hash, policy.htlc_expiry); }
		}
		let mut evidence = BTreeMap::new();
		for (hash, expiry) in sends {
			let Some(_guard) = self.payment_guards.lock(hash).now_or_never() else {
				evidence.insert(hash, SendRefundEvidence::Unresolved("payment is in progress".into())); continue;
			};
			// This supplies only the outgoing fact. The source proposal attaches
			// all granted receive IDs; freeze must cancel and retire their complete
			// closure in this carrier or require confirmed retirement beforehand.
			let observed = self.lightning_send_outgoing_evidence(hash, expiry, unavailable_nodes).await?;
			evidence.insert(hash, observed);
		}
		Ok(evidence)
	}
}

fn mature(expiry: u32, context: &OwnershipContext<'_>) -> bool {
	u64::from(expiry) + u64::from(context.grace) <= u64::from(context.tip)
}

fn pending(reason: OwnershipPending) -> OwnershipDisposition { OwnershipDisposition::Pending(reason) }

fn customer(id: VtxoId, source: ExpirySource, owner: PublicKey) -> OwnershipDisposition {
	OwnershipDisposition::Customer(ExpiryInput { id, source, owner })
}

fn validate_source(vtxo: &Vtxo, context: &OwnershipContext<'_>) -> anyhow::Result<Option<OwnershipPending>> {
	ensure!(vtxo.server_pubkey() == context.server_pubkey, "ownership source uses another server signer");
	let Some(funding) = context.funding.get(&vtxo.chain_anchor().txid) else { return Ok(Some(OwnershipPending::MissingFunding)); };
	// Pending Board storage precedes the user's combination of the cosign
	// witness. Its funded aggregate-key script and immutable origin establish
	// ownership; expiry consumes that actual anchor using the server signer.
	vtxo.validate_unsigned(funding).context("pending Board ownership path is invalid")?;
	Ok(None)
}

pub(super) fn is_padding(vtxo: &ark::ServerVtxo<ark::vtxo::Full>) -> bool {
	let ServerVtxoPolicy::User(policy) = vtxo.policy() else { return false; };
	policy.as_pubkey().is_some()
		&& policy.user_pubkey().to_string() == "02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c"
		&& vtxo.unlock_hash().is_some_and(|h| h.to_string() == "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
		&& vtxo.amount() == Amount::from_sat(330)
}

#[cfg(test)]
mod captured_exchange_tests {
	use super::*;
	use bitcoin::hashes::Hash;
	fn fixture() -> (CapturedExchangeParticipation,Vec<CapturedExchangeInput>) {
		let id=VtxoId::from(bitcoin::OutPoint::new(Txid::from_byte_array([7;32]),0));
		(CapturedExchangeParticipation {id:7,unlock_hash:UnlockHash::from_byte_array([9;32]).to_string(),
			round_id:Some(Txid::from_byte_array([8;32])),forfeited_at:None},
			vec![CapturedExchangeInput {participation_id:7,vtxo_id:id,signed_forfeit_tx:None,
				source:Some(CapturedExchangeSource {vtxo_id:id,confirmed_height:None,offboarded_in:None,oor_spent_txid:None})}])
	}
	fn retained_signed_body(point: VtxoId) -> Vec<u8> {
		// Canonical synthetic body only. No signature/forfeit/ownership authority.
		bitcoin::consensus::serialize(&bitcoin::Transaction {version:bitcoin::transaction::Version::TWO,
			lock_time:bitcoin::absolute::LockTime::ZERO,input:vec![bitcoin::TxIn {previous_output:point.to_point(),
				script_sig:bitcoin::ScriptBuf::new(),sequence:bitcoin::Sequence::ZERO,
				witness:bitcoin::Witness::from_slice(&[vec![1;64]])}],
			output:vec![bitcoin::TxOut {value:Amount::from_sat(10_000),script_pubkey:bitcoin::ScriptBuf::new()}]})
	}
	#[test]
	fn captured_exchange_keeps_unfinished_original_dependency_instead_of_free_operator_principal() {
		let (mut participation,mut inputs)=fixture();
		let observed=captured_exchange_closure(&participation,&inputs,10).unwrap();
		assert!(observed.round_assigned && !observed.completion_fields_present && observed.requires_complete_exchange_closure);
		assert_eq!(observed.pending,None);
		assert_eq!(observed.inputs,vec![inputs[0].vtxo_id]);
		assert_eq!(observed.unlock_hash,participation.unlock_hash.parse().unwrap());
		inputs[0].signed_forfeit_tx=Some(retained_signed_body(inputs[0].vtxo_id));
		let partial=captured_exchange_closure(&participation,&inputs,10).unwrap();
		assert!(partial.partial_forfeits && partial.requires_complete_exchange_closure);
		assert_eq!(partial.pending,Some(OwnershipPending::PartialForfeits(observed.unlock_hash)));
		participation.forfeited_at=Some(chrono::Utc::now());
		let retained=captured_exchange_closure(&participation,&inputs,10).unwrap();
		assert!(retained.completion_fields_present && !retained.requires_complete_exchange_closure);
		assert!(!retained.missing && !retained.partial_forfeits);
		// This records native persisted presence only, not valid forfeit signatures
		// or physical retirement; the return type has no owner/payment capability.
	}
	#[test]
	fn captured_exchange_missing_partial_unissued_or_exited_states_remain_explicit() {
		for which in 0..5 {
			let (mut participation,mut inputs)=fixture();
			let hash:UnlockHash=participation.unlock_hash.parse().unwrap();
			let expected=match which {
				0 => {inputs.clear();OwnershipPending::MissingExchange},
				1 => {inputs[0].source=None;OwnershipPending::MissingExchange},
				2 => {participation.round_id=None;OwnershipPending::ExchangeNotIssued(hash)},
				3 => {participation.forfeited_at=Some(chrono::Utc::now());OwnershipPending::MissingExchange},
				_ => {inputs[0].source.as_mut().unwrap().confirmed_height=Some(1);OwnershipPending::ExchangeCancellation(hash)},
			};
			let observed=captured_exchange_closure(&participation,&inputs,10).unwrap();
			assert!(!observed.completion_fields_present && observed.requires_complete_exchange_closure);
			assert_eq!(observed.pending,Some(expected));
			if which==4 {assert!(observed.exited);}
		}
	}
	#[test]
	fn captured_exchange_refuses_missing_nullable_columns_foreign_join_duplicate_or_malformed_originals() {
		let (participation,inputs)=fixture();
		let mut raw=serde_json::to_value(&participation).unwrap();raw.as_object_mut().unwrap().remove("round_id");
		assert!(serde_json::from_value::<CapturedExchangeParticipation>(raw).is_err());
		for field in ["source","signed_forfeit_tx"] {
			let mut raw=serde_json::to_value(&inputs[0]).unwrap();raw.as_object_mut().unwrap().remove(field);
			assert!(serde_json::from_value::<CapturedExchangeInput>(raw).is_err());
		}
		for which in 0..5 {
			let mut changed=inputs.clone();
			match which {
				0 => changed[0].participation_id+=1,
				1 => changed.push(changed[0].clone()),
				2 => changed[0].signed_forfeit_tx=Some(vec![]),
				3 => changed[0].source.as_mut().unwrap().confirmed_height=Some(-1),
				_ => changed[0].source.as_mut().unwrap().vtxo_id=VtxoId::from(bitcoin::OutPoint::new(Txid::from_byte_array([3;32]),0)),
			}
			assert!(captured_exchange_closure(&participation,&changed,10).is_err());
		}
	}
}
