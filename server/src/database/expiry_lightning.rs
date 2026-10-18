//! Economic discharge of a specific signed Lightning source.
//!
//! A preimage is shared by every grant using its hash. It does not prove that
//! each of those grants funded a payment. Keep that cryptographic knowledge in
//! the upstream WAL and bind financial conclusions to actual attempt links.

use anyhow::Context;
use ark::VtxoId;
use ark::lightning::{PaymentHash, Preimage};
use bitcoin_ext::{BlockDelta, BlockHeight};

use super::Tx;
use super::ln::LightningPaymentAttempt;

/// Source-specific internal routing survives completion of the initiating
/// attempt. A current subscription sharing its hash is never routing evidence.
pub(crate) struct InternalReceiveFunding {
	pub attempt: LightningPaymentAttempt,
	pub claim_complete: bool,
}

// Only immutable off-chain grants count here. A preimage or an attempt status
// alone cannot establish that this subscription's recipient was paid/promised.
pub(crate) fn internal_settlement_is_ready(own_claim_complete: bool) -> bool {
	own_claim_complete
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
	/// Exactly one internal attempt funded by this source has issued grants.
	InternalCommitted { attempt_id: i64, subscription_id: i64 },
	InternalZeroGrantCancelled { attempt_id: i64, subscription_id: i64 },
	Discharged,
}

