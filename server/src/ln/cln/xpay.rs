//! Manages outbound Lightning payments. Sends payments via CLN's `xpay` RPC
//! and tracks their status via `listsendpays` streams.
//!
//! ## Payment lifecycle
//!
//! [`ClnXpay::pay`] is fire-and-forget: it spawns a task that calls `xpay` over gRPC
//! and then reconciles the attempt against `listpays`. An attempt is only failed on
//! evidence that CLN cannot complete the payment: an xpay error that CLN only returns
//! before it sends any HTLC, or a reconciliation by the monitor once CLN stopped
//! retrying. Any other error, such as a dropped connection, leaves the attempt open.
//!
//! CLN only stops retrying a request it received. A request that CLN never
//! answered may still be on its way and start late, so the monitor fails it only
//! after its invoice expired, since CLN refuses to start an expired invoice.
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

use std::{cmp, fmt, str};
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
use crate::system::RuntimeManager;
use crate::telemetry;

use super::ClnGrpcClient;
use super::super::payment_handler::PaymentAttemptHandler;


/// The buffer we add to the xpay timeout before we check invoice
pub const XPAY_TIMEOUT_BUFFER: Duration = Duration::from_secs(15);

/// xpay error codes that CLN only returns before the payment sends its first
/// HTLC, so the payment can no longer succeed.
///
/// From CLN v26.06 `plugins/xpay/xpay.c`: invalid parameters and an expired
/// invoice are refused before the payment starts. A route-finding failure
/// keeps its own code only while no attempt was made; once an HTLC was sent,
/// every failure is reported as `PAY_UNSPECIFIED_ERROR` or as a destination
/// failure, and xpay can report those while other parts are still in flight.
const XPAY_ERRORS_BEFORE_ANY_HTLC: [i32; 3] = [
	-32602, // JSONRPC2_INVALID_PARAMS
	205, // PAY_ROUTE_NOT_FOUND
	207, // PAY_INVOICE_EXPIRED
];

/// The code lightningd answered an xpay call with, or `None` when the error
/// did not come from lightningd, as for a transport error.
fn xpay_error_code(err: &anyhow::Error) -> Option<i32> {
	// cln-grpc reports lightningd's error as the debug format of its
	// `RpcError`, which starts with the code CLN returned.
	let status = err.downcast_ref::<tonic::Status>()?;
	if status.code() != tonic::Code::Unknown {
		return None;
	}
	let rest = status.message().strip_prefix("Error calling method Xpay: RpcError { code: Some(")?;
	rest.split_once(')')?.0.parse::<i32>().ok()
}

