//! Native admission quarantine tests. These do not qualify the later release/classifier.

use std::time::Duration;

use bitcoin::{Amount, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;

use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use server::database::Db;

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
	srv.stop().await.unwrap();
	srv.start().await.unwrap();
	ctx.generate_blocks(2).await;
	let count = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM recovery_capital_release WHERE id = $1", &[&id]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, 1, "restart must not release principal a second time");
}
