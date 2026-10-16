//! SOURCE-ONLY prototype: not connected to any signing or publication path.
//! The legacy watchman lifetime lock and legacy parser must be migrated together
//! before this operation-lock protocol can be used. Never deploy this file alone.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use bitcoin::consensus::serialize;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::{Amount, FeeRate, Transaction, Wtxid};

use super::ordinary_fee::{OrdinaryFeePolicy, BUDGET_FILE, POLICY_FILE};

const MAX_HISTORY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PROOF_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationPath { Round, Pool }
impl PublicationPath {
	fn name(self) -> &'static str { match self { Self::Round => "round", Self::Pool => "pool" } }
}

// Pricing only: this type does not prove ownership, confirmed urgent liquidity,
// original chain, signatures, physical reservations or permission to publish.
struct PricedPublication {
	path: PublicationPath,
	wtxid: Wtxid,
	raw_sha256: sha256::Hash,
	fee_sat: u64,
}
impl PricedPublication {
	fn price(path: PublicationPath, tx: &Transaction, parents: &[Transaction], max_rate: FeeRate) -> anyhow::Result<Self> {
		ensure!(!tx.is_coinbase() && !tx.input.is_empty() && tx.input.len() <= 4096,
			"not a bounded ordinary publication");
		ensure!(tx.weight().to_wu() <= 400_000, "ordinary publication exceeds weight bound");
		let mut proof_bytes = serialize(tx).len();
		let mut source = BTreeMap::new();
		for parent in parents {
			proof_bytes = proof_bytes.checked_add(serialize(parent).len()).context("parent proof overflow")?;
			ensure!(proof_bytes <= MAX_PROOF_BYTES, "ordinary parent proof exceeds byte bound");
			ensure!(source.insert(parent.compute_txid(), parent).is_none(), "duplicate original parent");
		}
		let mut points = BTreeSet::new();
		let mut used_parents = BTreeSet::new();
		let mut gross = Amount::ZERO;
		for input in &tx.input {
			ensure!(points.insert(input.previous_output), "duplicate ordinary input");
			let parent = source.get(&input.previous_output.txid).context("original input parent missing")?;
			let output = parent.output.get(input.previous_output.vout as usize).context("original prevout missing")?;
			ensure!(output.value <= Amount::MAX_MONEY, "original prevout exceeds money bound");
			gross = gross.checked_add(output.value).context("ordinary input sum overflow")?;
			ensure!(gross <= Amount::MAX_MONEY, "ordinary input sum exceeds money bound");
			used_parents.insert(input.previous_output.txid);
		}
		ensure!(used_parents.len() == source.len(), "unrelated original parent");
		let outputs = tx.output.iter().try_fold(Amount::ZERO, |sum, output| {
			ensure!(output.value <= Amount::MAX_MONEY, "ordinary output exceeds money bound");
			let sum = sum.checked_add(output.value).context("ordinary output sum overflow")?;
			ensure!(sum <= Amount::MAX_MONEY, "ordinary output sum exceeds money bound");
			Ok::<_, anyhow::Error>(sum)
		})?;
		let fee = gross.checked_sub(outputs).context("ordinary outputs exceed original inputs")?;
		let maximum = max_rate.fee_wu(tx.weight()).context("signed rate ceiling overflow")?;
		ensure!(fee > Amount::ZERO && fee <= maximum, "actual signed ordinary fee exceeds rate ceiling or is zero");
		Ok(Self { path, wtxid: tx.compute_wtxid(), raw_sha256: sha256::Hash::hash(&serialize(tx)), fee_sat: fee.to_sat() })
	}
	fn entry(&self) -> String {
		format!("v1\t{}\t{}\t{}\t{}\n", self.path.name(), self.wtxid, self.raw_sha256, self.fee_sat)
	}
}

#[derive(Default)]
struct Usage {
	total_sat: u64,
	// Legacy numeric charges are ONLY carrier ceiling reservations. Pool and
	// round fees must never cover missing retained carrier reservations.
	carrier_ceiling_sat: u64,
	publications: BTreeMap<Wtxid, String>,
}
impl Usage {
	fn require_retained_carriers(&self, retained: u64) -> anyhow::Result<()> {
		ensure!(retained <= self.carrier_ceiling_sat, "carrier history lacks its own reservations");
		Ok(())
	}
}
fn usage(history: &str, policy: OrdinaryFeePolicy) -> anyhow::Result<Usage> {
	policy.validate()?; // Qualification marker semantics intentionally not invented here.
	ensure!(history.is_empty() || history.ends_with('\n'), "torn joint fee history");
	let mut result = Usage::default();
	for line in history.lines() {
		let charge = if line.starts_with("v1\t") {
			let fields = line.split('\t').collect::<Vec<_>>();
			ensure!(fields.len() == 5 && matches!(fields[1], "round" | "pool"), "invalid publication history");
			let id = fields[2].parse::<Wtxid>()?;
			let digest = fields[3].parse::<sha256::Hash>()?;
			let fee = fields[4].parse::<u64>()?;
			ensure!(id.to_string() == fields[2] && digest.to_string() == fields[3]
				&& fee > 0 && fee.to_string() == fields[4], "noncanonical publication history");
			ensure!(result.publications.insert(id, line.to_string()).is_none(), "duplicate publication charge");
			fee
		} else {
			let amount = line.parse::<u64>()?;
			ensure!(amount > 0 && amount.to_string() == line, "invalid legacy carrier charge");
			result.carrier_ceiling_sat = result.carrier_ceiling_sat.checked_add(amount).context("carrier sum overflow")?;
			amount
		};
		result.total_sat = result.total_sat.checked_add(charge).context("joint fee sum overflow")?;
		ensure!(result.total_sat <= policy.ordinary_budget_sat, "joint native ordinary allowance exhausted");
	}
	Ok(result)
}

