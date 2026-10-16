//! Schedule complete source-frozen groups. Ownership stays in captaind.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{FeeRate, Network, ScriptBuf};
use bitcoin_ext::BlockHeight;
use bitcoind_async_client::Client;
use bitcoind_async_client::traits::Reader;
use tracing::{info, warn};

use crate::database::{Db, carrier_snapshot::BackingSnapshotLimits};
use crate::payout_witness::DirectPayoutWitness;
use crate::utils::InstrumentedLock;
use crate::wallet::PersistedWallet;
use super::carrier::CarrierLimits;
use super::direct_delivery::DeliveryState;
use super::frozen_expiry::FrozenExpiryProcessor;
use super::WatchmanSigner;

struct Work {
	processor: FrozenExpiryProcessor,
	cursor: Option<String>,
}

pub(crate) struct DirectExpiryRuntime {
	db: Db,
	budget: BackingSnapshotLimits,
	work: tokio::sync::Mutex<Work>,
}

impl DirectExpiryRuntime {
	pub async fn new(
		db: Db, wallet: InstrumentedLock<PersistedWallet>, network: Network,
		operator_script: ScriptBuf, budget: BackingSnapshotLimits, policy: CarrierLimits,
		history: Arc<DirectPayoutWitness>, ordinary: super::ordinary_fee::OrdinaryFeeBudget,
	) -> anyhow::Result<Self> {
		let mut processor = FrozenExpiryProcessor::new(db.clone(), wallet, network,
			operator_script, budget, policy, history).with_ordinary_fee_budget(ordinary);
		// No daemon may construct new fee spends until every viable signed
		// version's existing input reservation is restored.
		processor.recover_durable_groups().await?;
		Ok(Self { db, budget, work: tokio::sync::Mutex::new(Work { processor, cursor: None }) })
	}

	pub async fn tick(&self, signer: &WatchmanSigner, node: &Client,
		rate: FeeRate, incremental: FeeRate, limit: usize,
	) -> anyhow::Result<()> {
		ensure!(limit > 0, "direct expiry work limit is zero");
		// The startup sync checkpoint can lag the mature backing. Signing with
		// that height would durably retain an unsatisfiable CLTV transaction.
		let height = BlockHeight::new(u32::try_from(node.get_block_count().await?)
			.context("direct expiry tip exceeds supported block height")?);
		let mut work = self.work.lock().await;
		work.processor.probe_publication().await?;
		work.processor.recover_durable_groups().await?;
		let mut groups = self.db.frozen_expiry_group_ids(self.budget.max_rows).await?
			.into_iter().collect::<BTreeSet<_>>();
		// Confirmation stops the database's pending listing. Keep the group
		// scheduled until the owning wallet sync permits reservation release.
		groups.extend(work.processor.tracked_group_ids().map(str::to_owned));
		let groups = groups.into_iter().collect::<Vec<_>>();
		let split = work.cursor.as_ref().map_or(0, |last| groups.partition_point(|id| id <= last));
		let selected = groups[split..].iter().chain(&groups[..split]).take(limit).cloned().collect::<Vec<_>>();
		let (mut submitted, mut confirmed, mut deferred) = (0, 0, 0);
		for id in selected {
			work.cursor = Some(id.clone());
			let result = async {
				if self.db.direct_delivery_generation(&id).await?.is_none() {
					work.processor.prepare_group(signer, node, &id, height, rate).await?;
				} else {
					work.processor.accelerate_group(signer, node, &id, height, rate, incremental).await?;
				}
				match work.processor.observe_and_submit(node, &id).await? {
					DeliveryState::Pending { .. } => { submitted += 1; },
					DeliveryState::Confirmed { .. } => {
						work.processor.release_confirmed_group(node, &id).await
							.context("confirmed direct group awaits owning-wallet sync")?;
						confirmed += 1;
					},
				}
				Ok::<_, anyhow::Error>(())
			}.await;
			if let Err(error) = result {
				deferred += 1;
				warn!(group = %id, error = %format!("{error:#}"), "direct expiry group deferred; backing never drains through legacy claims");
				if work.processor.retained_publication_id().is_some() { break; }
			}
		}
		info!(groups = groups.len(), submitted, confirmed, deferred, "direct expiry tick");
		Ok(())
	}
}
