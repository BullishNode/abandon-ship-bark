//! Manages outbound Lightning payments. Sends payments via CLN's `xpay` RPC
//! and tracks their status via `listsendpays` streams.
//!
//! ## Payment lifecycle
//!
//! [`ClnXpay::pay`] is fire-and-forget: it spawns a task that calls `xpay` over gRPC.
//! When CLN answers, success or error, the attempt is reconciled right away, and an
//! error is trusted as a final failure. When the call breaks in transport, xpay may
//! still be running, so the monitor reconciles the attempt once its retry time has
//! passed.
//!
//! ## Sendpay stream
//!
//! The main loop `wait`s on CLN for new `created` and `updated` sendpay events.
//! Each event is matched against open payment attempts in the DB and transitions
//! them through `Requested → Submitted → Succeeded/Failed`.
//!
//! ## Payment reconciliation
//!
//! On a periodic interval, queries `listpays` for all open attempts to catch anything
//! the stream missed (e.g. events during downtime). Uses exponential backoff per
//! invoice to avoid hammering CLN.

use std::{fmt, str};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bitcoin::Amount;
use bitcoin::hex::DisplayHex;
use chrono::{DateTime, Local};
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;
use tracing::{debug, error, trace, warn};

use ark::lightning::{Invoice, PaymentHash, Preimage};
use bitcoin_ext::BlockDelta;
use cln_rpc::listpays_pays::ListpaysPaysStatus;

use crate::database;
use crate::database::ln::{LightningNodeId, LightningPaymentAttempt, LightningPaymentStatus};
use crate::ln::settler::HtlcSettler;
use crate::ln::guard::PaymentGuards;
use crate::system::RuntimeManager;
use crate::telemetry;

use super::ClnGrpcClient;
use super::super::payment_handler::PaymentAttemptHandler;


/// The buffer we add to the xpay timeout before we check invoice
pub const XPAY_TIMEOUT_BUFFER: Duration = Duration::from_secs(15);

/// Shared client for sending xpay RPCs and reconciling payment status against CLN.
///
/// Wrapped in an `Arc` so both [`ClnXpay::pay`] (fire-and-forget spawned tasks)
/// and [`ClnXpayProcess`] (periodic reconciliation) can use it concurrently.
pub(crate) struct ClnXpayClient {
	db: database::Db,
	rpc: ClnGrpcClient,
	settler: Arc<HtlcSettler>,
	/// Notifies [`LightningManager::wait_payment_status`] when a payment reaches a final state.
	///
	/// [`LightningManager::wait_payment_status`]: crate::ln::node_manager::LightningManager::wait_payment_status
	payment_update_tx: broadcast::Sender<PaymentHash>,
	mailbox_manager: Arc<crate::mailbox_manager::MailboxManager>,
	payment_guards: PaymentGuards,
}

