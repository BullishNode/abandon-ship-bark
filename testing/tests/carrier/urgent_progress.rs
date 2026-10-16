//! Current-owner SDK exit: automatic direct checkpoint progression only.
//! No stale wallet, competing owner, raw transaction controller or Lightning.

use super::*;
use bitcoin::Txid;
use bitcoin::hashes::Hash;
use std::time::Duration;

fn chain_receipt(core: &impl RpcApi, baseline: u64, funding: &[(String, Txid, u64)], path: &Path) -> u64 {
	let tip = core.get_block_count().unwrap();
	assert!(tip - baseline <= 512);
	let mut txs = BTreeMap::new();
	for height in 0..=tip {
		for tx in core.get_block(&core.get_block_hash(height).unwrap()).unwrap().txdata {
			if !tx.is_coinbase() { txs.insert(tx.compute_txid(), tx); }
		}
	}
	for id in core.get_raw_mempool().unwrap() { txs.insert(id, core.get_raw_transaction(&id, None).unwrap()); }
	assert!(txs.len() <= 100);
	let mut fees = 0_u64;
	let rows = txs.values().map(|tx| {
		let input = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		let output = tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
		let fee = input.checked_sub(output).unwrap(); fees = fees.checked_add(fee).unwrap();
		serde_json::json!({"txid":tx.compute_txid(),"raw":bitcoin::consensus::encode::serialize_hex(tx),"input_sat":input,"output_sat":output,"miner_sat":fee})
	}).collect::<Vec<_>>();
	let bytes = serde_json::to_vec_pretty(&serde_json::json!({"baseline_height":baseline,"captured_from_height":0,"height":tip,
		"funding":funding,"principal_sat":funding.iter().map(|r|r.2).sum::<u64>(),"transactions":rows,"actual_miner_sat":fees,
		"coinbase_bank":"outside actor treasury; no injection other than named funding outputs",
		"scope":"public actual transactions; does not mark remaining Ark obligations paid"})).unwrap();
	assert!(bytes.len() <= 2 * 1024 * 1024);
	let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).unwrap();
	file.write_all(&bytes).unwrap(); file.sync_all().unwrap(); fees
}

// Preserve public transaction evidence even if a later assertion fails, while
// this fixture's Core is still alive. Never print wallet/seed/private SQL data.
struct KeepChain<'a> {
	core: &'a bitcoin_ext::rpc::BitcoinRpcClient,
	baseline: u64,
	funding: &'a [(String, Txid, u64)],
	path: PathBuf,
}
impl Drop for KeepChain<'_> {
	fn drop(&mut self) {
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
			chain_receipt(self.core, self.baseline, self.funding, &self.path)));
		if result.is_err() {
			let _ = std::fs::write(self.path.with_extension("capture-error.json"),
				b"{\"complete\":false,\"reason\":\"public Core capture failed; no scalar cost credit\"}");
		}
	}
}

#[tokio::test]
async fn direct_current_owner_sdk_exit_automatically_progresses_checkpoint() {
	Box::pin(current_owner_progress()).await;
}

