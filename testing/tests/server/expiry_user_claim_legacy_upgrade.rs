//! A real pre-retention database upgrades in place with seventeen funded
//! timeout observations. This is finite-epoch coverage, not an incompatible
//! witness or sustained-arrival starvation reproduction.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::{Full, TapScriptClause};
use ark::vtxo::policy::signing::VtxoSigner;
use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, Vtxo, VtxoPolicy};
use ark_testing::{btc, sat, TestContext};
use bitcoin::{OutPoint, Transaction, TxIn, TxOut, Txid, Witness};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::Db;
use server_rpc::protos;
use tokio_postgres::{Client, NoTls};

use super::expiry_internal_partial_terminal::{assert_retained_claim, confirm_actual_path, wait_partial_confirmation};

const OLD_ROOT: &str = "/fresh-cache/runtime-sponsor-terminal-v10-20261018";
const NEW_ROOT: &str = "/fresh-cache/runtime-readiness-and-partial-intent-v18-20261018";

fn pinned_product(root: &str, name: &str, expected: &str) -> PathBuf {
	let path = Path::new(root).join(name);
	assert_eq!(sha256::Hash::hash(&std::fs::read(&path).expect("retained product must exist")).to_string(), expected,
		"the per-instance upgrade product must match its immutable capture");
	path
}

/// This connection can SELECT old schema facts without Db::connect running the
/// CURRENT migrations. It performs no financial or schema writes.
async fn old_read_only(config: &server::config::Postgres) -> (Client, tokio::task::JoinHandle<Result<(),tokio_postgres::Error>>) {
	let mut config = Db::config(&config.name, config);
	config.options("-c default_transaction_read_only=on -c statement_timeout=10000");
	let (client, connection) = config.connect(NoTls).await.unwrap();
	let driver = tokio::spawn(connection);
	assert_eq!(client.query_one("SHOW transaction_read_only", &[]).await.unwrap().get::<_,&str>(0), "on");
	(client, driver)
}

#[derive(Debug, PartialEq, Eq)]
struct LegacyFact {
	row: i64,
	id: String,
	body: Vec<u8>,
	claim: Option<String>,
	height: Option<i32>,
	resolution: Option<String>,
	resolution_height: Option<i32>,
}

async fn legacy_facts(client: &Client, ids: &[String]) -> Vec<LegacyFact> {
	client.query("SELECT v.id,v.vtxo_id,v.vtxo,v.onchain_spent_txid,v.onchain_spent_height,h.chain_resolution::TEXT,h.chain_resolution_height \
		FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=ANY($1) ORDER BY v.id", &[&ids]).await.unwrap()
		.into_iter().map(|row| LegacyFact { row: row.get("id"), id: row.get("vtxo_id"), body: row.get("vtxo"),
			claim: row.get("onchain_spent_txid"), height: row.get("onchain_spent_height"), resolution: row.get("chain_resolution"), resolution_height: row.get("chain_resolution_height") }).collect()
}

async fn confirm_old_prefix(ctx: &TestContext, old: &Client, leaf: &Vtxo<Full>, seen: &mut BTreeSet<Txid>) -> OutPoint {
	let steps = leaf.transactions().collect::<Vec<_>>();
	assert!(steps.len() >= 2);
	assert_eq!(steps.last().unwrap().tx.input.len(), 1);
	let point = steps.last().unwrap().tx.input[0].previous_output;
	for step in &steps[..steps.len()-1] {
		if !seen.insert(step.tx.compute_txid()) { continue; }
		let chain = ctx.bitcoind().sync_client();
		if chain.custom_get_raw_transaction_info(step.tx.compute_txid(), None).unwrap()
			.is_some_and(|info| info.confirmations.is_some_and(|depth| depth > 0)) { continue; }
		ctx.broadcast_cpfp(&step.tx).await;
		ctx.generate_blocks(1).await;
	}
	let row = old.query_one("SELECT vtxo FROM vtxo WHERE vtxo_id=$1", &[&point.to_string()]).await.unwrap();
	let prefix: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo")).unwrap();
	assert_eq!(prefix.point(),point);
	assert!(matches!(prefix.policy(), ServerVtxoPolicy::Checkpoint(_) | ServerVtxoPolicy::Expiry(_)
		| ServerVtxoPolicy::HarkLeaf(_) | ServerVtxoPolicy::HarkLeaf_v0(_)));
	let output = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	assert!(output.confirmations > 0);
	assert_eq!(output.value, sat(100_000));
	assert!(ctx.bitcoind().sync_client().get_tx_out(&leaf.point().txid, leaf.point().vout, Some(true)).unwrap().is_none());
	point
}