impl ClnXpayClient {
	fn payment_handler(&self) -> PaymentAttemptHandler<'_> {
		PaymentAttemptHandler::new(&self.db, &self.mailbox_manager, &self.payment_update_tx)
	}

	pub async fn new(
		db: database::Db,
		payment_update_tx: broadcast::Sender<PaymentHash>,
		rpc: ClnGrpcClient,
		settler: Arc<HtlcSettler>,
		mailbox_manager: Arc<crate::mailbox_manager::MailboxManager>,
		payment_guards: PaymentGuards,
	) -> Arc<Self> {
		Arc::new(Self { db, payment_update_tx, rpc, settler, mailbox_manager, payment_guards })
	}

	/// Calls xpay and then reconciles the attempt status against CLN.
	///
	/// On RPC success the sendpay stream will pick up the result, but we still
	/// reconcile afterwards to handle edge cases (e.g. the stream missing an event).
	/// An error from CLN means xpay stopped trying, so the attempt is failed.
	/// On a transport error it is left to the monitor.
	#[tracing::instrument(skip_all, fields(
		payment_hash = %invoice.payment_hash(),
		invoice = %invoice,
		payment_amount = %payment_amount,
		max_routing_fee = %max_routing_fee,
		max_cltv_expiry_delta,
		retry_for = ?retry_for,
	))]
	pub async fn pay(
		&self,
		attempt: LightningPaymentAttempt,
		invoice: Box<Invoice>,
		payment_amount: Amount,
		max_routing_fee: Amount,
		max_cltv_expiry_delta: BlockDelta,
		retry_for: Duration,
	) {
		let payment_hash = invoice.payment_hash();
		let _dispatch = self.payment_guards.lock_dispatch(payment_hash).await;
		let payment = self.payment_guards.lock(payment_hash).await;
		// A task can wait after admission while reconciliation or a returning
		// client closes its attempt. Never dispatch such an obsolete task.
		let admitted = self.db.write(async |tx| tx.admit_lightning_dispatch(&attempt).await).await;
		match admitted {
			Ok(true) => {},
			Ok(false) => { debug!(attempt_id=attempt.id, "Skipping claimed or obsolete queued xpay attempt"); return; },
			Err(error) => {
				warn!(attempt_id=attempt.id, %error, "Refusing queued xpay dispatch");
				if let Err(error) = self.payment_handler().fail_payment_attempt(&attempt, Some(&error.to_string())).await {
					error!(attempt_id=attempt.id, %error, "Recording refused xpay dispatch failed");
				}
				return;
			},
		}
		let label = match self.db.read(async |t| Ok(t.query_one(
			"SELECT label FROM expiry_lightning_dispatch WHERE id=$1", &[&attempt.id],
		).await?.get::<_, String>("label"))).await {
			Ok(label) => label,
			Err(error) => { error!(attempt_id=attempt.id, %error, "Reading actual dispatch label failed"); return; },
		};
		// The durable dispatch keeps its source pending. Only xpay and its
		// monitors wait for the remote result; a same-hash receive can proceed.
		drop(payment);
		let mut rpc = self.rpc.clone();
		let result = call_xpay(
			&mut rpc, &label, &invoice, payment_amount, max_routing_fee, max_cltv_expiry_delta, retry_for,
		).await;
		let _payment = self.payment_guards.lock(payment_hash).await;
		let pay_error = match result {
			Ok(preimage) => {
				// The attempt may already be coarse-final after a lost status
				// race. Never discard the actual xpay result in that case.
				if preimage.compute_payment_hash() != payment_hash {
					error!(%payment_hash, "xpay returned a preimage for another payment hash");
					return;
				}
				if let Err(error) = self.settler.settle(preimage).await {
					error!(%payment_hash, %error, "Recording xpay settlement failed");
				}
				trace!("Payment successful for payment hash {}", payment_hash.as_hex());
				None
			},
			Err(pay_err) if !is_cln_error(&pay_err) => {
				// xpay may still be trying, and syncing now could fail an
				// attempt CLN goes on to pay.
				warn!("xpay call for {} broke, leaving it to the monitor: {:#}",
					payment_hash, pay_err,
				);
				return;
			},
			Err(pay_err) => {
				debug!("Error calling pay-command: {}", pay_err);
				if let Some(code) = pay_err.downcast_ref::<tonic::Status>().and_then(no_attempt_rejection) {
					if let Err(error) = self.db.write(async |t| {
						t.execute("
							INSERT INTO expiry_lightning_rpc_rejection(id,code) VALUES($1,$2)
							ON CONFLICT(id) DO NOTHING
						", &[&attempt.id,&code]).await?;
						Ok(())
					}).await { error!(attempt_id=attempt.id, %error, "Recording actual JSON-RPC rejection failed"); }
				}
				Some(pay_err.to_string())
			},
		};

		let attempt_res = self.db
			.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(payment_hash).await).await;

		let attempt_res = match (attempt_res, pay_error.as_deref()) {
			// Store the error before reconciling, so the reason survives if
			// `listpays` fails and the monitor has to finish the attempt.
			(Ok(Some(attempt)), Some(e)) => self.store_pay_error(attempt, e).await,
			(res, _) => res,
		};

		match attempt_res {
			Ok(Some(attempt)) => {
				if let Err(e) = self.sync_payment_attempt_status_locked(attempt, pay_error.as_deref()).await {
					error!("Error syncing payment attempt status: {e:#}");
				}
			},
			Ok(None) => {
				error!("Attempt not found for payment hash after calling xpay: {}", payment_hash);
			},
			Err(e) => {
				error!("Error getting open payment attempt by payment hash: {e:#}");
			},
		}
	}

	/// Stores the xpay error on `attempt` and returns it as stored, so later
	/// updates carry the new `updated_at`. When the attempt was changed in the
	/// meantime, it is reloaded and the store retried. Returns `None` once the
	/// attempt is no longer open, leaving a finalized attempt untouched.
	async fn store_pay_error(
		&self,
		mut attempt: LightningPaymentAttempt,
		error: &str,
	) -> anyhow::Result<Option<LightningPaymentAttempt>> {
		loop {
			let updated_at = self.db.write(async |t|
				t.set_lightning_payment_attempt_error(&attempt, error).await
			).await?;
			if let Some(updated_at) = updated_at {
				attempt.updated_at = updated_at;
				attempt.error = Some(error.to_owned());
				return Ok(Some(attempt));
			}

			let reloaded = self.db.read(async |t|
				t.get_open_lightning_payment_attempt_by_payment_hash(attempt.payment_hash).await
			).await?;
			match reloaded {
				Some(a) if a.id == attempt.id => attempt = a,
				_ => return Ok(None),
			}
		}
	}

	/// Queries CLN's `listpays` for the given attempt and updates the DB to match.
	///
	/// If CLN has no record of the payment and the attempt is still open, it is
	/// marked `Failed`. If CLN reports a different status than the DB, the DB is
	/// updated (unless the DB status is already final, which is logged as an error).
	/// Sends on `payment_update_tx` when the status changes.
	///
	/// `pay_error` is the error CLN returned for the xpay call this follows.
	/// xpay only returns once it has stopped trying, so the attempt is failed
	/// even when `listpays` still reports it pending. The monitor loop passes
	/// `None`; it is reconciling attempts it did not make.
	/// Source-specific receipts alone decide source release and mailbox completion.
	pub async fn sync_payment_attempt_status(&self, attempt: LightningPaymentAttempt, pay_error: Option<&str>) -> anyhow::Result<()> {
		// A pending xpay reconciles its own result, even across a reconnect.
		// Always take dispatch before decision; skip rather than wait here.
		let Some(_dispatch) = self.payment_guards.try_lock_dispatch(attempt.payment_hash) else { return Ok(()); };
		let Some(_payment) = self.payment_guards.try_lock(attempt.payment_hash) else { return Ok(()); };
		self.sync_payment_attempt_status_locked(attempt, pay_error).await
	}

	async fn sync_payment_attempt_status_locked(&self, attempt: LightningPaymentAttempt, pay_error: Option<&str>) -> anyhow::Result<()> {
		let payment_hash = attempt.payment_hash;
		let error = pay_error.or(attempt.error.as_deref());
		debug!("Lightning payment attempt ({}): with payment hash {} is being verified.",
			attempt.id, payment_hash,
		);

		telemetry::add_payment_sync(attempt.lightning_node_id, attempt.status);

		let req = cln_rpc::ListpaysRequest {
			bolt11: None,
			payment_hash: Some(payment_hash.to_vec()),
			status: None,
			index: None,
			limit: None,
			start: None,
		};
		let listpays_response = self.rpc.clone().list_pays(req).await
			.context("Could not fetch cln payments")?
			.into_inner();
		let dispatch = self.db.read(async |t| Ok(t.query_opt(
			"
				SELECT d.label,r.code FROM expiry_lightning_dispatch d
			LEFT JOIN expiry_lightning_rpc_rejection r ON r.id=d.id WHERE d.id=$1
			", &[&attempt.id],
		).await?.map(|row| (row.get::<_, String>("label"),row.get::<_, Option<i32>>("code"))))).await?;
		if let Some((label,rejection)) = dispatch {
			// CLN can return an old successful invoice to a new caller. Only
			// a completed group carrying THIS dispatch's unique label discharges
			// its source coins. Store the actual preimage and receipt together.
			for pay in &listpays_response.pays {
				if pay.label.as_deref() != Some(label.as_str()) || pay.payment_hash != payment_hash.to_vec()
					|| pay.status() != ListpaysPaysStatus::Complete { continue; }
				let bytes = pay.preimage.as_ref().context("completed source dispatch has no preimage")?;
				let preimage = Preimage::from_slice(bytes).context("invalid source dispatch preimage")?;
				ensure!(preimage.compute_payment_hash() == payment_hash, "source dispatch preimage has another hash");
				let created = i64::try_from(pay.created_index.context("completed source dispatch has no index")?)?;
				let updated = pay.updated_index.map(i64::try_from).transpose()?;
				self.db.write(async |t| {
					t.store_htlc_settlement(preimage).await?;
					t.execute("
						INSERT INTO expiry_lightning_external_success(id,created_index,updated_index)
						VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING
					", &[&attempt.id,&created,&updated]).await?;
					Ok(())
				}).await?;
			}
			let observed = terminal_failure(&listpays_response.pays, &label, payment_hash);
			let no_attempt = rejection.filter(|code| matches!(code,205|207))
				.filter(|_| listpays_response.pays.iter().all(|pay|
					pay.payment_hash == payment_hash.to_vec() && pay.status() == ListpaysPaysStatus::Failed && pay.preimage.is_none()
				));
			if observed.is_some() || no_attempt.is_some() {
				let version = self.rpc.clone().getinfo(cln_rpc::GetinfoRequest {}).await?.into_inner().version;
				ensure!(version == "v26.06.6", "CLN {version} terminal failure semantics have not been qualified");
				let created = observed.map(|(created,_)| i64::try_from(created)).transpose()?;
				let updated = observed.and_then(|(_,updated)| updated).map(i64::try_from).transpose()?;
				let error_code = if observed.is_some() { None } else { no_attempt };
				self.db.write(async |t| {
					t.execute("
						INSERT INTO expiry_lightning_terminal_failure(id,created_index,updated_index,jsonrpc_error_code)
						VALUES($1,$2,$3,$4) ON CONFLICT(id) DO NOTHING
					", &[&attempt.id,&created,&updated,&error_code]).await?;
					Ok(())
				}).await?;
			}
		}
		if listpays_response.pays.is_empty() {
			match attempt.status {
				LightningPaymentStatus::Succeeded => {
					error!("Lightning payment attempt ({}): flagged succeeded \
						when it cannot be found in CLN for payment hash {}",
						attempt.id, payment_hash,
					);
				},
				LightningPaymentStatus::Failed => {
					error!("Lightning payment attempt ({}): flagged failed \
						when it cannot be found in CLN for payment hash {}",
						attempt.id, payment_hash,
					)
				},
				LightningPaymentStatus::Requested
					| LightningPaymentStatus::Submitted =>
				{
					self.payment_handler().fail_payment_attempt(&attempt, error).await?;
				},
			}
		} else {
			// A coarse final attempt status can be stale after a lost response
			// or optimistic-write race. Every authentic preimage is settlement
			// evidence, including one attached to an older CLN payment group.
			// Preserve it before deciding whether a status transition applies.
			for pay in &listpays_response.pays {
				if let Some(bytes) = &pay.preimage {
					let preimage = Preimage::from_slice(bytes)
						.context("CLN returned a preimage that is not 32 bytes")?;
					ensure!(preimage.compute_payment_hash() == payment_hash, "CLN returned a preimage for another payment hash");
					self.settler.settle(preimage).await?;
				}
			}

			let latest = listpays_response.pays.into_iter().max_by_key(|p| {
				p.created_index.expect("should have index")
			}).expect("we have at least one");

			let updated_status = match latest.status() {
				// xpay told us it failed, trust that over the listpays record.
				ListpaysPaysStatus::Pending if pay_error.is_some() => LightningPaymentStatus::Failed,
				ListpaysPaysStatus::Pending => LightningPaymentStatus::Submitted,
				ListpaysPaysStatus::Complete => {
					if latest.preimage.is_none() {
						error!("Lightning payment attempt ({}): completed but no preimage \
							specified for payment hash {}",
							attempt.id, payment_hash,
						);
						LightningPaymentStatus::Submitted
					} else {
						LightningPaymentStatus::Succeeded
					}
				},
				ListpaysPaysStatus::Failed => LightningPaymentStatus::Failed,
			};

			// The onion is binary, so keep it as hex next to the xpay error.
			let onion = latest.erroronion.as_ref().map(|b| format!("erroronion={}", b.as_hex()));
			let error_string = match (error, onion) {
				(Some(e), Some(o)) => Some(format!("{e}; {o}")),
				(e, o) => e.map(String::from).or(o),
			};

			if attempt.status != updated_status {
				if attempt.status.is_final() {
					error!("Lightning payment attempt ({}): flagged {} when it \
						actually {} for payment hash {}",
						attempt.id, attempt.status, updated_status,
						payment_hash,
					);
				} else {
					let preimage = latest.preimage.map(|b| Preimage::from_slice(&b))
						.transpose()
						.context("CLN returned a preimage that is not 32 bytes")?;

					if let Some(preimage) = &preimage {
						if preimage.compute_payment_hash() != attempt.payment_hash {
							bail!("preimage does not match payment hash");
						}
					}

					// NB: for intra-ark payments, settle_invoice may also post
					// the mailbox notification for the same payment hash.
					self.payment_handler().process_payment_attempt(
						&self.settler,
						&attempt,
						updated_status,
						error_string.as_deref(),
						latest.amount_sent_msat.map(|v| v.msat),
						preimage,
					).await?;
				}
			}
		}

		Ok(())
	}
}

/// Timing knobs for the xpay monitor loop and reconciliation backoff.
#[derive(Debug, Clone)]
pub struct ClnXpayConfig {
	pub invoice_check_interval: Duration,
	/// Retry time for attempts that were stored without one.
	pub cln_xpay_timeout: Duration,
	pub check_base_delay: Duration,
	pub max_check_delay: Duration,
}

/// Handle for the xpay monitor process.
///
/// Tracks outbound payment status via CLN's listsendpays streams
/// and periodically verifies open payment attempts.
pub struct ClnXpay {
	jh: Option<JoinHandle<anyhow::Result<()>>>,
	client: Arc<ClnXpayClient>,
}

impl ClnXpay {
	/// Spawns the background reconciliation loop and returns a handle.
	///
	/// Reads the current payment stream indices from the DB so the monitor
	/// knows where to resume after a restart.
	pub async fn start(
		rtmgr: RuntimeManager,
		mgr_waker: Arc<Notify>,
		db: database::Db,
		payment_update_tx: broadcast::Sender<PaymentHash>,
		node_id: LightningNodeId,
		rpc: ClnGrpcClient,
		config: ClnXpayConfig,
		settler: Arc<HtlcSettler>,
		mailbox_manager: Arc<crate::mailbox_manager::MailboxManager>,
		payment_guards: PaymentGuards,
	) -> anyhow::Result<ClnXpay> {
		let payment_idxs = db.read(async |t| t.get_lightning_payment_indexes(node_id).await).await
			.with_context(|| format!("failed to fetch payment indices for {}", node_id))?
			.unwrap_or_default();

		slog!(XpayStarted,
			node_id: node_id,
			created_index: payment_idxs.created_index,
			updated_index: payment_idxs.updated_index,
		);

		let client = ClnXpayClient::new(db.clone(), payment_update_tx, rpc, settler, mailbox_manager, payment_guards).await;

		let proc = ClnXpayProcess {
			config, db, node_id,
			client: client.clone(),
			attempt_next_check_at: HashMap::new(),
		};

		let jh = tokio::spawn(async move {
			let ret = proc.run(rtmgr, mgr_waker).await;
			if let Err(ref e) = ret {
				slog!(XpayStopped, node_id: node_id, error: format!("{:?}", e));
			}
			ret
		});

		Ok(ClnXpay { jh: Some(jh), client })
	}

	pub fn is_running(&self) -> bool {
		self.jh.as_ref().is_some_and(|jh| !jh.is_finished())
	}

	/// Cheap clone of the shared command client. Used by [`NodeHandle`] to
	/// issue xpay payments directly without going through the spawn helper.
	pub(crate) fn client(&self) -> Arc<ClnXpayClient> {
		self.client.clone()
	}

	/// Wait for the process to end.
	pub async fn wait(mut self) -> Result<anyhow::Result<()>, tokio::task::JoinError> {
		match self.jh.take() {
			Some(jh) => Ok(jh.await?),
			None => Ok(Ok(())),
		}
	}

}

impl Drop for ClnXpay {
	fn drop(&mut self) {
		if let Some(jh) = self.jh.take() {
			jh.abort();
		}
	}
}

impl fmt::Debug for ClnXpay {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("ClnXpay")
	}
}

