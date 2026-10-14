//! Actual daemon scheduling of an already source-frozen Board group.
//! This does not qualify automatic source discovery or unresolved exchanges.

use super::*;

fn config(case: &CrashCase, max_miner_fee: u64) -> PathBuf {
	let original = case.ctx.datadir.join("server-watchmand/config.toml");
	let mut cfg: toml::Value = toml::from_str(&std::fs::read_to_string(original).unwrap()).unwrap();
	let cap: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	cfg["watchman"].as_table_mut().unwrap().insert("direct_expiry".into(), toml::Value::Table(
		toml::map::Map::from_iter([
			("history_dir".into(), toml::Value::String(cap.data_dir.to_str().unwrap().into())),
			("max_miner_fee".into(), toml::Value::String(format!("{max_miner_fee} sat"))),
		])));
	cfg["fee_estimator"].as_table_mut().unwrap().insert("max_fee_rate".into(), toml::Value::String("50 sat/vb".into()));
	cfg["watchman"]["reaction_interval"] = toml::Value::String("1s".into());
	cfg["watchman"]["sweep_interval"] = toml::Value::String("1s".into());
	let path = case.ctx.datadir.join(format!("direct-watchmand-{max_miner_fee}.toml"));
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
	file.write_all(toml::to_string_pretty(&cfg).unwrap().as_bytes()).unwrap();
	file.sync_all().unwrap();
	path
}

fn spawn(case: &CrashCase, cfg: &PathBuf, name: &str) -> tokio::process::Child {
	let stdout = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(case.ctx.datadir.join(format!("{name}-stdout.log"))).unwrap();
	let stderr = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(case.ctx.datadir.join(format!("{name}-stderr.log"))).unwrap();
	Command::new(std::env::var_os("WATCHMAND_EXEC").unwrap())
		.arg("start").arg("--config").arg(cfg).stdout(stdout).stderr(stderr)
		.env_remove("RUST_MIN_STACK")
		.kill_on_drop(true).spawn().unwrap()
}

async fn carrier(case: &CrashCase, child: &mut tokio::process::Child) -> bitcoin::Transaction {
	let result = tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			assert!(child.try_wait().unwrap().is_none(), "watchmand exited before direct payout");
			let row = case.db.read(async |t| Ok(t.query_opt("SELECT c.signed_tx FROM expiry_carrier c
				JOIN expiry_carrier_allocation m ON m.txid=c.txid JOIN expiry_allocation a ON a.id=m.allocation_id
				WHERE a.freeze_set=$1 ORDER BY c.generation DESC LIMIT 1", &[&case.recipe.freeze_set]).await?)).await.unwrap();
			if let Some(row) = row {
				let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&row.get::<_, Vec<u8>>("signed_tx")).unwrap();
				if case.ctx.bitcoind().sync_client().get_raw_mempool().unwrap().contains(&tx.compute_txid()) { break tx; }
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await;
	if result.is_err() {
		child.kill().await.unwrap();
		child.wait().await.unwrap();
	}
	result.expect("watchmand failed to pay the already-frozen Board directly without a component worker")
}

#[tokio::test]
async fn watchmand_direct_frozen_board_is_paid_by_actual_daemon() {
	let case = crash_case("watchmand_direct_frozen_board_is_paid_by_actual_daemon", vec![]).await;
	let cfg = config(&case, 10_000);
	let mut child = spawn(&case, &cfg, "watchmand");
	confirm_and_recover(&case, &mut child).await;
}

async fn confirm_and_recover(case: &CrashCase, child: &mut tokio::process::Child) {
	let tx = carrier(case, child).await;
	assert!(tx.output.contains(&case.recipe.recipients[0]));
	let root: ServerVtxo = ServerVtxo::deserialize(&case.recipe.backing[0]).unwrap();
	assert!(tx.input.iter().any(|i| i.previous_output == root.point()));
	assert!(std::fs::metadata(case.recipe.config_path.parent().unwrap().join("direct-expiry-witness")
		.join(format!("{}.json", tx.compute_txid()))).unwrap().len() > 0);
	case.ctx.generate_blocks(1).await;
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let confirmed = case.db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n
				FROM expiry_carrier WHERE txid=$1 AND confirmed_height IS NOT NULL", &[&tx.compute_txid().to_string()]).await?
				.get::<_, i64>("n"))).await.unwrap();
			if confirmed == 1 { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	child.kill().await.unwrap();
	child.wait().await.unwrap();
	assert_eq!(case.db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n FROM expiry_carrier", &[]).await?
		.get::<_, i64>("n"))).await.unwrap(), 1);
	let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
	assert_eq!(outputs.iter().filter(|(p,_)| p.txid == tx.compute_txid()).map(|(_,a)| *a).collect::<Vec<_>>(), vec![sat(19_000)]);
}

