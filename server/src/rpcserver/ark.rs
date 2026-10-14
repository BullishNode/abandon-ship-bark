
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic;
use std::time::{Duration, UNIX_EPOCH};

use ark::attestations::DelegatedRoundParticipationAttestation;
use ark::attestations::OffboardRequestAttestation;
use ark::attestations::VtxoStatusAttestation;
use ark::rounds::RoundAttemptAttestation;
use bitcoin::consensus::serialize;
use bitcoin::Txid;
use bitcoin::{Amount, OutPoint};
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::{rand, PublicKey};
use tokio::sync::oneshot;
use futures::StreamExt;
use tokio_stream::Stream;
use tonic::codec::CompressionEncoding;
use tower_http::cors::CorsLayer;
use tonic_tracing_opentelemetry::middleware::server::OtelGrpcLayer;
use tracing::info;

use ark::{musig, ProtocolEncoding, Vtxo, VtxoId, VtxoPolicy};
use ark::arkoor::package::ArkoorPackageCosignRequest;
use ark::mailbox::MailboxIdentifier;
use ark::forfeit::HashLockedForfeitBundle;
use ark::lightning::{
	offer_request_amount, Bolt12InvoiceExt, Invoice, Offer, PaymentHash, Preimage,
};
use ark::tree::signed::{LeafVtxoCosignRequest, UnlockHash, UnlockPreimage};
use ark::vtxo::Full;
use ark::vtxo::policy::check_block_height;
use bitcoin_ext::{BlockDelta, BlockHeight, TxStatus};
use server_rpc::{self as rpc, protos, RequestExt, TryFromBytes};
use crate::database::SpendState;
use crate::database::carrier_snapshot::BackingSnapshotLimits;
use crate::database::fallback::FallbackRecord;

use crate::database::rounds::StoredRoundOutput;
use crate::round::DelegatedInput;
use crate::round::SelfSignedInput;
use crate::Server;
use crate::rpcserver::{
	middleware,
	ReceiverExt,
	StatusContext,
	ToStatusResult,
	DEFAULT_HTTP2_MAX_PENDING_ACCEPT_RESET_STREAMS,
	MAX_PROTOCOL_VERSION,
	MIN_PROTOCOL_VERSION,
	RPC_RICH_ERRORS,
};
use crate::round::RoundInput;
use crate::rpcserver::macros;
use crate::telemetry;


/// Map an internal [`SpendState`] to the client-facing [`protos::VtxoSpendState`].
///
/// Returns `None` for server-internal states (pool, forfeit, connector). A user
/// can never prove ownership of such a vtxo, so the `get_vtxo_status` endpoint
/// reports those as not found rather than leaking their existence.
fn client_spend_state(spend_state: SpendState) -> Option<protos::VtxoSpendState> {
	match spend_state {
		SpendState::Spendable => Some(protos::VtxoSpendState::Spendable),
		SpendState::HtlcRecvUnclaimed => Some(protos::VtxoSpendState::HtlcRecvUnclaimed),
		SpendState::Spent => Some(protos::VtxoSpendState::Spent),
		SpendState::LnSpent => Some(protos::VtxoSpendState::Spent),
		SpendState::Unclaimed => Some(protos::VtxoSpendState::Unclaimed),
		SpendState::Unregistered => Some(protos::VtxoSpendState::Unregistered),
		SpendState::Pool
		| SpendState::RoundForfeit
		| SpendState::OffboardForfeit
		| SpendState::OffboardConnector => None,
	}
}

impl Server {
	/// A confirmed carrier retires this exact frozen allocation. Spent describes
	/// retirement, including forfeiture; it does not claim a customer payment.
	async fn expiry_allocation_confirmed(&self, id: VtxoId) -> anyhow::Result<bool> {
		let freeze_set = self.db.read(async |t| Ok(t.query_opt(
			"SELECT a.freeze_set FROM expiry_allocation a WHERE a.id=$1 AND EXISTS (
				SELECT 1 FROM expiry_carrier_allocation m JOIN expiry_carrier c ON c.txid=m.txid
				WHERE m.allocation_id=a.id AND c.confirmed_height IS NOT NULL)",
			&[&id.to_string()],
		).await?.map(|row| row.get::<_, String>("freeze_set")))).await?;
		let Some(freeze_set) = freeze_set else { return Ok(false); };
		// Reuse complete signed-version and authenticated-output validation. A
		// freeze, submission, or an unmapped transaction cannot retire the coin.
		let versions = self.db.read_direct_delivery(&freeze_set, self.config.network,
			BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 16 * 1024 * 1024 },
		).await?;
		let mut confirmed = versions.iter().filter_map(|v| v.recorded_confirmation.map(|b| (v, b)));
		let Some((winner, recorded)) = confirmed.next() else { return Ok(false); };
		ensure!(confirmed.next().is_none(), "multiple recorded expiry carrier winners");
		// The direct delivery record means an actual active-chain confirmation;
		// the legacy two-step sweep depth is not this route's adoption policy.
		Ok(crate::bitcoind::tx_status(&self.bitcoind, winner.tx.compute_txid()).await? == TxStatus::Confirmed(recorded))
	}
}

