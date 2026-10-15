fn stress_payment(env: &Env, coins: &mut Vec<Value>, rate: u64) -> Txid {
	let coin = coins.pop().expect("declared confirmed training inventory");
	let address = env.core().get_new_address(None, None).unwrap().assume_checked();
	let funded: Value = env.core().call("walletcreatefundedpsbt", &[
		json!([{ "txid": coin["txid"], "vout": coin["vout"] }]),
		json!([{ address.to_string(): 0.001 }]), 0.into(),
		json!({ "fee_rate": rate, "add_inputs": false, "lockUnspents": true, "minconf": 1 }),
	]).unwrap();
	let signed: Value = env.core().call("walletprocesspsbt", &[funded["psbt"].clone()]).unwrap();
	let finalized: Value = env.core().call("finalizepsbt", &[signed["psbt"].clone()]).unwrap();
	assert_eq!(finalized["complete"], true);
	let tx: Transaction = bitcoin::consensus::encode::deserialize_hex(finalized["hex"].as_str().unwrap()).unwrap();
	env.core().send_raw_transaction(&tx).unwrap()
}

fn stress_estimate(env: &Env, phase: &str) -> u64 {
	let estimate: Value = env.core().call("estimatesmartfee", &[6.into(), "economical".into()]).unwrap();
	let btc = estimate["feerate"].as_f64().expect("real economical estimate");
	let sat_kvb = Amount::from_btc(btc).unwrap().to_sat();
	env.out.line("fee-estimates.jsonl", &json!({ "phase": phase, "tip": env.core().get_block_count().unwrap(),
		"estimate": estimate, "sat_kvb": sat_kvb }));
	sat_kvb
}

async fn stress_mine(env: &Env, ctx: &TestContext, txids: &[Txid]) {
	let address = "mzU8XRVhUdXtdxmSA3Vw8XeU2FDV4iBDRW";
	let _: Value = env.core().call("generateblock", &[address.into(),
		json!(txids.iter().map(ToString::to_string).collect::<Vec<_>>())]).unwrap();
	ctx.await_block_count_sync().await;
}

async fn stress_add_board(env: &Env, ctx: &TestContext, owner: &mut Owner, wallet: &bark::Wallet, amount: u64) -> Vtxo {
	let board = env.board(wallet, amount).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let id = *board.vtxos.first().unwrap();
	tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			wallet.sync().await;
			if wallet.spendable_vtxos().await.unwrap().iter().any(|c| c.id()==id) { break; }
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("new board registered before disappearance");
	let full = wallet.get_full_vtxo(id).await.unwrap();
	owner.coins.push(Coin { id: full.id().to_string(), amount: full.amount().to_sat(),
		source: "fee_stress_board", user_pubkey: full.user_pubkey().to_string() });
	owner.anchors.push(full.clone());
	full
}


