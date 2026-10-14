//! Native source decisions, separate from freeze/carrier/payment qualification.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use bitcoin::{ScriptBuf, Transaction, TxIn, TxOut, Txid, Witness};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoincore_rpc::RpcApi;

use ark::{ProtocolEncoding, ServerVtxo, Vtxo, VtxoPolicy};
use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::vtxo::Full;
use ark::vtxo::policy::signing::VtxoSigner;
use ark::lightning::PaymentHash;
use bitcoin_ext::BlockDelta;
use server::database::{Db, SpendState};
use server::database::carrier_snapshot::{BackingSnapshot, BackingSnapshotLimits};
use server::database::expiry_ownership::{
	ExpirySource, OwnershipContext, OwnershipDecision, OwnershipDisposition, OwnershipPending,
	OperatorRecovery, RetirementRule,
};
use server::ln::SendRefundEvidence;
use server_rpc::protos;
use server_log::RoundFinished;

use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::util::FutureExt;

async fn snapshot(db: &Db, roots: &[ServerVtxo]) -> BackingSnapshot {
	db.read(async |t| t.backing_snapshot(roots, BackingSnapshotLimits {
		max_rows: 1000, max_serialized_bytes: 10_000_000,
	}).await).await.unwrap()
}

async fn root(db: &Db, vtxo: &Vtxo) -> ServerVtxo {
	db.read(async |t| Ok(t.get_bare_vtxo_by_id(vtxo.chain_anchor().into()).await?.vtxo)).await.unwrap()
}

async fn proposal(db: &Db, snapshot: &BackingSnapshot, coins: &[Vtxo], funding: &BTreeMap<Txid, Transaction>, tip: u32) -> Vec<OwnershipDecision> {
	let context = OwnershipContext { tip, grace: 0, max_dependencies: 1000,
		server_pubkey: coins[0].server_pubkey(), funding, canceled_receives: &BTreeSet::new() };
	let ids = coins.iter().map(Vtxo::id).collect::<Vec<_>>();
	db.read(async |t| t.expiry_ownership_proposal(snapshot, &ids, &context, &BTreeMap::new()).await).await.unwrap()
}

#[tokio::test]
async fn ownership_real_pending_and_registered_boards_preserve_source_and_signer() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_pending_and_registered_boards_preserve_source_and_signer").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.cfg(|c| c.vtxo_lifetime = BlockDelta::new(128)).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let mut coins = Vec::new();
	let mut funding = BTreeMap::new();
	let mut roots = Vec::new();
	for (name, amount) in [("registered", 20_000), ("pending", 30_000)] {
		let wallet = ctx.bark_sdk(name, &srv).cfg(|c| c.daemon_manual_sync = true).funded(sat(amount + 1_000_000)).create().await;
		wallet.stop_daemon_wait().await.unwrap();
		let board = wallet.board_amount(sat(amount)).await.unwrap();
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		let coin = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
		funding.insert(board.funding_tx.compute_txid(), board.funding_tx);
		roots.push(root(&db, &coin).await);
		coins.push(coin);
		if name == "registered" {
			ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
			wallet.sync_onchain().await.unwrap();
			wallet.sync_pending_boards().await.unwrap();
		}
	}
	let inventory = snapshot(&db, &roots).await;
	let early = proposal(&db, &inventory, &coins, &funding, core.get_block_count().unwrap() as u32).await;
	assert!(early.iter().all(|d| matches!(d.disposition, OwnershipDisposition::Pending(OwnershipPending::NotMature))));
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32).await;
	let inventory = snapshot(&db, &roots).await;
	let decisions = proposal(&db, &inventory, &coins, &funding, core.get_block_count().unwrap() as u32).await;
	for (index, source) in [ExpirySource::Registered, ExpirySource::PendingBoard].into_iter().enumerate() {
		let OwnershipDisposition::Customer(input) = decisions[index].disposition else { panic!("unexpected Board ownership: {:?}", decisions[index]); };
		assert_eq!(input.id, coins[index].id());
		assert_eq!(input.owner, coins[index].user_pubkey());
		assert_eq!(input.source, source);
		assert_eq!(decisions[index].gross, coins[index].amount());
		assert!(decisions[index].dependencies.is_empty());
	}
	let mut wrong_signer = coins[0].server_pubkey().serialize();
	wrong_signer[0] ^= 1;
	let context = OwnershipContext { tip: expiry, grace: 0, max_dependencies: 1000,
		server_pubkey: bitcoin::secp256k1::PublicKey::from_slice(&wrong_signer).unwrap(), funding: &funding, canceled_receives: &BTreeSet::new() };
	assert!(db.read(async |t| t.expiry_ownership_proposal(&inventory, &[coins[0].id()], &context, &BTreeMap::new()).await).await.is_err());
	let mut corrupted_funding = funding.clone();
	corrupted_funding.get_mut(&coins[0].chain_anchor().txid).unwrap().lock_time = bitcoin::absolute::LockTime::from_consensus(1);
	let context = OwnershipContext { server_pubkey: coins[0].server_pubkey(), funding: &corrupted_funding, ..context };
	assert!(db.read(async |t| t.expiry_ownership_proposal(&inventory, &[coins[0].id()], &context, &BTreeMap::new()).await).await.is_err());
	println!("native source proposal only: registered20k/pending30k matured, exact owner/gross/source, wrong signer/funding refused");
}

