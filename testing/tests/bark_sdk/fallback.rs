use std::time::Duration;

use bitcoin::{Address, FeeRate, Network, OutPoint, Transaction, Txid};
use bitcoin::bip32::Xpriv;
use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use bark::lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use bdk_wallet::{KeychainKind, SignOptions};
use bdk_wallet::template::Bip84;

use ark_testing::{TestContext, btc, sat};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use ark::ProtocolEncoding;
use ark::attestations::FallbackRecordAttestation;
use server::database::Db;
use server_rpc::protos;

#[tokio::test]
async fn fallback_register_board_rotate_and_offline_pool() {
	let ctx = TestContext::new("bark_sdk/fallback_register_board_rotate_and_offline_pool").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let wallet = ctx.bark_sdk("wallet", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	assert!(record.spk.is_p2wpkh());
	let mailbox = wallet.mailbox_keypair().public_key().serialize().to_vec();
	let stored = db.read(async |t| Ok(t.query_one(
		"SELECT spk, seq FROM fallback_record WHERE mailbox_pk = $1", &[&mailbox],
	).await?)).await.unwrap();
	assert_eq!(stored.get::<_, Vec<u8>>(0), record.spk.as_bytes());
	assert_eq!(stored.get::<_, i64>(1), record.seq as i64);
	for vtxo in wallet.spendable_vtxos().await.unwrap() {
		let pk = vtxo.user_pubkey().serialize().to_vec();
		let linked = db.read(async |t| Ok(t.query_one(
			"SELECT mailbox_pk FROM key_link WHERE user_pubkey = $1", &[&pk],
		).await?)).await.unwrap();
		assert_eq!(linked.get::<_, Vec<u8>>(0), mailbox);
	}

	// The rotation trigger is an actual chain receive. S5 separately proves
	// the automatic expiry transaction; this test isolates client registration.
	let address = Address::from_script(&record.spk, Network::Regtest).unwrap();
	ctx.bitcoind().fund_addr(address, sat(20_000)).await;
	ctx.generate_blocks(1).await;
	wallet.sync_onchain().await.unwrap();
	let rotated = wallet.fallback_destination().await.unwrap();
	assert_ne!(rotated.spk, record.spk);
	assert!(rotated.seq > record.seq);
	let foreign = ctx.bitcoind().get_new_address().script_pubkey();
	assert!(wallet.set_fallback_destination(foreign).await.is_err());
	assert_eq!(wallet.fallback_destination().await.unwrap(), rotated);

	wallet.sync().await;
	srv.stop().await.unwrap();
	let mut keys = Vec::<PublicKey>::new();
	for _ in 0..8 {
		let (address, _) = wallet.new_address_with_index().await.unwrap();
		let pk = address.policy().user_pubkey();
		assert!(!keys.contains(&pk));
		keys.push(pk);
	}
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.new_address()).await
		.expect("offline exhaustion returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"));
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.board_amount(sat(10_000))).await
		.expect("offline board returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"));
	assert!(wallet.pending_boards().await.unwrap().is_empty());

	// An empty pool must fail before Lightning send locks any input. No LN
	// node is needed: the valid invoice must never reach an HTLC cosign step.
	let secp = Secp256k1::new();
	let key = SecretKey::from_slice(&[42; 32]).unwrap();
	let invoice = InvoiceBuilder::new(Currency::Regtest)
		.description("offline linking failure".into())
		.payment_hash(sha256::Hash::hash(b"fallback offline send"))
		.payment_secret(PaymentSecret([43; 32]))
		.current_timestamp().min_final_cltv_expiry_delta(144)
		.amount_milli_satoshis(10_000_000)
		.build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &key)).unwrap();
	let before = wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<Vec<_>>();
	assert!(!before.is_empty());
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.pay_lightning_invoice(invoice, None, false))
		.await.expect("offline LN send returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"), "{err:#}");
	assert_eq!(wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<Vec<_>>(), before);
	assert!(wallet.pending_lightning_sends().await.unwrap().is_empty());
}

/// Real funded boards, sweep, expiry task and nursery. The absent wallet's
/// individual coins are all below the configured minimum; their group is not.
#[tokio::test]
async fn fallback_grouped_expiry_without_client() {
	grouped_expiry_without_client("fallback_grouped_expiry_without_client", 3, 5_000, 100, false, false).await;
}