async fn timeout_receipt(ctx: &TestContext, wallet: &bark::Wallet, sources: &[Vtxo<Full>], destination: bitcoin::ScriptBuf, fee: bitcoin::Amount) -> Transaction {
	assert!(!sources.is_empty());
	let chain = ctx.bitcoind().sync_client();
	let current = ctx.bitcoind().get_block_count().await as u32;
	let clauses = sources.iter().map(|source| source.policy().as_server_htlc_send().unwrap()
		.user_claim_after_expiry_clause(source.exit_delta())).collect::<Vec<_>>();
	let mut mature = current;
	for (source, clause) in sources.iter().zip(&clauses) {
		let output = chain.get_tx_out(&source.point().txid, source.point().vout, Some(true)).unwrap().unwrap();
		assert_eq!(output.value, source.amount());
		assert!(output.confirmations > 0);
		mature = mature.max(clause.timelock_height.to_u32())
			.max(current + 1 - u32::try_from(output.confirmations).unwrap() + clause.block_delta.to_u32());
	}
	if mature >= current { ctx.generate_blocks(mature-current+1).await; }
	let lock_time = clauses.iter().max_by_key(|clause|clause.timelock_height).unwrap().locktime();
	let total = sources.iter().map(Vtxo::amount).sum::<bitcoin::Amount>();
	let mut claim = Transaction { version: bitcoin::transaction::Version::TWO, lock_time,
		input: sources.iter().zip(&clauses).map(|(source,clause)| TxIn { previous_output: source.point(),
			script_sig: Default::default(), sequence: clause.sequence(), witness: Witness::new() }).collect(),
		output: vec![TxOut { value: total-fee, script_pubkey: destination }],
	};
	let prevouts = sources.iter().map(Vtxo::txout).collect::<Vec<_>>();
	for (index,(source,clause)) in sources.iter().zip(clauses).enumerate() {
		claim.input[index].witness = wallet.sign_input_with_clause(source,&clause.into(),index,
			&mut SighashCache::new(&claim),&Prevouts::All(&prevouts)).await.unwrap();
	}
	assert!(claim.weight().to_vbytes_ceil()<=fee.to_sat(),"the real signed receipt must meet the ordinary 1sat/vB relay floor");
	chain.send_raw_transaction(&claim).unwrap();
	ctx.generate_blocks(6).await;
	assert_eq!(total, claim.output.iter().map(|output| output.value).sum::<bitcoin::Amount>() + fee);
	assert!(chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap().confirmations >= 6);
	claim
}

