//! Original fee activation on the existing Watchman-owned journal. No second
//! allowance or fee owner is created. No publication permission follows from
//! loading this identity; every candidate still needs the owning R/F check.
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use bitcoin::{Network, Transaction};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use bitcoind_async_client::{Client, traits::Reader};

use crate::database::Db;
use super::ordinary_fee::OrdinaryFeeBudget;
use super::ordinary_fee_auth::FeeServiceIdentity;
use super::ordinary_fee_opening::verify_original;
use super::ordinary_fee_publication::{private_root, open_record, read_record};

pub(crate) struct NativeFeeEpoch { identity: FeeServiceIdentity }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFeeOpeningConfig {
	pub original_opening: PathBuf,
	pub original_opening_sha256: sha256::Hash,
	pub source_manifest: PathBuf,
	pub original_publication_logs: Vec<PathBuf>,
}

// Only the protected complete original journal constructs this replay token.
// It waives capture AGE alone; source equality and all old debits still apply.
pub(crate) struct ValidatedOpeningJournal { digest: sha256::Hash }
impl ValidatedOpeningJournal {
	pub(crate) fn matches(&self, digest: sha256::Hash) -> bool { self.digest == digest }
}
impl NativeFeeEpoch {
	pub(crate) fn identity(&self) -> &FeeServiceIdentity { &self.identity }
	pub(crate) async fn activate_config(db: &Db, core: &Client, budget: &OrdinaryFeeBudget,
		server: PublicKey, network: Network, config: &NativeFeeOpeningConfig,
	) -> anyhow::Result<Self> {
		let owner = fs::symlink_metadata(budget.directory())?.uid();
		ensure!(config.original_publication_logs.len() <= 4096, "original publication log count exceeds bound");
		let original = read_record(&config.original_opening, owner)?;
		let source = read_record(&config.source_manifest, owner)?;
		let mut bytes = original.len().checked_add(source.len()).context("original input byte overflow")?;
		let mut logs = Vec::new();
		for path in &config.original_publication_logs {
			let raw = read_record(path, owner)?;
			bytes = bytes.checked_add(raw.len()).context("original publication log sum overflow")?;
			ensure!(bytes <= 16 * 1024 * 1024, "original activation input exceeds total byte bound");
			logs.push(raw);
		}
		Self::activate(db, core, budget, server, network, &original,
			config.original_opening_sha256, &source, &logs).await
	}

