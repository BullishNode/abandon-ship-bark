//! Source-checked allocation freeze. Frozen value is not yet paid.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, Network, TxOut};
use bitcoin::hashes::{sha256, Hash};
use ark::{ProtocolEncoding, ServerVtxo, VtxoId};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;

use crate::ln::SendRefundEvidence;
use crate::watchman::settlement::{allocate, CustomerLeaf, LeafAllocation};

use super::carrier_snapshot::{BackingPartitionResidual, BackingSnapshotLimits};
use super::expiry_ownership::{DependencySource, ExpirySource, OwnershipContext, OwnershipDisposition, RetirementDependency, RetirementRule};
use super::Tx;

pub struct FrozenExpiryPlan {
	pub id: String,
	pub allocation_ids: Vec<VtxoId>,
	pub backing_ids: Vec<VtxoId>,
	pub recipients: Vec<TxOut>,
}

struct FrozenLeaf {
	amounts: LeafAllocation,
	owner: Option<bitcoin::secp256k1::PublicKey>,
	script: Option<bitcoin::ScriptBuf>,
	disposition: &'static str,
	source: String,
	evidence: serde_json::Value,
}

/// Admission and freeze share ordered durable source locks. Check after the
/// locks, so a writer that waited for a freeze sees its committed reservation.
/// This protects source and physical-retirement rows without changing their
/// spend state; frozen ownership remains separate from confirmed payment.
pub(super) async fn check_not_expiry_frozen(
	tx: &tokio_postgres::Transaction<'_>, ids: &[String],
) -> anyhow::Result<()> {
	if ids.is_empty() { return Ok(()); }
	tx.query("SELECT id FROM pending_board WHERE vtxo_id=ANY($1) OR id=ANY($1) ORDER BY id FOR UPDATE", &[&ids]).await?;
	tx.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE", &[&ids]).await?;
	if tx.query_opt("SELECT id FROM expiry_allocation WHERE id=ANY($1) OR retirement_ids && $1::text[] LIMIT 1", &[&ids]).await?.is_some() {
		return badarg!("{}", server_rpc::EXPIRY_FROZEN_ERROR);
	}
	Ok(())
}


fn receive_dependency_retired(
	dependency: &RetirementDependency, retired: &BTreeMap<PaymentHash, BTreeSet<VtxoId>>,
) -> bool {
	let DependencySource::ReceiveGrants(hash) = &dependency.source else { return false; };
	let expected = dependency.ids.iter().copied().collect::<BTreeSet<_>>();
	dependency.rule == RetirementRule::ConfirmedOrSameCarrier && !expected.is_empty()
		&& expected.len() == dependency.ids.len() && retired.get(hash) == Some(&expected)
}

