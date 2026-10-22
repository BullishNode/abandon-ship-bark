//! Background preparation of independently ready expiry backing outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use bitcoin::{OutPoint, Transaction};
use bitcoin_ext::BlockHeight;
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use bitcoind_async_client::traits::Reader;
use tracing::{debug, info, warn};

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoPolicy};

use crate::Server;
use crate::database::Tx;
use crate::database::expiry_attribution::Attribution;
use crate::database::expiry_chain::ExpiryViewToken;
use crate::database::expiry_decision::{CommittedExpiryVersions, ReadyCoin, ReadyOutput, VerifiedPayee};
use crate::database::expiry_inventory::{
	BackingSnapshotLimits, BackingVtxoFact,
};
use crate::database::expiry_ownership::OwnershipDisposition;
use crate::database::expiry_geometry::BackingGeometry;

const ROOTS_PER_PASS: i64 = 64;
pub(crate) const INVENTORY_LIMITS: BackingSnapshotLimits = BackingSnapshotLimits {
	max_rows: 250_000,
	max_serialized_bytes: 256 * 1024 * 1024,
};

enum PreparedCoin {
	Ready(ReadyCoin),
	Hold,
	AwaitingAttribution,
	NeedsHtlcProgress,
}

impl Tx<'_> {
	async fn prepare_expiry_coin(
		&self,
		fact: &BackingVtxoFact,
		funding: &Transaction,
		view: ExpiryViewToken,
		network: bitcoin::Network,
		internal_refund_depth: u32,
	) -> anyhow::Result<PreparedCoin> {
		let ownership = self.propose_expiry_ownership(
			fact,
			funding,
			view.captaind.height,
			Some(view.watchmand.height),
			internal_refund_depth,
		).await?;
		let payee = match &ownership.disposition {
			OwnershipDisposition::Customer { user_pubkey, mailbox } => {
				self.queue_expiry_owner_with_candidate_proof(*user_pubkey, *mailbox).await?;
				match self.expiry_owner(*user_pubkey, network).await? {
					Attribution::Owned { mailbox_pk, child_index, spk } =>
						Some(VerifiedPayee { mailbox: mailbox_pk, child_index, script: spk }),
					Attribution::Unresolved => return Ok(PreparedCoin::AwaitingAttribution),
					Attribution::NoRegisteredOwner => {
						warn!(vtxo_id = %ownership.id, "Expiry customer has no registered payout destination; no operator sweep authorized");
						return Ok(PreparedCoin::Hold);
					},
				}
			},
			OwnershipDisposition::Operator => None,
			OwnershipDisposition::Unresolved(reason) => {
				debug!(vtxo_id = %ownership.id, reason, "Expiry settlement remains unresolved");
				return Ok(if ownership.needs_chain_progress {
					PreparedCoin::NeedsHtlcProgress
				} else {
					PreparedCoin::Hold
				});
			},
		};
		Ok(PreparedCoin::Ready(ReadyCoin { ownership, payee }))
	}
}

impl Server {
	pub(crate) fn start_expiry_readiness(self: Arc<Self>) {
		let worker = self.rtmgr.spawn_critical("ExpiryReadiness");
		tokio::spawn(async move {
			let _worker = worker;
			let mut cursor = 0;
			while !self.rtmgr.shutdown_requested() {
				match self.expiry_readiness_pass(cursor).await {
					Ok(next) => cursor = next,
					Err(error) => warn!("Expiry readiness pass failed; no incomplete payout authorized: {:#}", error),
				}
				tokio::select! {
					_ = self.rtmgr.shutdown_signal() => break,
					_ = tokio::time::sleep(Duration::from_secs(1)) => {},
				}
			}
		});
	}

	pub(crate) async fn verify_readiness_view(&self, view: ExpiryViewToken) -> anyhow::Result<()> {
		for point in [view.captaind, view.watchmand] {
			ensure!(self.bitcoind.get_block_hash(point.height.into()).await? == point.hash,
				"expiry readiness view is no longer canonical");
		}
		Ok(())
	}

