//! Private offline codec for a captured fee source. This proves native syntax,
//! journal/SQL equality and package arithmetic, never current chain ownership.
use super::*;
use crate::database::native_funding::{decode_funding_row, require_funding_inputs, require_funding_history};
use crate::database::urgent_publication::UrgentPublication;
use crate::payout_witness::DIRECT_HISTORY_LIMITS;
use bitcoin::{BlockHash, OutPoint, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hex::{DisplayHex, FromHex};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
	schema: u32,
	kind: String,
	sdk_commit: String,
	source_observation: Observation,
	expected: Expected,
	originals: Originals,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation { id: String, height: u32, tip_hash: BlockHash }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
	genesis: BlockHash, server_pubkey: PublicKey,
	policy_sha256: sha256::Hash, opening_sha256: sha256::Hash,
	journal_sha256: sha256::Hash, database_sha256: sha256::Hash,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Originals { policy_raw: String, opening_raw: String, journal_raw: String, database_raw: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Database {
	native_funding_fee_intent: Vec<Intent>,
	native_funding_fee_input: Vec<Input>,
	native_funding_fee_resolution: Vec<Resolution>,
	nursery_tx: Vec<Nursery>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent { attempt_key: String, reservation: FundingReservation, signed_tx_hex: Option<String> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input { outpoint: OutPoint, attempt_key: sha256::Hash }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution { winner_index: u64, proof: FundingResolution }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Nursery { txid: Txid, tx_hex: String, kind: String, urgent_fee_attempt: Option<UrgentPublication>, urgent_fee_bound: bool }

fn raw_transaction(raw: &str) -> anyhow::Result<Transaction> {
	ensure!(raw.len() <= 800_000, "transaction bytes exceed native bound");
	let bytes = Vec::<u8>::from_hex(raw)?;
	let tx = deserialize(&bytes)?;
	ensure!(serialize(&tx) == bytes, "noncanonical original transaction bytes");
	Ok(tx)
}

fn public_transaction(tx: &Transaction, prevouts: &[TxOut]) -> anyhow::Result<Value> {
	ensure!(tx.input.len() == prevouts.len(), "transaction input source inventory differs");
	let input = prevouts.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
		.filter(|n| *n <= Amount::MAX_MONEY).context("transaction input overflow")?;
	let output = tx.output.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
		.context("transaction output overflow")?;
	let fee = input.checked_sub(output).context("transaction exceeds original input values")?;
	Ok(json!({"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"weight_wu":tx.weight().to_wu(),
		"fee_sat":fee.to_sat(),"inputs":tx.input.iter().zip(prevouts).map(|(i,o)| json!({
			"outpoint":i.previous_output,"value_sat":o.value.to_sat(),"script_pubkey":o.script_pubkey.as_bytes().as_hex().to_string()
		})).collect::<Vec<_>>(),"outputs":tx.output.iter().enumerate().map(|(v,o)| json!({
			"vout":v,"value_sat":o.value.to_sat(),"script_pubkey":o.script_pubkey.as_bytes().as_hex().to_string()
		})).collect::<Vec<_>>() }))
}

/// Verify exactly one authenticated captured request. No files, configuration,
/// database, Core, wallet or signer are accessed here. The caller authenticates
/// capture/proof-executable custody and independently rejoins current ownership.
pub fn verify_native_fee_source(raw: &[u8]) -> anyhow::Result<Vec<u8>> {
	ensure!(raw.len() <= MAX_REQUEST_BYTES, "native fee request exceeds whole bound");
	let request: Request = serde_json::from_slice(raw)?;
	ensure!(request.schema == 1 && request.kind == "b-native-fee-source-request"
		&& request.sdk_commit == env!("GIT_HASH"), "native fee request source differs");
	let observation = &request.source_observation;
	ensure!(!observation.id.is_empty() && observation.id.len() <= 128 && observation.id.bytes()
		.all(|c| c.is_ascii_alphanumeric() || b"-_:.".contains(&c)) && observation.height > 0
		&& observation.tip_hash != BlockHash::all_zeros(), "invalid public source observation");
	let originals = &request.originals;
	let expected = &request.expected;
	ensure!(originals.policy_raw.len() <= 2048 && originals.opening_raw.len() <= 2048
		&& originals.journal_raw.len() as u64 <= MAX_HISTORY_BYTES, "original native files exceed bounds");
	let hashes = [sha256::Hash::hash(originals.policy_raw.as_bytes()), sha256::Hash::hash(originals.opening_raw.as_bytes()),
		sha256::Hash::hash(originals.journal_raw.as_bytes()), sha256::Hash::hash(originals.database_raw.as_bytes())];
	ensure!(hashes == [expected.policy_sha256,expected.opening_sha256,expected.journal_sha256,expected.database_sha256],
		"original native source hashes differ");
	let stored: StoredPolicy = serde_json::from_str(&originals.policy_raw)?;
	ensure!(serde_json::to_vec(&stored)? == originals.policy_raw.as_bytes(), "noncanonical original policy");
	stored.policy.validate()?;
	let opening: OrdinaryFeeOpening = serde_json::from_str(&originals.opening_raw)?;
	ensure!(serde_json::to_vec(&opening)? == originals.opening_raw.as_bytes() && hashes[1] == stored.opening_sha256
		&& stored.policy.opening_sha256.is_none_or(|h| h == hashes[1]), "original opening identity differs");
	opening.validate(stored.policy)?;
	ensure!(opening.genesis == expected.genesis && opening.server_pubkey == expected.server_pubkey
		&& opening.genesis == bitcoin::constants::genesis_block(opening.network).block_hash(), "original fee chain/server differs");
	let parsed = parse_history(&originals.journal_raw, opening.initial_charge(stored.policy)?, stored.policy)?;
	ensure!(parsed.funding.values().all(|s| s.reservation.policy_sha256 == hashes[0]), "funding policy differs");
	let database: Database = serde_json::from_str(&originals.database_raw)?;
	ensure!(database.native_funding_fee_intent.len() <= 4096 && database.native_funding_fee_resolution.len() <= 4096
		&& database.native_funding_fee_input.len() <= DIRECT_HISTORY_LIMITS.max_rows
		&& database.nursery_tx.len() <= DIRECT_HISTORY_LIMITS.max_rows, "complete SQL row inventory exceeds bound");

	let mut inputs = BTreeMap::<sha256::Hash,BTreeSet<String>>::new();
	for row in &database.native_funding_fee_input {
		ensure!(inputs.entry(row.attempt_key).or_default().insert(row.outpoint.to_string()), "duplicate native input row");
	}
	let mut states = Vec::new(); let mut funding_rows = Vec::new(); let mut keys = BTreeSet::new(); let mut bytes = 0usize;
	for row in &database.native_funding_fee_intent {
		let reservation = serde_json::to_string(&row.reservation)?;
		let signed = row.signed_tx_hex.as_deref().map(raw_transaction).transpose()?;
		let signed_bytes = signed.as_ref().map(serialize);
		bytes = bytes.checked_add(reservation.len()).and_then(|n| n.checked_add(signed_bytes.as_ref().map_or(0,Vec::len)))
			.context("native intent byte overflow")?;
		ensure!(bytes <= 4 * 1024 * 1024, "native intent inventory exceeds byte bound");
		let state = decode_funding_row(&row.attempt_key, &reservation, signed_bytes.as_deref())?;
		let key = state.reservation.candidate.key();
		ensure!(keys.insert(key), "duplicate native funding intent");
		require_funding_inputs(&state, &inputs.remove(&key).unwrap_or_default())?;
		let candidate = &state.reservation.candidate;
		let tx = candidate.transaction()?;
		funding_rows.push(json!({"attempt_key":key,"index":state.reservation.index,"purpose":candidate.purpose,
			"family":candidate.family,"family_sha256":sha256::Hash::hash(&serde_json::to_vec(&candidate.family)?),
			"generation":candidate.generation,"unsigned_body_sha256":sha256::Hash::hash(&candidate.unsigned),
			"prevouts_sha256":sha256::Hash::hash(&serde_json::to_vec(&candidate.prevouts)?),
			"reservation_native_sha256":sha256::Hash::hash(reservation.as_bytes()),
			"ceiling_sat":state.reservation.fee()?.to_sat(),"max_fee_rate_sat_kwu":state.reservation.max_fee_rate.to_sat_per_kwu(),
			"unsigned":public_transaction(&tx,&candidate.prevouts)?,
			"signed":state.signed.as_ref().map(|t| public_transaction(t,&candidate.prevouts)).transpose()?}));
		states.push(state);
	}
	ensure!(inputs.is_empty(), "orphan native input row");
	let mut resolutions = Vec::new(); let mut winners = BTreeSet::new();
	for row in &database.native_funding_fee_resolution {
		ensure!(serde_json::to_vec(&row.proof)?.len() <= 1024 && row.winner_index == row.proof.winner_index
			&& winners.insert(row.winner_index), "native resolution row/index differs");
		validate_resolution(&row.proof,&parsed.funding)?;
		resolutions.push(row.proof.clone());
	}
	require_funding_history(&states,&resolutions,&parsed.funding.values().cloned().collect::<Vec<_>>(),
		&parsed.resolutions.values().cloned().collect::<Vec<_>>())?;

	let mut nursery = BTreeMap::new(); let mut nursery_inventory = Vec::new(); let mut total_bytes=0usize;
	for row in &database.nursery_tx {
		let tx=raw_transaction(&row.tx_hex)?;
		let _kind: crate::nursery::NurseryTxKind = row.kind.parse()?;
		ensure!(tx.compute_txid()==row.txid
			&& (!row.urgent_fee_bound || row.urgent_fee_attempt.is_some()), "nursery original identity differs");
		let metadata_bytes=row.urgent_fee_attempt.as_ref().map(serde_json::to_vec).transpose()?.map_or(0,|v|v.len());
		total_bytes=total_bytes.checked_add(serialize(&tx).len()).and_then(|n| n.checked_add(metadata_bytes))
			.context("whole nursery inventory byte overflow")?;
		ensure!(total_bytes<=DIRECT_HISTORY_LIMITS.max_serialized_bytes, "whole nursery package inventory exceeds byte bound");
		ensure!(nursery.insert(row.txid,(row,tx.clone())).is_none(), "duplicate nursery transaction row");
		nursery_inventory.push(json!({"txid":row.txid,"wtxid":tx.compute_wtxid(),"weight_wu":tx.weight().to_wu(),
			"kind":row.kind,"urgent_fee_bound":row.urgent_fee_bound,
			"urgent_metadata_native_sha256":row.urgent_fee_attempt.as_ref().map(|m|serde_json::to_vec(m).map(|v|sha256::Hash::hash(&v))).transpose()?}));
	}
	let mut urgent_rows=Vec::new(); let mut attempts=BTreeSet::new(); let mut members=BTreeSet::new();
	for row in &database.nursery_tx {
		let Some(meta)=&row.urgent_fee_attempt else {continue};
		ensure!(!meta.txids.is_empty() && meta.txids.len()<=2 && meta.prevouts.len()==meta.txids.len(),
			"urgent complete package membership exceeds native bound");
		ensure!(meta.policy_sha256==hashes[0] && attempts.insert(meta.attempt_index), "urgent original policy/attempt differs");
		let attempt=parsed.urgent.get(&meta.attempt_index).context("urgent DB attempt missing from independent journal")?;
		ensure!(meta.ceiling_sat==attempt.ceiling, "urgent DB ceiling differs");
		let mut txs=Vec::new(); let mut transactions=Vec::new();
		for (position,id) in meta.txids.iter().enumerate() {
			let (member,tx)=nursery.get(id).context("urgent whole package member missing")?;
			let original=member.urgent_fee_attempt.as_ref().map(serde_json::to_string).transpose()?;
			meta.require_member(position,&member.kind,original.as_deref())?;
			members.insert(*id);txs.push(tx.clone());
		}
		let fee=meta.validate(&txs)?;
		for (tx,prevouts) in txs.iter().zip(&meta.prevouts) {
			for (input,prevout) in tx.input.iter().zip(prevouts) {
				if let Some(parent)=txs.iter().find(|p|p.compute_txid()==input.previous_output.txid) {
					ensure!(parent.output.get(input.previous_output.vout as usize)==Some(prevout),
						"urgent shared parent prevout differs from original member bytes");
				}
			}
		}
		let binding=sha256::Hash::hash(&serde_json::to_vec(meta)?);
		if let Some((old_binding,old_txs))=&attempt.signed {
			ensure!(*old_binding==binding && *old_txs==txs, "urgent retained bytes/metadata binding differ");
		} else {ensure!(!row.urgent_fee_bound, "urgent publication permitted before independent binding");}
		for (tx,prevouts) in txs.iter().zip(&meta.prevouts) {transactions.push(public_transaction(tx,prevouts)?);}
		urgent_rows.push(json!({"index":meta.attempt_index,"ceiling_sat":meta.ceiling_sat,
			"policy_sha256":meta.policy_sha256,"metadata_binding_sha256":binding,"source_anchors":meta.source_anchors,
			"journal_binding_sha256":attempt.signed.as_ref().map(|(h,_)|*h),"urgent_fee_bound":row.urgent_fee_bound,
			"fee_sat":fee.to_sat(),"transactions":transactions}));
	}
	ensure!(parsed.urgent.iter().all(|(i,a)| a.signed.is_none() || attempts.contains(i)), "retained urgent journal lost original SQL publication");
	let unrelated_nursery_txids=nursery.keys().filter(|id|!members.contains(*id)).collect::<Vec<_>>();
	let snapshot=super::public_snapshot::project_history(parsed,stored.policy,&opening,hashes[0],originals.journal_raw.as_bytes(),0,0)?;
	let mut snapshot: Value=serde_json::from_slice(&snapshot)?;
	let snapshot_object=snapshot.as_object_mut().context("native snapshot object missing")?;
	snapshot_object.remove("journal_device");snapshot_object.remove("journal_inode");
	let response=json!({"schema":1,"kind":"b-native-fee-source-proof","sdk_commit":env!("GIT_HASH"),
		"input_sha256":sha256::Hash::hash(raw),"source_observation":request.source_observation,
		"original_sha256":{"policy_raw":hashes[0],"opening_raw":hashes[1],"journal_raw":hashes[2],"database_raw":hashes[3]},
		"snapshot":snapshot,"funding_rows":funding_rows,"urgent_rows":urgent_rows,"resolutions":resolutions,
		"nursery_inventory":nursery_inventory,"unrelated_nursery_txids":unrelated_nursery_txids});
	let output=serde_json::to_vec(&response)?;
	ensure!(output.len()<=MAX_RESPONSE_BYTES,"native fee response exceeds whole bound");
	Ok(output)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::watchman::ordinary_funding::test_candidate;

	fn request(journal: String, database: Value) -> Value {
		let key=ark::test_util::dummy::DUMMY_SERVER_KEY.public_key();
		let policy=OrdinaryFeePolicy::default();
		let opening=OrdinaryFeeOpening { version:1,network:Network::Regtest,
			genesis:bitcoin::constants::genesis_block(Network::Regtest).block_hash(),server_pubkey:key,
			history_prefix_bytes:0,history_prefix_sha256:sha256::Hash::hash(b""),native_spent_sat:0,
			external_spent_sat:0,accounting_receipt_sha256:None };
		let opening_raw=serde_json::to_string(&opening).unwrap();
		let policy_raw=serde_json::to_string(&StoredPolicy {policy,opening_sha256:sha256::Hash::hash(opening_raw.as_bytes())}).unwrap();
		let database_raw=serde_json::to_string(&database).unwrap();
		json!({"schema":1,"kind":"b-native-fee-source-request","sdk_commit":env!("GIT_HASH"),
			"source_observation":{"id":"ordinary-offline-unit","height":1,"tip_hash":BlockHash::from_byte_array([1;32])},
			"expected":{"genesis":opening.genesis,"server_pubkey":key,"policy_sha256":sha256::Hash::hash(policy_raw.as_bytes()),
				"opening_sha256":sha256::Hash::hash(opening_raw.as_bytes()),"journal_sha256":sha256::Hash::hash(journal.as_bytes()),
				"database_sha256":sha256::Hash::hash(database_raw.as_bytes())},
			"originals":{"policy_raw":policy_raw,"opening_raw":opening_raw,"journal_raw":journal,"database_raw":database_raw}})
	}
	fn empty() -> Value {json!({"native_funding_fee_intent":[],"native_funding_fee_input":[],"native_funding_fee_resolution":[],"nursery_tx":[]})}
	fn verify(value: &Value) -> anyhow::Result<Value> {
		Ok(serde_json::from_slice(&verify_native_fee_source(&serde_json::to_vec(value)?)?)?)
	}
	fn replace_database(value: &mut Value, database: Value) {
		let raw=serde_json::to_string(&database).unwrap();
		value["expected"]["database_sha256"]=json!(sha256::Hash::hash(raw.as_bytes()));
		value["originals"]["database_raw"]=json!(raw);
	}
	fn funding() -> (Value, Value) {
		let (candidate,signed)=test_candidate(0,500);
		let base=request(String::new(),empty());
		let policy_sha256=serde_json::from_value(base["expected"]["policy_sha256"].clone()).unwrap();
		let reservation=FundingReservation {policy_sha256,index:1,candidate,max_fee_rate:bitcoin::FeeRate::from_sat_per_vb(100).unwrap()};
		let key=reservation.candidate.key();let point=reservation.candidate.transaction().unwrap().input[0].previous_output;
		let journal=format!("f:{}\ng:1:{}\n",serde_json::to_string(&reservation).unwrap(),serde_json::to_string(&serialize(&signed)).unwrap());
		let database=json!({"native_funding_fee_intent":[{"attempt_key":key,"reservation":reservation,"signed_tx_hex":serialize(&signed).as_hex().to_string()}],
			"native_funding_fee_input":[{"attempt_key":key,"outpoint":point}],"native_funding_fee_resolution":[],"nursery_tx":[]});
		(request(journal,database.clone()),database)
	}
	fn urgent() -> (Value,Value) {
		let (candidate,parent)=test_candidate(0,500);
		// The native urgent validator checks exact member/prevout arithmetic;
		// this fixture does not assert chain script validity or source ownership.
		let mut child=parent.clone();child.input[0].previous_output=OutPoint::new(parent.compute_txid(),0);
		child.output[0].value=Amount::from_sat(9_000);child.input[0].witness=bitcoin::Witness::new();
		let txs=vec![parent.clone(),child.clone()];
		let base=request(String::new(),empty());
		let meta=UrgentPublication {version:1,policy_sha256:serde_json::from_value(base["expected"]["policy_sha256"].clone()).unwrap(),
			attempt_index:1,ceiling_sat:1_000,source_anchors:vec![parent.input[0].previous_output],
			txids:txs.iter().map(Transaction::compute_txid).collect(),prevouts:vec![candidate.prevouts,vec![parent.output[0].clone()]]};
		let binding=sha256::Hash::hash(&serde_json::to_vec(&meta).unwrap());
		let journal=format!("u:1:1000\np:1:{binding}:{}\n",encode_package(&txs).unwrap());
		let database=json!({"native_funding_fee_intent":[],"native_funding_fee_input":[],"native_funding_fee_resolution":[],"nursery_tx":[
			{"txid":parent.compute_txid(),"tx_hex":serialize(&parent).as_hex().to_string(),"kind":"internal","urgent_fee_attempt":null,"urgent_fee_bound":false},
			{"txid":child.compute_txid(),"tx_hex":serialize(&child).as_hex().to_string(),"kind":"internal","urgent_fee_attempt":meta,"urgent_fee_bound":true}]});
		(request(journal,database.clone()),database)
	}
	#[test]
	fn offline_native_fee_empty_source_projects_exact_identity_without_custody() {
		let input=request(String::new(),empty());let out=verify(&input).unwrap();
		assert_eq!(out["snapshot"]["charged_sat"],20_000);
		assert_eq!(out["snapshot"]["discretionary_remaining_sat"],25_000);
		assert!(out["snapshot"].get("journal_device").is_none());assert!(out["snapshot"].get("journal_inode").is_none());
		assert_eq!(out["input_sha256"],json!(sha256::Hash::hash(&serde_json::to_vec(&input).unwrap())));
		if let Some(directory)=std::env::var_os("NATIVE_FEE_GOLDEN_DIR") {
			let directory=Path::new(&directory);
			std::fs::write(directory.join("native-empty-request.json"),serde_json::to_vec(&input).unwrap()).unwrap();
			std::fs::write(directory.join("native-empty-reply.json"),serde_json::to_vec(&out).unwrap()).unwrap();
		}
	}
	#[test]
	fn offline_native_fee_refuses_changed_original_or_tool_identity() {
		let mut input=request(String::new(),empty());input["sdk_commit"]=json!("0".repeat(40));assert!(verify(&input).is_err());
		let mut input=request(String::new(),empty());input["originals"]["journal_raw"]=json!("secret-sentinel");assert!(verify(&input).is_err());
		assert!(verify(&request("17".into(),empty())).is_err());
	}
	#[test]
	fn offline_native_fee_signed_funding_uses_native_signature_and_exact_inputs() {
		let (input,mut database)=funding();let out=verify(&input).unwrap();
		assert_eq!(out["funding_rows"][0]["signed"]["fee_sat"],500);
		database["native_funding_fee_input"]=json!([]);let mut changed=input.clone();replace_database(&mut changed,database);assert!(verify(&changed).is_err());
		let (mut input,mut database)=funding();let row=database["native_funding_fee_input"][0].clone();database["native_funding_fee_input"].as_array_mut().unwrap().push(row);
		replace_database(&mut input,database);assert!(verify(&input).is_err());
	}
	#[test]
	fn offline_native_fee_refuses_omitted_signed_state_and_resolution() {
		let (mut input,mut database)=funding();database["native_funding_fee_intent"][0]["signed_tx_hex"]=Value::Null;
		replace_database(&mut input,database);assert!(verify(&input).is_err());
		let (mut input,mut database)=funding();database["native_funding_fee_resolution"]=json!([{"winner_index":1,"proof":{"winner_index":1,"block_height":1,"block_hash":BlockHash::from_byte_array([2;32])}}]);
		replace_database(&mut input,database);assert!(verify(&input).is_err());
	}
	#[test]
	fn offline_native_fee_preserves_shared_null_parent_and_rejects_missing_member() {
		let (input,mut database)=urgent();let out=verify(&input).unwrap();assert_eq!(out["urgent_rows"][0]["transactions"].as_array().unwrap().len(),2);
		assert_eq!(out["urgent_rows"][0]["fee_sat"],1000);assert!(out["unrelated_nursery_txids"].as_array().unwrap().is_empty());
		database["nursery_tx"].as_array_mut().unwrap().remove(0);let mut changed=input.clone();replace_database(&mut changed,database);assert!(verify(&changed).is_err());
	}
	#[test]
	fn offline_native_fee_refuses_urgent_prevout_or_binding_changes() {
		let (mut input,mut database)=urgent();database["nursery_tx"][1]["urgent_fee_attempt"]["ceiling_sat"]=json!(1001);
		replace_database(&mut input,database);assert!(verify(&input).is_err());
		let (mut input,mut database)=urgent();database["nursery_tx"][1]["urgent_fee_attempt"]["prevouts"][0][0]["value"]=json!(50_000);
		replace_database(&mut input,database);assert!(verify(&input).is_err());
	}
	#[test]
	fn offline_native_fee_unbound_sql_gap_has_no_publication_permission() {
		let (_,mut database)=urgent();database["nursery_tx"][1]["urgent_fee_bound"]=json!(false);
		let input=request("u:1:1000\n".into(),database.clone());let out=verify(&input).unwrap();
		assert!(out["urgent_rows"][0]["journal_binding_sha256"].is_null());assert_eq!(out["urgent_rows"][0]["urgent_fee_bound"],false);
		database["nursery_tx"][1]["urgent_fee_bound"]=json!(true);assert!(verify(&request("u:1:1000\n".into(),database)).is_err());
	}
	#[test]
	fn offline_native_fee_preserves_unrelated_nursery_without_raw_output() {
		let (_,signed)=test_candidate(0,500);let mut database=empty();database["nursery_tx"]=json!([{"txid":signed.compute_txid(),
			"tx_hex":serialize(&signed).as_hex().to_string(),"kind":"round","urgent_fee_attempt":null,"urgent_fee_bound":false}]);
		let out=verify(&request(String::new(),database)).unwrap();assert_eq!(out["unrelated_nursery_txids"].as_array().unwrap().len(),1);
		let text=serde_json::to_string(&out).unwrap();assert!(!text.contains(&serialize(&signed).as_hex().to_string()));
		for key in ["rawtx","witness","preimage","ready","qualified","complete"] {assert!(!text.contains(&format!("\"{key}\":")));}
	}
	#[test]
	fn offline_native_fee_refuses_orphan_input_and_changed_parent_witness() {
		let (mut input,mut database)=funding();let mut orphan=database["native_funding_fee_input"][0].clone();
		orphan["attempt_key"]=json!(sha256::Hash::from_byte_array([51;32]));
		database["native_funding_fee_input"].as_array_mut().unwrap().push(orphan);
		replace_database(&mut input,database);assert!(verify(&input).is_err());
		let (mut input,mut database)=urgent();let mut parent=raw_transaction(database["nursery_tx"][0]["tx_hex"].as_str().unwrap()).unwrap();
		parent.input[0].witness=bitcoin::Witness::from_slice(&[[0_u8;64]]);
		database["nursery_tx"][0]["tx_hex"]=json!(serialize(&parent).as_hex().to_string());
		replace_database(&mut input,database);assert!(verify(&input).is_err());
	}
	#[test]
	fn offline_native_fee_refuses_whole_input_overflow_without_partial_parse() {
		assert!(verify_native_fee_source(&vec![b' ';MAX_REQUEST_BYTES+1]).is_err());
		assert!(verify(&request("1".repeat(MAX_HISTORY_BYTES as usize+1),empty())).is_err());
	}

	#[test]
	fn offline_native_fee_owner_projection_matches_pure_codec() {
		use std::os::unix::fs::PermissionsExt;
		let dir=std::env::temp_dir().join(format!("b-native-fee-source-unit-{}-{}",std::process::id(),
			std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
		std::fs::create_dir(&dir).unwrap();std::fs::set_permissions(&dir,std::fs::Permissions::from_mode(0o700)).unwrap();
		let policy=OrdinaryFeePolicy::default();
		policy.initialize_fresh(&dir,Network::Regtest,ark::test_util::dummy::DUMMY_SERVER_KEY.public_key()).unwrap();
		let owner=OrdinaryFeeBudget::load(&dir,policy).unwrap();
		let actual=owner.public_snapshot().unwrap();let mut semantic:Value=serde_json::from_slice(&actual).unwrap();
		semantic.as_object_mut().unwrap().remove("journal_device");semantic.as_object_mut().unwrap().remove("journal_inode");
		let input=request(String::new(),empty());let output=verify(&input).unwrap();assert_eq!(semantic,output["snapshot"]);
		if let Some(directory)=std::env::var_os("NATIVE_FEE_GOLDEN_DIR") {
			std::fs::write(Path::new(&directory).join("native-empty-owner-snapshot.json"),&actual).unwrap();
		}
		drop(owner);std::fs::remove_dir_all(dir).unwrap();
	}

}
