//! Read-only projection from the SAME locked fee owner. No transaction bytes leave it.
use super::super::ordinary_funding::FundingPurpose;
use super::*;
use bitcoin::{Txid, Wtxid};
use std::os::unix::fs::{FileExt, MetadataExt};

const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicFeeSnapshot {
	schema: u32,
	policy_sha256: sha256::Hash,
	opening_sha256: sha256::Hash,
	journal_sha256: sha256::Hash,
	journal_device: u64,
	journal_inode: u64,
	journal_bytes: u64,
	policy: OrdinaryFeePolicy,
	opening: OrdinaryFeeOpening,
	charged_sat: u64,
	remaining_total_sat: u64,
	urgent_remaining_sat: u64,
	discretionary_remaining_sat: u64,
	direct_attempt_count: u64,
	direct_reserved_sat: u64,
	funding_reserved_sat: u64,
	round_reserved_sat: u64,
	pool_reserved_sat: u64,
	urgent_reserved_sat: u64,
	funding: Vec<PublicFunding>,
	urgent: Vec<PublicUrgent>,
	resolutions: Vec<FundingResolution>,
}
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PublicFunding {
	index: u64,
	purpose: FundingPurpose,
	family_sha256: sha256::Hash,
	generation: u32,
	unsigned_body_sha256: sha256::Hash,
	prevouts_sha256: sha256::Hash,
	txid: Txid,
	signed_wtxid: Option<Wtxid>,
	ceiling_sat: u64,
}
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PublicUrgent {
	index: u64,
	ceiling_sat: u64,
	source_binding_sha256: Option<sha256::Hash>,
	transactions: Vec<PublicTransactionId>,
}
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PublicTransactionId {
	txid: Txid,
	wtxid: Wtxid,
}

fn same_file(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
	before.dev() == after.dev()
		&& before.ino() == after.ino()
		&& before.len() == after.len()
		&& before.mtime() == after.mtime()
		&& before.mtime_nsec() == after.mtime_nsec()
		&& before.ctime() == after.ctime()
		&& before.ctime_nsec() == after.ctime_nsec()
}

impl OrdinaryFeeBudget {
	/// Positioned read preserves the owning append FD and its FLOCK. Reparse every
	/// retained kind, then require exact equality to the live owner's memory.
	pub(crate) fn public_snapshot(&self) -> anyhow::Result<Vec<u8>> {
		let ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(!ledger.native_uncertain, "native fee outcome uncertain");
		let before = ledger.file.metadata()?;
		ensure!(
			before.is_file() && before.len() == ledger.length && before.len() <= MAX_HISTORY_BYTES,
			"fee journal changed or exceeds snapshot bound"
		);
		let mut raw = vec![0; usize::try_from(before.len())?];
		ledger.file.read_exact_at(&mut raw, 0)?;
		let after = ledger.file.metadata()?;
		ensure!(
			same_file(&before, &after),
			"fee journal changed during snapshot"
		);
		let parsed = parse_history(
			std::str::from_utf8(&raw)?,
			self.opening.initial_charge(self.policy)?,
			self.policy,
		)?;
		ensure!(
			parsed.charged == ledger.charged
				&& parsed.attempts == ledger.attempts
				&& parsed.urgent_charged == ledger.urgent_charged
				&& parsed.funding_charged == ledger.funding_charged
				&& parsed.resolutions == ledger.resolutions
				&& parsed.funding.len() == ledger.funding.len()
				&& parsed.urgent.len() == ledger.urgent.len(),
			"fee journal differs from owning memory"
		);
		for (key, state) in &parsed.funding {
			let loaded = ledger
				.funding
				.get(key)
				.context("native fee attempt absent from owner")?;
			ensure!(
				state.reservation.policy_sha256 == self.identity
					&& state.reservation == loaded.reservation
					&& state.signed == loaded.signed,
				"native fee attempt differs from owner"
			);
		}
		for (index, state) in &parsed.urgent {
			let loaded = ledger
				.urgent
				.get(index)
				.context("urgent attempt absent from owner")?;
			ensure!(
				state.ceiling == loaded.ceiling && state.signed == loaded.signed,
				"urgent attempt differs from owner"
			);
		}
		project_history(parsed, self.policy, &self.opening, self.identity, &raw, before.dev(), before.ino())
	}
}

