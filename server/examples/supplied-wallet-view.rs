//! Hash-bound persisted wallet graph inspection only. No daemon, keys, RPC or persistence.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use bdk_wallet::chain::Merge;
use bdk_wallet::{ChangeSet, KeychainKind, Wallet};
use bitcoin::consensus::serialize;
use bitcoin::hashes::{Hash, sha256};
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::{Network, OutPoint};
use serde::Deserialize;
use serde_json::{Value, json};

const INPUT_BYTES: usize = 32 * 1024 * 1024;
const ROW_BYTES: usize = 2 * 1024 * 1024;
const ORIGINAL_BYTES: usize = 16 * 1024 * 1024;
const OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 10_000;
const MAX_NODES: usize = 100_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
	schema: String,
	source_sql_sha256: String,
	network: String,
	min_trusted_confirmations: u32,
	rows: Vec<Original>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Original { id: u64, kind: String, content_hex: String }

fn digest(bytes: &[u8]) -> String { sha256::Hash::hash(bytes).to_string() }
fn hash_name(s: &str) -> bool { s.len()==64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

// Preflight MessagePack lengths/counts before serde can materialize a claimed collection.
fn bounded_messagepack(bytes: &[u8]) -> Result<()> {
	fn integer(bytes: &[u8], offset: &mut usize, count: usize) -> Result<usize> {
		let end=offset.checked_add(count).context("MessagePack offset overflow")?;
		ensure!(end<=bytes.len(),"truncated MessagePack length");
		let mut value=0usize;
		for b in &bytes[*offset..end] { value=value.checked_mul(256).and_then(|v|v.checked_add(*b as usize)).context("MessagePack length overflow")?; }
		*offset=end;Ok(value)
	}
	fn item(bytes: &[u8], offset: &mut usize, depth: usize, nodes: &mut usize) -> Result<()> {
		ensure!(depth<=64 && *nodes<MAX_NODES,"MessagePack shape budget exceeded");*nodes+=1;
		let marker=*bytes.get(*offset).context("truncated MessagePack value")?;*offset+=1;
		let (payload,children)=match marker {
			0x00..=0x7f|0xc0|0xc2|0xc3|0xe0..=0xff => (0,0),
			0x80..=0x8f => (0,2*(marker as usize&15)),
			0x90..=0x9f => (0,marker as usize&15),
			0xa0..=0xbf => (marker as usize&31,0),
			0xc4|0xd9 => (integer(bytes,offset,1)?,0),
			0xc5|0xda => (integer(bytes,offset,2)?,0),
			0xc6|0xdb => (integer(bytes,offset,4)?,0),
			0xc7 => (integer(bytes,offset,1)?.checked_add(1).context("extension overflow")?,0),
			0xc8 => (integer(bytes,offset,2)?.checked_add(1).context("extension overflow")?,0),
			0xc9 => (integer(bytes,offset,4)?.checked_add(1).context("extension overflow")?,0),
			0xca|0xce|0xd2 => (4,0),0xcb|0xcf|0xd3 => (8,0),
			0xcc|0xd0 => (1,0),0xcd|0xd1 => (2,0),
			0xd4 => (2,0),0xd5 => (3,0),0xd6 => (5,0),0xd7 => (9,0),0xd8 => (17,0),
			0xdc => (0,integer(bytes,offset,2)?),0xdd => (0,integer(bytes,offset,4)?),
			0xde => (0,integer(bytes,offset,2)?.checked_mul(2).context("map count overflow")?),
			0xdf => (0,integer(bytes,offset,4)?.checked_mul(2).context("map count overflow")?),
			_ => anyhow::bail!("unsupported MessagePack marker"),
		};
		ensure!(payload<=ROW_BYTES && children<=MAX_NODES && *nodes<=MAX_NODES-children,"MessagePack declared size budget exceeded");
		*offset=offset.checked_add(payload).context("MessagePack body overflow")?;
		ensure!(*offset<=bytes.len(),"truncated MessagePack payload");
		for _ in 0..children {item(bytes,offset,depth+1,nodes)?;}
		Ok(())
	}
	ensure!(!bytes.is_empty() && bytes.len()<=ROW_BYTES,"MessagePack row byte budget exceeded");
	let mut offset=0;item(bytes,&mut offset,0,&mut 0)?;
	ensure!(offset==bytes.len(),"trailing MessagePack bytes");Ok(())
}

struct Limited(Vec<u8>);
impl Write for Limited {
	fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
		if self.0.len().checked_add(bytes.len()).is_none_or(|n|n>OUTPUT_BYTES) { return Err(std::io::Error::other("serialized view exceeds budget")); }
		self.0.extend_from_slice(bytes);Ok(bytes.len())
	}
	fn flush(&mut self) -> std::io::Result<()> {Ok(())}
}

