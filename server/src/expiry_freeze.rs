//! Guarded captaind handoff of an explicitly supplied current physical cut.
//! Conditional exchanges and grant retirement remain reconciliation work.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{OutPoint, ScriptBuf};
use bitcoin::consensus::deserialize;
use futures::FutureExt;

use ark::{ServerVtxo, ServerVtxoPolicy, VtxoId, VtxoPolicy};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;
use ark::vtxo::policy::clause::VtxoClause;
use ark::vtxo::policy::signing::VtxoSigner;
use bitcoin_ext::TxOutExt;
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;

use crate::bitcoind as bcd;
use crate::database::carrier_snapshot::{BackingPartitionResidual, BackingSnapshot, BackingSnapshotLimits};
use crate::database::expiry_freeze::FrozenExpiryPlan;
use crate::database::expiry_ownership::OwnershipContext;
use crate::database::ln::{LightningHtlcSubscriptionStatus, LightningPaymentStatus};
use crate::ln::SendRefundEvidence;
use crate::watchman::WatchmanSigner;
use crate::Server;

impl Server {
	/// Freeze an exact current cut using stored source paths and actual node
	/// evidence. The caller supplies IDs and a bounded read budget, never a tip,
	/// funding transaction, address, outgoing verdict or cancellation fact.
	///
	/// This reserves value, not payment. Complete conditional original/grant
	/// retirement remains refused by the existing source freeze. This does not
	/// discover work, construct a carrier, broadcast or activate a daemon.
	pub async fn freeze_current_expiry_backing(
		&self, root_ids: &[VtxoId], terminal_ids: &[VtxoId], budget: BackingSnapshotLimits,
	) -> anyhow::Result<FrozenExpiryPlan> {
		ensure!(!root_ids.is_empty() && !terminal_ids.is_empty() && budget.max_rows > 0 && budget.max_serialized_bytes > 0,
			"empty expiry cut or invalid read budget");
		ensure!(root_ids.len() <= budget.max_rows && terminal_ids.len() <= budget.max_rows &&
			root_ids.iter().collect::<BTreeSet<_>>().len() == root_ids.len() &&
			terminal_ids.iter().collect::<BTreeSet<_>>().len() == terminal_ids.len(), "invalid expiry cut IDs");
		let roots = self.db.read(async |t| {
			let mut roots = Vec::new();
			for id in root_ids { roots.push(t.get_bare_vtxo_by_id(*id).await?.vtxo); }
			Ok(roots)
		}).await?;
		let before = self.db.read(async |t| t.backing_snapshot(&roots, budget).await).await?;
		let ids = inventory_ids(&before);
		let hashes = payment_hashes(&before)?;
		// Match cooperative Lightning order. A busy payment defers this whole
		// cut; no partial prefix may be frozen while its remaining guards wait.
		let mut payment_guards = Vec::new();
		for hash in &hashes {
			payment_guards.push(self.payment_guards.lock(*hash).now_or_never().context("expiry payment is in progress")?);
		}
		let _flux = self.vtxos_in_flux.try_lock(ids.iter().copied())?;
		let signer = WatchmanSigner::new(self.server_key.clone(), self.ephemeral_master_key.clone(), self.db.clone());
		let keys = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let result = self.db.write(async |t| {
			// These are the existing freeze locks, taken before observing Core so
			// a delayed durable source writer cannot age the observation first.
			t.lock_htlc_settlements().await?;
			t.lock_expiry_carriers().await?;
			t.lock_expiry_inputs(&keys).await?;
			let snapshot = t.backing_snapshot(&roots, budget).await?;
			ensure!(inventory_ids(&snapshot) == ids && payment_hashes(&snapshot)? == hashes,
				"expiry source closure changed while acquiring guards; retry the complete cut");
			let terminals = terminal_ids.iter().map(|id| {
				snapshot.regular.iter().find(|f| f.state.vtxo_id == *id).and_then(|f| f.full_vtxo.clone())
					.or_else(|| snapshot.pending_boards.iter().find(|f| f.vtxo.id() == *id).map(|f| ServerVtxo::from(f.vtxo.clone())))
					.with_context(|| format!("expiry terminal {id} has no stored full path"))
			}).collect::<anyhow::Result<Vec<_>>>()?;
			let tip = bcd::tip(&self.bitcoind).await?;
			let anchors = roots.iter().map(|r| r.chain_anchor().txid).chain(terminals.iter().map(|v| v.chain_anchor().txid)).collect::<BTreeSet<_>>();
			let mut funding = BTreeMap::new();
			for anchor in anchors {
				let raw = bcd::custom_get_raw_transaction_info(&self.bitcoind, anchor, None).await?.context("expiry funding is unavailable")?;
				funding.insert(anchor, deserialize(&raw.hex)?);
			}
			let residuals = transition_residuals(&roots, &terminals);
			snapshot.check_partition(&terminals, &funding.values().cloned().collect::<Vec<_>>(), &residuals, budget)?;
			// Anchor inventory is a superset. Only proven ancestors may be left
			// out of this cut; missing or alternative paths cannot disappear just
			// because their source row says Spent.
			let selected = terminal_ids.iter().copied().collect::<BTreeSet<_>>();
			let ancestors = terminals.iter().flat_map(|v| v.transactions().flat_map(|i|
				i.tx.input.into_iter().map(|input| VtxoId::from(input.previous_output)))).collect::<BTreeSet<_>>();
			for id in &ids {
				ensure!(selected.contains(id) || ancestors.contains(id), "expiry inventory {id} is not covered by the current cut");
			}
			let mut unavailable = BTreeSet::new();
			let mut canceled = BTreeSet::new();
			let mut send_expiries = BTreeMap::new();
			let mut receives = BTreeSet::new();
			for terminal in &terminals {
				let Ok(vtxo) = terminal.clone().try_into_user_vtxo() else { continue; };
				if let Some(policy) = vtxo.policy().as_server_htlc_send() {
					ensure!(send_expiries.insert(policy.payment_hash, policy.htlc_expiry).is_none_or(|old| old == policy.htlc_expiry),
						"expiry outgoing coins have inconsistent HTLC deadlines");
				}
				if let Some(policy) = vtxo.policy().as_server_htlc_recv() {
					receives.insert(policy.payment_hash);
				}
			}
			let mut sends = BTreeMap::new();
			for (hash, expiry) in send_expiries {
				ensure!(self.chain_tip() == tip, "expiry outgoing observation awaits the actual node tip");
				let observed = self.lightning_send_outgoing_evidence(hash, expiry, &mut unavailable).await?;
				if matches!(observed, SendRefundEvidence::Refundable) {
					// Cancellation and allocation share this COMMIT. A backing or
					// ownership refusal rolls both back; no grant can be prepared
					// while the already-held payment guard fences this decision.
					if let Some(row) = t.query_opt("SELECT id FROM lightning_htlc_subscription WHERE payment_hash=$1 FOR UPDATE",
						&[&hash.to_string()]).await? {
						let granted: bool = t.query_one("SELECT EXISTS(SELECT 1 FROM vtxo WHERE lightning_htlc_subscription_id=$1)",
							&[&row.get::<_, i64>("id")]).await?.get(0);
						ensure!(!granted, "expiry refund requires receive cancellation and complete grant retirement qualification");
					}
					if let Some(sub) = t.get_htlc_subscription_by_payment_hash(hash).await? {
						let attempts = t.get_payment_attempts_by_payment_hash(hash).await?;
						ensure!(!attempts.is_empty() && attempts.iter().all(|a| a.lightning_htlc_subscription_id == Some(sub.id)),
							"expiry zero-grant cancellation requires a complete pure intra-Ark association");
						ensure!(sub.htlc_vtxos.is_empty() && matches!(sub.status,
							LightningHtlcSubscriptionStatus::Accepted | LightningHtlcSubscriptionStatus::Canceled),
							"expiry refund requires receive cancellation and complete grant retirement qualification");
						t.ensure_not_settled(hash).await?;
						// Partial intra-Ark setup can leave the incoming hold alive.
						// Only the actual node cancellation fences its later settlement;
						// the intra association or absence of grants does not prove it.
						ensure!(!unavailable.contains(&sub.lightning_node_id), "expiry incoming hold node is unavailable");
						let incoming = tokio::time::timeout(std::time::Duration::from_secs(10),
							self.lightning_manager.hold_invoice_state(sub.lightning_node_id, hash)).await;
						match incoming {
							Ok(Ok(Some(cln_rpc::plugins::hold::InvoiceState::Cancelled))) => {},
							Ok(Ok(state)) => bail!("expiry incoming hold cancellation is not authoritative: {state:?}"),
							Ok(Err(error)) => {
								unavailable.insert(sub.lightning_node_id);
								return Err(error.context("expiry incoming hold node is unavailable"));
							},
							Err(error) => {
								unavailable.insert(sub.lightning_node_id);
								return Err(anyhow::Error::new(error).context("expiry incoming hold node did not answer"));
							},
						}
						if sub.status == LightningHtlcSubscriptionStatus::Accepted {
							// Reuse the upstream setup timeout, with its actual accepted
							// timestamp. Also require the actual incoming claim deadline
							// to have passed. Neither clock determines an external verdict.
							ensure!(sub.accepted_at.is_some_and(|at| at < chrono::Local::now() - self.config.receive_htlc_forward_timeout)
								&& sub.lowest_incoming_htlc_expiry.is_some_and(|height| tip.height > height),
								"expiry intra-Ark receive setup timeout or claim deadline has not passed");
							// No receive grant exists and this locked row is Accepted.
							// Do not infer grant maturity; the helper returns its pre-cancel state.
							ensure!(t.cancel_revocable_htlc_subscription(hash, false).await?
								== Some((LightningHtlcSubscriptionStatus::Accepted, false)),
								"expiry receive cancellation lost its source predicate");
							for attempt in attempts.iter().filter(|a| !a.status.is_final()) {
								ensure!(t.update_lightning_payment_attempt_status(attempt, LightningPaymentStatus::Failed,
									Some("htlc vtxo setup timed out")).await?.is_some(), "expiry intra-Ark cancellation lost its attempt predicate");
							}
						}
					}
				}
				sends.insert(hash, observed);
			}
			for hash in receives {
				if self.htlc_settler.is_settled(hash).await?.is_some() { continue; }
				if let Some(sub) = t.get_htlc_subscription_by_payment_hash(hash).await? {
					if unavailable.contains(&sub.lightning_node_id) { continue; }
					match self.lightning_manager.hold_invoice_state(sub.lightning_node_id, hash).await {
						Ok(Some(cln_rpc::plugins::hold::InvoiceState::Cancelled)) => { canceled.insert(hash); },
						Ok(_) => {},
						Err(_) => { unavailable.insert(sub.lightning_node_id); },
					}
				}
			}
			// Node observations can wait. Check actual mempool-sensitive backing
			// after them, immediately before the source freeze.
			for root in &roots {
				ensure!(root.server_pubkey() == self.server_pubkey, "expiry backing uses another server signer");
				let utxo = self.bitcoind.try_get_tx_out(root.point(), true).await?
					.context("expiry backing is spent or unavailable")?;
				ensure!(utxo.bestblock == tip.hash && utxo.confirmations > 0 && (!utxo.coinbase || utxo.confirmations >= 100),
					"expiry backing confirmation or node tip changed");
				ensure!(utxo.value == root.amount() && ScriptBuf::from(utxo.script_pub_key.hex) == root.txout().script_pubkey,
					"expiry backing script or value differs from Core");
				if *root.policy() == ServerVtxoPolicy::ServerOwned {
					ensure!(root.output_taproot().internal_key() == self.server_pubkey.x_only_public_key().0,
						"expiry backing keyspend is not server owned");
				} else {
					let clause = signer.find_signable_clause(root).await.context("expiry backing has no server signing clause")?;
					let (absolute, relative) = match clause {
						VtxoClause::TimelockSign(c) => (Some(c.timelock_height), 0),
						VtxoClause::DelayedTimelockSign(c) => (Some(c.timelock_height), c.block_delta.to_u32()),
						VtxoClause::DelayedSign(c) => (None, c.block_delta.to_u32()),
						VtxoClause::HashDelaySign(c) => (None, c.block_delta.to_u32()),
						VtxoClause::HashDelaySign_v0(c) => (None, c.block_delta.to_u32()),
						VtxoClause::HashSign(_) | VtxoClause::HashSign_v0(_) => (None, 0),
					};
					ensure!(absolute.is_none_or(|h| tip.height >= h) && utxo.confirmations >= relative,
						"expiry backing signing clause is not mature");
				}
			}
			ensure!(bcd::tip(&self.bitcoind).await? == tip, "expiry node tip changed before freeze; retry the complete cut");
			let context = OwnershipContext { tip: tip.height.to_u32(), grace: self.config.expiry_payout.grace_blocks,
				max_dependencies: budget.max_rows, server_pubkey: self.server_pubkey, funding: &funding, canceled_receives: &canceled };
			t.freeze_expiry_sources(&roots, &terminals, &residuals, &context, &sends, self.config.network, budget).await
		}).await;
		if result.is_ok() {
			for hash in &hashes { self.lightning_manager.notify_payment_update(*hash); }
		}
		drop(payment_guards); // The flux guard also outlives the database COMMIT.
		result
	}
}

