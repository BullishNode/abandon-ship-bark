//! Complete frozen-group processing for an explicitly admitted startup caller.
//!
//! Configured direct expiry scheduling uses this component after restoring its
//! independent history and fee reservations. Current-source admission and
//! financial operating limits remain separate qualification requirements.
//! Failure never authorizes sending its backing through the legacy drain.

use std::collections::BTreeMap;
use std::time::Duration;
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{Amount, FeeRate, Network, ScriptBuf, Txid};
use bitcoin_ext::{BlockHeight, rpc::BitcoinAsyncRpcExt};
use bitcoind_async_client::Client;
use bitcoind_async_client::traits::Reader;

use crate::database::{Db, carrier_snapshot::BackingSnapshotLimits, frozen_expiry::FrozenExpiryGroup};
use crate::payout_witness::DirectPayoutWitness;
use crate::utils::{InstrumentedLock, InstrumentedOwnedLockGuard};
use crate::wallet::{BdkWalletExt, PersistedWallet};
use super::WatchmanSigner;
use super::carrier::{build_direct_carrier_with_replacement, CarrierLimits, DirectCarrier};
use super::direct_delivery::{DeliveryState, DirectDelivery};

struct UncertainPublication {
	group: FrozenExpiryGroup,
	carrier: DirectCarrier,
	rate: FeeRate,
	wallet: InstrumentedOwnedLockGuard<PersistedWallet>,
}

pub struct FrozenExpiryProcessor {
	db: Db,
	witness: Arc<DirectPayoutWitness>,
	wallet: InstrumentedLock<PersistedWallet>,
	network: Network,
	operator_script: ScriptBuf,
	budget: BackingSnapshotLimits,
	policy: CarrierLimits,
	deliveries: BTreeMap<String, DirectDelivery>,
	uncertain: Option<UncertainPublication>,
	hydrated: bool,
	ordinary_fee_budget: Option<Arc<super::ordinary_fee::OrdinaryFeeBudget>>,
}

impl FrozenExpiryProcessor {
	pub fn new(
		db: Db, wallet: InstrumentedLock<PersistedWallet>, network: Network,
		operator_script: ScriptBuf, budget: BackingSnapshotLimits, policy: CarrierLimits, witness: Arc<DirectPayoutWitness>,
	) -> Self {
		Self { db, witness, wallet, network, operator_script, budget, policy,
			deliveries: BTreeMap::new(), uncertain: None, hydrated: false, ordinary_fee_budget: None }
	}

	pub(crate) fn with_ordinary_fee_budget(mut self, budget: Arc<super::ordinary_fee::OrdinaryFeeBudget>) -> Self {
		self.ordinary_fee_budget = Some(budget);
		self
	}

	/// Includes confirmed groups still retaining their owning-wallet fee guards.
	pub fn tracked_group_ids(&self) -> impl Iterator<Item = &str> {
		self.deliveries.keys().map(String::as_str)
	}

	/// Restore every durable group's union reservation before new construction.
	/// This is journal hydration, not qualification of a stale restored database.
	pub async fn recover_durable_groups(&mut self) -> anyhow::Result<()> {
		ensure!(self.uncertain.is_none(), "uncertain publication retains the exclusive wallet; probe it first");
		// A recorded winner can precede owning-wallet sync and process death.
		// Restore all groups at startup, including those no longer pending.
		let include_confirmed = !self.hydrated;
		self.hydrated = false;
		let groups = self.db.expiry_group_ids(self.budget.max_rows, include_confirmed).await?;
		let held = self.wallet.lock_owned().await;
		for id in groups {
			if let Some(delivery) = self.deliveries.get(&id) {
				delivery.check_generation_frontier().await?;
				continue;
			}
			if self.db.direct_delivery_generation(&id).await?.is_none() { continue; }
			let delivery = DirectDelivery::recover_group(self.db.clone(), &held, &id,
				self.network, self.budget, self.policy, self.witness.clone()).await?;
			self.deliveries.insert(id, delivery);
		}
		self.hydrated = true;
		Ok(())
	}

