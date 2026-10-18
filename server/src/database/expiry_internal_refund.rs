//! Source-specific preparation for one inseparable internal timeout refund.
//! This is not an ownership authorization: the financial commit must consume
//! every receive grant in the SAME parent as its original incoming backing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use ark::{ProtocolEncoding, ServerVtxo, VtxoId};
use ark::lightning::PaymentHash;
use bitcoin::{OutPoint, Transaction};
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::BlockHeight;
use chrono::{DateTime, Local};

use super::{SpendState, Tx};
use super::expiry_chain::ExpiryViewToken;
use super::expiry_decision::ReadyOutput;
use super::expiry_user_claim::ReplayableTerminalClaim;
use super::htlc_vtxo::{self, HtlcDirection, HtlcVtxo};

pub(crate) const MAX_JOINT_GRANTS: i64 = 64;

#[derive(Clone)]
pub(crate) struct InternalRefundGrant {
	pub vtxo: ServerVtxo,
	confirmed: BlockHeight,
	updated_at: DateTime<Local>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExistingInternalRefund {
	pub entitlement_id: i64,
	pub claimant: PublicKey,
	pub mailbox: PublicKey,
	pub script: bitcoin::ScriptBuf,
	pub gross: bitcoin::Amount,
	/// Every historical physical reservation remains retained. Only this
	/// exact conditional source's conflicted alternatives may be superseded.
	pub allocations: BTreeMap<i64, Vec<VtxoId>>,
}

/// Classification belongs to one rebuilt canonical view. A terminal claim
/// remains a retained user-authorized transaction, never an irreversible
/// off-chain settlement or permission to repay its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InternalRefundSource {
	Live,
	Terminal(ReplayableTerminalClaim),
}

#[derive(Clone)]
pub(crate) struct InternalRefundCandidate {
	pub attempt_id: i64,
	pub subscription_id: i64,
	pub payment_hash: PaymentHash,
	pub view: ExpiryViewToken,
	/// Exact immutable original funding membership, including observed paid
	/// sources. Only Live sources enter this refund's physical input set.
	pub incoming: BTreeMap<VtxoId, InternalRefundSource>,
	pub existing: BTreeMap<VtxoId, ExistingInternalRefund>,
	/// Existing decisions remain immutable. These exact scope fingerprints
	/// add every retained physical row BEFORE the first sorted lock union.
	pub terminal_reservations: BTreeMap<VtxoId, ConflictedInternalUserClaimScope>,
	pub grants: Vec<InternalRefundGrant>,
}

/// Only actual original-attempt membership may widen a fresh terminal proof.
/// Construction stays in this database module; arbitrary caller ID lists are
/// not an authorization. Final financial commit reconstructs it under locks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InternalOriginalClaimScope {
	attempt: i64,
	subscription: i64,
	hash: PaymentHash,
	originals: Vec<VtxoId>,
}

impl InternalOriginalClaimScope {
	pub(crate) fn originals(&self) -> &[VtxoId] { &self.originals }

	pub(crate) fn check_source(&self, source: &HtlcVtxo) -> anyhow::Result<()> {
		ensure!(self.originals.contains(&source.vtxo.vtxo.id()), "batch timeout source is not an original of this internal attempt");
		check_incoming_identity(source, self.hash)
	}

	fn check_evidence(&self, evidence: &serde_json::Value, grants: &[VtxoId]) -> anyhow::Result<()> {
		let values=evidence.get("original_sources").and_then(serde_json::Value::as_array)
			.context("batch premise lacks its complete original membership")?;
		let originals=values.iter().map(|value|value.as_str().context("invalid batch original")?
			.parse::<VtxoId>().map_err(Into::into)).collect::<anyhow::Result<BTreeSet<_>>>()?;
		ensure!(originals.len()==values.len() && originals==self.originals.iter().copied().collect(),
			"batch premise changes its actual original attempt membership");
		check_existing_joint_evidence(evidence,self.attempt,self.subscription,self.hash,Some(grants))
	}
}

/// Original attempts keep their separate funding identities. Only their
/// inseparable Bitcoin input dependency is grouped; this creates no new owner.
pub(crate) struct InternalRefundComponent {
	pub view: ExpiryViewToken,
	pub members: Vec<InternalRefundCandidate>,
}

fn check_existing_joint_evidence(
	evidence: &serde_json::Value, attempt: i64, subscription: i64, hash: PaymentHash, grants: Option<&[VtxoId]>,
) -> anyhow::Result<()> {
	ensure!(evidence.get("kind").and_then(serde_json::Value::as_str) == Some("htlc_internal_joint_timeout")
		&& evidence.get("attempt_id").and_then(serde_json::Value::as_i64) == Some(attempt)
		&& evidence.get("subscription_id").and_then(serde_json::Value::as_i64) == Some(subscription)
		&& evidence.get("payment_hash").and_then(serde_json::Value::as_str) == Some(hash.to_string().as_str()),
		"replan evidence belongs to a different original internal obligation");
	let required = evidence.get("required_grants").and_then(serde_json::Value::as_array)
		.context("joint replan lacks its original complete grant membership")?;
	let ids = required.iter().map(|value| Ok(value.as_str().context("invalid stored joint grant")?.parse::<VtxoId>()?))
		.collect::<anyhow::Result<Vec<VtxoId>>>()?;
	let set = ids.iter().copied().collect::<BTreeSet<_>>();
	ensure!(!ids.is_empty() && set.len() == ids.len(), "joint replan grant membership is empty or repeated");
	if let Some(actual) = grants {
		ensure!(actual.len() == set.len() && actual.iter().copied().collect::<BTreeSet<_>>().len() == actual.len()
			&& actual.iter().all(|id| set.contains(id)),
			"joint replan changes or drops its original receive dependency");
	}
	Ok(())
}

fn check_connected_roots(members: &[BTreeSet<OutPoint>]) -> anyhow::Result<()> {
	let mut connected = members.first().context("empty internal refund component")?.clone();
	ensure!(!connected.is_empty(), "internal refund member has no incoming backing");
	loop {
		let before = connected.len();
		for roots in members {
			ensure!(!roots.is_empty(), "internal refund member has no incoming backing");
			if !roots.is_disjoint(&connected) { connected.extend(roots); }
		}
		if connected.len() == before { break; }
	}
	ensure!(members.iter().all(|roots| roots.is_subset(&connected)),
		"internal refund component combines unrelated backing groups");
	Ok(())
}

impl InternalRefundComponent {
	pub(crate) fn new(view: ExpiryViewToken, members: Vec<InternalRefundCandidate>) -> anyhow::Result<Self> {
		ensure!(!members.is_empty() && members.len() <= MAX_JOINT_GRANTS as usize,
			"internal refund component exceeds its attempt budget");
		let mut attempts = BTreeSet::new();
		let mut subscriptions = BTreeSet::new();
		let mut incoming = BTreeSet::new();
		let mut grants = BTreeSet::new();
		for member in &members {
			ensure!(member.view == view && attempts.insert(member.attempt_id)
				&& subscriptions.insert(member.subscription_id), "internal refund repeats or rebinds an attempt/subscription");
			ensure!(!member.incoming.is_empty() && !member.grants.is_empty()
				&& !member.live_incoming_ids().is_empty(), "internal refund member omits a live obligation");
			ensure!(member.terminal_reservations.iter().all(|(id,scope)| scope.source==*id
				&& matches!(member.incoming.get(id),Some(InternalRefundSource::Terminal(_)))),
				"reserved terminal scope is not its exact observed original");
			ensure!(member.existing.keys().all(|id| matches!(member.incoming.get(id), Some(InternalRefundSource::Live))),
				"joint refund reassigns a terminal source's old reservation");
			for (id, state) in &member.incoming {
				ensure!(incoming.insert(*id), "internal attempts share an original logical source");
				if let InternalRefundSource::Terminal(claim) = state {
					ensure!(claim.source == *id && claim.confirmed.height <= view.captaind.height.min(view.watchmand.height),
						"joint terminal proof belongs to another source or completed prefix");
				}
			}
			for id in member.grant_ids() {
				ensure!(grants.insert(id), "internal attempts repeat a receive grant");
			}
		}
		ensure!(incoming.is_disjoint(&grants) && incoming.len() <= MAX_JOINT_GRANTS as usize
			&& grants.len() <= MAX_JOINT_GRANTS as usize, "internal refund input union overlaps or exceeds its budget");
		Ok(Self { view, members })
	}

	pub(crate) fn incoming_ids(&self) -> Vec<VtxoId> {
		self.members.iter().flat_map(|member| member.incoming.keys().copied()).collect()
	}


	pub(crate) fn obligation_lock_ids(&self) -> Vec<String> {
		self.incoming_ids().into_iter().chain(self.grant_ids()).map(|id|id.to_string())
			.chain(self.members.iter().flat_map(|member|member.terminal_reservations.values())
				.flat_map(ConflictedInternalUserClaimScope::lock_ids))
			.collect::<BTreeSet<_>>().into_iter().collect()
	}

	pub(crate) fn subscription_ids(&self) -> Vec<i64> {
		self.members.iter().map(|member|member.subscription_id)
			.chain(self.members.iter().flat_map(|member|member.terminal_reservations.values())
				.flat_map(|scope|scope.subscriptions.iter().copied()))
			.collect::<BTreeSet<_>>().into_iter().collect()
	}

	pub(crate) fn live_incoming_ids(&self) -> Vec<VtxoId> {
		self.members.iter().flat_map(InternalRefundCandidate::live_incoming_ids).collect()
	}

	pub(crate) fn terminal_ids(&self) -> BTreeSet<VtxoId> {
		self.members.iter().flat_map(|member| member.incoming.iter().filter_map(|(id, state)|
			matches!(state, InternalRefundSource::Terminal(_)).then_some(*id))).collect()
	}

	pub(crate) fn existing_source(&self, id: VtxoId) -> Option<&ExistingInternalRefund> {
		self.members.iter().find_map(|member| member.existing.get(&id))
	}

	pub(crate) fn superseded_allocations(&self) -> Vec<i64> {
		self.members.iter().flat_map(|member| member.existing.values())
			.flat_map(|source| source.allocations.keys().copied())
			.chain(self.members.iter().flat_map(|member|member.terminal_reservations.values()).flat_map(|scope|scope.allocations.iter().copied()))
			.collect::<BTreeSet<_>>().into_iter().collect()
	}

	pub(crate) fn grants(&self) -> impl Iterator<Item = &InternalRefundGrant> {
		self.members.iter().flat_map(|member| &member.grants)
	}

	pub(crate) fn grant_ids(&self) -> Vec<VtxoId> { self.grants().map(|grant| grant.vtxo.id()).collect() }

	pub(crate) fn for_source(&self, id: VtxoId) -> Option<&InternalRefundCandidate> {
		self.members.iter().find(|member| matches!(member.incoming.get(&id), Some(InternalRefundSource::Live)))
	}