/// Pure projection shared by the owning-FD snapshot and the offline codec.
/// Device/inode supplied here are emitted only by the live owner; the offline
/// entry removes those fields instead of claiming filesystem custody.
pub(super) fn project_history(
	parsed: ParsedHistory, policy: OrdinaryFeePolicy, opening: &OrdinaryFeeOpening,
	identity: sha256::Hash, raw: &[u8], device: u64, inode: u64,
) -> anyhow::Result<Vec<u8>> {
	let mut funding = Vec::new();
	let (mut round_reserved_sat, mut pool_reserved_sat) = (0u64, 0u64);
	for state in parsed.funding.values() {
		let candidate = &state.reservation.candidate;
		let ceiling_sat = state.reservation.fee()?.to_sat();
		let sum = match candidate.purpose {
			FundingPurpose::Round => &mut round_reserved_sat,
			FundingPurpose::Pool => &mut pool_reserved_sat,
		};
		*sum = sum
			.checked_add(ceiling_sat)
			.context("funding purpose total overflow")?;
		funding.push(PublicFunding {
			index: state.reservation.index,
			purpose: candidate.purpose,
			family_sha256: sha256::Hash::hash(&serde_json::to_vec(&candidate.family)?),
			generation: candidate.generation,
			unsigned_body_sha256: sha256::Hash::hash(&candidate.unsigned),
			prevouts_sha256: sha256::Hash::hash(&serde_json::to_vec(&candidate.prevouts)?),
			txid: candidate.transaction()?.compute_txid(),
			signed_wtxid: state.signed.as_ref().map(Transaction::compute_wtxid),
			ceiling_sat,
		});
	}
	funding.sort_by_key(|s| s.index);
	let urgent = parsed
		.urgent
		.iter()
		.map(|(index, state)| PublicUrgent {
			index: *index,
			ceiling_sat: state.ceiling,
			source_binding_sha256: state.signed.as_ref().map(|(binding, _)| *binding),
			transactions: state
				.signed
				.as_ref()
				.map(|(_, txs)| {
					txs.iter()
						.map(|tx| PublicTransactionId {
							txid: tx.compute_txid(),
							wtxid: tx.compute_wtxid(),
						})
						.collect()
				})
				.unwrap_or_default(),
		})
		.collect();
	let direct_reserved_sat = parsed
		.charged
		.checked_sub(opening.initial_charge(policy)?)
		.and_then(|n| n.checked_sub(parsed.urgent_charged))
		.and_then(|n| n.checked_sub(parsed.funding_charged))
		.context("invalid direct reservation total")?;
	let remaining_total_sat = policy
		.ordinary_budget_sat
		.checked_sub(parsed.charged)
		.context("invalid fee total")?;
	let urgent_remaining_sat = policy
		.urgent_reserve_sat
		.checked_sub(parsed.urgent_charged)
		.context("invalid urgent total")?;
	let snapshot = PublicFeeSnapshot {
		schema: 1,
		policy_sha256: identity,
		opening_sha256: sha256::Hash::hash(&serde_json::to_vec(opening)?),
		journal_sha256: sha256::Hash::hash(raw),
		journal_device: device,
		journal_inode: inode,
		journal_bytes: raw.len() as u64,
		policy,
		opening: opening.clone(),
		charged_sat: parsed.charged,
		remaining_total_sat,
		urgent_remaining_sat,
		discretionary_remaining_sat: remaining_total_sat
			.checked_sub(urgent_remaining_sat)
			.context("urgent authorization not retained")?,
		direct_attempt_count: parsed
			.attempts
			.checked_sub(u64::try_from(funding.len())?)
			.context("invalid direct count")?,
		direct_reserved_sat,
		funding_reserved_sat: parsed.funding_charged,
		round_reserved_sat,
		pool_reserved_sat,
		urgent_reserved_sat: parsed.urgent_charged,
		funding,
		urgent,
		resolutions: parsed.resolutions.into_values().collect(),
	};
	let result = serde_json::to_vec(&snapshot)?;
	ensure!(
		result.len() <= MAX_SNAPSHOT_BYTES,
		"fee snapshot exceeds complete transport bound"
	);
	Ok(result)
}
