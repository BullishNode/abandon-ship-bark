//! Atomic expiry payments. The nursery and receipt share the coin-state commit.

use std::collections::BTreeMap;

use anyhow::Context;
use ark::{ProtocolEncoding, VtxoId, Vtxo};
use bitcoin::{ScriptBuf, Transaction};
use bitcoin::consensus::{deserialize, serialize};
use crate::expiry_payout::{FeeOutput, Payment, Status};
use tokio_postgres::Row;

use crate::nursery::NurseryTxKind;
use crate::SECP;
use super::Tx;

pub(crate) struct SettlementVtxo {
	pub id: VtxoId,
	pub vtxo: Vec<u8>,
	pub expiry: u32,
	pub payment_txid: String,
	pub unclaimed: bool,
	pub predecessors: Vec<Vec<u8>>,
}

impl SettlementVtxo {
	fn from_row(row: Row) -> anyhow::Result<Self> {
		Ok(Self {
			id: row.get::<_, &str>("vtxo_id").parse()?,
			payment_txid: row.get("payment_txid"),
			unclaimed: row.get("unclaimed"), predecessors: Vec::new(),
			vtxo: row.get("vtxo"), expiry: u32::try_from(row.get::<_, i32>("expiry"))?,
		})
	}
}

pub(crate) fn payout_script(vtxo: &Vtxo) -> ScriptBuf {
	ScriptBuf::new_p2tr(&SECP, vtxo.user_pubkey().x_only_public_key().0, None)
}

