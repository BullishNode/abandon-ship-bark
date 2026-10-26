//! Real server decisions must not discard a restored or imported exit path.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bark::{ExitProtectionIncomplete, ImportVtxoArgs};
use bark::{OpenWalletArgs, Wallet, WalletSeed};
use bark::exit::ExitState;
use bark::movement::{Movement, MovementId, MovementStatus, PaymentMethod};
use bark::onchain::OnchainWallet;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
use bitcoin_ext::BlockDelta;
use bitcoin::{Address, Network, OutPoint, Script, TxOut};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::Db;
use server_rpc::protos;

use ark::{ProtocolEncoding, VtxoId};
use ark_testing::{sat, TestContext};
use ark_testing::context::builders::BarkSdkBuilder;
use ark_testing::daemon::captaind::{self, ArkClient};

#[derive(Clone, Default)]
struct HistoryGate {
	refuse: Arc<AtomicBool>,
	requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for HistoryGate {
	async fn get_vtxo_spend_proof(&self, upstream: &mut ArkClient, request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		self.requests.fetch_add(1, Ordering::SeqCst);
		if self.refuse.load(Ordering::SeqCst) { return Err(tonic::Status::unavailable("history deliberately unavailable")); }
		Ok(upstream.get_vtxo_spend_proof(request).await?.into_inner())
	}
}

fn config(config: &mut bark::Config) {
	config.daemon_manual_sync = true;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
}

async fn mnemonic(ctx: &TestContext, name: &str) -> bip39::Mnemonic {
	let text = tokio::fs::read_to_string(ctx.datadir.join(name).join("mnemonic")).await.unwrap();
	bip39::Mnemonic::from_str(text.trim()).unwrap()
}

async fn reopen(ctx: &TestContext, name: &str, srv: &captaind::Captaind) -> Wallet {
	// The harness deliberately reallocates listener ports after stop/start.
	// Reopen the existing databases against its current endpoint.
	let mnemonic = mnemonic(ctx, name).await;
	let db = Arc::new(SqliteClient::open(ctx.datadir.join(name).join("db.sqlite")).unwrap());
	let onchain = OnchainWallet::load_or_create(Network::Regtest, mnemonic.to_seed(""), db.clone()).await.unwrap();
	let script = onchain.fallback_script();
	let mut cfg = ctx.bark_default_cfg(srv, Some(ctx.bitcoind()));
	config(&mut cfg);
	Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), cfg, OpenWalletArgs {
		persister: Some(db), onchain: Some(Arc::new(tokio::sync::RwLock::new(onchain))),
		fallback_spk: Some(script), run_daemon: false, ..Default::default()
	}).await.unwrap()
}

