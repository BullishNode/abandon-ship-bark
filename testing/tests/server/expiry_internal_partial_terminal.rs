//! UNBOUND source-only release-gap fixture. Not compiled or executed.
//! One actual attempt uses TWO original inputs and one complete receive set.

use std::collections::BTreeSet;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::{Full, TapScriptClause};
use ark::vtxo::policy::signing::VtxoSigner;
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use bitcoin::{Transaction, TxIn, TxOut, Witness};
use bitcoin::consensus::deserialize;
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::Db;
use server_rpc::protos;

async fn confirm_actual_path(ctx: &TestContext, leaf: &Vtxo<Full>, seen: &mut BTreeSet<bitcoin::Txid>) {
	for step in leaf.transactions() {
		let txid = step.tx.compute_txid();
		if !seen.insert(txid) { continue; }
		if ctx.bitcoind().sync_client().custom_get_raw_transaction_info(txid, None).unwrap()
			.is_some_and(|info| info.confirmations.is_some_and(|depth| depth > 0))
		{ continue; }
		ctx.broadcast_cpfp(&step.tx).await;
		ctx.generate_blocks(1).await;
	}
	assert!(ctx.bitcoind().sync_client().get_tx_out(&leaf.point().txid, leaf.point().vout, Some(true))
		.unwrap().is_some_and(|output| output.confirmations > 0));
}

