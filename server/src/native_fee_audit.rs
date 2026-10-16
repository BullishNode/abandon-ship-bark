//! Read-only projection of one already captured private source packet.
//! This checks original byte/SQL reciprocity only. It does not authenticate the
//! transport, open a fee owner, query Core, establish current liabilities, or
//! authorize publication. Legacy DirectRecord closure is deliberately excluded.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::Context;
use ark::{ProtocolEncoding, ServerVtxo, Vtxo};
use ark::vtxo::Full;
use bitcoin::{BlockHash, Network, Transaction};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::database::native_urgent_publication::validate_captured_urgent_intent;
use crate::watchman::ordinary_fee::{OrdinaryFeePolicy, validate_captured_joint_history};
use crate::watchman::ordinary_fee_auth::FeeServiceIdentity;
use crate::watchman::ordinary_fee_opening::decode_captured_original;
use crate::watchman::ordinary_round_rights::{self, CapturedRoundInput, CapturedRoundOutput,
	CapturedRoundParticipation, CapturedRoundProjection};

const MAX_REQUEST: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const MAX_BODY: usize = 4 * 1024 * 1024;
const FORMAT: &str = "c-native-captured-audit-v1";
const IDENTITY: &[&str] = &["snapshot", "backend_pid", "database", "role", "read_only", "isolation"];

// serde_json::Value normally accepts duplicate object keys. Reject them at ALL
// depths before projecting closed SQL objects or deserializing native originals.
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
	fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		struct V;
		impl<'de> Visitor<'de> for V {
			type Value = Unique;
			fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("JSON without duplicate keys") }
			fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique,E> { Ok(Unique(v.into())) }
			fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique,E> { Ok(Unique(v.into())) }
			fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique,E> { Ok(Unique(v.into())) }
			fn visit_f64<E: de::Error>(self, _: f64) -> Result<Unique,E> { Err(E::custom("fractional JSON values are outside captured SQL ABI")) }
			fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique,E> { Ok(Unique(v.into())) }
			fn visit_string<E: de::Error>(self, v: String) -> Result<Unique,E> { Ok(Unique(v.into())) }
			fn visit_none<E: de::Error>(self) -> Result<Unique,E> { Ok(Unique(Value::Null)) }
			fn visit_unit<E: de::Error>(self) -> Result<Unique,E> { Ok(Unique(Value::Null)) }
			fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique,A::Error> {
				let mut values = Vec::new();
				while let Some(v) = a.next_element::<Unique>()? { values.push(v.0); }
				Ok(Unique(Value::Array(values)))
			}
			fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique,A::Error> {
				let mut values = Map::new();
				while let Some((k,v)) = a.next_entry::<String,Unique>()? {
					if values.insert(k,v.0).is_some() { return Err(de::Error::custom("duplicate captured JSON field")); }
				}
				Ok(Unique(Value::Object(values)))
			}
		}
		d.deserialize_any(V)
	}
}
fn json(raw: &[u8]) -> anyhow::Result<Value> {
	ensure!(raw.len() <= MAX_REQUEST, "captured JSON exceeds unchanged 32MiB bound");
	Ok(serde_json::from_slice::<Unique>(raw)?.0)
}
fn object<'a>(v: &'a Value, keys: &[&str]) -> anyhow::Result<&'a Map<String,Value>> {
	let row = v.as_object().context("captured closed object required")?;
	ensure!(row.len() == keys.len() && keys.iter().all(|key|row.contains_key(*key)), "captured closed field set differs");
	Ok(row)
}
fn text(v: &Value) -> anyhow::Result<&str> { v.as_str().context("captured SQL text required") }
fn nullable_text(v: &Value) -> anyhow::Result<Option<&str>> { if v.is_null() { Ok(None) } else { Ok(Some(text(v)?)) } }
fn integer(v: &Value) -> anyhow::Result<i64> { v.as_i64().context("captured SQL integer required") }
fn positive(v: &Value) -> anyhow::Result<i64> { let n=integer(v)?; ensure!(n>0,"captured positive ID required"); Ok(n) }
fn nonnegative(v: &Value) -> anyhow::Result<u64> { Ok(u64::try_from(integer(v)?)?) }
fn nullable_integer(v: &Value) -> anyhow::Result<Option<i64>> { if v.is_null() { Ok(None) } else { Ok(Some(integer(v)?)) } }
fn hex(raw: &Value, empty: bool) -> anyhow::Result<Vec<u8>> {
	let raw = text(raw)?;
	ensure!(raw.len() <= MAX_BODY*2 && raw.len()%2 == 0 && (empty || !raw.is_empty())
		&& raw.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "captured bytea must be bounded lowercase hex");
	let mut bytes = Vec::with_capacity(raw.len()/2);
	for pair in raw.as_bytes().chunks_exact(2) {
		let digit=|b:u8| if b<=b'9' { b-b'0' } else { b-b'a'+10 };
		bytes.push(digit(pair[0])*16+digit(pair[1]));
	}
	Ok(bytes)
}
fn transaction(v: &Value) -> anyhow::Result<Transaction> {
	let raw=hex(v,false)?; let tx:Transaction=deserialize(&raw)?;
	ensure!(serialize(&tx)==raw,"captured transaction encoding differs"); Ok(tx)
}
fn files(v:&Value) -> anyhow::Result<Option<BTreeMap<String,String>>> {
	if v.is_null() { return Ok(None); }
	let values=v.as_object().context("captured original file map required")?;
	ensure!(values.len()<=4098,"captured original file count exceeds bound");
	values.iter().map(|(name,v)| {
		ensure!(name.len()<=128 && !name.contains('/') && !name.contains('\\') && name!="." && name!="..", "captured original filename differs");
		let raw=text(v)?;ensure!(raw.len()<=MAX_BODY,"captured original file exceeds bound");
		Ok((name.clone(),raw.to_owned()))
	}).collect::<anyhow::Result<BTreeMap<_,_>>>().map(Some)
}
fn combined_witness_bound(publications:Option<&BTreeMap<String,String>>,rights:Option<&BTreeMap<String,String>>)->anyhow::Result<()> {
	let p=publications.map_or(0,BTreeMap::len);
	let r=match rights {Some(files)=>{ensure!(files.len()>=2,"original Round metadata absent");files.len()-2},None=>0};
	ensure!(p.checked_add(r).is_some_and(|n|n<=4096),"combined original publication/Round count exceeds same bound");
	let mut bytes=0usize;
	if let Some(files) = publications {
		for raw in files.values(){bytes=bytes.checked_add(raw.len()).context("combined original byte overflow")?;}
	}
	if let Some(files) = rights {
		for (name,raw) in files {if name!="identity.json"&&name!="history"{bytes=bytes.checked_add(raw.len()).context("combined original byte overflow")?;}}
	}
	ensure!(bytes<=64*1024*1024,"combined original publication/Round bytes exceed same bound");
	Ok(())
}

