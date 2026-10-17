//! Short durable allocation commits for independently ready, same-expiry outputs.

use std::collections::BTreeMap;

use anyhow::Context;
use bitcoin::{Amount, Network, ScriptBuf, Transaction, TxIn, TxOut, Sequence, Witness};
use bitcoin::hashes::Hash;
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::PublicKey;
use chrono::{DateTime, Local};

use ark::{ServerVtxo, VtxoId};

use super::expiry_chain::ExpiryViewToken;
use super::expiry_inventory::{BackingPartitionResidual, BackingSnapshot, BackingSnapshotLimits};
use super::expiry_ownership::{OwnershipDisposition, OwnershipEvidence, OwnershipProposal};
use super::{SpendState, Tx};
use crate::watchman::settlement::{allocate, CustomerLeaf};

pub struct VerifiedPayee {
	pub mailbox: PublicKey,
	pub child_index: u32,
	pub script: ScriptBuf,
}

pub struct ReadyCoin {
	pub ownership: OwnershipProposal,
	pub payee: Option<VerifiedPayee>,
	/// Retained attribution evidence; a negative search alone never authorizes
	/// taking customer funds or committing a payout without a verified payee.
	pub no_owner_registry_version: Option<i64>,
}

pub struct ReadyOutput {
	root: ServerVtxo,
	coins: Vec<ReadyCoin>,
	versions: Vec<(VtxoId, DateTime<Local>, SpendState)>,
	snapshot: BackingSnapshot,
	funding: Vec<Transaction>,
}

impl ReadyOutput {
	/// Conservatively preflight complete groups before their short decision
	/// commit. Consensus serialization supplies the real CompactSize lengths;
	/// the largest actual policy witness bounds the later signer's choice.
	pub fn group_fits(outputs: &[&Self]) -> anyhow::Result<bool> {
		let leaves = outputs.iter().flat_map(|o| &o.coins).filter_map(|c| c.payee.as_ref().map(|p| CustomerLeaf {
			id: c.ownership.id, amount: c.ownership.gross, script: p.script.clone(),
		})).collect();
		let plan = allocate(leaves)?;
		let mut payouts = plan.recipients.into_iter().map(|r| r.output).collect::<Vec<_>>();
		// Every address accepted for the operator drain has a script no
		// larger than native P2WSH/P2TR. Count both drain and P2A even when
		// a sub-dust operator remainder will use only the latter.
		payouts.push(TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_p2wsh(&bitcoin::WScriptHash::all_zeros()) });
		payouts.push(bitcoin_ext::fee::fee_anchor());
		let tx = Transaction {
			version: bitcoin::transaction::Version::non_standard(3), lock_time: bitcoin::absolute::LockTime::ZERO,
			input: outputs.iter().map(|o| TxIn { previous_output: o.root.point(),script_sig: ScriptBuf::new(),sequence: Sequence::ZERO,witness: Witness::new() }).collect(),
			output: payouts,
		};
		let mut weight = tx.weight().to_wu().checked_add(2).context("expiry group weight overflow")?;
		for output in outputs {
			let root = &output.root;
			let witness = root.policy().clauses(root.exit_delta(),root.expiry_height(),root.server_pubkey())
				.iter().map(|clause| clause.witness_size(root)).max().unwrap_or(66).max(66);
			weight = weight.checked_add(u64::try_from(witness)?).context("expiry group witness overflow")?;
		}
		Ok(weight.div_ceil(4) <= 10_000)
	}

	pub fn root(&self) -> &ServerVtxo { &self.root }

	/// No geometric cut supplied by an external caller can bypass the actual
	/// partition proof. Signature and settlement evidence remain W5's job.
	pub fn prove(
		snapshot: BackingSnapshot,
		coins: Vec<ReadyCoin>,
		funding: &[Transaction],
		residuals: &[BackingPartitionResidual],
		limits: BackingSnapshotLimits,
	) -> anyhow::Result<Self> {
		ensure!(snapshot.roots.len() == 1, "readiness is isolated per backing output");
		ensure!(snapshot.regular.iter().all(|fact| fact.onchain_spent_height.is_none() && fact.onchain_spent_txid.is_none()),
			"ready backing contains a spent or reorg-lagged source; rebuild before allocation");
		let mut terminals = Vec::with_capacity(coins.len());
		let mut versions = snapshot.regular.iter().map(|fact|
			(fact.state.vtxo_id, fact.state.updated_at, fact.state.spend_state)
		).collect::<Vec<_>>();
		for coin in &coins {
			ensure!(!matches!(coin.ownership.evidence, Some(OwnershipEvidence::HtlcChain { .. })),
				"chain-resolved terminal is not an unspent expiry payout source");
			match (&coin.ownership.disposition, &coin.payee) {
				(OwnershipDisposition::Customer { .. }, Some(_)) => {},
				(OwnershipDisposition::Operator, None) => {},
				_ => bail!("ready ownership lacks a verified customer payee or has an invalid operator payee"),
			}
			let fact = snapshot.regular.iter().find(|f| f.state.vtxo_id == coin.ownership.id)
				.context("ready coin is absent from source inventory")?;
			ensure!(coin.ownership.gross == fact.state.vtxo.amount()
				&& coin.ownership.updated_at == fact.state.updated_at && coin.ownership.spend_state == fact.state.spend_state,
				"ready coin differs from its source row");
			terminals.push(fact.full_vtxo.clone().context("ready coin lacks its actual genesis")?);
		}
		snapshot.check_partition(&terminals, funding, residuals, limits)?;
		let root = snapshot.roots[0].clone();
		ensure!(snapshot.regular.iter().any(|f| f.state.vtxo_id == root.id()),
			"readiness inventory omits its physical backing root");
		versions.sort_by_key(|v| v.0);
		versions.dedup_by_key(|v| v.0);
		Ok(Self { root, coins, versions, snapshot, funding: funding.to_vec() })
	}
}

