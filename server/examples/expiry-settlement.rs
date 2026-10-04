//! Small operator client for the experimental settlement endpoint.
//! cargo run -p bark-server --example expiry-settlement -- URL page|claimed|claim|spenders [VTXO_ID...]

use ark::VtxoId;
use server_rpc::admin::ExpirySettlementAdminServiceClient;
use server_rpc::protos::{self, expiry_settlement_request::Operation};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let url = args.next().ok_or_else(|| anyhow::anyhow!("missing admin URL"))?;
	let op = args.next().ok_or_else(|| anyhow::anyhow!("missing operation"))?;
	let ids = args.map(|id| Ok(id.parse::<VtxoId>()?.to_bytes().to_vec()))
		.collect::<anyhow::Result<Vec<_>>>()?;
	let operation = match op.as_str() {
		"page" | "claimed" => Operation::Page(protos::ExpirySettlementPage {
			claimed: op == "claimed", grace_blocks: 144, min_amount_sat: 330,
			limit: 256, ..Default::default()
		}),
		"claim" => Operation::Claim(protos::ExpirySettlementClaim { vtxo_ids: ids, grace_blocks: 144 }),
		"spenders" => Operation::Spenders(protos::ExpirySettlementSpenders { outpoints: ids }),
		_ => anyhow::bail!("expected page, claimed, claim or spenders"),
	};
	let channel = server_rpc::tonic::transport::Endpoint::from_shared(url)?.connect().await?;
	let response = ExpirySettlementAdminServiceClient::new(channel)
		.exchange(protos::ExpirySettlementRequest { operation: Some(operation) }).await?
		.into_inner();
	println!("{response:#?}");
	Ok(())
}
