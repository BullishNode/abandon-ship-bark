//! A killed writer must leave the exact financial handoff recoverable.
//! This fixture never starts the legacy sweeping route against its direct inputs.

use super::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use bitcoind_async_client::traits::Reader;
use server::watchman::direct_delivery::{DirectDelivery, DeliveryState};

#[path = "frozen_expiry.rs"]
mod frozen_expiry;

#[path = "watchmand_direct.rs"]
mod watchmand_direct;
#[path = "witness_submission.rs"]
mod witness_submission;

#[derive(Clone, Serialize, Deserialize)]
struct CommitRecipe {
	config_path: PathBuf,
	backing: Vec<Vec<u8>>,
	allocation_ids: Vec<ark::VtxoId>,
	freeze_set: String,
	recipients: Vec<TxOut>,
	operator_script: ScriptBuf,
	committed_marker: PathBuf,
	fee_rates: Vec<u64>,
}

fn limits() -> CarrierLimits {
	CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb(50).unwrap(),
		max_miner_fee: sat(10_000), max_weight: Weight::from_wu(400_000) }
}

fn budget() -> BackingSnapshotLimits {
	BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 }
}

// The parent owns Core/Postgres and the stopped server fixture. This child owns
// only signing, the durable handoff and its fee guard; no daemon is launched.
#[tokio::test]
#[ignore = "invoked only by the isolated commit-before-broadcast crash fixture"]
async fn direct_delivery_commit_worker() {
	let path = std::env::var_os("DIRECT_DELIVERY_COMMIT_RECIPE").expect("owned recipe is required");
	let recipe: CommitRecipe = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&recipe.config_path).unwrap()).unwrap();
	let db = Db::connect(&config.postgres).await.unwrap();
	let core = server::bitcoind::build_client(&config.bitcoind.url, config.bitcoind.auth()).unwrap();
	let height = BlockHeight::new(core.get_block_count().await.unwrap() as u32);
	let tip = server::bitcoind::get_block_by_height(&core, height).await.unwrap();
	let mnemonic = wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	let fee_wallet = PersistedWallet::load_derive_from_master_xpriv(db.clone(), core,
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let fee_wallet = InstrumentedLock::new("direct_delivery_crash_writer", fee_wallet);
	let backing = recipe.backing.iter().map(|v| ServerVtxo::deserialize(v)).collect::<Result<Vec<_>, _>>().unwrap();
	let mut guards = Vec::new();
	let mut txids = Vec::new();
	for rate in &recipe.fee_rates {
		let rate = FeeRate::from_sat_per_vb(*rate).unwrap();
		let (mut held, carrier) = build_direct_carrier(&signer, fee_wallet.lock_owned().await, &backing,
			&recipe.recipients, recipe.operator_script.clone(), height, rate, limits()).await.unwrap();
		held.commit_tx(&carrier.tx);
		db.write(async |t| t.store_frozen_expiry_carrier(&recipe.freeze_set, &recipe.allocation_ids,
			&backing, &carrier, &held, &recipe.operator_script, Network::Regtest, rate, limits(), budget()).await).await.unwrap();
		let txid = carrier.tx.compute_txid().to_string();
		let generation = db.read(async |t| Ok(t.query_one("SELECT generation FROM expiry_carrier WHERE txid=$1", &[&txid]).await?.get::<_, i64>("generation"))).await.unwrap();
		db.write(async |t| t.store_frozen_expiry_carrier(&recipe.freeze_set, &recipe.allocation_ids,
			&backing, &carrier, &held, &recipe.operator_script, Network::Regtest, rate, limits(), budget()).await).await.unwrap();
		let retried = db.read(async |t| Ok(t.query_one("SELECT generation FROM expiry_carrier WHERE txid=$1", &[&txid]).await?.get::<_, i64>("generation"))).await.unwrap();
		assert_eq!(generation, retried, "idempotent publication must retain its original generation");
		held.take_staged();
		txids.push(carrier.tx.compute_txid());
		guards.push(carrier.fee_reservation);
	}
	let mut marker = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(&recipe.committed_marker).unwrap();
	marker.write_all(&serde_json::to_vec(&txids).unwrap()).unwrap();
	marker.sync_all().unwrap();
	// SIGKILL is sent only after this durable commit marker exists. Keep both
// process-local guards alive; the child has no broadcast call at all.
	std::future::pending::<()>().await;
	drop(guards);
}

