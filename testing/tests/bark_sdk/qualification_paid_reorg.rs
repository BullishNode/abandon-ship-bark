//! A previously paid fallback must not suppress offline protection after a real fork.
//! One explicit app drain closes the restored principal; ordinary spend proofs survive.

use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

use ark::{ProtocolEncoding, VtxoId};
use ark_testing::{sat, TestContext};
use ark_testing::daemon::captaind::{self, ArkClient};
use bark::{Balance, ImportVtxoArgs, OpenWalletArgs, Wallet, WalletSeed};
use bark::exit::ExitState;
use bark::movement::MovementStatus;
use bark::onchain::OnchainWallet;
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
	async fn get_fallback_payouts(&self, upstream: &mut ArkClient, request: protos::GetFallbackPayoutsRequest)
		-> Result<protos::GetFallbackPayoutsResponse, tonic::Status>
	{
		if self.refuse.load(Ordering::SeqCst) {
			self.hits.fetch_add(1, Ordering::SeqCst);
			return Err(tonic::Status::unavailable("owned fixture: history unavailable after paid fallback reorg"));
		}
		Ok(upstream.get_fallback_payouts(request).await?.into_inner())
	}
}

fn configure(config: &mut bark::Config) {
	// The first phase must genuinely adopt a server payment before any native exit.
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
	tokio::time::timeout(Duration::from_secs(180), Box::pin(run())).await
		.expect("paid fallback/fork/cold-reopen/default-daemon/explicit-claim must finish within 180s");
}

