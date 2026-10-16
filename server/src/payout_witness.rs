//! Expiry payout history kept with the server identity, outside PostgreSQL.
//!
//! A database-only restore must not forget a broadcast payout. Before relaying
//! one, persist its raw transaction and immutable settlements. Before starting
//! workers, require every witnessed payout to survive in the database. This
//! cannot detect restoring the database and the witness to an older point together.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{Network, Transaction, Txid};
use bitcoin::constants::ChainHash;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::sha256;
use bitcoin::secp256k1::PublicKey;

use crate::database::{Db, Tx};
use crate::fs_perms;
use crate::database::carrier_snapshot::BackingSnapshotLimits;

pub(crate) const DIRECTORY: &str = "expiry-payout-witness";
pub(crate) const DIRECT_DIRECTORY: &str = "direct-expiry-witness";
pub(crate) const DIRECT_HISTORY_LIMITS: BackingSnapshotLimits = BackingSnapshotLimits {
	max_rows: 10_000,
	max_serialized_bytes: 16 * 1024 * 1024,
};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
	version: u32,
	genesis: String,
	server_pubkey: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Settlement {
	id: String,
	spk: Option<Vec<u8>>,
	fee_sat: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Witness {
	tx: Vec<u8>,
	settlements: Vec<Settlement>,
}

fn identity(network: Network, key: PublicKey) -> Identity {
	Identity { version: 1, genesis: ChainHash::using_genesis_block(network).to_string(),
		server_pubkey: key.to_string() }
}

/// The upstream writer fsyncs the temporary file before renaming it. Also
/// fsync the directory: a successful return must make the name durable.
fn write_new_or_identical(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
	match fs::read(path) {
		Ok(previous) => ensure!(previous == bytes, "conflicting payout recovery witness: {}", path.display()),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs_perms::write_atomic_owner_only(path, bytes)?,
		Err(e) => return Err(e).context("reading payout recovery witness"),
	}
	File::open(path.parent().context("witness has no directory")?)?.sync_all()?;
	Ok(())
}

impl Tx<'_> {
	async fn payout_witness(&self, tx: &Transaction) -> anyhow::Result<Witness> {
		let id = tx.compute_txid().to_string();
		let row = self.query_opt("SELECT tx, kind::TEXT FROM nursery_tx WHERE txid=$1", &[&id]).await?
			.context("payout recovery witness missing nursery transaction")?;
		ensure!(row.get::<_, String>(1) == "expiry-payout" && row.get::<_, Vec<u8>>(0) == serialize(tx),
			"payout recovery witness nursery transaction differs");
		let settlements = self.query("SELECT id, spk, fee_sat FROM expiry_settlement WHERE txid=$1 ORDER BY id",
			&[&id]).await?.into_iter().map(|r| Settlement {
				id: r.get(0), spk: r.get(1), fee_sat: r.get(2),
			}).collect::<Vec<_>>();
		ensure!(!settlements.is_empty() && settlements.iter().all(|s| s.fee_sat >= 0),
			"payout recovery witness missing or invalid settlements");
		Ok(Witness { tx: serialize(tx), settlements })
	}
}

#[derive(Clone)]
pub(crate) struct PayoutWitness {
	directory: PathBuf,
	indexed: Arc<tokio::sync::Mutex<BTreeSet<Txid>>>,
}

impl PayoutWitness {
	/// Only a newly created server may start an empty witness. Existing servers
	/// require an independently reconciled bootstrap, never automatic DB trust.
	pub(crate) fn initialize(datadir: &Path, network: Network, key: PublicKey) -> anyhow::Result<()> {
		let directory = datadir.join(DIRECTORY);
		fs::create_dir(&directory).context("creating payout recovery witness")?;
		fs_perms::harden(&directory, 0o700)?;
		write_new_or_identical(&directory.join("identity.json"), &serde_json::to_vec(&identity(network, key))?)?;
		write_new_or_identical(&directory.join("history"), b"")?;
		File::open(datadir)?.sync_all()?;
		Ok(())
	}

