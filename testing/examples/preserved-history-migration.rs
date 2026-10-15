//! Test-only offline migration entrypoint; no Server, keys, chain, or dispatch.
use std::path::Path;

use server::database::Db;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let path = std::env::args_os().nth(1).ok_or("fresh public PG config required")?;
	let raw = std::fs::read(path)?;
	if raw.len() > 4096 { return Err("bounded local PG config required".into()); }
	let config: server::config::Postgres = serde_json::from_slice(&raw)?;
	if !Path::new(&config.host).starts_with("/opt/two-step/native-runs")
		|| !config.host.ends_with("/migration-socket")
		|| !["a69_preserved", "a72_fresh"].contains(&config.name.as_str())
		|| config.password.is_some()
		|| config.max_connections != 2 {
		return Err("isolated local migration config required".into());
	}
	let _db = Db::connect(&config).await?;
	println!("ACTUAL_EMBEDDED_REFINERY_DB_CONNECT_COMPLETED");
	Ok(())
}
