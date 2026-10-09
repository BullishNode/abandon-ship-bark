use ark::{ProtocolEncoding, Vtxo};
use bitcoin::Txid;

use super::Tx;

impl Tx<'_> {
	/// A cosign retry may refer to a funding transaction we already track.
	pub(crate) async fn is_pending_board_funding(&self, txid: Txid) -> anyhow::Result<bool> {
		Ok(self.query_one("SELECT EXISTS (SELECT 1 FROM pending_board p
			JOIN vtxo v ON v.vtxo_id=p.id WHERE v.vtxo_txid=$1)",
			&[&txid.to_string()]).await?.get(0))
	}

	/// Call before inserting the funding anchor, in the same transaction.
	/// Competing requests cannot reserve two leaf IDs against one outpoint.
	pub(crate) async fn store_pending_board(&self, vtxo: &Vtxo) -> anyhow::Result<()> {
		let anchor = vtxo.chain_anchor().to_string();
		self.execute("INSERT INTO pending_board (id,vtxo_id,vtxo,expiry)
			VALUES ($1,$2,$3,$4) ON CONFLICT (id) DO NOTHING",
			&[&anchor, &vtxo.id().to_string(), &vtxo.serialize(),
				&(vtxo.expiry_height().to_u32() as i32)]).await?;
		let row = self.query_one("SELECT vtxo FROM pending_board WHERE id=$1 FOR UPDATE",
			&[&anchor]).await?;
		ensure!(row.get::<_, Vec<u8>>(0) == vtxo.serialize(),
			"funding outpoint already reserved for a different board");
		Ok(())
	}

	/// Keep this lock until registration commits. Payout uses the same durable
	/// row even before the unsigned user coin exists in the ordinary table.
	pub(crate) async fn lock_board_registration(&self, vtxo: &Vtxo) -> anyhow::Result<()> {
		if let Some(row) = self.query_opt("SELECT vtxo_id FROM pending_board WHERE id=$1 FOR UPDATE",
			&[&vtxo.chain_anchor().to_string()]).await? {
			ensure!(row.get::<_, String>(0) == vtxo.id().to_string(),
				"funding outpoint reserved for a different board");
		}
		ensure!(self.query_opt("SELECT id FROM expiry_settlement WHERE id=$1",
			&[&vtxo.id().to_string()]).await?.is_none(), "board already paid at expiry");
		Ok(())
	}
}