#[async_trait]
impl rpc::server::ArkService for Server {
	#[tracing::instrument(skip(self, req))]
	async fn handshake(
		&self,
		req: tonic::Request<protos::HandshakeRequest>,
	) -> Result<tonic::Response<protos::HandshakeResponse>, tonic::Status> {
		let req = req.into_inner();

		// Don't refuse unknown versions: attach a PSA nudge instead so
		// we don't lock out existing users.
		let class = telemetry::classify_bark_version(req.bark_version.as_deref());
		telemetry::count_bark_version(class);
		let version_psa = match class {
			telemetry::BarkVersionClass::Known(_) => None,
			telemetry::BarkVersionClass::Missing => Some(
				"Your bark client did not identify its version to this Ark server. \
				 You may be running an outdated build; please update your bark client \
				 or contact support if the problem persists.".to_string()
			),
			telemetry::BarkVersionClass::Unknown => Some(
				"Your bark client version is not recognised by this Ark server. \
				 You may be running an outdated or unofficial build; please update \
				 your bark client or contact support if the problem persists.".to_string()
			),
		};
		let psa = match (version_psa, self.config.handshake_psa.clone()) {
			(None, op) => op,
			(Some(v), None) => Some(v),
			(Some(v), Some(op)) => Some(format!("{}\n\n{}", v, op)),
		};

		let ret = protos::HandshakeResponse {
			min_protocol_version: MIN_PROTOCOL_VERSION,
			max_protocol_version: MAX_PROTOCOL_VERSION,
			psa,
		};
		Ok(tonic::Response::new(ret))
	}

