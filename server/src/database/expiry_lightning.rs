//! Economic discharge of a specific signed Lightning source.
//!
//! A preimage is shared by every grant using its hash. It does not prove that
//! each of those grants funded a payment. Keep that cryptographic knowledge in
//! the upstream WAL and bind financial conclusions to actual attempt links.

use ark::VtxoId;
use ark::lightning::PaymentHash;

use super::Tx;
use super::ln::{LightningPaymentAttempt, LightningPaymentStatus};

/// Source-specific internal routing survives completion of the initiating
/// attempt. A current subscription sharing its hash is never routing evidence.
pub(crate) struct InternalReceiveFunding {
	pub attempt: LightningPaymentAttempt,
	pub claim_complete: bool,
}

// Only immutable off-chain grants count here. A preimage or an attempt status
// alone cannot establish that this subscription's recipient was paid/promised.
pub(crate) fn internal_settlement_is_ready(status: LightningPaymentStatus, own_claim_complete: bool) -> bool {
	!status.is_final() || own_claim_complete
}

const INTERNAL_RECEIVE_FUNDING: &str = "
	SELECT a.id,a.lightning_node_id,a.payment_hash,a.amount_msat,a.final_amount_msat,
		a.status,a.error,a.block_height,a.user_fee_sat,a.retry_for_secs,a.user_agent,
		a.lightning_htlc_subscription_id,a.created_at,a.updated_at,
		f.subscription_id AS funded_subscription_id,
		(EXISTS(SELECT 1 FROM htlc_settlement hs WHERE hs.payment_hash=a.payment_hash)
		AND EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			WHERE v.lightning_htlc_subscription_id=f.subscription_id AND h.direction='outgoing')
		AND NOT EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			WHERE v.lightning_htlc_subscription_id=f.subscription_id AND h.direction='outgoing'
			AND (h.payment_hash<>a.payment_hash OR h.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled')))
		AS claim_complete
	FROM expiry_lightning_internal_funding f
	JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id
	JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash
";


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LightningSourceState {
	Unattempted,
	Failed,
	Pending,
	InternalCommitted,
	Discharged,
}

impl Tx<'_> {
	pub(crate) async fn funded_internal_receive(
		&self, subscription_id: i64, hash: PaymentHash,
	) -> anyhow::Result<Option<InternalReceiveFunding>> {
		let query = format!("WITH funding AS ({INTERNAL_RECEIVE_FUNDING}) SELECT id,lightning_node_id,payment_hash, \
			amount_msat,final_amount_msat,status,error,block_height,user_fee_sat,retry_for_secs,user_agent, \
			lightning_htlc_subscription_id,created_at,updated_at,claim_complete FROM funding \
			WHERE funded_subscription_id=$1 AND payment_hash=$2");
		self.query_opt(&query, &[&subscription_id, &hash.to_string()]).await?.map(|row| {
			let claim_complete = row.get("claim_complete");
			Ok(InternalReceiveFunding { attempt: row.try_into()?, claim_complete })
		}).transpose()
	}

	/// Replay completed internal commitments after a missed status write. The
	/// bounded maintenance pass can run without the original client, new WAL
	/// entries, or the intentionally canceled native hold invoice.
	/// No row changes on an identical pass, so status/history remain idempotent.
	pub async fn reconcile_completed_internal_receives(&self, limit: i64) -> anyhow::Result<Vec<PaymentHash>> {
		ensure!((1..=128).contains(&limit), "invalid internal receive reconciliation limit");
		let query = format!("WITH funding AS ({INTERNAL_RECEIVE_FUNDING}), ready AS ( \
			SELECT s.id FROM lightning_htlc_subscription s JOIN funding f ON f.funded_subscription_id=s.id \
			WHERE s.status<>'settled' AND f.claim_complete ORDER BY s.id LIMIT $1 FOR UPDATE OF s SKIP LOCKED \
		) UPDATE lightning_htlc_subscription s SET status='settled',updated_at=NOW() FROM ready \
		WHERE s.id=ready.id AND s.status<>'settled' RETURNING s.payment_hash");
		self.query(&query, &[&limit]).await?.into_iter()
			.map(|row| Ok(row.get::<_, &str>("payment_hash").parse()?)).collect()
	}

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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn internal_receive_replay_requires_own_commitment_after_attempt_completion() {
		assert!(internal_settlement_is_ready(LightningPaymentStatus::Requested, false));
		assert!(internal_settlement_is_ready(LightningPaymentStatus::Submitted, false));
		assert!(!internal_settlement_is_ready(LightningPaymentStatus::Succeeded, false));
		assert!(!internal_settlement_is_ready(LightningPaymentStatus::Failed, false));
		assert!(internal_settlement_is_ready(LightningPaymentStatus::Succeeded, true));
		assert!(internal_settlement_is_ready(LightningPaymentStatus::Failed, true));
	}
}
