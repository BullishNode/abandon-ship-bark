//! Bounded, complete readback of an already committed direct handoff.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::BlockRef;

use ark::VtxoId;
use crate::database::carrier_snapshot::BackingSnapshotLimits;
use crate::watchman::settlement::{allocate, CustomerLeaf, LeafAllocation};
use super::Db;

pub(crate) struct StoredCarrier {
	pub tx: Transaction,
	pub bytes: Vec<u8>,
	pub generation: i64,
	pub fee_inputs: Vec<(OutPoint, TxOut)>,
	pub miner_fee: Amount,
	pub recorded_confirmation: Option<BlockRef>,
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
			ensure!(versions > 0 && versions <= limits.max_rows && allocation_bytes.checked_add(
				usize::try_from(stats.get::<_, i64>("bytes"))?).is_some_and(|n| n <= limits.max_serialized_bytes),
				"direct delivery versions are missing or exceed budget");
			let rows = t.query("SELECT id,owner_pubkey,spk,disposition,gross_sat,fee_sat,payout_sat,
				forfeited_sat,retirement_ids FROM expiry_allocation WHERE freeze_set=$1 ORDER BY id", &[&freeze_set]).await?;
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
			let authenticated = t.authenticated_fallback_scripts(&owners.iter().map(|(p,_)| *p).collect::<Vec<_>>(), network).await?;
			ensure!(owners.iter().all(|(key,spk)| authenticated.get(key) == Some(spk)), "direct delivery record or account child mismatch");
			let plan = allocate(leaves)?;
			for leaf in plan.recipients.iter().flat_map(|r| &r.leaves) {
				ensure!(amounts.get(&leaf.id).is_some_and(|(stored,disposition)| stored == leaf && disposition == "pay"),
					"direct delivery payout allocation differs from policy");
			}
			for leaf in &plan.forfeits {
				ensure!(amounts.get(&leaf.id).is_some_and(|(stored,disposition)| stored == leaf && disposition == "forfeit"),
					"direct delivery forfeiture differs from policy");
			}
			let carriers = t.query("SELECT txid,signed_tx,backing_ids,fee_inputs::text AS fee_inputs,
				miner_fee_sat,generation,confirmed_height,confirmed_block FROM expiry_carrier
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
				ensure!(mapping.keys().copied().collect::<BTreeSet<_>>() == ids, "direct delivery allocation mappings are incomplete");
				let mut payout_outputs = BTreeSet::new();
				for recipient in &plan.recipients {
					let vout = mapping[&recipient.leaves[0].id].context("direct payout has no output mapping")?;
					ensure!(vout >= 0 && payout_outputs.insert(vout) && tx.output.get(vout as usize) == Some(&recipient.output) &&
						recipient.leaves.iter().all(|a| mapping[&a.id] == Some(vout)), "direct delivery output mapping or amount differs");
				}
				for (id,(_,disposition)) in &amounts {
					ensure!(disposition == "pay" || mapping[id].is_none(), "direct non-payment has a customer output mapping");
				}
				let input_json: serde_json::Value = serde_json::from_str(row.get("fee_inputs"))?;
				let mut fee_inputs = Vec::new();
				for input in input_json.as_array().context("invalid direct fee reservations")? {
					let point = input["outpoint"].as_str().context("missing direct fee point")?.parse()?;
					let value = input["value_sat"].as_u64().context("missing direct fee amount")?;
					let script = ScriptBuf::from_hex(input["script_pubkey"].as_str().context("missing direct fee script")?)?;
					fee_inputs.push((point, TxOut { value: Amount::from_sat(value), script_pubkey: script }));
				}
				let points = backing.iter().map(VtxoId::to_point).chain(fee_inputs.iter().map(|(p,_)| *p)).collect::<BTreeSet<_>>();
				ensure!(points.len() == backing.len()+fee_inputs.len() && points == tx.input.iter().map(|i| i.previous_output).collect() &&
					points.len() == tx.input.len(), "direct delivery reserved inputs differ from signed transaction");
				let recorded_confirmation = row.get::<_, Option<i32>>("confirmed_height").map(|h| -> anyhow::Result<_> {
					Ok(BlockRef { height: bitcoin_ext::BlockHeight::new(u32::try_from(h)?),
						hash: row.get::<_, &str>("confirmed_block").parse()? })
				}).transpose()?;
				let generation = row.get::<_, i64>("generation");
				ensure!(generation > 0, "invalid direct carrier generation");
				result.push(StoredCarrier { tx, bytes, fee_inputs, generation,
					miner_fee: Amount::from_sat(u64::try_from(row.get::<_, i64>("miner_fee_sat"))?), recorded_confirmation });
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
