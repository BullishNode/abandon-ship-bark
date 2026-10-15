//! Real Core fee-estimator inputs and native payout publication. Regtest only.

use std::{fs, io::Write, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::Path, time::Duration};

use ark_testing::{sat, Captaind, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use ark::ProtocolEncoding;
use bitcoin::{Amount, FeeRate, Network, ScriptBuf, Transaction, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin_ext::{BlockDelta, FeeRateExt};
use bitcoin_ext::rpc::RpcApi;
use serde_json::{json, Value};
use server::database::Db;

const TRAINING_BUDGET: u64 = 1_000_000;
const PAYOUT_OR_CHILD_BUDGET: u64 = 20_000;
// Core's bucket upper bound need not equal the requested 50 sat/vB.
// Refuse the setup before starting the native payout worker above 60.
const MAX_OBSERVED_ESTIMATE_SAT_VB: u32 = 60;

fn save_private(path: &Path, value: &Value) {
	let raw = serde_json::to_vec_pretty(value).unwrap();
	assert!(raw.len() <= 4 * 1024 * 1024, "private evidence byte bound");
	let dir = path.parent().unwrap();
	fs::create_dir_all(dir).unwrap();
	fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
	let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();
	file.write_all(&raw).unwrap(); file.sync_all().unwrap();
	fs::File::open(dir).unwrap().sync_all().unwrap();
}

fn actual_fee(ctx: &TestContext, tx: &Transaction) -> Amount {
	let core = ctx.bitcoind().sync_client();
	let input = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None)
		.unwrap().output[i.previous_output.vout as usize].value).sum::<Amount>();
	input.checked_sub(tx.output.iter().map(|o| o.value).sum()).unwrap()
}