	async fn expiry_readiness_pass(&self, cursor: i64) -> anyhow::Result<i64> {
		// Token validation takes a shared row lock, which PostgreSQL forbids
		// in a read-only transaction. Release it before background preparation.
		let Some(view) = self.db.write(async |t| {
			let Some(view) = t.expiry_rebuild_token().await? else {
				return Ok(None);
			};
			t.lock_valid_expiry_view(view).await?;
			Ok(Some(view))
		}).await? else {
			return Ok(cursor);
		};
		self.verify_readiness_view(view).await?;
		let tip = i32::try_from(view.captaind.height.min(view.watchmand.height).to_u32())?;
		let rows = self.db.read(async |t| Ok(t.query(
			"
				SELECT v.id,v.vtxo
				FROM vtxo v
				WHERE v.id>$1 AND v.expiry<=$2
				AND v.frontier_at IS NOT NULL
				AND v.confirmed_height<=$2
				AND v.onchain_spent_height IS NULL
				AND NOT EXISTS(
					SELECT 1 FROM expiry_allocation_input i
					JOIN expiry_allocation a ON a.id=i.allocation_id
					WHERE i.vtxo_id=v.vtxo_id AND a.status<>'conflicted'
				)
				ORDER BY v.id
				LIMIT $3
			",
			&[&cursor, &tip, &ROOTS_PER_PASS],
		).await?)).await?;
		if rows.is_empty() {
			return Ok(0);
		}
		let next = rows.last().expect("nonempty above").get("id");
		let mut anchors = BTreeMap::<OutPoint, Vec<ServerVtxo>>::new();
		for row in rows {
			let root = ServerVtxo::deserialize(row.get("vtxo"))?;
			anchors.entry(root.chain_anchor()).or_default().push(root);
		}
		let mut ready = BTreeMap::<BlockHeight, Vec<ReadyOutput>>::new();
		let mut prepared_bytes = 0usize;
		for (anchor, roots) in anchors {
			if self.rtmgr.shutdown_requested() {
				break;
			}
			if let Err(error) = self.prepare_expiry_anchor(
				view, anchor, roots, &mut ready, &mut prepared_bytes
			).await {
				warn!(%anchor, %error, "Expiry anchor remains unready; other anchors continue");
			}
		}
		let mut committed = CommittedExpiryVersions::default();
		for outputs in ready.into_values() {
			let mut group = Vec::new();
			for output in outputs {
				let mut proposed = group.iter().collect::<Vec<_>>();
				proposed.push(&output);
				if !ReadyOutput::group_fits(&proposed)? && !group.is_empty() {
					self.commit_ready_group(view, std::mem::take(&mut group), &mut committed).await?;
				}
				if !ReadyOutput::group_fits(&[&output])? {
					self.db.write(async |t| t.request_expiry_split(view, output.root()).await).await?;
				} else {
					group.push(output);
				}
			}
			self.commit_ready_group(view, group, &mut committed).await?;
		}
		Ok(next)
	}