pub struct CommittedExpiryGroup {
	pub id: i64,
	pub inputs: Vec<VtxoId>,
	pub dropped: Vec<VtxoId>,
}

fn evidence_json(evidence: &OwnershipEvidence, preimage: bool) -> serde_json::Value {
	let mut value = match evidence {
		OwnershipEvidence::IrreversibleEntitlement { entitlement_id } =>
			serde_json::json!({"kind":"irreversible_entitlement","entitlement_id":entitlement_id}),
		OwnershipEvidence::Pubkey => serde_json::json!({"kind":"pubkey"}),
		OwnershipEvidence::InputOwner => serde_json::json!({"kind":"input_owner"}),
		OwnershipEvidence::LightningReceiveClaim { source, subscription_id, payment_hash, output } =>
			serde_json::json!({"kind":"lightning_receive_claim","source":source.to_string(),
				"subscription_id":subscription_id,"payment_hash":payment_hash.to_string(),"output":output.to_string()}),
		OwnershipEvidence::Pool => serde_json::json!({"kind":"pool"}),
		OwnershipEvidence::Padding => serde_json::json!({"kind":"padding"}),
		OwnershipEvidence::Forfeit => serde_json::json!({"kind":"forfeit"}),
		OwnershipEvidence::Offboard(txid) => serde_json::json!({"kind":"offboard","txid":txid.to_string()}),
		OwnershipEvidence::Exchange { participation_id, unlock_hash, forfeited } =>
			serde_json::json!({"kind":"exchange","participation_id":participation_id,"unlock_hash":unlock_hash,"forfeited":forfeited}),
		OwnershipEvidence::HtlcStoredPreimage { payment_hash } =>
			serde_json::json!({"kind":"htlc_stored_preimage","payment_hash":payment_hash.to_string()}),
		OwnershipEvidence::HtlcSourceDischarged { payment_hash } =>
			serde_json::json!({"kind":"htlc_source_discharged","payment_hash":payment_hash.to_string()}),
		OwnershipEvidence::HtlcExternalFailed { payment_hash, htlc_expiry } =>
			serde_json::json!({"kind":"htlc_external_failed","payment_hash":payment_hash.to_string(),"htlc_expiry":htlc_expiry.to_u32()}),
		OwnershipEvidence::HtlcUnattemptedExpired { payment_hash, htlc_expiry } =>
			serde_json::json!({"kind":"htlc_unattempted_expired","payment_hash":payment_hash.to_string(),"htlc_expiry":htlc_expiry.to_u32()}),
		OwnershipEvidence::HtlcOffchain { payment_hash, resolution } =>
			serde_json::json!({"kind":"htlc_offchain","payment_hash":payment_hash.to_string(),"resolution":resolution.as_str()}),
		OwnershipEvidence::HtlcChain { payment_hash, resolution, block } =>
			serde_json::json!({"kind":"htlc_chain","payment_hash":payment_hash.to_string(),"resolution":resolution.as_str(),"height":block.height.to_u32(),"hash":block.hash.to_string()}),
	};
	value["stored_preimage"] = serde_json::json!(preimage);
	value
}