async fn await_decision(ctx: &TestContext, srv: &captaind::Captaind, coin: &bark::WalletVtxo) {
	let height = ctx.bitcoind().get_block_count().await as u32;
	let expired = ctx.generate_blocks(coin.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let database = Db::connect(&srv.config().postgres).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let exists = database.read(async |tx| Ok(tx.query_opt(
				"SELECT id AS entitlement_id FROM expiry_entitlement WHERE vtxo_id=$1", &[&coin.id().to_string()],
			).await?)).await.unwrap().is_some();
			if exists { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual unfunded background allocation did not decide the coin");
}

// Read at a stable manual-sync boundary. Equality includes every public row
// field; address queries keep their own ascending-ID order.
async fn assert_history_views(wallet: &Wallet, methods: &[PaymentMethod]) -> Vec<Movement> {
	let full = wallet.history().await.unwrap();
	assert_eq!(full.iter().map(|row| row.id).collect::<BTreeSet<_>>().len(), full.len());
	let mut ordered = full.clone();
	ordered.sort_by(|a, b| b.time.created_at.cmp(&a.time.created_at).then_with(|| b.id.cmp(&a.id)));
	assert_eq!(full, ordered, "full history order changed");
	for method in methods {
		let mut expected = full.iter().filter(|row| row.sent_to(method) || row.received_on(method))
			.cloned().collect::<Vec<_>>();
		expected.sort_by_key(|row| row.id);
		let actual = wallet.history_by_payment_method(method).await.unwrap();
		assert_eq!(actual, expected, "payment-method history differs for {method:?}");
		assert!(actual.windows(2).all(|pair| pair[0].id < pair[1].id), "query repeated or reordered a movement");
	}
	full
}

fn assert_one_principal_debit(history: &[Movement], coin: VtxoId, expected_id: MovementId) {
	let rows = history.iter().filter(|row| row.input_vtxos.contains(&coin)
		&& matches!(row.subsystem.name.as_str(), "bark.exit" | "abandon.fallback_payout")).collect::<Vec<_>>();
	assert_eq!(rows.iter().map(|row| row.effective_balance.to_sat()).sum::<i64>(), -100_000);
	let debits = rows.iter().filter(|row| row.effective_balance.to_sat() != 0).collect::<Vec<_>>();
	assert_eq!(debits.len(), 1, "expected exactly one nonzero history entry");
	assert_eq!(debits[0].id, expected_id);
	assert_eq!(debits[0].effective_balance.to_sat(), -100_000);
}

// Find all outputs to this script in the new confirmed blocks.
fn canonical_script_outputs(ctx: &TestContext, after_height: u64, script: &Script) -> Vec<(OutPoint, TxOut)> {
	let core = ctx.bitcoind().sync_client();
	let mut outputs = Vec::new();
	let mut inputs = BTreeSet::new();
	for height in after_height + 1..=core.get_block_count().unwrap() {
		let block = core.get_block(&core.get_block_hash(height).unwrap()).unwrap();
		for tx in block.txdata {
			if tx.is_coinbase() { continue; }
			for input in &tx.input {
				assert!(inputs.insert(input.previous_output), "a canonical input is reused");
			}
			for (index, output) in tx.output.iter().enumerate() {
				if output.script_pubkey.as_script() == script {
					outputs.push((OutPoint::new(tx.compute_txid(), u32::try_from(index).unwrap()), output.clone()));
				}
			}
		}
	}
	outputs
}

#[tokio::test]
async fn history_projection_agrees_after_protective_exit_payout_and_reorg() {
	let ctx = TestContext::new("bark_sdk/history_protective_payout_reorg").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let owner = create_proof_wallet(ctx.bark_sdk("owner", &srv).cfg(config)
		.funded(sat(500_000)).boarded(sat(100_000))).await;
	owner.stop_daemon_wait().await.unwrap();
	let coin = owner.spendable_vtxos().await.unwrap().remove(0);
	let signed = owner.get_full_vtxo(coin.id()).await.unwrap();
	let script = owner.fallback_destination().await.unwrap().spk;
	let payout_method = PaymentMethod::Bitcoin(Address::from_script(&script, Network::Regtest).unwrap().into_unchecked());
	let scan_start = ctx.bitcoind().get_block_count().await;
	await_decision(&ctx, &srv, &coin).await;
	owner.sync_fallback_payouts().await.unwrap();
	assert_eq!(owner.balance().await.unwrap().total(), sat(100_000));
	assert_eq!(owner.protect_expiring_vtxos().await.unwrap().exits_started, vec![coin.id()]);
	let exit_id = owner.exit_mgr().get_exit_vtxo(coin.id()).await.unwrap().movement_id().unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let stored = db.get_movement_by_id(exit_id).await.unwrap();
	assert_eq!(stored.status, MovementStatus::Pending);
	assert_eq!(stored.metadata["protective_exit"], true);
	assert_eq!(stored.input_vtxos, vec![coin.id()]);
	assert_eq!(stored.sent_to.len(), 1);
	let exit_method = stored.sent_to[0].destination.clone();
	assert_ne!(exit_method, payout_method, "the fixture must distinguish native exit and fallback destinations");
	let methods = [exit_method.clone(), payout_method.clone()];
	assert!(owner.history_by_payment_method(&exit_method).await.unwrap().iter().any(|row| row.id == exit_id),
		"the address query must include the exit");
	let pending = assert_history_views(&owner, &methods).await;
	assert_one_principal_debit(&pending, coin.id(), exit_id);
	let owed = owner.balance().await.unwrap();
	assert_eq!(owed.spendable, sat(0));
	assert_eq!(owed.awaiting_settlement + owed.pending_exit, sat(100_000));
	assert_eq!(owed.total(), sat(100_000));
	assert!(pending.iter().filter(|row| row.subsystem.name == "abandon.fallback_payout")
		.all(|row| row.status == MovementStatus::Pending));

	// Stop only this fixture's watchman while the fee coins confirm. Otherwise
	// it can publish a parent that the second funding block already mines.
	let fee_address = srv.watchmand().wait_wallet_address().await;
	let watchman_dir = srv.watchmand().config().data_dir.clone();
	srv.watchmand().stop().await.unwrap();
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	let fee_tip = ctx.generate_blocks(2).await;
	assert!(canonical_script_outputs(&ctx, scan_start, &script).is_empty());
	srv.watchmand().start().await.unwrap();
	assert_eq!(srv.watchmand().config().data_dir, watchman_dir);
	srv.watchmand().wait_for_sync_height(fee_tip).await;
	// Wait for the payout transaction in the mempool before mining.
	let parent = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				let tx = ctx.bitcoind().await_transaction(txid).await;
				if tx.output.iter().any(|output| output.script_pubkey == script && output.value == sat(99_000)) {
					return tx;
				}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual fallback parent did not broadcast");
	assert!(parent.input.iter().any(|input| input.previous_output == signed.chain_anchor()));
	assert!(ctx.bitcoind().sync_client().get_mempool_entry(&parent.compute_txid()).is_ok());
	owner.sync_fallback_payouts().await.unwrap();
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.is_empty());
	assert_one_principal_debit(&assert_history_views(&owner, &methods).await, coin.id(), exit_id);
	let first_confirmation = ctx.generate_blocks(1).await;
	let core = ctx.bitcoind().sync_client();
	let confirmation_hash = core.get_block_hash(first_confirmation.into()).unwrap();
	ctx.generate_blocks(5).await;
	// Raw JSON avoids older typed RPC decoders rejecting the parent's P2A output.
	let info: serde_json::Value = core.call("getrawtransaction",
		&[serde_json::json!(parent.compute_txid()), serde_json::json!(true)]).unwrap();
	assert!(info["confirmations"].as_u64().unwrap() >= 6);
	assert_eq!(info["blockhash"], serde_json::json!(confirmation_hash));
	let outputs = canonical_script_outputs(&ctx, scan_start, &script);
	assert_eq!(outputs.len(), 1, "the declared canonical range must have exactly one customer output");
	assert_eq!(outputs[0].0.txid, parent.compute_txid());
	assert_eq!(outputs[0].1.value, sat(99_000));
	let actual = core.get_tx_out(&outputs[0].0.txid, outputs[0].0.vout, Some(true)).unwrap().unwrap();
	assert!(actual.confirmations >= 6);
	assert_eq!(actual.value, sat(99_000));
	assert_eq!(actual.script_pub_key.script().unwrap(), script);
	owner.chain().invalidate_caches().await;
	owner.sync_fallback_payouts().await.unwrap();
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.contains(&coin.id()));
	assert_eq!(owner.verify_fallback_output(outputs[0].0).await.unwrap(), sat(99_000));
	assert_eq!(db.get_movement_by_id(exit_id).await.unwrap().status, MovementStatus::Pending);
	let paid = assert_history_views(&owner, &methods).await;
	let exit = paid.iter().find(|row| row.id == exit_id).unwrap();
	assert_eq!(exit.intended_balance, stored.intended_balance);
	assert!(paid.iter().all(|row| row.subsystem.name != "abandon.fallback_payout"));

	assert_eq!(owner.balance().await.unwrap().total(), sat(0));
	let sampled_query = owner.history_by_payment_method(&exit_method).await.unwrap();
	assert_eq!(sampled_query, vec![exit.clone()]);
	drop(owner);
	let owner = Box::pin(reopen(&ctx, "owner", &srv)).await;
	assert_eq!(assert_history_views(&owner, &methods).await, paid, "reopen must preserve public rows");

	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	// Disconnect the first payout block after six confirmations. This is a
	// deep client-history reversal, not the <=3-block protocol gate.
	core.invalidate_block(&confirmation_hash).unwrap();
	owner.chain().invalidate_caches().await;
	owner.sync_fallback_payouts().await.unwrap();
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.is_empty());
	assert!(canonical_script_outputs(&ctx, scan_start, &script).is_empty());
	let reorged = assert_history_views(&owner, &methods).await;
	assert_one_principal_debit(&reorged, coin.id(), exit_id);
	assert_eq!(reorged.iter().find(|row| row.id == exit_id).unwrap().status, MovementStatus::Pending);
	let owed_again = owner.balance().await.unwrap();
	assert_eq!(owed_again.spendable, sat(0));
	assert_eq!(owed_again.awaiting_settlement + owed_again.pending_exit, sat(100_000));
	assert_eq!(owed_again.total(), sat(100_000));
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	drop(owner);
	let owner = Box::pin(reopen(&ctx, "owner", &srv)).await;
	assert_eq!(assert_history_views(&owner, &methods).await, reorged, "offline reopen must retain reversal");
	assert_one_principal_debit(&owner.history().await.unwrap(), coin.id(), exit_id);
}

