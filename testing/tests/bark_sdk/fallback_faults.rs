//! Faults around an expiry payout's durable commit: a real SIGKILL of captaind
//! at each side of it, a COMMIT whose reply is lost, a COMMIT that completes
//! after its process died, a physical base backup restored with archived WAL,
//! and a pending board's registration racing its payout.
//!
//! A payout has one database commit: coin states, settlement rows, wallet
//! metadata and the nursery's raw transaction. The rounds wallet then persists
//! its own copy of the spend, and the nursery broadcasts. Every fault is
//! injected from the fixture: a SIGKILL by PID, a TCP relay in front of
//! PostgreSQL, or a trigger that holds the payout on an advisory lock the test
//! owns. The server runs its ordinary code.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use ark::{ProtocolEncoding, Vtxo, VtxoPolicy};
use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark_testing::{Captaind, TestContext, btc, sat};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::captaind::{ArkClient, proxy::ArkRpcProxy};
use ark_testing::daemon::watchmand::WATCHMAND_CONFIG_FILE;
use ark_testing::ports::pick_port;
use bitcoin::{Address, Network, ScriptBuf, Transaction, Txid};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server_rpc::protos;

/// The advisory lock a fixture trigger waits on while the test holds it.
const PAUSE: i64 = 727064212;

/// An absent owner: its fallback destination and its expired coins.
struct Owner {
	spk: ScriptBuf,
	coins: Vec<Vtxo>,
	ids: Vec<String>,
}

impl Owner {
	fn new(spk: ScriptBuf, coins: Vec<Vtxo>) -> Owner {
		let ids = coins.iter().map(|v| v.id().to_string()).collect();
		Owner { spk, coins, ids }
	}
}

async fn server(name: &str) -> (TestContext, Arc<Captaind>, Db) {
	let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.min_board_amount = sat(330);
		}).watchmand().create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	(ctx, srv, db)
}

