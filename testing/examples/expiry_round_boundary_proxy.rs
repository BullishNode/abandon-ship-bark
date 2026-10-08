//! Replay an old round attempt immediately before a fresh one on private regtest.
//! All payment requests go to the real server without modification.
use std::io::Write;

use ark::rounds::RoundEvent;
use ark_testing::daemon::captaind::{ArkClient, MailboxClient};
use ark_testing::daemon::captaind::proxy::{ArkRpcProxy, ArkRpcProxyServer};
use futures::{Stream, StreamExt};
use server_rpc::{protos, RequestExt};

#[derive(Clone)]
struct Boundary;

#[async_trait::async_trait]
impl ArkRpcProxy for Boundary {
	async fn subscribe_rounds(&self, upstream: &mut ArkClient, req: protos::Empty) -> Result<Box<
		dyn Stream<Item = Result<protos::RoundEvent, tonic::Status>> + Unpin + Send + 'static
	>, tonic::Status> {
		let mut stream = upstream.subscribe_rounds(req).await?.into_inner();
		let (old, old_seq) = loop {
			let event = stream.message().await?.ok_or_else(|| tonic::Status::unavailable("stream ended"))?;
			if let Ok(RoundEvent::Attempt(attempt)) = RoundEvent::try_from(event.clone()) {
				break (event, attempt.round_seq);
			}
		};
		let fresh = loop {
			let event = stream.message().await?.ok_or_else(|| tonic::Status::unavailable("stream ended"))?;
			if let Ok(RoundEvent::Attempt(attempt)) = RoundEvent::try_from(event.clone()) {
				if attempt.round_seq > old_seq && attempt.attempt_seq == 0 {
					println!("{}", serde_json::json!({"event": "stale-attempt-replayed",
						"old_round": old_seq.to_string(), "current_round": attempt.round_seq.to_string()}));
					std::io::stdout().flush().unwrap();
					break event;
				}
			}
		};
		Ok(Box::new(futures::stream::iter([Ok(old), Ok(fresh)]).chain(stream)))
	}

	async fn submit_payment(&self, upstream: &mut ArkClient, req: protos::SubmitPaymentRequest)
		-> Result<protos::SubmitPaymentResponse, tonic::Status>
	{
		let result = upstream.submit_payment(req).await;
		println!("{}", serde_json::json!({"event": "submission-result", "accepted": result.is_ok(),
			"error": result.as_ref().err().map(|e| e.message())}));
		std::io::stdout().flush().unwrap();
		Ok(result?.into_inner())
	}
}

fn inject_pver(mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
	req.set_pver(server_rpc::MAX_PROTOCOL_VERSION);
	Ok(req)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let url = "http://127.0.0.1:48535";
	let channel = tonic::transport::Endpoint::from_static(url).connect().await?;
	let client = server_rpc::ArkServiceClient::with_interceptor(channel,
		inject_pver as fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status>);
	let mailbox = MailboxClient::connect(url).await?;
	let proxy = ArkRpcProxyServer::start((Boundary, client), ((), mailbox)).await;
	println!("{}", serde_json::json!({"event": "ready", "address": proxy.address}));
	std::io::stdout().flush()?;
	tokio::signal::ctrl_c().await?;
	let _ = proxy.stop.send(());
	Ok(())
}
