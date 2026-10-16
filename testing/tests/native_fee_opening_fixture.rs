//! One explicitly admitted OWN REGTEST opening/startup component control.
//! Source preparation/typecheck alone never authorizes executing this test.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, ensure};
use bitcoin::{BlockHash, FeeRate, Network, Transaction};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::rpc::RpcApi;
use serde::Serialize;
use server::database::Db;
use server::wallet::WalletKind;
use server::watchman::{NativeFeeOpeningConfig, NativeFeeOwnerConfig};
use server::watchman::ordinary_fee::OrdinaryFeePolicy;
use ark_testing::{TestContext, daemon::captaind::Captaind};

// EXACT C original-opening source contract; A's 37-table list is not reused.
const TABLES: &[&str] = &["wallet_changeset", "nursery_tx", "round", "round_participation",
	"round_part_input", "round_part_output", "vtxo_pool", "vtxo", "virtual_transaction",
	"pending_board", "offboards", "sweep", "fallback_record", "key_link",
	"expiry_allocation", "expiry_carrier", "expiry_carrier_allocation", "vtxo_pool_retirement",
	"htlc_vtxo", "htlc_settlement", "lightning_htlc_subscription", "lightning_invoice",
	"lightning_payment_attempt", "lightning_payment_attempt_htlc_vtxo"];
const MAX_FILE: u64 = 4 * 1024 * 1024;

// Field order mirrors the frozen native structs, because native decode checks
// exact serde bytes. These are observations, never an activation capability.
#[derive(Serialize)]
struct TableCut { rows: i64, bytes: i64, sha256: sha256::Hash }
#[derive(Serialize)]
struct WalletCut { descriptor_sha256: sha256::Hash, transaction_ids: BTreeSet<String> }
#[derive(Serialize)]
struct Publication {
	version: u8, path: &'static str, policy_sha256: sha256::Hash,
	raw: Vec<u8>, input_parents: Vec<Vec<u8>>, max_fee_rate: FeeRate,
}
#[derive(Serialize)]
struct Opening {
	version: u8, server_pubkey: PublicKey, genesis: BlockHash,
	policy_sha256: sha256::Hash, created_at_unix: u64,
	tip_height: u32, tip_hash: BlockHash, minimum_confirmations: u32,
	original_history: Vec<u8>, previously_charged_sat: u64, import_charge_sat: u64,
	remaining_ordinary_sat: u64, ordinary_total_sat: u64, urgent_floor_sat: u64,
	source_manifest_sha256: sha256::Hash, publication_logs_sha256: BTreeSet<sha256::Hash>,
	tables: BTreeMap<String, TableCut>, wallets: BTreeMap<String, WalletCut>,
	external_funding_ids: BTreeSet<String>, publications: Vec<Publication>,
}

fn read_original(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
	let before = fs::symlink_metadata(path)?;
	ensure!(before.is_file() && before.nlink() == 1 && before.len() <= limit,
		"original fixture file type, links or byte bound differ");
	let mut file = OpenOptions::new().read(true).custom_flags(nix::libc::O_NOFOLLOW).open(path)?;
	let opened = file.metadata()?;
	ensure!((before.dev(),before.ino(),before.len()) == (opened.dev(),opened.ino(),opened.len()),
		"original fixture file changed before read");
	let mut bytes = Vec::new(); Read::by_ref(&mut file).take(limit + 1).read_to_end(&mut bytes)?;
	let after = fs::symlink_metadata(path)?;
	ensure!(bytes.len() as u64 == before.len() && bytes.len() as u64 <= limit
		&& (before.dev(),before.ino(),before.len(),before.mtime(),before.mtime_nsec(),before.ctime(),before.ctime_nsec())
		== (after.dev(),after.ino(),after.len(),after.mtime(),after.mtime_nsec(),after.ctime(),after.ctime_nsec()),
		"original fixture file changed during bounded read");
	Ok(bytes)
}
fn private_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
	ensure!(bytes.len() as u64 <= MAX_FILE, "new fixture observation exceeds bound");
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.custom_flags(nix::libc::O_NOFOLLOW).open(path)?;
	file.write_all(bytes)?; file.sync_all()?; Ok(())
}
fn old_numeric_charges(raw: &[u8]) -> anyhow::Result<u64> {
	let text = std::str::from_utf8(raw)?;
	ensure!(text.is_empty() || text.ends_with('\n'), "original fee journal is torn");
	let mut charged = 0u64;
	for line in text.lines() {
		let value: u64 = line.parse().context("fixture opening requires original numeric carrier journal, not another epoch")?;
		ensure!(value > 0 && value.to_string() == line, "original numeric charge is noncanonical");
		charged = charged.checked_add(value).context("original charges overflow")?;
	}
	Ok(charged)
}