	pub(crate) async fn load(datadir: &Path, network: Network, key: PublicKey, db: &Db) -> anyhow::Result<Self> {
		let directory = datadir.join(DIRECTORY);
		let stored: Identity = serde_json::from_slice(&fs::read(directory.join("identity.json"))
			.context("payout recovery witness missing; restore its independent backup or reconcile history before enabling payouts")?)?;
		ensure!(stored == identity(network, key), "payout recovery witness server or network identity differs");
		let history = fs::read_to_string(directory.join("history"))
			.context("payout recovery witness history missing or corrupt")?;
		ensure!(history.is_empty() || history.ends_with('\n'), "incomplete payout recovery witness history");
		let mut indexed = BTreeSet::new();
		for line in history.lines() {
			let txid: Txid = line.parse().context("corrupt payout recovery witness history")?;
			ensure!(indexed.insert(txid), "duplicate payout recovery witness history entry");
		}
		let mut present = BTreeSet::new();
		for entry in fs::read_dir(&directory)? {
			let path = entry?.path();
			let name = path.file_name().and_then(|n| n.to_str()).context("invalid payout recovery witness filename")?;
			if name == "identity.json" || name == "history" || (name.starts_with('.') && name.ends_with(".tmp")) { continue; }
			let txid: Txid = name.strip_suffix(".json").context("invalid payout recovery witness filename")?.parse()?;
			let witness: Witness = serde_json::from_slice(&fs::read(&path)?)
				.context("corrupt payout recovery witness")?;
			let tx: Transaction = bitcoin::consensus::deserialize(&witness.tx)?;
			ensure!(tx.compute_txid() == txid, "payout recovery witness transaction id differs");
			let current = db.read(async |t| t.payout_witness(&tx).await).await?;
			ensure!(current == witness, "payout recovery witness differs from restored database for {txid}");
			present.insert(txid);
		}
		ensure!(indexed.is_subset(&present), "payout recovery witness file missing from indexed history");
		// A crash after the durable record rename but before indexing cannot
		// have broadcast it. Its already verified DB identity permits indexing
		// that same transaction, never a replacement or new owner.
		let witness = Self { directory, indexed: Arc::new(tokio::sync::Mutex::new(indexed)) };
		for txid in present { witness.index(txid).await?; }
		Ok(witness)
	}

	async fn index(&self, txid: Txid) -> anyhow::Result<()> {
		let mut indexed = self.indexed.lock().await;
		let mut history = OpenOptions::new().append(true).open(self.directory.join("history"))?;
		ensure!(history.metadata()?.len() == indexed.len() as u64 * 65,
			"payout recovery witness history changed or has an incomplete append; restart and reconcile");
		if !indexed.contains(&txid) {
			// Never create missing history during a retry. A torn append fails
			// startup rather than guessing which broadcast history survived.
			writeln!(history, "{txid}")?;
			history.sync_all()?;
			indexed.insert(txid);
		}
		Ok(())
	}

	pub(crate) async fn record(&self, db: &Db, tx: &Transaction) -> anyhow::Result<()> {
		let witness = db.read(async |t| t.payout_witness(tx).await).await?;
		let path = self.directory.join(format!("{}.json", tx.compute_txid()));
		write_new_or_identical(&path, &serde_json::to_vec(&witness)?)?;
		self.index(tx.compute_txid()).await
	}
}