impl Tx<'_> {
	pub(crate) async fn expiry_settlement_page(
		&self, claimed: bool, tip: u32, grace: u32, minimum: u64,
		after: (u32, String), limit: u32,
	) -> anyhow::Result<Vec<SettlementVtxo>> {
		let rows = if claimed {
			self.query("SELECT id AS vtxo_id, ''::bytea AS vtxo, 0::integer AS expiry, txid AS payment_txid, false AS unclaimed
				FROM expiry_settlement WHERE id > $1 ORDER BY id LIMIT $2",
				&[&after.1, &(limit as i64)]).await?
		} else {
			self.query("SELECT v.vtxo_id, v.vtxo, v.expiry, ''::text AS payment_txid,
				v.spend_state='unclaimed' AS unclaimed FROM vtxo v
				WHERE (v.expiry::bigint, v.vtxo_id) > ($1::bigint, $2)
				AND NOT EXISTS (SELECT 1 FROM expiry_settlement s WHERE s.id = v.vtxo_id)
				AND v.policy_type = 'pubkey' AND v.spend_state IN ('spendable', 'unclaimed')
				AND v.confirmed_height IS NULL AND v.expiry::bigint + $3 <= $4
				AND v.amount >= $5 ORDER BY v.expiry, v.vtxo_id LIMIT $6",
				&[&(after.0 as i64), &after.1, &(grace as i64), &(tip as i64),
					&i64::try_from(minimum)?, &(limit as i64)]).await?
		};
		let mut candidates = rows.into_iter().map(SettlementVtxo::from_row).collect::<anyhow::Result<Vec<_>>>()?;
		for coin in &mut candidates {
			if !coin.unclaimed { continue; }
			let vtxo: Vtxo = Vtxo::deserialize(&coin.vtxo)?;
			let Some(hash) = vtxo.unlock_hash() else { continue; };
			coin.predecessors = self.query("SELECT v.vtxo FROM round_participation p
				JOIN round_part_input i ON i.participation_id=p.id
				LEFT JOIN vtxo v ON v.vtxo_id=i.vtxo_id
				WHERE p.unlock_hash=$1 AND p.round_id=$2 ORDER BY v.vtxo_id",
				&[&hash.to_string(), &vtxo.chain_anchor().txid.to_string()]).await?
				.into_iter().map(|row| row.get::<_, Option<Vec<u8>>>("vtxo"))
				.collect::<Option<Vec<_>>>().unwrap_or_default();
		}
		Ok(candidates)
	}

	/// A subset of one stored batch replays that full payment. Mixed/new IDs
	/// cannot consume a partial new batch after an ambiguous caller response.
	pub(crate) async fn expiry_replay(&self, ids: &[String]) -> anyhow::Result<Option<Payment>> {
		let rows = self.query("SELECT id,txid FROM expiry_settlement WHERE id = ANY($1)", &[&ids]).await?;
		if rows.is_empty() { return Ok(None) }
		let txid: String = rows[0].get("txid");
		if rows.len() != ids.len() || rows.iter().any(|r| r.get::<_, String>("txid") != txid) {
			return Ok(Some(Payment { status: Status::Ineligible,
				reason: "batch overlaps existing payments; retry each stored ID separately".into(),
				..Default::default() }));
		}
		Ok(Some(self.expiry_receipt(&txid).await?))
	}

	pub(crate) async fn expiry_receipt(&self, txid: &str) -> anyhow::Result<Payment> {
		let rows = self.query("SELECT s.raw_tx,s.fee_sat,v.vtxo FROM expiry_settlement s
			JOIN vtxo v ON v.vtxo_id=s.id WHERE s.txid=$1 ORDER BY s.id", &[&txid]).await?;
		let first = rows.first().context("expiry payment missing")?;
		let raw_tx: Vec<u8> = first.get("raw_tx");
		let tx: Transaction = deserialize(&raw_tx)?;
		let fee_sat = u64::try_from(first.get::<_, i64>("fee_sat"))?;
		let mut gross = BTreeMap::<ScriptBuf, u64>::new();
		for row in rows {
			let v = Vtxo::deserialize(row.get("vtxo"))?;
			*gross.entry(payout_script(&v)).or_default() += v.amount().to_sat();
		}
		let mut outputs = Vec::new();
		for (i, o) in tx.output.iter().enumerate() {
			if let Some(amount) = gross.remove(&o.script_pubkey) {
				outputs.push(FeeOutput { vout: i as u32,
					amount_sat: o.value.to_sat(), fee_sat: amount.checked_sub(o.value.to_sat())
						.context("expiry receipt exceeds gross")? });
			}
		}
		ensure!(gross.is_empty() && outputs.iter().map(|o| o.fee_sat).sum::<u64>() == fee_sat,
			"expiry receipt metadata does not match its transaction");
		Ok(Payment { status: Status::Paid, txid: txid.into(),
			raw_tx, fee_sat, outputs, reason: String::new() })
	}

	pub(crate) async fn expiry_inputs(
		&self, ids: &[String], tip: u32, grace: u32, minimum: u64,
	) -> anyhow::Result<Vec<Vtxo>> {
		let rows = self.query("SELECT v.vtxo FROM vtxo v WHERE v.vtxo_id = ANY($1)
			AND v.policy_type='pubkey' AND v.spend_state IN ('spendable','unclaimed')
			AND v.confirmed_height IS NULL AND v.expiry::bigint + $2 <= $3
			AND v.amount >= $4 AND v.spent_in_round IS NULL AND v.oor_spent_txid IS NULL
			AND v.offboarded_in IS NULL AND NOT EXISTS
			(SELECT 1 FROM round_part_input i JOIN round_participation p ON p.id=i.participation_id
			 WHERE i.vtxo_id=v.vtxo_id AND p.forfeited_at IS NULL)
			ORDER BY v.vtxo_id", &[&ids, &(grace as i64), &(tip as i64), &i64::try_from(minimum)?]).await?;
		rows.into_iter().map(|r| Ok(Vtxo::deserialize(r.get("vtxo"))?)).collect()
	}

	pub(crate) async fn store_expiry_payment(
		&self, ids: &[String], tx: &Transaction, fee: u64, tip: u32, grace: u32, minimum: u64,
		confirm_target: bitcoin_ext::BlockHeight,
	) -> anyhow::Result<()> {
		ensure!(self.expiry_inputs(ids, tip, grace, minimum).await?.len() == ids.len(), "expiry inputs changed");
		let n = self.execute("UPDATE vtxo SET spend_state='spent',updated_at=NOW()
			WHERE vtxo_id=ANY($1) AND spend_state IN ('spendable','unclaimed')", &[&ids]).await?;
		ensure!(n as usize == ids.len(), "expiry inputs changed during commit");
		let txid = tx.compute_txid().to_string();
		self.execute("INSERT INTO expiry_settlement (id,txid,raw_tx,fee_sat)
			SELECT unnest($1::text[]),$2,$3,$4", &[&ids, &txid, &serialize(tx), &i64::try_from(fee)?]).await?;
		self.upsert_nursery_tx(tx, NurseryTxKind::ExpiryPayout, confirm_target).await?;
		Ok(())
	}

	pub(crate) async fn pending_expiry_payments(&self, wallet_height: u32) -> anyhow::Result<Vec<Transaction>> {
		self.query("SELECT tx FROM nursery_tx WHERE kind::text='expiry-payout'
			AND (confirmed_at_height IS NULL OR confirmed_at_height::bigint > $1) ORDER BY id", &[&(wallet_height as i64)]).await?.into_iter()
			.map(|r| Ok(deserialize(r.get("tx"))?)).collect()
	}

	pub(crate) async fn expiry_settlement_spenders(&self, ids: &[VtxoId]) -> anyhow::Result<Vec<Option<String>>> {
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		Ok(self.query("SELECT v.onchain_spent_txid FROM UNNEST($1::text[]) WITH ORDINALITY AS requested(id,position)
			LEFT JOIN vtxo v ON v.vtxo_id=requested.id ORDER BY requested.position", &[&ids]).await?
			.into_iter().map(|r| r.get(0)).collect())
	}
}
