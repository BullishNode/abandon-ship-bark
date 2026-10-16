//! Reciprocal metadata for the SAME owning fee journal's original urgent
//! attempts. These methods grant no source eligibility, signatures or relay.
//! DIRECT entry points remain held until the caller's source/input/nursery
//! transaction and startup restoration are complete and qualified.
use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{OutPoint, Transaction};
use bitcoin::consensus::serialize;
use bitcoin_ext::BlockHeight;
use bitcoin::hashes::{sha256, Hash};

use super::Tx;
use crate::watchman::ordinary_fee::{OrdinaryFeeBudget, OrdinaryFeePolicy};
use crate::watchman::ordinary_fee_auth::FeeServiceIdentity;
use crate::watchman::urgent_publication::{BoundUrgentPublication, UrgentCandidate, UrgentReservationEntry, UrgentSignedEntry};

const MAX_ATTEMPTS: i64 = 4096;
const MAX_INPUTS: i64 = 50_000;
const MAX_TOTAL_BYTES: i64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct UrgentSqlIntent {
	index: i64,
	candidate_hash: String,
	publication_txid: String,
	purpose: String,
	policy_hash: String,
	opening_hash: String,
	reservation_entry: Vec<u8>,
	candidate: Vec<u8>,
	signed_entry: Option<Vec<u8>>,
	signed_record: Option<Vec<u8>>,
}
impl UrgentSqlIntent {
	fn original(candidate: &UrgentCandidate, signed: Option<&BoundUrgentPublication>,
		identity: &FeeServiceIdentity, policy: OrdinaryFeePolicy,
	) -> anyhow::Result<Self> {
		let entry = candidate.reservation_entry(policy)?; entry.require_identity(identity)?;
		let (signed_entry, signed_record) = match signed {
			Some(publication) => {
				let (matched, signed) = publication.signed_entry(identity, candidate.limits(), policy)?;
				ensure!(matched == entry, "urgent SQL signed package differs from original candidate");
				(Some(serde_json::to_vec(&signed)?), Some(serde_json::to_vec(publication)?))
			},
			None => (None, None),
		};
		Ok(Self { index: i64::try_from(entry.attempt_index)?, candidate_hash: entry.candidate_sha256.to_string(),
			publication_txid: entry.publication_txid.to_string(), purpose: candidate.purpose_name().into(),
			policy_hash: identity.policy_sha256.to_string(), opening_hash: identity.opening_sha256.to_string(),
			reservation_entry: serde_json::to_vec(&entry)?, candidate: serde_json::to_vec(candidate)?, signed_entry, signed_record })
	}
	fn decode(&self, identity: &FeeServiceIdentity, policy: OrdinaryFeePolicy)
		-> anyhow::Result<(UrgentCandidate, Option<BoundUrgentPublication>)> {
		ensure!(self.reservation_entry.len() <= 4096 && self.candidate.len() <= 4 * 1024 * 1024
			&& self.signed_entry.as_ref().is_none_or(|raw| raw.len() <= 4096)
			&& self.signed_record.as_ref().is_none_or(|raw| raw.len() <= 4 * 1024 * 1024), "urgent SQL body exceeds original bound");
		let original = UrgentReservationEntry::parse(std::str::from_utf8(&self.reservation_entry)?, policy)?;
		let candidate = UrgentCandidate::decode_reserved(&self.candidate, &original, policy)?;
		let publication = match (&self.signed_entry, &self.signed_record) {
			(Some(entry), Some(raw)) => {
				let entry = UrgentSignedEntry::parse(std::str::from_utf8(entry)?, &original)?;
				Some(BoundUrgentPublication::decode(raw, entry.record_sha256, identity, candidate.limits(), policy)?)
			},
			(None, None) => None,
			_ => bail!("urgent SQL signed entry and original package are incomplete"),
		};
		let expected = Self::original(&candidate, publication.as_ref(), identity, policy)?;
		self.require_exact(&expected)?;
		Ok((candidate, publication))
	}
	fn require_exact(&self, expected: &Self) -> anyhow::Result<()> {
		ensure!(self == expected, "urgent original DB body, signed state, purpose or identity differs from independent journal");
		Ok(())
	}
	fn from_row(row: &tokio_postgres::Row) -> Self {
		Self { index: row.get("attempt_index"), candidate_hash: row.get("candidate_hash"),
			publication_txid: row.get("publication_txid"), purpose: row.get("purpose"),
			policy_hash: row.get("policy_hash"), opening_hash: row.get("opening_hash"),
			reservation_entry: row.get("reservation_entry"), candidate: row.get("candidate"),
			signed_entry: row.get("signed_entry"), signed_record: row.get("signed_record") }
	}
}
fn require_nursery_tuple(signed: &Transaction, kind: &str, raw: &[u8], abandoned: bool) -> anyhow::Result<()> {
	ensure!(kind == "native-urgent" && raw == serialize(signed) && !abandoned,
		"urgent original nursery body, class or retained state differs");
	Ok(())
}
const COLUMNS: &str = "attempt_index,candidate_hash,publication_txid,purpose,policy_hash,opening_hash,reservation_entry,candidate,signed_entry,signed_record";

