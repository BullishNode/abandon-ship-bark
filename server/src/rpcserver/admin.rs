//! The admin gRPC interface of captaind and watchmand.
//!
//! This interface deliberately has no authentication, authorization or transport encryption. It is
//! an operator plane as privileged as shell access on the host, kept unreachable by deployment and
//! not by the daemon.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{atomic, Arc};

use ark::VtxoId;
use tonic_tracing_opentelemetry::middleware::server::OtelGrpcLayer;
use tracing::{info, trace, warn};
use server_rpc::{self as rpc, protos};
use protos::expiry_settlement_request::Operation;
use protos::expiry_settlement_claim_result::Status as SettlementStatus;

use crate::rpcserver::{
	middleware, StatusContext, ToStatusResult,
	DEFAULT_HTTP2_MAX_PENDING_ACCEPT_RESET_STREAMS, RPC_RICH_ERRORS,
};
use crate::system::RuntimeManager;
use crate::Server;
use crate::database::expiry_settlement::{ClaimResult, SettlementVtxo};

impl From<SettlementVtxo> for protos::ExpirySettlementVtxo {
	fn from(v: SettlementVtxo) -> Self {
		Self { vtxo_id: v.id.to_bytes().to_vec(), vtxo: v.vtxo, expiry: v.expiry }
	}
}

fn settlement_ids(ids: &[Vec<u8>]) -> Result<Vec<VtxoId>, tonic::Status> {
	if !(1..=256).contains(&ids.len()) {
		return Err(tonic::Status::invalid_argument("expected 1..256 VTXO IDs"));
	}
	ids.iter().map(|id| VtxoId::from_slice(id).badarg("invalid VTXO ID")).collect()
}

#[async_trait]
impl rpc::server::ExpirySettlementAdminService for Server {
	async fn exchange(
		&self,
		req: tonic::Request<protos::ExpirySettlementRequest>,
	) -> Result<tonic::Response<protos::ExpirySettlementResponse>, tonic::Status> {
		let tip = self.chain_tip().height.to_u32();
		let mut response = protos::ExpirySettlementResponse {
			chain_tip: tip, ..Default::default()
		};
		match req.into_inner().operation.badarg("missing settlement operation")? {
			Operation::Page(p) => {
				if !(1..=256).contains(&p.limit) || p.min_amount_sat > i64::MAX as u64 {
					return Err(tonic::Status::invalid_argument("invalid page limit or amount"));
				}
				let after_id = if p.after_vtxo_id.is_empty() {
					String::new()
				} else {
					VtxoId::from_slice(&p.after_vtxo_id).badarg("invalid cursor ID")?.to_string()
				};
				response.vtxos = self.db.read(async |tx| tx.expiry_settlement_page(
					p.claimed, tip, p.grace_blocks, p.min_amount_sat,
					(p.after_expiry, after_id), p.limit,
				).await).await.to_status()?.into_iter().map(Into::into).collect();
			},
			Operation::Claim(c) => {
				for id in settlement_ids(&c.vtxo_ids)? {
					let result = self.db.claim_expired_vtxo(
						&self.vtxos_in_flux, id, tip, c.grace_blocks,
					).await.to_status()?;
					let (status, vtxo) = match result {
						ClaimResult::Claimed(v) => (SettlementStatus::Claimed, Some(v.into())),
						ClaimResult::Busy => (SettlementStatus::Busy, None),
						ClaimResult::Ineligible => (SettlementStatus::Ineligible, None),
					};
					response.claims.push(protos::ExpirySettlementClaimResult {
						vtxo_id: id.to_bytes().to_vec(), status: status as i32, vtxo,
					});
				}
			},
			Operation::Spenders(s) => {
				let ids = settlement_ids(&s.outpoints)?;
				let spenders = self.db.read(async |tx| tx.expiry_settlement_spenders(&ids).await)
					.await.to_status()?;
				response.spenders = ids.into_iter().zip(spenders).map(|(id, txid)| {
					protos::ExpirySettlementSpender { outpoint: id.to_bytes().to_vec(), txid }
				}).collect();
			},
		}
		Ok(tonic::Response::new(response))
	}
}