struct LockedLedger { file: File }
impl Drop for LockedLedger {
	fn drop(&mut self) { let _ = self.file.unlock(); }
}
// Every operation opens a fresh file description: the kernel lock serializes
// separate processes as well as threads, unlike an in-memory mutex/cache.
struct SharedHistoryPrototype { directory: PathBuf, policy: OrdinaryFeePolicy }
impl SharedHistoryPrototype {
	fn existing(directory: &Path, policy: OrdinaryFeePolicy) -> anyhow::Result<Self> {
		policy.validate()?;
		let stored = OrdinaryFeePolicy::require_live_stored(directory)?;
		ensure!(stored == policy, "joint history policy differs");
		ensure!(directory.join(BUDGET_FILE).is_file(), "existing native fee ledger missing");
		Ok(Self { directory: directory.into(), policy })
	}
	fn locked(&self) -> anyhow::Result<(LockedLedger, String)> {
		ensure!(OrdinaryFeePolicy::require_live_stored(&self.directory)? == self.policy, "native allowance changed");
		let path = self.directory.join(BUDGET_FILE);
		ensure!(std::fs::symlink_metadata(&path)?.file_type().is_file(), "joint ledger is not a regular file");
		let mut file = OpenOptions::new().read(true).append(true).open(&path)?;
		file.lock().context("joint native history lock failed")?;
		let mut locked = LockedLedger { file };
		let actual = std::fs::symlink_metadata(&path)?;
		let opened = locked.file.metadata()?;
		ensure!(actual.file_type().is_file() && actual.dev() == opened.dev() && actual.ino() == opened.ino(),
			"joint fee history file was replaced");
		ensure!(opened.len() <= MAX_HISTORY_BYTES, "joint native history exceeds byte bound");
		locked.file.seek(SeekFrom::Start(0))?;
		let mut history = String::new();
		(&mut locked.file).take(MAX_HISTORY_BYTES + 1).read_to_string(&mut history)?;
		ensure!(history.len() as u64 <= MAX_HISTORY_BYTES, "joint native history exceeds byte bound");
		usage(&history, self.policy)?;
		Ok((locked, history))
	}
	fn reserve_publication(&self, candidate: &PricedPublication) -> anyhow::Result<()> {
		let (mut locked, history) = self.locked()?;
		let before = usage(&history, self.policy)?;
		let entry = candidate.entry();
		if let Some(old) = before.publications.get(&candidate.wtxid) {
			ensure!(old == entry.trim_end_matches('\n'), "same signed publication has conflicting fee proof");
			return Ok(());
		}
		let next = before.total_sat.checked_add(candidate.fee_sat).context("joint fee sum overflow")?;
		ensure!(next <= self.policy.ordinary_budget_sat, "joint native ordinary allowance exhausted");
		ensure!(history.len() as u64 + entry.len() as u64 <= MAX_HISTORY_BYTES, "joint native history capacity exhausted");
		locked.file.write_all(entry.as_bytes())?;
		locked.file.sync_all()?;
		// An uncertain append/fsync never returns a permit. Next open reparses the
		// original bytes and conservatively retains a complete charge or refuses a torn one.
		Ok(())
	}
}

#[test]
fn joint_pricing_refuses_missing_changed_parent_and_duplicate_input() {
	let (parent, tx) = transaction(300);
	let rate = FeeRate::from_sat_per_vb_u32(100);
	assert_eq!(PricedPublication::price(PublicationPath::Round, &tx, &[parent.clone()], rate).unwrap().fee_sat, 300);
	assert!(PricedPublication::price(PublicationPath::Round, &tx, &[], rate).is_err());
	let mut wrong = parent.clone(); wrong.output[0].value = Amount::from_sat(1_000);
	assert!(PricedPublication::price(PublicationPath::Round, &tx, &[wrong], rate).is_err());
	let mut duplicate = tx.clone(); duplicate.input.push(duplicate.input[0].clone());
	assert!(PricedPublication::price(PublicationPath::Round, &duplicate, &[parent], rate).is_err());
	let (parent, _) = transaction(300);
	assert!(PricedPublication::price(PublicationPath::Round, &tx, &[parent], FeeRate::from_sat_per_vb_u32(1)).is_err());
}