/// The error message of an xpay call that CLN refused before it sent any
/// HTLC, or `None` when the error proves nothing about the payment, as for a
/// transport error while CLN may still be paying.
fn xpay_failed_before_any_htlc(err: &anyhow::Error) -> Option<String> {
	let code = xpay_error_code(err)?;
	XPAY_ERRORS_BEFORE_ANY_HTLC.contains(&code).then(|| err.downcast_ref::<tonic::Status>()
		.expect("lightningd errors are statuses").message().to_owned())
}

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
	) -> Arc<Self> {
		Arc::new(Self { db, payment_update_tx, rpc, settler, mailbox_manager })
	}

	/// Calls xpay and then reconciles the attempt status against CLN.
	///
	/// Only an xpay error that proves no HTLC was sent lets this reconciliation
	/// fail the attempt. After any other error CLN may still be paying, so the
	/// attempt stays open for the monitor.
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
		invoice: Box<Invoice>,
		payment_amount: Amount,
		max_routing_fee: Amount,
		max_cltv_expiry_delta: BlockDelta,
		retry_for: Duration,
	) {
		let mut rpc = self.rpc.clone();
		let payment_hash = invoice.payment_hash();
		let failure = match call_xpay(
			&mut rpc, &invoice, payment_amount, max_routing_fee, max_cltv_expiry_delta, retry_for,
		).await {
			Ok(_preimage) => {
				trace!("Payment successful for payment hash {}", payment_hash.as_hex());
				None
			},
			Err(pay_err) => {
				debug!("Error calling pay-command: {}", pay_err);
				Some((xpay_failed_before_any_htlc(&pay_err), xpay_error_code(&pay_err).is_some()))
			},
		};

		let attempt_res = self.db
			.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(payment_hash).await).await;

		match attempt_res {
			Ok(Some(attempt)) => {
				let evidence = match failure {
					Some((Some(ref error), _)) => FailureEvidence::Refused(error),
					Some((None, true)) => FailureEvidence::Answered,
					Some((None, false)) | None => FailureEvidence::Unproven,
				};
				if let Err(e) = self.sync_payment_attempt_status(attempt, evidence).await {
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

	/// Queries CLN's `listpays` for the given attempt and updates the DB to match.
	///
	/// A complete payment succeeds the attempt and a pending one keeps it
	/// open. CLN can report no payment, or only failed ones, while xpay has
	/// not sent its first HTLC yet or is between retries. The attempt is then
	/// marked `Failed` only with `evidence` that CLN can no longer be sending
	/// it. If CLN reports a different status than a final DB status, that is
	/// logged as an error. Sends on `payment_update_tx` when the status
	/// changes.
	pub async fn sync_payment_attempt_status(
		&self,
		attempt: LightningPaymentAttempt,
		evidence: FailureEvidence<'_>,
	) -> anyhow::Result<()> {
		let payment_hash = attempt.payment_hash;
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
					| LightningPaymentStatus::Submitted => match evidence {
					FailureEvidence::Unproven => {
						debug!("Lightning payment attempt ({}): CLN shows no payment for \
							payment hash {} yet; leaving it open",
							attempt.id, payment_hash,
						);
					},
					// CLN received the request: record that, so the monitor
					// counts its retry time from now.
					FailureEvidence::Answered => if attempt.status == LightningPaymentStatus::Requested {
						self.payment_handler().process_payment_attempt(
							&self.settler, &attempt, LightningPaymentStatus::Submitted, None, None, None,
						).await?;
					},
					FailureEvidence::Refused(error) => {
						self.payment_handler().fail_payment_attempt(&attempt, Some(error)).await?;
					},
					FailureEvidence::RetriesOver => {
						self.payment_handler().fail_payment_attempt(&attempt, None).await?;
					},
				},
			}
		} else {
			// A complete or pending payment outweighs any failed earlier one.
			let latest = listpays_response.pays.into_iter().max_by_key(|p| {
				let rank = match p.status() {
					ListpaysPaysStatus::Failed => 0,
					ListpaysPaysStatus::Pending => 1,
					ListpaysPaysStatus::Complete => 2,
				};
				(rank, p.created_index.expect("should have index"))
			}).expect("we have at least one");

			let updated_status = match latest.status() {
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
				ListpaysPaysStatus::Failed => match evidence {
					// xpay reports a failure before CLN marks every part failed,
					// and between retries all parts can be failed.
					FailureEvidence::Unproven => attempt.status,
					// CLN received the request: record that, so the monitor
					// counts its retry time from now.
					FailureEvidence::Answered => LightningPaymentStatus::Submitted,
					FailureEvidence::Refused(_) | FailureEvidence::RetriesOver =>
						LightningPaymentStatus::Failed,
				},
			};

			let error_string = latest.erroronion.as_ref().map(|b| {
				str::from_utf8(b).unwrap_or_else(|e| {
					warn!("Failed to decode erroronion from cln: '{}', {}", b.as_hex(), e);
					"failed to decode erroronion field"
				})
			});

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
						error_string,
						latest.amount_sent_msat.map(|v| v.msat),
						preimage,
					).await?;
				}
			}
		}

		Ok(())
	}
}