/// Background loop that periodically reconciles open payment attempts against CLN.
///
/// Runs on a fixed interval ([`ClnXpayConfig::invoice_check_interval`]) and,
/// for each open attempt old enough to have finished xpay retries, queries
/// `listpays` to drive the attempt to a final state. Uses exponential backoff
/// per invoice to avoid hammering CLN for long-lived pending payments.
struct ClnXpayProcess {
	config: ClnXpayConfig,
	db: database::Db,

	node_id: LightningNodeId,

	client: Arc<ClnXpayClient>,

	/// Per-attempt backoff state: maps attempt id → (check count, earliest next check time).
	/// Entries are pruned once their next-check time lapses.
	attempt_next_check_at: HashMap<i64, (usize, DateTime<Local>)>,
}

impl ClnXpayProcess {
	/// Bumps the backoff counter for `attempt_id` and schedules the next check.
	///
	/// Delay doubles each check starting from `check_base_delay`, capped at
	/// `max_check_delay`.
	fn update_next_attempt_check(&mut self, attempt_id: i64) {
		let (checks, next_check) = self.attempt_next_check_at.entry(attempt_id)
			.or_insert((0, Local::now()));
		*checks += 1;

		// Calculate delay: grows with each check
		// e.g. base 10 seconds, doubling each time, capped to a max delay
		let base_delay_secs = self.config.check_base_delay.as_secs();
		let max_delay_secs = self.config.max_check_delay.as_secs();
		let delay_secs = (base_delay_secs.saturating_mul(2u64.saturating_pow(*checks as u32 - 1)))
			.min(max_delay_secs);

		*next_check = Local::now() + Duration::from_secs(delay_secs);

		trace!("Lightning payment attempt ({}): Check {} done, updated next check to {}.",
			attempt_id, checks, next_check,
		);
	}

