//! Real native epoch admission: no fabricated wallet or monetary SQL state.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::time::Duration;

use bitcoin::{FeeRate, Transaction};
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use serde_json::json;
use server::database::Db;
use server_log::RoundFinished;
use server_rpc::protos;

use ark_testing::{sat, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::daemon::captaind::{Captaind, WalletAdminClient};
use ark_testing::util::FutureExt;

async fn prepare_fixture_epoch(ctx:&TestContext,srv:&Captaind,ordinary:u64)->server::fee_allowance::Config {
	let core=ctx.bitcoind().sync_client();
	let key=srv.ark_info().await.server_pubkey;
	let endpoint=srv.watchmand().config().admin_address.unwrap();
	let mut watch=WalletAdminClient::connect(format!("http://{endpoint}")).await.unwrap();
	let inventory=watch.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap();
	assert_eq!(inventory.wallet,"watchman");
	let policy=json!({
		"epoch_id":sha256::Hash::hash(format!("native-test-{key}").as_bytes()).to_string(),
		"genesis":core.get_block_hash(0).unwrap().to_string(),"server_pubkey":key.to_string(),
		"ordinary_sat":ordinary,"urgent_sat":4000,"ordinary_sat_per_kwu":250,
		"urgent_child_sat_per_kwu":2000,"urgent_package_sat_per_kwu":2000,
		"urgent_max_candidate_sat":2000,"urgent_required_confirmed_sat":20000,
		"watchman_descriptor_sha256":sha256::Hash::hash(inventory.public_descriptor.as_bytes()).to_string(),
		"watchman_admin_address":endpoint.to_string(),
	});
	let bytes=serde_json::to_vec(&policy).unwrap();
	let file=ctx.datadir.join("native-fee-policy.json");
	let mut out=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file).unwrap();
	out.write_all(&bytes).unwrap();out.sync_all().unwrap();
	let history=ctx.datadir.join("native-fee-history");
	fs::DirBuilder::new().mode(0o700).create(&history).unwrap();
	let config=server::fee_allowance::Config {initialize_empty:true,opening:None,policy_file:file,
		policy_sha256:sha256::Hash::hash(&bytes).to_string(),history_dir:history};
	config
}

async fn enable_fixture_epoch(ctx:&TestContext,srv:&Captaind,ordinary:u64) {
	let config=prepare_fixture_epoch(ctx,srv,ordinary).await;
	// Initialization is explicitly paused before any operator publication.
	srv.stop().await.unwrap();srv.watchmand().stop().await.unwrap();
	srv.config_mut().fee_allowance=Some(config.clone());
	{
		let mut cfg=srv.watchmand().config_mut();cfg.fee_allowance=Some(config);
		let path=cfg.data_dir.join(ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE);
		cfg.write_into(&mut fs::File::create(path).unwrap()).unwrap();
	}
	srv.watchmand().start().await.unwrap();srv.start().await.unwrap();
}

fn actual_fee(core:&bitcoin_ext::rpc::BitcoinRpcClient,tx:&Transaction)->u64 {
	let gross=tx.input.iter().map(|i| {
		let p=core.get_raw_transaction(&i.previous_output.txid,None).unwrap();
		p.output[i.previous_output.vout as usize].value.to_sat()
	}).sum::<u64>();
	gross.checked_sub(tx.output.iter().map(|o|o.value.to_sat()).sum()).unwrap()
}

async fn rejected_refresh(ctx:&TestContext,srv:&Captaind,bark:&ark_testing::Bark,needle:&str) {
	let result=tokio::time::timeout(Duration::from_secs(45),
		ctx.trigger_rounds_until(srv,bark.try_refresh_all_no_retry())).await
		.expect("native request must reach a known refusal; timeout is setup-unqualified");
	assert!(result.is_err(),"native fee guard must reject before another funding publication");
	let logs=fs::read_to_string(srv.config().data_dir.join(ark_testing::daemon::STDOUT_LOGFILE)).unwrap();
	assert!(logs.contains(needle),"must corroborate the actual native guard, not an unrelated refusal");
}

