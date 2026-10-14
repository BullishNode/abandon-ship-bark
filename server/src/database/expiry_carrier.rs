//! Durable handoff of an already authorized direct-settlement closure.
//!
//! This does not infer ownership, freeze new entitlements, observe the chain or
//! broadcast. The ownership resolver must first authorize the complete physical
//! retirement set, retaining its logical dependency and path evidence. Carrier
//! versions preserve that set and every customer amount. Conditional proposals
//! invalidated by confirmed foreign spends require separate reconciliation.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, FeeRate, Network, ScriptBuf};
use bitcoin::consensus::serialize;
use bitcoin::secp256k1::PublicKey;
use tokio_postgres::Row;

use ark::{ProtocolEncoding, ServerVtxo, VtxoId};

use crate::database::carrier_snapshot::BackingSnapshotLimits;
use crate::wallet::{PersistedWallet, WalletKind};
use crate::watchman::carrier::{validate_fee_policy, CarrierLimits, DirectCarrier};
use crate::watchman::settlement::{allocate, CustomerLeaf, LeafAllocation};

use super::{AdvisoryLock, Tx};

struct Allocation {
	id: VtxoId,
	owner: Option<PublicKey>,
	script: Option<ScriptBuf>,
	disposition: String,
	amounts: LeafAllocation,
	retirement: BTreeSet<VtxoId>,
}

impl Allocation {
	fn from_row(row: &Row) -> anyhow::Result<Self> {
		let id = row.get::<_, &str>("id").parse()?;
		Ok(Self {
			id,
			owner: row.get::<_, Option<Vec<u8>>>("owner_pubkey")
				.map(|v| PublicKey::from_slice(&v)).transpose()?,
			script: row.get::<_, Option<Vec<u8>>>("spk").map(ScriptBuf::from),
			disposition: row.get("disposition"),
			amounts: LeafAllocation { id, gross: amount(row, "gross_sat")?, fee: amount(row, "fee_sat")?,
				payout: amount(row, "payout_sat")?, forfeited: amount(row, "forfeited_sat")? },
			retirement: row.get::<_, Vec<String>>("retirement_ids").into_iter()
				.map(|id| -> anyhow::Result<_> { Ok(id.parse()?) }).collect::<anyhow::Result<_>>()?,
		})
	}
}

fn amount(row: &Row, field: &str) -> anyhow::Result<Amount> {
	Ok(Amount::from_sat(u64::try_from(row.get::<_, i64>(field))?))
}

fn total(amounts: impl IntoIterator<Item = Amount>) -> anyhow::Result<Amount> {
	let sum = amounts.into_iter().try_fold(Amount::ZERO, |sum, v|
		sum.checked_add(v).context("direct handoff amount overflow"))?;
	ensure!(sum <= Amount::MAX_MONEY, "direct handoff amount exceeds Bitcoin money range");
	Ok(sum)
}