impl Tx<'_> {
	/// An internal promotion never dispatches xpay. Once its exact receive is
	/// cancelled without EVER granting a receive coin, no recipient commitment
	/// exists. Do not infer this from the payer's coarse Failed state, a shared
	/// preimage, or expiration of signed grants (which is a different case).
	pub(crate) async fn internal_zero_grant_cancellation(
		&self, attempt_id: i64, hash: PaymentHash,
	) -> anyhow::Result<Option<i64>> {
		Ok(self.query_opt("SELECT f.subscription_id FROM expiry_lightning_internal_funding f \
			JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
			JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash \
			WHERE f.id=$1 AND a.payment_hash=$2 AND s.status='canceled' \
			AND NOT EXISTS(SELECT 1 FROM vtxo WHERE lightning_htlc_subscription_id=s.id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE subscription_id=s.id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_replay WHERE subscription_id=s.id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_dispatch WHERE id=a.id) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=a.id) \
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original \
				JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id \
				JOIN expiry_lightning_dispatch d ON d.id=other.lightning_payment_attempt_id \
				WHERE original.lightning_payment_attempt_id=a.id) \
			AND EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo WHERE lightning_payment_attempt_id=a.id) \
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l \
				LEFT JOIN vtxo v ON v.vtxo_id=l.vtxo_id LEFT JOIN htlc_vtxo h ON h.id=v.id \
				WHERE l.lightning_payment_attempt_id=a.id AND (h.direction::TEXT IS DISTINCT FROM 'incoming' \
				OR h.payment_hash IS DISTINCT FROM a.payment_hash OR h.offchain_resolution='fulfilled'))",
			&[&attempt_id,&hash.to_string()]).await?.map(|r| r.get("subscription_id")))
	}

	/// Lock the actual funding source and recipient grants together before a
	/// NEW internal claim publishes its preimage. A global WAL or LnSpent flag
	/// cannot replace this source-specific, still-collectible signed grant.
	pub(crate) async fn lock_internal_receive_claim(
		&self, subscription_id: i64, preimage: Preimage, receive_inputs: &[VtxoId], margin: BlockDelta,
	) -> anyhow::Result<()> {
		let hash = preimage.compute_payment_hash();
		let funding = self.funded_internal_receive(subscription_id, hash).await?
			.context("internal receive has no exact funding attempt")?;
		let rows = self.query("SELECT v.vtxo_id FROM lightning_payment_attempt_htlc_vtxo l \
			JOIN vtxo v ON v.vtxo_id=l.vtxo_id WHERE l.lightning_payment_attempt_id=$1 ORDER BY v.vtxo_id",
			&[&funding.attempt.id]).await?;
		let incoming = rows.into_iter().map(|r| r.get::<_, &str>("vtxo_id").parse())
			.collect::<Result<Vec<VtxoId>, _>>()?;
		ensure!(!incoming.is_empty() && !receive_inputs.is_empty(), "internal receive lacks signed sources");
		let mut all = incoming.iter().copied().chain(receive_inputs.iter().copied()).collect::<Vec<_>>();
		all.sort_unstable(); all.dedup();
		super::expiry_guard::guard_obligation_changes(self, &all).await?;
		let current = self.funded_internal_receive(subscription_id, hash).await?
			.context("internal receive funding disappeared")?;
		ensure!(current.attempt.id == funding.attempt.id, "internal funding attempt changed");
		if current.claim_complete || self.internal_receive_collected(subscription_id, hash).await? {
			return Ok(()); // Actual prior commitment, never a coarse attempt flag.
		}
		let rows = self.query("SELECT v.vtxo_id,v.spend_state::TEXT,h.direction::TEXT,h.payment_hash, \
			h.htlc_expiry,h.offchain_resolution::TEXT,v.onchain_spent_height,v.onchain_spent_txid \
			FROM lightning_payment_attempt_htlc_vtxo l JOIN vtxo v ON v.vtxo_id=l.vtxo_id \
			JOIN htlc_vtxo h ON h.id=v.id WHERE l.lightning_payment_attempt_id=$1 ORDER BY v.vtxo_id",
			&[&current.attempt.id]).await?;
		let actual = rows.iter().map(|r| r.get::<_, &str>("vtxo_id").parse())
			.collect::<Result<Vec<VtxoId>, _>>()?;
		ensure!(actual == incoming, "internal signed source set changed");
		let tip = self.query_one("SELECT COALESCE(MAX(height),0) FROM captaind_block", &[]).await?.get::<_, i64>(0);
		let last = BlockHeight::try_from(tip)?.checked_add(margin).context("internal receive collection margin overflows")?;
		for row in rows {
			ensure!(row.get::<_, &str>("direction") == "incoming" && row.get::<_, &str>("payment_hash") == hash.to_string(),
				"internal funding has a different signed source");
			ensure!(matches!(row.get::<_, &str>("spend_state"), "spendable"|"ln-spent")
				&& row.get::<_, Option<&str>>("offchain_resolution") != Some("revoked")
				&& row.get::<_, Option<i32>>("onchain_spent_height").is_none()
				&& row.get::<_, Option<&str>>("onchain_spent_txid").is_none(),
				"internal incoming source is not safely collectible");
			ensure!(last <= BlockHeight::try_from(row.get::<_, i32>("htlc_expiry"))?,
				"internal incoming source expired before claim materialization");
		}
		Ok(())
	}

	/// The WAL consumer may already have observed an unrelated shared preimage.
	/// Maintenance must also finish open attempts after their own real claim
	/// commits, even without a new WAL item or the original caller.
	pub(crate) async fn completed_internal_attempts(&self, limit: i64) -> anyhow::Result<Vec<InternalReceiveFunding>> {
		ensure!((1..=128).contains(&limit), "invalid completed internal attempt limit");
		let query = format!("WITH funding AS ({INTERNAL_RECEIVE_FUNDING}) SELECT id,lightning_node_id,payment_hash, \
			amount_msat,final_amount_msat,status,error,block_height,user_fee_sat,retry_for_secs,user_agent, \
			lightning_htlc_subscription_id,created_at,updated_at,claim_complete FROM funding \
			WHERE claim_complete AND status NOT IN ('failed','succeeded') ORDER BY id LIMIT $1");
		self.query(&query, &[&limit]).await?.into_iter().map(|row| {
			let claim_complete = row.get("claim_complete");
			Ok(InternalReceiveFunding { attempt: row.try_into()?, claim_complete })
		}).collect()
	}
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
		let attempts = self.query("SELECT a.id,a.status='failed' AS failed, \
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
		let mut internal = Vec::new();
		let mut cancelled_internal = None;
		for attempt in attempts {
			if preimage_known && (attempt.get::<_, bool>("external_success")
				|| (attempt.get::<_, bool>("internal_funding")
					&& attempt.get::<_, bool>("internal_claimed"))) {
				return Ok(LightningSourceState::Discharged);
			}
			if attempt.get::<_, bool>("internal_funding") {
				let attempt_id = attempt.get::<_, i64>("id");
				if let Some(subscription_id) = self.internal_zero_grant_cancellation(attempt_id, hash).await? {
					cancelled_internal = Some((attempt_id, subscription_id));
					continue;
				}
				internal.push(attempt_id);
				continue;
			}
			let never_dispatched = attempt.get::<_, bool>("queued")
				&& attempt.get::<_, bool>("failed") && !attempt.get::<_, bool>("dispatched");
			if !attempt.get::<_, bool>("terminal_failure") && !never_dispatched { pending = true; }
		}
		Ok(if pending { LightningSourceState::Pending }
			else if let [attempt_id] = internal[..] {
				let subscription_id = self.query_one("SELECT subscription_id FROM expiry_lightning_internal_funding WHERE id=$1",
					&[&attempt_id]).await?.get("subscription_id");
				LightningSourceState::InternalCommitted { attempt_id, subscription_id }
			}
			// Several internal attempts on one source never resolve automatically.
			else if !internal.is_empty() { LightningSourceState::Pending }
			else if let Some((attempt_id, subscription_id)) = cancelled_internal {
				LightningSourceState::InternalZeroGrantCancelled { attempt_id, subscription_id }
			}
			else { LightningSourceState::Failed })
	}

	/// The block at which every receive grant of an internal receive had been
	/// taken back through the server's timeout clause, at least `depth` blocks
	/// deep in both completed views. Any grant still open, collected off-chain,
	/// or claimed with the preimage keeps the sender's source unresolved.
	pub(crate) async fn internal_grants_reclaimed(
		&self, subscription_id: i64, hash: PaymentHash, tip: BlockHeight, watchman_tip: BlockHeight, depth: u32,
	) -> anyhow::Result<Option<BlockRef>> {
		if self.get_htlc_settlement_by_payment_hash(hash).await?.is_some() { return Ok(None); }
		let grants = self.query("SELECT h.payment_hash,h.direction::TEXT,h.offchain_resolution::TEXT, \
			h.chain_resolution::TEXT,h.chain_resolution_height FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
			WHERE v.lightning_htlc_subscription_id=$1 ORDER BY v.vtxo_id", &[&subscription_id]).await?;
		if grants.is_empty() { return Ok(None); }
		let completed = tip.min(watchman_tip).to_u32();
		let mut last = 0u32;
		for grant in &grants {
			let reclaimed = grant.get::<_, &str>("payment_hash") == hash.to_string()
				&& grant.get::<_, &str>("direction") == "outgoing"
				&& grant.get::<_, Option<&str>>("offchain_resolution").is_none()
				&& grant.get::<_, Option<&str>>("chain_resolution") == Some("revoked");
			let Some(height) = grant.get::<_, Option<i32>>("chain_resolution_height") else { return Ok(None); };
			let height = u32::try_from(height)?;
			if !reclaimed || height + depth.saturating_sub(1) > completed { return Ok(None); }
			last = last.max(height);
		}
		let block = self.get_block_by_height(super::BlockTable::Watchmand, BlockHeight::new(last)).await?
			.context("grant reclaim block is not in the completed watchman view")?;
		Ok(Some(block))
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
		assert!(!internal_settlement_is_ready(false));
		assert!(internal_settlement_is_ready(true));
	}
}
