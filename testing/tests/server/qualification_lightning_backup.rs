//! One orderly physical backup at genuine internal HtlcsReady, followed by
//! automatic sender refund and fresh-seed BIP84 recovery. Core and CLN continue.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::PaymentHash;
use ark::vtxo::{Full, TapScriptClause};
use ark::{ProtocolEncoding, Vtxo, VtxoId, VtxoPolicy};
use async_trait::async_trait;
use bitcoin::{Amount, Network, OutPoint, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::hex::DisplayHex;
use bitcoin_ext::{BlockDelta, BlockHeight, BlockRef, TxOutExt};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use tokio::process::Command;

use ark_testing::{require_bark_version, require_bitcoind_chain_source, sat, Captaind, TestContext};
use ark_testing::daemon::{Daemon, DaemonHelper};
use ark_testing::daemon::captaind::WalletAdminClient;
use ark_testing::daemon::postgres::PostgresHelper;
use ark_testing::daemon::watchmand::Watchmand;
use server::database::{Db, SpendState};
use server_rpc::protos;

/// The standard helper's prepare deletes the cluster and runs initdb. This
/// wrapper delegates only reservations, the real postgres command and readiness.
struct ExistingCluster(Arc<PostgresHelper>);

#[async_trait]
impl DaemonHelper for ExistingCluster {
	fn name(&self) -> &str { &self.0.name }
	fn datadir(&self) -> PathBuf { self.0.datadir.clone() }
	async fn make_reservations(&self) -> anyhow::Result<()> { self.0.make_reservations().await }
	async fn get_command(&self) -> anyhow::Result<Command> { self.0.get_command().await }
	async fn wait_for_init(&self) -> anyhow::Result<()> { self.0.wait_for_init().await }
	async fn prepare(&self) -> anyhow::Result<()> {
		assert!(self.0.pgdata().join("PG_VERSION").is_file());
		assert!(self.0.pgdata().join("global/pg_control").is_file());
		assert!(self.0.pgdata().join("pg_wal").is_dir());
		assert!(!self.0.pgdata().join("postmaster.pid").exists());
		Ok(())
	}
}

fn file_hash(path: &Path) -> String {
	let mut file = File::open(path).unwrap();
	let mut engine = sha256::Hash::engine();
	let mut buffer = [0; 65_536];
	loop {
		let length = file.read(&mut buffer).unwrap();
		if length == 0 { break; }
		engine.input(&buffer[..length]);
	}
	sha256::Hash::from_engine(engine).to_string()
}

/// Include the entire cluster, especially pg_wal. Reject links/tablespaces and
/// special files instead of silently creating an incomplete physical backup.
fn copy_tree(source: &Path, target: &Path) -> BTreeMap<String, (u64, String)> {
	assert!(!target.exists());
	assert!(fs::symlink_metadata(source).unwrap().file_type().is_dir());
	fs::create_dir(target).unwrap();
	fs::set_permissions(target, fs::Permissions::from_mode(0o700)).unwrap();
	let mut pending = vec![(source.to_path_buf(), target.to_path_buf())];
	let mut manifest = BTreeMap::new();
	let mut bytes = 0_u64;
	while let Some((source_dir, target_dir)) = pending.pop() {
		for entry in fs::read_dir(source_dir).unwrap() {
			let entry = entry.unwrap();
			let kind = entry.file_type().unwrap();
			let destination = target_dir.join(entry.file_name());
			if kind.is_dir() {
				fs::create_dir(&destination).unwrap();
				fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
				pending.push((entry.path(), destination));
			} else {
				assert!(kind.is_file(), "physical backup cannot omit links or special files");
				let length = entry.metadata().unwrap().len();
				bytes = bytes.checked_add(length).unwrap();
				assert!(bytes <= 512 * 1024 * 1024, "single-case cold backup copy budget exceeded");
				let before = file_hash(&entry.path());
				fs::copy(entry.path(), &destination).unwrap();
				fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
				assert_eq!(file_hash(&destination), before);
				assert_eq!(file_hash(&entry.path()), before, "cold source changed during copy");
				manifest.insert(entry.path().strip_prefix(source).unwrap().to_string_lossy().into_owned(), (length, before));
			}
		}
	}
	manifest
}

fn postgres_tool(name: &str) -> Command {
	Command::new(std::env::var_os("POSTGRES_BINS").map(PathBuf::from)
		.map(|directory| directory.join(name)).unwrap_or_else(|| PathBuf::from(name)))
}

async fn assert_clean_cluster(path: &Path) {
	assert!(!path.join("postmaster.pid").exists());
	let result = tokio::time::timeout(Duration::from_secs(10), postgres_tool("pg_controldata")
		.arg("-D").arg(path).env("LC_ALL", "C").kill_on_drop(true).output()).await.unwrap().unwrap();
	assert!(result.status.success(), "pg_controldata must inspect the stopped cluster");
	let output = String::from_utf8(result.stdout).unwrap();
	assert!(output.lines().any(|line| line.split_once(':').is_some_and(|(key, value)|
		key.trim() == "Database cluster state" && value.trim() == "shut down")),
		"a reaped postmaster alone does not establish a clean cold backup");
}

async fn watchman_coins(watchman: &Watchmand) -> BTreeSet<OutPoint> {
	let address = watchman.config().admin_address.unwrap();
	let mut rpc = WalletAdminClient::connect(format!("http://{address}")).await.unwrap();
	rpc.wallet_status(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap()
		.confirmed_utxos.into_iter().map(|point| OutPoint::from_str(&point).unwrap()).collect()
}

async fn wait_watchman_coins(watchman: &Watchmand, expected: &BTreeSet<OutPoint>) -> BTreeSet<OutPoint> {
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let actual = watchman_coins(watchman).await;
			if expected.is_subset(&actual) { break actual; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the real restored watchman wallet must observe every expected confirmed output")
}

/// Advance only the continuing Core here, then explicitly synchronize the
/// restored services. TestContext correctly skips stopped service helpers;
/// this local choice makes the cold-backup authority boundary explicit.
async fn mine_continuing_core(ctx: &TestContext, count: u32) -> BlockHeight {
	tokio::time::sleep(Duration::from_secs(1)).await;
	ctx.bitcoind().generate(count).await;
	BlockHeight::new(u32::try_from(ctx.bitcoind().get_block_count().await).unwrap())
}

async fn actual_fee(ctx: &TestContext, tx: &Transaction) -> Amount {
	let mut inputs = Amount::ZERO;
	for input in &tx.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		inputs += previous.output[input.previous_output.vout as usize].value;
	}
	inputs.checked_sub(tx.output.iter().map(|output| output.value).sum()).unwrap()
}

fn canonical(ctx: &TestContext, tx: &Transaction) -> BlockRef {
	let core = ctx.bitcoind().sync_client();
	let info = core.custom_get_raw_transaction_info(tx.compute_txid(), None).unwrap().unwrap();
	assert!(info.confirmations.unwrap_or(0) >= 6);
	let hash = info.blockhash.unwrap();
	let header = core.get_block_header_info(&hash).unwrap();
	assert_eq!(core.get_block_hash(header.height as u64).unwrap(), hash);
	assert!(core.get_block(&hash).unwrap().txdata.iter().any(|actual| actual == tx));
	BlockRef { height: BlockHeight::new(u32::try_from(header.height).unwrap()), hash }
}

async fn rebuilt(db: &Db, point: BlockRef) {
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			if db.read(async |tx| {
				let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
				Ok(view.captaind == point && view.watchmand == point && tx.expiry_view_is_rebuilt(view).await?)
			}).await.unwrap() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("both restored daemon ledger views must acknowledge this exact Core tip");
}

/// Freeze the real source/attempt/subscription membership before any terminal
/// claim, preimage disclosure, timeout or financial allocation.
async fn pending_checkpoint(db: &Db, hash: PaymentHash, source: VtxoId, grants: &[Vtxo<Full>]) -> (i64, i64, String) {
	let row = db.read(async |tx| Ok(tx.query_one("SELECT a.id,a.status::TEXT AS attempt_status,s.id AS subscription_id, \
		s.status::TEXT AS receive_status,a.user_fee_sat, \
		(SELECT COUNT(*) FROM lightning_payment_attempt p WHERE p.payment_hash=a.payment_hash) AS attempts, \
		(SELECT COUNT(*) FROM lightning_payment_attempt_htlc_vtxo l WHERE l.lightning_payment_attempt_id=a.id) AS inputs, \
		(SELECT COUNT(*) FROM lightning_payment_attempt_htlc_vtxo l WHERE l.lightning_payment_attempt_id=a.id AND l.vtxo_id=$2) AS source, \
		(SELECT COUNT(*) FROM vtxo v WHERE v.lightning_htlc_subscription_id=s.id) AS grants, \
		(SELECT COUNT(*) FROM expiry_lightning_receive_claim c WHERE c.subscription_id=s.id) AS claims, \
		(SELECT COUNT(*) FROM expiry_lightning_dispatch d WHERE d.id=a.id) AS dispatches, \
		(SELECT COUNT(*) FROM expiry_entitlement) AS entitlements \
		FROM expiry_lightning_internal_funding f JOIN lightning_payment_attempt a ON a.id=f.id \
		JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND a.lightning_htlc_subscription_id=s.id \
		WHERE a.payment_hash=$1 AND s.payment_hash=a.payment_hash", &[&hash.to_string(), &source.to_string()]).await?)).await.unwrap();
	assert_eq!(row.get::<_, String>("receive_status"), "htlcs-ready");
	let status: String = row.get("attempt_status");
	assert!(matches!(status.as_str(), "requested" | "submitted"));
	assert_eq!(row.get::<_, Option<i64>>("user_fee_sat"), Some(0));
	for name in ["attempts", "inputs", "source"] { assert_eq!(row.get::<_, i64>(name), 1); }
	assert_eq!(row.get::<_, i64>("grants"), i64::try_from(grants.len()).unwrap());
	for name in ["claims", "dispatches", "entitlements"] { assert_eq!(row.get::<_, i64>(name), 0); }
	assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let incoming = db.read(async |tx| server::database::htlc_vtxo::get_htlc_vtxo(tx, source).await).await.unwrap().unwrap();
	assert!(incoming.htlc.offchain_resolution.is_none() && incoming.htlc.chain_resolution.is_none());
	assert_eq!(incoming.vtxo.spend_state, SpendState::Spendable);
	for grant in grants {
		let actual = db.read(async |tx| server::database::htlc_vtxo::get_htlc_vtxo(tx, grant.id()).await).await.unwrap().unwrap();
		assert_eq!(actual.vtxo.vtxo.serialize(), grant.serialize());
		assert_eq!(actual.vtxo.spend_state, SpendState::HtlcRecvUnclaimed);
		assert!(actual.htlc.offchain_resolution.is_none() && actual.htlc.chain_resolution.is_none());
	}
	(row.get("id"), row.get("subscription_id"), status)
}

async fn signed_grant_ancestry(db: &Db, grants: &[Vtxo<Full>]) -> BTreeMap<String, Vec<u8>> {
	let mut expected = BTreeMap::new();
	for grant in grants {
		for step in grant.transactions() {
			let raw = bitcoin::consensus::serialize(&step.tx);
			if let Some(old) = expected.insert(step.tx.compute_txid().to_string(), raw.clone()) { assert_eq!(old, raw); }
		}
	}
	let ids = expected.keys().cloned().collect::<Vec<_>>();
	let rows = db.read(async |tx| Ok(tx.query("SELECT txid,signed_tx FROM virtual_transaction WHERE txid=ANY($1)", &[&ids]).await?)).await.unwrap();
	let actual = rows.into_iter().map(|row|
		(row.get::<_, String>("txid"), row.get::<_, Option<Vec<u8>>>("signed_tx"))).collect::<BTreeMap<_, _>>();
	assert_eq!(actual.len(), expected.len());
	for (txid, raw) in &expected { assert_eq!(actual.get(txid).and_then(Option::as_ref), Some(raw)); }
	expected
}

/// A finite case-local physical cut. Only actual canonical descendants of the
/// four original funding outputs enter this graph. Any extra input fails the
/// cut rather than silently adding an unbudgeted sponsor or historic liability.
fn canonical_descendants(ctx: &TestContext, first_height: u32, external: &BTreeMap<OutPoint, Amount>) -> BTreeMap<bitcoin::Txid, Transaction> {
	let core = ctx.bitcoind().sync_client();
	let last = u32::try_from(core.get_block_count().unwrap()).unwrap();
	assert!(last >= first_height && last - first_height <= 160);
	let mut known = external.keys().copied().collect::<BTreeSet<_>>();
	let mut graph = BTreeMap::new();
	for height in first_height + 1..=last {
		let hash = core.get_block_hash(u64::from(height)).unwrap();
		for tx in core.get_block(&hash).unwrap().txdata {
			if !tx.input.iter().any(|input| known.contains(&input.previous_output)) { continue; }
			assert!(tx.input.iter().all(|input| known.contains(&input.previous_output)),
				"a connected transaction introduced an unbudgeted external input");
			let txid = tx.compute_txid();
			for vout in 0..tx.output.len() { known.insert(OutPoint::new(txid, u32::try_from(vout).unwrap())); }
			assert!(graph.insert(txid, tx).is_none());
			assert!(graph.len() <= 64, "single-case descendant graph exceeded its declared work bound");
		}
	}
	graph
}

#[tokio::test]
async fn qualification_internal_htlcs_ready_physical_backup_refunds_absent_sender() {
	require_bark_version!(> "0.7.1");
	require_bitcoind_chain_source!();
	tokio::time::timeout(Duration::from_secs(180), Box::pin(lightning_backup())).await
		.expect("one internal signed-grant physical backup and seed recovery must finish within 180 seconds");
}

async fn lightning_backup() {
	assert!(!ark_testing::postgres::externally_hosted(), "the physical cluster must belong to this test");
	let ctx = TestContext::new("server/qualification_internal_htlcs_ready_physical_backup").await;
	let first_height = u32::try_from(ctx.bitcoind().get_block_count().await).unwrap();
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	assert_eq!(lightning.internal.grpc_client().await.getinfo(cln_rpc::GetinfoRequest {}).await
		.unwrap().into_inner().version, "v26.06.6");
	// Keep real operator returns outside the rounds wallet: the fixture must
	// not start unrelated replenishment while measuring this finite backup cut.
	let operator_address = ctx.bitcoind().get_new_address();
	let original = Box::pin(ctx.captaind("original").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal)
		.funded(sat(205_000)).cfg(|config| {
			config.vtxo_lifetime = BlockDelta::new(32);
			config.vtxopool.vtxo_lifetime = BlockDelta::new(32);
			config.vtxopool.vtxo_pre_expiry = BlockDelta::new(2);
			config.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { amount: sat(50_000), count: 4 }];
			config.invoice_check_interval = Duration::from_secs(1);
		}).watchmand_cfg(move |config| config.sweep_address = Some(operator_address.into_unchecked())).create()).await;
	original.wait_for_vtxopool(&ctx).await;
	let public_key = original.ark_info().await.server_pubkey;
	let fee_address = original.watchmand().wait_wallet_address().await;
	let mut sponsors = BTreeMap::new();
	for _ in 0..2 {
		let txid = ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
		let tx = ctx.bitcoind().await_transaction(txid).await;
		let outputs = tx.output.iter().enumerate().filter(|(_, output)|
			output.script_pubkey == fee_address.script_pubkey() && output.value == sat(100_000)).collect::<Vec<_>>();
		assert_eq!(outputs.len(), 1);
		assert!(sponsors.insert(OutPoint::new(txid, u32::try_from(outputs[0].0).unwrap()), sat(100_000)).is_none());
	}
	let funded = ctx.generate_blocks(6).await;
	original.wait_for_sync_height(funded).await;
	original.watchmand().wait_for_sync_height(funded).await;
	wait_watchman_coins(original.watchmand(), &sponsors.keys().copied().collect()).await;

	let recipient = ctx.bark_sdk("original-recipient", &original)
		.cfg(|config| config.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let invoice = recipient.bolt11_invoice(sat(90_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let receiver_key = recipient.new_address().await.unwrap().policy().user_pubkey();
	let receiver_registration = recipient.fallback_destination().await.unwrap();
	let receiver_mailbox = recipient.mailbox_identifier().as_pubkey();
	receiver_registration.verify(Network::Regtest, receiver_mailbox).unwrap();
	let sender = Box::pin(ctx.bark("original-sender", &original)
		.cfg(|config| config.daemon_manual_sync = true).create()).await;
	let customer_funding = ctx.fund_bark(&sender, sat(200_000)).await;
	let customer_initial = sender.onchain_utxos().await.into_iter().map(|u| (u.outpoint, u.amount)).collect::<BTreeMap<_, _>>();
	assert_eq!(customer_initial.len(), 1);
	assert_eq!(customer_initial.values().copied().sum::<Amount>(), sat(200_000));
	assert!(customer_initial.keys().all(|point| point.txid == customer_funding));
	let ids = Box::pin(sender.board_and_confirm_and_register(&ctx, sat(100_000))).await;
	assert_eq!(ids.len(), 1);
	let confirmed = ctx.generate_blocks(3).await;
	original.wait_for_sync_height(confirmed).await;
	original.watchmand().wait_for_sync_height(confirmed).await;
	let seed = sender.mnemonic().await.unwrap();
	let client = Box::pin(sender.client()).await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	assert_eq!(input.amount(), sat(100_000));
	let customer_root = input.chain_anchor();
	let board = ctx.bitcoind().await_transaction(customer_root.txid).await;
	input.validate(&board).unwrap();
	let board_fee = actual_fee(&ctx, &board).await;
	assert!(board_fee > Amount::ZERO);
	assert_eq!(board.input.iter().map(|i| i.previous_output).collect::<BTreeSet<_>>(), customer_initial.keys().copied().collect());
	let customer_change = sender.onchain_utxos().await.into_iter().map(|u| (u.outpoint, u.amount)).collect::<BTreeMap<_, _>>();
	assert_eq!(customer_change.values().copied().sum::<Amount>() + sat(100_000) + board_fee, sat(200_000));
	assert!(customer_change.keys().all(|point| point.txid == board.compute_txid() && *point != customer_root));
	let registration = client.fallback_destination().await.unwrap();
	let mailbox = client.mailbox_identifier().as_pubkey();
	registration.verify(Network::Regtest, mailbox).unwrap();
	assert!(registration.spk.is_p2wpkh());
	assert_ne!(registration.spk, receiver_registration.spk);
	assert_ne!(mailbox, receiver_mailbox);
	let key = client.get_vtxo_key(&input).await.unwrap();
	let owner_key = key.public_key();
	let tip = BlockHeight::new(u32::try_from(ctx.bitcoind().get_block_count().await).unwrap());
	let incoming_deadline = tip + original.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let outgoing_deadline = tip + BlockDelta::new(36);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
		VtxoPolicy::new_server_htlc_send(owner_key, hash, incoming_deadline)).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let mut rpc = original.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let incoming = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	assert_eq!(incoming.len(), 1);
	assert_eq!(incoming[0].amount(), sat(100_000));
	assert_eq!(incoming[0].chain_anchor(), customer_root);
	incoming[0].validate(&board).unwrap();
	client.register_vtxo_transactions_with_server(&incoming).await.unwrap();
	let source = incoming[0].id();
	lightning.sync().await;
	rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice: invoice.to_string(), payment_amount_sat: 90_000,
		htlc_vtxo_ids: vec![source.to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(5),
	}).await.unwrap();
	let response = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: receiver_key.serialize().to_vec(),
		htlc_recv_expiry: outgoing_deadline.to_u32(), lightning_receive_anti_dos: None,
	}).await.unwrap().into_inner();
	let grants = response.htlc_vtxos.iter().map(|bytes| Vtxo::<Full>::deserialize(bytes).unwrap()).collect::<Vec<_>>();
	assert_eq!(grants.len(), 2);
	assert_eq!(grants.iter().map(|grant| grant.amount()).sum::<Amount>(), sat(90_000));
	let pool_root = grants[0].chain_anchor();
	assert!(grants.iter().all(|grant| grant.chain_anchor() == pool_root));
	assert_ne!(pool_root, customer_root);
	let pool_funding = ctx.bitcoind().await_transaction(pool_root.txid).await;
	assert_eq!(pool_funding.output[pool_root.vout as usize].value, sat(200_000));
	for grant in &grants {
		grant.validate(&pool_funding).unwrap();
		let policy = grant.policy().as_server_htlc_recv().unwrap();
		assert_eq!(policy.payment_hash, hash);
		assert_eq!(policy.user_pubkey, receiver_key);
		assert_eq!(policy.htlc_expiry, outgoing_deadline);
		assert!(tip + BlockDelta::new(12) <= grant.expiry_height());
	}
	let mut pool_initial = BTreeMap::new();
	for input in &pool_funding.input {
		let prev = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		assert!(pool_initial.insert(input.previous_output, prev.output[input.previous_output.vout as usize].value).is_none());
	}
	assert_eq!(pool_initial.len(), 1);
	assert_eq!(pool_initial.values().copied().sum::<Amount>(), sat(205_000));
	let pool_fee = actual_fee(&ctx, &pool_funding).await;
	assert!(pool_fee > Amount::ZERO);
	let pool_change = pool_funding.output.iter().enumerate().filter(|(vout, _)| *vout != pool_root.vout as usize)
		.map(|(vout, output)| (OutPoint::new(pool_root.txid, u32::try_from(vout).unwrap()), output.value)).collect::<BTreeMap<_, _>>();
	assert_eq!(pool_change.values().copied().sum::<Amount>() + sat(200_000) + pool_fee, sat(205_000));
	assert!(pool_change.values().copied().sum::<Amount>() < sat(5_000), "the rounds change cannot finance a two-leaf refill");
	let server_coins = original.wallet_status().await.rounds.confirmed_utxos.into_iter().collect::<BTreeSet<_>>();
	assert!(pool_change.keys().all(|point| server_coins.contains(point)));
	for point in [customer_root, pool_root].into_iter().chain(sponsors.keys().copied()) {
		assert!(ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap().confirmations >= 6);
	}
	client.stop_daemon_wait().await.unwrap();
	drop(client);
	drop(sender);
	recipient.stop_daemon_wait().await.unwrap();
	drop(recipient);
	drop(rpc); // No returning client, claim/preimage, cooperative revoke or retry.
	let mut server_config = original.config().clone();
	let mut watch_config = original.watchmand().config().clone();
	let operator_script = watch_config.sweep_address.clone().unwrap().require_network(watch_config.network).unwrap().script_pubkey();
	assert!(server_config.data_dir.starts_with(&ctx.datadir) && watch_config.data_dir.starts_with(&ctx.datadir));
	assert!(server_config.postgres.password.is_none());
	original.watchmand().stop().await.unwrap();
	original.stop().await.unwrap();
	// The real operators have stopped before the final durable snapshot is read.
	let db = Db::connect(&server_config.postgres).await.unwrap();
	let checkpoint = pending_checkpoint(&db, hash, source, &grants).await;
	let ancestry = signed_grant_ancestry(&db, &grants).await;
	assert_eq!(db.read(async |tx| tx.get_fallback(mailbox).await).await.unwrap(), Some(registration.clone()));
	assert_eq!(db.read(async |tx| tx.get_fallback(receiver_mailbox).await).await.unwrap(), Some(receiver_registration.clone()));
	let changesets = db.read(async |tx| Ok(tx.query("SELECT kind::TEXT AS kind,id,content FROM wallet_changeset ORDER BY kind,id", &[]).await?
		.into_iter().map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1), row.get::<_, Vec<u8>>(2))).collect::<Vec<_>>())).await.unwrap();
	assert!(changesets.iter().any(|(kind, _, _)| kind == "watchman"));
	let before = BlockHeight::new(u32::try_from(ctx.bitcoind().get_block_count().await).unwrap());
	assert!(before + BlockDelta::new(2) < outgoing_deadline);
	assert!(grants.iter().all(|grant| before + BlockDelta::new(2) < grant.expiry_height()));
	let before_hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(before.to_u32())).unwrap();
	drop(db); // No old connection/pool is used after the owned cluster stops.
	ctx.postgres_manager().stop().await.unwrap();
	let source_pg = ctx.datadir.join("postgres/pg_data");
	assert_clean_cluster(&source_pg).await;
	let during_backup = mine_continuing_core(&ctx, 1).await;
	assert_eq!(during_backup.to_u32(), before.to_u32() + 1);
	assert_eq!(ctx.bitcoind().sync_client().get_block_hash(u64::from(before.to_u32())).unwrap(), before_hash);
	let backup = ctx.datadir.join("lightning-backup-private");
	let restored_root = ctx.datadir.join("lightning-restored-private");
	for dir in [&backup, &restored_root] {
		fs::create_dir(dir).unwrap();
		fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
	}
	let pg_hashes = copy_tree(&ctx.datadir.join("postgres"), &backup.join("postgres"));
	assert_eq!(copy_tree(&backup.join("postgres"), &restored_root.join("postgres")), pg_hashes);
	let mut hashes = BTreeMap::from([("postgres", pg_hashes.clone())]);
	for (name, source) in [("server", &server_config.data_dir), ("watchman", &watch_config.data_dir)] {
		assert!(source.join("mnemonic").is_file() && source.join("config.toml").is_file());
		let manifest = copy_tree(source, &backup.join(name));
		assert_eq!(copy_tree(&backup.join(name), &restored_root.join(name)), manifest);
		hashes.insert(name, manifest);
	}
	let private_manifest = backup.join("manifest.private.json");
	fs::write(&private_manifest, serde_json::to_vec_pretty(&serde_json::json!({
		"scope": "orderly physical PG and service identity backup at native internal HtlcsReady; Core and CLN continue",
		"files": hashes, "height": before, "blockhash": before_hash,
		"source_sha256": sha256::Hash::hash(include_bytes!("qualification_lightning_backup.rs")).to_string(),
		"native_test_sha256": file_hash(&std::env::current_exe().unwrap()),
	})).unwrap()).unwrap();
	fs::set_permissions(&private_manifest, fs::Permissions::from_mode(0o600)).unwrap();
	assert_clean_cluster(&restored_root.join("postgres/pg_data")).await;
	let helper = Arc::new(PostgresHelper::new("lightning-restored-postgres", restored_root.join("postgres")));
	let postgres = Daemon::wrap(ExistingCluster(helper.clone()));
	Box::pin(postgres.start()).await.unwrap();
	let config = helper.into_config(&server_config.postgres.name);
	assert_eq!(config.name, server_config.postgres.name);
	assert_ne!(config.port, server_config.postgres.port);
	let db = Db::connect(&config).await.unwrap();
	assert_eq!(pending_checkpoint(&db, hash, source, &grants).await, checkpoint);
	assert_eq!(signed_grant_ancestry(&db, &grants).await, ancestry);
	let restored_changesets = db.read(async |tx| Ok(tx.query("SELECT kind::TEXT AS kind,id,content FROM wallet_changeset ORDER BY kind,id", &[]).await?
		.into_iter().map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1), row.get::<_, Vec<u8>>(2))).collect::<Vec<_>>())).await.unwrap();
	assert_eq!(restored_changesets, changesets);
	for (id, expected) in [(mailbox, registration.clone()), (receiver_mailbox, receiver_registration.clone())] {
		assert_eq!(db.read(async |tx| tx.get_fallback(id).await).await.unwrap(), Some(expected));
	}
	server_config.postgres = config.clone();
	server_config.data_dir = restored_root.join("server");
	watch_config.postgres = config;
	watch_config.data_dir = restored_root.join("watchman");
	let restored = Arc::new(Captaind::new("lightning-restored-server", ctx.bitcoind_arc(), server_config));
	Box::pin(restored.start()).await.unwrap();
	let watchman = Watchmand::new("lightning-restored-watchman", ctx.bitcoind_arc(), watch_config);
	Box::pin(watchman.start()).await.unwrap();
	restored.attach_watchmand(watchman);
	ctx.register_test_captaind(restored.clone());
	assert_eq!(restored.ark_info().await.server_pubkey, public_key);
	assert_eq!(pending_checkpoint(&db, hash, source, &grants).await, checkpoint);
	wait_watchman_coins(restored.watchmand(), &sponsors.keys().copied().collect()).await;
	assert_eq!(db.read(async |tx| tx.get_fallback(mailbox).await).await.unwrap(), Some(registration.clone()));

	// Drive authentic ancestry, then use actual confirmed-height+CSV and signed
	// CLTV maturity. A source-face-value refund cannot bypass either grant.
	let mut mined = 1_u32;
	let mut maturity = None;
	for _ in 0..80 {
		let height = mine_continuing_core(&ctx, 1).await;
		mined += 1;
		restored.wait_for_sync_height(height).await;
		restored.watchmand().wait_for_sync_height(height).await;
		let hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(height.to_u32())).unwrap();
		rebuilt(&db, BlockRef { height, hash }).await;
		let mut all_confirmed = true;
		let mut latest = outgoing_deadline.to_u32();
		for grant in &grants {
			let info = ctx.bitcoind().sync_client().custom_get_raw_transaction_info(grant.point().txid, None).unwrap();
			let Some(info) = info.filter(|info| info.confirmations.unwrap_or(0) > 0) else { all_confirmed = false; break; };
			let blockhash = info.blockhash.unwrap();
			let header = ctx.bitcoind().sync_client().get_block_header_info(&blockhash).unwrap();
			assert_eq!(ctx.bitcoind().sync_client().get_block_hash(header.height as u64).unwrap(), blockhash);
			latest = latest.max(u32::try_from(header.height).unwrap() + u32::from(grant.exit_delta().to_u16()));
		}
		if all_confirmed { maturity = Some(latest); break; }
	}
	let maturity = maturity.expect("both real receive grants must become canonical without a returning client");
	let height = u32::try_from(ctx.bitcoind().get_block_count().await).unwrap();
	assert!(height < maturity, "the fixture must observe its genuine pre-maturity barrier");
	let no_early_payout = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_allocation a \
		JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
		WHERE e.vtxo_id=$1 AND c.disposition='customer' AND a.broadcast_at IS NOT NULL", &[&source.to_string()]).await?
		.get::<_, i64>(0))).await.unwrap();
	assert_eq!(no_early_payout, 0);
	if maturity > height + 1 {
		let count = maturity - 1 - height;
		let height = mine_continuing_core(&ctx, count).await;
		mined += count;
		restored.wait_for_sync_height(height).await;
		restored.watchmand().wait_for_sync_height(height).await;
		let hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(height.to_u32())).unwrap();
		rebuilt(&db, BlockRef { height, hash }).await;
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_allocation a \
			JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			WHERE e.vtxo_id=$1 AND c.disposition='customer' AND a.broadcast_at IS NOT NULL", &[&source.to_string()]).await?
			.get::<_, i64>(0))).await.unwrap(), 0);
	}
	let height = mine_continuing_core(&ctx, 1).await;
	mined += 1;
	assert_eq!(height.to_u32(), maturity);
	restored.wait_for_sync_height(height).await;
	restored.watchmand().wait_for_sync_height(height).await;
	let block_hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(height.to_u32())).unwrap();
	rebuilt(&db, BlockRef { height, hash: block_hash }).await;
	let (allocation, parent) = tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let rows = db.read(async |tx| Ok(tx.query("SELECT a.id,a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL",
				&[&source.to_string()]).await?)).await.unwrap();
			assert!(rows.len() <= 1);
			if let Some(row) = rows.first() { break (row.get::<_, i64>(0), deserialize::<Transaction>(row.get(1)).unwrap()); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("cold-restored signed grants must jointly close and refund the absent sender");
	let grant_points = grants.iter().map(|grant| grant.point()).collect::<BTreeSet<_>>();
	let mut sender_path = BTreeSet::from([customer_root]);
	for step in incoming[0].transactions() {
		for vout in 0..step.tx.output.len() {
			sender_path.insert(OutPoint::new(step.tx.compute_txid(), u32::try_from(vout).unwrap()));
		}
	}
	let original_inputs = parent.input.iter().filter(|input| !grant_points.contains(&input.previous_output)).collect::<Vec<_>>();
	assert_eq!(original_inputs.len(), 1);
	assert!(sender_path.contains(&original_inputs[0].previous_output));
	let previous = ctx.bitcoind().await_transaction(original_inputs[0].previous_output.txid).await;
	assert_eq!(previous.output[original_inputs[0].previous_output.vout as usize].value, sat(100_000));
	for grant in &grants {
		let inputs = parent.input.iter().filter(|i| i.previous_output == grant.point()).collect::<Vec<_>>();
		assert_eq!(inputs.len(), 1);
		let script = grant.policy().as_server_htlc_recv().unwrap()
			.server_claim_after_expiry_clause(grant.server_pubkey(), grant.exit_delta()).tapscript();
		assert_eq!(inputs[0].witness.taproot_leaf_script().unwrap().script, script.as_script());
	}
	assert_eq!(parent.input.len(), grants.len() + 1);
	assert_eq!(parent.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>().len(), parent.input.len());
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let confirmed = mine_continuing_core(&ctx, 6).await;
	mined += 6;
	restored.wait_for_sync_height(confirmed).await;
	restored.watchmand().wait_for_sync_height(confirmed).await;
	let confirmed_hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(confirmed.to_u32())).unwrap();
	rebuilt(&db, BlockRef { height: confirmed, hash: confirmed_hash }).await;
	let parent_block = canonical(&ctx, &parent);
	assert!(parent_block.height.to_u32() >= maturity);
	assert_eq!(parent.version.0, 3);
	assert!(parent.vsize() <= 10_000);
	assert_eq!(actual_fee(&ctx, &parent).await, Amount::ZERO);
	let outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == registration.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert_eq!(outputs[0].1.value, sat(99_000));
	let payout = OutPoint::new(parent.compute_txid(), u32::try_from(outputs[0].0).unwrap());
	for output in parent.output.iter().filter(|output| output.script_pubkey != registration.spk) {
		assert!(output.is_p2a_fee_anchor() || output.script_pubkey == operator_script);
	}
	let anchors = parent.output.iter().enumerate().filter(|(_, output)| output.is_p2a_fee_anchor())
		.map(|(index, _)| OutPoint::new(parent.compute_txid(), u32::try_from(index).unwrap())).collect::<Vec<_>>();
	assert_eq!(anchors.len(), 1);
	let block = ctx.bitcoind().sync_client().get_block(&parent_block.hash).unwrap();
	let children = block.txdata.iter().filter(|tx| tx.input.iter().any(|input| input.previous_output == anchors[0])).collect::<Vec<_>>();
	assert_eq!(children.len(), 1);
	let child = (*children[0]).clone();
	assert_eq!(canonical(&ctx, &child), parent_block);
	assert_eq!(child.version.0, 3);
	assert!(child.vsize() <= 1_000);
	let child_fee = actual_fee(&ctx, &child).await;
	assert!(child_fee > Amount::ZERO);
	let operator_remainder = parent.output.iter().filter(|output| output.script_pubkey != registration.spk).map(|output| output.value).sum::<Amount>();
	assert_eq!(operator_remainder, sat(91_000), "90k operator grants are not additional customer principal");
	assert!(pool_change.values().copied().sum::<Amount>() + operator_remainder < sat(100_000));
	let stored_child = db.read(async |tx| Ok(tx.query_one("SELECT raw_tx,fee_sat,operator_remainder_sat,confirmed_height,confirmed_hash \
		FROM expiry_fee_child WHERE allocation_id=$1 AND txid=$2", &[&allocation, &child.compute_txid().to_string()]).await?)).await.unwrap();
	assert_eq!(deserialize::<Transaction>(stored_child.get("raw_tx")).unwrap(), child);
	assert_eq!(stored_child.get::<_, i64>("fee_sat"), i64::try_from(child_fee.to_sat()).unwrap());
	assert_eq!(stored_child.get::<_, i64>("operator_remainder_sat"), i64::try_from(operator_remainder.to_sat()).unwrap());
	assert_eq!(stored_child.get::<_, Option<i64>>("confirmed_height"), Some(i64::from(parent_block.height.to_u32())));
	assert_eq!(stored_child.get::<_, Option<String>>("confirmed_hash"), Some(parent_block.hash.to_string()));
	let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,c.owner_mailbox,c.user_pubkey,c.spk,c.gross_sat,c.fee_sat, \
		c.payout_sat,c.forfeited_sat,c.active,c.paying_txid,a.id,a.status,a.sweep_tx FROM expiry_allocation_coin c \
		JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN expiry_allocation a ON a.id=c.allocation_id \
		WHERE c.disposition='customer'", &[]).await?)).await.unwrap();
	assert_eq!(rows.len(), 1, "only one original sender principal can receive customer money");
	let row = &rows[0];
	assert_eq!(row.get::<_, String>("vtxo_id"), source.to_string());
	assert_eq!(row.get::<_, String>("owner_mailbox"), mailbox.to_string());
	assert_eq!(row.get::<_, String>("user_pubkey"), owner_key.to_string());
	assert_eq!(row.get::<_, String>("spk"), registration.spk.as_bytes().to_lower_hex_string());
	assert_eq!(row.get::<_, i64>("gross_sat"), 100_000);
	assert_eq!(row.get::<_, i64>("fee_sat"), 1_000);
	assert_eq!(row.get::<_, i64>("payout_sat"), 99_000);
	assert_eq!(row.get::<_, i64>("forfeited_sat"), 0);
	assert!(row.get::<_, bool>("active"));
	assert_eq!(row.get::<_, Option<String>>("paying_txid"), Some(payout.txid.to_string()));
	assert_eq!(row.get::<_, i64>("id"), allocation);
	assert_eq!(row.get::<_, String>("status"), "confirmed");
	assert_eq!(deserialize::<Transaction>(row.get("sweep_tx")).unwrap(), parent);
	assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let state = db.read(async |tx| server::database::htlc_vtxo::get_htlc_vtxo(tx, source).await).await.unwrap().unwrap();
	assert!(state.htlc.offchain_resolution.is_none(), "physical recovery must never manufacture a cooperative revocation");
	let kept = db.read(async |tx| Ok(tx.query_one("SELECT \
		(SELECT COUNT(*) FROM expiry_lightning_internal_funding WHERE id=$1 AND subscription_id=$2) AS funding, \
		(SELECT COUNT(*) FROM lightning_payment_attempt WHERE payment_hash=$3) AS attempts, \
		(SELECT COUNT(*) FROM vtxo WHERE lightning_htlc_subscription_id=$2) AS grants, \
		(SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE subscription_id=$2) AS claims, \
		(SELECT COUNT(*) FROM expiry_lightning_dispatch WHERE id=$1) AS dispatches",
		&[&checkpoint.0,&checkpoint.1,&hash.to_string()]).await?)).await.unwrap();
	assert_eq!(kept.get::<_, i64>("funding"), 1);
	assert_eq!(kept.get::<_, i64>("attempts"), 1);
	assert_eq!(kept.get::<_, i64>("grants"), 2);
	assert_eq!(kept.get::<_, i64>("claims"), 0);
	assert_eq!(kept.get::<_, i64>("dispatches"), 0);

	// Only now may a NEW directory use the sender seed and genesis birthday.
	// No original client database, signed VTXO or payout transaction is imported.
	assert!(!ctx.datadir.join("seed-only-sender").exists());
	let recovered = Box::pin(ctx.bark("seed-only-sender", &restored).mnemonic(seed)
		.cfg(|config| config.daemon_manual_sync = true).create()).await;
	let client = Box::pin(recovered.client()).await;
	assert_eq!(client.mailbox_identifier().as_pubkey(), mailbox);
	let rebound = client.fallback_destination().await.unwrap();
	rebound.verify(Network::Regtest, mailbox).unwrap();
	assert!(rebound.same_binding(&registration));
	client.sync_fallback_payouts().await.unwrap();
	let receipts = client.fallback_payouts().await.unwrap();
	assert_eq!(receipts.len(), 1);
	assert_eq!(receipts[0].vtxo_id, source);
	assert_eq!(receipts[0].gross, sat(100_000));
	let active = receipts[0].attempts.iter().filter(|attempt| attempt.active).collect::<Vec<_>>();
	assert_eq!(active.len(), 1);
	assert_eq!(active[0].disposition, bark::FallbackDisposition::CustomerPayout);
	assert_eq!(active[0].parent.as_ref(), Some(&parent));
	assert_eq!(active[0].output_index, Some(payout.vout));
	assert_eq!(active[0].payout, sat(99_000));
	assert_eq!(active[0].fee, sat(1_000));
	let balance = client.balance().await.unwrap();
	assert_eq!([
		balance.spendable, balance.needs_refresh, balance.awaiting_settlement,
		balance.pending_board, balance.pending_in_round, balance.pending_arkoor_send,
		balance.pending_offboard, balance.pending_lightning_send,
		balance.claimable_lightning_receive, balance.pending_exit,
	], [Amount::ZERO; 10]);
	client.stop_daemon_wait().await.unwrap();
	drop(client);
	let recovered_inputs = recovered.onchain_utxos().await.into_iter().map(|u| (u.outpoint, u.amount)).collect::<BTreeMap<_, _>>();
	let mut expected_inputs = customer_change.clone();
	assert!(expected_inputs.insert(payout, sat(99_000)).is_none());
	assert_eq!(recovered_inputs, expected_inputs);
	let recovered_value = recovered_inputs.values().copied().sum::<Amount>();
	assert_eq!(recovered_value + board_fee + sat(1_000), sat(200_000));
	let destination = ctx.bitcoind().get_new_address();
	recovered.onchain_drain(&destination).await;
	let mut drains = Vec::new();
	for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
		let tx = ctx.bitcoind().await_transaction(txid).await;
		if tx.input.iter().any(|input| input.previous_output == payout) { drains.push(tx); }
	}
	assert_eq!(drains.len(), 1);
	let drain = drains.pop().unwrap();
	let mut actual_inputs = BTreeMap::new();
	for input in &drain.input {
		assert_eq!(input.witness.len(), 2, "the seed-derived BIP84 wallet must produce actual witnesses");
		let prev = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		let output = &prev.output[input.previous_output.vout as usize];
		assert!(output.script_pubkey.is_p2wpkh());
		assert!(actual_inputs.insert(input.previous_output, output.value).is_none());
	}
	assert_eq!(actual_inputs, recovered_inputs);
	assert_eq!(drain.output.len(), 1);
	assert_eq!(drain.output[0].script_pubkey, destination.script_pubkey());
	let drain_fee = actual_fee(&ctx, &drain).await;
	assert!(drain_fee > Amount::ZERO);
	assert_eq!(drain.output[0].value + drain_fee, recovered_value);
	let final_height = mine_continuing_core(&ctx, 6).await;
	mined += 6;
	assert!(mined <= 96, "post-backup progression and confirmations exceeded the bounded block budget");
	restored.wait_for_sync_height(final_height).await;
	restored.watchmand().wait_for_sync_height(final_height).await;
	let final_hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(final_height.to_u32())).unwrap();
	rebuilt(&db, BlockRef { height: final_height, hash: final_hash }).await;
	canonical(&ctx, &drain);
	let actual = ctx.bitcoind().sync_client().get_tx_out(&drain.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(actual.confirmations >= 6);
	assert_eq!(actual.value, drain.output[0].value);
	assert_eq!(actual.script_pub_key.script().unwrap(), destination.script_pubkey());
	assert_eq!(ctx.bitcoind().get_received_by_address(&destination), actual.value);
	for point in recovered_inputs.keys() {
		assert!(ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
	}
	assert_eq!(actual.value + drain_fee + board_fee + sat(1_000), sat(200_000));

	let mut external = pool_initial.clone();
	for (point, value) in customer_initial.iter().chain(sponsors.iter()) { assert!(external.insert(*point, *value).is_none()); }
	assert_eq!(external.len(), 4);
	assert_eq!(external.values().copied().sum::<Amount>(), sat(605_000));
	let graph = canonical_descendants(&ctx, first_height, &external);
	for tx in [&board, &pool_funding, &parent, &drain] { assert_eq!(graph.get(&tx.compute_txid()), Some(tx)); }
	assert_eq!(graph.get(&child.compute_txid()), Some(&child));
	for (txid, raw) in &ancestry { assert_eq!(graph.get(&bitcoin::Txid::from_str(txid).unwrap()), Some(&deserialize::<Transaction>(raw).unwrap())); }
	let mut spent = BTreeSet::new();
	let mut all_fees = Amount::ZERO;
	let mut monetary_outputs = BTreeMap::new();
	let mut sender_outputs = BTreeMap::new();
	for (txid, tx) in &graph {
		canonical(&ctx, tx);
		all_fees += actual_fee(&ctx, tx).await;
		for input in &tx.input { assert!(spent.insert(input.previous_output), "canonical connected cut repeats an input"); }
		for (vout, output) in tx.output.iter().enumerate() {
			let point = OutPoint::new(*txid, u32::try_from(vout).unwrap());
			assert!(monetary_outputs.insert(point, output.clone()).is_none());
			assert_ne!(output.script_pubkey, receiver_registration.spk, "unclaimed grants cannot create a recipient refund");
			if output.script_pubkey == registration.spk { assert!(sender_outputs.insert(point, output.value).is_none()); }
		}
	}
	assert_eq!(sender_outputs, BTreeMap::from([(payout, sat(99_000))]), "only one actual customer economic output is allowed in the entire connected graph");
	let watch_coins = watchman_coins(restored.watchmand()).await;
	let server_coins = restored.wallet_status().await.rounds.confirmed_utxos.into_iter().collect::<BTreeSet<_>>();
	let recovery_point = OutPoint::new(drain.compute_txid(), 0);
	let mut operator_retained = Amount::ZERO;
	let mut leaf_sum = Amount::ZERO;
	let candidates = monetary_outputs.keys().copied().chain(external.keys().copied()).collect::<BTreeSet<_>>();
	for point in candidates.into_iter().filter(|point| !spent.contains(point)) {
		let output = if let Some(output) = monetary_outputs.get(&point) { output.clone() } else {
			let tx = ctx.bitcoind().await_transaction(point.txid).await;
			tx.output[point.vout as usize].clone()
		};
		if output.value == Amount::ZERO { continue; }
		let actual = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, output.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), output.script_pubkey);
		leaf_sum += actual.value;
		if point == recovery_point { continue; }
		assert!(pool_change.contains_key(&point) && server_coins.contains(&point)
			|| watch_coins.contains(&point) || output.script_pubkey == operator_script,
			"every other actual nonzero terminal output must have explicit operator ownership");
		operator_retained += actual.value;
	}
	assert_eq!(leaf_sum, actual.value + operator_retained);
	assert_eq!(leaf_sum + all_fees, sat(605_000), "all actual board/pool/progression/payout-child/drain fees and terminal values must conserve the finite initial cut");
	assert!(all_fees >= board_fee + pool_fee + child_fee + drain_fee);
	for (point, amount) in &pool_change {
		assert!(server_coins.contains(point));
		let output = ctx.bitcoind().sync_client().get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert_eq!(output.value, *amount);
		assert!(output.confirmations >= 6);
	}
	for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
		let tx = ctx.bitcoind().await_transaction(txid).await;
		assert!(!tx.input.iter().any(|input| monetary_outputs.contains_key(&input.previous_output) || external.contains_key(&input.previous_output)),
			"no unconfirmed continuation can hide another connected payment or drain");
	}
	assert_eq!(signed_grant_ancestry(&db, &grants).await, ancestry);
	let raw = db.read(async |tx| Ok(tx.query_one("SELECT sweep_tx FROM expiry_allocation WHERE id=$1", &[&allocation]).await?
		.get::<_, Vec<u8>>(0))).await.unwrap();
	assert_eq!(deserialize::<Transaction>(&raw).unwrap(), parent);
	fs::write(ctx.datadir.join("lightning-backup-public-receipt.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"scope": "orderly same-candidate PG/server/watch physical backup at internal HtlcsReady; continuing Core and CLN; both original clients absent",
		"source_sha256": sha256::Hash::hash(include_bytes!("qualification_lightning_backup.rs")).to_string(),
		"source": source, "grants": grants.iter().map(|g| g.id()).collect::<Vec<_>>(), "attempt": checkpoint.0,
		"subscription": checkpoint.1, "prebackup_height": before, "prebackup_blockhash": before_hash,
		"customer_root": customer_root, "pool_root": pool_root, "sponsors": sponsors,
		"customer_initial_sat": 200_000, "operator_initial_sat": 205_000, "sponsors_initial_sat": 200_000,
		"grant_amount_sat": 90_000, "sender_principal_sat": 100_000, "payout": payout, "payout_sat": 99_000,
		"fallback_fee_sat": 1_000, "joint_operator_remainder_sat": operator_remainder.to_sat(),
		"actual_board_miner_fee_sat": board_fee.to_sat(), "actual_pool_miner_fee_sat": pool_fee.to_sat(),
		"actual_joint_child_miner_fee_sat": child_fee.to_sat(), "recovery_txid": drain.compute_txid(),
		"recovery_input_sat": recovered_value.to_sat(), "recovery_output_sat": actual.value.to_sat(),
		"actual_recovery_miner_fee_sat": drain_fee.to_sat(), "all_connected_miner_fees_sat": all_fees.to_sat(),
		"operator_retained_sat": operator_retained.to_sat(), "external_cut_sat": 605_000,
		"canonical_graph_transactions": graph.len(), "joint_parent_block": parent_block,
		"postbackup_blocks_mined": mined, "grant_maturity_height": maturity,
		"cluster_file_count": pg_hashes.len(), "wallet_changesets_preserved": changesets.len(),
		"private_manifest_sha256": file_hash(&private_manifest),
	})).unwrap()).unwrap();
	drop(recovered);
	drop(db);
	restored.watchmand().stop().await.unwrap();
	restored.stop().await.unwrap();
	postgres.stop().await.unwrap();
}
