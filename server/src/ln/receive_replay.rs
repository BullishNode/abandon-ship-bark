//! Resume only the original admitted receive claim, never a new payment.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use cln_rpc::plugins::hold::InvoiceState;
use tracing::warn;

use crate::Server;
use crate::database::ln::LightningHtlcSubscriptionStatus;

use super::{validate_receive_claim, RECEIVE_CLAIM_EXPIRY_MARGIN};

impl Server {
	pub(crate) fn start_receive_claim_recovery(self: Arc<Self>) {
		let worker = self.rtmgr.spawn_critical("ReceiveClaimRecovery");
		tokio::spawn(async move {
			let _worker = worker;
			while !self.rtmgr.shutdown_requested() {
				match self.db.write(async |t| t.pending_receive_requests(8).await).await {
					Ok(subscriptions) => for id in subscriptions {
						if self.rtmgr.shutdown_requested() { break; }
						// An ambiguous remote operation is retried from its durable
						// original request. It cannot block every later subscription.
						match tokio::time::timeout(Duration::from_secs(30), self.resume_receive_claim(id)).await {
							Ok(Ok(())) => {},
							Ok(Err(error)) => warn!(subscription_id=id, "Original receive claim remains pending: {:#}", error),
							Err(_) => warn!(subscription_id=id, "Original receive claim timed out; retaining its durable request"),
						}
					},
					Err(error) => warn!("Could not read pending original receive claims: {:#}", error),
				}
				tokio::select! {
					_ = self.rtmgr.shutdown_signal() => break,
					_ = tokio::time::sleep(Duration::from_secs(5)) => {},
				}
			}
		});
	}

	async fn resume_receive_claim(&self, subscription_id: i64) -> anyhow::Result<()> {
		let original = self.db.read(async |t| t.receive_claim_request(subscription_id).await).await?
			.context("original receive request disappeared")?;
		let _guard = self.payment_guards.lock(original.payment_hash).await;
		let subscription = self.db.read(async |t| t.get_htlc_subscription_by_id(subscription_id).await).await?
			.context("original receive subscription disappeared")?;
		ensure!(subscription.payment_hash == original.payment_hash, "stored receive subscription identity differs");
		if self.db.read(async |t| t.receive_request_materialized(subscription_id).await).await? { return Ok(()); }

		let internal = self.db.read(async |t| t.funded_internal_receive(subscription_id, original.payment_hash).await).await?.is_some();
		let margin = if internal { self.config.htlc_expiry_delta } else { RECEIVE_CLAIM_EXPIRY_MARGIN };
		let still_collectible = subscription.lowest_incoming_htlc_expiry.map(|expiry|
			validate_receive_claim(expiry, self.sync_manager.chain_tip().height, margin).is_ok()
		).unwrap_or(false);
		let mut collected = false;
		if !still_collectible || subscription.status == LightningHtlcSubscriptionStatus::Canceled {
			if internal {
				collected = self.db.read(async |t| t.internal_receive_collected(subscription_id, original.payment_hash).await).await?;
			} else {
				match self.lightning_manager.original_receive_invoice_state(&subscription).await? {
					Some(InvoiceState::Paid) => {
						// Actual Paid is irreversible evidence from the exact node
						// and invoice. Repair coarse state through the existing path.
						self.htlc_settler.settle(original.preimage).await?;
						self.lightning_manager.settle_invoice(subscription_id, original.preimage).await?;
						collected = true;
					},
					Some(InvoiceState::Cancelled) => {
						let stopped = self.db.write(async |t| t.stop_uncollected_receive_request(subscription_id).await).await?;
						ensure!(stopped, "cancelled invoice no longer meets uncollected source predicates; recheck actual claim state");
						return Ok(());
					},
					_ => {},
				}
			}
			ensure!(collected, "original receive is no longer safely collectible and has no permanent source-specific collection proof");
		}

		// Keep the original payment guard through the existing method's checks
		// of original bytes, source inputs, attestation, destination and fences.
		// No new-nonce public request receives the late-collection exception.
		self.claim_lightning_receive_inner(original.payment_hash, original.preimage,
			original.request, original.protocol_version, collected).await?;
		Ok(())
	}
}
