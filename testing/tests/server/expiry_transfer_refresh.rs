//! A frozen recipient must still be paid when the sender refreshes its change.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use bark::FallbackDisposition;
use bark::movement::{Movement, MovementStatus, PaymentMethod};
use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::util::FutureExt;
use bitcoin::{Address, Amount, Network, OutPoint, Transaction, Txid};
use bitcoin::consensus::deserialize;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;

/// Historic accepted-forfeit rows remain present after refresh, but no longer
/// describe the current economic cut through the old root.
#[tokio::test]
async fn expiry_frozen_recipient_is_paid_after_sender_change_refresh() {
	let ctx = TestContext::new("server/expiry_transfer_refresh_recipient").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(btc(1)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(40);
			cfg.vtxo_exit_delta = BlockDelta::new(2);
			// Match the declared public-Signet board fee, rather than the zero-fee test default.
			cfg.fees.board.base_fee = sat(330);
		})
		.watchmand().create().await;
	let absent = ctx.bark("absent", &srv).funded(sat(500_000)).create().await;
	let control = ctx.bark("control", &srv).funded(sat(500_000)).create().await;
	// No intervening block: both backing roots have the same expiry.
	let own_board = absent.board(sat(30_000)).await;
	let sender_board = control.board(sat(30_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	absent.sync().await;
	control.sync().await;
	let owner = absent.client().await;
	let own = owner.get_full_vtxo(own_board.vtxos[0]).await.unwrap();
	let destination = owner.fallback_destination().await.unwrap();
	drop(owner);
	let sender = control.client().await;
	let original = sender.get_full_vtxo(sender_board.vtxos[0]).await.unwrap();
	drop(sender);
	assert_eq!(own.expiry_height(), original.expiry_height());
	assert_eq!(own.amount(), sat(29_670));
	assert_eq!(original.amount(), sat(29_670));
	control.send_oor(absent.address().await, sat(10_000)).await;
	absent.sync().await;
	let owner = absent.client().await;
	let held = owner.spendable_vtxos().await.unwrap();
	assert_eq!(held.len(), 2);
	let received = held.iter().find(|coin| coin.id() != own.id()).unwrap();
	assert_eq!(received.amount(), sat(10_000));
	let received_id = received.id();
	let received = owner.get_full_vtxo(received_id).await.unwrap();
	assert_eq!(received.chain_anchor(), original.chain_anchor());
	assert_ne!(received.chain_anchor(), own.chain_anchor(), "history must cover two distinct backing roots");
	assert_eq!(received.expiry_height(), own.expiry_height());
	let history_before_absence = owner.history().await.unwrap();
	drop(owner);
	// Do not reopen the absent actor until every original payout has at least
	// six independent Core confirmations. Only then test its returning history.
	control.refresh_all_delegated_no_sync().await;
	let mut round_finished = srv.subscribe_log::<server_log::RoundFinished>();
	srv.trigger_round().await;
	round_finished.recv().wait(Duration::from_secs(30)).await.unwrap();
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	let controller = control.client().await;
	controller.sync_pending_rounds().await.unwrap();
	drop(controller);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let forfeit_rows = db.read(async |tx| Ok(tx.query("SELECT amount FROM vtxo \
		WHERE anchor_point=$1 AND spend_state='round-forfeit' ORDER BY vtxo_id", &[&original.chain_anchor().to_string()]).await?)).await.unwrap();
	assert_eq!(forfeit_rows.len(), 2, "the native refresh must really accept both change-input forfeits");
	assert!(forfeit_rows.iter().all(|row| row.get::<_, i64>("amount") == 9_835));
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let height = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(own.expiry_height().to_u32() + 1 - height).await;
	let ids = vec![own.id().to_string(), received_id.to_string()];
	let parents = tokio::time::timeout(Duration::from_secs(30), async {
		// Confirm only real signed prefixes published by the normal watchman;
		// their children enter the frontier after confirmation and its grace.
		let mut pending_prefixes = received.transactions().map(|step| {
			assert!(step.tx.input.iter().all(|input| input.sequence == bitcoin::Sequence::ZERO));
			step.tx.compute_txid()
		}).collect::<BTreeSet<_>>();
		let blocks_per_prefix = srv.watchmand().config().watchman.progress_grace_period.to_u32()
			.checked_add(1).expect("prefix confirmation and grace block count must fit");
		let prefix_block_budget = u32::try_from(pending_prefixes.len()).unwrap()
			.checked_mul(blocks_per_prefix).expect("signed received path block budget must fit");
		let mut mined_prefix_blocks = 0_u32;
		loop {
			let rows = db.read(async |tx| Ok(tx.query("SELECT a.id,a.sweep_tx,e.vtxo_id FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=ANY($1) AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL", &[&ids]).await?)).await.unwrap();
			if rows.len() == 2 {
				assert_ne!(rows[0].get::<_, String>("vtxo_id"), rows[1].get::<_, String>("vtxo_id"));
				break rows.into_iter().map(|row| (row.get::<_, i64>("id"), deserialize::<Transaction>(row.get("sweep_tx")).unwrap()))
					.collect::<BTreeMap<_, _>>();
			}
			assert!(rows.len() <= 2, "one source must not have duplicate active fallback payments");
			let core = ctx.bitcoind().sync_client();
			if let Some(txid) = core.get_raw_mempool().unwrap().into_iter().find(|txid| pending_prefixes.contains(txid)) {
				assert!(pending_prefixes.remove(&txid)); // One batch per observed signed prefix.
				mined_prefix_blocks = mined_prefix_blocks.checked_add(blocks_per_prefix).unwrap();
				assert!(mined_prefix_blocks <= prefix_block_budget);
				let tip = ctx.generate_blocks(blocks_per_prefix).await;
				let hash = core.get_block_hash(u64::from(tip.to_u32())).unwrap();
				loop {
					if db.read(async |tx| {
						let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
						Ok(view.captaind.height == tip && view.watchmand.height == tip
							&& view.captaind.hash == hash && view.watchmand.hash == hash && tx.expiry_view_is_rebuilt(view).await?)
					}).await.unwrap() { break; }
					tokio::time::sleep(Duration::from_millis(50)).await;
				}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both frozen customer coins must receive actual payouts despite the sender's historical forfeits");
	// Both source coins have the same payout script and expiry height.
	// One or two allocations must charge the 1,000 sat floor only once.
	let mut total = Amount::ZERO;
	let mut outputs = Vec::<(Txid, u32)>::new();
	for parent in parents.values() {
		assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, *parent);
		let matching = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
		assert_eq!(matching.len(), 1);
		total += matching[0].1.value;
		outputs.push((parent.compute_txid(), matching[0].0 as u32));
	}
	assert_eq!(total + sat(1_000), own.amount() + received.amount());
	let confirmed = ctx.generate_blocks(6).await;
	let mut expected_outputs = BTreeMap::new();
	for (txid, vout) in &outputs {
		let output = ctx.bitcoind().sync_client().get_tx_out(txid, *vout, Some(true)).unwrap().unwrap();
		assert!(output.confirmations >= 6);
		assert!(expected_outputs.insert(OutPoint::new(*txid, *vout), output.value).is_none());
	}
	// All absence/chain oracles above must finish before the original wallet
	// is opened. A listener tip ACK alone does not mean its ledger caught up.
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	let returned = absent.client().await;
	let history = tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			returned.chain().invalidate_caches().await;
			returned.sync_fallback_payouts().await.expect("authenticated returning-wallet payout history");
			let history = returned.history().await.unwrap();
			let payouts = history.iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout").collect::<Vec<_>>();
			if payouts.len() == expected_outputs.len() && payouts.iter().all(|movement|
				movement.status == MovementStatus::Successful && movement.metadata["fallback"]["stage"] == "confirmed") {
				break history;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("confirmed two-root payout history did not catch up with Core");
	let expected_coins = BTreeMap::from([(own.id(), own.amount()), (received_id, received.amount())]);
	let receipts = returned.fallback_payouts().await.unwrap();
	assert_eq!(receipts.len(), 2);
	assert_eq!(receipts.iter().map(|receipt| (receipt.vtxo_id, receipt.gross)).collect::<BTreeMap<_, _>>(), expected_coins);
	let mut receipt_fees = Amount::ZERO;
	let mut receipt_payouts = Amount::ZERO;
	let mut receipt_inputs = BTreeMap::<OutPoint, BTreeSet<_>>::new();
	for receipt in receipts {
		let active = receipt.attempts.iter().filter(|attempt| attempt.active).collect::<Vec<_>>();
		assert_eq!(active.len(), 1);
		let attempt = active[0];
		assert_eq!(attempt.disposition, FallbackDisposition::CustomerPayout);
		let point = OutPoint::new(attempt.parent.as_ref().unwrap().compute_txid(), attempt.output_index.unwrap());
		assert_eq!(attempt.output_amount, Some(expected_outputs[&point]));
		assert_eq!(attempt.script.as_ref(), Some(&destination.spk));
		assert_eq!(attempt.payout + attempt.fee, receipt.gross);
		receipt_inputs.entry(point).or_default().insert(receipt.vtxo_id);
		receipt_fees += attempt.fee;
		receipt_payouts += attempt.payout;
	}
	assert_eq!(receipt_fees, sat(1_000));
	assert_eq!(receipt_payouts, total);
	let payout_address = PaymentMethod::Bitcoin(Address::from_script(&destination.spk, Network::Regtest).unwrap().into_unchecked());
	// Retain existing debits, including any earlier native operation, but do
	// not let another subsystem also debit either of the two payout inputs.
	let baseline_other_debits = history_before_absence.iter().filter(|movement|
		movement.subsystem.name != "abandon.fallback_payout" && movement.effective_balance.to_sat() < 0
		&& movement.input_vtxos.iter().any(|id| expected_coins.contains_key(id)))
		.map(|movement| (movement.id, movement.effective_balance.to_sat())).collect::<BTreeMap<_, _>>();
	let summarize = |history: Vec<Movement>| {
		let mut other_debits = BTreeMap::new();
		let mut all_subsystem_debit = 0;
		for movement in &history {
			if movement.effective_balance.to_sat() >= 0
				|| !movement.input_vtxos.iter().any(|id| expected_coins.contains_key(id)) { continue; }
			all_subsystem_debit += movement.effective_balance.to_sat();
			if movement.subsystem.name != "abandon.fallback_payout" {
				assert!(other_debits.insert(movement.id, movement.effective_balance.to_sat()).is_none());
			}
		}
		assert_eq!(other_debits, baseline_other_debits, "another subsystem added or changed a debit of a payout input");
		assert_eq!(all_subsystem_debit, baseline_other_debits.values().sum::<i64>() - 39_670,
			"the two original coins must incur only their single payout debit across all subsystems");
		let mut summary = BTreeMap::new();
		let mut inputs = BTreeSet::new();
		let mut debit = 0;
		for movement in history.into_iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout") {
			let metadata = &movement.metadata["fallback"];
			let point = OutPoint::new(serde_json::from_value::<Txid>(metadata["txid"].clone()).unwrap(),
				metadata["output_index"].as_u64().unwrap().try_into().unwrap());
			assert_eq!(movement.status, MovementStatus::Successful);
			assert_eq!(metadata["stage"], "confirmed");
			assert_eq!(metadata["chain_verified"], true);
			assert_eq!(movement.sent_to.len(), 1);
			assert_eq!(movement.sent_to[0].destination, payout_address);
			assert_eq!(movement.sent_to[0].amount, expected_outputs[&point]);
			assert_eq!(movement.input_vtxos.iter().copied().collect::<BTreeSet<_>>(), receipt_inputs[&point]);
			let expected_debit = movement.input_vtxos.iter().map(|id| expected_coins[id].to_sat() as i64).sum::<i64>();
			assert_eq!(movement.offchain_fee, sat(u64::try_from(expected_debit).unwrap()) - expected_outputs[&point]);
			assert!(movement.offchain_fee == Amount::ZERO || movement.offchain_fee == sat(1_000));
			assert_eq!(movement.effective_balance.to_sat(), -expected_debit);
			for input in &movement.input_vtxos { assert!(inputs.insert(*input), "a coin was debited in two payout movements"); }
			debit += movement.effective_balance.to_sat();
			assert!(summary.insert(point, (movement.id, movement.effective_balance, movement.offchain_fee,
				movement.sent_to[0].amount, movement.input_vtxos)).is_none(), "an output appeared in two payout movements");
		}
		assert_eq!(summary.len(), expected_outputs.len());
		assert_eq!(inputs, expected_coins.keys().copied().collect::<BTreeSet<_>>());
		assert_eq!(debit, -39_670, "the original locally held coins must be debited exactly once");
		summary
	};
	let original_history = summarize(history);
	for _ in 0..2 {
		returned.sync_fallback_payouts().await.unwrap();
		assert_eq!(summarize(returned.history().await.unwrap()), original_history);
	}
	drop(returned);
	let reopened = absent.client().await;
	reopened.sync_fallback_payouts().await.unwrap();
	assert_eq!(summarize(reopened.history().await.unwrap()), original_history,
		"reopening the original database must preserve payout movement IDs and the single debit");
}
