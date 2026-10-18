//! PoC scenario 9: a reorg removes the server's reclaim of the receive grants
//! after the absent sender was refunded, and the receiver then claims the
//! same grants on chain with the preimage. The guarantee under test is "no
//! double payment": the sender's refund and the receiver's claim must never
//! both confirm.
//!
//! Every transaction of the removed blocks is mined again on the new branch
//! except the reclaim and anything spending it, because a refund that does not
//! depend on the reclaim is public and anyone can rebroadcast it.

use std::collections::BTreeSet;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::policy::signing::VtxoSigner;
use ark::vtxo::Full;
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use bitcoin::consensus::{deserialize, encode::serialize_hex};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::{Transaction, TxIn, TxOut, Txid, Witness};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use bitcoin_ext::{BlockDelta, BlockHeight};
use server::database::Db;
use server_rpc::protos;

#[derive(Clone, Copy, PartialEq, Eq)]
enum When {
	/// The refund is already confirmed when the reorg happens.
	AfterRefund,
	/// The reclaim is confirmed but below the refund depth, so no refund is
	/// signed yet (run with ABANDON_INTERNAL_REFUND_DEPTH above the wait).
	BeforeRefund,
}

#[tokio::test]
async fn expiry_internal_reclaim_reorg_after_refund_pays_once() {
	tokio::time::timeout(Duration::from_secs(420), reclaim_reorg(When::AfterRefund)).await
		.expect("reclaim reorg scenario exceeded its budget");
}

#[tokio::test]
async fn expiry_internal_reclaim_reorg_before_refund_pays_once() {
	tokio::time::timeout(Duration::from_secs(420), reclaim_reorg(When::BeforeRefund)).await
		.expect("reclaim reorg scenario exceeded its budget");
}

async fn customer_allocations(db: &Db, source: &str, status: &str) -> Vec<(i64, Transaction)> {
	db.read(async |t| Ok(t.query("SELECT a.id,a.sweep_tx FROM expiry_allocation a \
		JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
		WHERE e.vtxo_id=$1 AND c.disposition='customer' AND a.status=$2 AND a.sweep_tx IS NOT NULL",
		&[&source, &status]).await?.into_iter()
		.map(|row| (row.get::<_, i64>(0), deserialize(row.get::<_, &[u8]>(1)).unwrap())).collect())).await.unwrap()
}