impl Tx<'_> {
	async fn urgent_intent_bounds(&self) -> anyhow::Result<()> {
		let row = self.query_one("SELECT count(*),COALESCE(sum(octet_length(candidate)+octet_length(reservation_entry)
			+COALESCE(octet_length(signed_entry),0)+COALESCE(octet_length(signed_record),0)),0)::bigint FROM native_urgent_attempt", &[]).await?;
		ensure!(row.get::<_, i64>(0) <= MAX_ATTEMPTS && row.get::<_, i64>(1) <= MAX_TOTAL_BYTES,
			"urgent original source exceeds bounded complete admission");
		ensure!(self.query_one("SELECT count(*) FROM native_urgent_input", &[]).await?.get::<_, i64>(0) <= MAX_INPUTS,
			"urgent retained whole-input source exceeds bound");
		Ok(())
	}
	async fn require_urgent_opening(&self, identity: &FeeServiceIdentity, policy: OrdinaryFeePolicy) -> anyhow::Result<()> {
		let row = self.query_opt("SELECT opening_hash,policy_bytes FROM native_fee_opening WHERE id=1", &[]).await?
			.context("urgent original source lacks actual existing fee opening")?;
		let policy_bytes: Vec<u8> = row.get(1);
		ensure!(row.get::<_, String>(0) == identity.opening_sha256.to_string()
			&& policy_bytes == serde_json::to_vec(&policy)? && sha256::Hash::hash(&policy_bytes) == identity.policy_sha256,
			"urgent original DB opening or same-ledger policy differs");
		Ok(())
	}
	async fn require_urgent_input_set(&self, index: i64, expected: &BTreeSet<OutPoint>) -> anyhow::Result<()> {
		ensure!(!expected.is_empty() && expected.len() <= 8192, "urgent external input union is empty or exceeds original package bound");
		let rows = self.query("SELECT outpoint FROM native_urgent_input WHERE attempt_index=$1 ORDER BY outpoint LIMIT 8193", &[&index]).await?;
		let actual = rows.into_iter().map(|row| row.get::<_, String>(0)).collect::<BTreeSet<_>>();
		let expected = expected.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
		ensure!(actual == expected, "urgent whole external input set differs from original paid candidate");
		let conflict = self.query_one("SELECT EXISTS(SELECT 1 FROM native_urgent_input u JOIN native_fee_input n USING(outpoint)
			WHERE u.attempt_index=$1)", &[&index]).await?.get::<_, bool>(0);
		ensure!(!conflict, "urgent original input overlaps a retained round/pool publication");
		Ok(())
	}
	/// Caller retains owning Watchman then source locks 1/2/3/4, validates the
	/// actual native action/Core state and fsyncs u1 BEFORE calling. A failed or
	/// unknown COMMIT must poison that owner and retain inputs; no re-signing.
	pub(crate) async fn stage_native_urgent_intent(&self, candidate: &UrgentCandidate,
		budget: &OrdinaryFeeBudget, identity: &FeeServiceIdentity,
	) -> anyhow::Result<()> {
		self.lock_native_fee_publication().await?; self.urgent_intent_bounds().await?;
		budget.require_urgent_candidate(candidate)?;
		let original = UrgentSqlIntent::original(candidate, None, identity, budget.policy())?;
		self.require_urgent_opening(identity, budget.policy()).await?;
		self.execute("INSERT INTO native_urgent_attempt(attempt_index,candidate_hash,publication_txid,purpose,policy_hash,opening_hash,reservation_entry,candidate)
			VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(attempt_index) DO NOTHING",
			&[&original.index,&original.candidate_hash,&original.publication_txid,&original.purpose,&original.policy_hash,&original.opening_hash,
				&original.reservation_entry,&original.candidate]).await?;
		let query = format!("SELECT {COLUMNS} FROM native_urgent_attempt WHERE attempt_index=$1 FOR UPDATE");
		let existing = UrgentSqlIntent::from_row(&self.query_one(&query, &[&original.index]).await?);
		existing.require_exact(&original)?;
		let inputs = candidate.external_inputs(budget.policy())?;
		for point in &inputs {
			let point = point.to_string();
			ensure!(self.query_opt("SELECT wtxid FROM native_fee_input WHERE outpoint=$1", &[&point]).await?.is_none(),
				"urgent external input already retained by ordinary native publication");
			self.execute("INSERT INTO native_urgent_input(outpoint,attempt_index) VALUES($1,$2) ON CONFLICT(outpoint) DO NOTHING",
				&[&point,&original.index]).await?;
			ensure!(self.query_one("SELECT attempt_index FROM native_urgent_input WHERE outpoint=$1", &[&point]).await?.get::<_, i64>(0) == original.index,
				"urgent whole external input already retained by another attempt");
		}
		self.require_urgent_input_set(original.index, &inputs).await?; self.urgent_intent_bounds().await?;
		Ok(())
	}
	/// Private signed bytes already fsynced as u2 bind to the same SQL attempt
	/// once. This neither marks publication nor inserts/reconstructs nursery or
	/// customer rows. The actual atomic publisher is deliberately still held.
	pub(crate) async fn bind_native_urgent_signed(&self, candidate: &UrgentCandidate,
		publication: &BoundUrgentPublication, budget: &OrdinaryFeeBudget, identity: &FeeServiceIdentity,
	) -> anyhow::Result<()> {
		self.lock_native_fee_publication().await?; self.urgent_intent_bounds().await?;
		budget.require_urgent_candidate(candidate)?; budget.require_urgent_signed(publication)?;
		self.require_urgent_opening(identity, budget.policy()).await?;
		let unsigned = UrgentSqlIntent::original(candidate, None, identity, budget.policy())?;
		let signed = UrgentSqlIntent::original(candidate, Some(publication), identity, budget.policy())?;
		let query = format!("SELECT {COLUMNS} FROM native_urgent_attempt WHERE attempt_index=$1 FOR UPDATE");
		let before = UrgentSqlIntent::from_row(&self.query_opt(&query, &[&signed.index]).await?
			.context("urgent signed DB tuple lacks original pre-sign input retention")?);
		ensure!(before == unsigned || before == signed, "urgent signed DB binding conflicts with retained original");
		self.require_urgent_input_set(signed.index, &candidate.external_inputs(budget.policy())?).await?;
		self.execute("UPDATE native_urgent_attempt SET signed_entry=$2,signed_record=$3 WHERE attempt_index=$1 AND signed_entry IS NULL",
			&[&signed.index,&signed.signed_entry,&signed.signed_record]).await?;
		UrgentSqlIntent::from_row(&self.query_one(&query, &[&signed.index]).await?).require_exact(&signed)?;
		self.urgent_intent_bounds().await?;
		Ok(())
	}
	/// Bind private original signed package and nursery in the SAME SQL
	/// transaction. There is no relay here; each later relay additionally needs
	/// the independent owning journal. A lost COMMIT cannot authorize re-signing.
	pub(crate) async fn publish_native_urgent_intent(&self, candidate: &UrgentCandidate,
		publication: &BoundUrgentPublication, budget: &OrdinaryFeeBudget, identity: &FeeServiceIdentity,
		confirm_target: BlockHeight,
	) -> anyhow::Result<()> {
		self.bind_native_urgent_signed(candidate, publication, budget, identity).await?;
		let signed = publication.last_transaction()?;
		self.upsert_nursery_tx(&signed, crate::nursery::NurseryTxKind::NativeUrgent, confirm_target).await?;
		self.require_urgent_nursery(&signed).await
	}
	pub(crate) async fn bind_native_urgent_nursery(&self, signed: &Transaction) -> anyhow::Result<()> {
		self.lock_native_fee_publication().await?; self.urgent_intent_bounds().await?;
		let query = format!("SELECT {COLUMNS} FROM native_urgent_attempt WHERE publication_txid=$1 FOR UPDATE");
		let retained = UrgentSqlIntent::from_row(&self.query_opt(&query, &[&signed.compute_txid().to_string()]).await?
			.context("urgent nursery candidate lacks original unsigned and signed DB intent")?);
		let opening = self.query_opt("SELECT opening_hash,policy_bytes FROM native_fee_opening WHERE id=1", &[]).await?
			.context("urgent nursery candidate lacks original fee opening")?;
		let policy_bytes: Vec<u8> = opening.get(1);
		ensure!(policy_bytes.len() <= 4096, "urgent nursery policy exceeds original bound");
		let policy: OrdinaryFeePolicy = serde_json::from_slice(&policy_bytes)?;
		ensure!(serde_json::to_vec(&policy)? == policy_bytes, "urgent nursery policy is not original canonical bytes");
		let original = UrgentReservationEntry::parse(std::str::from_utf8(&retained.reservation_entry)?, policy)?;
		let candidate = UrgentCandidate::decode_reserved(&retained.candidate, &original, policy)?;
		let identity = candidate.identity();
		ensure!(identity.policy_sha256 == sha256::Hash::hash(&policy_bytes)
			&& identity.opening_sha256.to_string() == opening.get::<_, String>(0), "urgent nursery original opening identity differs");
		let (candidate, publication) = retained.decode(&identity, policy)?;
		let publication = publication.context("urgent nursery candidate has no original signed binding")?;
		ensure!(publication.last_transaction()? == *signed, "urgent nursery body differs from original signed package");
		self.require_urgent_input_set(retained.index, &candidate.external_inputs(policy)?).await?;
		Ok(())
	}
	async fn require_urgent_nursery(&self, signed: &Transaction) -> anyhow::Result<()> {
		let row = self.query_opt("SELECT kind::text,tx,abandoned_at IS NOT NULL FROM nursery_tx WHERE txid=$1 FOR UPDATE",
			&[&signed.compute_txid().to_string()]).await?.context("urgent original signed package lost its nursery tuple")?;
		require_nursery_tuple(signed, &row.get::<_, String>(0), &row.get::<_, Vec<u8>>(1), row.get::<_, bool>(2))
	}
	/// Exact complete journal-to-SQL sets, including UNSIGNED paid attempts.
	/// No original input is released and no unbroadcast change is credited.
	/// This returns exclusions only; activation/publication still needs the
	/// separate native action, chain, nursery and uncertain-COMMIT checks.
	pub(crate) async fn retained_native_urgent_inputs(&self, budget: &OrdinaryFeeBudget,
		identity: &FeeServiceIdentity,
	) -> anyhow::Result<BTreeSet<OutPoint>> {
		self.lock_native_fee_publication().await?; self.urgent_intent_bounds().await?;
		self.require_urgent_opening(identity, budget.policy()).await?;
		let rows = self.query(&format!("SELECT {COLUMNS} FROM native_urgent_attempt ORDER BY attempt_index LIMIT 4097"), &[]).await?;
		let retained = budget.retained_urgent_publications()?;
		ensure!(rows.len() == retained.len() && rows.len() <= MAX_ATTEMPTS as usize,
			"urgent complete journal and DB attempt sets differ");
		let mut inputs = BTreeSet::new(); let mut signed_count = 0i64;
		for (row, (candidate, publication)) in rows.iter().zip(retained) {
			let expected = UrgentSqlIntent::original(&candidate, publication.as_ref(), identity, budget.policy())?;
			UrgentSqlIntent::from_row(row).require_exact(&expected)?;
			if let Some(publication) = &publication {
				self.require_urgent_nursery(&publication.last_transaction()?).await?; signed_count += 1;
			} else {
				ensure!(self.query_opt("SELECT txid FROM nursery_tx WHERE txid=$1", &[&expected.publication_txid]).await?.is_none(),
					"unsigned urgent attempt already has an unbound nursery body");
			}
			let expected_inputs = candidate.external_inputs(budget.policy())?;
			self.require_urgent_input_set(expected.index, &expected_inputs).await?;
			for point in expected_inputs { ensure!(inputs.insert(point), "urgent retained attempts overlap an original input"); }
		}
		ensure!(self.query_one("SELECT count(*) FROM native_urgent_input", &[]).await?.get::<_, i64>(0) == i64::try_from(inputs.len())?,
			"urgent complete input set has an orphan, duplicate or missing row");
		ensure!(self.query_one("SELECT count(*) FROM nursery_tx WHERE kind='native-urgent'", &[]).await?.get::<_, i64>(0) == signed_count,
			"urgent nursery set contains an unbound or missing original package");
		Ok(inputs)
	}
}