fn rows<'a>(pg:&'a Value,table:&str,keys:&[&str]) -> anyhow::Result<Vec<&'a Map<String,Value>>> {
	let values=pg.get(table).and_then(Value::as_array).context("complete captured SQL table required")?;
	ensure!(values.len()<=MAX_ROWS,"captured table exceeds complete row bound");
	values.iter().map(|v|object(v,keys)).collect()
}
fn unique<'a>(rows:&[&'a Map<String,Value>],key:&str) -> anyhow::Result<BTreeMap<String,&'a Map<String,Value>>> {
	let mut result=BTreeMap::new();
	for row in rows { let id=text(&row[key])?.to_owned();ensure!(result.insert(id,*row).is_none(),"duplicate captured SQL primary key"); }
	Ok(result)
}
fn same_cut(joint:&Value,body:&Value) -> anyhow::Result<()> {
	for key in IDENTITY { ensure!(joint.get(*key)==body.get(*key),"private captured SQL is not one original RR cut"); }
	ensure!(text(&joint["database"])?=="trackc" && text(&joint["role"])?=="trackcbodyobserver"
		&& text(&joint["read_only"])?=="on" && text(&joint["isolation"])?=="repeatable read"
		&& positive(&joint["backend_pid"])? > 0 && !text(&joint["snapshot"])?.is_empty(),"captured SQL source identity differs");
	Ok(())
}

#[derive(Serialize)]
pub struct CapturedAudit {
	format: &'static str,
	request_sha256: sha256::Hash,
	charged_sat: u64,
	carrier_charged_sat: u64,
	ordinary_publications: usize,
	urgent_attempts: usize,
	urgent_signed: usize,
	round_rights: usize,
	pending_participations: usize,
	coin_rows: usize,
	complete: bool,
	qualified: bool,
	financial_readiness: bool,
	scope: &'static str,
}

