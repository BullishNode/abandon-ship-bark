//! Native admission quarantine tests. These do not qualify the later release/classifier.

use std::{str::FromStr, time::Duration};

use bitcoin::{Address, Amount, FeeRate, Network, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;

use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use server::database::Db;
use serde_json::json;
use server_log::RoundFinished;
use server_rpc::protos;
use ark_testing::util::FutureExt;

#[tokio::test]
async fn recovery_reserve_books_gross_backing_and_fences_funded_drain_after_restart() {
	let ctx = TestContext::new("bark_sdk/recovery_reserve_books_gross_backing_and_fences_funded_drain_after_restart").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	// Enable admission before any backing is reclaimed. Existing histories must
	// not acquire a fabricated zero reserve during this startup.
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(7_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.sync().await;
	let coin = wallet.spendable_vtxos().await.unwrap().into_iter().next().unwrap();
	let coin = wallet.get_full_vtxo(coin.id()).await.unwrap();
	drop(wallet);
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &[coin]).await;

	let core = ctx.bitcoind().sync_client();
	let count = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, 0, "the real below-minimum customer claim is still retained");
	let before = srv.wallet_status().await.rounds;
	assert!(before.trusted_balance > sat(100_000), "the refusal must not be an unfunded setup");
	let original_inputs = before.confirmed_utxos;
	srv.stop().await.unwrap();
	// Turning the payout task off cannot turn durable protection off.
	srv.config_mut().expiry_payout.enabled = false;
	let destination = ctx.bitcoind().get_new_address().to_string();
	let output = srv.get_custom_command(&["drain", &destination]).await.unwrap().output().await.unwrap();
	assert!(!output.status.success(), "a funded native drain must not spend quarantined cash");
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(stderr.contains("recovery capital quarantined"), "{stderr}");
	for point in original_inputs {
		assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_some(),
			"refusal must occur before any input is spent");
	}
	srv.start().await.unwrap();
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement", &[]).await?.get::<_, i64>(0))).await.unwrap(), 0);

	let receipts = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT id, transaction_bytes, proof_state FROM recovery_receipt", &[],
			).await?)).await.unwrap();
			if !rows.is_empty() { break rows; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("actual confirmed backing sweep must be admitted with its hold");
	for row in receipts {
		assert_eq!(row.get::<_, String>("proof_state"), "physical_bound");
		let tx: Transaction = deserialize(&row.get::<_, Vec<u8>>("transaction_bytes")).unwrap();
		let txid = tx.compute_txid().to_string();
		assert_eq!(row.get::<_, String>("id"), txid);
		let actual = core.get_raw_transaction(&tx.compute_txid(), None).unwrap();
		assert_eq!(actual, tx, "the receipt preserves the actual transaction bytes");
		let sources = db.read(async |t| Ok(t.query(
			"SELECT id, gross_sat, held_sat FROM recovery_source_hold WHERE receipt_id = $1", &[&txid],
		).await?)).await.unwrap();
		assert_eq!(sources.len(), tx.input.len());
		let mut gross = Amount::ZERO;
		for input in &tx.input {
			let point = input.previous_output;
			let parent = core.get_raw_transaction(&point.txid, None).unwrap();
			let value = parent.output[point.vout as usize].value;
			let source = sources.iter().find(|row| row.get::<_, String>("id") == point.to_string()).unwrap();
			assert_eq!(source.get::<_, i64>("gross_sat") as u64, value.to_sat());
			assert_eq!(source.get::<_, i64>("held_sat") as u64, value.to_sat());
			gross += value;
		}
		let net: Amount = tx.output.iter().map(|output| output.value).sum();
		assert!(gross > net, "the operator paid an actual mining fee; net is not the gross hold");
	}
}

#[tokio::test]
async fn recovery_reserve_refuses_unbackfilled_sweep_history() {
	let ctx = TestContext::new("bark_sdk/recovery_reserve_refuses_unbackfilled_sweep_history").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(7_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.sync().await;
	let coin = wallet.spendable_vtxos().await.unwrap().into_iter().next().unwrap();
	let coin = wallet.get_full_vtxo(coin.id()).await.unwrap();
	drop(wallet);
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &[coin]).await;
	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.recovery_fee_headroom_sat = Some(20_000);
		config.expiry_payout.watchman_config = Some(srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE));
	}
	assert!(srv.start().await.is_err(), "old sweep cash must not silently initialize a zero reserve");
	let count = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM recovery_reserve", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, 0, "a failed backfill precondition must not install a fabricated checkpoint");
}

