//! Zero-principal App startup controls. No wallet/Board/LN/exit action is sent.

use super::*;

async fn empty_money_rows(db: &Db) -> (i64, i64, i64, i64) {
	db.read(async |t| {
		let r = t.query_one("SELECT (SELECT count(*) FROM vtxo)::bigint AS vtxos,
			(SELECT count(*) FROM expiry_allocation)::bigint AS allocations,
			(SELECT count(*) FROM nursery_tx)::bigint AS nursery,
			(SELECT count(*) FROM wallet_changeset)::bigint AS wallet", &[]).await?;
		Ok((r.get("vtxos"), r.get("allocations"), r.get("nursery"), r.get("wallet")))
	}).await.unwrap()
}

async fn preworker_refusal(cfg: &server::Config, db: &Db, name: &str, expected: &str) {
	let before = empty_money_rows(db).await;
	let path = cfg.data_dir.join(format!("{name}-config.toml"));
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
	file.write_all(toml::to_string_pretty(cfg).unwrap().as_bytes()).unwrap(); file.sync_all().unwrap();
	let stdout = cfg.data_dir.join(format!("{name}-stdout.log"));
	let stderr = cfg.data_dir.join(format!("{name}-stderr.log"));
	let mut child = Command::new(std::env::var_os("CAPTAIND_EXEC").unwrap()).arg("start").arg("--config").arg(path)
		.stdout(OpenOptions::new().write(true).create_new(true).mode(0o600).open(&stdout).unwrap())
		.stderr(OpenOptions::new().write(true).create_new(true).mode(0o600).open(&stderr).unwrap())
		.kill_on_drop(true).spawn().unwrap();
	let result = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait()).await;
	if result.is_err() { child.kill().await.unwrap(); child.wait().await.unwrap(); }
	let status = result.expect("preworker refusal did not terminate").unwrap();
	assert!(!status.success());
	let logs = std::fs::read_to_string(stdout).unwrap() + &std::fs::read_to_string(stderr).unwrap();
	assert!(logs.contains(expected), "startup exited for an unexpected reason; inspect private retained logs");
	assert_eq!(empty_money_rows(db).await, before, "refused startup changed financial rows or wallet state");
}

#[tokio::test]
async fn urgent_budget_empty_legacy_app_starts_and_direct_partial_installation_refuses() {
	Box::pin(startup_control()).await;
}

async fn startup_control() {
	let ctx = Box::pin(TestContext::new("carrier/urgent_budget_empty_legacy_app")).await;
	let srv = Box::pin(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.cfg(|cfg| cfg.direct_expiry.enabled = false).create()).await;
	let mut cfg = srv.config().clone();
	let db = Db::connect(&cfg.postgres).await.unwrap();
	let first = empty_money_rows(&db).await;
	assert_eq!((first.0, first.1, first.2), (0, 0, 0));
	assert_eq!(srv.wallet_status().await.total(), Amount::ZERO);
	srv.stop().await.unwrap();
	let directory = cfg.data_dir.join("direct-expiry-witness");
	let backup = cfg.data_dir.join("fresh-unused-budget-preserved"); std::fs::create_dir(&backup).unwrap();
	// Preserve the fresh zero-asset installation exactly; no prior charged
	// attempt, signed publication or customer source exists in this fixture.
	for name in ["ordinary-fee-policy.json", "ordinary-fee-opening.json", "ordinary-fee-budget"] {
		let path = directory.join(name);
		if path.exists() { std::fs::rename(path, backup.join(name)).unwrap(); }
	}
	srv.start().await.unwrap();
	assert_eq!(srv.wallet_status().await.total(), Amount::ZERO);
	srv.stop().await.unwrap();
	cfg.direct_expiry.enabled = true;
	preworker_refusal(&cfg, &db, "direct-missing", "direct financial workers require the installed immutable fee budget").await;
	cfg.direct_expiry.enabled = false;
	let file = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(directory.join("ordinary-fee-budget")).unwrap(); file.sync_all().unwrap();
	preworker_refusal(&cfg, &db, "legacy-partial", "direct payout recovery witness validation failed").await;
	assert_eq!(std::fs::metadata(directory.join("ordinary-fee-budget")).unwrap().len(), 0);
	assert!(!directory.join("ordinary-fee-policy.json").exists());
	assert!(!directory.join("ordinary-fee-opening.json").exists());
	println!("actual empty legacy App ready; missing direct and partial legacy budget refused before financial state change; principal=0");
}
