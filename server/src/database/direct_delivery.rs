//! Bounded, complete readback of an already committed direct handoff.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::BlockRef;

use ark::{ProtocolEncoding, ServerVtxo, VtxoId};
use crate::database::carrier_snapshot::BackingSnapshotLimits;
use crate::watchman::settlement::{allocate, CustomerLeaf, LeafAllocation, SettlementPlan};
use super::Db;

pub(crate) struct StoredCarrier {
	pub tx: Transaction,
	pub bytes: Vec<u8>,
	pub generation: i64,
	pub server_pubkey: PublicKey,
	pub fee_inputs: Vec<(OutPoint, TxOut)>,
	pub miner_fee: Amount,
	pub recorded_confirmation: Option<BlockRef>,
	pub allocation_hash: sha256::Hash,
	pub carrier_hash: sha256::Hash,
	pub remaining_rows: usize,
	pub remaining_bytes: usize,
}

// These exact original PostgreSQL JSONB text representations are part of the
// independent witness contract. Do not decode/re-encode them before hashing.
pub(crate) fn allocation_witness_hash(witnesses: &[String]) -> anyhow::Result<sha256::Hash> {
	Ok(sha256::Hash::hash(&serde_json::to_vec(witnesses)?))
}
pub(crate) fn carrier_witness_hash(witness: &str) -> sha256::Hash {
	sha256::Hash::hash(witness.as_bytes())
}

/// Shared native amount policy, not an ownership or source-authentication
/// capability. The caller must independently prove the actual customer leaves.
pub(crate) fn require_direct_allocation_policy(leaves: Vec<CustomerLeaf>,
	amounts: &BTreeMap<VtxoId, (LeafAllocation, String)>,
) -> anyhow::Result<SettlementPlan> {
	ensure!(leaves.iter().map(|leaf| leaf.id).collect::<BTreeSet<_>>() == amounts.iter()
		.filter(|(_,(_,disposition))| disposition != "operator").map(|(id,_)| *id).collect(),
		"direct delivery supplied customer leaf membership differs");
	let plan = allocate(leaves)?;
	for leaf in plan.recipients.iter().flat_map(|r| &r.leaves) {
		ensure!(amounts.get(&leaf.id).is_some_and(|(stored,disposition)| stored == leaf && disposition == "pay"),
			"direct delivery payout allocation differs from policy");
	}
	for leaf in &plan.forfeits {
		ensure!(amounts.get(&leaf.id).is_some_and(|(stored,disposition)| stored == leaf && disposition == "forfeit"),
			"direct delivery forfeiture differs from policy");
	}
	Ok(plan)
}

/// Preserve actual native payout grouping and nullable non-payment mappings.
/// No missing/NULL mapping is replaced with a guessed transaction output.
pub(crate) fn require_direct_output_mappings(tx: &Transaction, plan: &SettlementPlan,
	amounts: &BTreeMap<VtxoId, (LeafAllocation, String)>, ids: &BTreeSet<VtxoId>,
	mapping: &BTreeMap<VtxoId, Option<i32>>,
) -> anyhow::Result<()> {
	ensure!(amounts.keys().copied().collect::<BTreeSet<_>>() == *ids && plan.recipients.iter()
		.all(|recipient| !recipient.leaves.is_empty() && recipient.leaves.iter().all(|leaf| ids.contains(&leaf.id))),
		"direct delivery supplied plan/allocation identities differ");
	ensure!(mapping.keys().copied().collect::<BTreeSet<_>>() == *ids, "direct delivery allocation mappings are incomplete");
	let mut payout_outputs = BTreeSet::new();
	for recipient in &plan.recipients {
		let vout = mapping[&recipient.leaves[0].id].context("direct payout has no output mapping")?;
		ensure!(vout >= 0 && payout_outputs.insert(vout) && tx.output.get(vout as usize) == Some(&recipient.output) &&
			recipient.leaves.iter().all(|a| mapping[&a.id] == Some(vout)), "direct delivery output mapping or amount differs");
	}
	for (id,(_,disposition)) in amounts {
		ensure!(disposition == "pay" || mapping[id].is_none(), "direct non-payment has a customer output mapping");
	}
	Ok(())
}

