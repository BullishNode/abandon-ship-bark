//! Original Round rights retained independently of PostgreSQL restoration.
//! Reuses C's exact fee candidate/identity and NativeFeePublication lock. No
//! fee allowance, payment/settlement assertion or customer row is created.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use bitcoin::{BlockHash, Transaction, Wtxid};
use bitcoin::consensus::deserialize;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use ark::{ProtocolEncoding, ServerVtxo, Vtxo};
use ark::vtxo::Full;
use ark::tree::signed::SignedVtxoTreeSpec;
use crate::database::{Db, Tx};
use super::ordinary_fee::OrdinaryFeePolicy;
use super::ordinary_fee_auth::FeeServiceIdentity;
use super::ordinary_fee_publication::{PublicationRecord, private_root, open_record, read_record};

const MAX_RECORDS: usize = 4096;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
const FORMAT: &str = "native-c-round-rights-intent-v1";

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
	format: String, server_pubkey: PublicKey, genesis: BlockHash,
	policy_sha256: sha256::Hash, opening_sha256: sha256::Hash,
}
impl Identity {
	fn original(identity: &FeeServiceIdentity) -> Self {
		Self { format: FORMAT.into(), server_pubkey: identity.server_pubkey, genesis: identity.genesis,
			policy_sha256: identity.policy_sha256, opening_sha256: identity.opening_sha256 }
	}
}
#[derive(Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Output { policy: Vec<u8>, amount: u64, mailbox: Option<String> }
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Participation { unlock_hash: String, inputs: Vec<String>, outputs: Vec<Output>, leaves: Vec<usize> }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input { id: String, original: Vec<u8>, anchor: Vec<u8> }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
	format: String, policy_sha256: String, server_pubkey: String, genesis: String, opening_sha256: String,
	candidate_id: String, candidate_sha256: String, sequence: i64, expiry: i32,
	funding: Vec<u8>, tree: Vec<u8>, participations: Vec<Participation>, inputs: Vec<Input>,
}
fn digest(raw: &[u8]) -> String { sha256::Hash::hash(raw).to_string() }
fn root(history: &Path) -> anyhow::Result<PathBuf> {
	let name = history.file_name().and_then(|name| name.to_str()).context("native rights history path invalid")?;
	Ok(history.with_file_name(format!("{name}.ordinary-native-round-rights")))
}
fn stable(meta: &fs::Metadata) -> (u64,u64,u32,u32,u64,u64,i64,i64,i64,i64) {
	(meta.dev(),meta.ino(),meta.uid(),meta.mode(),meta.nlink(),meta.len(),meta.mtime(),meta.mtime_nsec(),meta.ctime(),meta.ctime_nsec())
}
fn directory(history: &Path, policy: OrdinaryFeePolicy) -> anyhow::Result<Option<(PathBuf, Identity)>> {
	let path = root(history)?; let owner = fs::symlink_metadata(history)?.uid();
	match fs::symlink_metadata(&path) {
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(error.into()),
		Ok(_) => {},
	}
	let (_guard, _) = private_root(&path, owner)?;
	let (_, metadata) = open_record(&path.join("identity.json"), owner)?;
	ensure!(metadata.len() <= 4096, "native rights identity exceeds bound");
	let raw = read_record(&path.join("identity.json"), owner)?;
	let identity: Identity = serde_json::from_slice(&raw)?;
	ensure!(serde_json::to_vec(&identity)? == raw && identity.format == FORMAT
		&& identity.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?)
		&& identity.opening_sha256 != sha256::Hash::all_zeros(), "native rights original identity or policy differs");
	read_record(&path.join("history"), owner)?;
	Ok(Some((path, identity)))
}
fn write_identical(path: &Path, owner: u32, raw: &[u8]) -> anyhow::Result<()> {
	match fs::symlink_metadata(path) {
		Ok(_) => ensure!(read_record(path, owner)? == raw, "conflicting original round rights witness"),
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => crate::fs_perms::create_new_owner_only(path, raw)?,
		Err(error) => return Err(error.into()),
	}
	let (file, _) = open_record(path, owner)?; file.sync_all()?;
	File::open(path.parent().context("native rights parent absent")?)?.sync_all()?;
	Ok(())
}
fn indexed(path: &Path, owner: u32) -> anyhow::Result<BTreeSet<String>> {
	let (_, metadata) = open_record(&path.join("history"), owner)?;
	ensure!(
		metadata.len() <= (MAX_RECORDS * 65) as u64,
		"native rights index exceeds complete bound"
	);
	let raw = read_record(&path.join("history"), owner)?;
	parse_index(&raw)
}
fn parse_index(raw: &[u8]) -> anyhow::Result<BTreeSet<String>> {
	ensure!(
		raw.len() <= MAX_RECORDS * 65 && (raw.is_empty() || raw.last() == Some(&b'\n')),
		"native rights index is torn or exceeds bound"
	);
	let mut ids = BTreeSet::new();
	for line in std::str::from_utf8(&raw)?.lines() {
		ensure!(
			line.parse::<Wtxid>()?.to_string() == line && ids.insert(line.to_owned()),
			"native rights index repeats or changes original id"
		);
	}
	Ok(ids)
}
fn append_index(path: &Path, owner: u32, id: &str, ids: &BTreeSet<String>) -> anyhow::Result<()> {
	if ids.contains(id) { return Ok(()); }
	ensure!(ids.len() < MAX_RECORDS && id.parse::<Wtxid>()?.to_string() == id, "native rights index bound or id differs");
	let index = path.join("history"); let old = read_record(&index, owner)?;
	ensure!(old.len() == ids.len() * 65, "native rights index changed under original global lock");
	let (_read, before) = open_record(&index, owner)?;
	let mut file = OpenOptions::new().read(true).append(true).open(&index)?;
	ensure!(stable(&before) == stable(&file.metadata()?) && stable(&before) == stable(&fs::symlink_metadata(&index)?),
		"native rights index changed while opening append handle");
	file.write_all(format!("{id}\n").as_bytes())?; file.sync_all()?;
	let mut expected = old; expected.extend_from_slice(format!("{id}\n").as_bytes());
	ensure!(read_record(&index, owner)? == expected, "native rights original index append differs");
	File::open(path)?.sync_all()?; Ok(())
}

/// Read-only sizing joins the EXISTING C publication limits, not A's larger
/// history limit. Missing namespace is observed as absent only; startup below
/// independently decides whether any actual old Round requires refusal.
pub(crate) fn inventory(history: &Path, policy: OrdinaryFeePolicy) -> anyhow::Result<(usize, usize)> {
	let Some((path, _)) = directory(history, policy)? else { return Ok((0,0)); };
	let owner = fs::symlink_metadata(history)?.uid(); let (guard, before) = private_root(&path, owner)?;
	let mut seen = BTreeSet::new(); let mut bytes = 0usize;
	for (number, item) in fs::read_dir(&path)?.enumerate() {
		ensure!(number < MAX_RECORDS + 2, "native rights namespace exceeds complete file bound");
		let item = item?; let name = item.file_name(); let name = name.to_str().context("native rights filename is invalid")?;
		if name == "identity.json" || name == "history" { continue; }
		let id = name.strip_suffix(".json").context("unknown native rights witness file")?;
		ensure!(id.parse::<Wtxid>()?.to_string() == id && seen.insert(id.to_owned()), "native rights witness repeats or changes id");
		bytes = bytes.checked_add(read_record(&item.path(), owner)?.len()).context("native rights witness byte overflow")?;
		ensure!(seen.len() <= MAX_RECORDS && bytes <= MAX_BYTES, "native rights witness capacity exceeded");
	}
	ensure!(seen == indexed(&path, owner)?, "native rights original file/index sets differ; preserve partial evidence and hold");
	ensure!(stable(&before) == stable(&guard.metadata()?) && stable(&before) == stable(&fs::symlink_metadata(&path)?),
		"native rights namespace changed during complete scan");
	Ok((seen.len(), bytes))
}