/// A real incoming chain transaction crosses the normal wallet listener. Force
/// the hold insert to fail after wallet metadata was written in the SQL body:
/// neither may commit; removing the fault lets the same immutable receipt retry.
#[tokio::test]
async fn recovery_reserve_import_and_hold_rollback_together_then_retry() {
	let ctx = TestContext::new("bark_sdk/recovery_reserve_import_and_hold_rollback_together_then_retry").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	db.write(async |t| {
		t.batch_execute("
			CREATE FUNCTION interrupt_recovery_booking() RETURNS TRIGGER AS $$
			BEGIN RAISE EXCEPTION 'test recovery booking interrupted'; END; $$ LANGUAGE plpgsql;
			CREATE TRIGGER interrupt_recovery_booking BEFORE INSERT ON recovery_source_hold
			FOR EACH ROW EXECUTE FUNCTION interrupt_recovery_booking();
		").await?;
		Ok(())
	}).await.unwrap();
	let address = srv.watchmand().config().sweep_address.clone().unwrap()
		.require_network(bitcoin::Network::Regtest).unwrap();
	// This operator injection deliberately targets the recovery address. Its
	// unresolved physical bound is NOT reported as customer economic debt.
	let receipt = ctx.bitcoind().fund_addr(address, sat(20_000)).await;
	let receipt_txid = receipt.to_string();
	ctx.generate_blocks_unsynced(1).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let log = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap_or_default();
			if log.contains("test recovery booking interrupted") { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("the actual listener must attempt and fail native atomic booking");
	let (changes, receipts, holds) = db.read(async |t| Ok((
		t.read_aggregate_changeset(server::wallet::WalletKind::Rounds).await?.unwrap(),
		t.query_one("SELECT count(*) FROM recovery_receipt WHERE id = $1", &[&receipt_txid]).await?.get::<_, i64>(0),
		t.query_one("SELECT count(*) FROM recovery_source_hold WHERE receipt_id = $1", &[&receipt_txid]).await?.get::<_, i64>(0),
	))).await.unwrap();
	assert!(!changes.tx_graph.txs.iter().any(|tx| tx.compute_txid() == receipt),
		"wallet import must roll back when physical hold insertion fails");
	assert_eq!((receipts, holds), (0, 0), "no partial receipt or hold may commit");
	db.write(async |t| {
		t.batch_execute("DROP TRIGGER interrupt_recovery_booking ON recovery_source_hold; DROP FUNCTION interrupt_recovery_booking();").await?;
		Ok(())
	}).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let (changes, receipts, holds) = db.read(async |t| Ok((
				t.read_aggregate_changeset(server::wallet::WalletKind::Rounds).await?.unwrap(),
				t.query_one("SELECT count(*) FROM recovery_receipt WHERE id = $1", &[&receipt_txid]).await?.get::<_, i64>(0),
				t.query_one("SELECT count(*) FROM recovery_source_hold WHERE receipt_id = $1", &[&receipt_txid]).await?.get::<_, i64>(0),
			))).await.unwrap();
			if receipts == 1 && holds > 0 {
				assert!(changes.tx_graph.txs.iter().any(|tx| tx.compute_txid() == receipt));
				break;
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("retry must atomically import the original receipt and its holds");
}

/// The first LargestFirst choice consumes the sole confirmed fee reserve, but
/// another real funded subset is affordable. Never call that a funding failure.
#[tokio::test]
async fn recovery_reserve_small_input_pays_while_large_confirmed_coin_preserves_headroom() {
	let ctx = TestContext::new("bark_sdk/recovery_reserve_small_input_pays_while_large_confirmed_coin_preserves_headroom").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).watchmand().create().await;
	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.recovery_fee_headroom_sat = Some(10_000);
		config.expiry_payout.watchman_config = Some(srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE));
	}
	srv.start().await.unwrap();
	let address = srv.get_rounds_funding_address().await;
	let small = ctx.bitcoind().fund_addr(address, sat(9_000)).await;
	ctx.generate_blocks(1).await;
	let before = srv.wallet_status().await.rounds;
	assert!(before.confirmed_utxos.iter().any(|point| point.txid == small));
	srv.stop().await.unwrap();
	let config = srv.config().clone();
	let seed = server::wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap().to_seed("");
	let master = bitcoin::bip32::Xpriv::new_master(config.network, &seed).unwrap();
	let client = server::bitcoind::build_client(&config.bitcoind.url, config.bitcoind.auth()).unwrap();
	let tip = server::bitcoind::deep_tip(&client).await.unwrap();
	let db = Db::connect(&config.postgres).await.unwrap();
	let mut wallet = server::wallet::PersistedWallet::load_derive_from_master_xpriv(
		db, client, config.network, &master, server::wallet::WalletKind::Rounds, tip, config.min_trusted_confs,
	).await.unwrap();
	let destination = ctx.bitcoind().get_new_address();
	let psbt = wallet.build_tx_at_chunk_feerate(
		bitcoin_ext::bdk::WithGuaranteedChange(bdk_wallet::coin_selection::LargestFirstCoinSelection),
		bitcoin::FeeRate::from_sat_per_vb_u32(1),
		|builder| {
			builder.ordering(bdk_wallet::TxOrdering::Untouched);
			builder.add_recipient(destination.script_pubkey(), sat(7_000)); Ok(())
		},
	).unwrap();
	assert!(psbt.unsigned_tx.input.iter().all(|input| input.previous_output.txid == small),
		"native retry must use the genuine small coin and retain the large confirmed reserve");
	assert_eq!(psbt.unsigned_tx.output[0].script_pubkey, destination.script_pubkey(), "funding root output order is preserved");
	let selected_guard = wallet.lock_wallet_utxos(psbt.unsigned_tx.input.iter().map(|input| input.previous_output)).unwrap();
	let other_guard = wallet.lock_wallet_utxos(before.confirmed_utxos.iter().copied().filter(|point| point.txid != small)).unwrap();
	assert!(wallet.finish_tx(psbt.clone()).is_err(), "other input guards cannot also be fee reserve");
	drop(other_guard);
	let signed = wallet.finish_tx(psbt).unwrap();
	drop(selected_guard);
	let core = ctx.bitcoind().sync_client();
	let txid = core.send_raw_transaction(&signed).unwrap();
	assert_eq!(txid, signed.compute_txid());
	ctx.generate_blocks_unsynced(1).await;
	assert!(core.get_raw_transaction_info(&txid, None).unwrap().confirmations.unwrap_or(0) > 0);
	let retained = before.confirmed_utxos.iter().filter(|point| point.txid != small).map(|point| {
		core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap().value
	}).sum::<Amount>();
	assert!(retained >= sat(10_000), "fee headroom must remain actually confirmed, not future change");
}