// Separate declared profiles reuse the real Core estimator and actual board journey.
// No mock fee estimate, SQL coin creation, maturity bypass, minimum-hold or float top-up.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn stage5_fee_stress() {
	require_operating_build();
	let ctx=TestContext::new("bark_sdk/stage5_fee_stress").await;let out=Out::new();
	let authority=ScaleFeeAuthority::from_env();
	let srv=scale_builder(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(sat(100_000_000)).watchmand(),&ctx,&authority).cfg(move |c| {authority.apply(c);c.vtxo_lifetime=BlockDelta::new(128);
			c.min_board_amount=sat(330);c.round_interval=Duration::from_secs(3600);}).create().await;
	let db=Db::connect(&srv.config().postgres).await.unwrap();
	let env=Env{bitcoind:ctx.bitcoind_arc(),datadir:ctx.datadir.clone(),cfg:ctx.bark_default_cfg(&srv.ark_url(),Some(ctx.bitcoind())),
		db,out:out.clone(),ln_client:None,hold_client:None};
	let profile=std::env::var("STAGE5_FEE_PROFILE").unwrap_or_else(|_|"high-low".into());
	assert!(["high-low","insufficient","fragmented"].contains(&profile.as_str()));
	provision_fee_reserve(&ctx,&env,&srv).await;
	super::fallback_lightning::train_fee_estimator(&ctx).await;
	let opening=treasury_edge(&env,&srv,"opening").await;
	let (mut dust,wallet)=create_owner(env.clone(),0,Profile::Registered,srv.ark_url()).await;
	let small=stress_add_board(&env,&ctx,&mut dust,&wallet,330).await;drop(wallet);
	let (mut normal,wallet)=create_owner(env.clone(),1,Profile::Registered,srv.ark_url()).await;
	let large=stress_add_board(&env,&ctx,&mut normal,&wallet,6000).await;drop(wallet);
	let owners=vec![dust,normal];for o in &owners {out.line("owners.jsonl",&o.public());}
	let expiry=small.expiry_height().to_u32().max(large.expiry_height().to_u32());
	ctx.generate_blocks(expiry.saturating_sub(env.core().get_block_count().unwrap() as u32)+1).await;
	let mut outputs=serde_json::Map::new();
	for _ in 0..490 {outputs.insert(env.core().get_new_address(None,None).unwrap().assume_checked().to_string(),json!(0.003));}
	let _:Txid=env.core().call("sendmany",&["".into(),Value::Object(outputs),1.into(),"".into(),json!([]),true.into(),Value::Null,"unset".into(),3.into()]).unwrap();
	ctx.generate_blocks(1).await;
	let unspent:Vec<Value>=env.core().call("listunspent",&[1.into()]).unwrap();
	let mut coins=unspent.into_iter().filter(|c|Amount::from_btc(c["amount"].as_f64().unwrap()).unwrap()==sat(300_000)).collect::<Vec<_>>();
	assert_eq!(coins.len(),490);
	let waiting=(0..30).map(|_|stress_payment(&env,&mut coins,2)).collect::<Vec<_>>();
	for _ in 0..40 {let high=(0..4).map(|_|stress_payment(&env,&mut coins,500)).collect::<Vec<_>>();stress_mine(&env,&ctx,&high).await;stress_estimate(&env,"congested");}
	assert!(stress_estimate(&env,"high-ready")>=500_000,"actual high fee estimator prerequisite");
	assert!(waiting.iter().all(|t|env.core().get_raw_transaction_info(t,None).unwrap().confirmations.unwrap_or(0)==0));
	soak_restart(&srv,true).await;let enabled_utc=utc();
	tokio::time::sleep(Duration::from_secs(8)).await;
	let observed=env.db.read(async |t|Ok(t.query("SELECT a.id,a.disposition,c.txid,c.confirmed_height FROM expiry_allocation a LEFT JOIN expiry_carrier_allocation m ON a.id=m.allocation_id LEFT JOIN expiry_carrier c ON m.txid=c.txid ORDER BY a.id",&[]).await?)).await.unwrap();
	// Declared caps must defer the payable claim at high fees. Dust does not authorize
	// a below-minimum retention; its terminal disposition also needs confirmed retirement.
	assert!(!observed.iter().any(|r|r.get::<_,String>(0)==large.id().to_string() && r.get::<_,Option<i32>>(3).is_some()),"high fee profile exceeded actual declared financial caps");
	out.json("fee-deferral.json",&json!({"utc":utc(),"estimate_sat_kvb":stress_estimate(&env,"deferred"),"actual_policy_reason_required":true,"qualified":false,"owning_wallets":read_owning_wallets(&srv).await}));
	if profile=="insufficient" {
		// Internal movement of existing bank capital, predeclared before dispatch.
		// Zero eligible reserve and actual task reason must be independently proven.
		let address=srv.watchmand().wait_wallet_address().await;
		let txid=env.core().send_to_address(&address,sat(2_000_000),None,None,None,None,None,None).unwrap();
		ctx.await_transaction(txid).await;ctx.generate_blocks(7).await;
		out.line("fee-reserve-restorations.jsonl",&json!({"txid":txid,"amount_sat":2_000_000,"source":"included bank","destination":"included fee wallet","external_injection":false,"declared_before_dispatch":true,"actual_prior_unavailability_reason_required":true}));
	}
	ctx.generate_blocks(1).await;
	for _ in 0..30 {for _ in 0..10 {stress_payment(&env,&mut coins,3);}ctx.generate_blocks(1).await;stress_estimate(&env,"low-recovery");tokio::time::sleep(Duration::from_millis(300)).await;}
	assert!(stress_estimate(&env,"low-ready")<=3000,"actual low fee recovery prerequisite");
	let deadline=Instant::now()+Duration::from_secs(90);
	loop {if tokio::time::timeout(Duration::from_secs(2),soak_wait_paid(&env,&owners,deadline)).await.is_ok(){break;}
		assert!(Instant::now()<deadline,"prerequisites restored: every non-forfeited claim must drain within declared bound");ctx.generate_blocks(1).await;}
	let reconciliation=reconcile(&env,&owners,enabled_utc).await;assert!(reconciliation["failures"].as_array().unwrap().is_empty(),"{reconciliation}");out.json("reconciliation.json",&reconciliation);
	let closing=treasury_edge(&env,&srv,"pre-recovery").await;conserve_treasury(&env,&opening,&closing,0);
	srv.stop().await.unwrap();let recovery=recover(&ctx,&env,&owners,&Params::from_env()).await;
	assert!(recovery["failures"].as_array().unwrap().is_empty());assert_eq!(recovery["spends"],1);out.json("recovery.json",&recovery);
	srv.watchmand().stop().await.unwrap();ctx.bitcoind().stop().await.unwrap();
	out.json("fee-stress-result.json",&json!({"result":"UNQUALIFIED","positive_seed_spends":1,"dust_owners":1,"whole_boundary_independent_audit_required":true,"insufficient_reserve_reason_required":true,"replacement_progression_proof_missing":true}));
}