/// Same actual-wallet/mined-block training shape as fallback_lightning's
/// helper. Inspect signed PSBT bytes before broadcasting each training tx,
/// so the declared total miner budget is enforced before publication.
async fn train_estimate(ctx: &TestContext, unconfirmed: Option<Txid>, requested_rate: u32, prior_fees: u64) -> Value {
	tokio::time::timeout(Duration::from_secs(120), async {
	let core = ctx.bitcoind().sync_client();
	assert_eq!(core.get_blockchain_info().unwrap().chain, Network::Regtest);
	assert_eq!(core.get_block_hash(0).unwrap(),
		bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash());
	assert!(requested_rate == 3 || requested_rate == 50);
	let mut spent = prior_fees;
	let mut records = Vec::new();
	for _ in 0..12 {
		for _ in 0..8 {
			let address = ctx.bitcoind().get_new_address().to_string();
			let mut outputs = serde_json::Map::new();
			outputs.insert(address, json!(0.001));
			let funded: Value = core.call("walletcreatefundedpsbt", &[
				json!([]), json!([outputs]), json!(0),
				json!({"fee_rate": requested_rate, "lockUnspents": true}), json!(true),
			]).unwrap();
			let signed: Value = core.call("walletprocesspsbt", &[
				funded["psbt"].clone(), json!(true), json!("ALL"),
			]).unwrap();
			let finalised: Value = core.call("finalizepsbt", &[signed["psbt"].clone()]).unwrap();
			assert_eq!(finalised["complete"], true, "training signer must finalise actual bytes");
			let raw = Vec::<u8>::from_hex(finalised["hex"].as_str().unwrap()).unwrap();
			let tx: Transaction = deserialize(&raw).unwrap();
			let fee = actual_fee(ctx, &tx).to_sat();
			spent = spent.checked_add(fee).unwrap();
			// Core funds an estimated size. Preserve the actual signed candidate
			// before a setup assertion can reject it, then enforce explicit bounds.
			let max_rate = if requested_rate == 3 { 5 } else { 60 };
			let signed_ceiling = FeeRate::from_sat_per_vb_u32(max_rate).fee_wu(tx.weight()).unwrap();
			let inputs = tx.input.iter().map(|input| {
				let parent = core.get_raw_transaction(&input.previous_output.txid, None).unwrap();
				json!({"outpoint": input.previous_output,
					"value_sat": parent.output[input.previous_output.vout as usize].value.to_sat(),
					"parent_raw": serialize(&parent).as_hex().to_string()})
			}).collect::<Vec<_>>();
			let candidate = json!({"txid": tx.compute_txid(), "raw": raw.as_hex().to_string(),
				"fee_sat": fee, "weight_wu": tx.weight().to_wu(), "vsize": tx.vsize(),
				"actual_rate_sat_per_kwu": FeeRate::from_amount_and_weight_ceil(sat(fee), tx.weight()).unwrap().to_sat_per_kwu(),
				"requested_rate_sat_vb": requested_rate, "signed_training_ceiling_sat": signed_ceiling.to_sat(),
				"signed_training_max_rate_sat_vb": max_rate, "cumulative_training_fee_sat": spent,
				"core_funded_fee_btc": funded["fee"], "inputs": inputs, "broadcast_at_capture": false});
			save_private(&ctx.datadir.join(format!("payout-fee-source/training-{requested_rate}-candidate-{:03}.private.json", records.len())), &candidate);
			assert!(fee > 0 && spent <= TRAINING_BUDGET,
				"setup: training budget must pass before broadcasting this signed candidate");
			assert!(fee <= signed_ceiling.to_sat(),
				"setup: actual signed training fee exceeds declared phase ceiling; do not broadcast");
			let accepted = core.test_mempool_accept(&[&tx]).unwrap();
			assert!(accepted[0].allowed, "actual signed training transaction must be accepted");
			assert_eq!(core.send_raw_transaction(&tx).unwrap(), tx.compute_txid());
			records.push(candidate);
		}
		ctx.generate_blocks(1).await;
		if let Some(txid) = unconfirmed {
			assert_eq!(core.get_raw_transaction_info(&txid, None).unwrap().confirmations.unwrap_or(0), 0,
				"setup: original payout must stay unconfirmed during estimator training");
		}
	}
	let estimate: Value = core.call("estimatesmartfee", &[json!(6), json!("economical")]).unwrap();
	let rate = FeeRate::from_amount_per_kvb_ceil(Amount::from_btc(estimate["feerate"].as_f64()
		.expect("setup: actual Core must supply an economical estimate")).unwrap());
	let proof = json!({"estimate": estimate, "actual_rate_sat_per_kwu": rate.to_sat_per_kwu(),
		"prior_training_fee_sat": prior_fees, "cumulative_training_miner_fee_sat": spent,
		"training_budget_sat": TRAINING_BUDGET,
		"max_observed_estimate_sat_vb": MAX_OBSERVED_ESTIMATE_SAT_VB, "transactions": records});
	save_private(&ctx.datadir.join(format!("payout-fee-source/training-{requested_rate}.private.json")), &proof);
	assert!(requested_rate != 50 || rate > FeeRate::from_sat_per_vb_u32(5),
		"setup: actual Core estimate must exceed configured ceiling; absence is not monetary RED");
	assert!(requested_rate != 3 || rate <= FeeRate::from_sat_per_vb_u32(5),
		"setup: original payout estimate must fit quiet ceiling before its worker starts");
	assert!(rate <= FeeRate::from_sat_per_vb_u32(MAX_OBSERVED_ESTIMATE_SAT_VB),
		"setup: actual Core estimate exceeds declared rate bound; do not start payout worker");
	proof
	}).await.expect("setup: bounded actual Core estimator training timed out")
}

async fn ordinary_swept_claim(ctx: &TestContext) -> (std::sync::Arc<Captaind>, Db, String, ScriptBuf, u64) {
	assert_eq!(ctx.bitcoind().sync_client().get_blockchain_info().unwrap().chain, Network::Regtest);
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(sat(1_000_000)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
			c.nursery_confirm_target_blocks = BlockDelta::new(2);
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb_u32(5));
		}).watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(200_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(100_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.sync().await;
	let coin = wallet.spendable_vtxos().await.unwrap().into_iter().next().unwrap();
	let coin = wallet.get_full_vtxo(coin.id()).await.unwrap();
	coin.validate(&board.funding_tx).expect("the original signed board must validate against actual funding bytes");
	let id = coin.id().to_string();
	let principal = coin.amount().to_sat();
	let destination = wallet.fallback_destination().await.unwrap().spk;
	save_private(&ctx.datadir.join("payout-fee-source/original-coin.private.json"), &json!({
		"id": id, "actual_signed_principal_sat": principal, "signed_vtxo": coin.serialize_hex(),
		"anchor_raw": serialize(&board.funding_tx).as_hex().to_string(),
		"fallback_script": destination.as_bytes().as_hex().to_string(),
		"signed_path_validated_before_expiry": true,
	}));
	drop(wallet);
	super::fallback_lightning::expire_and_confirm_sweeps(ctx, &db, &[coin]).await;
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement", &[])
		.await?.get::<_, i64>(0))).await.unwrap(), 0);
	assert!(srv.wallet_status().await.rounds.trusted_balance > sat(principal));
	(srv, db, id, destination, principal)
}

