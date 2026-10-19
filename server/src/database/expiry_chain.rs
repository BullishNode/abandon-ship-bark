//! Durable invalidation and completed-sync acknowledgments for payout work.

use anyhow::Context;
use bitcoin::BlockHash;
use bitcoin_ext::{BlockHeight, BlockRef};

use super::{BlockTable, Db, Tx};
use crate::sync::{BlockData, ChainEventListener, RawMempool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryViewToken {
	pub generation: i64,
	pub captaind: BlockRef,
	pub watchmand: BlockRef,
}

pub struct ExpiryChainListener {
	db: Db,
	process: BlockTable,
}

fn process_name(process: BlockTable) -> &'static str {
	match process { BlockTable::Captaind => "captaind", BlockTable::Watchmand => "watchmand" }
}

impl ExpiryChainListener {
	/// A restarted process must complete an actual sync before its old ack can
	/// authorize a rebuild. Initial invalidation is also durable across crashes.
	pub async fn start(db: Db, process: BlockTable) -> anyhow::Result<Self> {
		db.write(async |t| {
			t.invalidate_expiry_chain(BlockHeight::ZERO).await?;
			t.execute("DELETE FROM expiry_sync_ack WHERE process=$1", &[&process_name(process)]).await?;
			Ok(())
		}).await?;
		Ok(Self { db, process })
	}
}

#[async_trait]
impl ChainEventListener for ExpiryChainListener {
	async fn on_block_added(&self, _block: &BlockData) -> anyhow::Result<()> { Ok(()) }
	async fn on_mempool_update(&self, _mempool: &RawMempool) -> anyhow::Result<()> { Ok(()) }

	async fn on_reorg(&self, fork: BlockRef) -> anyhow::Result<()> {
		self.db.write(async |t| t.invalidate_expiry_chain(fork.height).await).await
	}

	async fn on_sync_complete(&self, point: BlockRef) -> anyhow::Result<()> {
		self.db.write(async |t| {
			let row = t.query_one("SELECT generation FROM expiry_chain_view WHERE id=1 FOR UPDATE", &[]).await?;
			let generation: i64 = row.get("generation");
			// This callback runs only after all concurrent listeners have
			// succeeded and their block table's tip has been published.
			ensure!(t.get_highest_block(self.process).await? == Some(point), "completed expiry sync tip changed");
			t.execute("INSERT INTO expiry_sync_ack(process,generation,height,hash) VALUES($1,$2,$3,$4) \
				ON CONFLICT(process) DO UPDATE SET generation=EXCLUDED.generation,height=EXCLUDED.height,hash=EXCLUDED.hash",
				&[&process_name(self.process), &generation, &i64::from(point.height), &point.hash.to_string()]).await?;
			Ok(())
		}).await
	}
}