/// Independent, immutable history of direct claims. PostgreSQL-only rollback
/// must refuse startup when it forgets signed bytes, allocation or attribution.
/// Restoring this history and PostgreSQL to an older point together is outside
/// that contract. The daemon owns the datadir exclusively while writing it.
pub struct DirectPayoutWitness {
	files: PayoutWitness,
	network: Network,
	key: PublicKey,
	budget: BackingSnapshotLimits,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirectRecord {
	freeze_set: String,
	tx: Vec<u8>,
	allocation_hash: sha256::Hash,
	carrier_hash: sha256::Hash,
}

/// Captured original facts only. This is not a validated delivery, ownership
/// proof, confirmed transaction, fee reservation or publication capability.
pub(crate) struct CapturedDirectRecord {
	pub freeze_set: String,
	pub transaction: Transaction,
}

/// Bind the complete captured legacy namespace to its original PG witness
/// strings with the SAME hashing as the actual native delivery reader. Source
/// provenance, the PG row projection, allocation/ownership policy, Full paths,
/// current chain and reservation accounting must be independently validated.
pub(crate) fn decode_captured_direct_history(identity_raw: &[u8], history: &str,
	records: &BTreeMap<String, String>, allocation_witnesses: &BTreeMap<String, Vec<String>>,
	carrier_witnesses: &BTreeMap<String, String>, signed_transactions: &BTreeMap<String, Vec<u8>>,
	network: Network, key: PublicKey,
) -> anyhow::Result<BTreeMap<Txid, CapturedDirectRecord>> {
	let max_rows = DIRECT_HISTORY_LIMITS.max_rows;
	let max_bytes = DIRECT_HISTORY_LIMITS.max_serialized_bytes;
	ensure!(identity_raw.len() <= 4096 && history.len() <= max_rows * 65,
		"captured direct identity/index exceeds native bound");
	let stored: Identity = serde_json::from_slice(identity_raw)?;
	ensure!(stored == identity(network, key) && serde_json::to_vec(&stored)? == identity_raw,
		"captured direct original server/network identity differs or is not canonical");
	ensure!(history.is_empty() || history.ends_with('\n'), "captured direct index is torn");
	let mut indexed = BTreeSet::new();
	for line in history.lines() {
		let txid: Txid = line.parse()?;
		ensure!(txid.to_string() == line && indexed.insert(txid), "captured direct index is noncanonical or repeated");
	}
	ensure!(records.len() == indexed.len() && carrier_witnesses.len() == indexed.len()
		&& signed_transactions.len() == indexed.len(),
		"captured direct original record/carrier membership differs from index");
	let mut bytes = identity_raw.len().checked_add(history.len()).context("captured direct bytes overflow")?;
	let mut rows = indexed.len();
	for raw in records.values().chain(carrier_witnesses.values()).chain(allocation_witnesses.values().flatten()) {
		bytes = bytes.checked_add(raw.len()).context("captured direct bytes overflow")?;
		rows = rows.checked_add(1).context("captured direct rows overflow")?;
		ensure!(bytes <= max_bytes && rows <= max_rows, "complete captured direct originals exceed native bounds");
	}
	for raw in signed_transactions.values() {
		bytes = bytes.checked_add(raw.len()).context("captured direct signed bytes overflow")?;
		ensure!(bytes <= max_bytes, "complete captured direct signed bodies exceed native bound");
	}
	let mut result = BTreeMap::new();
	let mut groups = BTreeSet::new();
	for txid in indexed {
		let raw = records.get(&format!("{txid}.json")).context("captured direct original file is missing")?;
		let record: DirectRecord = serde_json::from_str(raw)?;
		ensure!(serde_json::to_string(&record)? == *raw && !record.freeze_set.is_empty(),
			"captured direct original record is not canonical or lacks its group");
		let transaction: Transaction = deserialize(&record.tx)?;
		ensure!(transaction.compute_txid() == txid && serialize(&transaction) == record.tx
			&& signed_transactions.get(&txid.to_string()) == Some(&record.tx)
			&& !transaction.input.is_empty() && transaction.input.iter().all(|input| !input.witness.is_empty()),
			"captured direct signed transaction bytes or identity differ");
		let allocations = allocation_witnesses.get(&record.freeze_set).context("captured direct original allocation group is missing")?;
		let carrier = carrier_witnesses.get(&txid.to_string()).context("captured direct original carrier witness is missing")?;
		ensure!(!allocations.is_empty()
			&& crate::database::direct_delivery::allocation_witness_hash(allocations)? == record.allocation_hash
			&& crate::database::direct_delivery::carrier_witness_hash(carrier) == record.carrier_hash,
			"captured direct original allocation/carrier witness differs");
		groups.insert(record.freeze_set.clone());
		result.insert(txid, CapturedDirectRecord { freeze_set: record.freeze_set, transaction });
	}
	ensure!(groups == allocation_witnesses.keys().cloned().collect(), "captured direct original allocation group set differs");
	Ok(result)
}

impl DirectPayoutWitness {
	/// Called only during fresh server creation, before any direct journal rows.
	pub async fn initialize(datadir: &Path, network: Network, key: PublicKey, db: &Db) -> anyhow::Result<()> {
		Self::initialize_policy(datadir, network, key, db, crate::watchman::ordinary_fee::OrdinaryFeePolicy::default(), None, None).await
	}

	pub(crate) async fn initialize_with_fee_policy(
		datadir: &Path, network: Network, key: PublicKey, db: &Db,
		policy: crate::watchman::ordinary_fee::OrdinaryFeePolicy, chain: &crate::bitcoind::VerifiedFeeChain,
	) -> anyhow::Result<()> {
		ensure!(chain.matches(network), "direct history fee authority differs from witness network");
		policy.validate_config(network)?;
		let declaration = policy.declaration_bytes(datadir, chain)?;
		Self::initialize_policy(datadir, network, key, db, policy, Some(chain), declaration).await
	}

