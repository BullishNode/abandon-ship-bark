//! Arkoor send wallet action.
//!
//! Identity (`id`, `destination`, `amount`) and immutable parameters live
//! on [`ArkoorSend`] as top-level fields; the mutable bit is the [`Progress`] enum.

use std::time::Duration;

use anyhow::Context;
use bitcoin::Amount;
use bitcoin::hex::DisplayHex;
use log::{error, info, warn};

use ark::{ProtocolEncoding, Vtxo};
use ark::arkoor::ArkoorDestination;
use ark::vtxo::{Full, VtxoId};
use server_rpc::{protos, StatusExt};

use crate::Wallet;
use crate::actions::{Advance, AdvanceError, WalletAction, WalletActionId};
use crate::arkoor::{ArkoorCreateError, DeliveryOutcome, post_arkoor_to_mailboxes, split_change_amount};
use crate::movement::{MovementDestination, MovementId, MovementStatus};
use crate::movement::update::MovementUpdate;
use crate::subsystem::{ArkoorMovement, Subsystem};
use crate::vtxo::VtxoLockHolder;

/// How long to wait before re-attempting delivery in the
/// [`Progress::Delivery`] park path.
const DELIVERY_RETRY_BACKOFF: Duration = Duration::from_secs(60);

/// An in-flight arkoor payment to an [`ark::Address`], persisted
/// as a single checkpoint row and driven across crashes by the
/// executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkoorSend {
	// Immutable State:
	pub id: WalletActionId,
	pub destination: ark::Address,
	#[serde(with = "bitcoin::amount::serde::as_sat")]
	pub amount: Amount,
	pub input_vtxo_ids: Vec<VtxoId>,
	pub change_key_index: u32,
	/// Change piece amounts fixed at start. `None` when persisted by a
	/// pre-split bark, meaning one whole change output.
	#[serde(default, with = "crate::utils::serde::opt_amount_vec_sat")]
	pub change_pieces: Option<Vec<Amount>>,
	/// Expired destination outputs settled to the input owner before registration.
	#[serde(default, with = "bitcoin::amount::serde::as_sat")]
	pub refunded_amount: Amount,
	/// Change already settled on-chain; it must not be reintroduced as spendable.
	#[serde(default, with = "bitcoin::amount::serde::as_sat")]
	pub settled_change_amount: Amount,

	// Mutable state:
	pub progress: Progress,
}

impl ArkoorSend {
	pub fn id(&self) -> WalletActionId {
		self.id.clone()
	}
}

/// The four phases of an outgoing arkoor send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Progress {
	/// Inputs are locked and the change keypair is reserved.
	Cosigning,
	/// Cosign succeeded and the movement is recorded; pending registration of
	/// the signed vtxo transactions with the server.
	Registration {
		movement_id: MovementId,
		#[serde(with = "ark::encode::serde::vec")]
		signed_destination_vtxos: Vec<Vtxo<Full>>,
		#[serde(with = "ark::encode::serde::vec")]
		signed_change_vtxos: Vec<Vtxo<Full>>,
	},
	/// Registration succeeded; pending delivery of the signed vtxos to the
	/// recipient via the destination's mailbox mechanisms.
	Delivery {
		movement_id: MovementId,
		#[serde(with = "ark::encode::serde::vec")]
		signed_destination_vtxos: Vec<Vtxo<Full>>,
		#[serde(with = "ark::encode::serde::vec")]
		signed_change_vtxos: Vec<Vtxo<Full>>,
		/// Most recent reason a delivery pass parked. `None` until the first
		/// pass in which no mailbox accepted the post.
		last_park_error: Option<String>,
	},
	/// At least one delivery succeeded or the action was salvaged
	/// after retry exhaustion.
	Finalizing {
		movement_id: MovementId,
		#[serde(with = "ark::encode::serde::vec")]
		signed_change_vtxos: Vec<Vtxo<Full>>,
		/// `true` if at least one delivery mechanism acked the message or
		/// the address requested no delivery at all, `false` if we are
		/// finalizing post-retry-exhaustion to salvage the change.
		delivery_succeeded: bool,
	},
}

