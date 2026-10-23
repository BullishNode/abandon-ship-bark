//! Recover an orphaned fallback or exit claim while the Ark server is offline.
//! The daemon restores claimable exits; the app requests any final claim.

use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

use ark::{ProtocolEncoding, VtxoId};
use ark_testing::{sat, TestContext};
use ark_testing::daemon::captaind::{self, ArkClient};
use bark::{Balance, ImportVtxoArgs, OpenWalletArgs, Wallet, WalletSeed};
use bark::exit::ExitState;
use bark::movement::MovementStatus;
use bark::onchain::{OnchainWallet, OnchainWalletTrait};
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoState;
use bitcoin::{Amount, Network, OutPoint};
use bitcoin::hashes::{Hash, sha256};
use bitcoin_ext::{BlockDelta, rpc::{BitcoinRpcExt, RpcApi}};
use server_rpc::protos;
use tokio::sync::RwLock;

#[derive(Clone, Default)]
struct HistoryGate {
	refuse: Arc<AtomicBool>,
	hits: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for HistoryGate {
	async fn get_vtxo_spend_proof(&self, upstream: &mut ArkClient, request: protos::GetVtxoStatusRequest)
		-> Result<protos::GetVtxoSpendProofResponse, tonic::Status>
	{
		if self.refuse.load(Ordering::SeqCst) {
			self.hits.fetch_add(1, Ordering::SeqCst);
			return Err(tonic::Status::unavailable("owned fixture: history unavailable after paid fallback reorg"));
		}
		Ok(upstream.get_vtxo_spend_proof(request).await?.into_inner())
	}
}

fn configure(config: &mut bark::Config) {
	// Record the confirmed fallback before the daemon can start an exit.
	config.daemon_manual_sync = true;
	config.daemon_sync_interval_secs = 1;
	config.vtxo_refresh_expiry_threshold = BlockDelta::new(4);
	config.vtxo_exit_margin = BlockDelta::new(2);
}

async fn reopen(ctx: &TestContext, mnemonic: &bip39::Mnemonic, config: bark::Config, daemon: bool)
	-> (Wallet, Arc<RwLock<OnchainWallet>>)
{
	let db = Arc::new(SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap());
	let onchain = Arc::new(RwLock::new(OnchainWallet::load_or_create(
		Network::Regtest, mnemonic.to_seed(""), db.clone()).await.unwrap()));
	let script = onchain.read().await.fallback_script();
	let wallet = Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, mnemonic), config,
		OpenWalletArgs { datadir: Some(ctx.datadir.join("owner")), persister: Some(db),
			onchain: Some(onchain.clone()), fallback_spk: Some(script), run_daemon: daemon,
			..Default::default() }).await.unwrap();
	(wallet, onchain)
}

fn principal(balance: Balance, amount: Amount) {
	assert_eq!(balance.spendable, sat(0));
	assert_eq!(balance.needs_refresh, sat(0));
	assert_eq!(balance.pending_board, sat(0));
	assert_eq!(balance.pending_in_round, sat(0));
	assert_eq!(balance.pending_arkoor_send, sat(0));
	assert_eq!(balance.pending_offboard, sat(0));
	assert_eq!(balance.pending_lightning_send, sat(0));
	assert_eq!(balance.claimable_lightning_receive, sat(0));
	assert_eq!(balance.awaiting_settlement + balance.pending_exit, amount);
	assert_eq!(balance.total(), amount);
}

async fn ordinary_stays_spent(wallet: &Wallet, db: &SqliteClient, id: VtxoId, body: &[u8]) {
	assert_eq!(db.get_wallet_vtxo(id).await.unwrap().unwrap().state, VtxoState::Spent);
	assert!(db.get_fallback_snapshot().await.unwrap().verified_ordinary_spends.contains(&id));
	assert_eq!(wallet.get_full_vtxo(id).await.unwrap().serialize().as_slice(), body);
	assert!(wallet.exit_mgr().get_exit_vtxo(id).await.is_none());
}

#[tokio::test]
async fn paid_fallback_reorg_cold_reopen_offline_protects_and_claims_exact_original() {
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run(false))).await
		.expect("paid fallback/fork/cold-reopen/default-daemon/explicit-claim must finish within 180s");
}

#[tokio::test]
async fn own_claim_reorg_cold_reopen_offline_restores_actionable_exit_or_replays_exact_claim() {
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run(true))).await
		.expect("later own-claim fork/cold-reopen/default-daemon recovery must finish within 180s");
}

#[tokio::test]
async fn own_claim_and_path_reorg_cold_reopen_offline_rebuilds_from_retained_funding_root() {
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run_with_path_fork(true, true))).await
		.expect("lost own-claim and signed path must recover from its canonical funding root within 180s");
}

#[tokio::test]
async fn own_claim_tail_reorg_cold_reopen_offline_rebuilds_from_confirmed_signed_prefix() {
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run_with_prefix_fork(true, true, true, false))).await
		.expect("lost signed tail must recover from its surviving prefix and spent funding root within 180s");
}

#[tokio::test]
async fn own_claim_tail_reorg_cold_reopen_offline_spends_exact_recovered_principal() {
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run_with_prefix_fork(true, true, true, true))).await
		.expect("recovered signed-prefix principal must actually settle to the caller's destination within 180s");
}

async fn run(later_claim_fork: bool) {
	run_with_path_fork(later_claim_fork, false).await;
}

async fn run_with_path_fork(later_claim_fork: bool, remove_claim_path: bool) {
	run_with_prefix_fork(later_claim_fork, remove_claim_path, false, false).await;
}