struct CrashCase {
	ctx: TestContext,
	db: Db,
	wallet: PersistedWallet,
	mnemonic: bip39::Mnemonic,
	recipe: CommitRecipe,
	versions: Vec<bitcoin::Transaction>,
	board_funding_txid: bitcoin::Txid,
}

async fn run_commit_worker(recipe: &CommitRecipe, recipe_path: PathBuf) -> Vec<bitcoin::Txid> {
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&recipe_path).unwrap();
	file.write_all(&serde_json::to_vec(recipe).unwrap()).unwrap();
	file.sync_all().unwrap();
	let mut worker = Command::new(std::env::current_exe().unwrap())
		.args(["--ignored", "--exact", "direct_delivery::direct_delivery_commit_worker", "--nocapture"])
		.env("DIRECT_DELIVERY_COMMIT_RECIPE", &recipe_path).kill_on_drop(true).spawn().unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		while !recipe.committed_marker.exists() {
			assert!(worker.try_wait().unwrap().is_none(), "writer exited before durable commit");
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("durable handoff marker must precede injected crash");
	worker.kill().await.unwrap();
	let exit = worker.wait().await.unwrap();
	assert_eq!(std::os::unix::process::ExitStatusExt::signal(&exit), Some(9));
	serde_json::from_slice(&std::fs::read(&recipe.committed_marker).unwrap()).unwrap()
}

async fn crash_case(name: &str, fee_rates: Vec<u64>) -> CrashCase {
	let AutomaticBoardFixture { ctx, srv, coin, record, mnemonic } =
		automatic_board_fixture_with_fee(name, sat(1_000)).await;
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap();
	let roots = [root];
	let terminal = [ServerVtxo::from(coin.clone())];
	let residuals = partition_residuals(&roots, &terminal);
	let funding_tx = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let inventory = snapshot(&db, &roots, budget().max_rows, budget().max_serialized_bytes).await.unwrap();
	inventory.check_partition(&terminal, &[funding_tx.clone()], &residuals, budget()).unwrap();
	let master_mnemonic = wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &master_mnemonic.to_seed("")).unwrap();
	let initial_height = core.get_block_count().unwrap();
	let tip = BlockRef { height: BlockHeight::new(initial_height as u32), hash: core.get_block_hash(initial_height).unwrap() };
	let mut wallet = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	ctx.bitcoind().fund_addr(wallet.next_unused_address(KEYCHAIN).address, sat(1_000_000)).await;
	if fee_rates.len() > 1 {
		ctx.bitcoind().fund_addr(wallet.next_unused_address(KEYCHAIN).address, sat(900_000)).await;
	}
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(initial_height as u32) + 3).await;
	sync_fee_wallet(&mut wallet, &core);
	wallet.persist().await.unwrap();
	drop(wallet);
	let funding = BTreeMap::from([(funding_tx.compute_txid(), funding_tx)]);
	let cancellations = BTreeSet::new();
	let send_evidence = BTreeMap::new();
	let context = OwnershipContext { tip: core.get_block_count().unwrap() as u32, grace: 0,
		max_dependencies: 100, server_pubkey: coin.server_pubkey(), funding: &funding, canceled_receives: &cancellations };
	let freeze = db.write(async |t| t.freeze_expiry_sources(&roots, &terminal, &residuals,
		&context, &send_evidence, Network::Regtest, budget()).await).await.unwrap();
	let recipe = CommitRecipe { config_path: srv.config().data_dir.join("config.toml"),
		backing: roots.iter().map(ProtocolEncoding::serialize).collect(), allocation_ids: freeze.allocation_ids,
		freeze_set: freeze.id, recipients: freeze.recipients, operator_script: ctx.bitcoind().get_new_address().script_pubkey(),
		committed_marker: ctx.datadir.join("committed-before-broadcast"), fee_rates };
	let txids = run_commit_worker(&recipe, ctx.datadir.join("commit-recipe.json")).await;
	let restarted = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let mut versions = Vec::new();
	for txid in txids {
		let row = db.read(async |t| Ok(t.query_one("SELECT signed_tx FROM expiry_carrier WHERE txid=$1", &[&txid.to_string()]).await?)).await.unwrap();
		let bytes = row.get::<_, Vec<u8>>("signed_tx");
		let signed: bitcoin::Transaction = bitcoin::consensus::deserialize(&bytes).unwrap();
		assert_eq!(signed.compute_txid(), txid);
		assert!(signed.output.contains(&TxOut { value: sat(19_000), script_pubkey: record.spk.clone() }));
		assert!(!core.get_raw_mempool().unwrap().contains(&txid), "a database commit must not masquerade as delivery");
		assert!(restarted.tx_graph().get_tx(txid).is_some_and(|tx| bitcoin::consensus::serialize(&tx) == bytes));
		let mappings = db.read(async |t| Ok(t.query("SELECT allocation_id,vout FROM expiry_carrier_allocation WHERE txid=$1", &[&txid.to_string()]).await?)).await.unwrap();
		assert_eq!(mappings.len(), recipe.allocation_ids.len());
		versions.push(signed);
	}
	CrashCase { ctx, db, wallet: restarted, mnemonic, recipe, versions, board_funding_txid: coin.chain_anchor().txid }
}