#[tokio::test]
async fn recovery_reserve_native_selected_payout_discharges_gross_once_and_witnesses_release() {
	selected_native_payout(false).await;
}

#[tokio::test]
async fn recovery_reserve_native_release_failure_rolls_back_settlement_and_hold_then_retries() {
	selected_native_payout(true).await;
}

async fn selected_native_payout(interrupt: bool) {
	let name = if interrupt { "bark_sdk/recovery_reserve_native_release_rollback" }
		else { "bark_sdk/recovery_reserve_native_selected_release" };
	let ctx = TestContext::new(name).await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	if interrupt {
		db.write(async |t| {
			t.batch_execute("CREATE FUNCTION interrupt_selected_release() RETURNS TRIGGER AS $$
				BEGIN RAISE EXCEPTION 'test selected release interrupted'; END; $$ LANGUAGE plpgsql;
				CREATE TRIGGER interrupt_selected_release BEFORE INSERT ON recovery_capital_release
				FOR EACH ROW EXECUTE FUNCTION interrupt_selected_release();").await?;
			Ok(())
		}).await.unwrap();
	}
	let wallet = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(100_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.sync().await;
	let coin = wallet.spendable_vtxos().await.unwrap().into_iter().next().unwrap();
	let coin = wallet.get_full_vtxo(coin.id()).await.unwrap();
	let id = coin.id().to_string();
	let gross = coin.amount().to_sat();
	let destination = wallet.fallback_destination().await.unwrap().spk;
	drop(wallet);
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &[coin]).await;
	super::fallback_lightning::train_fee_estimator(&ctx).await;
	if interrupt {
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let log = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap_or_default();
				if log.contains("test selected release interrupted") { break; }
				tokio::time::sleep(Duration::from_millis(100)).await;
			}
		}).await.expect("the funded native payout must reach the release fault");
		let row = db.read(async |t| Ok(t.query_one("
			SELECT (SELECT count(*) FROM expiry_settlement WHERE id = $1) AS settlements,
				(SELECT count(*) FROM recovery_capital_release WHERE id = $1) AS releases,
				(SELECT count(*) FROM nursery_tx WHERE kind::text = 'expiry-payout') AS payouts,
				(SELECT sum(gross_sat)::bigint FROM recovery_source_hold) AS original_gross,
				(SELECT sum(held_sat)::bigint FROM recovery_source_hold) AS still_held,
				(SELECT spend_state::text FROM vtxo WHERE vtxo_id = $1) AS state
		", &[&id]).await?)).await.unwrap();
		assert_eq!(row.get::<_, i64>("settlements"), 0);
		assert_eq!(row.get::<_, i64>("releases"), 0);
		assert_eq!(row.get::<_, i64>("payouts"), 0);
		assert_eq!(row.get::<_, i64>("original_gross"), row.get::<_, i64>("still_held"));
		assert_eq!(row.get::<_, String>("state"), "spendable");
		db.write(async |t| {
			t.batch_execute("DROP TRIGGER interrupt_selected_release ON recovery_capital_release; DROP FUNCTION interrupt_selected_release();").await?;
			Ok(())
		}).await.unwrap();
	}
	let (point, _) = super::fallback_lightning::wait_and_reconcile_payout(&ctx, &db, &[id.clone()], &destination, gross).await;
	let row = db.read(async |t| Ok(t.query_one("
		SELECT c.payout_txid, c.gross_sat, h.gross_sat AS original_gross, h.held_sat,
			h.revision, c.source_revision
		FROM recovery_capital_release c JOIN recovery_source_hold h ON h.id = c.source_id
		WHERE c.id = $1
	", &[&id]).await?)).await.unwrap();
	assert_eq!(row.get::<_, String>("payout_txid"), point.txid.to_string());
	assert_eq!(row.get::<_, i64>("gross_sat") as u64, gross, "release is gross, not net recipient output");
	assert_eq!(row.get::<_, i64>("original_gross") - row.get::<_, i64>("held_sat"), gross as i64);
	assert_eq!(row.get::<_, i64>("revision"), row.get::<_, i64>("source_revision") + 1);
	let path = srv.config().data_dir.join("expiry-payout-witness").join(format!("{}.json", point.txid));
	let witness: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
	let releases = witness["capital_releases"].as_array().expect("published payout pins its capital effect");
	assert_eq!(releases.len(), 1);
	assert_eq!(releases[0]["id"], id);
	assert_eq!(releases[0]["gross_sat"], gross);
	export_selected_native_source(&ctx, &db, &id, point.txid, "after-selected-commit").await;
	srv.stop().await.unwrap();
	srv.start().await.unwrap();
	ctx.generate_blocks(2).await;
	let count = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM recovery_capital_release WHERE id = $1", &[&id]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, 1, "restart must not release principal a second time");
	export_selected_native_source(&ctx, &db, &id, point.txid, "after-restart").await;
}

/// The configured recovery destination belongs to this same single-keychain
/// wallet. Ordinary funding change must not become a recovery receipt.
#[tokio::test]
async fn recovery_reserve_ordinary_change_never_becomes_a_recovery_receipt() {
	let ctx = TestContext::new("bark_sdk/recovery_reserve_ordinary_change_never_becomes_a_recovery_receipt").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(sat(50_020_000)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(128);
			cfg.min_board_amount = sat(330);
			cfg.round_interval = Duration::from_secs(3600);
			cfg.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb_u32(1));
		}).watchmand_cfg(|cfg| {
			cfg.watchman.sweep_interval = Duration::from_secs(3600);
		}).recovery_fee_headroom(sat(20_000)).create().await;
	ctx.fund_captaind(&srv, sat(49_980_000)).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let sweep = srv.watchmand().config().sweep_address.clone().unwrap()
		.require_network(Network::Regtest).unwrap().script_pubkey();
	let mut rpc = srv.get_wallet_rpc().await;
	let initial = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
	let addresses: Vec<String> = core.call("deriveaddresses", &[
		json!(initial.public_descriptor), json!([0, 64]),
	]).unwrap();
	let scripts = addresses.iter().map(|address| Address::from_str(address).unwrap()
		.require_network(Network::Regtest).unwrap().script_pubkey()).collect::<Vec<_>>();
	let sweep_index = scripts.iter().position(|script| script == &sweep)
		.expect("the actual configured recovery destination must belong to the operator wallet");
	assert!(initial.utxos.iter().all(|u| u.script_pubkey != sweep.as_bytes()),
		"setup funding must use different real addresses; no backfill or forged zero history");
	assert!(initial.pending_transactions.is_empty() && initial.reserved_outpoints.is_empty());
	let opening_cash: u64 = initial.utxos.iter().map(|u| u.value_sat).sum();
	assert_eq!(opening_cash, btc(1).to_sat());
	let initial_receipts = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM recovery_receipt", &[],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(initial_receipts, 0);

	let bark = ctx.bark("bark", &srv).funded(sat(1_000_000)).create().await;
	bark.board(sat(80_000)).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	bark.sync().await;
	let mut finished = srv.subscribe_log::<RoundFinished>();
	let mut evidence = Vec::new();
	let mut miner_fees = 0u64;
	let mut outside_wallet = 0u64;
	let mut held = 0i64;
	let mut receipts = 0i64;
	for number in 0..8 {
		ctx.refresh_all(&srv, &[&bark]).await;
		let txid = finished.recv().wait(Duration::from_secs(30)).await.unwrap().txid;
		ctx.await_transaction(txid).await;
		let signed = core.get_raw_transaction(&txid, None).unwrap();
		let mut inputs = Vec::new();
		let mut gross = 0u64;
		for input in &signed.input {
			let parent = core.get_raw_transaction(&input.previous_output.txid, None).unwrap();
			let prevout = &parent.output[input.previous_output.vout as usize];
			assert!(scripts.contains(&prevout.script_pubkey), "only genuine operator inputs are priced here");
			gross += prevout.value.to_sat();
			inputs.push(json!({"outpoint":input.previous_output.to_string(),
				"value_sat":prevout.value.to_sat(),"script":prevout.script_pubkey.to_string(),
				"parent_signed_hex":bitcoin::consensus::encode::serialize_hex(&parent)}));
		}
		let output_total: u64 = signed.output.iter().map(|o| o.value.to_sat()).sum();
		let fee = gross.checked_sub(output_total).expect("real signed transaction must conserve sats");
		miner_fees += fee;
		outside_wallet += signed.output.iter().filter(|o| !scripts.contains(&o.script_pubkey))
			.map(|o| o.value.to_sat()).sum::<u64>();
		let collision = signed.output.iter().any(|o| o.script_pubkey == sweep);
		ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
		bark.sync().await;
		let snapshot = tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let inventory = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
				if !inventory.pending_transactions.iter().any(|tx| tx.txid == txid.to_string())
					&& inventory.tip_height == core.get_block_count().unwrap() as u32 { break inventory; }
				tokio::time::sleep(Duration::from_millis(100)).await;
			}
		}).await.expect("genuine native wallet must reconcile the confirmed funding transaction");
		let rows = db.read(async |t| Ok(t.query(
			"SELECT r.id, r.transaction_bytes, r.proof_state, h.id AS source_id, h.gross_sat, h.held_sat \
			FROM recovery_receipt r JOIN recovery_source_hold h ON h.receipt_id=r.id ORDER BY r.id,h.id", &[],
		).await?)).await.unwrap();
		let row_proof = rows.iter().map(|row| {
			let stored: Transaction = deserialize(&row.get::<_, Vec<u8>>("transaction_bytes")).unwrap();
			let actual = core.get_raw_transaction(&stored.compute_txid(), None).unwrap();
			assert_eq!(stored, actual, "durable receipts must bind exact genuine signed Core bytes");
			json!({"receipt_txid":row.get::<_, String>("id"),"source_outpoint":row.get::<_, String>("source_id"),
				"gross_sat":row.get::<_, i64>("gross_sat"),"held_sat":row.get::<_, i64>("held_sat"),
				"proof_state":row.get::<_, String>("proof_state")})
		}).collect::<Vec<_>>();
		held = rows.iter().map(|r| r.get::<_, i64>("held_sat")).sum();
		receipts = rows.len() as i64;
		let closing_cash: u64 = snapshot.utxos.iter().map(|u| u.value_sat).sum();
		assert_eq!(opening_cash, closing_cash + outside_wallet + miner_fees,
			"native operator cash plus actual non-wallet funding outputs and miners must conserve the opening injection");
		evidence.push(json!({"round_number":number,"round_txid":txid.to_string(),
			"signed_transaction_hex":bitcoin::consensus::encode::serialize_hex(&signed),
			"inputs":inputs,"miner_fee_sat":fee,"recovery_script_output":collision,
			"wallet_cash_sat":closing_cash,"cumulative_outside_wallet_sat":outside_wallet,
			"cumulative_miner_fees_sat":miner_fees,"held_sat":held,"receipt_sources":row_proof,
			"wallet_checkpoint_height":snapshot.tip_height,"wallet_checkpoint_hash":snapshot.tip_hash}));
		// Preserve the monetary failure before another funding attempt can be
		// refused by the newly fenced cash and mistaken for a setup failure.
		if held != 0 || receipts != 0 { break; }
	}
	let proof = json!({"scenario":"ordinary funding change is not reclaimed customer backing",
		"opening_injected_operator_sat":opening_cash,"recovery_descriptor_index":sweep_index,
		"public_descriptor":initial.public_descriptor,"configured_recovery_script":sweep.to_string(),
		"minimum_lifetime_blocks":128,"ordinary_rounds":evidence,"held_sat":held,"receipt_sources":receipts});
	std::fs::write(ctx.datadir.join("ordinary-change-recovery-receipt-proof.json"),
		serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
	assert_eq!(held, 0, "ordinary funding change must not quarantine operator principal as reclaimed customer backing");
	assert_eq!(receipts, 0, "a genuine ordinary round is not a recovery receipt");
}