/// Independently admitted caller must bind the exact input bytes, native ELF,
/// source/runtime closure, original source references and actual Core cut. No
/// authority is obtained by providing the expected fields inside this request.
pub fn audit_captured(raw:&[u8]) -> anyhow::Result<CapturedAudit> {
	let source=json(raw)?;
	let request=object(&source,&["format","expected","policy_utf8","history_utf8","publication_files",
		"round_rights_files","opening_files","joint_rows","body_rows"])?;
	ensure!(text(&request["format"])?==FORMAT,"captured native format differs");
	let expected=object(&request["expected"],&["server_pubkey","genesis","policy_sha256","opening_sha256"])?;
	let identity=FeeServiceIdentity { server_pubkey:text(&expected["server_pubkey"])?.parse::<PublicKey>()?,
		genesis:text(&expected["genesis"])?.parse::<BlockHash>()?,
		policy_sha256:text(&expected["policy_sha256"])?.parse::<sha256::Hash>()?,
		opening_sha256:text(&expected["opening_sha256"])?.parse::<sha256::Hash>()? };
	ensure!(text(&expected["server_pubkey"])?==identity.server_pubkey.to_string()
		&& text(&expected["genesis"])?==identity.genesis.to_string()
		&& text(&expected["policy_sha256"])?==identity.policy_sha256.to_string()
		&& text(&expected["opening_sha256"])?==identity.opening_sha256.to_string()
		&& text(&expected["server_pubkey"])?.len()==66
		&& identity.opening_sha256!=sha256::Hash::all_zeros(),"captured audit requires original opening binding");
	let network=[Network::Bitcoin,Network::Testnet,Network::Testnet4,Network::Signet,Network::Regtest].into_iter()
		.find(|n|bitcoin::constants::genesis_block(*n).block_hash()==identity.genesis).context("native genesis is not an actual network BlockHash")?;
	let policy_raw=text(&request["policy_utf8"])?;
	ensure!(policy_raw.len()<=1024,"captured policy exceeds original bound");
	let policy:OrdinaryFeePolicy=serde_json::from_value(json(policy_raw.as_bytes())?)?;
	policy.validate_config(network)?;
	let canonical_policy=serde_json::to_vec(&policy)?;
	ensure!(sha256::Hash::hash(&canonical_policy)==identity.policy_sha256,"canonical native policy hash differs from expected identity");
	let history=text(&request["history_utf8"])?;
	let publication_files=files(&request["publication_files"])?;
	let rights=files(&request["round_rights_files"])?;
	combined_witness_bound(publication_files.as_ref(),rights.as_ref())?;
	let empty=BTreeMap::new();
	let originals=validate_captured_joint_history(history,policy,publication_files.as_ref().unwrap_or(&empty))?;
	let joint=&request["joint_rows"];let body=&request["body_rows"];
	let mut joint_fields=IDENTITY.to_vec();joint_fields.extend(["native_fee_publication","native_fee_input","native_fee_opening",
		"native_urgent_attempt","native_urgent_input","nursery_tx","wallet_changeset","expiry_carrier","rounds",
		"round_participation","round_part_input","round_part_output","virtual_transaction"]);
	object(joint,&joint_fields)?;
	let mut body_fields=IDENTITY.to_vec();body_fields.extend(["coins","pending_boards","pool","pool_retirements","payment_links"]);
	object(body,&body_fields)?;same_cut(joint,body)?;

	let opening_files=files(&request["opening_files"])? .context("captured original opening files absent")?;
	ensure!(opening_files.len()==2 && opening_files.contains_key("certificate.json")
		&& opening_files.get("validated.sha256").is_some_and(|v|*v==identity.opening_sha256.to_string()),"original opening sibling set or validated digest differs");
	let certificate=opening_files["certificate.json"].as_bytes();
	let (old_history,old_records)=decode_captured_original(certificate,identity.opening_sha256,policy,identity.server_pubkey,network)?;
	ensure!(history.as_bytes().starts_with(&old_history),"captured journal lost original opening prefix");
	for old in old_records { let id=old.entry(policy)?.id();ensure!(originals.publications.get(&id)==Some(&old),"captured journal lost original imported publication"); }
	let opening=rows(joint,"native_fee_opening",&["id","opening_hash","policy_bytes_hex","opening_bytes_hex","history_identity","created_at"])?;
	ensure!(opening.len()==1,"captured native opening set is not singleton");let opening=opening[0];
	ensure!(integer(&opening["id"])?==1 && text(&opening["opening_hash"])?==identity.opening_sha256.to_string()
		&& hex(&opening["policy_bytes_hex"],false)?==canonical_policy && hex(&opening["opening_bytes_hex"],false)?==certificate
		&& !text(&opening["history_identity"])?.is_empty(),"captured SQL opening differs from original files or policy");
	text(&opening["created_at"])?;

	let nursery_rows=rows(joint,"nursery_tx",&["txid","kind","tx_hex","confirm_target_height","confirmed_at_height","abandoned_at","created_at","updated_at"])?;
	let nursery=unique(&nursery_rows,"txid")?;
	for row in &nursery_rows {
		ensure!(transaction(&row["tx_hex"])?.compute_txid().to_string()==text(&row["txid"])?,"nursery transaction identity differs");
		text(&row["kind"])?;nullable_integer(&row["confirm_target_height"])?;nullable_integer(&row["confirmed_at_height"])?;
		nullable_text(&row["abandoned_at"])?;text(&row["created_at"])?;text(&row["updated_at"])?;
	}
	let ordinary_rows=rows(joint,"native_fee_publication",&["wtxid","txid","purpose","record_hash","policy_hash","opening_hash","signed_tx_hex","created_at","published_at"])?;
	let ordinary=unique(&ordinary_rows,"wtxid")?;
	ensure!(ordinary.len()==originals.publications.len(),"complete SQL/native ordinary publication sets differ");
	let mut input_owners=BTreeMap::new();let mut published_rounds=BTreeSet::new();let mut native_nursery=BTreeSet::new();
	for (id,record) in &originals.publications {
		let key=id.to_string();let row=ordinary.get(&key).context("original ordinary publication absent in captured SQL")?;
		let entry=&originals.publication_entries[id];let tx=record.transaction()?;let txid=tx.compute_txid().to_string();
		ensure!(text(&row["txid"])?==txid && text(&row["purpose"])?==entry.purpose()
			&& text(&row["record_hash"])?==entry.record_hash().to_string() && text(&row["policy_hash"])?==identity.policy_sha256.to_string()
			&& record.policy_hash()==identity.policy_sha256 && text(&row["opening_hash"])?==identity.opening_sha256.to_string()
			&& hex(&row["signed_tx_hex"],false)?==serialize(&tx),"captured ordinary row differs from independent original");
		text(&row["created_at"])?;
		let published=nullable_text(&row["published_at"])?.is_some();
		if published {
			let n=nursery.get(&txid).context("published original ordinary candidate lost nursery tuple")?;
			let kind=if entry.purpose()=="round" {"round"} else {"vtxopool"};
			ensure!(text(&n["kind"])?==kind && hex(&n["tx_hex"],false)?==serialize(&tx)
				&& n["abandoned_at"].is_null(),"published ordinary original nursery tuple differs");
			native_nursery.insert(txid.clone());
			if entry.purpose()=="round" { published_rounds.insert(txid.clone()); }
		} else { ensure!(!nursery.contains_key(&txid),"unpublished original candidate has unbound nursery body"); }
		for input in &tx.input { ensure!(input_owners.insert(input.previous_output.to_string(),key.clone()).is_none(),"ordinary original whole inputs overlap"); }
	}
	let mut actual_inputs=BTreeMap::new();
	for row in rows(joint,"native_fee_input",&["outpoint","wtxid"])? {
		ensure!(actual_inputs.insert(text(&row["outpoint"])?.to_owned(),text(&row["wtxid"])?.to_owned()).is_none(),"duplicate ordinary SQL input");
	}
	ensure!(actual_inputs==input_owners,"ordinary complete whole-input SQL union differs");

	let urgent_rows=rows(joint,"native_urgent_attempt",&["attempt_index","candidate_hash","publication_txid","purpose","policy_hash","opening_hash","reservation_entry_hex","candidate_hex","signed_entry_hex","signed_record_hex","created_at"])?;
	ensure!(urgent_rows.len()==originals.urgent.len(),"complete captured urgent attempt set differs");
	let mut seen_urgent=BTreeSet::new();let mut urgent_inputs=BTreeMap::new();
	for row in urgent_rows {
		let index=u64::try_from(positive(&row["attempt_index"])?)?;
		ensure!(seen_urgent.insert(index),"duplicate urgent SQL attempt");
		let (candidate,signed)=originals.urgent.get(&index).context("SQL urgent attempt missing original journal")?;
		let dto=UrgentDto { attempt_index:i64::try_from(index)?,candidate_hash:text(&row["candidate_hash"])?.into(),
			publication_txid:text(&row["publication_txid"])?.into(),purpose:text(&row["purpose"])?.into(),
			policy_hash:text(&row["policy_hash"])?.into(),opening_hash:text(&row["opening_hash"])?.into(),
			reservation_entry:hex(&row["reservation_entry_hex"],false)?,candidate:hex(&row["candidate_hex"],false)?,
			signed_entry:nullable_hex(&row["signed_entry_hex"])?,signed_record:nullable_hex(&row["signed_record_hex"])? };
		validate_captured_urgent_intent(&serde_json::to_vec(&dto)?,candidate,signed.as_ref(),&identity,policy)?;
		text(&row["created_at"])?;
		if let Some(signed)=signed {
			let tx=signed.last_transaction()?;let txid=tx.compute_txid().to_string();let n=nursery.get(&txid).context("signed urgent publication lost original nursery tuple")?;
			ensure!(text(&n["kind"])?=="native-urgent" && hex(&n["tx_hex"],false)?==serialize(&tx)
				&& n["abandoned_at"].is_null(),"urgent nursery original body or state differs");native_nursery.insert(txid);
		} else { ensure!(!nursery.contains_key(&dto.publication_txid),"unsigned urgent attempt has unbound nursery body"); }
		for point in candidate.external_inputs(policy)? {
			let point=point.to_string();ensure!(!input_owners.contains_key(&point)
				&& urgent_inputs.insert(point,index).is_none(),"urgent/ordinary original whole inputs overlap");
		}
	}
	let mut actual_urgent=BTreeMap::new();
	for row in rows(joint,"native_urgent_input",&["outpoint","attempt_index"])? {
		ensure!(actual_urgent.insert(text(&row["outpoint"])?.into(),u64::try_from(positive(&row["attempt_index"])?)?).is_none(),"duplicate urgent SQL input");
	}
	ensure!(actual_urgent==urgent_inputs,"urgent complete whole-input SQL union differs");
	let actual_nursery=nursery.iter().filter_map(|(id,r)|r["kind"].as_str().filter(|kind|["round","vtxopool","native-urgent"].contains(kind)).map(|_|id.clone())).collect::<BTreeSet<_>>();
	ensure!(actual_nursery==native_nursery,"native nursery complete original publication set differs");

	validate_auxiliary_rows(joint)?;
	let coins=validate_body_rows(body,&identity)?;
	let (projections,pending)=round_projections(joint,&coins,&identity)?;
	let round_rights=ordinary_round_rights::verify_captured_rounds(policy,&identity,&originals.publications,rights.as_ref(),&projections,&published_rounds)?;
	Ok(CapturedAudit {format:FORMAT,request_sha256:sha256::Hash::hash(raw),charged_sat:originals.charged_sat,
		carrier_charged_sat:originals.carrier_charged_sat,ordinary_publications:originals.publications.len(),
		urgent_attempts:originals.urgent.len(),urgent_signed:originals.urgent_signed_entries.len(),round_rights,
		pending_participations:pending,coin_rows:coins.len(),complete:false,qualified:false,financial_readiness:false,
		scope:"Captured native byte/SQL reciprocity only; legacy DirectRecord closure, source authority, current ownership, liquidity and financial readiness excluded"})
}
#[derive(Serialize)]
struct UrgentDto {
	attempt_index:i64,candidate_hash:String,publication_txid:String,purpose:String,policy_hash:String,opening_hash:String,
	reservation_entry:Vec<u8>,candidate:Vec<u8>,signed_entry:Option<Vec<u8>>,signed_record:Option<Vec<u8>>,
}
fn nullable_hex(v:&Value)->anyhow::Result<Option<Vec<u8>>> {if v.is_null(){Ok(None)}else{Ok(Some(hex(v,false)?))}}

