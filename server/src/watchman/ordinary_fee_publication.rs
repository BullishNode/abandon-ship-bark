//! Exact original ordinary publications in the existing native fee history.
//! A reservation is accounting evidence, never proof of wallet ownership or a
//! substitute for the owning-wallet urgent-source check at broker admission.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};

use anyhow::Context;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::{Amount, FeeRate, Transaction, Wtxid};

use super::ordinary_fee::OrdinaryFeePolicy;

const DIRECTORY: &str = "ordinary-native-publications";
const MAX_RECORDS: usize = 4096;
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PublicationPath { Round, Pool }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationRecord {
	version: u8,
	path: PublicationPath,
	policy_sha256: sha256::Hash,
	raw: Vec<u8>,
	input_parents: Vec<Vec<u8>>,
	max_fee_rate: FeeRate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationEntry {
	version: u8,
	path: PublicationPath,
	wtxid: Wtxid,
	record_sha256: sha256::Hash,
	fee_sat: u64,
	charge_sat: u64,
}
impl PublicationEntry {
	pub(crate) fn parse(bytes: &str) -> anyhow::Result<Self> {
		let entry: Self = serde_json::from_str(bytes)?;
		ensure!(entry.version == 1 && serde_json::to_string(&entry)? == bytes,
			"noncanonical ordinary publication entry");
		Ok(entry)
	}
	pub(crate) fn line(&self) -> anyhow::Result<String> { Ok(format!("p1\t{}\n", serde_json::to_string(self)?)) }
	pub(crate) fn id(&self) -> Wtxid { self.wtxid }
	pub(crate) fn charge(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
		let expected = match policy.regtest_qualification {
			Some(marker) => marker.per_attempt_ceiling_sat,
			None => self.fee_sat,
		};
		ensure!(self.fee_sat > 0 && self.fee_sat <= expected && self.charge_sat == expected,
			"ordinary publication charge differs from existing policy");
		Ok(self.charge_sat)
	}
}

impl PublicationRecord {
	// The broker supplies actual original DB/Core parents and its own frozen
	// configured rate, not caller fee integers or a caller-selected policy.
	pub(crate) fn new(path: PublicationPath, policy: OrdinaryFeePolicy,
		tx: &Transaction, parents: &[Transaction], max_fee_rate: FeeRate,
	) -> anyhow::Result<Self> {
		let record = Self { version: 1, path,
			policy_sha256: sha256::Hash::hash(&serde_json::to_vec(&policy)?),
			raw: serialize(tx), input_parents: parents.iter().map(serialize).collect(), max_fee_rate };
		record.entry(policy)?;
		Ok(record)
	}
	pub(crate) fn matches(&self, tx: &Transaction) -> anyhow::Result<()> {
		ensure!(self.raw == serialize(tx), "ordinary permit signed bytes differ");
		Ok(())
	}
	pub(crate) fn entry(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<PublicationEntry> {
		ensure!(self.version == 1 && self.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?),
			"ordinary publication policy identity differs");
		ensure!(self.raw.len() as u64 <= MAX_RECORD_BYTES && self.input_parents.len() <= 4096,
			"ordinary publication input proof exceeds bound");
		let proof_bytes = self.input_parents.iter().try_fold(self.raw.len() as u64, |sum, raw| {
			sum.checked_add(raw.len() as u64).context("ordinary publication proof sum overflow")
		})?;
		ensure!(proof_bytes <= MAX_RECORD_BYTES, "ordinary publication input proof exceeds byte bound");
		let bytes = serde_json::to_vec(self)?;
		ensure!(bytes.len() as u64 <= MAX_RECORD_BYTES, "ordinary publication proof exceeds byte bound");
		let tx: Transaction = deserialize(&self.raw)?;
		ensure!(serialize(&tx) == self.raw && !tx.is_coinbase() && !tx.input.is_empty()
			&& tx.input.len() <= 4096 && tx.weight().to_wu() <= 400_000,
			"ordinary publication is not a canonical bounded spend");
		let mut parents = BTreeMap::new();
		for bytes in &self.input_parents {
			let parent: Transaction = deserialize(bytes)?;
			ensure!(serialize(&parent) == *bytes && parents.insert(parent.compute_txid(), parent).is_none(),
				"noncanonical or duplicate original parent");
		}
		let mut points = BTreeSet::new();
		let mut used = BTreeSet::new();
		let mut gross = Amount::ZERO;
		for input in &tx.input {
			ensure!(points.insert(input.previous_output), "duplicate ordinary input");
			let parent = parents.get(&input.previous_output.txid).context("original fee input parent missing")?;
			let output = parent.output.get(input.previous_output.vout as usize).context("original fee prevout missing")?;
			gross = add_money(gross, output.value)?;
			used.insert(input.previous_output.txid);
		}
		ensure!(used.len() == parents.len(), "unrelated ordinary fee parent");
		let outputs = tx.output.iter().try_fold(Amount::ZERO, |sum, output| add_money(sum, output.value))?;
		let fee = gross.checked_sub(outputs).context("ordinary outputs exceed original inputs")?;
		let ceiling = self.max_fee_rate.fee_wu(tx.weight()).context("ordinary signed rate ceiling overflow")?;
		ensure!(fee > Amount::ZERO && fee <= ceiling, "actual signed ordinary fee exceeds rate ceiling or is zero");
		let charge = policy.regtest_qualification.map_or(fee.to_sat(), |marker| marker.per_attempt_ceiling_sat);
		let entry = PublicationEntry { version: 1, path: self.path, wtxid: tx.compute_wtxid(),
			record_sha256: sha256::Hash::hash(&bytes), fee_sat: fee.to_sat(), charge_sat: charge };
		entry.charge(policy)?;
		Ok(entry)
	}
}
fn add_money(left: Amount, right: Amount) -> anyhow::Result<Amount> {
	ensure!(right <= Amount::MAX_MONEY, "ordinary value exceeds money bound");
	let sum = left.checked_add(right).context("ordinary value overflow")?;
	ensure!(sum <= Amount::MAX_MONEY, "ordinary sum exceeds money bound");
	Ok(sum)
}

// A fixed sibling of the existing history, not a caller-selected witness root.
// This leaves the independently authenticated direct-carrier namespace intact.
fn directory(history: &Path) -> anyhow::Result<PathBuf> {
	let name = history.file_name().and_then(|name| name.to_str()).context("invalid ordinary history name")?;
	Ok(history.with_file_name(format!("{name}.{DIRECTORY}")))
}
fn identity(meta: &fs::Metadata) -> (u64, u64, u32, u32, u64, u64, i64, i64, i64, i64) {
	(meta.dev(), meta.ino(), meta.uid(), meta.mode(), meta.nlink(), meta.len(),
		meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec())
}
fn private_root(path: &Path, owner: u32) -> anyhow::Result<(File, fs::Metadata)> {
	let before = fs::symlink_metadata(path)?;
	ensure!(before.is_dir() && before.uid() == owner && before.mode() & 0o7777 == 0o700,
		"ordinary publication root must be a real owner-private directory");
	let file = File::open(path)?;
	ensure!(identity(&before) == identity(&file.metadata()?)
		&& identity(&before) == identity(&fs::symlink_metadata(path)?),
		"ordinary publication root changed during open");
	Ok((file, before))
}
fn open_record(path: &Path, owner: u32) -> anyhow::Result<(File, fs::Metadata)> {
	let before = fs::symlink_metadata(path)?;
	ensure!(before.is_file() && before.nlink() == 1 && before.uid() == owner
		&& before.mode() & 0o7777 == 0o600 && before.len() <= MAX_RECORD_BYTES,
		"ordinary publication proof must be bounded, singly linked and owner-private");
	let file = File::open(path)?;
	ensure!(identity(&before) == identity(&file.metadata()?)
		&& identity(&before) == identity(&fs::symlink_metadata(path)?),
		"ordinary publication proof changed during open");
	Ok((file, before))
}
fn read_record(path: &Path, owner: u32) -> anyhow::Result<Vec<u8>> {
	let (file, before) = open_record(path, owner)?;
	let mut bytes = Vec::new(); (&file).take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
	ensure!(bytes.len() as u64 == before.len() && bytes.len() as u64 <= MAX_RECORD_BYTES
		&& identity(&before) == identity(&file.metadata()?)
		&& identity(&before) == identity(&fs::symlink_metadata(path)?),
		"ordinary publication proof changed during bounded read");
	Ok(bytes)
}

pub(crate) fn validate_witnesses(history: &Path, policy: OrdinaryFeePolicy,
	entries: &BTreeMap<Wtxid, PublicationEntry>,
) -> anyhow::Result<()> {
	let directory = directory(history)?;
	let owner = fs::symlink_metadata(history)?.uid();
	match fs::symlink_metadata(&directory) {
		Ok(_) => {},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			ensure!(entries.is_empty(), "indexed ordinary publication witness missing"); return Ok(());
		},
		Err(error) => return Err(error.into()),
	}
	let (root, root_before) = private_root(&directory, owner)?;
	let mut present = BTreeSet::new(); let mut bytes = 0u64;
	for (index, item) in fs::read_dir(&directory)?.enumerate() {
		ensure!(index < MAX_RECORDS, "ordinary publication witness count exceeds bound");
		let item = item?; let name = item.file_name(); let name = name.to_str().context("invalid publication filename")?;
		let id: Wtxid = name.strip_suffix(".json").context("unknown ordinary publication file")?.parse()?;
		ensure!(name == format!("{id}.json"), "noncanonical publication filename");
		let raw = read_record(&item.path(), owner)?;
		bytes = bytes.checked_add(raw.len() as u64).context("ordinary witness byte overflow")?;
		ensure!(bytes <= MAX_TOTAL_BYTES, "ordinary publication witness bytes exceed bound");
		let record: PublicationRecord = serde_json::from_slice(&raw)?;
		ensure!(serde_json::to_vec(&record)? == raw, "noncanonical ordinary publication witness");
		let expected = record.entry(policy)?;
		ensure!(expected.id() == id, "ordinary publication witness id differs");
		let entry = entries.get(&id).context("unindexed ordinary publication intent; preserve and reconcile original reservation before workers")?;
		ensure!(*entry == expected && present.insert(id), "ordinary publication witness differs from retained fee entry");
	}
	ensure!(present.len() == entries.len(), "indexed ordinary publication witness missing");
	ensure!(identity(&root_before) == identity(&root.metadata()?)
		&& identity(&root_before) == identity(&fs::symlink_metadata(&directory)?),
		"ordinary publication root changed during complete scan");
	Ok(())
}