async fn legacy_upgrade() {
	let old_cap = pinned_product(OLD_ROOT,"captaind","f7c8fd77d91533a389ef8bb509dce5b2b92295036f93ef5d9a2be5900e9e0156");
	let old_watch = pinned_product(OLD_ROOT,"watchmand","920c6a742daae000274f44075e3e674262a4421fe0db0806bd59d0ab301836de");
	let new_cap = pinned_product(NEW_ROOT,"captaind","4bc16a305a40d455135bfd2df985d9dcb21a4987523148ec83cfdbae32ce4f25");
	let new_watch = pinned_product(NEW_ROOT,"watchmand","1660f365f1fb14909d4aad7975c171db49b0aa4c351b2a07d71513e7ff750366");
	let ctx = TestContext::new("server/legacy_funded_user_claim_epoch_reaches_later_supported_source").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	// Do not use CaptaindBuilder.watchmand(): that would start the new default
	// watchman and migrate this database before the legacy phase.
	let srv = Box::pin(ctx.captaind("server").exec(old_cap).lightningd(&lightning.internal)
		.bitcoind(ctx.bitcoind_arc()).funded(btc(10)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(128);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(128);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget {amount:sat(50_000),count:8}];
			cfg.invoice_check_interval = Duration::from_secs(1);
		}).create()).await;
	let watch = Box::pin(ctx.watchmand("server-watchmand").exec(old_watch).bitcoind(ctx.bitcoind_arc()).create(&srv)).await;
	srv.attach_watchmand(watch);
	let fee_address = srv.watchmand().wait_wallet_address().await;
	let operator = srv.watchmand().config().sweep_address.clone().unwrap().assume_checked();
	srv.watchmand().stop().await.unwrap();
	srv.wait_for_vtxopool(&ctx).await;
	let pg = srv.config().postgres.clone();
	let (old,old_driver) = old_read_only(&pg).await;
	let oid: u32 = old.query_one("SELECT oid FROM pg_database WHERE datname=current_database()",&[]).await.unwrap().get(0);
	assert_eq!(old.query_one("SELECT MAX(version) FROM expiry_refinery_schema_history",&[]).await.unwrap().get::<_,i32>(0),14);
	assert!(old.query_one("SELECT to_regclass('expiry_user_claim_tx') IS NULL AND to_regclass('expiry_user_claim_backfill_service') IS NULL",&[])
		.await.unwrap().get::<_,bool>(0));
	let actor = Box::pin(ctx.bark("sender",&srv).cfg(|cfg|cfg.daemon_manual_sync=true).funded(sat(1_000_000)).create()).await;
	let sender_seed_path = actor.datadir().join("mnemonic");
	let sender_seed_digest = sha256::Hash::hash(&std::fs::read(&sender_seed_path).unwrap());
	let mut prefix = Vec::new();
	// The actual configured fanout is FOUR. Four independent 80k boards are
	// legal four-output packages; no synthetic sixteen-output input is used.
	for batch in 0_u8..4 {
		let board = actor.board_and_confirm_and_register(&ctx,sat(80_000)).await;
		assert_eq!(board.len(),1);
		let wallet = actor.client().await;
		let input = wallet.get_full_vtxo(board[0]).await.unwrap();
		let key = wallet.get_vtxo_key(&input).await.unwrap();
		let hash = PaymentHash::from(sha256::Hash::hash(&[b'v',b'1',b'0',batch]).to_byte_array());
		let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
		let policy = VtxoPolicy::new_server_htlc_send(key.public_key(),hash,tip+srv.config().htlc_send_expiry_delta+BlockDelta::new(2));
		let destinations = (0..4).map(|_|ArkoorDestination {policy:policy.clone(),total_amount:sat(20_000)}).collect();
		let builder = ArkoorPackageBuilder::new_with_checkpoints([input],destinations).unwrap().generate_user_nonces(&[key]).unwrap();
		let mut rpc = srv.get_public_rpc().await;
		let reply = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
			parts:protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
		}).await.unwrap().into_inner();
		let leaves = builder.user_cosign(&[key],ArkoorPackageCosignResponse::try_from(reply).unwrap()).unwrap().build_signed_vtxos();
		assert_eq!(leaves.len(),4);
		assert!(leaves.iter().all(|leaf|leaf.amount()==sat(20_000)));
		wallet.register_vtxo_transactions_with_server(&leaves).await.unwrap();
		prefix.extend(leaves);
	}
	assert_eq!(prefix.len(),16);
	let recipient = Box::pin(ctx.bark_sdk("recipient",&srv).cfg(|cfg|cfg.daemon_manual_sync=true).create()).await;
	let invoice = recipient.bolt11_invoice(sat(180_000),None,None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let receive_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let recipient_fallback = recipient.fallback_destination().await.unwrap();
	let a = actor.board_and_confirm_and_register(&ctx,sat(100_000)).await;
	let b = actor.board_and_confirm_and_register(&ctx,sat(100_000)).await;
	assert_eq!(a.len(),1); assert_eq!(b.len(),1);
	let sender = actor.client().await;
	let fallback = sender.fallback_destination().await.unwrap();
	fallback.verify(bitcoin::Network::Regtest,sender.mailbox_identifier().as_pubkey()).unwrap();
	assert_ne!(fallback.spk,recipient_fallback.spk);
	let inputs = vec![sender.get_full_vtxo(a[0]).await.unwrap(),sender.get_full_vtxo(b[0]).await.unwrap()];
	let keys = vec![sender.get_vtxo_key(&inputs[0]).await.unwrap(),sender.get_vtxo_key(&inputs[1]).await.unwrap()];
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(inputs,
		VtxoPolicy::new_server_htlc_send(keys[0].public_key(),hash,tip+srv.config().htlc_send_expiry_delta+BlockDelta::new(2)))
		.unwrap().generate_user_nonces(&keys).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let reply = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts:protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let sources = builder.user_cosign(&keys,ArkoorPackageCosignResponse::try_from(reply).unwrap()).unwrap().build_signed_vtxos();
	assert_eq!(sources.len(),2);
	assert!(sources.iter().all(|source|source.amount()==sat(100_000)));
	sender.register_vtxo_transactions_with_server(&sources).await.unwrap();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {invoice:invoice.to_string(),payment_amount_sat:180_000,
		htlc_vtxo_ids:sources.iter().map(|source|source.id().to_bytes().to_vec()).collect(),mailbox_id:None,retry_for_secs:Some(5)}).await.unwrap();
	let reply = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash:hash.to_vec(),user_pubkey:receive_key.serialize().to_vec(),htlc_recv_expiry:(tip+BlockDelta::new(36)).to_u32(),
		lightning_receive_anti_dos:None,
	}).await.unwrap().into_inner();
	let grants = reply.htlc_vtxos.iter().map(|raw|Vtxo::<Full>::deserialize(raw).unwrap()).collect::<Vec<_>>();
	assert_eq!(grants.iter().map(Vtxo::amount).sum::<bitcoin::Amount>(),sat(180_000));
	let grant_ids = grants.iter().map(|grant|grant.id().to_string()).collect::<BTreeSet<_>>();
	let scope = old.query_one("SELECT f.id,f.subscription_id FROM expiry_lightning_internal_funding f \
		JOIN lightning_payment_attempt a ON a.id=f.id WHERE a.payment_hash=$1",&[&hash.to_string()]).await.unwrap();
	let attempt: i64 = scope.get("id"); let subscription: i64 = scope.get("subscription_id");
	let original_ids = sources.iter().map(|source|source.id().to_string()).collect::<Vec<_>>();
	let actual_originals = old.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id",&[&attempt]).await.unwrap()
		.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
	assert_eq!(actual_originals,original_ids.iter().cloned().collect());
	let actual_grants = old.query("SELECT vtxo_id FROM vtxo WHERE lightning_htlc_subscription_id=$1 ORDER BY vtxo_id",&[&subscription]).await.unwrap()
		.into_iter().map(|row|row.get::<_,String>(0)).collect::<BTreeSet<_>>();
	assert_eq!(actual_grants,grant_ids);
	let prefix_ids = prefix.iter().map(|source|source.id().to_string()).collect::<Vec<_>>();
	let mut target_ids = prefix_ids.clone(); target_ids.push(sources[0].id().to_string());
	let inserted = legacy_facts(&old,&target_ids).await;
	assert_eq!(inserted.len(),17);
	assert_eq!(inserted.last().unwrap().id,sources[0].id().to_string(),"A must be after sixteen actual numeric source rows");
	assert!(inserted[..16].iter().all(|fact|prefix_ids.contains(&fact.id)));

	let mut seen = BTreeSet::new();
	for source in &prefix {confirm_actual_path(&ctx,source,&mut seen).await;}
	for grant in &grants {confirm_actual_path(&ctx,grant,&mut seen).await;}
	confirm_actual_path(&ctx,&sources[0],&mut seen).await;
	let b_prefix = confirm_old_prefix(&ctx,&old,&sources[1],&mut seen).await;
	// All sponsors exist before abandonment. Four independent confirmed inputs
	// avoid relying on one preceding package's unconfirmed change.
	for _ in 0..4 {ctx.bitcoind().fund_addr(fee_address.clone(),sat(150_000)).await;}
	ctx.generate_blocks(2).await;
	let prefix_claim = timeout_receipt(&ctx,&sender,&prefix,fallback.spk.clone(),sat(2_000)).await;
	assert_eq!(prefix_claim.input.len(),16);
	assert_eq!(prefix_claim.output[0].value,sat(318_000));
	let a_claim = timeout_receipt(&ctx,&sender,&sources[..1],fallback.spk.clone(),sat(2_000)).await;
	assert_eq!(a_claim.output[0].value,sat(98_000));
	let expected_claims = target_ids.iter().map(|id|(id.clone(),if id==&sources[0].id().to_string() {a_claim.compute_txid().to_string()}
		else {prefix_claim.compute_txid().to_string()})).collect::<BTreeMap<_,_>>();
	drop(sender); drop(recipient); drop(rpc); drop(actor);
	let old_tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let old_hash = ctx.bitcoind().sync_client().get_block_hash(old_tip.to_u32() as u64).unwrap();
	let actual_claim_heights = [&prefix_claim,&a_claim].into_iter().map(|claim| {
		let receipt = ctx.bitcoind().sync_client().get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap();
		assert!(receipt.confirmations>=6);
		(claim.compute_txid().to_string(),i32::try_from(old_tip.to_u32()+1-u32::try_from(receipt.confirmations).unwrap()).unwrap())
	}).collect::<BTreeMap<_,_>>();
	srv.watchmand().start().await.unwrap();
	srv.wait_for_sync_height(old_tip).await;
	srv.watchmand().wait_for_sync_height(old_tip).await;
	let observed = tokio::time::timeout(Duration::from_secs(20),async {
		loop {
			let facts = legacy_facts(&old,&target_ids).await;
			let current = old.query_one("SELECT COUNT(*) FROM expiry_sync_ack a CROSS JOIN expiry_chain_view v \
				WHERE v.id=1 AND a.generation=v.generation AND a.height=$1 AND a.hash=$2",
				&[&i64::from(old_tip.to_u32()),&old_hash.to_string()]).await.unwrap().get::<_,i64>(0)==2;
			if current && facts.len()==17 && facts.iter().all(|fact|fact.resolution.as_deref()==Some("revoked")
				&& fact.claim.as_ref()==expected_claims.get(&fact.id)
				&& fact.height==expected_claims.get(&fact.id).and_then(|txid|actual_claim_heights.get(txid)).copied()
				&& fact.resolution_height==fact.height) {break facts;}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual V10 watchman must observe all seventeen rightful timeout spends");
	assert!(old.query_one("SELECT to_regclass('expiry_user_claim_tx') IS NULL AND to_regclass('expiry_user_claim_backfill_service') IS NULL",&[])
		.await.unwrap().get::<_,bool>(0),"no new client helper may migrate the old database");
	let old_acks = old.query("SELECT a.process,a.height,a.hash,a.generation=v.generation AS current_generation FROM expiry_sync_ack a CROSS JOIN expiry_chain_view v WHERE v.id=1 ORDER BY a.process",&[]).await.unwrap();
	assert_eq!(old_acks.len(),2);
	assert!(old_acks.iter().all(|row|row.get::<_,bool>("current_generation") && row.get::<_,i64>("height")==i64::from(old_tip.to_u32())
		&& row.get::<_,&str>("hash")==old_hash.to_string()));
	std::fs::write(srv.config().data_dir.join("legacy-upgrade-before.json"),serde_json::to_vec_pretty(&serde_json::json!({
		"old_capture":OLD_ROOT,"old_max_migration":14,"retention_tables_absent":true,"database_oid":oid,
		"tip":old_tip.to_u32(),"hash":old_hash.to_string(),"observed_sources":observed.iter().map(|fact|serde_json::json!({
			"row":fact.row,"id":fact.id,"body_sha256":sha256::Hash::hash(&fact.body).to_string(),
			"claim":fact.claim,"height":fact.height,"resolution":fact.resolution,"resolution_height":fact.resolution_height})).collect::<Vec<_>>()
	})).unwrap()).unwrap();
	let chain = ctx.bitcoind().sync_client();
	for grant in &grants {assert!(chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(true)).unwrap().is_some());}
	assert_eq!(chain.get_tx_out(&b_prefix.txid,b_prefix.vout,Some(true)).unwrap().unwrap().value,sat(100_000));
	srv.watchmand().stop().await.unwrap(); srv.stop().await.unwrap();
	drop(old); old_driver.await.unwrap().unwrap();
	let seed_paths = [srv.config().data_dir.join("mnemonic"),srv.watchmand().config().data_dir.join("mnemonic")];
	let seeds = seed_paths.iter().map(|path|sha256::Hash::hash(&std::fs::read(path).unwrap())).collect::<Vec<_>>();
	srv.set_exec(Some(new_cap)); srv.watchmand().set_exec(Some(new_watch));
	srv.start().await.unwrap(); srv.watchmand().start().await.unwrap();
	assert_eq!(seeds,seed_paths.iter().map(|path|sha256::Hash::hash(&std::fs::read(path).unwrap())).collect::<Vec<_>>(),
		"upgrade must use the identical two server wallet seeds");
	assert_eq!(sha256::Hash::hash(&std::fs::read(&sender_seed_path).unwrap()),sender_seed_digest);
	let (new,new_driver) = old_read_only(&pg).await;
	assert_eq!(new.query_one("SELECT oid FROM pg_database WHERE datname=current_database()",&[]).await.unwrap().get::<_,u32>(0),oid);
	assert_eq!(new.query_one("SELECT MAX(version) FROM expiry_refinery_schema_history",&[]).await.unwrap().get::<_,i32>(0),16);
	assert_eq!(legacy_facts(&new,&target_ids).await,observed,"same-database upgrade must preserve actual numeric identities, signed bodies and spend facts");
	drop(new); new_driver.await.unwrap().unwrap();
	// Only the upgraded processes have now run the current migration runner.
	let db = Db::connect(&pg).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30),async {
		loop {
			let mut complete = assert_retained_claim(&db,sources[0].id(),&a_claim).await;
			for source in &prefix {complete &= assert_retained_claim(&db,source.id(),&prefix_claim).await;}
			if complete {break;}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("funded finite epoch must link the later A as well as all sixteen earlier valid sources");
	let service = db.read(async |t| {
		let row=t.query_one("SELECT after_id,through_id FROM expiry_user_claim_backfill_service WHERE id=1",&[]).await?;
		Ok((row.get::<_,i64>("after_id"),row.get::<_,i64>("through_id")))
	}).await.unwrap();
	assert!(service.1>=observed.last().unwrap().row,"the closed epoch must include the actual later source");
	for grant in &grants {assert!(chain.get_tx_out(&grant.point().txid,grant.point().vout,Some(false)).unwrap().is_some(),
		"real canonical G consumption must follow durable A retention");}
	let parent:Transaction = tokio::time::timeout(Duration::from_secs(40),async {
		loop {
			let row=db.read(async |t|Ok(t.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND a.status='pending' AND a.broadcast_at IS NOT NULL AND a.sweep_tx IS NOT NULL",
				&[&sources[1].id().to_string()]).await?)).await.unwrap();
			if let Some(row)=row {
				let parent:Transaction=deserialize(row.get("sweep_tx")).unwrap();
				if chain.get_mempool_entry(&parent.compute_txid()).is_ok() {break parent;}
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("normal upgraded worker must publish the rightful B refund while both actors stay absent");
	assert_eq!(parent.input.len(),grants.len()+1);
	assert!(parent.input.iter().any(|input|input.previous_output==b_prefix));
	for grant in &grants {
		let input=parent.input.iter().find(|input|input.previous_output==grant.point()).expect("ALL genuine grant timeouts are required");
		let script=grant.policy().as_server_htlc_recv().unwrap().server_claim_after_expiry_clause(grant.server_pubkey(),grant.exit_delta()).tapscript();
		assert_eq!(input.witness.taproot_leaf_script().unwrap().script,script.as_script());
	}
	assert!(!parent.output.iter().any(|output|output.script_pubkey==recipient_fallback.spk));
	assert_eq!(parent.output.iter().filter(|output|output.script_pubkey==fallback.spk).map(|output|output.value).sum::<bitcoin::Amount>(),sat(99_000));
	assert_eq!(parent.output.iter().filter(|output|output.script_pubkey==operator.script_pubkey()).map(|output|output.value).sum::<bitcoin::Amount>(),sat(181_000));
	let anchors=parent.output.iter().enumerate().filter(|(_,output)|output.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
	assert_eq!(anchors.len(),1,"the real immutable parent must have one actual P2A anchor");
	assert_eq!(anchors[0].1.value,sat(0));
	let anchor=OutPoint::new(parent.compute_txid(),u32::try_from(anchors[0].0).unwrap());
	let mut actual_inputs=sat(0);
	for input in &parent.input {actual_inputs+=ctx.bitcoind().await_transaction(input.previous_output.txid).await.output[input.previous_output.vout as usize].value;}
	assert_eq!(actual_inputs,sat(280_000));
	assert_eq!(parent.output.iter().map(|output|output.value).sum::<bitcoin::Amount>(),actual_inputs,"the immutable parent pays zero miner fee");
	assert_eq!(sat(520_000)+sat(180_000),prefix_claim.output[0].value+a_claim.output[0].value+actual_inputs+sat(4_000));
	let height=ctx.generate_blocks(6).await;
	wait_partial_confirmation(&ctx,&srv,&db,height,sources[0].id(),a_claim.compute_txid(),sources[1].id(),&parent).await;
	let parent_info:serde_json::Value=chain.call("getrawtransaction",&[serde_json::json!(parent.compute_txid().to_string()),serde_json::json!(true)]).unwrap();
	assert!(parent_info["confirmations"].as_u64().unwrap()>=6);
	let customer=parent.output.iter().enumerate().filter(|(_,output)|output.script_pubkey==fallback.spk).collect::<Vec<_>>();
	assert_eq!(customer.len(),1);
	let receipt=chain.get_tx_out(&parent.compute_txid(),u32::try_from(customer[0].0).unwrap(),Some(true)).unwrap().unwrap();
	assert_eq!(receipt.value,sat(99_000));
	assert!(receipt.confirmations>=6,"the absent user's actual rightful output must remain confirmed and unspent");
	let child_row=db.read(async |t|Ok(t.query_one("SELECT f.txid,f.raw_tx,f.fee_sat,f.confirmed_hash FROM expiry_fee_child f \
		JOIN expiry_allocation a ON a.id=f.allocation_id WHERE a.sweep_tx=$1 AND f.confirmed_hash IS NOT NULL",&[&serialize(&parent)]).await?)).await.unwrap();
	let child:Transaction=deserialize(child_row.get("raw_tx")).unwrap();
	assert_eq!(child.input.iter().filter(|input|input.previous_output==anchor).count(),1,
		"the confirmed fee child must actually spend this parent's unique P2A");
	assert_eq!(child_row.get::<_,&str>("txid"),child.compute_txid().to_string());
	let child_info:serde_json::Value=chain.call("getrawtransaction",&[serde_json::json!(child.compute_txid().to_string()),serde_json::json!(true)]).unwrap();
	assert!(child_info["confirmations"].as_u64().unwrap()>=6);
	assert_eq!(child_info["blockhash"],parent_info["blockhash"]);
	assert_eq!(child_row.get::<_,&str>("confirmed_hash"),child_info["blockhash"].as_str().unwrap());
	let mut child_inputs=sat(0);
	for input in &child.input {child_inputs+=ctx.bitcoind().await_transaction(input.previous_output.txid).await.output[input.previous_output.vout as usize].value;}
	let actual_child_fee=child_inputs-child.output.iter().map(|output|output.value).sum::<bitcoin::Amount>();
	assert!(actual_child_fee>sat(0));
	assert_eq!(actual_child_fee,sat(u64::try_from(child_row.get::<_,i64>("fee_sat")).unwrap()));
	for claim in [&prefix_claim,&a_claim] {assert!(chain.get_tx_out(&claim.compute_txid(),0,Some(true)).unwrap().unwrap().confirmations>=6);}
	assert!(assert_retained_claim(&db,sources[0].id(),&a_claim).await);
	for source in &prefix {assert!(assert_retained_claim(&db,source.id(),&prefix_claim).await);}
	let duplicate_prefix=db.read(async |t|Ok(t.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
		WHERE e.vtxo_id=ANY($1) AND c.disposition='customer' AND a.status='confirmed'",&[&prefix_ids]).await?.get::<_,i64>(0))).await.unwrap();
	assert_eq!(duplicate_prefix,0,"the real batched user receipt cannot also receive a customer fallback");
	std::fs::write(srv.config().data_dir.join("legacy-upgrade-after.json"),serde_json::to_vec_pretty(&serde_json::json!({
		"old_capture":OLD_ROOT,"new_capture":NEW_ROOT,"database_oid":oid,"new_max_migration":16,
		"same_server_wallet_seeds":true,"same_sender_wallet_seed":true,"actors_absent_before_upgrade":true,"retained_sources":17,
		"prefix_claim":prefix_claim.compute_txid().to_string(),"a_claim":a_claim.compute_txid().to_string(),
		"originals":original_ids,"grants":grant_ids,"attempt":attempt,"subscription":subscription,
		"backfill_service":{"after":service.0,"through":service.1},"parent":parent_info,"child":child_info,
		"actual_child_fee_sat":actual_child_fee.to_sat(),"parent_anchor":anchor.to_string(),
		"scope":"finite old-process funded epoch and later B closure; no incompatible-prefix or general batched replay claim"
	})).unwrap()).unwrap();
}

#[tokio::test]
async fn legacy_funded_user_claim_epoch_reaches_later_supported_source() {
	// Proposed first bound includes all old setup, genuine claims, migration,
	// autonomous retention and payout. It is not yet measured qualification.
	tokio::time::timeout(Duration::from_secs(180),Box::pin(legacy_upgrade())).await
		.expect("funded same-database legacy upgrade exceeded its proposed total bound");
}
