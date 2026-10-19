use bitcoin::consensus::{deserialize, serialize};
use bitcoin::{Transaction, Txid};
use bitcoin_ext::BlockHeight;

use super::Tx;

impl Tx<'_> {
	pub(crate) async fn record_user_exit_claim(
		&self, transaction: &Transaction, height: BlockHeight,
	) -> anyhow::Result<()> {
		let height = i32::try_from(height.to_u32())?;
		self.execute("
			INSERT INTO user_exit_claim (id, raw_tx, confirmed_height)
			VALUES ($1, $2, $3)
			ON CONFLICT (id) DO UPDATE SET confirmed_height = EXCLUDED.confirmed_height
		", &[&transaction.compute_txid().to_string(), &serialize(transaction), &height]).await?;
		Ok(())
	}

	pub(crate) async fn confirm_user_exit_claims(&self, txids: &[Txid], height: BlockHeight) -> anyhow::Result<()> {
		let height = i32::try_from(height.to_u32())?;
		let txids = txids.iter().map(ToString::to_string).collect::<Vec<_>>();
		self.execute("
			UPDATE user_exit_claim SET confirmed_height = $2
			WHERE id = ANY($1) AND confirmed_height IS DISTINCT FROM $2::int4
		", &[&txids, &height]).await?;
		Ok(())
	}

	pub(crate) async fn reorg_user_exit_claims(&self, height: BlockHeight) -> anyhow::Result<()> {
		let height = i32::try_from(height.to_u32())?;
		self.execute("
			UPDATE user_exit_claim SET confirmed_height = NULL WHERE confirmed_height > $1
		", &[&height]).await?;
		Ok(())
	}

	pub(crate) async fn pending_user_exit_claims(&self) -> anyhow::Result<Vec<Transaction>> {
		let rows = self.query("
			SELECT raw_tx FROM user_exit_claim WHERE confirmed_height IS NULL ORDER BY id
		", &[]).await?;
		rows.iter().map(|row| Ok(deserialize(row.get("raw_tx"))?)).collect()
	}
}
