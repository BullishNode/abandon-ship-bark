//! Funded attribution under registration pressure, with the scanner and readiness tasks running.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ark::fallback::FallbackRecord;
use ark::vtxo::Full;
use ark::{ProtocolEncoding, Vtxo, VtxoId, SECP};
use ark::mailbox::MailboxAuthorization;
use ark_testing::{sat, Captaind, TestContext};
use ark_testing::constants::{BOARD_CONFIRMATIONS, ROUND_CONFIRMATIONS};
use ark_testing::context::builders::{BarkSdkBuilder, CaptaindBuilder};
use ark_testing::daemon::captaind::{self, ArkClient, MailboxClient, WalletAdminClient};
use ark_testing::daemon::{Daemon, DaemonHelper};
use ark_testing::daemon::postgres::PostgresHelper;
use bitcoin::bip32::Xpriv;
use bitcoin::consensus::deserialize;
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::{Keypair, PublicKey};
use bitcoin::{Amount, Network, OutPoint, ScriptBuf, Transaction, TxOut};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use serde_json::{json, Value};
use server::database::Db;
use server::database::expiry_inventory::{BackingPartitionResidual, BackingSnapshot, BackingSnapshotLimits};
use server_rpc::protos;

/// A temporary failure of the SDK's best-effort recovery post. Every
/// board cosignature, funding transaction and registration uses the server.
#[derive(Clone, Default)]
struct RecoveryPostUnavailable(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl captaind::proxy::MailboxRpcProxy for RecoveryPostUnavailable {
	async fn post_recovery_vtxo_ids(
		&self, _upstream: &mut MailboxClient, _request: protos::mailbox_server::PostRecoveryVtxoIdsRequest,
	) -> Result<protos::core::Empty, tonic::Status> {
		self.0.fetch_add(1, Ordering::SeqCst);
		Err(tonic::Status::unavailable("recovery-post outage"))
	}
}

#[inline(never)]
fn create_server(builder: CaptaindBuilder<'_>) -> Pin<Box<dyn Future<Output = Arc<Captaind>> + '_>> {
	Box::pin(builder.create())
}

#[inline(never)]
fn create_wallet(builder: BarkSdkBuilder<'_>) -> Pin<Box<dyn Future<Output = bark::Wallet> + '_>> {
	Box::pin(builder.create())
}

fn signed_record(index: u32) -> (PublicKey, FallbackRecord) {
	let mut seed = [77; 64];
	seed[..4].copy_from_slice(&index.to_le_bytes());
	let account = Xpriv::new_master(Network::Regtest, &seed).unwrap();
	let mailbox = account.to_keypair(&SECP);
	let key = bitcoin::PublicKey::new(mailbox.public_key()).wpubkey_hash().unwrap();
	let script = ScriptBuf::new_p2wpkh(&key);
	(mailbox.public_key(), FallbackRecord::new(Network::Regtest, script, &account, &mailbox).unwrap())
}

async fn set_record(rpc: &mut ArkClient, record: &(PublicKey, FallbackRecord)) -> f64 {
	let started = Instant::now();
	let encoded = record.1.serialize();
	let stored = rpc.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: record.0.serialize().to_vec(), record: encoded.clone(),
	}).await.unwrap().into_inner().record;
	assert_eq!(stored, encoded, "normal authenticated registration must retain its canonical binding");
	started.elapsed().as_secs_f64()
}

/// Board through the public app-wallet PSBT API using an already stored owned
/// key. This permits a later low-index board without editing keys,
/// source rows, cursors, signatures or mailbox records.
async fn board_key(ctx: &TestContext, wallet: &bark::Wallet, key: Keypair) -> Vtxo<Full> {
	let (address, expiry) = wallet.board_funding_address(&key).await.unwrap();
	let rate = wallet.chain().fee_rates().await.regular;
	let onchain = wallet.onchain().unwrap();
	let proposal = {
		let mut app = onchain.write().await;
		let psbt = app.prepare_tx(&[(address, sat(100_000))], rate).await.unwrap();
		app.sign_psbt(psbt).await.unwrap()
	};
	let board = wallet.board_psbt(proposal, key, expiry).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	wallet.chain().invalidate_caches().await;
	wallet.sync_pending_boards().await.unwrap();
	assert_eq!(board.vtxos.len(), 1);
	let source = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
	assert_eq!(source.policy().user_pubkey(), key.public_key());
	let funding = ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
	source.validate(&funding).expect("the fallback actor must own a fully signed board path");
	source
}

// Resolve public transaction ancestry only as far as the already declared funding cut.
// A concurrently replaced mempool transaction can disappear; it authorizes no mining.
fn declared_coin(
	core: &bitcoin_ext::rpc::BitcoinRpcClient, point: OutPoint,
	known: &mut BTreeMap<OutPoint, TxOut>, checked: &mut BTreeSet<bitcoin::Txid>,
) -> Option<TxOut> {
	if let Some(output) = known.get(&point) { return Some(output.clone()); }
	let info = core.custom_get_raw_transaction_info(point.txid, None).unwrap()?;
	let tx = deserialize::<Transaction>(&info.hex).unwrap();
	assert_eq!(tx.compute_txid(), point.txid);
	assert!(!tx.is_coinbase(), "a fee package cannot introduce another funding root");
	assert!(checked.insert(point.txid), "a funding ancestry cannot repeat a transaction");
	assert!(checked.len() <= 1024, "retain the declared traffic graph bound");
	let mut inputs = Amount::ZERO;
	for input in &tx.input {
		let Some(output) = declared_coin(core, input.previous_output, known, checked) else {
			checked.remove(&point.txid);
			return None;
		};
		inputs = inputs.checked_add(output.value).unwrap();
	}
	let outputs = tx.output.iter().map(|output| output.value).sum::<Amount>();
	assert!(inputs.checked_sub(outputs).is_some(), "a declared-cut package must conserve value");
	for (vout, output) in tx.output.iter().enumerate() {
		known.insert(OutPoint::new(point.txid, vout as u32), output.clone());
	}
	Some(known.get(&point).expect("the public funding output must exist").clone())
}

struct CohortProgress {
	prefixes: BTreeSet<bitcoin::Txid>,
	confirmed: BTreeSet<bitcoin::Txid>,
	known: BTreeMap<OutPoint, TxOut>,
	checked: BTreeSet<bitcoin::Txid>,
	first_height: u32,
}

impl CohortProgress {
	fn new(source: Option<&Vtxo<Full>>, traffic: &FundedTraffic, roots: &BTreeMap<OutPoint, TxOut>, first_height: u32) -> Self {
		let mut prefixes = BTreeSet::new();
		for coin in source.into_iter().chain(traffic.terminals.values().map(|(_, coin)| coin)) {
			for step in coin.transactions() {
				assert!(step.tx.input.iter().all(|input| input.sequence == bitcoin::Sequence::ZERO));
				prefixes.insert(step.tx.compute_txid());
			}
		}
		Self { prefixes, confirmed: BTreeSet::new(), known: roots.clone(), checked: BTreeSet::new(), first_height }
	}

	// Confirm one real group package, including only its declared-cut fee funding.
	// The existing transaction and total chain budgets also bound every later wait.
	async fn confirm_one(&mut self, ctx: &TestContext, srv: &Captaind, db: &Db) {
		let core = ctx.bitcoind().sync_client();
		let mut mempool = BTreeMap::new();
		for txid in core.get_raw_mempool().unwrap() {
			if let Some(info) = core.custom_get_raw_transaction_info(txid, None).unwrap() {
				let tx = deserialize::<Transaction>(&info.hex).unwrap();
				assert_eq!(tx.compute_txid(), txid);
				mempool.insert(txid, tx);
			}
		}
		let mut packages = Vec::new();
		for (txid, parent) in &mempool {
			if !self.prefixes.contains(txid) || self.confirmed.contains(txid) { continue; }
			let anchors = parent.output.iter().enumerate().filter(|(_, output)|
				output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
			assert_eq!(anchors.len(), 1);
			let anchor = OutPoint::new(*txid, anchors[0].0 as u32);
			for child in mempool.values().filter(|tx| tx.input.iter().any(|input| input.previous_output == anchor)) {
				packages.push((parent.clone(), child.clone()));
			}
		}
		// Only the journal's latest fee child may authorize confirming a payout.
		let published = db.read(async |tx| Ok(tx.query(
			"SELECT DISTINCT ON (a.id) a.sweep_tx,f.raw_tx FROM expiry_allocation a JOIN expiry_fee_child f ON f.allocation_id=a.id WHERE a.broadcast_at IS NOT NULL ORDER BY a.id,f.id DESC", &[],
		).await?)).await.unwrap();
		for row in published {
			let parent = deserialize::<Transaction>(&row.get::<_, Vec<u8>>(0)).unwrap();
			let child = deserialize::<Transaction>(&row.get::<_, Vec<u8>>(1)).unwrap();
			if mempool.get(&parent.compute_txid()) == Some(&parent)
				&& mempool.get(&child.compute_txid()) == Some(&child)
				&& !self.confirmed.contains(&parent.compute_txid()) { packages.push((parent, child)); }
		}
		for (parent, child) in packages {
			let anchors = parent.output.iter().enumerate().filter(|(_, output)|
				output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
			assert_eq!(anchors.len(), 1);
			let anchor = OutPoint::new(parent.compute_txid(), anchors[0].0 as u32);
			assert!(child.input.iter().any(|input| input.previous_output == anchor));
			if !parent.input.iter().chain(&child.input).all(|input|
				declared_coin(&core, input.previous_output, &mut self.known, &mut self.checked).is_some()) { continue; }
			let current = core.get_raw_mempool().unwrap();
			if !current.contains(&parent.compute_txid()) || !current.contains(&child.compute_txid()) { continue; }
			assert!(self.confirmed.insert(parent.compute_txid()));
			assert!(self.confirmed.len() <= 1024);
			let blocks = srv.watchmand().config().watchman.progress_grace_period.to_u32().checked_add(1).unwrap();
			let height = ctx.bitcoind().get_block_count().await as u32;
			assert!(height.checked_add(blocks).unwrap().checked_sub(self.first_height).unwrap() <= 10_000);
			let tip = ctx.generate_blocks(blocks).await;
			let hash = core.get_block_hash(u64::from(tip.to_u32())).unwrap();
			loop {
				if db.read(async |tx| {
					let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
					Ok(view.captaind.height == tip && view.watchmand.height == tip
						&& view.captaind.hash == hash && view.watchmand.hash == hash && tx.expiry_view_is_rebuilt(view).await?)
				}).await.unwrap() { break; }
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
			break; // Each observed parent can authorize only one bounded confirmation batch.
		}
	}
}

async fn payout(
	ctx: &TestContext, srv: &Captaind, db: &Db, rpc: &mut ArkClient,
	source: &Vtxo<Full>, expected_index: u32, owner_mailbox: PublicKey, destination: &FallbackRecord,
	pressure: &[(PublicKey, FallbackRecord)], candidate: Option<PublicKey>,
	progress: Option<(&FundedTraffic, &BTreeMap<OutPoint, TxOut>, u32)>,
) -> OutPoint {
	let height = ctx.bitcoind().get_block_count().await as u32;
	assert!(candidate.is_some() || height < source.expiry_height().to_u32() + 1);
	payout_ready(ctx, srv, db, rpc, source, expected_index, owner_mailbox, destination, pressure, candidate, true, progress).await
}

// New qualification cases enter here after observing a expired search.
// Their outer120s deadline includes the interruption/registration and this wait.
async fn payout_ready(
	ctx: &TestContext, srv: &Captaind, db: &Db, rpc: &mut ArkClient,
	source: &Vtxo<Full>, expected_index: u32, owner_mailbox: PublicKey, destination: &FallbackRecord,
	pressure: &[(PublicKey, FallbackRecord)], candidate: Option<PublicKey>, require_pressure: bool,
	progress: Option<(&FundedTraffic, &BTreeMap<OutPoint, TxOut>, u32)>,
) -> OutPoint {
	assert!(!require_pressure || !pressure.is_empty(), "ordinary pressure qualification needs registered pressure actors");
	let height = ctx.bitcoind().get_block_count().await as u32;
	let mature = source.expiry_height().to_u32() + 1;
	let expired = if height < mature { ctx.generate_blocks(mature-height).await }
		else { BlockHeight::new(height) };
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let key = source.policy().user_pubkey().to_string();
	let id = source.id().to_string();
	let expected = source.amount() - sat(1_000);
	let started = Instant::now();
	let mut latencies = Vec::new();
	let published = tokio::time::timeout(Duration::from_secs(120), async {
		let mut progress = progress.map(|(traffic, roots, first)| CohortProgress::new(Some(source), traffic, roots, first));
		loop {
			let (row, mailbox_rows) = db.read(async |tx| {
				let row = tx.query_opt("
					SELECT a.sweep_tx, f.raw_tx, f.fee_sat
					FROM expiry_allocation a
					JOIN expiry_allocation_coin c ON c.allocation_id = a.id
					JOIN expiry_entitlement e ON e.id = c.entitlement_id
					JOIN expiry_fee_child f ON f.allocation_id = a.id
					WHERE e.vtxo_id = $1
					AND c.active
					AND c.disposition = 'customer'
					AND a.broadcast_at IS NOT NULL
					ORDER BY f.id DESC
					LIMIT 1
				", &[&id]).await?;
				let mailbox_rows = tx.query("SELECT unblinded_mailbox_id FROM mailbox WHERE vtxo_id=$1 ORDER BY id", &[&id])
					.await?.into_iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>();
				Ok((row, mailbox_rows))
			}).await.unwrap();
			if let Some(candidate) = candidate {
				assert_eq!(mailbox_rows, vec![ark::mailbox::MailboxIdentifier::from_pubkey(candidate).to_string()],
					"the genuine wrong registered mailbox must remain the only hint");
			} else { assert!(mailbox_rows.is_empty(), "the live path must remain mailbox-less"); }
			if let Some(row) = row {
				break (deserialize::<Transaction>(row.get("sweep_tx")).unwrap(),
					deserialize::<Transaction>(row.get("raw_tx")).unwrap(), row.get::<_, i64>("fee_sat"));
			}
			assert!(latencies.len() < 2048, "declared normal registration pressure request bound exceeded");
			// New records append cursors while the readiness task requeues
			// this expired coin. Thereafter ordinary retries continue writer
			// pressure; no scanner/queue helper is invoked by this test.
			if !pressure.is_empty() {
				let record = &pressure[latencies.len()%pressure.len()];
				latencies.push(set_record(rpc, record).await);
			}
			if let Some(progress) = progress.as_mut() {
				// Preserve the ownership-search boundary before driving this target's publication.
				let ready = db.read(async |tx| Ok(tx.query_one(
					"SELECT EXISTS(SELECT 1 FROM expiry_allocation a JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.sweep_tx IS NOT NULL)",
					&[&id],
				).await?.get::<_, bool>(0))).await.unwrap();
				if ready { progress.confirm_one(ctx, srv, db).await; }
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await;
	println!("child_index={expected_index} lookup_publication_seconds={} registration_requests={}",
		started.elapsed().as_secs_f64(), latencies.len());
	let (parent, child, fee) = published.expect("live funded background attribution exceeded the declared 120-second leg budget");
	assert!(!require_pressure || candidate.is_some() || !latencies.is_empty(), "the funded search must overlap registration pressure");
	latencies.sort_by(f64::total_cmp);
	if !latencies.is_empty() {
		println!("registration_rpc_p50_seconds={} p95_seconds={} p99_seconds={}",
			latencies[latencies.len()/2], latencies[latencies.len()*95/100], latencies[latencies.len()*99/100]);
	}
	let attribution = db.read(async |tx| Ok(tx.query_one(
		"
			SELECT mailbox_pk, child_index
			FROM expiry_owner_key
			WHERE user_pubkey = $1
		", &[&key],
	).await?)).await.unwrap();
	assert_eq!(attribution.get::<_, String>("mailbox_pk"), owner_mailbox.to_string());
	assert_eq!(attribution.get::<_, i64>("child_index"), i64::from(expected_index));
	let txid = parent.compute_txid();
	assert_eq!(ctx.bitcoind().await_transaction(txid).await, parent);
	assert_eq!(ctx.bitcoind().await_transaction(child.compute_txid()).await, child);
	assert_eq!(parent.version.0, 3);
	assert!(parent.vsize() <= 10_000);
	assert_eq!(parent.input.len(), 1);
	assert_eq!(parent.input[0].previous_output, source.chain_anchor());
	let outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert_eq!(outputs[0].1.value, expected);
	let point = OutPoint::new(txid, outputs[0].0 as u32);
	let mut inputs = sat(0);
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		inputs += previous.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(inputs, parent.output.iter().map(|output| output.value).sum());
	let anchor = parent.output.iter().position(|output| output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT).unwrap();
	assert_eq!(child.input.iter().filter(|input| input.previous_output == OutPoint::new(txid, anchor as u32)).count(), 1);
	let mut child_inputs = sat(0);
	for input in &child.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		child_inputs += previous.output[input.previous_output.vout as usize].value;
	}
	let actual_fee = child_inputs - child.output.iter().map(|output| output.value).sum::<bitcoin::Amount>();
	assert!(actual_fee > sat(0));
	assert_eq!(actual_fee.to_sat(), u64::try_from(fee).unwrap());
	ctx.generate_blocks(6).await;
	let core = ctx.bitcoind().sync_client();
	// Core31's P2A type is not recognized by the older typed RPC decoder.
	let canonical: Value = core.call("getrawtransaction", &[json!(txid.to_string()), json!(true)]).unwrap();
	let child_canonical: Value = core.call("getrawtransaction", &[json!(child.compute_txid().to_string()), json!(true)]).unwrap();
	assert!(canonical["confirmations"].as_u64().unwrap() >= 6);
	assert!(child_canonical["confirmations"].as_u64().unwrap() >= 6);
	assert_eq!(canonical["blockhash"], child_canonical["blockhash"]);
	let utxo = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	assert!(utxo.confirmations >= 6);
	assert_eq!(utxo.value, expected);
	assert_eq!(utxo.script_pub_key.script().unwrap(), destination.spk);
	point
}

/// Follow every descendant of the declared initial funding outputs, including
/// ordinary change, operator income and fee sponsors. Known scripts are scanned
/// over their entire history, so an already spent duplicate cannot disappear.
async fn assert_owner_funding_cut(
	ctx: &TestContext, roots: &BTreeMap<OutPoint, TxOut>, start_height: u64,
	known_payments: &BTreeMap<ScriptBuf, BTreeMap<OutPoint, Amount>>,
) -> (Amount, BTreeMap<OutPoint, TxOut>) {
	assert!(!roots.is_empty());
	let core = ctx.bitcoind().sync_client();
	let mut cut = roots.clone();
	let mut fees = Amount::ZERO;
	let mut transactions = BTreeSet::new();
	let mut observed = known_payments.keys().cloned().map(|script| (script, BTreeMap::new()))
		.collect::<BTreeMap<_, _>>();
	for (point, output) in roots {
		assert_eq!(core.get_raw_transaction(&point.txid, None).unwrap().output[point.vout as usize], *output);
	}
	for height in start_height + 1..=core.get_block_count().unwrap() {
		let block = core.get_block(&core.get_block_hash(height).unwrap()).unwrap();
		for tx in block.txdata {
			let txid = tx.compute_txid();
			for (vout, output) in tx.output.iter().enumerate() {
				if let Some(outputs) = observed.get_mut(&output.script_pubkey) {
					assert!(outputs.insert(OutPoint::new(txid, vout as u32), output.value).is_none());
				}
			}
			if !tx.input.iter().any(|input| cut.contains_key(&input.previous_output)) { continue; }
			assert!(transactions.insert(txid));
			let points = tx.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>();
			assert_eq!(points.len(), tx.input.len());
			assert!(points.iter().all(|point| cut.contains_key(point)), "undeclared funding entered the complete cut");
			let mut inputs = Amount::ZERO;
			for point in points {
				let output = cut.remove(&point).unwrap();
				assert_eq!(core.get_raw_transaction(&point.txid, None).unwrap().output[point.vout as usize], output);
				inputs += output.value;
			}
			fees += inputs.checked_sub(tx.output.iter().map(|output| output.value).sum()).unwrap();
			for (vout, output) in tx.output.iter().cloned().enumerate() {
				assert!(cut.insert(OutPoint::new(txid, vout as u32), output).is_none());
			}
		}
	}
	assert_eq!(&observed, known_payments, "canonical known-script history must match the exact declared payouts");
	assert_eq!(roots.values().map(|output| output.value).sum::<Amount>(),
		cut.values().map(|output| output.value).sum::<Amount>() + fees);
	for (point, output) in &cut {
		if output.value == Amount::ZERO { continue; }
		let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, output.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), output.script_pubkey);
	}
	for txid in core.get_raw_mempool().unwrap() {
		let pending = core.get_raw_transaction(&txid, None).unwrap();
		assert!(!pending.input.iter().any(|input| cut.contains_key(&input.previous_output)), "a pending spend cannot hide a terminal");
	}
	(fees, cut)
}

struct FundedTraffic {
	wallets: Vec<bark::Wallet>,
	destinations: Vec<FallbackRecord>,
	seed: bip39::Mnemonic,
	reserved: Keypair,
	wrong: Option<Vtxo<Full>>,
	terminals: BTreeMap<VtxoId,(usize,Vtxo<Full>)>,
	customer_points: BTreeMap<OutPoint,TxOut>,
}

async fn bind_funding(ctx: &TestContext, wallet: &bark::Wallet, source: &Vtxo<Full>, amount: Amount, roots: &mut BTreeMap<OutPoint,TxOut>) {
	let tx=ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
	assert_eq!(tx.input.len(),1,"a newly funded actor board must use its one declared initial funding output");
	let point=tx.input[0].previous_output;
	let funding=ctx.bitcoind().await_transaction(point.txid).await;
	let output=funding.output[point.vout as usize].clone();
	assert_eq!(output.value,amount);
	assert!(wallet.onchain().unwrap().read().await.is_mine(&output.script_pubkey).await.unwrap());
	assert!(roots.insert(point,output).is_none());
}

async fn prepare_traffic(
	ctx: &TestContext,srv: &Captaind,proxy: &captaind::proxy::ArkRpcProxyServer,roots: &mut BTreeMap<OutPoint,TxOut>,
) -> FundedTraffic {
	let mut wallets=Vec::new();
	let mut destinations=Vec::new();
	let mut reserved=None;
	let mut customer_points=BTreeMap::new();
	for index in 0..16 {
		let amount=if index==0 { sat(500_000) } else { sat(125_000) };
		let wallet=create_wallet(ctx.bark_sdk(format!("traffic{index}"),proxy)
			.cfg(|cfg|cfg.daemon_manual_sync=true).funded(amount)).await;
		let (key,child)=wallet.derive_store_next_keypair().await.unwrap();
		assert_eq!(child,1);
		let source=board_key(ctx,&wallet,key).await;
		bind_funding(ctx,&wallet,&source,amount,roots).await;
		let board=ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
		for (vout,output) in board.output.iter().enumerate() {
			if wallet.onchain().unwrap().read().await.is_mine(&output.script_pubkey).await.unwrap() {
				customer_points.insert(OutPoint::new(board.compute_txid(),vout as u32),output.clone());
			}
		}
		if index==0 {
			for expected in 2..=17 {
				let (key,child)=wallet.derive_store_next_keypair().await.unwrap(); assert_eq!(child,expected);
				if expected==17 { reserved=Some(key); }
			}
		}
		destinations.push(wallet.fallback_destination().await.unwrap());
		wallets.push(wallet);
	}
	let text=tokio::fs::read_to_string(ctx.datadir.join("traffic0/mnemonic")).await.unwrap();
	let seed=bip39::Mnemonic::from_str(text.trim()).unwrap();
	assert_eq!(wallets.len(),16);
	assert_eq!(destinations.iter().map(|record|record.spk.clone()).collect::<BTreeSet<_>>().len(),16);
	assert!(srv.config().vtxopool.vtxo_targets.is_empty());
	FundedTraffic { wallets,destinations,seed,reserved:reserved.unwrap(),wrong:None,terminals:BTreeMap::new(),customer_points }
}

async fn actual_transfer(ctx: &TestContext,sender: &bark::Wallet,recipient: &bark::Wallet) -> (f64,Vec<VtxoId>) {
	let start=Instant::now();
	let before=sender.spendable_vtxos().await.unwrap();
	let old=recipient.spendable_vtxos().await.unwrap().into_iter().map(|coin|coin.id()).collect::<BTreeSet<_>>();
	let recipient_before=recipient.spendable_vtxos().await.unwrap().iter().map(|coin|coin.amount()).sum::<Amount>();
	let sender_before=before.iter().map(|coin|coin.amount()).sum::<Amount>();
	let address=recipient.new_address().await.unwrap();
	Box::pin(sender.send_arkoor_payment(&address,sat(5_000))).await.unwrap();
	assert!(sender.pending_arkoor_sends().await.unwrap().is_empty());
	recipient.sync_mailbox().await.unwrap();
	let received=recipient.spendable_vtxos().await.unwrap();
	let mut delivered=Amount::ZERO; let mut ids=Vec::new();
	for coin in received.iter().filter(|coin|!old.contains(&coin.id())) {
		let full=recipient.get_full_vtxo(coin.id()).await.unwrap();
		let funding=ctx.bitcoind().await_transaction(full.chain_anchor().txid).await;
		full.validate(&funding).unwrap();
		assert_eq!(recipient.get_vtxo_key(&full).await.unwrap().public_key(),full.policy().user_pubkey());
		assert_eq!(full.policy(),address.policy());
		assert_eq!(recipient.get_vtxo_by_id(full.id()).await.unwrap().state,bark::vtxo::VtxoState::Spendable);
		delivered+=full.amount(); ids.push(full.id());
	}
	assert_eq!(delivered,sat(5_000),"completion requires durable fully signed recipient coins, not an RPC acknowledgment");
	assert_eq!(received.iter().map(|coin|coin.amount()).sum::<Amount>(),recipient_before+sat(5_000));
	let after=sender.spendable_vtxos().await.unwrap();
	assert_eq!(after.iter().map(|coin|coin.amount()).sum::<Amount>()+sat(5_000),sender_before);
	let current=after.iter().map(|coin|coin.id()).collect::<BTreeSet<_>>();
	let spent=before.iter().filter(|coin|!current.contains(&coin.id())).collect::<Vec<_>>();
	assert!(!spent.is_empty());
	for coin in spent { assert_eq!(sender.get_vtxo_by_id(coin.id()).await.unwrap().state,bark::vtxo::VtxoState::Spent); }
	(start.elapsed().as_secs_f64(),ids)
}

async fn traffic_maintenance(ctx: &TestContext,srv: &Captaind,traffic: &FundedTraffic) -> bool {
	let mut needed=false;
	for wallet in &traffic.wallets { needed|=!wallet.get_vtxos_to_refresh().await.unwrap().is_empty(); }
	if !needed { return false; }
	let work=futures::future::join_all(traffic.wallets.iter().map(|wallet|Box::pin(wallet.maintenance_refresh())));
	let (results,())=tokio::join!(work,async { tokio::time::sleep(Duration::from_secs(2)).await; srv.trigger_round().await; });
	let mut refreshed=false;
	for result in results { if let Some(status)=result.unwrap() { assert!(status.is_success()); refreshed=true; } }
	assert!(refreshed); ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	for wallet in &traffic.wallets { wallet.chain().invalidate_caches().await; wallet.sync().await; }
	true
}

async fn run_traffic(ctx: &TestContext,srv: &Captaind,db: &Db,traffic: &mut FundedTraffic,search: &Vtxo<Full>) {
	let start=Instant::now();
	let mut latencies=Vec::new();
	let mut rounds=0; let mut credits=BTreeSet::new();
	let mut completed_during_lookup=0;
	let search_key=search.policy().user_pubkey().to_string();
	for batch in 0..60usize {
		let a=(batch*2)%16; let b=a+1;
		let start_height=ctx.bitcoind().get_block_count().await as u32;
		let pending_before=db.read(async |tx| Ok(tx.query_one(
			"SELECT EXISTS(SELECT 1 FROM expiry_pending_key WHERE user_pubkey=$1) AND NOT EXISTS(SELECT 1 FROM expiry_owner_key WHERE user_pubkey=$1)",
			&[&search_key]).await?.get::<_,bool>(0))).await.unwrap();
		let (left,right)=tokio::join!(Box::pin(actual_transfer(ctx,&traffic.wallets[a],&traffic.wallets[(a+8)%16])),
			Box::pin(actual_transfer(ctx,&traffic.wallets[b],&traffic.wallets[(b+8)%16])));
		for (latency,ids) in [left,right] {
			for id in ids { assert!(credits.insert(id),"a previously credited VTXO cannot count as a new payment"); }
			latencies.push(latency);
		}
		let unresolved=db.read(async |tx| Ok(tx.query_one(
			"SELECT EXISTS(SELECT 1 FROM expiry_pending_key WHERE user_pubkey=$1) AND NOT EXISTS(SELECT 1 FROM expiry_owner_key WHERE user_pubkey=$1)",
			&[&search_key]).await?.get::<_,bool>(0))).await.unwrap();
		if start_height>=search.expiry_height().to_u32()+1 && pending_before && unresolved { completed_during_lookup+=2; }
		if batch%8==7 && Box::pin(traffic_maintenance(ctx,srv,traffic)).await { rounds+=1; }
		let stats=db.read(async |tx| Ok(tx.query_one("SELECT (SELECT COUNT(*) FROM expiry_pending_key) AS pending, \
			(SELECT COUNT(*) FROM expiry_scan) AS scans, pg_database_size(current_database()) AS bytes",&[]).await?)).await.unwrap();
		println!("traffic_completed={} in_flight=0 concurrency_bound=2 elapsed_seconds={} pending_keys={} scan_rows={} database_bytes={}",
			latencies.len(),start.elapsed().as_secs_f64(),stats.get::<_,i64>("pending"),stats.get::<_,i64>("scans"),stats.get::<_,i64>("bytes"));
		tokio::time::sleep_until(tokio::time::Instant::from_std(start+Duration::from_secs((batch+1) as u64))).await;
	}
	assert_eq!(latencies.len(),120); assert!(start.elapsed()>=Duration::from_secs(60));
	assert!(completed_during_lookup>0,"no durable transfer completed while the expired funded search remained pending and unresolved");
	println!("durable_completed_during_unresolved_funded_lookup={completed_during_lookup}");
	latencies.sort_by(f64::total_cmp);
	println!("funded_wallets=18 registered=1000 completed_transfers=120 amount_sat=5000 traffic_seconds={} p50={} p95={} p99={} maintenance_rounds={rounds}",
		start.elapsed().as_secs_f64(),latencies[60],latencies[114],latencies[118]);
	let wrong=board_key(ctx,&traffic.wallets[0],traffic.reserved).await;
	let key=traffic.wallets[1].mailbox_keypair();
	let mailbox=traffic.wallets[1].mailbox_identifier();
	let auth=MailboxAuthorization::new(&key,chrono::Local::now()+Duration::from_secs(60));
	srv.get_mailbox_public_rpc().await.post_recovery_vtxo_ids(protos::mailbox_server::PostRecoveryVtxoIdsRequest {
		mailbox_id:mailbox.serialize(),vtxo_ids:vec![wrong.id().to_bytes().to_vec()],authorization:Some(auth.serialize()),
	}).await.unwrap();
	let hints=db.read(async |tx|Ok(tx.query("SELECT unblinded_mailbox_id FROM mailbox WHERE vtxo_id=$1 ORDER BY id",&[&wrong.id().to_string()]).await?)).await.unwrap();
	assert_eq!(hints.len(),1); assert_eq!(hints[0].get::<_,String>(0),mailbox.to_string());
	assert!(db.read(async |tx|Ok(tx.query_opt("SELECT user_pubkey FROM expiry_owner_key WHERE user_pubkey=$1",&[&traffic.reserved.public_key().to_string()]).await?)).await.unwrap().is_none());
	let board=ctx.bitcoind().await_transaction(wrong.chain_anchor().txid).await;
	for (vout,output) in board.output.iter().enumerate() {
		if traffic.wallets[0].onchain().unwrap().read().await.is_mine(&output.script_pubkey).await.unwrap() {
			traffic.customer_points.insert(OutPoint::new(board.compute_txid(),vout as u32),output.clone());
		}
	}
	traffic.wrong=Some(wrong);
	for (index,wallet) in traffic.wallets.iter().enumerate() {
		let mut total=Amount::ZERO;
		for coin in wallet.spendable_vtxos().await.unwrap() {
			let full=wallet.get_full_vtxo(coin.id()).await.unwrap();
			let funding=ctx.bitcoind().await_transaction(full.chain_anchor().txid).await;
			full.validate(&funding).unwrap();
			assert_eq!(wallet.get_vtxo_key(&full).await.unwrap().public_key(),full.policy().user_pubkey());
			total+=full.amount(); assert!(traffic.terminals.insert(full.id(),(index,full)).is_none());
		}
		let expected=(if index<8 {95_000} else {105_000})+if index==0 {100_000} else {0};
		assert_eq!(total,sat(expected));
	}
	assert_eq!(traffic.terminals.values().map(|(_,coin)|coin.amount()).sum::<Amount>(),sat(1_700_000));
}

/// The boundary is exactly the declared setup outputs, including unused
/// sponsor/rounds/customer change. No discovered input becomes a new root.
fn traffic_cut(ctx: &TestContext,first: u32,roots: &BTreeMap<OutPoint,TxOut>) -> (BTreeMap<bitcoin::Txid,Transaction>,BTreeMap<OutPoint,TxOut>,Amount) {
	let core=ctx.bitcoind().sync_client();
	let last=core.get_block_count().unwrap() as u32;
	assert!(last-first<=10_000,"finite two-expiry traffic chain budget exceeded");
	let mut known=roots.clone(); let mut terminal=roots.clone(); let mut graph=BTreeMap::new();
	let mut spent=BTreeSet::new(); let mut fees=Amount::ZERO;
	for height in first+1..=last {
		let block=core.get_block(&core.get_block_hash(u64::from(height)).unwrap()).unwrap();
		for tx in block.txdata {
			if !tx.input.iter().any(|input|known.contains_key(&input.previous_output)) { continue; }
			let mut value=Amount::ZERO;
			for input in &tx.input {
				let point=input.previous_output;
				assert!(spent.insert(point),"canonical funding graph cannot reuse an input");
				value+=known.get(&point).expect("a connected transaction introduced undeclared funding").value;
				assert!(terminal.remove(&point).is_some());
			}
			fees+=value.checked_sub(tx.output.iter().map(|out|out.value).sum()).unwrap();
			let txid=tx.compute_txid();
			for (vout,out) in tx.output.iter().enumerate() {
				let point=OutPoint::new(txid,vout as u32);
				assert!(known.insert(point,out.clone()).is_none()); terminal.insert(point,out.clone());
			}
			assert!(graph.insert(txid,tx).is_none()); assert!(graph.len()<=1024);
		}
	}
	let total=roots.values().map(|out|out.value).sum::<Amount>();
	assert_eq!(total,terminal.values().map(|out|out.value).sum::<Amount>()+fees);
	for (point,out) in &terminal {
		if out.value==Amount::ZERO { continue; }
		let actual=core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().unwrap();
		assert!(actual.confirmations>=6); assert_eq!(actual.value,out.value);
		assert_eq!(actual.script_pub_key.script().unwrap(),out.script_pubkey);
	}
	(graph,terminal,fees)
}

async fn finish_traffic(
	ctx: &TestContext,srv: &Captaind,db: &Db,traffic: &mut FundedTraffic,roots: &BTreeMap<OutPoint,TxOut>,
	first: u32,wrong: OutPoint,mut customer_points: BTreeMap<OutPoint,TxOut>,ordinary_payouts: BTreeMap<ScriptBuf,BTreeSet<OutPoint>>,
) {
	assert_eq!(roots.len(),22); assert_eq!(roots.values().map(|out|out.value).sum::<Amount>(),sat(33_375_000));
	let ids=traffic.terminals.keys().map(ToString::to_string).collect::<Vec<_>>();
	let rows=tokio::time::timeout(Duration::from_secs(120),async {
		let mut progress=CohortProgress::new(None,traffic,roots,first);
		loop {
			let rows=db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat, \
				c.spk,c.owner_mailbox,c.user_pubkey,a.sweep_tx FROM expiry_entitlement e \
				JOIN expiry_allocation_coin c ON c.entitlement_id=e.id JOIN expiry_allocation a ON a.id=c.allocation_id \
				WHERE e.vtxo_id=ANY($1) AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL",&[&ids]).await?)).await.unwrap();
			assert!(rows.len()<=ids.len()); if rows.len()==ids.len() { break rows; }
			progress.confirm_one(ctx,srv,db).await;
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("all funded traffic terminal rights must drain to actual payouts within120seconds");
	let mut paid=BTreeMap::<OutPoint,(usize,Amount)>::new(); let mut parents=BTreeMap::new();
	let mut row_fees=vec![Amount::ZERO;16]; let mut gross=vec![Amount::ZERO;16];
	let mut by_expiry=BTreeMap::<(usize,u32),(Amount,Amount)>::new();
	for row in &rows {
		let id=VtxoId::from_str(&row.get::<_,String>("vtxo_id")).unwrap();
		let (actor,coin)=&traffic.terminals[&id]; let destination=&traffic.destinations[*actor];
		assert_eq!(row.get::<_,i64>("gross_sat"),coin.amount().to_sat() as i64);
		assert_eq!(row.get::<_,i64>("forfeited_sat"),0);
		assert_eq!(row.get::<_,i64>("payout_sat")+row.get::<_,i64>("fee_sat"),coin.amount().to_sat() as i64);
		assert_eq!(row.get::<_,String>("spk"),destination.spk.as_bytes().to_lower_hex_string());
		assert_eq!(row.get::<_,String>("owner_mailbox"),traffic.wallets[*actor].mailbox_identifier().as_pubkey().to_string());
		assert_eq!(row.get::<_,String>("user_pubkey"),coin.policy().user_pubkey().to_string());
		let fee=sat(u64::try_from(row.get::<_,i64>("fee_sat")).unwrap());
		row_fees[*actor]+=fee; gross[*actor]+=coin.amount();
		let group=by_expiry.entry((*actor,coin.expiry_height().to_u32())).or_insert((Amount::ZERO,Amount::ZERO));
		group.0+=coin.amount(); group.1+=fee;
		let parent=deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
		let outputs=parent.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==destination.spk).collect::<Vec<_>>();
		assert_eq!(outputs.len(),1);
		let point=OutPoint::new(parent.compute_txid(),outputs[0].0 as u32); let value=outputs[0].1.value;
		if let Some(previous)=paid.insert(point,(*actor,value)) { assert_eq!(previous,(*actor,value)); }
		parents.insert(parent.compute_txid(),parent);
	}
	assert_eq!(paid[&wrong],(0,sat(99_000)));
	ctx.generate_blocks(6).await;
	let core=ctx.bitcoind().sync_client();
	for (point,(_,amount)) in &paid {
		let actual=core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().unwrap();
		assert!(actual.confirmations>=6); assert_eq!(actual.value,*amount);
	}
	for actor in 0..16 {
		let own=paid.values().filter(|(index,_)|*index==actor).collect::<Vec<_>>();
		let fees=by_expiry.iter().filter(|((index,_),_)|*index==actor).map(|(_, (source_gross,charged))| {
			let fee=sat(source_gross.to_sat().div_ceil(100).max(1_000));
			assert_eq!(*charged,fee,"charge each original source expiry height once");
			fee
		}).sum::<Amount>();
		assert_eq!(row_fees[actor],fees); assert_eq!(gross[actor],own.iter().map(|(_,value)|*value).sum::<Amount>()+fees);
		assert!(traffic.wallets[actor].pending_arkoor_sends().await.unwrap().is_empty());
		for (point,(index,amount)) in &paid {
			if *index==actor { customer_points.insert(*point,TxOut { value:*amount,script_pubkey:traffic.destinations[actor].spk.clone() }); }
		}
	}
	customer_points.extend(traffic.customer_points.iter().map(|(point,out)|(*point,out.clone())));
	let (graph,before,_)=traffic_cut(ctx,first,roots);
	let operator=srv.watchmand().config().sweep_address.clone().unwrap().require_network(Network::Regtest).unwrap().script_pubkey();
	let mut expected=ordinary_payouts;
	for actor in 0..16 {
		assert!(expected.insert(traffic.destinations[actor].spk.clone(),paid.iter().filter(|(_, (index,_))|*index==actor).map(|(point,_)|*point).collect()).is_none());
	}
	for (script,wanted) in expected {
		let actual=graph.values().flat_map(|tx|tx.output.iter().enumerate().filter_map(|(vout,out)|
			(out.script_pubkey==script).then_some(OutPoint::new(tx.compute_txid(),vout as u32)))).collect::<BTreeSet<_>>();
		assert_eq!(actual,wanted,"the complete declared funding graph must have no extra known customer payout");
	}
	// Retain original SDK ownership independently of the allocator's disposition.
	let mut sdk_coins=BTreeMap::new();
	for (actor,wallet) in traffic.wallets.iter().enumerate() {
		for coin in wallet.all_vtxos().await.unwrap() {
			assert!(sdk_coins.insert(coin.id(),(actor,coin)).is_none());
		}
	}
	let mut proved_parts=BTreeSet::new(); let mut operator_originals=BTreeSet::new();
	for parent in parents.values() {
		assert_eq!(graph.get(&parent.compute_txid()),Some(parent));
		let inputs=parent.input.iter().map(|input| {
			let previous=graph.get(&input.previous_output.txid).unwrap(); previous.output[input.previous_output.vout as usize].value
		}).sum::<Amount>();
		assert_eq!(inputs,parent.output.iter().map(|out|out.value).sum());
		assert_eq!(parent.version.0,3); assert!(parent.vsize()<=10_000);
		let anchor=parent.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT).collect::<Vec<_>>();
		assert_eq!(anchor.len(),1); assert_eq!(anchor[0].1.value,Amount::ZERO);
		let anchor=OutPoint::new(parent.compute_txid(),anchor[0].0 as u32);
		let child=graph.values().filter(|tx|tx.input.iter().any(|input|input.previous_output==anchor)).collect::<Vec<_>>();
		assert_eq!(child.len(),1); assert_eq!(child[0].version.0,3); assert!(child[0].vsize()<=1_000);
		let parent_info: Value=core.call("getrawtransaction",&[json!(parent.compute_txid().to_string()),json!(true)]).unwrap();
		let child_info: Value=core.call("getrawtransaction",&[json!(child[0].compute_txid().to_string()),json!(true)]).unwrap();
		assert_eq!(parent_info["blockhash"],child_info["blockhash"]); assert!(child_info["confirmations"].as_u64().unwrap()>=6);
		let child_inputs=child[0].input.iter().map(|input| {
			let point=input.previous_output;
			if let Some(output)=roots.get(&point) { output.value } else { graph[&point.txid].output[point.vout as usize].value }
		}).sum::<Amount>();
		assert!(child_inputs>child[0].output.iter().map(|out|out.value).sum::<Amount>());
		let payees=paid.keys().filter(|point|point.txid==parent.compute_txid()).count();
		let returned=parent.output.iter().filter(|out|out.script_pubkey==operator).map(|out|out.value).sum::<Amount>();
		// Refresh spends can return already forfeited principal as well as charges.
		// Neither a server-funded ancestor nor the observed remainder proves ownership.
		let parent_id=parent.compute_txid().to_string();
		let operator_rows=db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat \
			FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			JOIN expiry_allocation a ON a.id=c.allocation_id WHERE a.sweep_txid=$1 AND c.active AND c.disposition='operator'",
			&[&parent_id]).await?)).await.unwrap();
		let mut leaves=rows.iter().filter(|row| {
			deserialize::<Transaction>(row.get("sweep_tx")).unwrap().compute_txid()==parent.compute_txid()
		}).map(|row| ark::ServerVtxo::from(traffic.terminals[&VtxoId::from_str(row.get("vtxo_id")).unwrap()].1.clone())).collect::<Vec<_>>();
		let mut principal=Amount::ZERO;
		for row in operator_rows {
			let id=VtxoId::from_str(row.get("vtxo_id")).unwrap();
			let full=db.read(async |tx| tx.get_server_vtxo_by_id(id).await).await.unwrap().vtxo;
			let funding=&graph[&full.chain_anchor().txid];
			full.validate(funding).expect("operator principal requires a complete signed source path");
			let original_id=if full.clone().try_into_user_vtxo().is_ok() { id } else {
				assert!(matches!(full.policy(),ark::ServerVtxoPolicy::HarkForfeit(_)));
				let step=full.transactions().find(|step|step.tx.compute_txid()==full.point().txid).unwrap();
				assert_eq!(step.tx.input.len(),1); assert_eq!(full.point().vout,0);
				VtxoId::from(step.tx.input[0].previous_output)
			};
			assert!(operator_originals.insert(original_id),"one forfeited source cannot return operator principal twice");
			let (actor,old)=&sdk_coins[&original_id]; let wallet=&traffic.wallets[*actor];
			assert_eq!(old.state,bark::vtxo::VtxoState::Spent);
			let original=wallet.get_full_vtxo(original_id).await.unwrap();
			original.validate(&graph[&original.chain_anchor().txid]).unwrap();
			assert_eq!(wallet.get_vtxo_key(&original).await.unwrap().public_key(),original.user_pubkey());
			assert_eq!(full.amount(),original.amount());
			let native=db.read(async |tx| tx.get_user_vtxo_by_id(original_id).await).await.unwrap();
			assert_eq!(native.vtxo,original);
			let part=db.read(async |tx| Ok(tx.query_one("SELECT p.id,p.round_id,p.unlock_hash,i.signed_forfeit_tx \
				FROM round_part_input i JOIN round_participation p ON p.id=i.participation_id \
				JOIN round r ON r.funding_txid=p.round_id WHERE i.vtxo_id=$1 AND r.id=$2 \
				AND p.forfeited_at IS NOT NULL AND i.signed_forfeit_tx IS NOT NULL",
				&[&original_id.to_string(),&native.spent_in_round.unwrap()]).await?)).await.unwrap();
			let forfeit=deserialize::<Transaction>(part.get("signed_forfeit_tx")).unwrap();
			assert_eq!(forfeit.input.len(),1); assert_eq!(forfeit.input[0].witness.len(),1);
			let signature=bitcoin::secp256k1::schnorr::Signature::from_slice(forfeit.input[0].witness.iter().next().unwrap()).unwrap();
			let unlock_hash=ark::tree::signed::UnlockHash::from_str(part.get("unlock_hash")).unwrap();
			assert_eq!(forfeit,ark::forfeit::create_hark_forfeit_tx(&original,unlock_hash,Some(&signature)));
			let sighash=bitcoin::sighash::SighashCache::new(&forfeit).taproot_key_spend_signature_hash(0,
				&bitcoin::sighash::Prevouts::All(&[original.txout()]),bitcoin::TapSighashType::Default).unwrap();
			SECP.verify_schnorr(&signature,&sighash.into(),&original.output_taproot().output_key().to_x_only_public_key()).unwrap();
			if id==original_id { assert_eq!(full.clone().try_into_user_vtxo().unwrap(),original); } else {
				assert_eq!(full.point(),OutPoint::new(forfeit.compute_txid(),0));
				assert_eq!(full.transactions().find(|step|step.tx.compute_txid()==forfeit.compute_txid()).unwrap().tx,forfeit);
			}
			let round=bitcoin::Txid::from_str(part.get("round_id")).unwrap();
			if proved_parts.insert(part.get::<_,i64>("id")) {
				let canonical=core.custom_get_raw_transaction_info(round,None).unwrap().unwrap();
				assert!(canonical.confirmations.is_some_and(|depth|depth>=6));
				assert_eq!(deserialize::<Transaction>(&canonical.hex).unwrap(),graph[&round]);
				// The original owner's signed replacement coins must actually be delivered.
				let part_id=part.get::<_,i64>("id");
				let input_rows=db.read(async |tx| Ok(tx.query("SELECT vtxo_id FROM round_part_input WHERE participation_id=$1",&[&part_id]).await?)).await.unwrap();
				let mut input_ids=BTreeSet::new(); let mut input_amount=Amount::ZERO;
				for row in input_rows {
					let id=VtxoId::from_str(row.get("vtxo_id")).unwrap(); assert!(input_ids.insert(id));
					let (owner,coin)=&sdk_coins[&id];
					assert_eq!(owner,actor); assert_eq!(coin.state,bark::vtxo::VtxoState::Spent);
					let full=wallet.get_full_vtxo(id).await.unwrap();
					full.validate(&graph[&full.chain_anchor().txid]).unwrap();
					assert_eq!(wallet.get_vtxo_key(&full).await.unwrap().public_key(),full.user_pubkey());
					input_amount+=full.amount();
				}
				let outputs=db.read(async |tx| Ok(tx.query("SELECT policy,amount FROM round_part_output WHERE participation_id=$1",&[&part_id]).await?)).await.unwrap();
				assert!(!outputs.is_empty()); let mut delivered=BTreeSet::new(); let mut replacement=Amount::ZERO;
				for output in outputs {
					let amount=sat(u64::try_from(output.get::<_,i64>("amount")).unwrap());
					let candidates=sdk_coins.iter().filter(|(_, (owner,coin))|owner==actor && coin.chain_anchor().txid==round
						&& coin.amount()==amount && coin.policy().serialize()==output.get::<_,Vec<u8>>("policy")).collect::<Vec<_>>();
					assert_eq!(candidates.len(),1); let (replacement_id,_)=candidates[0];
					assert!(delivered.insert(*replacement_id));
					let coin=wallet.get_full_vtxo(*replacement_id).await.unwrap();
					coin.validate(&graph[&round]).unwrap();
					assert_eq!(wallet.get_vtxo_key(&coin).await.unwrap().public_key(),coin.user_pubkey());
					replacement+=coin.amount();
				}
				assert_eq!(replacement,input_amount,"a refresh must replace all forfeited customer value");
			}
			assert_eq!(row.get::<_,i64>("gross_sat"),full.amount().to_sat() as i64);
			for field in ["fee_sat","payout_sat","forfeited_sat"] { assert_eq!(row.get::<_,i64>(field),0); }
			principal+=full.amount(); leaves.push(full);
		}
		// Reuse the existing bounded complete-partition checker, with independently
		// authenticated leaves: no unrepresented branch may become operator capital.
		let mut backing=Vec::new(); let mut funding=BTreeMap::new(); let mut residuals=BTreeMap::new();
		for input in &parent.input {
			let root=db.read(async |tx| tx.get_bare_vtxo_by_id(input.previous_output.into()).await).await.unwrap().vtxo;
			assert_eq!(root.txout(),graph[&input.previous_output.txid].output[input.previous_output.vout as usize]);
			backing.push(root);
		}
		for leaf in &leaves {
			funding.insert(leaf.chain_anchor().txid,graph[&leaf.chain_anchor().txid].clone());
			let mut inside=false;
			for step in leaf.transactions() {
				inside|=backing.iter().any(|root|step.tx.input.iter().any(|input|input.previous_output==root.point()));
				if inside { for (vout,out) in step.tx.output.iter().enumerate() {
					if out.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT {
						residuals.insert(OutPoint::new(step.tx.compute_txid(),vout as u32),out.clone());
					}
				} }
			}
		}
		let residuals=residuals.into_iter().map(|(outpoint,txout)|BackingPartitionResidual { outpoint,txout }).collect::<Vec<_>>();
		BackingSnapshot { roots:backing,regular:Vec::new(),serialized_bytes:0 }.check_partition(&leaves,
			&funding.into_values().collect::<Vec<_>>(),&residuals,BackingSnapshotLimits { max_rows:16_384,max_serialized_bytes:16*1024*1024 }).unwrap();
		let charges=sat(1000*payees as u64);
		assert_eq!(returned,principal+charges);
		println!("traffic_parent={} authorized_refresh_principal_sat={} customer_charges_sat={}",parent_id,principal.to_sat(),charges.to_sat());
		assert!(parent.output.iter().all(|out|out.script_pubkey==operator||out.script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT
			||traffic.destinations.iter().any(|destination|out.script_pubkey==destination.spk)));
	}
	let restored=create_wallet(ctx.bark_sdk("traffic-seed-restored",srv).mnemonic(traffic.seed.clone())
		.cfg(|cfg|cfg.daemon_manual_sync=true)).await;
	assert_eq!(restored.fallback_destination().await.unwrap(),traffic.destinations[0]);
	restored.sync_onchain().await.unwrap(); let app=restored.onchain().unwrap();
	let mut owned=BTreeMap::new();
	for (point,out) in &before {
		if app.read().await.is_mine(&out.script_pubkey).await.unwrap() { owned.insert(*point,out.clone()); }
	}
	assert!(owned.contains_key(&wrong));
	assert_eq!(app.read().await.balance().await,owned.values().map(|out|out.value).sum::<Amount>());
	let destination=ctx.bitcoind().get_new_address();
	let drain={
		let mut app=app.write().await;
		let proposal=app.prepare_drain_tx(destination.clone(),bitcoin::FeeRate::from_sat_per_vb(2).unwrap()).await.unwrap();
		let signed=app.sign_psbt(proposal).await.unwrap(); app.finish_psbt(signed).await.unwrap().extract_tx().unwrap()
	};
	assert_eq!(drain.input.iter().map(|input|input.previous_output).collect::<BTreeSet<_>>(),owned.keys().copied().collect());
	assert_eq!(drain.input.len(),owned.len()); assert_eq!(drain.output.len(),1); assert_eq!(drain.output[0].script_pubkey,destination.script_pubkey());
	let drain_fee=owned.values().map(|out|out.value).sum::<Amount>()-drain.output[0].value; assert!(drain_fee>Amount::ZERO);
	restored.chain().broadcast_tx(&drain).await.unwrap(); ctx.generate_blocks(6).await;
	let (graph,terminal,fees)=traffic_cut(ctx,first,roots);
	assert_eq!(graph.get(&drain.compute_txid()),Some(&drain)); assert!(!terminal.contains_key(&wrong));
	let drain_point=OutPoint::new(drain.compute_txid(),0); assert_eq!(terminal[&drain_point],drain.output[0]);
	customer_points.insert(drain_point,drain.output[0].clone());
	let mut fee_funds=roots.iter().filter(|(_,out)|out.value==sat(100_000)).map(|(point,_)|*point).collect::<BTreeSet<_>>();
	assert_eq!(fee_funds.len(),3);
	let mut fee_outputs=fee_funds.iter().map(|point|(*point,roots[point].clone())).collect::<BTreeMap<_,_>>();
	let mut fee_spends=BTreeSet::new();
	let is_anchor=|point: OutPoint|graph.get(&point.txid).is_some_and(|tx|tx.output[point.vout as usize].script_pubkey==*bitcoin_ext::fee::P2A_SCRIPT);
	let mut children=graph.values().filter(|tx|tx.input.iter().any(|input|is_anchor(input.previous_output))).collect::<Vec<_>>();
	for _ in 0..graph.len() {
		children.retain(|child| {
			if !child.input.iter().all(|input|is_anchor(input.previous_output)||fee_funds.contains(&input.previous_output)) { return true; }
			assert!(child.input.iter().any(|input|fee_funds.contains(&input.previous_output)));
			fee_spends.extend(child.input.iter().filter(|input|fee_funds.contains(&input.previous_output)).map(|input|input.previous_output));
			for (vout,out) in child.output.iter().enumerate() {
				if out.script_pubkey!=*bitcoin_ext::fee::P2A_SCRIPT {
					let point=OutPoint::new(child.compute_txid(),vout as u32);
					fee_funds.insert(point); fee_outputs.insert(point,out.clone());
				}
			}
			false
		});
		if children.is_empty() { break; }
	}
	assert!(children.is_empty(),"all actual fee children must descend from the three declared watchman sponsors");
	let address=srv.watchmand().config().admin_address.unwrap();
	let mut rpc=WalletAdminClient::connect(format!("http://{address}")).await.unwrap();
	let mut operator_points=srv.wallet_status().await.rounds.confirmed_utxos.into_iter().collect::<BTreeSet<_>>();
	operator_points.extend(rpc.wallet_status(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap().confirmed_utxos
		.into_iter().map(|point|OutPoint::from_str(&point).unwrap()));
	let operator=srv.watchmand().config().sweep_address.clone().unwrap().require_network(Network::Regtest).unwrap().script_pubkey();
	// Sponsor capital cannot become an allowed customer change merely because its script is known.
	for (point,out) in fee_outputs {
		if out.value==Amount::ZERO { continue; }
		if let Some(current)=terminal.get(&point) {
			assert_eq!(current,&out);
			assert!(operator_points.contains(&point)||out.script_pubkey==operator,
				"terminal sponsor capital must remain in the actual operator wallet or configured drain");
		} else {
			assert!(fee_spends.contains(&point),"intermediate sponsor change may only be reused by an attested fee child");
		}
	}
	for (point,out) in &terminal {
		if out.value==Amount::ZERO { continue; }
		assert!(customer_points.get(point)==Some(out)||out.script_pubkey==operator||operator_points.contains(point),
			"every remaining declared-root satoshi must belong to a verified customer or operator destination");
	}
	let keys=traffic.terminals.values().map(|(_,coin)|coin.policy().user_pubkey().to_string()).collect::<Vec<_>>();
	let backlog=db.read(async |tx| Ok(tx.query_one("SELECT (SELECT COUNT(*) FROM expiry_pending_key WHERE user_pubkey=ANY($1)) AS cohort, \
		(SELECT COUNT(*) FROM expiry_pending_key) AS pending, (SELECT COUNT(*) FROM fallback_record) AS registered",&[&keys]).await?)).await.unwrap();
	assert_eq!(backlog.get::<_,i64>("cohort"),0); assert_eq!(backlog.get::<_,i64>("registered"),1000);
	println!("traffic_final funded_wallets=18 registered=1000 source_rights={} actual_payout_outputs={} terminal_outputs={} miner_fees_sat={} cohort_pending=0 global_pending={}",
		ids.len(),paid.len(),terminal.len(),fees.to_sat(),backlog.get::<_,i64>("pending"));
}

/// An expired, genuinely unregistered raw account becomes payable only after
/// its FIRST authenticated registration. This is native/raw admission, not SDK
/// wallet creation or SDK seed/history recovery (creation already registers).
#[tokio::test]
async fn expiry_owner_late_first_registration_pays_existing_expired_source_once() {
	let ctx = TestContext::new("server/expiry_owner_late_first_registration").await;
	let first_height = ctx.bitcoind().get_block_count().await;
	let operator_address = ctx.bitcoind().get_new_address();
	let operator_script = operator_address.script_pubkey();
	let srv = create_server(ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.expiry_owner_scan_chunk_size = 32;
		cfg.expiry_owner_scan_workers = 2;
	}).watchmand_cfg(move |cfg| cfg.sweep_address = Some(operator_address.into_unchecked()))).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let account = Xpriv::new_master(Network::Regtest, &[119; 64]).unwrap();
	let mailbox = account.to_keypair(&SECP);
	let owner_mailbox = mailbox.public_key();
	let destination = FallbackRecord::new(Network::Regtest,
		ScriptBuf::new_p2wpkh(&bitcoin::PublicKey::new(owner_mailbox).wpubkey_hash().unwrap()),
		&account, &mailbox).unwrap();
	destination.verify(Network::Regtest, owner_mailbox).unwrap();
	let child_number = bitcoin::bip32::ChildNumber::from_normal_idx(80).unwrap();
	let user = account.derive_priv(&SECP, &[child_number]).unwrap().to_keypair(&SECP);
	assert_eq!(destination.account_xpub.ckd_pub(&SECP, child_number).unwrap().public_key, user.public_key());
	assert_ne!(destination.spk, operator_script);
	let pressure = (2000..2008).map(signed_record).collect::<Vec<_>>();
	assert!(pressure.iter().all(|record| record.0 != owner_mailbox && record.1.spk != destination.spk && record.1.spk != operator_script));
	assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM fallback_record", &[]).await?.get::<_, i64>(0))).await.unwrap(), 0);

	// All capital is declared before the event:500k raw funder plus three100k
	// native watchman sponsors. No SDK creation or fallback registration occurs.
	let mut roots = BTreeMap::new();
	let fee_address = srv.watchmand().wait_wallet_address().await;
	for _ in 0..3 {
		let txid = ctx.bitcoind().fund_addr(&fee_address, sat(100_000)).await;
		let transaction = ctx.bitcoind().await_transaction(txid).await;
		let outputs = transaction.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == fee_address.script_pubkey()).collect::<Vec<_>>();
		assert_eq!(outputs.len(), 1);
		assert_eq!(outputs[0].1.value, sat(100_000));
		assert!(roots.insert(OutPoint::new(txid, outputs[0].0 as u32), outputs[0].1.clone()).is_none());
	}
	let funding_address = ctx.bitcoind().get_new_address();
	let funding_txid = ctx.bitcoind().fund_addr(&funding_address, sat(500_000)).await;
	let funding = ctx.bitcoind().await_transaction(funding_txid).await;
	let funding_outputs = funding.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == funding_address.script_pubkey()).collect::<Vec<_>>();
	assert_eq!(funding_outputs.len(), 1);
	let funding_point = OutPoint::new(funding_txid, funding_outputs[0].0 as u32);
	assert_eq!(funding_outputs[0].1.value, sat(500_000));
	assert_eq!(core.get_address_info(&funding_address).unwrap().is_mine, Some(true));
	assert!(roots.insert(funding_point, funding_outputs[0].1.clone()).is_none());
	assert_eq!(roots.values().map(|output| output.value).sum::<Amount>(), sat(800_000));
	let tip = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let info = srv.ark_info().await;
	let expiry = tip + info.vtxo_lifetime;
	assert_eq!(info.fees.board.calculate(sat(100_000)).unwrap(), Amount::ZERO);
	let builder = ark::board::BoardBuilder::new(user.public_key(), expiry, info.server_pubkey, info.vtxo_exit_delta);
	let change_address = ctx.bitcoind().get_new_address();
	let unsigned = Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: funding_point, script_sig: Default::default(),
			sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::new() }],
		output: vec![TxOut { value: sat(100_000), script_pubkey: builder.funding_script_pubkey() },
			TxOut { value: sat(399_000), script_pubkey: change_address.script_pubkey() }],
	};
	let signed = core.sign_raw_transaction_with_wallet(&unsigned, None, None).unwrap();
	assert!(signed.complete);
	let board_funding = signed.transaction().unwrap();
	assert_eq!(board_funding.input.len(), 1);
	assert_eq!(board_funding.input[0].previous_output, funding_point);
	assert_eq!(board_funding.output, unsigned.output);
	assert_eq!(sat(500_000) - board_funding.output.iter().map(|output| output.value).sum::<Amount>(), sat(1_000));
	let board_point = OutPoint::new(board_funding.compute_txid(), 0);
	let builder = builder.set_funding_details(sat(100_000), Amount::ZERO, board_point).unwrap().generate_user_nonces();
	let mut rpc = srv.get_public_rpc().await;
	let cosign: ark::board::BoardCosignResponse = rpc.request_board_cosign(protos::BoardCosignRequest {
		amount: 100_000, utxo: bitcoin::consensus::serialize(&board_point), expiry_height: expiry.into(),
		user_pubkey: user.public_key().serialize().to_vec(), pub_nonce: builder.user_pub_nonce().serialize().to_vec(),
		funding_tx: bitcoin::consensus::serialize(&board_funding),
	}).await.unwrap().into_inner().try_into().unwrap();
	assert!(builder.verify_cosign_response(&cosign));
	let source = builder.build_vtxo(&cosign, &user).unwrap();
	source.validate(&board_funding).unwrap();
	assert!(source.has_all_witnesses());
	assert_eq!(source.amount(), sat(100_000));
	assert_eq!(source.user_pubkey(), user.public_key());
	core.send_raw_transaction(&board_funding).unwrap();
	let tip = ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	rpc.register_board_vtxo(protos::BoardVtxoRequest { board_vtxo: source.serialize() }).await.unwrap();
	assert_eq!(db.read(async |tx| tx.get_user_vtxo_by_id(source.id()).await).await.unwrap().vtxo.user_pubkey(), user.public_key());
	let key = user.public_key().to_string();
	let source_id = source.id().to_string();
	let point = tokio::time::timeout(Duration::from_secs(120), Box::pin(async {
		let height = ctx.bitcoind().get_block_count().await as u32;
		assert!(height < source.expiry_height().to_u32() + 1);
		let tip = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - height).await;
		srv.wait_for_sync_height(tip).await;
		srv.watchmand().wait_for_sync_height(tip).await;
		loop {
			let state = db.read(async |tx| tx.expiry_owner(user.public_key(), Network::Regtest).await).await.unwrap();
			if state == server::database::expiry_attribution::Attribution::NoRegisteredOwner { break; }
			assert_eq!(state, server::database::expiry_attribution::Attribution::Unresolved);
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
		let negative = db.read(async |tx| Ok(tx.query_one("SELECT \
			EXISTS(SELECT 1 FROM expiry_pending_key WHERE user_pubkey=$1 AND mailbox_pk IS NULL) AS pending, \
			(SELECT COUNT(*) FROM fallback_record) AS registered, \
			(SELECT COUNT(*) FROM expiry_owner_key WHERE user_pubkey=$1) AS owners, \
			(SELECT COUNT(*) FROM mailbox WHERE vtxo_id=$2) AS hints, \
			(SELECT COUNT(*) FROM expiry_allocation_coin) AS allocations", &[&key, &source_id]).await?)).await.unwrap();
		assert!(negative.get::<_, bool>("pending"));
		for field in ["registered", "owners", "hints", "allocations"] { assert_eq!(negative.get::<_, i64>(field), 0); }
		tokio::time::sleep(Duration::from_millis(50)).await;
		assert_eq!(db.read(async |tx| tx.expiry_owner(user.public_key(), Network::Regtest).await).await.unwrap(),
			server::database::expiry_attribution::Attribution::NoRegisteredOwner);
		let held = core.get_tx_out(&board_point.txid, board_point.vout, Some(true)).unwrap().unwrap();
		assert!(held.confirmations >= 6);
		assert_eq!(held.value, sat(100_000));
		assert_eq!(held.script_pub_key.script().unwrap(), board_funding.output[0].script_pubkey);
		let absent = std::iter::once(destination.spk.clone()).chain(std::iter::once(operator_script.clone()))
			.chain(pressure.iter().map(|record| record.1.spk.clone())).map(|script| (script, BTreeMap::new())).collect::<BTreeMap<_, _>>();
		let (fees, _) = assert_owner_funding_cut(&ctx, &roots, first_height, &absent).await;
		assert_eq!(fees, sat(1_000), "before first registration only the real board funding fee may leave the declared cut");
		// FIRST registration of this mailbox/account; no SDK created or
		// registered it earlier, and no cursor/owner/decision row is written here.
		set_record(&mut rpc, &(owner_mailbox, destination.clone())).await;
		assert_eq!(db.read(async |tx| tx.get_fallback(owner_mailbox).await).await.unwrap().unwrap().serialize(), destination.serialize());
		let resumed = db.read(async |tx| tx.expiry_owner(user.public_key(), Network::Regtest).await).await.unwrap();
		assert_ne!(resumed, server::database::expiry_attribution::Attribution::NoRegisteredOwner,
			"new authenticated registration must invalidate the completed negative result");
		payout_ready(&ctx, &srv, &db, &mut rpc, &source, 80, owner_mailbox, &destination, &pressure, None, false, None).await
	})).await.expect("expired negative lookup, first registration and actual funded payout exceeded the original120-second leg");
	assert_eq!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap().value, sat(99_000));
	let parent = ctx.bitcoind().await_transaction(point.txid).await;
	let operator_outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == operator_script).collect::<Vec<_>>();
	assert_eq!(operator_outputs.len(), 1);
	assert_eq!(operator_outputs[0].1.value, sat(1_000));
	assert!(parent.output.iter().all(|output| output.script_pubkey == destination.spk || output.script_pubkey == operator_script || output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT));
	let owner = db.read(async |tx| Ok(tx.query_one("SELECT mailbox_pk,child_index FROM expiry_owner_key WHERE user_pubkey=$1", &[&key]).await?)).await.unwrap();
	assert_eq!(owner.get::<_, String>("mailbox_pk"), owner_mailbox.to_string());
	assert_eq!(owner.get::<_, i64>("child_index"), 80);
	assert!(db.read(async |tx| Ok(tx.query("SELECT id FROM mailbox WHERE vtxo_id=$1", &[&source_id]).await?)).await.unwrap().is_empty());
	let customer = db.read(async |tx| Ok(tx.query("SELECT c.spk,c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat,c.disposition \
		FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=$1 AND c.active", &[&source_id]).await?)).await.unwrap();
	assert_eq!(customer.len(), 1);
	assert_eq!(customer[0].get::<_, String>("spk"), destination.spk.as_bytes().to_lower_hex_string());
	assert_eq!(customer[0].get::<_, String>("disposition"), "customer");
	for (field, value) in [("gross_sat", 100_000), ("fee_sat", 1_000), ("payout_sat", 99_000), ("forfeited_sat", 0)] { assert_eq!(customer[0].get::<_, i64>(field), value); }
	// Raw-account recovery: its P2WPKH key signs the 99k payout.
	// No SDK mnemonic, offchain balance or SDK history assertion is invented.
	let recipient = ctx.bitcoind().get_new_address();
	let mut recovery = Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: point, script_sig: Default::default(), sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::new() }],
		output: vec![TxOut { value: sat(98_000), script_pubkey: recipient.script_pubkey() }],
	};
	let sighash = bitcoin::sighash::SighashCache::new(&recovery).p2wpkh_signature_hash(0, &destination.spk, sat(99_000), bitcoin::EcdsaSighashType::All).unwrap();
	let signature = bitcoin::ecdsa::Signature::sighash_all(SECP.sign_ecdsa(&sighash.into(), &mailbox.secret_key()));
	recovery.input[0].witness = bitcoin::Witness::p2wpkh(&signature, &owner_mailbox);
	core.send_raw_transaction(&recovery).unwrap();
	ctx.generate_blocks(6).await;
	assert_eq!(ctx.bitcoind().await_transaction(recovery.compute_txid()).await, recovery);
	let recovered_point = OutPoint::new(recovery.compute_txid(), 0);
	let recovered = core.get_tx_out(&recovered_point.txid, recovered_point.vout, Some(true)).unwrap().unwrap();
	assert!(recovered.confirmations >= 6);
	assert_eq!(recovered.value, sat(98_000));
	assert_eq!(recovered.script_pub_key.script().unwrap(), recipient.script_pubkey());
	assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
	let mut known = pressure.iter().map(|record| (record.1.spk.clone(), BTreeMap::new())).collect::<BTreeMap<_, _>>();
	assert!(known.insert(destination.spk.clone(), BTreeMap::from([(point, sat(99_000))])).is_none());
	assert!(known.insert(operator_script, BTreeMap::from([(OutPoint::new(parent.compute_txid(), operator_outputs[0].0 as u32), sat(1_000))])).is_none());
	assert!(known.insert(recipient.script_pubkey(), BTreeMap::from([(recovered_point, sat(98_000))])).is_none());
	let (fees, cut) = assert_owner_funding_cut(&ctx, &roots, first_height, &known).await;
	let core_points = BTreeMap::from([
		(OutPoint::new(board_funding.compute_txid(), 1), board_funding.output[1].clone()),
		(OutPoint::new(parent.compute_txid(), operator_outputs[0].0 as u32), operator_outputs[0].1.clone()),
		(recovered_point, recovery.output[0].clone()),
	]);
	for (point, output) in &core_points { assert_eq!(cut.get(point), Some(output)); }
	assert!(fees > sat(2_000), "derive board, real native fee child and raw-owner recovery miner fees from the full800k cut");
	let mut admin = WalletAdminClient::connect(format!("http://{}", srv.watchmand().config().admin_address.unwrap())).await.unwrap();
	let watchman = admin.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap().confirmed_utxos
		.into_iter().map(|point| point.parse::<OutPoint>().unwrap()).collect::<BTreeSet<_>>();
	for (point, output) in cut {
		if output.value == Amount::ZERO { continue; }
		if watchman.contains(&point) { continue; }
		assert_eq!(core_points.get(&point), Some(&output),
			"only exact declared funder change, operator fee and raw-owner recovery output may leave watchman capital");
		let address = bitcoin::Address::from_script(&output.script_pubkey, Network::Regtest).unwrap();
		assert_eq!(core.get_address_info(&address).unwrap().is_mine, Some(true));
	}
}