#[tokio::test]
async fn direct_history_refuses_forgotten_signed_version_and_allocation_after_confirmation() {
	let case = crash_case("direct_history_refuses_forgotten_signed_version_and_allocation_after_confirmation", vec![30,40]).await;
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let root: ServerVtxo = ServerVtxo::deserialize(&case.recipe.backing[0]).unwrap();
	let key = root.server_pubkey();
	let load = async || server::payout_witness::DirectPayoutWitness::load(&config.data_dir,
		Network::Regtest, key, &case.db, budget()).await;
	let history = load().await.unwrap();
	history.record_group(&case.db, &case.recipe.freeze_set).await.unwrap();
	history.record_group(&case.db, &case.recipe.freeze_set).await.unwrap();
	let delivery = recovered(&case).await;
	confirm_once(&case, &delivery, &case.versions[1]).await;
	// Actual confirmation changes only observation, never the witnessed amounts,
	// signed bytes or economic owner. Restart must accept that legitimate change.
	load().await.unwrap();
	assert!(server::payout_witness::DirectPayoutWitness::initialize(&config.data_dir,
		Network::Regtest, key, &case.db).await.is_err(), "existing journal cannot bootstrap empty history");
	let latest = case.versions[1].compute_txid().to_string();
	case.db.write(async |t| {
		t.execute("CREATE TABLE direct_history_saved_carrier AS SELECT txid,generation,signed_tx,backing_ids,fee_inputs,
			miner_fee_sat,funding_subsidy_sat,created_at,confirmed_height,confirmed_block FROM expiry_carrier WHERE txid=$1", &[&latest]).await?;
		t.execute("CREATE TABLE direct_history_saved_mapping AS SELECT txid,allocation_id,vout FROM expiry_carrier_allocation WHERE txid=$1", &[&latest]).await?;
		t.execute("DELETE FROM expiry_carrier_allocation WHERE txid=$1", &[&latest]).await?;
		t.execute("DELETE FROM expiry_carrier WHERE txid=$1", &[&latest]).await?;
		Ok(())
	}).await.unwrap();
	let error = load().await.err().expect("older journal must refuse independently witnessed later transaction");
	assert!(format!("{error:#}").contains("differs from restored database"));
	case.db.write(async |t| {
		t.execute("INSERT INTO expiry_carrier (txid,generation,signed_tx,backing_ids,fee_inputs,miner_fee_sat,
			funding_subsidy_sat,created_at,confirmed_height,confirmed_block) SELECT txid,generation,signed_tx,backing_ids,
			fee_inputs,miner_fee_sat,funding_subsidy_sat,created_at,confirmed_height,confirmed_block FROM direct_history_saved_carrier", &[]).await?;
		t.execute("INSERT INTO expiry_carrier_allocation (txid,allocation_id,vout) SELECT txid,allocation_id,vout FROM direct_history_saved_mapping", &[]).await?;
		Ok(())
	}).await.unwrap();
	load().await.unwrap();
	let sources = case.db.read(async |t| Ok(t.query("SELECT id,source FROM expiry_allocation WHERE freeze_set=$1",
		&[&case.recipe.freeze_set]).await?.iter().map(|r| (r.get::<_, String>("id"), r.get::<_, String>("source"))).collect::<Vec<_>>())).await.unwrap();
	case.db.write(async |t| {
		t.execute("UPDATE expiry_allocation SET source='changed-attribution' WHERE freeze_set=$1", &[&case.recipe.freeze_set]).await?;
		Ok(())
	}).await.unwrap();
	let error = load().await.err().expect("otherwise valid amounts cannot hide changed source attribution");
	assert!(format!("{error:#}").contains("differs from restored database"));
	case.db.write(async |t| {
		for (id, source) in &sources { t.execute("UPDATE expiry_allocation SET source=$2 WHERE id=$1", &[id,source]).await?; }
		Ok(())
	}).await.unwrap();
	load().await.unwrap();
}

#[tokio::test]
async fn direct_history_captaind_startup_refuses_forgotten_signed_version() {
	let case = crash_case("direct_history_captaind_startup_refuses_forgotten_signed_version", vec![30,40]).await;
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let root: ServerVtxo = ServerVtxo::deserialize(&case.recipe.backing[0]).unwrap();
	let history = server::payout_witness::DirectPayoutWitness::load(&config.data_dir,
		Network::Regtest, root.server_pubkey(), &case.db, budget()).await.unwrap();
	history.record_group(&case.db, &case.recipe.freeze_set).await.unwrap();
	let delivery = recovered(&case).await;
	confirm_once(&case, &delivery, &case.versions[1]).await;
	server::payout_witness::DirectPayoutWitness::load(&config.data_dir,
		Network::Regtest, root.server_pubkey(), &case.db, budget()).await.unwrap();
	let latest = case.versions[1].compute_txid().to_string();
	case.db.write(async |t| {
		t.execute("DELETE FROM expiry_carrier_allocation WHERE txid=$1", &[&latest]).await?;
		t.execute("DELETE FROM expiry_carrier WHERE txid=$1", &[&latest]).await?;
		Ok(())
	}).await.unwrap();
	let stderr = case.ctx.datadir.join("restored-captaind-stderr.log");
	let output = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&stderr).unwrap();
	let mut child = Command::new(std::env::var_os("CAPTAIND_EXEC").unwrap())
		.arg("start").arg("--config").arg(&case.recipe.config_path)
		.stdout(std::process::Stdio::null()).stderr(output).kill_on_drop(true).spawn().unwrap();
	let status = match tokio::time::timeout(Duration::from_secs(8), child.wait()).await {
		Ok(status) => status.unwrap(),
		Err(_) => {
			child.kill().await.unwrap();
			child.wait().await.unwrap();
			panic!("captaind accepted a database that forgot the independently witnessed confirmed carrier");
		},
	};
	assert!(!status.success());
	let error = std::fs::read_to_string(stderr).unwrap();
	assert!(error.contains("direct payout recovery witness validation failed"), "unexpected startup failure: {error}");
	assert!(error.contains("differs from restored database"), "unexpected history refusal: {error}");
}