async fn current_owner_progress() {
	let ctx = Box::pin(TestContext::new("carrier/direct_current_owner_checkpoint_progress")).await;
	let core = ctx.bitcoind().sync_client();
	let baseline = core.get_block_count().unwrap();
	let srv = Box::pin(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|c| {
		c.direct_expiry.enabled = true;
		c.vtxo_lifetime = BlockDelta::new(128);
	}).watchmand_cfg(|c| {
		c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(),
			history_dir: c.data_dir.parent().unwrap().join("server"), max_miner_fee: sat(10_000) });
		c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(20).unwrap());
		c.watchman.reaction_interval = Duration::from_secs(1);
		c.watchman.sweep_interval = Duration::from_secs(30);
	}).create()).await;
	let wm = srv.watchmand();
	let sender = Box::pin(ctx.bark("new-sender", &srv).create()).await;
	let recipient = Box::pin(ctx.bark("new-current-owner", &srv).create()).await;
	let mut funding = Vec::new();
	for (role, address, amount) in [
		("rounds", srv.get_rounds_funding_address().await, 200_000),
		("sender", sender.get_onchain_address().await, 100_000),
		("recipient", recipient.get_onchain_address().await, 50_000),
		("watch-ordinary", wm.wait_wallet_address().await, 35_000),
		("watch-urgent", wm.wait_wallet_address().await, 15_000),
	] {
		funding.push((role.to_owned(), ctx.bitcoind().fund_addr(address, sat(amount)).await, amount));
	}
	let receipt = srv.config().data_dir.join("CURRENT-OWNER-PUBLIC-CHAIN-LEDGER.json");
	assert_eq!(funding.iter().map(|r|r.2).sum::<u64>(), 400_000);
	let _keep_chain = KeepChain { core: &core, baseline, funding: &funding, path: receipt.clone() };
	let tip = ctx.generate_blocks(3).await; srv.wait_for_sync_height(tip).await; wm.wait_for_sync_height(tip).await;
	assert!(chain_receipt(&core, baseline, &funding, &receipt) <= 10_000);
	sender.sync().await; recipient.sync().await;
	sender.board_and_confirm_and_register(&ctx, sat(50_000)).await;
	sender.send_oor(recipient.address().await, sat(20_000)).await;
	recipient.sync().await;
	let ids = recipient.vtxo_ids().await; assert_eq!(ids.len(), 1);
	let full = recipient.raw_vtxo(ids[0]).await;
	let path = full.transactions().map(|t|t.tx).collect::<Vec<_>>();
	assert!(path.len() >= 3, "fixture needs board, checkpoint and current-owner leaf");
	let checkpoint = &path[1]; let checkpoint_id = checkpoint.compute_txid();
	let directory = srv.config().data_dir.join("direct-expiry-witness");
	let journal_path = directory.join("ordinary-fee-budget");
	assert!(std::fs::read(&journal_path).unwrap().is_empty());
	// The legitimate CURRENT owner starts its ordinary SDK exit once. No old
	// wallet or independently built exit is used; no claim-height race is driven.
	recipient.start_exit_all().await;
	let _ = recipient.progress_exit().await;
	assert!(core.get_raw_transaction(&path[0].compute_txid(), None).is_ok());
	assert!(core.get_raw_transaction(&checkpoint_id, None).is_err(), "SDK already published checkpoint; cannot credit watchman");
	let tip = ctx.generate_blocks(1 + wm.config().watchman.progress_grace_period.to_u32()).await;
	wm.wait_for_sync_height(tip).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let metadata = tokio::time::timeout(Duration::from_secs(45), async {
		loop {
			let rows = db.read(async |t| t.query("SELECT urgent_fee_attempt, urgent_fee_bound FROM nursery_tx WHERE urgent_fee_attempt IS NOT NULL ORDER BY id LIMIT 2", &[]).await).await.unwrap();
			assert!(rows.len() <= 1);
			if let Some(row) = rows.first() {
				if row.get::<_, bool>("urgent_fee_bound") { break row.get::<_, serde_json::Value>("urgent_fee_attempt"); }
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("automatic direct urgent publication did not occur");
	let txids: Vec<Txid> = serde_json::from_value(metadata["txids"].clone()).unwrap();
	assert_eq!(txids.len(), 2); assert_eq!(txids[0], checkpoint_id);
	assert_eq!(core.get_raw_transaction(&checkpoint_id, None).unwrap(), *checkpoint);
	ctx.await_transactions_across_nodes(txids.clone(), [wm.bitcoind().as_ref()]).await;
	let tip = ctx.generate_blocks(1).await; srv.wait_for_sync_height(tip).await; wm.wait_for_sync_height(tip).await;
	for id in &txids { assert!(core.get_raw_transaction_info(id, None).unwrap().confirmations.unwrap_or(0) > 0); }
	let journal = std::fs::read(&journal_path).unwrap(); let text = std::str::from_utf8(&journal).unwrap();
	assert_eq!(text.lines().filter(|l|l.starts_with("u:")).count(), 1);
	assert_eq!(text.lines().filter(|l|l.starts_with("p:")).count(), 1);
	assert!(!text.lines().any(|l|l.as_bytes().first().is_some_and(u8::is_ascii_digit)));
	let fees = chain_receipt(&core, baseline, &funding, &receipt); assert!(fees <= 10_000);
	let result = serde_json::json!({"scope":"automatic direct checkpoint Progress/CPFP only; Claim/complete exit/retirement NOT qualified",
		"current_owner_vtxo":ids[0],"checkpoint_txid":checkpoint_id,"cpfp_txid":txids[1],"new_principal_sat":400000,"actual_miner_sat":fees,
		"urgent_ceiling_sat":metadata["ceiling_sat"],"original_user_rights_retained":true,"full_source_sha256":bitcoin::hashes::sha256::Hash::hash(&full.serialize()).to_string()});
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(srv.config().data_dir.join("CURRENT-OWNER-PROGRESS-RECEIPT.json")).unwrap();
	file.write_all(&serde_json::to_vec_pretty(&result).unwrap()).unwrap(); file.sync_all().unwrap();
	println!("actual autonomous current-owner checkpoint Progress and CPFP confirmed; no Claim or full exit completion credit");
}