#[tokio::test]
async fn ordinary_round_allowance_survives_restart_and_refuses_second_publication() {
	let ctx=TestContext::new("bark_sdk/ordinary_round_allowance_survives_restart").await;
	let srv=ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(sat(50_020_000)).cfg(|c| {
			c.vtxo_lifetime=BlockDelta::new(128);c.min_board_amount=sat(330);
			c.round_interval=Duration::from_secs(3600);
			c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(1));
		}).watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	ctx.fund_captaind(&srv,sat(49_980_000)).await;
	ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await,sat(30_000)).await;
	enable_fixture_epoch(&ctx,&srv,260).await;
	let db=Db::connect(&srv.config().postgres).await.unwrap();
	let core=ctx.bitcoind().sync_client();
	let bark=ctx.bark("receiver",&srv).funded(sat(1_000_000)).create().await;
	bark.board(sat(80_000)).await;ctx.generate_blocks(BOARD_CONFIRMATIONS).await;bark.sync().await;
	let mut rounds=srv.subscribe_log::<RoundFinished>();
	ctx.refresh_all(&srv,&[&bark]).await;
	let txid=rounds.recv().wait(Duration::from_secs(30)).await.unwrap().txid;
	ctx.await_transaction(txid).await;
	let signed=core.get_raw_transaction(&txid,None).unwrap();
	let fee=actual_fee(&core,&signed);
	assert!((131..=260).contains(&fee),"setup-unqualified: fixed260 allowance needs one genuine round above130");
	assert!(fee<=FeeRate::from_sat_per_vb_u32(1).fee_wu(signed.weight()).unwrap().to_sat());
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;bark.sync().await;
	let before=db.read(async|t| Ok(t.query_one("SELECT count(*),COALESCE(sum(fee_sat),0)::bigint FROM fee_allowance_candidate WHERE urgent=false",&[]).await?
		.get::<_,i64>(1))).await.unwrap();assert_eq!(before,fee as i64);
	srv.stop().await.unwrap();srv.start().await.unwrap();
	bark.set_ark_url(&srv).await;
	let mut rpc=srv.get_wallet_rpc().await;
	let inputs=rpc.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().rounds.unwrap();
	rejected_refresh(&ctx,&srv,&bark,"durable epoch fee allowance exhausted").await;
	let after=db.read(async|t| Ok(t.query_one("SELECT count(*),COALESCE(sum(fee_sat),0)::bigint FROM fee_allowance_candidate WHERE urgent=false",&[]).await?
		.get::<_,i64>(1))).await.unwrap();assert_eq!(before,after,"restart and refusal cannot renew or debit another allowance");
	for input in inputs.utxos.iter().filter(|u|u.confirmed_fee_eligible) {
		let point=input.outpoint.parse::<bitcoin::OutPoint>().unwrap();
		assert!(core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().is_some());
	}
	assert_eq!(db.read(async|t|Ok(t.query_one("SELECT count(*) FROM nursery_tx WHERE kind='round'",&[]).await?.get::<_,i64>(0))).await.unwrap(),1);
	assert_eq!(db.read(async|t|Ok(t.query_one("SELECT COALESCE(sum(held_sat),0)::bigint FROM recovery_source_hold",&[]).await?.get::<_,i64>(0))).await.unwrap(),0);
}

