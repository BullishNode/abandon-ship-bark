//! Native fee intents retain actual signed inputs across process death. These
//! rows do not create customer VTXOs or reconstruct a missing round/pool tree.
use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::Transaction;

use super::{AdvisoryLock, Tx};
use crate::watchman::ordinary_fee::OrdinaryFeePolicy;
use crate::watchman::ordinary_fee_publication::PublicationRecord;

const MAX_INTENTS: i64 = 4096;
const MAX_SIGNED_BYTES: i64 = 64 * 1024 * 1024;
const MAX_INPUTS: i64 = 50_000;

/// Only a published candidate with the exact original nursery body may enter
/// the wallet graph. An unused signed intent excludes its inputs without
/// admitting the unbroadcast transaction's change as a trusted wallet source.
pub(crate) struct RetainedNativeFeeIntent {
	pub transaction: Transaction,
	pub published: bool,
}

fn retained_candidate(id: &str, raw: &[u8], published: bool,
	purpose: &str, nursery: Option<(&str, &[u8])>,
) -> anyhow::Result<RetainedNativeFeeIntent> {
	ensure!(raw.len() <= 400_000, "retained native candidate exceeds byte bound");
	let tx: Transaction = deserialize(raw)?;
	ensure!(tx.compute_wtxid().to_string() == id && serialize(&tx) == raw
		&& !tx.is_coinbase() && !tx.input.is_empty() && tx.input.len() <= 4096
		&& tx.weight().to_wu() <= 400_000, "retained native candidate differs or exceeds bounds");
	let points = tx.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>();
	ensure!(points.len() == tx.input.len(), "retained native candidate repeats an input");
	let expected_kind = match purpose { "round" => "round", "pool" => "vtxopool",
		_ => bail!("retained native candidate has unknown publication purpose") };
	if published {
		let (kind, body) = nursery.context("published native candidate lacks original nursery publication")?;
		ensure!(kind == expected_kind && body == raw,
			"published native candidate differs from original nursery body or purpose");
	} else {
		ensure!(nursery.is_none(), "unpublished native candidate already has an unbound nursery publication");
	}
	Ok(RetainedNativeFeeIntent { transaction: tx, published })
}