impl Tx<'_> {
	/// A conservative physical fence: any frozen member/retirement root under
	/// the same original anchor holds this entire anchor. Logical leaf IDs alone
	/// cannot authorize consuming an ancestor. Disjoint siblings may defer.
	pub(crate) async fn lock_unfrozen_urgent_sources(&self, sources: &[ServerVtxo]) -> anyhow::Result<()> {
		ensure!(!sources.is_empty(), "urgent reaction has no authenticated sources");
		self.lock_htlc_settlements().await?;
		self.lock_expiry_carriers().await?;
		let ids = sources.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
		let anchors = sources.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
		self.lock_expiry_inputs(&ids).await?;
		for source in sources {
			let stored = self.get_bare_vtxo_by_id(source.id()).await?.vtxo;
			ensure!(stored.chain_anchor() == source.chain_anchor() && stored.txout() == source.txout()
				&& stored.policy() == source.policy(), "urgent source identity changed");
		}
		let frozen = self.query_opt("SELECT a.id FROM expiry_allocation a
			WHERE a.id=ANY($1) OR a.retirement_ids && $1::text[]
			OR EXISTS (SELECT 1 FROM vtxo v WHERE v.anchor_point=ANY($2)
				AND (v.vtxo_id=a.id OR v.vtxo_id=ANY(a.retirement_ids)))
			OR EXISTS (SELECT 1 FROM pending_board p WHERE p.id=ANY($2)
				AND (p.vtxo_id=a.id OR p.vtxo_id=ANY(a.retirement_ids))) LIMIT 1",
			&[&ids, &anchors]).await?;
		ensure!(frozen.is_none(), "urgent reaction overlaps a frozen physical anchor or retirement dependency");
		Ok(())
	}

	/// Reciprocal fence for a previously retained urgent publication. The
	/// existing nursery stores exact signed bytes before any Core await, under
	/// the same carrier lock. Confirmation/abandonment never erases those bytes.
	/// Only internal nursery transactions can be urgent; direct payout entries
	/// retain their existing complete allocation authorization independently.
	pub(crate) async fn refuse_retained_urgent_overlap(
		&self, snapshot: &super::carrier_snapshot::BackingSnapshot, budget: BackingSnapshotLimits,
	) -> anyhow::Result<()> {
		let anchors = snapshot.roots.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
		let ids = snapshot.regular.iter().map(|v| v.state.vtxo_id.to_string())
			.chain(snapshot.pending_boards.iter().map(|v| v.vtxo.id().to_string())).collect::<BTreeSet<_>>();
		let size = self.query_one("SELECT count(*)::bigint AS rows, COALESCE(sum(octet_length(tx)),0)::bigint AS bytes
			FROM nursery_tx WHERE kind='internal'", &[]).await?;
		ensure!(usize::try_from(size.get::<_, i64>("rows"))? <= budget.max_rows &&
			usize::try_from(size.get::<_, i64>("bytes"))? <= budget.max_serialized_bytes,
			"retained urgent publication inventory exceeds freeze read bounds");
		for row in self.query("SELECT tx FROM nursery_tx WHERE kind='internal' ORDER BY txid", &[]).await? {
			let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&row.get::<_, Vec<u8>>("tx"))?;
			let inputs = tx.input.iter().map(|i| i.previous_output.to_string()).collect::<Vec<_>>();
			ensure!(!inputs.iter().any(|id| ids.contains(id)), "expiry freeze overlaps retained urgent signed inputs");
			ensure!(self.query_opt("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) AND anchor_point=ANY($2) LIMIT 1",
				&[&inputs, &anchors]).await?.is_none(), "expiry freeze shares retained urgent physical backing");
		}
		Ok(())
	}

	pub(crate) async fn check_not_expiry_frozen(&self, ids: &[VtxoId]) -> anyhow::Result<()> {
		let keys = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		check_not_expiry_frozen(self, &keys).await
	}

	/// Caller holds the sorted payment and VTXO flux guards for the entire
	/// expanded dependency closure. Node observations must be fenced against a
	/// late payment. This takes the settlement lock before ordered source locks,
	/// reconstructs the complete inventory and resolves ownership again.
	///
	/// An unresolved or conditional closure is refused as a whole, with no
	/// allocation prefix. Conditional supersession/retirement reconciliation must
	/// be qualified before such a plan becomes publication-ready. Admission and
	/// automatic dispatch integration remain separate from this storage method.
	pub async fn freeze_expiry_sources(
		&self, roots: &[ServerVtxo], terminals: &[ServerVtxo<Full>],
		residuals: &[BackingPartitionResidual], context: &OwnershipContext<'_>,
		send_evidence: &BTreeMap<PaymentHash, SendRefundEvidence>, network: Network,
		budget: BackingSnapshotLimits,
	) -> anyhow::Result<FrozenExpiryPlan> {
		self.freeze_expiry_sources_after_receive_retirement(roots, terminals, residuals, context,
			send_evidence, network, budget, &BTreeMap::new()).await
	}

	/// Only the guarded server handoff supplies already confirmed, exact
	/// operator grant proofs. The public storage entry keeps all dependencies held.
	pub(crate) async fn freeze_expiry_sources_after_receive_retirement(
		&self, roots: &[ServerVtxo], terminals: &[ServerVtxo<Full>],
		residuals: &[BackingPartitionResidual], context: &OwnershipContext<'_>,
		send_evidence: &BTreeMap<PaymentHash, SendRefundEvidence>, network: Network,
		budget: BackingSnapshotLimits, retired: &BTreeMap<PaymentHash, BTreeSet<VtxoId>>,
	) -> anyhow::Result<FrozenExpiryPlan> {
		ensure!(!roots.is_empty() && !terminals.is_empty(), "empty expiry source freeze");
		ensure!(roots.len() <= budget.max_rows && terminals.len() <= budget.max_rows && residuals.len() <= budget.max_rows,
			"expiry source freeze exceeds row budget");
		self.lock_htlc_settlements().await?;
		self.lock_expiry_carriers().await?;
		self.require_expiry_route(true).await?;
		let before = self.backing_snapshot(roots, budget).await?;
		let keys = before.regular.iter().map(|f| f.state.vtxo_id.to_string())
			.chain(before.pending_boards.iter().map(|f| f.vtxo.id().to_string())).collect::<Vec<_>>();
		self.lock_expiry_inputs(&keys).await?;
		let snapshot = self.backing_snapshot(roots, budget).await?;
		self.refuse_retained_urgent_overlap(&snapshot, budget).await?;
		ensure!(snapshot.regular.iter().filter(|f| roots.iter().any(|r| r.id() == f.state.vtxo_id))
			.all(|f| f.onchain_spent_txid.is_none()), "expiry backing already consumed");
		let funding = context.funding.values().cloned().collect::<Vec<_>>();
		let partition = snapshot.check_partition(terminals, &funding, residuals, budget)?;
		let covered = terminals.iter().map(|v| v.id()).collect::<Vec<_>>();
		let mut decisions = self.expiry_ownership_proposal(&snapshot, &covered, context, send_evidence).await?;
		for decision in &mut decisions {
			decision.dependencies.retain(|dependency| !receive_dependency_retired(dependency, retired));
		}
		ensure!(decisions.iter().all(|d| d.dependencies.is_empty()), "expiry closure requires retirement reconciliation before freeze");
		let owners = decisions.iter().filter_map(|d| match d.disposition {
			OwnershipDisposition::Customer(input) => Some(input.owner), _ => None,
		}).collect::<Vec<_>>();
		let scripts = self.authenticated_fallback_scripts(&owners, network).await?;
		let mut rows = BTreeMap::new();
		let mut customers = Vec::new();
		for decision in decisions {
			let proof = terminals.iter().find(|v| v.id() == decision.id).context("missing covered expiry terminal")?;
			ensure!(decision.gross == proof.amount(), "expiry source gross differs from physical terminal");
			let evidence = serde_json::json!({"version":1,"source_path_sha256":sha256::Hash::hash(&proof.serialize()).to_string()});
			match decision.disposition {
				OwnershipDisposition::Customer(input) => {
					let script = scripts.get(&input.owner).context("expiry source has no authenticated address")?.clone();
					customers.push(CustomerLeaf { id: decision.id, script: script.clone(), amount: decision.gross });
					rows.insert(decision.id, FrozenLeaf {
						amounts: LeafAllocation { id: decision.id, gross: decision.gross, fee: Amount::ZERO,
							payout: Amount::ZERO, forfeited: Amount::ZERO }, owner: Some(input.owner), script: Some(script),
						disposition: "pay", source: source_name(input.source), evidence,
					});
				},
				OwnershipDisposition::Operator(reason) => {
					rows.insert(decision.id, FrozenLeaf { amounts: LeafAllocation { id: decision.id, gross: decision.gross,
						fee: Amount::ZERO, payout: Amount::ZERO, forfeited: Amount::ZERO }, owner: None, script: None,
						disposition: "operator", source: format!("operator:{reason:?}"), evidence });
				},
				OwnershipDisposition::Pending(reason) => bail!("expiry source {} is pending: {reason:?}", decision.id),
				_ => bail!("expiry source {} has conditional ownership", decision.id),
			}
		}
		for residual in residuals.iter().filter(|r| r.txout.value > Amount::ZERO) {
			let id = VtxoId::from(residual.outpoint);
			ensure!(rows.insert(id, FrozenLeaf { amounts: LeafAllocation { id, gross: residual.txout.value,
				fee: Amount::ZERO, payout: Amount::ZERO, forfeited: Amount::ZERO }, owner: None, script: None,
				disposition: "operator", source: "transition-fee-anchor".into(), evidence: serde_json::json!({
					"version":1,"exact_p2a_outpoint":residual.outpoint.to_string(),
					"txout_sha256":sha256::Hash::hash(&bitcoin::consensus::serialize(&residual.txout)).to_string(),
				}) }).is_none(), "expiry anchor duplicates a terminal");
		}
		let plan = allocate(customers)?;
		for recipient in &plan.recipients {
			for leaf in &recipient.leaves {
				let row = rows.get_mut(&leaf.id).context("missing expiry customer leaf")?;
				row.amounts = LeafAllocation { ..*leaf };
			}
		}
		for leaf in &plan.forfeits {
			let row = rows.get_mut(&leaf.id).context("missing expiry forfeit leaf")?;
			row.disposition = "forfeit"; row.amounts = LeafAllocation { ..*leaf };
		}
		let principal = rows.values().try_fold(Amount::ZERO, |sum, r| sum.checked_add(r.amounts.gross))
			.context("expiry freeze gross overflow")?;
		ensure!(principal <= Amount::MAX_MONEY && Some(principal) == roots.iter().try_fold(Amount::ZERO, |sum, r| sum.checked_add(r.amount())),
			"expiry frozen gross does not conserve physical backing");
		ensure!(rows.len() <= budget.max_rows, "complete expiry allocation exceeds row budget");
		let backing_ids = partition.roots.iter().map(|r| r.root).collect::<BTreeSet<_>>();
		let retirement = backing_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let facts = rows.iter().map(|(id,r)| serde_json::json!({"id":id.to_string(),"owner":r.owner.map(|p| p.to_string()),
			"script":r.script.as_ref().map(|s| s.to_hex_string()),"disposition":r.disposition,"gross":r.amounts.gross.to_sat(),
			"fee":r.amounts.fee.to_sat(),"payout":r.amounts.payout.to_sat(),"forfeited":r.amounts.forfeited.to_sat(),
			"source":r.source,"evidence":r.evidence})).collect::<Vec<_>>();
		let binding = serde_json::to_vec(&serde_json::json!({"version":1,"genesis":bitcoin::constants::ChainHash::using_genesis_block(network).to_string(),
			"server":context.server_pubkey.to_string(),"retirement":retirement,"allocations":facts}))?;
		ensure!(binding.len() <= budget.max_serialized_bytes, "complete expiry freeze exceeds byte budget");
		let id = sha256::Hash::hash(&binding).to_string();
		for (key, row) in &rows {
			let owner = row.owner.map(|p| p.serialize().to_vec());
			let script = row.script.as_ref().map(|s| s.as_bytes().to_vec());
			let evidence = serde_json::to_string(&row.evidence)?;
			self.execute("INSERT INTO expiry_allocation (id,freeze_set,owner_pubkey,spk,disposition,gross_sat,fee_sat,payout_sat,
				forfeited_sat,source,source_evidence,retirement_ids) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::text::jsonb,$12)
				ON CONFLICT (id) DO NOTHING", &[&key.to_string(), &id, &owner, &script, &row.disposition,
				&i64::try_from(row.amounts.gross.to_sat())?, &i64::try_from(row.amounts.fee.to_sat())?,
				&i64::try_from(row.amounts.payout.to_sat())?, &i64::try_from(row.amounts.forfeited.to_sat())?, &row.source, &evidence, &retirement]).await?;
			let stored = self.query_one("SELECT freeze_set,owner_pubkey,spk,disposition,gross_sat,fee_sat,payout_sat,forfeited_sat,
				source,source_evidence::text AS evidence,retirement_ids FROM expiry_allocation WHERE id=$1", &[&key.to_string()]).await?;
			ensure!(stored.get::<_, &str>("freeze_set") == id && stored.get::<_, Option<Vec<u8>>>("owner_pubkey") == owner &&
				stored.get::<_, Option<Vec<u8>>>("spk") == script && stored.get::<_, &str>("disposition") == row.disposition &&
				stored.get::<_, i64>("gross_sat") == i64::try_from(row.amounts.gross.to_sat())? &&
				stored.get::<_, i64>("fee_sat") == i64::try_from(row.amounts.fee.to_sat())? &&
				stored.get::<_, i64>("payout_sat") == i64::try_from(row.amounts.payout.to_sat())? &&
				stored.get::<_, i64>("forfeited_sat") == i64::try_from(row.amounts.forfeited.to_sat())? &&
				stored.get::<_, &str>("source") == row.source && serde_json::from_str::<serde_json::Value>(stored.get("evidence"))? == row.evidence &&
				stored.get::<_, Vec<String>>("retirement_ids") == retirement, "conflicting frozen expiry source");
		}
		Ok(FrozenExpiryPlan { id, allocation_ids: rows.keys().copied().collect(),
			backing_ids: backing_ids.into_iter().collect(), recipients: plan.recipients.into_iter().map(|r| r.output).collect() })
	}
}