	/// Iterates over all open payment attempts for this node and reconciles
	/// each one that is old enough and not in backoff. Prunes expired backoff
	/// entries afterwards.
	async fn process_payment_attempts(&mut self) -> anyhow::Result<()> {
		let open_attempts = self.db.read(async |t| t.get_open_lightning_payment_attempts(self.node_id).await).await?;

		for attempt in open_attempts {
			if attempt.is_self_payment() {
				trace!("Lightning payment attempt ({}): Skipping since it is a self payment.",
					attempt.id,
				);

				continue;
			}

			// We don't want to go further if we aren't sure CLN
			// didn't finished retrying payment attempts. Each attempt carries
			// the retry time it was started with; older rows predate that.
			let retry_for = attempt.retry_for.unwrap_or(self.config.cln_xpay_timeout);
			let safe_delay_cln_stopped_retries = retry_for + XPAY_TIMEOUT_BUFFER;
			if attempt.created_at > Local::now() - safe_delay_cln_stopped_retries {
				trace!("Lightning payment attempt ({}): Skipping since it was just created.",
					attempt.id,
				);

				continue;
			}

			let next_check = self.attempt_next_check_at.get(&attempt.id);
			if next_check.is_some() && next_check.unwrap().1 > Local::now() {
				trace!("Lightning payment attempt ({}): Skipping since it was checked recently.",
					attempt.id,
				);

				continue;
			}

			let attempt_id = attempt.id;
			if let Err(e) = self.client.sync_payment_attempt_status(attempt, None).await {
				error!("Error syncing payment attempt status: {e:#}");
			} else {
				self.update_next_attempt_check(attempt_id);
			}

		}

		// A coarse Failed row can precede CLN's terminal result. Keep checking
		// expired actual dispatches until their own remote group is proved.
		let failed = self.db.write(async |t| t.unproven_expiry_lightning_dispatches(self.node_id, 16).await).await?;
		for attempt in failed {
			if let Err(error) = self.client.sync_payment_attempt_status(attempt).await {
				warn!(%error, "Checking terminal expiry Lightning dispatch failed");
			}
		}

		self.attempt_next_check_at.retain(|_, &mut (_, datetime)| datetime > Local::now());

		telemetry::set_pending_payment_syncs(
			self.node_id,
			self.attempt_next_check_at.len(),
		);

		Ok(())
	}