/// A configured indexed but unrevealed owned destination must not be
/// returned as an ordinary deposit address, including across a real restart.
#[tokio::test]
async fn recovery_reserve_ordinary_address_issuance_skips_recovery_script_after_restart() {
	let ctx = TestContext::new("bark_sdk/recovery_reserve_ordinary_address_issuance_skips_recovery_script_after_restart").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).watchmand().create().await;
	let mut rpc = srv.get_wallet_rpc().await;
	let initial = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
	let addresses: Vec<String> = ctx.bitcoind().sync_client().call("deriveaddresses", &[
		json!(initial.public_descriptor), json!([0, 256]),
	]).unwrap();
	let recovery = Address::from_str(&addresses[16]).unwrap().require_network(Network::Regtest).unwrap();
	srv.stop().await.unwrap();
	srv.watchmand().stop().await.unwrap();
	let watchman_path = {
		let mut cfg = srv.watchmand().config_mut();
		cfg.sweep_address = Some(recovery.clone().into_unchecked());
		let path = cfg.data_dir.join(WATCHMAND_CONFIG_FILE);
		cfg.write_into(&mut std::fs::File::create(&path).unwrap()).unwrap();
		path
	};
	{
		let mut cfg = srv.config_mut();
		cfg.expiry_payout.recovery_fee_headroom_sat = Some(20_000);
		cfg.expiry_payout.watchman_config = Some(watchman_path);
	}
	srv.start().await.unwrap();
	let mut issued = Vec::new();
	for phase in 0..2 {
		for _ in 0..40 {
			let fresh = srv.get_rounds_funding_address().await;
			let status = srv.wallet_status().await.rounds.address.require_network(Network::Regtest).unwrap();
			for address in [fresh, status] {
				assert_ne!(address.to_string(), recovery.to_string(), "ordinary address issuance must never return the recovery script");
				let index = addresses.iter().position(|a| a == &address.to_string()).unwrap();
				issued.push(json!({"phase":phase,"index":index,"address":address.to_string()}));
			}
		}
		if phase == 0 { srv.stop().await.unwrap(); srv.start().await.unwrap(); }
	}
	assert!(issued.iter().all(|row| row["index"].as_u64().unwrap() > 16));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let receipts = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM recovery_receipt", &[]).await?.get::<_,i64>(0))).await.unwrap();
	assert_eq!(receipts, 0);
	std::fs::write(ctx.datadir.join("recovery-address-issuance-proof.json"), serde_json::to_vec_pretty(&json!({
		"configured_owned_recovery_index":16,"recovery_script":recovery.script_pubkey().to_string(),
		"issued":issued,"actual_server_restart":true,"recovery_receipts":receipts,
	})).unwrap()).unwrap();
}