#[tokio::test]
async fn ownership_real_arkoor_unregistered_outputs_keep_each_actual_input_owner() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_arkoor_unregistered_outputs_keep_each_actual_input_owner").await;
	let srv = ctx.captaind("server").no_vtxo_pool().cfg(|c| c.vtxo_lifetime = BlockDelta::new(128)).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let mut wallets = Vec::new();
	for (name, amount) in [("x", 25_000), ("y", 30_000)] {
		let wallet = ctx.bark_sdk(name, &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(amount)).create().await;
		wallet.stop_daemon_wait().await.unwrap();
		wallets.push(wallet);
	}
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let address = receiver.new_address().await.unwrap();
	let mut inputs = Vec::new();
	let mut keys = Vec::new();
	let mut roots = Vec::new();
	let mut funding = BTreeMap::new();
	for wallet in &wallets {
		let coin = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
		keys.push(wallet.pubkey_keypair(&coin.user_pubkey()).await.unwrap().unwrap().1);
		roots.push(root(&db, &coin).await);
		funding.insert(coin.chain_anchor().txid, core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap());
		inputs.push(coin);
	}
	let (change, _) = wallets[1].derive_store_next_keypair().await.unwrap();
	let builder = ArkoorPackageBuilder::new_with_checkpoints(inputs.clone(), vec![
		ArkoorDestination { total_amount: sat(40_000), policy: address.policy().clone() },
		ArkoorDestination { total_amount: sat(15_000), policy: VtxoPolicy::new_pubkey(change.public_key()) },
	]).unwrap().generate_user_nonces(&keys).unwrap();
	let mut cosign_request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &mut cosign_request.parts {
		for output in part.outputs.iter_mut().chain(part.isolated_outputs.iter_mut()) {
			let key = VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
			let hint = if key == address.policy().user_pubkey() { address.account_child().unwrap().unwrap() }
				else { wallets[1].fallback_child_hint(key).await.unwrap() };
			output.origin = Some(hint.into());
		}
	}
	let response = srv.get_public_rpc().await.request_arkoor_cosign(cosign_request).await.unwrap().into_inner();
	let outputs = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap().build_signed_vtxos();
	let from_x = outputs.iter().filter(|v| v.chain_anchor() == inputs[0].chain_anchor()).cloned().collect::<Vec<_>>();
	srv.get_public_rpc().await.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest { vtxos: from_x.iter().map(|v| v.serialize()).collect() }).await.unwrap();
	let expiry = outputs.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	drop(wallets);
	drop(receiver);
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32).await;
	let inventory = snapshot(&db, &roots).await;
	let decisions = proposal(&db, &inventory, &outputs, &funding, core.get_block_count().unwrap() as u32).await;
	for (coin, decision) in outputs.iter().zip(&decisions) {
		let OwnershipDisposition::Customer(input) = decision.disposition else { panic!("unexpected Arkoor ownership: {decision:?}"); };
		let from_x = coin.chain_anchor() == inputs[0].chain_anchor();
		assert_eq!(input.source, if from_x { ExpirySource::Registered } else { ExpirySource::Unregistered });
		assert_eq!(input.owner, if from_x { coin.user_pubkey() } else { inputs[1].user_pubkey() });
		assert_eq!(decision.gross, coin.amount());
	}
	assert_eq!(decisions.iter().map(|d| d.gross.to_sat()).sum::<u64>(), 55_000);
	let ancestors = proposal(&db, &inventory, &inputs, &funding, core.get_block_count().unwrap() as u32).await;
	assert!(ancestors.iter().all(|d| matches!(d.disposition, OwnershipDisposition::Pending(OwnershipPending::ExitedOrConsumed))));
	println!("native source proposal only: partially registered55k Arkoor, actual parent ownership retained, spent ancestors cannot become operator value");
}