fn source_name(source: ExpirySource) -> String {
	match source {
		ExpirySource::Registered => "registered".into(), ExpirySource::PendingBoard => "pending-board".into(),
		ExpirySource::Unregistered => "unregistered".into(),
		ExpirySource::LightningReceive(hash) => format!("lightning-receive:{hash}"),
		ExpirySource::LightningSend(hash) => format!("lightning-send:{hash}"),
	}
}

#[cfg(test)]
mod receive_retirement_tests {
	use super::*;

	#[test]
	fn receive_dependency_requires_exact_complete_hash_bound_proof() {
		let hash = "01".repeat(32).parse::<PaymentHash>().unwrap();
		let other = "02".repeat(32).parse::<PaymentHash>().unwrap();
		let first = format!("{:064x}:0", 1).parse::<VtxoId>().unwrap();
		let second = format!("{:064x}:0", 2).parse::<VtxoId>().unwrap();
		let dependency = RetirementDependency { source: DependencySource::ReceiveGrants(hash),
			ids: vec![first, second], rule: RetirementRule::ConfirmedOrSameCarrier };
		assert!(!receive_dependency_retired(&dependency, &BTreeMap::new()));
		assert!(!receive_dependency_retired(&dependency, &BTreeMap::from([(hash, BTreeSet::from([first]))])));
		assert!(!receive_dependency_retired(&dependency, &BTreeMap::from([(other, BTreeSet::from([first, second]))])));
		assert!(receive_dependency_retired(&dependency, &BTreeMap::from([(hash, BTreeSet::from([first, second]))])));
	}
}