impl From<ArkoorCreateError> for AdvanceError {
	fn from(e: ArkoorCreateError) -> Self {
		match e {
			// Keep the cosign status typed so `is_server_rejection` can tell a
			// rejection (InvalidArgument/NotFound) from a transient failure.
			ArkoorCreateError::Cosign(status) => AdvanceError::Server(status),
			ArkoorCreateError::Other(err) => AdvanceError::Other(err),
		}
	}
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl WalletAction for ArkoorSend {
	fn id(&self) -> WalletActionId { ArkoorSend::id(self) }

	async fn advance(self, wallet: &Wallet) -> Result<Advance<Self>, AdvanceError> {
		let new_progress = match self.progress.clone() {
			Progress::Cosigning => run_cosign(wallet, &self).await?,
			Progress::Registration {
				movement_id, signed_destination_vtxos, signed_change_vtxos,
			} => {
				run_registration(
					wallet, &signed_destination_vtxos, &signed_change_vtxos,
				).await?;
				Progress::Delivery {
					movement_id,
					signed_destination_vtxos,
					signed_change_vtxos,
					last_park_error: None,
				}
			},
			Progress::Delivery {
				movement_id, signed_destination_vtxos, signed_change_vtxos,
				last_park_error: _,
			} => {
				let tip = wallet.chain().tip().await?;
				let (destination, refunded) = remove_settled_outputs(wallet, &signed_destination_vtxos, tip, false).await?;
				let (change, settled_change) = remove_settled_outputs(wallet, &signed_change_vtxos, tip, true).await?;
				if destination.len() != signed_destination_vtxos.len() || change.len() != signed_change_vtxos.len() {
					return Ok(Advance::Next(ArkoorSend {
						refunded_amount: self.refunded_amount.checked_add(refunded).context("refund amount overflow")?,
						settled_change_amount: self.settled_change_amount.checked_add(settled_change).context("settled change overflow")?,
						progress: Progress::Delivery {
							movement_id, signed_destination_vtxos: destination,
							signed_change_vtxos: change, last_park_error: None,
						},
						..self
					}));
				}
				// The receiver explicitly wants no delivery. We should honour it
				// Settled outputs have already reached the owner chosen at registration.
				if self.destination.delivery().is_empty() || signed_destination_vtxos.is_empty() {
					return Ok(Advance::Next(ArkoorSend {
						progress: Progress::Finalizing {
							movement_id,
							signed_change_vtxos,
							delivery_succeeded: true,
						},
						..self
					}));
				}

				let (mut srv, _) = wallet.require_server().await?;
				match post_arkoor_to_mailboxes(
					&mut srv, self.destination.delivery(), &signed_destination_vtxos,
				).await {
					DeliveryOutcome::AnySucceeded => Progress::Finalizing {
						movement_id,
						signed_change_vtxos,
						delivery_succeeded: true,
					},
					// The settled outputs are removed when the action resumes.
					DeliveryOutcome::AllFailed { summary } | DeliveryOutcome::ExpirySettled { summary } => {
						return Ok(Advance::Park {
							state: ArkoorSend {
								progress: Progress::Delivery {
									movement_id, signed_destination_vtxos,
									signed_change_vtxos,
									last_park_error: Some(summary.clone()),
								},
								..self
							},
							wake_after: Some(DELIVERY_RETRY_BACKOFF),
							error: Some(AdvanceError::Other(anyhow!(summary))),
						});
					},
				}
			},
			Progress::Finalizing { movement_id, signed_change_vtxos, delivery_succeeded } => {
				if !signed_change_vtxos.is_empty() {
					let tip = wallet.chain().tip().await?;
					let (change, settled_change) = remove_settled_outputs(wallet, &signed_change_vtxos, tip, true).await?;
					if change.len() != signed_change_vtxos.len() {
						return Ok(Advance::Next(ArkoorSend {
							settled_change_amount: self.settled_change_amount.checked_add(settled_change).context("settled change overflow")?,
							progress: Progress::Finalizing { movement_id, signed_change_vtxos: change, delivery_succeeded },
							..self
						}));
					}
				}
				finalize_arkoor_send(
					wallet, &self, movement_id,
					&signed_change_vtxos, delivery_succeeded,
				).await?;
				if self.refunded_amount != Amount::ZERO {
					return Ok(Advance::Failed(anyhow!(
						"Payment outputs expired before registration and were returned on-chain to the sender",
					)));
				}
				return Ok(Advance::Done);
			},
		};

		Ok(Advance::Next(ArkoorSend { progress: new_progress, ..self }))
	}

	/// The inputs. They stay locked, and the change is not stored, until the
	/// send finalizes, which spends the inputs and releases the change.
	fn pending_balance_vtxo_ids(&self) -> Vec<VtxoId> {
		self.input_vtxo_ids.clone()
	}

	async fn on_rejection(
		self,
		wallet: &Wallet,
		error: AdvanceError,
	) -> anyhow::Result<Advance<Self>> {
		match self.progress.clone() {
			Progress::Cosigning => {
				let id = self.id.clone();
				error!("arkoor send {} rejected during cosign: {:?}", id, error);
				if let Err(cancel_err) = wallet.stop_wallet_action(&id).await {
					warn!(
						"could not stop arkoor send action {} after rejection: {:#}",
						id, cancel_err,
					);
				}
				Ok(Advance::Failed(error.into()))
			},
			// Cosign already burned the inputs server-side and inserted the
			// destination vtxos as `Unregistered`. Registration is what flips
			// them to `Spendable` on the server; a stable rejection here means
			// the recipient can't spend via the server either. They CAN still
			// emergency exit from the signed transaction chain we hold, so
			// fall through to Delivery rather than foreclose that recovery
			// path by skipping the mailbox post.
			// Delivery separately reconciles expired outputs before posting: a
			// settlement rejection may instead mean the sender was refunded.
			Progress::Registration {
				movement_id, signed_destination_vtxos, signed_change_vtxos,
			} => {
				if matches!(&error, AdvanceError::Server(status) if status.is_expiry_settled()) {
					// Settlement can race the cached tip at the expiry boundary.
					wallet.chain().invalidate_caches().await;
				}
				Ok(Advance::Next(ArkoorSend {
					progress: Progress::Delivery {
						movement_id,
						signed_destination_vtxos,
						signed_change_vtxos,
						last_park_error: None,
					},
					..self
				}))
			},
			Progress::Delivery { .. } | Progress::Finalizing { .. } => {
				// Expiry reconciliation has not established every outcome. Keep
				// both the signed outputs and input locks until it can retry.
				Ok(Advance::Park {
					state: self, wake_after: Some(DELIVERY_RETRY_BACKOFF), error: Some(error),
				})
			},
		}
	}
}

/// Build a fresh [`ArkoorSend`] in [`Progress::Cosigning`]
pub(crate) async fn start_arkoor_send(
	wallet: &Wallet,
	destination: ark::Address,
	amount: Amount,
) -> anyhow::Result<ArkoorSend> {
	let _ = wallet.require_server().await?;
	wallet.validate_arkoor_address(&destination).await
		.context("invalid arkoor address")?;

	let (change_keypair, change_key_index) = wallet.derive_store_next_keypair().await
		.context("failed to derive arkoor change keypair")?;
	if destination.policy().user_pubkey() == change_keypair.public_key() {
		bail!("Cannot create arkoor to same address as change");
	}

	// The server refuses expired vtxos as arkoor inputs; they have to wait
	// for a refresh.
	let tip = wallet.inner.chain.tip().await?;
	let inputs = wallet.spend_input_selection().await?
		.expires_after(tip)
		.select(wallet.spendable_vtxos().await?, amount)?;
	let input_vtxo_ids = inputs.iter().map(|v| v.id()).collect::<Vec<_>>();

	let total_input = inputs.iter().map(|v| v.amount()).sum::<Amount>();
	let change = total_input.checked_sub(amount)
		.context("selected inputs don't cover amount")?;

	let id = new_action_id();
	wallet.lock_vtxos(
		inputs.iter(),
		Some(VtxoLockHolder::Action { id: id.clone() }),
	).await?;

	Ok(ArkoorSend {
		id,
		destination,
		amount,
		input_vtxo_ids,
		change_key_index,
		change_pieces: Some(split_change_amount(
			change, amount, wallet.config().change_vtxo_split_factor,
		)),
		refunded_amount: Amount::ZERO,
		settled_change_amount: Amount::ZERO,
		progress: Progress::Cosigning,
	})
}

/// Random 128-bit identifier hex-encoded for use as the action id.
fn new_action_id() -> String {
	rand::random::<[u8; 16]>().to_lower_hex_string()
}

/// Cosigning -> Registration. Cosigns the arkoor with the server and
/// records the movement.
async fn run_cosign(wallet: &Wallet, send: &ArkoorSend) -> Result<Progress, AdvanceError> {
	let _ = wallet.require_server().await?;

	let inputs = wallet.inner.db.get_wallet_vtxos(&send.input_vtxo_ids).await?;
	let change_keypair = wallet.peek_keypair(send.change_key_index).await
		.with_context(|| format!(
			"action {}: stored change_key_index {} not in keystore",
			send.id, send.change_key_index,
		))?;

	let dest = ArkoorDestination {
		total_amount: send.amount,
		policy: send.destination.policy().clone(),
	};
	let neg_amount = -send.amount.to_signed().context("amount out-of-range")?;

	// `?` converts via `From<ArkoorCreateError>` below: a cosign failure
	// becomes `AdvanceError::Server` so the executor can route a genuine
	// rejection to on_rejection instead of retrying forever.
	let arkoor = wallet.create_checkpointed_arkoor_with_vtxos(
		dest, inputs, change_keypair, send.change_pieces.clone(),
	).await?;

	let initial_update = MovementUpdate::new()
		.intended_and_effective_balance(neg_amount)
		.consumed_vtxos(&arkoor.inputs)
		.sent_to([MovementDestination::ark(send.destination.clone(), send.amount)]);

	let movement_id = wallet.inner.movements.get_or_create_movement_with_action(
		Subsystem::ARKOOR,
		ArkoorMovement::Send.to_string(),
		&send.id,
		initial_update,
	).await.context("failed to create arkoor movement")?;

	Ok(Progress::Registration {
		movement_id,
		signed_destination_vtxos: arkoor.created,
		signed_change_vtxos: arkoor.change,
	})
}

/// Registration -> Delivery. Push the signed transaction chains for the
/// cosigned output vtxos to the server so receivers don't have to re-register
/// them on receive and spends don't have to lazily retry the registration.
async fn run_registration(
	wallet: &Wallet,
	signed_destination_vtxos: &[Vtxo<Full>],
	signed_change_vtxos: &[Vtxo<Full>],
) -> Result<(), AdvanceError> {
	let serialized: Vec<Vec<u8>> = signed_destination_vtxos.iter()
		.chain(signed_change_vtxos.iter())
		.map(|v| v.serialize().to_vec())
		.collect();
	if serialized.is_empty() {
		return Ok(());
	}

	let (mut srv, _) = wallet.require_server().await?;
	// Call the RPC directly rather than going through
	// `wallet.register_vtxo_transactions_with_server` so we preserve the typed
	// `tonic::Status` for `is_server_rejection`, instead of letting it get
	// wrapped in an opaque `anyhow::Error` that would always retry.
	srv.client.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
		vtxos: serialized,
	}).await.map_err(AdvanceError::Server)?;
	Ok(())
}