fn validate_request(request: &Request) -> Result<Network> {
	ensure!(request.schema=="native-supplied-wallet-changesets-v1" && hash_name(&request.source_sql_sha256),"unbound supplied source schema");
	ensure!(!request.rows.is_empty() && request.rows.len()<=MAX_ROWS && request.min_trusted_confirmations<=100_000,"source row/config budget exceeded");
	let network=Network::from_str(&request.network).context("unsupported network")?;
	let mut previous=0;let mut bytes=0usize;
	for row in &request.rows {
		ensure!(row.id>previous && matches!(row.kind.as_str(),"rounds"|"watchman"),"duplicate/out-of-order original or unsupported wallet kind");previous=row.id;
		ensure!(row.content_hex.len()%2==0 && row.content_hex.len()<=2*ROW_BYTES,"encoded row budget exceeded");
		bytes=bytes.checked_add(row.content_hex.len()/2).context("source aggregate overflow")?;
		ensure!(bytes<=ORIGINAL_BYTES,"source aggregate byte budget exceeded");
	}
	Ok(network)
}

fn inspect(request: Request, input_sha: &str) -> Result<Value> {
	let network=validate_request(&request)?;let mut changes:BTreeMap<String,ChangeSet>=BTreeMap::new();let mut originals=Vec::new();
	for row in &request.rows {
		let raw=Vec::<u8>::from_hex(&row.content_hex).context("invalid source encoding")?;bounded_messagepack(&raw)?;
		let delta:ChangeSet=rmp_serde::from_slice(&raw).context("invalid original ChangeSet")?;
		ensure!(delta.network.is_none_or(|n|n==network),"original network differs");
		originals.push(json!({"id":row.id,"kind":row.kind,"bytes":raw.len(),"sha256":digest(&raw)}));
		if let Some(aggregate)=changes.get_mut(&row.kind) {
			ensure!(aggregate.descriptor.is_none() || delta.descriptor.is_none() || aggregate.descriptor==delta.descriptor,"original descriptor changed");
			ensure!(aggregate.change_descriptor.is_none() || delta.change_descriptor.is_none() || aggregate.change_descriptor==delta.change_descriptor,"original change descriptor changed");
			aggregate.merge(delta);
		} else {changes.insert(row.kind.clone(),delta);}
	}
	let mut views=Vec::new();
	for (kind,aggregate) in changes {
		let mut encoded=Limited(Vec::new());serde_json::to_writer(&mut encoded,&aggregate)?;let aggregate_sha=digest(&encoded.0);
		ensure!(aggregate.local_chain.len()<=MAX_NODES && aggregate.tx_graph.txs.len()<=MAX_ROWS && aggregate.tx_graph.txouts.len()<=MAX_NODES && aggregate.tx_graph.anchors.len()<=MAX_NODES,"merged graph budget exceeded");
		let has_change=aggregate.change_descriptor.is_some();
		let wallet=Wallet::load().check_network(network).load_wallet_no_persist(aggregate)?.context("original public wallet incomplete")?;
		let checkpoints=wallet.latest_checkpoint().iter().map(|p|p.block_id()).collect::<Vec<_>>();ensure!(checkpoints.len()<=MAX_NODES,"checkpoint budget exceeded");
		let tip=wallet.latest_checkpoint().block_id();let mut descriptors=Vec::new();
		for keychain in [KeychainKind::External,KeychainKind::Internal] {
			if keychain==KeychainKind::Internal && !has_change {continue;}
			let descriptor=wallet.public_descriptor(keychain).to_string();ensure!(!descriptor.contains("prv"),"private descriptor refused");
			descriptors.push(json!({"keychain":format!("{keychain:?}"),"descriptor":descriptor,"last_revealed_index":wallet.derivation_index(keychain)}));
		}
		ensure!(wallet.tx_graph().full_txs().take(MAX_ROWS+1).count()<=MAX_ROWS,"graph transaction budget exceeded");
		ensure!(wallet.tx_graph().all_txouts().take(MAX_NODES+1).count()<=MAX_NODES,"graph output budget exceeded");
		let mut transactions=Vec::new();let mut spends:BTreeMap<OutPoint,BTreeSet<String>>=BTreeMap::new();
		for node in wallet.tx_graph().full_txs() {
			let raw=serialize(node.tx.as_ref());ensure!(raw.len()<=ROW_BYTES,"graph transaction byte budget exceeded");
			for input in &node.tx.input {if !input.previous_output.is_null() {spends.entry(input.previous_output).or_default().insert(node.txid.to_string());}}
			transactions.push(json!({"txid":node.txid,"wtxid":node.tx.compute_wtxid(),"raw_sha256":digest(&raw),"signed_weight_wu":node.tx.weight().to_wu(),"coinbase":node.tx.is_coinbase(),
				"inputs":node.tx.input.iter().map(|i|i.previous_output.to_string()).collect::<Vec<_>>(),
				"outputs":node.tx.output.iter().enumerate().map(|(vout,o)|json!({"vout":vout,"value_sat":o.value.to_sat(),"script_pubkey":o.script_pubkey.as_bytes().to_lower_hex_string()})).collect::<Vec<_>>(),"persisted_anchors":node.anchors}));
		}
		transactions.sort_by_key(|v|v["txid"].as_str().unwrap().to_owned());
		let mut graph_bytes=Limited(Vec::new());serde_json::to_writer(&mut graph_bytes,&transactions)?;let graph_sha=digest(&graph_bytes.0);let mut owned=Vec::new();let mut unknown=Vec::new();
		for (point,output) in wallet.tx_graph().all_txouts() {
			if let Some((keychain,index))=wallet.derivation_of_spk(output.script_pubkey.clone()) {
				if wallet.tx_graph().get_tx(point.txid).is_none() {unknown.push(point.to_string());}
				owned.push(json!({"outpoint":point.to_string(),"value_sat":output.value.to_sat(),"script_pubkey":output.script_pubkey.as_bytes().to_lower_hex_string(),"keychain":format!("{keychain:?}"),"derivation_index":index,"known_spending_txids":spends.get(&point).cloned().unwrap_or_default()}));
			}
		}
		owned.sort_by_key(|v|v["outpoint"].as_str().unwrap().to_owned());
		let pending=wallet.transactions().filter(|t|!t.chain_position.is_confirmed()).map(|t|t.tx_node.txid.to_string()).collect::<Vec<_>>();
		views.push(json!({"wallet":kind,"network":wallet.network().to_string(),"aggregate_changeset_sha256":aggregate_sha,"graph_sha256":graph_sha,"descriptors":descriptors,
			"persisted_tip":{"height":tip.height,"hash":tip.hash},"persisted_checkpoints":checkpoints.iter().map(|b|json!({"height":b.height,"hash":b.hash})).collect::<Vec<_>>(),
			"graph_transactions":transactions,"known_spent_inputs":spends.into_iter().map(|(p,t)|json!({"outpoint":p.to_string(),"known_spending_txids":t})).collect::<Vec<_>>(),"owned_graph_outputs":owned,
			"canonical_persisted_pending_txids":pending,"owned_outputs_without_full_graph_body":unknown,"runtime_locks":null,"persisted_lock_source":null,
			"lock_scope":"BDK ChangeSet has no native locked_outputs index; exact durable SQL commitments and stopped-process proof required separately"}));
	}
	Ok(json!({"schema":"native-persisted-wallet-graph-view-v1","result":"PERSISTED_GRAPH_VIEW_ONLY_NOT_LIVE_RPC","input_sha256":input_sha,"source_sql_sha256":request.source_sql_sha256,
		"original_rows":originals,"wallets":views,"native_min_trusted_confirmations_supplied":request.min_trusted_confirmations,
		"current_Core_or_maturity_verdict":null,"live_runtime_reservation_or_eligibility_verdict":null,"scope":"exact supplied persisted graph/descriptor view; independent active-chain/Core/unseen-credit/SQL-commitment/process-stop joins required; no signing, reveal, sync, database or persistence"}))
}

