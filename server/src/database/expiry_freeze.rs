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
use super::expiry_ownership::{ExpirySource, OwnershipContext, OwnershipDisposition};
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

impl Tx<'_> {
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
		ensure!(!roots.is_empty() && !terminals.is_empty(), "empty expiry source freeze");
		ensure!(roots.len() <= budget.max_rows && terminals.len() <= budget.max_rows && residuals.len() <= budget.max_rows,
			"expiry source freeze exceeds row budget");
		self.lock_htlc_settlements().await?;
		self.lock_expiry_carriers().await?;
		let before = self.backing_snapshot(roots, budget).await?;
		let keys = before.regular.iter().map(|f| f.state.vtxo_id.to_string())
			.chain(before.pending_boards.iter().map(|f| f.vtxo.id().to_string())).collect::<Vec<_>>();
		self.lock_expiry_inputs(&keys).await?;
		let snapshot = self.backing_snapshot(roots, budget).await?;
		ensure!(snapshot.regular.iter().filter(|f| roots.iter().any(|r| r.id() == f.state.vtxo_id))
			.all(|f| f.onchain_spent_txid.is_none()), "expiry backing already consumed");
		let funding = context.funding.values().cloned().collect::<Vec<_>>();
		let partition = snapshot.check_partition(terminals, &funding, residuals, budget)?;
		let covered = terminals.iter().map(|v| v.id()).collect::<Vec<_>>();
		let decisions = self.expiry_ownership_proposal(&snapshot, &covered, context, send_evidence).await?;
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