// The SDK decoder bounds every allocation vector to 4,000,000 bytes.
// Check shape/counts before version detection or cached tree construction;
// version detection itself authenticates only the FIRST internal signature.
fn authenticate_tree(raw:&[u8],funding:&Transaction,server:&str)->anyhow::Result<ark::tree::signed::CachedSignedVtxoTree> {
	ensure!(raw.len()<=MAX_RECORD_BYTES,"original signed tree byte bound exceeded");
	let signed=SignedVtxoTreeSpec::deserialize(raw)?;
	ensure!(signed.serialize()==raw && (2..=MAX_RECORDS).contains(&signed.spec.vtxos.len())
		&& !signed.spec.global_cosign_pubkeys.is_empty() && signed.spec.global_cosign_pubkeys.len()<=MAX_RECORDS
		&& signed.cosign_sigs.len()==signed.spec.nb_internal_nodes(),"canonical bounded round tree shape required");
	ensure!(signed.spec.server_pubkey.to_string()==server && signed.utxo.txid==funding.compute_txid(),
		"original tree server or funding identity differs");
	let total=signed.spec.vtxos.iter().try_fold(0u64,|n,v|n.checked_add(v.vtxo.amount.to_sat()))
		.context("original tree amount overflow")?;
	ensure!(total>0 && total<=bitcoin::Amount::MAX_MONEY.to_sat(),"original tree amount outside monetary bound");
	let signed=signed.with_detected_hashlock_version()?;
	let unsigned=signed.spec.clone().into_unsigned_tree(signed.utxo);
	unsigned.verify_cosign_sigs(&signed.cosign_sigs).map_err(|_|anyhow::anyhow!("original tree internal cosignature invalid"))?;
	ensure!(funding.output.get(signed.utxo.vout as usize)==Some(&signed.spec.funding_tx_txout()),
		"original tree funding output value or script differs");
	Ok(signed.into_cached_tree())
}
struct BoundedRecord(Vec<u8>);
impl Write for BoundedRecord {
	fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
		if !self.0.len().checked_add(bytes.len()).is_some_and(|n|n<=MAX_RECORD_BYTES) {
			return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"round intent serialized byte bound exceeded"));
		}
		self.0.extend_from_slice(bytes);Ok(bytes.len())
	}
	fn flush(&mut self)->std::io::Result<()> {Ok(())}
}

// Native values produced either by the locked SQL reader below or by the
// closed captured-row adapter. These are private originals, not proof that a
// capture was authentic, complete, current or economically backed.
pub(crate) struct CapturedRoundInput {
	pub id: String,
	pub original: Vec<u8>,
	pub stored_anchor: String,
	pub spent_round: Option<String>,
	pub anchor: Vec<u8>,
}
pub(crate) struct CapturedRoundOutput {
	pub policy: Vec<u8>,
	pub amount: u64,
	pub mailbox: Option<String>,
}
pub(crate) struct CapturedRoundParticipation {
	pub unlock_hash: String,
	pub inputs: Vec<CapturedRoundInput>,
	pub outputs: Vec<CapturedRoundOutput>,
}
pub(crate) struct CapturedRoundProjection {
	pub sequence: i64,
	pub expiry: i32,
	pub funding: Vec<u8>,
	pub virtual_funding: Option<Vec<u8>>,
	pub tree: Vec<u8>,
	pub participations: Vec<CapturedRoundParticipation>,
	pub leaves: BTreeMap<String, Vec<u8>>,
}

