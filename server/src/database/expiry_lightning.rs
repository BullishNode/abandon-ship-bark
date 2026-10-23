//! Read Lightning ownership from existing source links and upstream attempt status.

use std::collections::BTreeMap;

use ark::VtxoId;
use ark::lightning::PaymentHash;

use super::Tx;
use super::ln::LightningPaymentStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LightningSourceState {
	Unattempted,
	Failed,
	Pending,
	Discharged,
}

pub(crate) struct LightningPaymentOutcome {
	pub sources: Vec<VtxoId>,
	pub state: LightningSourceState,
}

fn source_state(statuses: impl IntoIterator<Item = LightningPaymentStatus>) -> LightningSourceState {
	let mut state = LightningSourceState::Unattempted;
	for status in statuses {
		match status {
			LightningPaymentStatus::Succeeded => return LightningSourceState::Discharged,
			LightningPaymentStatus::Requested | LightningPaymentStatus::Submitted => state = LightningSourceState::Pending,
			LightningPaymentStatus::Failed if state == LightningSourceState::Unattempted => state = LightningSourceState::Failed,
			LightningPaymentStatus::Failed => {},
		}
	}
	state
}

impl Tx<'_> {
	pub(crate) async fn lightning_source_state(
		&self, source: VtxoId, hash: PaymentHash,
	) -> anyhow::Result<LightningSourceState> {
		let rows = self.query("SELECT a.status FROM lightning_payment_attempt a \
			JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=a.id \
			WHERE l.vtxo_id=$1 AND a.payment_hash=$2", &[&source.to_string(), &hash.to_string()]).await?;
		Ok(source_state(rows.into_iter().map(|row| row.get("status"))))
	}

	/// Keep a complete existing source scope, without inferring from hash siblings.
	pub(crate) async fn lightning_payment_outcome(
		&self, hash: PaymentHash, source_ids: &[VtxoId], attempt_id: Option<i64>,
	) -> anyhow::Result<Option<LightningPaymentOutcome>> {
		let mut requested = source_ids.to_vec();
		requested.sort_unstable();
		requested.dedup();
		if requested.len() != source_ids.len() { return Ok(None); }
		let rows = self.query("SELECT a.id,a.status,l.vtxo_id FROM lightning_payment_attempt a \
			JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=a.id \
			JOIN vtxo v ON v.vtxo_id=l.vtxo_id JOIN htlc_vtxo h ON h.id=v.id \
			WHERE a.payment_hash=$1 AND ($2::BIGINT IS NULL OR a.id=$2) \
			AND h.payment_hash=a.payment_hash AND h.direction='incoming' ORDER BY a.id,l.vtxo_id",
			&[&hash.to_string(), &attempt_id]).await?;
		let mut attempts = BTreeMap::<i64, (LightningPaymentStatus, Vec<VtxoId>)>::new();
		for row in rows {
			let entry = attempts.entry(row.get("id")).or_insert_with(|| (row.get("status"), Vec::new()));
			entry.1.push(row.get::<_, &str>("vtxo_id").parse()?);
		}
		for (_, sources) in attempts.values_mut() { sources.sort_unstable(); }
		if requested.is_empty() {
			let Some((_, sources)) = attempts.values().next() else { return Ok(None); };
			if attempts.values().any(|(_, other)| other != sources) { return Ok(None); }
			requested = sources.clone();
		}
		let statuses = attempts.values().filter(|(_, sources)| *sources == requested)
			.map(|(status, _)| *status).collect::<Vec<_>>();
		if statuses.is_empty() { return Ok(None); }
		Ok(Some(LightningPaymentOutcome { sources: requested, state: source_state(statuses) }))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ownership_uses_only_existing_attempt_states() {
		use LightningPaymentStatus::*;
		assert_eq!(source_state([]), LightningSourceState::Unattempted);
		assert_eq!(source_state([Failed]), LightningSourceState::Failed);
		assert_eq!(source_state([Failed, Submitted]), LightningSourceState::Pending);
		assert_eq!(source_state([Requested, Failed]), LightningSourceState::Pending);
		assert_eq!(source_state([Failed, Succeeded]), LightningSourceState::Discharged);
	}
}
