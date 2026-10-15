//! Positive operator-source residual liveness; not mixed-source closure coverage.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use std::sync::Arc;

use ark::{ProtocolEncoding, Vtxo};
use ark_testing::{sat, TestContext};
use bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, Transaction};
use bitcoin::consensus::{deserialize, encode::serialize_hex};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use futures::TryStreamExt;
use ark_testing::daemon::captaind::Captaind;

fn save_evidence(ctx: &TestContext, evidence: &serde_json::Value) {
	std::fs::write(ctx.datadir.join("unused-pool-capital-evidence.json"),
		serde_json::to_vec_pretty(evidence).unwrap()).unwrap();
}

#[tokio::test]
async fn recovery_reserve_reclaimed_unused_pool_capital_becomes_fundable() {
	tokio::time::timeout(Duration::from_secs(540), unused_pool_capital()).await
		.expect("unused-pool case exceeded its declared wall-clock budget; this is not monetary RED");
}


async fn unused_pool_capital() {
	let _ = unused_pool_capital_for("bark_sdk/recovery_reserve_reclaimed_unused_pool_capital_becomes_fundable").await;
}

async fn unused_pool_capital_for(name: &str) -> (TestContext, Arc<Captaind>) {
	let ctx = TestContext::new(name).await;
	let rate = FeeRate::from_sat_per_vb_u32(1);
	// Disable issuance until exact bank funding and reserve activation are proved.
	// The helper splits this same total into 100k operating + 20k confirmed reserve.
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(sat(120_000)).watchmand().recovery_fee_headroom(sat(20_000))
		.cfg(move |c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.vtxopool.vtxo_lifetime = BlockDelta::new(128);
			c.vtxopool.vtxo_pre_expiry = BlockDelta::new(10);
			c.fee_estimator.fallback_fee_rate_fast = rate;
			c.fee_estimator.fallback_fee_rate_regular = rate;
			c.fee_estimator.fallback_fee_rate_slow = rate;
			c.fee_estimator.max_fee_rate = Some(rate);
		}).watchmand_cfg(move |c| {
			c.fee_estimator.fallback_fee_rate_fast = rate;
			c.fee_estimator.fallback_fee_rate_regular = rate;
			c.fee_estimator.fallback_fee_rate_slow = rate;
			c.fee_estimator.max_fee_rate = Some(rate);
		}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let opening = srv.wallet_status().await.rounds;
	assert_eq!(opening.trusted_balance, sat(120_000), "no hidden operator fixture funds");
	assert_eq!(opening.confirmed_utxos.len(), 2);
	let mut opening_coins = opening.confirmed_utxos.iter().map(|point| {
		let coin = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(coin.confirmations > 0);
		(*point, coin.value)
	}).collect::<Vec<_>>();
	opening_coins.sort_by_key(|(_, value)| *value);
	assert_eq!(opening_coins.iter().map(|(_, value)| *value).collect::<Vec<_>>(), [sat(20_000), sat(100_000)]);
	let reserve_point = opening_coins[0].0;
	let mut evidence = serde_json::json!({
		"scope":"native unused-pool residual only; no mixed-source/full reserve qualification",
		"phase":"funded_before_issuance", "declared_bank_sat":120_000, "fee_headroom_sat":20_000,
		"ordinary_lifetime_blocks":128, "pool_lifetime_blocks":128, "pool_pre_expiry_blocks":10,
		"native_exit_delta_blocks":u32::from(srv.config().vtxo_exit_delta),
		"pool_count":8, "pool_coin_sat":10_000, "funding_and_sweep_fee_ceiling_sat_vb":1,
		"opening_confirmed_coins":opening_coins.iter().map(|(p, v)|
			serde_json::json!({"outpoint":p.to_string(), "value_sat":v.to_sat()})).collect::<Vec<_>>(),
		"native_issue_refusals":[], "original_server_log":"server/stdout.log",
	});
	save_evidence(&ctx, &evidence);
	srv.stop().await.unwrap();
	srv.config_mut().vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(10_000), count: 8 }];
	srv.start().await.unwrap();
	evidence["phase"] = "observing_real_native_pool_issuance".into();
	evidence["configured_native_pool"] = serde_json::to_value(&srv.config().vtxopool).unwrap();
	evidence["min_trusted_confirmations"] = srv.config().min_trusted_confs.into();
	save_evidence(&ctx, &evidence);
	// Drive a finite number of normal native block ticks; never replace an input
	// or repair SQL to force issuance. Preserve initial selection refusals too.
	let mut issuance = None;
	let mut refusals = BTreeSet::new();
	for tick in 0..8 {
		tokio::time::sleep(Duration::from_millis(500)).await;
		let log = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
		refusals.extend(log.lines().filter(|line| line.contains("Error from VTXO pool"))
			.map(str::to_owned));
		evidence["native_issue_refusals"] = serde_json::json!(refusals);
		evidence["native_issue_observation_tick"] = serde_json::json!(tick);
		save_evidence(&ctx, &evidence);
		issuance = srv.vtxopool_last_issuance();
		if issuance.is_some() { break; }
		ctx.generate_blocks(1).await;
	}
	if issuance.is_none() {
		tokio::time::sleep(Duration::from_millis(500)).await;
		let log = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
		refusals.extend(log.lines().filter(|line| line.contains("Error from VTXO pool")).map(str::to_owned));
		evidence["native_issue_refusals"] = serde_json::json!(refusals);
		evidence["native_issue_observation_tick"] = 8.into();
		save_evidence(&ctx, &evidence);
		issuance = srv.vtxopool_last_issuance();
	}
	let issuance = issuance.expect("native pool did not issue within 8 recorded ticks; setup/selection failure is NOT residual RED");
	ctx.await_transaction(issuance).await;
	let confirmed_height = ctx.generate_blocks(1).await;
	srv.wait_for_sync_height(confirmed_height).await;
	let funding = core.get_raw_transaction(&issuance, None).unwrap();
	assert!(core.get_raw_transaction_info(&issuance, None).unwrap().confirmations.unwrap_or(0) > 0);
	assert!(funding.input.iter().all(|input| opening.confirmed_utxos.contains(&input.previous_output)));
	assert!(!funding.input.iter().any(|input| input.previous_output == reserve_point),
		"pool must leave the actual separately confirmed 20k headroom coin intact");
	let funding_inputs = funding.input.iter().map(|input| {
		opening_coins.iter().find(|(point, _)| *point == input.previous_output).unwrap().1
	}).sum::<Amount>();
	let funding_fee = funding_inputs - funding.output.iter().map(|output| output.value).sum::<Amount>();
	let rows = db.read(async |t| Ok(t.query(
		"SELECT p.vtxo_id, p.vtxo, p.amount, p.spent_at, v.spend_state::text AS state,
			v.anchor_point, v.oor_spent_txid, v.spent_in_round, v.offboarded_in
		 FROM vtxo_pool p JOIN vtxo v ON v.vtxo_id=p.vtxo_id ORDER BY p.vtxo_id", &[],
	).await?)).await.unwrap();
	assert_eq!(rows.len(), 8, "positive native issuance membership, not absence of payable claims");
	let mut coins = Vec::new();
	for row in &rows {
		let coin = Vtxo::deserialize(&row.get::<_, Vec<u8>>("vtxo")).unwrap();
		assert_eq!(row.get::<_, String>("vtxo_id"), coin.id().to_string());
		assert_eq!(row.get::<_, i64>("amount"), 10_000);
		assert_eq!(coin.amount(), sat(10_000));
		assert_eq!(row.get::<_, String>("state"), "pool");
		assert!(row.get::<_, Option<chrono::DateTime<chrono::Utc>>>("spent_at").is_none());
		assert!(row.get::<_, Option<String>>("oor_spent_txid").is_none());
		assert!(row.get::<_, Option<i64>>("spent_in_round").is_none());
		assert!(row.get::<_, Option<String>>("offboarded_in").is_none());
		assert!(coin.past_arkoor_pubkeys().is_empty());
		coin.validate(&funding).unwrap();
		assert_eq!(coin.chain_anchor().txid, issuance);
		assert_eq!(row.get::<_, String>("anchor_point"), coin.chain_anchor().to_string());
		coins.push(coin);
	}
	let anchor = coins[0].chain_anchor();
	assert!(coins.iter().all(|coin| coin.chain_anchor() == anchor));
	let gross = funding.output[anchor.vout as usize].value;
	assert_eq!(coins.iter().map(Vtxo::amount).sum::<Amount>(), sat(80_000));
	// Enumerate the actual signed full paths and account for EVERY tree output:
	// a real pool leaf, a covered internal input, or an exact zero-valued anchor.
	let leaves = coins.iter().map(|coin| (coin.point(), coin.txout())).collect::<BTreeMap<_, _>>();
	assert_eq!(leaves.len(), 8);
	let mut tree = BTreeMap::<bitcoin::Txid, Transaction>::new();
	for coin in &coins {
		for item in coin.transactions() {
			let id = item.tx.compute_txid();
			if let Some(previous) = tree.insert(id, item.tx.clone()) { assert_eq!(previous, item.tx); }
		}
	}
	let inputs = tree.values().flat_map(|tx| tx.input.iter().map(|input| input.previous_output)).collect::<BTreeSet<_>>();
	assert!(inputs.contains(&anchor));
	for (id, tx) in &tree {
		for (index, output) in tx.output.iter().enumerate() {
			let point = OutPoint::new(*id, index as u32);
			if let Some(leaf) = leaves.get(&point) { assert_eq!(leaf, output); }
			else if !inputs.contains(&point) {
				assert!(output.value == Amount::ZERO && output.script_pubkey == ScriptBuf::new_p2a(),
					"an unclassified native tree output invalidates operator-only coverage");
			}
		}
	}
	let public_keys = coins.iter().map(|coin| coin.user_pubkey().to_string()).collect::<Vec<_>>();
	let key_count = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM ephemeral_tweak WHERE pubkey=ANY($1)", &[&public_keys],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(key_count, 8, "native generated operator keys, public identities only");
	evidence["phase"] = "native_pool_issued_and_positively_covered".into();
	evidence["pool_funding_txid"] = issuance.to_string().into();
	evidence["raw_pool_funding"] = serialize_hex(&funding).into();
	evidence["pool_funding_miner_fee_sat"] = funding_fee.to_sat().into();
	evidence["source_outpoint"] = anchor.to_string().into();
	evidence["source_gross_sat"] = gross.to_sat().into();
	evidence["native_pool_leaf_ids"] = serde_json::json!(coins.iter().map(|coin| coin.id().to_string()).collect::<Vec<_>>());
	evidence["covered_tree_transaction_ids"] = serde_json::json!(tree.keys().map(ToString::to_string).collect::<Vec<_>>());
	save_evidence(&ctx, &evidence);
	// Stop automatic replacement issuance without deleting any native evidence.
	srv.stop().await.unwrap();
	srv.config_mut().vtxopool.vtxo_targets.clear();
	srv.start().await.unwrap();
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &coins).await;
	let (receipt, held) = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT h.gross_sat, h.held_sat, r.transaction_bytes, r.proof_state
				 FROM recovery_source_hold h JOIN recovery_receipt r ON r.id=h.receipt_id WHERE h.id=$1",
				&[&anchor.to_string()],
			).await?)).await.unwrap();
			if let Some(row) = rows.first() {
				assert_eq!(row.get::<_, String>("proof_state"), "physical_bound");
				assert_eq!(row.get::<_, i64>("gross_sat") as u64, gross.to_sat());
				let receipt: Transaction = deserialize(&row.get::<_, Vec<u8>>("transaction_bytes")).unwrap();
				break (receipt, row.get::<_, i64>("held_sat"));
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("actual confirmed sweep must be booked; missing admission is not residual RED");
	let actual = core.get_raw_transaction(&receipt.compute_txid(), None).unwrap();
	assert_eq!(receipt, actual);
	assert_eq!(receipt.input.len(), 1, "fixture must isolate the positively covered root");
	assert_eq!(receipt.input[0].previous_output, anchor);
	assert!(core.get_raw_transaction_info(&receipt.compute_txid(), None).unwrap().confirmations.unwrap_or(0) > 0);
	assert!(core.get_tx_out(&anchor.txid, anchor.vout, Some(false)).unwrap().is_none());
	let net = receipt.output.iter().map(|output| output.value).sum::<Amount>();
	assert!(net < gross && net > sat(26_000), "actual sweep fee and genuine 25k funding capacity");
	assert!(core.get_tx_out(&reserve_point.txid, reserve_point.vout, Some(true)).unwrap().is_some());
	evidence["phase"] = "confirmed_sweep_booked_before_residual_assertion".into();
	evidence["raw_sweep"] = serialize_hex(&receipt).into();
	evidence["sweep_fee_sat"] = (gross - net).to_sat().into();
	evidence["observed_held_sat_at_admission"] = held.into();
	save_evidence(&ctx, &evidence);
	// Train the real estimator AFTER the expiry advance, then enable native ticks.
	// These Core-bank transactions never fund the operator's declared wallet.
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let tick_tip = core.get_block_count().unwrap();
	let summaries = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let log = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
			let summaries = log.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
				.filter(|event| event["message"] == "expiry payout tick summary" && event["success"] == true
					&& event["tip"].as_u64() == Some(tick_tip)).collect::<Vec<_>>();
			if summaries.len() >= 2 { break summaries; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("real successful native payout ticks required; disabled/failed setup is not residual RED");
	evidence["successful_post_sweep_native_tick_summaries"] = serde_json::json!(summaries);
	let closing = srv.wallet_status().await.rounds;
	let confirmed_cash = closing.confirmed_utxos.iter().map(|point| {
		let coin = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(coin.confirmations > 0);
		coin.value
	}).sum::<Amount>();
	assert_eq!(confirmed_cash, sat(120_000) - funding_fee - (gross - net),
		"all operator closing cash must reconcile to exactly the declared 120k and actual miner fees");
	assert_eq!(closing.trusted_balance, confirmed_cash);
	evidence["closing_confirmed_cash_sat_before_residual_assertion"] = confirmed_cash.to_sat().into();
	let (pool_count, settlement_count, unknown_count) = db.read(async |t| Ok((
		t.query_one("SELECT count(*) FROM vtxo_pool", &[]).await?.get::<_, i64>(0),
		t.query_one("SELECT count(*) FROM expiry_settlement", &[]).await?.get::<_, i64>(0),
		t.query_one("SELECT count(*) FROM recovery_receipt WHERE proof_state='unknown_gross'", &[]).await?.get::<_, i64>(0),
	))).await.unwrap();
	assert_eq!((pool_count, settlement_count, unknown_count), (8, 0, 0),
		"positive pool coverage must not be replaced by another cohort, a customer payout or an unknown proof");
	let held = db.read(async |t| Ok(t.query_one(
		"SELECT held_sat FROM recovery_source_hold WHERE id=$1", &[&anchor.to_string()],
	).await?.get::<_, i64>(0))).await.unwrap();
	evidence["observed_held_sat_after_native_ticks"] = held.into();
	save_evidence(&ctx, &evidence);
	println!("unused-pool capital evidence: {}", ctx.datadir.join("unused-pool-capital-evidence.json").display());
	assert_eq!(held, 0, "RESIDUAL CAPITAL RED: positively covered reclaimed operator pool capital remains held");

	// Reachable only after genuine residual closure: sign and confirm ordinary
	// native spending while keeping real confirmed reserve, not pending change.
	srv.stop().await.unwrap();
	let config = srv.config().clone();
	let seed = server::wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap().to_seed("");
	let master = bitcoin::bip32::Xpriv::new_master(config.network, &seed).unwrap();
	let client = server::bitcoind::build_client(&config.bitcoind.url, config.bitcoind.auth()).unwrap();
	let tip = server::bitcoind::deep_tip(&client).await.unwrap();
	let mut wallet = server::wallet::PersistedWallet::load_derive_from_master_xpriv(
		db, client, config.network, &master, server::wallet::WalletKind::Rounds, tip, config.min_trusted_confs,
	).await.unwrap();
	let destination = ctx.bitcoind().get_new_address();
	let psbt = wallet.build_tx_at_chunk_feerate(
		bitcoin_ext::bdk::WithGuaranteedChange(bdk_wallet::coin_selection::LargestFirstCoinSelection), rate,
		|builder| { builder.add_recipient(destination.script_pubkey(), sat(25_000)); Ok(()) },
	).unwrap();
	assert!(!psbt.unsigned_tx.input.iter().any(|input| input.previous_output == reserve_point));
	let signed = wallet.finish_tx(psbt).unwrap();
	assert!(signed.output.iter().any(|output| output.script_pubkey == destination.script_pubkey()
		&& output.value == sat(25_000)));
	let ordinary_inputs = signed.input.iter().map(|input| {
		let point = input.previous_output;
		core.get_raw_transaction(&point.txid, None).unwrap().output[point.vout as usize].value
	}).sum::<Amount>();
	let ordinary_fee = ordinary_inputs - signed.output.iter().map(|output| output.value).sum::<Amount>();
	assert!(ordinary_fee > Amount::ZERO);
	let payment = core.send_raw_transaction(&signed).unwrap();
	assert_eq!(payment, signed.compute_txid());
	ctx.generate_blocks_unsynced(1).await;
	assert!(core.get_raw_transaction_info(&payment, None).unwrap().confirmations.unwrap_or(0) > 0);
	let remaining = core.get_tx_out(&reserve_point.txid, reserve_point.vout, Some(true)).unwrap().unwrap();
	assert!(remaining.confirmations > 0 && remaining.value == sat(20_000));
	evidence["phase"] = "ordinary25k_spend_confirmed_with20k_confirmed_headroom".into();
	evidence["raw_ordinary_spend"] = serialize_hex(&signed).into();
	evidence["ordinary_spend_miner_fee_sat"] = ordinary_fee.to_sat().into();
	evidence["retained_confirmed_fee_headroom_sat"] = remaining.value.to_sat().into();
	save_evidence(&ctx, &evidence);
	(ctx, srv)
}

#[tokio::test]
async fn recovery_reserve_operator_release_missing_witness_refuses_startup_with_payouts_disabled() {
	tokio::time::timeout(Duration::from_secs(540), async {
		let (ctx, srv) = unused_pool_capital_for("bark_sdk/operator_release_missing_witness_payouts_disabled").await;
		let db = Db::connect(&srv.config().postgres).await.unwrap();
		let rows = db.read(async |t| Ok(t.query(
			"SELECT h.gross_sat, h.held_sat, h.revision, r.kind FROM recovery_source_hold h
			 JOIN recovery_capital_release r ON r.source_id = h.id WHERE r.kind = 'unused-pool'", &[],
		).await?)).await.unwrap();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].get::<_, i64>("gross_sat"), 80_000);
		assert_eq!(rows[0].get::<_, i64>("held_sat"), 0);
		assert_eq!(rows[0].get::<_, i64>("revision"), 1);
		let retired = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM vtxo_pool p JOIN vtxo v ON v.vtxo_id = p.vtxo_id
			 WHERE p.spent_at IS NOT NULL AND v.spend_state = 'spent' AND v.oor_spent_txid IS NULL", &[],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(retired, 8, "positive exact native retirement, not only a zero SQL hold");
		let ids = db.read(async |t| Ok(t.query("SELECT vtxo_id FROM vtxo_pool ORDER BY vtxo_id", &[])
			.await?.into_iter().map(|row| row.get::<_, String>(0).parse().unwrap()).collect::<Vec<_>>())).await.unwrap();
		assert!(db.read(async |t| t.get_pool_vtxos_by_ids(&ids).await).await.is_err(),
			"a stale original selection must fail the authoritative presigning lookup");
		let reloaded = db.load_vtxopool().await.unwrap().try_collect::<Vec<_>>().await.unwrap();
		assert!(reloaded.is_empty(), "restart must not load the retired original pool");
		let directory = srv.config().data_dir.join("operator-recovery-witness");
		assert!(directory.join("history").metadata().unwrap().len() > 0);
		let preserved = ctx.datadir.join("preserved-operator-recovery-witness");
		assert!(!preserved.exists());
		std::fs::rename(&directory, &preserved).unwrap();
		srv.config_mut().expiry_payout.enabled = false;
		assert!(!directory.exists());
		let error = srv.start().await.expect_err("a durable release requires its witness even with payout dispatch off");
		let logs = format!("{}\n{}", std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap(),
			std::fs::read_to_string(ctx.datadir.join("server/stderr.log")).unwrap());
		assert!(logs.contains("operator capital witness validation failed"), "unrelated startup refusal: {error:#}");
		assert!(preserved.join("history").is_file(), "all independent history is preserved, not deleted");
	}).await.expect("native missing-witness restart case exceeded its declared budget");
}
