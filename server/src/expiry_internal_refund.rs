//! Refund connected internal senders only with every original receive timeout.
//! Each attempt keeps its own funding identity inside the complete component.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{OutPoint, Transaction, Txid};
use bitcoin_ext::BlockHeight;
use bitcoind_async_client::traits::Reader;
use tracing::{debug, info, warn};

use ark::{ProtocolEncoding, ServerVtxo, VtxoId};

use crate::Server;
use crate::database::expiry_chain::ExpiryViewToken;
use crate::database::expiry_decision::ReadyOutput;
use crate::database::expiry_geometry::{economic_cut, source_transitions};
use crate::database::expiry_internal_refund::{InternalRefundComponent, MAX_JOINT_GRANTS};
use crate::database::expiry_inventory::BackingSnapshot;
use crate::expiry_readiness::INVENTORY_LIMITS;

const ATTEMPTS_PER_PASS: i64 = 8;
const MAX_FRONTIER_ROOTS: usize = 256;

struct PreparedAnchor {
	snapshot: Arc<BackingSnapshot>,
	funding: Transaction,
	transitions: Arc<BTreeMap<Txid, Transaction>>,
}

impl Server {
	pub(crate) async fn internal_refund_readiness_pass(&self, cursor: i64) -> anyhow::Result<i64> {
		let Some(view) = self.db.write(async |t| {
			let Some(view) = t.expiry_rebuild_token().await? else { return Ok(None); };
			t.lock_valid_expiry_view(view).await?;
			Ok(Some(view))
		}).await? else { return Ok(cursor); };
		self.verify_readiness_view(view).await?;
		// Advance even when one complete component is held. Discovery also
		// marks its members so the same component is not retried eight times.
		let rows = self.db.read(async |t| Ok(t.query("SELECT f.id FROM expiry_lightning_internal_funding f \
			WHERE f.id>$1 AND EXISTS(SELECT 1 FROM vtxo g WHERE g.lightning_htlc_subscription_id=f.subscription_id) \
			AND EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l \
				JOIN vtxo source ON source.vtxo_id=l.vtxo_id WHERE l.lightning_payment_attempt_id=f.id \
				AND source.onchain_spent_height IS NULL AND source.onchain_spent_txid IS NULL \
				AND (NOT EXISTS(SELECT 1 FROM expiry_decided_coin d WHERE d.vtxo_id=l.vtxo_id) \
					OR EXISTS(SELECT 1 FROM expiry_decided_coin d \
						JOIN expiry_allocation_coin c ON c.entitlement_id=d.entitlement_id \
						JOIN expiry_allocation a ON a.id=c.allocation_id WHERE d.vtxo_id=l.vtxo_id \
						AND a.status='conflicted' AND c.evidence->>'kind'='htlc_internal_joint_timeout' \
						AND c.evidence->>'attempt_id'=f.id::TEXT))) \
			ORDER BY f.id LIMIT $2", &[&cursor,&ATTEMPTS_PER_PASS]).await?)).await?;
		let Some(last) = rows.last() else { return Ok(0); };
		let next = last.get::<_, i64>("id");
		let mut visited = BTreeSet::new();
		for row in rows {
			if self.rtmgr.shutdown_requested() { break; }
			let attempt = row.get::<_, i64>("id");
			if visited.contains(&attempt) { continue; }
			if let Err(error) = self.prepare_internal_refund(view, attempt, &mut visited).await {
				debug!(attempt_id = attempt, error = ?error, "Whole internal refund component remains pending");
			}
		}
		Ok(next)
	}

