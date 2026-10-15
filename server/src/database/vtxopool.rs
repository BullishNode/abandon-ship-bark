
use std::ops::Deref;
use std::collections::HashMap;

use anyhow::Context;

use futures::{Stream, TryStreamExt};
use postgres_types::Type;
use tokio_postgres::Row;
use ark::{ProtocolEncoding, Vtxo, VtxoId};
use ark::vtxo::Full;

use crate::database::{Db, OwnedRowStream, Tx, NOARG};


/// A struct reprensenting a vtxo currently in the vtxo pool.
#[derive(Debug, Clone)]
pub struct PoolVtxo(Vtxo<Full>);

impl PoolVtxo {
	pub fn new(vtxo: Vtxo<Full>) -> Self {
		PoolVtxo(vtxo)
	}

	pub fn inner(&self) -> &Vtxo<Full> {
		&self.0
	}

	pub fn into_inner(self) -> Vtxo<Full> {
		self.0
	}
}

impl Deref for PoolVtxo {
	type Target = Vtxo<Full>;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl TryFrom<Row> for PoolVtxo {
	type Error = anyhow::Error;

	fn try_from(row: Row) -> Result<Self, Self::Error> {
		Ok(PoolVtxo(Vtxo::deserialize(row.get("vtxo"))?))
	}
}


/// A private native observation. The tweak never leaves the owning daemon.
/// No Debug/Serialize implementation: an admin response contains only key flags.
pub(crate) struct PoolInventoryRow {
	pub id: String,
	pub vtxo: Vtxo<Full>,
	pub pool_spent: bool,
	pub spend_state: String,
	pub has_spend_reference: bool,
	pub key_tweak: Option<Vec<u8>>,
}

impl<'t> Tx<'t> {
	pub(crate) async fn pool_inventory_rows(&self, ids: &[VtxoId]) -> anyhow::Result<Vec<PoolInventoryRow>> {
		let ids = ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
		// The caller uses a repeatable-read transaction for these rows and keys.
		// Reject oversize ancestry before returning its bytes to the daemon.
		let rows = self.query(
			"SELECT p.vtxo_id, CASE WHEN octet_length(p.vtxo) <= 65536 \
				AND sum(octet_length(p.vtxo)) OVER () <= 524288 THEN p.vtxo END AS vtxo, \
				(p.spent_at IS NOT NULL) AS pool_spent, v.spend_state::text AS spend_state, \
				(v.oor_spent_txid IS NOT NULL OR v.spent_in_round IS NOT NULL OR v.offboarded_in IS NOT NULL \
				OR v.onchain_spent_txid IS NOT NULL) AS has_spend_reference \
			FROM vtxo_pool p JOIN vtxo v ON v.vtxo_id = p.vtxo_id \
			WHERE p.vtxo_id = ANY($1) ORDER BY p.vtxo_id",
			&[&ids],
		).await?;
		let mut result = rows.into_iter().map(|r| Ok(PoolInventoryRow {
			id: r.get("vtxo_id"),
			vtxo: Vtxo::deserialize(&r.get::<_, Option<Vec<u8>>>("vtxo")
				.context("pool candidate ancestry exceeds per-coin or aggregate byte limit")?)?,
			pool_spent: r.get("pool_spent"), spend_state: r.get("spend_state"),
			has_spend_reference: r.get("has_spend_reference"), key_tweak: None,
		})).collect::<anyhow::Result<Vec<_>>>()?;
		let pubkeys = result.iter().map(|r| r.vtxo.user_pubkey().to_string()).collect::<Vec<_>>();
		let keys = self.query("SELECT pubkey, tweak FROM ephemeral_tweak WHERE pubkey = ANY($1)", &[&pubkeys]).await?
			.into_iter().map(|r| (r.get::<_, String>("pubkey"), r.get::<_, Vec<u8>>("tweak")))
			.collect::<HashMap<_, _>>();
		for row in &mut result {
			row.key_tweak = keys.get(&row.vtxo.user_pubkey().to_string()).cloned();
		}
		Ok(result)
	}

	pub async fn get_pool_vtxos_by_ids(&self, ids: &[VtxoId]) -> anyhow::Result<Vec<PoolVtxo>> {
		let stmt = self.prepare_typed(
			"SELECT vtxo FROM vtxo_pool WHERE vtxo_id = ANY($1)",
			&[Type::TEXT_ARRAY],
		).await?;
		let rows = self.query(&stmt, &[&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()]).await?;

		let mut vtxos = vec![];
		for row in rows {
			vtxos.push(PoolVtxo::try_from(row)?);
		}

		Ok(vtxos)
	}

	pub async fn store_vtxopool_vtxo(
		&self,
		vtxo: &PoolVtxo,
	) -> anyhow::Result<()> {
		let stmt = self.prepare_typed(
			"INSERT INTO vtxo_pool (vtxo_id, vtxo, expiry_height, amount) \
				VALUES ( $1, $2, $3, $4)",
			&[Type::TEXT, Type::BYTEA, Type::INT4, Type::INT8],
		).await?;

		self.execute(&stmt, &[
			&vtxo.id().to_string(),
			&vtxo.serialize(),
			&(vtxo.expiry_height().to_u32() as i32),
			&(vtxo.amount().to_sat() as i64),
		]).await?;
		Ok(())
	}

	pub async fn store_vtxopool_vtxos<'a>(
		&self,
		vtxos: impl IntoIterator<Item = &'a PoolVtxo>,
	) -> anyhow::Result<()> {
		let stmt = self.prepare_typed(
			"INSERT INTO vtxo_pool (vtxo_id, vtxo, expiry_height, amount) \
				VALUES ( UNNEST($1), UNNEST($2), UNNEST($3), UNNEST($4) )",
			&[Type::TEXT_ARRAY, Type::BYTEA_ARRAY, Type::INT4_ARRAY, Type::INT8_ARRAY],
		).await?;

		let (ids, vtxos, expiries, amounts) = vtxos.into_iter().map(|v|
			(v.id().to_string(),
			v.serialize(),
			v.expiry_height().to_u32() as i32,
			v.amount().to_sat() as i64,
		)).collect::<(Vec<_>, Vec<_>, Vec<_>, Vec<_>)>();

		self.execute(&stmt, &[&ids, &vtxos, &expiries, &amounts]).await?;
		Ok(())
	}

	pub async fn mark_vtxopool_vtxos_spent(
		&self,
		ids: impl IntoIterator<Item = VtxoId>,
	) -> anyhow::Result<()> {
		let stmt = self.prepare_typed(
			"UPDATE vtxo_pool SET spent_at = NOW() WHERE vtxo_id = ANY($1)",
			&[Type::TEXT_ARRAY],
		).await?;

		let ids = ids.into_iter().map(|id| id.to_string()).collect::<Vec<_>>();
		self.execute(&stmt, &[&ids]).await?;
		Ok(())
	}
}

impl Db {
	pub async fn load_vtxopool(
		&self,
	) -> anyhow::Result<impl Stream<Item = anyhow::Result<PoolVtxo>> + '_> {
		let conn = self.get_conn().await?;
		// fetch without keys
		let stmt = conn.prepare(
			"SELECT vtxo_id, vtxo, expiry_height, amount FROM vtxo_pool WHERE spent_at IS NULL",
		).await?;

		let rows = conn.query_raw(&stmt, NOARG).await?;
		let stream = OwnedRowStream::new(conn, rows);
		Ok(stream.err_into::<anyhow::Error>().and_then(
			|row| futures::future::ready(PoolVtxo::try_from(row))
		))
	}
}