fn inventory_ids(snapshot: &BackingSnapshot) -> BTreeSet<VtxoId> {
	snapshot.regular.iter().map(|f| f.state.vtxo_id).chain(snapshot.pending_boards.iter().map(|f| f.vtxo.id())).collect()
}

fn payment_hashes(snapshot: &BackingSnapshot) -> anyhow::Result<BTreeSet<PaymentHash>> {
	let policies = snapshot.regular.iter().filter_map(|f| match f.state.vtxo.policy() {
		ServerVtxoPolicy::User(policy) => Some(policy), _ => None,
	}).chain(snapshot.pending_boards.iter().map(|f| f.vtxo.policy()));
	let mut hashes = BTreeSet::new();
	for policy in policies {
		// The source resolver's typed HTLC associations use the current policy
		// accessors. An older policy must not fall through as a plain registered
		// entitlement without its outgoing/receive arbitration.
		ensure!(!matches!(policy, VtxoPolicy::ServerHtlcSend_v0(_) | VtxoPolicy::ServerHtlcRecv_v0(_)),
			"legacy HTLC source requires typed ownership qualification before freeze");
		if let Some(p) = policy.as_server_htlc_send() { hashes.insert(p.payment_hash); }
		if let Some(p) = policy.as_server_htlc_recv() { hashes.insert(p.payment_hash); }
	}
	Ok(hashes)
}

fn transition_residuals(roots: &[ServerVtxo], terminals: &[ServerVtxo<Full>]) -> Vec<BackingPartitionResidual> {
	let points = roots.iter().map(|r| r.point()).collect::<BTreeSet<_>>();
	let mut residuals = BTreeMap::new();
	for terminal in terminals {
		let mut inside = false;
		for item in terminal.transactions() {
			inside |= item.tx.input.iter().any(|i| points.contains(&i.previous_output));
			if inside {
				let txid = item.tx.compute_txid();
				for (vout, txout) in item.tx.output.into_iter().enumerate() {
					if txout.is_p2a_fee_anchor() { residuals.insert(OutPoint::new(txid, vout as u32), txout); }
				}
			}
		}
	}
	residuals.into_iter().map(|(outpoint, txout)| BackingPartitionResidual { outpoint, txout }).collect()
}