#[tokio::test]
async fn history_native_and_other_device_claim_share_one_principal_debit() {
	let ctx = TestContext::new("bark_sdk/history_two_device_claim").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let observer = create_proof_wallet(ctx.bark_sdk("observer", &srv).cfg(config)
		.funded(sat(500_000)).boarded(sat(100_000))).await;
	observer.stop_daemon_wait().await.unwrap();
	let coin = observer.spendable_vtxos().await.unwrap().remove(0);
	let signed = observer.get_full_vtxo(coin.id()).await.unwrap();
	let claimant = create_proof_wallet(ctx.bark_sdk("claimant", &srv).cfg(config)
		.mnemonic(mnemonic(&ctx, "observer").await)).await;
	claimant.stop_daemon_wait().await.unwrap();
	assert_eq!(observer.balance().await.unwrap().total(), sat(100_000));
	assert_eq!(claimant.balance().await.unwrap().total(), sat(100_000));
	assert_eq!(claimant.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(observer.exit_mgr().get_exit_vtxo(coin.id()).await.is_none());
	let scan_start = ctx.bitcoind().get_block_count().await;
	let script = observer.fallback_destination().await.unwrap().spk;
	await_decision(&ctx, &srv, &coin).await;
	observer.sync_fallback_payouts().await.unwrap();
	claimant.sync_fallback_payouts().await.unwrap();
	assert_eq!(claimant.protect_expiring_vtxos().await.unwrap().exits_started, vec![coin.id()]);
	let movement = claimant.exit_mgr().get_exit_vtxo(coin.id()).await.unwrap().movement_id().unwrap();
	let claimant_db = SqliteClient::open(ctx.datadir.join("claimant/db.sqlite")).unwrap();
	let observer_db = SqliteClient::open(ctx.datadir.join("observer/db.sqlite")).unwrap();
	let native = claimant_db.get_movement_by_id(movement).await.unwrap();
	assert_eq!(native.status, MovementStatus::Pending);
	assert_eq!(native.input_vtxos, vec![coin.id()]);
	assert_eq!(native.sent_to.len(), 1);
	let native_method = native.sent_to[0].destination.clone();
	assert_one_principal_debit(&assert_history_views(&claimant, &[native_method.clone()]).await, coin.id(), movement);

	// Recovery restored the Ark coin into a fresh BDK store. Discover the
	// same seed's existing confirmed fee change before requesting CPFP.
	claimant.sync_onchain().await.unwrap();
	let fee_coins = {
		let onchain = claimant.onchain().unwrap();
		let guard = onchain.read().await;
		let wallet = (&*guard as &dyn std::any::Any).downcast_ref::<OnchainWallet>().unwrap();
		assert!(wallet.balance().confirmed > sat(0), "the restored claimant must discover existing confirmed fee funds");
		wallet.list_unspent().into_iter().filter(|coin| coin.chain_position.is_confirmed()).collect::<Vec<_>>()
	};
	for coin in fee_coins {
		let actual = ctx.bitcoind().sync_client().get_tx_out(&coin.outpoint.txid, coin.outpoint.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations > 0);
		assert_eq!(actual.value, coin.txout.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), coin.txout.script_pubkey);
	}

	// The server never receives fee liquidity. Only this device publishes the
	// signed user path and its explicitly authorized final claim.
	tokio::time::timeout(Duration::from_secs(40), async {
		loop {
			claimant.chain().invalidate_caches().await;
			claimant.sync_onchain().await.unwrap();
			claimant.exit_mgr().progress_exits_with_cpfp(&claimant, None).await.unwrap();
			if claimant.exit_mgr().list_claimable().await.iter().any(|exit| exit.id() == coin.id()) { break; }
			ctx.generate_blocks(1).await;
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("the real retained source did not become claimable");
	let claimable = claimant.exit_mgr().list_claimable().await;
	assert_eq!(claimable.len(), 1);
	assert_eq!(claimable[0].id(), coin.id());
	let destination = ctx.bitcoind().get_new_address();
	let claim = claimant.exit_mgr().drain_exits(&claimable, &claimant, destination.clone(),
		Some(bitcoin::FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert_eq!(claim.input.len(), 1);
	assert_eq!(claim.input[0].previous_output, signed.point());
	assert_eq!(claim.output.len(), 1);
	assert_eq!(claim.output[0].script_pubkey, destination.script_pubkey());
	let previous = ctx.bitcoind().await_transaction(signed.point().txid).await;
	assert_eq!(previous.output[signed.point().vout as usize].value, sat(100_000));
	let miner_fee = sat(100_000).checked_sub(claim.output[0].value).unwrap();
	assert!(miner_fee > sat(0) && claim.output[0].value > sat(0));
	assert_eq!(claim.output[0].value + miner_fee, sat(100_000));
	let core = ctx.bitcoind().sync_client();
	core.send_raw_transaction(&claim).unwrap();
	let confirmed = ctx.generate_blocks(6).await;
	let info = core.custom_get_raw_transaction_info(claim.compute_txid(), None).unwrap().unwrap();
	assert!(info.confirmations.unwrap_or(0) >= 6);
	let block = info.blockhash.unwrap();
	let height = core.get_block_header_info(&block).unwrap().height as u64;
	assert_eq!(core.get_block_hash(height).unwrap(), block);
	let output = core.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(output.confirmations >= 6);
	assert_eq!(output.value, claim.output[0].value);
	assert_eq!(output.script_pub_key.script().unwrap(), destination.script_pubkey());
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	claimant.chain().invalidate_caches().await;
	observer.chain().invalidate_caches().await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			claimant.exit_mgr().progress_exits(&claimant).await.unwrap();
			claimant.sync_fallback_payouts().await.unwrap();
			observer.sync_fallback_payouts().await.unwrap();
			let local = claimant_db.get_exit_vtxo_entry(&coin.id()).await.unwrap().unwrap();
			let snapshot = observer_db.get_fallback_snapshot().await.unwrap();
			if matches!(local.state, ExitState::Claimed(done) if done.txid == claim.compute_txid())
				&& snapshot.adopted_payout_vtxos.contains(&coin.id()) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("both genuine stores must independently reconcile the actual terminal claim");
	assert!(canonical_script_outputs(&ctx, scan_start, &script).is_empty(),
		"an unfunded fallback must not become another physical customer payment");
	let native_history = assert_history_views(&claimant, &[native_method.clone()]).await;
	assert_one_principal_debit(&native_history, coin.id(), movement);
	assert_eq!(native_history.iter().find(|row| row.id == movement).unwrap().status, MovementStatus::Successful);
	assert!(native_history.iter().all(|row| row.subsystem.name != "abandon.fallback_payout"
		|| !row.input_vtxos.contains(&coin.id())), "the local native exit already represents this principal");
	let methods = [native_method, PaymentMethod::Bitcoin(destination.into_unchecked()),
		PaymentMethod::Bitcoin(Address::from_script(&script, Network::Regtest).unwrap().into_unchecked())];
	let remote_history = assert_history_views(&observer, &methods).await;
	assert!(remote_history.iter().all(|row| row.subsystem.name != "abandon.fallback_payout"));
	assert!(matches!(observer.chain().tx_status(claim.compute_txid()).await.unwrap(), bitcoin_ext::TxStatus::Confirmed(_)));

	assert!(observer.exit_mgr().get_exit_vtxo(coin.id()).await.is_none());
	assert_eq!(observer.balance().await.unwrap().total(), sat(0));
	assert_eq!(claimant.balance().await.unwrap().total(), sat(0));
	drop(observer);
	drop(claimant);
	let observer = Box::pin(reopen(&ctx, "observer", &srv)).await;
	let claimant = Box::pin(reopen(&ctx, "claimant", &srv)).await;
	assert_eq!(assert_history_views(&observer, &methods).await, remote_history);
	assert_eq!(assert_history_views(&claimant, &methods).await, native_history);
}

#[tokio::test]
async fn fallback_adoption_fresh_seed_retains_unpaid_body_and_recovers_registered_payout() {
	let ctx = TestContext::new("bark_sdk/fallback_adoption_seed_pending").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(24);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let original = ctx.bark_sdk("original", &srv).cfg(config)
		.funded(sat(500_000)).boarded(sat(100_000)).create().await;
	let coin = original.spendable_vtxos().await.unwrap().remove(0);
	let signed = original.get_full_vtxo(coin.id()).await.unwrap();
	await_decision(&ctx, &srv, &coin).await;
	let restored = ctx.bark_sdk("restored", &srv).cfg(config)
		.mnemonic(mnemonic(&ctx, "original").await).create().await;
	let db = SqliteClient::open(ctx.datadir.join("restored/db.sqlite")).unwrap();
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(restored.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(restored.spendable_vtxos().await.unwrap().is_empty());
	assert!(restored.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.is_empty());
	assert!(restored.history().await.unwrap().iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout")
		.all(|movement| movement.effective_balance.to_sat() == 0), "fresh restore must not invent an earlier Ark debit");
	let paid = ctx.bark_sdk("paid", &srv).cfg(config)
		.mnemonic(mnemonic(&ctx, "original").await).create().await;
	srv.stop().await.unwrap();
	// Old: an authenticated customer receipt authorized this fresh seed's
	// automatic exit. New: without that API, a retained historic body alone
	// cannot establish current ownership. Keep it quarantined until payment.
	let protection = restored.protect_expiring_vtxos().await
		.expect_err("a fresh historical body has no current-ownership proof");
	let partial = protection.downcast_ref::<ExitProtectionIncomplete>().unwrap();
	assert!(partial.outcome.exits_started.is_empty());
	assert!(partial.outcome.blocked.contains(&coin.id()));
	assert!(restored.exit_mgr().get_exit_vtxo(coin.id()).await.is_none());
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert!(db.get_fallback_snapshot().await.unwrap().unproven_spent_vtxos.contains(&coin.id()));
	assert_eq!(restored.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert_eq!(restored.balance().await.unwrap().total(), sat(0), "an unknown historical body is not new principal");

	// A second independently restored database stays quarantined while the
	// first has its own protective movement. Fund the fee wallet and
	// cache the real signed payout before taking Ark offline at confirmation.
	srv.start().await.unwrap();
	drop(paid);
	drop(restored);
	let paid = reopen(&ctx, "paid", &srv).await;
	let restored = reopen(&ctx, "restored", &srv).await;
	ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let script = paid.fallback_destination().await.unwrap().spk;
	let payout = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				let tx = ctx.bitcoind().await_transaction(txid).await;
				if tx.output.iter().any(|output| output.script_pubkey == script && output.value == sat(99_000)) { return txid; }
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual payout did not broadcast after funding");
	paid.sync_fallback_payouts().await.unwrap();
	restored.sync_fallback_payouts().await.unwrap();
	assert!(restored.history().await.unwrap().iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout")
		.all(|movement| movement.effective_balance.to_sat() == 0), "a later protective holder must not invent a restore debit");
	ctx.generate_blocks(6).await;
	srv.stop().await.unwrap();
	paid.chain().invalidate_caches().await;
	paid.sync_fallback_payouts().await.unwrap();
	let paid_db = SqliteClient::open(ctx.datadir.join("paid/db.sqlite")).unwrap();
	assert_eq!(paid_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(paid_db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.contains(&coin.id()));
	let transaction = ctx.bitcoind().await_transaction(payout).await;
	assert!(transaction.output.iter().any(|output| output.script_pubkey == script && output.value == sat(99_000)));
	assert!(paid.exit_protection_schedule().await.unwrap().is_empty());
}

#[tokio::test]
async fn fallback_adoption_history_failure_quarantines_then_reconciles_an_ordinary_spend() {
	let ctx = TestContext::new("bark_sdk/fallback_adoption_history_failure").await;
	let srv = ctx.captaind("server").create().await;
	let gate = HistoryGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let owner = ctx.bark_sdk("owner", &proxy).cfg(config).boarded(sat(100_000)).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv).create().await;
	let coin = owner.spendable_vtxos().await.unwrap().remove(0);
	let signed = owner.get_full_vtxo(coin.id()).await.unwrap();
	owner.send_arkoor_payment(&recipient.new_address().await.unwrap(), coin.amount()).await.unwrap();
	owner.dangerous_drop_vtxo(coin.id()).await.unwrap();
	gate.refuse.store(true, Ordering::SeqCst);
	owner.import_vtxo(&signed, ImportVtxoArgs::default()).await.unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(owner.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	assert!(owner.exit_protection_schedule().await.unwrap().iter().any(|entry| entry.vtxo_id == coin.id()),
		"missing evidence retains the historical signed protection material");
	let watch = owner.exit_protection_report().await.unwrap();
	assert!(watch.unproven_historical.contains(&coin.id()),
		"watching a completed historic send must not authorize an automatic claim");
	let schedule = watch.schedules.iter().find(|schedule| schedule.vtxo_id == coin.id()).unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	if schedule.act_by.to_u32() > tip { ctx.generate_blocks(schedule.act_by.to_u32() - tip).await; }
	owner.chain().invalidate_caches().await;
	assert!(ctx.bitcoind().get_block_count().await >= u64::from(schedule.act_by.to_u32()));
	let refused = owner.protect_expiring_vtxos().await.expect_err("unknown historic ownership blocks automatic exit at act-by");
	let partial = refused.downcast_ref::<ExitProtectionIncomplete>().expect("structured ownership refusal");
	assert!(partial.outcome.blocked.contains(&coin.id()));
	assert!(partial.outcome.exits_started.is_empty());
	assert!(partial.report.issues.iter().any(|issue| issue.vtxo_id == Some(coin.id())
		&& issue.operation == "automatic_exit_ownership"));
	assert!(owner.exit_mgr().get_exit_vtxo(coin.id()).await.is_none());
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert!(owner.spendable_vtxos().await.unwrap().is_empty());
	let balance = owner.balance().await.unwrap();
	assert_eq!(balance.spendable, sat(0));
	assert_eq!(balance.awaiting_settlement, sat(0), "historic import cannot invent principal from a completed payment");
	assert_eq!(balance.total(), sat(0));
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.is_empty());
	gate.refuse.store(false, Ordering::SeqCst);
	owner.sync().await;
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(owner.exit_protection_schedule().await.unwrap().is_empty(), "ordinary historic outgoing is not resurrected");
	assert_eq!(owner.balance().await.unwrap().awaiting_settlement, sat(0));
}

/// Historical transfers remain restorable when their spend proof is unavailable.
#[derive(Clone, Default)]
struct OrdinaryProofGate {
	mode: Arc<AtomicUsize>,
	requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for OrdinaryProofGate {
	async fn get_vtxo_spend_proof(&self, upstream: &mut ArkClient, request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		self.requests.fetch_add(1, Ordering::SeqCst);
		if self.mode.load(Ordering::SeqCst) == 0 {
			return Err(tonic::Status::unavailable("ordinary proof deliberately unavailable"));
		}
		let mut proof = upstream.get_vtxo_spend_proof(request).await?.into_inner();
		if self.mode.load(Ordering::SeqCst) == 1 {
			assert!(proof.vtxos.len() >= 2, "genuine partial payment must have payment and all change branches");
			proof.vtxos.pop();
		}
		Ok(proof)
	}
}

// Keep this long, multi-device fixture's debug poll frame small. Construct
// repeated SDK create/sync futures on a separate frame and retain only their
// boxed pointer in the test; production stack limits and timers are unchanged.
#[inline(never)]
fn create_proof_wallet<'a>(builder: BarkSdkBuilder<'a>) -> Pin<Box<dyn Future<Output = Wallet> + 'a>> {
	Box::pin(builder.create())
}

#[inline(never)]
fn sync_proof_wallet(wallet: &Wallet) -> Pin<Box<dyn Future<Output = ()> + '_>> {
	Box::pin(wallet.sync())
}

#[tokio::test]
async fn fallback_spent_without_complete_signed_proof_retains_body_without_inventing_balance() {
	let ctx = TestContext::new("bark_sdk/fallback_ordinary_spend_proof").await;
	let srv = ctx.captaind("server").create().await;
	let source = create_proof_wallet(ctx.bark_sdk("source", &srv).cfg(config).boarded(sat(200_000))).await;
	let target = create_proof_wallet(ctx.bark_sdk("target", &srv).cfg(config)).await;
	source.send_arkoor_payment(&target.new_address().await.unwrap(), sat(100_000)).await.unwrap();
	sync_proof_wallet(&target).await;
	let original = target.spendable_vtxos().await.unwrap().remove(0);
	let signed = target.get_full_vtxo(original.id()).await.unwrap();
	let seed = mnemonic(&ctx, "target").await;
	let gate = OrdinaryProofGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	// This real second device already positively owned the input before the
	// other device paid. Its original principal must neither vanish nor be
	// counted again alongside the signed change it recovers later.
	let older = create_proof_wallet(ctx.bark_sdk("older-device", &proxy).cfg(config).mnemonic(seed.clone())).await;
	assert_eq!(older.balance().await.unwrap().total(), sat(100_000));
	target.send_arkoor_payment(&source.new_address().await.unwrap(), sat(60_000)).await.unwrap();
	assert_eq!(target.balance().await.unwrap().total(), sat(40_000));
	let intermediate = target.spendable_vtxos().await.unwrap();
	assert!(!intermediate.is_empty());
	assert_eq!(intermediate.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(40_000));
	older.recover_vtxos(std::iter::once(original.id()).chain(intermediate.iter().map(|coin| coin.id())), None).await.unwrap();
	let older_pending = older.balance().await.unwrap();
	assert_eq!(older_pending.spendable, sat(40_000));
	assert_eq!(older_pending.awaiting_settlement, sat(60_000));
	assert_eq!(older_pending.total(), sat(100_000), "old source plus its known signed change cannot become140k");
	// A real second payment creates a third generation. This device learns
	// only the newest change, so its intermediate remains locally Spendable.
	// Availability must detect that stale parent before any balance call.
	target.send_arkoor_payment(&source.new_address().await.unwrap(), sat(10_000)).await.unwrap();
	let newest = target.spendable_vtxos().await.unwrap();
	assert_eq!(newest.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(30_000));
	let current_ids = newest.iter().map(|coin| coin.id()).collect::<BTreeSet<_>>();
	let stale = intermediate.iter().filter(|coin| !current_ids.contains(&coin.id())).collect::<Vec<_>>();
	assert!(!stale.is_empty(), "the real second payment must consume a previously retained intermediate");
	let mut current_paths = Vec::new();
	for coin in &newest {
		let full = target.get_full_vtxo(coin.id()).await.unwrap();
		let path = full.ancestor_ids();
		assert!(path.contains(&original.id()), "every current change is on the actual original signed path");
		current_paths.push(path);
	}
	assert!(stale.iter().any(|coin| current_paths.iter().any(|path| path.contains(&coin.id()))),
		"actual change must exercise the stale middle ancestor, not an unrelated source");
	drop(target);
	older.recover_vtxos(newest.iter().map(|coin| coin.id()), None).await.unwrap();
	let older_db = SqliteClient::open(ctx.datadir.join("older-device/db.sqlite")).unwrap();
	for coin in &stale {
		assert_eq!(older_db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
		assert!(!older_db.get_fallback_snapshot().await.unwrap().awaiting_settlement.contains(&coin.id()));
	}
	assert_eq!(older.spendable_vtxos().await.unwrap().iter().map(|coin| coin.id()).collect::<BTreeSet<_>>(), current_ids);
	let fenced = older_db.get_fallback_snapshot().await.unwrap().awaiting_settlement;
	for coin in &stale {
		assert!(fenced.contains(&coin.id()), "inferred stale parent remains fenced after the original source resolves");
	}
	assert!(current_ids.iter().all(|id| !fenced.contains(id)), "current unchanged and new change outputs remain available");
	let transitive = older.balance().await.unwrap();
	assert_eq!(transitive.spendable, sat(30_000));
	assert_eq!(transitive.awaiting_settlement, sat(70_000));
	assert_eq!(transitive.total(), sat(100_000), "stale middle plus newest change cannot inflate principal");
	let restored = create_proof_wallet(ctx.bark_sdk("restored", &proxy).cfg(config).mnemonic(seed)).await;
	let db = SqliteClient::open(ctx.datadir.join("restored/db.sqlite")).unwrap();
	let before = gate.requests.load(Ordering::SeqCst);
	assert!(before > 0, "recovery must request actual signed-spender evidence");
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None });
	assert_eq!(restored.get_full_vtxo(original.id()).await.unwrap().serialize(), signed.serialize());
	assert!(restored.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000), "retained old input is not additional owned principal");
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.is_empty());
	assert!(restored.history().await.unwrap().iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout"));

	gate.mode.store(1, Ordering::SeqCst);
	sync_proof_wallet(&restored).await;
	assert!(gate.requests.load(Ordering::SeqCst) > before);
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Locked { holder: None },
		"one signed branch cannot retire the whole source");
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000));
	sync_proof_wallet(&older).await;
	assert_eq!(older.balance().await.unwrap().total(), sat(100_000));

	gate.mode.store(2, Ordering::SeqCst);
	sync_proof_wallet(&restored).await;
	sync_proof_wallet(&older).await;
	assert_eq!(older.balance().await.unwrap().total(), sat(30_000));
	assert!(!older.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(!restored.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert!(!db.get_fallback_snapshot().await.unwrap().quarantined_vtxos.contains(&original.id()));
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000));
	assert_eq!(restored.get_full_vtxo(original.id()).await.unwrap().serialize(), signed.serialize(),
		"verified retirement must retain signed recovery material");
	// Restore only the LOCAL checkpoint boundary before atomic retirement.
	// The signature fact is from the completed transfer above; no
	// server financial table or transaction is modified by this crash model.
	let mut interrupted = db.get_fallback_snapshot().await.unwrap();
	assert!(interrupted.verified_ordinary_spends.contains(&original.id()));
	interrupted.quarantined_vtxos.insert(original.id());
	interrupted.unproven_spent_vtxos.insert(original.id());
	interrupted.awaiting_settlement.insert(original.id());
	db.store_fallback_snapshot(&interrupted).await.unwrap();
	db.update_vtxo_state_checked(original.id(), VtxoState::Locked { holder: None },
		&[bark::vtxo::VtxoStateKind::Spent]).await.unwrap();
	srv.stop().await.unwrap();
	sync_proof_wallet(&restored).await;
	assert_eq!(db.get_wallet_vtxo(original.id()).await.unwrap().unwrap().state, VtxoState::Spent,
		"immutable signed-spend checkpoint must finish without the server");
	assert!(!restored.awaiting_settlement_vtxos().await.unwrap().contains(&original.id()));
	assert!(!restored.exit_protection_schedule().await.unwrap().iter().any(|entry| entry.vtxo_id == original.id()),
		"a crash cannot rearm an already owner-authorized ordinary transfer");
	assert_eq!(restored.balance().await.unwrap().total(), sat(30_000));
}