	pub(crate) fn check_parent(
		&self, current: ExpiryViewToken, incoming: &[ReadyOutput], parent: &Transaction,
	) -> anyhow::Result<()> {
		let ids = self.live_incoming_ids();
		ensure!(self.terminal_ids().iter().all(|id| incoming.iter().all(|output| !output.contains_coin(*id))),
			"a canonically exited original cannot enter the new customer allocation");
		for id in &ids {
			ensure!(incoming.iter().filter(|output| output.contains_coin(*id)).count() == 1,
				"internal component does not cover one physical path per live original source");
		}
		ensure!(incoming.iter().all(|output| ids.iter().any(|id| output.contains_coin(*id))),
			"internal component adds unrelated backing");
		let member_roots = self.members.iter().map(|member| incoming.iter()
			.filter(|output| member.live_incoming_ids().iter().any(|id| output.contains_coin(*id)))
			.map(|output| output.root().point()).collect()).collect::<Vec<BTreeSet<_>>>();
		check_connected_roots(&member_roots)?;
		for member in &self.members {
			for (source, scope) in &member.existing {
				let output = incoming.iter().find(|output| output.contains_coin(*source))
					.context("replan lost an owed source's current path")?;
				for points in scope.allocations.values() {
					ensure!(output.excludes_old_refund(*source, points)?,
						"replan lacks an exclusive original-path input against an old refund");
				}
			}
		}
		check_parent_inputs(self.view, current,
			&incoming.iter().map(|output| output.root().point()).collect::<Vec<_>>(), &self.grant_ids(), parent)
	}
}

#[cfg(test)]
fn check_incoming(source: &HtlcVtxo, hash: PaymentHash) -> anyhow::Result<()> {
	check_incoming_scope(source, hash, false)
}

fn check_incoming_identity(source: &HtlcVtxo, hash: PaymentHash) -> anyhow::Result<()> {
	ensure!(source.direction() == HtlcDirection::Incoming && source.htlc.payment_hash == hash,
		"joint refund original funding has different source metadata");
	let policy = source.vtxo.vtxo.policy().as_server_htlc_send().context("joint funding is not an actual send contract")?;
	ensure!(policy.payment_hash == hash && policy.htlc_expiry == source.htlc_expiry(),
		"joint incoming metadata differs from its signed source policy");
	Ok(())
}

fn check_incoming_scope(source: &HtlcVtxo, hash: PaymentHash, own_reservation: bool) -> anyhow::Result<()> {
	check_incoming_identity(source, hash)?;
	ensure!(source.htlc.offchain_resolution.is_none() && source.htlc.chain_resolution.is_none()
		&& ((source.vtxo.spend_state == SpendState::Spendable && !source.vtxo.decided_for_fallback)
			|| (own_reservation && source.vtxo.spend_state == SpendState::Spent && source.vtxo.decided_for_fallback))
		&& source.vtxo.oor_spent_txid.is_none() && source.vtxo.spent_in_round.is_none()
		&& source.vtxo.offboarded_in.is_none(), "joint incoming source already has another commitment");
	Ok(())
}

#[cfg(test)]
fn check_grant(grant: &HtlcVtxo, hash: PaymentHash, view: ExpiryViewToken) -> anyhow::Result<()> {
	check_grant_scope(grant, hash, view, false)
}

fn check_grant_scope(grant: &HtlcVtxo, hash: PaymentHash, view: ExpiryViewToken, own_reservation: bool) -> anyhow::Result<()> {
	ensure!(grant.direction() == HtlcDirection::Outgoing && grant.htlc.payment_hash == hash,
		"joint refund contains a different receive grant");
	ensure!(grant.htlc.offchain_resolution.is_none() && grant.htlc.chain_resolution.is_none(),
		"joint refund grant is already claimed or returned; an actual dependency bridge is required");
	ensure!((grant.vtxo.spend_state == SpendState::HtlcRecvUnclaimed
		|| (own_reservation && grant.vtxo.spend_state == SpendState::Spent)) && !grant.vtxo.decided_for_fallback
		&& grant.vtxo.oor_spent_txid.is_none() && grant.vtxo.spent_in_round.is_none()
		&& grant.vtxo.offboarded_in.is_none(), "joint refund grant already has another commitment");
	let confirmed = grant.vtxo.confirmed_height.context("joint refund grant is not an actual confirmed input")?;
	let maturity = confirmed.checked_add(grant.vtxo.vtxo.exit_delta()).context("joint refund CSV overflow")?
		.max(grant.htlc_expiry());
	ensure!(maturity <= view.captaind.height.min(view.watchmand.height),
		"joint refund timeout is not mature in both completed views");
	let policy = grant.vtxo.vtxo.policy().as_server_htlc_recv().context("joint refund is not a receive contract")?;
	ensure!(policy.payment_hash == hash && policy.htlc_expiry == grant.htlc_expiry(),
		"joint refund metadata differs from its actual signed policy");
	Ok(())
}

fn check_parent_inputs(
	prepared: ExpiryViewToken, current: ExpiryViewToken,
	incoming_roots: &[OutPoint], grants: &[VtxoId], parent: &Transaction,
) -> anyhow::Result<()> {
	ensure!(prepared == current, "joint refund view changed; rebuild all dependencies");
	ensure!(!incoming_roots.is_empty() && !grants.is_empty(), "joint refund omits one side of its obligation");
	let mut expected = incoming_roots.iter().copied().collect::<BTreeSet<_>>();
	ensure!(expected.len() == incoming_roots.len(), "joint refund repeats incoming backing");
	for id in grants {
		ensure!(expected.insert(id.to_point()), "joint refund repeats or overlaps a grant dependency");
	}
	let actual = parent.input.iter().map(|i| i.previous_output).collect::<BTreeSet<_>>();
	ensure!(actual.len() == parent.input.len() && actual == expected,
		"joint refund parent must spend every original grant and only its proven backing");
	Ok(())
}

impl InternalRefundCandidate {
	pub fn grant_ids(&self) -> Vec<VtxoId> { self.grants.iter().map(|g| g.vtxo.id()).collect() }

	pub(crate) fn live_incoming_ids(&self) -> Vec<VtxoId> {
		self.incoming.iter().filter_map(|(id, state)|
			matches!(state, InternalRefundSource::Live).then_some(*id)).collect()
	}

	pub(crate) fn terminal_evidence(&self) -> Vec<serde_json::Value> {
		self.incoming.values().filter_map(|state| match state {
			InternalRefundSource::Terminal(claim) => {
				let mut evidence=claim.evidence_json();
				if self.terminal_reservations.contains_key(&claim.source) {evidence["prior_joint"]=true.into();}
				if claim.claim.input.len()>1 {evidence["original_batch"]=true.into();}
				Some(evidence)
			},
			InternalRefundSource::Live => None,
		}).collect()
	}

	fn same_observation(&self, other: &Self) -> bool {
		self.attempt_id == other.attempt_id && self.subscription_id == other.subscription_id
			&& self.payment_hash == other.payment_hash && self.incoming == other.incoming
			&& self.existing == other.existing && self.terminal_reservations == other.terminal_reservations
			&& self.grant_ids() == other.grant_ids() && self.grants.iter().zip(&other.grants).all(|(a,b)|
				a.confirmed == b.confirmed && a.updated_at == b.updated_at && a.vtxo.serialize() == b.vtxo.serialize())
	}

}