	/// Partial urgent recovery is an unconditional HOLD, including setup without
	/// an opening config. Both actual startup and activation use this SAME guard.
	pub(crate) async fn require_no_unreconciled_urgent(db: &Db, budget: &OrdinaryFeeBudget) -> anyhow::Result<()> {
		budget.require_no_unreconciled_urgent()?;
		ensure!(!db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM native_urgent_attempt) OR EXISTS(SELECT 1 FROM native_urgent_input)", &[])
			.await?.get::<_, bool>(0))).await?, "retained urgent SQL source requires complete original package/input/nursery reconciliation before activation");
		Ok(())
	}

	/// Paused startup, before sharing the budget Arc or starting any worker.
	/// A failed/unknown COMMIT retains original files/debits/input intents. It
	/// never initializes a new ledger, lowers prior charges or reconstructs VTXOs.
	pub(crate) async fn activate(db: &Db, core: &Client, budget: &OrdinaryFeeBudget,
		server: PublicKey, network: Network, raw: &[u8], expected: sha256::Hash,
		source_manifest: &[u8], publication_logs: &[Vec<u8>],
	) -> anyhow::Result<Self> {
		ensure!(raw.len() <= 4 * 1024 * 1024 && sha256::Hash::hash(raw) == expected,
			"native fee activation original bytes/hash differ");
		Self::require_no_unreconciled_urgent(db,budget).await?;
		let genesis = core.get_block_hash(0).await?;
		ensure!(genesis == bitcoin::constants::genesis_block(network).block_hash(),
			"native fee activation actual Core genesis differs");
		let policy = budget.policy();
		let policy_bytes = serde_json::to_vec(&policy)?;
		let identity = FeeServiceIdentity { server_pubkey: server, genesis,
			policy_sha256: sha256::Hash::hash(&policy_bytes), opening_sha256: expected };
		let history = fs::symlink_metadata(budget.directory())?;
		ensure!(history.is_dir(), "native fee history is not a real directory");
		let owner = history.uid(); let history_identity = format!("{}:{}", history.dev(), history.ino());
		let journal = opening_root(budget.directory())?;
		let row = db.read(async |t| Ok(t.query_opt("SELECT opening_hash,policy_bytes,opening_bytes,history_identity
			FROM native_fee_opening WHERE id=1", &[]).await?)).await?;
		if let Some(row) = row {
			ensure!(row.get::<_, String>(0) == expected.to_string() && row.get::<_, Vec<u8>>(1) == policy_bytes
				&& row.get::<_, Vec<u8>>(2) == raw && row.get::<_, String>(3) == history_identity,
				"native fee opening differs from original active identity");
			check_journal(&journal, owner, raw, expected)?;
		} else {
			// No database row is NOT evidence of zero. Revalidate the exact
			// original paused source even when independent files/debits exist.
			db.write(async |t| {
				t.lock_native_fee_publication().await?;
				ensure!(t.query_opt("SELECT id FROM native_fee_opening WHERE id=1", &[]).await?.is_none(),
					"native fee opening initialized concurrently; retain originals");
				let replay = match fs::symlink_metadata(&journal) {
					Ok(_) => Some(check_journal(&journal, owner, raw, expected)?),
					Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
					Err(error) => return Err(error.into()),
				};
				let checked = verify_original(raw, expected, policy, server, network, t, core,
					source_manifest, publication_logs, replay.as_ref()).await?;
				ensure!(checked.digest() == expected && checked.canonical_bytes() == raw,
					"validated original opening bytes differ");
				write_journal(&journal, owner, raw, expected)?;
				budget.import_original(&checked)?;
				for record in checked.publications() {
					let signed = record.transaction()?;
					t.stage_native_fee_intent(record, policy, expected).await?;
					// Historical candidates were independently confirmed above;
					// exact original nursery body is required, never fabricated.
					let nursery = t.get_nursery_raw_tx(signed.compute_txid()).await?
						.context("original imported publication lost its nursery body")?;
					ensure!(nursery == signed, "original imported nursery candidate differs");
					t.publish_native_fee_intent(record, policy, expected, &signed).await?;
				}
				t.execute("INSERT INTO native_fee_opening(id,opening_hash,policy_bytes,opening_bytes,history_identity)
					VALUES(1,$1,$2,$3,$4)", &[&expected.to_string(),&policy_bytes,&raw,&history_identity]).await?;
				Ok(())
			}).await.context("native fee opening commit uncertain; preserve original certificate/history")?;
		}
		// Includes unused exact permits, without turning them into broadcast
		// transactions or allowing a DB-only restore to erase their input holds.
		let records = budget.publication_records()?;
		db.write(async |t| {
			t.lock_native_fee_publication().await?;
			let retained = t.retained_native_fee_inputs().await?;
			ensure!(retained.len() == records.len(), "native fee database/history candidate sets differ");
			for record in records.values() {
				let tx: Transaction = record.transaction()?;
				t.require_native_fee_intent(record, policy, expected, &tx).await?;
				budget.require_publication(record, &tx)?;
			}
			Ok(())
		}).await?;
		// Only this checked original activation may initialize a genuinely empty
		// Round-rights namespace. Booked or partially written history stays HOLD.
		db.write(async |t| {
			t.lock_native_fee_publication().await?;
			super::ordinary_round_rights::initialize_original(t, budget.directory(), policy, &identity).await
		}).await?;
		super::ordinary_round_rights::validate_startup(db, budget.directory(), policy,
			server, genesis, Some(expected)).await?;
		let epoch = Self { identity };
		budget.bind_native_epoch(&epoch)?;
		Ok(epoch)
	}
}

fn opening_root(history: &Path) -> anyhow::Result<PathBuf> {
	let name = history.file_name().and_then(|name| name.to_str()).context("native opening history path invalid")?;
	Ok(history.with_file_name(format!("{name}.ordinary-native-opening")))
}
fn check_journal(root: &Path, owner: u32, raw: &[u8], digest: sha256::Hash) -> anyhow::Result<ValidatedOpeningJournal> {
	ensure!(sha256::Hash::hash(raw) == digest, "native opening journal content hash differs");
	let (directory, before) = private_root(root, owner)?;
	let names = fs::read_dir(root)?.map(|item| Ok(item?.file_name())).collect::<anyhow::Result<Vec<_>>>()?;
	ensure!(names.len() == 2 && names.iter().all(|name| name == "certificate.json" || name == "validated.sha256"),
		"native opening journal has unknown or missing evidence");
	ensure!(read_record(&root.join("certificate.json"), owner)? == raw
		&& read_record(&root.join("validated.sha256"), owner)? == digest.to_string().as_bytes(),
		"native original opening journal differs");
	let after = fs::symlink_metadata(root)?; let opened = directory.metadata()?;
	ensure!((before.dev(),before.ino(),before.mtime(),before.mtime_nsec(),before.ctime(),before.ctime_nsec())
		== (after.dev(),after.ino(),after.mtime(),after.mtime_nsec(),after.ctime(),after.ctime_nsec())
		&& (opened.dev(),opened.ino()) == (before.dev(),before.ino()), "native opening directory changed during read");
	Ok(ValidatedOpeningJournal { digest })
}
fn write_original(path: &Path, owner: u32, raw: &[u8]) -> anyhow::Result<()> {
	match fs::symlink_metadata(path) {
		Ok(_) => ensure!(read_record(path, owner)? == raw, "conflicting original native opening evidence"),
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			crate::fs_perms::create_new_owner_only(path, raw)?;
		},
		Err(error) => return Err(error.into()),
	}
	let (file, _) = open_record(path, owner)?; file.sync_all()?;
	Ok(())
}
fn write_journal(root: &Path, owner: u32, raw: &[u8], digest: sha256::Hash) -> anyhow::Result<()> {
	match fs::symlink_metadata(root) {
		Ok(_) => {},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			fs::DirBuilder::new().mode(0o700).create(root)?;
			File::open(root.parent().context("native opening parent missing")?)?.sync_all()?;
		},
		Err(error) => return Err(error.into()),
	}
	let (directory, _) = private_root(root, owner)?;
	write_original(&root.join("certificate.json"), owner, raw)?;
	write_original(&root.join("validated.sha256"), owner, digest.to_string().as_bytes())?;
	directory.sync_all()?;
	check_journal(root, owner, raw, digest)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn protected_opening_journal_replays_exact_original_and_rejects_changed_partial_or_linked_evidence() {
		let base = std::env::temp_dir().join(format!("native-opening-journal-{}-{}", std::process::id(),
			bitcoin::secp256k1::rand::random::<u64>()));
		fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
		let owner = fs::symlink_metadata(&base).unwrap().uid();
		let root = base.join("journal"); let raw = b"synthetic exact original document, not admission";
		let digest = sha256::Hash::hash(raw);
		write_journal(&root, owner, raw, digest).unwrap();
		assert!(check_journal(&root, owner, raw, digest).unwrap().matches(digest));
		write_journal(&root, owner, raw, digest).unwrap();
		assert!(check_journal(&root, owner, b"changed", digest).is_err());
		fs::write(root.join("extra"), b"unexpected").unwrap();
		assert!(check_journal(&root, owner, raw, digest).is_err());
		fs::remove_file(root.join("extra")).unwrap();
		fs::remove_file(root.join("validated.sha256")).unwrap();
		assert!(check_journal(&root, owner, raw, digest).is_err());
		std::os::unix::fs::symlink(root.join("certificate.json"), root.join("validated.sha256")).unwrap();
		assert!(check_journal(&root, owner, raw, digest).is_err());
		fs::remove_dir_all(base).unwrap();
	}
}