// All projections omit unlock_preimage and mutable confirmation/forfeit fields.
async fn capture(
	t: &Tx<'_>,
	policy: OrdinaryFeePolicy,
	identity: &FeeServiceIdentity,
	candidate: &PublicationRecord,
) -> anyhow::Result<Intent> {
	ensure!(
		candidate.entry(policy)?.purpose() == "round",
		"rights stage needs original Round candidate"
	);
	let funding = candidate.transaction()?;
	let txid = funding.compute_txid().to_string();
	let bounds = t.query_one("SELECT count(*), COALESCE(sum(octet_length(v.vtxo)+octet_length(i.vtxo_id)+octet_length(v.anchor_point)+COALESCE(octet_length(a.signed_tx),0)),0)::bigint, count(a.signed_tx) AS anchors FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id JOIN vtxo v ON v.vtxo_id=i.vtxo_id LEFT JOIN virtual_transaction a ON a.txid=split_part(v.anchor_point,':',1) WHERE p.round_id=$1", &[&txid]).await?;
	ensure!(
		bounds.get::<_, i64>(0) <= MAX_RECORDS as i64
			&& bounds.get::<_, i64>(1) <= MAX_RECORD_BYTES as i64
			&& bounds.get::<_, i64>(0) == bounds.get::<_, i64>("anchors"),
		"round original input read bound exceeded"
	);
	let bound = t.query_one("SELECT octet_length(r.signed_tree) AS tree_bytes,octet_length(r.funding_tx) AS funding_bytes,octet_length(v.signed_tx) AS virtual_bytes FROM round r JOIN virtual_transaction v ON v.txid=r.funding_txid WHERE r.funding_txid=$1", &[&txid]).await?;
	ensure!(
		bound.get::<_, i32>("tree_bytes") <= MAX_RECORD_BYTES as i32
			&& bound.get::<_, i32>("funding_bytes") <= MAX_RECORD_BYTES as i32
			&& bound
				.get::<_, Option<i32>>("virtual_bytes")
				.is_some_and(|n| n <= MAX_RECORD_BYTES as i32),
		"round source byte bound exceeded"
	);
	let original_bytes = u64::try_from(bounds.get::<_, i64>(1))?;
	ensure!(
		original_bytes
			.checked_add(u64::try_from(bound.get::<_, i32>("tree_bytes"))?)
			.and_then(|n| n.checked_add(u64::try_from(bound.get::<_, i32>("funding_bytes")).ok()?))
			.is_some_and(|n| n <= MAX_RECORD_BYTES as u64),
		"aggregate original inputs/anchors/tree/funding read bound exceeded"
	);
	let part_bounds=t.query_one("SELECT count(*),COALESCE(sum(octet_length(unlock_hash)),0)::bigint FROM round_participation WHERE round_id=$1",&[&txid]).await?;
	ensure!(
		part_bounds.get::<_, i64>(0) <= MAX_RECORDS as i64
			&& part_bounds.get::<_, i64>(1) <= MAX_RECORD_BYTES as i64,
		"participation metadata read bound exceeded"
	);
	let allocation_bounds=t.query_one("SELECT count(*),COALESCE(sum(octet_length(o.policy)+COALESCE(octet_length(o.unblinded_mailbox_id),0)),0)::bigint FROM round_part_output o JOIN round_participation p ON p.id=o.participation_id WHERE p.round_id=$1",&[&txid]).await?;
	ensure!(
		allocation_bounds.get::<_, i64>(0) <= MAX_RECORDS as i64
			&& allocation_bounds.get::<_, i64>(1) <= MAX_RECORD_BYTES as i64,
		"round allocation metadata read bound exceeded"
	);
	let combined = [
		original_bytes,
		u64::try_from(bound.get::<_, i32>("tree_bytes"))?,
		u64::try_from(bound.get::<_, i32>("funding_bytes"))?,
		u64::try_from(part_bounds.get::<_, i64>(1))?,
		u64::try_from(allocation_bounds.get::<_, i64>(1))?,
	]
	.into_iter()
	.try_fold(0u64, |n, next| n.checked_add(next))
	.context("combined round original byte overflow")?;
	ensure!(
		combined <= MAX_RECORD_BYTES as u64,
		"combined original round payload read bound exceeded"
	);
	let complete_nonleaf_bytes = combined.checked_add(u64::try_from(bound.get::<_,Option<i32>>("virtual_bytes").context("original virtual funding absent")?)?)
		.and_then(|n|n.checked_add(u64::try_from(bounds.get::<_,i64>(0)).ok()?.checked_mul(txid.len() as u64)?))
		.context("complete Round projection byte overflow")?;
	ensure!(complete_nonleaf_bytes <= MAX_RECORD_BYTES as u64, "complete Round projection virtual funding/input metadata exceeds existing bound");
	let row = t.query_one("SELECT r.seq,r.expiry,r.funding_tx,r.signed_tree,v.signed_tx AS virtual_funding FROM round r JOIN virtual_transaction v ON v.txid=r.funding_txid WHERE r.funding_txid=$1", &[&txid]).await?;
	let raw_funding: Vec<u8> = row.get("funding_tx");
	let tree_raw: Vec<u8> = row.get("signed_tree");
	let tree = authenticate_tree(&tree_raw, &funding, &identity.server_pubkey.to_string())?;
	let parts = t.query("SELECT id,unlock_hash FROM round_participation WHERE round_id=$1 ORDER BY unlock_hash LIMIT 4097", &[&txid]).await?;
	ensure!(
		parts.len() <= MAX_RECORDS,
		"round participation bound exceeded"
	);
	let mut participations = Vec::new();
	let mut total_bytes = raw_funding
		.len()
		.checked_add(tree_raw.len())
		.context("round record byte overflow")?;
	for part in parts {
		let part_id: i64 = part.get("id");
		let source = t.query("SELECT i.vtxo_id,v.vtxo,v.anchor_point AS stored_anchor,r.funding_txid AS spent_round FROM round_part_input i JOIN vtxo v ON v.vtxo_id=i.vtxo_id LEFT JOIN round r ON r.id=v.spent_in_round AND r.funding_txid=$2 WHERE i.participation_id=$1 AND octet_length(v.vtxo)<=4194304 AND octet_length(i.vtxo_id)<=80 AND octet_length(v.anchor_point)<=80 ORDER BY i.vtxo_id LIMIT 4097", &[&part_id,&txid]).await?;
		ensure!(
			!source.is_empty() && source.len() <= MAX_RECORDS,
			"round input membership absent or over bound"
		);
		let mut inputs = Vec::new();
		for input in source {
			let original: Vec<u8> = input.get("vtxo");
			let original_vtxo = decode_user_full(&original)?;
			let anchor_id = original_vtxo.chain_anchor().txid.to_string();
			let anchor = t.query_one("SELECT signed_tx FROM virtual_transaction WHERE txid=$1 AND octet_length(signed_tx) BETWEEN 1 AND 4194304", &[&anchor_id]).await.context("input original anchor absent or over bound")?;
			let anchor = anchor
				.get::<_, Option<Vec<u8>>>("signed_tx")
				.context("original signed anchor absent")?;
			total_bytes = total_bytes
				.checked_add(original.len())
				.and_then(|n| n.checked_add(anchor.len()))
				.context("round original byte overflow")?;
			ensure!(
				total_bytes <= MAX_RECORD_BYTES,
				"round original aggregate byte bound exceeded"
			);
			inputs.push(CapturedRoundInput {
				id: input.get("vtxo_id"),
				original,
				stored_anchor: input.get("stored_anchor"),
				spent_round: input.get("spent_round"),
				anchor,
			});
		}
		let bounds = t.query_one("SELECT count(*),COALESCE(sum(octet_length(policy)+COALESCE(octet_length(unblinded_mailbox_id),0)),0)::bigint FROM round_part_output WHERE participation_id=$1", &[&part_id]).await?;
		ensure!(
			bounds.get::<_, i64>(0) <= MAX_RECORDS as i64
				&& bounds.get::<_, i64>(1) <= MAX_RECORD_BYTES as i64,
			"round output read bound exceeded"
		);
		let mut outputs = Vec::new();
		for output in t.query("SELECT policy,amount,unblinded_mailbox_id FROM round_part_output WHERE participation_id=$1 LIMIT 4097", &[&part_id]).await? {
			let policy: Vec<u8> = output.get("policy"); let mailbox: Option<String> = output.get("unblinded_mailbox_id");
			total_bytes = total_bytes.checked_add(policy.len()).and_then(|n| n.checked_add(mailbox.as_ref().map_or(0,String::len))).context("round output bytes overflow")?;
			ensure!(total_bytes <= MAX_RECORD_BYTES, "round aggregate output bytes exceeded");
			outputs.push(CapturedRoundOutput { policy, amount: u64::try_from(output.get::<_,i64>("amount"))?, mailbox });
		}
		participations.push(CapturedRoundParticipation {
			unlock_hash: part.get("unlock_hash"),
			inputs,
			outputs,
		});
	}
	let leaf_ids = tree
		.output_vtxos()
		.map(|leaf| leaf.id().to_string())
		.collect::<Vec<_>>();
	let leaf_bounds = t.query_one("SELECT count(*),COALESCE(sum(octet_length(vtxo)+octet_length(vtxo_id)),0)::bigint FROM vtxo WHERE vtxo_id=ANY($1)", &[&leaf_ids]).await?;
	ensure!(
		leaf_bounds.get::<_, i64>(0) == i64::try_from(leaf_ids.len())?
			&& u64::try_from(leaf_bounds.get::<_, i64>(1))?
				.checked_add(complete_nonleaf_bytes)
				.is_some_and(|n| n <= MAX_RECORD_BYTES as u64),
		"complete Round projection including retained output bodies exceeds existing byte bound"
	);

	let mut leaves = BTreeMap::new();
	for leaf in tree.output_vtxos() {
		let id = leaf.id().to_string();
		let output = t
			.query_one(
				"SELECT vtxo FROM vtxo WHERE vtxo_id=$1 AND octet_length(vtxo)<=4194304",
				&[&id],
			)
			.await?;
		ensure!(
			leaves
				.insert(id, output.get::<_, Vec<u8>>("vtxo"))
				.is_none(),
			"duplicate round leaf source"
		);
	}
	capture_projection(
		policy,
		identity,
		candidate,
		&CapturedRoundProjection {
			sequence: row.get("seq"),
			expiry: row.get("expiry"),
			funding: raw_funding,
			virtual_funding: row.get("virtual_funding"),
			tree: tree_raw,
			participations,
			leaves,
		},
	)
}

/// Reuses genuine native tree verification before the captured-row adapter
/// selects current leaf bodies from the complete original SQL body table.
pub(crate) fn captured_leaf_ids(
	raw: &[u8],
	funding: &Transaction,
	identity: &FeeServiceIdentity,
) -> anyhow::Result<BTreeSet<String>> {
	Ok(
		authenticate_tree(raw, funding, &identity.server_pubkey.to_string())?
			.output_vtxos()
			.map(|leaf| leaf.id().to_string())
			.collect(),
	)
}
fn projection_bound(source: &CapturedRoundProjection) -> anyhow::Result<()> {
	ensure!(
		source.participations.len() <= MAX_RECORDS && source.leaves.len() <= MAX_RECORDS,
		"complete Round projection count exceeds bound"
	);
	let mut bytes = source
		.funding
		.len()
		.checked_add(source.tree.len())
		.and_then(|n|n.checked_add(source.virtual_funding.as_ref().map_or(0,Vec::len)))
		.context("Round projection byte overflow")?;
	let mut inputs = 0usize;
	let mut outputs = 0usize;
	for part in &source.participations {
		inputs = inputs
			.checked_add(part.inputs.len())
			.context("Round projection input overflow")?;
		outputs = outputs
			.checked_add(part.outputs.len())
			.context("Round projection output overflow")?;
		bytes = bytes
			.checked_add(part.unlock_hash.len())
			.context("Round projection metadata overflow")?;
		ensure!(
			inputs <= MAX_RECORDS && outputs <= MAX_RECORDS,
			"complete Round projection input/output bound exceeded"
		);
		for input in &part.inputs {
			ensure!(
				input.id.len() <= 80 && input.stored_anchor.len() <= 80,
				"captured original input identifier exceeds bound"
			);
			bytes = bytes
				.checked_add(input.original.len())
				.and_then(|n| n.checked_add(input.anchor.len()))
				.and_then(|n| n.checked_add(input.id.len()))
				.and_then(|n| n.checked_add(input.stored_anchor.len()))
				.and_then(|n| n.checked_add(input.spent_round.as_ref().map_or(0,String::len)))
				.context("Round projection original byte overflow")?;
		}
		for output in &part.outputs {
			bytes = bytes
				.checked_add(output.policy.len())
				.and_then(|n| n.checked_add(output.mailbox.as_ref().map_or(0, String::len)))
				.context("Round projection output byte overflow")?;
		}
	}
	for (id, body) in &source.leaves {
		ensure!(
			id.len() <= 80,
			"captured original output identifier exceeds bound"
		);
		bytes = bytes
			.checked_add(body.len())
			.and_then(|n| n.checked_add(id.len()))
			.context("Round projection leaf byte overflow")?;
	}
	ensure!(
		bytes <= MAX_RECORD_BYTES,
		"complete Round projection including retained output bodies exceeds existing byte bound"
	);
	Ok(())
}