#[tokio::test]
async fn ownership_real_absent_delegated_exchange_keeps_complete_original_retirement_closure() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_absent_delegated_exchange_keeps_complete_original_retirement_closure").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(128); c.round_interval = Duration::from_secs(3600);
	}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let wallet = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true).funded(sat(1_100_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	for amount in [20_000, 30_000] {
		let board = wallet.board_amount(sat(amount)).await.unwrap();
		ctx.await_transaction(board.funding_tx.compute_txid()).await;
		ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
		wallet.sync_onchain().await.unwrap(); wallet.sync_pending_boards().await.unwrap();
	}
	let mut originals = Vec::new();
	for coin in wallet.spendable_vtxos().await.unwrap() { originals.push(wallet.get_full_vtxo(coin.id()).await.unwrap()); }
	assert_eq!(originals.len(), 2);
	let request = wallet.refresh_vtxos_delegated(originals.iter().map(Vtxo::id).collect::<Vec<_>>()).await.unwrap().unwrap();
	let hash = request.state().unlock_hash().unwrap();
	drop(request); drop(wallet);
	let mut finished = srv.subscribe_log::<RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(60)).await.unwrap();
	let rows = db.read(async |t| Ok(t.query("SELECT vtxo FROM vtxo WHERE spend_state='unclaimed'", &[]).await?)).await.unwrap();
	let replacements = rows.iter().map(|r| Vtxo::<Full>::deserialize(r.get::<_, &[u8]>("vtxo")).unwrap())
		.filter(|v| v.unlock_hash() == Some(hash)).collect::<Vec<_>>();
	assert!(!replacements.is_empty());
	let mut roots = Vec::new();
	let mut funding = BTreeMap::new();
	for coin in originals.iter().chain(&replacements) {
		let physical = root(&db, coin).await;
		if !roots.iter().any(|r: &ServerVtxo| r.id() == physical.id()) { roots.push(physical); }
		ctx.await_transaction(coin.chain_anchor().txid).await;
		funding.insert(coin.chain_anchor().txid, core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap());
	}
	let expiry = originals.iter().chain(&replacements).map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32).await;
	let inventory = snapshot(&db, &roots).await;
	let decisions = proposal(&db, &inventory, &originals, &funding, core.get_block_count().unwrap() as u32).await;
	let mut original_ids = originals.iter().map(Vtxo::id).collect::<Vec<_>>(); original_ids.sort();
	for decision in &decisions {
		assert!(matches!(decision.disposition, OwnershipDisposition::ConditionalOriginalRecovery(h) if h == hash));
		assert_eq!(decision.dependencies.len(), 1);
		assert_eq!(decision.dependencies[0].ids, original_ids);
		assert_eq!(decision.dependencies[0].rule, RetirementRule::CompleteExchangeClosure);
	}
	let replacement = proposal(&db, &inventory, &replacements, &funding, core.get_block_count().unwrap() as u32).await;
	for decision in &replacement {
		assert!(matches!(decision.disposition, OwnershipDisposition::Customer(_)));
		assert_eq!(decision.dependencies[0].ids, original_ids);
		assert_eq!(decision.dependencies[0].rule, RetirementRule::ConfirmedOrSameCarrier);
	}
	assert_eq!(decisions.iter().map(|d| d.gross.to_sat()).sum::<u64>(), 50_000);
	assert_eq!(replacement.iter().map(|d| d.gross.to_sat()).sum::<u64>(), 50_000);
	let context = OwnershipContext { tip: expiry, grace: 0, max_dependencies: 1,
		server_pubkey: originals[0].server_pubkey(), funding: &funding, canceled_receives: &BTreeSet::new() };
	assert!(db.read(async |t| t.expiry_ownership_proposal(&inventory, &[originals[0].id()], &context, &BTreeMap::new()).await).await.is_err(),
		"dependency overflow must refuse a complete proposal; no payable prefix");
	println!("native source proposal only: actual absent50k delegated exchange, both original ancestors remain one conditional closure, replacement principal retained");
}