fn run() -> Result<()> {
	let args=std::env::args().collect::<Vec<_>>();ensure!(args.len()==3,"usage: supplied-wallet-view INPUT EXPECTED_SHA256");
	let path=Path::new(&args[1]);ensure!(path.is_absolute() && hash_name(&args[2]),"absolute hash-bound source required");
	let before=std::fs::symlink_metadata(path)?;let uid=std::fs::metadata("/proc/self")?.uid();
	ensure!(before.is_file() && before.nlink()==1 && before.uid()==uid && before.mode()&0o777==0o600 && before.len()<=INPUT_BYTES as u64,"private original metadata refused");
	let mut file=OpenOptions::new().read(true).open(path)?;let opened=file.metadata()?;ensure!((opened.dev(),opened.ino())==(before.dev(),before.ino()),"original changed while opening");
	let mut raw=Vec::new();Read::by_ref(&mut file).take((INPUT_BYTES+1)as u64).read_to_end(&mut raw)?;ensure!(raw.len()<=INPUT_BYTES,"input byte budget exceeded");
	let after=std::fs::symlink_metadata(path)?;ensure!((after.dev(),after.ino(),after.len(),after.mtime(),after.mtime_nsec())==(opened.dev(),opened.ino(),raw.len()as u64,opened.mtime(),opened.mtime_nsec()),"original metadata changed");
	ensure!(digest(&raw)==args[2],"original SHA mismatch");let request:Request=serde_json::from_slice(&raw)?;let value=inspect(request,&args[2])?;
	let mut output=Limited(Vec::new());serde_json::to_writer(&mut output,&value)?;output.write_all(b"\n")?;std::io::stdout().write_all(&output.0)?;Ok(())
}
fn main() {if run().is_err(){eprintln!("supplied persisted wallet view refused; inspect bound originals and source contract");std::process::exit(1);}}