/// The SAME native tree/signature/allocation/Full validation for locked SQL and
/// immutable captured SQL. No database, filesystem, activation or fee owner is
/// opened here. Only the caller can authenticate the actual source cut.
fn capture_projection(
	policy: OrdinaryFeePolicy,
	identity: &FeeServiceIdentity,
	candidate: &PublicationRecord,
	source: &CapturedRoundProjection,
) -> anyhow::Result<Intent> {
	projection_bound(source)?;
	let entry = candidate.entry(policy)?;
	ensure!(
		entry.purpose() == "round",
		"rights stage needs original Round candidate"
	);
	let funding = candidate.transaction()?;
	let txid = funding.compute_txid().to_string();
	ensure!(
		source.funding == bitcoin::consensus::serialize(&funding)
			&& source.virtual_funding.as_ref() == Some(&source.funding),
		"round signed funding differs from original permit"
	);
	ensure!(
		source.expiry >= 0
			&& source.participations.len() <= MAX_RECORDS
			&& source.leaves.len() <= MAX_RECORDS,
		"captured Round shape exceeds bound"
	);
	let tree = authenticate_tree(&source.tree, &funding, &identity.server_pubkey.to_string())?;
	ensure!(
		tree.spec.spec.expiry_height.to_u32() == source.expiry as u32,
		"round expiry differs"
	);
	let leaves = tree.output_vtxos().collect::<Vec<_>>();
	ensure!(
		leaves
			.iter()
			.map(|leaf| leaf.id().to_string())
			.collect::<BTreeSet<_>>()
			== source.leaves.keys().cloned().collect(),
		"complete original Round leaf set differs"
	);
	let mut participations = Vec::new();
	let mut inputs = BTreeMap::new();
	let mut allocated = BTreeSet::new();
	let mut hashes = BTreeSet::new();
	let mut total_bytes = source
		.funding
		.len()
		.checked_add(source.tree.len())
		.context("round record byte overflow")?;
	for part in &source.participations {
		ensure!(
			hashes.insert(part.unlock_hash.clone())
				&& !part.inputs.is_empty()
				&& part.inputs.len() <= MAX_RECORDS
				&& !part.outputs.is_empty()
				&& part.outputs.len() <= MAX_RECORDS,
			"round participation duplicate, absent or over bound"
		);
		let mut ids = Vec::new();
		let mut value = 0u64;
		for input in &part.inputs {
			let original = decode_user_full(&input.original)?;
			ensure!(
				original.server_pubkey() == identity.server_pubkey
					&& original.id().to_string() == input.id
					&& input.spent_round.as_deref() == Some(&txid),
				"original input server or consumption differs"
			);
			ensure!(
				input.stored_anchor == original.chain_anchor().to_string(),
				"original anchor differs from bounded source projection"
			);
			total_bytes = total_bytes
				.checked_add(input.original.len())
				.and_then(|n| n.checked_add(input.anchor.len()))
				.context("round original byte overflow")?;
			ensure!(
				inputs.len() < MAX_RECORDS && total_bytes <= MAX_RECORD_BYTES,
				"round original aggregate input bound exceeded"
			);
			validate_original(&original, &input.anchor)?;
			value = value
				.checked_add(original.amount().to_sat())
				.context("round input amount overflow")?;
			ids.push(input.id.clone());
			ensure!(
				inputs
					.insert(
						input.id.clone(),
						Input {
							id: input.id.clone(),
							original: input.original.clone(),
							anchor: input.anchor.clone()
						}
					)
					.is_none(),
				"duplicate round input ownership"
			);
		}
		ids.sort();
		let mut outputs = part
			.outputs
			.iter()
			.map(|o| Output {
				policy: o.policy.clone(),
				amount: o.amount,
				mailbox: o.mailbox.clone(),
			})
			.collect::<Vec<_>>();
		outputs.sort();
		for output in &outputs {
			total_bytes = total_bytes
				.checked_add(output.policy.len())
				.and_then(|n| n.checked_add(output.mailbox.as_ref().map_or(0, String::len)))
				.context("round output bytes overflow")?;
		}
		ensure!(
			total_bytes <= MAX_RECORD_BYTES,
			"round aggregate output bytes exceeded"
		);
		let indexes = leaves
			.iter()
			.enumerate()
			.filter(|(_, v)| {
				v.unlock_hash()
					.is_some_and(|h| h.to_string() == part.unlock_hash)
			})
			.map(|(i, _)| i)
			.collect::<Vec<_>>();
		let mut expected = indexes
			.iter()
			.map(|i| {
				(
					leaves[*i].policy().serialize(),
					leaves[*i].amount().to_sat(),
				)
			})
			.collect::<Vec<_>>();
		expected.sort();
		let mut actual = outputs
			.iter()
			.map(|o| (o.policy.clone(), o.amount))
			.collect::<Vec<_>>();
		actual.sort();
		ensure!(
			actual == expected,
			"complete indexed round allocation differs"
		);
		let sum = outputs
			.iter()
			.try_fold(0u64, |n, o| n.checked_add(o.amount))
			.context("round output amount overflow")?;
		ensure!(
			sum <= value,
			"round participation creates unsupported excess value"
		);
		for index in &indexes {
			ensure!(allocated.insert(*index), "round leaf allocated twice");
		}
		participations.push(Participation {
			unlock_hash: part.unlock_hash.clone(),
			inputs: ids,
			outputs,
			leaves: indexes,
		});
	}
	participations.sort_by(|a, b| a.unlock_hash.cmp(&b.unlock_hash));
	for (index, leaf) in leaves.iter().enumerate() {
		if !allocated.contains(&index) {
			ensure!(
				leaf.policy()
					== &ark::VtxoPolicy::new_pubkey(
						"02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c"
							.parse()?
					)
					&& leaf.amount() == bitcoin_ext::P2WSH_DUST
					&& leaf.unlock_hash().is_some_and(|h| h.to_string()
						== "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
				"non-padding entitlement missing from round participation"
			);
		}
		let current = decode_user_full(
			source
				.leaves
				.get(&leaf.id().to_string())
				.context("original Round leaf absent")?,
		)?;
		validate_representation(&current, leaf, &source.funding)?;
	}
	let result = Intent {
		format: FORMAT.into(),
		policy_sha256: identity.policy_sha256.to_string(),
		server_pubkey: identity.server_pubkey.to_string(),
		genesis: identity.genesis.to_string(),
		opening_sha256: identity.opening_sha256.to_string(),
		candidate_id: entry.id().to_string(),
		candidate_sha256: digest(&serde_json::to_vec(candidate)?),
		sequence: source.sequence,
		expiry: source.expiry,
		funding: source.funding.clone(),
		tree: source.tree.clone(),
		participations,
		inputs: inputs.into_values().collect(),
	};
	let mut bounded = BoundedRecord(Vec::new());
	serde_json::to_writer(&mut bounded, &result)?;
	Ok(result)
}

fn validate_original(vtxo:&Vtxo,anchor:&[u8])->anyhow::Result<()> {
	let anchor:Transaction=deserialize(anchor)?;
	vtxo.validate_unsigned(&anchor)?;
	if vtxo.has_all_witnesses() {vtxo.validate(&anchor)?;}
	Ok(())
}
fn validate_representation(current:&Vtxo,original:&Vtxo,anchor:&[u8])->anyhow::Result<()> {
	validate_original(original,anchor)?;
	if current.serialize()==original.serialize() {return validate_original(current,anchor);}
	ensure!(current.is_final_hark_leaf_completion_of(original), "unsupported native Full change; preserve known original fields");
	current.validate(&deserialize::<Transaction>(anchor)?)?;
	Ok(())
}

fn decode_user_full(raw: &[u8]) -> anyhow::Result<Vtxo> {
	let current = ServerVtxo::<Full>::deserialize(raw)?.try_into_user_vtxo()
		.map_err(|_| anyhow::anyhow!("round rights source is not a native user Full"))?;
	ensure!(current.serialize() == raw, "round rights source Full is noncanonical");
	Ok(current)
}

// Called ONLY after the original paused opening was independently verified,
// with the existing NativeFeePublication lock. Absence never means old rounds
// were zero; SQL and the original journal must independently prove that cut.
pub(crate) async fn initialize_original(t: &Tx<'_>, history: &Path, policy: OrdinaryFeePolicy,
	identity: &FeeServiceIdentity,
) -> anyhow::Result<()> {
	if let Some((_, old)) = directory(history, policy)? {
		ensure!(old == Identity::original(identity), "original rights namespace belongs to another opening");
		inventory(history, policy)?; return Ok(());
	}
	let records = super::ordinary_fee::original_publications_locked(history, policy)?;
	ensure!(records.values().all(|record| record.entry(policy).is_ok_and(|entry| entry.purpose() != "round"))
		&& !t.query_one("SELECT EXISTS(SELECT 1 FROM round)", &[]).await?.get::<_,bool>(0),
		"missing original Round rights namespace on retained history; no implicit historical-zero bootstrap");
	let path = root(history)?; let owner = fs::symlink_metadata(history)?.uid();
	fs::DirBuilder::new().mode(0o700).create(&path)?;
	File::open(path.parent().context("native rights parent absent")?)?.sync_all()?;
	private_root(&path, owner)?;
	write_identical(&path.join("identity.json"), owner, &serde_json::to_vec(&Identity::original(identity))?)?;
	write_identical(&path.join("history"), owner, b"")?;
	inventory(history, policy)?; Ok(())
}

/// LAST fallible operation in the same transaction that persists Round rights,
/// funding and nursery. A marker surviving unknown/aborted COMMIT stays a HOLD;
/// it never reconstructs SQL or declares an abandoned attempt safe.
pub(crate) async fn record(history: &Path, t: &Tx<'_>, policy: OrdinaryFeePolicy,
	identity: &FeeServiceIdentity, candidate: &PublicationRecord, expected_inputs: &BTreeSet<String>,
) -> anyhow::Result<()> {
	tokio::time::timeout(std::time::Duration::from_secs(30),
		record_locked(history, t, policy, identity, candidate, expected_inputs)).await
		.context("bounded original Round rights stage exceeded; preserve unknown marker and reservation")?
}
async fn record_locked(history: &Path, t: &Tx<'_>, policy: OrdinaryFeePolicy,
	identity: &FeeServiceIdentity, candidate: &PublicationRecord, expected_inputs: &BTreeSet<String>,
) -> anyhow::Result<()> {
	let (path, old) = directory(history, policy)?.context("original Round rights namespace is not initialized")?;
	ensure!(old == Identity::original(identity), "original Round rights identity differs");
	let originals = super::ordinary_fee::original_publications_locked(history, policy)?;
	let entry = candidate.entry(policy)?;
	ensure!(originals.get(&entry.id()) == Some(candidate), "Round rights lost its original fee reservation");
	t.require_native_fee_intent(candidate, policy, identity.opening_sha256, &candidate.transaction()?).await?;
	let intent = capture(t, policy, identity, candidate).await?;
	ensure!(intent.inputs.iter().map(|input| input.id.clone()).collect::<BTreeSet<_>>() == *expected_inputs,
		"Round rights omits an original owning input");
	let mut bounded = BoundedRecord(Vec::new()); serde_json::to_writer(&mut bounded, &intent)?;
	let owner = fs::symlink_metadata(history)?.uid();
	let file = path.join(format!("{}.json", intent.candidate_id));
	let usage = inventory(history, policy)?;
	let publication_usage = super::ordinary_fee_publication::witness_usage(history)?;
	if fs::symlink_metadata(&file).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
		ensure!(usage.0.checked_add(publication_usage.0).is_some_and(|count| count < MAX_RECORDS)
			&& usage.1.checked_add(publication_usage.1).and_then(|bytes| bytes.checked_add(bounded.0.len()))
				.is_some_and(|bytes| bytes <= MAX_BYTES), "combined original history capacity exhausted");
	}
	write_identical(&file, owner, &bounded.0)?;
	let ids = indexed(&path, owner)?; append_index(&path, owner, &intent.candidate_id, &ids)?;
	inventory(history, policy)?; Ok(())
}