/// Registration is atomic per request. Retry expired outputs individually so
/// one settled leaf cannot obscure the other leaves' outcomes. Only the explicit
/// settlement refusal establishes a payout; an ordinary rejection or outage
/// leaves the checkpoint intact. Stored signatures then distinguish a recipient
/// payment (including a lost registration reply) from an input-owner refund.
async fn remove_settled_outputs(
	wallet: &Wallet, vtxos: &[Vtxo<Full>], tip: bitcoin_ext::BlockHeight, change: bool,
) -> Result<(Vec<Vtxo<Full>>, Amount), AdvanceError> {
	let mut remaining = Vec::new();
	let mut removed_amount = Amount::ZERO;
	for vtxo in vtxos {
		// Finalization may have stored change before a crash. Ordinary status
		// adoption owns that coin's debit; counting it here too would debit twice.
		if vtxo.expiry_height() > tip
			|| (change && wallet.inner.db.get_wallet_vtxo(vtxo.id()).await?.is_some()) {
			remaining.push(vtxo.clone());
			continue;
		}
		match run_registration(wallet, std::slice::from_ref(vtxo), &[]).await {
			Ok(()) => remaining.push(vtxo.clone()),
			Err(AdvanceError::Server(status)) if status.is_expiry_settled() => {
				let stored = wallet.fetch_vtxo(vtxo.id()).await?;
				if stored.to_bare() != vtxo.to_bare() {
					return Err(anyhow!("settled VTXO differs from saved payment").into());
				}
				if stored.has_all_witnesses() { wallet.validate_vtxo(&stored).await.map_err(AdvanceError::Vtxo)?; }
				if change || !stored.has_all_witnesses() {
					removed_amount = removed_amount.checked_add(vtxo.amount()).context("settled output amount overflow")?;
				}
			},
			Err(err) => return Err(err),
		}
	}
	Ok((remaining, removed_amount))
}

