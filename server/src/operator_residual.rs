//! Release an exhaustively proved, untouched original operator pool source.
//!
//! The independent certificate becomes durable while the source is still held.
//! Its exact native retirement and capital effect then commit together. An
//! incomplete or conflicting intent refuses recovery rather than guessing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::os::unix::fs::MetadataExt;

use anyhow::Context;
use ark::{ProtocolEncoding, ServerVtxo, Vtxo, VtxoId};
use ark::vtxo::{Full, VtxoPolicyKind};
use bitcoin::{Amount, Network, OutPoint, Transaction, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use tokio_postgres::Row;
use tracing::warn;
use bitcoind_async_client::Client as BitcoindClient;

use crate::database::{Db, Tx};
use crate::{bitcoind, fs_perms, Server};

pub(crate) const DIRECTORY: &str = "operator-recovery-witness";
const MAX_LEAVES: usize = 256;
const MAX_TREE_TRANSACTIONS: usize = 1_024;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CERTIFICATE_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_RECORDS: usize = 100_000;
const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
const SOURCES_PER_TICK: i64 = 4;

fn digest(bytes: &[u8]) -> String { sha256::Hash::hash(bytes).to_string() }

fn public_body(tx: &Transaction) -> anyhow::Result<Vec<u8>> {
	// This first source kind only supports native witness-only funding/sweeps.
	// Never copy an arbitrary scriptSig or an unlock witness into the journal.
	let mut body = tx.clone();
	for input in &mut body.input {
		ensure!(input.script_sig.is_empty(), "operator source has an unsupported scriptSig");
		input.witness.clear();
	}
	Ok(serialize(&body))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
	version: u32,
	genesis: String,
	server_pubkey: String,
}

fn identity(network: Network, server: PublicKey) -> Identity {
	Identity { version: 1, genesis: ChainHash::using_genesis_block(network).to_string(),
		server_pubkey: server.to_string() }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Leaf {
	id: String,
	amount_sat: u64,
	expiry: u32,
	pool_hash: String,
	native_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Certificate {
	identity: Identity,
	source: String,
	receipt: String,
	receipt_height: u32,
	receipt_block: String,
	revision: i64,
	gross_sat: u64,
	sweep_script: Vec<u8>,
	funding_body: Vec<u8>,
	funding_hash: String,
	receipt_body: Vec<u8>,
	receipt_hash: String,
	leaves: Vec<Leaf>,
	tree_hashes: BTreeMap<String, String>,
}

impl Certificate {
	fn id(&self) -> String {
		// Independent of the mutable source revision and of JSON field ordering.
		digest(format!("unused-pool\n{}\n{}\n{}\n{}\n", self.identity.genesis,
			self.identity.server_pubkey, self.source, self.receipt).as_bytes())
	}

	fn ledger_id(&self) -> String { format!("unused-pool:{}", self.id()) }

	fn bytes(&self) -> anyhow::Result<Vec<u8>> {
		let bytes = serde_json::to_vec(self)?;
		ensure!(bytes.len() <= MAX_CERTIFICATE_BYTES, "operator certificate budget exceeded");
		Ok(bytes)
	}

	fn ids(&self) -> anyhow::Result<Vec<VtxoId>> {
		self.leaves.iter().map(|leaf| leaf.id.parse().map_err(Into::into)).collect()
	}
}

struct Source {
	point: OutPoint,
	receipt: Transaction,
	height: u32,
	block: String,
	revision: i64,
	gross: Amount,
	script: Vec<u8>,
}

impl Source {
	fn from_row(row: &Row) -> anyhow::Result<Self> {
		let receipt = row.get::<_, Vec<u8>>("transaction_bytes");
		ensure!(receipt.len() <= MAX_SOURCE_BYTES, "operator receipt budget exceeded");
		Ok(Self {
			point: row.get::<_, String>("id").parse()?, receipt: deserialize(&receipt)?,
			height: u32::try_from(row.get::<_, i64>("block_height"))?,
			block: row.get("block_hash"), revision: row.get("revision"),
			gross: Amount::from_sat(u64::try_from(row.get::<_, i64>("gross_sat"))?),
			script: row.get("sweep_spk"),
		})
	}
}

/// Check the actual signed graph, including every positive terminal output.
/// No amount, branch count or ownership assertion comes from a caller.
fn prove_tree(
	source: &Source,
	funding: &Transaction,
	rows: &[Row],
	server: PublicKey,
) -> anyhow::Result<(Vec<Leaf>, BTreeMap<String, String>)> {
	ensure!(!rows.is_empty() && rows.len() <= MAX_LEAVES, "original pool membership incomplete or over budget");
	ensure!(funding.compute_txid() == source.point.txid, "operator funding identity differs");
	let backing = funding.output.get(source.point.vout as usize).context("operator source output absent")?;
	ensure!(backing.value == source.gross && source.gross > Amount::ZERO, "operator source gross differs");
	let mut leaves = BTreeMap::<OutPoint, TxOut>::new();
	let mut tree = BTreeMap::<Txid, Transaction>::new();
	let mut commitments = Vec::with_capacity(rows.len());
	let mut bytes = 0usize;
	for row in rows {
		let raw: Vec<u8> = row.get("pool_vtxo");
		let native: Vec<u8> = row.get("native_vtxo");
		bytes = bytes.checked_add(raw.len()).and_then(|n| n.checked_add(native.len()))
			.context("operator source byte count overflow")?;
		ensure!(bytes <= MAX_SOURCE_BYTES, "operator source byte budget exceeded");
		let coin: Vtxo = Vtxo::deserialize(&raw).context("invalid original pool coin")?;
		ensure!(coin.policy_type() == VtxoPolicyKind::Pubkey && coin.past_arkoor_pubkeys().is_empty(),
			"operator source is not an untouched original pubkey leaf");
		ensure!(coin.server_pubkey() == server && coin.chain_anchor() == source.point
			&& coin.expiry_height().to_u32() <= source.height && coin.amount() > Amount::ZERO,
			"operator leaf identity or expiry differs");
		coin.validate(funding).context("original pool signature or funding path differs")?;
		let original = ServerVtxo::<Full>::deserialize(&native).context("invalid native original pool leaf")?;
		original.validate_unsigned(funding).context("native original pool commitments differ")?;
		ensure!(row.get::<_, String>("vtxo_id") == coin.id().to_string()
			&& row.get::<_, i64>("pool_amount") == i64::try_from(coin.amount().to_sat())?
			&& row.get::<_, i32>("pool_expiry") == i32::try_from(coin.expiry_height().to_u32())?
			&& row.get::<_, i64>("native_amount") == i64::try_from(coin.amount().to_sat())?
			&& row.get::<_, i32>("native_expiry") == i32::try_from(coin.expiry_height().to_u32())?
			&& row.get::<_, String>("server_pubkey") == server.to_string()
			&& row.get::<_, String>("policy_type") == "pubkey"
			&& ServerVtxo::from(coin.clone()).is_final_hark_leaf_completion_of(&original),
			"native and full operator leaf records differ");
		ensure!(leaves.insert(coin.point(), coin.txout()).is_none(), "duplicate operator leaf");
		for item in coin.transactions() {
			let txid = item.tx.compute_txid();
			if let Some(previous) = tree.insert(txid, item.tx.clone()) {
				ensure!(previous == item.tx, "conflicting original pool path bytes");
			}
			ensure!(tree.len() <= MAX_TREE_TRANSACTIONS, "operator path budget exceeded");
		}
		commitments.push(Leaf { id: coin.id().to_string(), amount_sat: coin.amount().to_sat(),
			expiry: coin.expiry_height().to_u32(), pool_hash: digest(&raw), native_hash: digest(&native) });
	}
	let mut spenders = BTreeMap::new();
	for (txid, tx) in &tree {
		ensure!(tx.input.len() == 1 && spenders.insert(tx.input[0].previous_output, *txid).is_none(),
			"operator graph is not a single-parent tree");
	}
	let mut queue = VecDeque::from([(source.point, backing.clone())]);
	let mut visited = BTreeSet::new();
	let mut terminal = BTreeSet::new();
	let mut total = Amount::ZERO;
	while let Some((point, output)) = queue.pop_front() {
		if let Some(txid) = spenders.get(&point) {
			ensure!(!leaves.contains_key(&point) && visited.insert(*txid), "operator leaf consumed or graph cycles");
			let tx = tree.get(txid).context("operator tree edge absent")?;
			let sum = tx.output.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
				.context("operator tree value overflow")?;
			ensure!(sum == output.value, "operator tree does not conserve its source");
			for (index, output) in tx.output.iter().enumerate() {
				queue.push_back((OutPoint::new(*txid, u32::try_from(index)?), output.clone()));
			}
		} else if let Some(leaf) = leaves.get(&point) {
			ensure!(leaf == &output && terminal.insert(point), "operator terminal leaf differs");
			total = total.checked_add(output.value).context("operator terminal amount overflow")?;
		} else {
			ensure!(output.value == Amount::ZERO && output.script_pubkey == bitcoin::ScriptBuf::new_p2a(),
				"operator tree has an unclassified terminal output");
		}
	}
	ensure!(visited.len() == tree.len() && terminal.len() == leaves.len() && total == source.gross,
		"operator tree coverage is incomplete");
	commitments.sort_by(|a, b| a.id.cmp(&b.id));
	let hashes = tree.into_iter().map(|(id, tx)| (id.to_string(), digest(&serialize(&tx)))).collect();
	Ok((commitments, hashes))
}

impl Tx<'_> {
	async fn operator_source(&self, source: &str) -> anyhow::Result<Source> {
		let row = self.query_opt("
			SELECT h.id, h.gross_sat, h.revision, r.transaction_bytes, r.block_height,
				r.block_hash, s.sweep_spk FROM recovery_source_hold h
			JOIN recovery_receipt r ON r.id = h.receipt_id
			CROSS JOIN recovery_reserve s
			WHERE h.id = $1 AND r.proof_state = 'physical_bound' AND s.id = 'rounds'
		", &[&source]).await?.context("operator source has no physical receipt")?;
		Source::from_row(&row)
	}

	async fn original_pool_rows(&self, source: &str) -> anyhow::Result<Vec<Row>> {
		self.query("
			SELECT p.vtxo_id, p.vtxo AS pool_vtxo, p.amount AS pool_amount,
				p.expiry_height AS pool_expiry, p.spent_at IS NOT NULL AS pool_spent,
				v.vtxo AS native_vtxo, v.amount AS native_amount, v.expiry AS native_expiry,
				v.server_pubkey, v.policy_type, v.spend_state::text AS state,
				v.oor_spent_txid, v.spent_in_round, v.offboarded_in, v.confirmed_height,
				v.onchain_spent_txid, v.onchain_spent_height, v.lightning_htlc_subscription_id,
				EXISTS (SELECT 1 FROM htlc_vtxo x WHERE x.id = v.id) AS has_htlc,
				EXISTS (SELECT 1 FROM expiry_settlement x WHERE x.id = v.vtxo_id) AS settled,
				EXISTS (SELECT 1 FROM pending_board x WHERE x.vtxo_id = v.vtxo_id) AS pending
			FROM vtxo_pool p JOIN vtxo v ON v.vtxo_id = p.vtxo_id
			WHERE v.anchor_point = $1 ORDER BY p.vtxo_id LIMIT $2
		", &[&source, &i64::try_from(MAX_LEAVES + 1)?]).await.map_err(Into::into)
	}

	async fn operator_funding(&self, source: OutPoint) -> anyhow::Result<Transaction> {
		let row = self.query_opt("SELECT tx, kind::text FROM nursery_tx WHERE txid = $1",
			&[&source.txid.to_string()]).await?.context("original pool funding provenance missing")?;
		ensure!(row.get::<_, String>(1) == crate::nursery::NurseryTxKind::VtxoPool.name(),
			"source was not issued by the native operator pool");
		let bytes: Vec<u8> = row.get(0);
		ensure!(bytes.len() <= MAX_SOURCE_BYTES, "operator funding budget exceeded");
		Ok(deserialize(&bytes)?)
	}
}

async fn certificate(
	db: &Db,
	client: &BitcoindClient,
	network: Network,
	server: PublicKey,
	source_id: &str,
	expected_revision: Option<i64>,
	minimum_confirmations: u32,
) -> anyhow::Result<Certificate> {
	let (source, rows, funding) = db.read(async |t| {
		let source = t.operator_source(source_id).await?;
		let rows = t.original_pool_rows(source_id).await?;
		let funding = t.operator_funding(source.point).await?;
		Ok((source, rows, funding))
	}).await?;
	let revision = expected_revision.unwrap_or(source.revision);
	ensure!(revision >= 0 && (source.revision == revision
		|| revision.checked_add(1) == Some(source.revision)), "operator source revision changed");
	ensure!(source.receipt.input.len() == 1 && source.receipt.input[0].previous_output == source.point,
		"operator relief only supports a single original source receipt");
	ensure!(source.receipt.output.iter().all(|o| o.script_pubkey.as_bytes() == source.script
		|| (o.value == Amount::ZERO && o.script_pubkey == bitcoin::ScriptBuf::new_p2a())),
		"operator receipt has another destination");
	let amount = source.receipt.output.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
		.context("operator receipt output overflow")?;
	ensure!(amount > Amount::ZERO && amount < source.gross, "operator receipt fee or value differs");
	let actual_funding = bitcoind::custom_get_raw_transaction_info(client, source.point.txid, None).await?
		.context("operator funding unavailable from Core")?;
	ensure!(deserialize::<Transaction>(&actual_funding.hex)? == funding
		&& actual_funding.confirmations.unwrap_or(0) > 0, "operator funding is not the confirmed original");
	let actual_receipt = bitcoind::custom_get_raw_transaction_info(client, source.receipt.compute_txid(), None).await?
		.context("operator receipt unavailable from Core")?;
	ensure!(deserialize::<Transaction>(&actual_receipt.hex)? == source.receipt
		&& actual_receipt.confirmations.unwrap_or(0) >= minimum_confirmations
		&& actual_receipt.blockhash.map(|h| h.to_string()).as_ref() == Some(&source.block),
		"operator receipt bytes, depth or block differ");
	let block: String = client.call_raw("getblockhash", &[source.height.into()]).await?;
	ensure!(block == source.block, "operator receipt left the recorded chain");
	let (leaves, tree_hashes) = prove_tree(&source, &funding, &rows, server)?;
	let certificate = Certificate {
		identity: identity(network, server), source: source_id.to_owned(),
		receipt: source.receipt.compute_txid().to_string(), receipt_height: source.height,
		receipt_block: source.block, revision, gross_sat: source.gross.to_sat(), sweep_script: source.script,
		funding_body: public_body(&funding)?, funding_hash: digest(&serialize(&funding)),
		receipt_body: public_body(&source.receipt)?, receipt_hash: digest(&serialize(&source.receipt)),
		leaves, tree_hashes,
	};
	certificate.bytes()?;
	Ok(certificate)
}

/// A fresh native parent must be locked before any child upsert. This order is
/// shared by pool sends and operator retirement, including separate processes.
impl Tx<'_> {
	pub(crate) async fn lock_original_pool_inputs(&self, ids: &[VtxoId]) -> anyhow::Result<()> {
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let native = self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id = ANY($1) ORDER BY vtxo_id FOR UPDATE",
			&[&ids]).await?;
		let pool = self.query("SELECT vtxo_id FROM vtxo_pool WHERE vtxo_id = ANY($1) ORDER BY vtxo_id FOR UPDATE",
			&[&ids]).await?;
		ensure!(native.len() == ids.len() && pool.len() == ids.len(), "native pool input membership changed");
		Ok(())
	}

	async fn operator_disposition_state(&self, proof: &Certificate) -> anyhow::Result<bool> {
		let ids = proof.ids()?;
		self.lock_original_pool_inputs(&ids).await?;
		let hold = self.query_one("
			SELECT receipt_id, gross_sat, held_sat, revision FROM recovery_source_hold
			WHERE id = $1 FOR UPDATE
		", &[&proof.source]).await?;
		ensure!(hold.get::<_, String>("receipt_id") == proof.receipt
			&& hold.get::<_, i64>("gross_sat") == i64::try_from(proof.gross_sat)?, "operator physical hold differs");
		let recorded = self.query_opt("
			SELECT kind, source_id, receipt_id, gross_sat, source_revision, operator_proof
			FROM recovery_capital_release WHERE id = $1
		", &[&proof.ledger_id()]).await?;
		let committed = if let Some(row) = recorded {
			ensure!(row.get::<_, String>("kind") == "unused-pool"
				&& row.get::<_, String>("source_id") == proof.source
				&& row.get::<_, String>("receipt_id") == proof.receipt
				&& row.get::<_, i64>("gross_sat") == i64::try_from(proof.gross_sat)?
				&& row.get::<_, i64>("source_revision") == proof.revision
				&& row.get::<_, Vec<u8>>("operator_proof") == proof.bytes()?
				&& hold.get::<_, i64>("held_sat") == 0
				&& Some(hold.get::<_, i64>("revision")) == proof.revision.checked_add(1),
				"operator disposition identity or revision differs");
			true
		} else {
			ensure!(hold.get::<_, i64>("held_sat") == i64::try_from(proof.gross_sat)?
				&& hold.get::<_, i64>("revision") == proof.revision, "operator source has a prior disposition");
			false
		};
		let other: bool = self.query_one("SELECT EXISTS (SELECT 1 FROM recovery_capital_release WHERE source_id = $1 AND id <> $2)",
			&[&proof.source, &proof.ledger_id()]).await?.get(0);
		ensure!(!other, "operator source has another capital release");
		let rows = self.original_pool_rows(&proof.source).await?;
		ensure!(rows.len() == proof.leaves.len(), "operator source membership changed");
		for (row, leaf) in rows.iter().zip(&proof.leaves) {
			ensure!(row.get::<_, String>("vtxo_id") == leaf.id
				&& digest(&row.get::<_, Vec<u8>>("pool_vtxo")) == leaf.pool_hash
				&& digest(&row.get::<_, Vec<u8>>("native_vtxo")) == leaf.native_hash
				&& row.get::<_, i64>("native_amount") == i64::try_from(leaf.amount_sat)?
				&& row.get::<_, i64>("pool_amount") == i64::try_from(leaf.amount_sat)?
				&& row.get::<_, i32>("native_expiry") == i32::try_from(leaf.expiry)?
				&& row.get::<_, i32>("pool_expiry") == i32::try_from(leaf.expiry)?
				&& row.get::<_, String>("server_pubkey") == proof.identity.server_pubkey
				&& row.get::<_, String>("policy_type") == "pubkey"
				&& row.get::<_, String>("state") == (if committed { "spent" } else { "pool" })
				&& row.get::<_, bool>("pool_spent") == committed
				&& row.get::<_, Option<String>>("oor_spent_txid").is_none()
				&& row.get::<_, Option<i64>>("spent_in_round").is_none()
				&& row.get::<_, Option<String>>("offboarded_in").is_none()
				&& row.get::<_, Option<i32>>("confirmed_height").is_none()
				&& row.get::<_, Option<String>>("onchain_spent_txid").is_none()
				&& row.get::<_, Option<i32>>("onchain_spent_height").is_none()
				&& row.get::<_, Option<i64>>("lightning_htlc_subscription_id").is_none()
				&& !row.get::<_, bool>("has_htlc") && !row.get::<_, bool>("settled")
				&& !row.get::<_, bool>("pending"), "original operator leaf is missing, consumed or changed");
		}
		// Receipt rows are immutable, but recheck the exact binding after waiting.
		let source = self.operator_source(&proof.source).await?;
		ensure!(source.receipt.compute_txid().to_string() == proof.receipt
			&& digest(&serialize(&source.receipt)) == proof.receipt_hash
			&& source.height == proof.receipt_height && source.block == proof.receipt_block
			&& source.script == proof.sweep_script, "operator recovery receipt changed");
		Ok(committed)
	}

	async fn retire_original_pool(&self, proof: &Certificate) -> anyhow::Result<()> {
		let ids = proof.leaves.iter().map(|leaf| leaf.id.clone()).collect::<Vec<_>>();
		let native = self.execute("
			UPDATE vtxo SET spend_state = 'spent', updated_at = NOW()
			WHERE vtxo_id = ANY($1) AND spend_state = 'pool' AND confirmed_height IS NULL
				AND oor_spent_txid IS NULL AND spent_in_round IS NULL AND offboarded_in IS NULL
		", &[&ids]).await?;
		let pool = self.execute("UPDATE vtxo_pool SET spent_at = NOW() WHERE vtxo_id = ANY($1) AND spent_at IS NULL",
			&[&ids]).await?;
		ensure!(native == ids.len() as u64 && pool == ids.len() as u64, "operator retirement cardinality differs");
		let gross = i64::try_from(proof.gross_sat)?;
		let held = self.execute("
			UPDATE recovery_source_hold SET held_sat = 0, revision = revision + 1
			WHERE id = $1 AND receipt_id = $2 AND gross_sat = $3 AND held_sat = $3 AND revision = $4
		", &[&proof.source, &proof.receipt, &gross, &proof.revision]).await?;
		ensure!(held == 1, "operator source changed before retirement");
		let row = self.query_one("
			INSERT INTO recovery_capital_release (id, payout_txid, source_id, receipt_id,
				gross_sat, source_revision, kind, operator_proof)
			VALUES ($1, NULL, $2, $3, $4, $5, 'unused-pool', $6) RETURNING id
		", &[&proof.ledger_id(), &proof.source, &proof.receipt, &gross, &proof.revision, &proof.bytes()?]).await?;
		ensure!(row.get::<_, String>(0) == proof.ledger_id(), "operator disposition record differs");
		Ok(())
	}
}

/// Bounded private records use no unlock witness, seed or customer settlement.
/// Refuse a missing/corrupt journal; never infer a past release from SQL alone.
#[derive(Clone)]
pub(crate) struct OperatorWitness {
	directory: PathBuf,
	writer: Arc<tokio::sync::Mutex<()>>,
	cursor: Arc<tokio::sync::Mutex<String>>,
}

pub(crate) fn read_bounded(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
	let before = fs::symlink_metadata(path)?;
	ensure!(before.is_file() && before.nlink() == 1 && before.len() <= limit as u64,
		"operator witness is not a bounded regular file");
	let file = File::open(path)?;
	let opened = file.metadata()?;
	ensure!(opened.dev() == before.dev() && opened.ino() == before.ino(), "operator witness changed while opening");
	let mut bytes = Vec::new();
	file.take(u64::try_from(limit)?.checked_add(1).context("operator file limit overflow")?)
		.read_to_end(&mut bytes)?;
	let after = fs::symlink_metadata(path)?;
	ensure!(bytes.len() <= limit && after.dev() == opened.dev() && after.ino() == opened.ino()
		&& after.len() == bytes.len() as u64 && after.mtime_nsec() == opened.mtime_nsec()
		&& after.mtime() == opened.mtime(), "operator witness changed during read");
	Ok(bytes)
}

pub(crate) fn write_identical(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
	match read_bounded(path, bytes.len()) {
		Ok(previous) => ensure!(previous == bytes, "conflicting operator capital witness"),
		Err(error) if error.downcast_ref::<std::io::Error>()
			.is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {
			fs_perms::write_atomic_owner_only(path, bytes)?;
		},
		Err(error) => return Err(error),
	}
	File::open(path.parent().context("operator witness has no parent")?)?.sync_all()?;
	Ok(())
}

impl OperatorWitness {
	pub(crate) fn initialize(datadir: &Path, network: Network, key: PublicKey) -> anyhow::Result<()> {
		let directory = datadir.join(DIRECTORY);
		fs::create_dir(&directory)?;
		fs_perms::harden(&directory, 0o700)?;
		write_identical(&directory.join("identity.json"), &serde_json::to_vec(&identity(network, key))?)?;
		write_identical(&directory.join("history"), b"")?;
		File::open(datadir)?.sync_all()?;
		Ok(())
	}

	fn indexed(&self) -> anyhow::Result<BTreeSet<String>> {
		let history = read_bounded(&self.directory.join("history"), MAX_HISTORY_RECORDS * 65)?;
		ensure!(history.is_empty() || history.last() == Some(&b'\n'), "torn operator witness index");
		let text = std::str::from_utf8(&history)?;
		let mut ids = BTreeSet::new();
		for line in text.lines() {
			let hash = sha256::Hash::from_str(line)?;
			ensure!(hash.to_string() == line && ids.insert(line.to_owned()), "invalid or duplicate operator witness index");
		}
		Ok(ids)
	}

	// The SQL journal-append lock serializes different server processes as well
	// as the local mutex. Held capital is unchanged until BOTH fsyncs succeed.
	async fn record_locked(&self, proof: &Certificate) -> anyhow::Result<()> {
		let _writer = self.writer.lock().await;
		let mut indexed = self.indexed()?;
		let mut total_bytes = 0usize;
		let record = format!("{}.json", proof.id());
		for entry in fs::read_dir(&self.directory)? {
			let entry = entry?;
			let name = entry.file_name();
			let name = name.to_str().context("invalid operator record filename")?;
			if name.ends_with(".json") && name != "identity.json" && name != record {
				let metadata = fs::symlink_metadata(entry.path())?;
				ensure!(metadata.is_file() && metadata.nlink() == 1, "operator journal entry is not a regular file");
				total_bytes = total_bytes.checked_add(usize::try_from(metadata.len())?).context("operator journal size overflow")?;
			}
		}
		ensure!(total_bytes.checked_add(proof.bytes()?.len()).is_some_and(|n| n <= MAX_JOURNAL_BYTES),
			"operator witness publication byte budget exceeded");
		ensure!(indexed.len() < MAX_HISTORY_RECORDS || indexed.contains(&proof.id()), "operator witness index budget exceeded");
		write_identical(&self.directory.join(format!("{}.json", proof.id())), &proof.bytes()?)?;
		if indexed.insert(proof.id()) {
			let path = self.directory.join("history");
			let mut history = OpenOptions::new().append(true).open(&path)?;
			ensure!(history.metadata()?.len() == (indexed.len() - 1) as u64 * 65, "operator witness index changed");
			writeln!(history, "{}", proof.id())?;
			history.sync_all()?;
		}
		Ok(())
	}

	async fn apply(&self, db: &Db, proof: &Certificate) -> anyhow::Result<()> {
		let result = db.write(async |t| {
			// The append lock uses the existing advisory-lock namespace.
			// No Core call or unbounded IO occurs under these financial locks.
			t.lock_operator_witness().await?;
			let committed = t.operator_disposition_state(proof).await?;
			self.record_locked(proof).await?;
			if !committed { t.retire_original_pool(proof).await?; }
			Ok(())
		}).await;
		if let Err(error) = result {
			// Wait for the same financial locks after ambiguous COMMIT. A local
			// error/timeout is never permission to declare capital available.
			let committed = db.write(async |t| {
				t.lock_operator_witness().await?;
				t.operator_disposition_state(proof).await
			}).await.context("unable to resolve operator disposition commit")?;
			ensure!(committed, "operator disposition did not commit: {error:#}");
		}
		Ok(())
	}

	pub(crate) async fn load(
		datadir: &Path, network: Network, key: PublicKey, db: &Db,
		client: &BitcoindClient, minimum_confirmations: u32,
	) -> anyhow::Result<Self> {
		let directory = datadir.join(DIRECTORY);
		ensure!(fs::symlink_metadata(&directory)?.is_dir(), "operator witness directory missing or not a directory");
		let stored: Identity = serde_json::from_slice(&read_bounded(&directory.join("identity.json"), 4096)?)?;
		ensure!(stored == identity(network, key), "operator witness identity differs");
		let witness = Self { directory, writer: Arc::new(tokio::sync::Mutex::new(())),
			cursor: Arc::new(tokio::sync::Mutex::new(String::new())) };
		let indexed = witness.indexed()?;
		let mut present = BTreeMap::new();
		let mut journal_bytes = 0usize;
		for entry in fs::read_dir(&witness.directory)? {
			let path = entry?.path();
			let name = path.file_name().and_then(|n| n.to_str()).context("invalid operator witness filename")?;
			if name == "identity.json" || name == "history" || (name.starts_with('.') && name.ends_with(".tmp")) { continue; }
			ensure!(present.len() < MAX_HISTORY_RECORDS, "operator witness record budget exceeded");
			let id = name.strip_suffix(".json").context("invalid operator witness filename")?;
			let bytes = read_bounded(&path, MAX_CERTIFICATE_BYTES)?;
			journal_bytes = journal_bytes.checked_add(bytes.len()).context("operator journal size overflow")?;
			ensure!(journal_bytes <= MAX_JOURNAL_BYTES, "operator witness startup byte budget exceeded");
			let proof: Certificate = serde_json::from_slice(&bytes)?;
			ensure!(proof.identity == stored && proof.id() == id && proof.bytes()? == bytes,
				"operator witness record identity or canonical bytes differ");
			ensure!(present.insert(id.to_owned(), proof).is_none(), "duplicate operator witness record");
		}
		ensure!(indexed.iter().all(|id| present.contains_key(id)), "indexed operator witness is missing");
		// Missing non-indexed files must also refuse: SQL is never its own proof
		// that an independently durable operator release existed.
		let ledger = db.read(async |t| Ok(t.query(
			"SELECT id, operator_proof FROM recovery_capital_release WHERE kind = 'unused-pool' ORDER BY id LIMIT $1",
			&[&i64::try_from(MAX_HISTORY_RECORDS + 1)?],
		).await?)).await?;
		ensure!(ledger.len() <= MAX_HISTORY_RECORDS, "operator release ledger budget exceeded");
		for row in ledger {
			let id: String = row.get(0);
			let proof = present.get(id.strip_prefix("unused-pool:").context("invalid operator release id")?)
				.context("operator release has no independent witness")?;
			ensure!(row.get::<_, Vec<u8>>(1) == proof.bytes()?, "operator ledger witness differs");
		}
		for proof in present.values() {
			let current = certificate(db, client, network, key, &proof.source,
				Some(proof.revision), minimum_confirmations).await?;
			ensure!(current == *proof, "operator witness differs from restored source or chain");
			// Exact pending intents replay under the original native eligibility
			// guards. A consumed/missing leaf or older DB snapshot refuses startup.
			witness.apply(db, proof).await?;
		}
		Ok(witness)
	}
}

impl Server {
	pub(crate) async fn close_unused_operator_pool(&self) -> anyhow::Result<()> {
		let Some(witness) = &self.operator_witness else { return Ok(()); };
		let mut cursor = witness.cursor.lock().await;
		let sources: Vec<String> = self.db.read(async |t| Ok(t.query(
			"SELECT h.id FROM recovery_source_hold h JOIN recovery_receipt r ON r.id = h.receipt_id
			 WHERE h.held_sat = h.gross_sat AND h.held_sat > 0 AND r.proof_state = 'physical_bound' AND h.id > $2
			 AND EXISTS (SELECT 1 FROM vtxo v JOIN vtxo_pool p ON p.vtxo_id = v.vtxo_id WHERE v.anchor_point = h.id)
			 ORDER BY h.id LIMIT $1", &[&SOURCES_PER_TICK, &*cursor],
		).await?.into_iter().map(|row| row.get(0)).collect())).await?;
		*cursor = sources.last().cloned().unwrap_or_default();
		drop(cursor);
		for source in sources {
			let proof = match certificate(&self.db, &self.bitcoind, self.config.network, self.server_pubkey,
				&source, None, self.config.expiry_payout.sweep_min_confs).await {
				Ok(proof) => proof,
				Err(error) => { warn!("operator source remains held; incomplete original pool proof: {error:#}"); continue; },
			};
			let ids = proof.ids()?;
			let Ok(_flux) = self.vtxos_in_flux.try_lock(&ids) else { continue; };
			let mut wallet = self.rounds_wallet.lock().await;
			witness.apply(&self.db, &proof).await?;
			self.vtxopool.remove_retired(&ids);
			wallet.persist().await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::{ScriptBuf, Sequence, TxIn, Witness};
	use bitcoin::secp256k1::{Keypair, SecretKey};

	struct Directory(PathBuf);
	impl Directory {
		fn new() -> Self {
			let path = std::env::temp_dir().join(format!("ark-operator-witness-{}-{}", std::process::id(),
				bitcoin::secp256k1::rand::random::<u64>()));
			fs::create_dir(&path).unwrap();
			Self(path)
		}
	}
	impl Drop for Directory {
		fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
	}

	fn key() -> PublicKey {
		Keypair::from_secret_key(&ark::SECP, &SecretKey::from_slice(&[1; 32]).unwrap()).public_key()
	}

	fn proof() -> Certificate {
		// Journal controls only. This synthetic record is never admitted as
		// positive native/source/chain proof or passed to financial SQL.
		Certificate { identity: identity(Network::Regtest, key()), source: "source:0".into(),
			receipt: "receipt".into(), receipt_height: 100, receipt_block: "block".into(),
			revision: 0, gross_sat: 80_000, sweep_script: vec![1], funding_body: vec![],
			funding_hash: digest(b"funding"), receipt_body: vec![], receipt_hash: digest(b"receipt"),
			leaves: vec![], tree_hashes: BTreeMap::new() }
	}

	fn journal(dir: &Directory) -> OperatorWitness {
		OperatorWitness::initialize(&dir.0, Network::Regtest, key()).unwrap();
		OperatorWitness { directory: dir.0.join(DIRECTORY), writer: Arc::new(tokio::sync::Mutex::new(())),
			cursor: Arc::new(tokio::sync::Mutex::new(String::new())) }
	}

	#[tokio::test]
	async fn operator_witness_retry_is_idempotent_and_conflicting_gross_is_refused() {
		let dir = Directory::new();
		let witness = journal(&dir);
		let original = proof();
		witness.record_locked(&original).await.unwrap();
		witness.record_locked(&original).await.unwrap();
		assert_eq!(fs::read(witness.directory.join("history")).unwrap(), format!("{}\n", original.id()).as_bytes());
		let mut changed = original.clone(); changed.gross_sat -= 1;
		assert_eq!(changed.id(), original.id());
		assert!(witness.record_locked(&changed).await.is_err());
		assert_eq!(fs::read(witness.directory.join(format!("{}.json", original.id()))).unwrap(), original.bytes().unwrap());
	}

	#[tokio::test]
	async fn operator_witness_orphan_record_can_be_indexed_without_replacing_bytes() {
		let dir = Directory::new();
		let witness = journal(&dir);
		let proof = proof();
		let path = witness.directory.join(format!("{}.json", proof.id()));
		write_identical(&path, &proof.bytes().unwrap()).unwrap();
		assert!(witness.indexed().unwrap().is_empty());
		witness.record_locked(&proof).await.unwrap();
		assert_eq!(witness.indexed().unwrap(), BTreeSet::from([proof.id()]));
		assert_eq!(fs::read(path).unwrap(), proof.bytes().unwrap());
	}

	#[tokio::test]
	async fn operator_witness_torn_or_missing_index_cannot_publish_success() {
		let dir = Directory::new();
		let witness = journal(&dir);
		let path = witness.directory.join("history");
		fs::write(&path, b"partial").unwrap();
		assert!(witness.record_locked(&proof()).await.is_err());
		assert_eq!(fs::read(&path).unwrap(), b"partial");
		fs::remove_file(&path).unwrap();
		assert!(witness.record_locked(&proof()).await.is_err());
		assert!(!path.exists());
	}

	#[test]
	fn operator_witness_bounded_read_refuses_symlinks_hardlinks_and_oversize() {
		let dir = Directory::new();
		let file = dir.0.join("original"); fs::write(&file, b"source").unwrap();
		assert!(read_bounded(&file, 5).is_err());
		let link = dir.0.join("symlink"); std::os::unix::fs::symlink(&file, &link).unwrap();
		assert!(read_bounded(&link, 6).is_err());
		let hard = dir.0.join("hardlink"); fs::hard_link(&file, &hard).unwrap();
		assert!(read_bounded(&hard, 6).is_err());
		assert!(read_bounded(&file, 6).is_err());
	}

	#[test]
	fn operator_certificate_identity_binds_network_server_source_receipt() {
		let original = proof();
		for changed in [Certificate { identity: identity(Network::Signet, key()), ..original.clone() },
			Certificate { source: "another:0".into(), ..original.clone() },
			Certificate { receipt: "another".into(), ..original.clone() }] {
			assert_ne!(changed.id(), original.id());
		}
		let mut extra = serde_json::to_value(&original).unwrap(); extra["unlock_preimage"] = serde_json::json!("unexpected");
		assert!(serde_json::from_value::<Certificate>(extra).is_err());
	}

	#[test]
	fn operator_public_body_never_serializes_unlock_witness_or_script_sig() {
		let mut tx = Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO, input: vec![TxIn {
				previous_output: OutPoint::null(), script_sig: ScriptBuf::new(), sequence: Sequence::MAX,
				witness: Witness::from_slice(&[b"private-preimage"]),
			}], output: vec![] };
		let body: Transaction = deserialize(&public_body(&tx).unwrap()).unwrap();
		assert!(body.input[0].witness.is_empty());
		assert_eq!(body.compute_txid(), tx.compute_txid());
		tx.input[0].script_sig = ScriptBuf::from_bytes(vec![1, 2, 3]);
		assert!(public_body(&tx).is_err());
	}
}
