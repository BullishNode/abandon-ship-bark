//! PoC scenario 10: one rightful user timeout claim spends one original of
//! each of TWO internal attempts. The other original of each attempt must still
//! be paid to the sender's fallback script, exactly once.
//!
//! The assertions are money outcomes only (chain outputs, allocation rows), so
//! the same test judges the current joint refund and its replacements.

use std::collections::BTreeSet;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;
use ark::vtxo::policy::signing::VtxoSigner;
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use bitcoin::{Amount, OutPoint, Transaction, TxIn, TxOut, Witness};
use bitcoin::consensus::deserialize;
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server_rpc::protos;

use super::expiry_internal_partial_terminal::{confirm_actual_path, confirm_recoverable_prefix};

struct Attempt {
	sources: Vec<Vtxo<Full>>,
	grants: Vec<Vtxo<Full>>,
}

/// One confirmed customer payout of a source: (parent txid, payout sat, fee sat).
async fn confirmed_customer_payouts(db: &Db, source: &str) -> Vec<(String, i64, i64)> {
	db.read(async |t| Ok(t.query("SELECT a.sweep_txid,c.payout_sat,c.fee_sat FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
		WHERE e.vtxo_id=$1 AND c.disposition='customer' AND a.status='confirmed'", &[&source]).await?
		.into_iter().map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1), row.get::<_, i64>(2))).collect()))
		.await.unwrap()
}

#[tokio::test]
async fn expiry_internal_cross_attempt_claim_pays_remaining_sources() {
	tokio::time::timeout(Duration::from_secs(420), Box::pin(cross_attempt_claim())).await
		.expect("cross-attempt claim scenario exceeded its total budget");
}