#[test]
fn existing_carrier_charges_and_publication_restart_do_not_renew_native_58000() {
	let directory = directory("restart");
	let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
	std::fs::write(directory.join(POLICY_FILE), serde_json::to_vec(&policy).unwrap()).unwrap();
	std::fs::write(directory.join(BUDGET_FILE), b"50000\n").unwrap();
	let (parent, tx) = transaction(5_000);
	let candidate = PricedPublication::price(PublicationPath::Round, &tx, &[parent], FeeRate::from_sat_per_vb_u32(100)).unwrap();
	SharedHistoryPrototype::existing(&directory, policy).unwrap().reserve_publication(&candidate).unwrap();
	let restart = SharedHistoryPrototype::existing(&directory, policy).unwrap();
	restart.reserve_publication(&candidate).unwrap();
	let (parent2, mut next) = transaction(5_000); next.lock_time = bitcoin::absolute::LockTime::from_height(2).unwrap();
	let second = PricedPublication::price(PublicationPath::Pool, &next, &[parent2], FeeRate::from_sat_per_vb_u32(100)).unwrap();
	assert!(restart.reserve_publication(&second).is_err());
	let state = usage(&std::fs::read_to_string(directory.join(BUDGET_FILE)).unwrap(), policy).unwrap();
	assert_eq!(state.total_sat, 55_000); assert_eq!(state.carrier_ceiling_sat, 50_000); assert_eq!(state.publications.len(), 1);
	state.require_retained_carriers(50_000).unwrap();
	assert!(state.require_retained_carriers(50_001).is_err(), "publication credit cannot cover missing carrier reservations");
	std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn concurrent_round_pool_cannot_both_spend_same_native_remainder() {
	let directory = directory("concurrent");
	let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
	std::fs::write(directory.join(POLICY_FILE), serde_json::to_vec(&policy).unwrap()).unwrap();
	std::fs::write(directory.join(BUDGET_FILE), b"50000\n").unwrap();
	let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
	let workers = [PublicationPath::Round, PublicationPath::Pool].into_iter().enumerate().map(|(i, path)| {
		let directory = directory.clone(); let barrier = barrier.clone();
		std::thread::spawn(move || {
			let (parent, mut tx) = transaction(5_000); tx.lock_time = bitcoin::absolute::LockTime::from_height(i as u32 + 1).unwrap();
			let candidate = PricedPublication::price(path, &tx, &[parent], FeeRate::from_sat_per_vb_u32(100)).unwrap();
			let ledger = SharedHistoryPrototype::existing(&directory, policy).unwrap();
			barrier.wait(); ledger.reserve_publication(&candidate).is_ok()
		})
	}).collect::<Vec<_>>();
	assert_eq!(workers.into_iter().map(|w| w.join().expect("concurrent ledger worker panicked")).filter(|ok| *ok).count(), 1);
	let state = usage(&std::fs::read_to_string(directory.join(BUDGET_FILE)).unwrap(), policy).unwrap();
	assert_eq!(state.total_sat, 55_000); assert_eq!(state.carrier_ceiling_sat, 50_000);
	std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn torn_or_unknown_history_and_regtest_marker_cannot_become_new_credit() {
	let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
	for bad in ["50000", "050000\n", "v1\tunknown\tno\n", "58000\n1\n"] { assert!(usage(bad, policy).is_err()); }
	// A nonzero marker is not sufficient authority to change attempt semantics.
	let qualification = super::ordinary_fee::RegtestQualificationBudget {
		max_attempts: 6, per_attempt_ceiling_sat: 10_000,
		declaration_sha256: sha256::Hash::hash(b"SYNTHETIC, NOT COMMON AUTHORITY"),
	};
	assert!(usage("10000\n", OrdinaryFeePolicy { regtest_qualification: Some(qualification), ..policy }).is_err());
}

fn directory(name: &str) -> PathBuf {
	let path = std::env::temp_dir().join(format!("c-joint-fee-{name}-{}-{}", std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
	std::fs::create_dir(&path).unwrap(); path
}
fn transaction(fee: u64) -> (Transaction, Transaction) {
	let parent = Transaction { version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint::null(), script_sig: bitcoin::ScriptBuf::new(),
			sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::default() }],
		output: vec![bitcoin::TxOut { value: Amount::from_sat(10_000), script_pubkey: bitcoin::ScriptBuf::new() }] };
	let tx = Transaction { version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint::new(parent.compute_txid(), 0), script_sig: bitcoin::ScriptBuf::new(),
			sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::from_slice(&[vec![0; 64]]) }],
		output: vec![bitcoin::TxOut { value: Amount::from_sat(10_000 - fee), script_pubkey: bitcoin::ScriptBuf::new() }] };
	(parent, tx)
}
