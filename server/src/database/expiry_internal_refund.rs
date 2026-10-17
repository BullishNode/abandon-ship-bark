//! Source-specific preparation for one inseparable internal timeout refund.
//! This is not an ownership authorization: the financial commit must consume
//! every receive grant in the SAME parent as its original incoming backing.

use std::collections::BTreeSet;

use anyhow::Context;
use ark::{ProtocolEncoding, ServerVtxo, VtxoId};
use ark::lightning::PaymentHash;
use bitcoin::{OutPoint, Transaction};
use bitcoin_ext::BlockHeight;
use chrono::{DateTime, Local};

use super::{SpendState, Tx};
use super::expiry_chain::ExpiryViewToken;
use super::expiry_decision::ReadyOutput;
use super::htlc_vtxo::{self, HtlcDirection, HtlcVtxo};

pub(crate) const MAX_JOINT_GRANTS: i64 = 64;

pub(crate) struct InternalRefundGrant {
	pub vtxo: ServerVtxo,
	confirmed: BlockHeight,
	updated_at: DateTime<Local>,
}

pub(crate) struct InternalRefundCandidate {
	pub attempt_id: i64,
	pub subscription_id: i64,
	pub payment_hash: PaymentHash,
	pub view: ExpiryViewToken,
	pub incoming: Vec<VtxoId>,
	pub grants: Vec<InternalRefundGrant>,
}

/// Original attempts keep their separate funding identities. Only their
/// inseparable Bitcoin input dependency is grouped; this creates no new owner.
pub(crate) struct InternalRefundComponent {
	pub view: ExpiryViewToken,
	pub members: Vec<InternalRefundCandidate>,
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
			ensure!(!member.incoming.is_empty() && !member.grants.is_empty(), "internal refund member omits an obligation");
			for id in &member.incoming {
				ensure!(incoming.insert(*id), "internal attempts share an original logical source");
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
		self.members.iter().flat_map(|member| member.incoming.iter().copied()).collect()
	}

	pub(crate) fn grants(&self) -> impl Iterator<Item = &InternalRefundGrant> {
		self.members.iter().flat_map(|member| &member.grants)
	}

	pub(crate) fn grant_ids(&self) -> Vec<VtxoId> { self.grants().map(|grant| grant.vtxo.id()).collect() }

	pub(crate) fn for_source(&self, id: VtxoId) -> Option<&InternalRefundCandidate> {
		self.members.iter().find(|member| member.incoming.contains(&id))
	}

	pub(crate) fn check_parent(
		&self, current: ExpiryViewToken, incoming: &[ReadyOutput], parent: &Transaction,
	) -> anyhow::Result<()> {
		let ids = self.incoming_ids();
		for id in &ids {
			ensure!(incoming.iter().filter(|output| output.contains_coin(*id)).count() == 1,
				"internal component does not cover one physical path per original source");
		}
		ensure!(incoming.iter().all(|output| ids.iter().any(|id| output.contains_coin(*id))),
			"internal component adds unrelated backing");
		let member_roots = self.members.iter().map(|member| incoming.iter()
			.filter(|output| member.incoming.iter().any(|id| output.contains_coin(*id)))
			.map(|output| output.root().point()).collect()).collect::<Vec<BTreeSet<_>>>();
		check_connected_roots(&member_roots)?;
		check_parent_inputs(self.view, current,
			&incoming.iter().map(|output| output.root().point()).collect::<Vec<_>>(), &self.grant_ids(), parent)
	}
}

fn check_incoming(source: &HtlcVtxo, hash: PaymentHash) -> anyhow::Result<()> {
	ensure!(source.direction() == HtlcDirection::Incoming && source.htlc.payment_hash == hash,
		"joint refund original funding has different source metadata");
	let policy = source.vtxo.vtxo.policy().as_server_htlc_send().context("joint funding is not an actual send contract")?;
	ensure!(policy.payment_hash == hash && policy.htlc_expiry == source.htlc_expiry(),
		"joint incoming metadata differs from its signed source policy");
	ensure!(source.htlc.offchain_resolution.is_none() && source.htlc.chain_resolution.is_none()
		&& source.vtxo.spend_state == SpendState::Spendable && !source.vtxo.decided_for_fallback
		&& source.vtxo.oor_spent_txid.is_none() && source.vtxo.spent_in_round.is_none()
		&& source.vtxo.offboarded_in.is_none(), "joint incoming source already has another commitment");
	Ok(())
}