#[tokio::test]
async fn ordinary_round_refuses_actual_empty_urgent_wallet() {
	let ctx=TestContext::new("bark_sdk/ordinary_round_refuses_actual_empty_urgent_wallet").await;
	let srv=ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(sat(50_020_000))
		.cfg(|c|{c.vtxo_lifetime=BlockDelta::new(128);c.round_interval=Duration::from_secs(3600);
			c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(1));})
		.watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	ctx.fund_captaind(&srv,sat(49_980_000)).await;
	enable_fixture_epoch(&ctx,&srv,100_000).await;
	let db=Db::connect(&srv.config().postgres).await.unwrap();
	let bark=ctx.bark("receiver",&srv).funded(sat(1_000_000)).create().await;
	bark.board(sat(80_000)).await;ctx.generate_blocks(BOARD_CONFIRMATIONS).await;bark.sync().await;
	let endpoint=srv.watchmand().config().admin_address.unwrap();
	let mut rpc=WalletAdminClient::connect(format!("http://{endpoint}")).await.unwrap();
	let inventory=rpc.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap();
	assert!(inventory.utxos.is_empty(),"genuine fresh watchman receives no funding in this case");
	rejected_refresh(&ctx,&srv,&bark,"confirmed unreserved urgent inputs below declared floor").await;
	assert_eq!(db.read(async|t|Ok(t.query_one("SELECT count(*) FROM fee_allowance_candidate",&[]).await?.get::<_,i64>(0))).await.unwrap(),0);
	assert_eq!(db.read(async|t|Ok(t.query_one("SELECT count(*) FROM nursery_tx WHERE kind='round'",&[]).await?.get::<_,i64>(0))).await.unwrap(),0);
}

fn private_original(path:&std::path::Path, bytes:&[u8]) {
	let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();
	file.write_all(bytes).unwrap();file.sync_all().unwrap();
}
fn source_file(path:&std::path::Path)->serde_json::Value {
	let bytes=fs::read(path).unwrap();
	json!({"path":path,"bytes":bytes.len(),"sha256":sha256::Hash::hash(&bytes).to_string()})
}
async fn pg_command(name:&str,args:&[&str]) {
	let executable=std::path::PathBuf::from(std::env::var("POSTGRES_BINS").unwrap()).join(name);
	let output=tokio::process::Command::new(executable).args(args).output().await.unwrap();
	assert!(output.status.success(),"owned fixture PostgreSQL command failed: {}",String::from_utf8_lossy(&output.stderr));
}

// Capture real stopped native rows, not manufactured ledger or wallet state.
// The repeated SQL framing is certificate construction; assertions below bind
// exact original Core bytes/fees, customer rows, once-only native debit/replay.
async fn stopped_opening_tables(db:&Db)->serde_json::Value {
	let tables: &[&str]=&["wallet_changeset","captaind_block","watchmand_block","checkpoint_state","vtxo","virtual_transaction","virtual_transaction_history","vtxo_history","pending_board","fallback_record","key_link","arkoor_mailbox","mailbox","ephemeral_tweak","round","round_participation","round_part_input","round_part_output","vtxo_pool","offboards","nursery_tx","sweep","expiry_settlement","expiry_cancelled_participation","htlc_vtxo","htlc_settlement","lightning_htlc_subscription","lightning_invoice","lightning_payment_attempt","lightning_payment_attempt_htlc_vtxo","lightning_invoice_history","lightning_payment_attempt_history","lightning_htlc_subscription_history","recovery_reserve","recovery_receipt","recovery_source_hold","recovery_capital_release"];
	db.read(async |t| {
		t.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",&[]).await?;
		t.batch_execute("SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO,YMD'; SET LOCAL bytea_output='hex'; SET LOCAL extra_float_digits=3;").await?;
		let mut result=serde_json::Map::new();
		for table in tables {
			let rows=t.query(&format!("SELECT row_to_json(x)::text AS body FROM {table} x ORDER BY (row_to_json(x)::text) COLLATE \"C\""),&[]).await?;
			let mut bytes=0usize;let mut framed=Vec::new();
			for row in &rows {
				let body:String=row.get(0);bytes+=body.len();
				framed.extend_from_slice(&(body.len()as u64).to_le_bytes());framed.extend_from_slice(body.as_bytes());
			}
			result.insert((*table).to_string(),json!({"rows":rows.len(),"bytes":bytes,"sha256":sha256::Hash::hash(&framed).to_string()}));
		}
		Ok(serde_json::Value::Object(result))
	}).await.unwrap()
}

