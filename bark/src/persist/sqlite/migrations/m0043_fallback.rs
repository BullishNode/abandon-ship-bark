//! Fresh installation of the authenticated immutable fallback record.

use rusqlite::Transaction;

use super::Migration;

pub struct Migration0043 {}

impl Migration for Migration0043 {
	fn name(&self) -> &str { "Immutable fallback account record" }

	fn to_version(&self) -> i64 { 43 }

	fn do_migration(&self, conn: &Transaction) -> anyhow::Result<()> {
		conn.execute_batch("
			CREATE TABLE bark_fallback_record (
				id INTEGER PRIMARY KEY CHECK (id = 1),
				record BLOB NOT NULL,
				spk BLOB NOT NULL,
				account_xpub BLOB NOT NULL
			);
		")?;
		Ok(())
	}
}