fn same_rights(old: &Intent, new: &Intent) -> anyhow::Result<()> {
	ensure!(old.format == new.format && old.policy_sha256 == new.policy_sha256
		&& old.server_pubkey == new.server_pubkey && old.genesis == new.genesis && old.opening_sha256 == new.opening_sha256
		&& old.candidate_id == new.candidate_id && old.candidate_sha256 == new.candidate_sha256
		&& old.sequence == new.sequence && old.expiry == new.expiry && old.funding == new.funding && old.tree == new.tree
		&& old.participations == new.participations && old.inputs.len() == new.inputs.len(),
		"immutable original Round rights differ; no settlement reconstruction");
	for (old, new) in old.inputs.iter().zip(&new.inputs) {
		ensure!(old.id == new.id && old.anchor == new.anchor, "original Round input or anchor differs");
		validate_representation(&decode_user_full(&new.original)?, &decode_user_full(&old.original)?, &old.anchor)?;
	}
	Ok(())
}

fn decode_original_intent(raw: &[u8], id: &str) -> anyhow::Result<Intent> {
	ensure!(
		raw.len() <= MAX_RECORD_BYTES,
		"original Round rights record exceeds bound"
	);
	let original: Intent = serde_json::from_slice(raw)?;
	let mut bounded = BoundedRecord(Vec::new());
	serde_json::to_writer(&mut bounded, &original)?;
	ensure!(
		bounded.0 == raw && original.candidate_id == id,
		"original Round rights record is noncanonical"
	);
	Ok(original)
}

/// Validate original rights against complete captured Round projections using
/// the same crypto/representation comparator as the native live reader. This
/// returns a count only, never readiness, chain ownership or settlement proof.
/// SQL opening/fee/nursery joins are separately mandatory at the caller.
pub(crate) fn verify_captured_rounds(
	policy: OrdinaryFeePolicy,
	identity: &FeeServiceIdentity,
	originals: &BTreeMap<Wtxid, PublicationRecord>,
	files: Option<&BTreeMap<String, String>>,
	projections: &BTreeMap<String, CapturedRoundProjection>,
	published_rounds: &BTreeSet<String>,
) -> anyhow::Result<usize> {
	ensure!(
		projections.len() <= MAX_RECORDS && published_rounds.len() <= MAX_RECORDS,
		"captured Round set exceeds complete bound"
	);
	let Some(files) = files else {
		ensure!(
			projections.is_empty()
				&& published_rounds.is_empty()
				&& originals.values().all(|candidate| candidate
					.entry(policy)
					.is_ok_and(|entry| entry.purpose() != "round")),
			"missing independent Round rights history on retained Round or fee history"
		);
		return Ok(0);
	};
	ensure!(
		files.len() >= 2 && files.len() <= MAX_RECORDS + 2,
		"captured Round rights namespace bound differs"
	);
	let raw = files
		.get("identity.json")
		.context("original Round rights identity absent")?
		.as_bytes();
	ensure!(raw.len() <= 4096, "native rights identity exceeds bound");
	let original_identity: Identity = serde_json::from_slice(raw)?;
	ensure!(
		serde_json::to_vec(&original_identity)? == raw
			&& original_identity == Identity::original(identity)
			&& identity.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?)
			&& identity.opening_sha256 != sha256::Hash::all_zeros(),
		"captured Round original identity differs"
	);
	let ids = parse_index(
		files
			.get("history")
			.context("original Round index absent")?
			.as_bytes(),
	)?;
	ensure!(
		files.len() == ids.len() + 2
			&& ids
				.iter()
				.all(|id| files.contains_key(&format!("{id}.json"))),
		"native rights original file/index sets differ; preserve partial evidence and hold"
	);
	let bytes = ids
		.iter()
		.try_fold(0usize, |n, id| {
			n.checked_add(files[&format!("{id}.json")].len())
		})
		.context("captured original rights byte overflow")?;
	ensure!(
		bytes <= MAX_BYTES,
		"captured original rights byte capacity exceeded"
	);
	let mut retained_rounds = BTreeSet::new();
	for id in &ids {
		let original = decode_original_intent(files[&format!("{id}.json")].as_bytes(), id)?;
		let candidate = originals
			.get(&id.parse::<Wtxid>()?)
			.context("original Round rights lost its exact fee candidate")?;
		let txid = candidate.transaction()?.compute_txid().to_string();
		let source = projections
			.get(&txid)
			.context("original Round obligations absent from complete captured SQL")?;
		let current = capture_projection(policy, identity, candidate, source)?;
		same_rights(&original, &current)?;
		ensure!(
			retained_rounds.insert(txid),
			"Round rights repeats one funding transaction"
		);
	}
	ensure!(
		projections.keys().cloned().collect::<BTreeSet<_>>() == retained_rounds
			&& published_rounds.is_subset(&retained_rounds),
		"captured database Round or published candidate lacks complete independent original rights"
	);
	Ok(ids.len())
}