#[cfg(test)]
mod tests {
	use super::*;
	#[test] fn messagepack_accepts_complete_named_value(){bounded_messagepack(&[0x81,0xa1,b'a',0x91,0xc0]).unwrap();}
	#[test] fn messagepack_refuses_oversized_or_truncated_shapes(){for raw in [&[0xdd,0xff,0xff,0xff,0xff][..],&[0xc6,0xff,0xff,0xff,0xff][..],&[0x91][..],&[0xc0,0xc0][..]]{assert!(bounded_messagepack(raw).is_err());}}
	#[test] fn messagepack_refuses_deep_nested_value(){let mut raw=vec![0x91;66];raw.push(0xc0);assert!(bounded_messagepack(&raw).is_err());}
	#[test] fn source_manifest_refuses_duplicate_or_foreign_rows(){let mut r=Request{schema:"native-supplied-wallet-changesets-v1".into(),source_sql_sha256:"a".repeat(64),network:"signet".into(),min_trusted_confirmations:1,rows:vec![Original{id:1,kind:"rounds".into(),content_hex:"c0".into()},Original{id:1,kind:"rounds".into(),content_hex:"c0".into()}]};assert!(validate_request(&r).is_err());r.rows.pop();r.rows[0].kind="unknown".into();assert!(validate_request(&r).is_err());}
	#[test] fn wrong_native_network_is_refused_before_wallet_load(){let c=ChangeSet{network:Some(Network::Bitcoin),..Default::default()};let raw=rmp_serde::to_vec_named(&c).unwrap();let r=Request{schema:"native-supplied-wallet-changesets-v1".into(),source_sql_sha256:"a".repeat(64),network:"signet".into(),min_trusted_confirmations:1,rows:vec![Original{id:1,kind:"rounds".into(),content_hex:raw.to_lower_hex_string()}]};assert!(inspect(r,&"b".repeat(64)).is_err());}
	#[test] fn serialized_output_fails_closed_at_limit(){let mut b=Limited(vec![0;OUTPUT_BYTES]);assert!(b.write_all(b"x").is_err());}
}
