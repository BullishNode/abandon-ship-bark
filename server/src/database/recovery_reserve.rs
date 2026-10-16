//! Durable admission records for recovery cash in the existing rounds wallet.

use std::collections::HashSet;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, ScriptBuf, Transaction};
use bitcoin::consensus::serialize;

use bitcoin_ext::BlockRef;

use crate::database::Tx;

#[derive(Clone, Debug)]
pub(crate) struct RecoveryReceipt {
	pub transaction: Transaction,
	pub block: BlockRef,
	/// Exact input prevouts, in transaction input order. None means gross is unknown.
	pub sources: Option<Vec<(OutPoint, Amount)>>,
	pub gap: Option<String>,
}

impl RecoveryReceipt {
	fn validate(&self) -> anyhow::Result<()> {
		ensure!(self.sources.is_some() != self.gap.is_some(), "invalid recovery proof state");
		if let Some(sources) = &self.sources {
			ensure!(sources.len() == self.transaction.input.len(), "incomplete recovery input proof");
			let mut points = HashSet::new();
			for (input, (point, _)) in self.transaction.input.iter().zip(sources) {
				ensure!(input.previous_output == *point && points.insert(*point), "recovery proof input mismatch or duplicate");
			}
			let gross = sources.iter().try_fold(Amount::ZERO, |total, (_, value)| total.checked_add(*value))
				.context("recovery gross amount overflow")?;
			let output = self.transaction.output.iter().try_fold(Amount::ZERO, |total, output| total.checked_add(output.value))
				.context("recovery output amount overflow")?;
			ensure!(gross >= output, "recovery outputs exceed proved gross inputs");
		}
		Ok(())
	}
}

#[derive(Debug)]
pub(crate) struct RecoveryReserveState {
	pub script: ScriptBuf,
	pub height: u32,
	pub hash: String,
	pub fee_headroom: Amount,
	pub held: Amount,
	pub unknown_gross: bool,
}

impl<'t> Tx<'t> {
	pub(crate) async fn check_recovery_receipt(&self, transaction: &Transaction) -> anyhow::Result<()> {
		let txid = transaction.compute_txid().to_string();
		let row = self.query_opt("SELECT transaction_bytes FROM recovery_receipt WHERE id = $1", &[&txid]).await?;
		ensure!(row.is_some_and(|row| row.get::<_, Vec<u8>>(0) == serialize(transaction)),
			"owning wallet contains an unbooked recovery receipt {txid}; explicit backfill required");
		Ok(())
	}

	pub(crate) async fn recovery_reserve_state(&self) -> anyhow::Result<Option<RecoveryReserveState>> {
		let row = self.query_opt(
			"SELECT sweep_spk, checkpoint_height, checkpoint_hash, fee_headroom_sat FROM recovery_reserve WHERE id = 'rounds'",
			&[],
		).await?;
		let Some(row) = row else { return Ok(None); };
		let fee = row.get::<_, Option<i64>>("fee_headroom_sat")
			.context("recovery reserve has no declared fee headroom; maintenance transition required")?;
		let fee = u64::try_from(fee)?;
		ensure!(fee > 0 && fee <= Amount::MAX_MONEY.to_sat(), "invalid recovery fee headroom policy");
		let (held, unknown_gross) = self.recovery_reserve_summary().await?;
		Ok(Some(RecoveryReserveState {
			script: ScriptBuf::from_bytes(row.get("sweep_spk")),
			height: u32::try_from(row.get::<_, i64>("checkpoint_height"))?,
			hash: row.get("checkpoint_hash"), fee_headroom: Amount::from_sat(fee), held, unknown_gross,
		}))
	}