impl Tx<'_> {
	/// Every DIRECT nursery insertion, including alternate callers, must bind
	/// the exact signed body/input set to the original fee epoch. No allowance
	/// is created here; the independent owning journal is checked at relay.
	pub(crate) async fn bind_native_fee_nursery(&self, signed: &Transaction, kind: crate::nursery::NurseryTxKind) -> anyhow::Result<()> {
		let purpose = match kind {
			crate::nursery::NurseryTxKind::Round => "round",
			crate::nursery::NurseryTxKind::VtxoPool => "pool",
			_ => return Ok(()),
		};
		let direct = self.query_opt("SELECT direct FROM expiry_route WHERE id=1", &[]).await?
			.is_some_and(|row| row.get::<_, bool>(0));
		if !direct { return Ok(()); }
		self.lock_native_fee_publication().await?;
		let id = signed.compute_wtxid().to_string();
		let row = self.query_opt("SELECT p.signed_tx,p.purpose,p.policy_hash,p.opening_hash,o.opening_hash,o.policy_bytes
			FROM native_fee_publication p CROSS JOIN native_fee_opening o
			WHERE p.wtxid=$1 AND o.id=1 FOR UPDATE OF p", &[&id]).await?
			.context("DIRECT nursery candidate lacks its actual original native fee intent/opening")?;
		ensure!(row.get::<_, Vec<u8>>(0) == serialize(signed) && row.get::<_, String>(1) == purpose
			&& row.get::<_, String>(3) == row.get::<_, String>(4)
			&& row.get::<_, String>(2) == sha256::Hash::hash(&row.get::<_, Vec<u8>>(5)).to_string(),
			"DIRECT nursery body, purpose or original fee identity differs");
		let actual = signed.input.iter().map(|input| input.previous_output.to_string()).collect::<BTreeSet<_>>();
		let retained = self.query("SELECT outpoint FROM native_fee_input WHERE wtxid=$1 ORDER BY outpoint", &[&id]).await?
			.into_iter().map(|row| row.get::<_, String>(0)).collect::<BTreeSet<_>>();
		ensure!(!actual.is_empty() && actual.len() == signed.input.len() && actual == retained,
			"DIRECT nursery whole-input intent differs");
		self.execute("UPDATE native_fee_publication SET published_at=COALESCE(published_at,NOW()) WHERE wtxid=$1", &[&id]).await?;
		Ok(())
	}
	pub(crate) async fn lock_native_fee_publication(&self) -> anyhow::Result<()> {
		self.query_one("SELECT pg_advisory_xact_lock($1)", &[&(AdvisoryLock::NativeFeePublication as i64)]).await?;
		Ok(())
	}
	async fn native_fee_bounds(&self) -> anyhow::Result<()> {
		let row = self.query_one("SELECT count(*),COALESCE(sum(octet_length(signed_tx)),0)::bigint FROM native_fee_publication", &[]).await?;
		ensure!(row.get::<_, i64>(0) <= MAX_INTENTS && row.get::<_, i64>(1) <= MAX_SIGNED_BYTES,
			"native fee intent source exceeds bounded admission");
		ensure!(self.query_one("SELECT count(*) FROM native_fee_input", &[]).await?.get::<_, i64>(0) <= MAX_INPUTS,
			"native fee input source exceeds bounded admission");
		Ok(())
	}
	pub(crate) async fn stage_native_fee_intent(&self, record: &PublicationRecord,
		policy: OrdinaryFeePolicy, opening: sha256::Hash,
	) -> anyhow::Result<()> {
		self.lock_native_fee_publication().await?;
		self.native_fee_bounds().await?;
		let entry = record.entry(policy)?;
		let signed = record.transaction()?;
		let raw = serialize(&signed);
		let id = entry.id().to_string(); let txid = signed.compute_txid().to_string();
		ensure!(raw.len() <= 400_000 && opening.to_byte_array().iter().any(|byte| *byte != 0),
			"native fee intent lacks bounded candidate or original opening");
		let purpose = entry.purpose(); let record_hash = entry.record_hash().to_string();
		let policy_hash = record.policy_hash().to_string(); let opening_hash = opening.to_string();
		self.execute("INSERT INTO native_fee_publication(wtxid,txid,purpose,record_hash,policy_hash,opening_hash,signed_tx)
			VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(wtxid) DO NOTHING",
			&[&id,&txid,&purpose,&record_hash,&policy_hash,&opening_hash,&raw]).await?;
		self.require_native_fee_intent(record, policy, opening, &signed).await?;
		let mut inputs = BTreeSet::new();
		for input in &signed.input {
			let point = input.previous_output.to_string();
			ensure!(inputs.insert(point.clone()), "duplicate native fee intent input");
			self.execute("INSERT INTO native_fee_input(outpoint,wtxid) VALUES($1,$2) ON CONFLICT(outpoint) DO NOTHING", &[&point,&id]).await?;
			let owner = self.query_one("SELECT wtxid FROM native_fee_input WHERE outpoint=$1", &[&point]).await?.get::<_, String>(0);
			ensure!(owner == id, "native fee input already retained by a different signed candidate");
		}
		let retained = self.query("SELECT outpoint FROM native_fee_input WHERE wtxid=$1 ORDER BY outpoint", &[&id]).await?
			.into_iter().map(|row| row.get::<_, String>(0)).collect::<BTreeSet<_>>();
		ensure!(retained == inputs, "native fee intent input set differs");
		self.native_fee_bounds().await?;
		Ok(())
	}
	pub(crate) async fn require_native_fee_intent(&self, record: &PublicationRecord,
		policy: OrdinaryFeePolicy, opening: sha256::Hash, signed: &Transaction,
	) -> anyhow::Result<()> {
		record.matches(signed)?;
		let entry = record.entry(policy)?;
		let row = self.query_opt("SELECT txid,purpose,record_hash,policy_hash,opening_hash,signed_tx
			FROM native_fee_publication WHERE wtxid=$1 FOR UPDATE", &[&entry.id().to_string()]).await?
			.context("signed ordinary publication has no original durable native intent")?;
		ensure!(row.get::<_, String>(0) == signed.compute_txid().to_string()
			&& row.get::<_, String>(1) == entry.purpose()
			&& row.get::<_, String>(2) == entry.record_hash().to_string()
			&& row.get::<_, String>(3) == record.policy_hash().to_string()
			&& row.get::<_, String>(4) == opening.to_string()
			&& row.get::<_, Vec<u8>>(5) == serialize(signed),
			"native fee publication differs from original candidate, purpose or opening");
		Ok(())
	}
	pub(crate) async fn publish_native_fee_intent(&self, record: &PublicationRecord,
		policy: OrdinaryFeePolicy, opening: sha256::Hash, signed: &Transaction,
	) -> anyhow::Result<()> {
		self.lock_native_fee_publication().await?;
		self.require_native_fee_intent(record, policy, opening, signed).await?;
		let id = signed.compute_wtxid().to_string();
		let expected = signed.input.iter().map(|input| input.previous_output.to_string()).collect::<BTreeSet<_>>();
		let retained = self.query("SELECT outpoint FROM native_fee_input WHERE wtxid=$1 ORDER BY outpoint", &[&id]).await?
			.into_iter().map(|row| row.get::<_, String>(0)).collect::<BTreeSet<_>>();
		ensure!(retained == expected, "native publication input reservation missing or changed");
		self.execute("UPDATE native_fee_publication SET published_at=COALESCE(published_at,NOW()) WHERE wtxid=$1", &[&id]).await?;
		Ok(())
	}
	// Startup excludes unpublished inputs and only replays published bodies
	// bound to the original nursery. It never submits or fabricates customer rows.
	pub(crate) async fn retained_native_fee_inputs(&self) -> anyhow::Result<Vec<RetainedNativeFeeIntent>> {
		self.lock_native_fee_publication().await?;
		self.native_fee_bounds().await?;
		let rows = self.query("SELECT p.wtxid,p.signed_tx,p.txid,p.published_at IS NOT NULL AS published,p.purpose,
			n.kind::TEXT AS nursery_kind,n.tx AS nursery_body FROM native_fee_publication p
			LEFT JOIN nursery_tx n ON n.txid=p.txid ORDER BY p.wtxid LIMIT 4097", &[]).await?;
		ensure!(rows.len() <= MAX_INTENTS as usize, "native fee intent count exceeds bound");
		let mut result = Vec::with_capacity(rows.len());
		for row in rows {
			let raw: Vec<u8> = row.get(1);
			let kind: Option<String> = row.get("nursery_kind");
			let nursery_raw: Option<Vec<u8>> = row.get("nursery_body");
			ensure!(kind.is_some() == nursery_raw.is_some(), "native nursery projection incomplete");
			let candidate = retained_candidate(&row.get::<_, String>(0), &raw, row.get("published"),
				&row.get::<_, String>("purpose"), kind.as_deref().zip(nursery_raw.as_deref()))?;
			ensure!(candidate.transaction.compute_txid().to_string() == row.get::<_, String>("txid"),
				"retained native transaction id differs from original body");
			let actual = candidate.transaction.input.iter().map(|input| input.previous_output.to_string()).collect::<BTreeSet<_>>();
			let retained = self.query("SELECT outpoint FROM native_fee_input WHERE wtxid=$1 ORDER BY outpoint", &[&row.get::<_, String>(0)]).await?
				.into_iter().map(|row| row.get::<_, String>(0)).collect::<BTreeSet<_>>();
			ensure!(retained == actual, "retained native fee input set incomplete");
			result.push(candidate);
		}
		Ok(result)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::{absolute, transaction, Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Txid, Witness};

	fn candidate() -> Transaction {
		Transaction { version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
			input: vec![TxIn { previous_output: OutPoint::new(Txid::all_zeros(), 3),
				script_sig: ScriptBuf::new(), sequence: Sequence::MAX,
				witness: Witness::from_slice(&[vec![0; 64]]) }],
			output: vec![TxOut { value: Amount::from_sat(1_000), script_pubkey: ScriptBuf::new() }] }
	}
	#[test]
	fn unpublished_intent_keeps_input_exclusion_without_publication_credit() {
		let tx = candidate(); let raw = serialize(&tx); let id = tx.compute_wtxid().to_string();
		let retained = retained_candidate(&id, &raw, false, "pool", None).unwrap();
		assert!(!retained.published);
		assert_eq!(retained.transaction, tx);
		assert!(retained_candidate(&id, &raw, true, "pool", None).is_err());
		assert!(retained_candidate(&id, &raw, false, "pool", Some(("vtxopool", &raw))).is_err());
	}
	#[test]
	fn published_intent_requires_exact_original_nursery_body_and_purpose() {
		let tx = candidate(); let raw = serialize(&tx); let id = tx.compute_wtxid().to_string();
		assert!(retained_candidate(&id, &raw, true, "pool", Some(("vtxopool", &raw))).unwrap().published);
		assert!(retained_candidate(&id, &raw, true, "pool", Some(("round", &raw))).is_err());
		let mut other = tx.clone(); other.output[0].value = Amount::from_sat(999);
		assert!(retained_candidate(&id, &raw, true, "pool", Some(("vtxopool", &serialize(&other)))).is_err());
		assert!(retained_candidate(&other.compute_wtxid().to_string(), &raw, true, "pool", Some(("vtxopool", &raw))).is_err());
	}
	#[test]
	fn retained_intent_rejects_noncanonical_body_duplicate_inputs_and_unknown_purpose() {
		let mut tx = candidate(); let raw = serialize(&tx); let id = tx.compute_wtxid().to_string();
		assert!(retained_candidate(&id, &raw, false, "urgent", None).is_err());
		let mut tail = raw; tail.push(0);
		assert!(retained_candidate(&id, &tail, false, "round", None).is_err());
		tx.input.push(tx.input[0].clone()); let duplicate = serialize(&tx);
		assert!(retained_candidate(&tx.compute_wtxid().to_string(), &duplicate, false, "round", None).is_err());
	}
}