/// The A claim is an ordinary rightful timeout receipt, made while its owner
/// is present. Both actors then disappear; B must not be stranded. There is no
/// incorrect payout request, SQL money mutation, or fabricated resolution.
#[tokio::test]
async fn expiry_internal_one_attempt_partial_terminal_keeps_other_source_owed() {
	let ctx = TestContext::new("server/expiry_internal_one_attempt_partial_terminal").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = Box::pin(ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 8 }];
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().create()).await;
	srv.wait_for_vtxopool(&ctx).await;
	let recipient = Box::pin(ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create()).await;
	let invoice = recipient.bolt11_invoice(sat(180_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let receive_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let receive_destination = recipient.fallback_destination().await.unwrap();
	let actor = ctx.bark("sender", &srv).cfg(|cfg| cfg.daemon_manual_sync = true)
		.funded(sat(400_000)).create().await;
	let first = actor.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let second = actor.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	assert_eq!(first.len(), 1);
	assert_eq!(second.len(), 1);
	let sender = actor.client().await;
	let inputs = vec![sender.get_full_vtxo(first[0]).await.unwrap(), sender.get_full_vtxo(second[0]).await.unwrap()];
	assert_ne!(inputs[0].chain_anchor(), inputs[1].chain_anchor());
	assert!(inputs.iter().all(|input| input.amount() == sat(100_000)));
	let keys = vec![sender.get_vtxo_key(&inputs[0]).await.unwrap(), sender.get_vtxo_key(&inputs[1]).await.unwrap()];
	let fallback = sender.fallback_destination().await.unwrap();
	assert_ne!(fallback.spk, receive_destination.spk);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let incoming_deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(inputs,
		VtxoPolicy::new_server_htlc_send(keys[0].public_key(), hash, incoming_deadline)).unwrap()
		.generate_user_nonces(&keys).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let reply = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let sources = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(reply).unwrap())
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
	let reply = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: receive_key.serialize().to_vec(),
		htlc_recv_expiry: (tip + BlockDelta::new(36)).to_u32(), lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner();
	let grants = reply.htlc_vtxos.iter().map(|raw| Vtxo::<Full>::deserialize(raw).unwrap()).collect::<Vec<_>>();
	assert_eq!(grants.iter().map(Vtxo::amount).sum::<bitcoin::Amount>(), sat(180_000));
	assert!(grants.len() >= 2);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let scope = db.read(async |t| {
		let rows = t.query("SELECT f.id,f.subscription_id FROM expiry_lightning_internal_funding f \
			JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
			WHERE a.payment_hash=$1", &[&hash.to_string()]).await?;
		assert_eq!(rows.len(), 1, "one actual funding attempt must own this receive subscription");
		let attempt: i64 = rows[0].get("id");
		let subscription: i64 = rows[0].get("subscription_id");
		let originals = t.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
			WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&attempt]).await?
			.into_iter().map(|row| row.get::<_, String>("vtxo_id")).collect::<BTreeSet<_>>();
		let issued = t.query("SELECT v.vtxo_id FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
			WHERE v.lightning_htlc_subscription_id=$1 AND h.direction='outgoing' ORDER BY v.vtxo_id", &[&subscription]).await?
			.into_iter().map(|row| row.get::<_, String>("vtxo_id")).collect::<BTreeSet<_>>();
		Ok((originals, issued))
	}).await.unwrap();
	assert_eq!(scope.0, sources.iter().map(|source| source.id().to_string()).collect());
	assert_eq!(scope.1, grants.iter().map(|grant| grant.id().to_string()).collect());
	// Preserve every actual grant. No receiver claim, preimage or returning
	// wallet action is used to discharge the receive side.
	let fee_address = srv.watchmand().wait_wallet_address().await;
	srv.watchmand().stop().await.unwrap();
	let mut seen = BTreeSet::new();
	for grant in &grants { confirm_actual_path(&ctx, grant, &mut seen).await; }
	confirm_actual_path(&ctx, &sources[0], &mut seen).await;
	let chain = ctx.bitcoind().sync_client();
	let point = sources[0].point();
	let current = ctx.bitcoind().get_block_count().await as u32;
	let confirmed = chain.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	let clause = sources[0].policy().as_server_htlc_send().unwrap()
		.user_claim_after_expiry_clause(sources[0].exit_delta());
	let csv = current + 1 - u32::try_from(confirmed.confirmations).unwrap() + clause.block_delta.to_u32();
	let mature = incoming_deadline.to_u32().max(csv) + 1;
	if mature > current { ctx.generate_blocks(mature - current).await; }
	let claim_address = ctx.bitcoind().get_new_address();
	let mut claim = Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: clause.locktime(),
		input: vec![TxIn { previous_output: point, script_sig: Default::default(), sequence: clause.sequence(), witness: Witness::new() }],
		output: vec![TxOut { value: sat(98_000), script_pubkey: claim_address.script_pubkey() }],
	};
	let prevouts = [sources[0].txout()];
	let witness = sender.sign_input_with_clause(&sources[0], &clause.clone().into(), 0,
		&mut SighashCache::new(&claim), &Prevouts::All(&prevouts)).await.unwrap();
	claim.input[0].witness = witness;
	chain.send_raw_transaction(&claim).unwrap();
	ctx.generate_blocks(6).await;
	assert!(chain.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap().confirmations >= 6);
	assert_eq!(claim.output[0].value, sources[0].amount() - sat(2_000));
	assert!(chain.get_tx_out(&sources[1].chain_anchor().txid, sources[1].chain_anchor().vout, Some(true)).unwrap().is_some());
	for grant in &grants {
		assert!(chain.get_tx_out(&grant.point().txid, grant.point().vout, Some(true)).unwrap().is_some());
	}
	drop(sender);
	drop(recipient);
	drop(rpc);
	for _ in 0..4 { ctx.bitcoind().fund_addr(fee_address.clone(), sat(150_000)).await; }
	ctx.generate_blocks(2).await;
	srv.watchmand().start().await.unwrap();
	let unpaid = sources[1].id();
	let parent: Transaction = tokio::time::timeout(Duration::from_secs(150), async {
		for _ in 0..100 {
			let height = ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(height).await;
			srv.watchmand().wait_for_sync_height(height).await;
			tokio::time::sleep(Duration::from_secs(1)).await;
			let row = db.read(async |t| Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND a.broadcast_at IS NOT NULL", &[&unpaid.to_string()]).await?)).await.unwrap();
			if let Some(row) = row { return deserialize(row.get("sweep_tx")).unwrap(); }
		}
		panic!("the genuinely unpaid original source still requires an absent user");
	}).await.expect("partial terminal closure must not strand the other original source");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	for grant in &grants {
		let input = parent.input.iter().find(|input| input.previous_output == grant.point())
			.expect("source observations alone cannot replace an actual receive-grant dependency");
		let script = grant.policy().as_server_htlc_recv().unwrap()
			.server_claim_after_expiry_clause(grant.server_pubkey(), grant.exit_delta()).tapscript();
		assert_eq!(input.witness.taproot_leaf_script().unwrap().script, script.as_script());
	}
	let mut unpaid_path = sources[1].transactions().map(|step|
		bitcoin::OutPoint::new(step.tx.compute_txid(), step.output_idx as u32)).collect::<BTreeSet<_>>();
	unpaid_path.insert(sources[1].chain_anchor());
	assert!(parent.input.iter().any(|input| unpaid_path.contains(&input.previous_output)),
		"a grant-funded parent alone cannot discharge B while its original backing remains independently live");
	assert!(!parent.input.iter().any(|input| input.previous_output == point));
	let (index, output) = parent.output.iter().enumerate().find(|(_, output)| output.script_pubkey == fallback.spk).unwrap();
	assert_eq!(output.value, sat(99_000));
	assert!(!parent.output.iter().any(|output| output.script_pubkey == receive_destination.spk));
	let mut value = sat(0);
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		value += previous.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(parent.output.iter().map(|output| output.value).sum::<bitcoin::Amount>(), value);
	let height = ctx.generate_blocks(6).await;
	assert!(chain.get_tx_out(&parent.compute_txid(), index as u32, Some(true)).unwrap().unwrap().confirmations >= 6);
	assert!(chain.get_tx_out(&claim.compute_txid(), 0, Some(true)).unwrap().unwrap().confirmations >= 6);
	srv.wait_for_sync_height(height).await;
	srv.watchmand().wait_for_sync_height(height).await;
	let expected_hash = chain.get_block_hash(height.to_u32() as u64).unwrap();
	let paid_source = sources[0].id().to_string();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let coherent = db.write(async |t| {
				let Some(view) = t.expiry_rebuild_token().await? else { return Ok(false); };
				if view.captaind.height != height || view.watchmand.height != height
					|| view.captaind.hash != expected_hash || view.watchmand.hash != expected_hash
					|| !t.expiry_view_is_rebuilt(view).await?
				{ return Ok(false); }
				t.lock_valid_expiry_view(view).await?;
				let recorded = t.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c \
					JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
					WHERE e.vtxo_id=$1 AND c.active AND a.status='confirmed' AND a.sweep_tx=$2",
					&[&unpaid.to_string(), &bitcoin::consensus::serialize(&parent)]).await?.get::<_, i64>(0);
				if recorded != 1 { return Ok(false); }
				let duplicate = t.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c \
					JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
					WHERE e.vtxo_id=$1 AND c.disposition='customer' AND a.status='confirmed'", &[&paid_source]).await?
					.get::<_, i64>(0);
				assert_eq!(duplicate, 0, "the actual prior user receipt must not receive a second fallback");
				Ok(true)
			}).await.unwrap();
			if coherent { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("no-duplicate oracle requires an actually rebuilt canonical B payout");
}
