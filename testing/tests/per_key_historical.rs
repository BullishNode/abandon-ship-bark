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

fn private_file(root: &Path, path: &Path, max_bytes: u64) -> Vec<u8> {
	assert!(path.starts_with(root) && fs::canonicalize(path).unwrap().starts_with(root));
	let metadata=fs::symlink_metadata(path).unwrap();
	assert!(metadata.is_file() && metadata.len()<=max_bytes);
	assert_eq!(metadata.permissions().mode() & 0o777,0o600);
	fs::read(path).unwrap()
}
async fn fresh_seed_wallet(root: &Path, name: &str, seed: &Path, handoff: &Path, cfg: &bark::Config) -> bark::Wallet {
	use std::sync::Arc;
	use bark::{OpenWalletArgs,WalletSeed,onchain::OnchainWallet,persist::{BarkPersister,sqlite::SqliteClient}};
	let bytes=private_file(handoff,seed,1024);
	let mnemonic:bip39::Mnemonic=std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap();
	let directory=root.join(name);fs::create_dir(&directory).unwrap();fs::set_permissions(&directory,fs::Permissions::from_mode(0o700)).unwrap();
	let db:Arc<dyn BarkPersister>=Arc::new(SqliteClient::open(directory.join("fresh.sqlite")).unwrap());
	let onchain=Arc::new(tokio::sync::RwLock::new(OnchainWallet::load_or_create(Network::Regtest,mnemonic.to_seed(""),db.clone()).await.unwrap()));
	// No old SQLite, VTXO ID, derivation index, expected body, changeset or auditor
	// map is supplied. Native recovery receives only this seed and real endpoints.
	let wallet=bark::Wallet::open(Network::Regtest,WalletSeed::new_from_mnemonic(Network::Regtest,&mnemonic),cfg.clone(),OpenWalletArgs {
		datadir:Some(directory),persister:Some(db),onchain:Some(onchain),run_daemon:false,create_if_not_exists:true,..Default::default()
	}).await.unwrap();
	assert_eq!(wallet.network().await.unwrap(),Network::Regtest);
	wallet.sync().await;
	wallet.sync_mailbox().await.unwrap(); // expose an actual RPC failure, rather than relying on sync warnings
	wallet
}
async fn discovered_signed_rights(wallet: &bark::Wallet) -> BTreeMap<String,Value> {
	let coins=wallet.spendable_vtxos().await.unwrap();assert!(!coins.is_empty() && coins.len()<=32);
	let balance=wallet.balance().await.unwrap();
	let sum=coins.iter().map(|v|v.amount()).sum::<bitcoin::Amount>();
	assert_eq!(balance.spendable,sum);assert_eq!(balance.total(),sum);
	assert_eq!(balance.needs_refresh,bitcoin::Amount::ZERO);assert_eq!(balance.pending(),bitcoin::Amount::ZERO);
	let unspent=wallet.vtxos().await.unwrap();
	assert_eq!(unspent.iter().map(|v|v.id()).collect::<std::collections::BTreeSet<_>>(),coins.iter().map(|v|v.id()).collect::<std::collections::BTreeSet<_>>(),"no extra held or unknown unspent rights may hide outside the positive map");
	let mailbox=wallet.mailbox_keypair().public_key();
	let record=wallet.fallback_destination().await.unwrap();record.verify(Network::Regtest,mailbox).unwrap();
	let server=wallet.require_ark_info().await.unwrap().server_pubkey;
	let mut result=BTreeMap::new();
	for coin in coins {
		let full=wallet.get_full_vtxo(coin.id()).await.unwrap();
		assert_eq!(full.server_pubkey(),server);
		let key=wallet.get_vtxo_key(&full).await.unwrap();assert_eq!(key.public_key(),full.user_pubkey());
		let link=wallet.fallback_key_link(full.user_pubkey()).await.unwrap();
		link.verify(Network::Regtest,mailbox,&record).unwrap();assert_eq!(link.user_pubkey,full.user_pubkey());
		wallet.validate_vtxo(&full).await.unwrap();
		assert!(full.amount()>bitcoin::Amount::ZERO);
		assert!(result.insert(full.id().to_string(),json!({"amount_sat":full.amount().to_sat(),"user_pubkey":full.user_pubkey().to_string(),"mailbox_pk":mailbox.to_string(),"vtxo":full.serialize(),"fallback_record":record.serialize(),"key_link":link.serialize()})).is_none());
	}
	result
}