	/// Recover the complete handoff of these SAME signed bytes. A failed read is
	/// never permission to rebuild: retry the original atomic publication under
	/// its existing advisory lock, which waits for the previous COMMIT to finish.
	/// A bounded retry failure retains the original carrier and exclusive wallet.
	pub async fn probe_publication(&mut self) -> anyhow::Result<()> {
		let Some(held) = self.uncertain.as_mut() else { return Ok(()); };
		let id = &held.group.plan.id;
		let versions = match self.db.read_direct_delivery(id, self.network, self.budget).await {
			Ok(versions) => versions,
			Err(_) => {
				// No new signing, coin selection, fee calculation or stage reset.
				// A still-running original transaction holds this same carrier lock.
				tokio::time::timeout(Duration::from_secs(10), self.db.write(async |t|
					t.store_frozen_expiry_carrier(id, &held.group.plan.allocation_ids, &held.group.backing,
						&held.carrier, &held.wallet, &self.operator_script, self.network, held.rate,
						self.policy, self.budget).await)).await
					.context("retained publication retry timed out; same carrier and wallet retained")??;
				self.db.read_direct_delivery(id, self.network, self.budget).await?
			},
		};
		ensure!(versions.iter().any(|v| v.tx == held.carrier.tx), "uncertain signed carrier is not durably present; wallet retained");
		// All readback validation happened before releasing the original fee guard.
		// The exclusive wallet remains held throughout the reservation transfer.
		self.deliveries.remove(id);
		let original_reservation = std::mem::replace(&mut held.carrier.fee_reservation,
			held.wallet.lock_wallet_utxos(std::iter::empty())?);
		drop(original_reservation);
		let delivery = DirectDelivery::recover_group(self.db.clone(), &held.wallet, id,
			self.network, self.budget, self.policy, self.witness.clone()).await?;
		held.wallet.take_staged();
		self.deliveries.insert(id.clone(), delivery);
		self.uncertain = None;
		Ok(())
	}

	/// Public identifiers only; raw witnesses remain in the owning process.
	pub fn retained_publication_id(&self) -> Option<(Txid, bitcoin::Wtxid)> {
		self.uncertain.as_ref().map(|held| (held.carrier.tx.compute_txid(), held.carrier.tx.compute_wtxid()))
	}

	/// Sign and atomically publish one complete source-frozen group. No broadcast
	/// happens here; a killed caller restarts through durable-group recovery.
	pub async fn prepare_group(
		&mut self, signer: &WatchmanSigner, node: &Client, id: &str,
		height: BlockHeight, rate: FeeRate,
	) -> anyhow::Result<Txid> {
		self.prepare_version(signer, node, id, height, rate, None).await
	}

	pub(crate) async fn accelerate_group(
		&mut self, signer: &WatchmanSigner, node: &Client, id: &str,
		height: BlockHeight, rate: FeeRate, incremental: FeeRate,
	) -> anyhow::Result<()> {
		ensure!(self.hydrated && self.uncertain.is_none(), "reconcile publication and reservations before acceleration");
		let delivery = self.deliveries.get(id).context("replacement has no complete retained history")?;
		let Some(previous) = delivery.replacement_fee(node, rate).await? else { return Ok(()); };
		let relay = node.get_mempool_info().await?.incremental_relay_fee
			.context("Core incremental relay fee is unknown; retained versions deferred")?;
		let incremental = incremental.max(relay);
		ensure!(incremental > FeeRate::ZERO, "incremental replacement fee is zero");
		self.prepare_version(signer, node, id, height, rate, Some((previous, incremental))).await?;
		Ok(())
	}