impl Tx<'_> {

	async fn existing_internal_refund(
		&self, source: VtxoId, attempt: i64, subscription: i64, hash: PaymentHash,
	) -> anyhow::Result<Option<ExistingInternalRefund>> {
		let rows = self.query("SELECT e.id,e.gross_sat,c.gross_sat AS allocated_gross,c.user_pubkey,c.owner_mailbox,c.spk,c.evidence::TEXT,a.id AS allocation_id \
			FROM expiry_decided_coin d JOIN expiry_entitlement e ON e.id=d.entitlement_id \
			JOIN expiry_allocation_coin c ON c.entitlement_id=e.id JOIN expiry_allocation a ON a.id=c.allocation_id \
			WHERE d.vtxo_id=$1 AND a.status='conflicted' AND c.disposition='customer' \
			AND c.evidence->>'kind'='htlc_internal_joint_timeout' \
			AND NOT EXISTS(SELECT 1 FROM expiry_terminal_claim p WHERE p.entitlement_id=e.id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_allocation_coin other JOIN expiry_allocation payment ON payment.id=other.allocation_id \
				WHERE other.entitlement_id=e.id AND (other.paying_txid IS NOT NULL OR payment.status='confirmed' \
				OR (other.active AND payment.status<>'conflicted'))) ORDER BY a.id", &[&source.to_string()]).await?;
		let Some(first) = rows.first() else { return Ok(None); };
		let mut scope = ExistingInternalRefund { entitlement_id: first.get("id"),
			claimant: first.get::<_, &str>("user_pubkey").parse()?, mailbox: first.get::<_, &str>("owner_mailbox").parse()?,
			script: bitcoin::ScriptBuf::from_hex(first.get("spk"))?,
			gross: bitcoin::Amount::from_sat(u64::try_from(first.get::<_, i64>("gross_sat"))?), allocations: BTreeMap::new() };
		for row in rows {
			let evidence: serde_json::Value = serde_json::from_str(row.get("evidence"))?;
			check_existing_joint_evidence(&evidence, attempt, subscription, hash, None)?;
			ensure!(row.get::<_, i64>("id") == scope.entitlement_id
				&& u64::try_from(row.get::<_, i64>("allocated_gross"))? == scope.gross.to_sat()
				&& row.get::<_, &str>("user_pubkey").parse::<PublicKey>()? == scope.claimant
				&& row.get::<_, &str>("owner_mailbox").parse::<PublicKey>()? == scope.mailbox
				&& bitcoin::ScriptBuf::from_hex(row.get("spk"))? == scope.script,
				"joint replan history changes its admitted claimant or destination");
			let id: i64 = row.get("allocation_id");
			let points = self.query("SELECT vtxo_id FROM expiry_allocation_input WHERE allocation_id=$1 ORDER BY vtxo_id", &[&id]).await?
				.into_iter().map(|row| row.get::<_, &str>("vtxo_id").parse()).collect::<Result<Vec<_>,_>>()?;
			scope.allocations.insert(id, points);
		}
		Ok(Some(scope))
	}

	async fn check_internal_refund_membership(
		&self, source: VtxoId, attempt: i64, subscription: i64, hash: PaymentHash, grants: &[VtxoId],
	) -> anyhow::Result<()> {
		for row in self.query("SELECT c.evidence::TEXT FROM expiry_decided_coin d \
			JOIN expiry_allocation_coin c ON c.entitlement_id=d.entitlement_id \
			WHERE d.vtxo_id=$1 AND c.evidence->>'kind'='htlc_internal_joint_timeout'", &[&source.to_string()]).await? {
			check_existing_joint_evidence(&serde_json::from_str(row.get("evidence"))?, attempt, subscription, hash, Some(grants))?;
		}
		Ok(())
	}

	/// This is PRIVATE authority for the exact joint commit, never an exception
	/// exposed to refresh/offboard/materialization or normal watchman writers.
	async fn guard_internal_refund_replan(&self, component: &InternalRefundComponent, ids: &[VtxoId]) -> anyhow::Result<()> {
		let all = ids.iter().map(ToString::to_string).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
		let sources = self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE", &[&all]).await?;
		ensure!(sources.len() == all.len(), "joint refund lost an original source or partition row");
		let terminal = component.terminal_ids();
		// These exact original rows are observed, not changed or allocated.
		// Fresh classification below still runs under their locks, WAL2 and
		// the subscription locks. No other writer learns this exclusion.
		let mutable = ids.iter().copied().filter(|id| !terminal.contains(id)).collect::<Vec<_>>();
		let allowed = component.superseded_allocations();
		if allowed.is_empty() { return super::expiry_guard::guard_obligation_changes(self, &mutable).await; }
		let ids = mutable.iter().map(ToString::to_string).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
		let allocations = self.query("SELECT id,status FROM expiry_allocation WHERE id=ANY($1) ORDER BY id FOR UPDATE", &[&allowed]).await?;
		ensure!(allocations.len() == allowed.len() && allocations.iter().all(|row| row.get::<_, &str>("status") == "conflicted"),
			"joint replan previous reservation is still active or paid");
		ensure!(self.query_opt("SELECT d.vtxo_id FROM expiry_decided_coin d WHERE d.vtxo_id=ANY($1) \
			AND NOT EXISTS(SELECT 1 FROM expiry_allocation_coin c WHERE c.entitlement_id=d.entitlement_id \
				AND c.allocation_id=ANY($2)) LIMIT 1", &[&ids,&allowed]).await?.is_none(),
			"joint replan would cross another economic fence");
		ensure!(self.query_opt("SELECT vtxo_id FROM expiry_allocation_input WHERE vtxo_id=ANY($1) \
			AND allocation_id<>ALL($2) LIMIT 1", &[&ids,&allowed]).await?.is_none(),
			"joint replan would cross another physical reservation");
		Ok(())
	}

	/// Acquire the complete physical/source union before the existing WAL2
	/// barrier and every subscription, never one member at a time. Discovery
	/// and geometric preparation remain outside this short transaction.
	pub(crate) async fn lock_unclaimed_internal_refund_component(
		&self, proposed: &InternalRefundComponent, current: ExpiryViewToken, incoming: &[ReadyOutput],
	) -> anyhow::Result<InternalRefundComponent> {
		ensure!(current == proposed.view, "internal component generation changed");
		self.lock_valid_expiry_view(current).await?;
		let ids = proposed.obligation_lock_ids().into_iter().map(|id|id.parse()).collect::<Result<Vec<VtxoId>,_>>()?
			.into_iter().chain(incoming.iter().flat_map(ReadyOutput::source_ids)).collect::<Vec<_>>();
		self.guard_internal_refund_replan(proposed, &ids).await?;
		self.lock_htlc_settlements().await?;
		let mut subscriptions = proposed.subscription_ids();
		subscriptions.sort_unstable();
		let rows = self.query("SELECT id FROM lightning_htlc_subscription WHERE id=ANY($1) ORDER BY id FOR UPDATE",
			&[&subscriptions]).await?;
		ensure!(rows.len() == subscriptions.len(), "internal component subscription disappeared");
		let mut actual = Vec::with_capacity(proposed.members.len());
		for member in &proposed.members {
			let fresh = self.unclaimed_internal_refund(member.attempt_id, current).await?
				.context("one internal component obligation changed; keep the entire component pending")?;
			ensure!(member.same_observation(&fresh), "internal component source membership or observation changed");
			actual.push(fresh);
		}
		InternalRefundComponent::new(current, actual)
	}

	/// Immutable identity plus the existing uncollected source-specific exclusions.
	/// This does not require grants to remain unspent: rebuild must validate an
	/// already-committed parent's premises after it consumes B and its grants.
	async fn internal_original_claim_scope(&self, attempt_id: i64) -> anyhow::Result<Option<InternalOriginalClaimScope>> {
		let Some(row) = self.query_opt("SELECT f.subscription_id,a.payment_hash FROM expiry_lightning_internal_funding f \
			JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
			JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash \
			WHERE f.id=$1 AND s.status IN ('htlcs-ready','canceled') \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE subscription_id=s.id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_replay WHERE subscription_id=s.id) \
			AND NOT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=a.payment_hash) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=a.id) \
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original \
				JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id \
				JOIN expiry_lightning_dispatch d ON d.id=other.lightning_payment_attempt_id \
				WHERE original.lightning_payment_attempt_id=a.id) \
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original \
				JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id \
				JOIN expiry_lightning_queued_attempt q ON q.id=other.lightning_payment_attempt_id \
				WHERE original.lightning_payment_attempt_id=a.id AND q.id<>a.id) \
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original \
				JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id \
				JOIN expiry_lightning_external_success paid ON paid.id=other.lightning_payment_attempt_id \
				WHERE original.lightning_payment_attempt_id=a.id)", &[&attempt_id]).await? else { return Ok(None); };
		let subscription_id = row.get::<_, i64>("subscription_id");
		let hash: PaymentHash = row.get::<_, &str>("payment_hash").parse()?;
		let incoming = self.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
			WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&attempt_id]).await?
			.into_iter().map(|row| row.get::<_, &str>("vtxo_id").parse()).collect::<Result<Vec<VtxoId>,_>>()?;
		ensure!(!incoming.is_empty() && incoming.len() as i64 <= MAX_JOINT_GRANTS,
			"joint refund original funding set exceeds its preparation budget");
		Ok(Some(InternalOriginalClaimScope { attempt: attempt_id, subscription: subscription_id, hash, originals: incoming }))
	}

	pub(crate) async fn rebuilding_internal_original_batch_claim(
		&self, source: VtxoId, customer: VtxoId, view: ExpiryViewToken, evidence: &serde_json::Value,
	) -> anyhow::Result<Option<ReplayableTerminalClaim>> {
		let attempt=evidence.get("attempt_id").and_then(serde_json::Value::as_i64)
			.context("batch premise has no actual attempt identity")?;
		let Some(scope)=self.internal_original_claim_scope(attempt).await? else { return Ok(None); };
		let grants=self.query("SELECT vtxo_id FROM vtxo WHERE lightning_htlc_subscription_id=$1 ORDER BY vtxo_id LIMIT $2",
			&[&scope.subscription,&(MAX_JOINT_GRANTS+1)]).await?.into_iter()
			.map(|row|row.get::<_,&str>("vtxo_id").parse()).collect::<Result<Vec<VtxoId>,_>>()?;
		ensure!(grants.len() as i64<=MAX_JOINT_GRANTS,"batch premise exceeds the complete grant budget");
		scope.check_evidence(evidence,&grants)?;
		let Some(proof)=self.replayable_internal_original_batch_claim(source,view,&scope,true).await? else {return Ok(None);};
		ensure!(scope.originals.contains(&customer)
			&& proof.claim.input.iter().all(|input|VtxoId::from(input.previous_output)!=customer),
			"batch premise does not belong to its distinct actual original customer");
		let premises=evidence.get("terminal_sources").and_then(serde_json::Value::as_array)
			.context("batch premise lacks its original terminal observations")?;
		let txid=proof.claim.compute_txid().to_string();
		let mut linked=BTreeSet::new();
		for premise in premises.iter().filter(|value|value.get("claim_txid").and_then(serde_json::Value::as_str)==Some(txid.as_str())) {
			ensure!(premise.get("original_batch").and_then(serde_json::Value::as_bool)==Some(true)
				&& premise.get("prior_joint").and_then(serde_json::Value::as_bool)!=Some(true),
				"batch premise cannot widen a reserved single-original scope");
			let id=premise.get("source").and_then(serde_json::Value::as_str).context("batch premise source is missing")?.parse::<VtxoId>()?;
			ensure!(linked.insert(id),"batch premise repeats an original");
		}
		ensure!(linked==proof.claim.input.iter().map(|input|VtxoId::from(input.previous_output)).collect(),
			"batch premise omits or adds a raw transaction input");
		Ok(Some(proof))
	}

	/// An unmaterialized receive retains its entire original source membership.
	/// Live originals are refunded; exact replayable canonical user claims are
	/// observation-only. Every receive grant must still be an actual input.
	/// Historical returns to a cold drain cannot authorize a cross-root refund
	/// without a real spendable bridge; observed depth alone is insufficient.
	pub(crate) async fn unclaimed_internal_refund(
		&self, attempt_id: i64, view: ExpiryViewToken,
	) -> anyhow::Result<Option<InternalRefundCandidate>> {
		let Some(original_scope) = self.internal_original_claim_scope(attempt_id).await? else { return Ok(None); };
		let subscription_id = original_scope.subscription;
		let hash = original_scope.hash;
		let incoming = original_scope.originals.clone();
		let mut existing = BTreeMap::new();
		let mut terminal_reservations = BTreeMap::new();
		let mut classified = BTreeMap::new();
		for id in &incoming {
			let source = htlc_vtxo::get_htlc_vtxo(self, *id).await?.context("joint incoming source lost its signed body")?;
			check_incoming_identity(&source, hash)?;
			let physical_fence=self.query_opt("SELECT vtxo_id FROM expiry_allocation_input WHERE vtxo_id=$1 LIMIT 1",&[&id.to_string()]).await?.is_some();
			let reserved=source.vtxo.decided_for_fallback || physical_fence;
			let terminal=if reserved {self.replayable_internal_partial_terminal_claim(*id,view).await?}
				else if let Some(single)=self.replayable_terminal_claim(*id,view).await? {Some(single)}
				else {self.replayable_internal_original_batch_claim(*id,view,&original_scope,false).await?};
			if let Some(claim) = terminal {
				if reserved {
					terminal_reservations.insert(*id,self.partial_internal_user_claim_scope(*id).await?
						.context("terminal original lost its exact prior joint scope")?);
				}
				// This is the actual original user's signed leaf spend in the
				// current canonical view. Retain its source membership while
				// keeping it out of the new payout and physical input set.
				ensure!(source.htlc.offchain_resolution.is_none()
					&& source.vtxo.oor_spent_txid.is_none() && source.vtxo.spent_in_round.is_none()
					&& source.vtxo.offboarded_in.is_none(), "terminal original has another off-chain commitment");
				classified.insert(*id, InternalRefundSource::Terminal(claim));
				continue;
			}
			if source.vtxo.decided_for_fallback {
				let Some(scope) = self.existing_internal_refund(*id, attempt_id, subscription_id, hash).await? else { return Ok(None); };
				ensure!(scope.gross == source.vtxo.vtxo.amount()
					&& scope.claimant == source.vtxo.vtxo.policy().as_server_htlc_send()
						.context("stored joint source policy changed")?.user_pubkey,
					"joint replan changes its original amount or claimant");
				existing.insert(*id, scope);
			}
			check_incoming_scope(&source, hash, existing.contains_key(id))?;
			ensure!(self.query_one(
				"SELECT onchain_spent_height IS NULL AND onchain_spent_txid IS NULL AS unspent FROM vtxo WHERE vtxo_id=$1",
				&[&id.to_string()]).await?.get::<_, bool>("unspent"), "joint incoming source has a terminal chain spend");
			classified.insert(*id, InternalRefundSource::Live);
		}
		if !classified.values().any(|state| matches!(state, InternalRefundSource::Live)) { return Ok(None); }
		let rows = self.query("SELECT vtxo_id,onchain_spent_height,onchain_spent_txid FROM vtxo \
			WHERE lightning_htlc_subscription_id=$1 ORDER BY vtxo_id LIMIT $2", &[&subscription_id,&(MAX_JOINT_GRANTS+1)]).await?;
		ensure!(!rows.is_empty() && rows.len() as i64 <= MAX_JOINT_GRANTS,
			"joint refund must include a bounded nonempty whole receive grant set");
		let mut grants = Vec::with_capacity(rows.len());
		for row in rows {
			ensure!(row.get::<_, Option<i32>>("onchain_spent_height").is_none()
				&& row.get::<_, Option<&str>>("onchain_spent_txid").is_none(),
				"joint refund grant is already spent; cannot replace its dependency with an observation");
			let id: VtxoId = row.get::<_, &str>("vtxo_id").parse()?;
			let grant = htlc_vtxo::get_htlc_vtxo(self, id).await?.context("joint receive grant has no actual HTLC")?;
			let own_reservation = !existing.is_empty() && self.query_opt(
				"SELECT i.vtxo_id FROM expiry_allocation_input i WHERE i.vtxo_id=$1 AND i.allocation_id=ANY($2) LIMIT 1",
				&[&id.to_string(), &existing.values().flat_map(|scope| scope.allocations.keys().copied()).collect::<Vec<_>>()]).await?.is_some();
			check_grant_scope(&grant, hash, view, own_reservation)?;
			grants.push(InternalRefundGrant { confirmed: grant.vtxo.confirmed_height.expect("checked above"),
				updated_at: grant.vtxo.updated_at, vtxo: ServerVtxo::from(grant.vtxo.vtxo).to_bare() });
		}
		let grant_ids = grants.iter().map(|grant| grant.vtxo.id()).collect::<Vec<_>>();
		for source in existing.keys() {
			self.check_internal_refund_membership(*source, attempt_id, subscription_id, hash, &grant_ids).await?;
		}
		Ok(Some(InternalRefundCandidate { attempt_id, subscription_id, payment_hash: hash, view,
			incoming: classified, existing, terminal_reservations, grants }))
	}

}

