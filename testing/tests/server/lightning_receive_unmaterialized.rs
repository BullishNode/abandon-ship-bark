//! A collected receive pays its recipient after the forwarding provider stops
//! before the recipient's output is stored.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ark::{ProtocolEncoding, ServerVtxo, Vtxo};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;
use ark_testing::{sat, Captaind, TestContext};
use ark_testing::daemon::captaind::{self, ArkClient, MailboxClient, WalletAdminClient};
use bitcoin::{Amount, OutPoint, ScriptBuf, Transaction, TxOut};
use bitcoin::consensus::deserialize;
use bitcoin::hex::DisplayHex;
use bitcoin_ext::{AmountExt, BlockDelta, TxOutExt};
use bitcoin_ext::rpc::RpcApi;
use cln_rpc::plugins::hold;
use cln_rpc::listpays_pays::ListpaysPaysStatus;
use server::database::{Db, SpendState};
use server::vtxopool::VtxoTarget;
use server_rpc::protos;
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct ReceiveGate {
	prepared: Arc<Notify>,
	release: Arc<Notify>,
	claims: Arc<AtomicUsize>,
	registrations: Arc<AtomicUsize>,
	posts: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for ReceiveGate {
	async fn prepare_lightning_receive_claim(&self, upstream: &mut ArkClient,
		request: protos::PrepareLightningReceiveClaimRequest,
	) -> Result<protos::PrepareLightningReceiveClaimResponse, tonic::Status> {
		let response = upstream.prepare_lightning_receive_claim(request).await?.into_inner();
		self.prepared.notify_one();
		self.release.notified().await;
		Ok(response)
	}

	async fn claim_lightning_receive(&self, upstream: &mut ArkClient,
		request: protos::ClaimLightningReceiveRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		assert_eq!(self.claims.fetch_add(1, Ordering::SeqCst), 0, "only one public claim is admitted");
		Ok(upstream.claim_lightning_receive(request).await?.into_inner())
	}

	async fn register_vtxo_transactions(&self, _upstream: &mut ArkClient,
		_request: protos::RegisterVtxoTransactionsRequest,
	) -> Result<protos::Empty, tonic::Status> {
		self.registrations.fetch_add(1, Ordering::SeqCst);
		Err(tonic::Status::unavailable("provider registration must never be reached"))
	}
}

#[async_trait::async_trait]
impl captaind::proxy::MailboxRpcProxy for ReceiveGate {
	async fn post_arkoor_message(&self, _upstream: &mut MailboxClient,
		_request: protos::mailbox_server::PostArkoorMessageRequest,
	) -> Result<protos::core::Empty, tonic::Status> {
		self.posts.fetch_add(1, Ordering::SeqCst);
		Err(tonic::Status::unavailable("recipient remains absent"))
	}
}

struct Binding {
	request: Vec<u8>,
	protocol: i64,
	subscription: i64,
	source: Vtxo<Full>,
	promised: Vtxo<Full>,
}

async fn original_binding(db: &Db, hash: PaymentHash, materialized: bool) -> Binding {
	let rows = db.read(async |tx| Ok(tx.query("SELECT r.request,r.protocol_version,r.subscription_id, \
		c.source_vtxo_id,c.output_vtxo_id,c.output_vtxo,c.materialized,source.vtxo AS source_vtxo, \
		s.status::TEXT AS status,h.offchain_resolution::TEXT AS resolution, \
		actual.vtxo AS actual_vtxo,actual.spend_state::TEXT AS output_state \
		FROM expiry_lightning_receive_replay r JOIN expiry_lightning_receive_claim c \
		ON c.subscription_id=r.subscription_id AND c.payment_hash=r.payment_hash \
		JOIN lightning_htlc_subscription s ON s.id=r.subscription_id AND s.payment_hash=r.payment_hash \
		JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id AND source.lightning_htlc_subscription_id=s.id \
		JOIN htlc_vtxo h ON h.id=source.id AND h.direction='outgoing' AND h.payment_hash=r.payment_hash \
		LEFT JOIN vtxo actual ON actual.vtxo_id=c.output_vtxo_id WHERE r.payment_hash=$1",
		&[&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(rows.len(), 1, "one receive source and one immutable destination");
	let row = &rows[0];
	let source = ServerVtxo::<Full>::deserialize(row.get("source_vtxo")).unwrap().try_into_user_vtxo().unwrap();
	let promised = Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
	assert_eq!(row.get::<_, &str>("source_vtxo_id"), source.id().to_string());
	assert_eq!(row.get::<_, &str>("output_vtxo_id"), promised.id().to_string());
	assert_eq!(row.get::<_, &str>("status"), "settled");
	assert_eq!(row.get::<_, bool>("materialized"), materialized);
	if materialized {
		assert_eq!(row.get::<_, Option<&str>>("resolution"), Some("fulfilled"));
		assert_eq!(row.get::<_, Option<&str>>("output_state"), Some("unregistered"));
		let actual = Vtxo::<Full>::deserialize(row.get::<_, Option<&[u8]>>("actual_vtxo").unwrap()).unwrap();
		assert_eq!(actual.id(), promised.id());
		assert_eq!(actual.policy(), promised.policy());
		assert_eq!(actual.amount(), promised.amount());
		assert!(!actual.has_all_witnesses(), "callerless recovery does not invent the absent provider's signatures");
	} else {
		assert!(row.get::<_, Option<&[u8]>>("actual_vtxo").is_none());
		assert!(row.get::<_, Option<&str>>("resolution").is_none());
	}
	let request = row.get::<_, Vec<u8>>("request");
	assert!(!request.is_empty());
	let protocol = row.get("protocol_version");
	assert!(protocol > 0);
	Binding { request, protocol, subscription: row.get("subscription_id"), source, promised }
}

fn unchanged(original: &Binding, actual: &Binding) {
	// The stored request contains a preimage. Never print its bytes on failure.
	assert!(actual.request == original.request, "original admitted request changed");
	assert_eq!(actual.protocol, original.protocol);
	assert_eq!(actual.subscription, original.subscription);
	assert_eq!(actual.source.id(), original.source.id());
	assert_eq!(actual.promised.id(), original.promised.id());
	assert_eq!(actual.promised.policy(), original.promised.policy());
	assert_eq!(actual.promised.amount(), original.promised.amount());
}

async fn rebuilt(ctx: &TestContext, db: &Db) {
	let point = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let done = db.read(async |tx| {
				let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
				Ok(view.captaind == point && view.watchmand == point && tx.expiry_view_is_rebuilt(view).await?)
			}).await.unwrap();
			if done { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both daemon financial views must rebuild the Core tip");
}

/// Scan the entire bounded canonical chain before selecting descendants. The
/// physical cut includes unused sponsor/pool capital, not just the payee parent.
async fn canonical_cut(ctx: &TestContext, srv: &Captaind, boundary: &BTreeMap<OutPoint, TxOut>,
	recipient: &ScriptBuf, provider: &ScriptBuf, payout: OutPoint,
) {
	let core = ctx.bitcoind().sync_client();
	let height = core.get_block_count().unwrap();
	assert!(height <= 512, "bounded fixture exceeded its declared chain scan");
	let mut inventory = Vec::new();
	let mut canonical_inputs = BTreeSet::new();
	let mut customer_outputs = BTreeMap::new();
	for index in 0..=height {
		let block = core.get_block(&core.get_block_hash(index).unwrap()).unwrap();
		for transaction in block.txdata {
			let txid = transaction.compute_txid();
			for (vout, output) in transaction.output.iter().enumerate() {
				assert_ne!(output.script_pubkey, *provider, "no canonical provider fallback/refund is permitted");
				if output.script_pubkey == *recipient {
					assert!(customer_outputs.insert(OutPoint::new(txid, vout as u32), output.value.to_sat()).is_none());
				}
			}
			if !transaction.is_coinbase() {
				for input in &transaction.input {
					assert!(canonical_inputs.insert(input.previous_output), "canonical input reuse");
				}
				inventory.push((index, transaction));
			}
		}
	}
	assert_eq!(customer_outputs, BTreeMap::from([(payout, 99_000)]));
	let mut frontier = boundary.clone();
	let mut fees = 0_u64;
	let mut selected = 0;
	let mut child = None;
	let parent = inventory.iter().find(|(_, tx)| tx.compute_txid() == payout.txid).unwrap();
	let anchors = parent.1.output.iter().enumerate().filter(|(_, out)| out.is_p2a_fee_anchor())
		.map(|(vout, out)| { assert_eq!(out.value, Amount::ZERO); OutPoint::new(payout.txid, vout as u32) }).collect::<Vec<_>>();
	assert_eq!(anchors.len(), 1);
	for (mined, transaction) in &inventory {
		if !transaction.input.iter().any(|input| frontier.contains_key(&input.previous_output)) { continue; }
		let mut input_value = 0_u64;
		for input in &transaction.input {
			let output = frontier.remove(&input.previous_output).expect("cut transaction has an undeclared external input");
			input_value = input_value.checked_add(output.value.to_sat()).unwrap();
		}
		let output_value = transaction.output.iter().map(|output| output.value.to_sat()).sum::<u64>();
		let miner = input_value.checked_sub(output_value).unwrap();
		fees += miner;
		let txid = transaction.compute_txid();
		if transaction.input.iter().any(|input| input.previous_output == anchors[0]) {
			assert!(child.replace(*mined).is_none(), "one fee child");
			assert!(miner > 0);
			assert!(transaction.input.iter().any(|input| boundary.contains_key(&input.previous_output)),
				"the child uses a separately declared confirmed fee sponsor");
		}
		for (vout, output) in transaction.output.iter().enumerate() {
			assert!(frontier.insert(OutPoint::new(txid, vout as u32), output.clone()).is_none());
		}
		selected += 1;
	}
	let parent_height = inventory.iter().find(|(_, tx)| tx.compute_txid() == payout.txid).unwrap().0;
	let child_height = child.expect("Core must contain the funded fee child");
	assert_eq!(child_height, parent_height);
	assert!(height + 1 - parent_height >= 6);
	let root_total = boundary.values().map(|output| output.value.to_sat()).sum::<u64>();
	let terminal = frontier.values().map(|output| output.value.to_sat()).sum::<u64>();
	assert_eq!(root_total, terminal + fees);
	assert_eq!(frontier.get(&payout).unwrap().value, sat(99_000));
	assert_eq!(selected, 2, "one complete physical-root parent and its funded fee child");
	let operator = {
		let config = srv.watchmand().config();
		config.sweep_address.clone().unwrap().require_network(config.network).unwrap().script_pubkey()
	};
	let operator_terminal = frontier.values().filter(|out| out.script_pubkey == operator)
		.map(|out| out.value.to_sat()).sum::<u64>();
	assert_eq!(operator_terminal, 101_000, "unused pool principal100k and output charge1k remain operator capital");
	let watch_owned = frontier.iter().filter(|(point, out)| **point != payout && out.value > Amount::ZERO
		&& out.script_pubkey != operator).map(|(point, _)| *point).collect::<BTreeSet<_>>();
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let address = srv.watchmand().config().admin_address.unwrap();
			let mut rpc = WalletAdminClient::connect(format!("http://{address}")).await.unwrap();
			let coins = rpc.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap()
				.confirmed_utxos.into_iter().map(|point| OutPoint::from_str(&point).unwrap()).collect::<BTreeSet<_>>();
			if watch_owned.is_subset(&coins) { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("unused fee funding and fee change must remain in the watchman wallet");
	let watch_total = frontier.iter().filter(|(point, _)| watch_owned.contains(*point))
		.map(|(_, out)| out.value.to_sat()).sum::<u64>();
	assert_eq!(watch_total + fees, 200_000);
	for (point, output) in &frontier {
		if output.value == Amount::ZERO { continue; }
		let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, output.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), output.script_pubkey);
	}
	for txid in core.get_raw_mempool().unwrap() {
		let transaction = core.get_raw_transaction(&txid, None).unwrap();
		assert!(transaction.input.iter().all(|input| !frontier.contains_key(&input.previous_output)),
			"no selected terminal may await spending");
		assert!(transaction.output.iter().all(|out| out.script_pubkey != *recipient && out.script_pubkey != *provider),
			"no late or duplicate known customer output may hide in the mempool");
	}
}

#[tokio::test]
async fn collected_external_receive_materializes_without_provider_then_pays_recipient_once() {
	let ctx = TestContext::new("server/lightning_receive_unmaterialized").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(sat(500_000))
		.cfg(|config| {
			config.vtxo_lifetime = BlockDelta::new(32);
			config.vtxopool.vtxo_lifetime = BlockDelta::new(32);
			// Issuance requires two targets. After one receive the deficit is
			// only one and the normal replenisher deliberately does not issue.
			config.vtxopool.vtxo_targets = vec![VtxoTarget { count: 2, amount: sat(100_000) }];
			config.ln_receive_anti_dos_required = false;
		}).watchmand().create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let fee_address = srv.watchmand().wait_wallet_address().await;
	let mut sponsors = BTreeMap::new();
	for _ in 0..2 {
		let txid = ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
		let transaction = ctx.bitcoind().await_transaction(txid).await;
		let vout = transaction.output.iter().position(|out| out.value == sat(100_000)
			&& out.script_pubkey == fee_address.script_pubkey()).unwrap();
		sponsors.insert(OutPoint::new(txid, vout as u32), transaction.output[vout].clone());
	}
	let tip = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	for (point, output) in &sponsors {
		let actual = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, output.value);
	}
	let gate = ReceiveGate::default();
	let proxy = srv.start_proxy_with_mailbox(gate.clone(), gate.clone()).await;
	let provider = ctx.bark_sdk("provider", &proxy).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	provider.stop_daemon_wait().await.unwrap();
	let provider_destination = provider.fallback_destination().await.unwrap();
	assert_eq!(provider.balance().await.unwrap().spendable, sat(0));
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	let recipient_key = address.policy().user_pubkey();
	let destination = recipient.fallback_destination().await.unwrap();
	let recipient_mailbox = recipient.mailbox_keypair().public_key().to_string();
	assert_ne!(destination.account_xpub, provider_destination.account_xpub);
	assert_ne!(destination.spk, provider_destination.spk);
	assert_eq!(recipient.balance().await.unwrap().spendable, sat(0));
	drop(recipient);
	let gross = sat(100_000);
	assert_eq!(srv.ark_info().await.fees.lightning_receive.calculate(gross).unwrap(), sat(0));
	let invoice = provider.bolt11_invoice_for_address(gross, address.clone(), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let registered = db.read(async |tx| tx.get_fallback(recipient_mailbox.parse().unwrap()).await).await.unwrap().unwrap();
	registered.verify(bitcoin::Network::Regtest, recipient_mailbox.parse().unwrap()).unwrap();
	assert_eq!(registered.spk, destination.spk);
	assert_eq!(registered.account_xpub, destination.account_xpub);
	let operation_provider = provider.clone();
	let claim = tokio::spawn(async move { operation_provider.try_claim_lightning_receive(hash, true).await });
	let payment = lightning.external.try_pay_bolt11(invoice.to_string());
	tokio::pin!(payment);
	tokio::select! {
		_ = gate.prepared.notified() => {},
		_ = &mut payment => panic!("payment completed before prepare pause"),
		_ = tokio::time::sleep(Duration::from_secs(30)) => panic!("receive grants did not prepare"),
	}
	let (original, writer) = db.write(async |tx| {
		// This existing PostgreSQL scheduling barrier changes no financial row.
		tx.batch_execute("SET LOCAL lock_timeout='5s'; LOCK TABLE virtual_transaction IN SHARE MODE").await?;
		let blocker = tx.query_one("SELECT pg_backend_pid()", &[]).await?.get::<_, i32>(0);
		gate.release.notify_one();
		let writer = tokio::time::timeout(Duration::from_secs(20), async {
			loop {
				tx.query_one("SELECT pg_stat_clear_snapshot()", &[]).await?;
				let rows = tx.query("SELECT DISTINCT a.pid FROM pg_stat_activity a JOIN pg_locks l ON l.pid=a.pid \
					WHERE a.datname=current_database() AND a.wait_event_type='Lock' \
					AND a.query LIKE '%INSERT INTO virtual_transaction%' AND l.relation='virtual_transaction'::regclass \
					AND NOT l.granted AND l.mode='RowExclusiveLock' AND $1=ANY(pg_blocking_pids(a.pid))",
					&[&blocker]).await?;
				assert!(rows.len() <= 1, "quiescent pool and one public claim must identify one blocked tree writer");
				if let Some(row) = rows.first() { break Ok::<_, anyhow::Error>(row.get::<_, i32>(0)); }
				tokio::time::sleep(Duration::from_millis(20)).await;
			}
		}).await.expect("receive never reached the owned table barrier")?;
		tokio::time::timeout(Duration::from_secs(20), &mut payment).await
			.expect("original payment did not complete").expect("external payment failed");
		let invoices = lightning.internal.hold_client().await.list(hold::ListRequest {
			constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
		}).await.unwrap().into_inner().invoices;
		assert_eq!(invoices.len(), 1);
		assert_eq!(invoices[0].invoice, invoice.to_string());
		assert_eq!(invoices[0].payment_hash, hash.to_vec());
		assert_eq!(invoices[0].state(), hold::InvoiceState::Paid);
		let pays = lightning.external.grpc_client().await.list_pays(cln_rpc::ListpaysRequest {
			bolt11: None, payment_hash: Some(hash.to_vec()), status: None,
			index: None, limit: None, start: None,
		}).await.unwrap().into_inner().pays;
		assert_eq!(pays.len(), 1);
		assert_eq!(pays[0].payment_hash, hash.to_vec());
		assert_eq!(pays[0].status(), ListpaysPaysStatus::Complete);
		assert_eq!(pays[0].amount_msat.as_ref().unwrap().msat, gross.to_msat());
		let original = original_binding(&db, hash, false).await;
		assert_eq!(original.source.amount(), gross);
		assert_eq!(original.promised.amount(), gross);
		assert_eq!(original.promised.policy(), address.policy());
		assert_eq!(original.promised.user_pubkey(), recipient_key);
		assert_ne!(original.source.user_pubkey(), recipient_key);
		assert!(original.source.has_all_witnesses(), "the grant itself was signed before the interruption");
		let funding = ctx.bitcoind().await_transaction(original.source.chain_anchor().txid).await;
		original.source.validate(&funding).unwrap();
		assert_eq!(gate.claims.load(Ordering::SeqCst), 1);
		assert_eq!(gate.registrations.load(Ordering::SeqCst), 0);
		assert_eq!(gate.posts.load(Ordering::SeqCst), 0);
		assert!(!claim.is_finished());
		// Keep SHARE held until the server process is stopped and reaped.
		// Abort the only explicit provider action; no provider clone survives.
		claim.abort();
		let _ = claim.await;
		drop(provider);
		let _ = proxy.stop.send(());
		unchanged(&original, &original_binding(&db, hash, false).await);
		assert_eq!(gate.claims.load(Ordering::SeqCst), 1);
		assert_eq!(gate.registrations.load(Ordering::SeqCst), 0);
		assert_eq!(gate.posts.load(Ordering::SeqCst), 0);
		srv.stop().await.unwrap();
		Ok((original, writer))
	}).await.unwrap();
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let alive = db.read(async |tx| Ok(tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
				&[&writer]).await?.get::<_, bool>(0))).await.unwrap();
			if !alive { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("stopped tree writer must release its incomplete transaction");
	unchanged(&original, &original_binding(&db, hash, false).await);
	let ancestor_deadline = original.source.policy().as_server_htlc_recv().unwrap().htlc_expiry;
	let anchor = original.source.chain_anchor();
	let anchor_transaction = ctx.bitcoind().await_transaction(anchor.txid).await;
	let anchor_output = anchor_transaction.output[anchor.vout as usize].clone();
	assert_eq!(anchor_output.value, sat(200_000), "the full two-target backing includes unused operator pool principal");
	let confirmed_root = ctx.bitcoind().sync_client().get_tx_out(&anchor.txid, anchor.vout, Some(true)).unwrap().unwrap();
	assert!(confirmed_root.confirmations >= 6);
	assert_eq!(confirmed_root.value, anchor_output.value);
	let mut boundary = sponsors;
	assert!(boundary.insert(anchor, anchor_output).is_none());
	assert_eq!(boundary.values().map(|output| output.value).sum::<Amount>(), sat(400_000));
	// The original signed grants, unused pool principal and callerless request
	// remain intact. Disable only future issuance through the ordinary config:
	// after expiry a two-target replenishment could recycle operator capital
	// into unrelated new backing and contaminate this finite physical cut.
	srv.config_mut().vtxopool.vtxo_targets.clear();
	srv.start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let done = db.read(async |tx| Ok(tx.query_one("SELECT c.materialized FROM expiry_lightning_receive_claim c WHERE c.payment_hash=$1",
				&[&hash.to_string()]).await?.get::<_, bool>(0))).await.unwrap();
			if done { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("callerless original-request recovery must materialize without the provider");
	unchanged(&original, &original_binding(&db, hash, true).await);
	let stored = db.read(async |tx| tx.get_user_vtxo_by_id(original.promised.id()).await).await.unwrap();
	assert_eq!(stored.spend_state, SpendState::Unregistered);
	let height = ctx.bitcoind().get_block_count().await as u32;
	let target = original.source.expiry_height().to_u32() + 1;
	assert!(target > height);
	let expired = ctx.generate_blocks(target - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let parent = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let rows = db.read(async |tx| Ok(tx.query("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL",
				&[&original.promised.id().to_string()]).await?)).await.unwrap();
			assert!(rows.len() <= 1);
			if let Some(row) = rows.first() { break deserialize::<Transaction>(row.get("sweep_tx")).unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("expiry path must pay the absent recipient from its backing");
	let outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert_eq!(outputs[0].1.value, sat(99_000));
	let payout = OutPoint::new(parent.compute_txid(), outputs[0].0 as u32);
	assert_eq!(ctx.bitcoind().await_transaction(payout.txid).await, parent);
	let mut parent_inputs = Amount::ZERO;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		parent_inputs += previous.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(parent_inputs, parent.output.iter().map(|out| out.value).sum::<Amount>());
	assert_eq!(parent.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>(), BTreeSet::from([anchor]));
	assert_eq!(parent.input.len(), 1);
	let confirmed = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	for observation in 0..2 {
		if observation == 1 {
			let height = ctx.bitcoind().get_block_count().await as u32;
			let late = (ancestor_deadline.to_u32() + 1).max(original.source.expiry_height().to_u32() + 1).max(height + 6);
			let tip = ctx.generate_blocks(late - height).await;
			srv.wait_for_sync_height(tip).await;
			srv.watchmand().wait_for_sync_height(tip).await;
		}
		rebuilt(&ctx, &db).await;
		let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,c.owner_mailbox,c.user_pubkey,c.spk,c.gross_sat,c.fee_sat, \
			c.payout_sat,c.forfeited_sat,c.active,a.sweep_txid,a.status FROM expiry_allocation_coin c \
			JOIN expiry_allocation a ON a.id=c.allocation_id \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN vtxo v ON v.vtxo_id=e.vtxo_id \
			WHERE v.anchor_point=$1 AND c.disposition='customer'", &[&anchor.to_string()]).await?)).await.unwrap();
		assert_eq!(rows.len(), 1, "old grant, provider checkpoint and recipient leaf cannot each get paid");
		let row = &rows[0];
		assert_eq!(row.get::<_, &str>("vtxo_id"), original.promised.id().to_string());
		assert_eq!(row.get::<_, &str>("owner_mailbox"), recipient_mailbox);
		assert_eq!(row.get::<_, &str>("user_pubkey"), recipient_key.to_string());
		assert_eq!(row.get::<_, &str>("spk"), destination.spk.as_bytes().to_lower_hex_string());
		assert_eq!(row.get::<_, i64>("gross_sat"), 100_000);
		assert_eq!(row.get::<_, i64>("fee_sat"), 1_000);
		assert_eq!(row.get::<_, i64>("payout_sat"), 99_000);
		assert_eq!(row.get::<_, i64>("forfeited_sat"), 0);
		assert!(row.get::<_, bool>("active"));
		assert_eq!(row.get::<_, String>("status"), "confirmed");
		assert_eq!(row.get::<_, Option<String>>("sweep_txid"), Some(payout.txid.to_string()));
		canonical_cut(&ctx, &srv, &boundary, &destination.spk, &provider_destination.spk, payout).await;
	}
	assert_eq!(gate.claims.load(Ordering::SeqCst), 1);
	assert_eq!(gate.registrations.load(Ordering::SeqCst), 0);
	assert_eq!(gate.posts.load(Ordering::SeqCst), 0);
}