fn validate_auxiliary_rows(joint:&Value)->anyhow::Result<()> {
	let mut ids=BTreeSet::new();
	for row in rows(joint,"wallet_changeset",&["id","kind","content_hex"])? {
		ensure!(ids.insert(positive(&row["id"])?),"duplicate wallet changeset ID");text(&row["kind"])?;
		// An explicit unknown changeset cannot be relabelled as complete bytes.
		hex(&row["content_hex"],true)?;
	}
	let carriers=rows(joint,"expiry_carrier",&["txid","signed_tx_hex","backing_ids","fee_inputs","miner_fee_sat","funding_subsidy_sat","confirmed_height","confirmed_block"])?;
	unique(&carriers,"txid")?;
	for row in carriers {
		ensure!(transaction(&row["signed_tx_hex"])?.compute_txid().to_string()==text(&row["txid"])?,"carrier transaction identity differs");
		let mut ids=BTreeSet::new();
		for id in row["backing_ids"].as_array().context("carrier complete backing array required")? {
			ensure!(ids.insert(text(id)?),"duplicate carrier backing identifier");
		}
		let mut points=BTreeSet::new();
		for input in row["fee_inputs"].as_array().context("carrier complete fee input array required")? {
			let input=object(input,&["outpoint","value_sat","script_pubkey"])?;
			ensure!(points.insert(text(&input["outpoint"])?),"duplicate carrier fee input");
			nonnegative(&input["value_sat"])?;hex(&input["script_pubkey"],true)?;
		}
		nonnegative(&row["miner_fee_sat"])?;nonnegative(&row["funding_subsidy_sat"])?;
		ensure!(row["confirmed_height"].is_null()==row["confirmed_block"].is_null(),"partial carrier confirmation tuple");
		nullable_integer(&row["confirmed_height"])?;nullable_text(&row["confirmed_block"])?;
	}
	Ok(())
}

