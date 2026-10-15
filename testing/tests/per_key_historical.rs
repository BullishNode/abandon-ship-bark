//! Isolated old-binary producer only. The new DB/SDK phase consumes its snapshot separately.
use std::{collections::BTreeMap, fs, io::Write, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::{Path, PathBuf}, time::{Duration, Instant}};
use ark::{ProtocolEncoding, fallback::{FallbackRecord, KeyLink}};
use ark_testing::{TestContext, sat};
use bitcoin::{Network, hashes::{Hash, sha256}};
use bitcoincore_rpc::RpcApi;
use serde_json::{Value, json};
use server::database::Db;

const OLD: &str = "af52be109466e47de96b0fed0e42c7688420a06c";
// No path defaults: the fixture runner supplies separately admitted private roots.
fn admitted_directory(name:&str)->PathBuf {
	let path=PathBuf::from(std::env::var_os(name).expect("explicit root-admitted fixture path required"));
	assert!(path.is_absolute() && path.components().all(|p|!matches!(p,std::path::Component::ParentDir|std::path::Component::CurDir)));
	assert_eq!(fs::canonicalize(&path).unwrap(),path,"fixture root cannot alias a different directory");
	assert!(path.is_dir());path
}
fn digest(bytes: &[u8]) -> String { sha256::Hash::hash(bytes).to_string() }
fn exclusive(path: &Path, bytes: &[u8]) {
	let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();
	file.write_all(bytes).unwrap(); file.sync_all().unwrap();
	fs::File::open(path.parent().unwrap()).unwrap().sync_all().unwrap();
}
fn old_binary(directory:&Path,name: &str, expected: &str) -> PathBuf {
	let path = directory.join(name);
	assert!(fs::symlink_metadata(&path).unwrap().is_file(),"retained old executable must be a regular file");
	assert_eq!(digest(&fs::read(&path).unwrap()), expected, "old executable bytes must match the retained qualification");
	path
}
fn processes(root: &Path) -> Vec<Value> {
	let mut found = Vec::new();
	for entry in fs::read_dir("/proc").unwrap() {
		let entry = entry.unwrap();
		let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
		let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else { continue };
		if !cmdline.windows(root.as_os_str().as_encoded_bytes().len()).any(|p| p == root.as_os_str().as_encoded_bytes()) { continue }
		let Ok(exe) = fs::read_link(entry.path().join("exe")) else { continue };
		let name = exe.file_name().unwrap().to_string_lossy();
		if ["postgres", "bitcoind", "captaind", "watchmand", "lightningd", "bark"].contains(&name.as_ref()) {
			found.push(json!({"pid":pid,"exe":exe,"cmdline_sha256":digest(&cmdline)}));
		}
	}
	found.sort_by_key(|v| v["pid"].as_u64().unwrap()); found
}
// Hash every application row without publishing private auth/seed-bearing columns.
// The q sidecar independently repeats this exact row_to_json/newline projection.
async fn all_rows(db: &tokio_postgres::Client) -> Value {
	db.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL timezone='UTC'; SET LOCAL statement_timeout='8s'").await.unwrap();
	let tables = db.query("SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename<>'refinery_schema_history' ORDER BY tablename", &[]).await.unwrap();
	assert!(tables.len() <= 256);
	let mut result = BTreeMap::new();
	let mut total_bytes = 0usize;
	for table in tables {
		let name: String = table.get(0);
		assert!(name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
		let query = format!("SELECT row_to_json(t)::text FROM public.\"{name}\" t ORDER BY row_to_json(t)::text");
		let size = db.query_one(&format!("SELECT count(*)::bigint,coalesce(sum(octet_length(row_to_json(t)::text)+1),0)::bigint FROM public.\"{name}\" t"), &[]).await.unwrap();
		let count: i64 = size.get(0); let bytes: i64 = size.get(1);
		assert!((0..=10_000).contains(&count) && (0..=8_388_608).contains(&bytes));
		total_bytes = total_bytes.checked_add(usize::try_from(bytes).unwrap()).unwrap(); assert!(total_bytes <= 16_777_216);
		let rows = db.query(&query, &[]).await.unwrap(); assert_eq!(rows.len(), usize::try_from(count).unwrap());
		let mut payload = Vec::new(); for row in rows { payload.extend_from_slice(row.get::<_, String>(0).as_bytes()); payload.push(b'\n'); }
		assert_eq!(payload.len(), usize::try_from(bytes).unwrap());
		result.insert(name, json!({"rows":count,"bytes":bytes,"sha256":digest(&payload)}));
	}
	let history = db.query("SELECT version,name,checksum,applied_on::text FROM public.refinery_schema_history ORDER BY version", &[]).await.unwrap();
	assert_eq!(history.len(), 76, "old binaries must create only their original migration history");
	let history: Vec<Value> = history.iter().enumerate().map(|(i,r)| {
		let version: i32 = r.get(0); assert_eq!(version as usize, i);
		json!({"version":version,"name":r.get::<_,String>(1),"checksum":r.get::<_,String>(2),"applied_on":r.get::<_,String>(3)})
	}).collect();
	db.batch_execute("COMMIT").await.unwrap(); json!({"application_tables":result,"original_history":history})
}

#[tokio::test]
async fn per_key_historical_af52_board_arkoor_snapshot_producer() {
	let test_root=admitted_directory("C_FUNDED_UPGRADE_TEST_ROOT");
	let handoff_root=admitted_directory("C_FUNDED_UPGRADE_HANDOFF");
	let old_bin_dir=admitted_directory("C_FUNDED_UPGRADE_OLD_BIN_DIR");
	assert_ne!(test_root,handoff_root);assert!(!handoff_root.starts_with(&test_root));
	assert_eq!(PathBuf::from(std::env::var_os("TEST_DIRECTORY").unwrap()),test_root);
	assert!(!test_root.join("per-key/historical-af52-board-arkoor-producer").exists(),"preserve any prior completed or failed producer; never regenerate its financial actions");
	assert!(matches!(std::env::var("TEST_POSTGRES_HOST"), Err(std::env::VarError::NotPresent)), "dedicated managed PG only");
	assert_eq!(std::env::var("KEEP_ALL_TEST_DATA").unwrap(), "1", "private source state must survive producer completion");
	let handoff = handoff_root.as_path();
	assert!(handoff.is_dir()); assert_eq!(fs::metadata(handoff).unwrap().permissions().mode() & 0o777, 0o700);
	assert!(!handoff.join("READY.json").exists() && !handoff.join("ACK.json").exists());
	let old_cap = old_binary(&old_bin_dir,"captaind", "7e00a393434c594656878e080674c3f50462d99013bb8834f605003e1bcdb623");
	let old_bark = old_binary(&old_bin_dir,"bark", "40276d98f7782f475a02bfa522df27260d84e8adbd7f495c64cb53bb110c2b22");
	let ctx = TestContext::new("per-key/historical-af52-board-arkoor-producer").await;
	assert!(ctx.datadir.starts_with(&test_root));
	let srv = ctx.captaind("old-server").exec(old_cap).bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.cfg(|c| c.fees.board.min_fee = sat(330)).create().await;
	let payer = ctx.bark("old-payer", &srv).exec(old_bark.clone()).funded(sat(200_000))
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark("old-recipient", &srv).exec(old_bark)
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	let pg = srv.config().postgres.clone();
	// Raw connection, intentionally NOT Db::connect (which would migrate the old DB).
	let (db, connection) = Db::config(&pg.name, &pg).connect(tokio_postgres::NoTls).await.unwrap();
	let connection = tokio::spawn(async move { connection.await.unwrap() });
	let before = all_rows(&db).await; assert!(before["application_tables"]["vtxo"]["rows"].as_i64().unwrap() == 0);
	let payer_identity: Value = payer.run_json(["debug-info"]).await;
	let payer_mailbox: bitcoin::secp256k1::PublicKey = payer_identity["mailbox_id"].as_str().unwrap().parse().unwrap();
	let identity: Value = recipient.run_json(["debug-info"]).await;
	let mailbox: bitcoin::secp256k1::PublicKey = identity["mailbox_id"].as_str().unwrap().parse().unwrap();
	let address: ark::Address = recipient.address().await.trim().parse().unwrap();
	let key = address.policy().user_pubkey();
	ctx.generate_blocks(1).await; // actual confirmed payer funding, matching the standard Board builder
	let board = payer.board(sat(100_000)).await;
	ctx.await_transaction(board.funding_tx.txid).await;
	ctx.generate_blocks(ark_testing::constants::BOARD_CONFIRMATIONS).await;
	payer.run(["balance"]).await; // actual old CLI sync/admission
	payer.send_oor(&address, sat(25_000)).await;
	recipient.run(["balance"]).await;
	let board_tx = ctx.bitcoind().sync_client().get_raw_transaction(&board.funding_tx.txid, None).unwrap();
	let board_input_sat = board_tx.input.iter().map(|input| {
		let tx = ctx.bitcoind().sync_client().get_raw_transaction(&input.previous_output.txid, None).unwrap();
		tx.output[input.previous_output.vout as usize].value.to_sat()
	}).sum::<u64>();
	let board_output_sat = board_tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
	let board_miner_fee_sat = board_input_sat.checked_sub(board_output_sat).unwrap();
	assert!(board_miner_fee_sat > 0 && board_miner_fee_sat <= 10_000, "finite actual regtest Board fee");
	let mut rpc = srv.get_public_rpc().await;
	let rows = db.query("SELECT vtxo_id,amount,vtxo,spend_state::text FROM vtxo WHERE spend_state='spendable' ORDER BY vtxo_id", &[]).await.unwrap();
	assert!(!rows.is_empty() && rows.len() <= 32);
	let mut rights = BTreeMap::new(); let mut recipient_sat = 0u64; let mut payer_sat = 0u64;
	for row in rows {
		let id: ark::VtxoId = row.get::<_,String>(0).parse().unwrap();
		let public = rpc.get_vtxo(server_rpc::protos::GetVtxoRequest { vtxo_id:id.to_bytes().to_vec() }).await.unwrap().into_inner();
		let coin = ark::Vtxo::<ark::vtxo::Full>::deserialize(&public.vtxo).unwrap();
		assert_eq!(coin.id(), id); assert_eq!(coin.amount().to_sat(), u64::try_from(row.get::<_,i64>(1)).unwrap());
		assert_eq!(coin.serialize(), row.get::<_,Vec<u8>>(2), "public complete signed body equals the independently committed durable body"); assert_eq!(row.get::<_,String>(3), "spendable");
		let record = FallbackRecord::deserialize(&public.fallback_record).unwrap();
		let link = KeyLink::deserialize(&public.key_link).unwrap();
		assert_eq!(link.user_pubkey, coin.user_pubkey());
		let durable = db.query_one("SELECT l.mailbox_pk,l.sig,l.recovery_index,r.spk,r.mailbox_sig FROM key_link l JOIN fallback_record r ON r.mailbox_pk=l.mailbox_pk WHERE l.user_pubkey=$1", &[&coin.user_pubkey().serialize().as_slice()]).await.unwrap();
		let linked_mailbox = bitcoin::secp256k1::PublicKey::from_slice(&durable.get::<_,Vec<u8>>(0)).unwrap();
		assert_eq!(link.signature.serialize().to_vec(), durable.get::<_,Vec<u8>>(1));
		assert_eq!(link.recovery_index, durable.get::<_,Option<i32>>(2).map(|i| u32::try_from(i).unwrap()));
		assert_eq!(record.spk.as_bytes(), durable.get::<_,Vec<u8>>(3));
		assert_eq!(record.mailbox_signature.serialize().to_vec(), durable.get::<_,Vec<u8>>(4));
		record.verify(Network::Regtest, linked_mailbox).unwrap(); link.verify(Network::Regtest, linked_mailbox, &record).unwrap();
		assert_eq!(public.child_index, link.recovery_index);
		let anchor = ctx.bitcoind().sync_client().get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
		coin.validate(&anchor).unwrap();
		if coin.user_pubkey() == key { assert_eq!(linked_mailbox, mailbox); recipient_sat += coin.amount().to_sat(); }
		else { assert_eq!(linked_mailbox, payer_mailbox, "every other surviving right belongs to the actual payer"); payer_sat += coin.amount().to_sat(); }
		rights.insert(id.to_string(), json!({"amount_sat":coin.amount().to_sat(),"user_pubkey":coin.user_pubkey().to_string(),"mailbox_pk":linked_mailbox.to_string(),"vtxo":public.vtxo,"fallback_record":public.fallback_record,"key_link":public.key_link,"anchor":bitcoin::consensus::serialize(&anchor)}));
	}
	assert_eq!(recipient_sat, 25_000); assert_eq!(payer_sat, 74_670);
	assert_eq!(recipient_sat + payer_sat, 100_000 - 330);
	let genesis = ctx.bitcoind().sync_client().get_block_hash(0).unwrap(); assert_eq!(genesis, bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash());
	let tip = ctx.bitcoind().sync_client().get_blockchain_info().unwrap();
	let processes_before = processes(&ctx.datadir);
	srv.stop().await.unwrap(); ctx.bitcoind().stop().await.unwrap();
	let processes_after = processes(&ctx.datadir);
	assert!(!processes_after.is_empty());
	assert!(processes_after.iter().all(|p| Path::new(p["exe"].as_str().unwrap()).file_name().unwrap() == "postgres"));
	let snapshot = all_rows(&db).await;
	let config_path = handoff.join("postgres-private.json"); exclusive(&config_path, &serde_json::to_vec(&pg).unwrap());
	let rights_path = handoff.join("signed-rights-private.json"); let rights_bytes = serde_json::to_vec(&rights).unwrap(); exclusive(&rights_path, &rights_bytes);
	let ready = json!({"version":1,"old_commit":OLD,"new_production":"8c4478cfbfac8a193d0a6549387229dfedee06de","phase":"old-writers-stopped-pg-alive","ctx":ctx.datadir,"postgres_private_config":config_path,"payer_datadir":payer.datadir(),"recipient_datadir":recipient.datadir(),"server_datadir":srv.config().data_dir,"core_datadir":ctx.bitcoind().datadir(),"genesis":genesis.to_string(),"height":tip.blocks,"tip_hash":tip.best_block_hash.to_string(),"recipient_sat":recipient_sat,"payer_sat":payer_sat,"board_gross_sat":100_000,"board_service_sat":330,"board_miner_fee_sat":board_miner_fee_sat,"board_input_sat":board_input_sat,"board_output_sat":board_output_sat,"rights_sha256":digest(&rights_bytes),"rights_count":rights.len(),"signed_rights_private":rights_path,"snapshot":snapshot,"processes_before":processes_before,"processes_after":processes_after});
	let bytes = serde_json::to_vec(&ready).unwrap();
	drop(db); connection.await.unwrap();
	exclusive(&handoff.join("READY.json"), &bytes);
	let began = Instant::now();
	loop {
		assert!(began.elapsed() < Duration::from_secs(120), "no snapshot ACK; producer aborts without retrying any old action");
		if let Ok(ack) = fs::read(handoff.join("ACK.json")) {
			assert!(ack.len() <= 4096); let ack: Value = serde_json::from_slice(&ack).unwrap();
			assert_eq!(ack["ready_sha256"].as_str().unwrap(), digest(&bytes));
			assert_eq!(ack["snapshot_complete"], true); break;
		}
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	// Context tears down only its own remaining PG after the independent dump ACK.
}