	pub(crate) async fn recovery_reserve_summary(&self) -> anyhow::Result<(Amount, bool)> {
		let row = self.query_one("
			SELECT (SELECT COALESCE(SUM(held_sat), 0)::text FROM recovery_source_hold) AS held,
				EXISTS (SELECT 1 FROM recovery_receipt WHERE proof_state = 'unknown_gross') AS unknown_gross
		", &[]).await?;
		let held = row.get::<_, String>("held").parse::<u64>().context("recovery held amount overflow")?;
		Ok((Amount::from_sat(held), row.get("unknown_gross")))
	}

	pub(crate) async fn store_recovery_reserve(
		&self,
		script: &ScriptBuf,
		fee_headroom: Amount,
		checkpoint: (u32, String),
		receipts: &[RecoveryReceipt],
	) -> anyhow::Result<(Amount, bool)> {
		for receipt in receipts {
			receipt.validate()?;
			let txid = receipt.transaction.compute_txid().to_string();
			let bytes = serialize(&receipt.transaction);
			let height = i64::from(receipt.block.height.to_u32());
			let hash = receipt.block.hash.to_string();
			let state = if receipt.sources.is_some() { "physical_bound" } else { "unknown_gross" };
			let row = self.query_one("
				INSERT INTO recovery_receipt (id, transaction_bytes, block_height, block_hash, proof_state, proof_gap)
				VALUES ($1, $2, $3, $4, $5, $6)
				ON CONFLICT (id) DO UPDATE SET
					proof_state = CASE WHEN EXCLUDED.proof_state = 'physical_bound'
						THEN EXCLUDED.proof_state ELSE recovery_receipt.proof_state END,
					proof_gap = CASE WHEN EXCLUDED.proof_state = 'physical_bound'
						THEN NULL ELSE recovery_receipt.proof_gap END
				WHERE recovery_receipt.transaction_bytes = EXCLUDED.transaction_bytes
					AND recovery_receipt.block_height = EXCLUDED.block_height
					AND recovery_receipt.block_hash = EXCLUDED.block_hash
				RETURNING transaction_bytes, block_height, block_hash, proof_state
			", &[&txid, &bytes, &height, &hash, &state, &receipt.gap]).await?;
			ensure!(row.get::<_, Vec<u8>>("transaction_bytes") == bytes
				&& row.get::<_, i64>("block_height") == height
				&& row.get::<_, String>("block_hash") == hash,
				"conflicting recovery receipt {txid}");
			if let Some(sources) = &receipt.sources {
				ensure!(sources.len() == receipt.transaction.input.len(), "incomplete recovery input proof");
				for (input, (point, amount)) in receipt.transaction.input.iter().zip(sources) {
					ensure!(input.previous_output == *point, "recovery proof input mismatch");
					let point = point.to_string();
					let gross = i64::try_from(amount.to_sat()).context("recovery amount out of range")?;
					let row = self.query_one("
						INSERT INTO recovery_source_hold (id, receipt_id, gross_sat, held_sat)
						VALUES ($1, $2, $3, $3)
						ON CONFLICT (id) DO UPDATE SET id = recovery_source_hold.id
						RETURNING receipt_id, gross_sat
					", &[&point, &txid, &gross]).await?;
					ensure!(row.get::<_, String>("receipt_id") == txid && row.get::<_, i64>("gross_sat") == gross,
						"conflicting reclaimed source {point}");
				}
			}
		}
		let height = i64::from(checkpoint.0);
		let fee = i64::try_from(fee_headroom.to_sat())?;
		ensure!(fee > 0 && fee_headroom <= Amount::MAX_MONEY, "invalid declared recovery fee headroom");
		let row = self.query_one("
			INSERT INTO recovery_reserve (id, sweep_spk, checkpoint_height, checkpoint_hash, fee_headroom_sat)
			VALUES ('rounds', $1, $2, $3, $4)
			ON CONFLICT (id) DO UPDATE SET checkpoint_height = EXCLUDED.checkpoint_height,
				checkpoint_hash = EXCLUDED.checkpoint_hash
			WHERE recovery_reserve.sweep_spk = EXCLUDED.sweep_spk
				AND recovery_reserve.fee_headroom_sat = EXCLUDED.fee_headroom_sat
			RETURNING id
		", &[&script.as_bytes(), &height, &checkpoint.1, &fee]).await
			.context("recovery reserve script mismatch")?;
		ensure!(row.get::<_, String>(0) == "rounds", "invalid recovery reserve identity");
		self.recovery_reserve_summary().await
	}
}

#[cfg(test)]
mod test {
	use super::*;

	use bitcoin::{absolute, transaction, BlockHash, TxIn, TxOut, Txid};
	use bitcoin::hashes::Hash;
	use bitcoin_ext::BlockHeight;

	fn receipt() -> RecoveryReceipt {
		let point = OutPoint::new(Txid::from_byte_array([7; 32]), 1);
		RecoveryReceipt {
			transaction: Transaction {
				version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
				input: vec![TxIn { previous_output: point, ..Default::default() }],
				output: vec![TxOut { value: Amount::from_sat(6_500), script_pubkey: ScriptBuf::new() }],
			},
			block: BlockRef { height: BlockHeight::new(100), hash: BlockHash::from_byte_array([8; 32]) },
			sources: Some(vec![(point, Amount::from_sat(7_000))]), gap: None,
		}
	}

	#[test]
	fn recovery_receipt_preserves_gross_bound_above_net_cash() {
		let receipt = receipt();
		receipt.validate().unwrap();
		assert_eq!(receipt.sources.unwrap()[0].1, Amount::from_sat(7_000));
		assert_eq!(receipt.transaction.output[0].value, Amount::from_sat(6_500));
	}

	#[test]
	fn recovery_receipt_rejects_incomplete_misattributed_and_underfunded_proofs() {
		let mut incomplete = receipt();
		incomplete.sources.as_mut().unwrap().clear();
		assert!(incomplete.validate().is_err());
		let mut wrong = receipt();
		wrong.sources.as_mut().unwrap()[0].0.vout += 1;
		assert!(wrong.validate().is_err());
		let mut short = receipt();
		short.sources.as_mut().unwrap()[0].1 = Amount::from_sat(6_000);
		assert!(short.validate().is_err());
	}

	#[test]
	fn recovery_receipt_unknown_gross_requires_explicit_gap() {
		let mut unknown = receipt();
		unknown.sources = None;
		assert!(unknown.validate().is_err());
		unknown.gap = Some("input proof unavailable".into());
		unknown.validate().unwrap();
	}
}

/// Private native credit for one exact existing settlement selection. There is
/// no caller-provided amount or public/RPC reserve override.
#[derive(Clone, Debug)]
pub(crate) struct PayoutCapitalCredit {
	releases: Vec<CapitalRelease>,
	total: Amount,
}

#[derive(Clone, Debug)]
struct CapitalRelease {
	id: String,
	source: String,
	receipt: String,
	gross: Amount,
	source_revision: i64,
}

impl PayoutCapitalCredit {
	pub(crate) fn total(&self) -> Amount { self.total }
}

impl<'t> Tx<'t> {
	pub(crate) async fn prepare_payout_capital_credit(
		&self,
		inputs: &[crate::database::expiry_settlement::ExpiryInput],
		coins: &[ark::Vtxo],
	) -> anyhow::Result<PayoutCapitalCredit> {
		let coins = coins.iter().map(|coin| (coin.id(), coin)).collect::<std::collections::BTreeMap<_, _>>();
		ensure!(coins.len() == inputs.len(), "capital credit coin selection mismatch");
		let mut releases = Vec::with_capacity(inputs.len());
		let mut seen = HashSet::new();
		let sources = inputs.iter().map(|input| input.backing.source.to_string()).collect::<Vec<_>>();
		let rows = self.query("
			SELECT h.id, h.receipt_id, h.gross_sat, h.held_sat, h.revision,
				r.block_hash, r.proof_state
			FROM recovery_source_hold h JOIN recovery_receipt r ON r.id = h.receipt_id
			WHERE h.id = ANY($1)
		", &[&sources]).await?;
		let rows = rows.iter().map(|row| (row.get::<_, String>("id"), row)).collect::<std::collections::BTreeMap<_, _>>();
		let mut selected_by_source = std::collections::BTreeMap::<String, Amount>::new();
		let mut total = Amount::ZERO;
		for input in inputs {
			ensure!(seen.insert(input.id), "duplicate capital release coin");
			let coin = coins.get(&input.id).context("selected capital coin missing")?;
			let source = input.backing.source.to_string();
			let row = rows.get(&source).context("selected backing has no booked physical hold")?;
			ensure!(row.get::<_, String>("proof_state") == "physical_bound"
				&& row.get::<_, String>("receipt_id") == input.backing.receipt.to_string()
				&& row.get::<_, String>("block_hash") == input.backing.block_hash.to_string()
				&& u64::try_from(row.get::<_, i64>("gross_sat"))? == input.backing.source_value.to_sat(),
				"selected backing proof differs from booked source");
			let gross = coin.amount();
			ensure!(gross > Amount::ZERO, "empty selected capital release");
			let source_total = selected_by_source.entry(source.clone()).or_default();
			*source_total = source_total.checked_add(gross).context("selected source credit overflow")?;
			ensure!(source_total.to_sat() <= u64::try_from(row.get::<_, i64>("held_sat"))?,
				"selected claims exceed remaining physical bound");
			total = total.checked_add(gross).context("selected capital credit overflow")?;
			releases.push(CapitalRelease {
				id: input.id.to_string(), source, receipt: input.backing.receipt.to_string(), gross,
				source_revision: row.get("revision"),
			});
		}
		ensure!(!releases.is_empty(), "empty capital release selection");
		Ok(PayoutCapitalCredit { releases, total })
	}

	/// Called after existing native settlement validation, in its SAME SQL COMMIT.
	pub(crate) async fn store_payout_capital_release(
		&self, credit: &PayoutCapitalCredit, transaction: &Transaction,
	) -> anyhow::Result<()> {
		let txid = transaction.compute_txid().to_string();
		let ids = credit.releases.iter().map(|release| release.id.clone()).collect::<Vec<_>>();
		let settled = self.query("SELECT id FROM expiry_settlement WHERE id = ANY($1) AND txid = $2", &[&ids, &txid]).await?;
		ensure!(settled.len() == ids.len(), "capital release must match exact committed settlement inputs");
		let mut by_source = std::collections::BTreeMap::<&str, (Amount, i64)>::new();
		for release in &credit.releases {
			let (amount, revision) = by_source.entry(&release.source).or_insert((Amount::ZERO, release.source_revision));
			ensure!(*revision == release.source_revision, "mixed physical hold revisions");
			*amount = amount.checked_add(release.gross).context("capital discharge overflow")?;
		}
		// Native HTLC/pending-board/Vtxo locks precede these rows. Every capital
		// discharge locks physical sources in this same sorted OutPoint order.
		for (source, (gross, revision)) in by_source {
			let gross = i64::try_from(gross.to_sat())?;
			let changed = self.execute("
				UPDATE recovery_source_hold SET held_sat = held_sat - $2, revision = revision + 1
				WHERE id = $1 AND held_sat >= $2 AND revision = $3
			", &[&source, &gross, &revision]).await?;
			ensure!(changed == 1, "physical hold changed before settlement commit");
		}
		for release in &credit.releases {
			let gross = i64::try_from(release.gross.to_sat())?;
			self.execute("
				INSERT INTO recovery_capital_release (id, payout_txid, source_id, receipt_id, gross_sat, source_revision)
				VALUES ($1, $2, $3, $4, $5, $6)
			", &[&release.id, &txid, &release.source, &release.receipt, &gross, &release.source_revision]).await?;
		}
		Ok(())
	}

	pub(crate) async fn payout_capital_release_matches(
		&self, credit: &PayoutCapitalCredit, transaction: &Transaction,
	) -> anyhow::Result<bool> {
		let txid = transaction.compute_txid().to_string();
		let rows = self.query("
			SELECT id, source_id, receipt_id, gross_sat, source_revision FROM recovery_capital_release WHERE payout_txid = $1
		", &[&txid]).await?;
		Ok(rows.len() == credit.releases.len() && credit.releases.iter().all(|release| rows.iter().any(|row| {
			row.get::<_, String>("id") == release.id
				&& row.get::<_, String>("source_id") == release.source
				&& row.get::<_, String>("receipt_id") == release.receipt
				&& row.get::<_, i64>("gross_sat") >= 0
				&& row.get::<_, i64>("gross_sat") as u64 == release.gross.to_sat()
				&& row.get::<_, i64>("source_revision") == release.source_revision
		})))
	}
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapitalReleaseWitness {
	id: String,
	source: String,
	receipt: String,
	gross_sat: i64,
	source_gross_sat: i64,
	source_revision: i64,
	receipt_block_hash: String,
}

impl<'t> Tx<'t> {
	pub(crate) async fn payout_capital_witness(&self, txid: bitcoin::Txid) -> anyhow::Result<Option<Vec<CapitalReleaseWitness>>> {
		let rows = self.query("
			SELECT c.id, c.source_id, c.receipt_id, c.gross_sat, c.source_revision,
				h.gross_sat AS source_gross_sat, r.block_hash
			FROM recovery_capital_release c
			JOIN recovery_source_hold h ON h.id = c.source_id AND h.receipt_id = c.receipt_id
			JOIN recovery_receipt r ON r.id = c.receipt_id
			WHERE c.payout_txid = $1 ORDER BY c.id
		", &[&txid.to_string()]).await?;
		if rows.is_empty() { return Ok(None); }
		Ok(Some(rows.into_iter().map(|row| CapitalReleaseWitness {
			id: row.get("id"), source: row.get("source_id"), receipt: row.get("receipt_id"),
			gross_sat: row.get("gross_sat"), source_gross_sat: row.get("source_gross_sat"),
			source_revision: row.get("source_revision"), receipt_block_hash: row.get("block_hash"),
		}).collect()))
	}
}

impl<'t> Tx<'t> {
	/// Durable signed publications, including the round-before-nursery and
	/// offboard-before-wallet-commit boundaries. Abandonment is not cancellation.
	pub(crate) async fn signed_wallet_publication_page(
		&self, after: &str, wallet_height: u32,
	) -> anyhow::Result<Vec<Transaction>> {
		let rows = self.query("
			WITH publications AS (
				SELECT txid, tx AS body FROM nursery_tx
				WHERE confirmed_at_height IS NULL OR confirmed_at_height::bigint > $2
				UNION ALL
				SELECT r.funding_txid AS txid, v.signed_tx AS body FROM round r
				JOIN virtual_transaction v ON v.txid = r.funding_txid
				WHERE v.is_funding AND v.signed_tx IS NOT NULL
				UNION ALL
				SELECT r.funding_txid AS txid, r.funding_tx AS body FROM round r
				JOIN virtual_transaction v ON v.txid = r.funding_txid
				WHERE v.is_funding AND v.signed_tx IS NOT NULL
				UNION ALL
				SELECT txid, signed_tx AS body FROM offboards
			)
			SELECT txid, ARRAY_AGG(DISTINCT body) AS bodies FROM publications
			WHERE txid > $1 GROUP BY txid ORDER BY txid LIMIT 64
		", &[&after, &i64::from(wallet_height)]).await?;
		let mut transactions = Vec::with_capacity(rows.len());
		for row in rows {
			let bodies = row.get::<_, Vec<Vec<u8>>>("bodies");
			ensure!(bodies.len() == 1, "conflicting durable signed publication bytes");
			let transaction: Transaction = bitcoin::consensus::deserialize(&bodies[0])?;
			ensure!(transaction.compute_txid().to_string() == row.get::<_, String>("txid"),
				"signed publication txid differs from stored identity");
			transactions.push(transaction);
		}
		Ok(transactions)
	}
}

impl<'t> Tx<'t> {
	pub(crate) async fn signed_wallet_publication(&self, txid: bitcoin::Txid) -> anyhow::Result<Option<Transaction>> {
		let rows = self.query("
			SELECT tx AS body FROM nursery_tx WHERE txid = $1
			UNION
			SELECT signed_tx AS body FROM virtual_transaction WHERE txid = $1 AND is_funding AND signed_tx IS NOT NULL
			UNION
			SELECT signed_tx AS body FROM offboards WHERE txid = $1
		", &[&txid.to_string()]).await?;
		if rows.is_empty() { return Ok(None); }
		ensure!(rows.len() == 1, "conflicting signed publication parent bytes");
		let transaction: Transaction = bitcoin::consensus::deserialize(rows[0].get("body"))?;
		ensure!(transaction.compute_txid() == txid, "signed parent publication txid mismatch");
		Ok(Some(transaction))
	}
}
