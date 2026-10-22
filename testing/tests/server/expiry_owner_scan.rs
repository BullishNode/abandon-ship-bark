use std::time::Duration;

use ark_testing::TestContext;
use bitcoin::Network;
use server::database::Db;
use server::database::expiry_attribution::Attribution;

use super::expiry_attribution::{key, register};

#[tokio::test]
async fn expiry_owner_scan_runs_in_captaind_and_stops_gracefully() {
	let ctx = TestContext::new("server/expiry_owner_scan_runtime").await;
	let srv = ctx.captaind("server").create().await;
	let config = srv.config().postgres.clone();
	let db = Db::connect(&config).await.unwrap();
	let (account, mailbox) = register(&db, 19).await;
	let wanted = key(&account, 250);
	db.write(async |tx| tx.queue_expiry_owner(wanted, Some(mailbox)).await).await.unwrap();
	// No manual scanner invocation: only the captaind task can resolve
	// this key. The index also requires several immediately consecutive passes.
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let result = db.read(async |tx| tx.expiry_owner(wanted, Network::Regtest).await).await.unwrap();
			if let Attribution::Owned { mailbox_pk, child_index, .. } = result {
				assert_eq!(mailbox_pk, mailbox.as_pubkey());
				assert_eq!(child_index, 250);
				break;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}).await.unwrap();
	let mut stopped = srv.subscribe_log::<server_log::WorkerStopped>();
	let log_path = srv.config().data_dir.join("stdout.log");
	let log_offset = tokio::fs::metadata(&log_path).await.unwrap().len() as usize;
	tokio::time::timeout(Duration::from_secs(10), srv.stop()).await.unwrap().unwrap();
	tokio::time::timeout(Duration::from_secs(2), async {
		loop {
			if stopped.recv().await.unwrap().name == "ExpiryOwnerScan" { break; }
		}
	}).await.unwrap();
	// The daemon log subscriber can stop when the process is reaped, before
	// forwarding its final line. Check the new persisted log suffix;
	// an earlier run's shutdown record must not satisfy this assertion.
	let logs = tokio::fs::read(&log_path).await.unwrap();
	assert!(logs[log_offset..].split(|byte| *byte==b'\n').any(|line|
		serde_json::from_slice::<serde_json::Value>(line).is_ok_and(|record| record["slog_id"]=="ServerTerminated")
	), "the real captaind process did not finish its graceful shutdown");
}
