//! Expiry payout history kept with the server identity, outside PostgreSQL.
//!
//! A database-only restore must not forget a broadcast payout. Before relaying
//! one, persist its raw transaction and immutable settlements. Before starting
//! workers, require every witnessed payout to survive in the database. This
//! cannot detect restoring the database and the witness to an older point together.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bitcoin::{Network, Transaction, Txid};
use bitcoin::constants::ChainHash;
use bitcoin::consensus::serialize;
use bitcoin::secp256k1::PublicKey;

use crate::database::{Db, Tx};
use crate::fs_perms;

pub(crate) const DIRECTORY: &str = "expiry-payout-witness";

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
	// Old witnessed payouts preserve their exact bytes. New reserved payouts
	// additionally pin the immutable same-COMMIT capital effect.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	capital_releases: Option<Vec<crate::database::recovery_reserve::CapitalReleaseWitness>>,
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
		let capital_releases = self.payout_capital_witness(tx.compute_txid()).await?;
		if let Some(releases) = &capital_releases {
			ensure!(releases.len() == settlements.len(), "payout capital witness has incomplete release coverage");
		}
		Ok(Witness { tx: serialize(tx), settlements, capital_releases })
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
		if witness.capital_releases.is_none() && !path.exists() {
			let active = db.read(async |t| Ok(t.query_one(
				"SELECT EXISTS (SELECT 1 FROM recovery_reserve WHERE id = 'rounds')", &[],
			).await?.get::<_, bool>(0))).await?;
			ensure!(!active, "new payout publication requires its immutable native capital release witness");
		}
		write_new_or_identical(&path, &serde_json::to_vec(&witness)?)?;
		self.index(tx.compute_txid()).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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

#[cfg(test)]
mod capital_release_compat_test {
	use super::*;

	#[test]
	fn payout_witness_preserves_legacy_bytes_without_fabricated_release() {
		let original = br#"{"tx":[1,2],"settlements":[{"id":"coin","spk":null,"fee_sat":3}]}"#;
		let witness: Witness = serde_json::from_slice(original).unwrap();
		assert!(witness.capital_releases.is_none());
		assert_eq!(serde_json::to_vec(&witness).unwrap(), original);
	}

	#[test]
	fn payout_witness_gross_release_is_part_of_recovery_identity() {
		let original = serde_json::json!({
			"tx": [1, 2], "settlements": [{"id": "coin", "spk": null, "fee_sat": 3}],
			"capital_releases": [{"id": "coin", "source": "source:0", "receipt": "receipt",
				"gross_sat": 7000, "source_gross_sat": 10000, "source_revision": 0,
				"receipt_block_hash": "block"}]
		});
		let witness: Witness = serde_json::from_value(original.clone()).unwrap();
		let mut changed = original;
		changed["capital_releases"][0]["gross_sat"] = 6999.into();
		let restored: Witness = serde_json::from_value(changed).unwrap();
		assert_ne!(witness, restored, "same payout bytes/settlements cannot hide a changed gross release");
	}
}
