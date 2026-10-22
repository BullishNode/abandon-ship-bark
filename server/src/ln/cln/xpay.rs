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
use cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus;

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
		let admitted: anyhow::Result<bool> = async {
			let mut rpc = self.rpc.clone();
			let info = tokio::time::timeout(Duration::from_secs(20), rpc.getinfo(cln_rpc::GetinfoRequest {}))
				.await.context("CLN qualification check timed out")??.into_inner();
			ensure!(info.version == super::QUALIFIED_CLN_VERSION,
				"CLN {} is not the qualified node build {}", info.version, super::QUALIFIED_CLN_VERSION);
			self.db.write(async |tx| tx.admit_lightning_dispatch(&attempt).await).await
		}.await;
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
				// race. Never discard the xpay result in that case.
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

		let attempt = if let Some(error) = pay_error.as_deref() {
			match self.store_pay_error(attempt.clone(), error).await {
				Ok(Some(stored)) => stored,
				Ok(None) => attempt,
				Err(error) => {
					error!(attempt_id=attempt.id, %error, "Recording xpay error failed");
					attempt
				},
			}
		} else { attempt };

		// The WAL consumer may already have closed the coarse status. This
		// original dispatch still needs its own payment-part receipt.
		if let Err(e) = self.sync_payment_attempt_status_locked(attempt, pay_error.as_deref()).await {
			error!("Error syncing original payment attempt status: {e:#}");
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

		let dispatch = self.db.read(async |t| Ok(t.query_opt(
			"
				SELECT d.label,r.code FROM expiry_lightning_dispatch d
			LEFT JOIN expiry_lightning_rpc_rejection r ON r.id=d.id WHERE d.id=$1
			", &[&attempt.id],
		).await?.map(|row| (row.get::<_, String>("label"),row.get::<_, Option<i32>>("code"))))).await?;
		// Read raw parts first: listpays retains only the first part's label.
		let sends = if dispatch.is_some() {
			self.rpc.clone().list_send_pays(cln_rpc::ListsendpaysRequest {
				payment_hash: Some(payment_hash.to_vec()), ..Default::default()
			}).await.context("Could not fetch cln payment parts")?.into_inner().payments
		} else { Vec::new() };
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
		if let Some((label,rejection)) = dispatch {
			// A group label comes only from its first part. Require this
			// dispatch's raw successful parts to cover the whole principal.
			for pay in &listpays_response.pays {
				let Some(preimage) = source_success(&sends, pay, &label, payment_hash, attempt.amount_msat) else { continue; };
				let created = i64::try_from(pay.created_index.context("completed source dispatch has no index")?)?;
				let updated = pay.updated_index.map(i64::try_from).transpose()?;
				let recorded = self.db.write(async |t| {
					t.lock_lightning_attempt_sources(attempt.id).await?;
					t.store_htlc_settlement(preimage).await?;
					let recorded = t.execute("
						INSERT INTO expiry_lightning_external_success(id,created_index,updated_index)
						VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING
					", &[&attempt.id,&created,&updated]).await?;
					let not_spendable = t.mark_htlc_send_vtxos_ln_spent(attempt.id).await?;
					if !not_spendable.is_empty() {
						warn!("Completed Lightning attempt {} left nonspendable sources untouched: {:?}",
							attempt.id, not_spendable);
					}
					Ok(recorded)
				}).await?;
				if recorded != 0 {
					let _ = self.payment_update_tx.send(payment_hash);
					self.payment_handler().post_lightning_send_finished(&attempt).await;
				}
			}
			let mut observed = terminal_failure(&sends, &listpays_response.pays, &label, payment_hash);
			if observed.is_some() {
				// listpays reads rows before asking xpay whether it is still live.
				// A completion between those steps must invalidate the Failed witness.
				let rechecked = self.rpc.clone().list_send_pays(cln_rpc::ListsendpaysRequest {
					payment_hash: Some(payment_hash.to_vec()), ..Default::default()
				}).await.context("Could not recheck cln payment parts")?.into_inner().payments;
				let mut before: Vec<_> = sends.iter().filter(|pay| pay.label.as_deref() == Some(label.as_str())).collect();
				let mut after: Vec<_> = rechecked.iter().filter(|pay| pay.label.as_deref() == Some(label.as_str())).collect();
				before.sort_by_key(|pay| (pay.created_index, pay.groupid, pay.partid));
				after.sort_by_key(|pay| (pay.created_index, pay.groupid, pay.partid));
				if before != after || terminal_failure(&rechecked, &listpays_response.pays, &label, payment_hash) != observed {
					observed = None;
				}
			}
			let no_attempt = rejection.filter(|code| matches!(code,205|207))
				.filter(|_| listpays_response.pays.iter().all(|pay|
					pay.payment_hash == payment_hash.to_vec() && pay.status() == ListpaysPaysStatus::Failed && pay.preimage.is_none()
				));
			if observed.is_some() || no_attempt.is_some() {
				let version = self.rpc.clone().getinfo(cln_rpc::GetinfoRequest {}).await?.into_inner().version;
				ensure!(version == super::QUALIFIED_CLN_VERSION, "CLN {version} terminal failure semantics have not been qualified");
				let created = observed.map(|(created,_)| i64::try_from(created)).transpose()?;
				let updated = observed.and_then(|(_,updated)| updated).map(i64::try_from).transpose()?;
				let error_code = if observed.is_some() { None } else { no_attempt };
				let recorded = self.db.write(async |t| {
					Ok(t.execute("
						INSERT INTO expiry_lightning_terminal_failure(id,created_index,updated_index,jsonrpc_error_code)
						VALUES($1,$2,$3,$4) ON CONFLICT(id) DO NOTHING
					", &[&attempt.id,&created,&updated,&error_code]).await?)
				}).await?;
				if recorded != 0 {
					let _ = self.payment_update_tx.send(payment_hash);
					self.payment_handler().post_lightning_send_finished(&attempt).await;
				}
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
			// or optimistic-write race. Every valid preimage is settlement
			// proof, including one attached to an older CLN payment group.
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
		// expired dispatches until their own remote group is proved.
		let failed = self.db.write(async |t| t.unproven_expiry_lightning_dispatches(self.node_id, 16).await).await?;
		for attempt in failed {
			if let Err(error) = self.client.sync_payment_attempt_status(attempt, None).await {
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

/// Recognizes adapter RPC errors for coarse status and error recording.
/// Source-specific terminal receipts are verified separately.
fn is_cln_error(err: &anyhow::Error) -> bool {
	err.downcast_ref::<tonic::Status>().is_some_and(|status|
		status.metadata().get("abandon-cln-jsonrpc-error-code")
			.and_then(|code| code.to_str().ok())
			.is_some_and(|code| code.parse::<i32>().is_ok())
			|| status.message().contains("RpcError { code: Some(")
	)
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

fn source_success(
	sends: &[cln_rpc::ListsendpaysPayments], pay: &cln_rpc::ListpaysPays,
	label: &str, hash: PaymentHash, amount_msat: u64,
) -> Option<Preimage> {
	if label.is_empty() || amount_msat == 0 || pay.payment_hash != hash.to_vec()
		|| pay.status() != ListpaysPaysStatus::Complete
		|| pay.amount_msat.as_ref()?.msat != amount_msat { return None; }
	let created = pay.created_index.filter(|index| *index > 0)?;
	let first = sends.iter().find(|part| part.created_index == created)?;
	let group: Vec<_> = sends.iter().filter(|part| part.groupid == first.groupid).collect();
	if group.iter().map(|part| part.created_index).min()? != created { return None; }
	let bytes = pay.preimage.as_deref()?;
	let preimage = Preimage::from_slice(bytes).ok()?;
	if preimage.compute_payment_hash() != hash { return None; }
	let mut completed_msat = 0u64;
	for part in group {
		if part.payment_hash != hash.to_vec() || part.label.as_deref() != Some(label) { return None; }
		match part.status() {
			ListsendpaysPaymentsStatus::Complete => {
				if part.payment_preimage.as_deref() != Some(bytes) { return None; }
				completed_msat = completed_msat.checked_add(part.amount_msat.as_ref()?.msat)?;
			},
			ListsendpaysPaymentsStatus::Failed if part.payment_preimage.is_none() => {},
			_ => return None,
		}
	}
	(completed_msat == amount_msat).then_some(preimage)
}

/// A Failed aggregate proves no same-hash xpay object remains. Complete
/// takes precedence over that liveness check, so absence of Pending is not
/// sufficient. The one-shot request keeps the same group through retries.
fn terminal_failure(
	sends: &[cln_rpc::ListsendpaysPayments], pays: &[cln_rpc::ListpaysPays],
	label: &str, hash: PaymentHash,
) -> Option<(u64, Option<u64>)> {
	if label.is_empty() || sends.iter().any(|pay|
		pay.payment_hash != hash.to_vec() || pay.status() == ListsendpaysPaymentsStatus::Pending
	) || pays.iter().any(|pay|
		pay.payment_hash != hash.to_vec() || pay.status() == ListpaysPaysStatus::Pending
			|| (pay.label.as_deref() == Some(label)
				&& (pay.status() != ListpaysPaysStatus::Failed || pay.preimage.is_some()))
	) { return None; }
	let mut groups = HashMap::new();
	for own in sends.iter().filter(|pay| pay.label.as_deref() == Some(label)) {
		if own.status() != ListsendpaysPaymentsStatus::Failed || own.payment_preimage.is_some()
			|| own.created_index == 0 { return None; }
		if sends.iter().filter(|pay| pay.groupid == own.groupid).any(|pay|
			pay.status() != ListsendpaysPaymentsStatus::Failed || pay.payment_preimage.is_some()
		) { return None; }
		let first = sends.iter().filter(|pay| pay.groupid == own.groupid)
			.map(|pay| pay.created_index).min()?;
		if first == 0 { return None; }
		groups.insert(own.groupid, first);
	}
	let mut receipt = None;
	for first in groups.values() {
		let mut matching = pays.iter().filter(|pay| pay.created_index == Some(*first));
		let witness = matching.next()?;
		if matching.next().is_some() || witness.status() != ListpaysPaysStatus::Failed
			|| witness.preimage.is_some() { return None; }
		if receipt.map_or(true, |(created, _)| *first > created) {
			receipt = Some((*first, witness.updated_index));
		}
	}
	receipt
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

		let mut structured = tonic::Status::unknown("CLN RPC failure");
		structured.metadata_mut().insert("abandon-cln-jsonrpc-error-code", "205".parse().unwrap());
		assert!(is_cln_error(&structured.into()));

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

	fn failed_part(label: &str, hash: PaymentHash, groupid: u64, created_index: u64) -> cln_rpc::ListsendpaysPayments {
		cln_rpc::ListsendpaysPayments {
			payment_hash: hash.to_vec(), status: ListsendpaysPaymentsStatus::Failed as i32,
			label: Some(label.to_owned()), groupid, created_index, ..Default::default()
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
	fn terminal_failure_requires_raw_own_parts_and_a_failed_group_witness() {
		let hash = PaymentHash::from([17;32]);
		let parts = [failed_part("own", hash, 1, 7)];
		assert_eq!(terminal_failure(&[], &[], "own", hash), None);
		assert_eq!(terminal_failure(&[], &[failed("own", hash)], "own", hash), None);
		assert_eq!(terminal_failure(&parts, &[failed("old", hash)], "own", hash), Some((7,Some(9))));
		assert_eq!(terminal_failure(&parts, &[failed("own", hash)], "own", hash), Some((7,Some(9))));
		let mut pending = failed("old", hash);
		pending.status = ListpaysPaysStatus::Pending as i32;
		assert_eq!(terminal_failure(&parts, &[failed("own", hash),pending], "own", hash), None);
		let mut known = failed("own", hash);
		known.preimage = Some(vec![1;32]);
		assert_eq!(terminal_failure(&parts, &[known], "own", hash), None);
		assert_eq!(terminal_failure(&parts, &[failed("own", PaymentHash::from([18;32]))], "own", hash), None);
		let mut zero_index = parts[0].clone();
		zero_index.created_index = 0;
		assert_eq!(terminal_failure(&[zero_index], &[failed("own", hash)], "own", hash), None);
	}

	#[test]
	fn terminal_failure_allows_foreign_success_only_outside_the_failed_own_group() {
		let hash = PaymentHash::from([17;32]);
		let own = failed_part("own", hash, 1, 7);
		let mut foreign = failed_part("foreign", hash, 2, 11);
		foreign.status = ListsendpaysPaymentsStatus::Complete as i32;
		foreign.payment_preimage = Some(vec![1;32]);
		let mut paid = failed("foreign", hash);
		paid.status = ListpaysPaysStatus::Complete as i32;
		paid.created_index = Some(11);
		paid.preimage = Some(vec![1;32]);
		assert_eq!(terminal_failure(&[own.clone(),foreign.clone()],
			&[failed("own", hash),paid.clone()], "own", hash), Some((7,Some(9))));

		// Complete masks a live xpay object; no Failed own-group witness means HOLD.
		assert_eq!(terminal_failure(&[own.clone(),foreign.clone()], &[paid.clone()], "own", hash), None);
		foreign.groupid = own.groupid;
		// A newer raw completion also invalidates an earlier Failed aggregate.
		assert_eq!(terminal_failure(&[own.clone(),foreign.clone()], &[failed("own", hash)], "own", hash), None);
		foreign.created_index = 6;
		paid.created_index = Some(6);
		assert_eq!(terminal_failure(&[own.clone(),foreign], &[paid], "own", hash), None);

		// The first part may carry a foreign label, but the entire group must fail.
		let first = failed_part("foreign", hash, own.groupid, 6);
		let mut witness = failed("foreign", hash);
		witness.created_index = Some(6);
		assert_eq!(terminal_failure(&[first,own.clone()], &[witness], "own", hash), Some((6,Some(9))));
		assert_eq!(terminal_failure(&[own.clone(),failed_part("own", hash, 3, 12)],
			&[failed("own", hash)], "own", hash), None);
	}

	#[test]
	fn terminal_failure_rejects_hidden_own_pending_success_and_preimage() {
		let hash = PaymentHash::from([17;32]);
		let first = failed_part("foreign", hash, 1, 6);
		let mut own = failed_part("own", hash, 1, 7);
		let mut witness = failed("foreign", hash);
		witness.created_index = Some(6);
		for status in [ListsendpaysPaymentsStatus::Pending,ListsendpaysPaymentsStatus::Complete] {
			own.status = status as i32;
			assert_eq!(terminal_failure(&[first.clone(),own.clone()], &[witness.clone()], "own", hash), None);
		}
		own.status = ListsendpaysPaymentsStatus::Failed as i32;
		own.payment_preimage = Some(vec![1;32]);
		assert_eq!(terminal_failure(&[first,own], &[witness], "own", hash), None);
	}

	#[test]
	fn source_success_requires_the_full_principal_from_own_completed_parts() {
		let preimage = Preimage::from([1;32]);
		let hash = preimage.compute_payment_hash();
		let failed_retry = failed_part("own", hash, 1, 7);
		let mut first = failed_part("own", hash, 1, 11);
		first.status = ListsendpaysPaymentsStatus::Complete as i32;
		first.payment_preimage = Some(vec![1;32]);
		first.amount_msat = Some(cln_rpc::Amount {msat:70_000});
		let mut second = first.clone();
		second.created_index = 12;
		second.partid = Some(2);
		second.amount_msat = Some(cln_rpc::Amount {msat:30_000});
		let mut aggregate = failed("own", hash);
		aggregate.status = ListpaysPaysStatus::Complete as i32;
		aggregate.preimage = Some(vec![1;32]);
		aggregate.amount_msat = Some(cln_rpc::Amount {msat:100_000});
		assert_eq!(source_success(&[failed_retry.clone(),first.clone(),second.clone()],
			&aggregate, "own", hash, 100_000), Some(preimage));
		assert_eq!(source_success(&[failed_retry.clone(),first.clone()],
			&aggregate, "own", hash, 100_000), None);
		second.label = Some("foreign".into());
		assert_eq!(source_success(&[failed_retry.clone(),first.clone(),second.clone()],
			&aggregate, "own", hash, 100_000), None);
		second.label = Some("own".into());
		second.payment_preimage = Some(vec![2;32]);
		assert_eq!(source_success(&[failed_retry,first,second], &aggregate, "own", hash, 100_000), None);
	}

	#[test]
	fn source_success_rejects_own_failed_part_with_foreign_complete_in_the_same_group() {
		let hash = Preimage::from([1;32]).compute_payment_hash();
		let own = failed_part("own", hash, 1, 7);
		let mut foreign = failed_part("foreign", hash, 1, 11);
		foreign.status = ListsendpaysPaymentsStatus::Complete as i32;
		foreign.payment_preimage = Some(vec![1;32]);
		foreign.amount_msat = Some(cln_rpc::Amount {msat:100_000});
		let mut aggregate = failed("own", hash);
		aggregate.status = ListpaysPaysStatus::Complete as i32;
		aggregate.preimage = Some(vec![1;32]);
		aggregate.amount_msat = Some(cln_rpc::Amount {msat:100_000});
		assert_eq!(source_success(&[own,foreign], &aggregate, "own", hash, 100_000), None);
	}
}