async fn validate_locked(
	t: &Tx<'_>,
	history: &Path,
	policy: OrdinaryFeePolicy,
	server: PublicKey,
	genesis: BlockHash,
	expected_opening: Option<sha256::Hash>,
) -> anyhow::Result<()> {
	let records = super::ordinary_fee::original_publications_locked(history, policy)?;
	let bounds = t
		.query_one(
			"SELECT count(*),COALESCE(sum(octet_length(funding_txid)),0)::bigint FROM round",
			&[],
		)
		.await?;
	ensure!(
		bounds.get::<_, i64>(0) <= MAX_RECORDS as i64
			&& bounds.get::<_, i64>(1) <= (MAX_RECORDS * 64) as i64,
		"Round identity projection exceeds complete bound"
	);
	let rounds = t
		.query(
			"SELECT funding_txid FROM round ORDER BY funding_txid LIMIT 4097",
			&[],
		)
		.await?
		.into_iter()
		.map(|row| row.get::<_, String>(0))
		.collect::<BTreeSet<_>>();
	let Some((path, original_identity)) = directory(history, policy)? else {
		ensure!(
			rounds.is_empty()
				&& records.values().all(|record| record
					.entry(policy)
					.is_ok_and(|entry| entry.purpose() != "round")),
			"missing independent Round rights history on retained Round or fee history"
		);
		return Ok(());
	};
	ensure!(
		original_identity.server_pubkey == server
			&& original_identity.genesis == genesis
			&& expected_opening.is_none_or(|opening| opening == original_identity.opening_sha256),
		"Round rights original server, chain or opening differs"
	);
	let identity = FeeServiceIdentity {
		server_pubkey: server,
		genesis,
		policy_sha256: original_identity.policy_sha256,
		opening_sha256: original_identity.opening_sha256,
	};
	inventory(history, policy)?;
	let owner = fs::symlink_metadata(history)?.uid();
	let ids = indexed(&path, owner)?;
	if !ids.is_empty() || !rounds.is_empty() {
		let opening = t
			.query_opt(
				"SELECT opening_hash,policy_bytes FROM native_fee_opening WHERE id=1",
				&[],
			)
			.await?
			.context("original Round rights lost its original native opening")?;
		ensure!(
			opening.get::<_, String>(0) == identity.opening_sha256.to_string()
				&& opening.get::<_, Vec<u8>>(1) == serde_json::to_vec(&policy)?,
			"Round rights original opening SQL differs"
		);
	}
	let mut retained_rounds = BTreeSet::new();
	for id in ids {
		let raw = read_record(&path.join(format!("{id}.json")), owner)?;
		let original = decode_original_intent(&raw, &id)?;
		let candidate = records
			.get(&id.parse::<Wtxid>()?)
			.context("original Round rights lost its exact fee candidate")?;
		t.require_native_fee_intent(
			candidate,
			policy,
			identity.opening_sha256,
			&candidate.transaction()?,
		)
		.await?;
		let current = capture(t, policy, &identity, candidate)
			.await
			.context("original Round obligations absent or mismatched; financial startup held")?;
		same_rights(&original, &current)?;
		ensure!(
			retained_rounds.insert(candidate.transaction()?.compute_txid().to_string()),
			"Round rights repeats one funding transaction"
		);
	}
	ensure!(
		rounds == retained_rounds,
		"database Round has no complete independent original rights witness"
	);
	// Unused precommit permits remain reserved but grant no rights. Every SQL
	// publication is already nursery-bound by the existing complete startup
	// reader, and each published Round must also have its independent marker.
	let published = t.query("SELECT txid FROM native_fee_publication WHERE purpose='round' AND published_at IS NOT NULL ORDER BY txid LIMIT 4097", &[]).await?;
	ensure!(
		published.len() <= MAX_RECORDS
			&& published
				.into_iter()
				.all(|row| retained_rounds.contains(&row.get::<_, String>(0))),
		"published Round candidate lacks its original rights marker"
	);
	Ok(())
}