#[tokio::test]
async fn ownership_real_intra_ark_send_and_unclaimed_grants_require_complete_retirement() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_intra_ark_send_and_unclaimed_grants_require_complete_retirement").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(512); c.vtxopool.vtxo_lifetime = BlockDelta::new(512);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let invoice = receiver.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	sender.pay_lightning_invoice(invoice.to_string(), None, false).await.unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	let (key, _) = receiver.derive_store_next_keypair().await.unwrap();
	let granted = srv.get_public_rpc().await.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		origin: Some(receiver.fallback_child_hint(key.public_key()).await.unwrap().into()),
		payment_hash: hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: sub.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta).into(),
		lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner().htlc_vtxos.into_iter().map(|bytes| Vtxo::<Full>::deserialize(&bytes).unwrap()).collect::<Vec<_>>();
	assert!(!granted.is_empty());
	let sender_ids = sender.all_vtxos().await.unwrap().into_iter()
		.filter(|w| w.vtxo.policy().as_server_htlc_send().is_some_and(|p| p.payment_hash == hash)).map(|w| w.vtxo.id()).collect::<Vec<_>>();
	let htlcs = db.read(async |t| t.get_user_vtxos_by_id(&sender_ids).await).await.unwrap().into_iter().map(|v| v.vtxo).collect::<Vec<_>>();
	assert!(!htlcs.is_empty());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert!(attempt.is_self_payment() && !attempt.status.is_final());
	let core = ctx.bitcoind().sync_client();
	let mut funding = BTreeMap::new();
	let mut roots = Vec::new();
	for coin in htlcs.iter().chain(&granted) {
		let physical = root(&db, coin).await;
		if !roots.iter().any(|r: &ServerVtxo| r.id() == physical.id()) { roots.push(physical); }
		funding.insert(coin.chain_anchor().txid, core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap());
	}
	drop(sender); drop(receiver);
	let expiry = htlcs.iter().chain(&granted).map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32 + 1).await;
	let inventory = snapshot(&db, &roots).await;
	let tip = core.get_block_count().unwrap() as u32;
	let context = OwnershipContext { tip, grace: 0, max_dependencies: 1000, server_pubkey: htlcs[0].server_pubkey(),
		funding: &funding, canceled_receives: &BTreeSet::new() };
	let mut evidence = BTreeMap::from([(hash, SendRefundEvidence::Unresolved("node outcome unavailable".into()))]);
	let unresolved = db.read(async |t| t.expiry_ownership_proposal(&inventory, &sender_ids, &context, &evidence).await).await.unwrap();
	assert!(unresolved.iter().all(|d| matches!(d.disposition, OwnershipDisposition::Pending(OwnershipPending::LightningOutcome { .. }))));
	// Fixture observation: only an unfinished intra-Ark attempt exists, with no
	// external payment and no recorded preimage. Carrier/guard qualification is separate.
	evidence.insert(hash, SendRefundEvidence::Refundable);
	let sends = db.read(async |t| t.expiry_ownership_proposal(&inventory, &sender_ids, &context, &evidence).await).await.unwrap();
	let mut grant_ids = granted.iter().map(Vtxo::id).collect::<Vec<_>>(); grant_ids.sort();
	for decision in &sends {
		assert!(matches!(decision.disposition, OwnershipDisposition::Customer(input) if input.source == ExpirySource::LightningSend(hash)));
		assert_eq!(decision.dependencies.len(), 1);
		assert_eq!(decision.dependencies[0].ids, grant_ids);
		assert_eq!(decision.dependencies[0].rule, RetirementRule::ConfirmedOrSameCarrier);
	}
	let receives = proposal(&db, &inventory, &granted, &funding, tip).await;
	for decision in &receives {
		assert!(matches!(decision.disposition, OwnershipDisposition::ConditionalUnpaidReceive(h) if h == hash));
		assert_eq!(decision.dependencies[0].ids, grant_ids);
	}
	let unused_pool = inventory.regular.iter().filter(|f| f.state.spend_state == SpendState::Pool)
		.filter_map(|f| f.full_vtxo.as_ref().and_then(|v| v.clone().try_into_user_vtxo().ok())).collect::<Vec<_>>();
	assert!(!unused_pool.is_empty(), "actual pool must retain unused operator principal");
	let pool = proposal(&db, &inventory, &unused_pool, &funding, tip).await;
	assert!(pool.iter().all(|d| matches!(d.disposition, OwnershipDisposition::Operator(OperatorRecovery::UnusedPool))));
	println!("native source proposal only: intra-Ark100k sender refund requires every real granted receive; unavailable verdict stays pending, grants remain conditional");
}