/// Observe the actual default selector and native reserve, using real Core coins.
#[tokio::test]
async fn recovery_reserve_default_round_selector_keeps_affordable_confirmed_headroom() {
	use std::sync::{Arc, Mutex};
	use bdk_wallet::coin_selection::{CoinSelectionAlgorithm, CoinSelectionResult, DefaultCoinSelectionAlgorithm, InsufficientFunds};
	use bdk_wallet::WeightedUtxo;
	use bitcoin_ext::bdk::{PreferConfirmedCoinSelection, WithGuaranteedChange};

	#[derive(Debug, Clone)]
	struct Recorded<A> { inner: A, trace: Arc<Mutex<Vec<serde_json::Value>>> }
	impl<A: CoinSelectionAlgorithm> CoinSelectionAlgorithm for Recorded<A> {
		fn coin_select<R: bitcoin::secp256k1::rand::RngCore>(
			&self, required: Vec<WeightedUtxo>, optional: Vec<WeightedUtxo>,
			fee_rate: FeeRate, target: Amount, drain: &bitcoin::Script, rng: &mut R,
		) -> Result<CoinSelectionResult, InsufficientFunds> {
			let offered = required.iter().chain(&optional).map(|u| json!({
				"point":u.utxo.outpoint().to_string(), "sat":u.utxo.txout().value.to_sat(),
			})).collect::<Vec<_>>();
			let result = self.inner.coin_select(required, optional, fee_rate, target, drain, rng);
			let row = match &result {
				Ok(result) => json!({"offered":offered,"selected":result.selected.iter()
					.map(|u| json!({"point":u.outpoint().to_string(),"sat":u.txout().value.to_sat()})).collect::<Vec<_>>() }),
				Err(error) => json!({"offered":offered,"error":error.to_string()}),
			};
			self.trace.lock().unwrap().push(row);
			result
		}
	}

	let ctx = TestContext::new("bark_sdk/recovery_reserve_default_round_selector_keeps_affordable_confirmed_headroom").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).watchmand().recovery_fee_headroom(sat(20_000)).create().await;
	let mut rpc = srv.get_wallet_rpc().await;
	let initial = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
	assert_eq!(initial.utxos.len(), 2);
	assert_eq!(initial.utxos.iter().map(|u| u.value_sat).sum::<u64>(), 100_000_000);
	assert!(initial.pending_transactions.is_empty() && initial.reserved_outpoints.is_empty());
	let core = ctx.bitcoind().sync_client();
	let original_coins = initial.utxos.iter().map(|u| {
		let point = bitcoin::OutPoint::from_str(&u.outpoint).unwrap();
		let parent = core.get_raw_transaction(&point.txid, None).unwrap();
		let unspent = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(unspent.confirmations > 0);
		assert_eq!(unspent.value.to_sat(), u.value_sat);
		json!({"point":point.to_string(),"value_sat":u.value_sat,
			"parent_signed_hex":bitcoin::consensus::encode::serialize_hex(&parent),"confirmations":unspent.confirmations})
	}).collect::<Vec<_>>();
	let largest = initial.utxos.iter().max_by_key(|u| u.value_sat).unwrap();
	assert_eq!(largest.value_sat, 99_980_000);
	let large_point = bitcoin::OutPoint::from_str(&largest.outpoint).unwrap();
	srv.stop().await.unwrap();
	let cfg = srv.config().clone();
	let seed = server::wallet::read_mnemonic_from_datadir(&cfg.data_dir).unwrap().to_seed("");
	let master = bitcoin::bip32::Xpriv::new_master(cfg.network, &seed).unwrap();
	let client = server::bitcoind::build_client(&cfg.bitcoind.url, cfg.bitcoind.auth()).unwrap();
	let tip = server::bitcoind::deep_tip(&client).await.unwrap();
	let db = Db::connect(&cfg.postgres).await.unwrap();
	let mut wallet = server::wallet::PersistedWallet::load_derive_from_master_xpriv(
		db, client, cfg.network, &master, server::wallet::WalletKind::Rounds, tip, cfg.min_trusted_confs,
	).await.unwrap();
	let destination = ctx.bitcoind().get_new_address().script_pubkey();
	let rate = FeeRate::from_sat_per_vb_u32(1);
	let mut attempts = Vec::new();
	let mut refusal = None;
	for number in 0..32 {
		let trace = Arc::new(Mutex::new(Vec::new()));
		let selection = Recorded { inner: WithGuaranteedChange(PreferConfirmedCoinSelection(DefaultCoinSelectionAlgorithm::default())), trace: trace.clone() };
		let result = wallet.build_quiet_tx_at_chunk_feerate(selection, rate, Some(rate), |builder| {
			builder.ordering(bdk_wallet::TxOrdering::Untouched);
			builder.add_recipient(destination.clone(), sat(80_000)); Ok(())
		});
		let error = result.as_ref().err().map(|error| format!("{error:#}"));
		attempts.push(json!({"attempt":number,"trace":trace.lock().unwrap().clone(),"error":error}));
		if let Some(error) = error { refusal = Some(error); break; }
	}
	// Prove the same native guard and signed ceiling admit a real affordable subset.
	let valid = wallet.build_quiet_tx_at_chunk_feerate(
		WithGuaranteedChange(PreferConfirmedCoinSelection(DefaultCoinSelectionAlgorithm::default())), rate, Some(rate), |builder| {
			builder.ordering(bdk_wallet::TxOrdering::Untouched);
			builder.add_utxo(large_point)?;
			builder.manually_selected_only();
			builder.add_recipient(destination.clone(), sat(80_000)); Ok(())
		},
	).unwrap();
	assert_eq!(valid.unsigned_tx.input.len(), 1);
	assert_eq!(valid.unsigned_tx.input[0].previous_output, large_point);
	let fee = valid.fee().unwrap();
	let signed = wallet.finish_tx_with_max_fee_rate(valid, Some(rate)).unwrap();
	assert!(fee <= rate.fee_wu(signed.weight()).unwrap());
	for u in &initial.utxos {
		let point = bitcoin::OutPoint::from_str(&u.outpoint).unwrap();
		assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_some(), "no proof transaction was broadcast");
	}
	let proof = json!({"scope":"native funding-selection liveness; no transaction broadcast and no money-loss verdict",
		"original_core_coins":original_coins,"actual_attempts":attempts,"default_refusal":refusal,
		"valid_large_only_signed_hex":bitcoin::consensus::encode::serialize_hex(&signed),
		"valid_large_only_fee_sat":fee.to_sat(),"valid_large_only_weight_wu":signed.weight().to_wu(),"confirmed_remainder_sat":20_000});
	std::fs::write(ctx.datadir.join("default-round-selection-proof.json"), serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
	assert!(refusal.is_none(), "native default round selector refused funded work although an actual signed guarded subset exists: {refusal:?}");
}