impl Tx<'_> {
	/// Caller groups only ready outputs at one expiry height and preflights their
	/// size. A changed output is dropped independently, allowing the rest to pay.
	pub async fn commit_expiry_group(
		&self,
		view: ExpiryViewToken,
		outputs: Vec<ReadyOutput>,
		network: Network,
	) -> anyhow::Result<Option<CommittedExpiryGroup>> {
		if outputs.is_empty() { return Ok(None); }
		ensure!(ReadyOutput::group_fits(&outputs.iter().collect::<Vec<_>>())?,
			"expiry group exceeds version-3 sweep size; regroup or split before deciding");
		self.lock_valid_expiry_view(view).await?;
		self.execute("SELECT pg_advisory_xact_lock_shared($1)", &[&3_i64]).await?;
		let expiry = outputs[0].root.expiry_height();
		ensure!(outputs.iter().all(|o| o.root.expiry_height() == expiry), "expiry group mixes expiry heights");
		ensure!(expiry <= view.captaind.height && expiry <= view.watchmand.height, "expiry group is not mature in the completed view");
		let mut ids = outputs.iter().flat_map(|o| o.versions.iter().map(|v| v.0.to_string())).collect::<Vec<_>>();
		let participations = outputs.iter().flat_map(|o| o.coins.iter().flat_map(|c| c.ownership.cancellations.iter().copied())).collect::<Vec<_>>();
		if !participations.is_empty() {
			ids.extend(self.query("SELECT vtxo_id FROM round_part_input WHERE participation_id=ANY($1)", &[&participations]).await?
				.into_iter().map(|r| r.get::<_, String>("vtxo_id")));
		}
		ids.sort(); ids.dedup();
		let rows = self.query("SELECT vtxo_id,updated_at,spend_state::TEXT,onchain_spent_height IS NOT NULL OR onchain_spent_txid IS NOT NULL AS chain_spent FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE", &[&ids]).await?;
		self.lock_htlc_settlements().await?;
		let actual = rows.into_iter().map(|r| (r.get::<_, String>("vtxo_id"), (r.get::<_, DateTime<Local>>("updated_at"), r.get::<_, String>("spend_state"), r.get::<_, bool>("chain_spent")))).collect::<BTreeMap<_, _>>();
		let mut accepted = Vec::new();
		let mut existing_entitlements = BTreeMap::new();
		let mut dropped = Vec::new();
		for output in outputs {
			let mut valid = output.versions.iter().all(|(id,time,state)| actual.get(&id.to_string()) == Some(&(*time,state.as_str().to_owned(),false)));
			for coin in &output.coins {
				let fact = output.snapshot.regular.iter().find(|f| f.state.vtxo_id == coin.ownership.id)
					.context("ready coin source disappeared")?;
				let funding = output.funding.iter().find(|t| t.compute_txid() == fact.state.vtxo.chain_anchor().txid)
					.context("ready coin funding disappeared")?;
				let current = self.propose_expiry_ownership(fact, funding, view.captaind.height, Some(view.watchmand.height)).await?;
				valid &= current.disposition == coin.ownership.disposition && current.evidence == coin.ownership.evidence
					&& current.has_stored_preimage == coin.ownership.has_stored_preimage
					&& current.cancellations == coin.ownership.cancellations;
				valid &= self.revalidate_expiry_payee(coin, network).await?;
				if let Some(row) = self.query_opt("SELECT e.id,e.gross_sat, EXISTS(SELECT 1 FROM expiry_allocation_coin c JOIN expiry_allocation a ON a.id=c.allocation_id WHERE c.entitlement_id=e.id AND ((c.active AND a.status<>'conflicted') OR c.paying_txid IS NOT NULL)) AS reserved, EXISTS(SELECT 1 FROM expiry_terminal_claim p WHERE p.entitlement_id=e.id) AS claimed FROM expiry_decided_coin d JOIN expiry_entitlement e ON e.id=d.entitlement_id WHERE d.vtxo_id=$1", &[&coin.ownership.id.to_string()]).await? {
					valid &= !row.get::<_, bool>("reserved") && !row.get::<_, bool>("claimed")
						&& u64::try_from(row.get::<_, i64>("gross_sat"))? == coin.ownership.gross.to_sat();
					existing_entitlements.insert(coin.ownership.id,row.get::<_, i64>("id"));
				}
			}
			// A never-attempted incoming grant did not fund an outgoing receive.
			// Its current no-attempt/no-outgoing predicates were rechecked above
			// under source locks and the settlement barrier. Never cancel an
			// unrelated external-origin receive merely because hashes match.

			if valid {
				let cancellations = output.coins.iter().flat_map(|c| c.ownership.cancellations.iter().copied()).collect::<Vec<_>>();
				valid = self.cancel_expiry_participations(&cancellations).await?;
			}
			if valid { accepted.push(output); } else { dropped.push(output.root.id()); }
		}
		if accepted.is_empty() { return Ok(None); }
		let leaves = accepted.iter().flat_map(|o| &o.coins).filter_map(|c| c.payee.as_ref().map(|payee| CustomerLeaf {
			id: c.ownership.id, script: payee.script.clone(), amount: c.ownership.gross,
		})).collect();
		let plan = allocate(leaves)?;
		let policy = plan.forfeits.iter().chain(plan.recipients.iter().flat_map(|r| &r.leaves))
			.map(|a| (a.id,a)).collect::<BTreeMap<_, _>>();
		let id: i64 = self.query_one("INSERT INTO expiry_allocation(expiry_height,generation,status) VALUES($1,$2,'pending') RETURNING id",
			&[&i32::try_from(expiry.to_u32())?, &view.generation]).await?.get(0);
		for (index, recipient) in plan.recipients.iter().enumerate() {
			self.execute("INSERT INTO expiry_allocation_payout(allocation_id,output_index,spk,amount_sat) VALUES($1,$2,$3,$4)",
				&[&id,&i32::try_from(index)?,&recipient.output.script_pubkey.as_bytes().to_lower_hex_string(),&i64::try_from(recipient.output.value.to_sat())?]).await?;
		}
		for output in &accepted {
			self.execute("INSERT INTO expiry_allocation_input(allocation_id,vtxo_id) VALUES($1,$2)", &[&id,&output.root.id().to_string()]).await?;
			for coin in &output.coins {
				let proposal = &coin.ownership;
				let evidence = proposal.evidence.as_ref().context("ready ownership lacks evidence")?;
				let observed = evidence_json(evidence, proposal.has_stored_preimage);
				let mut stable = if matches!(evidence, OwnershipEvidence::HtlcChain { .. }) {
					serde_json::json!({"stored_preimage": proposal.has_stored_preimage})
				} else { observed.clone() };
				if !matches!(evidence, OwnershipEvidence::HtlcChain { .. }) {
					match &proposal.disposition {
						OwnershipDisposition::Customer { user_pubkey,.. } => stable["customer_key"] = serde_json::json!(user_pubkey.to_string()),
						OwnershipDisposition::Operator => stable["operator"] = serde_json::json!(true),
						OwnershipDisposition::Unresolved(_) => bail!("readiness contains unresolved ownership"),
					}
				}
				let entitlement = if let Some(existing) = existing_entitlements.get(&proposal.id) {
					self.execute("UPDATE expiry_allocation_coin SET active=FALSE WHERE entitlement_id=$1 AND active", &[existing]).await?;
					*existing
				} else {
					let entitlement: i64 = self.query_one("INSERT INTO expiry_entitlement(vtxo_id,gross_sat,irreversible_facts) VALUES($1,$2,$3::TEXT::JSONB) RETURNING id",
						&[&proposal.id.to_string(),&i64::try_from(proposal.gross.to_sat())?,&stable.to_string()]).await?.get(0);
					self.execute("INSERT INTO expiry_decided_coin(vtxo_id,entitlement_id) VALUES($1,$2)", &[&proposal.id.to_string(),&entitlement]).await?;
					entitlement
				};
				let allocation = policy.get(&proposal.id);
				let fee = allocation.map_or(Amount::ZERO, |a| a.fee).to_sat();
				let payout = allocation.map_or(Amount::ZERO, |a| a.payout).to_sat();
				let forfeited = allocation.map_or(Amount::ZERO, |a| a.forfeited).to_sat();
				let (mailbox, user_key, script) = match (&proposal.disposition,&coin.payee) {
					(OwnershipDisposition::Customer { user_pubkey, .. },Some(payee)) => (Some(payee.mailbox.to_string()),Some(user_pubkey.to_string()),Some(payee.script.as_bytes().to_lower_hex_string())),
					_ => (None,None,None),
				};
				let disposition = match (&proposal.disposition, &coin.payee) {
					(OwnershipDisposition::Customer { .. }, Some(_)) => "customer",
					(OwnershipDisposition::Operator, None) => "operator",
					_ => bail!("allocation ownership lacks its verified payout destination"),
				};
				self.execute("INSERT INTO expiry_allocation_coin(allocation_id,entitlement_id,owner_mailbox,user_pubkey,spk,disposition,gross_sat,fee_sat,payout_sat,forfeited_sat,evidence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::TEXT::JSONB)",
					&[&id,&entitlement,&mailbox,&user_key,&script,&disposition,&i64::try_from(proposal.gross.to_sat())?,&i64::try_from(fee)?,&i64::try_from(payout)?,&i64::try_from(forfeited)?,&observed.to_string()]).await?;
				self.execute("UPDATE vtxo SET spend_state='spent',updated_at=NOW() WHERE vtxo_id=$1", &[&proposal.id.to_string()]).await?;
			}
		}
		Ok(Some(CommittedExpiryGroup { id, inputs: accepted.iter().map(|o| o.root.id()).collect(), dropped }))
	}

	async fn revalidate_expiry_payee(&self, coin: &ReadyCoin, network: Network) -> anyhow::Result<bool> {
		let OwnershipDisposition::Customer { user_pubkey, .. } = &coin.ownership.disposition else { return Ok(coin.payee.is_none()); };
		let current = self.expiry_owner(*user_pubkey, network).await?;
		Ok(match (&coin.payee,current) {
			(Some(payee),super::expiry_attribution::Attribution::Owned { mailbox_pk,child_index,spk }) =>
				mailbox_pk == payee.mailbox && child_index == payee.child_index && spk == payee.script,
			_ => false,
		})
	}
}