async fn cross_attempt_claim() {
	let ctx = TestContext::new("server/expiry_internal_cross_attempt_claim").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let operator = ctx.bitcoind().get_new_address();
	let drain = operator.clone();
	let srv = Box::pin(ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 12 }];
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().watchmand_cfg(move |cfg| cfg.sweep_address = Some(drain.into_unchecked())).create()).await;
	srv.wait_for_vtxopool(&ctx).await;
	let recipient = Box::pin(ctx.bark_sdk("recipient", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).create()).await;
	let receive_destination = recipient.fallback_destination().await.unwrap();
	let actor = Box::pin(ctx.bark("sender", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).funded(sat(1_000_000)).create()).await;
	let mut ids = Vec::new();
	for _ in 0..4 {
		let board = actor.board_and_confirm_and_register(&ctx, sat(100_000)).await;
		assert_eq!(board.len(), 1);
		ids.push(board[0]);
	}
	let sender = actor.client().await;
	let fallback = sender.fallback_destination().await.unwrap();
	assert_ne!(fallback.spk, receive_destination.spk);
	let mut rpc = srv.get_public_rpc().await;

	// Two separate internal attempts, each funded by two separate originals.
	let mut attempts = Vec::new();
	for pair in ids.chunks(2) {
		let invoice = recipient.bolt11_invoice(sat(180_000), None, None).await.unwrap();
		let hash = PaymentHash::from(&invoice);
		let receive_key = recipient.new_address().await.unwrap().policy().user_pubkey();
		let mut inputs = Vec::new();
		let mut keys = Vec::new();
		for id in pair {
			let input = sender.get_full_vtxo(*id).await.unwrap();
			keys.push(sender.get_vtxo_key(&input).await.unwrap());
			inputs.push(input);
		}
		let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
		let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(inputs,
			VtxoPolicy::new_server_htlc_send(keys[0].public_key(), hash, deadline)).unwrap()
			.generate_user_nonces(&keys).unwrap();
		let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
			parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
		}).await.unwrap().into_inner();
		let sources = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(response).unwrap())
			.unwrap().build_signed_vtxos();
		assert_eq!(sources.len(), 2);
		assert!(sources.iter().all(|source| source.amount() == sat(100_000)));
		sender.register_vtxo_transactions_with_server(&sources).await.unwrap();
		lightning.sync().await;
		rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
			invoice: invoice.to_string(), payment_amount_sat: 180_000,
			htlc_vtxo_ids: sources.iter().map(|source| source.id().to_bytes().to_vec()).collect(),
			mailbox_id: None, retry_for_secs: Some(5),
		}).await.unwrap();
		let response = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
			payment_hash: hash.to_vec(), user_pubkey: receive_key.serialize().to_vec(),
			htlc_recv_expiry: (tip + BlockDelta::new(36)).to_u32(), lightning_receive_anti_dos: None,
		}).await.unwrap().into_inner();
		let grants = response.htlc_vtxos.iter().map(|raw| Vtxo::<Full>::deserialize(raw).unwrap()).collect::<Vec<_>>();
		assert!(!grants.is_empty());
		assert_eq!(grants.iter().map(Vtxo::amount).sum::<Amount>(), sat(180_000));
		attempts.push(Attempt { sources, grants });
	}
	assert_eq!(attempts.len(), 2);
	let claimed = [attempts[0].sources[0].clone(), attempts[1].sources[0].clone()];
	let remaining = [attempts[0].sources[1].clone(), attempts[1].sources[1].clone()];
	let db = Db::connect(&srv.config().postgres).await.unwrap();

	// Both actors go away. The sender exits one original of each attempt and
	// claims both in ONE transaction, as `drain_exits` would.
	let fee_address = srv.watchmand().wait_wallet_address().await;
	srv.watchmand().stop().await.unwrap();
	let mut seen = BTreeSet::new();
	for grant in attempts.iter().flat_map(|a| &a.grants) { confirm_actual_path(&ctx, grant, &mut seen).await; }
	for source in &claimed { confirm_actual_path(&ctx, source, &mut seen).await; }
	let mut prefixes = Vec::new();
	for source in &remaining { prefixes.push(confirm_recoverable_prefix(&ctx, &db, source, &mut seen).await); }
	for _ in 0..6 { ctx.bitcoind().fund_addr(fee_address.clone(), sat(150_000)).await; }
	ctx.generate_blocks(2).await;
	let chain = ctx.bitcoind().sync_client();
	let current = ctx.bitcoind().get_block_count().await as u32;
	let clauses = claimed.iter().map(|source| source.policy().as_server_htlc_send().unwrap()
		.user_claim_after_expiry_clause(source.exit_delta())).collect::<Vec<_>>();
	let depth_start = |point: OutPoint| current + 1 - u32::try_from(
		chain.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap().confirmations).unwrap();
	let mut mature = current;
	for (source, clause) in claimed.iter().zip(&clauses) {
		mature = mature.max(clause.timelock_height.to_u32())
			.max(depth_start(source.point()) + clause.block_delta.to_u32());
	}
	for (source, prefix) in remaining.iter().zip(&prefixes) {
		mature = mature.max(source.expiry_height().to_u32()).max(depth_start(*prefix) + source.exit_delta().to_u32());
	}
	for grant in attempts.iter().flat_map(|a| &a.grants) {
		mature = mature.max(grant.policy().as_server_htlc_recv().unwrap().htlc_expiry.to_u32())
			.max(depth_start(grant.point()) + grant.exit_delta().to_u32());
	}
	if mature >= current { ctx.generate_blocks(mature - current + 1).await; }
	let mut claim = Transaction {
		version: bitcoin::transaction::Version::TWO,
		lock_time: clauses.iter().max_by_key(|clause| clause.timelock_height).unwrap().locktime(),
		input: claimed.iter().zip(&clauses).map(|(source, clause)| TxIn { previous_output: source.point(),
			script_sig: Default::default(), sequence: clause.sequence(), witness: Witness::new() }).collect(),
		output: vec![TxOut { value: sat(198_000), script_pubkey: fallback.spk.clone() }],
	};
	let prevouts = claimed.iter().map(Vtxo::txout).collect::<Vec<_>>();
	for (index, (source, clause)) in claimed.iter().zip(&clauses).enumerate() {
		claim.input[index].witness = sender.sign_input_with_clause(source, &clause.clone().into(), index,
			&mut SighashCache::new(&claim), &Prevouts::All(&prevouts)).await.unwrap();
	}
	chain.send_raw_transaction(&claim).unwrap();
	let claim_height = ctx.generate_blocks(6).await;
	let receipt = chain.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert_eq!(receipt.value, sat(198_000));
	assert!(receipt.confirmations >= 6);
	drop(sender); drop(recipient); drop(rpc); drop(actor);
	srv.watchmand().start().await.unwrap();
	srv.wait_for_sync_height(claim_height).await;
	srv.watchmand().wait_for_sync_height(claim_height).await;

	// Positive oracle: both remaining originals are paid and confirmed. Blocks
	// are mined while waiting so a design that waits for confirmations can act.
	let remaining_ids = remaining.iter().map(|source| source.id().to_string()).collect::<Vec<_>>();
	tokio::time::timeout(Duration::from_secs(240), async {
		loop {
			let mut done = true;
			for id in &remaining_ids {
				if confirmed_customer_payouts(&db, id).await.is_empty() { done = false; }
			}
			if done { break; }
			ctx.generate_blocks(1).await;
			tokio::time::sleep(Duration::from_secs(2)).await;
		}
	}).await.expect("HOLD: a cross-attempt user claim stranded the remaining originals' payouts");
	ctx.generate_blocks(6).await;

	// Money: each remaining original paid exactly once, 100,000 - 1,000 fee;
	// the fallback outputs of the confirmed parents carry exactly those payouts.
	let mut parents = BTreeSet::new();
	let mut paid = 0i64;
	for id in &remaining_ids {
		let payouts = confirmed_customer_payouts(&db, id).await;
		assert_eq!(payouts.len(), 1, "remaining original {} paid more than once", id);
		assert_eq!((payouts[0].1, payouts[0].2), (99_000, 1_000));
		paid += payouts[0].1;
		parents.insert(payouts[0].0.clone());
	}
	let mut fallback_out = Amount::ZERO;
	for txid in &parents {
		let parent: Transaction = deserialize(&db.read(async |t| Ok(t.query_one(
			"SELECT sweep_tx FROM expiry_allocation WHERE sweep_txid=$1", &[txid]).await?.get::<_, Vec<u8>>(0))).await.unwrap()).unwrap();
		assert!(!parent.output.iter().any(|output| output.script_pubkey == receive_destination.spk),
			"the absent recipient must not be paid for an uncollected payment");
		for (vout, output) in parent.output.iter().enumerate() {
			if output.script_pubkey != fallback.spk { continue; }
			let actual = chain.get_tx_out(&parent.compute_txid(), vout as u32, Some(true)).unwrap().unwrap();
			assert!(actual.confirmations >= 6);
			assert_eq!(actual.value, output.value);
			fallback_out += output.value;
		}
	}
	assert_eq!(fallback_out, sat(u64::try_from(paid).unwrap()));
	for source in &claimed {
		assert!(confirmed_customer_payouts(&db, &source.id().to_string()).await.is_empty(),
			"a source the user already claimed must not also be paid");
	}
}