#[derive(Clone, Default)]
struct MissingHistoryGate {
	proof_requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for MissingHistoryGate {

	async fn get_vtxo_spend_proof(&self, _upstream: &mut ArkClient, _request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		self.proof_requests.fetch_add(1, Ordering::SeqCst);
		Err(tonic::Status::unavailable("ordinary proof deliberately unavailable"))
	}
}

/// An admission fence cannot discharge unpaid principal while history is unavailable.
#[tokio::test]
async fn fallback_locally_owned_unpaid_spent_retains_principal_with_missing_history() {
	let ctx = TestContext::new("bark_sdk/fallback_owned_missing_history").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create().await;
	let gate = MissingHistoryGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let owner = ctx.bark_sdk("owner", &proxy).cfg(config).boarded(sat(100_000)).create().await;
	let coin = owner.spendable_vtxos().await.unwrap().remove(0);
	let signed = owner.get_full_vtxo(coin.id()).await.unwrap();
	assert_eq!(owner.balance().await.unwrap().total(), sat(100_000));
	await_decision(&ctx, &srv, &coin).await;
	owner.recover_vtxos([coin.id()], None).await.unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	let snapshot = db.get_fallback_snapshot().await.unwrap();
	assert!(snapshot.adopted_payout_vtxos.is_empty(), "an unpaid source has no canonical payout");
	assert!(gate.proof_requests.load(Ordering::SeqCst) > 0);
	assert!(!snapshot.unproven_spent_vtxos.contains(&coin.id()), "known local ownership is not a fresh historic import");
	assert_eq!(owner.get_full_vtxo(coin.id()).await.unwrap().serialize(), signed.serialize());
	assert_eq!(db.get_wallet_vtxo(coin.id()).await.unwrap().unwrap().state, VtxoState::Spendable);
	assert!(owner.spendable_vtxos().await.unwrap().is_empty());
	assert!(owner.awaiting_settlement_vtxos().await.unwrap().contains(&coin.id()));
	let pending = owner.balance().await.unwrap();
	assert_eq!(pending.spendable, sat(0));
	assert_eq!(pending.awaiting_settlement, sat(100_000));
	assert_eq!(pending.total(), sat(100_000));
	assert!(owner.exit_protection_schedule().await.unwrap().iter().any(|schedule| schedule.vtxo_id == coin.id()));
	assert!(owner.history().await.unwrap().iter().all(|movement| movement.subsystem.name != "abandon.fallback_payout"));
}
