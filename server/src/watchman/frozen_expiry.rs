//! Complete frozen-group processing for an explicitly admitted startup caller.
//!
//! Not enabled by daemon configuration. The caller still needs independently
//! qualified recovery/current-source admission and financial operating limits.
//! Existing sweep/CTRL scheduling may invoke this component after those gates;
//! a failure never authorizes sending its backing through the legacy drain.

use std::collections::BTreeMap;

use anyhow::Context;
use bitcoin::{FeeRate, Network, ScriptBuf, Transaction, Txid};
use bitcoin_ext::{BlockHeight, rpc::BitcoinAsyncRpcExt};
use bitcoind_async_client::Client;

use crate::database::{Db, carrier_snapshot::BackingSnapshotLimits};
use crate::utils::{InstrumentedLock, InstrumentedOwnedLockGuard};
use crate::wallet::{BdkWalletExt, PersistedWallet, WalletUtxosGuard};
use super::WatchmanSigner;
use super::carrier::{build_direct_carrier, CarrierLimits};
use super::direct_delivery::{DeliveryState, DirectDelivery};

struct UncertainPublication {
	freeze_set: String,
	tx: Transaction,
	wallet: InstrumentedOwnedLockGuard<PersistedWallet>,
	reservation: Option<WalletUtxosGuard>,
}

pub struct FrozenExpiryProcessor {
	db: Db,
	wallet: InstrumentedLock<PersistedWallet>,
	network: Network,
	operator_script: ScriptBuf,
	budget: BackingSnapshotLimits,
	policy: CarrierLimits,
	deliveries: BTreeMap<String, DirectDelivery>,
	uncertain: Option<UncertainPublication>,
	hydrated: bool,
}

impl FrozenExpiryProcessor {
	pub fn new(
		db: Db, wallet: InstrumentedLock<PersistedWallet>, network: Network,
		operator_script: ScriptBuf, budget: BackingSnapshotLimits, policy: CarrierLimits,
	) -> Self {
		Self { db, wallet, network, operator_script, budget, policy,
			deliveries: BTreeMap::new(), uncertain: None, hydrated: false }
	}

	/// Restore every durable group's union reservation before new construction.
	/// This is journal hydration, not qualification of a stale restored database.
	pub async fn recover_durable_groups(&mut self) -> anyhow::Result<()> {
		ensure!(self.uncertain.is_none(), "uncertain publication retains the exclusive wallet; probe it first");
		self.hydrated = false;
		let groups = self.db.frozen_expiry_group_ids(self.budget.max_rows).await?;
		let held = self.wallet.lock_owned().await;
		for id in groups {
			if let Some(delivery) = self.deliveries.get(&id) {
				delivery.check_generation_frontier().await?;
				continue;
			}
			if self.db.direct_delivery_generation(&id).await?.is_none() { continue; }
			let delivery = DirectDelivery::recover_group(self.db.clone(), &held, &id,
				self.network, self.budget, self.policy).await?;
			self.deliveries.insert(id, delivery);
		}
		self.hydrated = true;
		Ok(())
	}

	/// Probe the complete committed handoff of these SAME signed bytes. Absence
	/// or a read error keeps the wallet guard/reservation and forbids new work.
	pub async fn probe_publication(&mut self) -> anyhow::Result<()> {
		let Some(held) = self.uncertain.as_mut() else { return Ok(()); };
		let versions = self.db.read_direct_delivery(&held.freeze_set, self.network, self.budget).await?;
		ensure!(versions.iter().any(|v| v.tx == held.tx), "uncertain signed carrier is not durably present; wallet retained");
		// All readback validation happened before releasing the original fee guard.
		// The exclusive wallet remains held throughout the reservation transfer.
		drop(held.reservation.take());
		let delivery = DirectDelivery::recover_group(self.db.clone(), &held.wallet, &held.freeze_set,
			self.network, self.budget, self.policy).await?;
		held.wallet.take_staged();
		self.deliveries.insert(held.freeze_set.clone(), delivery);
		self.uncertain = None;
		Ok(())
	}

	/// Sign and atomically publish one complete source-frozen group. No broadcast
	/// happens here; a killed caller restarts through durable-group recovery.
	pub async fn prepare_group(
		&mut self, signer: &WatchmanSigner, node: &Client, id: &str,
		height: BlockHeight, rate: FeeRate,
	) -> anyhow::Result<Txid> {
		ensure!(self.hydrated && self.uncertain.is_none(), "restore durable reservations and reconcile publication before construction");
		for delivery in self.deliveries.values() { delivery.check_generation_frontier().await?; }
		ensure!(!self.deliveries.contains_key(id) && self.db.direct_delivery_generation(id).await?.is_none(),
			"frozen group already has durable signed bytes; recover them without rebuilding principal");
		crate::bitcoind::require_network(node, self.network).await?;
		let group = self.db.read_frozen_expiry_group(id, self.network, self.budget).await?;
		let held = self.wallet.lock_owned().await;
		for root in &group.backing {
			let actual = node.try_get_tx_out(root.point(), true).await?.context("frozen backing is not currently unspent")?;
			ensure!(actual.confirmations > 0 && actual.value == root.amount() && actual.script_pub_key.hex == root.txout().script_pubkey.as_bytes(),
				"frozen backing differs from confirmed live output");
		}
		ensure!(self.db.direct_delivery_generation(id).await?.is_none(), "frozen generation changed before construction");
		let (mut held, carrier) = build_direct_carrier(signer, held, &group.backing,
			&group.plan.recipients, self.operator_script.clone(), height, rate, self.policy).await?;
		held.commit_tx(&carrier.tx);
		let outcome = self.db.write(async |t| t.store_frozen_expiry_carrier(id, &group.plan.allocation_ids,
			&group.backing, &carrier, &held, &self.operator_script, self.network, rate, self.policy, self.budget).await).await;
		let txid = carrier.tx.compute_txid();
		self.uncertain = Some(UncertainPublication { freeze_set: id.to_owned(), tx: carrier.tx,
			wallet: held, reservation: Some(carrier.fee_reservation) });
		outcome.context("signed carrier publication failed or COMMIT is unknown; same bytes and wallet retained")?;
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
		ensure!(held.latest_checkpoint().height() >= block.height.to_u32() && (!has_fee_inputs || held.transactions().any(|t|
			t.tx_node.txid == txid && t.chain_position.is_confirmed())), "owning wallet must sync the actual confirmed carrier before releasing fee inputs");
		self.deliveries.remove(id);
		Ok(())
	}
}
