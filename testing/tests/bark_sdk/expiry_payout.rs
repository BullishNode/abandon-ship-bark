use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bitcoin_ext::BlockDelta;

use ark::VtxoId;
use ark_testing::{TestContext, btc, sat};
use ark_testing::constants::ROUND_CONFIRMATIONS;
use ark_testing::daemon::captaind::{self, ArkClient, Captaind};
use bark::expiry_payout::{self, AdoptedVtxoState, AdoptedVtxoStatus};
use bark::movement::MovementStatus;
use bark::vtxo::VtxoStateKind;
use server::database::Db;
use server_rpc::protos;
use tokio::sync::Notify;

/// Mark `vtxo` spent in the server's database, like an operator that paid it
/// out on-chain does.
async fn server_marks_spent(srv: &Captaind, vtxo: VtxoId) {
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let n = db.write(async |t| {
		Ok(t.execute(
			"UPDATE vtxo SET spend_state = 'spent', updated_at = NOW() WHERE vtxo_id = $1",
			&[&vtxo.to_string()],
		).await?)
	}).await.unwrap();
	assert_eq!(n, 1, "server should know vtxo {vtxo}");
}

/// The server pays an expired VTXO out on-chain and marks it spent. The
/// wallet, which was never told:
/// - still refreshes its other VTXOs, dropping the paid one;
/// - adopts the spent state once, so the VTXO leaves the balance.
#[tokio::test]
async fn expiry_payout_adopt() {
	let ctx = TestContext::new("bark_sdk/expiry_payout_adopt").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1))
		.cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(32);
			// Refresh skips the initial replayed round and joins the next one.
			c.round_interval = Duration::from_secs(10);
		})
		.create().await;

	let wallet = ctx.bark_sdk("bark", &srv)
		.cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(300_000))
		.boarded(sat(400_000))
		.create().await;

	let vtxos = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(vtxos.len(), 2);
	let paid = vtxos.iter().min_by_key(|v| v.amount()).unwrap().vtxo.clone();
	let kept = vtxos.iter().max_by_key(|v| v.amount()).unwrap().id();

	// The server settles `paid`. An explicit refresh of both must still
	// refresh `kept`.
	server_marks_spent(&srv, paid.id()).await;
	let (res, _) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(wallet.refresh_vtxos(vec![paid.id(), kept]), async {
			tokio::time::sleep(Duration::from_secs(2)).await;
			srv.trigger_round().await;
		})
	}).await.expect("refresh must join a subsequent complete round");
	res.expect("refresh should retry without the paid vtxo").expect("a round happened");
	ctx.generate_blocks(ROUND_CONFIRMATIONS).await;
	wallet.sync().await;
	let ids = wallet.spendable_vtxos().await.unwrap().into_iter().map(|v| v.id()).collect::<Vec<_>>();
	assert!(!ids.contains(&kept), "kept vtxo should have been refreshed: {ids:?}");
	assert!(ids.contains(&paid.id()), "the wallet does not know yet that `paid` is gone");

	// Let `paid` expire, but not the refreshed vtxo.
	let balance_before = wallet.balance().await.unwrap().total();
	let tip = wallet.chain().tip().await.unwrap();
	ctx.generate_blocks(paid.expiry_height().checked_blocks_since(tip).unwrap()).await;
	wallet.sync().await;
	assert_eq!(wallet.balance().await.unwrap().total(), balance_before - paid.amount());

	// Explicit adoption remains idempotent after automatic reconciliation.
	let adopted = expiry_payout::adopt_server_vtxo_status(&wallet, vec![paid.id()]).await.unwrap();
	assert_eq!(adopted, vec![AdoptedVtxoStatus { vtxo_id: paid.id(), state: AdoptedVtxoState::Spent }]);
	assert_eq!(wallet.get_vtxo_by_id(paid.id()).await.unwrap().state.kind(), VtxoStateKind::Spent);
	assert_eq!(wallet.balance().await.unwrap().total(), balance_before - paid.amount());

	let debits = wallet.history().await.unwrap().into_iter()
		.filter(|m| m.subsystem.name == "bark.server_spend").collect::<Vec<_>>();
	assert_eq!(debits.len(), 1);
	assert_eq!(debits[0].effective_balance, -paid.amount().to_signed().unwrap());
}