#[tokio::test]
async fn confirmed_sweep_opening_imports_original_debit_once_after_database_restore() {
	use std::collections::{BTreeMap,BTreeSet};
	use bitcoin::consensus::{deserialize,serialize};
	use server::wallet::WalletKind;
	let ctx=TestContext::new("bark_sdk/confirmed_sweep_fee_opening").await;
	let rate=FeeRate::from_sat_per_vb_u32(1);
	let srv=ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(sat(50_020_000)).cfg(move |c| {
			c.vtxo_lifetime=BlockDelta::new(128);c.min_board_amount=sat(330);
			c.round_interval=Duration::from_secs(3600);c.expiry_payout.enabled=false;
			c.fee_estimator.max_fee_rate=Some(rate);
		}).watchmand_cfg(move |c| {
			c.fee_estimator.fallback_fee_rate_fast=rate;
			c.fee_estimator.fallback_fee_rate_regular=rate;
			c.fee_estimator.fallback_fee_rate_slow=rate;
			c.fee_estimator.max_fee_rate=Some(rate);
			c.watchman.sweep_interval=Duration::from_secs(1);
		}).recovery_fee_headroom(sat(20_000)).create().await;
	ctx.fund_captaind(&srv,sat(49_980_000)).await;
	let db=Db::connect(&srv.config().postgres).await.unwrap();
	let wallet=ctx.bark_sdk("absent",&srv).cfg(|c|c.daemon_manual_sync=true)
		.funded(sat(1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board=wallet.board_amount(sat(80_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;wallet.sync().await;
	let coin=wallet.spendable_vtxos().await.unwrap().into_iter().next().unwrap();
	let coin=wallet.get_full_vtxo(coin.id()).await.unwrap();drop(wallet);
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx,&db,&[coin]).await;
	tokio::time::timeout(Duration::from_secs(30),async {
		loop {
			let count=db.read(async|t|Ok(t.query_one("SELECT count(*) FROM recovery_receipt WHERE proof_state='physical_bound'",&[]).await?.get::<_,i64>(0))).await.unwrap();
			if count==1 {break}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("genuine sweep must enter native owning wallet and gross reserve first");
	let mut config=prepare_fixture_epoch(&ctx,&srv,10_000).await;
	config.initialize_empty=false;
	srv.stop().await.unwrap();srv.watchmand().stop().await.unwrap();
	let core=ctx.bitcoind().sync_client();
	let receipts=db.read(async|t|Ok(t.query("SELECT transaction_bytes FROM recovery_receipt ORDER BY id",&[]).await?)).await.unwrap();
	assert_eq!(receipts.len(),1);
	let original:Transaction=deserialize(&receipts[0].get::<_,Vec<u8>>(0)).unwrap();
	assert_eq!(core.get_raw_transaction(&original.compute_txid(),None).unwrap(),original);
	let fee=actual_fee(&core,&original);assert!(fee>0 && fee<10_000);
	let parents=original.input.iter().map(|i|i.previous_output.txid).collect::<BTreeSet<_>>()
		.into_iter().map(|id|serialize(&core.get_raw_transaction(&id,None).unwrap())).collect::<Vec<_>>();
	let policy:serde_json::Value=serde_json::from_slice(&fs::read(&config.policy_file).unwrap()).unwrap();
	let tables=stopped_opening_tables(&db).await;
	let customer_cut=tables["vtxo"].clone();let held_cut=tables["recovery_source_hold"].clone();
	assert_eq!(tables["expiry_settlement"]["rows"],0);
	let (descriptors,graphs)=db.read(async|t| {
		let mut descriptors=BTreeMap::new();let mut graphs=BTreeMap::new();
		for kind in [WalletKind::Rounds,WalletKind::Watchman] {
			let graph=t.read_aggregate_changeset(kind).await?.unwrap();
			descriptors.insert(kind.name(),sha256::Hash::hash(graph.descriptor.unwrap().to_string().as_bytes()).to_string());
			graphs.insert(kind.name(),graph.tx_graph.txs.iter().map(|tx|tx.compute_txid().to_string()).collect::<BTreeSet<_>>());
		}Ok((descriptors,graphs))
	}).await.unwrap();
	let external=graphs.values().flat_map(|ids|ids.iter().cloned()).filter(|id|id!=&original.compute_txid().to_string()).collect::<BTreeSet<_>>();
	let frozen=ctx.datadir.join("opening-source-private");fs::DirBuilder::new().mode(0o700).create(&frozen).unwrap();
	let mut logs=Vec::new();
	for (name,directory) in [("cap",srv.config().data_dir.clone()),("watch",srv.watchmand().config().data_dir.clone())] {
		let path=frozen.join(format!("{name}.log"));
		private_original(&path,&fs::read(directory.join(ark_testing::daemon::STDOUT_LOGFILE)).unwrap());logs.push(source_file(&path));
	}
	let manifest=frozen.join("native-source-cut.json");
	private_original(&manifest,&serde_json::to_vec(&json!({"fixture":ctx.datadir,"server_key":policy["server_pubkey"],
		"genesis":policy["genesis"],"original_sweep":original.compute_txid().to_string(),"original_signed_fee_sat":fee,
		"external_funding_txids":external,"source_tables":tables})).unwrap());
	let tip=core.get_block_count().unwrap();
	let certificate=json!({"version":1,"policy_sha256":config.policy_sha256,"epoch_id":policy["epoch_id"],
		"genesis":policy["genesis"],"server_pubkey":policy["server_pubkey"],
		"created_at_unix":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
		"tip_height":tip,"tip_hash":core.get_block_hash(tip).unwrap().to_string(),"minimum_confirmations":1,
		"ordinary_total_sat":10000,"urgent_total_sat":4000,"previously_booked_ordinary_sat":0,"previously_booked_urgent_sat":0,
		"import_ordinary_sat":fee,"remaining_ordinary_sat":10000-fee,"remaining_urgent_sat":4000,
		"source_manifest":source_file(&manifest),"publication_logs":logs,"tables":tables,
		"wallet_descriptors":descriptors,"wallet_transactions":graphs,"external_funding_transactions":external,
		"candidates":[{"epoch_id":policy["epoch_id"],"path":"Sweep","raw":serialize(&original),"input_parents":parents,
			"package_parents":[],"package_input_parents":[]}]});
	let path=frozen.join("opening.json");let bytes=serde_json::to_vec(&certificate).unwrap();private_original(&path,&bytes);
	config.opening=Some(server::fee_allowance_opening::Config {file:path,sha256:sha256::Hash::hash(&bytes).to_string()});
	let postgres=srv.config().postgres.clone();let port=postgres.port.to_string();
	let backup=frozen.join("before-import.dump");
	pg_command("pg_dump",&["-h","127.0.0.1","-p",&port,"-Fc","-f",backup.to_str().unwrap(),&postgres.name]).await;
	srv.config_mut().fee_allowance=Some(config.clone());
	{
		let mut watch=srv.watchmand().config_mut();watch.fee_allowance=Some(config.clone());
		let path=watch.data_dir.join(ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE);
		watch.write_into(&mut fs::File::create(path).unwrap()).unwrap();
	}
	// A real nonzero source cannot be admitted with a forged fresh remainder.
	// This negative invokes native startup; no monetary rows are manufactured.
	let mut inconsistent=certificate.clone();inconsistent["remaining_ordinary_sat"]=json!(10_000);
	let wrong_path=frozen.join("wrong-opening.json");let wrong_bytes=serde_json::to_vec(&inconsistent).unwrap();
	private_original(&wrong_path,&wrong_bytes);
	let mut wrong_config=config.clone();wrong_config.opening=Some(server::fee_allowance_opening::Config {
		file:wrong_path,sha256:sha256::Hash::hash(&wrong_bytes).to_string()});
	srv.config_mut().fee_allowance=Some(wrong_config);
	assert!(srv.start().await.is_err(),"actual native opening must refuse inconsistent TOTAL/import/remainder");
	let logs=fs::read_to_string(srv.config().data_dir.join(ark_testing::daemon::STDOUT_LOGFILE)).unwrap();
	let errors=fs::read_to_string(srv.config().data_dir.join(ark_testing::daemon::STDERR_LOGFILE)).unwrap();
	assert!(format!("{logs}\n{errors}").contains("opening totals/booked/imported/remainder inconsistent"),"must be the actual native accounting refusal");
	assert_eq!(db.read(async|t|Ok(t.query_one("SELECT count(*) FROM fee_allowance_policy",&[]).await?.get::<_,i64>(0))).await.unwrap(),0);
	assert!(!config.history_dir.join("fee-opening-validated.json").exists());
	srv.config_mut().fee_allowance=Some(config.clone());
	// Start cap only: no dispatcher, payout, pool or reaction can add exposure.
	srv.start().await.unwrap();
	let recorded=db.read(async|t|Ok(t.query_one("SELECT count(*),sum(fee_sat)::bigint FROM fee_allowance_candidate",&[]).await?)).await.unwrap();
	assert_eq!(recorded.get::<_,i64>(0),1);assert_eq!(recorded.get::<_,i64>(1),fee as i64);
	srv.stop().await.unwrap();srv.start().await.unwrap();srv.stop().await.unwrap();
	assert_eq!(db.read(async|t|Ok(t.query_one("SELECT sum(fee_sat)::bigint FROM fee_allowance_candidate",&[]).await?.get::<_,i64>(0))).await.unwrap(),fee as i64);
	let restored=format!("{}-fee-opening",postgres.name);
	pg_command("createdb",&["-h","127.0.0.1","-p",&port,"-T","template0",&restored]).await;
	pg_command("pg_restore",&["-h","127.0.0.1","-p",&port,"--exit-on-error","-d",&restored,backup.to_str().unwrap()]).await;
	let mut restored_config=postgres;restored_config.name=restored.clone();
	let restored_db=Db::connect(&restored_config).await.unwrap();
	assert_eq!(restored_db.read(async|t|Ok(t.query_one("SELECT count(*) FROM fee_allowance_candidate",&[]).await?.get::<_,i64>(0))).await.unwrap(),0);
	assert_eq!(stopped_opening_tables(&restored_db).await,tables,"actual original liabilities and wallet graph restored, not fabricated");
	srv.config_mut().postgres.name=restored.clone();srv.watchmand().config_mut().postgres.name=restored;
	srv.start().await.unwrap();srv.stop().await.unwrap();
	let row=restored_db.read(async|t|Ok(t.query_one("SELECT count(*),sum(fee_sat)::bigint FROM fee_allowance_candidate",&[]).await?)).await.unwrap();
	assert_eq!(row.get::<_,i64>(0),1);assert_eq!(row.get::<_,i64>(1),fee as i64,"same original historical fee, never renewed credit");
	let after=stopped_opening_tables(&restored_db).await;
	assert_eq!(after["vtxo"],customer_cut);assert_eq!(after["recovery_source_hold"],held_cut);
	assert_eq!(after["expiry_settlement"]["rows"],0);
	let original_core=core.get_raw_transaction(&original.compute_txid(),None).unwrap();assert_eq!(original_core,original);
	private_original(&frozen.join("actual-opening-result.json"),&serde_json::to_vec(&json!({
		"original_txid":original.compute_txid().to_string(),"original_wtxid":original.compute_wtxid().to_string(),
		"original_signed_fee_sat":fee,"imported_fee_sat":row.get::<_,i64>(1),"candidate_count":row.get::<_,i64>(0),
		"total_ordinary_sat":10000,"remaining_ordinary_sat":10000-fee,"customer_frontier_preserved":after["vtxo"]==customer_cut,
		"reserve_frontier_preserved":after["recovery_source_hold"]==held_cut,"payout_count":after["expiry_settlement"]["rows"]})).unwrap());
}