struct Coin { raw:Vec<u8>, anchor:String, spent_round:Option<i64> }
fn validate_body_rows(body:&Value,identity:&FeeServiceIdentity)->anyhow::Result<BTreeMap<String,Coin>> {
	let mut coins=BTreeMap::new();let mut native_ids=BTreeSet::new();let mut all_pool_ids=BTreeSet::new();
	for row in rows(body,"coins",&["metadata","source_encoding","body_hex","native_id","lightning_htlc_subscription_id"])? {
		ensure!(text(&row["source_encoding"])?=="ServerVtxoFull","stored coin source encoding differs");
		let m=object(&row["metadata"],&["vtxo_id","amount","expiry","spend_state","policy_type","policy","anchor_point","vtxo_txid","oor_spent_txid","spent_in_round","offboarded_in","frontier_at","confirmed_height","onchain_spent_height","onchain_spent_txid"])?;
		let raw=hex(&row["body_hex"],false)?;let v=ServerVtxo::<Full>::deserialize(&raw)?;
		ensure!(v.serialize()==raw && v.server_pubkey()==identity.server_pubkey && v.id().to_string()==text(&m["vtxo_id"])?
			&& v.chain_anchor().to_string()==text(&m["anchor_point"])? && v.amount().to_sat()==nonnegative(&m["amount"])?
			&& u64::from(v.expiry_height().to_u32())==nonnegative(&m["expiry"])? && native_ids.insert(positive(&row["native_id"])?),"stored native Full metadata differs");
		text(&m["spend_state"])?;
		ensure!(text(&m["policy_type"])?==v.policy_type().to_string()
			&& hex(&m["policy"],false)?==v.policy().serialize()
			&& text(&m["vtxo_txid"])?==v.point().txid.to_string(),"stored native policy or transaction metadata differs");
		for field in ["oor_spent_txid","offboarded_in","frontier_at","onchain_spent_txid"] {nullable_text(&m[field])?;}
		for field in ["confirmed_height","onchain_spent_height"] {nullable_integer(&m[field])?;}
		nullable_integer(&row["lightning_htlc_subscription_id"])?;
		ensure!(coins.insert(v.id().to_string(),Coin {raw,anchor:text(&m["anchor_point"])?.into(),spent_round:nullable_integer(&m["spent_in_round"])?}).is_none(),"duplicate native coin ID");
	}
	for (table,kind) in [("pending_boards",true),("pool",false)] {
		let keys=if kind {vec!["metadata","source_encoding","body_hex","anchor_point"]}else{vec!["metadata","source_encoding","body_hex","spent_at"]};
		let mut ids=BTreeSet::new();let mut pool_ids=BTreeSet::new();
		for row in rows(body,table,&keys)? {
			ensure!(text(&row["source_encoding"])?=="VtxoFull","pending/pool native encoding differs");
			let m=object(&row["metadata"],if kind {&["vtxo_id","expiry"]}else{&["id","vtxo_id","expiry_height","amount"]})?;
			let raw=hex(&row["body_hex"],false)?;let v=Vtxo::<Full>::deserialize(&raw)?;
			ensure!(v.serialize()==raw && v.server_pubkey()==identity.server_pubkey && v.id().to_string()==text(&m["vtxo_id"])?
				&& ids.insert(v.id().to_string()) && u64::from(v.expiry_height().to_u32())==nonnegative(&m[if kind {"expiry"}else{"expiry_height"}])?,"pending/pool original body metadata differs");
			if kind {ensure!(v.chain_anchor().to_string()==text(&row["anchor_point"])?,"pending original funding anchor differs");}
			else {all_pool_ids.insert(v.id().to_string());ensure!(pool_ids.insert(positive(&m["id"])?) && v.amount().to_sat()==nonnegative(&m["amount"])?,"pool identity or amount differs");nullable_text(&row["spent_at"])?;}
		}
	}
	let mut retired=BTreeSet::new();for row in rows(body,"pool_retirements",&["vtxo_id","pool_input_id"])? {
		ensure!(retired.insert(text(&row["vtxo_id"])?) && coins.contains_key(text(&row["vtxo_id"])?)
			&& all_pool_ids.contains(text(&row["pool_input_id"])?),"duplicate or orphan pool retirement");
	}
	let mut links=BTreeSet::new();for row in rows(body,"payment_links",&["lightning_payment_attempt_id","vtxo_id"])? {
		ensure!(links.insert((positive(&row["lightning_payment_attempt_id"])?,text(&row["vtxo_id"])?))
			&& coins.contains_key(text(&row["vtxo_id"])?),"duplicate or orphan payment association");
	}
	Ok(coins)
}