/// Why a reconciliation may fail an attempt that CLN shows nothing pending or
/// complete for.
#[derive(Debug, Clone, Copy)]
pub enum FailureEvidence<'a> {
	/// No proof: CLN may still be sending the payment.
	Unproven,
	/// No proof of failure, but lightningd answered the xpay call: CLN
	/// received the request, so it cannot start later.
	Answered,
	/// xpay refused the payment before sending any HTLC, with this error.
	Refused(&'a str),
	/// CLN stopped retrying the attempt and no delayed request can start it,
	/// see [ClnXpayProcess::retries_over].
	RetriesOver,
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
	) -> anyhow::Result<ClnXpay> {
		let payment_idxs = db.read(async |t| t.get_lightning_payment_indexes(node_id).await).await
			.with_context(|| format!("failed to fetch payment indices for {}", node_id))?
			.unwrap_or_default();

		slog!(XpayStarted,
			node_id: node_id,
			created_index: payment_idxs.created_index,
			updated_index: payment_idxs.updated_index,
		);

		let client = ClnXpayClient::new(db.clone(), payment_update_tx, rpc, settler, mailbox_manager).await;

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

	/// Whether CLN can no longer be paying the attempt, nor start it later.
	///
	/// CLN retries a request it received for the attempt's retry time, so once
	/// our status shows that CLN received it, that bounds the attempt. A request
	/// CLN never answered may still be on its way and can start whenever it
	/// arrives, unless its invoice expired by then: CLN refuses to start paying
	/// an expired invoice. So such an attempt is only over once its invoice
	/// expired and a request that arrived just before had its retry time.
	fn retries_over(attempt: &LightningPaymentAttempt, retry_for: Duration, now: DateTime<Local>) -> bool {
		let received_by = match attempt.status {
			LightningPaymentStatus::Submitted => attempt.updated_at,
			_ => match attempt.invoice_expires_at {
				Some(expires_at) => cmp::max(attempt.created_at, expires_at),
				// Older attempts did not store the expiry; they stay open.
				None => return false,
			},
		};
		received_by + retry_for + XPAY_TIMEOUT_BUFFER <= now
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
			let evidence = if Self::retries_over(&attempt, retry_for, Local::now()) {
				FailureEvidence::RetriesOver
			} else {
				FailureEvidence::Unproven
			};
			if let Err(e) = self.client.sync_payment_attempt_status(attempt, evidence).await {
				error!("Error syncing payment attempt status: {e:#}");
			} else {
				self.update_next_attempt_check(attempt_id);
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

/// Calls the xpay-command over gRPC.
/// If the payment completes successfully it will return the pre-image
/// Otherwise, an error will be returned
async fn call_xpay(
	rpc: &mut ClnGrpcClient,
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
		label: None,
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
mod tests {
	use super::*;

	fn cln_error(message: &str) -> anyhow::Error {
		tonic::Status::new(tonic::Code::Unknown, message).into()
	}

	#[test]
	fn only_errors_before_any_htlc_fail_the_payment() {
		// Errors as cln-grpc returned them on the CLN v26.06.6 test image.
		let refused = [
			"Error calling method Xpay: RpcError { code: Some(205), message: \"Failed: Unknown \
				source node 0356715caa742a65f95b8d7e8abe50265dbbe012b961beb73c667ee8ece9c104a3\", data: None }",
			"Error calling method Xpay: RpcError { code: Some(207), message: \"Invoice expired 2 \
				seconds ago\", data: None }",
			"Error calling method Xpay: RpcError { code: Some(-32602), message: \"Invalid bolt11 \
				invoice: Bad bech32 string\", data: None }",
		];
		for message in refused {
			assert_eq!(xpay_failed_before_any_htlc(&cln_error(message)).as_deref(), Some(message));
		}

		// A destination failure can arrive while other parts are in flight.
		let destination = cln_error("Error calling method Xpay: RpcError { code: Some(203), \
			message: \"Destination said it doesn't know invoice: incorrect_or_unknown_payment_details\", \
			data: None }");
		assert_eq!(xpay_failed_before_any_htlc(&destination), None);
		// Errors that did not come from lightningd prove nothing.
		let no_code = cln_error("Error calling method Xpay: RpcError { code: None, \
			message: \"reading response from socket\", data: None }");
		assert_eq!(xpay_failed_before_any_htlc(&no_code), None);
		let transport = tonic::Status::unavailable("error trying to connect: tcp connect error").into();
		assert_eq!(xpay_failed_before_any_htlc(&transport), None);
		let not_status = anyhow!("missing preimage");
		assert_eq!(xpay_failed_before_any_htlc(&not_status), None);
		// Only an answer from lightningd shows that CLN received the request.
		assert_eq!(xpay_error_code(&destination), Some(203));
		assert_eq!(xpay_error_code(&no_code), None);
		assert_eq!(xpay_error_code(&transport), None);
		assert_eq!(xpay_error_code(&not_status), None);
		// The code must be the one cln-grpc puts first, not text in a message.
		let spoofed = cln_error("Error calling method Xpay: RpcError { code: Some(209), \
			message: \"code: Some(205)\", data: None }");
		assert_eq!(xpay_failed_before_any_htlc(&spoofed), None);
	}

	#[test]
	fn unanswered_request_is_over_only_after_its_invoice_expired() {
		let created = Local::now();
		let retry_for = Duration::from_secs(5);
		let attempt = LightningPaymentAttempt {
			id: 1, lightning_node_id: 1, payment_hash: Preimage::from_slice(&[1; 32]).unwrap().compute_payment_hash(),
			amount_msat: 1_000_000, final_amount_msat: None, status: LightningPaymentStatus::Requested,
			lightning_htlc_subscription_id: None, error: None, block_height: None, user_fee: None,
			user_agent: None, retry_for: Some(retry_for),
			invoice_expires_at: Some(created + Duration::from_secs(600)),
			created_at: created, updated_at: created,
		};
		let over = |a: &LightningPaymentAttempt, secs: u64|
			ClnXpayProcess::retries_over(a, retry_for, created + Duration::from_secs(secs));
		// A request CLN never answered may arrive until its invoice expires.
		assert!(!over(&attempt, 20));
		assert!(!over(&attempt, 619));
		assert!(over(&attempt, 620));
		// Once CLN received it, its retry time bounds it.
		let received = LightningPaymentAttempt {
			status: LightningPaymentStatus::Submitted, updated_at: created + Duration::from_secs(10), ..attempt.clone()
		};
		assert!(!over(&received, 29));
		assert!(over(&received, 30));
		// Without a stored expiry, an unanswered request stays open.
		let older = LightningPaymentAttempt { invoice_expires_at: None, ..attempt };
		assert!(!over(&older, 1_000_000));
	}
}