/// Exact physical input partition only; prevout validity/current ownership and
/// confirmation, fees, reservation history and retirement need separate proofs.
pub(crate) fn require_direct_input_partition(tx: &Transaction, backing: &BTreeSet<VtxoId>,
	fee_inputs: &[(OutPoint, TxOut)],
) -> anyhow::Result<()> {
	let points = backing.iter().map(VtxoId::to_point).chain(fee_inputs.iter().map(|(p,_)| *p)).collect::<BTreeSet<_>>();
	ensure!(points.len() == backing.len()+fee_inputs.len() && points == tx.input.iter().map(|i| i.previous_output).collect() &&
		points.len() == tx.input.len(), "direct delivery reserved inputs differ from signed transaction");
	Ok(())
}

impl Db {
	pub(crate) async fn direct_delivery_generation(&self, freeze_set: &str) -> anyhow::Result<Option<i64>> {
		self.read(async |t| Ok(t.query_one("SELECT max(c.generation) AS generation FROM expiry_carrier c
			JOIN expiry_carrier_allocation m ON m.txid=c.txid JOIN expiry_allocation a ON a.id=m.allocation_id
			WHERE a.freeze_set=$1", &[&freeze_set]).await?.get("generation"))).await
	}

	pub(crate) async fn read_direct_delivery(
		&self, freeze_set: &str, network: Network, limits: BackingSnapshotLimits,
	) -> anyhow::Result<Vec<StoredCarrier>> {
		ensure!(!freeze_set.is_empty() && limits.max_rows > 0 && limits.max_serialized_bytes > 0,
			"invalid direct delivery read budget or freeze set");
		self.read(async |t| {
			// This is the first statement of the fresh transaction. Count/byte
			// preflight and all mappings must observe the same committed snapshot.
			t.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[]).await?;
			let size = t.query_one("SELECT count(*)::bigint AS rows,
				COALESCE(sum(octet_length(id)+COALESCE(octet_length(spk),0)+
					octet_length(source_evidence::text)+octet_length(retirement_ids::text)),0)::bigint AS bytes
				FROM expiry_allocation WHERE freeze_set=$1", &[&freeze_set]).await?;
			let count = usize::try_from(size.get::<_, i64>("rows"))?;
			let allocation_bytes = usize::try_from(size.get::<_, i64>("bytes"))?;
			ensure!(count > 0 && count <= limits.max_rows && allocation_bytes <= limits.max_serialized_bytes,
				"direct delivery allocation is missing or exceeds budget");
			let stats = t.query_one("SELECT count(*)::bigint AS rows,
				COALESCE(sum(octet_length(signed_tx)+octet_length(fee_inputs::text)+octet_length(backing_ids::text)),0)::bigint AS bytes
				FROM expiry_carrier WHERE txid IN (SELECT m.txid FROM expiry_carrier_allocation m
				JOIN expiry_allocation a ON a.id=m.allocation_id WHERE a.freeze_set=$1)", &[&freeze_set]).await?;
			let versions = usize::try_from(stats.get::<_, i64>("rows"))?;
			let carrier_bytes = usize::try_from(stats.get::<_, i64>("bytes"))?;
			ensure!(versions > 0 && versions <= limits.max_rows && allocation_bytes.checked_add(
				carrier_bytes).is_some_and(|n| n <= limits.max_serialized_bytes),
				"direct delivery versions are missing or exceed budget");
			let rows = t.query("SELECT id,owner_pubkey,spk,disposition,gross_sat,fee_sat,payout_sat,
				forfeited_sat,retirement_ids,jsonb_build_object('id',id,'owner',owner_pubkey,'spk',spk,
				'disposition',disposition,'gross',gross_sat,'fee',fee_sat,'payout',payout_sat,
				'forfeited',forfeited_sat,'source',source,'evidence',source_evidence,'retirement',retirement_ids)::text AS witness
				FROM expiry_allocation WHERE freeze_set=$1 ORDER BY id", &[&freeze_set]).await?;
			let allocation_hash = allocation_witness_hash(&rows.iter()
				.map(|r| r.get::<_, String>("witness")).collect::<Vec<_>>())?;
			let mut ids = BTreeSet::new();
			let mut retirement = BTreeSet::new();
			let mut leaves = Vec::new();
			let mut amounts = BTreeMap::new();
			let mut owners = Vec::new();
			for row in &rows {
				let id: VtxoId = row.get::<_, &str>("id").parse()?;
				ids.insert(id);
				retirement.extend(row.get::<_, Vec<String>>("retirement_ids").into_iter()
					.map(|v| v.parse()).collect::<Result<Vec<VtxoId>, _>>()?);
				let amount = |field| -> anyhow::Result<Amount> { Ok(Amount::from_sat(u64::try_from(row.get::<_, i64>(field))?)) };
				let leaf = LeafAllocation { id, gross: amount("gross_sat")?, fee: amount("fee_sat")?,
					payout: amount("payout_sat")?, forfeited: amount("forfeited_sat")? };
				let disposition: &str = row.get("disposition");
				if disposition != "operator" {
					let owner = PublicKey::from_slice(&row.get::<_, Vec<u8>>("owner_pubkey"))?;
					let script = ScriptBuf::from(row.get::<_, Vec<u8>>("spk"));
					owners.push((owner, script.clone()));
					leaves.push(CustomerLeaf { id, script, amount: leaf.gross });
				}
				amounts.insert(id, (leaf, disposition.to_owned()));
			}
			ensure!(!retirement.is_empty() && count.checked_add(versions).and_then(|n| n.checked_add(retirement.len()))
				.is_some_and(|n| n <= limits.max_rows), "complete direct delivery roots exceed row budget");
			let root_ids = retirement.iter().map(ToString::to_string).collect::<Vec<_>>();
			let root_size = t.query_one("SELECT count(*)::bigint AS rows,
				COALESCE(sum(octet_length(vtxo)+octet_length(vtxo_id)+octet_length(server_pubkey)),0)::bigint AS bytes
				FROM vtxo WHERE vtxo_id=ANY($1)", &[&root_ids]).await?;
			ensure!(usize::try_from(root_size.get::<_, i64>("rows"))? == retirement.len() &&
				allocation_bytes.checked_add(carrier_bytes).and_then(|n| n.checked_add(
					usize::try_from(root_size.get::<_, i64>("bytes")).ok()?)).is_some_and(|n| n <= limits.max_serialized_bytes),
				"direct delivery backing is missing or exceeds byte budget");
			let mut server_pubkey = None;
			let remaining_rows = limits.max_rows - count - versions - retirement.len();
			let remaining_bytes = limits.max_serialized_bytes - allocation_bytes - carrier_bytes -
				usize::try_from(root_size.get::<_, i64>("bytes"))?;
			let mut backing_amount = Amount::ZERO;
			for row in t.query("SELECT vtxo_id,vtxo,server_pubkey,amount FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id", &[&root_ids]).await? {
				let root: ServerVtxo = ServerVtxo::deserialize(&row.get::<_, Vec<u8>>("vtxo"))?;
				let key: PublicKey = row.get::<_, &str>("server_pubkey").parse()?;
				ensure!(retirement.contains(&root.id()) && root.id().to_string() == row.get::<_, &str>("vtxo_id") &&
					root.server_pubkey() == key && i64::try_from(root.amount().to_sat())? == row.get::<_, i64>("amount") &&
					server_pubkey.is_none_or(|previous| previous == key), "direct delivery backing identity or server differs");
				server_pubkey = Some(key);
				backing_amount = backing_amount.checked_add(root.amount()).context("direct delivery backing amount overflow")?;
			}
			ensure!(amounts.values().try_fold(Amount::ZERO, |sum, (leaf,_)| sum.checked_add(leaf.gross)) == Some(backing_amount),
				"direct delivery backing no longer conserves allocation gross");
			let server_pubkey = server_pubkey.context("direct delivery has no backing server identity")?;
			let authenticated = t.authenticated_fallback_scripts(&owners.iter().map(|(p,_)| *p).collect::<Vec<_>>(), network).await?;
			ensure!(owners.iter().all(|(key,spk)| authenticated.get(key) == Some(spk)), "direct delivery record or account child mismatch");
			let plan = require_direct_allocation_policy(leaves, &amounts)?;
			let carriers = t.query("SELECT txid,signed_tx,backing_ids,fee_inputs::text AS fee_inputs,
				miner_fee_sat,generation,confirmed_height,confirmed_block,jsonb_build_object('generation',generation,
				'backing',backing_ids,'fee_inputs',fee_inputs,'miner',miner_fee_sat,'subsidy',funding_subsidy_sat,
				'mappings',(SELECT jsonb_agg(jsonb_build_object('id',m.allocation_id,'vout',m.vout) ORDER BY m.allocation_id)
				FROM expiry_carrier_allocation m WHERE m.txid=expiry_carrier.txid))::text AS witness FROM expiry_carrier
				WHERE txid IN (SELECT m.txid FROM expiry_carrier_allocation m JOIN expiry_allocation a
				ON a.id=m.allocation_id WHERE a.freeze_set=$1) ORDER BY generation", &[&freeze_set]).await?;
			let mut result = Vec::new();
			for row in carriers {
				let txid: Txid = row.get::<_, &str>("txid").parse()?;
				let bytes: Vec<u8> = row.get("signed_tx");
				let tx: Transaction = deserialize(&bytes)?;
				ensure!(tx.compute_txid() == txid && serialize(&tx) == bytes && tx.input.iter().all(|i| !i.witness.is_empty()),
					"direct delivery signed bytes are invalid");
				let backing = row.get::<_, Vec<String>>("backing_ids").into_iter()
					.map(|v| v.parse()).collect::<Result<BTreeSet<VtxoId>, _>>()?;
				ensure!(backing == retirement, "direct delivery physical retirement changed");
				let mapping = t.query("SELECT allocation_id,vout FROM expiry_carrier_allocation WHERE txid=$1", &[&txid.to_string()]).await?
					.into_iter().map(|r| -> anyhow::Result<_> { Ok((r.get::<_, &str>("allocation_id").parse::<VtxoId>()?, r.get::<_, Option<i32>>("vout"))) })
					.collect::<anyhow::Result<BTreeMap<_, _>>>()?;
				require_direct_output_mappings(&tx, &plan, &amounts, &ids, &mapping)?;
				let input_json: serde_json::Value = serde_json::from_str(row.get("fee_inputs"))?;
				let mut fee_inputs = Vec::new();
				for input in input_json.as_array().context("invalid direct fee reservations")? {
					let point = input["outpoint"].as_str().context("missing direct fee point")?.parse()?;
					let value = input["value_sat"].as_u64().context("missing direct fee amount")?;
					let script = ScriptBuf::from_hex(input["script_pubkey"].as_str().context("missing direct fee script")?)?;
					fee_inputs.push((point, TxOut { value: Amount::from_sat(value), script_pubkey: script }));
				}
				require_direct_input_partition(&tx, &backing, &fee_inputs)?;
				let recorded_confirmation = row.get::<_, Option<i32>>("confirmed_height").map(|h| -> anyhow::Result<_> {
					Ok(BlockRef { height: bitcoin_ext::BlockHeight::new(u32::try_from(h)?),
						hash: row.get::<_, &str>("confirmed_block").parse()? })
				}).transpose()?;
				let generation = row.get::<_, i64>("generation");
				ensure!(generation > 0, "invalid direct carrier generation");
				let carrier_hash = carrier_witness_hash(&row.get::<_, String>("witness"));
				result.push(StoredCarrier { tx, bytes, fee_inputs, generation, server_pubkey, allocation_hash, carrier_hash,
					miner_fee: Amount::from_sat(u64::try_from(row.get::<_, i64>("miner_fee_sat"))?), recorded_confirmation,
					remaining_rows, remaining_bytes });
			}
			Ok(result)
		}).await
	}

	pub(crate) async fn confirm_direct_delivery(&self, freeze_set: &str, tx: &StoredCarrier, block: BlockRef) -> anyhow::Result<()> {
		self.write(async |t| {
			t.lock_expiry_carriers().await?;
			let txid = tx.tx.compute_txid().to_string();
			let stored = t.query_one("SELECT signed_tx FROM expiry_carrier WHERE txid=$1 FOR UPDATE", &[&txid]).await?;
			ensure!(stored.get::<_, Vec<u8>>("signed_tx") == tx.bytes, "direct confirmation signed bytes changed");
			let other = t.query("SELECT DISTINCT c.txid FROM expiry_carrier c JOIN expiry_carrier_allocation m
				ON m.txid=c.txid JOIN expiry_allocation a ON a.id=m.allocation_id WHERE a.freeze_set=$1
				AND c.confirmed_height IS NOT NULL AND c.txid<>$2", &[&freeze_set,&txid]).await?;
			ensure!(other.is_empty(), "another direct carrier is recorded confirmed for this allocation");
			t.execute("UPDATE expiry_carrier SET confirmed_height=$2,confirmed_block=$3 WHERE txid=$1",
				&[&txid,&i32::try_from(block.height.to_u32())?,&block.hash.to_string()]).await?;
			Ok(())
		}).await
	}
}

#[cfg(test)]
mod captured_policy_tests {
	use super::*;
	use bitcoin::{Sequence, TxIn, Witness, WPubkeyHash};

	fn leaves() -> Vec<CustomerLeaf> {
		let script = ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([3;20]));
		[1,2].into_iter().map(|n| CustomerLeaf { id: VtxoId::from(OutPoint::new(Txid::from_byte_array([n;32]),0)),
			script: script.clone(), amount: Amount::from_sat(10_000) }).collect()
	}
	fn stored_amounts() -> BTreeMap<VtxoId, (LeafAllocation,String)> {
		allocate(leaves()).unwrap().recipients.into_iter().flat_map(|recipient| recipient.leaves)
			.map(|leaf| (leaf.id,(leaf,"pay".into()))).collect()
	}
	fn transaction(output: TxOut, inputs: &[OutPoint]) -> Transaction {
		Transaction { version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
			input: inputs.iter().map(|point| TxIn { previous_output:*point, script_sig:ScriptBuf::new(),
				sequence:Sequence::ZERO, witness:Witness::new() }).collect(), output:vec![output] }
	}
	#[test]
	fn captured_shared_policy_preserves_flat_group_fee_null_nonpayment_and_exact_output() {
		let mut amounts = stored_amounts();
		let plan = require_direct_allocation_policy(leaves(), &amounts).unwrap();
		assert_eq!(plan.recipients.len(),1);
		assert_eq!(plan.recipients[0].output.value,Amount::from_sat(19_000));
		assert_eq!(plan.recipients[0].leaves.iter().map(|leaf| leaf.fee.to_sat()).sum::<u64>(),1_000);
		let operator = VtxoId::from(OutPoint::new(Txid::from_byte_array([9;32]),0));
		amounts.insert(operator,(LeafAllocation { id:operator,gross:Amount::from_sat(330),fee:Amount::ZERO,
			payout:Amount::ZERO,forfeited:Amount::ZERO },"operator".into()));
		let ids = amounts.keys().copied().collect::<BTreeSet<_>>();
		let mut mapping = ids.iter().map(|id| (*id,if *id==operator {None}else{Some(0)})).collect::<BTreeMap<_,_>>();
		let mut tx = transaction(plan.recipients[0].output.clone(),&[]);
		require_direct_output_mappings(&tx,&plan,&amounts,&ids,&mapping).unwrap();
		mapping.insert(operator,Some(0));
		assert!(require_direct_output_mappings(&tx,&plan,&amounts,&ids,&mapping).is_err());
		mapping.insert(operator,None);tx.output[0].value-=Amount::from_sat(1);
		assert!(require_direct_output_mappings(&tx,&plan,&amounts,&ids,&mapping).is_err());
		let mut changed = stored_amounts();
		changed.values_mut().next().unwrap().0.fee+=Amount::from_sat(1);
		assert!(require_direct_allocation_policy(leaves(),&changed).is_err());
		let mut incomplete = leaves();incomplete.pop();
		assert!(require_direct_allocation_policy(incomplete,&stored_amounts()).is_err());
	}
	#[test]
	fn captured_shared_policy_refuses_incomplete_mapping_and_duplicate_or_missing_physical_inputs() {
		let amounts = stored_amounts();let plan = require_direct_allocation_policy(leaves(),&amounts).unwrap();
		let ids = amounts.keys().copied().collect::<BTreeSet<_>>();
		let mut mapping = ids.iter().map(|id| (*id,Some(0))).collect::<BTreeMap<_,_>>();
		let points = ids.iter().map(VtxoId::to_point).collect::<Vec<_>>();
		let mut tx = transaction(plan.recipients[0].output.clone(),&points);
		require_direct_input_partition(&tx,&ids,&[]).unwrap();
		mapping.remove(ids.first().unwrap());
		assert!(require_direct_output_mappings(&tx,&plan,&amounts,&ids,&mapping).is_err());
		tx.input.pop();assert!(require_direct_input_partition(&tx,&ids,&[]).is_err());
		tx.input.push(tx.input[0].clone());assert!(require_direct_input_partition(&tx,&ids,&[]).is_err());
		let tx = transaction(plan.recipients[0].output.clone(),&points);
		assert!(require_direct_input_partition(&tx,&ids,&[(points[0],tx.output[0].clone())]).is_err());
	}
}