async fn start_payouts(srv: &Captaind) {
	srv.stop().await.unwrap();
	{
		let path = srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE);
		let mut c = srv.config_mut();
		c.expiry_payout.enabled = true;
		c.expiry_payout.interval = Duration::from_secs(1);
		c.expiry_payout.grace_blocks = 0;
		c.expiry_payout.sweep_min_confs = 1;
		c.expiry_payout.min_payout_sat = 10_000;
		c.expiry_payout.watchman_config = Some(path);
	}
	srv.start().await.unwrap();
}

async fn native_payout(db: &Db, id: &str) -> (Transaction, u64) {
	tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt(
				"SELECT n.tx, s.fee_sat FROM expiry_settlement s JOIN nursery_tx n ON n.txid=s.txid WHERE s.id=$1",
				&[&id],
			).await?)).await.unwrap();
			if let Some(row) = row { break (deserialize(&row.get::<_, Vec<u8>>(0)).unwrap(), row.get::<_, i64>(1) as u64); }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("setup: genuine native payout must commit; a missing payout is not fee RED")
}

async fn settlement_identity(db: &Db, id: &str) -> Value {
	db.read(async |t| {
		let row = t.query_one("SELECT s.txid,s.fee_sat,s.spk,c.gross_sat,c.source_id,c.source_revision,
			(SELECT fee_headroom_sat FROM recovery_reserve WHERE id='rounds'),
			(SELECT count(*) FROM recovery_capital_release WHERE id=$1)
			FROM expiry_settlement s JOIN recovery_capital_release c ON c.id=s.id WHERE s.id=$1", &[&id]).await?;
		assert_eq!(row.get::<_, i64>(6), 20_000);
		assert_eq!(row.get::<_, i64>(7), 1);
		Ok(json!({"payout_txid": row.get::<_, String>(0), "recipient_fee_sat": row.get::<_, i64>(1),
			"script": row.get::<_, Vec<u8>>(2).as_hex().to_string(), "released_gross_sat": row.get::<_, i64>(3),
			"source_id": row.get::<_, String>(4), "source_revision": row.get::<_, i64>(5),
			"fee_headroom_sat": row.get::<_, i64>(6), "release_count": row.get::<_, i64>(7)}))
	}).await.unwrap()
}

#[tokio::test]
async fn expiry_payout_raw_estimate_retains_signed_fee_ceiling() {
	let ctx = TestContext::new("bark_sdk/expiry_payout_raw_estimate_retains_signed_fee_ceiling").await;
	let (srv, db, id, destination, principal) = ordinary_swept_claim(&ctx).await;
	let training = train_estimate(&ctx, None, 50, 0).await;
	start_payouts(&srv).await;
	let (tx, recipient_fee) = native_payout(&db, &id).await;
	ctx.await_transaction(tx.compute_txid()).await;
	let actual = ctx.bitcoind().sync_client().get_raw_transaction(&tx.compute_txid(), None).unwrap();
	assert_eq!(actual, tx, "native durable bytes must match actual Core publication");
	let fee = actual_fee(&ctx, &tx);
	assert!(fee > Amount::ZERO && fee <= sat(PAYOUT_OR_CHILD_BUDGET), "separate payout exposure bound");
	let paid = tx.output.iter().filter(|o| o.script_pubkey == destination).collect::<Vec<_>>();
	assert_eq!(paid.len(), 1);
	assert_eq!(paid[0].value + sat(recipient_fee), sat(principal));
	let ceiling = FeeRate::from_sat_per_vb_u32(5).fee_wu(tx.weight()).unwrap();
	let identity = settlement_identity(&db, &id).await;
	assert_eq!(identity["released_gross_sat"], principal);
	save_private(&ctx.datadir.join("payout-fee-source/payout.private.json"), &json!({
		"training": training, "id": id, "actual_signed_principal_sat": principal,
		"payout_raw": serialize(&tx).as_hex().to_string(),
		"settlement": identity, "fee_sat": fee.to_sat(), "weight_wu": tx.weight().to_wu(),
		"ceiling_sat": ceiling.to_sat(), "payout_exposure_limit_sat": PAYOUT_OR_CHILD_BUDGET,
	}));
	assert!(fee <= ceiling, "actual native payout signed fee exceeds configured5sat/vB ceiling");
}

#[tokio::test]
async fn expiry_payout_cpfp_retains_signed_child_fee_ceiling_and_original_payment() {
	let ctx = TestContext::new("bark_sdk/expiry_payout_cpfp_retains_signed_child_fee_ceiling_and_original_payment").await;
	let (srv, db, id, destination, principal) = ordinary_swept_claim(&ctx).await;
	let low_training = train_estimate(&ctx, None, 3, 0).await;
	start_payouts(&srv).await;
	let (original, recipient_fee) = native_payout(&db, &id).await;
	let txid = original.compute_txid();
	ctx.await_transaction(txid).await;
	srv.stop().await.unwrap();
	let before = settlement_identity(&db, &id).await;
	assert_eq!(before["released_gross_sat"], principal);
	let original_fee = actual_fee(&ctx, &original);
	assert!(original_fee <= FeeRate::from_sat_per_vb_u32(5).fee_wu(original.weight()).unwrap(),
		"setup: original native payment must be cheap before testing its child");
	let core = ctx.bitcoind().sync_client();
	core.call::<bool>("prioritisetransaction", &[
		json!(txid), json!(0), json!(-i64::try_from(original_fee.to_sat()).unwrap()),
	]).unwrap();
	let training = train_estimate(&ctx, Some(txid), 50,
		low_training["cumulative_training_miner_fee_sat"].as_u64().unwrap()).await;
	assert_eq!(core.get_raw_transaction(&txid, None).unwrap(), original);
	let log = srv.config().data_dir.join("stdout.log");
	let old_len = fs::metadata(&log).unwrap().len() as usize;
	srv.start().await.unwrap();
	let child = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let raws = db.read(async |t| Ok(t.query("SELECT tx FROM nursery_tx WHERE kind::text='internal'", &[]).await?)).await.unwrap();
			let children = raws.iter().map(|r| deserialize::<Transaction>(&r.get::<_, Vec<u8>>(0)).unwrap())
				.filter(|tx| tx.input.iter().any(|i| i.previous_output.txid == txid)).collect::<Vec<_>>();
			if !children.is_empty() { assert_eq!(children.len(), 1); break Some(children[0].clone()); }
			let raw = fs::read(&log).unwrap(); assert!(raw.len() >= old_len);
			let tail = String::from_utf8_lossy(&raw[old_len..]);
			if tail.contains("expiry payout bump not built") && tail.contains("configured maximum") { break None; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("setup: native child or explicit signed-ceiling refusal must occur");
	assert_eq!(settlement_identity(&db, &id).await, before,
		"CPFP/restart must preserve original recipient accounting and once-only gross credit");
	assert_eq!(core.get_raw_transaction(&txid, None).unwrap(), original);
	let (stored, stored_fee) = native_payout(&db, &id).await;
	assert_eq!(stored, original); assert_eq!(stored_fee, recipient_fee);
	assert_eq!(original.output.iter().filter(|o| o.script_pubkey == destination).count(), 1);
	let (fee, ceiling, child_proof) = if let Some(child) = child {
		ctx.await_transaction(child.compute_txid()).await;
		assert_eq!(core.get_raw_transaction(&child.compute_txid(), None).unwrap(), child);
		let fee = actual_fee(&ctx, &child);
		assert!(fee > Amount::ZERO && fee <= sat(PAYOUT_OR_CHILD_BUDGET), "separate child exposure bound");
		let ceiling = FeeRate::from_sat_per_vb_u32(5).fee_wu(child.weight()).unwrap();
		(fee, ceiling, json!({"raw": serialize(&child).as_hex().to_string(), "fee_sat": fee.to_sat(),
			"weight_wu": child.weight().to_wu(), "signed_child_ceiling_sat": ceiling.to_sat(),
			"actual_signed_package_fee_sat": (original_fee + fee).to_sat(),
			"actual_signed_package_weight_wu": (original.weight() + child.weight()).to_wu(),
			"package_fee_policy_is_separate": true}))
	} else { (Amount::ZERO, Amount::ZERO, json!({"explicit_signed_ceiling_refusal": true})) };
	save_private(&ctx.datadir.join("payout-fee-source/child.private.json"), &json!({
		"id": id, "actual_signed_principal_sat": principal,
		"low_training": low_training, "training": training,
		"original_raw": serialize(&original).as_hex().to_string(),
		"original_fee_sat": original_fee.to_sat(), "settlement_before_after": before, "child": child_proof,
		"note": "Core priority adjustment affects mining/chunk view; package fee above is actual signed fees, not modified fees",
	}));
	assert!(fee <= ceiling, "actual native payout child signed fee exceeds configured5sat/vB ceiling");
}