#[tokio::test]
async fn ownership_real_completed_exchange_and_round_padding_have_explicit_operator_sources() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_completed_exchange_and_round_padding_have_explicit_operator_sources").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(512); c.round_interval = Duration::from_secs(3600);
	}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let wallet = ctx.bark_sdk("present", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(50_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let original = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	ctx.trigger_rounds_until(&srv, wallet.refresh_vtxos(vec![original.id()])).await.unwrap();
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.sync_onchain().await.unwrap();
	wallet.sync_pending_rounds().await.unwrap();
	let current = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let hash = current.unlock_hash().unwrap();
	let completed = db.read(async |t| t.get_round_participation_by_unlock_hash(hash).await).await.unwrap().unwrap();
	assert!(completed.forfeited_at.is_some() && completed.inputs.iter().all(|i| i.is_forfeited()));
	let clause = wallet.find_signable_clause(&original).await.unwrap();
	let mut stale_claim = Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![TxIn { previous_output: original.point(), sequence: clause.sequence().unwrap(),
			script_sig: ScriptBuf::new(), witness: Witness::new() }],
		output: vec![TxOut { value: original.amount() - sat(2_000),
			script_pubkey: ctx.bitcoind().get_new_address().script_pubkey() }],
	};
	stale_claim.input[0].witness = wallet.sign_input(&original, 0, &mut SighashCache::new(&stale_claim),
		&Prevouts::All(&[original.txout()])).await.unwrap();
	let original_exit_delta = original.exit_delta();
	drop(wallet);
	let roots = [root(&db, &original).await, root(&db, &current).await];
	let core = ctx.bitcoind().sync_client();
	ctx.await_transaction(current.chain_anchor().txid).await;
	let funding = [original.chain_anchor().txid, current.chain_anchor().txid].into_iter()
		.map(|txid| (txid, core.get_raw_transaction(&txid, None).unwrap())).collect::<BTreeMap<_, _>>();
	let expiry = original.expiry_height().to_u32().max(current.expiry_height().to_u32());
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32).await;
	let inventory = snapshot(&db, &roots).await;
	let padding = inventory.regular.iter().filter(|f| f.state.spend_state == SpendState::Unclaimed)
		.filter_map(|f| f.full_vtxo.as_ref().and_then(|v| v.clone().try_into_user_vtxo().ok()))
		.filter(|v| v.user_pubkey().to_string() == "02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c")
		.collect::<Vec<_>>();
	assert_eq!(padding.len(), 1, "single real output must yield actual round-builder padding");
	let original_decision = proposal(&db, &inventory, &[original], &funding, expiry).await;
	assert!(matches!(original_decision[0].disposition, OwnershipDisposition::Pending(OwnershipPending::ExitedOrConsumed)),
		"the original is superseded by its actual signed forfeit, not a second operator leaf");
	let forfeits = inventory.regular.iter().filter(|f| f.state.spend_state == SpendState::RoundForfeit).map(|f| f.state.vtxo_id).collect::<Vec<_>>();
	assert_eq!(forfeits.len(), 1);
	let context = OwnershipContext { tip: expiry, grace: 0, max_dependencies: 1000, server_pubkey: current.server_pubkey(),
		funding: &funding, canceled_receives: &BTreeSet::new() };
	let forfeit_decision = db.read(async |t| t.expiry_ownership_proposal(&inventory, &forfeits, &context, &BTreeMap::new()).await).await.unwrap();
	assert!(matches!(forfeit_decision[0].disposition, OwnershipDisposition::Operator(OperatorRecovery::CompletedExchange(h)) if h == hash), "{:?}", forfeit_decision[0]);
	assert_eq!(forfeit_decision[0].gross, sat(50_000));
	assert!(forfeit_decision[0].dependencies.is_empty());
	let current_decision = proposal(&db, &inventory, &[current], &funding, expiry).await;
	assert!(matches!(current_decision[0].disposition, OwnershipDisposition::Customer(_)));
	assert!(current_decision[0].dependencies.is_empty());
	let padding_decision = proposal(&db, &inventory, &padding, &funding, expiry).await;
	assert!(matches!(padding_decision[0].disposition, OwnershipDisposition::Operator(OperatorRecovery::Padding)));
	let forfeit = inventory.regular.iter().find(|f| f.state.vtxo_id == forfeits[0]).unwrap().full_vtxo.as_ref().unwrap();
	let anchor = &funding[&forfeit.chain_anchor().txid];
	forfeit.validate(anchor).unwrap();
	assert_eq!(inventory.path_root(forfeit, anchor).unwrap(), Some(roots[0].id()),
		"the actual completed-forfeit source must prove containment under its actual original backing");
	let transactions = forfeit.transactions().map(|item| item.tx).collect::<Vec<_>>();
	assert_eq!(transactions.last().unwrap().input[0].sequence, bitcoin::Sequence::ZERO,
		"fresh producer uses zero relative delay and signals opt-in RBF");
	for transaction in &transactions {
		ctx.broadcast_cpfp(transaction).await;
		ctx.generate_blocks(1).await;
		// Core 31's `anchor` script type predates this RPC crate's enum.
		let confirmed: serde_json::Value = core.call("getrawtransaction", &[
			serde_json::json!(transaction.compute_txid().to_string()), serde_json::json!(true),
		]).unwrap();
		assert!(confirmed["confirmations"].as_u64().unwrap() >= 1);
		if transaction == &transactions[transactions.len() - 2] {
			ctx.generate_blocks(original_exit_delta.to_u32()).await;
			let claimable: serde_json::Value = core.call("testmempoolaccept", &[
				serde_json::json!([bitcoin::consensus::encode::serialize_hex(&stale_claim)]),
			]).unwrap();
			assert_eq!(claimable[0]["allowed"], true, "the stale user's signed claim must be node-valid before the forfeit wins");
		}
	}
	assert!(core.get_tx_out(&forfeit.point().txid, forfeit.point().vout, Some(true)).unwrap().is_some());
	let conflicted: serde_json::Value = core.call("testmempoolaccept", &[
		serde_json::json!([bitcoin::consensus::encode::serialize_hex(&stale_claim)]),
	]).unwrap();
	assert_eq!(conflicted[0]["allowed"], false);
	assert_eq!(conflicted[0]["reject-reason"], "missing-inputs",
		"confirmed actual forfeit retires the otherwise valid stale original claim");
	println!("native source proposal only: completed50k exchange gives operator original/customer replacement; actual330sat padding is explicit operator recovery");
}

