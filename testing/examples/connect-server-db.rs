use server::database::Db;

async fn connect_only() -> anyhow::Result<()> {
	let raw = std::env::var("POSTGRES")?;
	let config: server::config::Postgres = serde_json::from_str(&raw)?;
	let db = Db::connect(&config).await?;
	drop(db);
	Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
	match connect_only().await {
		Ok(()) => {
			println!("db-connect-only ok");
			std::process::ExitCode::SUCCESS
		}
		Err(_) => {
			eprintln!("db-connect-only failed");
			std::process::ExitCode::FAILURE
		}
	}
}