	async fn initialize_policy(
		datadir: &Path, network: Network, key: PublicKey, db: &Db,
		policy: crate::watchman::ordinary_fee::OrdinaryFeePolicy, chain: Option<&crate::bitcoind::VerifiedFeeChain>,
		declaration: Option<Vec<u8>>,
	) -> anyhow::Result<()> {
		let empty = db.read(async |t| Ok(t.query_one("SELECT NOT EXISTS (SELECT 1 FROM expiry_allocation)
			AND NOT EXISTS (SELECT 1 FROM expiry_carrier) AS empty", &[]).await?.get::<_, bool>("empty"))).await?;
		ensure!(empty, "cannot initialize empty direct history over an existing journal");
		let directory = datadir.join(DIRECT_DIRECTORY);
		fs::create_dir(&directory).context("creating direct payout history")?;
		fs_perms::harden(&directory, 0o700)?;
		if let Some(bytes) = declaration {
			let path = directory.join(crate::watchman::ordinary_fee::DECLARATION_FILE);
			fs_perms::create_new_owner_only(&path, &bytes)?;
			File::open(path)?.sync_all()?;
		}
		match chain {
			Some(chain) => policy.initialize_verified(&directory, chain)?,
			None => policy.initialize(&directory)?,
		}
		write_new_or_identical(&directory.join("identity.json"), &serde_json::to_vec(&identity(network, key))?)?;
		write_new_or_identical(&directory.join("history"), b"")?;
		File::open(datadir)?.sync_all()?;
		Ok(())
	}

	/// Verification precedes all signing, submission and fee-wallet construction.
	/// Missing/corrupt history is an error; it is never bootstrapped from the DB.
	pub async fn load(datadir: &Path, network: Network, key: PublicKey, db: &Db,
		budget: BackingSnapshotLimits,
	) -> anyhow::Result<Self> {
		ensure!(budget.max_rows > 0 && budget.max_serialized_bytes > 0, "empty direct history budget");
		let directory = datadir.join(DIRECT_DIRECTORY);
		let stored: Identity = serde_json::from_slice(&fs::read(directory.join("identity.json"))
			.context("independent direct payout identity missing")?)?;
		ensure!(stored == identity(network, key), "direct payout history identity differs");
		let history = fs::read_to_string(directory.join("history"))?;
		ensure!(history.is_empty() || history.ends_with('\n'), "incomplete direct payout history");
		let mut indexed = BTreeSet::new();
		for line in history.lines() {
			ensure!(indexed.insert(line.parse::<Txid>()?), "duplicate direct payout history entry");
		}
		let files = PayoutWitness { directory, indexed: Arc::new(tokio::sync::Mutex::new(indexed.clone())) };
		let witness = Self { files, network, key, budget };
		let mut present = BTreeSet::new();
		for entry in fs::read_dir(&witness.files.directory)? {
			let path = entry?.path();
			let name = path.file_name().and_then(|n| n.to_str()).context("invalid direct history filename")?;
			if name == crate::watchman::ordinary_fee::DECLARATION_FILE || name == "identity.json" || name == "history" ||
				name == crate::watchman::ordinary_fee::POLICY_FILE || name == crate::watchman::ordinary_fee::BUDGET_FILE ||
				(name.starts_with('.') && name.ends_with(".tmp")) { continue; }
			let txid: Txid = name.strip_suffix(".json").context("invalid direct history filename")?.parse()?;
			ensure!(fs::metadata(&path)?.len() <= u64::try_from(budget.max_serialized_bytes)?, "direct history record exceeds byte budget");
			let recorded: DirectRecord = serde_json::from_slice(&fs::read(&path)?)?;
			let tx: Transaction = deserialize(&recorded.tx)?;
			ensure!(tx.compute_txid() == txid, "direct history signed transaction identity differs");
			let current = witness.records(db, &recorded.freeze_set).await?;
			ensure!(current.iter().any(|r| *r == recorded), "direct payout history differs from restored database for {txid}");
			present.insert(txid);
		}
		ensure!(indexed.is_subset(&present), "indexed direct payout history file is missing");
		// A crash between durable rename and the index append cannot have passed
		// the publication gate. Verify its DB state before completing that append.
		for txid in present { witness.files.index(txid).await?; }
		Ok(witness)
	}

	/// Bind the loaded history to the actual backing server, not a caller's
	/// convenient empty directory. A changed on-disk identity also fails closed.
	pub(crate) fn require_identity(&self, network: Network, key: PublicKey) -> anyhow::Result<()> {
		ensure!(self.network == network && self.key == key, "direct payout witness does not belong to backing server/network");
		let path = self.files.directory.join("identity.json");
		ensure!(fs::metadata(&path)?.len() <= u64::try_from(self.budget.max_serialized_bytes)?, "direct payout identity exceeds budget");
		let stored: Identity = serde_json::from_slice(&fs::read(path)?)?;
		ensure!(stored == identity(self.network, self.key), "direct payout history identity changed");
		Ok(())
	}

	async fn records(&self, db: &Db, freeze_set: &str) -> anyhow::Result<Vec<DirectRecord>> {
		// Hashes and bytes come from the same bounded, fully authenticated
		// repeatable-read snapshot. Observation timestamps are deliberately excluded.
		let versions = db.read_direct_delivery(freeze_set, self.network, self.budget).await?;
		ensure!(versions.iter().all(|v| v.server_pubkey == self.key), "direct payout witness does not belong to current backing server");
		Ok(versions.into_iter().map(|v| DirectRecord { freeze_set: freeze_set.to_owned(), tx: v.bytes,
			allocation_hash: v.allocation_hash, carrier_hash: v.carrier_hash }).collect())
	}

	/// Successful return means every signed version's financial identity is
	/// fsynced independently of PostgreSQL. Call before observing/submitting it.
	pub async fn record_group(&self, db: &Db, freeze_set: &str) -> anyhow::Result<()> {
		self.require_identity(self.network, self.key)?;
		for record in self.records(db, freeze_set).await? {
			let tx: Transaction = deserialize(&record.tx)?;
			let txid = tx.compute_txid();
			write_new_or_identical(&self.files.directory.join(format!("{txid}.json")), &serde_json::to_vec(&record)?)?;
			self.files.index(txid).await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Witness};
	use bitcoin::hashes::Hash;

	struct CapturedFixture {
		key: PublicKey, identity: Vec<u8>, history: String,
		records: BTreeMap<String, String>, allocations: BTreeMap<String, Vec<String>>,
		carriers: BTreeMap<String, String>, signed: BTreeMap<String, Vec<u8>>,
	}
	impl CapturedFixture {
		fn new() -> Self {
			let key = bitcoin::secp256k1::Keypair::new(&crate::SECP,
				&mut bitcoin::secp256k1::rand::thread_rng()).public_key();
			// Synthetic original-byte transport fixture, not a spend/signature or
			// current financial ownership proof.
			let tx = Transaction { version: bitcoin::transaction::Version::TWO,
				lock_time: bitcoin::absolute::LockTime::ZERO,
				input: vec![TxIn { previous_output: OutPoint::new(Txid::from_byte_array([7;32]), 0),
					script_sig: ScriptBuf::new(), sequence: Sequence::ZERO, witness: Witness::from_slice(&[vec![1;64]]) }],
				output: vec![TxOut { value: Amount::from_sat(9_000), script_pubkey: ScriptBuf::new() }] };
			let txid = tx.compute_txid(); let bytes = serialize(&tx);
			let allocations = vec!["{\"id\": \"synthetic\", \"owner\": null, \"spk\": null, \"disposition\": \"operator\", \"gross\": 10000, \"fee\": 0, \"payout\": 0, \"forfeited\": 0, \"source\": \"operator\", \"evidence\": {}, \"retirement\": []}".to_owned()];
			let carrier = "{\"generation\": 1, \"backing\": [], \"fee_inputs\": [], \"miner\": 1000, \"subsidy\": 0, \"mappings\": [{\"id\": \"synthetic\", \"vout\": null}]}".to_owned();
			let record = DirectRecord { freeze_set: "synthetic-group".into(), tx: bytes.clone(),
				allocation_hash: crate::database::direct_delivery::allocation_witness_hash(&allocations).unwrap(),
				carrier_hash: crate::database::direct_delivery::carrier_witness_hash(&carrier) };
			Self { key, identity: serde_json::to_vec(&identity(Network::Regtest, key)).unwrap(), history: format!("{txid}\n"),
				records: [(format!("{txid}.json"), serde_json::to_string(&record).unwrap())].into_iter().collect(),
				allocations: [(record.freeze_set, allocations)].into_iter().collect(),
				carriers: [(txid.to_string(), carrier)].into_iter().collect(),
				signed: [(txid.to_string(), bytes)].into_iter().collect() }
		}
		fn decode(&self) -> anyhow::Result<BTreeMap<Txid, CapturedDirectRecord>> {
			decode_captured_direct_history(&self.identity, &self.history, &self.records,
				&self.allocations, &self.carriers, &self.signed, Network::Regtest, self.key)
		}
	}

	#[test]
	fn captured_direct_originals_bind_exact_pg_text_nulls_and_signed_bytes_without_financial_authority() {
		let fixture = CapturedFixture::new();
		let records = fixture.decode().unwrap();
		assert_eq!(records.len(), 1);
		for (id, record) in records {
			assert_eq!(record.freeze_set, "synthetic-group");
			assert_eq!(serialize(&record.transaction), fixture.signed[&id.to_string()]);
		}
		let mut changed = CapturedFixture::new();
		changed.allocations.get_mut("synthetic-group").unwrap()[0].push(' ');
		assert!(changed.decode().is_err(), "equivalent JSON is not original PG witness text");
		let mut changed = CapturedFixture::new();
		for raw in changed.carriers.values_mut() { *raw = raw.replace("null", "0"); }
		assert!(changed.decode().is_err(), "NULL non-payment mapping must not become an output");
		let mut changed = CapturedFixture::new();
		for raw in changed.records.values_mut() {
			let mut record: DirectRecord = serde_json::from_str(raw).unwrap();
			let mut tx: Transaction = deserialize(&record.tx).unwrap();
			tx.input[0].witness = Witness::from_slice(&[vec![2;64]]);
			record.tx = serialize(&tx); *raw = serde_json::to_string(&record).unwrap();
		}
		assert!(changed.decode().is_err(), "same txid with different witness must fail the original SQL-body join");
	}
	#[test]
	fn captured_direct_originals_refuse_torn_missing_duplicate_unindexed_or_foreign_sources() {
		for which in 0..7 {
			let mut fixture = CapturedFixture::new();
			match which {
				0 => { fixture.history.pop(); },
				1 => fixture.history.push_str(&fixture.history.clone()),
				2 => fixture.records.clear(),
				3 => { fixture.records.insert("unindexed.json".into(), "{}".into()); },
				4 => fixture.identity = serde_json::to_vec(&identity(Network::Signet, fixture.key)).unwrap(),
				5 => { fixture.allocations.insert("unknown-group".into(), Vec::new()); },
				_ => fixture.signed.clear(),
			}
			assert!(fixture.decode().is_err());
		}
	}

	struct Directory(PathBuf);
	impl Directory {
		fn new() -> Self {
			let p = std::env::temp_dir().join(format!("ark-payout-witness-{}-{}", std::process::id(),
				bitcoin::secp256k1::rand::random::<u64>()));
			fs::create_dir(&p).unwrap();
			Self(p)
		}
	}
	impl Drop for Directory {
		fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
	}

	#[test]
	fn immutable_record_retry_and_conflict() {
		let dir = Directory::new();
		let path = dir.0.join("record.json");
		write_new_or_identical(&path, b"original committed history").unwrap();
		write_new_or_identical(&path, b"original committed history").unwrap();
		assert!(write_new_or_identical(&path, b"changed payee").is_err());
		assert_eq!(fs::read(&path).unwrap(), b"original committed history");
		assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
	}

	#[test]
	fn partial_existing_record_is_never_replaced() {
		let dir = Directory::new();
		let path = dir.0.join("record.json");
		fs::write(&path, b"partial").unwrap();
		assert!(write_new_or_identical(&path, b"complete committed history").is_err());
		assert_eq!(fs::read(&path).unwrap(), b"partial");
	}

	#[test]
	fn inaccessible_record_path_cannot_report_success() {
		let dir = Directory::new();
		let path = dir.0.join("record.json");
		fs::create_dir(&path).unwrap();
		assert!(write_new_or_identical(&path, b"committed history").is_err());
		assert!(path.is_dir());
	}
}
