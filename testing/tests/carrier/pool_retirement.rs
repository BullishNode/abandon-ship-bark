//! Real completed Lightning receive leaves depth-capped operator change.
//! Both native daemons must retire the same backing and pay the co-resident user.
use super::*;
use std::time::Duration;
use ark::vtxo::Full;
use ark::lightning::PaymentHash;

#[tokio::test]
async fn pool_depth_capped_change_recovers_with_completed_customer_claim() {
	Box::pin(pool_change_case()).await;
}

async fn pool_change_case() {
	let ctx = TestContext::new("carrier-builder/pool_depth_capped_change_recovers_with_completed_customer_claim").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let history = ctx.datadir.join("server");
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(sat(500_000)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(256);
			c.direct_expiry.enabled = true; c.direct_expiry.interval = Duration::from_secs(3600); c.direct_expiry.grace_blocks = 0;
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
			c.vtxopool.vtxo_lifetime = BlockDelta::new(256);
			c.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { count: 2, amount: sat(100_000) }];
		}).watchmand_cfg(move |c| {
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(), history_dir: history, max_miner_fee: sat(10_000) });
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
			c.watchman.reaction_interval = Duration::from_secs(3600);
			c.watchman.sweep_interval = Duration::from_secs(3600);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(60), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let pool = db.read(async |t| {
		let row = t.query_one("SELECT vtxo FROM vtxo_pool WHERE spent_at IS NULL ORDER BY vtxo_id LIMIT 1", &[]).await?;
		Ok(ark::Vtxo::<Full>::deserialize(&row.get::<_, Vec<u8>>(0))?)
	}).await.unwrap();
	let cap_depth = pool.exit_depth();
	srv.stop().await.unwrap();
	srv.config_mut().vtxopool.max_vtxo_exit_depth = cap_depth;
	// Keep the real existing lot, but disable restocking to bound the fixture.
	srv.config_mut().vtxopool.vtxo_targets.clear();
	srv.start().await.unwrap();
	lightning.sync().await;
	ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await, sat(50_000)).await;
	ctx.generate_blocks(6).await;
	lightning.sync().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let receiver = ctx.bark_sdk("recipient", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let record = receiver.fallback_destination().await.unwrap();
	let invoice = receiver.bolt11_invoice(sat(20_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let paying = tokio::spawn(async move { lightning.external.pay_bolt11(invoice.to_string()).await });
	let settled = tokio::time::timeout(Duration::from_secs(60), receiver.try_claim_lightning_receive(hash, true)).await.unwrap().unwrap();
	assert!(matches!(settled, bark::actions::lightning::receive::LightningReceiveState::Settled(_)));
	paying.await.unwrap();
	let coins = receiver.spendable_vtxos().await.unwrap();
	assert_eq!(coins.iter().map(|v| v.amount()).sum::<Amount>(), sat(20_000));
	let coin = receiver.get_full_vtxo(coins[0].id()).await.unwrap();
	let change = db.read(async |t| {
		let row = t.query_one("SELECT v.vtxo_id FROM vtxo v LEFT JOIN vtxo_pool p ON p.vtxo_id=v.vtxo_id
			WHERE v.spend_state='pool' AND v.oor_spent_txid IS NULL AND p.vtxo_id IS NULL", &[]).await?;
		let id = ark::VtxoId::from_str(row.get(0))?;
		Ok(t.get_user_vtxo_by_id(id).await?.vtxo)
	}).await.unwrap();
	assert_eq!(change.amount(), sat(80_000));
	assert!(change.exit_depth() > cap_depth, "genuine allocation must cross the original lot's reuse depth");
	let source_funding = ctx.bitcoind().sync_client().get_raw_transaction(&pool.chain_anchor().txid, None).unwrap();
	change.validate(&source_funding).unwrap();
	coin.validate(&source_funding).unwrap();
	assert_eq!(change.chain_anchor(), coin.chain_anchor());
	let unused = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM vtxo_pool WHERE vtxo_id=$1 AND spent_at IS NULL",
		&[&change.id().to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(unused, 0, "retirement evidence must not enroll discarded change for reuse");
	drop(receiver); // No original client calls after the genuine completed claim.
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let cap_cfg = srv.config().clone();
	assert!(cap_cfg.direct_expiry.enabled);
	assert!(srv.watchmand().config().watchman.direct_expiry.is_some());
	let mut cap = cap_cfg.clone();
	cap.expiry_payout.enabled = false;
	cap.direct_expiry.enabled = true; cap.direct_expiry.interval = Duration::from_millis(100);
	cap.direct_expiry.grace_blocks = 0; cap.direct_expiry.max_groups_per_tick = 2;
	let cap_path = ctx.datadir.join("direct-captaind.toml");
	write_automatic_config(&cap_path, &toml::to_string_pretty(&cap).unwrap());
	let mut watch = srv.watchmand().config().clone();
	watch.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(), history_dir: cap_cfg.data_dir.clone(), max_miner_fee: sat(10_000) });
	watch.watchman.reaction_interval = Duration::from_secs(1); watch.watchman.sweep_interval = Duration::from_secs(1);
	watch.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
	let watch_path = ctx.datadir.join("direct-watchmand.toml");
	write_automatic_config(&watch_path, &toml::to_string_pretty(&watch).unwrap());
	let mut cap_daemon = spawn_automatic_daemon("CAPTAIND_EXEC", &cap_path, ctx.datadir.join("direct-captaind.log"));
	let mut watch_daemon = spawn_automatic_daemon("WATCHMAND_EXEC", &watch_path, ctx.datadir.join("direct-watchmand.log"));
	let core = ctx.bitcoind().sync_client();
	let maturity = coin.expiry_height().to_u32().max(change.expiry_height().to_u32());
	ctx.generate_blocks(maturity.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let carrier = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			assert!(cap_daemon.try_wait().unwrap().is_none()); assert!(watch_daemon.try_wait().unwrap().is_none());
			let tx = core.get_raw_mempool().unwrap().into_iter().map(|id| core.get_raw_transaction(&id, None).unwrap())
				.find(|tx| tx.input.iter().any(|i| i.previous_output == pool.chain_anchor()));
			if let Some(tx) = tx { break tx; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("depth-capped operator change must not block the completed customer's automatic payout");
	let outputs = carrier.output.iter().filter(|o| o.script_pubkey == record.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1); assert_eq!(outputs[0].value, sat(19_000));
	ctx.generate_blocks(3).await;
	assert!(core.get_raw_transaction_info(&carrier.compute_txid(), None).unwrap().confirmations.unwrap() >= 1);
	assert!(core.get_tx_out(&pool.chain_anchor().txid, pool.chain_anchor().vout, Some(true)).unwrap().is_none());
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let confirmed = db.read(async |t| Ok(t.query_one("SELECT confirmed_height FROM expiry_carrier WHERE txid=$1", &[&carrier.compute_txid().to_string()]).await?
				.get::<_, Option<i32>>(0))).await.unwrap();
			if confirmed.is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	let change_allocation = db.read(async |t| Ok(t.query_one("SELECT disposition,gross_sat,fee_sat,payout_sat FROM expiry_allocation WHERE id=$1", &[&change.id().to_string()]).await?)).await.unwrap();
	assert_eq!(change_allocation.get::<_, &str>(0), "operator");
	assert_eq!(change_allocation.get::<_, i64>(1), 80_000);
	assert_eq!(change_allocation.get::<_, i64>(2), 0); assert_eq!(change_allocation.get::<_, i64>(3), 0);
	let miner = carrier.input.iter().map(|i| { let p=core.get_raw_transaction(&i.previous_output.txid,None).unwrap(); p.output[i.previous_output.vout as usize].value.to_sat() }).sum::<u64>()
		- carrier.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
	assert!(miner <= 10_000);
	let recorded = db.read(async |t| Ok(t.query_one("SELECT miner_fee_sat,funding_subsidy_sat,jsonb_array_length(fee_inputs)
		FROM expiry_carrier WHERE txid=$1", &[&carrier.compute_txid().to_string()]).await?)).await.unwrap();
	assert_eq!(recorded.get::<_, i64>(0), miner as i64);
	assert_eq!(recorded.get::<_, i64>(1), 0); assert_eq!(recorded.get::<_, i32>(2), 0);
	let surplus = watch.sweep_address.as_ref().unwrap().clone().require_network(Network::Regtest).unwrap().script_pubkey();
	assert!(carrier.output.iter().filter(|o| o.script_pubkey != record.spk).all(|o| o.script_pubkey == surplus),
		"all recovered operator value must go to the configured surplus wallet");
	let surplus_vout = carrier.output.iter().position(|o| o.script_pubkey == surplus).unwrap();
	let expected_point = OutPoint::new(carrier.compute_txid(), surplus_vout as u32).to_string();
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let mut rpc = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}", cap.rpc.admin_address.unwrap())).await.unwrap();
			let inventory = rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
			if inventory.tip_height as u64 >= core.get_block_count().unwrap() && inventory.utxos.iter().any(|u|
				u.outpoint == expected_point && u.confirmations >= 3 && u.confirmed_fee_eligible && !u.reserved &&
				u.value_sat == carrier.output[surplus_vout].value.to_sat() && u.script_pubkey == surplus.as_bytes()) { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual owning rounds wallet must recover confirmed, unreserved operator capital");
	let op = carrier.output.iter().filter(|o| o.script_pubkey != record.spk).map(|o| o.value.to_sat()).sum::<u64>();
	assert_eq!(carrier.input.iter().map(|i| {let p=core.get_raw_transaction(&i.previous_output.txid,None).unwrap();p.output[i.previous_output.vout as usize].value.to_sat()}).sum::<u64>(), 19_000 + op + miner);
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_carrier", &[]).await?.get::<_,i64>(0))).await.unwrap(), 1);
	cap_daemon.kill().await.unwrap();cap_daemon.wait().await.unwrap();watch_daemon.kill().await.unwrap();watch_daemon.wait().await.unwrap();
	let (discovered, spend) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	assert!(discovered.iter().any(|(p,a)| p.txid == carrier.compute_txid() && *a == sat(19_000)));
	assert!(spend.input.iter().any(|i| i.previous_output.txid == carrier.compute_txid()));
}