#[tokio::test]
async fn direct_delivery_parent_crash_control_retains_unbroadcast_handoff() {
	let case = crash_case("direct_delivery_parent_crash_control_retains_unbroadcast_handoff", vec![30]).await;
	assert_eq!(case.versions.len(), 1);
	println!("parent crash control: signed transaction and complete mappings durable; exact carrier remains unbroadcast after real SIGKILL");
}

pub(super) async fn history_at(datadir: &Path, db: &Db, key: bitcoin::secp256k1::PublicKey) -> Arc<server::payout_witness::DirectPayoutWitness> {
	Arc::new(server::payout_witness::DirectPayoutWitness::load(datadir, Network::Regtest, key, db, budget()).await.unwrap())
}

async fn history_for(recipe: &CommitRecipe, db: &Db) -> Arc<server::payout_witness::DirectPayoutWitness> {
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&recipe.config_path).unwrap()).unwrap();
	let root: ServerVtxo = ServerVtxo::deserialize(&recipe.backing[0]).unwrap();
	history_at(&config.data_dir, db, root.server_pubkey()).await
}

async fn recovered(case: &CrashCase) -> DirectDelivery {
	DirectDelivery::recover_group(case.db.clone(), &case.wallet, &case.recipe.freeze_set,
		Network::Regtest, budget(), limits(), history_for(&case.recipe, &case.db).await).await.unwrap()
}