async fn reclaim_reorg(when: When) {
	let name = match when {
		When::AfterRefund => "server/expiry_internal_reclaim_reorg_after_refund",
		When::BeforeRefund => "server/expiry_internal_reclaim_reorg_before_refund",
	};
	let ctx = TestContext::new(name).await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 4 }];
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create().await;
	srv.wait_for_vtxopool(&ctx).await;
	// The receiver keeps its wallet: it will come back with the preimage.
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(90_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let receiver_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let receiver_destination = recipient.fallback_destination().await.unwrap();
	let sender = ctx.bark("absent-sender", &srv).funded(sat(200_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let client = sender.client().await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	let key = client.get_vtxo_key(&input).await.unwrap();
	let destination = client.fallback_destination().await.unwrap();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let incoming_deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let outgoing_deadline = tip + BlockDelta::new(36);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
		VtxoPolicy::new_server_htlc_send(key.public_key(), hash, incoming_deadline)).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let incoming = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	client.register_vtxo_transactions_with_server(&incoming).await.unwrap();
	let source = incoming[0].id().to_string();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice: invoice.to_string(), payment_amount_sat: 90_000,
		htlc_vtxo_ids: vec![incoming[0].id().to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	let granted = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: receiver_key.serialize().to_vec(),
		htlc_recv_expiry: outgoing_deadline.to_u32(), lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner();
	let grants = granted.htlc_vtxos.iter().map(|b| Vtxo::<Full>::deserialize(b).unwrap()).collect::<Vec<_>>();
	assert_eq!(grants.iter().map(|g| g.amount()).sum::<bitcoin::Amount>(), sat(90_000));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	drop(client); drop(rpc);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(fee_address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let chain = ctx.bitcoind().sync_client();
	let is_confirmed = |txid: Txid| chain.custom_get_raw_transaction_info(txid, None).unwrap()
		.is_some_and(|info| info.confirmations.is_some_and(|d| d > 0));

	// Run until the grants are reclaimed and, for AfterRefund, the refund is confirmed.
	let grant_ids = grants.iter().map(|g| g.id().to_string()).collect::<Vec<_>>();
	let reclaims: BTreeSet<Txid> = tokio::time::timeout(Duration::from_secs(200), async {
		loop {
			let height = ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(height).await;
			srv.watchmand().wait_for_sync_height(height).await;
			tokio::time::sleep(Duration::from_secs(1)).await;
			let rows = db.read(async |t| Ok(t.query("SELECT v.onchain_spent_txid FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
				WHERE v.vtxo_id=ANY($1) AND h.chain_resolution='revoked'", &[&grant_ids]).await?)).await.unwrap();
			if rows.len() != grants.len() { continue; }
			let reclaims = rows.iter().map(|r| r.get::<_, &str>(0).parse().unwrap()).collect::<BTreeSet<Txid>>();
			match when {
				When::BeforeRefund => {
					assert!(customer_allocations(&db, &source, "pending").await.is_empty()
						&& customer_allocations(&db, &source, "confirmed").await.is_empty(),
						"run this case with a refund depth above the reclaim's depth");
					break reclaims;
				},
				When::AfterRefund => {
					if !customer_allocations(&db, &source, "confirmed").await.is_empty() { break reclaims; }
				},
			}
		}
	}).await.expect("grants were not reclaimed (or the sender not refunded)");

	// The reorg: drop the earliest block holding a reclaim.
	let reclaim_heights = reclaims.iter().map(|txid| {
		let info = chain.custom_get_raw_transaction_info(*txid, None).unwrap().unwrap();
		chain.get_block_header_info(&info.blockhash.unwrap()).unwrap().height
	}).collect::<Vec<_>>();
	let fork_height = *reclaim_heights.iter().min().unwrap();
	let old_tip = chain.get_block_count().unwrap() as usize;
	// Replay block by block so every transaction keeps a final locktime.
	let mut replay = Vec::<Vec<Transaction>>::new();
	let mut excluded = reclaims.clone();
	for height in fork_height..=old_tip {
		let block = chain.get_block(&chain.get_block_hash(height as u64).unwrap()).unwrap();
		let mut kept = Vec::new();
		for tx in block.txdata.into_iter().skip(1) {
			if excluded.contains(&tx.compute_txid())
				|| tx.input.iter().any(|i| excluded.contains(&i.previous_output.txid))
			{
				excluded.insert(tx.compute_txid());
				continue;
			}
			kept.push(tx);
		}
		replay.push(kept);
	}
	let refund_replayed = customer_allocations(&db, &source, "confirmed").await.iter()
		.any(|(_, tx)| replay.iter().flatten().any(|r| r.compute_txid() == tx.compute_txid()));
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	chain.invalidate_block(&chain.get_block_hash(fork_height as u64).unwrap()).unwrap();
	let miner = ctx.bitcoind().get_new_address();
	let generate = |txs: &[Transaction]| -> serde_json::Value {
		chain.call("generateblock", &[serde_json::json!(miner.to_string()),
			serde_json::json!(txs.iter().map(serialize_hex).collect::<Vec<_>>())]).unwrap()
	};
	for txs in &replay { generate(txs); }

	// The receiver comes back and claims every grant with the preimage.
	let mut claim = Transaction {
		version: bitcoin::transaction::Version::TWO,
		lock_time: bitcoin::absolute::LockTime::ZERO,
		input: Vec::new(),
		output: vec![TxOut { value: sat(90_000 - 2_000), script_pubkey: receiver_destination.spk.clone() }],
	};
	let clauses = grants.iter().map(|g| g.policy().as_server_htlc_recv().unwrap()
		.user_reveals_preimage_clause(g.exit_delta())).collect::<Vec<_>>();
	for (grant, clause) in grants.iter().zip(&clauses) {
		claim.input.push(TxIn { previous_output: grant.point(), script_sig: Default::default(),
			sequence: clause.sequence(), witness: Witness::new() });
	}
	let prevouts = grants.iter().map(Vtxo::txout).collect::<Vec<_>>();
	for (index, (grant, clause)) in grants.iter().zip(&clauses).enumerate() {
		claim.input[index].witness = recipient.sign_input_with_clause(grant, &clause.clone().into(), index,
			&mut SighashCache::new(&claim), &Prevouts::All(&prevouts)).await.unwrap();
	}
	// Mine empty blocks until the receiver's relative timelock allows the claim.
	for _ in 0..200 {
		if chain.test_mempool_accept(&[&claim]).unwrap()[0].allowed { break; }
		generate(&[]);
	}
	generate(std::slice::from_ref(&claim));
	assert!(is_confirmed(claim.compute_txid()), "the receiver's preimage claim must confirm");
	srv.start().await.unwrap();
	srv.watchmand().start().await.unwrap();

	// Let the server see the new branch; keep mining for a while.
	for _ in 0..20 {
		let height = ctx.generate_blocks(1).await;
		srv.wait_for_sync_height(height).await;
		srv.watchmand().wait_for_sync_height(height).await;
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
	let sender_refunded = customer_allocations(&db, &source, "confirmed").await.iter()
		.any(|(_, tx)| is_confirmed(tx.compute_txid()) && tx.output.iter().any(|o| o.script_pubkey == destination.spk));
	let receiver_paid = is_confirmed(claim.compute_txid());
	std::fs::write(srv.config().data_dir.join("reclaim-reorg-outcome.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"case": name, "fork_height": fork_height, "old_tip": old_tip, "refund_in_replayed_blocks": refund_replayed,
		"excluded": excluded.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"sender_refunded": sender_refunded, "receiver_paid": receiver_paid,
	})).unwrap()).unwrap();
	assert!(!(sender_refunded && receiver_paid),
		"DOUBLE PAYMENT: the sender's refund and the receiver's preimage claim both confirmed");
}