impl Tx<'_> {
	pub async fn invalidate_expiry_chain(&self, fork: BlockHeight) -> anyhow::Result<()> {
		self.execute("UPDATE expiry_chain_view SET generation=generation+1, \
			invalid_from=CASE WHEN invalid THEN LEAST(invalid_from,$1) ELSE $1 END, invalid=TRUE WHERE id=1",
			&[&i64::from(fork)]).await?;
		Ok(())
	}

	/// A token is available for rebuilding only after both daemons completed
	/// successful sync cycles for the current invalidation generation. The
	/// caller additionally verifies both hashes against Core's canonical chain.
	pub async fn expiry_rebuild_token(&self) -> anyhow::Result<Option<ExpiryViewToken>> {
		let row = self.query_one("SELECT generation FROM expiry_chain_view WHERE id=1", &[]).await?;
		let generation: i64 = row.get("generation");
		let acks = self.query("SELECT process,height,hash FROM expiry_sync_ack WHERE generation=$1 ORDER BY process", &[&generation]).await?;
		if acks.len() != 2 { return Ok(None); }
		let point = |name: &str| -> anyhow::Result<BlockRef> {
			let row = acks.iter().find(|r| r.get::<_, &str>("process") == name).context("missing expiry process ack")?;
			Ok(BlockRef {
				height: BlockHeight::try_from(row.get::<_, i64>("height"))?,
				hash: row.get::<_, &str>("hash").parse::<BlockHash>()?,
			})
		};
		let token = ExpiryViewToken { generation, captaind: point("captaind")?, watchmand: point("watchmand")? };
		if self.get_highest_block(BlockTable::Captaind).await? != Some(token.captaind)
			|| self.get_highest_block(BlockTable::Watchmand).await? != Some(token.watchmand) { return Ok(None); }
		Ok(Some(token))
	}

	/// A completed view with exactly the same visible financial commits needs
	/// no reconstruction. The watchman still verifies these hashes against Core.
	pub async fn expiry_view_is_rebuilt(&self, token: ExpiryViewToken) -> anyhow::Result<bool> {
		let row = self.query_one("SELECT invalid,generation,rebuilt_captaind_height,rebuilt_captaind_hash, \
			rebuilt_watchmand_height,rebuilt_watchmand_hash,rebuilt_ledger_count,rebuilt_ledger_revision, \
			(SELECT COUNT(*) FROM expiry_ledger_commit) AS ledger_count, \
			(SELECT COALESCE(MAX(revision),0) FROM expiry_ledger_commit) AS ledger_revision \
			FROM expiry_chain_view WHERE id=1", &[]).await?;
		Ok(!row.get::<_, bool>("invalid") && row.get::<_, i64>("generation")==token.generation
			&& row.get::<_, Option<i64>>("rebuilt_captaind_height")==Some(i64::from(token.captaind.height))
			&& row.get::<_, Option<&str>>("rebuilt_captaind_hash")==Some(token.captaind.hash.to_string().as_str())
			&& row.get::<_, Option<i64>>("rebuilt_watchmand_height")==Some(i64::from(token.watchmand.height))
			&& row.get::<_, Option<&str>>("rebuilt_watchmand_hash")==Some(token.watchmand.hash.to_string().as_str())
			&& row.get::<_, Option<i64>>("rebuilt_ledger_count")==Some(row.get::<_, i64>("ledger_count"))
			&& row.get::<_, Option<i64>>("rebuilt_ledger_revision")==Some(row.get::<_, i64>("ledger_revision")))
	}

	/// Call only after every chain-derived allocation and entitlement relationship
	/// has been rebuilt in this same transaction from the token's canonical view.
	/// An older rebuild can never clear a later invalidation.
	pub async fn finish_expiry_rebuild(&self, token: ExpiryViewToken) -> anyhow::Result<bool> {
		self.query_one("SELECT generation FROM expiry_chain_view WHERE id=1 FOR UPDATE", &[]).await?;
		if self.expiry_rebuild_token().await? != Some(token) { return Ok(false); }
		// Include this actual rebuild transaction before taking the ledger
		// watermark. The row trigger deduplicates the following view update.
		self.execute("INSERT INTO expiry_ledger_commit(transaction_id) VALUES(pg_current_xact_id()) ON CONFLICT(transaction_id) DO NOTHING", &[]).await?;
		let ledger = self.query_one("SELECT COUNT(*) AS count,COALESCE(MAX(revision),0) AS revision FROM expiry_ledger_commit", &[]).await?;
		let n = self.execute("UPDATE expiry_chain_view SET invalid=FALSE, rebuilt_captaind_height=$2, \
			rebuilt_captaind_hash=$3, rebuilt_watchmand_height=$4, rebuilt_watchmand_hash=$5, \
			rebuilt_ledger_count=$6,rebuilt_ledger_revision=$7 \
			WHERE id=1 AND generation=$1",
			&[&token.generation, &i64::from(token.captaind.height), &token.captaind.hash.to_string(),
				&i64::from(token.watchmand.height), &token.watchmand.hash.to_string(),
				&ledger.get::<_, i64>("count"), &ledger.get::<_, i64>("revision")]).await?;
		Ok(n == 1)
	}

	/// Hold this shared row lock through decision commit or transaction broadcast.
	/// Invalidation takes the exclusive lock, serializing it with both callers.
	pub async fn lock_valid_expiry_view(&self, token: ExpiryViewToken) -> anyhow::Result<()> {
		let row = self.query_one("SELECT generation,invalid,rebuilt_captaind_height,rebuilt_captaind_hash, \
			rebuilt_watchmand_height,rebuilt_watchmand_hash FROM expiry_chain_view WHERE id=1 FOR SHARE", &[]).await?;
		ensure!(!row.get::<_, bool>("invalid") && row.get::<_, i64>("generation") == token.generation,
			"expiry chain view is invalidated");
		ensure!(row.get::<_, Option<i64>>("rebuilt_captaind_height") == Some(i64::from(token.captaind.height))
			&& row.get::<_, Option<&str>>("rebuilt_captaind_hash") == Some(token.captaind.hash.to_string().as_str())
			&& row.get::<_, Option<i64>>("rebuilt_watchmand_height") == Some(i64::from(token.watchmand.height))
			&& row.get::<_, Option<&str>>("rebuilt_watchmand_hash") == Some(token.watchmand.hash.to_string().as_str()),
			"expiry view token does not identify the rebuilt view");
		Ok(())
	}
}
