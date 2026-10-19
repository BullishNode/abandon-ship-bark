//! Funded attribution under registration pressure, with the scanner and readiness tasks running.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ark::fallback::FallbackRecord;
use ark::vtxo::Full;
use ark::{ProtocolEncoding, Vtxo, SECP};
use ark_testing::{sat, Captaind, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::context::builders::{BarkSdkBuilder, CaptaindBuilder};
use ark_testing::daemon::captaind::{self, ArkClient, MailboxClient};
use bitcoin::bip32::Xpriv;
use bitcoin::consensus::deserialize;
use bitcoin::secp256k1::{Keypair, PublicKey};
use bitcoin::{Network, OutPoint, ScriptBuf, Transaction};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use serde_json::{json, Value};
use server::database::Db;
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

async fn payout(
	ctx: &TestContext, srv: &Captaind, db: &Db, rpc: &mut ArkClient,
	source: &Vtxo<Full>, expected_index: u32, owner_mailbox: PublicKey, destination: &FallbackRecord,
	pressure: &[(PublicKey, FallbackRecord)],
) -> OutPoint {
	let height = ctx.bitcoind().get_block_count().await as u32;
	let mature = source.expiry_height().to_u32() + 1;
	assert!(height < mature);
	let expired = ctx.generate_blocks(mature - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let key = source.policy().user_pubkey().to_string();
	let id = source.id().to_string();
	let expected = source.amount() - sat(1_000);
	let started = Instant::now();
	let mut latencies = Vec::new();
	let published = tokio::time::timeout(Duration::from_secs(120), async {
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
				let mailbox_rows = tx.query_one("
					SELECT COUNT(*)
					FROM mailbox
					WHERE vtxo_id = $1
				", &[&id])
					.await?.get::<_, i64>(0);
				Ok((row, mailbox_rows))
			}).await.unwrap();
			assert_eq!(mailbox_rows, 0, "the live path must remain mailbox-less");
			if let Some(row) = row {
				break (deserialize::<Transaction>(row.get("sweep_tx")).unwrap(),
					deserialize::<Transaction>(row.get("raw_tx")).unwrap(), row.get::<_, i64>("fee_sat"));
			}
			assert!(latencies.len() < 2048, "declared normal registration pressure request bound exceeded");
			// New records append cursors while the readiness task requeues
			// this expired coin. Thereafter ordinary retries continue writer
			// pressure; no scanner/queue helper is invoked by this test.
			let record = &pressure[latencies.len()%pressure.len()];
			latencies.push(set_record(rpc, record).await);
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await;
	println!("child_index={expected_index} lookup_publication_seconds={} registration_requests={}",
		started.elapsed().as_secs_f64(), latencies.len());
	let (parent, child, fee) = published.expect("live funded background attribution exceeded the declared 120-second leg budget");
	assert!(!latencies.is_empty(), "the funded search must overlap registration pressure");
	latencies.sort_by(f64::total_cmp);
	println!("registration_rpc_p50_seconds={} p95_seconds={} p99_seconds={}",
		latencies[latencies.len()/2], latencies[latencies.len()*95/100], latencies[latencies.len()*99/100]);
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

async fn funded_background(count: u32) {
	let ctx = TestContext::new(&format!("server/expiry_owner_live_funded_{count}")).await;
	let srv = create_server(ctx.captaind("server").no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(24);
		cfg.expiry_owner_scan_chunk_size = 32;
		cfg.expiry_owner_scan_workers = 2;
	}).watchmand()).await;
	let control = create_wallet(ctx.bark_sdk("control", &*srv).cfg(|cfg| cfg.daemon_manual_sync = true)
		.boarded(sat(100_000))).await;
	let control_coin = control.spendable_vtxos().await.unwrap().remove(0);
	let control_destination = control.fallback_destination().await.unwrap();
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
	for index in 0..count - 2 {
		set_record(&mut rpc, &signed_record(index)).await;
	}
	let pressure = (count..count + 8).map(signed_record).collect::<Vec<_>>();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(db.read(async |tx| Ok(tx.query_one("
		SELECT COUNT(*)
		FROM fallback_record
	", &[]).await?.get::<_, i64>(0))).await.unwrap(), i64::from(count));
	let (low, index) = wallet.derive_store_next_keypair().await.unwrap();
	assert_eq!(index, 1);
	let mut high = low;
	for expected in 2..=80 {
		let (key, index) = wallet.derive_store_next_keypair().await.unwrap();
		assert_eq!(index, expected);
		high = key;
	}
	let old = board_key(&ctx, &wallet, high).await;
	assert_ne!(old.policy().user_pubkey(), low.public_key());
	// Independent confirmed funding per payout: usable fee inputs are
	// not inferred from an unconfirmed change output of the first fee child.
	let address = srv.watchmand().wait_wallet_address().await;
	ctx.bitcoind().fund_addr(address.clone(), sat(100_000)).await;
	ctx.bitcoind().fund_addr(address.clone(), sat(100_000)).await;
	ctx.bitcoind().fund_addr(address, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	let first = payout(&ctx, &srv, &db, &mut rpc, &old, 80, owner_mailbox, &destination, &pressure).await;
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
	let late_pressure = (count + 8..count + 16).map(signed_record).collect::<Vec<_>>();
	let second = payout(&ctx, &srv, &db, &mut rpc, &late, 1, owner_mailbox, &destination, &late_pressure).await;
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
