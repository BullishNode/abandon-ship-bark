//! Recover and submit exact previously committed carriers, never construct one.
//!
//! This component is deliberately not wired into daemon startup or sweep loops.
//! The caller must first qualify independent stale-restore recovery, current
//! source/admission arbitration and the financial operating envelope. A durable
//! database alone does not prove those prerequisites. Hold the owning wallet
//! exclusively while recovering guards, before any new fee-input construction.

use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Network, Txid};
use bitcoin_ext::{BlockRef, TxStatus};
use bitcoind_async_client::Client;

use crate::bitcoind as bcd;
use crate::database::{Db, carrier_snapshot::BackingSnapshotLimits, direct_delivery::StoredCarrier};
use crate::wallet::{PersistedWallet, WalletKind, WalletUtxosGuard};
use super::carrier::CarrierLimits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
	Pending { txid: Txid },
	Confirmed { txid: Txid, block: BlockRef },
}

pub struct DirectDelivery {
	db: Db,
	freeze_set: String,
	versions: Vec<StoredCarrier>,
	// Every viable version's fee inputs stay reserved until the owner drops this
	// group after reconciliation. Confirmation never erases a signed version.
	fee_reservation: WalletUtxosGuard,
	policy: CarrierLimits,
}

impl DirectDelivery {
	pub async fn recover_group(
		db: Db, wallet: &PersistedWallet, freeze_set: &str, network: Network,
		budget: BackingSnapshotLimits, policy: CarrierLimits,
	) -> anyhow::Result<Self> {
		ensure!(wallet.kind() == WalletKind::Watchman && wallet.network() == network,
			"direct delivery requires the owning watchman wallet and network");
		ensure!(policy.max_fee_rate > bitcoin::FeeRate::ZERO && policy.max_weight.to_wu() > 0 &&
			policy.max_weight.to_wu() <= 400_000, "invalid direct delivery fee or weight limit");
		let versions = db.read_direct_delivery(freeze_set, network, budget).await?;
		let mut inputs = BTreeSet::new();
		for version in &versions {
			if !version.fee_inputs.is_empty() {
				ensure!(wallet.tx_graph().get_tx(version.tx.compute_txid()).is_some_and(|tx| *tx == version.tx),
					"direct delivery signed version is absent from durable wallet graph");
			}
			for (point, output) in &version.fee_inputs {
				let funding = wallet.tx_graph().get_tx(point.txid).context("direct fee funding is missing from owning wallet")?;
				ensure!(funding.output.get(point.vout as usize) == Some(output) && wallet.is_mine(output.script_pubkey.clone()),
					"direct delivery fee reservation is not wallet owned");
				inputs.insert(*point);
			}
		}
		let fee_reservation = wallet.lock_wallet_utxos(inputs)?;
		Ok(Self { db, freeze_set: freeze_set.to_owned(), versions, fee_reservation, policy })
	}

	pub fn reserved_fee_inputs(&self) -> &[bitcoin::OutPoint] { self.fee_reservation.utxos() }

	pub(crate) async fn check_generation_frontier(&self) -> anyhow::Result<()> {
		ensure!(self.db.direct_delivery_generation(&self.freeze_set).await? == self.versions.iter().map(|v| v.generation).max(),
			"direct delivery generation frontier changed; recover the complete group before submission; reservations retained");
		Ok(())
	}

	/// One bounded observation/submission pass. A timeout/error retains guards;
	/// reconcile these same bytes on the next call rather than creating payment.
	pub async fn observe_and_submit(&self, node: &Client) -> anyhow::Result<DeliveryState> {
		self.check_generation_frontier().await?;
		let mut confirmed = Vec::new();
		let mut pending = BTreeSet::new();
		for version in &self.versions {
			let txid = version.tx.compute_txid();
			match bcd::tx_status(node, txid).await? {
				TxStatus::Confirmed(block) => confirmed.push((version, block)),
				TxStatus::Mempool => { pending.insert(txid); },
				TxStatus::NotFound => {},
			}
			ensure!(version.recorded_confirmation.is_none() || confirmed.iter().any(|(v,b)|
				v.tx.compute_txid() == txid && Some(*b) == version.recorded_confirmation),
				"recorded direct confirmation is absent or different on the active chain");
		}
		ensure!(confirmed.len() <= 1, "multiple physical carrier versions are confirmed");
		if let Some((winner, block)) = confirmed.into_iter().next() {
			self.db.confirm_direct_delivery(&self.freeze_set, winner, block).await?;
			return Ok(DeliveryState::Confirmed { txid: winner.tx.compute_txid(), block });
		}
		let generation = self.versions.iter().map(|v| v.generation).max().context("empty direct delivery group")?;
		let latest = self.versions.iter().filter(|v| v.generation == generation).collect::<Vec<_>>();
		ensure!(latest.len() == 1, "direct carrier generation is ambiguous; caller reconciliation required");
		let latest = latest[0];
		let txid = latest.tx.compute_txid();
		if pending.contains(&txid) { return Ok(DeliveryState::Pending { txid }); }
		ensure!(latest.tx.weight() <= self.policy.max_weight && latest.miner_fee <= self.policy.max_miner_fee &&
			latest.miner_fee <= self.policy.max_fee_rate.fee_wu(latest.tx.weight()).context("direct delivery fee ceiling overflow")?,
			"durable carrier exceeds current delivery limits; fee reservations retained");
		// An older version still in the mempool is not recovery of a newer durable
		// replacement that crashed before submission. Core decides replacement
		// admission; no customer output or fee input is reconstructed here.
		self.check_generation_frontier().await?;
		bcd::broadcast_tx(node, &latest.tx).await.with_context(||
			format!("exact durable carrier {txid} submission failed; observed older pending versions: {pending:?}; reservations retained"))?;
		match bcd::tx_status(node, txid).await? {
			TxStatus::Confirmed(block) => {
				self.db.confirm_direct_delivery(&self.freeze_set, latest, block).await?;
				Ok(DeliveryState::Confirmed { txid, block })
			},
			TxStatus::Mempool => Ok(DeliveryState::Pending { txid }),
			TxStatus::NotFound => bail!("submitted direct carrier outcome is unknown; reservations retained"),
		}
	}
}
