
use std::ops::Deref;
use std::str::FromStr;

use bitcoin::Txid;

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

impl<'t> Tx<'t> {
	/// Record operator change with its genuine, now-consumed pool input in
	/// the same transaction as the allocation. This is never reusable membership.
	pub(crate) async fn store_pool_change_retirements(
		&self, change: &[(VtxoId, VtxoId)],
	) -> anyhow::Result<()> {
		ensure!(change.len() <= 2, "pool retirement exceeds change-output bound");
		for (output, input) in change {
			self.execute("INSERT INTO vtxo_pool_retirement (vtxo_id,pool_input_id)
				SELECT o.vtxo_id,p.vtxo_id FROM vtxo o
				JOIN vtxo_pool p ON p.vtxo_id=$2 AND p.spent_at IS NOT NULL
				JOIN vtxo i ON i.vtxo_id=p.vtxo_id AND i.spend_state='spent' AND i.oor_spent_txid IS NOT NULL
				WHERE o.vtxo_id=$1 AND o.spend_state='pool' AND o.policy_type='pubkey'
				ON CONFLICT (vtxo_id) DO NOTHING", &[&output.to_string(), &input.to_string()]).await?;
			let stored = self.query_opt("SELECT pool_input_id FROM vtxo_pool_retirement WHERE vtxo_id=$1",
				&[&output.to_string()]).await?.map(|r| r.get::<_, String>(0));
			ensure!(stored.as_deref() == Some(input.to_string().as_str()), "conflicting or invalid pool change provenance");
		}
		Ok(())
	}

	pub(crate) async fn pool_change_retirement_input(&self, id: VtxoId)
		-> anyhow::Result<Option<(VtxoId, Txid)>>
	{
		let row = self.query_opt("SELECT r.pool_input_id,i.oor_spent_txid FROM vtxo_pool_retirement r
			JOIN vtxo_pool p ON p.vtxo_id=r.pool_input_id AND p.spent_at IS NOT NULL
			JOIN vtxo i ON i.vtxo_id=p.vtxo_id AND i.spend_state='spent' AND i.oor_spent_txid IS NOT NULL
			WHERE r.vtxo_id=$1", &[&id.to_string()]).await?;
		row.map(|r| Ok((VtxoId::from_str(r.get(0))?, Txid::from_str(r.get(1))?))).transpose()
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