#[async_trait]
impl rpc::server::WalletAdminService for Server {
	#[tracing::instrument(skip(self, _req))]
	async fn wallet_status(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::WalletStatusResponse>, tonic::Status> {

		let rounds = self.rounds_wallet.lock().await.status();

		Ok(tonic::Response::new(protos::WalletStatusResponse {
			rounds: Some(rounds.into()),
			// the watchman wallet is managed by the watchmand process
			watchman: None,
		}))
	}
}

#[async_trait]
impl rpc::server::RoundAdminService for Server {
	#[tracing::instrument(skip(self, _req))]
	async fn trigger_round(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {

		match self.rounds.round_trigger_tx.try_send(()) {
			Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
				panic!("round scheduler closed");
			},
			Err(e) => warn!("Failed to send round trigger: {:?}", e),
			Ok(_) => trace!("round scheduler not closed"),
		}

		Ok(tonic::Response::new(protos::Empty{}))
	}
}

#[async_trait]
impl rpc::server::LightningAdminService for Server {
	#[tracing::instrument(skip(self, req))]
	async fn start_lightning_node(
		&self,
		req: tonic::Request<protos::LightningNodeUri>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();
		let uri = http::Uri::from_str(req.uri.as_str()).unwrap();
		let _ = self.lightning_manager.activate(uri);
		Ok(tonic::Response::new(protos::Empty{}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn stop_lightning_node(
		&self,
		req: tonic::Request<protos::LightningNodeUri>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();
		let uri = http::Uri::from_str(req.uri.as_str()).unwrap();
		let _ = self.lightning_manager.disable(uri);
		Ok(tonic::Response::new(protos::Empty{}))
	}
}

#[async_trait]
impl rpc::server::SweepAdminService for crate::watchman::Daemon {
	#[tracing::instrument(skip(self, _req))]
	async fn trigger_sweep(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {

		self.watchman_handle().trigger_sweep();
		Ok(tonic::Response::new(protos::Empty {}))
	}
}

#[async_trait]
impl rpc::server::NurseryAdminService for Server {
	#[tracing::instrument(skip(self, req))]
	async fn list_nursery_txs(
		&self,
		req: tonic::Request<protos::ListNurseryTxsRequest>,
	) -> Result<tonic::Response<protos::ListNurseryTxsResponse>, tonic::Status> {
		let req = req.into_inner();
		let txs = self.tx_nursery.list_txs(req.include_confirmed, req.include_abandoned).await
			.to_status()?;
		Ok(tonic::Response::new(protos::ListNurseryTxsResponse {
			txs: txs.into_iter().map(|r| protos::NurseryTxInfo {
				txid: r.tx.txid.to_string(),
				kind: r.tx.kind.name().into(),
				in_mempool: r.in_mempool,
				chunk_fee_rate_kwu: r.chunk_fee_rate.map(|f| f.to_sat_per_kwu()),
				confirm_target_height: r.tx.confirm_target_height.into(),
				confirmed_at_height: r.tx.confirmed_at_height.map(Into::into),
				created_at: r.tx.created_at.timestamp() as u64,
				abandoned_at: r.tx.abandoned_at.map(|t| t.timestamp() as u64),
			}).collect(),
		}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn abandon(
		&self,
		req: tonic::Request<protos::AbandonRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();
		let txid = bitcoin::Txid::from_str(&req.txid)
			.badarg("invalid txid")?;
		if self.tx_nursery.abandon(txid).await.to_status()? {
			Ok(tonic::Response::new(protos::Empty {}))
		} else {
			Err(tonic::Status::not_found("no active nursery tx with that txid"))
		}
	}
}

#[async_trait]
impl rpc::server::BanAdminService for Server {
	#[tracing::instrument(skip(self, req))]
	async fn ban_vtxo(
		&self,
		req: tonic::Request<protos::BanVtxoRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();
		let vtxo_id = VtxoId::from_slice(&req.vtxo_id)
			.badarg("invalid vtxo id")?;
		let chain_tip = self.chain_tip().height;
		let until_height = bitcoin_ext::BlockHeight::new(
			chain_tip.to_u32().saturating_add(req.ban_blocks),
		);
		self.db.write(async |t| t.ban_vtxo(vtxo_id, until_height).await).await.to_status()?;
		Ok(tonic::Response::new(protos::Empty {}))
	}

	#[tracing::instrument(skip(self, req))]
	async fn unban_vtxo(
		&self,
		req: tonic::Request<protos::UnbanVtxoRequest>,
	) -> Result<tonic::Response<protos::Empty>, tonic::Status> {
		let req = req.into_inner();
		let vtxo_id = VtxoId::from_slice(&req.vtxo_id)
			.badarg("invalid vtxo id")?;
		self.db.write(async |t| t.unban_vtxo(vtxo_id).await).await.to_status()?;
		Ok(tonic::Response::new(protos::Empty {}))
	}

	#[tracing::instrument(skip(self, _req))]
	async fn list_banned_vtxos(
		&self,
		_req: tonic::Request<protos::Empty>,
	) -> Result<tonic::Response<protos::ListBannedVtxosResponse>, tonic::Status> {
		let chain_tip = self.sync_manager.chain_tip().height;
		let banned = self.db.read(async |t| t.list_banned_vtxos(chain_tip).await).await.to_status()?;
		let banned_vtxos = banned.into_iter().map(|v| {
			protos::BannedVtxo {
				vtxo_id: v.vtxo_id.to_bytes().to_vec(),
				banned_until_height: v.banned_until_height.into(),
			}
		}).collect();
		Ok(tonic::Response::new(protos::ListBannedVtxosResponse { banned_vtxos }))
	}
}

/// Run the captaind admin gRPC endpoint.
pub async fn run_rpc_server(srv: Arc<Server>) -> anyhow::Result<()> {
	RPC_RICH_ERRORS.store(srv.config.rpc_rich_errors, atomic::Ordering::Relaxed);

	let _worker = srv.rtmgr.spawn_critical("AdminRpcServer");

	let addr = srv.config.rpc.admin_address.expect("shouldn't call this method otherwise");
	info!("Starting admin gRPC service on address {}", addr);

	let routes = tonic::service::Routes::default()
		.add_service(rpc::server::WalletAdminServiceServer::from_arc(srv.clone()))
		.add_service(rpc::server::RoundAdminServiceServer::from_arc(srv.clone()))
		.add_service(rpc::server::LightningAdminServiceServer::from_arc(srv.clone()))
		.add_service(rpc::server::BanAdminServiceServer::from_arc(srv.clone()))
		.add_service(rpc::server::ExpirySettlementAdminServiceServer::from_arc(srv.clone()))
		.add_service(rpc::server::NurseryAdminServiceServer::from_arc(srv.clone()));

	tonic::transport::Server::builder()
		.http2_max_pending_accept_reset_streams(Some(DEFAULT_HTTP2_MAX_PENDING_ACCEPT_RESET_STREAMS))
		.layer(OtelGrpcLayer::default())
		.layer(middleware::TelemetryMetricsLayer)
		.add_routes(routes)
		.serve_with_shutdown(addr, srv.rtmgr.shutdown_signal()).await?;

	info!("Terminated admin gRPC service on address {}", addr);

	Ok(())
}

/// Run the watchmand admin gRPC server, exposing only `SweepAdminService`.
pub async fn run_watchmand_admin_rpc_server(
	addr: SocketAddr,
	daemon: Arc<crate::watchman::Daemon>,
	rtmgr: RuntimeManager,
) -> anyhow::Result<()> {
	info!("Starting watchmand admin gRPC service on address {}", addr);

	let routes = tonic::service::Routes::default()
		.add_service(rpc::server::SweepAdminServiceServer::from_arc(daemon));

	tonic::transport::Server::builder()
		.http2_max_pending_accept_reset_streams(Some(DEFAULT_HTTP2_MAX_PENDING_ACCEPT_RESET_STREAMS))
		.layer(OtelGrpcLayer::default())
		.layer(middleware::TelemetryMetricsLayer)
		.add_routes(routes)
		.serve_with_shutdown(addr, rtmgr.shutdown_signal())
		.await?;

	info!("Terminated watchmand admin gRPC service on address {}", addr);

	Ok(())
}