// Test evidence only. Original signed bodies/coin bytes remain owner-only.
async fn export_selected_native_source(
	ctx: &TestContext, db: &Db, id: &str, payout: bitcoin::Txid, phase: &str,
) {
	use bitcoin::hashes::{sha256, Hash};
	use std::collections::{BTreeMap, BTreeSet};
	use std::io::Write;
	use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
	assert!(matches!(phase, "after-selected-commit" | "after-restart"));
	let core = ctx.bitcoind().sync_client();
	let before = core.get_best_block_hash().unwrap();
	// ONE SELECT means every subquery has the same PostgreSQL statement snapshot.
	let source: String = db.read(async |t| Ok(t.query_one(r#"
		WITH selected AS (SELECT * FROM vtxo WHERE vtxo_id=$1),
		s AS (SELECT * FROM expiry_settlement WHERE txid=$2),
		c AS (SELECT * FROM recovery_capital_release WHERE payout_txid=$2),
		h AS (SELECT * FROM recovery_source_hold WHERE id IN (SELECT source_id FROM c)),
		r AS (SELECT * FROM recovery_receipt WHERE id IN (SELECT receipt_id FROM c))
		SELECT json_build_object(
		 'read_only',current_setting('transaction_read_only'),
		 'statement_snapshot',txid_current_snapshot()::TEXT,
		 'selected_vtxos',(SELECT json_agg(x) FROM (SELECT vtxo_id,encode(vtxo,'hex') AS original_vtxo_hex,amount,expiry,anchor_point,server_pubkey,encode(policy,'hex') AS policy_hex,spend_state,spent_in_round,oor_spent_txid,offboarded_in,onchain_spent_txid FROM selected) x),
		 'settlements',(SELECT json_agg(x) FROM (SELECT id,txid,fee_sat,encode(spk,'hex') AS spk FROM s ORDER BY id) x),
		 'nursery',(SELECT json_agg(x) FROM (SELECT txid,encode(tx,'hex') AS original_signed_hex,kind::TEXT,confirmed_at_height,abandoned_at,confirm_target_height FROM nursery_tx WHERE txid=$2) x),
		 'capital_releases',(SELECT json_agg(x) FROM (SELECT * FROM c ORDER BY id) x),
		 'source_holds',(SELECT json_agg(x) FROM (SELECT * FROM h ORDER BY id) x),
		 'receipts',(SELECT json_agg(x) FROM (SELECT id,encode(transaction_bytes,'hex') AS original_signed_hex,block_height,block_hash,proof_state,proof_gap FROM r ORDER BY id) x),
		 'source_release_history',(SELECT json_agg(x) FROM (SELECT * FROM recovery_capital_release WHERE source_id IN (SELECT id FROM h) ORDER BY id LIMIT 2001) x),
		 'reserve',(SELECT row_to_json(x) FROM (SELECT id,encode(sweep_spk,'hex') AS sweep_spk,checkpoint_height,checkpoint_hash,fee_headroom_sat FROM recovery_reserve WHERE id='rounds') x),
		 'global_held_sat',(SELECT coalesce(sum(held_sat),0)::TEXT FROM recovery_source_hold),
		 'key_links',(SELECT json_agg(x) FROM (SELECT encode(user_pubkey,'hex') AS user_pubkey,encode(mailbox_pk,'hex') AS mailbox_pk,encode(sig,'hex') AS sig FROM key_link WHERE user_pubkey IN (SELECT substring(policy FROM 2 FOR 33) FROM selected)) x),
		 'fallback_records',(SELECT json_agg(x) FROM (SELECT encode(mailbox_pk,'hex') AS mailbox_pk,encode(spk,'hex') AS spk,seq,encode(sig,'hex') AS sig FROM fallback_record WHERE mailbox_pk IN (SELECT mailbox_pk FROM key_link WHERE user_pubkey IN (SELECT substring(policy FROM 2 FOR 33) FROM selected))) x)
		)::TEXT
	"#, &[&id, &payout.to_string()]).await?.get(0))).await.unwrap();
	assert!(source.len() <= 1024 * 1024, "private selected SQL source byte bound");
	let sql: serde_json::Value = serde_json::from_str(&source).unwrap();
	assert_eq!(sql["read_only"], "on");
	for key in ["selected_vtxos", "settlements", "nursery", "capital_releases", "source_holds", "receipts", "source_release_history", "key_links", "fallback_records"] {
		assert!(sql[key].as_array().expect("source rows must exist").len() <= 2000);
	}
	let mut roots = BTreeSet::from([payout]);
	for row in sql["receipts"].as_array().unwrap() { roots.insert(row["id"].as_str().unwrap().parse().unwrap()); }
	for row in sql["selected_vtxos"].as_array().unwrap() {
		roots.insert(row["anchor_point"].as_str().unwrap().split(':').next().unwrap().parse().unwrap());
	}
	assert!(roots.len() <= 32);
	let mut txs = BTreeMap::new();
	for txid in &roots { txs.insert(*txid, core.get_raw_transaction(txid, None).unwrap()); }
	let parents = txs.values().flat_map(|tx| tx.input.iter().map(|i| i.previous_output))
		.filter(|point| !point.is_null()).map(|point| point.txid).collect::<BTreeSet<_>>();
	assert!(parents.len() <= 96);
	for txid in parents { txs.entry(txid).or_insert_with(|| core.get_raw_transaction(&txid, None).unwrap()); }
	assert!(txs.len() <= 128);
	let mut originals = Vec::new();
	let mut projected = Vec::new();
	let mut total_bytes = 0;
	for (txid, tx) in txs {
		let raw = bitcoin::consensus::serialize(&tx);
		assert!(raw.len() <= 256 * 1024); total_bytes += raw.len(); assert!(total_bytes <= 1024 * 1024);
		let info = core.get_raw_transaction_info(&txid, None).unwrap();
		originals.push(serde_json::json!({"txid":txid.to_string(),"original_signed_bytes":raw,
			"block_hash":info.blockhash.map(|h| h.to_string()),"confirmations":info.confirmations}));
		if tx.input.iter().all(|input| input.script_sig.is_empty()) {
			let weight = tx.weight().to_wu(); let mut body = tx;
			for input in &mut body.input { input.witness = bitcoin::Witness::default(); }
			let encoded = bitcoin::consensus::serialize(&body).iter().map(|b| format!("{b:02x}")).collect::<String>();
			projected.push(serde_json::json!({"txid":txid.to_string(),"transaction_body":encoded,
				"encoding":"bitcoin-stripped-body-v1","original_signed_weight_wu":weight}));
		} else { projected.push(serde_json::json!({"txid":txid.to_string(),"gap":"private nonempty scriptSig; no public body"})); }
	}
	let mut receipt_blocks = Vec::new();
	for row in sql["receipts"].as_array().unwrap() {
		let height = row["block_height"].as_u64().unwrap();
		receipt_blocks.push(serde_json::json!({"height":height,"actual_core_block_hash":core.get_block_hash(height).unwrap().to_string()}));
	}
	let after = core.get_best_block_hash().unwrap();
	let private = serde_json::to_vec(&serde_json::json!({"phase":phase,"sql":sql,"core_originals":originals,
		"receipt_blocks":receipt_blocks,"core_genesis":core.get_block_hash(0).unwrap().to_string(),
		"before_tip":before.to_string(),"after_tip":after.to_string(),"stable_tip":before==after})).unwrap();
	assert!(private.len() <= 4 * 1024 * 1024);
	let directory = ctx.datadir.join("selected-payout-source-private");
	if phase == "after-selected-commit" { std::fs::create_dir(&directory).unwrap(); }
	assert!(std::fs::symlink_metadata(&directory).unwrap().is_dir());
	std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
	let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(directory.join(format!("{phase}.private.json"))).unwrap();
	file.write_all(&private).unwrap(); file.sync_all().unwrap();
	let public = serde_json::to_vec(&serde_json::json!({"phase":phase,"payout_txid":payout.to_string(),
		"private_source_sha256":sha256::Hash::hash(&private).to_string(),"private_source_bytes":private.len(),
		"public_transactions":projected,"before_tip":before.to_string(),"after_tip":after.to_string(),
		"stable_tip":before==after,"complete":false,"terminal_payment_verified":false})).unwrap();
	assert!(public.len() <= 2 * 1024 * 1024);
	let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(directory.join(format!("{phase}.public.json"))).unwrap();
	file.write_all(&public).unwrap(); file.sync_all().unwrap();
	std::fs::File::open(&directory).unwrap().sync_all().unwrap();
}