/// Pauses the actual test services, captures original facts and writes only NEW
/// private observation files. No financial rows or original files are changed.
/// Native activate_config still independently validates every field and source.
#[allow(dead_code)]
async fn capture_original_opening(ctx: &TestContext, srv: &Captaind)
	-> anyhow::Result<NativeFeeOpeningConfig> {
	let key = srv.ark_info().await.server_pubkey;
	let cap = srv.config().clone();
	let watch = srv.watchmand();
	let watch_cfg = watch.config().clone();
	ensure!(cap.network == Network::Regtest && watch_cfg.network == Network::Regtest,
		"fixture helper is OWN REGTEST only");
	ensure!(cap.bitcoind.url == ctx.bitcoind().rpc_url() && watch_cfg.bitcoind.url == ctx.bitcoind().rpc_url(),
		"native services do not use this exact owned fixture Core");
	let direct = watch_cfg.watchman.direct_expiry.as_ref().context("actual DIRECT Watchman missing")?;
	ensure!(cap.direct_expiry.enabled && direct.history_dir == cap.data_dir,
		"fixture must share its original owning history");
	let history = cap.data_dir.join("direct-expiry-witness");
	let policy_bytes = read_original(&history.join("ordinary-fee-policy.json"), 4096)?;
	let policy: OrdinaryFeePolicy = serde_json::from_slice(&policy_bytes)?;
	ensure!(serde_json::to_vec(&policy)? == policy_bytes && policy == direct.ordinary_fee_policy,
		"fixture cannot change its original stored fee policy");
	ensure!(cap.direct_expiry.regtest_qualification_fee_policy.map_or(policy.regtest_qualification.is_none(), |p| p == policy),
		"cap and owning policy differ");
	let rate = watch_cfg.fee_estimator.max_fee_rate.context("actual owner rate ceiling missing")?;
	ensure!(watch_cfg.min_trusted_confs > 0, "owning confirmation policy missing");
	srv.stop().await?; watch.stop().await?;
	let original_history = read_original(&history.join("ordinary-fee-budget"), 2 * 1024 * 1024)?;
	let charged = old_numeric_charges(&original_history)?;
	let db = Db::connect(&cap.postgres).await?;
	let core = ctx.bitcoind().sync_client();
	let tip = core.get_block_count()?;
	let genesis = core.get_block_hash(0)?;
	ensure!(genesis == bitcoin::constants::genesis_block(Network::Regtest).block_hash(), "actual Core genesis differs");
	let tip_hash = core.get_block_hash(tip)?;
	let (tables, wallets, nursery) = db.read(async |t| {
		t.batch_execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;
			SET LOCAL statement_timeout='10s'; SET LOCAL lock_timeout='5s';
			SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO,YMD';
			SET LOCAL bytea_output='hex'; SET LOCAL extra_float_digits=3;").await?;
		let mut tables = BTreeMap::new(); let mut aggregate = 0i64;
		for name in TABLES {
			let stats = t.query_one(&format!("SELECT count(*),COALESCE(sum(octet_length(row_to_json(x)::text)),0)::bigint,
				COALESCE(max(octet_length(row_to_json(x)::text)),0)::bigint FROM {name} x"), &[]).await?;
			let rows: i64 = stats.get(0); let bytes: i64 = stats.get(1);
			aggregate = aggregate.checked_add(bytes).context("source projection overflow")?;
			ensure!(rows <= 10_000 && bytes <= 8 * 1024 * 1024 && stats.get::<_, i64>(2) <= 2 * 1024 * 1024
				&& aggregate <= 16 * 1024 * 1024, "original source projection exceeds native bounds");
			let values = t.query(&format!("SELECT row_to_json(x)::text FROM {name} x ORDER BY (row_to_json(x)::text) COLLATE \"C\""), &[]).await?;
			ensure!(values.len() as i64 == rows, "source row count differs");
			let mut framed = Vec::new(); let mut actual_bytes = 0i64;
			for row in values { let body: String = row.get(0); actual_bytes += body.len() as i64;
				framed.extend_from_slice(&(body.len() as u64).to_le_bytes()); framed.extend_from_slice(body.as_bytes()); }
			ensure!(actual_bytes == bytes, "source byte count differs");
			tables.insert((*name).to_owned(), TableCut { rows, bytes, sha256: sha256::Hash::hash(&framed) });
		}
		let mut wallets = BTreeMap::new();
		for kind in [WalletKind::Rounds, WalletKind::Watchman] {
			let state = t.read_aggregate_changeset(kind).await?.context("actual persisted owning graph missing")?;
			ensure!(state.network == Some(Network::Regtest) && state.tx_graph.txs.len() <= 4096
				&& state.tx_graph.txouts.is_empty() && state.locked_outpoints.outpoints.values().all(|locked| !locked),
				"actual owning graph is incomplete, locked, wrong chain or oversized");
			let descriptor = state.descriptor.context("actual persisted owning descriptor missing")?;
			wallets.insert(kind.name().to_owned(), WalletCut {
				descriptor_sha256: sha256::Hash::hash(descriptor.to_string().as_bytes()),
				transaction_ids: state.tx_graph.txs.iter().map(|tx| tx.compute_txid().to_string()).collect(),
			});
		}
		let rows = t.query("SELECT txid,kind::TEXT,tx FROM nursery_tx ORDER BY txid LIMIT 4097", &[]).await?;
		ensure!(rows.len() <= 4096, "original nursery exceeds native count bound");
		let mut nursery = Vec::new(); let mut total = 0usize;
		for row in rows { let raw: Vec<u8> = row.get(2); total += raw.len();
			ensure!(raw.len() <= MAX_FILE as usize && total <= 16 * 1024 * 1024, "original nursery byte bound exceeded");
			nursery.push((row.get::<_, String>(0), row.get::<_, String>(1), raw)); }
		Ok((tables, wallets, nursery))
	}).await?;
	let mut publications = Vec::new(); let mut published_ids = BTreeSet::new(); let mut import = 0u64;
	let policy_hash = sha256::Hash::hash(&policy_bytes); let mut total_parent_bytes = 0usize;
	for (id, kind, raw) in nursery {
		let path = match kind.as_str() { "round" => "round", "vtxopool" => "pool", _ => anyhow::bail!("unsupported original nursery purpose") };
		let tx: Transaction = deserialize(&raw)?;
		ensure!(serialize(&tx) == raw && tx.compute_txid().to_string() == id && published_ids.insert(id), "original nursery body differs");
		let mut input_parents = Vec::new(); let mut value = 0u64;
		for id in tx.input.iter().map(|i| i.previous_output.txid).collect::<BTreeSet<_>>() {
			let parent = core.get_raw_transaction(&id, None)?; let bytes = serialize(&parent);
			total_parent_bytes = total_parent_bytes.checked_add(bytes.len()).context("parent bytes overflow")?;
			ensure!(bytes.len() <= MAX_FILE as usize && total_parent_bytes <= 16 * 1024 * 1024, "original parent byte bound exceeded");
			for input in tx.input.iter().filter(|input| input.previous_output.txid == id) {
				value = value.checked_add(parent.output.get(input.previous_output.vout as usize).context("original prevout missing")?.value.to_sat()).context("prevout sum overflow")?;
			}
			input_parents.push(bytes);
		}
		let outputs = tx.output.iter().try_fold(0u64, |sum, o| sum.checked_add(o.value.to_sat()).context("output sum overflow"))?;
		let fee = value.checked_sub(outputs).context("original fee negative")?; ensure!(fee > 0, "original fee must be positive");
		let debit = policy.regtest_qualification.map_or(fee, |p| p.per_attempt_ceiling_sat);
		ensure!(fee <= debit, "original fee exceeds original declared ceiling");
		import = import.checked_add(debit).context("original import overflow")?;
		publications.push(Publication { version: 1, path, policy_sha256: policy_hash, raw, input_parents, max_fee_rate: rate });
	}
	let external_funding_ids = wallets.values().flat_map(|w| w.transaction_ids.iter().cloned())
		.filter(|id| !published_ids.contains(id)).collect();
	let root = ctx.datadir.join("native-original-opening-observation"); fs::DirBuilder::new().mode(0o700).create(&root)?;
	let mut publication_logs_sha256 = BTreeSet::new(); let mut log_paths = Vec::new();
	for (name, dir) in [("cap.log", &cap.data_dir), ("watch.log", &watch_cfg.data_dir)] {
		let bytes = read_original(&dir.join(ark_testing::daemon::STDOUT_LOGFILE), MAX_FILE)?;
		let path = root.join(name); private_new(&path, &bytes)?;
		ensure!(publication_logs_sha256.insert(sha256::Hash::hash(&bytes)), "original publication logs duplicate"); log_paths.push(path);
	}
	let source = serde_json::to_vec(&(ctx.datadir.clone(), key, genesis, &tables, &wallets, &published_ids))?;
	let source_path = root.join("source.json"); private_new(&source_path, &source)?;
	ensure!(core.get_block_count()? == tip && core.get_block_hash(tip)? == tip_hash, "actual Core crossed original source cut");
	ensure!(read_original(&history.join("ordinary-fee-budget"), 2 * 1024 * 1024)? == original_history, "original journal changed while paused");
	let remaining = policy.ordinary_budget_sat.checked_sub(charged).and_then(|v| v.checked_sub(import)).context("original debits exceed unchanged ordinary allowance")?;
	let opening = Opening { version: 1, server_pubkey: key, genesis, policy_sha256: policy_hash,
		created_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(), tip_height: u32::try_from(tip)?, tip_hash,
		minimum_confirmations: watch_cfg.min_trusted_confs, original_history, previously_charged_sat: charged,
		import_charge_sat: import, remaining_ordinary_sat: remaining, ordinary_total_sat: policy.ordinary_budget_sat,
		urgent_floor_sat: policy.urgent_reserve_sat, source_manifest_sha256: sha256::Hash::hash(&source),
		publication_logs_sha256, tables, wallets, external_funding_ids, publications };
	let raw = serde_json::to_vec(&opening)?; let path = root.join("opening.json"); private_new(&path, &raw)?;
	fs::File::open(&root)?.sync_all()?;
	Ok(NativeFeeOpeningConfig { original_opening: path, original_opening_sha256: sha256::Hash::hash(&raw),
		source_manifest: source_path, original_publication_logs: log_paths })
}

/// Both services are already stopped by capture_original_opening. Watchman
/// activation must succeed before cap receives its ACTUAL newly reserved port.
/// This has no financial case or initializer; runtime admission is separate.
#[allow(dead_code)]
async fn restart_with_actual_native_fee_owner(srv: &Captaind, opening: NativeFeeOpeningConfig)
	-> anyhow::Result<()> {
	let watch = srv.watchmand();
	watch.config_mut().native_fee_opening = Some(opening.clone());
	watch.try_start().await?;
	let address = watch.config().admin_address.context("started owner admin endpoint missing")?;
	ensure!(address.ip().is_loopback() && address.port() != 0, "actual owner endpoint is not loopback");
	srv.config_mut().native_fee_owner = Some(NativeFeeOwnerConfig {
		address, original_opening_sha256: opening.original_opening_sha256,
	});
	srv.try_start().await?;
	Ok(())
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ComponentDeclaration {
	version: u8, purpose: String, network: Network, genesis: BlockHash,
	max_attempts: u32, per_attempt_ceiling_sat: u64, ordinary_budget_sat: u64,
	urgent_reserve_sat: u64, workload_sha256: sha256::Hash,
}

async fn current_original_tables(db: &Db) -> anyhow::Result<serde_json::Value> {
	db.read(async |t| {
		t.batch_execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;
			SET LOCAL statement_timeout='10s'; SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO,YMD';
			SET LOCAL bytea_output='hex'; SET LOCAL extra_float_digits=3;").await?;
		let mut result = BTreeMap::new(); let mut total = 0i64;
		for table in TABLES {
			let stats = t.query_one(&format!("SELECT count(*),COALESCE(sum(octet_length(row_to_json(x)::text)),0)::bigint,
				COALESCE(max(octet_length(row_to_json(x)::text)),0)::bigint FROM {table} x"), &[]).await?;
			let rows: i64 = stats.get(0); let bytes: i64 = stats.get(1); total += bytes;
			ensure!(rows <= 10_000 && bytes <= 8 * 1024 * 1024 && stats.get::<_, i64>(2) <= 2 * 1024 * 1024
				&& total <= 16 * 1024 * 1024, "component table projection exceeds original native bound");
			let values = t.query(&format!("SELECT row_to_json(x)::text FROM {table} x ORDER BY (row_to_json(x)::text) COLLATE \"C\""), &[]).await?;
			ensure!(values.len() as i64 == rows, "component projection is incomplete");
			let mut framed = Vec::new(); let mut count = 0i64;
			for row in values { let value: String = row.get(0); count += value.len() as i64;
				framed.extend_from_slice(&(value.len() as u64).to_le_bytes()); framed.extend_from_slice(value.as_bytes()); }
			ensure!(count == bytes, "component source byte count differs");
			result.insert((*table).to_owned(), TableCut { rows, bytes, sha256: sha256::Hash::hash(&framed) });
		}
		Ok(serde_json::to_value(result)?)
	}).await
}

#[tokio::test]
async fn genuine_stopped_original_opening_activates_actual_owner_before_cap() {
	use std::time::Duration;
	use ark_testing::sat;
	use ark_testing::daemon::captaind::WalletAdminClient;
	use server_rpc::{admin::NativeFeeServiceClient, protos};
	use server::watchman::ordinary_fee::RegtestQualificationBudget;
	let inputs = PathBuf::from(std::env::var("C_NATIVE_OPENING_COMPONENT_INPUT_ROOT").unwrap());
	assert_eq!(inputs.canonicalize().unwrap(), inputs);
	let raw_workload = read_original(&inputs.join("workload.json"), 4096).unwrap();
	let workload: serde_json::Value = serde_json::from_slice(&raw_workload).unwrap();
	assert_eq!(workload["purpose"], "own-regtest-native-opening-startup-component");
	assert_eq!(workload["nominal_attempts"], 3); assert_eq!(workload["per_attempt_ceiling_sat"], 20_000);
	assert_eq!(workload["nominal_ordinary_budget_sat"], 60_000); assert_eq!(workload["physical_floor_sat"], 40_000);
	assert_eq!(workload["native_publications"], 0); assert_eq!(workload["customers"], 0);
	assert_eq!(workload["live_58k_qualification"], false);
	let declaration_bytes = read_original(&inputs.join("declaration.json"), 4096).unwrap();
	let declaration: ComponentDeclaration = serde_json::from_slice(&declaration_bytes).unwrap();
	assert!(serde_json::to_vec(&declaration).unwrap() == declaration_bytes, "native declaration must be canonical");
	assert_eq!(declaration.version, 1); assert_eq!(declaration.purpose, "common-regtest-scale-qualification");
	assert_eq!(declaration.network, Network::Regtest);
	assert_eq!(declaration.genesis, bitcoin::constants::genesis_block(Network::Regtest).block_hash());
	assert_eq!(declaration.max_attempts, 3); assert_eq!(declaration.per_attempt_ceiling_sat, 20_000);
	assert_eq!(declaration.ordinary_budget_sat, 60_000); assert_eq!(declaration.urgent_reserve_sat, 40_000);
	assert_eq!(declaration.workload_sha256, sha256::Hash::hash(&raw_workload));
	let policy = OrdinaryFeePolicy { ordinary_budget_sat: 60_000, urgent_reserve_sat: 40_000,
		regtest_qualification: Some(RegtestQualificationBudget { max_attempts: 3, per_attempt_ceiling_sat: 20_000,
			declaration_sha256: sha256::Hash::hash(&declaration_bytes) }) };
	let name = "native_fee_opening_fixture/genuine_stopped_original_opening_activates_actual_owner_before_cap";
	assert!(!ark_testing::util::test_data_directory().await.join(name).exists(), "component namespace already exists; no retry");
	let ctx = TestContext::new(name).await;
	let history_dir = ctx.datadir.join("server"); let owning_history = history_dir.clone();
	let rate = FeeRate::from_sat_per_vb_u32(25);
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.regtest_fee_declaration(declaration_bytes).cfg(move |c| {
			c.direct_expiry.enabled = true; c.direct_expiry.interval = Duration::from_secs(3600);
			c.direct_expiry.regtest_qualification_fee_policy = Some(policy);
			c.expiry_payout.enabled = false; c.round_interval = Duration::from_secs(3600);
			c.fee_estimator.max_fee_rate = Some(rate);
		}).watchmand_cfg(move |c| {
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig {
				ordinary_fee_policy: policy, history_dir: owning_history, max_miner_fee: sat(20_000),
			});
			c.watchman.reaction_interval = Duration::from_secs(3600); c.watchman.sweep_interval = Duration::from_secs(3600);
			c.min_trusted_confs = 1; c.fee_estimator.max_fee_rate = Some(rate);
		}).create().await;
	assert!(srv.config().vtxopool.vtxo_targets.is_empty());
	assert!(srv.config().cln_array.is_empty());
	let watch = srv.watchmand();
	// ONLY the explicit original physical operator floor: no customer or native publication.
	let address = watch.wait_wallet_address().await;
	let funding = ctx.bitcoind().fund_addr(&address, sat(40_000)).await;
	ctx.generate_blocks(1).await;
	let tx = ctx.bitcoind().sync_client().get_raw_transaction(&funding, None).unwrap();
	let vout = tx.output.iter().position(|o| o.value == sat(40_000) && o.script_pubkey == address.script_pubkey()).unwrap();
	let point = bitcoin::OutPoint::new(funding, u32::try_from(vout).unwrap());
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let endpoint = watch.config().admin_address.unwrap();
			let mut client = WalletAdminClient::connect(format!("http://{endpoint}")).await.unwrap();
			let inventory = client.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap();
			if inventory.complete && inventory.utxos.iter().any(|u| u.outpoint == point.to_string()
				&& u.value_sat == 40_000 && u.confirmed_fee_eligible && !u.reserved) { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	let opening = capture_original_opening(&ctx, &srv).await.unwrap();
	let raw = read_original(&opening.original_opening, MAX_FILE).unwrap();
	let observed: serde_json::Value = serde_json::from_slice(&raw).unwrap();
	assert_eq!(observed["previously_charged_sat"], 0); assert_eq!(observed["import_charge_sat"], 0);
	assert!(observed["publications"].as_array().unwrap().is_empty());
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert!(current_original_tables(&db).await.unwrap() == observed["tables"], "complete paused original source differs");
	let history = history_dir.join("direct-expiry-witness");
	let mut original_files = vec![opening.original_opening.clone(), opening.source_manifest.clone(),
		history.join("ordinary-fee-budget"), history.join("ordinary-fee-policy.json")];
	original_files.extend(opening.original_publication_logs.iter().cloned());
	let originals = original_files.iter().map(|p| (p.clone(), sha256::Hash::hash(&read_original(p, MAX_FILE).unwrap()))).collect::<Vec<_>>();
	// A missing required binding and a changed actual source each have an exact
	// native error. Generic ENOENT/port/timeouts never count as source refusal.
	let refusal_root = ctx.datadir.join("native-source-refusals"); fs::DirBuilder::new().mode(0o700).create(&refusal_root).unwrap();
	for changed in [false, true] {
		let mut bad = opening.clone();
		let (label, marker) = if changed {
			bad.source_manifest = opening.source_manifest.with_file_name("changed-source.json");
			private_new(&bad.source_manifest, b"different observed source, no authority").unwrap();
			("changed-source", "original paused source manifest differs")
		} else {
			let mut missing: serde_json::Value = serde_json::from_slice(&raw).unwrap();
			assert!(missing.as_object_mut().unwrap().remove("source_manifest_sha256").is_some());
			let bytes = serde_json::to_vec(&missing).unwrap();
			bad.original_opening = opening.original_opening.with_file_name("missing-source-binding.json");
			private_new(&bad.original_opening, &bytes).unwrap();
			bad.original_opening_sha256 = sha256::Hash::hash(&bytes);
			("missing-source-binding", "missing field `source_manifest_sha256`")
		};
		watch.config_mut().native_fee_opening = Some(bad);
		require_exact_source_startup_refusal(watch, &refusal_root, label, marker).await.unwrap();
		assert!(current_original_tables(&db).await.unwrap() == observed["tables"], "source refusal mutated original rows");
		let count: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM native_fee_opening", &[]).await?.get(0))).await.unwrap();
		assert_eq!(count, 0);
	}
	// Source-observation originals remain immutable through all refusal paths.
	for (p, digest) in &originals { assert_eq!(sha256::Hash::hash(&read_original(p, MAX_FILE).unwrap()), *digest); }
	restart_with_actual_native_fee_owner(&srv, opening.clone()).await.unwrap();
	let endpoint = watch.config().admin_address.unwrap();
	assert_eq!(srv.config().native_fee_owner.as_ref().unwrap().address, endpoint);
	let mut native = NativeFeeServiceClient::connect(format!("http://{endpoint}")).await.unwrap();
	let refusal = native.verify(protos::NativeFeeRequest { authenticated_request: vec![] }).await.unwrap_err();
	assert_eq!(refusal.code(), tonic::Code::FailedPrecondition);
	assert!(refusal.message().contains("EOF while parsing a value") && !refusal.message().contains("epoch is not activated"),
		"actual activated native broker must reach its authentication parser, not a missing epoch or unrelated failure");
	let row = db.read(async |t| t.query_one("SELECT opening_hash,opening_bytes FROM native_fee_opening WHERE id=1", &[]).await.map_err(Into::into)).await.unwrap();
	assert_eq!(row.get::<_, String>(0), opening.original_opening_sha256.to_string());
	assert!(row.get::<_, Vec<u8>>(1) == raw, "native activated bytes differ from sealed original");
	for table in ["native_fee_publication", "native_fee_input", "round", "vtxo_pool", "vtxo", "key_link", "fallback_record", "expiry_allocation"] {
		let count: i64 = db.read(async |t| Ok(t.query_one(&format!("SELECT count(*) FROM {table}"), &[]).await?.get(0))).await.unwrap();
		assert_eq!(count, 0, "zero-obligation component must not publish or issue rights");
	}
	assert!(current_original_tables(&db).await.unwrap() == observed["tables"], "activation changed original customer/native source cut");
	for (p, digest) in &originals { assert_eq!(sha256::Hash::hash(&read_original(p, MAX_FILE).unwrap()), *digest); }
	assert!(read_original(&history.join("ordinary-fee-budget"), MAX_FILE).unwrap().is_empty());
	let live = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	assert_eq!(live.value, sat(40_000));
	srv.stop().await.unwrap(); watch.stop().await.unwrap();
	private_new(&ctx.datadir.join("opening-component-result.json"), &serde_json::to_vec(&serde_json::json!({
		"status":"PASS", "scope":"OWN_REGTEST_STARTUP_COMPONENT_ONLY", "source_originals_unchanged":true,
		"native_opening_sha256":opening.original_opening_sha256, "original_24_table_cut_unchanged":true,
		"ordinary_native_debits_sat":0,"synthetic_nominal_budget_sat":60000,"physical_unspent_sat":40000,
		"live_58k_qualification":false,"financial_readiness":false,"native_publications":0,
		"missing_source_binding_refused":true,"changed_source_refused":true,"watchman_endpoint":endpoint,
	})).unwrap()).unwrap();
}

async fn require_exact_source_startup_refusal(
	watch: &ark_testing::daemon::watchmand::Watchmand, output: &Path,
	label: &str, expected_native_error: &str,
) -> anyhow::Result<()> {
	use std::process::Stdio;
	let cfg = watch.config().clone();
	cfg.write_into(&mut fs::File::create(cfg.data_dir.join(ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE))?)?;
	let stdout = output.join(format!("{label}.stdout.private.log"));
	let stderr = output.join(format!("{label}.stderr.private.log"));
	let out = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&stdout)?;
	let err = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&stderr)?;
	let mut command = watch.get_custom_command(&["start"]).await?;
	command.kill_on_drop(true).stdout(Stdio::from(out)).stderr(Stdio::from(err));
	let mut child = command.spawn()?;
	let pid = child.id().context("owned negative process has no PID")?;
	let status = match tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await {
		Ok(status) => status?,
		Err(_) => { child.kill().await?; child.wait().await?;
			anyhow::bail!("source refusal timed out; actual raw output retained, not a native refusal PASS"); }
	};
	ensure!(status.code() == Some(1), "source control requires native exit1, not signal/port/timeout success");
	let actual_error = String::from_utf8(read_original(&stderr, MAX_FILE)?)?;
	ensure!(actual_error.contains(expected_native_error), "actual native failure is not the required source refusal; raw retained privately");
	let receipt = serde_json::to_vec(&serde_json::json!({"pid":pid,"wait_reaped":true,"exit_code":status.code(),
		"expected_native_error":expected_native_error,"stderr_sha256":sha256::Hash::hash(actual_error.as_bytes())}))?;
	private_new(&output.join(format!("{label}.process.json")), &receipt)?;
	Ok(())
}
