//! Normal successful receive-for-address retains the intended recipient in the
//! server's durable receive binding, independently of the provider's key.

use std::time::Duration;
use std::sync::Arc;

use ark::{ProtocolEncoding, ServerVtxo, Vtxo};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;
use ark_testing::{btc, sat, TestContext};
use bark::actions::lightning::receive::LightningReceiveState;
use server::database::Db;
use ark_testing::daemon::captaind::{self, ArkClient};
use server_rpc::protos;

/// Pause at the normal completed-grant reply boundary. The grants themselves
/// are real and committed; this controller never changes a financial row.
#[derive(Clone)]
struct PausePreparedInternalReceive {
	arrived: Arc<tokio::sync::Notify>,
	resume: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for PausePreparedInternalReceive {
	async fn prepare_lightning_receive_claim(
		&self, upstream: &mut ArkClient, request: protos::PrepareLightningReceiveClaimRequest,
	) -> Result<protos::PrepareLightningReceiveClaimResponse, tonic::Status> {
		let response = upstream.prepare_lightning_receive_claim(request).await?.into_inner();
		self.arrived.notify_one();
		self.resume.notified().await;
		Ok(response)
	}
}

/// An admitted receive claim request survives a restart while the tree writer is pending,
/// and completes without either wallet returning.
#[tokio::test]
async fn receive_binding_internal_original_request_resumes_without_either_wallet() {
	let ctx = TestContext::new("bark_sdk/receive_binding_internal_callerless_restart").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let pause = PausePreparedInternalReceive {
		arrived: Arc::new(tokio::sync::Notify::new()),
		resume: Arc::new(tokio::sync::Notify::new()),
	};
	let proxy = srv.start_proxy_no_mailbox(pause.clone()).await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &proxy)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(50_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let (original, protocol, subscription, pending_pid) = {
		let operation = async {
			tokio::join!(sender.pay_lightning_invoice(invoice, None, true),
				recipient.try_claim_lightning_receive(hash, true))
		};
		tokio::pin!(operation);
		tokio::select! {
			_ = pause.arrived.notified() => {},
			result = &mut operation => panic!("internal operation finished before the normal grant pause: {result:?}"),
			_ = tokio::time::sleep(Duration::from_secs(60)) => panic!("normal internal grants were not prepared"),
		}
		// Lock only the existing infrastructure barrier, after real grants
		// commit. No subscription, attempt, coin, or settlement is fabricated.
		db.write(async |t| {
			t.execute("SELECT pg_advisory_xact_lock(2)", &[]).await?;
			pause.resume.notify_one();
			let admitted = async {
				loop {
					let row = db.read(async |tx| Ok(tx.query_opt("SELECT r.request,r.protocol_version,r.subscription_id, \
						(SELECT l.pid FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid \
						WHERE l.locktype='advisory' AND l.classid=0 AND l.objid=2 AND NOT l.granted \
						AND a.datname=current_database() AND a.query LIKE '%INSERT INTO htlc_settlement%' LIMIT 1) AS pending_pid \
						FROM expiry_lightning_receive_replay r WHERE r.payment_hash=$1", &[&hash.to_string()]).await?)).await?;
					if let Some(row) = row {
						if let Some(pid) = row.get::<_, Option<i32>>("pending_pid") {
							break Ok::<_, anyhow::Error>((row.get::<_, Vec<u8>>("request"),
								row.get::<_, i64>("protocol_version"), row.get::<_, i64>("subscription_id"), pid));
						}
					}
					tokio::time::sleep(Duration::from_millis(20)).await;
				}
			};
			let admitted = tokio::select! {
				result = tokio::time::timeout(Duration::from_secs(30), admitted) => result.expect("original claim never reached its actual pending WAL writer")?,
				result = &mut operation => panic!("claim finished while its actual WAL writer was held: {result:?}"),
			};
			let before = t.query_one("SELECT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal, \
				EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE payment_hash=$1 AND materialized) AS materialized", &[&hash.to_string()]).await?;
			assert!(!before.get::<_, bool>("wal") && !before.get::<_, bool>("materialized"),
				"recipient materialization and preimage must remain invisible together before commit");
			srv.stop().await?;
			Ok(admitted)
		}).await.unwrap()
	}; // Both original client futures end before the server restarts.
	drop(sender);
	drop(recipient);
	drop(proxy);
	tokio::time::timeout(Duration::from_secs(10), async {
		while db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pending_pid]).await?.get::<_, bool>(0))).await.unwrap() {
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}).await.expect("interrupted server writer did not close after its infrastructure gate was released");
	let rolled_back = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal, \
		EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE payment_hash=$1 AND materialized) AS materialized", &[&hash.to_string()]).await?)).await.unwrap();
	assert!(!rolled_back.get::<_, bool>("wal") && !rolled_back.get::<_, bool>("materialized"));
	srv.start().await.unwrap();
	let recovered = tokio::time::timeout(Duration::from_secs(60), async {
		loop {
			let row = db.read(async |t| Ok(t.query_one("SELECT r.request,r.protocol_version,s.status::TEXT AS receive_status, \
				a.status::TEXT AS attempt_status,COUNT(c.source_vtxo_id) AS bindings,BOOL_AND(c.materialized) AS materialized, \
				BOOL_AND(h.offchain_resolution::TEXT='fulfilled') AS fulfilled, \
				COALESCE(SUM(v.amount),0)::BIGINT AS amount, \
				EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal \
				FROM expiry_lightning_receive_replay r JOIN lightning_htlc_subscription s ON s.id=r.subscription_id \
				JOIN expiry_lightning_internal_funding f ON f.subscription_id=s.id \
				JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=s.id \
				JOIN expiry_lightning_receive_claim c ON c.subscription_id=s.id \
				JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id JOIN htlc_vtxo h ON h.id=source.id \
				LEFT JOIN vtxo v ON v.vtxo_id=c.output_vtxo_id \
				WHERE r.subscription_id=$2 GROUP BY r.subscription_id,r.request,r.protocol_version,s.status,a.status",
				&[&hash.to_string(),&subscription]).await?)).await.unwrap();
			if row.get::<_, &str>("receive_status") == "settled" && row.get::<_, &str>("attempt_status") == "succeeded" { break row; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("original admitted internal receive required a returning wallet");
	assert_eq!(recovered.get::<_, Vec<u8>>("request"), original);
	assert_eq!(recovered.get::<_, i64>("protocol_version"), protocol);
	assert!(recovered.get::<_, i64>("bindings") > 0);
	assert!(recovered.get::<_, bool>("materialized") && recovered.get::<_, bool>("fulfilled") && recovered.get::<_, bool>("wal"));
	assert_eq!(recovered.get::<_, i64>("amount"), 50_000);
	let actual = db.read(async |t| Ok(t.query("SELECT c.output_vtxo,v.vtxo FROM expiry_lightning_receive_claim c \
		JOIN vtxo v ON v.vtxo_id=c.output_vtxo_id WHERE c.subscription_id=$1", &[&subscription]).await?)).await.unwrap();
	assert_eq!(actual.len() as i64, recovered.get::<_, i64>("bindings"));
	for row in actual {
		let promised = Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
		let committed = Vtxo::<Full>::deserialize(row.get("vtxo")).unwrap();
		assert_eq!(committed.id(), promised.id());
		assert_eq!(committed.user_pubkey(), promised.user_pubkey());
		assert_eq!(committed.amount(), promised.amount());
	}
}

#[tokio::test]
async fn receive_binding_normal_external_claim_keeps_the_actual_recipient() {
	let ctx = TestContext::new("bark_sdk/receive_binding_normal_external_claim").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let provider = ctx.bark_sdk("provider", &srv).boarded(sat(100_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let address = recipient.new_address().await.unwrap();
	let recipient_key = address.policy().user_pubkey();
	assert_ne!(provider.fallback_destination().await.unwrap().account_xpub,
		recipient.fallback_destination().await.unwrap().account_xpub);
	let invoice = provider.bolt11_invoice_for_address(sat(50_000), address, None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	lightning.sync().await;
	let (paid, claimed) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(
			lightning.external.try_pay_bolt11(invoice.to_string()),
			provider.try_claim_lightning_receive(hash, true),
		)
	}).await.expect("normal receive did not complete");
	paid.expect("external payment failed");
	assert!(matches!(claimed.unwrap(), LightningReceiveState::Settled(_)));
	recipient.sync().await;
	let coins = recipient.spendable_vtxos().await.unwrap();
	assert!(!coins.is_empty(), "normal delivery must give the recipient its signed coins");
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let original_request = db.read(async |t| Ok(t.query_one("SELECT request,protocol_version FROM expiry_lightning_receive_replay WHERE payment_hash=$1",
		&[&hash.to_string()]).await?)).await.unwrap();
	assert!(!original_request.get::<_, &[u8]>("request").is_empty());
	assert!(original_request.get::<_, i64>("protocol_version") > 0);
	let bindings = db.read(async |t| Ok(t.query("SELECT c.source_vtxo_id,c.output_vtxo_id,c.output_vtxo, \
		c.materialized,source.vtxo AS source_vtxo,actual.vtxo AS actual_vtxo, \
		h.offchain_resolution::TEXT AS resolution,s.status::TEXT AS status \
		FROM expiry_lightning_receive_claim c \
		JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id \
		JOIN htlc_vtxo h ON h.id=source.id \
		JOIN vtxo actual ON actual.vtxo_id=c.output_vtxo_id \
		JOIN lightning_htlc_subscription s ON s.id=c.subscription_id \
		WHERE c.payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
	assert!(!bindings.is_empty());
	let mut received = bitcoin::Amount::ZERO;
	for row in &bindings {
		let original = Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
		let actual = Vtxo::<Full>::deserialize(row.get("actual_vtxo")).unwrap();
		let source = ServerVtxo::<Full>::deserialize(row.get("source_vtxo")).unwrap().try_into_user_vtxo().unwrap();
		assert!(row.get::<_, bool>("materialized"));
		assert_eq!(row.get::<_, &str>("resolution"), "fulfilled");
		assert_eq!(row.get::<_, &str>("status"), "settled");
		assert_eq!(original.id(), actual.id());
		assert_eq!(original.user_pubkey(), recipient_key);
		assert_eq!(actual.user_pubkey(), recipient_key);
		assert_ne!(source.user_pubkey(), recipient_key);
		assert_eq!(source.amount(), original.amount());
		assert!(coins.iter().any(|coin| coin.id() == actual.id()));
		received += actual.amount();
	}
	assert_eq!(coins.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), received);
	// A completed normal action is idempotent; it cannot mint another binding.
	assert!(matches!(provider.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
	let count = db.read(async |t| Ok(t.query_one("SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE payment_hash=$1", &[&hash.to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, bindings.len() as i64);
	let retry_request = db.read(async |t| Ok(t.query_one("SELECT request,protocol_version FROM expiry_lightning_receive_replay WHERE payment_hash=$1",
		&[&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(original_request.get::<_, &[u8]>("request"), retry_request.get::<_, &[u8]>("request"));
	assert_eq!(original_request.get::<_, i64>("protocol_version"), retry_request.get::<_, i64>("protocol_version"));
}

/// A normal intra-Ark receive uses its durable funding link after the payer's
/// attempt completes. Replaying maintenance must not consult the canceled native
/// hold invoice, duplicate the binding, or write another identical status row.
#[tokio::test]
async fn receive_binding_normal_internal_claim_and_reconciliation_are_idempotent() {
	let ctx = TestContext::new("bark_sdk/receive_binding_normal_internal_claim").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(50_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let (paid, claimed) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(
			sender.pay_lightning_invoice(invoice, None, true),
			recipient.try_claim_lightning_receive(hash, true),
		)
	}).await.expect("normal internal receive did not complete");
	paid.unwrap();
	assert!(matches!(claimed.unwrap(), LightningReceiveState::Settled(_)));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let row = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let row = db.read(async |t| Ok(t.query_one("SELECT s.id,s.status::TEXT,s.updated_at,a.status::TEXT AS attempt_status, \
				COUNT(c.source_vtxo_id) AS bindings,BOOL_AND(c.materialized) AS materialized \
				FROM lightning_htlc_subscription s JOIN expiry_lightning_internal_funding f ON f.subscription_id=s.id \
				JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=s.id \
				JOIN expiry_lightning_receive_claim c ON c.subscription_id=s.id AND c.payment_hash=s.payment_hash \
				WHERE s.payment_hash=$1 GROUP BY s.id,a.status", &[&hash.to_string()]).await?)).await.unwrap();
			if row.get::<_, &str>("status") == "settled" { break row; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("internal receive did not reach its actual settled subscription");
	assert_eq!(row.get::<_, &str>("attempt_status"), "succeeded");
	assert!(row.get::<_, i64>("bindings") > 0 && row.get::<_, bool>("materialized"));
	let before: chrono::DateTime<chrono::Local> = row.get("updated_at");
	assert!(db.write(async |t| t.reconcile_completed_internal_receives(128).await).await.unwrap().is_empty());
	let after = db.read(async |t| Ok(t.query_one("SELECT updated_at FROM lightning_htlc_subscription WHERE id=$1", &[&row.get::<_, i64>("id")]).await?.get::<_, chrono::DateTime<chrono::Local>>(0))).await.unwrap();
	assert_eq!(before, after, "identical reconciliation must not change status history");
	let replay = db.read(async |t| Ok(t.query_one("SELECT COUNT(*) AS total,BOOL_AND(octet_length(request)>0) AS complete \
		FROM expiry_lightning_receive_replay WHERE payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(replay.get::<_, i64>("total"), 1);
	assert!(replay.get::<_, bool>("complete"));
	let receipt = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(receipt.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(50_000));
	let mut external = lightning.internal.grpc_client().await;
	let pays = external.list_send_pays(cln_rpc::ListsendpaysRequest {
		payment_hash: Some(hash.to_vec()), ..Default::default()
	}).await.unwrap().into_inner();
	assert!(pays.payments.is_empty(), "actual internal receive must not be sent through external CLN");
}

/// ML030/ML057: an admitted but uncollected internal receive must not strand
/// either original signed principal after the original claim window closes.
/// This is the no-exit case; it does not qualify a previously exposed claim.
#[tokio::test]
async fn receive_binding_internal_expired_before_replay_refunds_absent_sender_once() {
	use std::collections::{BTreeMap, BTreeSet};
		use ark_testing::daemon::captaind::WalletAdminClient;
	use bark::{OpenWalletArgs, Wallet, WalletSeed};
	use bark::actions::lightning::pay::Progress;
	use bark::movement::MovementStatus;
	use bark::subsystem::Subsystem;
	use bitcoin::{Amount, Network, OutPoint, Transaction};
	use bitcoin::consensus::deserialize;
	use bitcoin_ext::BlockDelta;
	use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};

	tokio::time::timeout(Duration::from_secs(180), Box::pin(async {
		let ctx = TestContext::new("bark_sdk/receive_binding_internal_expired_before_replay").await;
		let first_height = ctx.bitcoind().get_block_count().await;
		let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
		let operator = ctx.bitcoind().get_new_address();
		let operator_script = operator.script_pubkey();
		let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(sat(205_000)).cfg(|cfg| {
			cfg.vtxo_lifetime = BlockDelta::new(32);
			cfg.vtxo_exit_delta = BlockDelta::new(3);
			cfg.htlc_send_expiry_delta = BlockDelta::new(40);
			cfg.invoice_check_interval = Duration::from_secs(1);
			cfg.vtxopool.vtxo_lifetime = BlockDelta::new(32);
			cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			cfg.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 4 }];
		}).watchmand_cfg(move |cfg| cfg.sweep_address = Some(operator.into_unchecked())).create().await;
		srv.wait_for_vtxopool(&ctx).await;
		let fee_address = srv.watchmand().wait_wallet_address().await;
		let mut roots = BTreeMap::new();
		for _ in 0..2 {
			let id = ctx.bitcoind().fund_addr(fee_address.clone(),sat(100_000)).await;
			let tx = ctx.bitcoind().await_transaction(id).await;
			let outputs = tx.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==fee_address.script_pubkey()).collect::<Vec<_>>();
			assert_eq!(outputs.len(),1); assert_eq!(outputs[0].1.value,sat(100_000));
			assert!(roots.insert(OutPoint::new(id,outputs[0].0 as u32),outputs[0].1.clone()).is_none());
		}
		let confirmed = ctx.generate_blocks(6).await;
		srv.wait_for_sync_height(confirmed).await; srv.watchmand().wait_for_sync_height(confirmed).await;
		for (point,out) in &roots {
			let actual=ctx.bitcoind().sync_client().get_tx_out(&point.txid,point.vout,Some(true)).unwrap().unwrap();
			assert!(actual.confirmations>=6);assert_eq!(actual.value,out.value);assert_eq!(actual.script_pub_key.script().unwrap(),out.script_pubkey);
		}
		let pause = PausePreparedInternalReceive { arrived: Arc::new(tokio::sync::Notify::new()), resume: Arc::new(tokio::sync::Notify::new()) };
		let proxy = srv.start_proxy_no_mailbox(pause.clone()).await;
		let mnemonic = bip39::Mnemonic::generate(12).unwrap();
		let sender = Box::pin(ctx.bark_sdk("sender",&srv).mnemonic(mnemonic.clone()).funded(sat(300_000)).boarded(sat(50_000)).cfg(|cfg| {
			cfg.daemon_manual_sync=true; cfg.vtxo_exit_margin=BlockDelta::new(2);
			cfg.vtxo_refresh_expiry_threshold=BlockDelta::new(4); cfg.htlc_recv_claim_delta=BlockDelta::new(2);
		}).create()).await;
		sender.stop_daemon_wait().await.unwrap();
		let mut sender_config = sender.config().clone();
		let destination = sender.fallback_destination().await.unwrap();
		let board_coins = sender.vtxos().await.unwrap();
		assert_eq!(board_coins.len(),1); assert_eq!(board_coins[0].amount(),sat(50_000));
		let board_coin = sender.get_full_vtxo(board_coins[0].id()).await.unwrap();
		let board = ctx.bitcoind().await_transaction(board_coin.chain_anchor().txid).await;
		board_coin.validate(&board).unwrap();
		assert_eq!(board.input.len(),1);
		let prior = ctx.bitcoind().await_transaction(board.input[0].previous_output.txid).await;
		let funding = prior.output[board.input[0].previous_output.vout as usize].clone();
		assert_eq!(funding.value,sat(300_000));
		assert!(roots.insert(board.input[0].previous_output,funding).is_none());
		let board_fee = sat(300_000)-board.output.iter().map(|out|out.value).sum::<Amount>();
		assert!(board_fee>Amount::ZERO);
		let mut customer_change = BTreeMap::new();
		for (index,out) in board.output.iter().enumerate() {
			let point=OutPoint::new(board.compute_txid(),index as u32);
			if point==board_coin.chain_anchor() || out.value==Amount::ZERO { continue; }
			assert!(sender.onchain().unwrap().read().await.is_mine(&out.script_pubkey).await.unwrap());
			customer_change.insert(point,out.clone());
		}
		assert_eq!(customer_change.values().map(|out|out.value).sum::<Amount>()+board_fee+sat(50_000),sat(300_000));
		let recipient = ctx.bark_sdk("recipient",&proxy).cfg(|cfg| {
			cfg.daemon_manual_sync=true; cfg.vtxo_exit_margin=BlockDelta::new(2);
			cfg.vtxo_refresh_expiry_threshold=BlockDelta::new(4); cfg.htlc_recv_claim_delta=BlockDelta::new(2);
		}).create().await;
		recipient.stop_daemon_wait().await.unwrap();
		let recipient_destination=recipient.fallback_destination().await.unwrap();
		assert_ne!(destination.spk,recipient_destination.spk);
		let invoice=recipient.bolt11_invoice(sat(50_000),None,None).await.unwrap();
		assert!(invoice.min_final_cltv_expiry_delta()<=40,"the real invoice must fit the explicit bounded send profile");
		let invoice_text=invoice.to_string();
		let hash=PaymentHash::from(&invoice);
		let db=Db::connect(&srv.config().postgres).await.unwrap();
		let (original,protocol,subscription,pending_pid)={
			let operation=async { tokio::join!(sender.pay_lightning_invoice(invoice,None,true),recipient.try_claim_lightning_receive(hash,true)) };
			tokio::pin!(operation);
			tokio::select! {
				_ = pause.arrived.notified()=>{},
				result=&mut operation=>panic!("claim finished before the actual prepared-grant pause: {result:?}"),
				_ = tokio::time::sleep(Duration::from_secs(60))=>panic!("normal internal grants were not prepared"),
			}
			db.write(async |t| {
				t.execute("SELECT pg_advisory_xact_lock(2)",&[]).await?;
				pause.resume.notify_one();
				let admitted=async {
					loop {
						let row=db.read(async |tx|Ok(tx.query_opt("SELECT r.request,r.protocol_version,r.subscription_id, \
						(SELECT l.pid FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid \
						WHERE l.locktype='advisory' AND l.classid=0 AND l.objid=2 AND NOT l.granted \
						AND a.datname=current_database() AND a.query LIKE '%INSERT INTO htlc_settlement%' LIMIT 1) AS pending_pid \
						FROM expiry_lightning_receive_replay r WHERE r.payment_hash=$1",&[&hash.to_string()]).await?)).await?;
						if let Some(row)=row { if let Some(pid)=row.get::<_,Option<i32>>("pending_pid") {
							break Ok::<_,anyhow::Error>((row.get::<_,Vec<u8>>("request"),row.get::<_,i64>("protocol_version"),row.get::<_,i64>("subscription_id"),pid));
						} }
						tokio::time::sleep(Duration::from_millis(20)).await;
					}
				};
				let admitted=tokio::select! {
					result=tokio::time::timeout(Duration::from_secs(30),admitted)=>result.expect("original claim never reached its actual pending WAL writer")?,
					result=&mut operation=>panic!("claim finished while its actual WAL writer was held: {result:?}"),
				};
				let before=t.query_one("SELECT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal, \
				EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE payment_hash=$1 AND materialized) AS materialized",&[&hash.to_string()]).await?;
				assert!(!before.get::<_,bool>("wal") && !before.get::<_,bool>("materialized"));
				srv.watchmand().stop().await?; srv.stop().await?;
				Ok(admitted)
			}).await.unwrap()
		}; // Both original wallet operations end before any clock progression.
		assert!(!original.is_empty()); assert!(protocol>0);
		let send=sender.lightning_send_checkpoint(hash).await.unwrap().unwrap();
		let htlcs=match &send.progress { Progress::PaymentInitiated(htlcs)=>htlcs.clone(),other=>panic!("original sender was not initiated: {other:?}") };
		assert_eq!(send.total_amount(),sat(50_000)); assert_eq!(send.fee,Amount::ZERO); assert_eq!(htlcs.vtxo_ids.len(),1);
		let live=sender.vtxos().await.unwrap();
		assert_eq!(live.len(),1);assert_eq!(live[0].id(),htlcs.vtxo_ids[0]);assert_eq!(live[0].amount(),sat(50_000));
		assert!(matches!(live[0].policy(),ark::VtxoPolicy::ServerHtlcSend(_)),"the only live coin must be the original HTLC, not ordinary change");
		assert_eq!(live[0].state,bark::vtxo::VtxoState::Locked {holder:Some(bark::vtxo::VtxoLockHolder::Action {id:send.id()})});
		let source=sender.get_full_vtxo(htlcs.vtxo_ids[0]).await.unwrap();
		assert_eq!(source.chain_anchor(),board_coin.chain_anchor()); source.validate(&board).unwrap();
		assert_eq!(sender.get_vtxo_key(&source).await.unwrap().public_key(),source.user_pubkey());
		let grant_rows=db.read(async |t|Ok(t.query("SELECT v.vtxo FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.lightning_htlc_subscription_id=$1 AND h.direction='outgoing' ORDER BY v.vtxo_id",&[&subscription]).await?)).await.unwrap();
		let grants=grant_rows.iter().map(|row|ServerVtxo::<Full>::deserialize(row.get("vtxo")).unwrap().try_into_user_vtxo().unwrap()).collect::<Vec<_>>();
		assert_eq!(grants.len(),1); assert_eq!(grants[0].amount(),sat(50_000));
		let pool_root=grants[0].chain_anchor();
		let pool=ctx.bitcoind().await_transaction(pool_root.txid).await;
		assert_eq!(pool.output[pool_root.vout as usize].value,sat(200_000));
		assert_eq!(pool.input.len(),1);
		let prior=ctx.bitcoind().await_transaction(pool.input[0].previous_output.txid).await;
		let pool_funding=prior.output[pool.input[0].previous_output.vout as usize].clone();
		assert_eq!(pool_funding.value,sat(205_000)); assert!(roots.insert(pool.input[0].previous_output,pool_funding).is_none());
		let pool_fee=sat(205_000)-pool.output.iter().map(|out|out.value).sum::<Amount>();
		assert!(pool_fee>Amount::ZERO);
		assert!(pool.output.iter().enumerate().filter(|(index,_)|*index!=pool_root.vout as usize).map(|(_,out)|out.value).sum::<Amount>()<sat(5_000));
		for grant in &grants {
			grant.validate(&pool).unwrap();assert_eq!(grant.policy().as_server_htlc_recv().unwrap().payment_hash,hash);
			assert_eq!(recipient.get_vtxo_key(grant).await.unwrap().public_key(),grant.user_pubkey());
		}
		let destinations=db.read(async |t|Ok(t.query("SELECT output_vtxo,materialized FROM expiry_lightning_receive_claim WHERE subscription_id=$1",&[&subscription]).await?)).await.unwrap();
		assert_eq!(destinations.len(),grants.len());
		for row in destinations {
			assert!(!row.get::<_,bool>("materialized"));
			let promised=Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
			assert_eq!(promised.amount(),sat(50_000));
			assert!(matches!(promised.policy(),ark::VtxoPolicy::Pubkey(_)));
			assert_eq!(recipient.get_vtxo_key(&promised).await.unwrap().public_key(),promised.user_pubkey());
		}
		assert_eq!(roots.len(),4); assert_eq!(roots.values().map(|out|out.value).sum::<Amount>(),sat(705_000));
		drop(sender); drop(recipient); drop(proxy);
		tokio::time::timeout(Duration::from_secs(10),async {
			while db.read(async |t|Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",&[&pending_pid]).await?.get::<_,bool>(0))).await.unwrap() { tokio::time::sleep(Duration::from_millis(20)).await; }
		}).await.expect("the actual interrupted WAL writer must be gone");
		let source_deadline=source.policy().as_server_htlc_send().unwrap().htlc_expiry;
		let grant_deadline=grants.iter().map(|grant|grant.policy().as_server_htlc_recv().unwrap().htlc_expiry).max().unwrap();
		assert!(source_deadline>=grant_deadline+srv.config().htlc_expiry_delta);
		let target=source_deadline.max(grant_deadline)+BlockDelta::new(1);
		let height=ctx.bitcoind().get_block_count().await as u32;
		assert!(height<grant_deadline.to_u32()); assert!(target.to_u32()-height<=96);
		ctx.generate_blocks(target.to_u32()-height).await;
		assert!(target>source_deadline && target>grant_deadline);
		srv.start().await.unwrap(); srv.watchmand().start().await.unwrap();
		let core=ctx.bitcoind().sync_client();
		let grant_ids=grants.iter().map(|grant|grant.id().to_string()).collect::<Vec<_>>();
		let closures=tokio::time::timeout(Duration::from_secs(40),async {
			loop {
				let point=server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
				srv.wait_for_sync_height(point.height).await; srv.watchmand().wait_for_sync_height(point.height).await;
				let rebuilt=db.read(async |t| { let Some(view)=t.expiry_rebuild_token().await? else {return Ok(false)}; Ok(view.captaind==point && view.watchmand==point && t.expiry_view_is_rebuilt(view).await?) }).await.unwrap();
				if !rebuilt {tokio::time::sleep(Duration::from_millis(50)).await;continue;}
				let rows=db.read(async |t|Ok(t.query("SELECT DISTINCT e.vtxo_id,a.sweep_tx,a.sweep_txid FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=ANY($1) AND c.active AND c.disposition='operator' AND a.broadcast_at IS NOT NULL",&[&grant_ids]).await?)).await.unwrap();
				let mut closures=BTreeMap::new(); let mut deep=rows.len()==grants.len();
				for row in rows {
					let tx=deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
					assert_eq!(tx.compute_txid().to_string(),row.get::<_,String>("sweep_txid"));
					let Some(info)=core.custom_get_raw_transaction_info(tx.compute_txid(),None).unwrap() else {
						deep=false; continue; // Publication on the server's Core can precede visibility here.
					};
					deep &= info.confirmations.unwrap_or(0)>=6;
					closures.insert(row.get::<_,String>("vtxo_id"),tx);
				}
				if deep {break closures;}
				assert_eq!(db.read(async |t|Ok(t.query_one("SELECT COUNT(*) FROM expiry_entitlement WHERE vtxo_id=$1",&[&source.id().to_string()]).await?.get::<_,i64>(0))).await.unwrap(),0,"the actual sender source must remain undecided until every grant closure is N6");
				// A legitimate signed split can precede the grant payout. Let the
				// ordinary watchman progress it; no controller split or replay.
				assert!(core.get_block_count().unwrap()-first_height<96);
				ctx.generate_blocks(1).await;
			}
		}).await.expect("expired admitted grants must close autonomously within40s");
		let binding=db.read(async |t|Ok(t.query_one("SELECT r.request,r.protocol_version,s.status='canceled' AS canceled, \
		EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$1) AS wal, \
		EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE payment_hash=$1 AND materialized) AS materialized, \
		EXISTS(SELECT 1 FROM expiry_lightning_receive_claim c JOIN vtxo v ON v.vtxo_id=c.source_vtxo_id JOIN htlc_vtxo h ON h.id=v.id WHERE c.subscription_id=r.subscription_id AND h.offchain_resolution='fulfilled') AS fulfilled \
		FROM expiry_lightning_receive_replay r JOIN lightning_htlc_subscription s ON s.id=r.subscription_id AND s.payment_hash=r.payment_hash \
		WHERE r.subscription_id=$2 AND r.payment_hash=$1",&[&hash.to_string(),&subscription]).await?)).await.unwrap();
		assert!(binding.get::<_,Vec<u8>>("request")==original,"the admitted request must remain immutable");
		assert_eq!(binding.get::<_,i64>("protocol_version"),protocol);
		assert!(binding.get::<_,bool>("canceled"));
		assert!(!binding.get::<_,bool>("wal") && !binding.get::<_,bool>("materialized") && !binding.get::<_,bool>("fulfilled"));
		let invoices=lightning.internal.hold_client().await.list(cln_rpc::plugins::hold::ListRequest {
			constraint:Some(cln_rpc::plugins::hold::list_request::Constraint::PaymentHash(hash.to_vec())),
		}).await.unwrap().into_inner().invoices;
		assert_eq!(invoices.len(),1);assert_eq!(invoices[0].invoice,invoice_text);assert_eq!(invoices[0].payment_hash,hash.to_vec());
		assert_eq!(invoices[0].state(),cln_rpc::plugins::hold::InvoiceState::Cancelled);
		let parent=tokio::time::timeout(Duration::from_secs(30),async {
			loop {
				let row=db.read(async |t|Ok(t.query_opt("SELECT a.sweep_tx,a.sweep_txid,f.raw_tx,f.txid AS fee_child_txid FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id JOIN expiry_allocation a ON a.id=c.allocation_id JOIN expiry_fee_child f ON f.allocation_id=a.id WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL ORDER BY f.id DESC LIMIT 1",&[&source.id().to_string()]).await?)).await.unwrap();
				if let Some(row)=row {
					let parent=deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
					let child=deserialize::<Transaction>(row.get("raw_tx")).unwrap();
					assert_eq!(parent.compute_txid().to_string(),row.get::<_,String>("sweep_txid"));
					assert_eq!(child.compute_txid().to_string(),row.get::<_,String>("fee_child_txid"));
					let anchors=parent.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
					assert_eq!(anchors.len(),1);
					let anchor=OutPoint::new(parent.compute_txid(),anchors[0].0 as u32);
					assert!(child.input.iter().any(|input|input.previous_output==anchor));
					// Publication on the server's Core can precede both transactions reaching the mining Core.
					if let (Some(parent_info),Some(child_info))=(
						core.custom_get_raw_transaction_info(parent.compute_txid(),None).unwrap(),
						core.custom_get_raw_transaction_info(child.compute_txid(),None).unwrap(),
					) {
						assert_eq!(deserialize::<Transaction>(&parent_info.hex).unwrap(),parent);
						assert_eq!(deserialize::<Transaction>(&child_info.hex).unwrap(),child);
						break parent;
					}
				}
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}).await.expect("the original50k source must receive its rightful refund after grant closureN6");
		let source_path=std::iter::once(source.chain_anchor()).chain(source.transactions().map(|step|OutPoint::new(step.tx.compute_txid(),step.output_idx as u32))).collect::<BTreeSet<_>>();
		assert!(parent.input.iter().any(|input|source_path.contains(&input.previous_output)),"the actual refund must consume its original signed50k source path");
		let outputs=parent.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==destination.spk).collect::<Vec<_>>();
		assert_eq!(outputs.len(),1); assert_eq!(outputs[0].1.value,sat(49_000));
		let payout=OutPoint::new(parent.compute_txid(),outputs[0].0 as u32);
		let tip=ctx.generate_blocks(6).await; srv.wait_for_sync_height(tip).await; srv.watchmand().wait_for_sync_height(tip).await;
		let actual=core.get_tx_out(&payout.txid,payout.vout,Some(true)).unwrap().unwrap();
		assert!(actual.confirmations>=6); assert_eq!(actual.value,sat(49_000)); assert_eq!(actual.script_pub_key.script().unwrap(),destination.spk);
		let mut known=roots.clone(); let mut terminal=roots.clone(); let mut graph=BTreeMap::new();
		let mut spent=BTreeSet::new(); let mut miners=Amount::ZERO; let mut customer_outputs=BTreeMap::new();
		for height in first_height+1..=core.get_block_count().unwrap() {
			for tx in core.get_block(&core.get_block_hash(height).unwrap()).unwrap().txdata {
				assert!(tx.output.iter().all(|out|out.script_pubkey!=recipient_destination.spk),"the uncollected recipient must not receive a fallback");
				for (index,out) in tx.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==destination.spk) {
					assert!(customer_outputs.insert(OutPoint::new(tx.compute_txid(),index as u32),out.clone()).is_none());
				}
				if !tx.input.iter().any(|input|known.contains_key(&input.previous_output)) {continue;}
				let mut value=Amount::ZERO;
				for input in &tx.input { assert!(spent.insert(input.previous_output)); value+=known.get(&input.previous_output).expect("undeclared funding entered the finite cut").value; assert!(terminal.remove(&input.previous_output).is_some()); }
				miners+=value.checked_sub(tx.output.iter().map(|out|out.value).sum()).unwrap();
				let txid=tx.compute_txid();
				for (index,out) in tx.output.iter().enumerate() {
					let point=OutPoint::new(txid,index as u32); known.insert(point,out.clone());terminal.insert(point,out.clone());
				}
				assert!(graph.insert(txid,tx).is_none()); assert!(graph.len()<=64);
			}
		}
		let mut expected_customer_outputs=BTreeMap::from([(payout,parent.output[payout.vout as usize].clone())]);
		// A wallet may legitimately use the same script for its initial funding
		// or recorded board change. Permit only those exact pre-captured points.
		for (point,out) in roots.iter().chain(customer_change.iter()).filter(|(_,out)|out.script_pubkey==destination.spk) {
			assert!(expected_customer_outputs.insert(*point,out.clone()).is_none());
		}
		assert_eq!(customer_outputs,expected_customer_outputs,"an undeclared funding root cannot hide a duplicate payment to the known sender script");
		assert_eq!(sat(705_000),terminal.values().map(|out|out.value).sum::<Amount>()+miners);
		assert!(miners>=board_fee+pool_fee);
		for tx in closures.values().chain(std::iter::once(&parent)) {
			assert_eq!(graph.get(&tx.compute_txid()),Some(tx)); assert_eq!(tx.version.0,3); assert!(tx.vsize()<=10_000);
			let inputs=tx.input.iter().map(|input|known[&input.previous_output].value).sum::<Amount>();
			assert_eq!(inputs,tx.output.iter().map(|out|out.value).sum());
			if tx.compute_txid()==parent.compute_txid() {
				assert_eq!(inputs,sat(50_000));
				assert_eq!(tx.output.iter().filter(|out|out.script_pubkey==operator_script).map(|out|out.value).sum::<Amount>(),sat(1_000));
			}
			let anchors=tx.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
			assert_eq!(anchors.len(),1);assert_eq!(anchors[0].1.value,Amount::ZERO);
			let anchor=OutPoint::new(tx.compute_txid(),anchors[0].0 as u32);
			let children=graph.values().filter(|child|child.input.iter().any(|input|input.previous_output==anchor)).collect::<Vec<_>>();
			assert_eq!(children.len(),1);assert_eq!(children[0].version.0,3);assert!(children[0].vsize()<=1_000);
			let info:serde_json::Value=core.call("getrawtransaction",&[serde_json::json!(tx.compute_txid().to_string()),serde_json::json!(true)]).unwrap();
			let child_info:serde_json::Value=core.call("getrawtransaction",&[serde_json::json!(children[0].compute_txid().to_string()),serde_json::json!(true)]).unwrap();
			assert_eq!(info["blockhash"],child_info["blockhash"]); assert!(info["confirmations"].as_u64().unwrap()>=6);assert!(child_info["confirmations"].as_u64().unwrap()>=6);
			let fee=children[0].input.iter().map(|input|known[&input.previous_output].value).sum::<Amount>()-children[0].output.iter().map(|out|out.value).sum::<Amount>();assert!(fee>Amount::ZERO);
			let recorded=db.read(async |t|Ok(t.query_one("SELECT raw_tx,fee_sat FROM expiry_fee_child WHERE txid=$1",&[&children[0].compute_txid().to_string()]).await?)).await.unwrap();
			assert_eq!(deserialize::<Transaction>(recorded.get(0)).unwrap(),*children[0]);assert_eq!(recorded.get::<_,i64>(1),i64::try_from(fee.to_sat()).unwrap());
		}
		for grant in &grants {
			let closure=&closures[&grant.id().to_string()];
			let path=std::iter::once(grant.chain_anchor()).chain(grant.transactions().map(|step|OutPoint::new(step.tx.compute_txid(),step.output_idx as u32))).collect::<BTreeSet<_>>();
			assert!(closure.input.iter().any(|input|path.contains(&input.previous_output)),"the actual operator closure must consume this signed grant's backing path");
		}
		let mut operator_points=srv.wallet_status().await.rounds.confirmed_utxos.into_iter().collect::<BTreeSet<_>>();
		let admin=srv.watchmand().config().admin_address.unwrap();
		let mut rpc=WalletAdminClient::connect(format!("http://{admin}")).await.unwrap();
		operator_points.extend(rpc.wallet_status(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap().confirmed_utxos.into_iter().map(|point|point.parse::<OutPoint>().unwrap()));
		for (point,out) in &terminal {
			if out.value==Amount::ZERO {continue;}
			let actual=core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().unwrap();assert!(actual.confirmations>=6);assert_eq!(actual.value,out.value);assert_eq!(actual.script_pub_key.script().unwrap(),out.script_pubkey);
			assert!(*point==payout || customer_change.get(point)==Some(out) || out.script_pubkey==operator_script || operator_points.contains(point));
		}
		for (point,out) in &customer_change {assert_eq!(terminal.get(point),Some(out));}
		sender_config.server_address=srv.ark_url();
		for _ in 0..2 {
			let returned=Box::pin(Wallet::open(Network::Regtest,WalletSeed::new_from_mnemonic(Network::Regtest,&mnemonic),sender_config.clone(),OpenWalletArgs {datadir:Some(ctx.datadir.join("sender")),run_daemon:false,..Default::default()})).await.unwrap();
			for _ in 0..2 {Box::pin(returned.sync()).await;}
			assert!(returned.lightning_send_checkpoint(hash).await.unwrap().is_none());assert!(!returned.is_invoice_paid(hash).await.unwrap());
			assert_eq!(returned.get_vtxo_by_id(source.id()).await.unwrap().state,bark::vtxo::VtxoState::Spent);
			let history=returned.history().await.unwrap();
			let sends=history.iter().filter(|movement|movement.subsystem.is_subsystem(Subsystem::LIGHTNING_SEND)).collect::<Vec<_>>();
			assert_eq!(sends.len(),1);assert_eq!(sends[0].id,htlcs.movement_id);assert_eq!(sends[0].status,MovementStatus::Failed);assert_eq!(sends[0].effective_balance.to_sat(),0);assert_eq!(sends[0].offchain_fee,Amount::ZERO);
			let debits=history.iter().filter(|movement|movement.effective_balance.to_sat()<0).collect::<Vec<_>>();
			assert_eq!(debits.len(),1);assert_eq!(debits[0].subsystem.name,"abandon.fallback_payout");assert_eq!(debits[0].effective_balance.to_sat(),-50_000);assert_eq!(debits[0].offchain_fee,sat(1_000));assert_eq!(debits[0].status,MovementStatus::Successful);
			assert_eq!(debits[0].input_vtxos,vec![source.id()]);
			let receipts=returned.fallback_payouts().await.unwrap();
			let receipt=receipts.iter().find(|entry|entry.vtxo_id==source.id()).unwrap();
			assert_eq!(receipt.gross,sat(50_000));
			let attempt=receipt.attempts.iter().find(|attempt|attempt.active).unwrap();
			assert_eq!(attempt.fee,sat(1_000));assert_eq!(attempt.payout,sat(49_000));
			assert_eq!(OutPoint::new(attempt.parent.as_ref().unwrap().compute_txid(),attempt.output_index.unwrap()),payout);
			assert_eq!(history.iter().map(|movement|movement.effective_balance.to_sat()).sum::<i64>(),0);
		}
		assert!(core.get_block_count().unwrap()-first_height<=96);
	})).await.expect("the admitted-but-uncollected expiry/refund case exceeded180seconds");
}
