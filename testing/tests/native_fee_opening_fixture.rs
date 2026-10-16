//! Test-only observation/bootstrap helper. No test is selected by this file.
//! Call only for a separately admitted, externally paused OWN REGTEST fixture.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
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
	watch.start().await?;
	let address = watch.config().admin_address.context("started owner admin endpoint missing")?;
	ensure!(address.ip().is_loopback() && address.port() != 0, "actual owner endpoint is not loopback");
	srv.config_mut().native_fee_owner = Some(NativeFeeOwnerConfig {
		address, original_opening_sha256: opening.original_opening_sha256,
	});
	srv.start().await?;
	Ok(())
}