fn check_grant(grant: &HtlcVtxo, hash: PaymentHash, view: ExpiryViewToken) -> anyhow::Result<()> {
	ensure!(grant.direction() == HtlcDirection::Outgoing && grant.htlc.payment_hash == hash,
		"joint refund contains a different receive grant");
	ensure!(grant.htlc.offchain_resolution.is_none() && grant.htlc.chain_resolution.is_none(),
		"joint refund grant is already claimed or returned; an actual dependency bridge is required");
	ensure!(grant.vtxo.spend_state == SpendState::HtlcRecvUnclaimed && !grant.vtxo.decided_for_fallback
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

	fn same_observation(&self, other: &Self) -> bool {
		self.attempt_id == other.attempt_id && self.subscription_id == other.subscription_id
			&& self.payment_hash == other.payment_hash && self.incoming == other.incoming
			&& self.grant_ids() == other.grant_ids() && self.grants.iter().zip(&other.grants).all(|(a,b)|
				a.confirmed == b.confirmed && a.updated_at == b.updated_at && a.vtxo.serialize() == b.vtxo.serialize())
	}

}

impl Tx<'_> {
	/// Acquire the complete physical/source union before the existing WAL2
	/// barrier and every subscription, never one member at a time. Discovery
	/// and geometric preparation remain outside this short transaction.
	pub(crate) async fn lock_unclaimed_internal_refund_component(
		&self, proposed: &InternalRefundComponent, current: ExpiryViewToken, incoming: &[ReadyOutput],
	) -> anyhow::Result<InternalRefundComponent> {
		ensure!(current == proposed.view, "internal component generation changed");
		self.lock_valid_expiry_view(current).await?;
		let ids = proposed.incoming_ids().into_iter().chain(proposed.grant_ids())
			.chain(incoming.iter().flat_map(ReadyOutput::source_ids)).collect::<Vec<_>>();
		super::expiry_guard::guard_obligation_changes(self, &ids).await?;
		self.lock_htlc_settlements().await?;
		let mut subscriptions = proposed.members.iter().map(|member| member.subscription_id).collect::<Vec<_>>();
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

	/// Only a NEW, unmaterialized and still-unspent receive is eligible here.
	/// Historical returns to a cold drain cannot authorize a cross-root refund
	/// without a real spendable bridge; observed depth alone is insufficient.
	pub(crate) async fn unclaimed_internal_refund(
		&self, attempt_id: i64, view: ExpiryViewToken,
	) -> anyhow::Result<Option<InternalRefundCandidate>> {
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
		let incoming = self.query("SELECT l.vtxo_id FROM lightning_payment_attempt_htlc_vtxo l \
			JOIN vtxo v ON v.vtxo_id=l.vtxo_id JOIN htlc_vtxo h ON h.id=v.id \
			WHERE l.lightning_payment_attempt_id=$1 AND h.direction='incoming' AND h.payment_hash=$2 \
			AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL \
			AND v.onchain_spent_height IS NULL AND v.onchain_spent_txid IS NULL \
			AND v.spend_state='spendable' AND NOT EXISTS(SELECT 1 FROM expiry_decided_coin d WHERE d.vtxo_id=v.vtxo_id) \
			ORDER BY l.vtxo_id", &[&attempt_id,&hash.to_string()]).await?
			.into_iter().map(|r| r.get::<_, &str>("vtxo_id").parse()).collect::<Result<Vec<VtxoId>,_>>()?;
		let total = self.query_one("SELECT COUNT(*) FROM lightning_payment_attempt_htlc_vtxo WHERE lightning_payment_attempt_id=$1",
			&[&attempt_id]).await?.get::<_, i64>(0);
		ensure!(!incoming.is_empty() && incoming.len() as i64 == total && total <= MAX_JOINT_GRANTS,
			"joint refund lacks its complete still-uncommitted original funding set");
		for id in &incoming {
			let source = htlc_vtxo::get_htlc_vtxo(self, *id).await?.context("joint incoming source lost its signed body")?;
			check_incoming(&source, hash)?;
		}
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
			check_grant(&grant, hash, view)?;
			grants.push(InternalRefundGrant { confirmed: grant.vtxo.confirmed_height.expect("checked above"),
				updated_at: grant.vtxo.updated_at, vtxo: ServerVtxo::from(grant.vtxo.vtxo).to_bare() });
		}
		Ok(Some(InternalRefundCandidate { attempt_id, subscription_id, payment_hash: hash, view, incoming, grants }))
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
}