#[tokio::test]
async fn ownership_real_collected_receive_survives_cooperative_claim_rollback() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_collected_receive_survives_cooperative_claim_rollback").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(512); c.vtxopool.vtxo_lifetime = BlockDelta::new(512);
		}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let invoice = receiver.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let preimage = receiver.lightning_receive_checkpoint(hash).await.unwrap().unwrap().payment_preimage;
	let external = lightning.external;
	let paying = tokio::spawn(async move { external.try_pay_bolt11(invoice.to_string()).await });
	let sub = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
			if sub.status == server::database::ln::LightningHtlcSubscriptionStatus::Accepted { break sub; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.unwrap();
	let (key, _) = receiver.derive_store_next_keypair().await.unwrap();
	let granted = srv.get_public_rpc().await.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		origin: Some(receiver.fallback_child_hint(key.public_key()).await.unwrap().into()),
		payment_hash: hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: sub.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta).into(),
		lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner().htlc_vtxos.into_iter().map(|bytes| Vtxo::<Full>::deserialize(&bytes).unwrap()).collect::<Vec<_>>();
	assert!(!granted.is_empty());
	// Real collection commits before this narrow fault refuses only the later
	// cooperative grant spend. No receipt, preimage or subscription is fabricated.
	db.write(async |t| {
		t.batch_execute("CREATE FUNCTION interrupt_ownership_receive_claim() RETURNS trigger AS $$
			BEGIN
				IF OLD.spend_state='htlc-recv-unclaimed' AND OLD.oor_spent_txid IS NULL AND NEW.oor_spent_txid IS NOT NULL THEN
					RAISE EXCEPTION 'test refuses cooperative grant spend after collection';
				END IF;
				RETURN NEW;
			END; $$ LANGUAGE plpgsql;
			CREATE TRIGGER interrupt_ownership_receive_claim BEFORE UPDATE ON vtxo
			FOR EACH ROW EXECUTE FUNCTION interrupt_ownership_receive_claim();").await?;
		Ok(())
	}).await.unwrap();
	let (output, _) = receiver.derive_store_next_keypair().await.unwrap();
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(granted.clone().into_iter(), VtxoPolicy::new_pubkey(output.public_key()))
		.unwrap().generate_user_nonces(&[key]).unwrap();
	let mut cosign_request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &mut cosign_request.parts {
		for output in part.outputs.iter_mut().chain(part.isolated_outputs.iter_mut()) {
			let key = VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
			output.origin = Some(receiver.fallback_child_hint(key).await.unwrap().into());
		}
	}
	assert!(srv.get_public_rpc().await.claim_lightning_receive(protos::ClaimLightningReceiveRequest {
		payment_hash: hash.to_vec(), payment_preimage: preimage.to_vec(),
		cosign_request: Some(cosign_request),
	}).await.is_err());
	tokio::time::timeout(Duration::from_secs(30), paying).await.unwrap().unwrap().unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert_eq!(sub.status, server::database::ln::LightningHtlcSubscriptionStatus::Settled);
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_some());
	db.write(async |t| { t.batch_execute("DROP TRIGGER interrupt_ownership_receive_claim ON vtxo; DROP FUNCTION interrupt_ownership_receive_claim();").await?; Ok(()) }).await.unwrap();
	drop(receiver);
	let core = ctx.bitcoind().sync_client();
	let mut roots = Vec::new();
	let mut funding = BTreeMap::new();
	for coin in &granted {
		let physical = root(&db, coin).await;
		if !roots.iter().any(|r: &ServerVtxo| r.id() == physical.id()) { roots.push(physical); }
		funding.insert(coin.chain_anchor().txid, core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap());
	}
	let expiry = granted.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32).await;
	let inventory = snapshot(&db, &roots).await;
	let decisions = proposal(&db, &inventory, &granted, &funding, core.get_block_count().unwrap() as u32).await;
	for (coin, decision) in granted.iter().zip(&decisions) {
		assert!(matches!(decision.disposition, OwnershipDisposition::Customer(input) if input.source == ExpirySource::LightningReceive(hash) && input.owner == coin.user_pubkey()));
		assert_eq!(decision.gross, coin.amount());
		assert!(decision.dependencies.is_empty());
	}
	assert_eq!(decisions.iter().map(|d| d.gross.to_sat()).sum::<u64>(), 100_000);
	println!("native source proposal only: external100k collected before cooperative rollback; settled subscription+recorded settlement preserve recipient principal");
}

