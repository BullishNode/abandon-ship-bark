//! Explicit, externally SHA-pinned import of original confirmed sweep fees.
//! This first opening kind refuses other operator histories. The certificate
//! binds a paused source cut; it never recreates missing customer obligations.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use bitcoin::Transaction;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoind_async_client::traits::Reader;

use crate::database::{Db, Tx};
use crate::fee_allowance::{Candidate, Policy};
use crate::fee_allowance_history::FeeHistory;
use crate::operator_residual::{read_bounded, write_identical};
use crate::wallet::WalletKind;

const MAX_BYTES: usize = 4 * 1024 * 1024;
// Every frontier table is locked before its first read. Missing fork tables
// refuse import; an absent table is never interpreted as an empty liability.
const TABLES: &[&str] = &[
	"wallet_changeset", "captaind_block", "watchmand_block", "checkpoint_state", "vtxo",
	"virtual_transaction", "virtual_transaction_history",
	"vtxo_history", "pending_board", "fallback_record", "key_link", "arkoor_mailbox",
	"mailbox", "ephemeral_tweak", "round", "round_participation", "round_part_input",
	"round_part_output", "vtxo_pool", "offboards", "nursery_tx", "sweep",
	"expiry_settlement", "expiry_cancelled_participation", "htlc_vtxo", "htlc_settlement",
	"lightning_htlc_subscription", "lightning_invoice", "lightning_payment_attempt",
	"lightning_payment_attempt_htlc_vtxo", "lightning_invoice_history", "lightning_payment_attempt_history",
	"lightning_htlc_subscription_history", "recovery_reserve", "recovery_receipt",
	"recovery_source_hold", "recovery_capital_release",
];
const UNSUPPORTED: &[&str] = &[
	"round", "round_participation", "round_part_input", "round_part_output", "vtxo_pool",
	"offboards", "nursery_tx", "sweep", "expiry_settlement", "expiry_cancelled_participation",
	"htlc_vtxo", "htlc_settlement", "lightning_htlc_subscription", "lightning_invoice",
	"lightning_payment_attempt", "lightning_payment_attempt_htlc_vtxo", "lightning_invoice_history",
	"lightning_payment_attempt_history", "lightning_htlc_subscription_history", "recovery_capital_release",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
	pub file: PathBuf,
	pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileCut { path: PathBuf, bytes: u64, sha256: String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TableCut { rows: i64, bytes: i64, sha256: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Certificate {
	version: u32,
	policy_sha256: String,
	epoch_id: String,
	genesis: String,
	server_pubkey: String,
	created_at_unix: u64,
	tip_height: u32,
	tip_hash: String,
	minimum_confirmations: u32,
	// TOTAL allowances, never an already-net remainder masquerading as TOTAL.
	ordinary_total_sat: u64,
	urgent_total_sat: u64,
	previously_booked_ordinary_sat: u64,
	previously_booked_urgent_sat: u64,
	import_ordinary_sat: u64,
	remaining_ordinary_sat: u64,
	remaining_urgent_sat: u64,
	// SHA of the independently reviewed pause/runtime/bootstrap/source manifest.
	// Native verifies its exact bytes, not a caller-supplied "paused" Boolean.
	source_manifest: FileCut,
	publication_logs: Vec<FileCut>,
	tables: BTreeMap<String, TableCut>,
	wallet_descriptors: BTreeMap<String, String>,
	wallet_transactions: BTreeMap<String, BTreeSet<String>>,
	external_funding_transactions: BTreeSet<String>,
	candidates: Vec<Candidate>,
}
fn digest(raw: &[u8]) -> String { sha256::Hash::hash(raw).to_string() }
fn hash_file(c: &FileCut, limit: usize) -> anyhow::Result<()> {
	ensure!(c.path.is_absolute() && c.bytes <= limit as u64, "opening source file exceeds bound");
	let raw = read_bounded(&c.path, limit)?;
	ensure!(raw.len() as u64 == c.bytes && digest(&raw) == c.sha256, "opening source file differs");
	Ok(())
}

// Called while the common fee lock is held, before any workers or wallet replay.
// SHARE table locks also exclude writes by a still-running legacy binary that
// cannot yet participate in that advisory lock. Host pause remains mandatory.
async fn snapshot(t: &Tx<'_>) -> anyhow::Result<BTreeMap<String, TableCut>> {
	t.batch_execute("SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO,YMD'; SET LOCAL bytea_output='hex'; SET LOCAL extra_float_digits=3;").await?;
	t.execute(&format!("LOCK TABLE {} IN SHARE MODE", TABLES.join(",")), &[]).await?;
	let mut result = BTreeMap::new();
	let mut total = 0i64;
	for table in TABLES {
		let stats = t.query_one(&format!("SELECT count(*),COALESCE(sum(octet_length(row_to_json(x)::text)),0)::bigint,COALESCE(max(octet_length(row_to_json(x)::text)),0)::bigint FROM {table} x"), &[]).await?;
		let rows: i64 = stats.get(0);
		let bytes: i64 = stats.get(1);
		total = total.checked_add(bytes).context("opening frontier byte overflow")?;
		ensure!(rows <= 10_000 && bytes <= 8*1024*1024 && stats.get::<_,i64>(2) <= 2*1024*1024
			&& total <= 16*1024*1024, "opening frontier exceeds bounded admission");
		ensure!(!UNSUPPORTED.contains(table) || rows == 0, "unsupported previous operator publication or conditional history");
		// Bytewise deterministic ordering; length framing preserves row boundaries.
		let raw = t.query(&format!("SELECT row_to_json(x)::text AS body FROM {table} x ORDER BY (row_to_json(x)::text) COLLATE \"C\""), &[]).await?;
		let mut framed = Vec::new();
		for row in raw {
			let body: String = row.get(0);
			framed.extend_from_slice(&(body.len() as u64).to_le_bytes());
			framed.extend_from_slice(body.as_bytes());
		}
		result.insert((*table).to_owned(), TableCut { rows, bytes, sha256: digest(&framed) });
	}
	Ok(result)
}

async fn confirmed(
	core: &bitcoind_async_client::Client, raw: &[u8], minimum: u32,
) -> anyhow::Result<Transaction> {
	let tx: Transaction = deserialize(raw)?;
	ensure!(serialize(&tx) == raw, "opening transaction is noncanonical");
	let actual = crate::bitcoind::custom_get_raw_transaction_info(core, tx.compute_txid(), None).await?
		.context("opening original transaction unavailable from Core")?;
	ensure!(actual.hex == raw && actual.confirmations.unwrap_or(0) >= minimum,
		"opening transaction is not the exact sufficiently confirmed original");
	let block = actual.blockhash.context("opening transaction has no active block")?;
	let height = core.get_block_height(&block).await?;
	ensure!(core.get_block_hash(height).await? == block, "opening transaction block is not active");
	Ok(tx)
}

async fn validate_source(
	t: &Tx<'_>, p: &Policy, c: &Certificate, core: &bitcoind_async_client::Client,
) -> anyhow::Result<()> {
	ensure!(snapshot(t).await? == c.tables, "opening customer/operator frontier changed; do not reconstruct or clear it");
	ensure!(core.get_block_hash(u64::from(c.tip_height)).await?.to_string() == c.tip_hash,
		"opening source anchor is no longer active");
	let receipts = t.query("SELECT id,transaction_bytes,block_height,block_hash,proof_state FROM recovery_receipt ORDER BY id", &[]).await?;
	ensure!(receipts.len() == c.candidates.len() && !receipts.is_empty() && receipts.len() <= 128,
		"opening confirmed receipt coverage differs");
	let mut candidates = BTreeMap::new();
	for candidate in &c.candidates {
		ensure!(candidate.path == crate::fee_allowance::Path::Sweep, "opening supports only original nonurgent sweeps");
		let (id, _) = candidate.checked(p)?;
		let tx = confirmed(core, &candidate.raw, c.minimum_confirmations).await?;
		for raw in &candidate.input_parents { confirmed(core, raw, c.minimum_confirmations).await?; }
		ensure!(candidates.insert(tx.compute_txid().to_string(), (id, candidate)).is_none(), "duplicate opening sweep");
	}
	for row in receipts {
		let id: String = row.get(0);
		let (_, candidate) = candidates.get(&id).context("unaccounted original sweep receipt")?;
		ensure!(row.get::<_,Vec<u8>>(1) == candidate.raw && row.get::<_,String>(4) == "physical_bound",
			"opening receipt lacks original physically bound bytes");
		let height = u64::try_from(row.get::<_,i64>(2))?;
		ensure!(core.get_block_hash(height).await?.to_string() == row.get::<_,String>(3), "receipt block no longer active");
	}
	ensure!(c.wallet_descriptors.len() == 2 && c.wallet_transactions.len() == 2, "opening owning wallet coverage missing");
	let mut graph_ids = BTreeSet::new();
	let mut graph_transactions = BTreeMap::new();
	let mut native_outputs = BTreeSet::new();
	for kind in [WalletKind::Rounds, WalletKind::Watchman] {
		let wallet = t.read_aggregate_changeset(kind).await?.context("opening native wallet graph missing")?;
		ensure!(wallet.tx_graph.txouts.is_empty() && wallet.locked_outpoints.outpoints.values().all(|locked| !locked),
			"opening has partial native transaction outputs or retained input locks");
		let network = wallet.network.context("opening native wallet network missing")?;
		ensure!(bitcoin::constants::genesis_block(network).block_hash().to_string() == p.genesis, "opening wallet network differs");
		let view = bdk_wallet::Wallet::load().check_network(network).load_wallet_no_persist(wallet.clone())?
			.context("opening public wallet graph cannot be reconstructed")?;
		let descriptor = wallet.descriptor.as_ref().context("opening native public descriptor missing")?.to_string();
		ensure!(c.wallet_descriptors.get(kind.name()) == Some(&digest(descriptor.as_bytes())), "opening descriptor identity differs");
		ensure!(wallet.tx_graph.txs.len() <= 4096, "opening wallet transaction count exceeds bound");
		let mut ids = BTreeSet::new();
		let mut size = 0usize;
		for tx in &wallet.tx_graph.txs {
			let raw = serialize(tx.as_ref());
			size = size.checked_add(raw.len()).context("opening wallet byte sum overflow")?;
			ensure!(size <= MAX_BYTES, "opening wallet graph exceeds byte bound");
			confirmed(core, &raw, c.minimum_confirmations).await?;
			ids.insert(tx.compute_txid().to_string());
			graph_transactions.insert(tx.compute_txid().to_string(), tx.clone());
			for (vout, output) in tx.output.iter().enumerate() {
				if view.is_mine(output.script_pubkey.clone()) {
					native_outputs.insert(bitcoin::OutPoint { txid:tx.compute_txid(), vout:u32::try_from(vout)? });
				}
			}
		}
		graph_ids.extend(ids.iter().cloned());
		ensure!(c.wallet_transactions.get(kind.name()) == Some(&ids), "opening native graph differs from declared originals");
	}
	ensure!(candidates.keys().all(|id| graph_ids.contains(id)), "opening sweep absent from owning graph");
	ensure!(c.external_funding_transactions.is_disjoint(&candidates.keys().cloned().collect()), "opening external funding overlaps fee publications");
	let expected = c.external_funding_transactions.iter().cloned().chain(candidates.keys().cloned()).collect::<BTreeSet<_>>();
	ensure!(expected == graph_ids, "opening native transaction classification is incomplete");
	for id in &c.external_funding_transactions {
		let tx = graph_transactions.get(id).context("declared external funding absent from graph")?;
		ensure!(tx.input.iter().all(|i| !native_outputs.contains(&i.previous_output)),
			"external funding classification spends a native owned output");
	}
	// External origin ownership is bound to the independently reviewed funding
	// receipts in source_manifest. It is not fee credit or a new injection.

	Ok(())
}

impl Certificate {
	fn accounting(&self, p: &Policy, policy_sha: &str) -> anyhow::Result<()> {
		ensure!(self.version == 1 && self.policy_sha256 == policy_sha && self.epoch_id == p.epoch_id
			&& self.genesis == p.genesis && self.server_pubkey == p.server_pubkey, "opening immutable identity differs");
		ensure!(self.ordinary_total_sat == p.ordinary_sat && self.urgent_total_sat == p.urgent_sat,
			"opening TOTAL allowance differs from original policy");
		// This initial import supports a previously unenforced server only. A
		// future import of an existing booked epoch needs its original ledger.
		ensure!(self.previously_booked_ordinary_sat == 0 && self.previously_booked_urgent_sat == 0,
			"opening already-booked budget requires original ledger reconciliation");
		let imported = self.candidates.iter().try_fold(0u64, |sum, candidate| -> anyhow::Result<u64> {
			Ok(sum.checked_add(candidate.checked(p)?.1).context("opening fee sum overflow")?)
		})?;
		ensure!(imported == self.import_ordinary_sat && imported > 0
			&& p.ordinary_sat.checked_sub(imported) == Some(self.remaining_ordinary_sat)
			&& self.remaining_urgent_sat == p.urgent_sat, "opening totals/booked/imported/remainder inconsistent");
		ensure!(self.minimum_confirmations > 0 && self.minimum_confirmations <= 144,
			"opening requires explicit bounded confirmation policy");
		ensure!(!self.publication_logs.is_empty() && self.publication_logs.len() <= 16, "opening log coverage missing");
		let mut paths = BTreeSet::new();
		let mut bytes = 0u64;
		for log in &self.publication_logs {
			bytes = bytes.checked_add(log.bytes).context("opening log byte overflow")?;
			ensure!(paths.insert(&log.path) && bytes <= 64*1024*1024, "opening logs duplicate or exceed bound");
		}
		Ok(())
	}
}

pub(crate) async fn load(
	config: &Config, history_root: &Path, policy_sha: &str, p: &Policy,
	db: &Db, core: &bitcoind_async_client::Client, history_identity: &str,
) -> anyhow::Result<()> {
	ensure!(config.file.is_absolute(), "opening certificate path must be absolute");
	let raw = read_bounded(&config.file, MAX_BYTES)?;
	ensure!(digest(&raw) == config.sha256, "opening external certificate SHA differs");
	let c: Certificate = serde_json::from_slice(&raw)?;
	c.accounting(p, policy_sha)?;
	let retained = history_root.join("fee-opening-certificate.json");
	let validated = history_root.join("fee-opening-validated.json");
	let existing = db.read(async |t| Ok(t.query_opt("SELECT opening_digest,policy_bytes FROM fee_allowance_policy WHERE id='active'", &[])
		.await?.map(|row| (row.get::<_,Option<String>>(0),row.get::<_,Vec<u8>>(1))))).await?;
	if let Some((digest,policy_bytes)) = existing {
		ensure!(policy_bytes == serde_json::to_vec(p)?, "opening retained native policy differs");
		ensure!(digest.as_deref() == Some(config.sha256.as_str()), "native opening binding differs");
		ensure!(read_bounded(&retained, MAX_BYTES)? == raw
			&& read_bounded(&validated, 1024)? == config.sha256.as_bytes(), "original opening journal missing or conflicting");
		return Ok(());
	}
	// A validated original survives unknown COMMIT / DB-only pre-opening restore.
	// It waives only original capture age, NEVER source equality or old debits.
	let replay = match read_bounded(&validated, 1024) {
		Ok(saved) => { ensure!(saved == config.sha256.as_bytes(), "conflicting retained opening"); true },
		Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::NotFound) => false,
		Err(e) => return Err(e),
	};
	if !replay {
		let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
		ensure!(now >= c.created_at_unix && now - c.created_at_unix <= 120, "opening requires a fresh paused certificate");
		ensure!(core.get_block_count().await? == u64::from(c.tip_height), "opening current tip differs from paused source cut");
	}
	hash_file(&c.source_manifest, MAX_BYTES)?;
	for log in &c.publication_logs { hash_file(log, 64*1024*1024)?; }
	db.write(async |t| {
		t.batch_execute("SET LOCAL statement_timeout='10s'; SET LOCAL lock_timeout='5s';").await?;
		t.lock_fee_allowance().await?;
		ensure!(t.query_opt("SELECT id FROM fee_allowance_policy WHERE id='active'", &[]).await?.is_none(),
			"opening initialized concurrently; restart same original configuration");
		validate_source(t, p, &c, core).await?;
		ensure!(t.query_one("SELECT count(*) FROM fee_allowance_candidate", &[]).await?.get::<_,i64>(0) == 0,
			"opening cannot overwrite existing native fee reservations");
		// Mark the verified exact source before creating any prospective fee
		// ledger; after this point recovery repeats originals, never new credit.
		write_identical(&retained, &raw)?;
		write_identical(&validated, config.sha256.as_bytes())?;
		FeeHistory::initialize_original(history_root, p)?;
		let history = FeeHistory::load(history_root, p)?;
		t.execute("INSERT INTO fee_allowance_policy(id,policy_bytes,history_identity,opening_digest) VALUES('active',$1,$2,$3)",
			&[&serde_json::to_vec(p)?, &history_identity, &config.sha256]).await?;
		history.import_originals_locked(t, p, &c.candidates).await?;
		ensure!(core.get_block_hash(u64::from(c.tip_height)).await?.to_string() == c.tip_hash,
			"opening source anchor changed during verification");
		if !replay {ensure!(core.get_block_count().await? == u64::from(c.tip_height), "opening current tip changed during verification");}
		Ok(())
	}).await
}