#[derive(Clone)]
struct DelayedStatusProxy {
	unavailable: Arc<AtomicBool>,
	delay: Arc<AtomicBool>,
	fetched: Arc<Notify>,
	release: Arc<Notify>,
}

#[async_trait::async_trait]
impl captaind::proxy::ArkRpcProxy for DelayedStatusProxy {
	async fn get_vtxo_status(
		&self, upstream: &mut ArkClient, req: protos::GetVtxoStatusRequest,
	) -> Result<protos::GetVtxoStatusResponse, tonic::Status> {
		if self.unavailable.load(Ordering::Relaxed) {
			return Err(tonic::Status::unavailable("status lookup unavailable"));
		}
		let response = upstream.get_vtxo_status(req).await?.into_inner();
		if self.delay.swap(false, Ordering::Relaxed) {
			self.fetched.notify_one();
			self.release.notified().await;
		}
		Ok(response)
	}
}

/// Status failures preserve the coin; a reply arriving after a local lock
/// cannot erase that lock. A later safe retry removes a paid coin without
/// creating a failed refresh movement.
#[tokio::test]
async fn expiry_status_defers_and_preserves_concurrent_lock() {
	let ctx = TestContext::new("bark_sdk/expiry_status_defers_and_preserves_concurrent_lock").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1))
		.cfg(|c| c.vtxo_lifetime = BlockDelta::new(32)).create().await;
	let unavailable = Arc::new(AtomicBool::new(false));
	let delay = Arc::new(AtomicBool::new(false));
	let fetched = Arc::new(Notify::new());
	let release = Arc::new(Notify::new());
	let proxy = srv.start_proxy_no_mailbox(DelayedStatusProxy {
		unavailable: unavailable.clone(), delay: delay.clone(),
		fetched: fetched.clone(), release: release.clone(),
	}).await;
	let wallet = ctx.bark_sdk("wallet", &proxy).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coin = wallet.spendable_vtxos().await.unwrap().remove(0);
	let tip = wallet.chain().tip().await.unwrap();
	ctx.generate_blocks(coin.expiry_height().checked_blocks_since(tip).unwrap()).await;
	server_marks_spent(&srv, coin.id()).await;
	let history_before = wallet.history().await.unwrap().len();

	unavailable.store(true, Ordering::Relaxed);
	wallet.sync().await;
	assert!(wallet.maybe_schedule_maintenance_refresh_delegated().await.unwrap().is_none());
	assert_eq!(wallet.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Spendable);
	assert_eq!(wallet.history().await.unwrap().len(), history_before);

	unavailable.store(false, Ordering::Relaxed);
	delay.store(true, Ordering::Relaxed);
	let (adopted, ()) = tokio::time::timeout(Duration::from_secs(15), async {
		tokio::join!(wallet.trust_and_adopt_server_vtxo_status(coin.id()), async {
			fetched.notified().await;
			wallet.lock_vtxos(&[coin.id()], None).await.unwrap();
			release.notify_one();
		})
	}).await.expect("delayed status and concurrent lock must finish");
	assert!(adopted.is_err());
	assert_eq!(wallet.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Locked);
	assert_eq!(wallet.history().await.unwrap().len(), history_before,
		"a refused transition must not write a debit");
	wallet.unlock_vtxos(&[coin.id()], None).await.unwrap();
	assert!(wallet.maybe_schedule_maintenance_refresh_delegated().await.unwrap().is_none());
	assert_eq!(wallet.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Spent);
	let history = wallet.history().await.unwrap();
	assert_eq!(history.len(), history_before + 1);
	let debit = history.iter().find(|m| m.subsystem.name == "bark.server_spend").unwrap();
	assert_eq!(debit.status, MovementStatus::Successful);
	assert_eq!(debit.effective_balance, -coin.amount().to_signed().unwrap());
	wallet.trust_and_adopt_server_vtxo_status(coin.id()).await.unwrap();
	assert_eq!(wallet.history().await.unwrap(), history, "adoption is idempotent");
}
