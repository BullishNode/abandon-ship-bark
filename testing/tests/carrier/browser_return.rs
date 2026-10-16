//! Opt-in browser/native qualification, paired with testing/browser/direct-return.mjs.
//! Control files advance only the fixture lifecycle; production daemons discover,
//! freeze, sign, publish and confirm the payout without manual worker requests.

use std::time::Duration;

use bitcoin::hashes::{sha256, Hash};

use ark_testing::daemon::barkd::{Barkd, BarkdChainSource};
use serde_json::{json, Value};

use super::*;

fn receipt(dir: &Path, name: &str, value: Value) {
	write_automatic_config(&dir.join(format!("{name}.json")), &serde_json::to_string_pretty(&value).unwrap());
}

async fn command(dir: &Path, name: &str) {
	tokio::time::timeout(Duration::from_secs(180), async {
		while !dir.join(format!("{name}.command")).exists() {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap_or_else(|_| panic!("paired browser did not reach {name}"));
}

async fn rest(wallet: &Barkd, path: &str) -> Value {
	wallet.client_config().client.get(format!("{}/api/v1/{path}", wallet.base_url()))
		.send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
}

#[tokio::test]
#[ignore = "requires declared private fixture and paired Playwright browser driver"]
async fn browser_native_board_absence_direct_return_and_seed_recovery() {
	let dir = PathBuf::from(std::env::var_os("C13_BROWSER_CONTROL").expect("private control directory required"));
	assert!(dir.is_dir());
	let ctx = TestContext::new("browser/browser_native_board_absence_direct_return_and_seed_recovery").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(sat(200_000)).cfg(|c| {
			c.round_interval = Duration::from_secs(3600);
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fees.board.base_fee = Amount::ZERO;
			c.expiry_payout.enabled = false;
			c.direct_expiry.enabled = true;
			c.direct_expiry_fee_policy = server::watchman::ordinary_fee::OrdinaryFeePolicy::default();
			c.direct_expiry.grace_blocks = 0;
			c.direct_expiry.interval = Duration::from_millis(100);
		}).watchmand_cfg(|c| {
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig {
				ordinary_fee_policy: Default::default(),
				history_dir: c.data_dir.parent().unwrap().join("server"), max_miner_fee: sat(10_000),
			});
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
			c.watchman.sweep_interval = Duration::from_secs(1);
			c.watchman.reaction_interval = Duration::from_secs(1);
		}).create().await;
	// Read the real fresh initializer; the fixture never creates budget metadata.
	let witness = srv.config().data_dir.join("direct-expiry-witness");
	let policy_bytes = std::fs::read(witness.join("ordinary-fee-policy.json")).unwrap();
	let opening_bytes = std::fs::read(witness.join("ordinary-fee-opening.json")).unwrap();
	let policy: Value = serde_json::from_slice(&policy_bytes).unwrap();
	let opening: Value = serde_json::from_slice(&opening_bytes).unwrap();
	assert_eq!(policy["policy"], serde_json::to_value(srv.config().direct_expiry_fee_policy).unwrap());
	assert_eq!(srv.watchmand().config().watchman.direct_expiry.as_ref().unwrap().ordinary_fee_policy,
		srv.config().direct_expiry_fee_policy);
	assert_eq!(policy["policy"]["ordinary_budget_sat"], 60_000);
	assert_eq!(policy["policy"]["urgent_reserve_sat"], 15_000);
	assert_eq!(policy["policy"]["external_future_reserved_sat"], 20_000);
	assert_eq!(policy["opening_sha256"], sha256::Hash::hash(&opening_bytes).to_string());
	assert_eq!(opening["network"], "regtest");
	assert_eq!(opening["history_prefix_bytes"], 0);
	assert_eq!(opening["native_spent_sat"], 0);
	assert_eq!(opening["external_spent_sat"], 0);
	assert!(std::fs::read(witness.join("ordinary-fee-budget")).unwrap().is_empty());
	let core = ctx.bitcoind().sync_client();
	let fee_address = srv.watchmand().wait_wallet_address().await;
	let fee_funding = ctx.bitcoind().fund_addr(fee_address.clone(), sat(35_000)).await;
	let urgent_fee_funding = ctx.bitcoind().fund_addr(fee_address.clone(), sat(15_000)).await;
	let fee_trust = srv.watchmand().config().min_trusted_confs;
	assert!(fee_trust > 0);
	let funded_height = ctx.generate_blocks(fee_trust).await;
	srv.watchmand().wait_for_sync_height(funded_height).await;
	let rounds_opening = srv.wallet_status().await.total();
	assert_eq!(rounds_opening, sat(200_000));
	let fee_tx = core.get_raw_transaction(&fee_funding, None).unwrap();
	let fee_vout = fee_tx.output.iter().position(|o| o.script_pubkey == fee_address.script_pubkey()).unwrap() as u32;
	let fee_opening = core.get_tx_out(&fee_funding, fee_vout, Some(true)).unwrap().unwrap();
	assert_eq!(fee_opening.value, sat(35_000));
	assert!(fee_opening.confirmations >= fee_trust);
	let urgent_tx = core.get_raw_transaction(&urgent_fee_funding, None).unwrap();
	let urgent_vout = urgent_tx.output.iter().position(|o|
		o.script_pubkey == fee_address.script_pubkey() && o.value == sat(15_000)).unwrap() as u32;
	let urgent_point = OutPoint::new(urgent_fee_funding, urgent_vout);
	let urgent_opening = core.get_tx_out(&urgent_point.txid, urgent_point.vout, Some(true)).unwrap().unwrap();
	assert_eq!(urgent_opening.value, sat(15_000));
	assert!(urgent_opening.confirmations >= fee_trust);
	let datadir = ctx.datadir.join("browser-wallet");
	std::fs::create_dir_all(&datadir).unwrap();
	let wallet = Barkd::new("browser-wallet", datadir.clone(), srv.ark_url(),
		BarkdChainSource::Bitcoind { url: ctx.bitcoind().rpc_url(), cookie: ctx.bitcoind().rpc_cookie() }, None);
	wallet.add_arg("--no-auth");
	wallet.start().await.unwrap();
	receipt(&dir, "ready", json!({"backend": wallet.base_url(), "ark_server": srv.ark_url(),
		"bitcoind_url": ctx.bitcoind().rpc_url(), "bitcoind_cookie": ctx.bitcoind().rpc_cookie(),
		"fee_funding": fee_funding, "fee_reserve_sat": 50_000, "rounds_injection_sat": 200_000,
		"rounds_opening_sat": rounds_opening.to_sat(), "fee_opening": fee_opening, "fee_opening_sat": 35_000,
		"urgent_fee_funding": urgent_fee_funding, "urgent_reserve": urgent_point,
		"urgent_fee_opening": urgent_opening, "urgent_reserve_sat": 15_000, "fee_trust_min_confs": fee_trust,
		"fresh_policy": policy, "fresh_opening": opening,
		"policy_sha256": sha256::Hash::hash(&policy_bytes).to_string(),
		"wallet_injection_sat": 100_000, "network": "regtest", "data": ctx.datadir}));
	command(&dir, "created").await;
	let mnemonic = bip39::Mnemonic::parse(std::fs::read_to_string(datadir.join("mnemonic")).unwrap()).unwrap();
	let fallback = rest(&wallet, "wallet/fallback-destination").await;
	let payout_address = Address::from_str(fallback["address"].as_str().unwrap()).unwrap()
		.require_network(Network::Regtest).unwrap();
	assert!(payout_address.to_string().starts_with("bcrt1q"));
	let funding = ctx.fund_barkd(&wallet, sat(100_000)).await;
	receipt(&dir, "funded", json!({"funding_txid": funding, "amount_sat": 100_000, "fallback": fallback}));
	command(&dir, "board").await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.wait_for_boards_synced().await;
	let coins = wallet.vtxos(None).await;
	assert_eq!(coins.len(), 1);
	assert_eq!(coins[0].amount, sat(20_000));
	let coin: ark::Vtxo = ark::Vtxo::deserialize_hex(&wallet.get_vtxo_encoded(&coins[0].id.to_string()).await.encoded.0).unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let key = coin.user_pubkey().serialize().to_vec();
	let linked: bool = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM key_link k
		JOIN fallback_record f ON f.mailbox_pk=k.mailbox_pk WHERE k.user_pubkey=$1)", &[&key]).await?.get(0))).await.unwrap();
	assert!(linked, "browser Board must be registered before wallet absence");
	let before = wallet.onchain_balance().await;
	let board_tx = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let board_inputs: u64 = board_tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
		.output[i.previous_output.vout as usize].value.to_sat()).sum();
	let board_fee = board_inputs - board_tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
	receipt(&dir, "boarded", json!({"coin": coins[0], "onchain_before_sat": before.to_sat(),
		"board_miner_fee_sat": board_fee, "board_txid": board_tx.compute_txid(),
		"ark_balance": wallet.bark_balance().await, "onchain": rest(&wallet, "onchain/balance").await}));
	command(&dir, "absent").await;
	wallet.stop().await.unwrap();
	let height = core.get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	let backing = coin.chain_anchor();
	let carrier = tokio::time::timeout(Duration::from_secs(45), async {
		loop {
			if let Some(tx) = core.get_raw_mempool().unwrap().into_iter()
				.map(|id| core.get_raw_transaction(&id, None).unwrap())
				.find(|tx| tx.input.iter().any(|i| i.previous_output == backing)) { break tx; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("automatic captaind/watchmand must pay the absent browser wallet");
	let expected = TxOut { value: sat(19_000), script_pubkey: payout_address.script_pubkey() };
	assert_eq!(carrier.output.iter().filter(|o| o.script_pubkey == expected.script_pubkey).collect::<Vec<_>>(), vec![&expected]);
	let payout = OutPoint::new(carrier.compute_txid(), carrier.output.iter().position(|o| o == &expected).unwrap() as u32);
	ctx.generate_blocks(1).await;
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let done: bool = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM expiry_carrier
				WHERE txid=$1 AND confirmed_height IS NOT NULL)", &[&carrier.compute_txid().to_string()]).await?.get(0))).await.unwrap();
			if done { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
	assert!(core.get_tx_out(&backing.txid, backing.vout, Some(true)).unwrap().is_none());
	let count: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_carrier", &[]).await?.get(0))).await.unwrap();
	assert_eq!(count, 1);
	let urgent_after = core.get_tx_out(&urgent_point.txid, urgent_point.vout, Some(true)).unwrap()
		.expect("ordinary Board payout must leave its separate confirmed urgent reserve unspent");
	assert_eq!(urgent_after.value, sat(15_000));
	assert!(urgent_after.confirmations >= fee_trust);
	let input_sat: u64 = carrier.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
		.output[i.previous_output.vout as usize].value.to_sat()).sum();
	let output_sat: u64 = carrier.output.iter().map(|o| o.value.to_sat()).sum();
	receipt(&dir, "payout", json!({"txid": carrier.compute_txid(), "payout": payout,
		"net_sat": 19_000, "gross_sat": 20_000, "fee_sat": 1_000, "carrier_inputs_sat": input_sat,
		"carrier_outputs_sat": output_sat, "carrier_miner_fee_sat": input_sat-output_sat,
		"onchain_before_sat": before.to_sat(), "expected_after_sat": before.to_sat()+19_000,
		"urgent_reserve": urgent_point, "urgent_reserve_sat": 15_000, "urgent_after": urgent_after,
		"client_stopped_during_payout": true, "confirmed": core.get_raw_transaction_info(&carrier.compute_txid(), None).unwrap(),
		"transaction": bitcoin::consensus::encode::serialize_hex(&carrier)}));
	command(&dir, "return").await;
	wallet.start().await.unwrap();
	receipt(&dir, "returned", json!({"backend": wallet.base_url()}));
	command(&dir, "recover").await;
	wallet.stop().await.unwrap();
	let (discovered, spend) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	assert!(discovered.contains(&(payout, sat(19_000))));
	assert!(spend.input.iter().any(|i| i.previous_output == payout));
	receipt(&dir, "recovered", json!({"discovered": discovered, "spend_txid": spend.compute_txid(),
		"transaction": bitcoin::consensus::encode::serialize_hex(&spend), "scope": "mnemonic-only standard BIP84 raw-block discovery and confirmed spend"}));
}
