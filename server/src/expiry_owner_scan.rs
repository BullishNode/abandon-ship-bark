//! Bounded background derivation for pending customer ownership keys.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::Server;

impl Server {
	pub(crate) fn start_expiry_owner_scan(self: Arc<Self>) {
		// Register before spawning so shutdown cannot race task registration.
		let worker = self.rtmgr.spawn_critical("ExpiryOwnerScan");
		tokio::spawn(async move {
			let _worker = worker;
			while !self.rtmgr.shutdown_requested() {
				// Let an active bounded pass finish before dropping its tracked
				// guard. Cancelling a spawn_blocking future would detach the
				// derivation while incorrectly reporting shutdown complete.
				let delay = match self.db.scan_expiry_owners(
					self.config.network,
					self.config.expiry_owner_scan_chunk_size,
					self.config.expiry_owner_scan_workers,
				).await {
					Ok(stats) if stats.children_derived != 0 => {
						debug!(records = stats.records_scanned, children = stats.children_derived,
							matched = stats.keys_matched, "Expiry owner scan progress");
						// A registry scan must not pay an idle polling delay for
						// every pair of records while there is queued work.
						tokio::task::yield_now().await;
						continue;
					},
					Ok(_) => Duration::from_secs(1),
					Err(error) => {
						warn!("Expiry owner scan failed; unresolved outputs remain blocked: {:#}", error);
						Duration::from_secs(2)
					},
				};
				tokio::select! {
					_ = self.rtmgr.shutdown_signal() => break,
					_ = tokio::time::sleep(delay) => {},
				}
			}
		});
	}
}