	pub(crate) async fn prepare_expiry_anchor(
		&self,
		view: ExpiryViewToken,
		anchor: OutPoint,
		roots: Vec<ServerVtxo>,
		ready: &mut BTreeMap<BlockHeight, Vec<ReadyOutput>>,
		prepared_bytes: &mut usize,
	) -> anyhow::Result<()> {
		let started = Instant::now();
		if self.expiry_roots_have_no_unspent_outputs(&roots).await {
			debug!(%anchor, roots = roots.len(), elapsed_secs = started.elapsed().as_secs_f64(),
				"Expiry preparation skipped absent physical roots");
			return Ok(());
		}
		// One immutable inventory per anchor per pass, including after a split.
		// Repeating the whole anchor SQL query once per child is quadratic work.
		let mut outputs = Vec::new();
		let snapshot = self.db.read(async |t| t.backing_snapshot(&roots, INVENTORY_LIMITS).await).await?;
		let funding = self.bitcoind.get_raw_transaction_verbosity_zero(&anchor.txid).await?.0;
		ensure!(funding.compute_txid() == anchor.txid, "expiry funding transaction differs from its anchor");
		// Path validation can traverse a large signed tree. Keep it outside
		// both database transactions and the async executor's worker threads.
		let (geometry, source_cut) = tokio::task::spawn_blocking(move || {
			let geometry = Arc::new(BackingGeometry::new(snapshot, funding, INVENTORY_LIMITS)?);
			let source = geometry.snapshot().regular.iter().find(|fact| fact.state.vtxo.point() == anchor)
				.context("expiry source cohort lacks its original backing anchor")?;
			let source_cut = geometry.economic_cut(&source.state.vtxo, INVENTORY_LIMITS)?;
			Ok::<_, anyhow::Error>((geometry, source_cut))
		}).await.context("expiry source validation task failed")??;
		let snapshot = geometry.snapshot();
		let funding = geometry.funding();
		debug!(%anchor, roots = roots.len(), rows = snapshot.regular.len(),
			terminals = source_cut.terminals.len(), elapsed_secs = started.elapsed().as_secs_f64(),
			"Expiry source geometry prepared");
		// Reject obsolete preparation before performing all owner lookups.
		// The decision and split transactions retain their final view locks.
		self.db.write(async |t| t.lock_valid_expiry_view(view).await).await?;
		if self.expiry_roots_have_no_unspent_outputs(&roots).await {
			debug!(%anchor, roots = roots.len(), elapsed_secs = started.elapsed().as_secs_f64(),
				"Expiry physical roots disappeared during source preparation");
			return Ok(());
		}
		let reserved = self.db.read(async |t| Ok(t.query("
			SELECT e.vtxo_id FROM expiry_entitlement e JOIN vtxo v ON v.vtxo_id=e.vtxo_id
			WHERE v.anchor_point=$1 AND (
				EXISTS(SELECT 1 FROM expiry_allocation_coin c
					JOIN expiry_allocation a ON a.id=c.allocation_id
					WHERE c.entitlement_id=e.id AND ((c.active AND a.status<>'conflicted') OR a.status='confirmed'))
				OR EXISTS(SELECT 1 FROM expiry_terminal_claim p WHERE p.entitlement_id=e.id))
		", &[&anchor.to_string()]).await?.into_iter().map(|row| row.get::<_, String>(0))
			.collect::<BTreeSet<_>>())).await?;
		let versions = source_cut.visited.iter().map(|index| {
			let fact = &snapshot.regular[*index];
			(fact.state.vtxo_id.to_string(), (fact.state.updated_at, fact.state.spend_state.as_str().to_owned(),
				fact.onchain_spent_height.is_some() || fact.onchain_spent_txid.is_some()))
		}).collect();
		let mut complete = true;
		let mut owners = BTreeMap::<bitcoin::secp256k1::PublicKey, BTreeSet<ark::VtxoId>>::new();
		let mut prepared = BTreeMap::new();
		let owner_preparation = Instant::now();
		for index in source_cut.terminals {
			let fact = &snapshot.regular[index];
			if fact.onchain_spent_height.is_some() || fact.onchain_spent_txid.is_some()
				|| reserved.contains(&fact.state.vtxo_id.to_string()) { continue; }
			if fact.frontier_at.is_some() {
				if let ServerVtxoPolicy::User(VtxoPolicy::Pubkey(policy)) = fact.state.vtxo.policy() {
					let owned = policy.user_pubkey == self.server_pubkey || self.db.read(async |t|
						t.fetch_ephemeral_tweak(policy.user_pubkey).await
					).await?.is_some();
					if !owned {
						if let Some(output) = self.bitcoind.try_get_tx_out(fact.state.vtxo.point(), true).await? {
							ensure!(output.confirmations > 0 && output.value == fact.state.vtxo.amount()
								&& output.script_pub_key.script()? == fact.state.vtxo.txout().script_pubkey,
								"expiry user exit differs from its confirmed output");
							// This right is already in the user's on-chain custody.
							// It cannot join a payout the server must sign.
							continue;
						}
					}
				}
			}
			if self.rtmgr.shutdown_requested() { return Ok(()); }
			let coin = self.db.write(async |t| t.prepare_expiry_coin(
				fact, &funding, view, self.config.network, self.config.expiry_internal_refund_depth,
			).await).await?;
			match &coin {
				PreparedCoin::Ready(coin) => if let Some(payee) = &coin.payee {
					owners.entry(payee.mailbox).or_default().insert(coin.ownership.id);
				},
				PreparedCoin::Hold | PreparedCoin::AwaitingAttribution | PreparedCoin::NeedsHtlcProgress => complete = false,
			}
			prepared.insert(fact.state.vtxo_id, coin);
		}
		debug!(%anchor, terminals = prepared.len(), owner_groups = owners.len(), complete,
			elapsed_secs = owner_preparation.elapsed().as_secs_f64(), "Expiry source owners prepared");
		for root in roots {
			let result = async {
				ensure!(root.server_pubkey() == self.server_pubkey, "expiry backing belongs to a different server key");
				if let ServerVtxoPolicy::User(VtxoPolicy::Pubkey(policy)) = root.policy() {
					let owned = policy.user_pubkey == self.server_pubkey || self.db.read(async |t|
						t.fetch_ephemeral_tweak(policy.user_pubkey).await
					).await?.is_some();
					if !owned {
						// This user exit has reached a leaf the server cannot sign.
						// Leave upstream's progress/exit handling intact; do not fence
						// the coin behind an allocation that can never be broadcast.
						return Ok(());
					}
				}
				let Some(output) = self.bitcoind.try_get_tx_out(root.point(), true).await? else {
					return Ok(());
				};
				ensure!(output.confirmations > 0 && output.value == root.amount()
					&& output.script_pub_key.script()? == root.txout().script_pubkey,
					"expiry physical backing differs from the confirmed output");
				let cut = {
					let geometry = Arc::clone(&geometry);
					let root = root.clone();
					// Building the current economic cut also traverses the complete
					// inventory. It must not starve unrelated async RPC work.
					tokio::task::spawn_blocking(move || geometry.economic_cut(&root, INVENTORY_LIMITS))
						.await.context("expiry economic cut task failed")??
				};
				let mut coins = Vec::with_capacity(cut.terminals.len());
				let mut hold = false;
				let mut awaiting_attribution = false;
				let mut needs_htlc_progress = false;
				for index in &cut.terminals {
					if self.rtmgr.shutdown_requested() {
						return Ok(());
					}
					// Release the registry lock between coins. A large tree must
					// not block registrations while preparing all of its owners.
					let fact = &snapshot.regular[*index];
					let coin = if let Some(coin) = prepared.remove(&fact.state.vtxo_id) { coin } else {
						self.db.write(async |t| t.prepare_expiry_coin(
							fact, &funding, view, self.config.network, self.config.expiry_internal_refund_depth,
						).await).await.with_context(|| format!("expiry ownership preparation for terminal {}", fact.state.vtxo_id))?
					};
					match coin {
						PreparedCoin::Ready(coin) => coins.push(coin),
						PreparedCoin::Hold => hold = true,
						PreparedCoin::AwaitingAttribution => awaiting_attribution = true,
						PreparedCoin::NeedsHtlcProgress => needs_htlc_progress = true,
					}
				}
				if hold || awaiting_attribution || needs_htlc_progress {
					// Separate held siblings only through signed steps that preserve
					// every branch's server recovery path, as checked by the split request.
					// Attribution can finish without spending this backing. Wait
					// for it before splitting a potentially payable owner group.
					if needs_htlc_progress || (hold && !awaiting_attribution && cut.terminals.len() > 1) {
						self.db.write(async |t| t.request_expiry_split(view, &root).await).await
							.context("expiry readiness split request")?;
					}
					return Ok(());
				}
				let bytes = cut.visited.iter().try_fold(0usize, |sum, index| {
					let fact = &snapshot.regular[*index];
					let size = fact.full_vtxo.as_ref().map_or_else(
						|| fact.state.vtxo.serialize().len(),
						|v| v.serialize().len()
					);
					sum.checked_add(size).context("expiry prepared inventory byte count overflow")
				})?;
				let next_bytes = prepared_bytes.checked_add(bytes).context("expiry pass byte count overflow")?;
				ensure!(next_bytes <= INVENTORY_LIMITS.max_serialized_bytes, "expiry readiness pass byte budget reached");
				let geometry = Arc::clone(&geometry);
				let proof_root = root.clone();
				let output = tokio::task::spawn_blocking(move || ReadyOutput::prove_with_geometry(
					&geometry, proof_root, &cut.visited, bytes, coins, &cut.residuals, INVENTORY_LIMITS,
				)).await.context("expiry ready output proof task failed")??;
				*prepared_bytes = next_bytes;
				outputs.push(output);
				Ok::<_, anyhow::Error>(())
			}.await;
			if let Err(error) = result {
				warn!(vtxo_id = %root.id(), %anchor, error = %format!("{error:#}"),
					"Expiry output remains unready; siblings continue");
			}
		}
		ReadyOutput::bind_source_cohort(&mut outputs, complete, owners, versions)?;
		for output in outputs {
			ready.entry(output.root().expiry_height()).or_default().push(output);
		}
		Ok(())
	}

	async fn expiry_roots_have_no_unspent_outputs(&self, roots: &[ServerVtxo]) -> bool {
		for root in roots {
			match self.bitcoind.try_get_tx_out(root.point(), true).await {
				Ok(None) => {},
				// An unavailable preflight leaves the existing preparation and
				// final Core, ownership and transaction checks in control.
				Ok(Some(_)) | Err(_) => return false,
			}
		}
		true
	}

	async fn commit_ready_group(
		&self, view: ExpiryViewToken, outputs: Vec<ReadyOutput>, committed: &mut CommittedExpiryVersions,
	) -> anyhow::Result<()> {
		let outputs = ReadyOutput::retain_complete_owner_groups(outputs);
		if outputs.is_empty() {
			return Ok(());
		}
		self.verify_readiness_view(view).await?;
		match self.db.write(async |t| t.commit_expiry_group(
			view,
			outputs,
			self.config.network,
			self.config.expiry_internal_refund_depth,
			committed,
		).await).await {
			Ok(Some(group)) => {
				// Db::write returns only after commit. An aborted decision must
				// never advance another group's source fence.
				committed.append(group.version_changes)?;
				info!(allocation_id = group.id, inputs = group.inputs.len(), dropped = group.dropped.len(),
					"Expiry payout decision committed");
			},
			Ok(None) => {},
			Err(error) => warn!(%error, "Expiry group was not committed; other ready groups continue"),
		}
		Ok(())
	}
}