#[tokio::test]
async fn ownership_real_paid_external_send_cannot_become_a_customer_refund() {
	let ctx = TestContext::new("expiry_ownership/ownership_real_paid_external_send_cannot_become_a_customer_refund").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.no_vtxo_pool().funded(btc(1)).cfg(|c| c.vtxo_lifetime = BlockDelta::new(512)).create().await;
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let invoice = lightning.external.invoice(Some(sat(100_000)), "ownership_paid_send", "native ownership fixture").await;
	sender.pay_lightning_invoice(invoice, None, true).await.unwrap();
	lightning.external.wait_invoice_paid("ownership_paid_send").await;
	let rows = db.read(async |t| Ok(t.query("SELECT v.vtxo FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE h.direction='incoming'", &[]).await?)).await.unwrap();
	let htlcs = rows.iter().map(|r| Vtxo::<Full>::deserialize(r.get::<_, &[u8]>("vtxo")).unwrap()).collect::<Vec<_>>();
	assert!(!htlcs.is_empty());
	let hash = htlcs[0].policy().as_server_htlc_send().unwrap().payment_hash;
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_some());
	drop(sender);
	let core = ctx.bitcoind().sync_client();
	let mut roots = Vec::new();
	let mut funding = BTreeMap::new();
	for coin in &htlcs {
		let physical = root(&db, coin).await;
		if !roots.iter().any(|r: &ServerVtxo| r.id() == physical.id()) { roots.push(physical); }
		funding.insert(coin.chain_anchor().txid, core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap());
	}
	let expiry = htlcs.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry - core.get_block_count().unwrap() as u32).await;
	let inventory = snapshot(&db, &roots).await;
	let context = OwnershipContext { tip: expiry, grace: 0, max_dependencies: 1000, server_pubkey: htlcs[0].server_pubkey(),
		funding: &funding, canceled_receives: &BTreeSet::new() };
	// An obsolete Refundable observation cannot override the actual payment's
	// durable settlement or its current completed attempt.
	let evidence = BTreeMap::from([(hash, SendRefundEvidence::Refundable)]);
	let ids = htlcs.iter().map(Vtxo::id).collect::<Vec<_>>();
	let decisions = db.read(async |t| t.expiry_ownership_proposal(&inventory, &ids, &context, &evidence).await).await.unwrap();
	assert!(decisions.iter().all(|d| matches!(d.disposition, OwnershipDisposition::Operator(OperatorRecovery::PaidLightningSend(h)) if h == hash)));
	println!("native source proposal only: actual external100k paid send remains operator recovery despite an obsolete refundable observation");
}
