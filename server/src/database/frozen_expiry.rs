//! Read complete source-frozen groups without deciding their ownership again.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, Network, ScriptBuf};
use bitcoin::secp256k1::PublicKey;

use ark::{ProtocolEncoding, ServerVtxo, VtxoId};
use crate::watchman::settlement::{allocate, CustomerLeaf, LeafAllocation};
use super::{Db, carrier_snapshot::BackingSnapshotLimits, expiry_freeze::FrozenExpiryPlan};

pub struct FrozenExpiryGroup {
	pub plan: FrozenExpiryPlan,
	pub backing: Vec<ServerVtxo>,
}

impl Db {
	/// A complete bounded listing, not a silently truncated queue. The caller
	/// must hydrate every durable fee reservation before constructing new work.
	pub async fn frozen_expiry_group_ids(&self, limit: usize) -> anyhow::Result<Vec<String>> {
		ensure!(limit > 0, "frozen group limit must be positive");
		let bound = i64::try_from(limit)?.checked_add(1).context("frozen group limit overflow")?;
		self.read(async |t| {
			let rows = t.query("SELECT DISTINCT a.freeze_set FROM expiry_allocation a WHERE NOT EXISTS (
				SELECT 1 FROM expiry_carrier_allocation m JOIN expiry_carrier c ON c.txid=m.txid
				WHERE m.allocation_id=a.id AND c.confirmed_height IS NOT NULL) ORDER BY a.freeze_set LIMIT $1", &[&bound]).await?;
			ensure!(rows.len() <= limit, "complete frozen group listing exceeds budget");
			Ok(rows.into_iter().map(|r| r.get("freeze_set")).collect())
		}).await
	}

	pub async fn read_frozen_expiry_group(
		&self, id: &str, network: Network, budget: BackingSnapshotLimits,
	) -> anyhow::Result<FrozenExpiryGroup> {
		ensure!(!id.is_empty() && budget.max_rows > 0 && budget.max_serialized_bytes > 0, "invalid frozen group read budget");
		self.read(async |t| {
			t.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[]).await?;
			let stats = t.query_one("SELECT count(*)::bigint AS rows,
				COALESCE(sum(octet_length(id)+COALESCE(octet_length(spk),0)+
					octet_length(source_evidence::text)+octet_length(retirement_ids::text)),0)::bigint AS bytes
				FROM expiry_allocation WHERE freeze_set=$1", &[&id]).await?;
			let count = usize::try_from(stats.get::<_, i64>("rows"))?;
			let mut bytes = usize::try_from(stats.get::<_, i64>("bytes"))?;
			ensure!(count > 0 && count <= budget.max_rows && bytes <= budget.max_serialized_bytes, "frozen group missing or exceeds budget");
			let rows = t.query("SELECT id,owner_pubkey,spk,disposition,gross_sat,fee_sat,payout_sat,forfeited_sat,retirement_ids
				FROM expiry_allocation WHERE freeze_set=$1 ORDER BY id", &[&id]).await?;
			let mut stored = BTreeMap::new();
			let mut leaves = Vec::new();
			let mut owners = Vec::new();
			let mut retirement = None;
			let mut gross = Amount::ZERO;
			for row in rows {
				let leaf_id: VtxoId = row.get::<_, &str>("id").parse()?;
				let amount = |field| -> anyhow::Result<Amount> { Ok(Amount::from_sat(u64::try_from(row.get::<_, i64>(field))?)) };
				let allocation = LeafAllocation { id: leaf_id, gross: amount("gross_sat")?, fee: amount("fee_sat")?,
					payout: amount("payout_sat")?, forfeited: amount("forfeited_sat")? };
				gross = gross.checked_add(allocation.gross).context("frozen gross overflow")?;
				let roots = row.get::<_, Vec<String>>("retirement_ids").into_iter()
					.map(|v| v.parse()).collect::<Result<BTreeSet<VtxoId>, _>>()?;
				ensure!(!roots.is_empty() && retirement.as_ref().is_none_or(|r| r == &roots), "frozen retirement set differs between allocations");
				retirement = Some(roots);
				let disposition: String = row.get("disposition");
				if disposition == "operator" {
					ensure!(row.get::<_, Option<Vec<u8>>>("owner_pubkey").is_none() && row.get::<_, Option<Vec<u8>>>("spk").is_none() &&
						allocation.fee == Amount::ZERO && allocation.payout == Amount::ZERO && allocation.forfeited == Amount::ZERO,
						"invalid frozen operator allocation");
				} else {
					let owner = PublicKey::from_slice(&row.get::<_, Vec<u8>>("owner_pubkey"))?;
					let script = ScriptBuf::from(row.get::<_, Vec<u8>>("spk"));
					owners.push((owner, script.clone()));
					leaves.push(CustomerLeaf { id: leaf_id, script, amount: allocation.gross });
				}
				stored.insert(leaf_id, (allocation, disposition));
			}
			let scripts = t.authenticated_fallback_scripts(&owners.iter().map(|(p,_)| *p).collect::<Vec<_>>(), network).await?;
			ensure!(owners.iter().all(|(p,s)| scripts.get(p) == Some(s)), "frozen address or account child differs from authenticated record");
			let policy = allocate(leaves)?;
			for leaf in policy.recipients.iter().flat_map(|r| &r.leaves) {
				ensure!(stored.get(&leaf.id).is_some_and(|(a,d)| a == leaf && d == "pay"), "frozen recipient policy differs");
			}
			for leaf in &policy.forfeits {
				ensure!(stored.get(&leaf.id).is_some_and(|(a,d)| a == leaf && d == "forfeit"), "frozen forfeiture policy differs");
			}
			let roots = retirement.context("frozen retirement is empty")?;
			ensure!(count.checked_add(roots.len()).is_some_and(|n| n <= budget.max_rows), "complete frozen group exceeds row budget");
			let mut backing = Vec::new();
			let mut backing_amount = Amount::ZERO;
			for root in &roots {
				let row = t.query_opt("SELECT vtxo FROM vtxo WHERE vtxo_id=$1", &[&root.to_string()]).await?.context("frozen backing source is missing")?;
				let encoded: Vec<u8> = row.get("vtxo");
				bytes = bytes.checked_add(encoded.len()).context("frozen byte budget overflow")?;
				ensure!(bytes <= budget.max_serialized_bytes, "complete frozen group exceeds byte budget");
				let vtxo = ServerVtxo::deserialize(&encoded)?;
				ensure!(vtxo.id() == *root, "frozen backing source identity differs");
				backing_amount = backing_amount.checked_add(vtxo.amount()).context("frozen backing overflow")?;
				backing.push(vtxo);
			}
			ensure!(gross == backing_amount && gross <= Amount::MAX_MONEY, "frozen backing no longer conserves allocations");
			Ok(FrozenExpiryGroup { backing, plan: FrozenExpiryPlan { id: id.to_owned(),
				allocation_ids: stored.keys().copied().collect(), backing_ids: roots.into_iter().collect(),
				recipients: policy.recipients.into_iter().map(|r| r.output).collect() } })
		}).await
	}
}