	async fn prepare_internal_refund(
		&self, view: ExpiryViewToken, attempt: i64, visited: &mut BTreeSet<i64>,
	) -> anyhow::Result<()> {
		visited.insert(attempt);
		let Some(first) = self.db.read(async |t| t.unclaimed_internal_refund(attempt, view).await).await? else {
			return Ok(());
		};
		let mut members = BTreeMap::from([(attempt, first)]);
		let mut pending = BTreeSet::from([attempt]);
		let mut inspected = BTreeSet::new();
		let mut selected = BTreeMap::<VtxoId, ServerVtxo>::new();
		let mut anchors = BTreeMap::<OutPoint, PreparedAnchor>::new();
		let mut rows = 0usize;
		let mut bytes = 0usize;
		let mut roots = 0usize;
		while let Some(attempt) = pending.pop_first() {
			ensure!(!self.rtmgr.shutdown_requested(), "shutdown cancels incomplete internal component preparation");
			let incoming = members.get(&attempt).expect("queued member exists").live_incoming_ids();
			let sources = self.db.read(async |t| t.get_server_vtxos_by_id(&incoming).await).await?;
			ensure!(sources.len() == incoming.len(), "internal component lacks its complete live original sources");
			for source in sources {
				let anchor = source.vtxo.chain_anchor();
				if !anchors.contains_key(&anchor) {
					let prepared = self.prepare_internal_refund_anchor(view, anchor).await?;
					roots = roots.checked_add(prepared.snapshot.roots.len()).context("internal frontier budget overflow")?;
					rows = rows.checked_add(prepared.snapshot.regular.len()).context("internal inventory budget overflow")?;
					bytes = bytes.checked_add(prepared.snapshot.serialized_bytes).context("internal byte budget overflow")?;
					ensure!(roots <= MAX_FRONTIER_ROOTS && rows <= INVENTORY_LIMITS.max_rows
						&& bytes <= INVENTORY_LIMITS.max_serialized_bytes, "internal component exceeds its aggregate inventory budget");
					anchors.insert(anchor, prepared);
				}
				let prepared = anchors.get(&anchor).expect("anchor prepared above");
				bytes = bytes.checked_add(source.vtxo.serialize().len()).context("internal source byte budget overflow")?;
				ensure!(bytes <= INVENTORY_LIMITS.max_serialized_bytes, "internal source paths exceed preparation budget");
				let snapshot = Arc::clone(&prepared.snapshot);
				let funding = prepared.funding.clone();
				let root = tokio::task::spawn_blocking(move || {
					source.vtxo.validate(&funding).context("internal source is not fully signed")?;
					let id = snapshot.path_root(&source.vtxo, &funding)?
						.context("internal source has no mature actual frontier backing")?;
					Ok::<_, anyhow::Error>(snapshot.roots.iter().find(|root| root.id() == id)
						.context("internal source backing disappeared")?.clone())
				}).await.context("internal source path task failed")??;
				selected.insert(root.id(), root.clone());
				if !inspected.insert(root.id()) { continue; }
				let snapshot = Arc::clone(&prepared.snapshot);
				let funding = prepared.funding.clone();
				let transitions = Arc::clone(&prepared.transitions);
				let terminals = tokio::task::spawn_blocking(move || {
					let cut = economic_cut(&snapshot, &root, &funding, &transitions, INVENTORY_LIMITS)?;
					Ok::<_, anyhow::Error>(cut.terminals.iter().map(|index| snapshot.regular[*index].state.vtxo_id.to_string()).collect::<Vec<_>>())
				}).await.context("internal component economic cut task failed")??;
				// Only still-unresolved incoming funding joins the component.
				// A discharged or zero-grant source keeps its ordinary ownership
				// proof; it must not be rebound to another attempt's grant set.
				let linked = self.db.read(async |t| Ok(t.query("SELECT DISTINCT f.id FROM expiry_lightning_internal_funding f \
					JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
					JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=f.id \
					JOIN vtxo v ON v.vtxo_id=l.vtxo_id JOIN htlc_vtxo h ON h.id=v.id \
					WHERE l.vtxo_id=ANY($1) AND h.direction='incoming' AND h.payment_hash=a.payment_hash \
					AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL \
					AND EXISTS(SELECT 1 FROM vtxo g WHERE g.lightning_htlc_subscription_id=f.subscription_id) \
					AND NOT EXISTS(SELECT 1 FROM htlc_settlement wal WHERE wal.payment_hash=h.payment_hash) \
					AND (NOT EXISTS(SELECT 1 FROM expiry_decided_coin d WHERE d.vtxo_id=v.vtxo_id) \
						OR EXISTS(SELECT 1 FROM expiry_decided_coin d JOIN expiry_allocation_coin c ON c.entitlement_id=d.entitlement_id \
						JOIN expiry_allocation a ON a.id=c.allocation_id WHERE d.vtxo_id=v.vtxo_id AND a.status='conflicted' \
						AND c.evidence->>'kind'='htlc_internal_joint_timeout' AND c.evidence->>'attempt_id'=f.id::TEXT)) \
					ORDER BY f.id LIMIT $2", &[&terminals,&(MAX_JOINT_GRANTS+1)]).await?)).await?;
				ensure!(linked.len() as i64 <= MAX_JOINT_GRANTS, "internal component exceeds its attempt budget");
				for row in linked {
					let id = row.get::<_, i64>("id");
					if members.contains_key(&id) { continue; }
					visited.insert(id);
					let member = self.db.read(async |t| t.unclaimed_internal_refund(id, view).await).await?
						.context("one connected funding obligation is not eligible; keep its whole backing pending")?;
					members.insert(id, member);
					ensure!(members.len() as i64 <= MAX_JOINT_GRANTS
						&& members.values().map(|member| member.incoming.len()).sum::<usize>() <= MAX_JOINT_GRANTS as usize
						&& members.values().map(|member| member.grants.len()).sum::<usize>() <= MAX_JOINT_GRANTS as usize,
						"internal component exceeds its aggregate input budget");
					pending.insert(id);
				}
			}
		}
		let component = InternalRefundComponent::new(view, members.into_values().collect())?;
		let mut grouped = BTreeMap::<OutPoint, Vec<ServerVtxo>>::new();
		for root in selected.into_values() { grouped.entry(root.chain_anchor()).or_default().push(root); }
		let mut ready = BTreeMap::<BlockHeight, Vec<ReadyOutput>>::new();
		let mut prepared_bytes = 0;
		for (anchor, roots) in grouped {
			self.prepare_expiry_anchor(view, anchor, roots, &mut ready, &mut prepared_bytes, Some(&component)).await?;
		}
		let outputs = ready.into_values().flatten().collect::<Vec<_>>();
		self.commit_internal_refund_outputs(view, component, outputs).await
	}

	async fn prepare_internal_refund_anchor(&self, view: ExpiryViewToken, anchor: OutPoint) -> anyhow::Result<PreparedAnchor> {
		let tip = i32::try_from(view.captaind.height.min(view.watchmand.height).to_u32())?;
		let rows = self.db.read(async |t| Ok(t.query("SELECT vtxo FROM vtxo WHERE anchor_point=$1 \
			AND frontier_at IS NOT NULL AND confirmed_height<=$2 AND expiry<=$2 \
			AND onchain_spent_height IS NULL AND onchain_spent_txid IS NULL ORDER BY vtxo_id LIMIT $3",
			&[&anchor.to_string(),&tip,&i64::try_from(MAX_FRONTIER_ROOTS+1)?]).await?)).await?;
		ensure!(rows.len() <= MAX_FRONTIER_ROOTS, "internal incoming frontier exceeds preparation budget");
		let roots = rows.into_iter().map(|row| ServerVtxo::deserialize(row.get("vtxo"))).collect::<Result<Vec<_>,_>>()?;
		let snapshot = self.db.read(async |t| t.backing_snapshot(&roots, INVENTORY_LIMITS).await).await?;
		let funding = self.bitcoind.get_raw_transaction_verbosity_zero(&anchor.txid).await?.0;
		ensure!(funding.compute_txid() == anchor.txid, "internal funding differs from its anchor");
		tokio::task::spawn_blocking(move || {
			let transitions = source_transitions(&snapshot, &funding)?;
			Ok::<_, anyhow::Error>(PreparedAnchor { snapshot: Arc::new(snapshot), funding, transitions: Arc::new(transitions) })
		}).await.context("internal inventory task failed")?
	}

	async fn commit_internal_refund_outputs(
		&self, view: ExpiryViewToken, component: InternalRefundComponent, outputs: Vec<ReadyOutput>,
	) -> anyhow::Result<()> {
		ensure!(!outputs.is_empty(), "internal component has no complete ready backing");
		self.verify_readiness_view(view).await?;
		let attempts = component.members.iter().map(|member| member.attempt_id).collect::<Vec<_>>();
		match self.db.write(async |t| t.commit_internal_refund(view, outputs, self.config.network, &component).await).await {
			Ok(Some(group)) => info!(?attempts, allocation_id = group.id, inputs = group.inputs.len(), "Complete internal timeout component committed"),
			Ok(None) => {},
			Err(error) => warn!(?attempts, error = ?error, "Internal component has no partial allocation"),
		}
		Ok(())
	}
}