impl Tx<'_> {
	/// Freeze writers and carrier publication use this same lock before the
	/// ordered pending-board/VTXO locks. No partial append to a freeze set may
	/// race publication. This is not a replacement for payment/round guards.
	pub(crate) async fn lock_expiry_carriers(&self) -> anyhow::Result<()> {
		self.query_one("SELECT pg_advisory_xact_lock($1)",
			&[&(AdvisoryLock::ExpiryCarrierWrite as i64)]).await?;
		Ok(())
	}

	/// Store signed bytes, exact output mappings and the staged watchman wallet
	/// graph in this transaction. Caller holds the exclusive wallet guard and
	/// the carrier's fee reservation, and has applied this exact transaction to
	/// the wallet with commit_tx. After a known commit it may discard the stage.
	/// An ambiguous COMMIT must retain those guards and be probed before any
	/// further construction or broadcast; a body/commit error is not permission
	/// to continue using a wallet whose staged transaction is uncommitted.
	///
	/// Allocation authorization, logical dependencies, live backing eligibility,
	/// supported recovery witness and source arbitration remain caller duties.
	/// Retirement IDs here are the complete physical inputs of this frozen set;
	/// this initial gate refuses regrouping or partial progression across versions.
	pub async fn store_frozen_expiry_carrier(
		&self, freeze_set: &str, expected_ids: &[VtxoId], backing: &[ServerVtxo],
		carrier: &DirectCarrier, wallet: &PersistedWallet, operator_script: &ScriptBuf,
		network: Network, fee_rate: FeeRate, policy: CarrierLimits, budget: BackingSnapshotLimits,
	) -> anyhow::Result<()> {
		ensure!(wallet.kind() == WalletKind::Watchman && wallet.network() == network,
			"direct handoff requires the correct watchman wallet");
		ensure!(!freeze_set.is_empty() && !expected_ids.is_empty() && !backing.is_empty(), "empty direct handoff");
		ensure!(budget.max_rows > 0 && budget.max_serialized_bytes > 0, "invalid direct handoff budget");
		ensure!(expected_ids.len() <= budget.max_rows && backing.len() <= budget.max_rows,
			"direct handoff exceeds row budget");
		let expected = expected_ids.iter().copied().collect::<BTreeSet<_>>();
		ensure!(expected.len() == expected_ids.len(), "duplicate direct allocation ID");
		let roots = backing.iter().map(|v| (v.id(), v)).collect::<BTreeMap<_, _>>();
		ensure!(roots.len() == backing.len() && carrier.backing_ids.iter().copied().collect::<BTreeSet<_>>() ==
			roots.keys().copied().collect() && carrier.backing_ids.len() == roots.len(), "direct backing differs from authorization");
		let bytes = serialize(&carrier.tx);
		let txid = carrier.tx.compute_txid().to_string();
		ensure!(bytes.len() <= budget.max_serialized_bytes, "direct signed bytes exceed budget");
		self.lock_expiry_carriers().await?;
		let keys = expected.iter().chain(roots.keys()).map(ToString::to_string).collect::<Vec<_>>();
		self.lock_expiry_inputs(&keys).await?;
		for (id, root) in &roots {
			let row = self.query_opt("SELECT vtxo,onchain_spent_txid FROM vtxo WHERE vtxo_id=$1", &[&id.to_string()]).await?
				.context("direct backing source row is missing")?;
			let stored: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo"))?;
			ensure!(stored.id() == *id && stored.txout() == root.txout() && stored.chain_anchor() == root.chain_anchor() &&
				stored.expiry_height() == root.expiry_height() && stored.policy() == root.policy(), "direct backing metadata differs from source");
			ensure!(row.get::<_, Option<String>>("onchain_spent_txid").is_none_or(|spender| spender == txid),
				"direct backing was consumed by another transaction");
		}
		let size = self.query_one("SELECT count(*)::bigint AS rows,
			COALESCE(sum(octet_length(id) + COALESCE(octet_length(spk),0) +
				octet_length(source_evidence::text) + octet_length(retirement_ids::text)),0)::bigint AS bytes
			FROM expiry_allocation WHERE freeze_set=$1", &[&freeze_set]).await?;
		ensure!(usize::try_from(size.get::<_, i64>("rows"))? <= budget.max_rows &&
			usize::try_from(size.get::<_, i64>("bytes"))?.checked_add(bytes.len())
				.is_some_and(|n| n <= budget.max_serialized_bytes), "complete direct allocation exceeds budget");
		let rows = self.query("SELECT id, owner_pubkey, spk, disposition, gross_sat, fee_sat,
			payout_sat, forfeited_sat, retirement_ids FROM expiry_allocation
			WHERE freeze_set=$1 ORDER BY id FOR UPDATE", &[&freeze_set]).await?;
		let allocations = rows.iter().map(Allocation::from_row).collect::<anyhow::Result<Vec<_>>>()?;
		let by_id = allocations.iter().map(|a| (a.id, a)).collect::<BTreeMap<_, _>>();
		ensure!(allocations.iter().map(|a| a.id).collect::<BTreeSet<_>>() == expected,
			"complete frozen allocation set changed");
		let root_ids = roots.keys().copied().collect::<BTreeSet<_>>();
		ensure!(allocations.iter().all(|a| !a.retirement.is_empty() && a.retirement.is_subset(&root_ids)) &&
			allocations.iter().flat_map(|a| a.retirement.iter().copied()).collect::<BTreeSet<_>>() == root_ids,
			"direct carrier does not retire the complete frozen physical closure");
		let gross = total(allocations.iter().map(|a| a.amounts.gross))?;
		ensure!(gross == total(backing.iter().map(|v| v.amount()))?, "frozen gross does not cover exact backing");
		// Reauthenticate each immutable record and exact account child. A stored
		// script or mailbox identifier alone cannot authorize a payee.
		let owners = allocations.iter().filter_map(|a| a.owner).collect::<Vec<_>>();
		let scripts = self.authenticated_fallback_scripts(&owners, network).await?;
		let mut leaves = Vec::new();
		for allocation in &allocations {
			if allocation.disposition == "operator" {
				ensure!(allocation.owner.is_none() && allocation.script.is_none() && allocation.amounts.fee == Amount::ZERO &&
					allocation.amounts.payout == Amount::ZERO && allocation.amounts.forfeited == Amount::ZERO,
					"invalid frozen operator allocation");
				continue;
			}
			let owner = allocation.owner.context("customer allocation has no owner")?;
			let script = allocation.script.as_ref().context("customer allocation has no script")?;
			ensure!(scripts.get(&owner) == Some(script), "frozen destination differs from authenticated record");
			leaves.push(CustomerLeaf { id: allocation.id, script: script.clone(), amount: allocation.amounts.gross });
		}
		let plan = allocate(leaves)?;
		ensure!(plan.recipients.len() == carrier.outputs.recipients.len(), "direct recipient count differs from policy");
		let mut mappings = BTreeMap::<VtxoId, Option<i32>>::new();
		for (recipient, vout) in plan.recipients.iter().zip(&carrier.outputs.recipients) {
			ensure!(carrier.tx.output.get(*vout as usize) == Some(&recipient.output), "direct carrier changes an authorized recipient");
			for leaf in &recipient.leaves {
				let allocation = by_id.get(&leaf.id).context("missing direct recipient allocation")?;
				ensure!(allocation.disposition == "pay" && allocation.amounts == *leaf, "direct fee or customer amount changed");
				mappings.insert(leaf.id, Some(i32::try_from(*vout)?));
			}
		}
		for leaf in &plan.forfeits {
			let allocation = by_id.get(&leaf.id).context("missing direct forfeit allocation")?;
			ensure!(allocation.disposition == "forfeit" && allocation.amounts == *leaf, "direct dust forfeiture differs from policy");
			mappings.insert(leaf.id, None);
		}
		for allocation in allocations.iter().filter(|a| a.disposition == "operator") { mappings.insert(allocation.id, None); }
		ensure!(mappings.keys().copied().collect::<BTreeSet<_>>() == expected, "unmapped direct allocation");
		let mut roles = carrier.outputs.recipients.clone();
		roles.extend(carrier.outputs.operator); roles.extend(carrier.outputs.fee_wallet_change);
		ensure!(roles.len() == carrier.tx.output.len() && roles.iter().copied().collect::<BTreeSet<_>>() ==
			(0..carrier.tx.output.len()).map(u32::try_from).collect::<Result<BTreeSet<_>, _>>()?, "direct output roles overlap or omit outputs");
		let role = |vout: Option<u32>| -> anyhow::Result<Amount> {
			vout.map(|v| carrier.tx.output.get(v as usize).map(|o| o.value).context("direct role is outside transaction"))
				.transpose().map(|v| v.unwrap_or(Amount::ZERO))
		};
		if let Some(vout) = carrier.outputs.operator {
			ensure!(carrier.tx.output[vout as usize].script_pubkey == *operator_script, "direct operator script changed");
		}
		if let Some(vout) = carrier.outputs.fee_wallet_change {
			ensure!(wallet.is_mine(carrier.tx.output[vout as usize].script_pubkey.clone()), "direct fee change is not wallet owned");
		}
		ensure!(role(carrier.outputs.operator)? == carrier.operator_surplus, "direct operator amount mismatch");
		let fee_points = carrier.fee_inputs.iter().map(|i| i.outpoint).collect::<BTreeSet<_>>();
		ensure!(fee_points.len() == carrier.fee_inputs.len() && fee_points.is_disjoint(&roots.keys().map(VtxoId::to_point).collect()),
			"duplicate or overlapping direct fee inputs");
		let points = roots.keys().map(VtxoId::to_point).chain(fee_points.iter().copied()).collect::<BTreeSet<_>>();
		ensure!(points.len() == carrier.tx.input.len() && carrier.tx.input.iter().map(|i| i.previous_output).collect::<BTreeSet<_>>() == points,
			"direct carrier input set differs from backing and fee reservation");
		ensure!(carrier.tx.input.iter().all(|i| !i.witness.is_empty()), "direct carrier is unsigned");
		for input in &carrier.fee_inputs {
			let funding = wallet.tx_graph().get_tx(input.outpoint.txid).context("direct fee input is absent from wallet graph")?;
			ensure!(funding.output.get(input.outpoint.vout as usize) == Some(&input.txout) && wallet.is_mine(input.txout.script_pubkey.clone()),
				"direct fee input metadata is not wallet owned");
		}
		let fee_input_amount = total(carrier.fee_inputs.iter().map(|i| i.txout.value))?;
		let subsidy = fee_input_amount
			.checked_sub(role(carrier.outputs.fee_wallet_change)?).context("direct fee change exceeds funding")?;
		let inputs = total([gross, fee_input_amount])?;
		let outputs = total(carrier.tx.output.iter().map(|o| o.value))?;
		let actual_fee = inputs.checked_sub(outputs).context("direct outputs exceed inputs")?;
		ensure!(subsidy == carrier.funding_subsidy && actual_fee == carrier.miner_fee && carrier.signed_weight == carrier.tx.weight(),
			"direct miner fee, subsidy or signed weight mismatch");
		ensure!(fee_rate > FeeRate::ZERO && fee_rate <= policy.max_fee_rate &&
			policy.max_weight.to_wu() > 0 && policy.max_weight.to_wu() <= 400_000, "invalid direct fee policy");
		ensure!(actual_fee >= validate_fee_policy(&carrier.tx, actual_fee, fee_rate, policy)?, "direct carrier is underpriced");
		ensure!(fee_points.is_empty() || wallet.tx_graph().get_tx(carrier.tx.compute_txid()).is_some_and(|tx| *tx == carrier.tx),
			"direct carrier has not been staged in its owning wallet");
		let fee_inputs = serde_json::Value::Array(carrier.fee_inputs.iter().map(|i| serde_json::json!({
			"outpoint": i.outpoint.to_string(), "value_sat": i.txout.value.to_sat(),
			"script_pubkey": i.txout.script_pubkey.to_hex_string(),
		})).collect());
		let fee_json = serde_json::to_string(&fee_inputs)?;
		ensure!(bytes.len().checked_add(serde_json::to_vec(&fee_inputs)?.len()).is_some_and(|n| n <= budget.max_serialized_bytes),
			"direct fee reservation metadata exceeds budget");
		let backing_ids = root_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let fee_ids = fee_points.iter().map(ToString::to_string).collect::<Vec<_>>();
		// Another economic closure cannot spend these physical or fee inputs. RBF
		// within this exact set retains every authorized mapping and signed version.
		let conflicts = self.query("SELECT DISTINCT a.freeze_set, c.backing_ids FROM expiry_carrier c
			JOIN expiry_carrier_allocation m ON m.txid=c.txid JOIN expiry_allocation a ON a.id=m.allocation_id
			WHERE c.backing_ids && $1 OR EXISTS (SELECT 1 FROM jsonb_array_elements(c.fee_inputs) f WHERE f->>'outpoint'=ANY($2))",
			&[&backing_ids, &fee_ids]).await?;
		ensure!(conflicts.iter().all(|r| r.get::<_, &str>("freeze_set") == freeze_set &&
			r.get::<_, Vec<String>>("backing_ids") == backing_ids), "direct inputs already reserved by another closure");
		let inserted = self.execute("INSERT INTO expiry_carrier (txid,signed_tx,backing_ids,fee_inputs,miner_fee_sat,funding_subsidy_sat)
			VALUES ($1,$2,$3,$4::text::jsonb,$5,$6) ON CONFLICT (txid) DO NOTHING", &[&txid, &bytes, &backing_ids, &fee_json,
			&i64::try_from(actual_fee.to_sat())?, &i64::try_from(subsidy.to_sat())?]).await?;
		let stored = self.query_one("SELECT signed_tx,backing_ids,fee_inputs::text AS fee_inputs,miner_fee_sat,funding_subsidy_sat
			FROM expiry_carrier WHERE txid=$1 FOR UPDATE", &[&txid]).await?;
		ensure!(stored.get::<_, Vec<u8>>("signed_tx") == bytes && stored.get::<_, Vec<String>>("backing_ids") == backing_ids &&
			serde_json::from_str::<serde_json::Value>(stored.get("fee_inputs"))? == fee_inputs && amount(&stored,"miner_fee_sat")? == actual_fee &&
			amount(&stored,"funding_subsidy_sat")? == subsidy, "stored carrier differs from signed handoff");
		for (id, vout) in &mappings {
			self.execute("INSERT INTO expiry_carrier_allocation (txid,allocation_id,vout) VALUES ($1,$2,$3)
				ON CONFLICT (txid,allocation_id) DO NOTHING", &[&txid, &id.to_string(), vout]).await?;
		}
		let stored_mappings = self.query("SELECT allocation_id,vout FROM expiry_carrier_allocation WHERE txid=$1 ORDER BY allocation_id", &[&txid]).await?
			.into_iter().map(|row| -> anyhow::Result<_> { Ok((row.get::<_, &str>("allocation_id").parse()?, row.get::<_, Option<i32>>("vout"))) })
			.collect::<anyhow::Result<BTreeMap<VtxoId, Option<i32>>>>()?;
		ensure!(stored_mappings == mappings, "stored direct carrier mappings differ from complete handoff");
		if inserted > 0 {
			ensure!(fee_points.is_empty() || wallet.staged().is_some_and(|stage| stage.tx_graph.txs.iter().any(|tx| **tx == carrier.tx)),
				"direct signed transaction is absent from wallet stage");
			if let Some(stage) = wallet.staged() {
				let stage_bytes = rmp_serde::to_vec_named(stage)?.len();
				ensure!(usize::try_from(size.get::<_, i64>("bytes"))?.checked_add(bytes.len())
					.and_then(|n| n.checked_add(fee_json.len())).and_then(|n| n.checked_add(stage_bytes))
					.is_some_and(|n| n <= budget.max_serialized_bytes), "complete direct wallet handoff exceeds budget");
				self.store_changeset(WalletKind::Watchman, stage).await?;
			}
		}
		Ok(())
	}
}
