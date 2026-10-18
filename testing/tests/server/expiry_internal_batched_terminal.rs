//! Three genuine originals fund one internal receive. A1 and A2 take one
//! ordinary DEFAULT timeout receipt; absent B remains entitled to fallback.
//! V18's single-input terminal-omission guard predicts a HOLD, not a pass.
//! This file is source-only until explicitly registered and qualified.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::{Full, TapScriptClause};
use ark::vtxo::policy::signing::VtxoSigner;
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use bitcoin::{OutPoint, Transaction, TxIn, TxOut, Witness};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::Db;
use server_rpc::protos;

use super::expiry_internal_partial_terminal::{assert_retained_claim, confirm_actual_path, confirm_recoverable_prefix};

fn product(variable: &str, expected: &str) -> PathBuf {
	let path = PathBuf::from(std::env::var_os(variable).expect("the existing runtime product environment is required"));
	assert_eq!(sha256::Hash::hash(&std::fs::read(&path).expect("qualified V18 product must exist")).to_string(), expected,
		"this first diagnostic must use the unchanged qualified V18 products");
	path
}

async fn batched_terminal() {
	let cap = product("CAPTAIND_EXEC","4bc16a305a40d455135bfd2df985d9dcb21a4987523148ec83cfdbae32ce4f25");
	product("WATCHMAND_EXEC","1660f365f1fb14909d4aad7975c171db49b0aa4c351b2a07d71513e7ff750366");
	product("BARK_EXEC","b0fdf36592f29d805ebeaf8dd6d2c92dc309d1be3c0499255590b9f36de45779");
	let ctx = TestContext::new("server/expiry_internal_two_originals_timeout_together_keeps_third_source_owed").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let operator = ctx.bitcoind().get_new_address();
	let drain = operator.clone();
	let srv = Box::pin(ctx.captaind("server").exec(cap).lightningd(&lightning.internal).bitcoind(ctx.bitcoind_arc())
		.funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(24);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget {amount:sat(50_000),count:8}];
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).watchmand().watchmand_cfg(move |cfg|cfg.sweep_address=Some(drain.into_unchecked())).create()).await;
	srv.wait_for_vtxopool(&ctx).await;
	let recipient = Box::pin(ctx.bark_sdk("recipient",&srv).cfg(|cfg|cfg.daemon_manual_sync=true).create()).await;
	let invoice = recipient.bolt11_invoice(sat(280_000),None,None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let receive_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let receive_destination = recipient.fallback_destination().await.unwrap();
	let actor = Box::pin(ctx.bark("sender",&srv).cfg(|cfg|cfg.daemon_manual_sync=true).funded(sat(600_000)).create()).await;
	let mut ids = Vec::new();
	for _ in 0..3 {
		let board = actor.board_and_confirm_and_register(&ctx,sat(100_000)).await;
		assert_eq!(board.len(),1);
		ids.push(board[0]);
	}
	let sender = actor.client().await;
	let mut inputs = Vec::new();
	let mut keys = Vec::new();
	for id in &ids {
		let input = sender.get_full_vtxo(*id).await.unwrap();
		keys.push(sender.get_vtxo_key(&input).await.unwrap());
		inputs.push(input);
	}
	assert_eq!(inputs.iter().map(Vtxo::chain_anchor).collect::<BTreeSet<_>>().len(),3);
	assert!(inputs.iter().all(|input|input.amount()==sat(100_000)));
	let fallback = sender.fallback_destination().await.unwrap();
	fallback.verify(bitcoin::Network::Regtest,sender.mailbox_identifier().as_pubkey()).unwrap();
	assert_ne!(fallback.spk,receive_destination.spk);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip+srv.config().htlc_send_expiry_delta+BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(inputs,
		VtxoPolicy::new_server_htlc_send(keys[0].public_key(),hash,deadline)).unwrap().generate_user_nonces(&keys).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts:protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let sources = builder.user_cosign(&keys,ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap().build_signed_vtxos();
	assert_eq!(sources.len(),3);
	assert!(sources.iter().all(|source|source.amount()==sat(100_000)));
	sender.register_vtxo_transactions_with_server(&sources).await.unwrap();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice:invoice.to_string(),payment_amount_sat:280_000,
		htlc_vtxo_ids:sources.iter().map(|source|source.id().to_bytes().to_vec()).collect(),mailbox_id:None,retry_for_secs:Some(5),
	}).await.unwrap();
	let response = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash:hash.to_vec(),user_pubkey:receive_key.serialize().to_vec(),
		htlc_recv_expiry:(tip+BlockDelta::new(36)).to_u32(),lightning_receive_anti_dos:None,
	}).await.unwrap().into_inner();
	let grants = response.htlc_vtxos.iter().map(|raw|Vtxo::<Full>::deserialize(raw).unwrap()).collect::<Vec<_>>();
	assert!(grants.len()>=2);
	assert_eq!(grants.iter().map(Vtxo::amount).sum::<bitcoin::Amount>(),sat(280_000));
	let grant_ids = grants.iter().map(|grant|grant.id().to_string()).collect::<BTreeSet<_>>();
	let originals = sources.iter().map(|source|source.id().to_string()).collect::<Vec<_>>();
	let paid_ids = originals[..2].to_vec();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let scope = db.read(async |t| {
		let rows = t.query("SELECT f.id,f.subscription_id FROM expiry_lightning_internal_funding f \
			JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id \
			WHERE a.payment_hash=$1",&[&hash.to_string()]).await?;
		assert_eq!(rows.len(),1,"this is one actual attempt, not unrelated same-hash liabilities");
		let attempt:i64=rows[0].get("id"); let subscription:i64=rows[0].get("subscription_id");
		let incoming=t.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
			WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id",&[&attempt]).await?
			.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
		let outgoing=t.query("SELECT v.vtxo_id FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
			WHERE v.lightning_htlc_subscription_id=$1 AND h.direction='outgoing' ORDER BY v.vtxo_id",&[&subscription]).await?
			.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
		Ok((attempt,subscription,incoming,outgoing))
	}).await.unwrap();
	assert_eq!(scope.2,originals.iter().cloned().collect());
	assert_eq!(scope.3,grant_ids);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	srv.watchmand().stop().await.unwrap();
	let mut seen = BTreeSet::new();
	for grant in &grants {confirm_actual_path(&ctx,grant,&mut seen).await;}
	for source in &sources[..2] {confirm_actual_path(&ctx,source,&mut seen).await;}
	let unpaid_prefix = confirm_recoverable_prefix(&ctx,&db,&sources[2],&mut seen).await;
	// Sponsor before user receipt: neither the198k receipt nor the later99k
	// payout is controlled by the Core funding wallet or recycled into fees.
	for _ in 0..4 {ctx.bitcoind().fund_addr(fee_address.clone(),sat(150_000)).await;}
	ctx.generate_blocks(2).await;
	let chain = ctx.bitcoind().sync_client();
	let current = ctx.bitcoind().get_block_count().await as u32;
	let clauses = sources[..2].iter().map(|source|source.policy().as_server_htlc_send().unwrap()
		.user_claim_after_expiry_clause(source.exit_delta())).collect::<Vec<_>>();
	let mut mature = current;
	for (source,clause) in sources[..2].iter().zip(&clauses) {
		let actual = chain.get_tx_out(&source.point().txid,source.point().vout,Some(true)).unwrap().unwrap();
		assert_eq!(actual.value,sat(100_000)); assert!(actual.confirmations>0);
		mature=mature.max(clause.timelock_height.to_u32())
			.max(current+1-u32::try_from(actual.confirmations).unwrap()+clause.block_delta.to_u32());
	}
	// The same finite mine also establishes every real grant timeout/CSV and
	// B prefix expiry/CSV. Publication will need no readiness shortcut/block.
	let prefix = chain.get_tx_out(&unpaid_prefix.txid,unpaid_prefix.vout,Some(true)).unwrap().unwrap();
	assert!(prefix.confirmations>0); assert_eq!(prefix.value,sat(100_000));
	mature=mature.max(sources[2].expiry_height().to_u32())
		.max(current+1-u32::try_from(prefix.confirmations).unwrap()+sources[2].exit_delta().to_u32());
	for grant in &grants {
		let actual=chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(true)).unwrap().unwrap();
		assert!(actual.confirmations>0);
		mature=mature.max(grant.policy().as_server_htlc_recv().unwrap().htlc_expiry.to_u32())
			.max(current+1-u32::try_from(actual.confirmations).unwrap()+grant.exit_delta().to_u32());
	}
	if mature>=current {ctx.generate_blocks(mature-current+1).await;}
	let mut claim = Transaction {version:bitcoin::transaction::Version::TWO,
		lock_time:clauses.iter().max_by_key(|clause|clause.timelock_height).unwrap().locktime(),
		input:sources[..2].iter().zip(&clauses).map(|(source,clause)|TxIn {previous_output:source.point(),
			script_sig:Default::default(),sequence:clause.sequence(),witness:Witness::new()}).collect(),
		output:vec![TxOut {value:sat(198_000),script_pubkey:fallback.spk.clone()}],
	};
	let prevouts=sources[..2].iter().map(Vtxo::txout).collect::<Vec<_>>();
	for (index,(source,clause)) in sources[..2].iter().zip(&clauses).enumerate() {
		claim.input[index].witness=sender.sign_input_with_clause(source,&clause.clone().into(),index,
			&mut SighashCache::new(&claim),&Prevouts::All(&prevouts)).await.unwrap();
		assert_eq!(claim.input[index].witness.len(),3);
		assert_eq!(claim.input[index].witness.iter().next().unwrap().len(),64,"actual DEFAULT signature, no ANYONECANPAY");
		assert_eq!(claim.input[index].witness.taproot_leaf_script().unwrap().script,clause.tapscript().as_script());
	}
	assert_eq!(claim.input.len(),2);
	assert_eq!(claim.input.iter().map(|input|input.previous_output).collect::<BTreeSet<_>>(),
		sources[..2].iter().map(Vtxo::point).collect());
	assert!(claim.weight().to_vbytes_ceil()<=2_000,"real fixed2k fee must meet ordinary relay floor");
	chain.send_raw_transaction(&claim).unwrap();
	let claim_height=ctx.generate_blocks(6).await;
	let receipt=chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap();
	assert_eq!(receipt.value,sat(198_000)); assert!(receipt.confirmations>=6);
	let actual_claim_height=i32::try_from(claim_height.to_u32()+1-u32::try_from(receipt.confirmations).unwrap()).unwrap();
	let claim_info:serde_json::Value=chain.call("getrawtransaction",&[serde_json::json!(claim.compute_txid().to_string()),serde_json::json!(true)]).unwrap();
	assert_eq!(claim_info["blockhash"].as_str().unwrap(),chain.get_block_hash(u64::try_from(actual_claim_height).unwrap()).unwrap().to_string());
	assert_eq!(sat(200_000),claim.output.iter().map(|output|output.value).sum::<bitcoin::Amount>()+sat(2_000));
	assert_eq!(chain.get_tx_out(&unpaid_prefix.txid,unpaid_prefix.vout,Some(true)).unwrap().unwrap().value,sat(100_000));
	for grant in &grants {assert!(chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(true)).unwrap().is_some());}
	drop(sender); drop(recipient); drop(rpc); drop(actor);
	srv.watchmand().start().await.unwrap();
	srv.wait_for_sync_height(claim_height).await;
	srv.watchmand().wait_for_sync_height(claim_height).await;
	tokio::time::timeout(Duration::from_secs(30),async {
		loop {
			if assert_retained_claim(&db,sources[0].id(),&claim).await && assert_retained_claim(&db,sources[1].id(),&claim).await {break;}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both real user sources must link to one actual canonical batch body before G consumption");
	let context=db.read(async |t| {
		let fences=t.query_one("SELECT COUNT(*) FROM (SELECT vtxo_id FROM expiry_decided_coin \
			UNION ALL SELECT vtxo_id FROM expiry_allocation_input) prior WHERE vtxo_id=ANY($1)",&[&paid_ids]).await?.get::<_,i64>(0);
		let paid=t.query("SELECT v.vtxo_id,v.onchain_spent_txid,v.onchain_spent_height,h.chain_resolution::TEXT, \
			h.offchain_resolution::TEXT,h.chain_resolution_height FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=ANY($1) ORDER BY v.vtxo_id",
			&[&paid_ids]).await?;
		let journal=t.query_one("SELECT confirmed_height,confirmed_hash, \
			(SELECT COUNT(*) FROM expiry_user_claim_source WHERE claim_txid=$1) AS links FROM expiry_user_claim_tx WHERE txid=$1",
			&[&claim.compute_txid().to_string()]).await?;
		Ok((fences,paid,journal))
	}).await.unwrap();
	assert_eq!(context.0,0,"this first case has no prior joint reservation or unrelated source fence");
	assert_eq!(context.1.len(),2);
	for row in &context.1 {
		assert_eq!(row.get::<_,Option<&str>>("onchain_spent_txid"),Some(claim.compute_txid().to_string().as_str()));
		assert_eq!(row.get::<_,Option<i32>>("onchain_spent_height"),Some(actual_claim_height));
		assert_eq!(row.get::<_,Option<i32>>("chain_resolution_height"),Some(actual_claim_height));
		assert_eq!(row.get::<_,Option<&str>>("chain_resolution"),Some("revoked"));
		assert!(row.get::<_,Option<&str>>("offchain_resolution").is_none());
	}
	assert_eq!(context.2.get::<_,i64>("links"),2);
	assert_eq!(context.2.get::<_,Option<i64>>("confirmed_height"),Some(i64::from(actual_claim_height)));
	assert_eq!(context.2.get::<_,Option<&str>>("confirmed_hash"),claim_info["blockhash"].as_str());
	for grant in &grants {assert!(chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(false)).unwrap().is_some(),
		"the actual batch raw and both links precede canonical consumption of every G");}
	std::fs::write(srv.config().data_dir.join("batched-terminal-before-publication.json"),serde_json::to_vec_pretty(&serde_json::json!({
		"actual_attempt":scope.0,"subscription":scope.1,"originals":originals,"grants":grant_ids,
		"actual_claim":claim.compute_txid().to_string(),"claim_raw_sha256":sha256::Hash::hash(&serialize(&claim)).to_string(),
		"claim_input_count":2,"claim_receipt_sat":198000,"claim_fee_sat":2000,"actual_claim_depth":receipt.confirmations,
		"b_actual_prefix":unpaid_prefix.to_string(),"prior_A_fences":context.0,"actors_absent":true,
		"source_expectation":"V18 rejects batch terminal omission at claim.input.len()==1; not an observed runtime result"
	})).unwrap()).unwrap();

	// This is a positive rightful-refund oracle. It is expected to fail on
	// unchanged V18; an observed HOLD must never be relabeled as a test PASS.
	let (allocation,parent): (i64,Transaction)=tokio::time::timeout(Duration::from_secs(90),async {
		loop {
			let row=db.read(async |t|Ok(t.query_opt("SELECT a.id,a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.status='pending' \
				AND a.broadcast_at IS NOT NULL AND a.sweep_tx IS NOT NULL",&[&sources[2].id().to_string()]).await?)).await.unwrap();
			if let Some(row)=row {
				let parent:Transaction=deserialize(row.get("sweep_tx")).unwrap();
				if chain.get_mempool_entry(&parent.compute_txid()).is_ok() {break (row.get("id"),parent);}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("a real two-source user receipt must not strand the same attempt's third rightful original");
	let expected_inputs=grants.iter().map(Vtxo::point).chain(std::iter::once(unpaid_prefix)).collect::<BTreeSet<_>>();
	assert_eq!(parent.input.len(),expected_inputs.len());
	assert_eq!(parent.input.iter().map(|input|input.previous_output).collect::<BTreeSet<_>>(),expected_inputs);
	for grant in &grants {
		let input=parent.input.iter().find(|input|input.previous_output==grant.point()).unwrap();
		let script=grant.policy().as_server_htlc_recv().unwrap().server_claim_after_expiry_clause(grant.server_pubkey(),grant.exit_delta()).tapscript();
		assert_eq!(input.witness.taproot_leaf_script().unwrap().script,script.as_script());
	}
	assert!(!parent.output.iter().any(|output|output.script_pubkey==receive_destination.spk));
	let customer=parent.output.iter().enumerate().filter(|(_,output)|output.script_pubkey==fallback.spk).collect::<Vec<_>>();
	assert_eq!(customer.len(),1); assert_eq!(customer[0].1.value,sat(99_000));
	let payout=OutPoint::new(parent.compute_txid(),u32::try_from(customer[0].0).unwrap());
	assert_eq!(parent.output.iter().filter(|output|output.script_pubkey==operator.script_pubkey()).map(|output|output.value).sum::<bitcoin::Amount>(),sat(281_000));
	let anchors=parent.output.iter().enumerate().filter(|(_,output)|output.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
	assert_eq!(anchors.len(),1); assert_eq!(anchors[0].1.value,sat(0));
	let anchor=OutPoint::new(parent.compute_txid(),u32::try_from(anchors[0].0).unwrap());
	let mut input_total=sat(0);
	for input in &parent.input {input_total+=ctx.bitcoind().await_transaction(input.previous_output.txid).await.output[input.previous_output.vout as usize].value;}
	assert_eq!(input_total,sat(380_000));
	assert_eq!(parent.output.iter().map(|output|output.value).sum::<bitcoin::Amount>(),input_total);
	assert_eq!(sat(300_000)+sat(280_000),sat(198_000)+sat(99_000)+sat(281_000)+sat(2_000));
	let height=ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(height).await; srv.watchmand().wait_for_sync_height(height).await;
	let hash=chain.get_block_hash(height.to_u32() as u64).unwrap();
	tokio::time::timeout(Duration::from_secs(30),async {
		loop {
			let finished=db.read(async |t| {
				let Some(view)=t.expiry_rebuild_token().await? else {return Ok(false);};
				if view.captaind.height!=height || view.watchmand.height!=height || view.captaind.hash!=hash || view.watchmand.hash!=hash
					|| !t.expiry_view_is_rebuilt(view).await? {return Ok(false);}
				let row=t.query_one("SELECT status,confirmed_hash FROM expiry_allocation WHERE id=$1",&[&allocation]).await?;
				Ok(row.get::<_,&str>("status")=="confirmed" && row.get::<_,Option<&str>>("confirmed_hash").is_some())
			}).await.unwrap();
			if finished {break;}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("money/no-duplicate checks require a completed actual canonical B allocation");
	let paid=chain.get_tx_out(&payout.txid,payout.vout,Some(true)).unwrap().unwrap();
	assert_eq!(paid.value,sat(99_000)); assert!(paid.confirmations>=6);
	assert!(chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap().confirmations>=6);
	let parent_info:serde_json::Value=chain.call("getrawtransaction",&[serde_json::json!(parent.compute_txid().to_string()),serde_json::json!(true)]).unwrap();
	assert!(parent_info["confirmations"].as_u64().unwrap()>=6);
	let child_row=db.read(async |t|Ok(t.query_one("SELECT txid,raw_tx,fee_sat,confirmed_hash FROM expiry_fee_child \
		WHERE allocation_id=$1 AND confirmed_hash IS NOT NULL",&[&allocation]).await?)).await.unwrap();
	let child:Transaction=deserialize(child_row.get("raw_tx")).unwrap();
	assert_eq!(child_row.get::<_,&str>("txid"),child.compute_txid().to_string());
	assert_eq!(child.input.iter().filter(|input|input.previous_output==anchor).count(),1);
	let child_info:serde_json::Value=chain.call("getrawtransaction",&[serde_json::json!(child.compute_txid().to_string()),serde_json::json!(true)]).unwrap();
	assert!(child_info["confirmations"].as_u64().unwrap()>=6); assert_eq!(child_info["blockhash"],parent_info["blockhash"]);
	assert_eq!(child_row.get::<_,&str>("confirmed_hash"),child_info["blockhash"].as_str().unwrap());
	let mut fee_inputs=sat(0);
	for input in &child.input {fee_inputs+=ctx.bitcoind().await_transaction(input.previous_output.txid).await.output[input.previous_output.vout as usize].value;}
	let actual_fee=fee_inputs-child.output.iter().map(|output|output.value).sum::<bitcoin::Amount>();
	assert!(actual_fee>sat(0)); assert_eq!(actual_fee,sat(u64::try_from(child_row.get::<_,i64>("fee_sat")).unwrap()));
	let recorded=db.read(async |t|Ok(t.query_one("SELECT sweep_txid,sweep_tx,confirmed_hash FROM expiry_allocation WHERE id=$1",&[&allocation]).await?)).await.unwrap();
	assert_eq!(recorded.get::<_,&str>("sweep_txid"),parent.compute_txid().to_string());
	assert_eq!(recorded.get::<_,&[u8]>("sweep_tx"),serialize(&parent).as_slice());
	assert_eq!(recorded.get::<_,&str>("confirmed_hash"),parent_info["blockhash"].as_str().unwrap());
	let final_rows=db.read(async |t| {
		let rows=t.query("SELECT c.evidence::TEXT,e.vtxo_id,c.gross_sat,c.payout_sat,c.fee_sat FROM expiry_allocation_coin c \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE c.allocation_id=$1 AND c.disposition='customer'",&[&allocation]).await?;
		let duplicates=t.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=ANY($1) AND c.disposition='customer' AND a.status='confirmed'",
			&[&paid_ids]).await?.get::<_,i64>(0);
		let discharged=t.query("SELECT v.vtxo_id,v.onchain_spent_txid,h.chain_resolution::TEXT,h.offchain_resolution::TEXT FROM vtxo v \
			JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=ANY($1) ORDER BY v.vtxo_id",
			&[&grant_ids.iter().cloned().collect::<Vec<_>>()]).await?;
		let claims=t.query_one("SELECT (SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE subscription_id=$1) AS claims, \
			(SELECT COUNT(*) FROM expiry_lightning_receive_replay WHERE subscription_id=$1) AS intents",&[&scope.1]).await?;
		let no_duplicate_ids=paid_ids.iter().cloned().chain(grant_ids.iter().cloned()).collect::<Vec<_>>();
		let active_duplicates=t.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			WHERE e.vtxo_id=ANY($1) AND c.disposition='customer' AND c.active",&[&no_duplicate_ids]).await?.get::<_,i64>(0);
		Ok((rows,duplicates,discharged,claims,active_duplicates))
	}).await.unwrap();
	assert_eq!(final_rows.0.len(),1);
	let row=&final_rows.0[0];
	assert_eq!(row.get::<_,&str>("vtxo_id"),sources[2].id().to_string());
	assert_eq!((row.get::<_,i64>("gross_sat"),row.get::<_,i64>("payout_sat"),row.get::<_,i64>("fee_sat")),(100_000,99_000,1_000));
	let evidence:serde_json::Value=serde_json::from_str(row.get("evidence")).unwrap();
	assert_eq!(evidence["attempt_id"].as_i64(),Some(scope.0));
	assert_eq!(evidence["subscription_id"].as_i64(),Some(scope.1));
	let terminals=evidence["terminal_sources"].as_array().unwrap();
	assert_eq!(terminals.len(),2);
	assert_eq!(terminals.iter().map(|item|item["source"].as_str().unwrap().to_owned()).collect::<BTreeSet<_>>(),paid_ids.iter().cloned().collect());
	assert!(terminals.iter().all(|item|item["claim_txid"].as_str()==Some(claim.compute_txid().to_string().as_str())));
	assert_eq!(final_rows.1,0,"the198k prior user receipt cannot also earn a customer fallback");
	assert_eq!(final_rows.2.len(),grants.len());
	for row in &final_rows.2 {
		assert_eq!(row.get::<_,Option<&str>>("onchain_spent_txid"),Some(parent.compute_txid().to_string().as_str()));
		assert_eq!(row.get::<_,Option<&str>>("chain_resolution"),Some("revoked"));
		assert!(row.get::<_,Option<&str>>("offchain_resolution").is_none(),"actual grant timeout cannot be manufactured as off-chain cancellation");
	}
	assert_eq!(final_rows.3.get::<_,i64>("claims"),0); assert_eq!(final_rows.3.get::<_,i64>("intents"),0);
	assert_eq!(final_rows.4,0,"already-received A and consensus-spent G cannot retain another active customer obligation");
	assert!(assert_retained_claim(&db,sources[0].id(),&claim).await);
	assert!(assert_retained_claim(&db,sources[1].id(),&claim).await);
	std::fs::write(srv.config().data_dir.join("batched-terminal-confirmed.json"),serde_json::to_vec_pretty(&serde_json::json!({
		"attempt":scope.0,"subscription":scope.1,"originals":originals,"grants":grant_ids,"claim":claim.compute_txid().to_string(),
		"b_parent":parent_info,"fee_child":child_info,"actual_child_fee_sat":actual_fee.to_sat(),"same_raw_bodies":true,
		"scope":"fresh unfenced same-attempt complete batch only; no prior-fence/foreign-input/reorg/general-batch credit"
	})).unwrap()).unwrap();
}

#[tokio::test]
async fn expiry_internal_two_originals_timeout_together_keeps_third_source_owed() {
	tokio::time::timeout(Duration::from_secs(180),Box::pin(batched_terminal())).await
		.expect("actual batched-terminal third-source refund exceeded proposed180second total");
}