async fn run() {
	let ctx = TestContext::new("bark_sdk/qualification_paid_reorg").await;
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
	let historic = owner.get_full_vtxo(original.iter().find(|coin| coin.id() != owned.id()).unwrap().id()).await.unwrap();
	assert_ne!(historic.chain_anchor(), owned.chain_anchor());
	assert_eq!(owned.amount(), sat(100_000));
	let owned_bytes = owned.serialize();
	let historic_bytes = historic.serialize();
	for coin in [&owned, &historic] {
		coin.validate(&ctx.bitcoind().await_transaction(coin.chain_anchor().txid).await).unwrap();
	}
	// A real completed ordinary transfer is recovered through the public import
	// API while its complete owner-signed branch proof is available. No state setter.
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
	// Independent confirmed fee coins cover both actual customer roots. This is
	// genuine operator funding, separate from the owner's 100k principal cut.
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
	// Raw Core JSON avoids the old typed decoder's unknown P2A script enum.
	let parent_info: serde_json::Value = core.call("getrawtransaction", &[serde_json::json!(parent.compute_txid()), serde_json::json!(true)]).unwrap();
	let child_info: serde_json::Value = core.call("getrawtransaction", &[serde_json::json!(child.compute_txid()), serde_json::json!(true)]).unwrap();
	assert!(parent_info["confirmations"].as_u64().unwrap() >= 6);
	assert!(child_info["confirmations"].as_u64().unwrap() >= 6);
	assert_eq!(parent_info["blockhash"], child_info["blockhash"]);
	let paid_block: bitcoin::BlockHash = parent_info["blockhash"].as_str().unwrap().parse().unwrap();
	let paid_height = core.get_block_header_info(&paid_block).unwrap().height as u64;
	assert_eq!(core.get_block_hash(paid_height).unwrap(), paid_block);
	// Core confirmations do not imply the authenticated history generation
	// has rebuilt yet. Only this pre-fork phase explicitly syncs the wallet.
	srv.wait_for_sync_height(paid_tip).await;
	srv.watchmand().wait_for_sync_height(paid_tip).await;
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			owner.chain().invalidate_caches().await;
			if owner.sync_fallback_payouts().await.is_ok()
				&& db.get_fallback_snapshot().await.unwrap().verified_payouts.values().any(|proof| proof.outpoint == payout) { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual coherent history and independently verified payout adoption must precede the fork");
	owner.sync_onchain().await.unwrap();
	let paid_snapshot = db.get_fallback_snapshot().await.unwrap();
	let (entitlement, proof) = paid_snapshot.verified_payouts.iter().find(|(_, proof)| proof.outpoint == payout).unwrap();
	assert_eq!(paid_snapshot.entitlements[entitlement].vtxo_id, owned.id());
	assert_eq!(proof.attempt.gross, sat(100_000));
	assert_eq!(proof.attempt.fee, sat(1_000));
	assert_eq!(proof.attempt.payout, sat(99_000));
	assert!(paid_snapshot.adopted_payout_vtxos.contains(&owned.id()));
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	principal(owner.balance().await.unwrap(), sat(0));
	assert_eq!(owner.onchain().unwrap().read().await.balance().await, baseline_onchain + sat(99_000));
	let paid_cut = onchain.read().await.list_unspent().into_iter().map(|coin| coin.outpoint).collect::<std::collections::BTreeSet<_>>();
	assert_eq!(paid_cut.difference(&before_paid).copied().collect::<Vec<_>>(), vec![payout]);
	let paid_history = owner.history().await.unwrap();
	let receipt = paid_history.iter().find(|entry| entry.subsystem.name == "abandon.fallback_payout" && entry.input_vtxos.contains(&owned.id())).unwrap();
	assert_eq!(receipt.status, MovementStatus::Successful);
	assert_eq!(receipt.effective_balance.to_sat(), -100_000);
	assert_eq!(receipt.offchain_fee, sat(1_000));
	assert!(owner.exit_mgr().get_exit_vtxo_ids().await.is_empty());
	ordinary_stays_spent(&owner, &db, historic.id(), &historic_bytes).await;
	std::fs::write(ctx.datadir.join("paid-before-fork-proof.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"source": owned.id().to_string(), "ordinary_spent": historic.id().to_string(),
		"parent": bitcoin::consensus::encode::serialize_hex(&parent),
		"child": bitcoin::consensus::encode::serialize_hex(&child),
		"block": paid_block, "height": paid_height, "parent_core": parent_info, "child_core": child_info,
		"customer_outpoint": payout, "gross_sat": 100_000, "customer_sat": 99_000,
		"output_fee_sat": 1_000, "child_miner_fee_sat": child_fee.to_sat(),
		"cached_spent": true, "adopted_original": true, "onchain_balance_sat": (baseline_onchain + sat(99_000)).to_sat()
	})).unwrap()).unwrap();
	drop(onchain);
	drop(owner);
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	gate.refuse.store(true, Ordering::SeqCst);
	let alternate_miner = ctx.bitcoind().get_new_address();
	core.invalidate_block(&paid_block).unwrap();
	let fork_height = core.get_block_count().unwrap();
	assert_eq!(fork_height + 1, paid_height);
	ctx.bitcoind().stop().await.unwrap();
	// Lose only the stopped node's ephemeral mempool, preserving its exact bytes.
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
	// The SQLite snapshot still says paid/Spent at this cold-open boundary.
	assert!(db.get_fallback_snapshot().await.unwrap().verified_payouts.values().any(|proof| proof.outpoint == payout));
	assert_eq!(db.get_wallet_vtxo(owned.id()).await.unwrap().unwrap().state, VtxoState::Spent);
	config.daemon_manual_sync = false;
	let (wallet, onchain) = Box::pin(reopen(&ctx, &mnemonic, config, true)).await;
	// No direct sync/protect/progress after reopen: only the real daemon and Core.
	let started = Instant::now();
	let mut last_block = Instant::now();
	loop {
		ordinary_stays_spent(&wallet, &db, historic.id(), &historic_bytes).await;
		let snapshot = db.get_fallback_snapshot().await.unwrap();
		if snapshot.verified_payouts.is_empty() {
			principal(wallet.balance().await.unwrap(), sat(100_000));
			assert!(snapshot.adopted_payout_vtxos.contains(&owned.id()));
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
	assert!(gate.hits.load(Ordering::SeqCst) > 0);
	assert!(db.get_fallback_snapshot().await.unwrap().verified_payouts.is_empty());
	assert!(!onchain.read().await.list_unspent().iter().any(|coin| coin.outpoint == payout),
		"vanished inbound payment must not remain in the onchain wallet's current UTXO cut");
	assert!(core.get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	principal(wallet.balance().await.unwrap(), sat(100_000));
	let claimable = wallet.exit_mgr().list_claimable().await;
	assert_eq!(claimable.len(), 1);
	assert_eq!(claimable[0].id(), owned.id());
	let movement = db.get_exit_vtxo_entry(&owned.id()).await.unwrap().unwrap().movement_id.unwrap();
	std::fs::write(ctx.datadir.join("reorg-claimable-proof.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"source": owned.id().to_string(), "orphan_parent": parent.compute_txid(),
		"orphan_child": child.compute_txid(), "fork_height": fork_height,
		"current_tip": core.get_block_count().unwrap(), "movement": movement,
		"principal_sat": 100_000, "history_refusals": gate.hits.load(Ordering::SeqCst),
		"original_body_sha256": sha256::Hash::hash(&owned_bytes).to_string(),
		"ordinary_body_sha256": sha256::Hash::hash(&historic_bytes).to_string()
	})).unwrap()).unwrap();
	let destination = ctx.bitcoind().get_new_address();
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
				&& db.get_movement_by_id(movement).await.unwrap().status == MovementStatus::Successful { break; }
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
	std::fs::write(ctx.datadir.join("paid-reorg-proof.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"owned_source": owned.id().to_string(), "ordinary_spent_source": historic.id().to_string(),
		"orphan_parent": bitcoin::consensus::encode::serialize_hex(&parent),
		"orphan_child": bitcoin::consensus::encode::serialize_hex(&child),
		"orphan_block": paid_block, "orphan_customer_sat": 99_000, "orphan_output_fee_sat": 1_000,
		"orphan_parent_input_sat": parent_in.to_sat(), "orphan_child_fee_sat": child_fee.to_sat(),
		"retained_mempool_sha256": cache_sha.to_string(), "fork_height": fork_height,
		"history_refusals": gate.hits.load(Ordering::SeqCst), "own_movement": movement,
		"claim": bitcoin::consensus::encode::serialize_hex(&claim), "claim_block": final_block,
		"claim_confirmations": info.confirmations, "claim_delivered_sat": received.value.to_sat(), "claim_fee_sat": claim_fee.to_sat(),
		"current_native_onchain_sat": canonical_local.to_sat(),
		"scope": "one original cached paid payout reorg; same seed/store true-daemon recovery; explicit app drain; ordinary signed-spent exclusion; no automatic drain or global fee-wallet audit"
	})).unwrap()).unwrap();
}