async fn confirm_once(case: &CrashCase, delivery: &DirectDelivery, winner: &bitcoin::Transaction) {
	let node = case.ctx.bitcoind().async_client();
	let txid = winner.compute_txid();
	assert_eq!(delivery.observe_and_submit(&node).await.unwrap(), DeliveryState::Pending { txid });
	let core = case.ctx.bitcoind().sync_client();
	assert_eq!(bitcoin::consensus::serialize(&core.get_raw_transaction(&txid, None).unwrap()), bitcoin::consensus::serialize(winner));
	case.ctx.generate_blocks(1).await;
	let state = delivery.observe_and_submit(&node).await.unwrap();
	assert!(matches!(state, DeliveryState::Confirmed { txid: id, .. } if id == txid));
	assert_eq!(delivery.observe_and_submit(&node).await.unwrap(), state);
	let rows = case.db.read(async |t| Ok(t.query("SELECT txid,confirmed_height FROM expiry_carrier ORDER BY generation", &[]).await?)).await.unwrap();
	assert_eq!(rows.len(), case.versions.len(), "reconciliation must preserve every signed version");
	assert_eq!(rows.iter().filter(|r| r.get::<_, Option<i32>>("confirmed_height").is_some()).count(), 1);
	let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
	// The standard restore also discovers the customer's pre-existing Board
	// funding change. It is an asset, not a second fallback principal payment.
	let version_ids = case.versions.iter().map(bitcoin::Transaction::compute_txid).collect::<BTreeSet<_>>();
	let payouts = outputs.iter().filter(|(p,_)| version_ids.contains(&p.txid)).collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1, "only one durable carrier version pays this obligation");
	assert_eq!(payouts[0].1, sat(19_000));
	assert_eq!(payouts[0].0.txid, txid);
	assert!(outputs.iter().filter(|(p,_)| !version_ids.contains(&p.txid))
		.all(|(p,_)| p.txid == case.board_funding_txid), "other restored assets must be the original Board funding change");
}