/// Both owning daemons call before wallet replay or financial workers. This
/// reader cannot initialize or repair missing/partial independent history.
pub(crate) async fn validate_startup(db: &Db, history: &Path, policy: OrdinaryFeePolicy,
	server: PublicKey, genesis: BlockHash, expected_opening: Option<sha256::Hash>,
) -> anyhow::Result<()> {
	tokio::time::timeout(std::time::Duration::from_secs(30), db.write(async |t| {
		t.lock_final_fee_sources().await?;
		t.batch_execute("LOCK TABLE round,round_participation,round_part_input,round_part_output,vtxo,virtual_transaction IN SHARE MODE").await?;
		validate_locked(t, history, policy, server, genesis, expected_opening).await
	})).await.context("bounded original Round rights startup exceeded; retain all original evidence")??;
	Ok(())
}
#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicU64, Ordering};
	use std::os::unix::fs::OpenOptionsExt;
	use ark::tree::signed::builder::SignedTreeBuilder;
	use bitcoin::secp256k1::{Keypair,SecretKey};
	static NEXT: AtomicU64 = AtomicU64::new(0);
	struct PrivateDirectory(std::path::PathBuf);
	impl Drop for PrivateDirectory {
		fn drop(&mut self) {let _=fs::remove_dir_all(&self.0);}
	}
	fn fixture() -> (PrivateDirectory, OrdinaryFeePolicy) {
		let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!("c-round-rights-unit-{}-{}-{}", std::process::id(),
			std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), nonce));
		fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
		let history = path.join("history-owner"); fs::DirBuilder::new().mode(0o700).create(&history).unwrap();
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		let identity = Identity::original(&FeeServiceIdentity {
			server_pubkey: Keypair::from_secret_key(&ark::SECP, &SecretKey::from_slice(&[3;32]).unwrap()).public_key(),
			genesis: bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash(),
			policy_sha256: sha256::Hash::hash(&serde_json::to_vec(&policy).unwrap()),
			opening_sha256: sha256::Hash::hash(b"synthetic original; NOT financial authority"),
		});
		let rights = root(&history).unwrap(); fs::DirBuilder::new().mode(0o700).create(&rights).unwrap();
		let owner = fs::symlink_metadata(&history).unwrap().uid();
		write_identical(&rights.join("identity.json"), owner, &serde_json::to_vec(&identity).unwrap()).unwrap();
		write_identical(&rights.join("history"), owner, b"").unwrap();
		(PrivateDirectory(path), policy)
	}
	fn rights_path(root: &PrivateDirectory) -> PathBuf { super::root(&root.0.join("history-owner")).unwrap() }
	fn owner(root: &PrivateDirectory) -> u32 { fs::symlink_metadata(&root.0).unwrap().uid() }

	#[test]
	fn private_namespace_binds_original_policy_and_rejects_unknown_entries() {
		let (root,policy)=fixture();assert_eq!(inventory(&root.0.join("history-owner"),policy).unwrap(),(0,0));
		let mut other=policy;other.ordinary_budget_sat+=1;
		assert!(inventory(&root.0.join("history-owner"),other).is_err());
		write_identical(&rights_path(&root).join("unknown"),owner(&root),b"unclassified").unwrap();
		assert!(inventory(&root.0.join("history-owner"),policy).is_err());
	}
	#[test]
	fn indexed_missing_original_refuses_before_database_access() {
		let (root,policy)=fixture();let directory=rights_path(&root);
		let ids=indexed(&directory,owner(&root)).unwrap();append_index(&directory,owner(&root),&"33".repeat(32),&ids).unwrap();
		let error=inventory(&root.0.join("history-owner"),policy).unwrap_err();
		assert!(error.to_string().contains("file/index sets differ"));
	}
	#[test]
	fn original_index_rejects_torn_duplicate_and_noncanonical_ids() {
		let (root,_policy)=fixture();let directory=rights_path(&root);let path=directory.join("history");
		let id="44".repeat(32);fs::write(&path,&id).unwrap();assert!(indexed(&directory,owner(&root)).is_err());
		fs::write(&path,format!("{id}\n{id}\n")).unwrap();assert!(indexed(&directory,owner(&root)).is_err());
		fs::write(&path,"not-an-original-wtxid\n").unwrap();assert!(indexed(&directory,owner(&root)).is_err());
		fs::write(&path,format!("{id}\n")).unwrap();assert_eq!(indexed(&directory,owner(&root)).unwrap(),BTreeSet::from([id]));
	}
	#[test]
	fn private_record_bound_and_symlink_refuse_before_deserialization() {
		let (root,policy)=fixture();let directory=rights_path(&root);let path=directory.join(format!("{}.json","55".repeat(32)));
		let file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
		file.set_len(MAX_RECORD_BYTES as u64+1).unwrap();assert!(inventory(&root.0.join("history-owner"),policy).is_err());
		drop(file);fs::remove_file(&path).unwrap();std::os::unix::fs::symlink(directory.join("identity.json"),&path).unwrap();
		assert!(inventory(&root.0.join("history-owner"),policy).is_err());
	}
	#[test]
	fn bounded_json_writer_refuses_before_extending_record() {
		let mut writer=BoundedRecord(Vec::new());writer.write_all(&vec![0;MAX_RECORD_BYTES]).unwrap();
		assert!(writer.write_all(&[0]).is_err());assert_eq!(writer.0.len(),MAX_RECORD_BYTES);
	}
	fn signed_tree_fixture()->(SignedVtxoTreeSpec,Transaction,String) { signed_tree_fixture_with_inputs(vec![]) }
	fn signed_tree_fixture_with_inputs(funding_inputs:Vec<bitcoin::TxIn>)->(SignedVtxoTreeSpec,Transaction,String) {
		let key=|byte|Keypair::from_secret_key(&ark::SECP,&SecretKey::from_slice(&[byte;32]).unwrap());
		let leaf=key(1);let user=key(2);let server=key(3);let cosign=key(4);let preimage=[7;32];
		let requests=[10000,10000,20000,30000,40000,50000].map(|n|ark::VtxoRequest {
			amount:bitcoin::Amount::from_sat(n),policy:ark::VtxoPolicy::new_pubkey(leaf.public_key())});
		let builder=SignedTreeBuilder::new(requests.iter().cloned(),user.public_key(),preimage,
			128u32.into(),server.public_key(),cosign.public_key(),12u16.into()).unwrap();
		let funding=Transaction {version:bitcoin::transaction::Version::TWO,lock_time:bitcoin::absolute::LockTime::ZERO,
			input:funding_inputs,output:vec![builder.funding_txout()]};
		let point=bitcoin::OutPoint::new(funding.compute_txid(),0);
		let builder=builder.set_utxo(point).generate_user_nonces(&user);
		let response=SignedTreeBuilder::new_for_cosign(requests.iter().cloned(),user.public_key(),preimage,
			128u32.into(),server.public_key(),cosign.public_key(),12u16.into(),point,builder.user_pub_nonces().to_vec())
			.unwrap().server_cosign(&cosign);
		builder.verify_cosign_response(&response).unwrap();
		(builder.build_tree(&response,&user).unwrap(),funding,server.public_key().to_string())
	}
	#[test]
	fn genuine_tree_checks_every_internal_cosignature_and_exact_shape() {
		let (signed,funding,server)=signed_tree_fixture();
		assert!(authenticate_tree(&signed.serialize(),&funding,&server).is_ok());
		let mut bad=signed.clone();assert!(bad.cosign_sigs.len()>1);bad.cosign_sigs[1]=bad.cosign_sigs[0];
		assert!(bad.clone().with_detected_hashlock_version().is_ok(),"first signature cannot prove remaining nodes");
		assert!(authenticate_tree(&bad.serialize(),&funding,&server).is_err());
		let mut short=signed.clone();short.cosign_sigs.pop();assert!(authenticate_tree(&short.serialize(),&funding,&server).is_err());
		let mut extra=signed;extra.cosign_sigs.push(extra.cosign_sigs[0]);assert!(authenticate_tree(&extra.serialize(),&funding,&server).is_err());
	}
	#[test]
	fn genuine_tree_rejects_wrong_server_funding_and_noncanonical_bytes() {
		let (signed,funding,server)=signed_tree_fixture();
		assert!(authenticate_tree(&signed.serialize(),&funding,"wrong-server").is_err());
		let mut other=funding.clone();other.output[0].value+=bitcoin::Amount::from_sat(1);
		assert!(authenticate_tree(&signed.serialize(),&other,&server).is_err());
		let mut raw=signed.serialize();raw.push(0);assert!(authenticate_tree(&raw,&funding,&server).is_err());
	}

	#[test]
	fn unindexed_file_and_conflicting_original_never_repair_or_overwrite() {
		let (root,policy)=fixture(); let path=rights_path(&root); let file=path.join(format!("{}.json","66".repeat(32)));
		write_identical(&file,owner(&root),b"retained original partial marker").unwrap();
		assert!(inventory(&root.0.join("history-owner"),policy).is_err());
		assert!(write_identical(&file,owner(&root),b"replacement").is_err());
		assert_eq!(read_record(&file,owner(&root)).unwrap(),b"retained original partial marker");
		assert_eq!(read_record(&path.join("history"),owner(&root)).unwrap(),b"");
	}
	#[test]
	fn original_rights_tuple_refuses_allocation_candidate_and_chain_substitution() {
		let value = || Intent { format:FORMAT.into(),policy_sha256:"policy".into(),server_pubkey:"server".into(),
			genesis:"chain".into(),opening_sha256:"opening".into(),candidate_id:"candidate".into(),candidate_sha256:"signed-body".into(),
			sequence:7,expiry:100,funding:vec![1],tree:vec![2],participations:vec![Participation { unlock_hash:"hash".into(),
				inputs:vec!["input".into()],outputs:vec![Output{policy:vec![3],amount:400,mailbox:Some("owner".into())}],leaves:vec![0]}],inputs:vec![] };
		assert!(same_rights(&value(),&value()).is_ok());
		let mut changed=value(); changed.participations[0].outputs[0].amount+=1; assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.participations[0].outputs[0].mailbox=Some("foreign".into()); assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.participations[0].inputs.clear(); assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.funding.push(0); assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.tree.push(0); assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.candidate_sha256="other-signed-body".into(); assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.genesis="other-chain".into(); assert!(same_rights(&value(),&changed).is_err());
		let mut changed=value(); changed.opening_sha256="other-opening".into(); assert!(same_rights(&value(),&changed).is_err());
	}

	#[test]
	fn captured_original_namespace_preserves_absence_and_exact_index_file_sets() {
		let (root, policy) = fixture();
		let directory = rights_path(&root);
		let raw = read_record(&directory.join("identity.json"), owner(&root)).unwrap();
		let original: Identity = serde_json::from_slice(&raw).unwrap();
		let identity = FeeServiceIdentity {
			server_pubkey: original.server_pubkey,
			genesis: original.genesis,
			policy_sha256: original.policy_sha256,
			opening_sha256: original.opening_sha256,
		};
		let mut files = BTreeMap::from([
			("identity.json".into(), String::from_utf8(raw).unwrap()),
			("history".into(), String::new()),
		]);
		assert_eq!(
			verify_captured_rounds(
				policy,
				&identity,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.unwrap(),
			0
		);
		assert_eq!(
			verify_captured_rounds(
				policy,
				&identity,
				&BTreeMap::new(),
				None,
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.unwrap(),
			0
		);
		let published = BTreeSet::from(["11".repeat(32)]);
		assert!(
			verify_captured_rounds(
				policy,
				&identity,
				&BTreeMap::new(),
				None,
				&BTreeMap::new(),
				&published
			)
			.is_err()
		);
		files.insert("unknown".into(), String::new());
		assert!(
			verify_captured_rounds(
				policy,
				&identity,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.is_err()
		);
		files.remove("unknown");
		files.insert("history".into(), "11".repeat(32));
		assert!(
			verify_captured_rounds(
				policy,
				&identity,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.is_err()
		);
		files.insert("history".into(), format!("{}\n", "11".repeat(32)));
		assert!(
			verify_captured_rounds(
				policy,
				&identity,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.is_err()
		);
	}
	#[test]
	fn captured_original_identity_never_rebinds_another_opening_chain_or_policy() {
		let (root, policy) = fixture();
		let raw = read_record(&rights_path(&root).join("identity.json"), owner(&root)).unwrap();
		let original: Identity = serde_json::from_slice(&raw).unwrap();
		let identity = FeeServiceIdentity {
			server_pubkey: original.server_pubkey,
			genesis: original.genesis,
			policy_sha256: original.policy_sha256,
			opening_sha256: original.opening_sha256,
		};
		let files = BTreeMap::from([
			("identity.json".into(), String::from_utf8(raw).unwrap()),
			("history".into(), String::new()),
		]);
		let mut wrong = identity.clone();
		wrong.opening_sha256 = sha256::Hash::hash(b"other opening");
		assert!(
			verify_captured_rounds(
				policy,
				&wrong,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.is_err()
		);
		let mut wrong = identity.clone();
		wrong.genesis = bitcoin::constants::genesis_block(bitcoin::Network::Signet).block_hash();
		assert!(
			verify_captured_rounds(
				policy,
				&wrong,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.is_err()
		);
		let mut wrong_policy = policy;
		wrong_policy.ordinary_budget_sat += 1;
		assert!(
			verify_captured_rounds(
				wrong_policy,
				&identity,
				&BTreeMap::new(),
				Some(&files),
				&BTreeMap::new(),
				&BTreeSet::new()
			)
			.is_err()
		);
	}

	#[test]
	fn captured_complete_signed_round_reuses_native_crypto_and_rejects_changed_consumption_and_allocation()
	 {
		use super::super::ordinary_fee_publication::PublicationPath;
		use bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Witness};
		let (old_tree, old_funding, server) = signed_tree_fixture();
		let old_leaves = authenticate_tree(&old_tree.serialize(), &old_funding, &server)
			.unwrap()
			.output_vtxos()
			.collect::<Vec<_>>();
		let (_, template, _) = signed_tree_fixture();
		let total = template
			.output
			.iter()
			.map(|o| o.value.to_sat())
			.sum::<u64>();
		let parent = Transaction {
			version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![],
			output: vec![TxOut {
				value: Amount::from_sat(total + 1_000),
				script_pubkey: ScriptBuf::new(),
			}],
		};
		let (signed, funding, _) = signed_tree_fixture_with_inputs(vec![TxIn {
			previous_output: OutPoint::new(parent.compute_txid(), 0),
			script_sig: ScriptBuf::new(),
			sequence: Sequence::MAX,
			witness: Witness::from_slice(&[vec![0; 64]]),
		}]);
		let policy = OrdinaryFeePolicy {
			ordinary_budget_sat: 58_000,
			..Default::default()
		};
		let identity = FeeServiceIdentity {
			server_pubkey: server.parse().unwrap(),
			genesis: bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash(),
			policy_sha256: sha256::Hash::hash(&serde_json::to_vec(&policy).unwrap()),
			opening_sha256: sha256::Hash::hash(b"synthetic capture; no runtime authority"),
		};
		let candidate = PublicationRecord::new(
			PublicationPath::Round,
			policy,
			&funding,
			&[parent],
			FeeRate::from_sat_per_vb_u32(100),
		)
		.unwrap();
		let leaves = authenticate_tree(&signed.serialize(), &funding, &server)
			.unwrap()
			.output_vtxos()
			.collect::<Vec<_>>();
		let hash = leaves.iter().find_map(|v|v.unlock_hash().filter(|h|h.to_string()!="deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")).unwrap().to_string();
		let inputs = old_leaves
			.iter()
			.map(|v| CapturedRoundInput {
				id: v.id().to_string(),
				original: v.serialize(),
				stored_anchor: v.chain_anchor().to_string(),
				spent_round: Some(funding.compute_txid().to_string()),
				anchor: bitcoin::consensus::serialize(&old_funding),
			})
			.collect();
		let outputs = leaves
			.iter()
			.filter(|v|v.unlock_hash().is_some_and(|h|h.to_string()==hash))
			.map(|v| CapturedRoundOutput {
				policy: v.policy().serialize(),
				amount: v.amount().to_sat(),
				mailbox: Some("synthetic actual SQL mailbox".into()),
			})
			.collect();
		let mut projection = CapturedRoundProjection {
			sequence: 7,
			expiry: 128,
			funding: bitcoin::consensus::serialize(&funding),
			virtual_funding: Some(bitcoin::consensus::serialize(&funding)),
			tree: signed.serialize(),
			participations: vec![CapturedRoundParticipation {
				unlock_hash: hash,
				inputs,
				outputs,
			}],
			leaves: leaves
				.iter()
				.map(|v| (v.id().to_string(), v.serialize()))
				.collect(),
		};
		let original = capture_projection(policy, &identity, &candidate, &projection).unwrap();
		let id = candidate.entry(policy).unwrap().id();
		let files = BTreeMap::from([
			(
				"identity.json".into(),
				serde_json::to_string(&Identity::original(&identity)).unwrap(),
			),
			("history".into(), format!("{id}\n")),
			(
				format!("{id}.json"),
				serde_json::to_string(&original).unwrap(),
			),
		]);
		let originals = BTreeMap::from([(id, candidate)]);
		let txid = funding.compute_txid().to_string();
		projection.participations[0].outputs[0].amount += 1;
		let mut projections = BTreeMap::from([(txid.clone(), projection)]);
		assert!(
			verify_captured_rounds(
				policy,
				&identity,
				&originals,
				Some(&files),
				&projections,
				&BTreeSet::new()
			)
			.is_err()
		);
		projections.get_mut(&txid).unwrap().participations[0].outputs[0].amount -= 1;
		assert_eq!(
			verify_captured_rounds(
				policy,
				&identity,
				&originals,
				Some(&files),
				&projections,
				&BTreeSet::from([txid.clone()])
			)
			.unwrap(),
			1
		);
		projections.get_mut(&txid).unwrap().participations[0].inputs[0].spent_round = None;
		assert!(
			verify_captured_rounds(
				policy,
				&identity,
				&originals,
				Some(&files),
				&projections,
				&BTreeSet::new()
			)
			.is_err()
		);
	}
	#[test]
	fn captured_projection_checks_whole_byte_bound_before_any_native_decode() {
		let mut projection = CapturedRoundProjection {
			sequence: 0,
			expiry: 0,
			funding: vec![],
			virtual_funding: None,
			tree: vec![],
			participations: vec![],
			leaves: BTreeMap::new(),
		};
		assert!(projection_bound(&projection).is_ok());
		projection
			.leaves
			.insert("id".into(), vec![0; MAX_RECORD_BYTES]);
		assert!(projection_bound(&projection).is_err());
		projection.leaves.clear();projection.funding.push(0);
		projection.virtual_funding=Some(vec![0;MAX_RECORD_BYTES]);
		assert!(projection_bound(&projection).is_err());
	}
}
