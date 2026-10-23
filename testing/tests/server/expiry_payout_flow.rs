use std::time::Duration;

use ark_testing::{sat, TestContext};
use bitcoin::{OutPoint, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;

/// No allocation, spend fence or chain acknowledgment is inserted by the test.
/// The running captaind and watchmand must discover, decide, sign and publish
/// the payout, and Bitcoin Core independently checks its real inputs and outputs.
#[tokio::test]
async fn expiry_payout_native_board_seed_restore_and_confirmed_spend() {
	let ctx = TestContext::new("server/expiry_payout_native_board_seed_restore").await;
	let srv = ctx.captaind("server").no_vtxo_pool()
		.cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.expiry_owner_scan_chunk_size = 1;
		})
		.watchmand().create().await;
	let wallet = ctx.bark("abandoned", &srv).funded(sat(200_000)).create().await;
	let issuer = wallet.client().await;
	for _ in 0..250 { issuer.new_address_with_index().await.unwrap(); }
	drop(issuer);
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	assert_eq!(ids.len(), 1);
	let seed = wallet.mnemonic().await.unwrap();
	let client = wallet.client().await;
	let source = client.get_full_vtxo(ids[0]).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	let expected = source.amount() - sat(1_000);
	drop(client);

	// Fund the actual distinct watchman wallet, rather than assuming the
	// captaind rounds wallet can sponsor this zero-fee payout parent.
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let height = ctx.bitcoind().get_block_count().await as u32;
	assert!(height < source.expiry_height().to_u32());
	let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;

	// Do not open or sync the abandoned wallet while waiting. In particular,
	// its expired input must not be refreshed by a test helper before deciding.
	let (allocation, parent, child, recorded_child_fee) = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_opt(
				"SELECT a.id,a.sweep_tx,f.raw_tx,f.fee_sat FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				JOIN expiry_fee_child f ON f.allocation_id=a.id \
				WHERE e.vtxo_id=$1 AND a.broadcast_at IS NOT NULL ORDER BY f.id DESC LIMIT 1",
				&[&source.id().to_string()],
			).await?)).await.unwrap();
			let split_count = db.read(async |t| Ok(t.query_one(
				"SELECT COUNT(*) FROM expiry_split_request", &[],
			).await?.get::<_, i64>(0))).await.unwrap();
			assert_eq!(split_count, 0, "pending owner search must not progress a board into a user-only leaf");
			if let Some(row) = row {
				break (row.get::<_, i64>("id"),
					deserialize::<Transaction>(row.get("sweep_tx")).unwrap(),
					deserialize::<Transaction>(row.get("raw_tx")).unwrap(),
					row.get::<_, i64>("fee_sat"));
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("real background payout did not reach publication; inspect both daemon logs");
	let parent_txid = parent.compute_txid();
	assert_eq!(ctx.bitcoind().await_transaction(parent_txid).await, parent);
	assert_eq!(ctx.bitcoind().await_transaction(child.compute_txid()).await, child);
	assert_eq!(parent.version.0, 3);
	assert!(parent.vsize() <= 10_000);
	let payouts = parent.output.iter().enumerate()
		.filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1, "one user in one backing output pays one fee");
	assert_eq!(payouts[0].1.value, expected);
	let payout_point = OutPoint::new(parent_txid, payouts[0].0 as u32);
	let mut parent_inputs = 0;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		parent_inputs += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	assert_eq!(parent_inputs, parent.output.iter().map(|o| o.value.to_sat()).sum::<u64>());
	let mut child_inputs = 0;
	for input in &child.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		child_inputs += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	let actual_child_fee = child_inputs - child.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
	assert!(actual_child_fee > 0);
	assert_eq!(actual_child_fee, u64::try_from(recorded_child_fee).unwrap());
	ctx.generate_blocks(6).await;
	let confirmed_payout = ctx.bitcoind().sync_client().get_tx_out(&parent_txid, payout_point.vout, Some(true))
		.unwrap().expect("fallback output must be independently unspent in Core");
	assert!(confirmed_payout.confirmations >= 6, "a mempool parent is not a confirmed payout");
	assert_eq!(confirmed_payout.value, expected);

	// A fresh database receives only the original seed and genesis birthday.
	// No private VTXO, payout transaction or address is imported into it.
	let restored = ctx.bark("seed_only", &srv).mnemonic(seed).create().await;
	assert_eq!(restored.client().await.fallback_destination().await.unwrap(), destination);
	let utxos = restored.onchain_utxos().await;
	assert!(utxos.iter().any(|u| u.outpoint == payout_point && u.amount == expected));
	let recovery_address = ctx.bitcoind().get_new_address();
	restored.onchain_drain(&recovery_address).await;
	let mempool = ctx.bitcoind().sync_client().get_raw_mempool().unwrap();
	let mut recovery = None;
	for txid in mempool {
		let tx = ctx.bitcoind().await_transaction(txid).await;
		if tx.input.iter().any(|input| input.previous_output == payout_point) {
			assert!(recovery.replace(tx).is_none(), "two mempool claims of the same payout");
		}
	}
	let recovery = recovery.expect("seed-only wallet did not spend its actual fallback output");
	ctx.generate_blocks(1).await;
	let recovered_output = recovery.output.iter().position(|output|
		output.script_pubkey == recovery_address.script_pubkey()
	).expect("recovery transaction must pay the chosen destination");
	assert!(ctx.bitcoind().sync_client().get_tx_out(&recovery.compute_txid(), recovered_output as u32, Some(true))
		.unwrap().unwrap().confirmations >= 1);
	assert!(ctx.bitcoind().get_received_by_address(&recovery_address) > sat(0));
	assert!(ctx.bitcoind().sync_client().get_tx_out(&parent_txid, payout_point.vout, Some(true)).unwrap().is_none());
	assert_eq!(ctx.bitcoind().await_transaction(recovery.compute_txid()).await, recovery);
	let raw = db.read(async |t| Ok(t.query_one("SELECT sweep_tx FROM expiry_allocation WHERE id=$1", &[&allocation]).await?
		.get::<_, Vec<u8>>(0))).await.unwrap();
	assert_eq!(deserialize::<Transaction>(&raw).unwrap(), parent, "confirmation/recovery must not rewrite the payout parent");
}
