use std::io::Read;
use std::os::unix::fs::{DirBuilderExt,MetadataExt,OpenOptionsExt,PermissionsExt};
use bitcoin::hashes::{sha256,Hash};

// TEST-ONLY: one original native epoch before any pool/customer publication.
// Numeric limits are predeclared REGTEST fixture values, never live policy.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleEpochDeclaration {
	epoch_id: String,
	ordinary_sat: u64,
	urgent_sat: u64,
	ordinary_sat_per_kwu: u64,
	urgent_child_sat_per_kwu: u64,
	urgent_package_sat_per_kwu: u64,
	urgent_max_candidate_sat: u64,
	urgent_required_confirmed_sat: u64,
	watchman_funding_sat: u64,
	watchman_fragments: u64,
}

async fn enable_scale_native_epoch(
	ctx: &TestContext, env: &Env, srv: &Captaind,
	desired_pool: Vec<server::vtxopool::VtxoTarget>,
) {
	tokio::time::timeout(Duration::from_secs(300),
		enable_scale_native_epoch_inner(ctx,env,srv,desired_pool)).await
		.expect("bounded native epoch bootstrap failed; preserve original fixture");
}

async fn enable_scale_native_epoch_inner(
	ctx: &TestContext, env: &Env, srv: &Captaind,
	desired_pool: Vec<server::vtxopool::VtxoTarget>,
) {
	let path=PathBuf::from(std::env::var("SCALE_NATIVE_EPOCH_DECLARATION").expect("frozen fixture epoch declaration required"));
	let expected=std::env::var("SCALE_NATIVE_EPOCH_DECLARATION_SHA256").expect("exact declaration SHA required");
	assert!(path.is_absolute());
	let before=std::fs::symlink_metadata(&path).unwrap();
	assert!(before.is_file() && before.nlink()==1 && before.len()<=65536 && before.permissions().mode()&0o777==0o600);
	let file=std::fs::File::open(&path).unwrap();let opened=file.metadata().unwrap();
	assert_eq!((opened.dev(),opened.ino()),(before.dev(),before.ino()));
	let mut raw=Vec::new();file.take(65537).read_to_end(&mut raw).unwrap();assert!(raw.len()<=65536);
	let after=std::fs::symlink_metadata(&path).unwrap();
	assert_eq!((after.dev(),after.ino(),after.len(),after.mtime(),after.mtime_nsec()),
		(opened.dev(),opened.ino(),raw.len() as u64,opened.mtime(),opened.mtime_nsec()));
	assert_eq!(sha256::Hash::hash(&raw).to_string(),expected);
	let d:ScaleEpochDeclaration=serde_json::from_slice(&raw).unwrap();
	assert!(d.epoch_id.len()==64 && d.epoch_id.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
	assert!(d.ordinary_sat>0 && d.urgent_sat>0 && d.urgent_max_candidate_sat>0 && d.urgent_max_candidate_sat<=d.urgent_sat);
	assert_eq!(d.ordinary_sat_per_kwu,1250,"preserve declared5sat/vB benchmark ceiling");
	assert!(d.urgent_child_sat_per_kwu>0 && d.urgent_package_sat_per_kwu>0 && d.urgent_required_confirmed_sat>0);
	assert_eq!(d.watchman_funding_sat,env_u64("STAGE4_FEE_RESERVE_SAT",2_000_000));
	assert_eq!(d.watchman_fragments,env_u64("STAGE4_FEE_RESERVE_FRAGMENTS",1));
	assert!(d.watchman_fragments>0 && d.watchman_fragments<=128);
	assert!(srv.config().vtxopool.vtxo_targets.is_empty(),"bootstrap must precede initial pool issuance");
	assert_eq!(srv.config().fee_estimator.max_fee_rate,Some(FeeRate::from_sat_per_vb_u32(5)));
	assert_eq!(srv.watchmand().config().fee_estimator.max_fee_rate,Some(FeeRate::from_sat_per_vb_u32(5)));
	let core=env.core();let genesis=core.get_block_hash(0).unwrap().to_string();
	assert_eq!(genesis,bitcoin::constants::genesis_block(Network::Regtest).block_hash().to_string());
	let key=tokio::time::timeout(Duration::from_secs(15),srv.ark_info()).await.expect("actual server identity timeout").server_pubkey;
	let endpoint=srv.watchmand().config().admin_address.unwrap();
	let mut rpc=tokio::time::timeout(Duration::from_secs(15),
		server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{endpoint}"))).await.expect("owning wallet connect timeout").unwrap();
	let inventory=tokio::time::timeout(Duration::from_secs(15),rpc.wallet_inventory(protos::Empty{}))
		.await.expect("owning inventory timeout").unwrap().into_inner().watchman.unwrap();
	assert!(inventory.complete && inventory.wallet=="watchman" && !inventory.public_descriptor.is_empty());
	assert!(inventory.reserved_outpoints.is_empty(),"fresh owning fee source must have no reservations");
	let mut points=BTreeSet::new();let mut eligible=0u64;
	for input in inventory.utxos.iter().filter(|input|input.confirmed_fee_eligible) {
		let point=input.outpoint.parse::<OutPoint>().unwrap();assert!(points.insert(point));
		let live=core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().expect("actual eligible input missing");
		assert!(live.confirmations>0);assert_eq!(live.value.to_sat(),input.value_sat);
		assert_eq!(live.script_pub_key.hex.to_lower_hex_string(),input.script_pubkey);
		eligible=eligible.checked_add(input.value_sat).unwrap();
	}
	assert!(eligible>=d.watchman_funding_sat && eligible>=d.urgent_required_confirmed_sat,
		"physical confirmed reserve required; not an assurance of future Claim/RBF liveness");
	let policy=json!({"epoch_id":d.epoch_id,"genesis":genesis,"server_pubkey":key.to_string(),
		"ordinary_sat":d.ordinary_sat,"urgent_sat":d.urgent_sat,"ordinary_sat_per_kwu":d.ordinary_sat_per_kwu,
		"urgent_child_sat_per_kwu":d.urgent_child_sat_per_kwu,"urgent_package_sat_per_kwu":d.urgent_package_sat_per_kwu,
		"urgent_max_candidate_sat":d.urgent_max_candidate_sat,"urgent_required_confirmed_sat":d.urgent_required_confirmed_sat,
		"watchman_descriptor_sha256":sha256::Hash::hash(inventory.public_descriptor.as_bytes()).to_string(),
		"watchman_admin_address":endpoint.to_string()});
	let bytes=serde_json::to_vec(&policy).unwrap();let policy_sha=sha256::Hash::hash(&bytes).to_string();
	let file=ctx.datadir.join("scale-native-fee-policy.json");
	let mut writer=std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&file).unwrap();
	writer.write_all(&bytes).unwrap();writer.sync_all().unwrap();
	let history=ctx.datadir.join("scale-native-fee-history");std::fs::DirBuilder::new().mode(0o700).create(&history).unwrap();
	std::fs::File::open(&ctx.datadir).unwrap().sync_all().unwrap();
	let config=server::fee_allowance::Config {initialize_empty:true,opening:None,
		policy_file:file,policy_sha256:policy_sha.clone(),history_dir:history};
	srv.stop().await.unwrap();srv.watchmand().stop().await.unwrap();
	let counts=env.db.read(async|t|Ok(t.query_one("SELECT (SELECT count(*) FROM fee_allowance_policy),
		(SELECT count(*) FROM fee_allowance_candidate),(SELECT count(*) FROM round),
		(SELECT count(*) FROM vtxo_pool),(SELECT count(*) FROM nursery_tx),
		(SELECT count(*) FROM offboards),(SELECT count(*) FROM recovery_receipt)",&[]).await?)).await.unwrap();
	for i in 0..7 {assert_eq!(counts.get::<_,i64>(i),0,"epoch bootstrap cannot erase previous publication/accounting");}
	{let mut cfg=srv.config_mut();cfg.fee_allowance=Some(config.clone());cfg.vtxopool.vtxo_targets=desired_pool;}
	{let mut cfg=srv.watchmand().config_mut();cfg.fee_allowance=Some(config);
		let path=cfg.data_dir.join(WATCHMAND_CONFIG_FILE);cfg.write_into(&mut std::fs::File::create(path).unwrap()).unwrap();}
	// Watchmand initializes the same original epoch while captaind is paused.
	// Captaind then starts its pool under that same policy/history; no renewal.
	srv.watchmand().start().await.unwrap();srv.start().await.unwrap();
	env.out.json("native-epoch-bootstrap.json",&json!({"declaration_sha256":expected,"policy_sha256":policy_sha,
		"policy":policy,"actual_confirmed_fee_eligible_sat":eligible,"actual_fee_inputs":points,
		"watchman_descriptor_sha256":sha256::Hash::hash(inventory.public_descriptor.as_bytes()).to_string(),
		"scope":"test-only initial native epoch, not paid cost or future liveness proof"}));
	if !srv.config().vtxopool.vtxo_targets.is_empty() {
		tokio::time::timeout(Duration::from_secs(120),srv.wait_for_vtxopool(ctx)).await.expect("first enforced pool issuance");
	}
}
