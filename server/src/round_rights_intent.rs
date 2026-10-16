//! Private original round obligations, independent of PostgreSQL restoration.
//! A stage is possibly committed intent, never proof of payment or settlement.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use anyhow::Context;
use bitcoin::{Transaction, Wtxid};
use bitcoin::consensus::deserialize;
use bitcoin::hashes::{sha256, Hash};
use serde::{Deserialize, Serialize};
use ark::{ProtocolEncoding, Vtxo};
use ark::tree::signed::SignedVtxoTreeSpec;
use crate::database::Tx;
use crate::fee_allowance::{Candidate, Path as PublicationPath, Policy};
use crate::operator_residual::{read_bounded, write_identical};

pub(crate) const DIRECTORY: &str = "round-rights";
pub(crate) const MAX_RECORDS: usize = 100_000;
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
const FORMAT: &str = "native-round-rights-intent-v1";

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
	format: String, policy_sha256: String, candidate_id: String, candidate_sha256: String,
	sequence: i64, expiry: i32, funding: Vec<u8>, tree: Vec<u8>,
	participations: Vec<Participation>, inputs: Vec<Input>,
}
fn digest(raw: &[u8]) -> String { sha256::Hash::hash(raw).to_string() }

pub(crate) fn initialize(root: &Path, policy: &Policy) -> anyhow::Result<()> {
	let directory = root.join(DIRECTORY);
	fs::create_dir(&directory)?;
	crate::fs_perms::harden(&directory, 0o700)?;
	write_identical(&directory.join("identity.json"), &serde_json::to_vec(&serde_json::json!({
		"format": FORMAT, "policy_sha256": digest(&serde_json::to_vec(policy)?),
	}))?)?;
	write_identical(&directory.join("history"), b"")?;
	File::open(root)?.sync_all()?;
	Ok(())
}
fn directory(root: &Path, policy: &Policy) -> anyhow::Result<std::path::PathBuf> {
	let directory = root.join(DIRECTORY);
	ensure!(fs::symlink_metadata(&directory)?.is_dir(), "round rights namespace absent; original reconciliation required");
	ensure!(fs::symlink_metadata(&directory)?.permissions().mode() & 0o777 == 0o700, "round rights namespace is not private");
	let owner=fs::symlink_metadata(root)?.uid();
	ensure!(fs::symlink_metadata(&directory)?.uid()==owner,"round rights namespace owner differs");
	for name in ["identity.json","history"] {
		let metadata=fs::symlink_metadata(directory.join(name))?;
		ensure!(metadata.uid()==owner && metadata.permissions().mode() & 0o777 == 0o600,
			"round rights original file is not owner-private");
	}
	let expected = serde_json::to_vec(&serde_json::json!({
		"format": FORMAT, "policy_sha256": digest(&serde_json::to_vec(policy)?),
	}))?;
	ensure!(read_bounded(&directory.join("identity.json"), 65536)? == expected,
		"round rights format or immutable policy differs");
	Ok(directory)
}
pub(crate) fn inventory(root: &Path, policy: &Policy) -> anyhow::Result<(usize, usize)> {
	let directory = directory(root, policy)?;
	let mut count = 0usize; let mut bytes = 0usize;
	for (number, entry) in fs::read_dir(&directory)?.enumerate() {
		ensure!(number <= MAX_RECORDS + 3, "round rights directory bound exceeded");
		let entry = entry?; let name = entry.file_name(); let name = name.to_str().context("invalid rights filename")?;
		if name == "identity.json" || name == "history" || (name.starts_with('.') && name.ends_with(".tmp")) { continue; }
		let id = name.strip_suffix(".json").context("unknown round rights entry")?;
		ensure!(id.parse::<Wtxid>()?.to_string() == id, "noncanonical rights candidate id");
		ensure!(fs::symlink_metadata(entry.path())?.uid()==fs::symlink_metadata(&directory)?.uid()
			&& fs::symlink_metadata(entry.path())?.permissions().mode() & 0o777 == 0o600,
			"round rights intent is not owner-private");
		let raw = read_bounded(&entry.path(), MAX_RECORD_BYTES)?;
		bytes = bytes.checked_add(raw.len()).context("rights byte overflow")?;
		count += 1; ensure!(count <= MAX_RECORDS && bytes <= MAX_BYTES, "round rights bound exceeded");
	}
	let ids=indexed(&directory)?;
	ensure!(ids.iter().all(|id|directory.join(format!("{id}.json")).is_file()),
		"indexed original round rights file missing; no financial admission");
	Ok((count, bytes))
}
fn indexed(directory: &Path) -> anyhow::Result<BTreeSet<String>> {
	let raw = read_bounded(&directory.join("history"), MAX_RECORDS * 65)?;
	ensure!(raw.is_empty() || raw.last() == Some(&b'\n'), "torn round rights index");
	let mut ids = BTreeSet::new();
	for line in std::str::from_utf8(&raw)?.lines() {
		ensure!(line.parse::<Wtxid>()?.to_string() == line && ids.insert(line.to_owned()), "invalid rights index");
	}
	Ok(ids)
}
fn append_index(directory: &Path, id: &str, ids: &mut BTreeSet<String>) -> anyhow::Result<()> {
	if ids.contains(id) { return Ok(()); }
	let mut index = OpenOptions::new().append(true).open(directory.join("history"))?;
	ensure!(index.metadata()?.len() == ids.len() as u64 * 65, "rights index changed while locked");
	writeln!(index, "{id}")?; index.sync_all()?; ids.insert(id.to_owned());
	File::open(directory)?.sync_all()?; Ok(())
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

// All projections omit unlock_preimage and mutable confirmation/forfeit fields.
async fn capture(t: &Tx<'_>, policy: &Policy, candidate: &Candidate) -> anyhow::Result<Intent> {
	ensure!(matches!(candidate.path, PublicationPath::Round), "rights stage needs original Round candidate");
	let (id, _) = candidate.checked(policy)?;
	let funding: Transaction = deserialize(&candidate.raw)?;
	let txid = funding.compute_txid().to_string();
	let bounds = t.query_one("SELECT count(*), COALESCE(sum(octet_length(v.vtxo)+octet_length(i.vtxo_id)+COALESCE(octet_length(a.signed_tx),0)),0)::bigint, count(a.signed_tx) AS anchors FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id JOIN vtxo v ON v.vtxo_id=i.vtxo_id LEFT JOIN virtual_transaction a ON a.txid=split_part(v.anchor_point,':',1) WHERE p.round_id=$1", &[&txid]).await?;
	ensure!(bounds.get::<_,i64>(0) <= MAX_RECORDS as i64 && bounds.get::<_,i64>(1) <= MAX_RECORD_BYTES as i64 && bounds.get::<_,i64>(0)==bounds.get::<_,i64>("anchors"), "round original input read bound exceeded");
	let bound = t.query_one("SELECT octet_length(r.signed_tree) AS tree_bytes,octet_length(r.funding_tx) AS funding_bytes,octet_length(v.signed_tx) AS virtual_bytes FROM round r JOIN virtual_transaction v ON v.txid=r.funding_txid WHERE r.funding_txid=$1", &[&txid]).await?;
	ensure!(bound.get::<_,i32>("tree_bytes") <= MAX_RECORD_BYTES as i32 && bound.get::<_,i32>("funding_bytes") <= MAX_RECORD_BYTES as i32 && bound.get::<_,Option<i32>>("virtual_bytes").is_some_and(|n|n<=MAX_RECORD_BYTES as i32), "round source byte bound exceeded");
	let original_bytes=u64::try_from(bounds.get::<_,i64>(1))?;
	ensure!(original_bytes.checked_add(u64::try_from(bound.get::<_,i32>("tree_bytes"))?)
		.and_then(|n|n.checked_add(u64::try_from(bound.get::<_,i32>("funding_bytes")).ok()?))
		.is_some_and(|n|n<=MAX_RECORD_BYTES as u64),"aggregate original inputs/anchors/tree/funding read bound exceeded");
	let part_bounds=t.query_one("SELECT count(*),COALESCE(sum(octet_length(unlock_hash)),0)::bigint FROM round_participation WHERE round_id=$1",&[&txid]).await?;
	ensure!(part_bounds.get::<_,i64>(0)<=MAX_RECORDS as i64 && part_bounds.get::<_,i64>(1)<=MAX_RECORD_BYTES as i64,"participation metadata read bound exceeded");
	let allocation_bounds=t.query_one("SELECT count(*),COALESCE(sum(octet_length(o.policy)+COALESCE(octet_length(o.unblinded_mailbox_id),0)),0)::bigint FROM round_part_output o JOIN round_participation p ON p.id=o.participation_id WHERE p.round_id=$1",&[&txid]).await?;
	ensure!(allocation_bounds.get::<_,i64>(0)<=MAX_RECORDS as i64 && allocation_bounds.get::<_,i64>(1)<=MAX_RECORD_BYTES as i64,"round allocation metadata read bound exceeded");
	let combined=[original_bytes,u64::try_from(bound.get::<_,i32>("tree_bytes"))?,
		u64::try_from(bound.get::<_,i32>("funding_bytes"))?,u64::try_from(part_bounds.get::<_,i64>(1))?,
		u64::try_from(allocation_bounds.get::<_,i64>(1))?].into_iter()
		.try_fold(0u64,|n,next|n.checked_add(next)).context("combined round original byte overflow")?;
	ensure!(combined<=MAX_RECORD_BYTES as u64,"combined original round payload read bound exceeded");
	let row = t.query_one("SELECT r.seq,r.expiry,r.funding_tx,r.signed_tree,v.signed_tx FROM round r JOIN virtual_transaction v ON v.txid=r.funding_txid WHERE r.funding_txid=$1", &[&txid]).await?;
	let raw_funding: Vec<u8> = row.get("funding_tx"); let virtual_funding: Option<Vec<u8>> = row.get("signed_tx");
	ensure!(raw_funding == candidate.raw && virtual_funding.as_ref() == Some(&raw_funding), "round signed funding differs from original permit");
	let tree_raw: Vec<u8> = row.get("signed_tree");
	let tree = authenticate_tree(&tree_raw,&funding,&policy.server_pubkey)?;
	ensure!(tree.spec.utxo.txid == funding.compute_txid(), "round tree anchor differs");
	ensure!(tree.spec.spec.expiry_height.to_u32() == row.get::<_,i32>("expiry") as u32, "round expiry differs");
	ensure!(tree.spec.spec.server_pubkey.to_string()==policy.server_pubkey,"round original server identity differs");
	let leaves = tree.output_vtxos().collect::<Vec<_>>();
	ensure!(leaves.len() <= MAX_RECORDS, "round leaf bound exceeded");
	let parts = t.query("SELECT id,unlock_hash FROM round_participation WHERE round_id=$1 ORDER BY unlock_hash LIMIT 100001", &[&txid]).await?;
	ensure!(parts.len() <= MAX_RECORDS, "round participation bound exceeded");
	let mut participations = Vec::new(); let mut inputs = BTreeMap::new(); let mut allocated = BTreeSet::new();
	let mut total_bytes=raw_funding.len().checked_add(tree_raw.len()).context("round record byte overflow")?;
	ensure!(total_bytes<=MAX_RECORD_BYTES,"round original funding/tree byte bound exceeded");
	for part in parts {
		let part_id: i64 = part.get("id"); let hash: String = part.get("unlock_hash");
		let source = t.query("SELECT i.vtxo_id,v.vtxo,v.anchor_point AS stored_anchor,r.funding_txid AS spent_round FROM round_part_input i JOIN vtxo v ON v.vtxo_id=i.vtxo_id LEFT JOIN round r ON r.id=v.spent_in_round WHERE i.participation_id=$1 AND octet_length(v.vtxo)<=4194304 AND octet_length(i.vtxo_id)<=80 ORDER BY i.vtxo_id LIMIT 100001", &[&part_id]).await?;
		ensure!(!source.is_empty() && source.len() <= MAX_RECORDS, "round input membership absent or over bound");
		let mut ids = Vec::new(); let mut value = 0u64;
		for input in source {
			let original: Vec<u8> = input.get("vtxo"); let original_vtxo = Vtxo::deserialize(&original)?;
			let input_id: String = input.get("vtxo_id");
			ensure!(original_vtxo.server_pubkey().to_string()==policy.server_pubkey,"original input server identity differs");
			ensure!(original_vtxo.id().to_string() == input_id && input.get::<_,Option<String>>("spent_round").as_deref() == Some(&txid), "original input consumption differs");
			ensure!(input.get::<_,String>("stored_anchor")==original_vtxo.chain_anchor().to_string(),"original anchor differs from bounded source projection");
			let anchor_id = original_vtxo.chain_anchor().txid.to_string();
			let anchor = t.query_one("SELECT signed_tx FROM virtual_transaction WHERE txid=$1 AND octet_length(signed_tx) BETWEEN 1 AND 4194304", &[&anchor_id]).await.context("input original anchor absent or over bound")?;
			let anchor: Vec<u8> = anchor.get::<_,Option<Vec<u8>>>("signed_tx").context("original signed anchor absent")?;
			total_bytes=total_bytes.checked_add(original.len()).and_then(|n|n.checked_add(anchor.len())).context("round original byte overflow")?;
			ensure!(total_bytes<=MAX_RECORD_BYTES,"round original aggregate byte bound exceeded");
			validate_original(&original_vtxo, &anchor)?;
			value = value.checked_add(original_vtxo.amount().to_sat()).context("round input amount overflow")?;
			ids.push(input_id.clone()); ensure!(inputs.insert(input_id.clone(), Input {id: input_id, original, anchor}).is_none(), "duplicate round input ownership");
		}
		let output_bounds = t.query_one("SELECT count(*),COALESCE(sum(octet_length(policy)+COALESCE(octet_length(unblinded_mailbox_id),0)),0)::bigint FROM round_part_output WHERE participation_id=$1", &[&part_id]).await?;
		ensure!(output_bounds.get::<_,i64>(0) <= MAX_RECORDS as i64 && output_bounds.get::<_,i64>(1) <= MAX_RECORD_BYTES as i64, "round output read bound exceeded");
		let mut outputs = t.query("SELECT policy,amount,unblinded_mailbox_id FROM round_part_output WHERE participation_id=$1 LIMIT 100001", &[&part_id]).await?.iter().map(|o| Ok(Output {policy:o.get("policy"),amount:u64::try_from(o.get::<_,i64>("amount"))?,mailbox:o.get("unblinded_mailbox_id")})).collect::<anyhow::Result<Vec<_>>>()?;
		outputs.sort(); ensure!(!outputs.is_empty(), "round output entitlement absent");
		for output in &outputs {total_bytes=total_bytes.checked_add(output.policy.len()).and_then(|n|n.checked_add(output.mailbox.as_ref().map_or(0,String::len))).context("round output bytes overflow")?;}
		ensure!(total_bytes<=MAX_RECORD_BYTES,"round aggregate output bytes exceeded");
		let indexes = leaves.iter().enumerate().filter(|(_,v)|v.unlock_hash().is_some_and(|h|h.to_string()==hash)).map(|(i,_)|i).collect::<Vec<_>>();
		let mut expected = indexes.iter().map(|i| (leaves[*i].policy().serialize(),leaves[*i].amount().to_sat())).collect::<Vec<_>>(); expected.sort();
		let mut actual = outputs.iter().map(|o|(o.policy.clone(),o.amount)).collect::<Vec<_>>(); actual.sort();
		ensure!(actual == expected, "complete indexed round allocation differs");
		let sum = outputs.iter().try_fold(0u64,|n,o|n.checked_add(o.amount)).context("round output amount overflow")?;
		ensure!(sum <= value, "round participation creates unsupported excess value");
		for index in &indexes {ensure!(allocated.insert(*index), "round leaf allocated twice");}
		participations.push(Participation {unlock_hash:hash,inputs:ids,outputs,leaves:indexes});
	}
	for (index, leaf) in leaves.iter().enumerate() {
		if !allocated.contains(&index) {
			ensure!(leaf.policy() == &ark::VtxoPolicy::new_pubkey("02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c".parse()?)
				&& leaf.amount() == bitcoin_ext::P2WSH_DUST
				&& leaf.unlock_hash().is_some_and(|h|h.to_string()=="deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"), "non-padding entitlement missing from round participation");
		}
		let output = t.query_one("SELECT vtxo FROM vtxo WHERE vtxo_id=$1 AND octet_length(vtxo)<=4194304", &[&leaf.id().to_string()]).await?;
		let current = Vtxo::deserialize(output.get::<_,&[u8]>("vtxo"))?;
		validate_representation(&current,leaf,&raw_funding)?;
	}
	Ok(Intent {format:FORMAT.to_owned(),policy_sha256:digest(&serde_json::to_vec(policy)?),candidate_id:id,
		candidate_sha256:digest(&serde_json::to_vec(candidate)?),sequence:row.get("seq"),expiry:row.get("expiry"),
		funding:raw_funding,tree:tree_raw,participations,inputs:inputs.into_values().collect()})
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

pub(crate) async fn record(
	root:&Path,t:&Tx<'_>,policy:&Policy,candidate:&Candidate,expected_inputs:&BTreeSet<String>,
	combined_records:usize,combined_bytes:usize,
)->anyhow::Result<()> {
	let intent=capture(t,policy,candidate).await?;
	ensure!(intent.inputs.iter().map(|v|v.id.clone()).collect::<BTreeSet<_>>()==*expected_inputs,
		"publication stage omits original owning input");
	let directory=directory(root,policy)?;let path=directory.join(format!("{}.json",intent.candidate_id));
	let mut bounded=BoundedRecord(Vec::new());serde_json::to_writer(&mut bounded,&intent)?;let raw=bounded.0;
	if !path.exists() {ensure!(combined_records<MAX_RECORDS && combined_bytes.checked_add(raw.len()).is_some_and(|n|n<=MAX_BYTES),"combined original history capacity exhausted");}
	write_identical(&path,&raw)?;
	let mut ids=indexed(&directory)?;append_index(&directory,&intent.candidate_id,&mut ids)?;
	Ok(())
}
pub(crate) async fn validate_all(root:&Path,t:&Tx<'_>,policy:&Policy,candidates:&BTreeMap<String,Candidate>)->anyhow::Result<()> {
	let directory=directory(root,policy)?;let mut ids=indexed(&directory)?;let mut seen=BTreeSet::new();
	for entry in fs::read_dir(&directory)? {
		let entry=entry?;let name=entry.file_name();let name=name.to_str().context("invalid round rights name")?;
		if name=="identity.json"||name=="history"||(name.starts_with('.')&&name.ends_with(".tmp")){continue;}
		let id=name.strip_suffix(".json").context("unknown round rights file")?;
		let original:Intent=serde_json::from_slice(&read_bounded(&entry.path(),MAX_RECORD_BYTES)?)?;
		let candidate=candidates.get(id).context("round rights lost its original fee candidate")?;
		ensure!(seen.insert(id.to_owned()) && original.candidate_id==id,"round rights original identity differs");
		let current=capture(t,policy,candidate).await.context("retained round obligations absent or mismatched; financial startup held")?;
		ensure!(original.format==current.format && original.policy_sha256==current.policy_sha256
			&& original.candidate_id==current.candidate_id && original.candidate_sha256==current.candidate_sha256
			&& original.sequence==current.sequence && original.expiry==current.expiry
			&& original.funding==current.funding && original.tree==current.tree
			&& original.participations==current.participations && original.inputs.len()==current.inputs.len(),
			"immutable retained round rights differ; no settlement reconstruction");
		for (old,new) in original.inputs.iter().zip(&current.inputs) {
			ensure!(old.id==new.id && old.anchor==new.anchor,"original round input or anchor differs");
			validate_representation(&Vtxo::deserialize(&new.original)?,&Vtxo::deserialize(&old.original)?,&old.anchor)?;
		}
		append_index(&directory,id,&mut ids)?;
	}
	ensure!(ids==seen,"indexed original round rights file missing; startup held");
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
	fn fixture() -> (PrivateDirectory,Policy) {
		let nonce=NEXT.fetch_add(1,Ordering::Relaxed);
		let path=std::env::temp_dir().join(format!("round-rights-unit-{}-{}-{}",std::process::id(),
			std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),nonce));
		fs::create_dir(&path).unwrap();crate::fs_perms::harden(&path,0o700).unwrap();
		let policy=Policy {epoch_id:"11".repeat(32),genesis:"unit-supplied-policy".into(),server_pubkey:"unit-policy-key".into(),
			ordinary_sat:1000,urgent_sat:1000,ordinary_sat_per_kwu:250,urgent_child_sat_per_kwu:1000,
			urgent_package_sat_per_kwu:1000,urgent_max_candidate_sat:1000,urgent_required_confirmed_sat:20000,
			watchman_descriptor_sha256:"22".repeat(32),watchman_admin_address:"127.0.0.1:1".into()};
		initialize(&path,&policy).unwrap();(PrivateDirectory(path),policy)
	}
	#[test]
	fn private_namespace_binds_original_policy_and_rejects_unknown_entries() {
		let (root,policy)=fixture();assert_eq!(inventory(&root.0,&policy).unwrap(),(0,0));
		let mut other=policy.clone();other.ordinary_sat+=1;
		assert!(inventory(&root.0,&other).is_err());
		write_identical(&root.0.join(DIRECTORY).join("unknown"),b"unclassified").unwrap();
		assert!(inventory(&root.0,&policy).is_err());
	}
	#[test]
	fn indexed_missing_original_refuses_before_database_access() {
		let (root,policy)=fixture();let directory=directory(&root.0,&policy).unwrap();
		let mut ids=indexed(&directory).unwrap();append_index(&directory,&"33".repeat(32),&mut ids).unwrap();
		let error=inventory(&root.0,&policy).unwrap_err();
		assert!(error.to_string().contains("indexed original round rights file missing"));
	}
	#[test]
	fn original_index_rejects_torn_duplicate_and_noncanonical_ids() {
		let (root,policy)=fixture();let directory=directory(&root.0,&policy).unwrap();let path=directory.join("history");
		let id="44".repeat(32);fs::write(&path,&id).unwrap();assert!(indexed(&directory).is_err());
		fs::write(&path,format!("{id}\n{id}\n")).unwrap();assert!(indexed(&directory).is_err());
		fs::write(&path,"not-an-original-wtxid\n").unwrap();assert!(indexed(&directory).is_err());
		fs::write(&path,format!("{id}\n")).unwrap();assert_eq!(indexed(&directory).unwrap(),BTreeSet::from([id]));
	}
	#[test]
	fn private_record_bound_and_symlink_refuse_before_deserialization() {
		let (root,policy)=fixture();let directory=directory(&root.0,&policy).unwrap();let path=directory.join(format!("{}.json","55".repeat(32)));
		let file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
		file.set_len(MAX_RECORD_BYTES as u64+1).unwrap();assert!(inventory(&root.0,&policy).is_err());
		drop(file);fs::remove_file(&path).unwrap();std::os::unix::fs::symlink(directory.join("identity.json"),&path).unwrap();
		assert!(inventory(&root.0,&policy).is_err());
	}
	#[test]
	fn bounded_json_writer_refuses_before_extending_record() {
		let mut writer=BoundedRecord(Vec::new());writer.write_all(&vec![0;MAX_RECORD_BYTES]).unwrap();
		assert!(writer.write_all(&[0]).is_err());assert_eq!(writer.0.len(),MAX_RECORD_BYTES);
	}
	fn signed_tree_fixture()->(SignedVtxoTreeSpec,Transaction,String) {
		let key=|byte|Keypair::from_secret_key(&ark::SECP,&SecretKey::from_slice(&[byte;32]).unwrap());
		let leaf=key(1);let user=key(2);let server=key(3);let cosign=key(4);let preimage=[7;32];
		let requests=[10000,10000,20000,30000,40000,50000].map(|n|ark::VtxoRequest {
			amount:bitcoin::Amount::from_sat(n),policy:ark::VtxoPolicy::new_pubkey(leaf.public_key())});
		let builder=SignedTreeBuilder::new(requests.iter().cloned(),user.public_key(),preimage,
			128u32.into(),server.public_key(),cosign.public_key(),12u16.into()).unwrap();
		let funding=Transaction {version:bitcoin::transaction::Version::TWO,lock_time:bitcoin::absolute::LockTime::ZERO,
			input:vec![],output:vec![builder.funding_txout()]};
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

}
