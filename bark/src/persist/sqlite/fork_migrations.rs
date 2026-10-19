//! Fork schema changes must not consume upstream SQLite migration numbers.

use rusqlite::Connection;

const VERSION: i64 = 1;
const NAME: &str = "abandon_ship";

pub(super) fn run(conn: &mut Connection) -> anyhow::Result<()> {
	let tx = conn.transaction()?;
	tx.execute_batch("CREATE TABLE IF NOT EXISTS bark_expiry_migration (
		id INTEGER PRIMARY KEY CHECK (id = 1),
		version INTEGER NOT NULL,
		name TEXT NOT NULL
	);")?;
	let version = tx.query_row("SELECT COALESCE(MAX(version), 0) FROM bark_expiry_migration", [], |row| row.get::<_, i64>(0))?;
	ensure!(version <= VERSION, "fallback schema is newer than this wallet build");
	// Earlier fork databases also used version one, but have a different schema.
	let named = tx.query_row("SELECT EXISTS (
		SELECT 1 FROM pragma_table_info('bark_expiry_migration') WHERE name = 'name'
	)", [], |row| row.get::<_, bool>(0))?;
	ensure!(named, "fallback schema was created by an unsupported wallet build");
	if version == 0 {
		tx.execute_batch("CREATE TABLE bark_fallback_record (
			id INTEGER PRIMARY KEY CHECK (id = 1),
			record BLOB NOT NULL,
			spk BLOB NOT NULL,
			account_xpub BLOB NOT NULL,
			registered INTEGER NOT NULL DEFAULT 1
		);
		CREATE TABLE bark_fallback_snapshot (
			id INTEGER PRIMARY KEY CHECK (id = 1), snapshot BLOB NOT NULL
		);")?;
		tx.execute("INSERT INTO bark_expiry_migration (id, version, name) VALUES (1, ?1, ?2)", (VERSION, NAME))?;
	} else {
		ensure!(version == VERSION, "fallback schema has an unsupported version");
		let name = tx.query_row("SELECT name FROM bark_expiry_migration WHERE id = 1", [], |row| row.get::<_, String>(0))?;
		ensure!(name == NAME, "fallback schema does not match this wallet build");
	}
	tx.commit()?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use super::super::migrations::MigrationContext;

	fn schema(conn: &Connection) -> Vec<(String, String)> {
		conn.prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
			.unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
			.unwrap().collect::<rusqlite::Result<_>>().unwrap()
	}

	#[test]
	fn final_schema_reopens_without_changing_wallet_records_or_upstream_history() {
		let mut conn = Connection::open_in_memory().unwrap();
		let upstream = MigrationContext::new();
		upstream.do_all_migrations(&mut conn).unwrap();
		let upstream_history: (i64, i64) = conn.query_row("SELECT COUNT(*), MAX(value) FROM migrations", [],
			|row| Ok((row.get(0)?, row.get(1)?))).unwrap();
		run(&mut conn).unwrap();
		conn.execute_batch("INSERT INTO bark_fallback_record (id, record, spk, account_xpub) VALUES (1, X'01', X'02', X'03');
			INSERT INTO bark_fallback_snapshot (id, snapshot) VALUES (1, X'04');").unwrap();
		assert_eq!(conn.query_row("SELECT registered FROM bark_fallback_record", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
		conn.execute("UPDATE bark_fallback_record SET registered = 0", []).unwrap();

		upstream.do_all_migrations(&mut conn).unwrap();
		run(&mut conn).unwrap();
		assert_eq!(conn.query_row("SELECT COUNT(*), MAX(value) FROM migrations", [],
			|row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).unwrap(), upstream_history);
		assert_eq!(conn.query_row("SELECT version, name FROM bark_expiry_migration", [],
			|row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).unwrap(), (VERSION, NAME.to_owned()));
		assert_eq!(conn.query_row("SELECT record, spk, account_xpub, registered FROM bark_fallback_record", [],
			|row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, i64>(3)?)))
			.unwrap(), (vec![1], vec![2], vec![3], 0));
		assert_eq!(conn.query_row("SELECT snapshot FROM bark_fallback_snapshot", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), vec![4]);
	}

	#[test]
	fn earlier_fork_version_one_is_refused_without_changes() {
		let mut conn = Connection::open_in_memory().unwrap();
		conn.execute_batch("CREATE TABLE bark_expiry_migration (
			id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL
		);
		INSERT INTO bark_expiry_migration (id, version) VALUES (1, 1);
		CREATE TABLE bark_fallback_record (
			id INTEGER PRIMARY KEY CHECK (id = 1), record BLOB NOT NULL,
			spk BLOB NOT NULL, account_xpub BLOB NOT NULL
		);
		INSERT INTO bark_fallback_record VALUES (1, X'01', X'02', X'03');").unwrap();
		let before = schema(&conn);
		assert!(run(&mut conn).unwrap_err().to_string().contains("unsupported wallet build"));
		assert_eq!(schema(&conn), before);
		assert_eq!(conn.query_row("SELECT record FROM bark_fallback_record", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), vec![1]);
		assert_eq!(conn.query_row("SELECT version FROM bark_expiry_migration", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
	}

	#[test]
	fn different_migration_identity_is_refused() {
		let mut conn = Connection::open_in_memory().unwrap();
		run(&mut conn).unwrap();
		conn.execute("UPDATE bark_expiry_migration SET name = 'other_schema'", []).unwrap();
		let before = schema(&conn);
		assert!(run(&mut conn).unwrap_err().to_string().contains("does not match"));
		assert_eq!(schema(&conn), before);
		assert_eq!(conn.query_row("SELECT name FROM bark_expiry_migration", [], |row| row.get::<_, String>(0)).unwrap(), "other_schema");
	}

	#[test]
	fn newer_fork_versions_are_refused() {
		for version in [2, 3] {
			let mut conn = Connection::open_in_memory().unwrap();
			conn.execute_batch("CREATE TABLE bark_expiry_migration (
				id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL
			);").unwrap();
			conn.execute("INSERT INTO bark_expiry_migration (id, version) VALUES (1, ?1)", [version]).unwrap();
			let before = schema(&conn);
			assert!(run(&mut conn).unwrap_err().to_string().contains("newer than this wallet build"));
			assert_eq!(schema(&conn), before);
		}
	}

	#[test]
	fn failed_schema_creation_rolls_back_the_migration() {
		let mut conn = Connection::open_in_memory().unwrap();
		conn.execute_batch("CREATE TABLE bark_fallback_snapshot (snapshot BLOB NOT NULL);
			INSERT INTO bark_fallback_snapshot VALUES (X'01');").unwrap();
		let before = schema(&conn);
		assert!(run(&mut conn).is_err());
		assert_eq!(schema(&conn), before);
		assert_eq!(conn.query_row("SELECT snapshot FROM bark_fallback_snapshot", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), vec![1]);
	}
}