fn spent_round_txid(id:Option<i64>,round_ids:&BTreeMap<i64,String>,expected:&str)->Option<String> {
	id.and_then(|id|round_ids.get(&id)).filter(|txid|txid.as_str()==expected).cloned()
}

fn round_projections(joint:&Value,coins:&BTreeMap<String,Coin>,identity:&FeeServiceIdentity)
	->anyhow::Result<(BTreeMap<String,CapturedRoundProjection>,usize)> {
	let round_rows=rows(joint,"rounds",&["id","seq","expiry","funding_txid","funding_tx_hex","signed_tree_hex"])?;
	let rounds=unique(&round_rows,"funding_txid")?;let mut round_ids=BTreeMap::new();
	for row in &round_rows {ensure!(round_ids.insert(positive(&row["id"])?,text(&row["funding_txid"])?.to_owned()).is_none(),"duplicate actual Round primary ID");}
	let virtual_rows=rows(joint,"virtual_transaction",&["txid","signed_tx_hex"])?;
	let virtuals=unique(&virtual_rows,"txid")?;
	for row in &virtual_rows {if !row["signed_tx_hex"].is_null(){ensure!(transaction(&row["signed_tx_hex"])?.compute_txid().to_string()==text(&row["txid"])?,"virtual signed transaction ID differs");}}
	let part_rows=rows(joint,"round_participation",&["id","round_id","unlock_hash"])?;
	let mut parts=BTreeMap::new();let mut pending=0;
	for row in part_rows {
		let id=positive(&row["id"])?;ensure!(parts.insert(id,row).is_none(),"duplicate participation primary ID");
		let round=nullable_text(&row["round_id"])?;let unlock=nullable_text(&row["unlock_hash"])?;
		if let Some(round)=round {ensure!(rounds.contains_key(round) && unlock.is_some(),"booked participation has unknown Round or missing unlock hash");}
		else {pending+=1;}
	}
	let inputs=rows(joint,"round_part_input",&["participation_id","vtxo_id"])?;
	let mut input_keys=BTreeSet::new();for row in &inputs {
		ensure!(parts.contains_key(&positive(&row["participation_id"])?) && input_keys.insert((integer(&row["participation_id"])?,text(&row["vtxo_id"])?)),"orphan or duplicate Round input");
		ensure!(coins.contains_key(text(&row["vtxo_id"])?),"Round input body absent from original complete source");
	}
	let outputs=rows(joint,"round_part_output",&["participation_id","policy_hex","amount","unblinded_mailbox_id"])?;
	for row in &outputs {ensure!(parts.contains_key(&positive(&row["participation_id"])?),"orphan Round output");hex(&row["policy_hex"],false)?;nonnegative(&row["amount"])?;nullable_text(&row["unblinded_mailbox_id"])?;}
	for coin in coins.values() {if let Some(id)=coin.spent_round {ensure!(round_ids.contains_key(&id),"coin consumption references missing actual Round primary ID");}}
	let mut projections=BTreeMap::new();
	for (txid,row) in rounds {
		let funding=transaction(&row["funding_tx_hex"])?;ensure!(funding.compute_txid().to_string()==txid,"Round funding transaction ID differs");
		let tree=hex(&row["signed_tree_hex"],false)?;
		let leaf_ids=ordinary_round_rights::captured_leaf_ids(&tree,&funding,identity)?;
		let mut leaves=BTreeMap::new();for id in leaf_ids {leaves.insert(id.clone(),coins.get(&id).context("Round leaf body absent from complete source")?.raw.clone());}
		let mut participations=Vec::new();
		for (id,part) in &parts {
			if nullable_text(&part["round_id"])?.is_none_or(|round|round!=txid) {continue;}
			let mut selected_inputs=Vec::new();
			for input in inputs.iter().filter(|input|input["participation_id"].as_i64()==Some(*id)) {
				let id=text(&input["vtxo_id"])?;let coin=&coins[id];
				// Actual round.id FK join; sequence is never an identity substitute.
				let spent=spent_round_txid(coin.spent_round,&round_ids,&txid);
				let v=ServerVtxo::<Full>::deserialize(&coin.raw)?.try_into_user_vtxo().map_err(|_|anyhow::anyhow!("Round input is not native user Full"))?;
				let anchor=virtuals.get(&v.chain_anchor().txid.to_string()).context("Round original anchor absent")?;
				selected_inputs.push(CapturedRoundInput {id:id.into(),original:coin.raw.clone(),stored_anchor:coin.anchor.clone(),spent_round:spent,anchor:hex(&anchor["signed_tx_hex"],false)?});
			}
			let mut selected_outputs=Vec::new();
			// Preserve the full SQL multiset, including identical output rows.
			for output in outputs.iter().filter(|output|output["participation_id"].as_i64()==Some(*id)) {
				selected_outputs.push(CapturedRoundOutput {policy:hex(&output["policy_hex"],false)?,amount:nonnegative(&output["amount"])?,mailbox:nullable_text(&output["unblinded_mailbox_id"])?.map(str::to_owned)});
			}
			participations.push(CapturedRoundParticipation {unlock_hash:text(&part["unlock_hash"])?.into(),inputs:selected_inputs,outputs:selected_outputs});
		}
		projections.insert(txid.clone(),CapturedRoundProjection {sequence:i64::try_from(nonnegative(&row["seq"])?)?,expiry:i32::try_from(nonnegative(&row["expiry"])?)?,
			funding:serialize(&funding),virtual_funding:virtuals.get(&txid).map(|v|nullable_hex(&v["signed_tx_hex"])).transpose()?.flatten(),tree,participations,leaves});
	}
	Ok((projections,pending))
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn captured_audit_refuses_duplicate_keys_at_every_depth() {
		assert!(json(br#"{"x":1,"x":2}"#).is_err());
		assert!(json(br#"{"outer":[{"id":1,"id":1}]}"#).is_err());
		assert!(json(br#"{"outer":[{"id":1,"nullable":null}]}"#).is_ok());
	}
	#[test]
	fn captured_audit_preserves_hex_null_and_exact_original_utf8() {
		assert_eq!(hex(&Value::String("000aff".into()),false).unwrap(),vec![0,10,255]);
		assert!(hex(&Value::String("000AFF".into()),false).is_err());
		assert!(hex(&Value::String("0".into()),false).is_err());
		assert!(nullable_hex(&Value::Null).unwrap().is_none());
		assert!(nullable_hex(&Value::String("".into())).is_err());
		let original=serde_json::json!({"certificate.json":"é\n "});
		assert_eq!(files(&original).unwrap().unwrap()["certificate.json"].as_bytes(),"é\n ".as_bytes());
		assert!(files(&serde_json::json!({"../certificate.json":"{}"})).is_err());
	}
	#[test]
	fn captured_audit_closed_sql_and_same_rr_never_alias() {
		let mut a=serde_json::json!({"snapshot":"1:2:","backend_pid":12,"database":"trackc","role":"trackcbodyobserver","read_only":"on","isolation":"repeatable read"});
		assert!(same_cut(&a,&a).is_ok());let b=a.clone();a["backend_pid"]=13.into();assert!(same_cut(&a,&b).is_err());
		assert!(object(&serde_json::json!({"id":1,"extra":null}),&["id"]).is_err());
		assert!(positive(&Value::Null).is_err());assert!(nonnegative(&(-1).into()).is_err());
	}
	#[test]
	fn captured_audit_round_consumption_joins_primary_id_not_sequence() {
		let ids=BTreeMap::from([(71,"actual-funding".into()),(4,"other-funding".into())]);
		assert_eq!(spent_round_txid(Some(71),&ids,"actual-funding"),Some("actual-funding".into()));
		// A sequence of 4 cannot substitute for actual primary ID 71.
		assert_eq!(spent_round_txid(Some(4),&ids,"actual-funding"),None);
		assert_eq!(spent_round_txid(None,&ids,"actual-funding"),None);
		assert_eq!(spent_round_txid(Some(99),&ids,"actual-funding"),None);
	}
	#[test]
	fn captured_audit_pending_nulls_are_unbooked_and_orphans_refuse() {
		let identity=FeeServiceIdentity {server_pubkey:"0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".parse().unwrap(),
			genesis:bitcoin::constants::genesis_block(Network::Regtest).block_hash(),policy_sha256:sha256::Hash::hash(b"policy"),opening_sha256:sha256::Hash::hash(b"opening")};
		let mut joint=serde_json::json!({"rounds":[],"virtual_transaction":[],
			"round_participation":[{"id":1,"round_id":null,"unlock_hash":null}],
			"round_part_input":[],"round_part_output":[]});
		let (booked,pending)=round_projections(&joint,&BTreeMap::new(),&identity).unwrap();
		assert!(booked.is_empty());assert_eq!(pending,1);
		joint["round_participation"][0]["round_id"]="unknown-funding".into();
		assert!(round_projections(&joint,&BTreeMap::new(),&identity).is_err());
		joint["round_participation"][0]["round_id"]=Value::Null;
		joint["round_part_output"]=serde_json::json!([{"participation_id":2,"policy_hex":"00","amount":1,"unblinded_mailbox_id":null}]);
		assert!(round_projections(&joint,&BTreeMap::new(),&identity).is_err());
	}

	#[test]
	fn captured_audit_actual_carrier_fee_input_objects_and_unknown_wallet_bytes() {
		let tx=Transaction { version:bitcoin::transaction::Version::TWO,lock_time:bitcoin::absolute::LockTime::ZERO,
			input:vec![bitcoin::TxIn {previous_output:bitcoin::OutPoint::new(bitcoin::Txid::all_zeros(),0),..Default::default()}],output:vec![] };
		let raw=serialize(&tx).iter().map(|b|format!("{b:02x}")).collect::<String>();
		let mut source=serde_json::json!({"wallet_changeset":[],"expiry_carrier":[{
			"txid":tx.compute_txid().to_string(),"signed_tx_hex":raw,"backing_ids":["original-backing"],
			"fee_inputs":[{"outpoint":tx.input[0].previous_output.to_string(),"value_sat":10000,"script_pubkey":"0014"}],
			"miner_fee_sat":1000,"funding_subsidy_sat":0,"confirmed_height":null,"confirmed_block":null}]});
		assert!(validate_auxiliary_rows(&source).is_ok());
		source["expiry_carrier"][0]["fee_inputs"]=serde_json::json!(["incorrect-string-alias"]);
		assert!(validate_auxiliary_rows(&source).is_err());
		source["expiry_carrier"]=serde_json::json!([]);
		source["wallet_changeset"]=serde_json::json!([{"id":1,"kind":"rounds","content_hex":null}]);
		assert!(validate_auxiliary_rows(&source).is_err());
	}

	#[test]
	fn captured_audit_rejects_request_bound_and_unknown_or_omitted_fields() {
		assert!(audit_captured(b"{}").is_err());
		assert!(json(&vec![b' ';MAX_REQUEST+1]).is_err());
		let source=serde_json::json!({"format":FORMAT,"expected":{},"policy_utf8":"{}","history_utf8":"",
			"publication_files":null,"round_rights_files":null,"opening_files":null,"joint_rows":{},"body_rows":{},"ready":true});
		assert!(audit_captured(&serde_json::to_vec(&source).unwrap()).is_err());
	}
	#[test]
	fn captured_joint_witness_capacity_is_one_shared_count_not_two_allowances() {
		let mut publications=(0..4096).map(|i|(i.to_string(),String::new())).collect::<BTreeMap<_,_>>();
		let mut rights=BTreeMap::from([("identity.json".into(),String::new()),("history".into(),String::new())]);
		assert!(combined_witness_bound(Some(&publications),Some(&rights)).is_ok());
		rights.insert("original.json".into(),String::new());
		assert!(combined_witness_bound(Some(&publications),Some(&rights)).is_err());
		publications.remove("0");assert!(combined_witness_bound(Some(&publications),Some(&rights)).is_ok());
		assert!(combined_witness_bound(None,Some(&BTreeMap::new())).is_err());
	}

	#[test]
	fn captured_audit_complete_byte_pipeline_retains_nonzero_history_without_financial_authority() {
		let (certificate,policy,key,network)=crate::watchman::ordinary_fee_opening::synthetic_captured_opening_facts();
		let original:Value=serde_json::from_slice(&certificate).unwrap();
		let policy_raw=serde_json::to_vec(&policy).unwrap();
		let opening_hash=sha256::Hash::hash(&certificate).to_string();
		let as_hex=|bytes:&[u8]|bytes.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
		let cut=serde_json::json!({"snapshot":"1:2:","backend_pid":12,"database":"trackc","role":"trackcbodyobserver","read_only":"on","isolation":"repeatable read"});
		let mut joint=cut.clone();let mut body=cut;
		for table in ["native_fee_publication","native_fee_input","native_fee_opening","native_urgent_attempt","native_urgent_input","nursery_tx","wallet_changeset","expiry_carrier","rounds","round_participation","round_part_input","round_part_output","virtual_transaction"] {
			joint[table]=serde_json::json!([]);
		}
		for table in ["coins","pending_boards","pool","pool_retirements","payment_links"] {body[table]=serde_json::json!([]);}
		joint["native_fee_opening"]=serde_json::json!([{"id":1,"opening_hash":opening_hash,"policy_bytes_hex":as_hex(&policy_raw),
			"opening_bytes_hex":as_hex(&certificate),"history_identity":"synthetic unchanged native source identity","created_at":"synthetic unit only"}]);
		let mut request=serde_json::json!({"format":FORMAT,"expected":{"server_pubkey":key.to_string(),
			"genesis":bitcoin::constants::genesis_block(network).block_hash().to_string(),"policy_sha256":sha256::Hash::hash(&policy_raw).to_string(),"opening_sha256":opening_hash},
			"policy_utf8":String::from_utf8(policy_raw).unwrap(),"history_utf8":"10000\n","publication_files":null,"round_rights_files":null,
			"opening_files":{"certificate.json":String::from_utf8(certificate).unwrap(),"validated.sha256":opening_hash},"joint_rows":joint,"body_rows":body});
		let raw=serde_json::to_vec(&request).unwrap();let result=audit_captured(&raw).unwrap();
		assert_eq!(result.request_sha256,sha256::Hash::hash(&raw));
		assert_eq!(result.charged_sat,10_000);assert_eq!(result.carrier_charged_sat,10_000);
		assert_eq!(result.ordinary_publications,0);assert_eq!(result.coin_rows,0);
		assert!(!result.complete && !result.qualified && !result.financial_readiness);
		// Complete byte reconciliation with zero SQL rows cannot erase the
		// retained nonzero legacy charge or become a monetary readiness proof.
		request["history_utf8"]="".into();assert!(audit_captured(&serde_json::to_vec(&request).unwrap()).is_err());
		request["history_utf8"]="10000\n".into();
		request["joint_rows"]["native_fee_opening"][0]["opening_hash"]=sha256::Hash::hash(b"other original").to_string().into();
		assert!(audit_captured(&serde_json::to_vec(&request).unwrap()).is_err());
		assert_eq!(original["previously_charged_sat"],10_000);
	}

}