/// A funded board the server cosigned, never registered: its owner left
/// before the funding confirmed.
async fn pending_board(ctx: &TestContext, srv: &Captaind, name: &str, amount: u64) -> Owner {
	let wallet = ctx.bark_sdk(name, srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(amount + 1_000_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(amount)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	let coin = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
	Owner::new(wallet.fallback_destination().await.unwrap().spk, vec![coin])
}

/// Three absent owners whose coins expire together: a registered coin, a
/// pending board, and a pending board below the 10,000 sat payout minimum.
/// With that minimum, the first payout pays the first two and retains the third.
async fn absent_owners(ctx: &TestContext, srv: &Captaind) -> [Owner; 3] {
	let wallet = ctx.bark_sdk("registered", srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(30_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coin = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let registered = Owner::new(wallet.fallback_destination().await.unwrap().spk, vec![coin]);
	drop(wallet);
	let pending = pending_board(ctx, srv, "pending", 25_000).await;
	let retained = pending_board(ctx, srv, "retained", 7_000).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	[registered, pending, retained]
}

/// Expire every coin, confirm the real sweeps and train Core's estimator.
async fn expire(ctx: &TestContext, db: &Db, owners: &[Owner]) {
	let coins = owners.iter().flat_map(|o| o.coins.iter().cloned()).collect::<Vec<_>>();
	super::fallback_lightning::expire_and_confirm_sweeps(ctx, db, &coins).await;
	super::fallback_lightning::train_fee_estimator(ctx).await;
}

/// Restart with payouts enabled while the test holds `expiry_settlement`
/// exclusively, so the first tick waits at its candidate scan, before it
/// takes the rounds wallet. Captaind's start, which reads the rounds wallet,
/// cannot wait on a payout held by a fixture trigger. Released once started.
async fn restart_payouts_held(srv: &Captaind, db: &Db, min_payout_sat: u64) {
	let ready = Arc::new(Notify::new());
	let release = Arc::new(Notify::new());
	let holder = tokio::spawn({
		let (db, ready, release) = (db.clone(), ready.clone(), release.clone());
		async move {
			db.write(async |t| {
				t.batch_execute("LOCK TABLE expiry_settlement IN ACCESS EXCLUSIVE MODE").await?;
				ready.notify_one();
				release.notified().await;
				Ok(())
			}).await.unwrap();
		}
	});
	ready.notified().await;
	restart_payouts(srv, min_payout_sat).await;
	release.notify_one();
	holder.await.unwrap();
}

/// The backend of the operation waiting on [PAUSE].
async fn wait_paused(db: &Db) -> i32 {
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			if let Some(pid) = paused(db).await { break pid; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("the payout must reach the fixture trigger")
}

fn configure_payouts(srv: &Captaind, min_payout_sat: u64) {
	let watchman = srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE);
	let mut config = srv.config_mut();
	config.expiry_payout.enabled = true;
	config.expiry_payout.interval = Duration::from_secs(1);
	config.expiry_payout.grace_blocks = 0;
	config.expiry_payout.sweep_min_confs = 1;
	config.expiry_payout.min_payout_sat = min_payout_sat;
	config.expiry_payout.watchman_config = Some(watchman);
}

async fn restart_payouts(srv: &Captaind, min_payout_sat: u64) {
	srv.stop().await.unwrap();
	configure_payouts(srv, min_payout_sat);
	srv.start().await.unwrap();
}

/// Hold [PAUSE] from a test connection until `release` is notified.
async fn hold_pause(db: &Db) -> (Arc<Notify>, JoinHandle<()>) {
	let ready = Arc::new(Notify::new());
	let release = Arc::new(Notify::new());
	let (ready_task, release_task, db) = (ready.clone(), release.clone(), db.clone());
	let gate = tokio::spawn(async move {
		db.write(async |t| {
			t.query_one("SELECT pg_advisory_xact_lock($1)", &[&PAUSE]).await?;
			ready_task.notify_one();
			release_task.notified().await;
			Ok(())
		}).await.unwrap();
	});
	ready.notified().await;
	(release, gate)
}

/// The backend of the operation waiting on [PAUSE], if any.
async fn paused(db: &Db) -> Option<i32> {
	db.read(async |t| Ok(t.query_opt(
		"SELECT pid FROM pg_locks WHERE locktype='advisory' AND objid=$1::bigint::oid AND NOT granted
		 AND database=(SELECT oid FROM pg_database WHERE datname=current_database())", &[&PAUSE],
	).await?)).await.unwrap().map(|row| row.get(0))
}

async fn wait_backend_gone(db: &Db, pid: i32) {
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let alive = db.read(async |t| Ok(t.query_one(
				"SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid],
			).await?.get::<_, bool>(0))).await.unwrap();
			if !alive { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("the dead process's session must end");
}

async fn settlement_count(db: &Db) -> i64 {
	db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement", &[])
		.await?.get::<_, i64>(0))).await.unwrap()
}

/// Every coin of `owners` is settled exactly once, to its own owner's
/// destination. Every payout confirms, so none was double spent, and the
/// nursery holds no other payout. In each payout every destination has one
/// output, its gross less its fee share, and the shares add up to the fee
/// recorded for its recipients. Each destination holds exactly those outputs.
/// The payouts must settle only coins of `owners`.
async fn assert_paid_once(ctx: &TestContext, db: &Db, owners: &[&Owner]) -> BTreeSet<Txid> {
	let core = ctx.bitcoind().sync_client();
	let ids = owners.iter().flat_map(|o| o.ids.iter().cloned()).collect::<Vec<_>>();
	let rows = tokio::time::timeout(Duration::from_secs(120), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT id, txid, fee_sat, spk FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
			).await?)).await.unwrap();
			if rows.len() == ids.len() { break rows; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("every expired coin must be paid");
	let mut gross = BTreeMap::<Txid, BTreeMap<ScriptBuf, u64>>::new();
	let mut fees = BTreeMap::<Txid, u64>::new();
	for owner in owners {
		for (id, coin) in owner.ids.iter().zip(&owner.coins) {
			let row = rows.iter().find(|r| r.get::<_, String>("id") == *id).unwrap();
			assert_eq!(row.get::<_, Vec<u8>>("spk"), owner.spk.as_bytes(), "coin {id} paid to another destination");
			let txid: Txid = row.get::<_, String>("txid").parse().unwrap();
			*gross.entry(txid).or_default().entry(owner.spk.clone()).or_default() += coin.amount().to_sat();
			fees.insert(txid, row.get::<_, i64>("fee_sat") as u64);
		}
	}
	tokio::time::timeout(Duration::from_secs(120), async {
		loop {
			if fees.keys().all(|txid| core.get_raw_transaction_info(txid, None).ok()
				.and_then(|i| i.confirmations).unwrap_or(0) > 0) { break; }
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.generate_blocks(1).await;
		}
	}).await.expect("every payout must confirm");
	let txids = |sql: &'static str| async move {
		db.read(async |t| Ok(t.query(sql, &[]).await?)).await.unwrap().iter()
			.map(|r| r.get::<_, String>(0).parse::<Txid>().unwrap()).collect::<BTreeSet<_>>()
	};
	let payouts = txids("SELECT txid FROM nursery_tx WHERE kind::TEXT='expiry-payout'").await;
	assert_eq!(payouts, txids("SELECT DISTINCT txid FROM expiry_settlement").await,
		"the nursery holds exactly the settled payouts");
	assert_eq!(payouts, fees.keys().copied().collect(), "only these owners' coins are settled");
	let settled = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement", &[])
		.await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(settled as usize, ids.len(), "no other coin is settled");
	// A server that forgot a committed payout's spends builds a payout from
	// the same rounds coins, which Core refuses while the first is known.
	let logs = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
	assert!(!logs.contains("payout not accepted"), "a payout was built from spent rounds coins");
	let mut expected = BTreeMap::<ScriptBuf, Vec<(Txid, u64)>>::new();
	for (txid, destinations) in &gross {
		let tx: Transaction = core.get_raw_transaction(txid, None).unwrap();
		let mut deducted = 0;
		for (spk, gross) in destinations {
			let paid = tx.output.iter().filter(|o| &o.script_pubkey == spk).collect::<Vec<_>>();
			assert_eq!(paid.len(), 1, "one output per destination in {txid}");
			deducted += gross.checked_sub(paid[0].value.to_sat()).unwrap();
			expected.entry(spk.clone()).or_default().push((*txid, paid[0].value.to_sat()));
		}
		assert!(deducted > 0);
		assert_eq!(deducted, fees[txid], "fee shares of {txid} add up to its recipients' fee");
	}
	for owner in owners {
		let address = Address::from_script(&owner.spk, Network::Regtest).unwrap();
		let scan: serde_json::Value = core.call("scantxoutset", &[
			"start".into(), serde_json::json!([{"desc": format!("addr({address})")}]),
		]).unwrap();
		assert_eq!(scan["success"], true);
		let mut found = scan["unspents"].as_array().unwrap().iter().map(|u| (
			u["txid"].as_str().unwrap().parse::<Txid>().unwrap(),
			bitcoin::Amount::from_btc(u["amount"].as_f64().unwrap()).unwrap().to_sat(),
		)).collect::<Vec<_>>();
		found.sort();
		let mut want = expected.remove(&owner.spk).unwrap();
		want.sort();
		assert_eq!(found, want, "the destination holds exactly its payouts, once");
	}
	payouts
}

/// The common end of each fault: the first payout paid the registered coin
/// and the pending board, or was rolled back. Restart with a 1,000 sat
/// minimum, which makes the retained board payable at once. Its payout must
/// not spend the first payout's inputs again: the first one confirms.
async fn finish(ctx: &TestContext, srv: &Captaind, db: &Db, owners: &[Owner; 3], restart: bool) {
	if restart {
		restart_payouts(srv, 1_000).await;
	}
	let txids = assert_paid_once(ctx, db, &owners.iter().collect::<Vec<_>>()).await;
	println!("paid once: payouts={txids:?}");
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kill {
	/// Inside the payout's database transaction, settlement rows written.
	BeforeCommit,
	/// After the commit, while the rounds wallet persists its spend and
	/// before the nursery broadcasts.
	AfterCommit,
	/// After the broadcast, before any confirmation.
	AfterBroadcast,
}

#[tokio::test]
async fn fallback_payout_killed_before_commit() {
	Box::pin(killed_payout("fallback_payout_killed_before_commit", Kill::BeforeCommit)).await;
}

#[tokio::test]
async fn fallback_payout_killed_after_commit_before_broadcast() {
	Box::pin(killed_payout("fallback_payout_killed_after_commit_before_broadcast", Kill::AfterCommit)).await;
}

#[tokio::test]
async fn fallback_payout_killed_after_broadcast() {
	Box::pin(killed_payout("fallback_payout_killed_after_broadcast", Kill::AfterBroadcast)).await;
}

async fn killed_payout(name: &str, kill: Kill) {
	let (ctx, srv, db) = server(name).await;
	let owners = absent_owners(&ctx, &srv).await;
	expire(&ctx, &db, &owners).await;
	let trigger = match kill {
		Kill::BeforeCommit => Some("CREATE FUNCTION fault_pause() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN PERFORM pg_advisory_xact_lock(727064212); RETURN NULL; END $$;
			CREATE TRIGGER fault_pause AFTER INSERT ON expiry_settlement
			FOR EACH STATEMENT EXECUTE FUNCTION fault_pause();"),
		// The payout holds the rounds wallet from its build through this
		// persist, so the first rounds changeset after its commit is its own.
		Kill::AfterCommit => Some("CREATE FUNCTION fault_pause() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN
				IF NEW.kind::text='rounds' AND EXISTS (SELECT 1 FROM expiry_settlement) THEN
					PERFORM pg_advisory_xact_lock(727064212);
				END IF;
				RETURN NULL;
			END $$;
			CREATE TRIGGER fault_pause AFTER INSERT ON wallet_changeset
			FOR EACH ROW EXECUTE FUNCTION fault_pause();"),
		Kill::AfterBroadcast => None,
	};
	let gate = match trigger {
		Some(sql) => {
			db.write(async |t| { t.batch_execute(sql).await?; Ok(()) }).await.unwrap();
			let gate = hold_pause(&db).await;
			restart_payouts_held(&srv, &db, 10_000).await;
			Some(gate)
		},
		None => {
			restart_payouts(&srv, 10_000).await;
			None
		},
	};
	let core = ctx.bitcoind().sync_client();
	let first = [&owners[0], &owners[1]];
	let ids = first.iter().flat_map(|o| o.ids.iter().cloned()).collect::<Vec<_>>();
	let backend = match gate {
		Some(_) => Some(wait_paused(&db).await),
		None => {
			let txid: Txid = tokio::time::timeout(Duration::from_secs(90), async {
				loop {
					let row = db.read(async |t| Ok(t.query_opt(
						"SELECT txid FROM expiry_settlement WHERE id=ANY($1) LIMIT 1", &[&ids],
					).await?)).await.unwrap();
					if let Some(row) = row {
						let txid: Txid = row.get::<_, String>(0).parse().unwrap();
						if core.get_raw_mempool().unwrap().contains(&txid) { break txid; }
					}
					tokio::time::sleep(Duration::from_millis(100)).await;
				}
			}).await.expect("the payout must reach the mempool");
			println!("payout {txid} in the mempool, unconfirmed");
			None
		},
	};
	srv.kill().await.unwrap();
	println!("captaind killed: {kill:?}");
	if let (Some((release, gate)), Some(pid)) = (gate, backend) {
		// The dead process's session finishes the statement it was running,
		// then finds its connection closed and rolls back.
		release.notify_one();
		gate.await.unwrap();
		wait_backend_gone(&db, pid).await;
		db.write(async |t| {
			t.batch_execute("DROP TRIGGER IF EXISTS fault_pause ON wallet_changeset;
				DROP TRIGGER IF EXISTS fault_pause ON expiry_settlement;
				DROP FUNCTION fault_pause();").await?;
			Ok(())
		}).await.unwrap();
	}
	let settled = settlement_count(&db).await;
	let mempool = core.get_raw_mempool().unwrap();
	match kill {
		Kill::BeforeCommit => assert_eq!(settled, 0, "an uncommitted payout leaves no settlement"),
		Kill::AfterCommit | Kill::AfterBroadcast => assert_eq!(settled as usize, ids.len()),
	}
	if kill == Kill::AfterCommit {
		let txid: String = db.read(async |t| Ok(t.query_one(
			"SELECT txid FROM expiry_settlement LIMIT 1", &[],
		).await?.get(0))).await.unwrap();
		assert!(!mempool.contains(&txid.parse().unwrap()), "killed before the broadcast");
	}
	configure_payouts(&srv, 1_000);
	srv.start().await.unwrap();
	finish(&ctx, &srv, &db, &owners, false).await;
}

#[tokio::test]
async fn fallback_payout_late_commit_completes_across_restart() {
	Box::pin(late_commit("fallback_payout_late_commit_completes_across_restart", true)).await;
}

#[tokio::test]
async fn fallback_payout_late_commit_fails_across_restart() {
	Box::pin(late_commit("fallback_payout_late_commit_fails_across_restart", false)).await;
}

/// The payout's COMMIT is still running in PostgreSQL when captaind dies, and
/// the new process starts before it finishes. Startup must wait for it, then
/// see its outcome, whichever it is.
async fn late_commit(name: &str, completes: bool) {
	let (ctx, srv, db) = server(name).await;
	let owners = absent_owners(&ctx, &srv).await;
	expire(&ctx, &db, &owners).await;
	// A deferred constraint trigger runs inside COMMIT itself.
	db.write(async |t| {
		t.batch_execute("CREATE TABLE fault_commit_fails (id int);
			CREATE FUNCTION fault_late_commit() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN
				PERFORM pg_advisory_xact_lock(727064212);
				IF EXISTS (SELECT 1 FROM fault_commit_fails) THEN
					RAISE EXCEPTION 'test late commit fails';
				END IF;
				RETURN NULL;
			END $$;
			CREATE CONSTRAINT TRIGGER fault_late_commit AFTER INSERT ON expiry_settlement
			DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fault_late_commit();").await?;
		Ok(())
	}).await.unwrap();
	let (release, gate) = hold_pause(&db).await;
	restart_payouts_held(&srv, &db, 10_000).await;
	let pid = wait_paused(&db).await;
	let in_commit = db.read(async |t| Ok(t.query_one(
		"SELECT query FROM pg_stat_activity WHERE pid=$1", &[&pid],
	).await?.get::<_, String>(0))).await.unwrap();
	assert_eq!(in_commit, "COMMIT", "the payout is waiting inside its COMMIT");
	srv.kill().await.unwrap();
	assert_eq!(settlement_count(&db).await, 0, "the COMMIT has not completed");

	configure_payouts(&srv, 1_000);
	let starting = tokio::spawn({
		let srv = srv.clone();
		async move { srv.start().await }
	});
	tokio::time::timeout(Duration::from_secs(20), async {
		loop {
			let waiting = db.read(async |t| Ok(t.query_one(
				"SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database()
				 AND wait_event_type='Lock' AND query LIKE 'LOCK TABLE nursery_tx%')", &[],
			).await?.get::<_, bool>(0))).await.unwrap();
			if waiting { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("the new process must wait for the dead process's COMMIT");
	println!("restarted captaind waits for the late COMMIT");
	if !completes {
		db.write(async |t| { t.execute("INSERT INTO fault_commit_fails VALUES (1)", &[]).await?; Ok(()) })
			.await.unwrap();
	}
	release.notify_one();
	gate.await.unwrap();
	starting.await.unwrap().unwrap();
	wait_backend_gone(&db, pid).await;
	db.write(async |t| {
		t.batch_execute("DROP TRIGGER fault_late_commit ON expiry_settlement;
			DROP FUNCTION fault_late_commit(); DROP TABLE fault_commit_fails;").await?;
		Ok(())
	}).await.unwrap();
	// A completed late COMMIT paid the first two owners before the new
	// process started; it then pays the retained board alone. Otherwise the
	// new process finds all three payable in its first scan.
	finish(&ctx, &srv, &db, &owners, false).await;
	let txid = |owner: &Owner| {
		let (db, id) = (db.clone(), owner.ids[0].clone());
		async move { db.read(async |t| Ok(t.query_one(
			"SELECT txid FROM expiry_settlement WHERE id=$1", &[&id],
		).await?.get::<_, String>(0))).await.unwrap() }
	};
	let (registered, pending, retained) = (txid(&owners[0]).await, txid(&owners[1]).await, txid(&owners[2]).await);
	assert_eq!(registered, pending);
	assert_eq!(registered != retained, completes, "the late COMMIT outcome decides the first payout");
	println!("late COMMIT completed={completes}: first payout {registered}, retained board in {retained}");
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LostCommit {
	/// PostgreSQL commits; its reply never reaches captaind.
	Reply,
	/// The COMMIT never reaches PostgreSQL; the connection drops.
	Request,
	/// PostgreSQL commits, its reply is lost, and the database then stops
	/// answering: captaind cannot learn the outcome and exits.
	ReplyThenOutage,
}

#[tokio::test]
async fn fallback_payout_commit_reply_lost() {
	Box::pin(lost_commit("fallback_payout_commit_reply_lost", LostCommit::Reply)).await;
}

#[tokio::test]
async fn fallback_payout_commit_request_lost() {
	Box::pin(lost_commit("fallback_payout_commit_request_lost", LostCommit::Request)).await;
}

#[tokio::test]
async fn fallback_payout_commit_reply_lost_then_database_outage() {
	Box::pin(lost_commit("fallback_payout_commit_reply_lost_then_database_outage", LostCommit::ReplyThenOutage)).await;
}

/// The simple-protocol COMMIT tokio-postgres sends, and PostgreSQL's reply.
const COMMIT_QUERY: &[u8] = b"Q\0\0\0\x0bCOMMIT\0";
const COMMIT_DONE: &[u8] = b"C\0\0\0\x0bCOMMIT\0";
const MARKER: &[u8] = b"fault-payout-marker";

struct PgRelay {
	port: u16,
	armed: AtomicBool,
	outage: AtomicBool,
	closing: Notify,
	fired: Notify,
	mode: LostCommit,
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
	haystack.windows(needle.len()).any(|w| w == needle)
}

/// A TCP relay in front of PostgreSQL. A connection whose server sends the
/// fixture trigger's notice is the payout's; its next COMMIT is lost once.
async fn pg_relay(upstream: u16, mode: LostCommit) -> Arc<PgRelay> {
	let port = pick_port();
	let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
	let relay = Arc::new(PgRelay {
		port, armed: AtomicBool::new(true), outage: AtomicBool::new(false),
		closing: Notify::new(), fired: Notify::new(), mode,
	});
	let shared = relay.clone();
	tokio::spawn(async move {
		loop {
			let Ok((client, _)) = listener.accept().await else { return };
			if shared.outage.load(Ordering::SeqCst) { continue; }
			let Ok(server) = TcpStream::connect(("127.0.0.1", upstream)).await else { continue };
			tokio::spawn(relay_connection(client, server, shared.clone()));
		}
	});
	relay
}

async fn relay_connection(mut client: TcpStream, mut server: TcpStream, relay: Arc<PgRelay>) {
	let (mut client_rx, mut client_tx) = client.split();
	let (mut server_rx, mut server_tx) = server.split();
	let (mut up, mut down) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
	let (mut up_tail, mut down_tail) = (Vec::new(), Vec::new());
	let (mut payout, mut commit_sent) = (false, false);
	loop {
		if relay.outage.load(Ordering::SeqCst) { return; }
		tokio::select! {
			n = client_rx.read(&mut up) => {
				let n = match n { Ok(0) | Err(_) => return, Ok(n) => n };
				let window = [up_tail.as_slice(), &up[..n]].concat();
				if payout && contains(&window, COMMIT_QUERY) && relay.armed.swap(false, Ordering::SeqCst) {
					if relay.mode == LostCommit::Request {
						relay.fired.notify_one();
						return;
					}
					commit_sent = true;
				}
				if server_tx.write_all(&up[..n]).await.is_err() { return; }
				up_tail = window[window.len().saturating_sub(64)..].to_vec();
			},
			n = server_rx.read(&mut down) => {
				let n = match n { Ok(0) | Err(_) => return, Ok(n) => n };
				let window = [down_tail.as_slice(), &down[..n]].concat();
				payout |= contains(&window, MARKER);
				if commit_sent && contains(&window, COMMIT_DONE) {
					if relay.mode == LostCommit::ReplyThenOutage {
						relay.outage.store(true, Ordering::SeqCst);
						relay.closing.notify_waiters();
					}
					relay.fired.notify_one();
					return;
				}
				if client_tx.write_all(&down[..n]).await.is_err() { return; }
				down_tail = window[window.len().saturating_sub(64)..].to_vec();
			},
			_ = relay.closing.notified() => return,
		}
	}
}

async fn lost_commit(name: &str, mode: LostCommit) {
	let (ctx, srv, db) = server(name).await;
	let owners = absent_owners(&ctx, &srv).await;
	expire(&ctx, &db, &owners).await;
	db.write(async |t| {
		t.batch_execute("CREATE FUNCTION fault_marker() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN RAISE NOTICE 'fault-payout-marker'; RETURN NULL; END $$;
			CREATE TRIGGER fault_marker AFTER INSERT ON expiry_settlement
			FOR EACH STATEMENT EXECUTE FUNCTION fault_marker();").await?;
		Ok(())
	}).await.unwrap();
	let upstream = srv.config().postgres.port;
	let relay = pg_relay(upstream, mode).await;
	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.postgres.host = "127.0.0.1".into();
		config.postgres.port = relay.port;
	}
	configure_payouts(&srv, 10_000);
	srv.start().await.unwrap();
	tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			tokio::select! {
				_ = relay.fired.notified() => break,
				_ = tokio::time::sleep(Duration::from_secs(2)) => { ctx.bitcoind().generate(1).await; },
			}
		}
	}).await.expect("the payout's COMMIT must reach the relay");
	println!("payout COMMIT lost: {mode:?}");
	let first = [&owners[0], &owners[1]];
	let logs = || std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
	match mode {
		LostCommit::Reply => {
			// The outcome probe finds the commit and the payout carries on.
			let txids = assert_paid_once(&ctx, &db, &first).await;
			assert_eq!(txids.len(), 1);
			assert!(!logs().contains("expiry payout tick failed"), "the committed payout carried on");
		},
		LostCommit::Request => {
			// The probe finds no commit; a later tick pays once.
			let txids = assert_paid_once(&ctx, &db, &first).await;
			assert_eq!(txids.len(), 1);
			assert!(logs().contains("expiry payout tick failed"), "the rolled back payout failed its tick");
		},
		LostCommit::ReplyThenOutage => {
			let status = tokio::time::timeout(Duration::from_secs(90), srv.wait_exit()).await
				.expect("captaind must exit when the COMMIT outcome is unknown").unwrap();
			assert_eq!(status.code(), Some(1));
			assert!(logs().contains("expiry commit outcome unknown; exiting"));
			assert_eq!(settlement_count(&db).await, 2, "the commit itself succeeded");
			relay.outage.store(false, Ordering::SeqCst);
			srv.start().await.unwrap();
			let txids = assert_paid_once(&ctx, &db, &first).await;
			assert_eq!(txids.len(), 1);
		},
	}
	finish(&ctx, &srv, &db, &owners, true).await;
}

/// The postgres binary directory the harness uses.
fn pg_bin(name: &str) -> String {
	format!("{}/{name}", std::env::var("POSTGRES_BINS").expect("POSTGRES_BINS"))
}

async fn run(cmd: &mut Command) {
	let out = cmd.output().await.unwrap();
	assert!(out.status.success(), "{cmd:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Every row of `table`, as one digest, for an exact comparison.
async fn table_digest(db: &Db, table: &str) -> (i64, String) {
	let row = db.read(async |t| Ok(t.query_one(&format!(
		"SELECT count(*), coalesce(md5(string_agg(r::text, '|' ORDER BY r::text)), '') FROM {table} r"), &[],
	).await?)).await.unwrap();
	(row.get(0), row.get(1))
}

const RESTORED_TABLES: [&str; 9] = ["fallback_record", "key_link", "expiry_settlement", "pending_board",
	"nursery_tx", "vtxo", "htlc_vtxo", "wallet_changeset", "expiry_cancelled_participation"];

/// A physical base backup taken before any owner existed, plus continuously
/// archived WAL, restored to a new cluster after a payout: wallet records,
/// key links, paid scripts, pending boards and the nursery's raw payout come
/// back exactly. The restored server does not pay anyone again, keeps the
/// retained board's obligation, and pays it once to its own destination,
/// without spending the unconfirmed first payout's inputs again.
#[tokio::test]
async fn fallback_physical_backup_wal_restore() {
	let (ctx, srv, db) = server("fallback_physical_backup_wal_restore").await;
	let pg_port = srv.config().postgres.port;
	let dbname = srv.config().postgres.name.clone();
	let dir = ctx.datadir.join("pitr");
	let (base, archive, restored) = (dir.join("base"), dir.join("wal"), dir.join("restored"));
	std::fs::create_dir_all(&archive).unwrap();
	db.write(async |t| {
		t.execute("SELECT pg_create_physical_replication_slot('fault_pitr', true)", &[]).await?;
		Ok(())
	}).await.unwrap();
	let mut receiver: Child = Command::new(pg_bin("pg_receivewal"))
		.args(["-h", "127.0.0.1", "-p", &pg_port.to_string(), "-D", archive.to_str().unwrap(),
			"--slot", "fault_pitr", "--synchronous", "-n"])
		.kill_on_drop(true).spawn().unwrap();
	run(Command::new(pg_bin("pg_basebackup"))
		.args(["-h", "127.0.0.1", "-p", &pg_port.to_string(), "-D", base.to_str().unwrap(),
			"-X", "none", "--checkpoint=fast"])).await;
	let backup_lsn: String = db.read(async |t| Ok(t.query_one(
		"SELECT pg_current_wal_lsn()::text", &[]).await?.get(0))).await.unwrap();
	assert_eq!(table_digest(&db, "fallback_record").await.0, 0, "the backup predates every owner");
	println!("base backup taken before any owner; WAL streamed from LSN {backup_lsn}");

	let owners = absent_owners(&ctx, &srv).await;
	expire(&ctx, &db, &owners).await;
	restart_payouts(&srv, 10_000).await;
	let core = ctx.bitcoind().sync_client();
	let first_ids = owners[..2].iter().flat_map(|o| o.ids.iter().cloned()).collect::<Vec<_>>();
	let txid: Txid = tokio::time::timeout(Duration::from_secs(90), async {
		loop {
			let rows = db.read(async |t| Ok(t.query(
				"SELECT txid FROM expiry_settlement WHERE id=ANY($1)", &[&first_ids],
			).await?)).await.unwrap();
			if rows.len() == first_ids.len() {
				let txid: Txid = rows[0].get::<_, String>(0).parse().unwrap();
				if core.get_raw_mempool().unwrap().contains(&txid) { break txid; }
			}
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	}).await.expect("the first payout must reach the mempool");
	let raw = db.read(async |t| t.get_nursery_raw_tx(txid).await).await.unwrap().unwrap();

	// Disaster: the server dies with its payout unconfirmed. The last WAL
	// segment is closed and archived before the old cluster is set aside.
	srv.kill().await.unwrap();
	srv.watchmand().stop().await.unwrap();
	let mut source = BTreeMap::new();
	for table in RESTORED_TABLES { source.insert(table, table_digest(&db, table).await); }
	let segment: String = db.write(async |t| {
		let name = t.query_one("SELECT pg_walfile_name(pg_current_wal_lsn())", &[]).await?.get(0);
		t.execute("SELECT pg_switch_wal()", &[]).await?;
		Ok(name)
	}).await.unwrap();
	tokio::time::timeout(Duration::from_secs(60), async {
		while !archive.join(&segment).exists() { tokio::time::sleep(Duration::from_millis(200)).await; }
	}).await.expect("the last WAL segment must be archived");
	receiver.kill().await.unwrap();
	println!("archived WAL through segment {segment}");

	run(Command::new("cp").args(["-a", base.to_str().unwrap(), restored.to_str().unwrap()])).await;
	let auto = restored.join("postgresql.auto.conf");
	let mut conf = std::fs::read_to_string(&auto).unwrap_or_default();
	conf.push_str(&format!("restore_command = 'cp {}/%f %p'\n", archive.display()));
	std::fs::write(&auto, conf).unwrap();
	std::fs::write(restored.join("recovery.signal"), "").unwrap();
	let restored_port = pick_port();
	let _restored_pg: Child = Command::new(pg_bin("postgres"))
		.args(["-D", restored.to_str().unwrap(), "-p", &restored_port.to_string(),
			"-k", "/tmp/ark-testing-postgres-locks", "-c", "listen_addresses=127.0.0.1"])
		.stdout(std::fs::File::create(dir.join("restored.log")).unwrap())
		.stderr(std::fs::File::create(dir.join("restored.err")).unwrap())
		.kill_on_drop(true).spawn().unwrap();
	let mut restored_config = srv.config().postgres.clone();
	restored_config.host = "127.0.0.1".into();
	restored_config.port = restored_port;
	let restored_db = tokio::time::timeout(Duration::from_secs(120), async {
		loop {
			if let Ok(db) = Db::connect(&restored_config).await {
				let promoted = db.read(async |t| Ok(t.query_one("SELECT NOT pg_is_in_recovery()", &[])
					.await?.get::<_, bool>(0))).await;
				if promoted.unwrap_or(false) { break db; }
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
	}).await.expect("the restored cluster must replay the archived WAL and promote");
	assert_eq!(restored_config.name, dbname);
	for table in RESTORED_TABLES {
		assert_eq!(table_digest(&restored_db, table).await, source[table], "{table} restored exactly");
	}
	assert_eq!(restored_db.read(async |t| t.get_nursery_raw_tx(txid).await).await.unwrap().unwrap(), raw);
	println!("physical restore matches the old cluster: {RESTORED_TABLES:?}");

	// The old cluster is no longer used by any server process.
	{
		let mut watchman = srv.watchmand().config_mut();
		watchman.postgres.host = "127.0.0.1".into();
		watchman.postgres.port = restored_port;
	}
	{
		let mut config = srv.config_mut();
		config.postgres.host = "127.0.0.1".into();
		config.postgres.port = restored_port;
	}
	srv.watchmand().start().await.unwrap();
	configure_payouts(&srv, 1_000);
	srv.start().await.unwrap();
	let txids = assert_paid_once(&ctx, &restored_db, &owners.iter().collect::<Vec<_>>()).await;
	assert!(txids.contains(&txid), "the first payout is the one that confirms");
	let paid_board = owners[1].coins[0].serialize();
	assert!(srv.get_public_rpc().await.register_board_vtxo(protos::BoardVtxoRequest {
		board_vtxo: paid_board,
	}).await.is_err(), "a paid board cannot be registered after the restore");
	println!("after the restore: payouts={txids:?}");
}

/// Register a pending board through the public RPC.
async fn register_board(srv: &Captaind, board: &Vtxo) -> JoinHandle<Result<(), tonic::Status>> {
	let mut rpc = srv.get_public_rpc().await;
	let board_vtxo = board.serialize();
	tokio::spawn(async move {
		rpc.register_board_vtxo(protos::BoardVtxoRequest { board_vtxo }).await.map(|_| ())
	})
}

/// The number of sessions waiting on a lock while running `query`.
async fn lock_waiters(db: &Db, query: &str) -> i64 {
	db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM pg_stat_activity WHERE datname=current_database()
		 AND wait_event_type='Lock' AND query LIKE $1", &[&query],
	).await?.get::<_, i64>(0))).await.unwrap()
}

const REGISTRATION_LOCK: &str = "SELECT vtxo_id FROM pending_board WHERE id=%FOR UPDATE";
const PAYOUT_LOCK: &str = "SELECT id FROM pending_board WHERE vtxo_id=ANY%FOR UPDATE";

/// A registration that checked the chain before the sweep holds the pending
/// row when the payout arrives. The registration commits; the payout finds
/// the coin's source changed, rolls back, and pays the now registered coin
/// once to the same owner.
#[tokio::test]
async fn fallback_pending_board_registration_wins_payout_race() {
	let (ctx, srv, db) = server("fallback_pending_board_registration_wins_payout_race").await;
	let owner = pending_board(&ctx, &srv, "pending", 25_000).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let board = owner.coins[0].clone();
	super::fallback_lightning::train_fee_estimator(&ctx).await;
	restart_payouts(&srv, 10_000).await;

	// The test holds the pending row, so the registration, past its chain
	// checks, waits on it with the anchor still unspent.
	let ready = Arc::new(Notify::new());
	let release = Arc::new(Notify::new());
	let holder = tokio::spawn({
		let (db, ready, release, id) = (db.clone(), ready.clone(), release.clone(), owner.ids[0].clone());
		async move {
			db.write(async |t| {
				t.query_one("SELECT id FROM pending_board WHERE vtxo_id=$1 FOR UPDATE", &[&id]).await?;
				ready.notify_one();
				release.notified().await;
				Ok(())
			}).await.unwrap();
		}
	});
	ready.notified().await;
	let registration = register_board(&srv, &board).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		while lock_waiters(&db, REGISTRATION_LOCK).await < 1 { tokio::time::sleep(Duration::from_millis(100)).await; }
	}).await.expect("the registration must wait on the pending row");
	println!("registration past its chain checks, waiting on the pending row");

	// Expire the board; watchman sweeps its anchor. The payout then queues
	// on the same row. Mine without waiting for captaind: a waiting payout
	// holds the rounds wallet, which captaind's block sync needs.
	let tip = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(board.expiry_height().to_u32().saturating_sub(tip + 1)).await;
	tokio::time::timeout(Duration::from_secs(120), async {
		while lock_waiters(&db, PAYOUT_LOCK).await < 1 {
			tokio::time::sleep(Duration::from_secs(1)).await;
			ctx.bitcoind().generate(1).await;
		}
	}).await.expect("the payout must wait on the pending row behind the registration");
	println!("payout waiting on the pending row behind the registration");
	release.notify_one();
	holder.await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), registration).await
		.expect("the registration must finish").unwrap()
		.expect("the registration holding the row first wins");
	let txids = assert_paid_once(&ctx, &db, &[&owner]).await;
	assert_eq!(txids.len(), 1);
	let state: String = db.read(async |t| Ok(t.query_one(
		"SELECT spend_state::text FROM vtxo WHERE vtxo_id=$1", &[&owner.ids[0]],
	).await?.get(0))).await.unwrap();
	assert_eq!(state, "spent");
	let logs = std::fs::read_to_string(ctx.datadir.join("server/stdout.log")).unwrap();
	assert!(logs.contains("expiry inputs changed"), "the payout lost the race and rolled back");
	println!("registration won; registered board paid once in {txids:?}");
}

/// The payout holds the pending row, its settlement written but not
/// committed. A registration now finds the anchor spent; after the commit
/// it is refused again. The board is paid once.
#[tokio::test]
async fn fallback_pending_board_payout_wins_registration_race() {
	let (ctx, srv, db) = server("fallback_pending_board_payout_wins_registration_race").await;
	let owner = pending_board(&ctx, &srv, "pending", 25_000).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let board = owner.coins[0].clone();
	expire(&ctx, &db, std::slice::from_ref(&owner)).await;
	db.write(async |t| {
		t.batch_execute("CREATE FUNCTION fault_pause() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN PERFORM pg_advisory_xact_lock(727064212); RETURN NULL; END $$;
			CREATE TRIGGER fault_pause AFTER INSERT ON expiry_settlement
			FOR EACH STATEMENT EXECUTE FUNCTION fault_pause();").await?;
		Ok(())
	}).await.unwrap();
	let (release, gate) = hold_pause(&db).await;
	restart_payouts_held(&srv, &db, 10_000).await;
	wait_paused(&db).await;
	let err = tokio::time::timeout(Duration::from_secs(30), register_board(&srv, &board).await).await
		.expect("the registration must not wait on the held payout").unwrap().unwrap_err();
	println!("registration during the payout commit refused: {}", err.message());
	release.notify_one();
	gate.await.unwrap();
	let txids = assert_paid_once(&ctx, &db, &[&owner]).await;
	assert_eq!(txids.len(), 1);
	let err = register_board(&srv, &board).await.await.unwrap().unwrap_err();
	println!("registration after the payout refused: {}", err.message());
	db.write(async |t| {
		t.batch_execute("DROP TRIGGER fault_pause ON expiry_settlement; DROP FUNCTION fault_pause();").await?;
		Ok(())
	}).await.unwrap();
}

/// One arkoor package spends coins of two independent wallets, X and Y, to
/// a recipient R, with change to a new key of Y. Only the output funded by
/// X's coin is registered. Each output belongs to its actual parent's owner
/// unless registered: R gets X's part, Y gets its own part and its change,
/// X gets nothing.
#[tokio::test]
async fn fallback_unregistered_arkoor_independent_owners_partial_registration() {
	let (ctx, srv, db) = server("fallback_unregistered_arkoor_independent_owners_partial_registration").await;
	let mut wallets = Vec::new();
	for (name, amount) in [("x", 25_000), ("y", 30_000)] {
		let wallet = ctx.bark_sdk(name, &srv).cfg(|c| c.daemon_manual_sync = true)
			.boarded(sat(amount)).create().await;
		wallet.stop_daemon_wait().await.unwrap();
		wallets.push(wallet);
	}
	let (x, y) = (&wallets[0], &wallets[1]);
	let recipient = ctx.bark_sdk("r", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	let (r_spk, x_spk, y_spk) = (
		recipient.fallback_destination().await.unwrap().spk,
		x.fallback_destination().await.unwrap().spk,
		y.fallback_destination().await.unwrap().spk,
	);
	assert!(r_spk != x_spk && r_spk != y_spk && x_spk != y_spk);

	let mut inputs = Vec::new();
	let mut keys = Vec::new();
	for wallet in [x, y] {
		let coin = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
		keys.push(wallet.pubkey_keypair(&coin.user_pubkey()).await.unwrap().unwrap().1);
		inputs.push(coin);
	}
	let (y_change, _) = y.derive_store_next_keypair().await.unwrap();
	let builder = ArkoorPackageBuilder::new_with_checkpoints(inputs.clone(), vec![
		ArkoorDestination { total_amount: sat(40_000), policy: address.policy().clone() },
		ArkoorDestination { total_amount: sat(15_000), policy: VtxoPolicy::new_pubkey(y_change.public_key()) },
	]).unwrap().generate_user_nonces(&keys).unwrap();
	let response = srv.get_public_rpc().await.request_arkoor_cosign(
		protos::ArkoorPackageCosignRequest::from(builder.cosign_request()),
	).await.unwrap().into_inner();
	let outputs = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	// Each output extends exactly one input's chain.
	let funded_by = |input: &Vtxo| outputs.iter().filter(|o| o.chain_anchor() == input.chain_anchor())
		.cloned().collect::<Vec<_>>();
	let (from_x, from_y) = (funded_by(&inputs[0]), funded_by(&inputs[1]));
	assert_eq!(from_x.len() + from_y.len(), outputs.len());
	assert_eq!(from_x.iter().map(|v| v.amount()).collect::<Vec<_>>(), vec![sat(25_000)]);
	assert!(from_x.iter().all(|v| v.policy() == address.policy()));
	assert_eq!(from_y.iter().map(|v| v.amount().to_sat()).sum::<u64>(), 30_000);
	assert_eq!(from_y.len(), 2, "R's remaining 15,000 sat and Y's 15,000 sat change");

	srv.get_public_rpc().await.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
		vtxos: from_x.iter().map(|v| v.serialize()).collect(),
	}).await.unwrap();
	let ids = |coins: &[Vtxo]| coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let states = db.read(async |t| Ok(t.query(
		"SELECT vtxo_id, spend_state::text FROM vtxo WHERE vtxo_id=ANY($1)", &[&ids(&outputs)],
	).await?)).await.unwrap().iter().map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
		.collect::<BTreeMap<_, _>>();
	for coin in &from_x { assert_eq!(states[&coin.id().to_string()], "spendable"); }
	for coin in &from_y { assert_eq!(states[&coin.id().to_string()], "unregistered"); }
	drop(wallets);
	drop(recipient);

	let owners = [Owner::new(r_spk, from_x), Owner::new(y_spk, from_y)];
	expire(&ctx, &db, &owners).await;
	restart_payouts(&srv, 10_000).await;
	let txids = assert_paid_once(&ctx, &db, &[&owners[0], &owners[1]]).await;
	let core = ctx.bitcoind().sync_client();
	let x_address = Address::from_script(&x_spk, Network::Regtest).unwrap();
	let scan: serde_json::Value = core.call("scantxoutset", &[
		"start".into(), serde_json::json!([{"desc": format!("addr({x_address})")}]),
	]).unwrap();
	assert_eq!(scan["success"], true);
	assert!(scan["unspents"].as_array().unwrap().is_empty(), "X's coin went to R: X is paid nothing");
	println!("independent input owners: R paid X's part, Y paid its part and change, X nothing: {txids:?}");
}

/// A server the client cannot register its board with.
#[derive(Clone)]
struct RefuseBoardRegistration;

#[async_trait::async_trait]
impl ArkRpcProxy for RefuseBoardRegistration {
	async fn register_board_vtxo(
		&self, _upstream: &mut ArkClient, _request: protos::BoardVtxoRequest,
	) -> Result<protos::Empty, tonic::Status> {
		Err(tonic::Status::unavailable("test refuses board registration"))
	}
}

/// The owner of a pending board returns just before it expires, cannot
/// register it, and exits it unilaterally, as upstream does. The exit
/// confirms before the expiry, so no sweep can spend the anchor. The owner
/// claims its exit; the payout task, running with a real fee estimate
/// throughout, never pays the board.
#[tokio::test]
async fn fallback_pending_board_exit_before_expiry_is_not_paid() {
	let (ctx, srv, db) = server("fallback_pending_board_exit_before_expiry_is_not_paid").await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("pending", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).funded(sat(1_025_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(25_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	let coin = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
	let owner = Owner::new(wallet.fallback_destination().await.unwrap().spk, vec![coin.clone()]);
	let mut config = wallet.config().clone();
	drop(wallet);
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	super::fallback_lightning::train_fee_estimator(&ctx).await;
	restart_payouts(&srv, 10_000).await;

	let expiry = coin.expiry_height().to_u32();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(expiry - 10 - tip).await;
	let proxy = srv.start_proxy_no_mailbox(RefuseBoardRegistration).await;
	config.server_address = proxy.address.clone();
	let wallet = bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("pending")), run_daemon: false, ..Default::default() },
	).await.unwrap();
	wallet.sync_onchain().await.unwrap();
	wallet.sync_pending_boards().await.unwrap();
	assert!(wallet.exit_mgr().is_exiting(coin.id()).await, "an unregistrable board near expiry is exited");
	let core = ctx.bitcoind().sync_client();
	tokio::time::timeout(Duration::from_secs(300), async {
		loop {
			wallet.sync_onchain().await.unwrap();
			let _ = wallet.progress_exits().await;
			if !wallet.exit_mgr().list_claimable().await.is_empty() { break; }
			ctx.generate_blocks(1).await;
		}
	}).await.expect("the exit must become claimable");
	let exit_txid = coin.point().txid;
	// The exit tx has a P2A output, which this RPC client cannot decode.
	let exit_info: serde_json::Value = core.call("getrawtransaction", &[exit_txid.to_string().into(), 1.into()]).unwrap();
	let exit_block: bitcoin::BlockHash = exit_info["blockhash"].as_str().unwrap().parse().unwrap();
	let exit_height = core.get_block_header_info(&exit_block).unwrap().height as u32;
	assert!(exit_height < expiry, "the exit confirmed at {exit_height}, before the expiry at {expiry}");
	let destination = ctx.bitcoind().get_new_address();
	let claimable = wallet.exit_mgr().list_claimable().await;
	let claim = wallet.exit_mgr().drain_exits(&claimable, &wallet, destination.clone(), None).await.unwrap()
		.extract_tx().unwrap();
	core.send_raw_transaction(&claim).unwrap();
	ctx.generate_blocks(1).await;
	let claimed = ctx.bitcoind().get_received_by_address(&destination);
	assert!(claimed > sat(20_000) && claimed < sat(25_000), "the owner claimed its exit: {claimed}");
	drop(wallet);

	// Past the expiry, the anchor was never swept: the board waits forever
	// and is never paid.
	ctx.generate_blocks(5).await;
	super::fallback_lightning::assert_no_payout(&ctx, &db, &owner.ids, &owner.spk).await;
	println!("exited pending board: exit {exit_txid} at {exit_height} < expiry {expiry}, claimed {claimed}, no payout");
}