/// Private permission for existing user authorizations and operator timeout
/// returns after an entire retained joint allocation became conflicted. This
/// describes immutable obligations, not present-day chain payment evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConflictedInternalUserClaimScope {
	pub(crate) source: VtxoId,
	pub(crate) incoming: Vec<VtxoId>,
	pub(crate) grants: Vec<VtxoId>,
	physical: Vec<VtxoId>,
	allow_dependent: bool,
	allocations: Vec<i64>,
	subscriptions: Vec<i64>,
	identity: Vec<serde_json::Value>,
}

/// A typed internal premise binds the omitted source to an existing retained
/// body. It is not a claim of current canonical confirmation.
fn dependent_terminal_txid(evidence:&serde_json::Value,source:VtxoId) -> anyhow::Result<Option<bitcoin::Txid>> {
	let Some(terminals)=evidence.get("terminal_sources") else {return Ok(None);};
	let terminals=terminals.as_array().context("invalid internal terminal premises")?;
	let matches=terminals.iter().filter(|value|value.get("prior_joint").and_then(serde_json::Value::as_bool)==Some(true)
		&& value.get("source").and_then(serde_json::Value::as_str)==Some(source.to_string().as_str())).collect::<Vec<_>>();
	let Some(premise)=matches.first() else {return Ok(None);};
	ensure!(matches.len()==1,"internal parent repeats its omitted original");
	let originals=evidence.get("original_sources").and_then(serde_json::Value::as_array)
		.context("dependent parent lacks complete original membership")?;
	let ids=originals.iter().map(|value|value.as_str().context("invalid original source")?.parse::<VtxoId>().map_err(Into::into))
		.collect::<anyhow::Result<BTreeSet<_>>>()?;
	ensure!(ids.len()==originals.len() && ids.contains(&source),"dependent parent rebinds its omitted source");
	Ok(Some(premise.get("claim_txid").and_then(serde_json::Value::as_str).context("missing omitted user body identity")?.parse()?))
}

fn check_dependent_parent_inputs(
	omitted:VtxoId,parent:&Transaction,grants:&BTreeSet<VtxoId>,paths:&BTreeMap<VtxoId,BTreeSet<OutPoint>>,
) -> anyhow::Result<()> {
	check_dependent_input_points(omitted,&parent.input.iter().map(|input|input.previous_output).collect::<Vec<_>>(),grants,paths)
}

fn check_dependent_input_points(
	omitted:VtxoId,points:&[OutPoint],grants:&BTreeSet<VtxoId>,paths:&BTreeMap<VtxoId,BTreeSet<OutPoint>>,
) -> anyhow::Result<()> {
	ensure!(!paths.is_empty() && !paths.contains_key(&omitted) && !grants.is_empty(),"dependent parent has no distinct live source/grant scope");
	let inputs=points.iter().copied().collect::<BTreeSet<_>>();
	ensure!(inputs.len()==points.len() && !inputs.contains(&omitted.to_point())
		&& grants.iter().all(|id|inputs.contains(&id.to_point())),"dependent parent drops a grant or allocates its omitted original");
	let physical=inputs.iter().filter(|point|!grants.contains(&VtxoId::from(**point))).collect::<Vec<_>>();
	ensure!(!physical.is_empty() && physical.iter().all(|point|paths.values().any(|path|path.contains(*point)))
		&& paths.values().all(|path|physical.iter().any(|point|path.contains(*point))),
		"dependent parent is not backed by each actual live original's signed path");
	Ok(())
}

/// An unpublished financial intent supplies membership/fences only. It is not
/// a Bitcoin parent, current payment evidence or permission to omit A's proof.
/// Historical never-signed conflicted intents retain those same reservations.
fn check_unsigned_dependent_intent(
	status:&str,txid:Option<&str>,broadcast:bool,confirmed:bool,fee_child:bool,paid_coin:bool,
) -> anyhow::Result<()> {
	ensure!(matches!(status,"pending"|"conflicted") && txid.is_none() && !broadcast
		&& !confirmed && !fee_child && !paid_coin,
		"unsigned dependent intent has a published, paid or incompatible financial identity");
	Ok(())
}

fn check_terminal_return_parent(inputs:&[VtxoId],parent:&Transaction) -> anyhow::Result<()> {
	let actual=parent.input.iter().map(|input|VtxoId::from(input.previous_output)).collect::<BTreeSet<_>>();
	ensure!(parent.version.0==3 && actual.len()==parent.input.len() && actual.len()==inputs.len()
		&& actual==inputs.iter().copied().collect(),"retained joint parent differs from its physical inputs");
	Ok(())
}

impl ConflictedInternalUserClaimScope {
	fn lock_ids(&self) -> Vec<String> {
		self.incoming.iter().chain(&self.grants).chain(&self.physical)
			.map(ToString::to_string).collect::<BTreeSet<_>>().into_iter().collect()
	}
}

/// A changed or unsupported scope is absence of private authorization, not a
/// database failure. Rebuild may invalidate that one dependent parent without
/// suppressing transport/storage/decode failures or pinning unrelated work.
#[derive(Debug)]
struct InternalUserClaimScopeIneligible(&'static str);
impl std::fmt::Display for InternalUserClaimScopeIneligible {
	fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(self.0)}
}
impl std::error::Error for InternalUserClaimScopeIneligible {}

macro_rules! scope_eligible {
	($condition:expr,$reason:literal) => {
		if !($condition) {return Err(InternalUserClaimScopeIneligible($reason).into());}
	};
}

