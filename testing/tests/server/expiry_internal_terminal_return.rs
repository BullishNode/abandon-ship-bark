//! Existing joint reservations survive genuine terminal user receipts. They
//! cannot permanently block the remaining operator timeout value or the user's
//! exact retained authorization after a fork. No financial state is fabricated.

use std::collections::BTreeSet;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::{Full, TapScriptClause};
use ark::vtxo::policy::signing::VtxoSigner;
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use ark_testing::daemon::captaind::Captaind;
use bitcoin::{Transaction, TxIn, TxOut, Witness};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::Db;
use server_rpc::protos;

use super::expiry_internal_partial_terminal::{assert_retained_claim, confirm_actual_path, confirm_recoverable_prefix, wait_partial_confirmation};

#[derive(Clone)]
struct OriginalReservation {
	id:i64,
	parent:Transaction,
	inputs:BTreeSet<String>,
	entitlements:BTreeSet<i64>,
}

async fn reserved_terminal(reorg:bool,partial:bool) {
	let name=match (reorg,partial) {
		(false,false)=>"server/expiry_internal_reserved_all_terminal_returns_grants",
		(true,false)=>"server/expiry_internal_reserved_all_terminal_replays_after_reorg",
		(false,true)=>"server/expiry_internal_reserved_partial_pays_remaining_source",
		(true,true)=>"server/expiry_internal_reserved_partial_replays_paid_source_after_reorg",
	};
	let ctx=TestContext::new(name).await;
	let lightning=ctx.new_lightning_setup_no_channel("lightningd").await;
	let operator=ctx.bitcoind().get_new_address();
	let drain=operator.clone();
	let srv=Box::pin(ctx.captaind("server").lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime=BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime=BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry=BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets=vec![server::vtxopool::VtxoTarget {amount:sat(50_000),count:8}];
			cfg.invoice_check_interval=Duration::from_secs(1);
		}).watchmand().watchmand_cfg(move |cfg|cfg.sweep_address=Some(drain.into_unchecked())).create()).await;
	srv.wait_for_vtxopool(&ctx).await;
	let recipient=Box::pin(ctx.bark_sdk("recipient",&srv).cfg(|cfg|cfg.daemon_manual_sync=true).create()).await;
	let invoice=recipient.bolt11_invoice(sat(180_000),None,None).await.unwrap();
	let hash=PaymentHash::from(&invoice);
	let receive_key=recipient.new_address().await.unwrap().policy().user_pubkey();
	let actor=Box::pin(ctx.bark("sender",&srv).cfg(|cfg|cfg.daemon_manual_sync=true).funded(sat(400_000)).create()).await;
	let a=actor.board_and_confirm_and_register(&ctx,sat(100_000)).await;
	let b=actor.board_and_confirm_and_register(&ctx,sat(100_000)).await;
	assert_eq!(a.len(),1); assert_eq!(b.len(),1);
	let sender=actor.client().await;
	let inputs=vec![sender.get_full_vtxo(a[0]).await.unwrap(),sender.get_full_vtxo(b[0]).await.unwrap()];
	assert_ne!(inputs[0].chain_anchor(),inputs[1].chain_anchor());
	let keys=vec![sender.get_vtxo_key(&inputs[0]).await.unwrap(),sender.get_vtxo_key(&inputs[1]).await.unwrap()];
	let fallback=sender.fallback_destination().await.unwrap();
	let tip=BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline=tip+srv.config().htlc_send_expiry_delta+BlockDelta::new(2);
	let builder=ArkoorPackageBuilder::new_claim_all_with_checkpoints(inputs,
		VtxoPolicy::new_server_htlc_send(keys[0].public_key(),hash,deadline)).unwrap().generate_user_nonces(&keys).unwrap();
	let mut rpc=srv.get_public_rpc().await;
	let reply=rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts:protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let sources=builder.user_cosign(&keys,ArkoorPackageCosignResponse::try_from(reply).unwrap()).unwrap().build_signed_vtxos();
	assert_eq!(sources.len(),2);
	assert!(sources.iter().all(|source|source.amount()==sat(100_000)));
	sender.register_vtxo_transactions_with_server(&sources).await.unwrap();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {invoice:invoice.to_string(),payment_amount_sat:180_000,
		htlc_vtxo_ids:sources.iter().map(|source|source.id().to_bytes().to_vec()).collect(),mailbox_id:None,retry_for_secs:Some(5)}).await.unwrap();
	let reply=rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash:hash.to_vec(),user_pubkey:receive_key.serialize().to_vec(),htlc_recv_expiry:(tip+BlockDelta::new(36)).to_u32(),
		lightning_receive_anti_dos:None,
	}).await.unwrap().into_inner();
	let grants=reply.htlc_vtxos.iter().map(|raw|Vtxo::<Full>::deserialize(raw).unwrap()).collect::<Vec<_>>();
	assert_eq!(grants.iter().map(Vtxo::amount).sum::<bitcoin::Amount>(),sat(180_000));
	let db=Db::connect(&srv.config().postgres).await.unwrap();
	let original_ids=sources.iter().map(|source|source.id().to_string()).collect::<Vec<_>>();
	let grant_ids=grants.iter().map(|grant|grant.id().to_string()).collect::<BTreeSet<_>>();
	let scope=db.read(async |t| {
		let rows=t.query("SELECT f.id,f.subscription_id FROM expiry_lightning_internal_funding f \
			JOIN lightning_payment_attempt a ON a.id=f.id WHERE a.payment_hash=$1",&[&hash.to_string()]).await?;
		assert_eq!(rows.len(),1);
		let attempt:i64=rows[0].get("id"); let subscription:i64=rows[0].get("subscription_id");
		let originals=t.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id",&[&attempt]).await?
			.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
		let issued=t.query("SELECT vtxo_id FROM vtxo WHERE lightning_htlc_subscription_id=$1 ORDER BY vtxo_id",&[&subscription]).await?
			.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
		Ok((attempt,subscription,originals,issued))
	}).await.unwrap();
	assert_eq!(scope.2,original_ids.iter().cloned().collect()); assert_eq!(scope.3,grant_ids);
	let fee_address=srv.watchmand().wait_wallet_address().await;
	srv.watchmand().stop().await.unwrap();
	let mut seen=BTreeSet::new();
	for grant in &grants {confirm_actual_path(&ctx,grant,&mut seen).await;}
	let mut prefixes=Vec::new();
	for source in &sources {prefixes.push(confirm_recoverable_prefix(&ctx,&db,source,&mut seen).await);}
	let chain=ctx.bitcoind().sync_client();
	let current=ctx.bitcoind().get_block_count().await as u32;
	let mut mature=current;
	for grant in &grants {
		let output=chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(true)).unwrap().unwrap();
		mature=mature.max(grant.policy().as_server_htlc_recv().unwrap().htlc_expiry.to_u32())
			.max(current+1-u32::try_from(output.confirmations).unwrap()+grant.exit_delta().to_u32());
	}
	for source in &sources {mature=mature.max(source.expiry_height().to_u32());}
	if mature>=current {ctx.generate_blocks(mature-current+1).await;}
	srv.watchmand().start().await.unwrap();
	// The watchman intentionally has NO confirmed fee sponsor yet. This is a
	// genuine financial commit with actual inputs, not a fabricated reservation.
	let reserved=tokio::time::timeout(Duration::from_secs(150),async {
		loop {
			let row=db.read(async |t|Ok(t.query_opt("SELECT a.id,a.sweep_tx FROM expiry_allocation a WHERE a.status='pending' \
				AND a.sweep_tx IS NOT NULL AND a.broadcast_at IS NULL AND (SELECT COUNT(*) FROM expiry_allocation_coin c \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE c.allocation_id=a.id AND e.vtxo_id=ANY($1) \
				AND c.disposition='customer' AND c.evidence->>'kind'='htlc_internal_joint_timeout')=2 ORDER BY a.id LIMIT 1",&[&original_ids]).await?)).await.unwrap();
			if let Some(row)=row {
				let id=row.get::<_,i64>("id");
				let parent:Transaction=deserialize(row.get("sweep_tx")).unwrap();
				let rows=db.read(async |t| {
					let physical=t.query("SELECT vtxo_id FROM expiry_allocation_input WHERE allocation_id=$1 ORDER BY vtxo_id",&[&id]).await?
						.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
					let entitlements=t.query("SELECT entitlement_id FROM expiry_allocation_coin WHERE allocation_id=$1 ORDER BY entitlement_id",&[&id]).await?
						.into_iter().map(|row|row.get::<_,i64>(0)).collect::<BTreeSet<_>>();
					Ok((physical,entitlements))
				}).await.unwrap();
				assert!(grant_ids.is_subset(&rows.0));
				assert!(prefixes.iter().all(|point|rows.0.contains(&point.to_string())));
				assert_eq!(rows.0,parent.input.iter().map(|input|input.previous_output.to_string()).collect());
				assert_eq!(rows.1.len(),2);
				assert_eq!(parent.output.iter().filter(|output|output.script_pubkey==fallback.spk).map(|output|output.value).sum::<bitcoin::Amount>(),sat(199_000));
				assert!(chain.get_mempool_entry(&parent.compute_txid()).is_err());
				return OriginalReservation {id,parent,inputs:rows.0,entitlements:rows.1};
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("normal all-input joint financial reservation was not actually committed");
	srv.watchmand().stop().await.unwrap();
	// All physical conflicts and fee UTXOs precede the user-claim fork. Thus
	// old reservations remain genuinely conflicted even when those claims are
	// unconfirmed; neither test nor product assigns that status by hand.
	for source in sources.iter().take(if partial {1}else{sources.len()}) {confirm_actual_path(&ctx,source,&mut seen).await;}
	for _ in 0..4 {ctx.bitcoind().fund_addr(fee_address.clone(),sat(150_000)).await;}
	ctx.generate_blocks(2).await;
	let current=ctx.bitcoind().get_block_count().await as u32;
	let mut mature=current;
	for source in sources.iter().take(if partial {1}else{sources.len()}) {
		let prior=chain.get_tx_out(&source.point().txid,source.point().vout,Some(true)).unwrap().unwrap();
		let clause=source.policy().as_server_htlc_send().unwrap().user_claim_after_expiry_clause(source.exit_delta());
		mature=mature.max(clause.timelock_height.to_u32()).max(current+1-u32::try_from(prior.confirmations).unwrap()+clause.block_delta.to_u32());
	}
	if mature>=current {ctx.generate_blocks(mature-current+1).await;}
	let mut claims=Vec::new();
	for source in sources.iter().take(if partial {1}else{sources.len()}) {
		let clause=source.policy().as_server_htlc_send().unwrap().user_claim_after_expiry_clause(source.exit_delta());
		let mut claim=Transaction {version:bitcoin::transaction::Version::TWO,lock_time:clause.locktime(),
			input:vec![TxIn {previous_output:source.point(),script_sig:Default::default(),sequence:clause.sequence(),witness:Witness::new()}],
			output:vec![TxOut {value:sat(98_000),script_pubkey:ctx.bitcoind().get_new_address().script_pubkey()}]};
		let witness=sender.sign_input_with_clause(source,&clause.into(),0,
			&mut SighashCache::new(&claim),&Prevouts::All(&[source.txout()])).await.unwrap();
		claim.input[0].witness=witness;
		chain.send_raw_transaction(&claim).unwrap(); claims.push(claim);
	}
	ctx.generate_blocks(6).await;
	for claim in &claims {assert!(chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap().confirmations>=6);}
	drop(sender); drop(recipient); drop(rpc);
	srv.watchmand().start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(30),async {
		loop {
			let mut all=true;
			for (source,claim) in sources.iter().zip(&claims) {all &= assert_retained_claim(&db,source.id(),claim).await;}
			if all {break;}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("every actual user raw authorization must precede canonical grant consumption");
	for grant in &grants {assert!(chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(false)).unwrap().is_some());}
	if partial {
		let parent=confirm_reserved_partial(&ctx,&srv,&db,&sources,&claims[0],&grants,&fallback.spk,&operator,&reserved,None).await;
		if reorg {replay_reserved_partial(&ctx,&srv,&db,&sources,&claims[0],&grants,&fallback.spk,&operator,&reserved,&parent).await;}
		return;
	}
	let first_returns=confirm_operator_returns(&ctx,&srv,&db,&sources,&claims,&grants,&operator,&reserved).await;
	if !reorg {return;}
	let claim_block=chain.custom_get_raw_transaction_info(claims[0].compute_txid(),None).unwrap().unwrap().blockhash.unwrap();
	assert_eq!(chain.custom_get_raw_transaction_info(claims[1].compute_txid(),None).unwrap().unwrap().blockhash,Some(claim_block));
	let old_height=chain.get_block_count().unwrap();
	let mining_address=ctx.bitcoind().get_new_address();
	srv.watchmand().stop().await.unwrap(); srv.stop().await.unwrap();
	chain.invalidate_block(&claim_block).unwrap();
	let fork_height=chain.get_block_count().unwrap(); assert!(fork_height<old_height);
	ctx.bitcoind().stop().await.unwrap();
	let cache=ctx.bitcoind().datadir().join("regtest/mempool.dat");
	let bytes=std::fs::read(&cache).unwrap();
	let preserved=ctx.bitcoind().datadir().join("reserved-terminal-before-replay.mempool.dat");
	std::fs::rename(cache,&preserved).unwrap();
	let hash=sha256::Hash::hash(&bytes);
	assert_eq!(sha256::Hash::hash(&std::fs::read(&preserved).unwrap()),hash);
	std::fs::write(ctx.bitcoind().datadir().join("reserved-terminal-before-replay.sha256"),hash.to_string()).unwrap();
	ctx.bitcoind().start().await.unwrap();
	let chain=ctx.bitcoind().sync_client();
	for (source,claim) in sources.iter().zip(&claims) {
		assert!(chain.get_mempool_entry(&claim.compute_txid()).is_err());
		assert!(chain.get_tx_out(&source.point().txid,source.point().vout,Some(true)).unwrap().is_some());
		assert!(assert_retained_claim(&db,source.id(),claim).await);
	}
	for txid in &first_returns {
		assert!(!chain.custom_get_raw_transaction_info(*txid,None).unwrap().is_some_and(|info|info.confirmations.is_some_and(|n|n>0)));
	}
	srv.start().await.unwrap(); srv.watchmand().start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(30),async {
		while !claims.iter().all(|claim|chain.get_mempool_entry(&claim.compute_txid()).is_ok()) {
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("prior joint reservations must not prevent exact rightful user claim replay");
	for claim in &claims {assert_eq!(ctx.bitcoind().await_transaction(claim.compute_txid()).await,*claim);}
	// Fresh coinbase destination avoids recreating a deliberately invalidated
	// block. Same replacement-height count; no fresh sponsor or financial retry.
	chain.generate_to_address(old_height-fork_height+1,&mining_address).unwrap();
	confirm_operator_returns(&ctx,&srv,&db,&sources,&claims,&grants,&operator,&reserved).await;
	for claim in &claims {
		let info=chain.custom_get_raw_transaction_info(claim.compute_txid(),None).unwrap().unwrap();
		assert_ne!(info.blockhash,Some(claim_block)); assert!(info.confirmations.unwrap()>=6);
	}
}

async fn confirm_operator_returns(
	ctx:&TestContext,srv:&Captaind,db:&Db,sources:&[Vtxo<Full>],claims:&[Transaction],grants:&[Vtxo<Full>],
	operator:&bitcoin::Address,reserved:&OriginalReservation,
) -> BTreeSet<bitcoin::Txid> {
	let chain=ctx.bitcoind().sync_client();
	let ids=grants.iter().map(|grant|grant.id().to_string()).collect::<Vec<_>>();
	let spenders=tokio::time::timeout(Duration::from_secs(150),async {
		for _ in 0..100 {
			let height=ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(height).await; srv.watchmand().wait_for_sync_height(height).await;
			let rows=db.read(async |t|Ok(t.query("SELECT v.vtxo_id,v.onchain_spent_txid,h.chain_resolution::TEXT \
				FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=ANY($1)",&[&ids]).await?)).await.unwrap();
			if rows.len()==grants.len() && rows.iter().all(|row|row.get::<_,Option<&str>>("chain_resolution")==Some("revoked")
				&& row.get::<_,Option<&str>>("onchain_spent_txid").is_some()) {
				let txids=rows.iter().map(|row|row.get::<_,&str>("onchain_spent_txid").parse::<bitcoin::Txid>().unwrap()).collect::<BTreeSet<_>>();
				if txids.iter().all(|txid|chain.custom_get_raw_transaction_info(*txid,None).unwrap()
					.is_some_and(|info|info.confirmations.is_some_and(|n|n>0))) {return txids;}
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
		panic!("exact conflicted joint reservations stranded operator grants after every original user receipt");
	}).await.expect("existing-authorization operator timeout recovery exceeded its bound");
	let mut covered=BTreeSet::new(); let mut total=sat(0); let mut returned=sat(0); let mut fees=sat(0);
	for txid in &spenders {
		let tx=ctx.bitcoind().await_transaction(*txid).await;
		assert_eq!(tx.version,bitcoin::transaction::Version::TWO);
		assert_eq!(tx.output.len(),1); assert_eq!(tx.output[0].script_pubkey,operator.script_pubkey());
		let mut actual=sat(0);
		for input in &tx.input {
			let parent=ctx.bitcoind().await_transaction(input.previous_output.txid).await;
			actual+=parent.output[input.previous_output.vout as usize].value;
			if let Some(grant)=grants.iter().find(|grant|grant.point()==input.previous_output) {
				assert!(covered.insert(grant.id()));
				let script=grant.policy().as_server_htlc_recv().unwrap()
					.server_claim_after_expiry_clause(grant.server_pubkey(),grant.exit_delta()).tapscript();
				assert_eq!(input.witness.taproot_leaf_script().unwrap().script,script.as_script());
			}
		}
		assert!(actual>tx.output[0].value); total+=actual; returned+=tx.output[0].value; fees+=actual-tx.output[0].value;
	}
	assert_eq!(covered,grants.iter().map(Vtxo::id).collect());
	assert!(total>=sat(180_000)); assert_eq!(total,returned+fees);
	// Extra ordinary operator inputs, if grouped by the existing watchman,
	// are included in this full fee cut, not mislabelled as grant-only fees.
	let height=ctx.generate_blocks(6).await;
	for txid in &spenders {assert!(chain.get_tx_out(txid,0,Some(true)).unwrap().unwrap().confirmations>=6);}
	for (source,claim) in sources.iter().zip(claims) {
		assert_eq!(ctx.bitcoind().await_transaction(claim.compute_txid()).await,*claim);
		assert!(chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap().confirmations>=6);
		assert!(assert_retained_claim(db,source.id(),claim).await);
	}
	srv.wait_for_sync_height(height).await; srv.watchmand().wait_for_sync_height(height).await;
	let expected_hash=chain.get_block_hash(height.to_u32() as u64).unwrap();
	tokio::time::timeout(Duration::from_secs(30),async {
		loop {
			let rebuilt=db.read(async |t| {
				let Some(view)=t.expiry_rebuild_token().await? else {return Ok(false);};
				Ok(view.captaind.height==height && view.watchmand.height==height && view.captaind.hash==expected_hash
					&& view.watchmand.hash==expected_hash && t.expiry_view_is_rebuilt(view).await?)
			}).await.unwrap();
			if rebuilt {break;} tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("reserved-return money assertions require a completed canonical rebuild");
	let originals=sources.iter().map(|source|source.id().to_string()).collect::<Vec<_>>();
	db.read(async |t| {
		let row=t.query_one("SELECT status,sweep_tx FROM expiry_allocation WHERE id=$1",&[&reserved.id]).await?;
		assert_eq!(row.get::<_,&str>("status"),"conflicted");
		assert_eq!(row.get::<_,&[u8]>("sweep_tx"),serialize(&reserved.parent).as_slice());
		let physical=t.query("SELECT vtxo_id FROM expiry_allocation_input WHERE allocation_id=$1 ORDER BY vtxo_id",&[&reserved.id]).await?
			.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
		assert_eq!(physical,reserved.inputs,"operator recovery must not delete or rewrite original physical reservations");
		let entitlements=t.query("SELECT entitlement_id FROM expiry_allocation_coin WHERE allocation_id=$1 ORDER BY entitlement_id",&[&reserved.id]).await?
			.into_iter().map(|row|row.get::<_,i64>(0)).collect::<BTreeSet<_>>();
		assert_eq!(entitlements,reserved.entitlements);
		let decided=t.query("SELECT entitlement_id FROM expiry_decided_coin WHERE vtxo_id=ANY($1)",&[&originals]).await?
			.into_iter().map(|row|row.get::<_,i64>(0)).collect::<BTreeSet<_>>();
		assert_eq!(decided,reserved.entitlements,"economic fences survive the operator return and fork");
		for (source,claim) in sources.iter().zip(claims) {
			let row=t.query_one("SELECT c.paying_txid FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE c.allocation_id=$1 AND e.vtxo_id=$2",&[&reserved.id,&source.id().to_string()]).await?;
			assert_eq!(row.get::<_,Option<&str>>("paying_txid"),Some(claim.compute_txid().to_string().as_str()));
		}
		let duplicate=t.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=ANY($1) AND c.disposition='customer' AND a.status='confirmed'",
			&[&originals]).await?.get::<_,i64>(0);
		assert_eq!(duplicate,0,"genuine prior user receipts cannot also receive a confirmed customer fallback");
		Ok(())
	}).await.unwrap();
	assert!(!chain.custom_get_raw_transaction_info(reserved.parent.compute_txid(),None).unwrap()
		.is_some_and(|info|info.confirmations.is_some_and(|n|n>0)));
	spenders
}

#[tokio::test]
async fn expiry_internal_reserved_all_terminal_returns_grants() {
	Box::pin(reserved_terminal(false,false)).await;
}

#[tokio::test]
async fn expiry_internal_reserved_all_terminal_replays_after_reorg() {
	Box::pin(reserved_terminal(true,false)).await;
}

/// Preserve every original fence while replacing only B's conflicted physical
/// reservation. The controller's pre-claim sponsors remain canonical on a fork.
async fn confirm_reserved_partial(
	ctx:&TestContext,srv:&Captaind,db:&Db,sources:&[Vtxo<Full>],claim:&Transaction,grants:&[Vtxo<Full>],
	fallback:&bitcoin::ScriptBuf,operator:&bitcoin::Address,reserved:&OriginalReservation,expected:Option<&Transaction>,
) -> Transaction {
	let chain=ctx.bitcoind().sync_client();
	let unpaid=sources[1].id();
	let parent:Transaction=tokio::time::timeout(Duration::from_secs(150),async {
		for _ in 0..100 {
			let height=ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(height).await; srv.watchmand().wait_for_sync_height(height).await;
			let row=db.read(async |t|Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND a.id<>$2 AND a.status IN ('pending','confirmed') AND a.broadcast_at IS NOT NULL ORDER BY a.id DESC LIMIT 1",
				&[&unpaid.to_string(),&reserved.id]).await?)).await.unwrap();
			if let Some(row)=row {
				let parent:Transaction=deserialize(row.get("sweep_tx")).unwrap();
				// A retained broadcast timestamp can outlive the old branch.
				// Require actual current presence before counting confirmation blocks.
				let canonical=chain.custom_get_raw_transaction_info(parent.compute_txid(),None).unwrap()
					.is_some_and(|info|info.confirmations.is_some_and(|n|n>0));
				if canonical || chain.get_mempool_entry(&parent.compute_txid()).is_ok() {return parent;}
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
		panic!("the actual prior reservation stranded B after A's genuine user receipt");
	}).await.expect("prior-reserved partial closure exceeded the unchanged payout bound");
	if let Some(expected)=expected {assert_eq!(&parent,expected,"a fork must reuse B's exact signed parent");}
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await,parent);
	let mut path=sources[1].transactions().map(|step|
		bitcoin::OutPoint::new(step.tx.compute_txid(),step.output_idx as u32)).collect::<BTreeSet<_>>();
	path.insert(sources[1].chain_anchor()); path.remove(&sources[1].point());
	assert!(parent.input.iter().any(|input|path.contains(&input.previous_output)));
	assert!(!parent.input.iter().any(|input|input.previous_output==sources[0].point()));
	for grant in grants {
		let input=parent.input.iter().find(|input|input.previous_output==grant.point()).unwrap();
		let script=grant.policy().as_server_htlc_recv().unwrap()
			.server_claim_after_expiry_clause(grant.server_pubkey(),grant.exit_delta()).tapscript();
		assert_eq!(input.witness.taproot_leaf_script().unwrap().script,script.as_script());
	}
	assert_eq!(parent.input.len(),grants.len()+1);
	let mut actual=sat(0);
	for input in &parent.input {
		let previous=ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		actual+=previous.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(actual,sat(280_000));
	assert_eq!(parent.output.iter().map(|output|output.value).sum::<bitcoin::Amount>(),actual);
	assert_eq!(parent.output.iter().filter(|output|&output.script_pubkey==fallback).map(|output|output.value).sum::<bitcoin::Amount>(),sat(99_000));
	assert_eq!(parent.output.iter().filter(|output|output.script_pubkey==operator.script_pubkey()).map(|output|output.value).sum::<bitcoin::Amount>(),sat(181_000));
	assert_eq!(parent.output.len(),3);
	let index=parent.output.iter().position(|output|&output.script_pubkey==fallback).unwrap();
	let height=ctx.generate_blocks(6).await;
	assert!(chain.get_tx_out(&parent.compute_txid(),index as u32,Some(true)).unwrap().unwrap().confirmations>=6);
	assert!(chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap().confirmations>=6);
	wait_partial_confirmation(ctx,srv,db,height,sources[0].id(),claim.compute_txid(),unpaid,&parent).await;
	assert!(assert_retained_claim(db,sources[0].id(),claim).await);
	db.read(async |t| {
		let row=t.query_one("SELECT status,sweep_tx FROM expiry_allocation WHERE id=$1",&[&reserved.id]).await?;
		assert_eq!(row.get::<_,&str>("status"),"conflicted");
		assert_eq!(row.get::<_,&[u8]>("sweep_tx"),serialize(&reserved.parent).as_slice());
		let physical=t.query("SELECT vtxo_id FROM expiry_allocation_input WHERE allocation_id=$1 ORDER BY vtxo_id",&[&reserved.id]).await?
			.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
		assert_eq!(physical,reserved.inputs);
		let entitlements=t.query("SELECT entitlement_id FROM expiry_allocation_coin WHERE allocation_id=$1 ORDER BY entitlement_id",&[&reserved.id]).await?
			.into_iter().map(|row|row.get::<_,i64>(0)).collect::<BTreeSet<_>>();
		assert_eq!(entitlements,reserved.entitlements);
		let paid=t.query_one("SELECT c.paying_txid FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			WHERE c.allocation_id=$1 AND e.vtxo_id=$2",&[&reserved.id,&sources[0].id().to_string()]).await?;
		assert_eq!(paid.get::<_,Option<&str>>("paying_txid"),Some(claim.compute_txid().to_string().as_str()));
		let row=t.query_one("SELECT c.entitlement_id,c.evidence::TEXT FROM expiry_allocation_coin c \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
			WHERE e.vtxo_id=$1 AND c.active AND a.status='confirmed' AND a.sweep_tx=$2",
			&[&unpaid.to_string(),&serialize(&parent)]).await?;
		assert!(reserved.entitlements.contains(&row.get::<_,i64>("entitlement_id")));
		let evidence:serde_json::Value=serde_json::from_str(row.get("evidence"))?;
		assert_eq!(evidence["terminal_sources"][0]["prior_joint"],true);
		assert_eq!(evidence["required_grants"].as_array().unwrap().iter().map(|id|id.as_str().unwrap().to_owned()).collect::<BTreeSet<_>>(),
			grants.iter().map(|grant|grant.id().to_string()).collect());
		let count=t.query_one("SELECT COUNT(*) FROM expiry_decided_coin WHERE entitlement_id=ANY($1)",
			&[&reserved.entitlements.iter().copied().collect::<Vec<_>>()]).await?.get::<_,i64>(0);
		assert_eq!(count,2,"the original economic fences must survive the dependent payout");
		Ok(())
	}).await.unwrap();
	parent
}

async fn replay_reserved_partial(
	ctx:&TestContext,srv:&Captaind,db:&Db,sources:&[Vtxo<Full>],claim:&Transaction,grants:&[Vtxo<Full>],
	fallback:&bitcoin::ScriptBuf,operator:&bitcoin::Address,reserved:&OriginalReservation,parent:&Transaction,
) {
	let chain=ctx.bitcoind().sync_client();
	let claim_block=chain.custom_get_raw_transaction_info(claim.compute_txid(),None).unwrap().unwrap().blockhash.unwrap();
	let old_height=chain.get_block_count().unwrap();
	let mining_address=ctx.bitcoind().get_new_address();
	srv.watchmand().stop().await.unwrap(); srv.stop().await.unwrap();
	chain.invalidate_block(&claim_block).unwrap();
	let fork_height=chain.get_block_count().unwrap(); assert!(fork_height<old_height);
	ctx.bitcoind().stop().await.unwrap();
	let cache=ctx.bitcoind().datadir().join("regtest/mempool.dat");
	let bytes=std::fs::read(&cache).unwrap();
	let preserved=ctx.bitcoind().datadir().join("reserved-partial-before-replay.mempool.dat");
	std::fs::rename(cache,&preserved).unwrap();
	let hash=sha256::Hash::hash(&bytes);
	assert_eq!(sha256::Hash::hash(&std::fs::read(&preserved).unwrap()),hash);
	std::fs::write(ctx.bitcoind().datadir().join("reserved-partial-before-replay.sha256"),hash.to_string()).unwrap();
	ctx.bitcoind().start().await.unwrap();
	let chain=ctx.bitcoind().sync_client();
	assert!(chain.get_mempool_entry(&claim.compute_txid()).is_err());
	assert!(chain.get_tx_out(&sources[0].point().txid,sources[0].point().vout,Some(true)).unwrap().is_some());
	assert!(assert_retained_claim(db,sources[0].id(),claim).await);
	assert!(!chain.custom_get_raw_transaction_info(parent.compute_txid(),None).unwrap()
		.is_some_and(|info|info.confirmations.is_some_and(|n|n>0)));
	srv.start().await.unwrap(); srv.watchmand().start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(30),async {
		while chain.get_mempool_entry(&claim.compute_txid()).is_err() {tokio::time::sleep(Duration::from_millis(50)).await;}
	}).await.expect("a dependent B reservation must not block exact original A user replay");
	assert_eq!(ctx.bitcoind().await_transaction(claim.compute_txid()).await,*claim);
	chain.generate_to_address(old_height-fork_height+1,&mining_address).unwrap();
	confirm_reserved_partial(ctx,srv,db,sources,claim,grants,fallback,operator,reserved,Some(parent)).await;
	let current=chain.custom_get_raw_transaction_info(claim.compute_txid(),None).unwrap().unwrap();
	assert!(current.confirmations.unwrap()>=6); assert_ne!(current.blockhash,Some(claim_block));
	assert!(assert_retained_claim(db,sources[0].id(),claim).await);
}

#[tokio::test]
async fn expiry_internal_reserved_partial_pays_remaining_source() {
	Box::pin(reserved_terminal(false,true)).await;
}

#[tokio::test]
async fn expiry_internal_reserved_partial_replays_paid_source_after_reorg() {
	Box::pin(reserved_terminal(true,true)).await;
}