#[tokio::test]
async fn per_key_historical_af52_seed_restore_after_strict_upgrade() {
	const NEW: &str="8c4478cfbfac8a193d0a6549387229dfedee06de";
	const SDK: &str="ca568d5a28aa16a2a4fbb853531d869ad22a9724";
	let handoff=admitted_directory("C_FUNDED_UPGRADE_HANDOFF");
	assert_eq!(fs::metadata(&handoff).unwrap().permissions().mode() & 0o777,0o700);
	let verify=admitted_directory("C_FUNDED_UPGRADE_VERIFY_ROOT");
	assert!(verify.starts_with(&handoff));assert_eq!(fs::metadata(&verify).unwrap().permissions().mode() & 0o777,0o700);
	assert_eq!(fs::read_dir(&verify).unwrap().count(),0,"preserve any prior restore; no unchanged retry");
	let clone:Value=serde_json::from_slice(&private_file(&handoff,&handoff.join("CLONE-8C-READY.json"),16_384)).unwrap();
	assert_eq!(clone["schema_version"],1);assert_eq!(clone["new_production"],NEW);
	assert_eq!(clone["strict_Db_connect_calls"],2);assert_eq!(clone["old_history_rows_preserved"],76);
	assert_eq!(clone["final_history_rows"],78);assert_eq!(clone["application_tables_unchanged"],true);
	assert_eq!(clone["database_name"],"c_funded_own_clone_8c");
	let runtime:Value=serde_json::from_slice(&private_file(&handoff,&handoff.join("NEW-8C-RUNTIME.json"),16_384)).unwrap();
	assert_eq!(runtime["schema_version"],1);assert_eq!(runtime["new_production"],NEW);assert_eq!(runtime["sdk_source"],SDK);
	assert_eq!(runtime["source_ready_sha256"],clone["source_ready_sha256"]);
	assert_eq!(runtime["source_signed_rights_sha256"],clone["source_signed_rights_sha256"]);
	assert_eq!(runtime["network"],"regtest");
	assert_eq!(runtime["genesis"],bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash().to_string());
	let pid=u32::try_from(runtime["captaind_pid"].as_u64().unwrap()).unwrap();assert!(pid>1);
	let exe=fs::read_link(format!("/proc/{pid}/exe")).unwrap();
	assert_eq!(digest(&fs::read(&exe).unwrap()),runtime["captaind_exe_sha256"].as_str().unwrap());
	let version=tokio::time::timeout(Duration::from_secs(3),tokio::process::Command::new(&exe).arg("--version").output()).await.unwrap().unwrap();
	assert!(version.status.success() && version.stdout.len()<=4096);
	assert!(std::str::from_utf8(&version.stdout).unwrap().contains(NEW),"actual running executable must report exact retained qualified new pin");
	let cap_config=PathBuf::from(runtime["captaind_config_path"].as_str().unwrap());
	assert_eq!(digest(&private_file(&handoff,&cap_config,65_536)),runtime["captaind_config_sha256"].as_str().unwrap());
	let cmdline=fs::read(format!("/proc/{pid}/cmdline")).unwrap();
	assert!(cmdline.split(|b|*b==0).any(|arg|arg==cap_config.as_os_str().as_encoded_bytes()));
	let cfg_bytes=private_file(&handoff,&handoff.join("new-client-config.toml"),65_536);
	assert_eq!(digest(&cfg_bytes),runtime["new_client_config_sha256"].as_str().unwrap());
	let mut cfg:bark::Config=toml::from_str(std::str::from_utf8(&cfg_bytes).unwrap()).unwrap();
	assert_eq!(cfg.server_address,runtime["server_address"].as_str().unwrap());
	assert!(cfg.server_address.starts_with("http://127.0.0.1:"));
	assert!(cfg.esplora_address.is_none());
	let core_url=cfg.bitcoind_address.as_ref().unwrap();assert!(core_url.starts_with("http://127.0.0.1:"));
	let cookie=cfg.bitcoind_cookiefile.as_ref().unwrap();assert!(fs::canonicalize(cookie).unwrap().starts_with(&handoff));
	let core=bitcoincore_rpc::Client::new(core_url,bitcoincore_rpc::Auth::CookieFile(cookie.clone())).unwrap();
	let chain=core.get_blockchain_info().unwrap();assert_eq!(chain.chain,Network::Regtest);assert!(!chain.initial_block_download);
	assert_eq!(core.get_block_hash(0).unwrap(),bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash());
	cfg.daemon_manual_sync=true;
	let payer_seed=PathBuf::from(std::env::var_os("C_FUNDED_UPGRADE_PAYER_SEED_FILE").expect("private source seed only"));
	let recipient_seed=PathBuf::from(std::env::var_os("C_FUNDED_UPGRADE_RECIPIENT_SEED_FILE").expect("private source seed only"));
	let payer=fresh_seed_wallet(&verify,"fresh-payer",&payer_seed,&handoff,&cfg).await;
	let recipient=fresh_seed_wallet(&verify,"fresh-recipient",&recipient_seed,&handoff,&cfg).await;
	let payer_discovered=discovered_signed_rights(&payer).await;
	let recipient_discovered=discovered_signed_rights(&recipient).await;
	let payer_sat=payer_discovered.values().map(|v|v["amount_sat"].as_u64().unwrap()).sum::<u64>();
	let recipient_sat=recipient_discovered.values().map(|v|v["amount_sat"].as_u64().unwrap()).sum::<u64>();
	assert_eq!((payer_sat,recipient_sat),(74_670,25_000));
	let mut discovered=payer_discovered.clone();for (id,coin) in &recipient_discovered {assert!(discovered.insert(id.clone(),coin.clone()).is_none(),"same right cannot belong to both fresh seeds");}
	assert!(discovered.len()<=32);assert_eq!(payer_sat+recipient_sat,99_670);
	// Native seed-only discovery is now complete for BOTH owners. Only now read
	// any original READY/expected ID/body/proof map to audit what was recovered.
	let ready_bytes=private_file(&handoff,&handoff.join("READY.json"),1_048_576);
	assert_eq!(digest(&ready_bytes),clone["source_ready_sha256"].as_str().unwrap());
	let ready:Value=serde_json::from_slice(&ready_bytes).unwrap();assert_eq!(ready["old_commit"],OLD);
	let expected_bytes=private_file(&handoff,&handoff.join("signed-rights-private.json"),8_388_608);
	assert_eq!(digest(&expected_bytes),clone["source_signed_rights_sha256"].as_str().unwrap());
	assert_eq!(ready["rights_sha256"],clone["source_signed_rights_sha256"]);
	let expected:BTreeMap<String,Value>=serde_json::from_slice(&expected_bytes).unwrap();
	assert_eq!(discovered.keys().collect::<Vec<_>>(),expected.keys().collect::<Vec<_>>());
	for (id,actual) in &discovered {for field in ["amount_sat","user_pubkey","mailbox_pk","vtxo","fallback_record","key_link"] {assert_eq!(actual[field],expected[id][field],"original signed entitlement/proof field {field} changed");}}
	// The same exact native sync, with no auditor hints injected, is idempotent.
	payer.sync().await;recipient.sync().await;
	payer.sync_mailbox().await.unwrap();recipient.sync_mailbox().await.unwrap();
	assert_eq!(discovered_signed_rights(&payer).await,payer_discovered);
	assert_eq!(discovered_signed_rights(&recipient).await,recipient_discovered);
	exclusive(&verify.join("RESULT.json"),&serde_json::to_vec(&json!({"schema_version":1,"result":"PASS","source_ready_sha256":clone["source_ready_sha256"],"source_signed_rights_sha256":clone["source_signed_rights_sha256"],"new_production":NEW,"sdk_source":SDK,"native_discovery_before_expected_lookup":true,"old_wallet_database_opened":false,"payer_sat":payer_sat,"recipient_sat":recipient_sat,"total_sat":payer_sat+recipient_sat,"rights_count":discovered.len(),"restored_public_map_sha256":digest(&serde_json::to_vec(&discovered).unwrap()),"repeat_sync_identical":true,"post_upgrade_spend":false})).unwrap());
}
