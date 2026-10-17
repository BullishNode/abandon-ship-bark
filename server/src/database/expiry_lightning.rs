//! Economic discharge of a specific signed Lightning source.
//!
//! A preimage is shared by every grant using its hash. It does not prove that
//! each of those grants funded a payment. Keep that cryptographic knowledge in
//! the upstream WAL and bind financial conclusions to actual attempt links.

use ark::VtxoId;
use ark::lightning::PaymentHash;

use super::Tx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LightningSourceState {
	Unattempted,
	Failed,
	Pending,
	InternalCommitted,
	Discharged,
}

impl Tx<'_> {
	pub(crate) async fn lightning_source_state(
		&self, source: VtxoId, hash: PaymentHash,
	) -> anyhow::Result<LightningSourceState> {
		let attempts = self.query("SELECT a.status='failed' AS failed, \
			q.id IS NOT NULL AS queued, d.id IS NOT NULL AS dispatched, \
			f.id IS NOT NULL AS terminal_failure, s.id IS NOT NULL AS external_success, \
			i.id IS NOT NULL AS internal_funding, \
			(EXISTS(SELECT 1 FROM vtxo rv JOIN htlc_vtxo rh ON rh.id=rv.id \
				WHERE rv.lightning_htlc_subscription_id=i.subscription_id AND rh.direction='outgoing') \
			AND NOT EXISTS(SELECT 1 FROM vtxo rv JOIN htlc_vtxo rh ON rh.id=rv.id \
				WHERE rv.lightning_htlc_subscription_id=i.subscription_id AND rh.direction='outgoing' \
				AND rh.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled')) AS internal_claimed \
			FROM lightning_payment_attempt_htlc_vtxo l \
			JOIN lightning_payment_attempt a ON a.id=l.lightning_payment_attempt_id \
			LEFT JOIN expiry_lightning_queued_attempt q ON q.id=a.id \
			LEFT JOIN expiry_lightning_dispatch d ON d.id=a.id \
			LEFT JOIN expiry_lightning_terminal_failure f ON f.id=a.id \
			LEFT JOIN expiry_lightning_external_success s ON s.id=a.id \
			LEFT JOIN expiry_lightning_internal_funding i ON i.id=a.id \
			WHERE l.vtxo_id=$1 AND a.payment_hash=$2", &[&source.to_string(),&hash.to_string()]).await?;
		if attempts.is_empty() { return Ok(LightningSourceState::Unattempted); }
		let preimage_known = self.get_htlc_settlement_by_payment_hash(hash).await?.is_some();
		let mut pending = false;
		let mut internal = false;
		for attempt in attempts {
			if preimage_known && (attempt.get::<_, bool>("external_success")
				|| (attempt.get::<_, bool>("internal_funding")
					&& attempt.get::<_, bool>("internal_claimed"))) {
				return Ok(LightningSourceState::Discharged);
			}
			if attempt.get::<_, bool>("internal_funding") {
				internal = true;
				continue;
			}
			let never_dispatched = attempt.get::<_, bool>("queued")
				&& attempt.get::<_, bool>("failed") && !attempt.get::<_, bool>("dispatched");
			if !attempt.get::<_, bool>("terminal_failure") && !never_dispatched { pending = true; }
		}
		Ok(if pending { LightningSourceState::Pending }
			else if internal { LightningSourceState::InternalCommitted }
			else { LightningSourceState::Failed })
	}

	/// Return only subscriptions the requested source coins actually funded.
	/// A payment-hash lookup alone can refer to an unrelated external receive.
	pub(crate) async fn lightning_source_subscriptions(&self, sources: &[VtxoId]) -> anyhow::Result<Vec<i64>> {
		let sources = sources.iter().map(ToString::to_string).collect::<Vec<_>>();
		Ok(self.query("SELECT DISTINCT i.subscription_id FROM expiry_lightning_internal_funding i \
			JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=i.id \
			WHERE l.vtxo_id=ANY($1) ORDER BY i.subscription_id", &[&sources]).await?
			.into_iter().map(|row| row.get("subscription_id")).collect())
	}
}