#[tokio::test]
async fn direct_delivery_recovers_exact_bytes_and_fee_guards_after_sigkill() {
	let case = crash_case("direct_delivery_recovers_exact_bytes_and_fee_guards_after_sigkill", vec![30]).await;
	let delivery = recovered(&case).await;
	assert!(!delivery.reserved_fee_inputs().is_empty());
	for point in delivery.reserved_fee_inputs() {
		assert!(case.wallet.lock_wallet_utxo(*point).is_err(), "all durable fee inputs must be reserved on recovery");
	}
	confirm_once(&case, &delivery, &case.versions[0]).await;
}

#[tokio::test]
async fn direct_delivery_replays_newer_durable_replacement_while_old_is_pending() {
	let case = crash_case("direct_delivery_replays_newer_durable_replacement_while_old_is_pending", vec![30,40]).await;
	let core = case.ctx.bitcoind().sync_client();
	core.send_raw_transaction(&case.versions[0]).unwrap();
	let delivery = recovered(&case).await;
	assert_eq!(delivery.reserved_fee_inputs().len(), 2, "both genuine fee reservations survive restart");
	confirm_once(&case, &delivery, &case.versions[1]).await;
}

#[tokio::test]
async fn direct_delivery_reconciles_old_confirmed_version_without_submitting_newer() {
	let case = crash_case("direct_delivery_reconciles_old_confirmed_version_without_submitting_newer", vec![30,40]).await;
	let core = case.ctx.bitcoind().sync_client();
	core.send_raw_transaction(&case.versions[0]).unwrap();
	case.ctx.generate_blocks(1).await;
	let delivery = recovered(&case).await;
	let state = delivery.observe_and_submit(&case.ctx.bitcoind().async_client()).await.unwrap();
	assert!(matches!(state, DeliveryState::Confirmed { txid, .. } if txid == case.versions[0].compute_txid()));
	assert!(!core.get_raw_mempool().unwrap().contains(&case.versions[1].compute_txid()));
	assert_eq!(delivery.observe_and_submit(&case.ctx.bitcoind().async_client()).await.unwrap(), state);
	let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
	let version_ids = case.versions.iter().map(bitcoin::Transaction::compute_txid).collect::<BTreeSet<_>>();
	let payouts = outputs.iter().filter(|(p,_)| version_ids.contains(&p.txid)).collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1);
	assert_eq!(payouts[0].1, sat(19_000));
	assert_eq!(payouts[0].0.txid, case.versions[0].compute_txid());
	assert!(outputs.iter().filter(|(p,_)| !version_ids.contains(&p.txid))
		.all(|(p,_)| p.txid == case.board_funding_txid));
}

#[tokio::test]
async fn direct_delivery_old_winner_releases_unused_replacement_fee_input_for_recycling() {
	let mut case = crash_case("direct_delivery_old_winner_releases_unused_replacement_fee_input_for_recycling", vec![30,40]).await;
	let core = case.ctx.bitcoind().sync_client();
	let old_inputs = case.versions[0].input.iter().map(|i| i.previous_output).collect::<BTreeSet<_>>();
	let unused = case.versions[1].input.iter().map(|i| i.previous_output)
		.filter(|p| !old_inputs.contains(p)).collect::<Vec<_>>();
	assert_eq!(unused.len(), 1, "the genuine replacement uses a distinct supplemental input");
	core.send_raw_transaction(&case.versions[0]).unwrap();
	case.ctx.generate_blocks(3).await;
	let delivery = recovered(&case).await;
	assert!(matches!(delivery.observe_and_submit(&case.ctx.bitcoind().async_client()).await.unwrap(),
		DeliveryState::Confirmed { txid, .. } if txid == case.versions[0].compute_txid()));
	assert!(case.wallet.lock_wallet_utxo(unused[0]).is_err());
	sync_fee_wallet(&mut case.wallet, &core);
	case.wallet.persist().await.unwrap();
	drop(delivery);
	let guard = case.wallet.lock_wallet_utxo(unused[0]).expect("reconciled process-local reservation releases");
	drop(guard);
	assert!(core.get_tx_out(&unused[0].txid, unused[0].vout, Some(true)).unwrap().is_some());
	assert!(case.wallet.list_unspent().any(|u| u.outpoint == unused[0]),
		"normal owning-wallet chain sync must restore the losing version's unused input");
	let destination = case.ctx.bitcoind().get_new_address().script_pubkey();
	let mut builder = case.wallet.build_tx();
	builder.add_utxo(unused[0]).unwrap().manually_selected_only().drain_to(destination).fee_rate(FeeRate::from_sat_per_vb(1).unwrap());
	let psbt = builder.finish().expect("recycled confirmed fee input is selectable");
	let recycled = case.wallet.finish_tx(psbt).unwrap();
	assert_eq!(recycled.input.iter().map(|i| i.previous_output).collect::<Vec<_>>(), unused);
	core.send_raw_transaction(&recycled).unwrap();
	case.ctx.generate_blocks(1).await;
	assert!(matches!(server::bitcoind::tx_status(&case.ctx.bitcoind().async_client(), recycled.compute_txid()).await.unwrap(),
		bitcoin_ext::TxStatus::Confirmed(_)));
	let versions = case.db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS rows FROM expiry_carrier", &[]).await?.get::<_, i64>("rows"))).await.unwrap();
	assert_eq!(versions, 2, "recycling must retain both signed carrier versions");
}