pub(crate) fn write_witness(history: &Path, policy: OrdinaryFeePolicy,
	existing: &BTreeMap<Wtxid, PublicationEntry>, record: &PublicationRecord,
) -> anyhow::Result<PublicationEntry> {
	validate_witnesses(history, policy, existing)?;
	let entry = record.entry(policy)?;
	let root = directory(history)?;
	match fs::symlink_metadata(&root) {
		Ok(_) => {},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			fs::DirBuilder::new().mode(0o700).create(&root)?;
			File::open(root.parent().context("publication parent missing")?)?.sync_all()?;
		},
		Err(error) => return Err(error.into()),
	}
	let owner = fs::symlink_metadata(history)?.uid();
	let (root_file, _) = private_root(&root, owner)?;
	let raw = serde_json::to_vec(record)?;
	let current = existing.values().try_fold(0u64, |sum, entry| {
		let len = fs::metadata(root.join(format!("{}.json", entry.id())))?.len();
		sum.checked_add(len).context("ordinary witness sum overflow")
	})?;
	ensure!(existing.len() < MAX_RECORDS && current.checked_add(raw.len() as u64)
		.is_some_and(|sum| sum <= MAX_TOTAL_BYTES), "ordinary publication witness capacity exhausted");
	let path = root.join(format!("{}.json", entry.id()));
	crate::fs_perms::create_new_owner_only(&path, &raw)?;
	ensure!(read_record(&path, owner)? == raw, "new ordinary witness readback differs");
	let (proof, _) = open_record(&path, owner)?;
	proof.sync_all()?; root_file.sync_all()?;
	ensure!(identity(&root_file.metadata()?) == identity(&fs::symlink_metadata(&root)?),
		"ordinary publication root changed during durable write");
	Ok(entry)
}