	/// Main select loop: runs [`process_payment_attempts`](Self::process_payment_attempts)
	/// on every tick and exits cleanly on shutdown.
	async fn run(mut self, rtmgr: RuntimeManager, mgr_waker: Arc<Notify>) -> anyhow::Result<()> {
		let _worker = rtmgr.spawn(format!("ClnXpay({})", self.node_id))
			.with_notify(mgr_waker);

		let mut check_interval = tokio::time::interval(self.config.invoice_check_interval);

		loop {
			tokio::select! {
				_ = check_interval.tick() => {
					self.process_payment_attempts().await?;
				},
				_ = rtmgr.shutdown_signal() => return Ok(()),
			}
		}
	}
}

/// Whether lightningd itself answered with an error, meaning xpay has stopped
/// trying. cln-grpc words every failed call as `Error calling method ...`, but
/// only an answer from lightningd carries `RpcError { code: Some(..) }`.
fn is_cln_error(err: &anyhow::Error) -> bool {
	err.downcast_ref::<tonic::Status>()
		.is_some_and(|s| s.message().contains("RpcError { code: Some("))
}

/// Only typed receipts from the qualified gRPC adapter are accepted. In
/// CLN26.06.6, 205 is returned before any new_attempt (later routing failures
/// map to 209), and 207 precedes routing. Generic and transport errors do not
/// establish quiescence, even if their explanatory text looks identical.
fn no_attempt_rejection(status: &tonic::Status) -> Option<i32> {
	if status.code() != tonic::Code::Unknown { return None; }
	let code = status.metadata().get("abandon-cln-jsonrpc-error-code")?.to_str().ok()?.parse().ok()?;
	matches!(code,205|207).then_some(code)
}

