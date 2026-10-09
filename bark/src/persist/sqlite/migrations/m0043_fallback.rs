//! Persist the fallback destination and distinguish linked keys from issued keys.

use rusqlite::Transaction;

use super::Migration;

pub struct Migration0043 {}

impl Migration for Migration0043 {
	fn name(&self) -> &str { "Fallback record and linked key pool" }

	fn to_version(&self) -> i64 { 43 }

	fn do_migration(&self, conn: &Transaction) -> anyhow::Result<()> {
		conn.execute_batch("
			ALTER TABLE bark_vtxo_key ADD COLUMN linked INTEGER NOT NULL DEFAULT 0;
			ALTER TABLE bark_vtxo_key ADD COLUMN issued INTEGER NOT NULL DEFAULT 1;
			CREATE TABLE bark_fallback_record (
				id INTEGER PRIMARY KEY CHECK (id = 1),
				spk BLOB NOT NULL,
				seq INTEGER NOT NULL CHECK (seq >= 0)
			);
		")?;
		Ok(())
	}
}