async fn run_with_prefix_fork(later_claim_fork: bool, remove_claim_path: bool, keep_signed_prefix: bool, settle_recovered: bool) {
	assert!(!remove_claim_path || later_claim_fork);
	assert!(!keep_signed_prefix || remove_claim_path);
	assert!(!settle_recovered || keep_signed_prefix);
	let ctx = TestContext::new(if settle_recovered {
		"bark_sdk/own_claim_prefix_settlement"
	} else if keep_signed_prefix {
		"bark_sdk/own_claim_prefix_reorg"
	} else if remove_claim_path {
		"bark_sdk/own_claim_path_reorg"
	} else if later_claim_fork {
		"bark_sdk/own_claim_reorg"
	} else { "bark_sdk/payout_reorg_recovery" }).await;
	let srv = Box::pin(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|config| {
		config.vtxo_lifetime = BlockDelta::new(48);
		config.vtxo_exit_delta = BlockDelta::new(3);
	}).watchmand().create()).await;
	let gate = HistoryGate::default();
	let proxy = srv.start_proxy_no_mailbox(gate.clone()).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let owner = Box::pin(ctx.bark_sdk("owner", &proxy).mnemonic(mnemonic.clone()).cfg(configure)
		.funded(sat(600_000)).boarded(sat(100_000)).boarded(sat(100_000)).create()).await;
	owner.stop_daemon_wait().await.unwrap();
	let recipient = Box::pin(ctx.bark_sdk("recipient", &srv).cfg(configure).create()).await;
	let original = owner.spendable_vtxos().await.unwrap();
	assert_eq!(original.len(), 2);
	owner.send_arkoor_payment(&recipient.new_address().await.unwrap(), sat(100_000)).await.unwrap();
	recipient.sync().await;
	assert_eq!(recipient.balance().await.unwrap().total(), sat(100_000));
	let remaining = owner.spendable_vtxos().await.unwrap();
	assert_eq!(remaining.len(), 1);
	let owned = owner.get_full_vtxo(remaining[0].id()).await.unwrap();
	// Retain the completed transfer's source before the self-payment creates a new coin.
	let historic = owner.get_full_vtxo(original.iter().find(|coin| coin.id() != owned.id()).unwrap().id()).await.unwrap();
	let self_send_source = keep_signed_prefix.then_some(owned.id());
	let owned = if keep_signed_prefix {
		let address = owner.new_address().await.unwrap();
		owner.send_arkoor_payment(&address, sat(100_000)).await.unwrap();
		assert!(owner.pending_arkoor_sends().await.unwrap().is_empty());
		owner.sync_mailbox().await.unwrap();
		let received = owner.spendable_vtxos().await.unwrap();
		assert_eq!(received.len(), 1, "the real self-payment must deliver one full-value recipient coin");
		let descendant = owner.get_full_vtxo(received[0].id()).await.unwrap();
		assert_ne!(descendant.id(), owned.id());
		assert_eq!(descendant.amount(), sat(100_000));
		assert_eq!(descendant.policy(), address.policy());
		assert_eq!(descendant.chain_anchor(), owned.chain_anchor());
		assert!(descendant.has_all_witnesses());
		assert!(descendant.transactions().len() >= 2, "native self-payment must supply a genuinely signed multistep path");
		assert_eq!(owner.get_vtxo_by_id(owned.id()).await.unwrap().state, VtxoState::Spent);
		descendant
	} else { owned };
	assert_ne!(historic.chain_anchor(), owned.chain_anchor());
	assert_eq!(owned.amount(), sat(100_000));
	let owned_bytes = owned.serialize();
	let historic_bytes = historic.serialize();
	for coin in [&owned, &historic] {
		coin.validate(&ctx.bitcoind().await_transaction(coin.chain_anchor().txid).await).unwrap();
	}
	// Re-import the completed transfer's source through the public wallet API.
	// The server's signed spending branch must keep this source spent.
	owner.dangerous_drop_vtxo(historic.id()).await.unwrap();
	owner.import_vtxo(&historic, ImportVtxoArgs::default()).await.unwrap();
	let db = SqliteClient::open(ctx.datadir.join("owner/db.sqlite")).unwrap();
	ordinary_stays_spent(&owner, &db, historic.id(), &historic_bytes).await;
	recipient.stop_daemon_wait().await.unwrap();
	drop(recipient);
	let mut config = owner.config().clone();
	drop(owner);
	let (owner, onchain) = Box::pin(reopen(&ctx, &mnemonic, config.clone(), false)).await;
	owner.sync_onchain().await.unwrap();
	let before_paid = onchain.read().await.list_unspent().into_iter().map(|coin| coin.outpoint).collect::<std::collections::BTreeSet<_>>();
	let baseline_onchain = owner.onchain().unwrap().read().await.balance().await;
	let registration = owner.fallback_destination().await.unwrap();
	// Separate confirmed watchman coins pay miner fees for both customer roots.
	// These coins are outside the owner's 100,000-sat recovery amount.
	for _ in 0..3 {
		let address = srv.watchmand().wait_wallet_address().await;
		ctx.bitcoind().fund_addr(address, sat(150_000)).await;
	}
	ctx.generate_blocks(2).await;
	let now = ctx.bitcoind().get_block_count().await as u32;
	assert!(now < owned.expiry_height().to_u32());
	let expired = ctx.generate_blocks(owned.expiry_height().to_u32() + 1 - now).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let (parent, child) = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let mut transactions = Vec::new();
			for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
				transactions.push(ctx.bitcoind().await_transaction(txid).await);
			}
			for parent in &transactions {
				if !parent.output.iter().any(|output| output.script_pubkey == registration.spk && output.value == sat(99_000)) { continue; }
				let anchors = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
				assert_eq!(anchors.len(), 1);
				assert_eq!(anchors[0].1.value, sat(0));
				let anchor = OutPoint::new(parent.compute_txid(), anchors[0].0 as u32);
				if let Some(child) = transactions.iter().find(|child| child.input.iter().any(|input| input.previous_output == anchor)) {
					return (parent.clone(), child.clone());
				}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("real background fallback parent and fee child did not reach Core");
	let payout = OutPoint::new(parent.compute_txid(), parent.output.iter().position(|output|
		output.script_pubkey == registration.spk && output.value == sat(99_000)).unwrap() as u32);
	let mut parent_in = sat(0);
	for input in &parent.input {
		parent_in += ctx.bitcoind().await_transaction(input.previous_output.txid).await.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(parent_in, parent.output.iter().map(|output| output.value).sum::<Amount>());
	let mut child_in = sat(0);
	for input in &child.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		child_in += previous.output[input.previous_output.vout as usize].value;
	}
	let child_fee = child_in.checked_sub(child.output.iter().map(|output| output.value).sum::<Amount>()).unwrap();
	assert!(child_fee > sat(0));
	let paid_tip = ctx.generate_blocks(6).await;
	let core = ctx.bitcoind().sync_client();
	// Read raw Core JSON because the typed RPC decoder does not recognize P2A outputs.
	let parent_info: serde_json::Value = core.call("getrawtransaction", &[serde_json::json!(parent.compute_txid()), serde_json::json!(true)]).unwrap();
	let child_info: serde_json::Value = core.call("getrawtransaction", &[serde_json::json!(child.compute_txid()), serde_json::json!(true)]).unwrap();
	assert!(parent_info["confirmations"].as_u64().unwrap() >= 6);
	assert!(child_info["confirmations"].as_u64().unwrap() >= 6);
	assert_eq!(parent_info["blockhash"], child_info["blockhash"]);
	let paid_block: bitcoin::BlockHash = parent_info["blockhash"].as_str().unwrap().parse().unwrap();
	let paid_height = core.get_block_header_info(&paid_block).unwrap().height as u64;
	assert_eq!(core.get_block_hash(paid_height).unwrap(), paid_block);
	// Wait for the server's history view to rebuild after chain confirmation.
	// This setup phase syncs the wallet explicitly; recovery below uses its daemon.
	srv.wait_for_sync_height(paid_tip).await;
	srv.watchmand().wait_for_sync_height(paid_tip).await;
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			owner.chain().invalidate_caches().await;
			if owner.sync_fallback_payouts().await.is_ok()
				&& db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.contains(&owned.id()) { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual coherent history and independently verified payout adoption must precede the fork");
	owner.sync_onchain().await.unwrap();
	let paid_snapshot = db.get_fallback_snapshot().await.unwrap();
	assert_eq!(owned.amount(), sat(100_000));
	assert_eq!(owner.verify_fallback_output(payout).await.unwrap(), sat(99_000));
	assert_eq!(owned.amount() - sat(99_000), sat(1_000));

	assert!(paid_snapshot.adopted_payout_vtxos.contains(&owned.id()));
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	principal(owner.balance().await.unwrap(), sat(0));
	assert_eq!(owner.onchain().unwrap().read().await.balance().await, baseline_onchain + sat(99_000));
	let paid_cut = onchain.read().await.list_unspent().into_iter().map(|coin| coin.outpoint).collect::<std::collections::BTreeSet<_>>();
	assert_eq!(paid_cut.difference(&before_paid).copied().collect::<Vec<_>>(), vec![payout]);
	let paid_history = owner.history().await.unwrap();
	assert!(paid_history.iter().all(|entry| entry.subsystem.name != "abandon.fallback_payout"));

	assert!(owner.exit_mgr().get_exit_vtxo_ids().await.is_empty());
	ordinary_stays_spent(&owner, &db, historic.id(), &historic_bytes).await;
	drop(onchain);
	drop(owner);
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	// Check that stop() closes the server listener before reopening the wallet.
	let stopped_listener = ("127.0.0.1", srv.rpc_port());
	let stopped_error = tokio::time::timeout(Duration::from_secs(2),
		tokio::net::TcpStream::connect(stopped_listener)).await.unwrap().expect_err("stopped Ark listener must refuse connection");
	assert_eq!(stopped_error.kind(), std::io::ErrorKind::ConnectionRefused);
	gate.refuse.store(true, Ordering::SeqCst);
	let alternate_miner = ctx.bitcoind().get_new_address();
	let destination = ctx.bitcoind().get_new_address();
	// Select the later app claim's destination before restarting Core.
	let settlement_destination = settle_recovered.then(|| ctx.bitcoind().get_new_address());
	// Core's test wallet is not loaded automatically after restart.
	let claim_fork_miner = later_claim_fork.then(|| ctx.bitcoind().get_new_address());
	core.invalidate_block(&paid_block).unwrap();
	let fork_height = core.get_block_count().unwrap();
	assert_eq!(fork_height + 1, paid_height);
	ctx.bitcoind().stop().await.unwrap();
	// Move mempool.dat aside after stopping Core, keeping its bytes for comparison.
	let cache = ctx.bitcoind().datadir().join("regtest/mempool.dat");
	let preserved = ctx.bitcoind().datadir().join("paid-reorg-original.mempool.dat");
	let cache_bytes = std::fs::read(&cache).unwrap();
	let cache_sha = sha256::Hash::hash(&cache_bytes);
	std::fs::rename(&cache, &preserved).unwrap();
	assert_eq!(sha256::Hash::hash(&std::fs::read(&preserved).unwrap()), cache_sha);
	ctx.bitcoind().start().await.unwrap();
	let core = ctx.bitcoind().sync_client();
	assert_eq!(core.get_block_count().unwrap(), fork_height);
	for transaction in [&parent, &child] {
		assert!(core.get_mempool_entry(&transaction.compute_txid()).is_err());
	}
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	let original_anchor = core.get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().unwrap();
	assert!(original_anchor.confirmations > 0);
	let original_funding = ctx.bitcoind().await_transaction(owned.chain_anchor().txid).await;
	let actual_anchor = &original_funding.output[owned.chain_anchor().vout as usize];
	assert_eq!(original_anchor.value, actual_anchor.value);
	assert_eq!(original_anchor.script_pub_key.script().unwrap(), actual_anchor.script_pubkey);
	// The retained SQLite store still records the orphaned payout as paid and the coin as spent.
	assert!(db.get_fallback_snapshot().await.unwrap().adopted_payout_vtxos.contains(&owned.id()));
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	config.daemon_manual_sync = false;
	let (wallet, onchain) = Box::pin(reopen(&ctx, &mnemonic, config.clone(), true)).await;
	// After reopen, only the default daemon and block production advance recovery.
	let started = Instant::now();
	let mut last_block = Instant::now();
	loop {
		ordinary_stays_spent(&wallet, &db, historic.id(), &historic_bytes).await;
		let snapshot = db.get_fallback_snapshot().await.unwrap();
		if snapshot.adopted_payout_vtxos.is_empty() {
			principal(wallet.balance().await.unwrap(), sat(100_000));
			assert!(snapshot.awaiting_settlement.contains(&owned.id()));
			assert_eq!(wallet.get_full_vtxo(owned.id()).await.unwrap().serialize(), owned_bytes);
			assert!(wallet.history().await.unwrap().iter().filter(|entry| entry.subsystem.name == "abandon.fallback_payout")
				.all(|entry| entry.status != MovementStatus::Successful));
		}
		if wallet.exit_mgr().list_claimable().await.iter().any(|entry| entry.id() == owned.id()) { break; }
		assert!(started.elapsed() < Duration::from_secs(35), "cold cached Spent must rearm and reach actual Claimable with Ark offline");
		if last_block.elapsed() >= Duration::from_secs(1) {
			core.generate_to_address(1, &alternate_miner).unwrap();
			last_block = Instant::now();
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	// Wallet startup can fail to reach the stopped server before requesting history.
	// The proxy counter only counts requests that reach the history endpoint.
	let offline = db.get_fallback_snapshot().await.unwrap();
	assert!(offline.adopted_payout_vtxos.is_empty());
	let still_stopped = tokio::time::timeout(Duration::from_secs(2),
		tokio::net::TcpStream::connect(stopped_listener)).await.unwrap().expect_err("Ark must remain stopped through Claimable");
	assert_eq!(still_stopped.kind(), std::io::ErrorKind::ConnectionRefused);
	assert!(!onchain.read().await.list_unspent().iter().any(|coin| coin.outpoint == payout),
		"vanished inbound payment must not remain in the onchain wallet's current UTXO cut");
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	principal(wallet.balance().await.unwrap(), sat(100_000));
	let claimable = wallet.exit_mgr().list_claimable().await;
	assert_eq!(claimable.len(), 1);
	assert_eq!(claimable[0].id(), owned.id());
	let movement = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap().movement_id.unwrap();
	let claim = wallet.exit_mgr().drain_exits(&claimable, &wallet, destination.clone(),
		Some(bitcoin::FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert_eq!(claim.input.len(), 1);
	assert_eq!(claim.input[0].previous_output, owned.point());
	assert_eq!(claim.output.len(), 1);
	assert_eq!(claim.output[0].script_pubkey, destination.script_pubkey());
	let previous = ctx.bitcoind().await_transaction(owned.point().txid).await;
	assert_eq!(previous.output[owned.point().vout as usize].value, sat(100_000));
	let claim_fee = sat(100_000).checked_sub(claim.output[0].value).unwrap();
	assert!(claim_fee > sat(0) && claim.output[0].value > sat(0));
	core.send_raw_transaction(&claim).unwrap();
	core.generate_to_address(6, &alternate_miner).unwrap();
	let info = core.custom_get_raw_transaction_info(claim.compute_txid(), None).unwrap().unwrap();
	assert!(info.confirmations.unwrap_or(0) >= 6);
	let final_block = info.blockhash.unwrap();
	let final_height = core.get_block_header_info(&final_block).unwrap().height as u64;
	assert_eq!(core.get_block_hash(final_height).unwrap(), final_block);
	let received = core.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(received.confirmations >= 6);
	assert_eq!(received.value, claim.output[0].value);
	assert_eq!(received.script_pub_key.script().unwrap(), destination.script_pubkey());
	assert_eq!(received.value + claim_fee, sat(100_000));
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let exit = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap();
			if matches!(exit.state, ExitState::Claimed(done) if done.txid == claim.compute_txid()
				&& done.block.hash == final_block && u64::from(done.block.height.to_u32()) == final_height)
				&& exit.movement_id == Some(movement)
				&& db.get_movement_by_id(movement).await.unwrap().status == MovementStatus::Successful
				&& wallet.balance().await.unwrap().total() == sat(0) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("default daemon must reconcile the actual canonical own claim");
	wallet.stop_daemon_wait().await.unwrap();
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, VtxoState::Exited);
	principal(wallet.balance().await.unwrap(), sat(0));
	let local_cut = onchain.read().await.list_unspent();
	let mut canonical_local = sat(0);
	for coin in &local_cut {
		assert_ne!(coin.outpoint, payout);
		let actual = core.get_tx_out(&coin.outpoint.txid, coin.outpoint.vout, Some(true)).unwrap().unwrap();
		assert_eq!(actual.value, coin.txout.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), coin.txout.script_pubkey);
		canonical_local += actual.value;
	}
	assert_eq!(wallet.onchain().unwrap().read().await.balance().await, canonical_local,
		"every displayed remaining onchain sat must exist in the actual current Core cut");
	ordinary_stays_spent(&wallet, &db, historic.id(), &historic_bytes).await;
	assert_eq!(wallet.get_full_vtxo(owned.id()).await.unwrap().serialize(), owned_bytes);
	let history = wallet.history().await.unwrap();
	let own_exit = history.iter().find(|entry| entry.id == movement).unwrap();
	assert_eq!(own_exit.status, MovementStatus::Successful);
	assert_eq!(own_exit.effective_balance.to_sat(), -100_000);
	assert_eq!(history.iter().filter(|entry| matches!(entry.subsystem.name.as_str(), "bark.exit" | "abandon.fallback_payout"))
		.map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), -100_000);
	assert_eq!(history.iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0);
	assert!(history.iter().filter(|entry| entry.subsystem.name == "abandon.fallback_payout")
		.all(|entry| entry.status != MovementStatus::Successful || (
			entry.metadata["fallback"]["disposition"] == "exited"
			&& entry.metadata["fallback"]["txid"] == serde_json::json!(claim.compute_txid())
			&& entry.effective_balance.to_sat() == 0 && entry.offchain_fee == sat(0))),
		"only the actual own claim may supply a successful terminal receipt; the orphaned payout fee was never paid");
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	assert!(core.get_mempool_entry(&child.compute_txid()).is_err());
	if !later_claim_fork { return; }

	// The preceding claim is confirmed on the current chain and the Ark balance is zero.
	// Keep the claim out of the on-chain wallet's cache so recovery cannot pass by replaying it.
	assert!(!OnchainWalletTrait::is_mine(&*onchain.read().await, &claim.output[0].script_pubkey).await.unwrap());
	assert!(onchain.read().await.list_transactions().iter().all(|tx| tx.compute_txid() != claim.compute_txid()));
	let claimed_snapshot = db.get_fallback_snapshot().await.unwrap();
	assert!(matches!(wallet.chain().tx_status(claim.compute_txid()).await.unwrap(),
		bitcoin_ext::TxStatus::Confirmed(block) if block.hash == final_block));

	assert!(claimed_snapshot.terminal_observations.values().flatten().any(|proof| proof.transaction == claim));
	let second_miner = claim_fork_miner.unwrap();
	assert_ne!(second_miner.script_pubkey(), alternate_miner.script_pubkey());
	// This variant disconnects the published signed exit path while retaining
	// its confirmed funding root and the completed transfer's root.
	let signed_path = owned.transactions().collect::<Vec<_>>();
	let retained_path_steps = if keep_signed_prefix { 1 } else { 0 };
	let retained_frontier = if keep_signed_prefix {
		assert!(signed_path.len() >= 2);
		let mut previous_point = owned.chain_anchor();
		for item in &signed_path {
			assert!(item.tx.input.iter().any(|input| input.previous_output == previous_point),
				"every signed step must spend the actual preceding relevant outpoint");
			previous_point = OutPoint::new(item.tx.compute_txid(), item.output_idx as u32);
		}
		assert_eq!(previous_point, owned.point());
		let first = &signed_path[0];
		assert_eq!(first.tx.output[first.output_idx].value, sat(100_000));
		Some(OutPoint::new(first.tx.compute_txid(), first.output_idx as u32))
	} else { None };
	let mut retained_path_blocks = Vec::new();
	let mut removed_path_blocks = Vec::new();
	let second_fork_block = if remove_claim_path {
		assert_ne!(owned.point(), owned.chain_anchor());
		assert!(!signed_path.is_empty());
		assert_eq!(signed_path.last().unwrap().tx.compute_txid(), owned.point().txid);
		owned.validate(&original_funding).unwrap();
		let root_info = core.custom_get_raw_transaction_info(owned.chain_anchor().txid, None).unwrap().unwrap();
		assert!(root_info.confirmations.unwrap_or(0) >= 6);
		let root_block = root_info.blockhash.unwrap();
		let root_height = core.get_block_header_info(&root_block).unwrap().height as u64;
		assert_eq!(core.get_block_hash(root_height).unwrap(), root_block);
		let ordinary_root = core.custom_get_raw_transaction_info(historic.chain_anchor().txid, None).unwrap().unwrap();
		let ordinary_root_block = ordinary_root.blockhash.unwrap();
		let ordinary_root_height = core.get_block_header_info(&ordinary_root_block).unwrap().height as u64;
		assert_eq!(core.get_block_hash(ordinary_root_height).unwrap(), ordinary_root_block);
		let indexed = onchain.read().await.list_transactions();
		for (index, item) in signed_path.iter().enumerate() {
			assert!(!indexed.iter().any(|tx| tx.compute_txid() == item.tx.compute_txid()),
				"BDK must not independently replay the missing signed path");
			let actual = ctx.bitcoind().await_transaction(item.tx.compute_txid()).await;
			assert_eq!(actual, item.tx);
			let info = core.custom_get_raw_transaction_info(actual.compute_txid(), None).unwrap().unwrap();
			assert!(info.confirmations.unwrap_or(0) >= 6);
			let block = info.blockhash.unwrap();
			let height = core.get_block_header_info(&block).unwrap().height as u64;
			assert_eq!(core.get_block_hash(height).unwrap(), block);
			assert!(height > root_height && height > ordinary_root_height);
			if index < retained_path_steps { retained_path_blocks.push((height, block)); }
			else { removed_path_blocks.push((height, block)); }
		}
		if keep_signed_prefix {
			let first_removed = removed_path_blocks.iter().map(|(height, _)| *height).min().unwrap();
			assert!(retained_path_blocks.iter().all(|(height, _)| *height < first_removed),
				"the actual signed prefix must remain below the tail fork");
			assert!(core.get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().is_none());
		}
		removed_path_blocks.iter().min_by_key(|(height, _)| *height).unwrap().1
	} else { final_block };
	drop(onchain);
	drop(wallet);
	core.invalidate_block(&second_fork_block).unwrap();
	let claim_fork_height = core.get_block_count().unwrap();
	if remove_claim_path {
		assert_eq!(claim_fork_height + 1, removed_path_blocks.iter().map(|(height, _)| *height).min().unwrap());
	} else { assert_eq!(claim_fork_height + 1, final_height); }
	ctx.bitcoind().stop().await.unwrap();
	let claim_cache = ctx.bitcoind().datadir().join("regtest/mempool.dat");
	let claim_preserved = ctx.bitcoind().datadir().join("own-claim-reorg-original.mempool.dat");
	let claim_cache_sha = sha256::Hash::hash(&std::fs::read(&claim_cache).unwrap());
	std::fs::rename(&claim_cache, &claim_preserved).unwrap();
	assert_eq!(sha256::Hash::hash(&std::fs::read(&claim_preserved).unwrap()), claim_cache_sha);
	ctx.bitcoind().start().await.unwrap();
	let core = ctx.bitcoind().sync_client();
	assert_eq!(core.get_block_count().unwrap(), claim_fork_height);
	assert_eq!(core.call::<serde_json::Value>("listwallets", &[]).unwrap(), serde_json::json!([]));
	for txid in [claim.compute_txid(), parent.compute_txid(), child.compute_txid()] {
		assert!(core.get_mempool_entry(&txid).is_err());
	}
	assert!(core.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().is_none());
	if remove_claim_path {
		for item in signed_path.iter().skip(retained_path_steps) {
			assert!(core.get_mempool_entry(&item.tx.compute_txid()).is_err());
			assert!(core.get_tx_out(&item.tx.compute_txid(), item.output_idx as u32, Some(true)).unwrap().is_none());
		}
		if let Some(frontier) = retained_frontier {
			assert!(core.get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().is_none(),
				"the genuine surviving prefix, rather than an untouched root, must back this recovery");
			for (item, (height, block)) in signed_path.iter().take(retained_path_steps).zip(&retained_path_blocks) {
				assert_eq!(core.get_block_hash(*height).unwrap(), *block);
				assert_eq!(core.get_raw_transaction(&item.tx.compute_txid(), None).unwrap(), item.tx);
				assert!(item.tx.input.iter().any(|input| input.previous_output == owned.chain_anchor()));
			}
			let actual = core.get_tx_out(&frontier.txid, frontier.vout, Some(true)).unwrap().unwrap();
			assert!(actual.confirmations > 0);
			assert_eq!(actual.value, sat(100_000));
			assert_eq!(actual.script_pub_key.script().unwrap(), signed_path[0].tx.output[frontier.vout as usize].script_pubkey);
		} else {
			let root = core.get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().unwrap();
			assert!(root.confirmations >= 6);
			assert_eq!(root.value, actual_anchor.value);
			assert_eq!(root.script_pub_key.script().unwrap(), actual_anchor.script_pubkey);
		}
		owned.validate(&original_funding).unwrap();
	} else {
		let reopened_leaf = core.get_tx_out(&owned.point().txid, owned.point().vout, Some(true)).unwrap().unwrap();
		assert!(reopened_leaf.confirmations > 0);
		assert_eq!(reopened_leaf.value, sat(100_000));
		assert_eq!(reopened_leaf.script_pub_key.script().unwrap(), previous.output[owned.point().vout as usize].script_pubkey);
	}
	// Mine the replacement branch to another coinbase destination.
	// Keep wallets and claim broadcasters stopped during mining.
	let replacement_blocks = if remove_claim_path {
		final_height - claim_fork_height + 5
	} else { 6 };
	core.generate_to_address(replacement_blocks, &second_miner).unwrap();
	assert_ne!(core.get_block_hash(final_height).unwrap(), final_block);
	assert!(core.get_mempool_entry(&claim.compute_txid()).is_err());
	if remove_claim_path {
		assert!(core.get_tx_out(&owned.point().txid, owned.point().vout, Some(true)).unwrap().is_none());
		for item in signed_path.iter().skip(retained_path_steps) { assert!(core.get_mempool_entry(&item.tx.compute_txid()).is_err()); }
		if let Some(frontier) = retained_frontier {
			assert!(core.get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().is_none());
			for (height, block) in &retained_path_blocks { assert_eq!(core.get_block_hash(*height).unwrap(), *block); }
			let actual = core.get_tx_out(&frontier.txid, frontier.vout, Some(true)).unwrap().unwrap();
			assert!(actual.confirmations >= 6);
			assert_eq!(actual.value, sat(100_000));
			assert_eq!(actual.script_pub_key.script().unwrap(), signed_path[0].tx.output[frontier.vout as usize].script_pubkey);
		} else { assert!(core.get_tx_out(&owned.chain_anchor().txid, owned.chain_anchor().vout, Some(true)).unwrap().is_some()); }
	} else { assert!(core.get_tx_out(&owned.point().txid, owned.point().vout, Some(true)).unwrap().is_some()); }
	assert!(matches!(db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap().state,
		ExitState::Claimed(done) if done.txid == claim.compute_txid() && done.block.hash == final_block));
	assert_eq!(db.get_movement_by_id(movement).await.unwrap().status, MovementStatus::Successful);
	let (reopened, onchain) = Box::pin(reopen(&ctx, &mnemonic, config, true)).await;
	let started = Instant::now();
	let mut last_block = Instant::now();
	let recovered_by;
	loop {
		ordinary_stays_spent(&reopened, &db, historic.id(), &historic_bytes).await;
		if let Some(id) = self_send_source {
			assert_eq!(db.get_wallet_vtxo(id).await.unwrap().unwrap().state, VtxoState::Spent);
			assert!(reopened.exit_mgr().get_exit_vtxo(id).await.is_none(), "a consumed self-send ancestor must not acquire a second principal exit");
			let indexed = onchain.read().await.list_transactions();
			for item in signed_path.iter().skip(retained_path_steps) {
				assert!(!indexed.iter().any(|tx| tx.compute_txid() == item.tx.compute_txid()),
					"BDK must not independently supply the missing signed tail");
			}
		}
		assert_eq!(reopened.get_full_vtxo(owned.id()).await.unwrap().serialize(), owned_bytes);
		assert!(onchain.read().await.list_transactions().iter().all(|tx| tx.compute_txid() != claim.compute_txid()));
		assert!(!onchain.read().await.list_unspent().iter().any(|coin| coin.outpoint == payout
			|| coin.outpoint == OutPoint::new(claim.compute_txid(), 0)));
		let actual = core.custom_get_raw_transaction_info(claim.compute_txid(), None).unwrap();
		let canonical_claim = actual.as_ref().filter(|info| info.confirmations.unwrap_or(0) >= 6)
			.and_then(|info| info.blockhash).filter(|block| {
				let height = core.get_block_header_info(block).unwrap().height as u64;
				core.get_block_hash(height).unwrap() == *block
			});
		let snapshot = db.get_fallback_snapshot().await.unwrap();
		let balance = reopened.balance().await.unwrap();
		let history = reopened.history().await.unwrap();
		let own_history = history.iter().filter(|entry| entry.input_vtxos.contains(&owned.id())
			&& matches!(entry.subsystem.name.as_str(), "bark.exit" | "abandon.fallback_payout")).collect::<Vec<_>>();
		assert_eq!(own_history.iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), -100_000,
			"reorg recovery must retain one principal debit/reservation, never duplicate it");
		assert_eq!(history.iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0);
		assert!(snapshot.terminal_observations.values().flatten().any(|proof| proof.transaction == claim));
		let exit = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap();
		if remove_claim_path {
			assert_eq!(exit.movement_id, Some(movement));
			let debits = own_history.iter().filter(|entry| entry.effective_balance.to_sat() == -100_000).collect::<Vec<_>>();
			assert_eq!(debits.len(), 1, "rebuilding a missing path must retain exactly one principal debit");
			assert_eq!(debits[0].id, movement);
		}
		let actionable = reopened.exit_mgr().list_claimable().await;
		let no_current_discharge = snapshot.adopted_payout_vtxos.is_empty();
		let truthful_pending = balance.total() == sat(100_000)
			&& own_history.iter().all(|entry| entry.status != MovementStatus::Successful);
		let actual_leaf = core.get_tx_out(&owned.point().txid, owned.point().vout, Some(true)).unwrap();
		let actionable_leaf = actual_leaf.as_ref().is_some_and(|leaf|
			leaf.confirmations >= owned.exit_delta().to_u32() && leaf.value == sat(100_000)
			&& leaf.script_pub_key.script().unwrap() == previous.output[owned.point().vout as usize].script_pubkey);
		if canonical_claim.is_none() && no_current_discharge && truthful_pending && actionable_leaf
			&& actionable.len() == 1 && actionable[0].id() == owned.id()
			&& matches!(exit.state, ExitState::Claimable(_))
		{
			principal(balance, sat(100_000));
			recovered_by = "actionable_exact_original";
			break;
		}
		if let Some(block) = canonical_claim {
			let height = core.get_block_header_info(&block).unwrap().height as u64;
			if matches!(exit.state, ExitState::Claimed(ref done) if done.txid == claim.compute_txid()
				&& done.block.hash == block && u64::from(done.block.height.to_u32()) == height)
				&& exit.movement_id == Some(movement)
				&& db.get_movement_by_id(movement).await.unwrap().status == MovementStatus::Successful
				&& balance.total() == sat(0)
			{
				assert_eq!(ctx.bitcoind().await_transaction(claim.compute_txid()).await, claim);
				let output = core.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap();
				assert!(output.confirmations >= 6);
				assert_eq!(output.value, claim.output[0].value);
				assert_eq!(output.script_pub_key.script().unwrap(), destination.script_pubkey());
				assert_eq!(output.value + claim_fee, sat(100_000));
				principal(balance, sat(0));
				recovered_by = "exact_authorized_raw_autonomous_reconfirmation";
				break;
			}
		}
		assert!(started.elapsed() < Duration::from_secs(35),
			"orphaned own Claimed must regain an actionable rightful exit or autonomously reconfirm exact authorized raw, with truthful balance/history");
		if last_block.elapsed() >= Duration::from_secs(1) {
			core.generate_to_address(1, &second_miner).unwrap();
			last_block = Instant::now();
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	// The settlement variant keeps the recovered wallet's default daemon running.
	// Its balance and history must observe the app's later explicit claim.
	if !settle_recovered { reopened.stop_daemon_wait().await.unwrap(); }
	let still_stopped = tokio::time::timeout(Duration::from_secs(2),
		tokio::net::TcpStream::connect(stopped_listener)).await.unwrap().expect_err("Ark must remain stopped after the own-claim fork");
	assert_eq!(still_stopped.kind(), std::io::ErrorKind::ConnectionRefused);
	ordinary_stays_spent(&reopened, &db, historic.id(), &historic_bytes).await;
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	assert!(core.get_mempool_entry(&child.compute_txid()).is_err());
	let mut second_canonical_local = sat(0);
	for coin in onchain.read().await.list_unspent() {
		let actual = core.get_tx_out(&coin.outpoint.txid, coin.outpoint.vout, Some(true)).unwrap().unwrap();
		assert_eq!(actual.value, coin.txout.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), coin.txout.script_pubkey);
		second_canonical_local += actual.value;
	}
	assert_eq!(reopened.onchain().unwrap().read().await.balance().await, second_canonical_local);
	if remove_claim_path {
		// Disconnecting an exit step also disconnects its fee child. Its unspent
		// fee inputs and rebuilt change may differ, so verify each listed outpoint.
		let mut customer_claims = Vec::new();
		for height in claim_fork_height + 1..=core.get_block_count().unwrap() {
			let block = core.get_block(&core.get_block_hash(height).unwrap()).unwrap();
			for tx in block.txdata {
				for (vout, output) in tx.output.iter().enumerate() {
					assert_ne!(output.script_pubkey, registration.spk, "orphaned fallback must not pay in addition to native recovery");
					if output.script_pubkey == destination.script_pubkey() {
						customer_claims.push((OutPoint::new(tx.compute_txid(), vout as u32), output.value));
					}
				}
			}
		}
		if recovered_by == "exact_authorized_raw_autonomous_reconfirmation" {
			assert_eq!(customer_claims, vec![(OutPoint::new(claim.compute_txid(), 0), claim.output[0].value)]);
		} else { assert!(customer_claims.is_empty()); }
	} else {
		assert_eq!(second_canonical_local, canonical_local, "reorg of an external claim cannot create or consume BDK principal");
	}
	if !settle_recovered { return; }

	// After the daemon restores a claimable exit, request a new claim through
	// the public signing and broadcast API used by `bark exit claim`.
	assert_eq!(recovered_by, "actionable_exact_original", "this variant requires an actually recovered unspent entitlement before the app spends it");
	let settlement_destination = settlement_destination.unwrap();
	assert_ne!(settlement_destination.script_pubkey(), destination.script_pubkey());
	assert!(!OnchainWalletTrait::is_mine(&*onchain.read().await, &settlement_destination.script_pubkey()).await.unwrap());
	let recovered_claimable = reopened.exit_mgr().list_claimable().await;
	assert_eq!(recovered_claimable.len(), 1);
	assert_eq!(recovered_claimable[0].id(), owned.id());
	principal(reopened.balance().await.unwrap(), sat(100_000));
	let recovered_leaf = core.get_tx_out(&owned.point().txid, owned.point().vout, Some(true)).unwrap().unwrap();
	assert!(recovered_leaf.confirmations >= owned.exit_delta().to_u32());
	assert_eq!(recovered_leaf.value, sat(100_000));
	assert_eq!(recovered_leaf.script_pub_key.script().unwrap(), previous.output[owned.point().vout as usize].script_pubkey);
	let settled_claim = reopened.exit_mgr().drain_exits(&recovered_claimable, &reopened, settlement_destination.clone(),
		Some(bitcoin::FeeRate::from_sat_per_vb(2).unwrap())).await.unwrap().extract_tx().unwrap();
	assert_ne!(settled_claim.compute_txid(), claim.compute_txid(), "a new app claim must not replay the orphaned transaction");
	assert_eq!(settled_claim.input.len(), 1);
	assert_eq!(settled_claim.input[0].previous_output, owned.point());
	assert_eq!(settled_claim.output.len(), 1);
	assert_eq!(settled_claim.output[0].script_pubkey, settlement_destination.script_pubkey());
	let settled_fee = recovered_leaf.value.checked_sub(settled_claim.output[0].value).unwrap();
	assert!(settled_fee > sat(0) && settled_claim.output[0].value > sat(0));
	reopened.chain().broadcast_tx(&settled_claim).await.unwrap();
	core.generate_to_address(6, &second_miner).unwrap();
	assert_eq!(ctx.bitcoind().await_transaction(settled_claim.compute_txid()).await, settled_claim);
	let settled_info = core.custom_get_raw_transaction_info(settled_claim.compute_txid(), None).unwrap().unwrap();
	assert!(settled_info.confirmations.unwrap_or(0) >= 6);
	let settled_block = settled_info.blockhash.unwrap();
	let settled_height = core.get_block_header_info(&settled_block).unwrap().height as u64;
	assert_eq!(core.get_block_hash(settled_height).unwrap(), settled_block);
	let settled_outpoint = OutPoint::new(settled_claim.compute_txid(), 0);
	let delivered = core.get_tx_out(&settled_outpoint.txid, settled_outpoint.vout, Some(true)).unwrap().unwrap();
	assert!(delivered.confirmations >= 6);
	assert_eq!(delivered.value, settled_claim.output[0].value);
	assert_eq!(delivered.script_pub_key.script().unwrap(), settlement_destination.script_pubkey());
	assert_eq!(delivered.value + settled_fee, recovered_leaf.value);
	assert!(core.get_tx_out(&owned.point().txid, owned.point().vout, Some(true)).unwrap().is_none());
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			ordinary_stays_spent(&reopened, &db, historic.id(), &historic_bytes).await;
			let ancestor = self_send_source.unwrap();
			assert_eq!(db.get_wallet_vtxo(ancestor).await.unwrap().unwrap().state, VtxoState::Spent);
			assert!(reopened.exit_mgr().get_exit_vtxo(ancestor).await.is_none());
			let exit = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap();
			if matches!(exit.state, ExitState::Claimed(done) if done.txid == settled_claim.compute_txid()
				&& done.block.hash == settled_block && u64::from(done.block.height.to_u32()) == settled_height)
				&& exit.movement_id == Some(movement)
				&& db.get_movement_by_id(movement).await.unwrap().status == MovementStatus::Successful
				&& reopened.balance().await.unwrap().total() == sat(0) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("default daemon must reconcile actual settlement of the recovered principal on its original movement");
	reopened.stop_daemon_wait().await.unwrap();
	principal(reopened.balance().await.unwrap(), sat(0));
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, VtxoState::Exited);
	assert!(reopened.exit_mgr().list_claimable().await.is_empty());
	assert_eq!(reopened.get_full_vtxo(owned.id()).await.unwrap().serialize(), owned_bytes);
	let settled_snapshot = db.get_fallback_snapshot().await.unwrap();
	assert!(settled_snapshot.terminal_observations.values().flatten().any(|backup| backup.transaction == settled_claim));
	assert!(matches!(reopened.chain().tx_status(settled_claim.compute_txid()).await.unwrap(),
		bitcoin_ext::TxStatus::Confirmed(block) if block.hash == settled_block));
	assert!(!matches!(reopened.chain().tx_status(claim.compute_txid()).await.unwrap(), bitcoin_ext::TxStatus::Confirmed(_)));

	let settled_history = reopened.history().await.unwrap();
	let settled_debits = settled_history.iter().filter(|entry| entry.input_vtxos.contains(&owned.id())
		&& matches!(entry.subsystem.name.as_str(), "bark.exit" | "abandon.fallback_payout")
		&& entry.effective_balance.to_sat() == -100_000).collect::<Vec<_>>();
	assert_eq!(settled_debits.len(), 1);
	assert_eq!(settled_debits[0].id, movement);
	assert_eq!(settled_debits[0].status, MovementStatus::Successful);
	assert_eq!(settled_history.iter().filter(|entry| matches!(entry.subsystem.name.as_str(), "bark.exit" | "abandon.fallback_payout"))
		.map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), -100_000);
	assert_eq!(settled_history.iter().map(|entry| entry.effective_balance.to_sat()).sum::<i64>(), 0);
	assert!(settled_history.iter().filter(|entry| entry.subsystem.name == "abandon.fallback_payout")
		.all(|entry| entry.status != MovementStatus::Successful || (
			entry.metadata["fallback"]["disposition"] == "exited"
			&& entry.metadata["fallback"]["txid"] == serde_json::json!(settled_claim.compute_txid())
			&& entry.effective_balance.to_sat() == 0 && entry.offchain_fee == sat(0))),
		"only the new canonical claim may provide a successful fallback receipt");
	let mut settlement_outputs = Vec::new();
	let mut source_spenders = Vec::new();
	for height in claim_fork_height + 1..=core.get_block_count().unwrap() {
		let block = core.get_block(&core.get_block_hash(height).unwrap()).unwrap();
		for transaction in block.txdata {
			if transaction.input.iter().any(|input| input.previous_output == owned.point()) {
				source_spenders.push(transaction.compute_txid());
			}
			for (vout, output) in transaction.output.iter().enumerate() {
				assert!(!parent.output.iter().chain(&child.output).any(|old| old.value > sat(0) && old.script_pubkey == output.script_pubkey),
					"no orphaned fallback recipient, operator or fee-child output may be paid alongside settlement");
				assert_ne!(output.script_pubkey, destination.script_pubkey(), "the old claim destination must remain unpaid");
				if output.script_pubkey == settlement_destination.script_pubkey() {
					settlement_outputs.push((OutPoint::new(transaction.compute_txid(), vout as u32), output.value));
				}
			}
		}
	}
	assert_eq!(settlement_outputs, vec![(settled_outpoint, delivered.value)]);
	assert_eq!(source_spenders, vec![settled_claim.compute_txid()]);
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	assert!(onchain.read().await.list_transactions().iter().all(|tx| tx.compute_txid() != settled_claim.compute_txid()));
	let mut settled_onchain = sat(0);
	for coin in onchain.read().await.list_unspent() {
		let actual = core.get_tx_out(&coin.outpoint.txid, coin.outpoint.vout, Some(true)).unwrap().unwrap();
		assert_eq!(actual.value, coin.txout.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), coin.txout.script_pubkey);
		settled_onchain += actual.value;
	}
	assert_eq!(onchain.read().await.balance().total(), settled_onchain);
	assert_eq!(settled_onchain, second_canonical_local, "claim miner fee must come from recovered principal, not consume BDK fee change");
	let still_stopped = tokio::time::timeout(Duration::from_secs(2),
		tokio::net::TcpStream::connect(stopped_listener)).await.unwrap().expect_err("Ark must stay offline through actual recovered-principal settlement");
	assert_eq!(still_stopped.kind(), std::io::ErrorKind::ConnectionRefused);
}