// One destination backed by 128 real boards: base input bytes alone prove
// a funding shortfall at the declared 30 sat/vB. Same build, new isolated case.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn stage5_fee_reserve_and_progression() {
	require_operating_build();let ctx=TestContext::new("bark_sdk/stage5_fee_reserve_and_progression").await;let out=Out::new();
	let profile=std::env::var("STAGE5_FEE_PROFILE").unwrap();assert!(["insufficient","fragmented"].contains(&profile.as_str()));
	let authority=ScaleFeeAuthority::from_env();
	let srv=scale_builder(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(sat(100_000_000)).watchmand(),&ctx,&authority)
		.cfg(move |c|{authority.apply(c);c.vtxo_lifetime=BlockDelta::new(432);c.min_board_amount=sat(330);c.round_interval=Duration::from_secs(3600);}).create().await;
	let db=Db::connect(&srv.config().postgres).await.unwrap();let env=Env{bitcoind:ctx.bitcoind_arc(),datadir:ctx.datadir.clone(),
		cfg:ctx.bark_default_cfg(&srv.ark_url(),Some(ctx.bitcoind())),db,out:out.clone(),ln_client:None,hold_client:None};
	provision_fee_reserve(&ctx,&env,&srv).await;super::fallback_lightning::train_fee_estimator(&ctx).await;
	let opening=treasury_edge(&env,&srv,"opening").await;
	let (mut owner,wallet)=create_owner(env.clone(),0,Profile::Registered,srv.ark_url()).await;
	for _ in 0..128 {let board=env.board(&wallet,100_000).await.unwrap();ctx.await_transaction(board.funding_tx.compute_txid()).await;ctx.generate_blocks(1).await;}
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let coins=wait_spendable(&wallet,128,Duration::from_secs(120)).await.unwrap();assert_eq!(coins.len(),128);
	for c in coins {let v=wallet.get_full_vtxo(c.id()).await.unwrap();owner.coins.push(Coin{id:v.id().to_string(),amount:v.amount().to_sat(),source:"fee_stress_board",user_pubkey:v.user_pubkey().to_string()});owner.anchors.push(v);}
	// Publish one actual signed exit path before absence. This produces genuine
	// progressed physical backing; never insert SQL spend markers or fake a child.
	let progressed=owner.anchors[0].clone();
	for node in progressed.transactions() {
		let sequence=node.tx.input[0].sequence.to_consensus_u32();
		if sequence&(1<<31)==0 {assert!(sequence&(1<<22)==0,"fixture declares block-based relative locks");ctx.generate_blocks(sequence&0xffff).await;}
		let txid=env.core().send_raw_transaction(&node.tx).expect("actual signed path must relay; keep refusal, no fabricated progression");
		ctx.await_transaction(txid).await;ctx.generate_blocks(1).await;
		out.line("actual-progression.jsonl",&json!({"owner":owner.index,"economic_id":progressed.id(),"txid":txid,"terminal_point":OutPoint::new(txid,u32::try_from(node.output_idx).unwrap()),"source":"actual full signed pubkey VTXO path","synthetic":false}));
	}
	assert!(env.core().get_block_count().unwrap()<owner.anchors.iter().map(|v|v.expiry_height().to_u32() as u64).min().unwrap(),"resource setup must preserve absence before first expiry; no threshold amendment");
	drop(wallet);out.line("owners.jsonl",&owner.public());out.event("owner-absent",json!({"owner":owner.index,"before_earliest_expiry":true}));
	let expiry=owner.anchors.iter().map(|v|v.expiry_height().to_u32()).max().unwrap();ctx.generate_blocks(expiry.saturating_sub(env.core().get_block_count().unwrap() as u32)+2).await;
	let mut outputs=serde_json::Map::new();for _ in 0..200 {outputs.insert(env.core().get_new_address(None,None).unwrap().assume_checked().to_string(),json!(0.003));}
	let _:Txid=env.core().call("sendmany",&["".into(),Value::Object(outputs)]).unwrap();ctx.generate_blocks(1).await;
	let unspent:Vec<Value>=env.core().call("listunspent",&[1.into()]).unwrap();let mut training=unspent.into_iter().filter(|c|Amount::from_btc(c["amount"].as_f64().unwrap()).unwrap()==sat(300_000)).collect::<Vec<_>>();assert_eq!(training.len(),200);
	for _ in 0..40 {let txids=(0..4).map(|_|stress_payment(&env,&mut training,30)).collect::<Vec<_>>();stress_mine(&env,&ctx,&txids).await;}
	assert!(stress_estimate(&env,"reserve-required")>=30_000,"actual declared 30 sat/vB estimator prerequisite");
	let gross=owner.coins.iter().map(|c|c.amount).sum::<u64>();let fixed_net=gross.checked_sub(1000).unwrap();
	let mut actual_points=BTreeSet::new();
	for v in &owner.anchors {
		let mut path=vec![v.chain_anchor()];path.extend(v.transactions().map(|n|OutPoint::new(n.tx.compute_txid(),u32::try_from(n.output_idx).unwrap())));
		let points=path.into_iter().filter(|p|env.core().get_tx_out(&p.txid,p.vout,Some(true)).unwrap().is_some()).collect::<Vec<_>>();
		assert_eq!(points.len(),1,"complete actual backing path must have one current point");actual_points.insert(points[0]);
	}
	assert_eq!(actual_points.len(),128);let physical=actual_points.iter().map(|p|env.core().get_tx_out(&p.txid,p.vout,Some(true)).unwrap().unwrap().value.to_sat()).sum::<u64>();
	let minimum_fee=128*41*30;let available=physical.checked_sub(fixed_net).unwrap();assert!(available<minimum_fee,"actual funding geometry must prove reserve shortfall before any credit");
	out.json("required-reserve-proof.json",&json!({"gross_sat":gross,"fixed_net_sat":fixed_net,"actual_backing_sat":physical,"minimum_base_input_fee_sat":minimum_fee,"available_without_fee_inputs_sat":available,"fee_rate_sat_vb":30,"actual_inputs":actual_points,"qualified":false}));
	soak_restart(&srv,true).await;let enabled=utc();let owners=vec![owner];
	if profile=="insufficient" {
		tokio::time::sleep(Duration::from_secs(8)).await;
		assert!(actual_points.iter().all(|p|env.core().get_tx_out(&p.txid,p.vout,Some(true)).unwrap().is_some()),"fee-funding failure must preserve supplied backing/fixed net; no adaptive extra flat charge");
		let address=srv.watchmand().wait_wallet_address().await;let txid=env.core().send_to_address(&address,sat(2_000_000),None,None,None,None,None,None).unwrap();ctx.await_transaction(txid).await;ctx.generate_blocks(7).await;
		out.line("fee-reserve-restorations.jsonl",&json!({"txid":txid,"amount_sat":2_000_000,"source":"included bank","destination":"included fee wallet","external_injection":false,"declared_before_dispatch":true}));
	}
	let deadline=Instant::now()+Duration::from_secs(180);loop {if tokio::time::timeout(Duration::from_secs(2),soak_wait_paid(&env,&owners,deadline)).await.is_ok(){break;}assert!(Instant::now()<deadline);ctx.generate_blocks(1).await;}
	let reconciliation=reconcile(&env,&owners,enabled).await;assert!(reconciliation["failures"].as_array().unwrap().is_empty());
	assert_eq!(reconciliation["recipient_fee_sat"],1000,"reserve/progression never changes frozen grouping/net");assert_eq!(reconciliation["net_sat"],fixed_net);assert!(reconciliation["funding_subsidy_sat"].as_u64().unwrap()>0,"actual supplemental fee funding required");out.json("reconciliation.json",&reconciliation);
	let closing=treasury_edge(&env,&srv,"pre-recovery").await;conserve_treasury(&env,&opening,&closing,0);srv.stop().await.unwrap();
	let recovery=recover(&ctx,&env,&owners,&Params::from_env()).await;assert_eq!(recovery["spends"],1);assert!(recovery["failures"].as_array().unwrap().is_empty());out.json("recovery.json",&recovery);
	srv.watchmand().stop().await.unwrap();ctx.bitcoind().stop().await.unwrap();
	out.json("reserve-result.json",&json!({"result":"UNQUALIFIED","actual_reserve_shortfall_and_signed_progression_executed":true,"typed_fee_unavailability_reason_and_independent_audit_required":true}));
}
