//! A signed, actual-owner source cut, rechecked under the publication SQL
//! locks. Not a reusable ready flag or a second fee reservation.
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::{BlockHash, OutPoint, Transaction, TxOut, Wtxid};
use bitcoind_async_client::{Client, traits::Reader};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use prost::Message;

use crate::database::Tx;
use super::ordinary_fee_auth::FeeServiceIdentity;
use super::ordinary_fee_publication::read_record;

const MAX_ROWS: usize = 50_000;
const MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024;
const MAX_AGE_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JournalStamp {
	device: u64,
	inode: u64,
	owner: u32,
	mode: u32,
	links: u64,
	bytes: u64,
	sha256: sha256::Hash,
}
impl JournalStamp {
	pub(crate) fn read(path: &Path, owner: u32) -> anyhow::Result<Self> {
		let (_root, _) = super::ordinary_fee_publication::private_root(path.parent().context("native journal parent missing")?, owner)?;
		let before = fs::symlink_metadata(path)?;
		ensure!(before.uid() == owner && before.len() <= 2 * 1024 * 1024,
			"final fee journal owner or byte bound differs");
		let raw = read_record(path, owner)?;
		let after = fs::symlink_metadata(path)?;
		ensure!((before.dev(),before.ino(),before.len(),before.mtime(),before.mtime_nsec(),before.ctime(),before.ctime_nsec())
			== (after.dev(),after.ino(),after.len(),after.mtime(),after.mtime_nsec(),after.ctime(),after.ctime_nsec()),
			"final fee journal changed during source read");
		Ok(Self { device: before.dev(), inode: before.ino(), owner, mode: before.mode(), links: before.nlink(),
			bytes: raw.len() as u64, sha256: sha256::Hash::hash(&raw) })
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableStamp { name: String, rows: u64, bytes: u64, sha256: sha256::Hash }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalFeeSource {
	version: u8,
	candidate: Wtxid,
	genesis: BlockHash,
	policy_sha256: sha256::Hash,
	opening_sha256: sha256::Hash,
	captured_ms: u64,
	height: u32,
	block_hash: BlockHash,
	min_confirmations: u32,
	wallet_inventory_sha256: sha256::Hash,
	eligible_unreserved: BTreeMap<OutPoint, TxOut>,
	journal: JournalStamp,
	tables: Vec<TableStamp>,
}

fn now_ms() -> anyhow::Result<u64> {
	Ok(u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?)
}

// Fixed projections only. Hash the actual BYTEA in PostgreSQL17 using built-in
// sha256(bytea), not a new extension or a caller-supplied SQL expression.
// https://www.postgresql.org/docs/17/functions-binarystring.html
const SOURCES: [(&str, &str, &str); 4] = [
	("wallet_changeset", "SELECT count(*) FROM wallet_changeset",
		"SELECT id,kind::text,octet_length(content) AS bytes,encode(sha256(content),'hex') AS hash FROM wallet_changeset ORDER BY id"),
	("native_fee_publication", "SELECT count(*) FROM native_fee_publication",
		"SELECT wtxid,txid,purpose,record_hash,policy_hash,opening_hash,octet_length(signed_tx) AS bytes,encode(sha256(signed_tx),'hex') AS hash,published_at IS NOT NULL AS published FROM native_fee_publication ORDER BY wtxid"),
	("native_fee_input", "SELECT count(*) FROM native_fee_input",
		"SELECT outpoint,wtxid FROM native_fee_input ORDER BY outpoint"),
	("expiry_carrier", "SELECT count(*) FROM expiry_carrier",
		"SELECT txid,octet_length(signed_tx) AS bytes,encode(sha256(signed_tx),'hex') AS hash,backing_ids,fee_inputs,miner_fee_sat,funding_subsidy_sat,confirmed_height,confirmed_block FROM expiry_carrier ORDER BY txid"),
];

async fn source_tables(t: &Tx<'_>) -> anyhow::Result<Vec<TableStamp>> {
	ensure!(!t.query_one("SELECT EXISTS(SELECT 1 FROM wallet_changeset WHERE content IS NULL)", &[]).await?.get::<_, bool>(0),
		"native wallet source contains unknown changeset bytes");
	let mut result = Vec::new(); let mut total_bytes = 0usize;
	for (name, count_sql, projection) in SOURCES {
		let count = u64::try_from(t.query_one(count_sql, &[]).await?.get::<_, i64>(0))?;
		ensure!(count <= MAX_ROWS as u64, "native final source exceeds complete row bound");
		// Both SQL fragments originate ONLY in the closed SOURCES constant.
		// Preflight complete projected bytes BEFORE buffering any row bodies.
		let size_sql = format!("SELECT COALESCE(sum(octet_length(row_to_json(s)::text)),0)::bigint FROM ({projection}) s");
		let expected_bytes = usize::try_from(t.query_one(&size_sql, &[]).await?.get::<_, i64>(0))?;
		ensure!(total_bytes.checked_add(expected_bytes).is_some_and(|n| n <= MAX_PROJECTION_BYTES),
			"native final source projected bytes exceed bound before row read");
		let rows_sql = format!("SELECT row_to_json(s)::text FROM ({projection}) s LIMIT $1");
		let rows = t.query(&rows_sql, &[&((MAX_ROWS + 1) as i64)]).await?;
		ensure!(rows.len() as u64 == count, "native final source row count changed or truncated");
		let mut engine = sha256::Hash::engine(); engine.input(name.as_bytes());
		engine.input(&count.to_le_bytes()); let mut bytes = 0u64;
		for row in rows {
			let raw: String = row.get(0);
			total_bytes = total_bytes.checked_add(raw.len()).context("native final source byte overflow")?;
			ensure!(total_bytes <= MAX_PROJECTION_BYTES, "native final source exceeds complete byte bound");
			bytes = bytes.checked_add(raw.len() as u64).context("native table byte overflow")?;
			engine.input(&(raw.len() as u64).to_le_bytes()); engine.input(raw.as_bytes());
		}
		ensure!(bytes == expected_bytes as u64, "native final source byte count changed or truncated");
		result.push(TableStamp { name: name.into(), rows: count, bytes, sha256: sha256::Hash::from_engine(engine) });
	}
	Ok(result)
}

impl FinalFeeSource {
	/// Caller has actual Watchman wallet guard and fixed source-table/financial
	/// locks, and already checked the COMPLETE owning inventory against Core.
	pub(crate) async fn capture(t: &Tx<'_>, core: &Client, identity: &FeeServiceIdentity,
		signed: &Transaction, wallet: &server_rpc::protos::WalletInventory,
		eligible_unreserved: BTreeMap<OutPoint, TxOut>, journal: JournalStamp,
	) -> anyhow::Result<Self> {
		let before = crate::bitcoind::tip(core).await?;
		ensure!(wallet.tip_height == before.height.to_u32() && wallet.tip_hash == before.hash.to_string(),
			"final owning source is not at current Core tip");
		let tables = source_tables(t).await?;
		ensure!(crate::bitcoind::tip(core).await? == before, "final native source crossed Core tip");
		Ok(Self { version: 1, candidate: signed.compute_wtxid(), genesis: identity.genesis,
			policy_sha256: identity.policy_sha256, opening_sha256: identity.opening_sha256,
			captured_ms: now_ms()?, height: before.height.to_u32(), block_hash: before.hash,
			min_confirmations: wallet.min_trusted_confirmations.max(1),
			wallet_inventory_sha256: sha256::Hash::hash(&wallet.encode_to_vec()), eligible_unreserved, journal, tables })
	}
	#[cfg(test)]
	pub(super) fn synthetic_for_test(identity: &FeeServiceIdentity, signed: &Transaction, captured_ms: u64) -> Self {
		let stamp = JournalStamp { device: 1, inode: 2, owner: 3, mode: 0o100600, links: 1, bytes: 0,
			sha256: sha256::Hash::hash(b"synthetic journal; no live source authority") };
		Self { version: 1, candidate: signed.compute_wtxid(), genesis: identity.genesis,
			policy_sha256: identity.policy_sha256, opening_sha256: identity.opening_sha256,
			captured_ms, height: 1, block_hash: identity.genesis, min_confirmations: 3,
			wallet_inventory_sha256: sha256::Hash::hash(b"synthetic owning inventory"),
			eligible_unreserved: BTreeMap::from([(OutPoint::null(), TxOut { value: bitcoin::Amount::from_sat(40_000), script_pubkey: bitcoin::ScriptBuf::new() })]),
			journal: stamp, tables: Vec::new() }
	}
	fn check_identity(&self, identity: &FeeServiceIdentity, signed: &Transaction, now: u64) -> anyhow::Result<()> {
		ensure!(self.version == 1 && self.candidate == signed.compute_wtxid() && self.genesis == identity.genesis
			&& self.policy_sha256 == identity.policy_sha256 && self.opening_sha256 == identity.opening_sha256,
			"final source is bound to another candidate, chain, policy or opening");
		ensure!(now.checked_sub(self.captured_ms).is_some_and(|age| age <= MAX_AGE_MS),
			"final native source is stale or from the future");
		ensure!(!self.eligible_unreserved.is_empty() && self.eligible_unreserved.len() <= 2000
			&& self.min_confirmations > 0 && self.wallet_inventory_sha256 != sha256::Hash::all_zeros(),
			"final native source lacks bounded actual owning inputs");
		Ok(())
	}
	pub(crate) async fn require_final(&self, t: &Tx<'_>, core: &Client, identity: &FeeServiceIdentity,
		signed: &Transaction, record: &super::ordinary_fee_publication::PublicationRecord, history: &Path, urgent_floor: u64,
	) -> anyhow::Result<()> {
		tokio::time::timeout(std::time::Duration::from_secs(15),
			self.require_final_bounded(t, core, identity, signed, record, history, urgent_floor)).await
			.context("aggregate final source SQL/Core deadline exceeded")?
	}
	async fn require_final_bounded(&self, t: &Tx<'_>, core: &Client, identity: &FeeServiceIdentity,
		signed: &Transaction, record: &super::ordinary_fee_publication::PublicationRecord, history: &Path, urgent_floor: u64,
	) -> anyhow::Result<()> {
		self.check_identity(identity, signed, now_ms()?)?;
		let tip = crate::bitcoind::tip(core).await?;
		ensure!(tip.height.to_u32() == self.height && tip.hash == self.block_hash
			&& core.get_block_hash(0).await? == self.genesis, "final native source tip or genesis changed");
		let owner = fs::symlink_metadata(history)?.uid();
		ensure!(JournalStamp::read(&history.join(super::ordinary_fee::BUDGET_FILE), owner)? == self.journal,
			"original native fee journal changed after owner revalidation");
		ensure!(source_tables(t).await? == self.tables, "native wallet or retained publication source changed after owner revalidation");
		// Current live candidate inputs are checked independently too. Original
		// signed bytes/parents remain the exact once-debited witness; no resign.
		record.matches(signed)?;
		let mut parents = BTreeMap::new();
		for raw in record.input_parents() {
			let parent: Transaction = bitcoin::consensus::deserialize(raw)?;
			ensure!(parents.insert(parent.compute_txid(), parent).is_none(), "duplicate final candidate parent");
		}
		for input in &signed.input {
			let output = parents.get(&input.previous_output.txid).and_then(|parent| parent.output.get(input.previous_output.vout as usize))
				.context("final candidate original prevout missing")?;
			let live = core.try_get_tx_out(input.previous_output, true).await?.context("final candidate input spent or unknown")?;
			ensure!(live.value == output.value && live.script_pub_key.script()? == output.script_pubkey
				&& live.bestblock == self.block_hash && (!live.coinbase || live.confirmations >= bitcoin::constants::COINBASE_MATURITY),
				"final candidate independent current prevout differs");
		}
		let mut total = 0u64;
		for (point, output) in &self.eligible_unreserved {
			ensure!(!signed.input.iter().any(|input| input.previous_output == *point), "ordinary funding selects urgent source input");
			let current = core.try_get_tx_out(*point, true).await?.context("final urgent source spent or missing")?;
			ensure!(current.value == output.value && current.script_pub_key.script()? == output.script_pubkey
				&& current.bestblock == self.block_hash && current.confirmations >= self.min_confirmations
				&& (!current.coinbase || current.confirmations >= bitcoin::constants::COINBASE_MATURITY),
				"final urgent source value/script/confirmation eligibility changed");
			total = total.checked_add(output.value.to_sat()).context("final urgent source amount overflow")?;
		}
		ensure!(total >= urgent_floor && crate::bitcoind::tip(core).await? == tip,
			"final ordinary publication lacks original protected physical urgent floor or crossed tip");
		self.check_identity(identity, signed, now_ms()?)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::os::unix::fs::PermissionsExt;
	#[test]
	fn final_source_refuses_other_candidate_policy_opening_and_stale_or_future_cut() {
		let key = bitcoin::secp256k1::Keypair::new(&crate::SECP, &mut bitcoin::secp256k1::rand::thread_rng());
		let identity = FeeServiceIdentity { server_pubkey: key.public_key(),
			genesis: bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash(),
			policy_sha256: sha256::Hash::hash(b"synthetic policy"), opening_sha256: sha256::Hash::hash(b"synthetic opening") };
		let tx = Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO, input: Vec::new(), output: Vec::new() };
		let source = FinalFeeSource::synthetic_for_test(&identity, &tx, 40_000);
		source.check_identity(&identity, &tx, 70_000).unwrap();
		assert!(source.check_identity(&identity, &tx, 70_001).is_err());
		assert!(source.check_identity(&identity, &tx, 39_999).is_err());
		let mut other = tx.clone(); other.lock_time = bitcoin::absolute::LockTime::from_height(1).unwrap();
		assert!(source.check_identity(&identity, &other, 40_000).is_err());
		let mut foreign = identity.clone(); foreign.opening_sha256 = sha256::Hash::hash(b"another opening");
		assert!(source.check_identity(&foreign, &tx, 40_000).is_err());
		foreign = identity.clone(); foreign.policy_sha256 = sha256::Hash::hash(b"another policy");
		assert!(source.check_identity(&foreign, &tx, 40_000).is_err());
		let mut missing = source; missing.eligible_unreserved.clear();
		assert!(missing.check_identity(&identity, &tx, 40_000).is_err());
	}
	#[test]
	fn current_journal_stamp_detects_actual_append_and_refuses_nonprivate_or_linked_source() {
		let dir = std::env::temp_dir().join(format!("ark-final-source-{}-{}", std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		fs::create_dir(&dir).unwrap(); fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
		let path = dir.join("journal"); fs::write(&path, b"10000\n").unwrap();
		fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
		let owner = fs::symlink_metadata(&path).unwrap().uid();
		let original = JournalStamp::read(&path, owner).unwrap();
		fs::write(&path, b"10000\n10000\n").unwrap();
		assert_ne!(JournalStamp::read(&path, owner).unwrap(), original);
		fs::hard_link(&path, dir.join("second-link")).unwrap();
		assert!(JournalStamp::read(&path, owner).is_err()); fs::remove_file(dir.join("second-link")).unwrap();
		fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
		assert!(JournalStamp::read(&path, owner).is_err()); fs::remove_dir_all(dir).unwrap();
	}
}