	async fn prepare_version(
		&mut self, signer: &WatchmanSigner, node: &Client, id: &str,
		height: BlockHeight, rate: FeeRate, replacement: Option<(Amount, FeeRate)>,
	) -> anyhow::Result<Txid> {
		ensure!(self.hydrated && self.uncertain.is_none(), "restore durable reservations and reconcile publication before construction");
		let ordinary = self.ordinary_fee_budget.as_ref().context("native ordinary fee guard is not installed")?;
		for delivery in self.deliveries.values() { delivery.check_generation_frontier().await?; }
		let generation = self.db.direct_delivery_generation(id).await?;
		ensure!(if replacement.is_some() { self.deliveries.contains_key(id) && generation.is_some() }
			else { !self.deliveries.contains_key(id) && generation.is_none() },
			"construction requires either a fresh group or its complete retained replacement history");
		crate::bitcoind::require_network(node, self.network).await?;
		let group = self.db.read_frozen_expiry_group(id, self.network, self.budget).await?;
		let held = self.wallet.lock_owned().await;
		for root in &group.backing {
			// A replacement necessarily conflicts with its known pending versions.
			// Its complete original root still must exist on the active chain.
			let actual = node.try_get_tx_out(root.point(), replacement.is_none()).await?.context("frozen backing is not currently unspent")?;
			ensure!(actual.confirmations > 0 && actual.value == root.amount() && actual.script_pub_key.hex == root.txout().script_pubkey.as_bytes(),
				"frozen backing differs from confirmed live output");
		}
		ensure!(self.db.direct_delivery_generation(id).await? == generation, "frozen generation changed before construction");
		let (mut held, carrier) = build_direct_carrier_with_replacement(signer, held, &group.backing,
			&group.plan.recipients, self.operator_script.clone(), height, rate, self.policy, replacement, Some(ordinary)).await?;
		held.commit_tx(&carrier.tx);
		let txid = carrier.tx.compute_txid();
		// Retain before the publication await: cancelling its future must not
		// release the original bytes, staged wallet or exclusive reservation.
		self.uncertain = Some(UncertainPublication { group, carrier, rate, wallet: held });
		let retained = self.uncertain.as_ref().expect("publication retained above");
		self.db.write(async |t| t.store_frozen_expiry_carrier(id, &retained.group.plan.allocation_ids,
			&retained.group.backing, &retained.carrier, &retained.wallet, &self.operator_script,
			self.network, retained.rate, self.policy, self.budget).await).await
			.context("signed carrier publication failed or COMMIT is unknown; same bytes and wallet retained")?;
		self.probe_publication().await?;
		Ok(txid)
	}

	pub async fn observe_and_submit(&self, node: &Client, id: &str) -> anyhow::Result<DeliveryState> {
		ensure!(self.uncertain.is_none(), "uncertain publication must be probed before any submission");
		self.deliveries.get(id).context("frozen group has no recovered exact carrier")?.observe_and_submit(node).await
	}

	/// Release union fee guards only after an actual winner and owning-wallet
	/// confirmation sync; losing signed versions remain in the journal forever.
	pub async fn release_confirmed_group(&mut self, node: &Client, id: &str) -> anyhow::Result<()> {
		let state = self.observe_and_submit(node, id).await?;
		let DeliveryState::Confirmed { txid, block } = state else { bail!("direct group is still pending"); };
		let held = self.wallet.lock_owned().await;
		let has_fee_inputs = !self.deliveries.get(id).context("confirmed delivery disappeared")?.reserved_fee_inputs().is_empty();
		// A winner without wallet inputs/outputs is retained as a conflict of a
		// losing fee-funded version. transactions() hides such canonical txs.
		ensure!(held.latest_checkpoint().height() >= block.height.to_u32() && (!has_fee_inputs || held.get_tx(txid)
			.is_some_and(|t| t.chain_position.is_confirmed())), "owning wallet must sync the actual confirmed carrier before releasing fee inputs");
		self.deliveries.remove(id);
		Ok(())
	}
}