impl Tx<'_> {
	/// Do not require current canonical claims here: replay needs this exact
	/// immutable scope after those observations disappeared. The final return
	/// guard separately requires all original claims in the completed view.
	pub(crate) async fn conflicted_internal_user_claim_scope(
		&self, source: VtxoId,
	) -> anyhow::Result<Option<ConflictedInternalUserClaimScope>> {
		self.internal_user_claim_scope(source,false).await
	}

	/// Explicit partial/replay permission. A new allocation may be live only
	/// when its own immutable parent depends on THIS omitted original's exact
	/// retained authorization and preserves the complete source/grant scope.
	pub(crate) async fn partial_internal_user_claim_scope(
		&self, source:VtxoId,
	) -> anyhow::Result<Option<ConflictedInternalUserClaimScope>> {
		self.internal_user_claim_scope(source,true).await
	}

	async fn internal_user_claim_scope(
		&self, source:VtxoId, allow_dependent:bool,
	) -> anyhow::Result<Option<ConflictedInternalUserClaimScope>> {
		match self.check_internal_user_claim_scope(source,allow_dependent).await {
			Err(error) if error.is::<InternalUserClaimScopeIneligible>() => Ok(None),
			result => result,
		}
	}

	async fn check_internal_user_claim_scope(
		&self, source:VtxoId, allow_dependent:bool,
	) -> anyhow::Result<Option<ConflictedInternalUserClaimScope>> {
		let first = self.query("SELECT f.id FROM expiry_lightning_internal_funding f \
			JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=f.id \
			WHERE l.vtxo_id=$1 ORDER BY f.id", &[&source.to_string()]).await?;
		if first.len()!=1 { return Ok(None); }
		let mut pending = BTreeSet::from([first[0].get::<_,i64>("id")]);
		let mut members = BTreeMap::<i64,(i64,PaymentHash,Vec<VtxoId>,Vec<VtxoId>)>::new();
		let mut allocations = BTreeSet::new();
		let mut identity = Vec::new();
		while let Some(attempt)=pending.pop_first() {
			if members.contains_key(&attempt) { continue; }
			scope_eligible!(members.len()<MAX_JOINT_GRANTS as usize,"terminal joint scope exceeds component budget");
			let Some(row)=self.query_opt("SELECT f.subscription_id,a.payment_hash FROM expiry_lightning_internal_funding f \
				JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
				JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash \
				WHERE f.id=$1 AND s.status IN ('htlcs-ready','canceled') \
				AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE subscription_id=s.id) \
				AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_replay WHERE subscription_id=s.id) \
				AND NOT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=a.payment_hash) \
				AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original \
					JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id \
					WHERE original.lightning_payment_attempt_id=a.id AND ( \
						EXISTS(SELECT 1 FROM expiry_lightning_dispatch d WHERE d.id=other.lightning_payment_attempt_id) \
						OR EXISTS(SELECT 1 FROM expiry_lightning_external_success paid WHERE paid.id=other.lightning_payment_attempt_id) \
						OR EXISTS(SELECT 1 FROM expiry_lightning_queued_attempt q WHERE q.id=other.lightning_payment_attempt_id AND q.id<>a.id)))",
				&[&attempt]).await? else { return Ok(None); };
			let subscription: i64=row.get("subscription_id");
			let hash: PaymentHash=row.get::<_,&str>("payment_hash").parse()?;
			let originals=self.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
				WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id",&[&attempt]).await?
				.into_iter().map(|row|row.get::<_,&str>("vtxo_id").parse()).collect::<Result<Vec<VtxoId>,_>>()?;
			let grants=self.query("SELECT vtxo_id FROM vtxo WHERE lightning_htlc_subscription_id=$1 ORDER BY vtxo_id",&[&subscription]).await?
				.into_iter().map(|row|row.get::<_,&str>("vtxo_id").parse()).collect::<Result<Vec<VtxoId>,_>>()?;
			scope_eligible!(!originals.is_empty() && !grants.is_empty()
				&& originals.len()<=MAX_JOINT_GRANTS as usize && grants.len()<=MAX_JOINT_GRANTS as usize,
				"terminal joint scope lost its complete original or grant membership");
			for id in &originals {
				let original=htlc_vtxo::get_htlc_vtxo(self,*id).await?.context("terminal joint source disappeared")?;
				check_incoming_identity(&original,hash)?;
				scope_eligible!(original.htlc.offchain_resolution.is_none() && original.vtxo.oor_spent_txid.is_none()
					&& original.vtxo.spent_in_round.is_none() && original.vtxo.offboarded_in.is_none(),
					"terminal joint source has an incompatible off-chain commitment");
				identity.push(serde_json::json!({"source":id.to_string(),"body":original.vtxo.vtxo.serialize()}));
			}
			for id in &grants {
				let grant=htlc_vtxo::get_htlc_vtxo(self,*id).await?.context("terminal joint grant disappeared")?;
				let policy=grant.vtxo.vtxo.policy().as_server_htlc_recv().context("terminal return is not a receive grant")?;
				scope_eligible!(grant.direction()==HtlcDirection::Outgoing && grant.htlc.payment_hash==hash
					&& policy.payment_hash==hash && policy.htlc_expiry==grant.htlc_expiry()
					&& grant.htlc.offchain_resolution.is_none() && grant.vtxo.oor_spent_txid.is_none()
					&& grant.vtxo.spent_in_round.is_none() && grant.vtxo.offboarded_in.is_none(),
					"terminal return grant has a different obligation");
				identity.push(serde_json::json!({"grant":id.to_string(),"body":grant.vtxo.vtxo.serialize()}));
			}
			let ids=originals.iter().chain(&grants).map(ToString::to_string).collect::<Vec<_>>();
			let rows=self.query("SELECT DISTINCT a.id,a.status FROM expiry_allocation a WHERE \
				EXISTS(SELECT 1 FROM expiry_allocation_input i WHERE i.allocation_id=a.id AND i.vtxo_id=ANY($1)) \
				OR EXISTS(SELECT 1 FROM expiry_allocation_coin c JOIN expiry_decided_coin d ON d.entitlement_id=c.entitlement_id \
					WHERE c.allocation_id=a.id AND d.vtxo_id=ANY($1)) ORDER BY a.id",&[&ids]).await?;
			scope_eligible!(!rows.is_empty(),"terminal joint scope has no retained reservation");
			for row in rows {
				scope_eligible!(allow_dependent || row.get::<_,&str>("status")=="conflicted","terminal joint reservation is still pending or paid");
				let allocation:i64=row.get("id");
				if !allocations.insert(allocation) {continue;}
				for coin in self.query("SELECT evidence::TEXT FROM expiry_allocation_coin WHERE allocation_id=$1 ORDER BY entitlement_id",&[&allocation]).await? {
					let evidence:serde_json::Value=serde_json::from_str(coin.get("evidence"))?;
					scope_eligible!(evidence["kind"]=="htlc_internal_joint_timeout","terminal return crosses an unrelated allocation");
					pending.insert(evidence["attempt_id"].as_i64().context("joint reservation has no attempt identity")?);
				}
			}
			members.insert(attempt,(subscription,hash,originals,grants));
		}
		let incoming=members.values().flat_map(|(_,_,ids,_)|ids.iter().copied()).collect::<BTreeSet<_>>();
		let grants=members.values().flat_map(|(_,_,_,ids)|ids.iter().copied()).collect::<BTreeSet<_>>();
		scope_eligible!(incoming.len()==members.values().map(|(_,_,ids,_)|ids.len()).sum::<usize>()
			&& grants.len()==members.values().map(|(_,_,_,ids)|ids.len()).sum::<usize>()
			&& incoming.contains(&source) && incoming.is_disjoint(&grants)
			&& incoming.len()<=MAX_JOINT_GRANTS as usize && grants.len()<=MAX_JOINT_GRANTS as usize,
			"terminal joint component overlaps or exceeds its original input budget");
		let mut physical=BTreeSet::new();
		let allocations=allocations.into_iter().collect::<Vec<_>>();
		let mut covered=BTreeSet::new();
		let mut original_conflict=false;
		for allocation in &allocations {
			let row=self.query_one("SELECT status,sweep_tx,sweep_txid,broadcast_at IS NOT NULL AS broadcast,confirmed_height,confirmed_hash FROM expiry_allocation WHERE id=$1",&[allocation]).await?;
			let status=row.get::<_,&str>("status");
			scope_eligible!(allow_dependent || (status=="conflicted" && row.get::<_,Option<i64>>("confirmed_height").is_none()),
				"terminal joint parent has a live canonical payment");
			let inputs=self.query("SELECT vtxo_id FROM expiry_allocation_input WHERE allocation_id=$1 ORDER BY vtxo_id",&[allocation]).await?
				.into_iter().map(|row|row.get::<_,&str>("vtxo_id").parse()).collect::<Result<Vec<VtxoId>,_>>()?;
			scope_eligible!(!inputs.is_empty(),"terminal joint parent lost its physical reservation");
			let raw=row.get::<_,Option<&[u8]>>("sweep_tx");
			if let Some(raw)=raw { check_terminal_return_parent(&inputs,&bitcoin::consensus::deserialize(raw)?)?; }
			identity.push(serde_json::json!({"allocation":allocation,"parent":raw,"inputs":inputs.iter().map(ToString::to_string).collect::<Vec<_>>()}));
			let allocation_inputs=inputs.iter().copied().collect::<BTreeSet<_>>();
			physical.extend(inputs);
			let coins=self.query("SELECT d.vtxo_id,e.id,e.gross_sat,c.gross_sat AS allocated_gross,c.disposition, \
				c.user_pubkey,c.owner_mailbox,c.spk,c.evidence::TEXT,c.paying_txid \
				FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				JOIN expiry_decided_coin d ON d.entitlement_id=e.id WHERE c.allocation_id=$1 ORDER BY d.vtxo_id",&[allocation]).await?;
			let coin_count=self.query_one("SELECT COUNT(*) FROM expiry_allocation_coin WHERE allocation_id=$1",&[allocation]).await?.get::<_,i64>(0);
			scope_eligible!(usize::try_from(coin_count)?==coins.len() && !coins.is_empty(),"terminal joint allocation has an unbound entitlement");
			let mut dependent=false;
			if allow_dependent {
				for coin in &coins {
					let evidence:serde_json::Value=serde_json::from_str(coin.get("evidence"))?;
					if let Some(txid)=dependent_terminal_txid(&evidence,source)? {
						let journal=self.query_opt("SELECT t.raw_tx FROM expiry_user_claim_source s JOIN expiry_user_claim_tx t ON t.txid=s.claim_txid \
							WHERE s.source_vtxo_id=$1 AND s.claim_txid=$2",&[&source.to_string(),&txid.to_string()]).await?
							.context("dependent joint parent lost its original user authorization")?;
						let claim:Transaction=bitcoin::consensus::deserialize(journal.get("raw_tx"))?;
						let original=htlc_vtxo::get_htlc_vtxo(self,source).await?.context("dependent original disappeared")?;
						let original=ServerVtxo::from(original.vtxo.vtxo).to_bare();
						scope_eligible!(claim.compute_txid()==txid && claim.input.len()==1
							&& super::expiry_user_claim::user_timeout_input(&original,&claim),
							"dependent parent lacks the exact original DEFAULT timeout authorization");
						dependent=true;
					}
				}
			}
			scope_eligible!(dependent || (status=="conflicted" && row.get::<_,Option<i64>>("confirmed_height").is_none()),
				"terminal original has an unrelated live or paid customer parent");
			if dependent {
				scope_eligible!(coins.iter().all(|coin|coin.get::<_,&str>("vtxo_id")!=source.to_string()),
					"dependent parent also allocates the omitted original");
				// A committed intent precedes parent signing in the normal payout
				// pipeline. Rebuild must validate that exact unpublished intent,
				// rather than require a parent that itself needs a rebuilt view.
				if raw.is_none() {
					let fee_child=self.query_opt("SELECT id FROM expiry_fee_child WHERE allocation_id=$1 LIMIT 1",&[allocation]).await?.is_some();
					check_unsigned_dependent_intent(status,row.get("sweep_txid"),row.get("broadcast"),
						row.get::<_,Option<i64>>("confirmed_height").is_some() || row.get::<_,Option<&str>>("confirmed_hash").is_some(),
						fee_child,coins.iter().any(|coin|coin.get::<_,Option<&str>>("paying_txid").is_some()))
						.map_err(|_|InternalUserClaimScopeIneligible("dependent unsigned intent is already published or paid"))?;
				}
				let mut paths=BTreeMap::new();
				let mut required=BTreeSet::new();
				for coin in &coins {
					let id:VtxoId=coin.get::<_,&str>("vtxo_id").parse()?;
					let (_,_,_,gs)=members.values().find(|(_,_,ids,_)|ids.contains(&id)).context("dependent parent changes its funding sources")?;
					required.extend(gs.iter().copied());
					let state=htlc_vtxo::get_htlc_vtxo(self,id).await?.context("dependent source lost its full signed path")?;
					let path=std::iter::once(state.vtxo.vtxo.chain_anchor()).chain(state.vtxo.vtxo.transactions()
						.map(|step|OutPoint::new(step.tx.compute_txid(),step.output_idx as u32)))
						.filter(|point|*point!=state.vtxo.vtxo.point()).collect::<BTreeSet<_>>();
					paths.insert(id,path);
				}
				if let Some(raw)=raw {
					let parent:Transaction=bitcoin::consensus::deserialize(raw)?;
					check_dependent_parent_inputs(source,&parent,&required,&paths)?;
				} else {
					check_dependent_input_points(source,&allocation_inputs.iter().map(|id|id.to_point()).collect::<Vec<_>>(),&required,&paths)?;
				}
			}
			for coin in coins {
				let id:VtxoId=coin.get::<_,&str>("vtxo_id").parse()?;
				let (attempt,(subscription,hash,originals,required))=members.iter().find(|(_,(_,_,ids,_))|ids.contains(&id))
					.context("terminal joint reservation contains an unrelated source")?;
				let evidence:serde_json::Value=serde_json::from_str(coin.get("evidence"))?;
				check_existing_joint_evidence(&evidence,*attempt,*subscription,*hash,Some(required))?;
				let original=htlc_vtxo::get_htlc_vtxo(self,id).await?.context("terminal joint claimant disappeared")?;
				let claimant=original.vtxo.vtxo.policy().as_server_htlc_send().context("terminal joint claimant policy changed")?.user_pubkey;
				scope_eligible!(coin.get::<_,&str>("disposition")=="customer"
					&& u64::try_from(coin.get::<_,i64>("gross_sat"))?==original.amount().to_sat()
					&& coin.get::<_,i64>("allocated_gross")==coin.get::<_,i64>("gross_sat")
					&& coin.get::<_,&str>("user_pubkey").parse::<PublicKey>()?==claimant,
					"terminal joint reservation changes its original principal or claimant");
				if id==source && !dependent {original_conflict=true;}
				if let Some(paid)=coin.get::<_,Option<&str>>("paying_txid") {
					let own_dependent=dependent && id!=source && raw.map(bitcoin::consensus::deserialize::<Transaction>)
						.transpose()?.is_some_and(|parent|parent.compute_txid().to_string()==paid);
					scope_eligible!(own_dependent ||self.query_opt("SELECT s.source_vtxo_id FROM expiry_user_claim_source s \
						WHERE s.source_vtxo_id=$1 AND s.claim_txid=$2",&[&id.to_string(),&paid]).await?.is_some(),
						"terminal joint entitlement has another paying transaction");
				}
				scope_eligible!(required.iter().all(|id|allocation_inputs.contains(id)),
					"terminal joint parent omitted its own complete grant set");
				if let Some(original_ids)=evidence.get("original_sources") {
					let ids=original_ids.as_array().context("invalid original joint source set")?.iter()
						.map(|value|value.as_str().context("invalid original joint source")?.parse::<VtxoId>().map_err(Into::into))
						.collect::<anyhow::Result<BTreeSet<_>>>()?;
					scope_eligible!(ids==originals.iter().copied().collect(),"terminal joint source membership changed");
				}
				covered.insert(id);
				identity.push(serde_json::json!({"allocation":allocation,"source":id.to_string(),"entitlement":coin.get::<_,i64>("id"),
					"gross":coin.get::<_,i64>("gross_sat"),"user_pubkey":coin.get::<_,&str>("user_pubkey"),
					"mailbox":coin.get::<_,&str>("owner_mailbox"),"script":coin.get::<_,&str>("spk"),"evidence":evidence}));
			}
		}
		scope_eligible!(covered==incoming && original_conflict,"terminal joint scope does not retain its original conflicted economic claim");
		let all=incoming.iter().chain(&grants).chain(&physical).map(ToString::to_string).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
		scope_eligible!(self.query_opt("SELECT vtxo_id FROM expiry_allocation_input WHERE vtxo_id=ANY($1) AND allocation_id<>ALL($2) LIMIT 1",&[&all,&allocations]).await?.is_none(),
			"terminal joint scope crosses another physical reservation");
		scope_eligible!(self.query_opt("SELECT d.vtxo_id FROM expiry_decided_coin d WHERE d.vtxo_id=ANY($1) \
			AND NOT EXISTS(SELECT 1 FROM expiry_allocation_coin c WHERE c.entitlement_id=d.entitlement_id AND c.allocation_id=ANY($2)) LIMIT 1",
			&[&all,&allocations]).await?.is_none(),"terminal joint scope crosses another economic fence");
		let mut subscriptions=members.values().map(|(id,_,_,_)|*id).collect::<Vec<_>>();
		subscriptions.sort_unstable(); subscriptions.dedup();
		Ok(Some(ConflictedInternalUserClaimScope {source,incoming:incoming.into_iter().collect(),grants:grants.into_iter().collect(),
			physical:physical.into_iter().collect(),allow_dependent,allocations,subscriptions,identity}))
	}

	/// Caller holds the completed view. No source-only lock may precede this
	/// complete sorted union. Every writer still observes the retained fences.
	pub(crate) async fn lock_conflicted_internal_user_claim_scope(
		&self, proposed:&ConflictedInternalUserClaimScope,
	) -> anyhow::Result<()> {
		let ids=proposed.lock_ids();
		let rows=self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE",&[&ids]).await?;
		ensure!(rows.len()==ids.len(),"terminal joint scope lost a retained physical/source row");
		self.query("SELECT id FROM expiry_allocation WHERE id=ANY($1) ORDER BY id FOR UPDATE",&[&proposed.allocations]).await?;
		self.lock_htlc_settlements().await?;
		self.query("SELECT id FROM lightning_htlc_subscription WHERE id=ANY($1) ORDER BY id FOR UPDATE",&[&proposed.subscriptions]).await?;
		let actual=self.internal_user_claim_scope(proposed.source,proposed.allow_dependent).await?.context("terminal joint scope became ineligible")?;
		ensure!(&actual==proposed,"terminal joint membership or retained reservations changed");
		Ok(())
	}

	/// CLAIM-only alternative to the normal guard. It never authorizes progress,
	/// customer payout or an unrelated mutation of an already reserved output.
	pub(crate) async fn lock_terminal_internal_grant_returns(
		&self, view:ExpiryViewToken, ids:&[VtxoId],
	) -> anyhow::Result<()> {
		self.lock_valid_expiry_view(view).await?;
		let names=ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let fenced=self.query("SELECT DISTINCT i.vtxo_id,f.id FROM expiry_allocation_input i \
			JOIN vtxo g ON g.vtxo_id=i.vtxo_id JOIN expiry_lightning_internal_funding f ON f.subscription_id=g.lightning_htlc_subscription_id \
			WHERE i.vtxo_id=ANY($1) ORDER BY i.vtxo_id,f.id",&[&names]).await?;
		if fenced.is_empty() {return self.lock_unallocated_watchman_inputs(ids).await;}
		let mut scopes=BTreeMap::new();
		for row in fenced {
			let attempt:i64=row.get("id");
			let original=self.query_one("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
				WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id LIMIT 1",&[&attempt]).await?.get::<_,String>(0).parse()?;
			let scope=self.conflicted_internal_user_claim_scope(original).await?.context("reserved grant lacks a complete conflicted internal scope")?;
			scopes.insert(scope.source,scope);
		}
		let all=scopes.values().flat_map(ConflictedInternalUserClaimScope::lock_ids).chain(names.iter().cloned())
			.collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
		self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE",&[&all]).await?;
		// All helpers now reacquire only rows already held, before the common
		// WAL/subscription barrier. No chunk fallback can waive partial proof.
		let allocations=scopes.values().flat_map(|scope|scope.allocations.iter().copied()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
		self.query("SELECT id FROM expiry_allocation WHERE id=ANY($1) ORDER BY id FOR UPDATE",&[&allocations]).await?;
		self.lock_htlc_settlements().await?;
		let subscriptions=scopes.values().flat_map(|scope|scope.subscriptions.iter().copied()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
		self.query("SELECT id FROM lightning_htlc_subscription WHERE id=ANY($1) ORDER BY id FOR UPDATE",&[&subscriptions]).await?;
		let mut allowed=BTreeSet::new();
		for proposed in scopes.values() {
			let actual=self.conflicted_internal_user_claim_scope(proposed.source).await?.context("operator return scope changed")?;
			ensure!(&actual==proposed,"operator return membership changed under its locks");
			for source in &actual.incoming {
				ensure!(self.replayable_internal_return_terminal_claim(*source,view).await?.is_some(),
					"operator return requires every original user's current canonical retained claim");
			}
			for grant in ids.iter().filter(|id|actual.grants.contains(id)) {
				let state=htlc_vtxo::get_htlc_vtxo(self,*grant).await?.context("operator timeout input disappeared")?;
				check_grant_scope(&state,state.htlc.payment_hash,view,true)?;
				ensure!(self.query_one("SELECT onchain_spent_height IS NULL AND onchain_spent_txid IS NULL AS unspent FROM vtxo WHERE vtxo_id=$1",&[&grant.to_string()]).await?.get::<_,bool>("unspent"),
					"operator return grant is already consumed");
				allowed.insert(*grant);
			}
		}
		let ordinary=ids.iter().copied().filter(|id|!allowed.contains(id)).collect::<Vec<_>>();
		self.lock_unallocated_watchman_inputs(&ordinary).await
	}
}

#[cfg(test)]
mod tests {
	use bitcoin::{absolute, transaction, TxIn, Txid};
	use bitcoin::hashes::Hash;
	use bitcoin_ext::BlockRef;
	use ark::test_util::VTXO_VECTORS;
	use super::super::htlc_vtxo::{Htlc, HtlcResolution};
	use super::super::VtxoState;
	use super::*;

	fn view() -> ExpiryViewToken {
		let point = BlockRef { height: BlockHeight::new(100), hash: bitcoin::BlockHash::all_zeros() };
		ExpiryViewToken { generation: 7, captaind: point, watchmand: point }
	}

	fn parent(points: &[OutPoint]) -> Transaction {
		Transaction { version: transaction::Version::non_standard(3), lock_time: absolute::LockTime::ZERO,
			input: points.iter().map(|point| TxIn { previous_output: *point, ..TxIn::default() }).collect(), output: vec![] }
	}

	fn receive_grant() -> (HtlcVtxo, ExpiryViewToken) {
		let vtxo = VTXO_VECTORS.round2_vtxo.clone();
		let policy = vtxo.policy().as_server_htlc_recv().expect("normal vector is an outgoing receive");
		let htlc = Htlc { payment_hash: policy.payment_hash, htlc_expiry: policy.htlc_expiry,
			direction: HtlcDirection::Outgoing, offchain_resolution: None,
			chain_resolution: None, chain_resolution_height: None };
		let confirmed = BlockHeight::new(1);
		let maturity = confirmed.checked_add(vtxo.exit_delta()).unwrap().max(htlc.htlc_expiry);
		let mut current = view(); current.captaind.height = maturity; current.watchmand.height = maturity;
		let state = VtxoState { id: 1, vtxo_id: vtxo.id(), vtxo, oor_spent_txid: None,
			spent_in_round: None, offboarded_in: None, banned_until_height: None,
			spend_state: SpendState::HtlcRecvUnclaimed, decided_for_fallback: false,
			confirmed_height: Some(confirmed), created_at: Local::now(), updated_at: Local::now() };
		(HtlcVtxo { htlc, vtxo: state },current)
	}

	// These tests exercise partition/membership only. Cryptographic and actual
	// canonical user-claim admission is tested by expiry_user_claim's helper.
	fn terminal_candidate() -> InternalRefundCandidate {
		let (grant, current) = receive_grant();
		let a = VtxoId::from(OutPoint::new(Txid::from_byte_array([121;32]),0));
		let b = VtxoId::from(OutPoint::new(Txid::from_byte_array([122;32]),0));
		InternalRefundCandidate { attempt_id: 1, subscription_id: 2, payment_hash: grant.htlc.payment_hash,
			view: current, incoming: BTreeMap::from([
				(a,InternalRefundSource::Terminal(ReplayableTerminalClaim { source:a,
					claim:parent(&[a.to_point()]),confirmed:current.watchmand })),
				(b,InternalRefundSource::Live),
			]),existing:BTreeMap::new(),terminal_reservations:BTreeMap::new(),grants:vec![InternalRefundGrant {
				vtxo:ServerVtxo::from(grant.vtxo.vtxo).to_bare(),confirmed:BlockHeight::new(1),updated_at:Local::now(),
			}] }
	}

	#[test]
	fn original_batch_rebuild_rejects_rebound_missing_or_repeated_membership() {
		let member=terminal_candidate();
		let scope=InternalOriginalClaimScope {attempt:member.attempt_id,subscription:member.subscription_id,
			hash:member.payment_hash,originals:member.incoming.keys().copied().collect()};
		let grants=member.grant_ids();
		let evidence=serde_json::json!({"kind":"htlc_internal_joint_timeout","attempt_id":scope.attempt,
			"subscription_id":scope.subscription,"payment_hash":scope.hash.to_string(),
			"original_sources":scope.originals.iter().map(ToString::to_string).collect::<Vec<_>>(),
			"required_grants":grants.iter().map(ToString::to_string).collect::<Vec<_>>()});
		assert!(scope.check_evidence(&evidence,&grants).is_ok());
		for field in ["attempt_id","subscription_id"] {
			let mut other=evidence.clone(); other[field]=999.into();
			assert!(scope.check_evidence(&other,&grants).is_err());
		}
		let mut missing=evidence.clone(); missing["original_sources"].as_array_mut().unwrap().pop();
		assert!(scope.check_evidence(&missing,&grants).is_err());
		let mut repeated=evidence.clone(); repeated["original_sources"][1]=repeated["original_sources"][0].clone();
		assert!(scope.check_evidence(&repeated,&grants).is_err());
		let mut grant=evidence.clone(); grant["required_grants"]=serde_json::json!([]);
		assert!(scope.check_evidence(&grant,&grants).is_err());
	}

	#[test]
	fn unsigned_partial_intent_accepts_only_never_published_pending_or_conflicted_identity() {
		assert!(check_unsigned_dependent_intent("pending",None,false,false,false,false).is_ok());
		assert!(check_unsigned_dependent_intent("conflicted",None,false,false,false,false).is_ok());
		assert!(check_unsigned_dependent_intent("confirmed",None,false,false,false,false).is_err());
		assert!(check_unsigned_dependent_intent("pending",Some("foreign"),false,false,false,false).is_err());
		assert!(check_unsigned_dependent_intent("pending",None,true,false,false,false).is_err());
		assert!(check_unsigned_dependent_intent("pending",None,false,true,false,false).is_err());
		assert!(check_unsigned_dependent_intent("pending",None,false,false,true,false).is_err());
		assert!(check_unsigned_dependent_intent("pending",None,false,false,false,true).is_err());
	}

	#[test]
	fn unsigned_intent_cannot_omit_a_grant_or_replace_its_actual_live_source_path() {
		let a=VtxoId::from(OutPoint::new(Txid::from_byte_array([51;32]),0));
		let b=VtxoId::from(OutPoint::new(Txid::from_byte_array([52;32]),0));
		let prefix=OutPoint::new(Txid::from_byte_array([53;32]),0);
		let g=VtxoId::from(OutPoint::new(Txid::from_byte_array([54;32]),0));
		let grants=BTreeSet::from([g]);let paths=BTreeMap::from([(b,BTreeSet::from([prefix]))]);
		assert!(check_dependent_input_points(a,&[prefix,g.to_point()],&grants,&paths).is_ok());
		assert!(check_dependent_input_points(a,&[prefix],&grants,&paths).is_err());
		assert!(check_dependent_input_points(a,&[a.to_point(),g.to_point()],&grants,&paths).is_err());
		let foreign=OutPoint::new(Txid::from_byte_array([55;32]),0);
		assert!(check_dependent_input_points(a,&[foreign,g.to_point()],&grants,&paths).is_err());
		let signed=parent(&[prefix,g.to_point()]);
		assert!(check_dependent_parent_inputs(a,&signed,&grants,&paths).is_ok());
		assert!(check_terminal_return_parent(&[VtxoId::from(prefix),g],&parent(&[prefix])).is_err(),
			"present signed bodies still require the complete immutable input set");
	}

	#[test]
	fn dependent_partial_parent_requires_explicit_retained_original_identity() {
		let a=VtxoId::from(OutPoint::new(Txid::from_byte_array([41;32]),0));
		let b=VtxoId::from(OutPoint::new(Txid::from_byte_array([42;32]),0));
		let claim=parent(&[a.to_point()]).compute_txid();
		let mut evidence=serde_json::json!({"original_sources":[a.to_string(),b.to_string()],
			"terminal_sources":[{"source":a.to_string(),"claim_txid":claim.to_string(),"prior_joint":true}]});
		assert_eq!(dependent_terminal_txid(&evidence,a).unwrap(),Some(claim));
		assert_eq!(dependent_terminal_txid(&evidence,b).unwrap(),None);
		evidence["terminal_sources"][0]["prior_joint"]=false.into();
		assert_eq!(dependent_terminal_txid(&evidence,a).unwrap(),None);
		evidence["terminal_sources"][0]["prior_joint"]=true.into();
		evidence["original_sources"]=serde_json::json!([b.to_string()]);
		assert!(dependent_terminal_txid(&evidence,a).is_err());
		evidence["original_sources"]=serde_json::json!([a.to_string(),b.to_string()]);
		let duplicate=evidence["terminal_sources"][0].clone();
		evidence["terminal_sources"].as_array_mut().unwrap().push(duplicate);
		assert!(dependent_terminal_txid(&evidence,a).is_err());
	}

	#[test]
	fn dependent_partial_parent_keeps_every_grant_and_live_signed_path_but_omits_paid_source() {
		let a=VtxoId::from(OutPoint::new(Txid::from_byte_array([43;32]),0));
		let b=VtxoId::from(OutPoint::new(Txid::from_byte_array([44;32]),0));
		let prefix=OutPoint::new(Txid::from_byte_array([45;32]),0);
		let g=VtxoId::from(OutPoint::new(Txid::from_byte_array([46;32]),0));
		let grants=BTreeSet::from([g]);
		let paths=BTreeMap::from([(b,BTreeSet::from([prefix]))]);
		assert!(check_dependent_parent_inputs(a,&parent(&[prefix,g.to_point()]),&grants,&paths).is_ok());
		assert!(check_dependent_parent_inputs(a,&parent(&[prefix]),&grants,&paths).is_err());
		assert!(check_dependent_parent_inputs(a,&parent(&[g.to_point()]),&grants,&paths).is_err());
		assert!(check_dependent_parent_inputs(a,&parent(&[a.to_point(),g.to_point()]),&grants,&paths).is_err());
		assert!(check_dependent_parent_inputs(a,&parent(&[prefix,prefix,g.to_point()]),&grants,&paths).is_err());
		let foreign=OutPoint::new(Txid::from_byte_array([47;32]),0);
		assert!(check_dependent_parent_inputs(a,&parent(&[prefix,foreign,g.to_point()]),&grants,&paths).is_err());
	}

	#[test]
	fn partial_reservation_fingerprint_extends_first_lock_union_and_observation() {
		let mut candidate=terminal_candidate();
		let a=*candidate.incoming.keys().find(|id|matches!(candidate.incoming.get(id),Some(InternalRefundSource::Terminal(_)))).unwrap();
		let p=VtxoId::from(OutPoint::new(Txid::from_byte_array([48;32]),0));
		let scope=ConflictedInternalUserClaimScope {source:a,incoming:candidate.incoming.keys().copied().collect(),
			grants:candidate.grant_ids(),physical:vec![p],allow_dependent:true,allocations:vec![91],subscriptions:vec![92],identity:vec![]};
		candidate.terminal_reservations.insert(a,scope);
		let changed=candidate.clone();
		assert!(candidate.same_observation(&changed));
		let mut changed=candidate.clone();
		changed.terminal_reservations.get_mut(&a).unwrap().allocations.push(93);
		assert!(!candidate.same_observation(&changed));
		assert_eq!(candidate.terminal_evidence()[0]["prior_joint"],true);
		let component=InternalRefundComponent::new(candidate.view,vec![candidate]).unwrap();
		assert!(component.obligation_lock_ids().contains(&p.to_string()));
		assert_eq!(component.subscription_ids(),vec![2,92]);
		assert_eq!(component.superseded_allocations(),vec![91]);
	}

	#[test]
	fn terminal_return_cannot_cross_a_different_or_partial_parent_input_set() {
		let a=VtxoId::from(OutPoint::new(Txid::from_byte_array([11;32]),0));
		let g=VtxoId::from(OutPoint::new(Txid::from_byte_array([12;32]),0));
		let admitted=parent(&[a.to_point(),g.to_point()]);
		assert!(check_terminal_return_parent(&[a,g],&admitted).is_ok());
		assert!(check_terminal_return_parent(&[a],&admitted).is_err());
		assert!(check_terminal_return_parent(&[a,g],&parent(&[a.to_point()])).is_err());
		assert!(check_terminal_return_parent(&[a,g],&parent(&[a.to_point(),a.to_point()])).is_err());
		let mut different_version=admitted;
		different_version.version=transaction::Version::TWO;
		assert!(check_terminal_return_parent(&[a,g],&different_version).is_err());
	}

	#[test]
	fn terminal_return_locks_the_complete_sorted_original_grant_and_old_physical_union() {
		let a=VtxoId::from(OutPoint::new(Txid::from_byte_array([13;32]),0));
		let g=VtxoId::from(OutPoint::new(Txid::from_byte_array([14;32]),0));
		let p=VtxoId::from(OutPoint::new(Txid::from_byte_array([15;32]),0));
		let scope=ConflictedInternalUserClaimScope {source:a,incoming:vec![a],grants:vec![g],physical:vec![p,g],
			allow_dependent:false,allocations:vec![7],subscriptions:vec![8],identity:vec![]};
		assert_eq!(scope.lock_ids(),[a,g,p].into_iter().map(|id|id.to_string()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>());
	}

	#[test]
	fn joint_terminal_partition_locks_every_original_but_allocates_only_live_sources() {
		let candidate = terminal_candidate();
		let all = candidate.incoming.keys().copied().collect::<Vec<_>>();
		let live = candidate.live_incoming_ids();
		let terminal = *candidate.incoming.keys().find(|id| !live.contains(id)).unwrap();
		let component = InternalRefundComponent::new(candidate.view,vec![candidate]).unwrap();
		assert_eq!(component.incoming_ids(),all,"terminal originals remain in the final sorted lock union");
		assert_eq!(component.live_incoming_ids(),live);
		assert_eq!(component.terminal_ids(),[terminal].into_iter().collect());
		assert!(component.for_source(terminal).is_none(),"an actual exited source gets no new customer allocation");
		assert!(component.for_source(live[0]).is_some());
		let required = component.grant_ids();
		let points = live.iter().map(|id| id.to_point()).chain(required.iter().map(|id|id.to_point())).collect::<Vec<_>>();
		assert!(check_parent_inputs(component.view,component.view,&[live[0].to_point()],&required,&parent(&points)).is_ok());
		assert!(check_parent_inputs(component.view,component.view,&[live[0].to_point()],&required,&parent(&[live[0].to_point()])).is_err());
		let mut extra = points;
		extra.push(terminal.to_point());
		assert!(check_parent_inputs(component.view,component.view,&[live[0].to_point()],&required,&parent(&extra)).is_err());
	}

	#[test]
	fn joint_terminal_partition_revalidates_exact_claim_body_and_block() {
		let candidate = terminal_candidate();
		let mut changed = candidate.clone();
		assert!(candidate.same_observation(&changed));
		let claim = changed.incoming.values_mut().find_map(|state| match state {
			InternalRefundSource::Terminal(claim) => Some(claim), _ => None,
		}).unwrap();
		claim.claim.lock_time = absolute::LockTime::from_height(1).unwrap();
		assert!(!candidate.same_observation(&changed),"the final commit cannot substitute another signed body");
		let mut changed = candidate.clone();
		let claim = changed.incoming.values_mut().find_map(|state| match state {
			InternalRefundSource::Terminal(claim) => Some(claim), _ => None,
		}).unwrap();
		claim.confirmed.hash = bitcoin::BlockHash::from_byte_array([123;32]);
		assert!(!candidate.same_observation(&changed),"a reorg requires fresh classification before commit");
	}

	#[test]
	fn joint_terminal_partition_rejects_a_rebound_or_future_terminal_observation() {
		let candidate = terminal_candidate();
		let mut changed = candidate.clone();
		let claim = changed.incoming.values_mut().find_map(|state| match state {
			InternalRefundSource::Terminal(claim) => Some(claim), _ => None,
		}).unwrap();
		claim.source = VtxoId::from(OutPoint::new(Txid::from_byte_array([124;32]),0));
		assert!(InternalRefundComponent::new(changed.view,vec![changed]).is_err());
		let mut changed = candidate.clone();
		let claim = changed.incoming.values_mut().find_map(|state| match state {
			InternalRefundSource::Terminal(claim) => Some(claim), _ => None,
		}).unwrap();
		claim.confirmed.height = changed.view.watchmand.height + bitcoin_ext::BlockDelta::new(1);
		assert!(InternalRefundComponent::new(changed.view,vec![changed]).is_err());
	}

	#[test]
	fn joint_refund_checks_original_incoming_policy_instead_of_metadata_alone() {
		let (mut source, _) = receive_grant();
		source.vtxo.vtxo = VTXO_VECTORS.arkoor_htlc_out_vtxo.clone();
		source.vtxo.spend_state = SpendState::Spendable;
		let policy = source.vtxo.vtxo.policy().as_server_htlc_send().unwrap();
		source.htlc.direction = HtlcDirection::Incoming;
		source.htlc.payment_hash = policy.payment_hash;
		source.htlc.htlc_expiry = policy.htlc_expiry;
		let hash = source.htlc.payment_hash;
		assert!(check_incoming(&source, hash).is_ok());
		source.htlc.htlc_expiry = source.htlc.htlc_expiry + bitcoin_ext::BlockDelta::new(1);
		assert!(check_incoming(&source, hash).is_err());
	}

	#[test]
	fn joint_refund_requires_the_native_unclaimed_receive_state() {
		let (mut grant, current) = receive_grant();
		assert_eq!(grant.vtxo.spend_state, SpendState::HtlcRecvUnclaimed);
		assert!(check_grant(&grant, grant.htlc.payment_hash, current).is_ok());
		grant.vtxo.spend_state = SpendState::Spendable;
		assert!(check_grant(&grant, grant.htlc.payment_hash, current).is_err(),
			"a different upstream state cannot replace the original unclaimed grant");
	}

	#[test]
	fn joint_refund_rejects_a_partially_claimed_receive_set() {
		let (mut grant,current) = receive_grant();
		let hash = grant.htlc.payment_hash;
		assert!(check_grant(&grant,hash,current).is_ok());
		grant.htlc.chain_resolution = Some(HtlcResolution::Fulfilled);
		grant.htlc.chain_resolution_height = Some(current.watchmand.height);
		assert!(check_grant(&grant,hash,current).is_err(), "one actual user claim excludes the entire joint refund");
	}

	#[test]
	fn joint_refund_cannot_replace_an_already_returned_input_with_chain_depth() {
		let (mut grant,current) = receive_grant();
		grant.htlc.chain_resolution = Some(HtlcResolution::Revoked);
		grant.htlc.chain_resolution_height = Some(BlockHeight::new(2));
		assert!(check_grant(&grant,grant.htlc.payment_hash,current).is_err());
	}

	#[test]
	fn joint_refund_requires_each_timeout_in_both_completed_views() {
		let (grant,mut current) = receive_grant();
		current.watchmand.height = current.watchmand.height.saturating_sub(bitcoin_ext::BlockDelta::new(1));
		assert!(check_grant(&grant,grant.htlc.payment_hash,current).is_err());
	}

	#[test]
	fn joint_refund_cannot_drop_one_signed_receive_dependency() {
		let source = OutPoint::new(Txid::from_byte_array([1;32]),0);
		let a = OutPoint::new(Txid::from_byte_array([2;32]),0);
		let b = OutPoint::new(Txid::from_byte_array([3;32]),0);
		let required = [VtxoId::from(a),VtxoId::from(b)];
		assert!(check_parent_inputs(view(),view(),&[source],&required,&parent(&[source,a,b])).is_ok());
		assert!(check_parent_inputs(view(),view(),&[source],&required,&parent(&[source,a])).is_err());
		assert!(check_parent_inputs(view(),view(),&[source],&required,&parent(&[source,a,b,b])).is_err());
	}

	#[test]
	fn joint_refund_reorg_cannot_reuse_an_old_prepared_input_set() {
		let source = OutPoint::new(Txid::from_byte_array([1;32]),0);
		let grant = OutPoint::new(Txid::from_byte_array([2;32]),0);
		let mut changed = view(); changed.generation += 1;
		assert!(check_parent_inputs(view(),changed,&[source],&[grant.into()],&parent(&[source,grant])).is_err());
	}

	#[test]
	fn joint_component_follows_shared_root_connections_transitively() {
		let a = OutPoint::new(Txid::from_byte_array([1;32]),0);
		let b = OutPoint::new(Txid::from_byte_array([2;32]),0);
		let c = OutPoint::new(Txid::from_byte_array([3;32]),0);
		// Deliberately put the far member before its bridge: closure must not
		// depend on discovery order or decide only the first ready attempt.
		let members = [[a].into_iter().collect(), [b,c].into_iter().collect(), [a,b].into_iter().collect()];
		assert!(check_connected_roots(&members).is_ok());
		assert!(check_connected_roots(&[[a].into_iter().collect(), [b].into_iter().collect()]).is_err());
	}

	#[test]
	fn joint_component_parent_keeps_both_attempts_receive_dependencies() {
		let source = OutPoint::new(Txid::from_byte_array([1;32]),0);
		let first = [OutPoint::new(Txid::from_byte_array([2;32]),0), OutPoint::new(Txid::from_byte_array([3;32]),0)];
		let second = [OutPoint::new(Txid::from_byte_array([4;32]),0), OutPoint::new(Txid::from_byte_array([5;32]),0)];
		let grants = first.into_iter().chain(second).map(VtxoId::from).collect::<Vec<_>>();
		assert!(check_parent_inputs(view(),view(),&[source],&grants,
			&parent(&[source,first[0],first[1],second[0],second[1]])).is_ok());
		assert!(check_parent_inputs(view(),view(),&[source],&grants,
			&parent(&[source,first[0],first[1]])).is_err(), "one attempt cannot silently shed the other grant set");
	}
	#[test]
	fn joint_replan_preserves_the_exact_original_obligation_and_entire_grant_set() {
		let hash = PaymentHash::from([9;32]);
		let a = VtxoId::from(OutPoint::new(Txid::from_byte_array([2;32]),0));
		let b = VtxoId::from(OutPoint::new(Txid::from_byte_array([3;32]),0));
		let evidence = serde_json::json!({"kind":"htlc_internal_joint_timeout", "attempt_id":7,
			"subscription_id":8,"payment_hash":hash.to_string(), "required_grants":[a.to_string(),b.to_string()]});
		assert!(check_existing_joint_evidence(&evidence,7,8,hash,Some(&[b,a])).is_ok());
		assert!(check_existing_joint_evidence(&evidence,6,8,hash,Some(&[a,b])).is_err());
		assert!(check_existing_joint_evidence(&evidence,7,6,hash,Some(&[a,b])).is_err());
		assert!(check_existing_joint_evidence(&evidence,7,8,PaymentHash::from([8;32]),Some(&[a,b])).is_err());
		assert!(check_existing_joint_evidence(&evidence,7,8,hash,Some(&[a])).is_err());
		assert!(check_existing_joint_evidence(&evidence,7,8,hash,Some(&[a,a])).is_err());
		let mut repeated = evidence;
		repeated["required_grants"] = serde_json::json!([a.to_string(),a.to_string()]);
		assert!(check_existing_joint_evidence(&repeated,7,8,hash,None).is_err());
	}

	#[test]
	fn joint_replan_spent_grant_requires_its_own_reservation_and_no_resolution() {
		let (mut grant,current) = receive_grant();
		let hash = grant.htlc.payment_hash;
		grant.vtxo.spend_state = SpendState::Spent;
		assert!(check_grant_scope(&grant,hash,current,false).is_err());
		assert!(check_grant_scope(&grant,hash,current,true).is_ok());
		grant.htlc.chain_resolution = Some(HtlcResolution::Revoked);
		assert!(check_grant_scope(&grant,hash,current,true).is_err(),
			"a historical reservation cannot replace a consumed dependency with an observation");
	}

}
