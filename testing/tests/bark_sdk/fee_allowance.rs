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

async fn enable_fixture_epoch(ctx:&TestContext,srv:&Captaind,ordinary:u64) {
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
	let config=server::fee_allowance::Config {initialize_empty:true,policy_file:file,
		policy_sha256:sha256::Hash::hash(&bytes).to_string(),history_dir:history};
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

fn actual_fee(core:&bitcoin_ext::rpc::Client,tx:&Transaction)->u64 {
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