async fn assert_backing_retained(case: &CrashCase) {
	assert_eq!(case.db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n FROM expiry_carrier", &[]).await?
		.get::<_, i64>("n"))).await.unwrap(), 0);
	let root: ServerVtxo = ServerVtxo::deserialize(&case.recipe.backing[0]).unwrap();
	assert!(case.ctx.bitcoind().sync_client().get_tx_out(&root.point().txid, root.point().vout, Some(true)).unwrap().is_some(),
		"refused or fee-deferred payout must not drain customer backing");
}

#[tokio::test]
async fn watchmand_direct_fee_ceiling_defers_then_resumes_exact_payout() {
	let case = crash_case("watchmand_direct_fee_ceiling_defers_then_resumes_exact_payout", vec![]).await;
	let cfg = config(&case, 100);
	let mut child = spawn(&case, &cfg, "fee-deferred");
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			assert!(child.try_wait().unwrap().is_none(), "watchmand exited instead of retaining fee-deferred work");
			let logs = std::fs::read_to_string(case.ctx.datadir.join("fee-deferred-stdout.log")).unwrap();
			if logs.contains("miner fee exceeds ceiling") { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual daemon must evidence its declared fee deferral");
	assert_backing_retained(&case).await;
	child.kill().await.unwrap();
	child.wait().await.unwrap();
	let cfg = config(&case, 10_000);
	let mut child = spawn(&case, &cfg, "fee-resumed");
	confirm_and_recover(&case, &mut child).await;
}

#[tokio::test]
async fn watchmand_direct_missing_history_or_disabled_route_refuses_before_workers() {
	let case = crash_case("watchmand_direct_missing_history_or_disabled_route_refuses_before_workers", vec![]).await;
	let cfg = config(&case, 10_000);
	let history = case.recipe.config_path.parent().unwrap().join("direct-expiry-witness/history");
	let retained = case.ctx.datadir.join("withheld-direct-history");
	std::fs::rename(&history, &retained).unwrap();
	let mut child = spawn(&case, &cfg, "history-missing");
	let exit = tokio::time::timeout(Duration::from_secs(15), child.wait()).await.unwrap().unwrap();
	assert!(!exit.success(), "watchmand must refuse incomplete independent payout history");
	assert!(std::fs::read_to_string(case.ctx.datadir.join("history-missing-stderr.log")).unwrap()
		.contains("watchmand direct payout recovery witness validation failed"));
	assert_backing_retained(&case).await;
	std::fs::rename(&retained, &history).unwrap();
	let original = case.ctx.datadir.join("server-watchmand/config.toml");
	let mut disabled: server::config::watchmand::Config =
		toml::from_str(&std::fs::read_to_string(original).unwrap()).unwrap();
	disabled.watchman.direct_expiry = None;
	let disabled_path = case.ctx.datadir.join("disabled-route.toml");
	write_automatic_config(&disabled_path, &toml::to_string_pretty(&disabled).unwrap());
	let mut child = spawn(&case, &disabled_path, "direct-disabled");
	let exit = tokio::time::timeout(Duration::from_secs(15), child.wait()).await.unwrap().unwrap();
	assert!(!exit.success(), "legacy route must not start over frozen direct obligations");
	assert!(std::fs::read_to_string(case.ctx.datadir.join("direct-disabled-stderr.log")).unwrap()
		.contains("source-frozen expiry state requires configured direct settlement"));
	assert_backing_retained(&case).await;
	let mut child = spawn(&case, &cfg, "history-restored");
	confirm_and_recover(&case, &mut child).await;
}
