//! Fork schema changes must not consume upstream SQLite migration numbers.

use rusqlite::Connection;

pub(super) fn run(conn: &mut Connection) -> anyhow::Result<()> {
	let tx = conn.transaction()?;
	tx.execute_batch("CREATE TABLE IF NOT EXISTS bark_expiry_migration (
		id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL
	);")?;
	let version = tx.query_row("SELECT COALESCE(MAX(version), 0) FROM bark_expiry_migration", [], |row| row.get::<_, i64>(0))?;
	ensure!(version <= 2, "fallback schema is newer than this wallet build");
	if version == 0 {
		tx.execute_batch("CREATE TABLE bark_fallback_record (
			id INTEGER PRIMARY KEY CHECK (id = 1),
			record BLOB NOT NULL,
			spk BLOB NOT NULL,
			account_xpub BLOB NOT NULL
		);
		INSERT INTO bark_expiry_migration (id, version) VALUES (1, 1);")?;
	}
	if version < 2 {
		tx.execute_batch("CREATE TABLE bark_fallback_snapshot (
			id INTEGER PRIMARY KEY CHECK (id = 1), snapshot BLOB NOT NULL
		);
		UPDATE bark_expiry_migration SET version = 2 WHERE id = 1;")?;
	}
	tx.commit()?;
	Ok(())
}