#[tokio::test]
async fn direct_delivery_replays_latest_generation_despite_reversed_wall_clock() {
	let case = crash_case("direct_delivery_replays_latest_generation_despite_reversed_wall_clock", vec![30,40]).await;
	case.db.write(async |t| {
		t.execute("UPDATE expiry_carrier SET created_at=TIMESTAMPTZ '2026-01-01 00:00:00+00' +
			((SELECT max(generation) FROM expiry_carrier)-generation)*INTERVAL '1 day'", &[]).await?;
		Ok(())
	}).await.unwrap();
	let generations = case.db.read(async |t| Ok(t.query("SELECT txid,generation FROM expiry_carrier ORDER BY generation", &[]).await?)).await.unwrap();
	assert!(generations[0].get::<_, i64>("generation") < generations[1].get::<_, i64>("generation"));
	case.ctx.bitcoind().sync_client().send_raw_transaction(&case.versions[0]).unwrap();
	let delivery = recovered(&case).await;
	confirm_once(&case, &delivery, &case.versions[1]).await;
}

#[tokio::test]
async fn direct_delivery_generation_migration_refuses_preexisting_journal() {
	let case = crash_case("direct_delivery_generation_migration_refuses_preexisting_journal", vec![30]).await;
	case.db.write(async |t| { t.execute("ALTER TABLE expiry_carrier DROP COLUMN generation", &[]).await?; Ok(()) }).await.unwrap();
	let refused = case.db.write(async |t| {
		t.batch_execute(include_str!("../../../server/src/database/migrations/V73__direct_carrier_generation.sql")).await?;
		Ok(())
	}).await.unwrap_err();
	assert!(format!("{refused:#}").contains("requires a fresh expiry_carrier journal"));
	let rows = case.db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS rows FROM expiry_carrier", &[]).await?.get::<_, i64>("rows"))).await.unwrap();
	assert_eq!(rows, 1, "refusal must preserve the signed historical handoff");
	assert!(case.ctx.bitcoind().sync_client().get_raw_mempool().unwrap().is_empty());
}

#[tokio::test]
async fn direct_delivery_stale_snapshot_refuses_before_any_submission_then_recovers_new_generation() {
	let mut case = crash_case("direct_delivery_stale_snapshot_refuses_before_any_submission_then_recovers_new_generation", vec![30]).await;
	let delivery = recovered(&case).await;
	let core = case.ctx.bitcoind().sync_client();
	let reserve = case.wallet.next_unused_address(KEYCHAIN).address;
	case.ctx.bitcoind().fund_addr(reserve, sat(900_000)).await;
	case.ctx.generate_blocks(3).await;
	sync_fee_wallet(&mut case.wallet, &core);
	case.wallet.persist().await.unwrap();
	// There remains one signer/writer. The old delivery object is a reader of
	// the earlier durable generation and holds its recovered fee reservation.
	let mut newer_recipe = case.recipe.clone();
	newer_recipe.fee_rates = vec![40];
	newer_recipe.committed_marker = case.ctx.datadir.join("newer-committed-before-broadcast");
	let newer_ids = run_commit_worker(&newer_recipe, case.ctx.datadir.join("newer-commit-recipe.json")).await;
	assert_eq!(newer_ids.len(), 1);
	let node = case.ctx.bitcoind().async_client();
	let error = delivery.observe_and_submit(&node).await.unwrap_err();
	assert!(error.to_string().contains("generation frontier changed"));
	assert!(!core.get_raw_mempool().unwrap().iter().any(|id| *id == case.versions[0].compute_txid() || *id == newer_ids[0]),
		"a stale snapshot must not submit either version");
	for point in delivery.reserved_fee_inputs() { assert!(case.wallet.lock_wallet_utxo(*point).is_err()); }
	drop(delivery);
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let mnemonic = wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let tip = server::bitcoind::get_block_by_height(&node, BlockHeight::new(core.get_block_count().unwrap() as u32)).await.unwrap();
	case.wallet = PersistedWallet::load_derive_from_master_xpriv(case.db.clone(), node,
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let bytes = case.db.read(async |t| Ok(t.query_one("SELECT signed_tx FROM expiry_carrier WHERE txid=$1", &[&newer_ids[0].to_string()]).await?.get::<_, Vec<u8>>("signed_tx"))).await.unwrap();
	case.versions.push(bitcoin::consensus::deserialize(&bytes).unwrap());
	let delivery = recovered(&case).await;
	assert_eq!(delivery.reserved_fee_inputs().len(), 2);
	confirm_once(&case, &delivery, &case.versions[1]).await;
}

#[tokio::test]
async fn direct_delivery_refuses_partial_mappings_and_current_fee_excess_before_submission() {
	let case = crash_case("direct_delivery_refuses_partial_mappings_and_current_fee_excess_before_submission", vec![30]).await;
	let tight = CarrierLimits { max_miner_fee: sat(1), ..limits() };
	let delivery = DirectDelivery::recover_group(case.db.clone(), &case.wallet, &case.recipe.freeze_set,
		Network::Regtest, budget(), tight, history_for(&case.recipe, &case.db).await).await.unwrap();
	let error = delivery.observe_and_submit(&case.ctx.bitcoind().async_client()).await.unwrap_err();
	assert!(error.to_string().contains("exceeds current delivery limits"));
	for point in delivery.reserved_fee_inputs() { assert!(case.wallet.lock_wallet_utxo(*point).is_err()); }
	drop(delivery);
	assert!(DirectDelivery::recover_group(case.db.clone(), &case.wallet, &case.recipe.freeze_set,
		Network::Regtest, BackingSnapshotLimits { max_serialized_bytes: 1, ..budget() }, limits(), history_for(&case.recipe, &case.db).await).await.is_err());
	let history = history_for(&case.recipe, &case.db).await;
	case.db.write(async |t| { t.execute("DELETE FROM expiry_carrier_allocation WHERE allocation_id=$1",
		&[&case.recipe.allocation_ids[0].to_string()]).await?; Ok(()) }).await.unwrap();
	assert!(DirectDelivery::recover_group(case.db.clone(), &case.wallet, &case.recipe.freeze_set,
		Network::Regtest, budget(), limits(), history).await.is_err(), "a partial allocation cannot earn replay credit");
	assert!(case.ctx.bitcoind().sync_client().get_raw_mempool().unwrap().is_empty());
}