/// CLN26.06.6's listpays stays Pending while any same-hash xpay object
/// exists. A matching random dispatch label proves this request reached an
/// actual sendpay group; an older failed group or an empty response cannot.
fn terminal_failure(pays: &[cln_rpc::ListpaysPays], label: &str, hash: PaymentHash) -> Option<(u64, Option<u64>)> {
	if pays.is_empty() || pays.iter().any(|pay|
		pay.payment_hash != hash.to_vec() || pay.status() != ListpaysPaysStatus::Failed || pay.preimage.is_some()
	) { return None; }
	let own = pays.iter().filter(|pay| pay.label.as_deref() == Some(label))
		.max_by_key(|pay| pay.created_index)?;
	Some((own.created_index.filter(|index| *index > 0)?, own.updated_index))
}

/// Calls the xpay-command over gRPC.
/// If the payment completes successfully it will return the pre-image
/// Otherwise, an error will be returned
async fn call_xpay(
	rpc: &mut ClnGrpcClient,
	label: &str,
	invoice: &Invoice,
	payment_amount: Amount,
	max_routing_fee: Amount,
	max_cltv_expiry_delta: BlockDelta,
	retry_for: Duration,
) -> anyhow::Result<Preimage> {
	let payment_hash = invoice.payment_hash();

	slog!(XpayRpcCalled,
		payment_hash, payment_amount, max_routing_fee,
		invoice: invoice.to_string(),
		max_delay: max_cltv_expiry_delta.to_u32(),
	);

	let pay_result = rpc.xpay(cln_rpc::XpayRequest {
		invstring: invoice.to_string(),
		// cln doesn't allow tipping
		amount_msat: if invoice.amount_msat().is_none() {
			Some(payment_amount.into())
		} else {
			None
		},
		maxdelay: Some(max_cltv_expiry_delta.to_u32()),
		maxfee: Some(max_routing_fee.into()),
		retry_for: Some(retry_for.as_secs() as u32),
		partial_msat: None,
		layers: vec![],
		payer_note: None,
		label: Some(label.to_owned()),
		localinvreqid: None,
		dev_use_shadow: None,
	}).await;

	let result = match pay_result {
		Err(e) => Err(e.into()),
		Ok(resp) => {
			let bytes = resp.into_inner().payment_preimage;
			if bytes.is_empty() {
				Err(anyhow!("missing preimage"))
			} else {
				bytes.try_into().ok().context("invalid preimage not 32 bytes")
			}
		}
	};

	slog!(XpayRpcReturned,
		payment_hash: payment_hash,
		error: result.as_ref().err().map(|e| e.to_string()),
	);

	result
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn is_cln_error_tells_cln_errors_from_transport_errors() {
		let cln = tonic::Status::unknown("Error calling method Xpay: RpcError { \
			code: Some(205), message: \"no route\", data: None }");
		assert!(is_cln_error(&cln.into()));

		let transport = tonic::Status::unavailable("transport error");
		assert!(!is_cln_error(&transport.into()));
		let reset = tonic::Status::unknown("h2 protocol error: stream reset");
		assert!(!is_cln_error(&reset.into()));
		let socket = tonic::Status::unknown("Error calling method Xpay: RpcError { \
			code: None, message: \"no response from lightningd\", data: None }");
		assert!(!is_cln_error(&socket.into()));
		assert!(!is_cln_error(&anyhow!("missing preimage")));
	}
}