	#[tracing::instrument(skip(self, _req))]
	async fn get_ark_info(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::ArkInfo>, tonic::Status> {

		Ok(tonic::Response::new(self.ark_info().into()))
	}

	#[tracing::instrument(skip(self, _req))]
	async fn get_offboard_fee_rate(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::OffboardFeeRateResponse>, tonic::Status> {
		let feerate = self.offboard_feerate();
		Ok(tonic::Response::new(protos::OffboardFeeRateResponse {
			sat_vkb: feerate.to_sat_per_kwu() * 4,
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn get_vtxo(
		&self,
		req: tonic::Request<protos::GetVtxoRequest>,
	) -> Result<tonic::Response<protos::GetVtxoResponse>, tonic::Status> {
		let req = req.into_inner();

		let id = VtxoId::from_bytes(req.vtxo_id)?;

		let vtxo_state = self.db.read(async |t| t.get_server_vtxo_by_id(id).await).await
			.to_status()?;

		let origin = self.db.read(async |tx| match vtxo_state.vtxo.policy().user_pubkey() {
			Some(key) => tx.fallback_origin(key).await,
			None => Ok(None),
		}).await.to_status()?;
		let response = protos::GetVtxoResponse {
			vtxo: vtxo_state.vtxo.serialize(),
			child_index: origin.map(|origin| origin.child_index()),
		};

		Ok(tonic::Response::new(response))
	}

	#[tracing::instrument(skip(self, req))]
	async fn get_vtxo_status(
		&self,
		req: tonic::Request<protos::GetVtxoStatusRequest>,
	) -> Result<tonic::Response<protos::GetVtxoStatusResponse>, tonic::Status> {
		let req = req.into_inner();

		let id = VtxoId::from_bytes(req.vtxo_id)?;
		let attestation = VtxoStatusAttestation::from_bytes(&req.attestation)?;

		let vtxo_state = self.db.read(async |t| t.get_server_vtxo_by_id(id).await).await
			.to_status()?;

		let spend_state = vtxo_state.spend_state;
		let user_vtxo = match vtxo_state.vtxo.try_into_user_vtxo() {
			Ok(v) => v,
			Err(_) => {
				macros::not_found!([id], "VTXO not found");
			},
		};

		attestation.verify(&user_vtxo)
			.badarg("invalid VTXO status attestation")?;

		let Some(mut spend_state) = client_spend_state(spend_state) else {
			macros::not_found!([id], "VTXO not found");
		};
		if spend_state != protos::VtxoSpendState::Spent && self.expiry_allocation_confirmed(id).await.to_status()? {
			spend_state = protos::VtxoSpendState::Spent;
		}

		Ok(tonic::Response::new(protos::GetVtxoStatusResponse {
			spend_state: spend_state as i32,
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn set_fallback(
		&self,
		req: tonic::Request<protos::SetFallbackRequest>,
	) -> Result<tonic::Response<protos::SetFallbackResponse>, tonic::Status> {
		let req = req.into_inner();
		let mailbox_pk = PublicKey::from_bytes(&req.mailbox_pk)?;
		let record = FallbackRecord::from_bytes(&req.record, mailbox_pk, self.config.network)
			.badarg("invalid fallback record")?;
		let stored = self.db.write(async |t| t.set_fallback(mailbox_pk, &record).await)
			.await.to_status()?;
		Ok(tonic::Response::new(protos::SetFallbackResponse {
			record: stored.to_bytes(),
		}))
	}

	// boarding

	#[tracing::instrument(skip(self, req), fields(
		amount = req.get_ref().amount,
		expiry_height = req.get_ref().expiry_height,
	))]
	async fn request_board_cosign(
		&self,
		req: tonic::Request<protos::BoardCosignRequest>,
	) -> Result<tonic::Response<protos::BoardCosignResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_BOARD_FUNDING_TX
			&& self.config.require_board_funding_tx
		{
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to board.");
		}

		let amount = Amount::from_sat(req.amount);
		let user_pubkey = PublicKey::from_bytes(&req.user_pubkey)?;
		let expiry_height = check_block_height(req.expiry_height)
			.map_err(|e| tonic::Status::invalid_argument(format!("expiry_height: {e}")))?;
		let utxo = OutPoint::from_bytes(&req.utxo)?;
		let pub_nonce = musig::PublicNonce::from_bytes(&req.pub_nonce)?;
		let funding_tx = if self.config.require_board_funding_tx {
			Some(bitcoin::Transaction::from_bytes(&req.funding_tx)?)
		} else {
			None
		};

		let hint = req.origin.context("missing account child origin").badarg("board origin")?.try_into()?;
		let origin = self.resolve_fallback_hint(hint, user_pubkey).badarg("invalid board origin")?;
		let resp = self.cosign_board(
			amount, user_pubkey, expiry_height, utxo, funding_tx.as_ref(), pub_nonce, origin,
		).await.to_status()?;

		Ok(tonic::Response::new(resp.into()))
	}

	/// Registers a board vtxo
	///
	/// This method is idempotent
	#[tracing::instrument(skip(self, req))]
	async fn register_board_vtxo(
		&self,
		req: tonic::Request<protos::BoardVtxoRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();

		let vtxo = <Vtxo<Full>>::deserialize(&req.board_vtxo[..])
			.map_err(|e| tonic::Status::invalid_argument(format!("invalid vtxo: {}", e)))?;
		self.register_board(vtxo).await.to_status()?;

		Ok(tonic::Response::new(protos::Empty {}))
	}

	/// Registers the signed transaction chains for VTXOs that already exist server-side.
	#[tracing::instrument(skip(self, req), fields(
		vtxo_count = req.get_ref().vtxos.len(),
	))]
	async fn register_vtxo_transactions(
		&self,
		req: tonic::Request<protos::RegisterVtxoTransactionsRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();

		if req.vtxos.is_empty() {
			macros::badarg!("no vtxos provided");
		}

		let vtxos = req.vtxos.iter()
			.map(|v| <Vtxo<Full>>::deserialize(&v[..]))
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| tonic::Status::invalid_argument(format!("invalid vtxo: {}", e)))?;

		Server::register_vtxo_transactions(self, &vtxos).await.to_status()?;

		Ok(tonic::Response::new(protos::Empty {}))
	}

	/// Deprecated path alias for `register_vtxo_transactions`, kept so
	/// pre-rename clients can still reach the RPC over the wire.
	#[tracing::instrument(skip(self, req), fields(
		vtxo_count = req.get_ref().vtxos.len(),
	))]
	async fn register_vtxos(
		&self,
		req: tonic::Request<protos::RegisterVtxoTransactionsRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();

		if req.vtxos.is_empty() {
			macros::badarg!("no vtxos provided");
		}

		let vtxos = req.vtxos.iter()
			.map(|v| <Vtxo<Full>>::deserialize(&v[..]))
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| tonic::Status::invalid_argument(format!("invalid vtxo: {}", e)))?;

		Server::register_vtxo_transactions(self, &vtxos).await.to_status()?;

		Ok(tonic::Response::new(protos::Empty {}))
	}

	// arkoor

	/// Handles an arkoor cosign request.
	#[tracing::instrument(skip(self, req))]
	async fn request_arkoor_cosign(
		&self,
		req: tonic::Request<protos::ArkoorPackageCosignRequest>,
	) -> Result<tonic::Response<protos::ArkoorPackageCosignResponse>, tonic::Status> {
		let request = req.into_inner();
		let hints = self.arkoor_origin_hints(&request).badarg("invalid output origins")?;
		let request = ArkoorPackageCosignRequest::try_from(request)
			.context("Failed to parse request")?;

		let response = self.cosign_oor(request, hints).await.to_status()?;
		Ok(tonic::Response::new(response.into()))
	}

	// lightning

	#[tracing::instrument(skip(self, req))]
	async fn request_lightning_pay_htlc_cosign(
		&self,
		req: tonic::Request<protos::LightningPayHtlcCosignRequest>,
	) -> Result<tonic::Response<protos::ArkoorPackageCosignResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to send Lightning payments.");
		}

		let wire = protos::ArkoorPackageCosignRequest { parts: req.parts };
		let hints = self.arkoor_origin_hints(&wire).badarg("invalid output origins")?;
		let cosign_requests = ArkoorPackageCosignRequest::try_from(wire)
			.context("Failed to parse request")?;

		let resp = self.request_lightning_pay_htlc_cosign(cosign_requests, hints)
			.await.context("error making payment")?;

		Ok(tonic::Response::new(resp.into()))
	}

	#[tracing::instrument(skip(self, req))]
	async fn initiate_lightning_payment(
		&self,
		req: tonic::Request<protos::InitiateLightningPaymentRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to send Lightning payments.");
		}

		let htlc_vtxo_ids = req.htlc_vtxo_ids.iter()
			.map(VtxoId::from_bytes)
			.collect::<Result<Vec<_>, _>>()?;

		let invoice = Invoice::from_str(&req.invoice).badarg("invalid invoice")?;
		let payment_amount = Amount::from_sat(req.payment_amount_sat);

		let mailbox_id = req.mailbox_id.as_deref()
			.map(ark::mailbox::MailboxIdentifier::deserialize)
			.transpose()
			.map_err(|_| tonic::Status::invalid_argument("invalid mailbox_id"))?;

		let retry_for = req.retry_for_secs.map(|s| Duration::from_secs(s as u64));

		self.initiate_lightning_payment(invoice, payment_amount, htlc_vtxo_ids, mailbox_id, retry_for)
			.await
			.to_status()?;
		Ok(tonic::Response::new(protos::Empty {}))
	}

	#[tracing::instrument(skip(self, req), fields(
		wait = req.get_ref().wait
	))]
	async fn check_lightning_payment(
		&self,
		req: tonic::Request<protos::CheckLightningPaymentRequest>,
	) -> Result<tonic::Response<protos::LightningPaymentStatus>, tonic::Status> {
		let req = req.into_inner();

		let payment_hash = PaymentHash::from_bytes(req.hash)?;
		let res = self.check_lightning_payment(payment_hash, req.wait).await.to_status()?;
		Ok(tonic::Response::new(protos::LightningPaymentStatus { payment_status: Some(res) }))
	}

	#[tracing::instrument(skip(self, req))]
	async fn request_lightning_pay_htlc_revocation(
		&self,
		req: tonic::Request<protos::ArkoorPackageCosignRequest>
	) -> Result<tonic::Response<protos::ArkoorPackageCosignResponse>, tonic::Status> {
		let wire = req.into_inner();
		let hints = self.arkoor_origin_hints(&wire).badarg("invalid output origins")?;
		let cosign_requests = ArkoorPackageCosignRequest::try_from(wire)
			.context("Failed to parse request")?;

		let cosign_resp = self.revoke_lightning_pay_htlcs(cosign_requests, hints).await
			.to_status()?;

		Ok(tonic::Response::new(cosign_resp.into()))
	}

	#[tracing::instrument(skip(self, req))]
	async fn fetch_bolt12_invoice(
		&self,
		req: tonic::Request<protos::FetchBolt12InvoiceRequest>,
	) -> Result<tonic::Response<protos::FetchBolt12InvoiceResponse>, tonic::Status> {
		let req = req.into_inner();

		let offer = match Offer::try_from(req.offer.to_vec()) {
			Ok(offer) => offer,
			Err(_) => {
				macros::badarg!("invalid offer");
			},
		};

		// NB the client derives the amount it authorizes the same way, and checks the
		// invoice we return against it, so both sides must agree on this.
		let amount = offer_request_amount(&offer, req.amount_sat.map(Amount::from_sat))
			.badarg("cannot determine the amount to request for this offer")?;

		let invoice = self.fetch_bolt12_invoice(offer, amount).await.to_status()?;

		Ok(tonic::Response::new(protos::FetchBolt12InvoiceResponse {
			invoice: invoice.bytes(),
		}))
	}

	#[tracing::instrument(skip(self, req), fields(
		amount_sats = ?req.get_ref().amount_sat
	))]
	async fn start_lightning_receive(
		&self,
		req: tonic::Request<protos::StartLightningReceiveRequest>,
	) -> Result<tonic::Response<protos::StartLightningReceiveResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to receive Lightning payments.");
		}

		let payment_hash = PaymentHash::from_bytes(req.payment_hash)?;
		let amount = Amount::from_sat(req.amount_sat);

		let mailbox_id = req.mailbox_id.as_deref()
			.map(ark::mailbox::MailboxIdentifier::deserialize)
			.transpose()
			.map_err(|_| tonic::Status::invalid_argument("invalid mailbox_id"))?;

		let resp = self.start_lightning_receive(
			payment_hash,
			amount,
			BlockDelta::try_from(req.min_cltv_delta).badarg("invalid min_cltv_delta")?,
			mailbox_id,
			req.description,
			req.description_hash,
		).await.to_status()?;

		Ok(tonic::Response::new(resp))
	}

	#[tracing::instrument(skip(self, req))]
	async fn check_lightning_receive(
		&self,
		req: tonic::Request<protos::CheckLightningReceiveRequest>,
	) -> Result<tonic::Response<protos::CheckLightningReceiveResponse>, tonic::Status> {
		let req = req.into_inner();

		let payment_hash = PaymentHash::from_bytes(req.hash)?;

		let sub = self.check_lightning_receive(payment_hash, req.wait).await.to_status()?;
		Ok(tonic::Response::new(sub.into()))
	}

	#[tracing::instrument(skip(self, req))]
	async fn prepare_lightning_receive_claim(
		&self,
		req: tonic::Request<protos::PrepareLightningReceiveClaimRequest>
	) -> Result<tonic::Response<protos::PrepareLightningReceiveClaimResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to receive Lightning payments.");
		}

		let payment_hash = PaymentHash::from_bytes(req.payment_hash)?;

		let user_pubkey = PublicKey::from_bytes(&req.user_pubkey)?;
		let htlc_recv_expiry = BlockHeight::new(req.htlc_recv_expiry);
		let hint = req.origin.badarg("missing account child origin")?.try_into()
			.badarg("invalid account child origin")?;
		let origin = self.resolve_fallback_hint(hint, user_pubkey).badarg("invalid grant origin")?;

		let (sub, htlcs) = self.prepare_lightning_claim(
			payment_hash, user_pubkey, origin, htlc_recv_expiry, req.lightning_receive_anti_dos,
		).await.to_status()?;

		Ok(tonic::Response::new(protos::PrepareLightningReceiveClaimResponse {
			receive: Some(sub.into()),
			htlc_vtxos: htlcs.into_iter().map(|v| v.serialize()).collect(),
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn claim_lightning_receive(
		&self,
		req: tonic::Request<protos::ClaimLightningReceiveRequest>
	) -> Result<tonic::Response<protos::ArkoorPackageCosignResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		let payment_hash = PaymentHash::from_bytes(req.payment_hash)?;

		let payment_preimage = Preimage::from_bytes(req.payment_preimage)?;
		let wire = req.cosign_request.badarg("cosign request missing")?;
		let hints = self.arkoor_origin_hints(&wire).badarg("invalid output origins")?;
		let cosign_request = ArkoorPackageCosignRequest::try_from(wire).badarg("invalid cosign request")?;

		let cosign_resp = self.claim_lightning_receive(
			payment_hash,
			payment_preimage,
			cosign_request,
			pver,
			hints,
		).await.to_status()?;

		Ok(tonic::Response::new(cosign_resp.into()))
	}

	#[tracing::instrument(skip(self, req))]
	async fn cancel_lightning_receive(
		&self,
		req: tonic::Request<protos::CancelLightningReceiveRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();

		let payment_hash = PaymentHash::from_bytes(req.payment_hash)?;

		self.cancel_lightning_receive(payment_hash).await.to_status()?;

		Ok(tonic::Response::new(protos::Empty {}))
	}

	// round

	#[tracing::instrument(skip(self, _req))]
	async fn next_round_time(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::NextRoundTimeResponse>, tonic::Status> {
		Ok(tonic::Response::new(protos::NextRoundTimeResponse {
			timestamp: self.rounds.next_round_time.read()
				.duration_since(UNIX_EPOCH).expect("we set this value beyond unix epoch")
				.as_secs()
		}))
	}

	type SubscribeRoundsStream = Pin<Box<
		dyn Stream<Item = Result<protos::RoundEvent, tonic::Status>> + Send + 'static
	>>;

	// Concurrency of open SubscribeRounds streams is bounded upstream by
	// the reverse proxy in front of captaind. Don't add a server-side
	// limit here.
	#[tracing::instrument(skip(self, _req))]
	async fn subscribe_rounds(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<Self::SubscribeRoundsStream>, tonic::Status> {
		let mgr = self.rtmgr.clone();
		let stream = self.rounds.events()
			.map(|e| Ok(e.as_ref().into()))
			.take_until(async move { mgr.shutdown_signal().await });
		Ok(tonic::Response::new(Box::pin(stream)))
	}

	#[tracing::instrument(skip(self, _req))]
	async fn last_round_event(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::RoundEvent>, tonic::Status> {
		if let Some(event) = self.rounds.last_event() {
			Ok(tonic::Response::new(event.as_ref().into()))
		} else {
			macros::not_found!([""], "no round event yet");
		}
	}

	#[tracing::instrument(skip(self, req), fields(
		input_vtxos_count = req.get_ref().input_vtxos.len(),
		vtxo_requests_count = req.get_ref().vtxo_requests.len()
	))]
	async fn submit_payment(
		&self,
		req: tonic::Request<protos::SubmitPaymentRequest>,
	) -> Result<tonic::Response<protos::SubmitPaymentResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to participate in rounds.");
		}

		let inputs =  req.input_vtxos.iter().map(|input| {
			let vtxo_id = VtxoId::from_bytes(&input.vtxo_id)?;
			let attestation = RoundAttemptAttestation::from_bytes(&input.attestation)?;
			Ok(SelfSignedInput { vtxo_id, attestation })
		}).collect::<Result<_, tonic::Status>>()?;

		let mut vtxo_requests = Vec::<ark::SignedVtxoRequest>::with_capacity(req.vtxo_requests.len());
		let mut output_origins = std::collections::HashMap::new();
		for signed in req.vtxo_requests {
			if signed.public_nonces.len() != self.config.nb_round_nonces {
				macros::badarg!("need exactly {} public nonces", self.config.nb_round_nonces);
			}
			let request = signed.vtxo.as_ref().context("missing signed output").badarg("round origin")?;
			let key = VtxoPolicy::deserialize(&request.policy).badarg("invalid round policy")?.user_pubkey();
			let hint = request.origin.clone().context("missing account child origin").badarg("round origin")?.try_into()?;
			let origin = self.resolve_fallback_hint(hint, key).badarg("invalid round origin")?;
			if let Some(previous) = output_origins.insert(key, origin) {
				if previous != origin { macros::badarg!("conflicting round origins for the same coin key"); }
			}
			vtxo_requests.push(signed.try_into().badarg("invalid signed vtxo request")?);
		}
		self.db.read(async |tx| {
			for (key, origin) in &output_origins { tx.verify_fallback_child(*origin, *key).await?; }
			Ok(())
		}).await.to_status()?;

		#[allow(deprecated)]
		if !req.offboard_requests.is_empty() {
			return Err(tonic::Status::unimplemented("offboards in rounds are no longer supported"));
		}

		let unlock_preimage = rand::random::<UnlockPreimage>();
		let unlock_hash = UnlockHash::hash(&unlock_preimage);

		let (tx, rx) = oneshot::channel();
		let inp = RoundInput::RegisterPayment {
			inputs,
			vtxo_requests,
			output_origins,
			unlock_preimage,
			user_agent_name: crate::telemetry::current_user_agent_name(),
		};

		self.rounds.round_input_tx.send((inp, tx))
			.expect("input channel closed");
		rx.wait_for_status().await?;

		Ok(tonic::Response::new(protos::SubmitPaymentResponse {
			unlock_hash: unlock_hash.to_byte_array().to_vec(),
		}))
	}

	#[tracing::instrument(skip(self, req), fields(
		signatures_count = req.get_ref().signatures.len()
	))]
	async fn provide_vtxo_signatures(
		&self,
		req: tonic::Request<protos::VtxoSignaturesRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to participate in rounds.");
		}

		let (tx, rx) = oneshot::channel();
		let inp = RoundInput::VtxoSignatures {
			pubkey: PublicKey::from_bytes(&req.pubkey)?,
			signatures: req.signatures.iter()
				.map(musig::PartialSignature::from_bytes)
				.collect::<Result<_, _>>()?,
		};

		self.rounds.round_input_tx.send((inp, tx)).expect("input channel closed");
		rx.wait_for_status().await?;

		Ok(tonic::Response::new(protos::Empty {}))
	}

	// hArk

	#[tracing::instrument(skip(self, req), fields(
		input_vtxos_count = req.get_ref().input_vtxos.len(),
		vtxo_requests_count = req.get_ref().vtxo_requests.len()
	))]
	async fn submit_round_participation(
		&self,
		req: tonic::Request<protos::RoundParticipationRequest>,
	) -> Result<tonic::Response<protos::RoundParticipationResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to participate in rounds.");
		}

		let inputs =  req.input_vtxos.iter().map(|input| {
			let vtxo_id = VtxoId::from_bytes(&input.vtxo_id)?;
			let attestation = DelegatedRoundParticipationAttestation::from_bytes(
				&input.attestation,
			)?;
			Ok(DelegatedInput { vtxo_id, attestation })
		}).collect::<Result<_, tonic::Status>>()?;

		let mut outputs = Vec::with_capacity(req.vtxo_requests.len());
		for request in req.vtxo_requests {
			let key = VtxoPolicy::deserialize(&request.policy).badarg("invalid round policy")?.user_pubkey();
			let hint = request.origin.clone().context("missing account child origin").badarg("round origin")?.try_into()?;
			let origin = self.resolve_fallback_hint(hint, key).badarg("invalid round origin")?;
			outputs.push(StoredRoundOutput {
				vtxo_request: request.try_into().badarg("invalid vtxo request")?, origin: Some(origin),
				unblinded_mailbox_id: Some(MailboxIdentifier::from_pubkey(origin.mailbox_pk())),
			});
		}

		let unlock_hash = self.register_delegated_round_participation(
			inputs, outputs, req.scheduled_height.map(BlockHeight::new),
		).await.to_status()?;

		Ok(tonic::Response::new(protos::RoundParticipationResponse {
			unlock_hash: unlock_hash.to_byte_array().to_vec(),
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn round_participation_status(
		&self,
		req: tonic::Request<protos::RoundParticipationStatusRequest>,
	) -> Result<tonic::Response<protos::RoundParticipationStatusResponse>, tonic::Status> {
		let req = req.into_inner();

		let unlock_hash = UnlockHash::from_bytes(&req.unlock_hash)?;
		let part = self.db.read(async |t| t.get_round_participation_by_unlock_hash(unlock_hash).await).await.to_status()?
			.not_found([unlock_hash], "round participation not found")?;
		let input_vtxo_ids = part.inputs.iter().map(|i| i.vtxo_id.to_bytes().to_vec()).collect();

		let res = if let Some(round_id) = part.round_id {
			//TODO(stevenroose) consider storing the new vtxos in the participation table
			// so that we don't have to create the entire cached tree here each time

			let round = self.db.read(async |t| t.get_round(round_id).await).await.to_status()?
				.context("our own db has unknown round")?;
			let round_funding_tx = Some(serialize(&round.funding_tx));

			let mut output_vtxos = Vec::with_capacity(part.outputs.len());
			// NB the round can predate the v1 hashlock clauses, so the tree's
			// hashlock version has to be detected to build the correct vtxos
			let tree = round.into_cached_tree().to_status()?;
			// NB bind every output to its own leaf: identical requests within
			// a participation correspond to distinct leaves in the tree
			let leaf_idxs = tree.spec.spec.leaf_idxs_for_participation(
				unlock_hash, part.outputs.iter().map(|o| &o.vtxo_request),
			).with_context(|| format!("participation outputs not in round {}", round_id))?;
			for idx in leaf_idxs {
				output_vtxos.push(tree.build_vtxo(idx).serialize());
			}

			if part.inputs.iter().all(|i| i.is_forfeited()) {
				protos::RoundParticipationStatusResponse {
					status: protos::RoundParticipationStatus::RoundPartReleased.into(),
					unlock_preimage: Some(part.unlock_preimage.leak_ref().to_vec()),
					input_vtxo_ids, round_funding_tx, output_vtxos,
				}
			} else {
				protos::RoundParticipationStatusResponse {
					status: protos::RoundParticipationStatus::RoundPartIssued.into(),
					unlock_preimage: None,
					input_vtxo_ids, round_funding_tx, output_vtxos,
				}
			}
		} else {
			protos::RoundParticipationStatusResponse {
				status: protos::RoundParticipationStatus::RoundPartPending.into(),
				input_vtxo_ids,
				round_funding_tx: None,
				unlock_preimage: None,
				output_vtxos: vec![],
			}
		};

		Ok(tonic::Response::new(res))
	}

	#[tracing::instrument(skip(self, req))]
	async fn request_leaf_vtxo_cosign(
		&self,
		req: tonic::Request<protos::LeafVtxoCosignRequest>,
	) -> Result<tonic::Response<protos::LeafVtxoCosignResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to participate in rounds.");
		}

		let vtxo_id = VtxoId::from_bytes(req.vtxo_id)?;

		let pub_nonce = musig::PublicNonce::from_bytes(req.public_nonce)?;
		let req = LeafVtxoCosignRequest { vtxo_id, pub_nonce };

		let resp = self.cosign_hashlocked_leaf_round(&req).await.to_status()?;
		Ok(tonic::Response::new(resp.into()))
	}

	#[tracing::instrument(skip(self, req))]
	async fn request_forfeit_nonces(
		&self,
		req: tonic::Request<protos::ForfeitNoncesRequest>,
	) -> Result<tonic::Response<protos::ForfeitNoncesResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to participate in rounds.");
		}

		if req.vtxo_ids.len() > rpc::MAX_NB_FORFEIT_NONCE_IDS {
			macros::badarg!("too many vtxo ids, max is {}", rpc::MAX_NB_FORFEIT_NONCE_IDS);
		}

		let unlock_hash = UnlockHash::from_bytes(req.unlock_hash)?;
		let vtxos = req.vtxo_ids.iter().map(|v| VtxoId::from_bytes(v))
			.collect::<Result<Vec<_>, _>>()?;

		let res = self.generate_forfeit_nonces(unlock_hash, &vtxos).await.to_status()?;
		Ok(tonic::Response::new(protos::ForfeitNoncesResponse {
			public_nonces: res.into_iter().map(|n| n.serialize().to_vec()).collect(),
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn forfeit_vtxos(
		&self,
		req: tonic::Request<protos::ForfeitVtxosRequest>,
	) -> Result<tonic::Response<protos::ForfeitVtxosResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		if pver < server_rpc::pver::PROTOCOL_VERSION_HASHLOCK_CLAUSES {
			macros::badarg!("Your client version is too old, you need to update \
				in order to be able to participate in rounds.");
		}

		let forfeits = req.forfeit_bundles.iter()
			.map(|v| HashLockedForfeitBundle::from_bytes(v))
			.collect::<Result<Vec<_>, _>>()?;

		let preimage = self.register_vtxo_forfeit(&forfeits).await.to_status()?;

		Ok(tonic::Response::new(protos::ForfeitVtxosResponse {
			unlock_preimage: preimage.to_vec(),
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn prepare_offboard(
		&self,
		req: tonic::Request<protos::PrepareOffboardRequest>,
	) -> Result<tonic::Response<protos::PrepareOffboardResponse>, tonic::Status> {
		let pver = req.pver()?;
		let req = req.into_inner();

		let request = req.offboard.badarg("missing offboard field")?.try_into()
			.badarg("invalid offboard request")?;

		if pver < server_rpc::pver::PROTOCOL_VERSION_OFFBOARD_FIX {
			if req.input_vtxo_ids.len() > 1 {
				return Err(tonic::Status::unavailable(
					"YOU NEED TO UPDATE YOUR WALLET VERSION FOR THIS OPERATION",
				));
			}
		}

		let input_vtxos = req.input_vtxo_ids.iter().map(|v| VtxoId::from_bytes(v))
			.collect::<Result<Vec<_>, _>>()?;
		let attestation = req.attestation.iter()
			.map(|v| OffboardRequestAttestation::from_bytes(v))
			.collect::<Result<Vec<_>, _>>()?;
		let resp = self.prepare_offboard(
			request, input_vtxos, attestation,
		).await.to_status()?;

		Ok(tonic::Response::new(protos::PrepareOffboardResponse {
			offboard_tx: serialize(&resp.offboard_tx),
			forfeit_cosign_nonces: resp.forfeit_cosign_nonces.into_iter()
				.map(|n| n.serialize().to_vec())
				.collect(),
		}))
	}

	#[tracing::instrument(skip(self, req), fields(
		offboard_txid = tracing::field::Empty,
		nonce_count = req.get_ref().user_nonces.len(),
		sig_count = req.get_ref().partial_signatures.len(),
	))]
	async fn finish_offboard(
		&self,
		req: tonic::Request<protos::FinishOffboardRequest>,
	) -> Result<tonic::Response<protos::FinishOffboardResponse>, tonic::Status> {
		let req = req.into_inner();

		let offboard_txid = Txid::from_bytes(req.offboard_txid)?;
		tracing::Span::current().record("offboard_txid", tracing::field::display(offboard_txid));
		let pub_nonces = req.user_nonces.iter()
			.map(musig::PublicNonce::from_bytes)
			.collect::<Result<Vec<_>, _>>()?;
		let partial_sigs = req.partial_signatures.iter()
			.map(musig::PartialSignature::from_bytes)
			.collect::<Result<Vec<_>, _>>()?;

		let tx = self.finish_offboard(offboard_txid, &pub_nonces, &partial_sigs).await.to_status()?;

		Ok(tonic::Response::new(protos::FinishOffboardResponse {
			signed_offboard_tx: serialize(&tx),
		}))
	}
}

/// Run the public gRPC endpoint.
pub async fn run_rpc_server(srv: Arc<Server>) -> anyhow::Result<()> {
	RPC_RICH_ERRORS.store(srv.config.rpc_rich_errors, atomic::Ordering::Relaxed);

	let _worker = srv.rtmgr.spawn_critical("PublicRpcServer");

	let addr = srv.config.rpc.public_address;
	info!("Starting public gRPC service on address {}", addr);

	let (_, health_server) = tonic_health::server::health_reporter();

	// send_compressed compresses responses only for clients that advertise
	// grpc-accept-encoding, so clients that don't are unaffected. This is
	// where nearly all the savings are. accept_compressed is kept for
	// forward-compat: no current client compresses its requests, but
	// supporting it now means a future client that does needs no server change.
	let routes = tonic::service::Routes::default()
		.add_service(rpc::server::ArkServiceServer::from_arc(srv.clone())
			.accept_compressed(CompressionEncoding::Zstd)
			.send_compressed(CompressionEncoding::Zstd))
		.add_service(rpc::server::MailboxServiceServer::from_arc(srv.clone())
			.accept_compressed(CompressionEncoding::Zstd)
			.send_compressed(CompressionEncoding::Zstd))
		.add_service(health_server);

	tonic::transport::Server::builder()
		.accept_http1(true)
		.http2_max_pending_accept_reset_streams(Some(
			srv.config.rpc.max_pending_accept_reset_streams
				.unwrap_or(DEFAULT_HTTP2_MAX_PENDING_ACCEPT_RESET_STREAMS)
		))
		.layer(CorsLayer::permissive())
		.layer(tonic_web::GrpcWebLayer::new())
		.layer(OtelGrpcLayer::default())
		.layer(middleware::TelemetryMetricsLayer)
		.add_routes(routes)
		.serve_with_shutdown(addr, srv.rtmgr.shutdown_signal()).await?;

	info!("Terminated public gRPC service on address {}", addr);

	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn client_spend_state_collapses_server_internal_states() {
		// Client-relevant states map to a concrete spend state.
		assert_eq!(client_spend_state(SpendState::Spendable), Some(protos::VtxoSpendState::Spendable));
		assert_eq!(client_spend_state(SpendState::HtlcRecvUnclaimed), Some(protos::VtxoSpendState::HtlcRecvUnclaimed));
		assert_eq!(client_spend_state(SpendState::Spent), Some(protos::VtxoSpendState::Spent));
		assert_eq!(client_spend_state(SpendState::LnSpent), Some(protos::VtxoSpendState::Spent));
		assert_eq!(client_spend_state(SpendState::Unclaimed), Some(protos::VtxoSpendState::Unclaimed));
		assert_eq!(client_spend_state(SpendState::Unregistered), Some(protos::VtxoSpendState::Unregistered));

		// Server-internal states (pool, forfeit, connector) collapse to `None`
		// so the endpoint reports them as not found.
		assert_eq!(client_spend_state(SpendState::Pool), None);
		assert_eq!(client_spend_state(SpendState::RoundForfeit), None);
		assert_eq!(client_spend_state(SpendState::OffboardForfeit), None);
		assert_eq!(client_spend_state(SpendState::OffboardConnector), None);
	}
}
