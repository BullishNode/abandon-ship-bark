//! Real native epoch admission: no fabricated wallet or monetary SQL state.
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::time::Duration;

use bitcoin::{FeeRate, Transaction};
use bitcoin::consensus::{deserialize,serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::{BlockDelta, TransactionExt};
use bitcoin_ext::rpc::RpcApi;
use serde_json::json;
use server::database::Db;
use server_log::{ClaimBroadcast,ProgressBroadcast,RoundFinished};
use server_rpc::protos;

use ark_testing::{sat, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::daemon::captaind::{Captaind, WalletAdminClient};
use ark_testing::util::FutureExt;
use ark_testing::exit::progress_exit_until_awaiting_delta;

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


/// Real deadline progression and claim with the epoch enabled. RBF uses an
/// independently confirmed spare fee coin; this does not qualify retained-input RBF.
#[tokio::test]
async fn urgent_progress_rbf_and_claim_debit_full_fees_without_ordinary_quote_cap() {
	let ctx=TestContext::new("bark_sdk/urgent_epoch_progress_rbf_claim").await;
	let quiet=FeeRate::from_sat_per_vb_u32(1);
	let urgent=FeeRate::from_sat_per_vb_u32(2);
	let srv=ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(sat(50_020_000)).cfg(move|c| {
			c.vtxo_lifetime=BlockDelta::new(128);c.round_interval=Duration::from_secs(3600);
			c.fee_estimator.max_fee_rate=Some(quiet);
		}).watchmand_cfg(move|c| {
			c.watchman.reaction_interval=Duration::from_secs(3600);
			c.watchman.sweep_interval=Duration::from_secs(3600);
			c.watchman.incremental_relay_fee=urgent;
			c.fee_estimator.fallback_fee_rate_fast=urgent;
			c.fee_estimator.fallback_fee_rate_regular=urgent;
			c.fee_estimator.fallback_fee_rate_slow=urgent;
			c.fee_estimator.max_fee_rate=Some(quiet);
		}).recovery_fee_headroom(sat(20_000)).create().await;
	ctx.fund_captaind(&srv,sat(49_980_000)).await;
	let wm=srv.watchmand();
	for _ in 0..2 {ctx.bitcoind().fund_addr(wm.wait_wallet_address().await,sat(30_000)).await;}
	enable_fixture_epoch(&ctx,&srv,100_000).await;
	let db=Db::connect(&srv.config().postgres).await.unwrap();
	let bark=ctx.bark("honest",&srv).funded(sat(1_000_000)).create().await;
	bark.board_and_confirm_and_register(&ctx,sat(200_000)).await;bark.sync().await;
	let exit_point=bark.vtxo_ids().await[0].to_point();
	let evil=bark.full_clone("stale").await;
	ctx.refresh_all(&srv,&[&bark]).await;
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;bark.sync().await;
	evil.start_exit_all().await;
	progress_exit_until_awaiting_delta(&ctx,&evil).await;
	let tip=ctx.generate_blocks(wm.config().watchman.progress_grace_period.to_u32()).await;
	wm.wait_for_sync_height(tip).await;
	let mut progress=wm.subscribe_log::<ProgressBroadcast>();
	let mut claims=wm.subscribe_log::<ClaimBroadcast>();
	wm.trigger_sweep().await;
	let first=progress.recv().wait(Duration::from_secs(20)).await.expect("actual epoch progression must publish");
	ctx.await_transactions_across_nodes([first.cpfp_txid],[wm.bitcoind().as_ref()]).await;
	let core=wm.bitcoind().sync_client();
	let parent=core.get_raw_transaction(&first.txid,None).unwrap();
	let original=core.get_raw_transaction(&first.cpfp_txid,None).unwrap();
	let (anchor,_)=parent.fee_anchor().unwrap();
	let old_inputs=original.input.iter().filter(|i|i.previous_output!=anchor)
		.map(|i|i.previous_output).collect::<BTreeSet<_>>();
	assert_eq!(old_inputs.len(),1,"source fixture must use exactly one original confirmed fee input");
	let original_fee=actual_fee(&core,&original);
	assert!(original_fee>quiet.fee_wu(original.weight()).unwrap().to_sat(),"actual urgent child exceeds ordinary quote cap");
	let before=db.read(async|t|Ok(t.query_one("SELECT COALESCE(sum(fee_sat),0)::bigint FROM fee_allowance_candidate WHERE urgent",&[]).await?.get::<_,i64>(0))).await.unwrap();
	// Wait for the real owning wallet to exclude the old pending spend. The
	// replacement must use a distinct confirmed spare, never unconfirmed change.
	let endpoint=wm.config().admin_address.unwrap();
	let mut rpc=WalletAdminClient::connect(format!("http://{endpoint}")).await.unwrap();
	tokio::time::timeout(Duration::from_secs(20),async {
		loop {
			let inventory=rpc.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap();
			let eligible=inventory.utxos.iter().filter(|u|u.confirmed_fee_eligible)
				.map(|u|u.outpoint.clone()).collect::<BTreeSet<_>>();
			if !eligible.is_empty() && old_inputs.iter().all(|p|!eligible.contains(&p.to_string())) {break}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("setup-unqualified: native wallet must observe pending child and confirmed spare");
	for point in &old_inputs {assert!(core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().is_none());}
	let delta=-i64::try_from(original_fee/2).unwrap();assert!(delta<0);
	core.call::<bool>("prioritisetransaction",&[json!(first.cpfp_txid),json!(0),json!(delta)]).unwrap();
	let entry:serde_json::Value=core.call("getmempoolentry",&[json!(first.cpfp_txid)]).unwrap();
	let modified=bitcoin::Amount::from_btc(entry["fees"]["chunk"].as_f64().unwrap()).unwrap().to_sat();
	assert!(modified>0 && modified<original_fee,"actual Core chunk estimate must require replacement");
	wm.trigger_sweep().await;
	let second=progress.recv().wait(Duration::from_secs(20)).await.expect("epoch-backed RBF must publish from actual spare");
	assert_eq!(second.txid,first.txid);assert_ne!(second.cpfp_txid,first.cpfp_txid);
	ctx.await_transactions_across_nodes([second.cpfp_txid],[wm.bitcoind().as_ref()]).await;
	let replacement=core.get_raw_transaction(&second.cpfp_txid,None).unwrap();
	let new_inputs=replacement.input.iter().filter(|i|i.previous_output!=anchor)
		.map(|i|i.previous_output).collect::<BTreeSet<_>>();
	assert!(!new_inputs.is_empty() && old_inputs.is_disjoint(&new_inputs),"this finite control qualifies fresh-spare RBF only");
	let replacement_fee=actual_fee(&core,&replacement);assert!(replacement_fee>original_fee);
	let after=db.read(async|t|Ok(t.query_one("SELECT COALESCE(sum(fee_sat),0)::bigint FROM fee_allowance_candidate WHERE urgent",&[]).await?.get::<_,i64>(0))).await.unwrap();
	assert_eq!(after-before,replacement_fee as i64,"RBF charges full new signed fee and never renews the parent debit");
	let tip=ctx.generate_blocks(1).await;wm.wait_for_sync_height(tip).await;
	wm.trigger_sweep().await;
	let claim=claims.recv().wait(Duration::from_secs(20)).await.expect("actual urgent forfeit claim must publish");
	assert!(claim.vtxo_ids.iter().any(|v|v.to_point().txid==first.txid));
	ctx.await_transactions_across_nodes([claim.txid],[wm.bitcoind().as_ref()]).await;
	let claimed=core.get_raw_transaction(&claim.txid,None).unwrap();
	assert!(actual_fee(&core,&claimed)>quiet.fee_wu(claimed.weight()).unwrap().to_sat(),"actual Claim also bypasses ordinary quote cap");
	let tip=ctx.generate_blocks(1).await;wm.wait_for_sync_height(tip).await;
	assert!(core.get_tx_out(&exit_point.txid,exit_point.vout,Some(true)).unwrap().is_none(),"forfeited exit cannot remain spendable by attacker");
	let rows=db.read(async|t|Ok(t.query("SELECT candidate_bytes,fee_sat FROM fee_allowance_candidate WHERE urgent ORDER BY id",&[]).await?)).await.unwrap();
	let mut total=0u64;let mut children=0;let mut parents=0;let mut claim_count=0;let mut frames=Vec::new();
	let ceiling=FeeRate::from_sat_per_kwu(2000);
	for row in rows {
		let value:serde_json::Value=serde_json::from_slice(&row.get::<_,Vec<u8>>(0)).unwrap();
		let bytes:Vec<u8>=serde_json::from_value(value["raw"].clone()).unwrap();
		let tx:Transaction=deserialize(&bytes).unwrap();let fee=actual_fee(&core,&tx);
		assert_eq!(row.get::<_,i64>(1),fee as i64);total=total.checked_add(fee).unwrap();
		assert!(fee<=ceiling.fee_wu(tx.weight()).unwrap().to_sat() && fee<=2000);
		match value["path"].as_str().unwrap() {
			"ProgressParent"=>{assert_eq!(tx,parent);parents+=1;},
			"ProgressChild"=>{
				assert!(tx==original || tx==replacement);children+=1;
				assert!(fee+actual_fee(&core,&parent)<=ceiling.fee_wu(tx.weight()+parent.weight()).unwrap().to_sat());
			},
			"Claim"=>{assert_eq!(tx,claimed);claim_count+=1;},
			other=>panic!("unexpected native urgent class {other}"),
		}
		frames.push(value);
	}
	assert_eq!((parents,children,claim_count),(1,2,1));assert!(total<=4000);
	assert!(db.read(async|t|Ok(t.query_one("SELECT COALESCE(sum(fee_sat),0)::bigint FROM fee_allowance_candidate WHERE NOT urgent",&[]).await?.get::<_,i64>(0))).await.unwrap()>0);
	private_original(&ctx.datadir.join("urgent-epoch-actual-originals.json"),&serde_json::to_vec(&json!({
		"scope":"real confirmed-spare RBF; retained same-input unqualified","old_inputs":old_inputs,"new_inputs":new_inputs,
		"first_child_raw":serialize(&original),"replacement_raw":serialize(&replacement),"parent_raw":serialize(&parent),
		"claim_raw":serialize(&claimed),"original_signed_fee_sat":original_fee,"replacement_signed_fee_sat":replacement_fee,
		"native_urgent_total_sat":total,"rows":frames})).unwrap());
}