#[tokio::test]
async fn fallback_grouped_200_small_coins() {
	grouped_expiry_without_client("fallback_grouped_200_small_coins", 200, 600, 200, false, false).await;
}

/// A funded board must survive the owner disappearing before registration.
#[tokio::test]
async fn fallback_abandoned_board_without_registration() {
	grouped_expiry_without_client("fallback_abandoned_board_without_registration", 1, 25_000, 100, true, false).await;
}

#[tokio::test]
async fn fallback_abandoned_board_return_clears_pending() {
	grouped_expiry_without_client("fallback_abandoned_board_return_clears_pending", 1, 25_000, 100, true, true).await;
}

async fn grouped_expiry_without_client(
	name: &str, count: usize, amount: u64, max_batch: usize, abandoned: bool, returning: bool,
) {
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().create().await;
	let wallet = ctx.bark_sdk("wallet", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(count as u64 * amount + 1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let never_broadcast = if abandoned && !returning {
		let (key, _) = wallet.derive_store_next_keypair().await.unwrap();
		let (address, expiry) = wallet.board_funding_address(&key).await.unwrap();
		let psbt = wallet.onchain().unwrap().write().await.prepare_tx(
			&[(address, sat(40_000))], FeeRate::from_sat_per_vb(3).unwrap(),
		).await.unwrap();
		let funding = &psbt.unsigned_tx;
		let info = srv.ark_info().await;
		let builder = ark::board::BoardBuilder::new(key.public_key(), expiry,
			info.server_pubkey, info.vtxo_exit_delta);
		let vout = funding.output.iter().position(|o| o.script_pubkey == builder.funding_script_pubkey()).unwrap();
		let (_, nonce) = ark::musig::nonce_pair(&key);
		let request = protos::BoardCosignRequest {
			amount: 40_000, utxo: OutPoint::new(funding.compute_txid(), vout as u32).serialize(),
			expiry_height: expiry.into(), user_pubkey: key.public_key().serialize().to_vec(),
			pub_nonce: nonce.serialize().to_vec(), funding_tx: bitcoin::consensus::serialize(funding),
		};
		let mut rpc = srv.get_public_rpc().await;
		let mut wrong_amount = request.clone();
		wrong_amount.amount += 1;
		let err = rpc.request_board_cosign(wrong_amount).await.unwrap_err();
		assert!(err.message().contains("amount or script"), "{err}");
		let mut wrong_script = funding.clone();
		wrong_script.output[vout].script_pubkey = ctx.bitcoind().get_new_address().script_pubkey();
		let mut wrong_request = request.clone();
		wrong_request.utxo = OutPoint::new(wrong_script.compute_txid(), vout as u32).serialize();
		wrong_request.funding_tx = bitcoin::consensus::serialize(&wrong_script);
		let err = rpc.request_board_cosign(wrong_request).await.unwrap_err();
		assert!(err.message().contains("amount or script"), "{err}");
		let pending = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM pending_board", &[])
			.await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(pending, 0, "invalid cosigns cannot install pending candidates");
		rpc.request_board_cosign(request.clone()).await.unwrap();
		rpc.request_board_cosign(request).await.unwrap();
		// A third cosign through the real client uses a new nonce but must
		// identify the same entitlement; the unfinished PSBT is never broadcast.
		Some(wallet.board_psbt(psbt, key, expiry).await.unwrap())
	} else { None };
	let mut board_coins = Vec::new();
	for i in 0..count {
		let board = wallet.board_amount(sat(amount)).await.unwrap();
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		board_coins.push(wallet.get_vtxo_by_id(board.vtxos[0]).await.unwrap());
		// Confirm short chains instead of changing Core's mempool limits.
		if i % 8 == 7 { ctx.generate_blocks(1).await; }
	}
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	if !abandoned {
		// Force watchman to observe funding before the registration RPC. The
		// anchor's reserved exit must still be an idempotent spend in this order.
		let anchors = board_coins.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let confirmed = db.read(async |t| Ok(t.query_one(
					"SELECT count(*) FROM vtxo WHERE vtxo_id=ANY($1) AND confirmed_height IS NOT NULL",
					&[&anchors],
				).await?.get::<_, i64>(0))).await.unwrap();
				if confirmed as usize == count { break; }
				tokio::time::sleep(Duration::from_millis(100)).await;
			}
		}).await.expect("watchman must confirm funding before client registration");
		wallet.sync().await;
	}
	let record = wallet.fallback_destination().await.unwrap();
	let mailbox_key = wallet.mailbox_keypair();
	let coins = if abandoned { board_coins } else { wallet.spendable_vtxos().await.unwrap() };
	let signed_board = if abandoned {
		Some(wallet.get_full_vtxo(coins[0].id()).await.unwrap())
	} else { None };
	if abandoned {
		let id = coins[0].id().to_string();
		let count = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM vtxo WHERE vtxo_id=$1", &[&id])
			.await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(count, 0, "cosign must not install a registerable user coin");
		let mut rpc = srv.get_public_rpc().await;
		let err = rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
			vtxos: vec![signed_board.as_ref().unwrap().serialize()],
		}).await.unwrap_err();
		assert!(err.message().contains("vtxo not found"), "{err}");
	}
	assert_eq!(coins.len(), count);
	let principal = coins.iter().map(|v| v.amount().to_sat()).sum::<u64>();
	assert_eq!(principal, count as u64 * amount);
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	let chain_balance_before = wallet.onchain().unwrap().read().await.balance().await;
	let return_config = if returning { Some(wallet.config().clone()) } else { None };
	drop(wallet);

	// Feed Core's real estimator with confirmed transactions. No fee estimate
	// is injected into the server or substituted by a test fallback.
	let core = ctx.bitcoind().sync_client();
	for _ in 0..12 {
		for _ in 0..8 {
			let address = ctx.bitcoind().get_new_address();
			let _: Txid = core.call("sendtoaddress", &[
				address.to_string().into(), 0.001.into(), "".into(), "".into(),
				false.into(), true.into(), serde_json::Value::Null, "unset".into(),
				serde_json::Value::Null, 3.into(),
			]).unwrap();
		}
		ctx.generate_blocks(1).await;
	}
	let estimate = core.estimate_smart_fee(6, None).unwrap();
	assert!(estimate.fee_rate.is_some(), "real fee estimate required: {estimate:?}");
	if count > 3 {
		// This gate tests one group of 200 simultaneously eligible coins.
		// Sweeps can confirm in several blocks. Keep payouts disabled until
		// every backing path has a confirmed real sweep, without changing
		// any coin state or substituting synthetic sweep history.
		let tip = ctx.bitcoind().get_block_count().await as u32;
		ctx.generate_blocks(expiry.saturating_sub(tip) + 3).await;
		let anchors = coins.iter().map(|v| v.chain_anchor().to_string()).collect::<Vec<_>>();
		tokio::time::timeout(Duration::from_secs(90), async {
			loop {
				let rows = db.read(async |t| Ok(t.query(
					"SELECT vtxo_id, onchain_spent_txid FROM vtxo WHERE vtxo_id = ANY($1)",
					&[&anchors],
				).await?)).await.unwrap();
				assert_eq!(rows.len(), count);
				let all_swept = rows.iter().all(|row| {
					let Some(txid) = row.get::<_, Option<String>>("onchain_spent_txid") else { return false; };
					let txid: Txid = txid.parse().unwrap();
					let info = core.get_raw_transaction_info(&txid, None).unwrap();
					if info.confirmations.unwrap_or(0) == 0 { return false; }
					let tx = core.get_raw_transaction(&txid, None).unwrap();
					let anchor: OutPoint = row.get::<_, String>("vtxo_id").parse().unwrap();
					assert!(tx.input.iter().any(|i| i.previous_output == anchor));
					true
				});
				if all_swept { break; }
				tokio::time::sleep(Duration::from_secs(1)).await;
				ctx.generate_blocks(1).await;
			}
		}).await.expect("all board sweeps must confirm before the grouped test starts");
	}

	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.interval = Duration::from_secs(1);
		config.expiry_payout.grace_blocks = 0;
		config.expiry_payout.sweep_min_confs = 1;
		config.expiry_payout.min_payout_sat = 10_000;
		config.expiry_payout.max_batch = max_batch;
		config.expiry_payout.receipt_dir = ctx.datadir.join("receipts");
		config.expiry_payout.watchman_config = Some(
			srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE),
		);
	}
	srv.start().await.unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(expiry.saturating_sub(tip) + 3).await;
	let rows = tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT id, txid, fee_sat, spk FROM expiry_settlement WHERE id = ANY($1) ORDER BY id",
				&[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("swept grouped boards must be paid within 90 seconds");
	if let Some(never) = never_broadcast {
		let missing_txid = never.funding_tx.compute_txid();
		assert!(core.get_raw_transaction(&missing_txid, None).is_err());
		let never_id = never.vtxos[0].to_string();
		let settled = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM expiry_settlement WHERE id=$1", &[&never_id],
		).await?.get::<_, i64>(0))).await.unwrap();
		assert_eq!(settled, 0, "a never-broadcast proposal has no payout entitlement");
		let mut rpc = srv.get_public_rpc().await;
		assert!(rpc.register_board_vtxo(protos::BoardVtxoRequest {
			board_vtxo: signed_board.as_ref().unwrap().serialize(),
		}).await.is_err(), "a paid board cannot be registered again");
	}
	let txid: Txid = rows[0].get::<_, String>("txid").parse().unwrap();
	let fee = rows[0].get::<_, i64>("fee_sat") as u64;
	for row in &rows {
		assert_eq!(row.get::<_, String>("txid"), txid.to_string());
		assert_eq!(row.get::<_, Vec<u8>>("spk"), record.spk.as_bytes());
		assert_eq!(row.get::<_, i64>("fee_sat") as u64, fee);
	}
	ctx.await_transaction(txid).await;
	ctx.generate_blocks(1).await;
	let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
	let outputs = tx.output.iter().filter(|o| o.script_pubkey == record.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert!(fee > 0);
	assert_eq!(outputs[0].value.to_sat(), principal - fee);
	assert!(outputs[0].value >= sat(10_000));
	let input = tx.input.iter().map(|i| {
		core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()
	}).sum::<u64>();
	assert_eq!(fee, input - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>());
	let vout = tx.output.iter().position(|o| o.script_pubkey == record.spk).unwrap() as u32;
	let unspent = core.get_tx_out(&txid, vout, Some(true)).unwrap().unwrap();
	assert!(unspent.confirmations >= 1);
	assert_eq!(unspent.value, outputs[0].value);
	let receipt_path = ctx.datadir.join("receipts").join(format!("{txid}.json"));
	tokio::time::timeout(Duration::from_secs(10), async {
		while !receipt_path.exists() { tokio::time::sleep(Duration::from_millis(100)).await; }
	}).await.expect("receipt must be exported");
	let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
	assert_eq!(receipt["outputs"].as_array().unwrap().len(), 1);
	assert_eq!(receipt["outputs"][0]["amount_sat"], principal - fee);
	assert_eq!(receipt["outputs"][0]["fee_sat"], fee);
	println!("grouped expiry payout: txid={txid}, coins={count}, principal_sat={principal}, net_sat={}, fee_sat={fee}",
		outputs[0].value.to_sat());

	let mut latest_record_seq = record.seq;
	if let Some(mut config) = return_config {
		// The test daemon reserves new ports on restart. Reopen the original
		// wallet data using the same server identity at its current test URL.
		config.server_address = srv.ark_url();
		let wallet = bark::Wallet::open(Network::Regtest,
			bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config.clone(),
			bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("wallet")), run_daemon: false, ..Default::default() },
		).await.unwrap();
		wallet.require_ark_info().await.unwrap();
		srv.stop().await.unwrap();
		wallet.sync_pending_boards().await.unwrap();
		assert_eq!(wallet.pending_boards().await.unwrap().len(), 1, "unknown status keeps the entitlement");
		assert_eq!(wallet.get_vtxo_by_id(coins[0].id()).await.unwrap().state.kind(),
			bark::vtxo::VtxoStateKind::Locked);
		assert!(!wallet.exit_mgr().is_exiting(coins[0].id()).await, "offline return must not start an impossible exit");
		drop(wallet);
		srv.start().await.unwrap();
		config.server_address = srv.ark_url();
		let wallet = bark::Wallet::open(Network::Regtest,
			bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
			bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("wallet")), run_daemon: false, ..Default::default() },
		).await.unwrap();
		for _ in 0..2 {
			wallet.sync().await;
			wallet.sync_onchain().await.unwrap();
		}
		assert!(wallet.pending_boards().await.unwrap().is_empty(), "paid board must leave pending state");
		assert_eq!(wallet.balance().await.unwrap().total(), sat(0));
		assert_eq!(wallet.get_vtxo_by_id(coins[0].id()).await.unwrap().state.kind(),
			bark::vtxo::VtxoStateKind::Spent);
		assert!(!wallet.exit_mgr().is_exiting(coins[0].id()).await, "paid board must not start an exit");
		assert_eq!(wallet.onchain().unwrap().read().await.balance().await,
			chain_balance_before + outputs[0].value);
		latest_record_seq = wallet.fallback_destination().await.unwrap().seq;
		let history = wallet.history().await.unwrap();
		let debits = history.iter().filter(|m| m.subsystem.name == "bark.server_spend").collect::<Vec<_>>();
		assert_eq!(debits.len(), 1, "repeated sync must not duplicate the Ark debit");
		assert_eq!(debits[0].effective_balance, -sat(principal).to_signed().unwrap());
		assert!(history.iter().all(|m| m.status == bark::movement::MovementStatus::Successful));
		assert_eq!(history.iter().map(|m| m.effective_balance).sum::<bitcoin::SignedAmount>(),
			bitcoin::SignedAmount::ZERO);
	}

	// Restore from only the mnemonic and Bitcoin blocks. A new standard BDK
	// wallet scans both BIP84 chains with lookahead 20, without Ark databases,
	// coin keys, record scripts, address indexes or a connection to captaind.
	srv.stop().await.unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let mut restored = bdk_wallet::Wallet::create(
		Bip84(master, KeychainKind::External), Bip84(master, KeychainKind::Internal),
	).network(Network::Regtest).lookahead(20).create_wallet_no_persist().unwrap();
	for height in 1..=core.get_block_count().unwrap() {
		let hash = core.get_block_hash(height).unwrap();
		restored.apply_block(&core.get_block(&hash).unwrap(), height as u32).unwrap();
	}
	let payout = OutPoint::new(txid, vout);
	let discovered = restored.list_unspent().find(|u| u.outpoint == payout)
		.expect("mnemonic-only BIP84 restore must discover the payout");
	assert_eq!(discovered.txout.value, outputs[0].value);
	let destination = ctx.bitcoind().get_new_address();
	let mut builder = restored.build_tx();
	builder.add_utxo(payout).unwrap().manually_selected_only()
		.drain_to(destination.script_pubkey()).fee_rate(FeeRate::from_sat_per_vb(3).unwrap());
	let mut psbt = builder.finish().unwrap();
	assert!(restored.sign(&mut psbt, SignOptions::default()).unwrap());
	let spend = psbt.extract_tx().unwrap();
	assert_eq!(spend.input.len(), 1);
	assert_eq!(spend.input[0].previous_output, payout);
	let spend_txid = core.send_raw_transaction(&spend).unwrap();
	ctx.bitcoind().generate(1).await;
	assert!(core.get_tx_out(&txid, vout, Some(true)).unwrap().is_none());
	assert_eq!(ctx.bitcoind().get_received_by_address(&destination), spend.output[0].value);
	println!("mnemonic-only BIP84 recovery: lookahead=20, payout={payout}, confirmed_spend={spend_txid}");

	// A later valid record update cannot rewrite the historical receipt.
	srv.start().await.unwrap();
	let next_spk = restored.next_unused_address(KeychainKind::External).script_pubkey();
	assert_ne!(next_spk, record.spk);
	let seq = latest_record_seq + 1;
	let mut signed_record = next_spk.as_bytes().to_vec();
	signed_record.extend_from_slice(&seq.to_le_bytes());
	signed_record.extend_from_slice(&FallbackRecordAttestation::new(&next_spk, seq, &mailbox_key).serialize());
	assert_eq!(srv.get_public_rpc().await.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox_key.public_key().serialize().to_vec(),
		record: Some(signed_record.clone()), key_links: vec![],
	}).await.unwrap().into_inner().record, signed_record);
	std::fs::remove_file(&receipt_path).unwrap();
	tokio::time::timeout(Duration::from_secs(10), async {
		while !receipt_path.exists() { tokio::time::sleep(Duration::from_millis(100)).await; }
	}).await.expect("receipt must regenerate after record rotation");
	let rebuilt: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
	assert_eq!(rebuilt, receipt);
}