#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::{Amount, FeeRate, Weight};
	use bitcoin_ext::TransactionExt;
	use crate::watchman::carrier::CarrierLimits;
	use crate::watchman::urgent_publication::synthetic_urgent_vector;

	fn limits() -> CarrierLimits {
		CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb_u32(100),
			max_miner_fee: Amount::from_sat(20_000), max_weight: Weight::from_wu(400_000) }
	}
	#[test]
	fn urgent_sql_codec_refuses_partial_noncanonical_or_foreign_original_signed_package() {
		let limit = limits(); let (candidate, transactions, identity, policy) = synthetic_urgent_vector(true, limit);
		let publication = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let original = UrgentSqlIntent::original(&candidate, Some(&publication), &identity, policy).unwrap();
		assert_eq!(original.decode(&identity, policy).unwrap(), (candidate.clone(), Some(publication.clone())));
		let mut missing = original.clone(); missing.signed_entry = None;
		assert!(missing.decode(&identity, policy).is_err());
		let mut changed = original.clone(); changed.reservation_entry.push(b' ');
		assert!(changed.decode(&identity, policy).is_err());
		changed = original.clone(); changed.signed_record.as_mut().unwrap().push(b' ');
		assert!(changed.decode(&identity, policy).is_err());
		let (foreign, _, other_identity, _) = synthetic_urgent_vector(true, limit);
		changed = original; changed.candidate = serde_json::to_vec(&foreign).unwrap();
		assert!(changed.decode(&other_identity, policy).is_err());
	}
	#[test]
	fn urgent_nursery_refuses_other_kind_changed_original_witness_and_abandoned_tuple() {
		let limit = limits(); let (candidate, transactions, identity, policy) = synthetic_urgent_vector(false, limit);
		let publication = candidate.bind_signed(&transactions, &identity, limit, policy).unwrap();
		let signed = publication.last_transaction().unwrap(); let bytes = serialize(&signed);
		require_nursery_tuple(&signed, "native-urgent", &bytes, false).unwrap();
		assert!(require_nursery_tuple(&signed, "expiry-payout", &bytes, false).is_err());
		assert!(require_nursery_tuple(&signed, "native-urgent", &bytes, true).is_err());
		let mut changed = signed.clone(); changed.input[0].witness.push([4;64]);
		assert_eq!(changed.compute_txid(), signed.compute_txid());
		assert!(require_nursery_tuple(&signed, "native-urgent", &serialize(&changed), false).is_err());
		changed = signed.clone(); changed.output[0].value = Amount::from_sat(8_999);
		assert!(require_nursery_tuple(&signed, "native-urgent", &serialize(&changed), false).is_err());
	}
	#[test]
	fn urgent_nursery_keeps_exact_parent_and_child_package_with_last_body_only_as_lookup() {
		let limit = limits(); let (candidate, transactions, identity, policy) = synthetic_urgent_vector(true, limit);
		let publication = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let intent = UrgentSqlIntent::original(&candidate, Some(&publication), &identity, policy).unwrap();
		let (_, restored) = intent.decode(&identity, policy).unwrap(); let restored = restored.unwrap();
		assert_eq!(restored.transactions().unwrap(), transactions);
		assert_eq!(restored.last_transaction().unwrap(), transactions[1]);
		assert!(restored.require_exact_bytes(&transactions[1..]).is_err(), "a nursery child lookup cannot erase its original parent package");
		assert!(require_nursery_tuple(&transactions[1], "native-urgent", &serialize(&transactions[0]), false).is_err());
	}
	#[test]
	fn urgent_sql_original_projection_refuses_changed_identity_body_inputs_and_unsigned_state() {
		let limit = limits(); let (candidate, transactions, identity, policy) = synthetic_urgent_vector(false, limit);
		let expected = UrgentSqlIntent::original(&candidate, None, &identity, policy).unwrap();
		expected.require_exact(&expected).unwrap();
		let mut wrong_identity = identity.clone(); wrong_identity.opening_sha256 = sha256::Hash::hash(b"different original opening");
		assert!(UrgentSqlIntent::original(&candidate, None, &wrong_identity, policy).is_err());
		let changed = [
			UrgentSqlIntent { index: expected.index + 1, ..expected.clone() },
			UrgentSqlIntent { purpose: "progression".into(), ..expected.clone() },
			UrgentSqlIntent { candidate_hash: sha256::Hash::hash(b"other candidate").to_string(), ..expected.clone() },
			UrgentSqlIntent { publication_txid: transactions[0].compute_wtxid().to_string(), ..expected.clone() },
			UrgentSqlIntent { policy_hash: sha256::Hash::hash(b"other policy").to_string(), ..expected.clone() },
			UrgentSqlIntent { opening_hash: wrong_identity.opening_sha256.to_string(), ..expected.clone() },
			UrgentSqlIntent { reservation_entry: b"{}".to_vec(), ..expected.clone() },
			UrgentSqlIntent { candidate: b"{}".to_vec(), ..expected.clone() },
		];
		for row in changed { assert!(row.require_exact(&expected).is_err()); }
		let signed = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let signed_row = UrgentSqlIntent::original(&candidate, Some(&signed), &identity, policy).unwrap();
		assert!(signed_row.require_exact(&expected).is_err(), "an existing signed attempt cannot authorize another signing");
	}
	#[test]
	fn urgent_sql_signed_projection_requires_same_original_full_package_and_exact_signed_state() {
		let limit = limits(); let (candidate, transactions, identity, policy) = synthetic_urgent_vector(true, limit);
		let signed = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let expected = UrgentSqlIntent::original(&candidate, Some(&signed), &identity, policy).unwrap();
		expected.require_exact(&expected).unwrap();
		let (foreign, _, _, _) = synthetic_urgent_vector(true, limit);
		assert!(UrgentSqlIntent::original(&foreign, Some(&signed), &identity, policy).is_err());
		for row in [UrgentSqlIntent { signed_entry: None, ..expected.clone() },
			UrgentSqlIntent { signed_record: None, ..expected.clone() },
			UrgentSqlIntent { signed_entry: Some(b"{}".to_vec()), ..expected.clone() },
			UrgentSqlIntent { signed_record: Some(b"{}".to_vec()), ..expected.clone() }] {
			assert!(row.require_exact(&expected).is_err());
		}
	}
	#[test]
	fn urgent_sql_external_exclusion_retains_sources_and_whole_fee_inputs_without_internal_anchor_credit() {
		let limit = limits(); let (candidate, transactions, _, policy) = synthetic_urgent_vector(true, limit);
		let retained = candidate.external_inputs(policy).unwrap();
		assert_eq!(retained.len(), 2);
		assert!(transactions[0].input.iter().all(|input| retained.contains(&input.previous_output)));
		let anchor = transactions[0].fee_anchor().unwrap().0;
		assert!(!retained.contains(&anchor));
		assert!(transactions[1].input.iter().filter(|input| input.previous_output != anchor)
			.all(|input| retained.contains(&input.previous_output)));
		let (claim, claim_transactions, _, policy) = synthetic_urgent_vector(false, limit);
		assert_eq!(claim.external_inputs(policy).unwrap(), claim_transactions[0].input.iter().map(|input| input.previous_output).collect());
	}
}