async fn funded_background(count: u32) {
	Box::pin(funded_background_mode(count, false)).await;
}

async fn funded_background_mode(count: u32, traffic_mode: bool) {
	let name = if traffic_mode { "server/expiry_owner_funded_traffic_18_of_1000".to_string() }
		else { format!("server/expiry_owner_live_funded_{count}") };
	let ctx = TestContext::new(&name).await;
	let first_height = ctx.bitcoind().get_block_count().await as u32;
	let operator = traffic_mode.then(|| ctx.bitcoind().get_new_address());
	let builder = ctx.captaind("server").no_vtxo_pool().cfg(move |cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(if traffic_mode { 4320 } else { 24 });
		cfg.expiry_owner_scan_chunk_size = 32;
		cfg.expiry_owner_scan_workers = 2;
		if traffic_mode {
			cfg.max_arkoor_fanout=4; cfg.max_vtxo_exit_depth=10; cfg.nb_round_nonces=3;
			cfg.vtxopool.max_vtxo_exit_depth=9; cfg.expiry_internal_refund_depth=6;
			cfg.round_interval=Duration::from_secs(86400);
		}
	}).watchmand_cfg(move |cfg| { if let Some(operator)=operator { cfg.sweep_address=Some(operator.into_unchecked()); } });
	let srv = create_server(if traffic_mode { builder.funded(sat(30_000_000)) } else { builder }).await;
	let mut traffic_roots = BTreeMap::new();
	if traffic_mode {
		let points=srv.wallet_status().await.rounds.confirmed_utxos;
		assert_eq!(points.len(),1);
		for point in points {
			let tx=ctx.bitcoind().await_transaction(point.txid).await;
			let output=tx.output[point.vout as usize].clone();
			assert_eq!(output.value,sat(30_000_000));
			traffic_roots.insert(point,output);
		}
	}
	let control = create_wallet(ctx.bark_sdk("control", &*srv).cfg(|cfg| cfg.daemon_manual_sync = true)
		.boarded(sat(100_000))).await;
	let control_coin = control.spendable_vtxos().await.unwrap().remove(0);
	let control_destination = control.fallback_destination().await.unwrap();
	let control_app = control.onchain().unwrap().clone();
	if traffic_mode {
		let full=control.get_full_vtxo(control_coin.id()).await.unwrap();
			bind_funding(&ctx,&control,&full,sat(200_000),&mut traffic_roots).await;
	}
	drop(control);
	let missing = RecoveryPostUnavailable::default();
	let proxy = srv.start_proxy_with_mailbox((), missing.clone()).await;
	let wallet = create_wallet(ctx.bark_sdk("owner", &proxy).cfg(|cfg| cfg.daemon_manual_sync = true)
		.funded(sat(500_000))).await;
	let seed_text = tokio::fs::read_to_string(ctx.datadir.join("owner/mnemonic")).await.unwrap();
	let seed = bip39::Mnemonic::from_str(seed_text.trim()).unwrap();
	let destination = wallet.fallback_destination().await.unwrap();
	let owner_mailbox = wallet.mailbox_identifier().as_pubkey();
	let mut rpc = srv.get_public_rpc().await;
	let mut traffic = None;
	let unfunded=count-(if traffic_mode { 18 } else { 2 });
	let mut traffic_pressure = Vec::new();
	for index in 0..unfunded {
		let record = signed_record(index);
		set_record(&mut rpc, &record).await;
		if traffic_mode && index < 24 { traffic_pressure.push(record); }
	}
	let pressure = if traffic_mode { traffic_pressure[..8].to_vec() }
		else { (count..count+8).map(signed_record).collect::<Vec<_>>() };
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	if !traffic_mode {
	assert_eq!(db.read(async |tx| Ok(tx.query_one("
		SELECT COUNT(*)
		FROM fallback_record
	", &[]).await?.get::<_, i64>(0))).await.unwrap(), i64::from(count));
	}
	let (low, index) = wallet.derive_store_next_keypair().await.unwrap();
	assert_eq!(index, 1);
	let mut high = low;
	for expected in 2..=80 {
		let (key, index) = wallet.derive_store_next_keypair().await.unwrap();
		assert_eq!(index, expected);
		high = key;
	}
	let old = board_key(&ctx, &wallet, high).await;
	if traffic_mode {
		bind_funding(&ctx,&wallet,&old,sat(500_000),&mut traffic_roots).await;
		traffic=Some(Box::pin(prepare_traffic(&ctx,&srv,&proxy,&mut traffic_roots)).await);
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM fallback_record",&[]).await?.get::<_,i64>(0))).await.unwrap(),1000);
	}
	assert_ne!(old.policy().user_pubkey(), low.public_key());
	// Independent confirmed funding per payout: usable fee inputs are
	// not inferred from an unconfirmed change output of the first fee child.
	let address = srv.watchmand().wait_wallet_address().await;
	for _ in 0..3 {
		let txid=ctx.bitcoind().fund_addr(address.clone(), sat(100_000)).await;
		if traffic_mode {
			let tx=ctx.bitcoind().await_transaction(txid).await;
			let outputs=tx.output.iter().enumerate().filter(|(_,out)|out.script_pubkey==address.script_pubkey()).collect::<Vec<_>>();
			assert_eq!(outputs.len(),1); assert_eq!(outputs[0].1.value,sat(100_000));
			assert!(traffic_roots.insert(OutPoint::new(txid,outputs[0].0 as u32),outputs[0].1.clone()).is_none());
		}
	}
	ctx.generate_blocks(2).await;
	let first = if let Some(traffic)=traffic.as_mut() {
		// Refresh the funded actors near the old search coin's expiry,
		// so that its lookup can overlap traffic without expiring the actors.
		let target=old.expiry_height().to_u32()-ROUND_CONFIRMATIONS-2;
		let tip=ctx.bitcoind().get_block_count().await as u32; assert!(tip<target);
		ctx.generate_blocks(target-tip).await;
		for wallet in &traffic.wallets { wallet.chain().invalidate_caches().await; wallet.sync().await; }
		assert!(Box::pin(traffic_maintenance(&ctx,&srv,traffic)).await);
		let ((),first)=tokio::join!(Box::pin(run_traffic(&ctx,&srv,&db,traffic,&old)),
			Box::pin(payout(&ctx,&srv,&db,&mut rpc,&old,80,owner_mailbox,&destination,&pressure,None,None)));
		first
	} else { payout(&ctx, &srv, &db, &mut rpc, &old, 80, owner_mailbox, &destination, &pressure, None, None).await };
	// The separately funded ordinary actor must not be stranded by this
	// account's deeper mailbox-less search. Its original wallet stays absent.
	let control_parent = db.read(async |tx| Ok(tx.query_one(
		"
			SELECT a.sweep_tx
			FROM expiry_allocation a
			JOIN expiry_allocation_coin c ON c.allocation_id = a.id
			JOIN expiry_entitlement e ON e.id = c.entitlement_id
			WHERE e.vtxo_id = $1
			AND c.active
			AND c.disposition = 'customer'
			AND a.broadcast_at IS NOT NULL
		", &[&control_coin.id().to_string()],
	).await?.get::<_, Vec<u8>>(0))).await.unwrap();
	let control_parent = deserialize::<Transaction>(&control_parent).unwrap();
	assert_eq!(ctx.bitcoind().await_transaction(control_parent.compute_txid()).await, control_parent);
	let control_outputs = control_parent.output.iter().enumerate().filter(|(_, output)|
		output.script_pubkey == control_destination.spk).collect::<Vec<_>>();
	assert_eq!(control_outputs.len(), 1);
	assert_eq!(control_outputs[0].1.value + sat(1_000), control_coin.amount());
	let control_point = OutPoint::new(control_parent.compute_txid(), control_outputs[0].0 as u32);
	assert!(ctx.bitcoind().sync_client().get_tx_out(&control_point.txid, control_point.vout, Some(true))
		.unwrap().unwrap().confirmations >= 6);
	// The first match proves the account was derived beyond child1.
	// A new board at that older unused key must receive fresh coverage.
	assert!(db.read(async |tx| Ok(tx.query_opt("
		SELECT child_index
		FROM expiry_owner_key
		WHERE user_pubkey = $1
	",
		&[&low.public_key().to_string()]).await?)).await.unwrap().is_none());
	wallet.sync_onchain().await.unwrap();
	let late = board_key(&ctx, &wallet, low).await;
	assert_eq!(late.policy().user_pubkey(), low.public_key());
	assert_ne!(old.id(), late.id());
	assert!(late.expiry_height()>old.expiry_height());
	assert!(missing.0.load(Ordering::SeqCst) >= 2);
	// This owner performs no more operations until both payouts
	// confirm. The daemon is manual throughout; background work is server-side.
	drop(wallet);
	let late_pressure = if traffic_mode { traffic_pressure[8..16].to_vec() }
		else { (count+8..count+16).map(signed_record).collect::<Vec<_>>() };
	let second = payout(&ctx, &srv, &db, &mut rpc, &late, 1, owner_mailbox, &destination, &late_pressure, None, traffic.as_ref().map(|traffic| (traffic, &traffic_roots, first_height))).await;
	let wrong_payout = if let Some(traffic)=traffic.as_ref() {
		let wrong=traffic.wrong.as_ref().unwrap();
		let pressure=traffic_pressure[16..24].to_vec();
		Some(payout(&ctx,&srv,&db,&mut rpc,wrong,17,
			traffic.wallets[0].mailbox_identifier().as_pubkey(), &traffic.destinations[0], &pressure,
			Some(traffic.wallets[1].mailbox_identifier().as_pubkey()), Some((traffic, &traffic_roots, first_height))).await)
	} else { None };
	let restored = create_wallet(ctx.bark_sdk("restored", &srv).mnemonic(seed)
		.cfg(|cfg| cfg.daemon_manual_sync = true)).await;
	assert_eq!(restored.fallback_destination().await.unwrap(), destination);
	restored.sync_onchain().await.unwrap();
	let app = restored.onchain().unwrap();
	// A normal late board may spend the first payout as well as app change.
	// Follow the exact four-transaction owner graph instead of requiring an
	// already spent historical payout to remain a fresh drain input.
	let old_board = ctx.bitcoind().await_transaction(old.chain_anchor().txid).await;
	let late_board = ctx.bitcoind().await_transaction(late.chain_anchor().txid).await;
	let first_parent = ctx.bitcoind().await_transaction(first.txid).await;
	let second_parent = ctx.bitcoind().await_transaction(second.txid).await;
	let graph = [old_board.clone(), first_parent.clone(), late_board.clone(), second_parent.clone()]
		.into_iter().map(|tx| (tx.compute_txid(), tx)).collect::<BTreeMap<_, _>>();
	assert_eq!(graph.len(), 4, "both boards and immutable payouts must remain distinct");
	let mut spent = BTreeSet::new();
	let mut original_inputs = BTreeSet::new();
	let mut graph_fees = sat(0);
	let mut board_fees = sat(0);
	for tx in graph.values() {
		let mut input_value = sat(0);
		for input in &tx.input {
			let point = input.previous_output;
			assert!(spent.insert(point), "the bounded owner graph cannot spend a principal twice");
			let previous = ctx.bitcoind().await_transaction(point.txid).await;
			input_value += previous.output[point.vout as usize].value;
			if !graph.contains_key(&point.txid) {
				original_inputs.insert(point);
			}
		}
		let output_value = tx.output.iter().map(|out| out.value).sum::<bitcoin::Amount>();
		let fee = input_value.checked_sub(output_value).expect("owner transaction must conserve value");
		if tx.compute_txid() == first.txid || tx.compute_txid() == second.txid {
			assert_eq!(fee, sat(0), "an immutable fallback parent must retain zero fee");
		} else {
			board_fees += fee;
		}
		graph_fees += fee;
	}
	assert_eq!(original_inputs, old_board.input.iter().map(|input| input.previous_output).collect(),
		"the late board may only reuse the original funding cut or its first payout");
	let mut original_value = sat(0);
	for point in &original_inputs {
		let previous = ctx.bitcoind().await_transaction(point.txid).await;
		original_value += previous.output[point.vout as usize].value;
	}
	assert_eq!(original_value, sat(500_000), "no additional owner funding is introduced");
	assert_eq!(graph_fees, board_fees);
	let mut operator_points = BTreeSet::new();
	for (point, parent) in [(first, &first_parent), (second, &second_parent)] {
		assert_eq!(parent.output[point.vout as usize].script_pubkey, destination.spk);
		assert_eq!(parent.output[point.vout as usize].value, sat(99_000));
		let mut suffix = sat(0);
		for (vout, output) in parent.output.iter().enumerate() {
			let output_point = OutPoint::new(parent.compute_txid(), vout as u32);
			if output_point == point {
				continue;
			}
			if output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT {
				assert_eq!(output.value, sat(0));
			} else {
				assert!(!app.read().await.is_mine(&output.script_pubkey).await.unwrap());
				suffix += output.value;
				operator_points.insert(output_point);
			}
		}
		assert_eq!(suffix, sat(1_000), "one exact operator fee per payout output");
	}
	let core = ctx.bitcoind().sync_client();
	let mut customer_cut = BTreeMap::new();
	for tx in graph.values() {
		for (vout, output) in tx.output.iter().enumerate() {
			let point = OutPoint::new(tx.compute_txid(), vout as u32);
			if spent.contains(&point) || operator_points.contains(&point) {
				continue;
			}
			if output.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT {
				assert_eq!(output.value, sat(0));
				continue;
			}
			assert!(app.read().await.is_mine(&output.script_pubkey).await.unwrap(),
				"every remaining customer output must belong to the freshly restored app seed");
			let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap()
				.expect("a current customer terminal cannot disappear outside the declared owner graph");
			assert!(actual.confirmations >= 6);
			assert_eq!(actual.value, output.value);
			assert_eq!(actual.script_pub_key.script().unwrap(), output.script_pubkey);
			assert!(customer_cut.insert(point, output.clone()).is_none());
		}
	}
	let reboarded_first = late_board.input.iter().any(|input| input.previous_output == first);
	assert_eq!(spent.contains(&first), reboarded_first,
		"only the late board may consume the first payout before seed recovery");
	assert_eq!(customer_cut.contains_key(&first), !reboarded_first);
	assert!(customer_cut.contains_key(&second));
	let current_customer = customer_cut.values().map(|out| out.value).sum::<bitcoin::Amount>();
	assert_eq!(original_value, current_customer + sat(2_000) + board_fees,
		"all 500k owner principal must remain in current customer outputs, two fees, or board miner fees");
	assert_eq!(app.read().await.balance().await, current_customer,
		"seed-only BDK balance must equal the complete current owner cut");
	let recipient = ctx.bitcoind().get_new_address();
	let recovery = {
		let mut app = app.write().await;
		let proposal = app.prepare_drain_tx(recipient.clone(), bitcoin::FeeRate::from_sat_per_vb(2).unwrap()).await.unwrap();
		let signed = app.sign_psbt(proposal).await.unwrap();
		app.finish_psbt(signed).await.unwrap().extract_tx().unwrap()
	};
	let recovered_points = recovery.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>();
	assert_eq!(recovered_points.len(), recovery.input.len(), "seed drain cannot repeat an input");
	assert_eq!(recovered_points, customer_cut.keys().copied().collect(),
		"fresh seed-only app must sign precisely every current customer input");
	let mut recovery_inputs = sat(0);
	for input in &recovery.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		recovery_inputs += previous.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(recovery_inputs, current_customer);
	assert_eq!(recovery.output.len(), 1);
	assert_eq!(recovery.output[0].script_pubkey, recipient.script_pubkey());
	let recovery_fee = recovery_inputs - recovery.output[0].value;
	assert!(recovery_fee > sat(0));
	restored.chain().broadcast_tx(&recovery).await.unwrap();
	ctx.generate_blocks(6).await;
	let output = ctx.bitcoind().sync_client().get_tx_out(&recovery.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(output.confirmations >= 6);
	assert_eq!(output.value, recovery_inputs - recovery_fee);
	assert_eq!(output.script_pub_key.script().unwrap(), recipient.script_pubkey());
	for point in customer_cut.keys() {
		assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
	}
	if let Some(traffic)=traffic.as_mut() {
		let mut customer_points=BTreeMap::from([(OutPoint::new(recovery.compute_txid(),0),recovery.output[0].clone()),
			(control_point,control_parent.output[control_point.vout as usize].clone())]);
		for tx in [&old_board,&late_board] {
			for (vout,output) in tx.output.iter().enumerate() {
				if app.read().await.is_mine(&output.script_pubkey).await.unwrap() {
					customer_points.insert(OutPoint::new(tx.compute_txid(),vout as u32),output.clone());
				}
			}
		}
		let control_board=ctx.bitcoind().await_transaction(control_coin.chain_anchor().txid).await;
		for (vout,output) in control_board.output.iter().enumerate() {
			if control_app.read().await.is_mine(&output.script_pubkey).await.unwrap() {
				customer_points.insert(OutPoint::new(control_board.compute_txid(),vout as u32),output.clone());
			}
		}
		Box::pin(finish_traffic(&ctx,&srv,&db,traffic,&traffic_roots,first_height,
			wrong_payout.unwrap(),customer_points,BTreeMap::from([
				(destination.spk.clone(),BTreeSet::from([first,second])),(control_destination.spk.clone(),BTreeSet::from([control_point]))]))).await;
	}
}

#[tokio::test]
#[ignore = "funded attribution under registration pressure"]
async fn expiry_owner_live_funded_100_registrations() {
	Box::pin(funded_background(100)).await;
}

#[tokio::test]
#[ignore = "funded attribution under registration pressure"]
async fn expiry_owner_live_funded_1000_registrations() {
	Box::pin(funded_background(1000)).await;
}

#[tokio::test]
#[ignore = "18 funded wallets/1000 registered, 120 paced transfers, serial finite600s qualification"]
async fn expiry_owner_funded_traffic_18_of_1000() {
	tokio::time::timeout(Duration::from_secs(600),Box::pin(funded_background_mode(1000,true))).await
		.expect("funded traffic plus attribution, payout and seed recovery exceeded600seconds");
}


// Same native-cluster reopen contract used by expiry_cold_backup. The standard
// PostgresHelper::prepare deletes and initializes its directory on every start.
struct OwnerSearchCluster(Arc<PostgresHelper>);

#[async_trait::async_trait]
impl DaemonHelper for OwnerSearchCluster {
	fn name(&self) -> &str { &self.0.name }
	fn datadir(&self) -> PathBuf { self.0.datadir.clone() }
	async fn make_reservations(&self) -> anyhow::Result<()> { self.0.make_reservations().await }
	async fn get_command(&self) -> anyhow::Result<tokio::process::Command> { self.0.get_command().await }
	async fn wait_for_init(&self) -> anyhow::Result<()> { self.0.wait_for_init().await }
	async fn prepare(&self) -> anyhow::Result<()> {
		assert!(self.0.pgdata().join("PG_VERSION").is_file());
		assert!(self.0.pgdata().join("global/pg_control").is_file());
		assert!(self.0.pgdata().join("pg_wal").is_dir());
		assert!(!self.0.pgdata().join("postmaster.pid").exists());
		Ok(())
	}
}

async fn unfinished_owner_search(
	db: &Db, source: &Vtxo<Full>, mailbox: PublicKey,
) -> (Vec<(String, i64, i64)>, Option<String>, Vec<u8>, Vec<(i64, Vec<u8>)>) {
	let id = source.id().to_string();
	let key = source.policy().user_pubkey().to_string();
	db.read(async |tx| {
		let pending = tx.query_one("SELECT mailbox_pk FROM expiry_pending_key WHERE user_pubkey=$1", &[&key]).await?;
		assert_eq!(pending.get::<_, Option<String>>(0), None, "the native search cannot have a mailbox hint");
		for query in [
			"SELECT COUNT(*) FROM mailbox WHERE vtxo_id=$1",
			"SELECT COUNT(*) FROM expiry_allocation_input WHERE vtxo_id=$1",
			"SELECT COUNT(*) FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=$1",
			"SELECT COUNT(*) FROM expiry_terminal_claim c JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=$1",
		] { assert_eq!(tx.query_one(query, &[&id]).await?.get::<_, i64>(0), 0); }
		assert!(tx.query_opt("SELECT child_index FROM expiry_owner_key WHERE user_pubkey=$1", &[&key]).await?.is_none());
		let scans = tx.query("SELECT mailbox_pk,scanned_up_to,service_ticket FROM expiry_scan WHERE user_pubkey=$1 ORDER BY mailbox_pk", &[&key]).await?
			.into_iter().map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1), row.get::<_, i64>(2))).collect::<Vec<_>>();
		assert_eq!(scans.len(), 100);
		let own = scans.iter().find(|row| row.0 == mailbox.to_string()).unwrap();
		assert!(own.1 > 0 && own.1 <= 256, "actual own-account coverage must be nonzero and unfinished");
		let evidence = tx.query_opt("SELECT irreversible_facts::TEXT FROM expiry_entitlement WHERE vtxo_id=$1", &[&id]).await?
			.map(|row| row.get::<_, String>(0));
		assert!(evidence.is_none(), "unresolved native readiness must not fabricate a decided entitlement");
		let signed = tx.query_one("SELECT vtxo FROM vtxo WHERE vtxo_id=$1", &[&id]).await?.get::<_, Vec<u8>>(0);
		let changesets = tx.query("SELECT id,content FROM wallet_changeset WHERE kind='watchman' ORDER BY id", &[]).await?
			.into_iter().map(|row| (row.get::<_, i64>(0), row.get::<_, Vec<u8>>(1))).collect::<Vec<_>>();
		assert!(!changesets.is_empty());
		Ok((scans, evidence, signed, changesets))
	}).await.unwrap()
}

#[tokio::test]
#[ignore = "funded mailbox-less native owner search across real product and PostgreSQL restart; original120s lookup"]
async fn expiry_owner_funded_missing_mailbox_search_survives_postgres_restart() {
	assert!(!ark_testing::postgres::externally_hosted(), "this fixture must restart its own actual PostgreSQL server");
	let ctx = TestContext::new("server/expiry_owner_funded_search_postgres_restart").await;
	let start_height = ctx.bitcoind().get_block_count().await;
	let operator = ctx.bitcoind().get_new_address();
	let operator_script = operator.script_pubkey();
	let srv = create_server(ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.expiry_owner_scan_chunk_size = 32;
		cfg.expiry_owner_scan_workers = 2;
	}).watchmand_cfg(move |cfg| cfg.sweep_address = Some(operator.into_unchecked()))).await;
	let server_key = srv.ark_info().await.server_pubkey;
	let missing = RecoveryPostUnavailable::default();
	let proxy = srv.start_proxy_with_mailbox((), missing.clone()).await;
	let wallet = create_wallet(ctx.bark_sdk("owner", &proxy).cfg(|cfg| cfg.daemon_manual_sync = true)
		.funded(sat(500_000))).await;
	let seed = bip39::Mnemonic::from_str(tokio::fs::read_to_string(ctx.datadir.join("owner/mnemonic")).await.unwrap().trim()).unwrap();
	let destination = wallet.fallback_destination().await.unwrap();
	let mailbox = wallet.mailbox_identifier().as_pubkey();
	let mut rpc = srv.get_public_rpc().await;
	let mut known = BTreeMap::from([(destination.spk.clone(), BTreeMap::new()), (operator_script.clone(), BTreeMap::new())]);
	assert_eq!(known.len(), 2);
	for index in 0..99 {
		let record = signed_record(index);
		assert!(known.insert(record.1.spk.clone(), BTreeMap::new()).is_none());
		set_record(&mut rpc, &record).await;
	}
	let pressure = (100..108).map(signed_record).collect::<Vec<_>>();
	for record in &pressure { assert!(known.insert(record.1.spk.clone(), BTreeMap::new()).is_none()); }
	let mut high = None;
	for expected in 1..=256 {
		let (key, index) = wallet.derive_store_next_keypair().await.unwrap();
		assert_eq!(index, expected);
		high = Some(key);
	}
	let source = board_key(&ctx, &wallet, high.unwrap()).await;
	assert_eq!(source.amount(), sat(100_000));
	assert!(missing.0.load(Ordering::SeqCst) > 0);
	let mut roots = BTreeMap::new();
	bind_funding(&ctx, &wallet, &source, sat(500_000), &mut roots).await;
	wallet.stop_daemon_wait().await.unwrap();
	drop(wallet);
	let address = srv.watchmand().wait_wallet_address().await;
	for _ in 0..3 {
		let txid = ctx.bitcoind().fund_addr(address.clone(), sat(100_000)).await;
		let tx = ctx.bitcoind().await_transaction(txid).await;
		let outputs = tx.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == address.script_pubkey()).collect::<Vec<_>>();
		assert_eq!(outputs.len(), 1);
		assert_eq!(outputs[0].1.value, sat(100_000));
		assert!(roots.insert(OutPoint::new(txid, outputs[0].0 as u32), outputs[0].1.clone()).is_none());
	}
	ctx.generate_blocks(2).await;
	assert_eq!(roots.values().map(|output| output.value).sum::<Amount>(), sat(800_000));
	let (point, db, postgres) = tokio::time::timeout(Duration::from_secs(120), async {
		let db = Db::connect(&srv.config().postgres).await.unwrap();
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM fallback_record", &[]).await?.get::<_, i64>(0))).await.unwrap(), 100);
		let height = ctx.bitcoind().get_block_count().await as u32;
		assert!(height < source.expiry_height().to_u32() + 1);
		let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - height).await;
		srv.wait_for_sync_height(expired).await;
		srv.watchmand().wait_for_sync_height(expired).await;
		let key = source.policy().user_pubkey().to_string();
		loop {
			let progress = db.read(async |tx| Ok(tx.query_opt("SELECT s.scanned_up_to FROM expiry_scan s WHERE s.user_pubkey=$1 AND s.mailbox_pk=$2 AND s.scanned_up_to>0 AND NOT EXISTS (SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=s.user_pubkey)", &[&key, &mailbox.to_string()]).await?)).await.unwrap();
			if progress.is_some() { break; }
			tokio::task::yield_now().await;
		}
		let logs = [srv.config().data_dir.join("stdout.log"), srv.watchmand().config().data_dir.join("stdout.log")];
		let offsets = logs.iter().map(|path| std::fs::metadata(path).unwrap().len() as usize).collect::<Vec<_>>();
		srv.stop().await.unwrap();
		srv.watchmand().stop().await.unwrap();
		for (path, offset) in logs.iter().zip(offsets) {
			let log = tokio::fs::read(path).await.unwrap();
			assert!(log[offset..].split(|byte| *byte == b'\n').any(|line|
				serde_json::from_slice::<Value>(line).is_ok_and(|record| record["slog_id"] == "ServerTerminated")),
				"actual daemon shutdown must finish before checking the durable unfinished boundary");
		}
		// This is the decisive boundary: a completed search during shutdown fails
		// here. No mutation, sleep or retry can manufacture an unfinished cursor.
		let checkpoint = unfinished_owner_search(&db, &source, mailbox).await;
		let own_prefix = checkpoint.0.iter().find(|row| row.0 == mailbox.to_string()).unwrap().1;
		let expected_registration = db.read(async |tx| tx.get_fallback(mailbox).await).await.unwrap().unwrap();
		assert_eq!(expected_registration.spk, destination.spk);
		let identity = db.read(async |tx| {
			let system = tx.query_one("SELECT system_identifier::TEXT FROM pg_control_system()", &[]).await?.get::<_, String>(0);
			let oid = tx.query_one("SELECT oid::BIGINT FROM pg_database WHERE datname=current_database()", &[]).await?.get::<_, i64>(0);
			let lsn = tx.query_one("SELECT pg_current_wal_lsn()::TEXT", &[]).await?.get::<_, String>(0);
			Ok((system, oid, lsn))
		}).await.unwrap();
		assert_eq!(assert_owner_funding_cut(&ctx, &roots, start_height, &known).await.0,
			// Only the board has spent declared capital at this point.
			{
				let board = ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
				let inputs = roots.iter().filter(|(point, _)| board.input.iter().any(|input| input.previous_output == **point)).map(|(_, output)| output.value).sum::<Amount>();
				inputs - board.output.iter().map(|output| output.value).sum::<Amount>()
			});
		let source_txout = ctx.bitcoind().sync_client().get_tx_out(&source.chain_anchor().txid, source.chain_anchor().vout, Some(true)).unwrap().unwrap();
		assert_eq!(source_txout.value, source.amount());
		let pg_config = srv.config().postgres.clone();
		drop(db);
		ctx.postgres_manager().stop().await.unwrap();
		let pgdata = ctx.datadir.join("postgres/pg_data");
		assert!(!pgdata.join("postmaster.pid").exists());
		let control = tokio::process::Command::new(std::env::var_os("POSTGRES_BINS").map(PathBuf::from)
			.map(|path| path.join("pg_controldata")).unwrap_or_else(|| PathBuf::from("pg_controldata")))
			.arg("-D").arg(&pgdata).env("LC_ALL", "C").kill_on_drop(true).output().await.unwrap();
		assert!(control.status.success());
		assert!(String::from_utf8(control.stdout).unwrap().lines().any(|line| line.split_once(':').is_some_and(|(key, value)| key.trim() == "Database cluster state" && value.trim() == "shut down")));
		let helper = Arc::new(PostgresHelper::new("owner-search-postgres", ctx.datadir.join("postgres")));
		let postgres = Daemon::wrap(OwnerSearchCluster(helper.clone()));
		Box::pin(postgres.start()).await.unwrap();
		let resumed_config = helper.into_config(&pg_config.name);
		assert_eq!(resumed_config.name, pg_config.name);
		assert_ne!(resumed_config.port, pg_config.port);
		let db = Db::connect(&resumed_config).await.unwrap();
		assert_eq!(unfinished_owner_search(&db, &source, mailbox).await, checkpoint, "the same physical database must retain every exact native cursor, ticket, source and wallet change");
		assert_eq!(db.read(async |tx| tx.get_fallback(mailbox).await).await.unwrap(), Some(expected_registration));
		let after = db.read(async |tx| {
			Ok((tx.query_one("SELECT system_identifier::TEXT FROM pg_control_system()", &[]).await?.get::<_, String>(0),
				tx.query_one("SELECT oid::BIGINT FROM pg_database WHERE datname=current_database()", &[]).await?.get::<_, i64>(0),
				tx.query_one("SELECT pg_current_wal_lsn()::TEXT", &[]).await?.get::<_, String>(0)))
		}).await.unwrap();
		assert_eq!((&after.0, after.1), (&identity.0, identity.1));
		let lsn_value = |lsn: &str| { let (high, low) = lsn.split_once('/').unwrap(); (u64::from_str_radix(high, 16).unwrap() << 32) | u64::from_str_radix(low, 16).unwrap() };
		assert!(lsn_value(&after.2) >= lsn_value(&identity.2));
		srv.config_mut().postgres = resumed_config.clone();
		srv.watchmand().config_mut().postgres = resumed_config;
		Box::pin(srv.start()).await.unwrap();
		Box::pin(srv.watchmand().start()).await.unwrap();
		assert_eq!(srv.ark_info().await.server_pubkey, server_key);
		loop {
			let progressed = db.read(async |tx| Ok(tx.query_opt("SELECT child_index FROM expiry_owner_key WHERE user_pubkey=$1", &[&key]).await?.map(|row| { assert_eq!(row.get::<_, i64>(0), 256); true }).unwrap_or(false)
				|| tx.query_opt("SELECT scanned_up_to FROM expiry_scan WHERE user_pubkey=$1 AND mailbox_pk=$2 AND scanned_up_to>$3", &[&key, &mailbox.to_string(), &own_prefix]).await?.is_some())).await.unwrap();
			if progressed { break; }
			tokio::task::yield_now().await;
		}
		let mut rpc = srv.get_public_rpc().await;
		let point = payout_ready(&ctx, &srv, &db, &mut rpc, &source, 256, mailbox, &destination, &pressure, None, false, None).await;
		(point, db, postgres)
	}).await.expect("native unfinished search, actual daemon/PostgreSQL restart and publication exceeded original120s lookup budget");
	let parent = ctx.bitcoind().await_transaction(point.txid).await;
	assert_eq!(parent.output[point.vout as usize].value, sat(99_000));
	let rights = db.read(async |tx| Ok(tx.query("SELECT e.gross_sat AS entitlement_gross,c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat,c.owner_mailbox,c.user_pubkey,c.spk,a.sweep_txid FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer'", &[&source.id().to_string()]).await?)).await.unwrap();
	assert_eq!(rights.len(), 1);
	let right = &rights[0];
	assert_eq!(right.get::<_, i64>("entitlement_gross"), 100_000);
	assert_eq!(right.get::<_, i64>("gross_sat"), 100_000);
	assert_eq!(right.get::<_, i64>("fee_sat"), 1_000);
	assert_eq!(right.get::<_, i64>("payout_sat"), 99_000);
	assert_eq!(right.get::<_, i64>("forfeited_sat"), 0);
	assert_eq!(right.get::<_, String>("owner_mailbox"), mailbox.to_string());
	assert_eq!(right.get::<_, String>("user_pubkey"), source.policy().user_pubkey().to_string());
	assert_eq!(right.get::<_, String>("spk"), destination.spk.as_bytes().to_lower_hex_string());
	assert_eq!(right.get::<_, String>("sweep_txid"), point.txid.to_string());
	known.insert(destination.spk.clone(), BTreeMap::from([(point, sat(99_000))]));
	let operator_outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == operator_script)
		.map(|(index, output)| (OutPoint::new(point.txid, index as u32), output.value)).collect::<BTreeMap<_, _>>();
	assert_eq!(operator_outputs.values().copied().sum::<Amount>(), sat(1_000));
	known.insert(operator_script, operator_outputs.clone());
	let recovery_proxy = srv.start_proxy_with_mailbox((), missing.clone()).await;
	let restored = create_wallet(ctx.bark_sdk("restored", &recovery_proxy).mnemonic(seed).cfg(|cfg| cfg.daemon_manual_sync = true)).await;
	assert_eq!(restored.fallback_destination().await.unwrap(), destination);
	restored.sync_onchain().await.unwrap();
	let app = restored.onchain().unwrap();
	let board = ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
	let board_fee = sat(500_000) - board.output.iter().map(|output| output.value).sum::<Amount>();
	let customer = sat(500_000) - board_fee - sat(1_000);
	assert_eq!(app.read().await.balance().await, customer);
	let recipient = ctx.bitcoind().get_new_address();
	let recovery = {
		let mut app = app.write().await;
		let proposal = app.prepare_drain_tx(recipient.clone(), bitcoin::FeeRate::from_sat_per_vb(2).unwrap()).await.unwrap();
		let signed = app.sign_psbt(proposal).await.unwrap();
		app.finish_psbt(signed).await.unwrap().extract_tx().unwrap()
	};
	let mut expected_inputs = BTreeMap::from([(point, sat(99_000))]);
	for (index, output) in board.output.iter().enumerate() {
		let candidate = OutPoint::new(board.compute_txid(), index as u32);
		if candidate == source.chain_anchor() || output.value == Amount::ZERO { continue; }
		assert!(app.read().await.is_mine(&output.script_pubkey).await.unwrap(), "every non-anchor board output must be genuine original-owner change");
		assert!(expected_inputs.insert(candidate, output.value).is_none());
	}
	assert_eq!(recovery.input.len(), expected_inputs.len());
	assert_eq!(recovery.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>(), expected_inputs.keys().copied().collect());
	assert_eq!(expected_inputs.values().copied().sum::<Amount>(), customer);
	assert_eq!(recovery.output.len(), 1);
	assert_eq!(recovery.output[0].script_pubkey, recipient.script_pubkey());
	let recovery_fee = customer.checked_sub(recovery.output[0].value).unwrap();
	assert!(recovery_fee > Amount::ZERO);
	restored.chain().broadcast_tx(&recovery).await.unwrap();
	ctx.generate_blocks(6).await;
	let paid = ctx.bitcoind().sync_client().get_tx_out(&recovery.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(paid.confirmations >= 6);
	assert_eq!(paid.value + recovery_fee + board_fee + sat(1_000), sat(500_000));
	assert_eq!(paid.script_pub_key.script().unwrap(), recipient.script_pubkey());
	for point in expected_inputs.keys() { assert!(ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none()); }
	assert!(db.read(async |tx| Ok(tx.query("SELECT unblinded_mailbox_id FROM mailbox WHERE vtxo_id=$1", &[&source.id().to_string()]).await?)).await.unwrap().is_empty());
	let (fees, cut) = assert_owner_funding_cut(&ctx, &roots, start_height, &known).await;
	assert!(fees >= board_fee + recovery_fee);
	let mut admin = WalletAdminClient::connect(format!("http://{}", srv.watchmand().config().admin_address.unwrap())).await.unwrap();
	let watchman_points = admin.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap()
		.confirmed_utxos.into_iter().map(|point| OutPoint::from_str(&point).unwrap()).collect::<BTreeSet<_>>();
	let allowed_core_points = operator_outputs.keys().copied().chain([OutPoint::new(recovery.compute_txid(), 0)]).collect::<BTreeSet<_>>();
	for (point, output) in &cut {
		if output.value == Amount::ZERO { continue; }
		if watchman_points.contains(point) { continue; }
		assert!(allowed_core_points.contains(point), "no declared sponsor capital may divert to another Core-owned destination");
		let address = bitcoin::Address::from_script(&output.script_pubkey, Network::Regtest).unwrap();
		assert_eq!(ctx.bitcoind().sync_client().get_address_info(&address).unwrap().is_mine, Some(true),
			"every terminal must be actual watchman-owned fee capital or the declared Core-owned operator/seed-drain destination");
	}
	println!("owner_search_postgres_restart source={} child=256 payout={} gross_sat=100000 payout_sat=99000 root_sat=800000 actual_total_miner_fees_sat={} seed_recovery={} seed_paid_sat={} seed_miner_fee_sat={}", source.id(), point, fees.to_sat(), recovery.compute_txid(), paid.value.to_sat(), recovery_fee.to_sat());
	drop(restored);
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	drop(db);
	postgres.stop().await.unwrap();
}