/// Finalize the send. All steps are idempotent.
async fn finalize_arkoor_send(
	wallet: &Wallet,
	send: &ArkoorSend,
	movement_id: MovementId,
	signed_change_vtxos: &[Vtxo<Full>],
	delivery_succeeded: bool,
) -> Result<(), AdvanceError> {
	wallet.mark_vtxos_as_spent(&send.input_vtxo_ids).await?;

	if !signed_change_vtxos.is_empty() {
		wallet.store_spendable_vtxos(signed_change_vtxos.iter()).await?;
		let change_ids = signed_change_vtxos.iter()
			.map(|v| v.id())
			.collect::<Vec<_>>();
		wallet.inner.movements.update_movement(
			movement_id,
			MovementUpdate::new().produced_vtxos(&change_ids),
		).await.context("failed to record arkoor change vtxos on movement")?;
	}

	let payment_succeeded = delivery_succeeded && send.refunded_amount == Amount::ZERO;
	let final_status = if payment_succeeded {
		MovementStatus::Successful
	} else {
		MovementStatus::Failed
	};
	let paid = send.amount.checked_sub(send.refunded_amount).context("refund exceeds payment")?;
	let debit = send.amount.checked_add(send.settled_change_amount).context("settled send amount overflow")?;
	let destinations = if paid == Amount::ZERO { Vec::new() }
		else { vec![MovementDestination::ark(send.destination.clone(), paid)] };
	let mut update = MovementUpdate::new()
		.effective_balance(-debit.to_signed().context("settled debit out-of-range")?)
		.replace_sent_on(destinations);
	if send.refunded_amount != Amount::ZERO || send.settled_change_amount != Amount::ZERO {
		update = update.metadata([
			("attempted_destination".into(), send.destination.to_string().into()),
			("expiry_refunded_principal_sat".into(), send.refunded_amount.to_sat().into()),
			("expiry_settled_change_sat".into(), send.settled_change_amount.to_sat().into()),
		]);
	}
	wallet.inner.movements.finish_movement_with_update(movement_id, final_status, update).await
		.context("failed to finalize arkoor movement")?;

	if payment_succeeded {
		info!("Successfully sent arkoor vtxos");
	}

	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	fn dummy_send() -> ArkoorSend {
		use std::str::FromStr;
		ArkoorSend {
			id: new_action_id(),
			destination: ark::Address::from_str(
				"tark1pwh9vsmezqqpharv69q4z8m6x364d5m5prnmcalcalq9pdmzw0y7mpveck4pcfhezqypczkrrj3lkx5ue4qrf4jc7ztpt9htdttmh2judhqnu7aue8p0y9mq47jn9z",
			).unwrap(),
			amount: Amount::from_sat(10_000),
			input_vtxo_ids: vec![],
			change_key_index: 0,
			change_pieces: Some(vec![Amount::from_sat(5_000), Amount::from_sat(5_000)]),
			refunded_amount: Amount::ZERO,
			settled_change_amount: Amount::ZERO,
			progress: Progress::Cosigning,
		}
	}

	/// A checkpoint written before change_pieces existed must deserialize
	/// with `None`, which rebuilds the single whole change output that
	/// version cosigned.
	#[test]
	fn checkpoint_without_change_pieces_reads_as_none() {
		let mut json = serde_json::to_value(&dummy_send()).unwrap();
		json.as_object_mut().unwrap().remove("change_pieces").unwrap();
		let old = serde_json::from_value::<ArkoorSend>(json).unwrap();
		assert_eq!(old.change_pieces, None);
	}

	#[test]
	fn change_pieces_roundtrip() {
		let send = dummy_send();
		let json = serde_json::to_value(&send).unwrap();
		assert_eq!(serde_json::from_value::<ArkoorSend>(json).unwrap(), send);
	}

	/// A cosign rejection must reach the executor as `AdvanceError::Server`
	/// so `is_server_rejection` routes it to `on_rejection` instead of the
	/// transient-retry path. Guards against regressing to a `.context(..)?`
	/// that would flatten the status into `Other`.
	#[test]
	fn cosign_rejection_classified_as_server_rejection() {
		let status = tonic::Status::new(tonic::Code::InvalidArgument, "vtxo already spent");
		let advance: AdvanceError = ArkoorCreateError::Cosign(status).into();
		assert!(matches!(advance, AdvanceError::Server(_)));
		assert!(advance.is_server_rejection());
	}

	/// A transient cosign failure is still a `Server` error but must NOT be
	/// classified as a rejection, so the executor retries it.
	#[test]
	fn transient_cosign_failure_is_not_a_rejection() {
		let status = tonic::Status::new(tonic::Code::Unavailable, "server restarting");
		let advance: AdvanceError = ArkoorCreateError::Cosign(status).into();
		assert!(matches!(advance, AdvanceError::Server(_)));
		assert!(!advance.is_server_rejection());
	}

	/// Non-cosign failures stay opaque `Other` (transient-retry path).
	#[test]
	fn other_create_error_is_not_a_rejection() {
		let advance: AdvanceError = ArkoorCreateError::Other(anyhow!("db error")).into();
		assert!(matches!(advance, AdvanceError::Other(_)));
		assert!(!advance.is_server_rejection());
	}
}