#[cfg(test)]
mod expiry_tests {
	use super::*;

	fn failed(label: &str, hash: PaymentHash) -> cln_rpc::ListpaysPays {
		cln_rpc::ListpaysPays {
			payment_hash: hash.to_vec(), status: ListpaysPaysStatus::Failed as i32,
			label: Some(label.to_owned()), created_index: Some(7), updated_index: Some(9),
			..Default::default()
		}
	}

	#[test]
	fn no_attempt_rejection_requires_typed_source_reviewed_code() {
		let mut status = tonic::Status::unknown("even convincing error text is not a receipt");
		assert_eq!(no_attempt_rejection(&status), None);
		for (code,expected) in [("205",Some(205)),("207",Some(207)),("200",None),("209",None),("garbled",None)] {
			status.metadata_mut().insert("abandon-cln-jsonrpc-error-code",code.parse().unwrap());
			assert_eq!(no_attempt_rejection(&status), expected);
		}
	}

	#[test]
	fn terminal_failure_requires_own_observed_dispatch_and_every_group_failed() {
		let hash = PaymentHash::from([17;32]);
		assert_eq!(terminal_failure(&[], "own", hash), None);
		assert_eq!(terminal_failure(&[failed("old", hash)], "own", hash), None);
		assert_eq!(terminal_failure(&[failed("own", hash)], "own", hash), Some((7,Some(9))));
		let mut pending = failed("old", hash);
		pending.status = ListpaysPaysStatus::Pending as i32;
		assert_eq!(terminal_failure(&[failed("own", hash),pending], "own", hash), None);
		let mut paid = failed("old", hash);
		paid.status = ListpaysPaysStatus::Complete as i32;
		assert_eq!(terminal_failure(&[failed("own", hash),paid], "own", hash), None);
		let mut known = failed("own", hash);
		known.preimage = Some(vec![1;32]);
		assert_eq!(terminal_failure(&[known], "own", hash), None);
		assert_eq!(terminal_failure(&[failed("own", PaymentHash::from([18;32]))], "own", hash), None);
	}
}
