//! Fixed authenticated owning-Watchman RPC. A timeout is an unknown outcome:
//! callers retain the exact signed candidate/inputs and never sign a retry.
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use bitcoin::Transaction;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::Keypair;
use server_rpc::{admin::NativeFeeServiceClient, protos};

use super::ordinary_fee::OrdinaryFeePolicy;
use super::ordinary_fee_auth::{FeeReply, FeeRequest, FeeRequestAction, FeeServiceIdentity};
use super::ordinary_fee_publication::{PublicationPath, PublicationRecord};
use super::ordinary_fee_source::FinalFeeSource;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFeeOwnerConfig {
	pub address: SocketAddr,
	pub original_opening_sha256: sha256::Hash,
}
impl NativeFeeOwnerConfig {
	pub(crate) fn validate(&self) -> anyhow::Result<()> {
		ensure!(self.address.ip().is_loopback() && self.address.port() != 0
			&& self.original_opening_sha256 != sha256::Hash::all_zeros(),
			"native fee owner requires explicit loopback endpoint and original opening identity");
		Ok(())
	}
}

pub(crate) struct NativeFeeClient {
	channel: tonic::transport::Channel,
	identity: FeeServiceIdentity,
	policy: OrdinaryFeePolicy,
	server_key: crate::secret::Secret<Keypair>,
}
impl NativeFeeClient {
	pub(crate) fn new(config: &NativeFeeOwnerConfig, identity: FeeServiceIdentity,
		policy: OrdinaryFeePolicy, server_key: Keypair,
	) -> anyhow::Result<Self> {
		config.validate()?;
		ensure!(identity.opening_sha256 == config.original_opening_sha256
			&& identity.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?)
			&& identity.server_pubkey == server_key.public_key(),
			"native fee client differs from actual original policy/opening");
		let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{}", config.address))?
			.connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(30));
		Ok(Self { channel: endpoint.connect_lazy(), identity, policy, server_key: crate::secret::Secret::new(server_key) })
	}
	pub(crate) fn identity(&self) -> &FeeServiceIdentity { &self.identity }
	pub(crate) fn policy(&self) -> OrdinaryFeePolicy { self.policy }
	pub(crate) async fn request(&self, action: FeeRequestAction, purpose: PublicationPath,
		signed: &Transaction,
	) -> anyhow::Result<PublicationRecord> {
		ensure!(action != FeeRequestAction::Revalidate, "source-aware consumer required");
		let (original_request, response) = self.exchange(action, purpose, signed).await?;
		let record = FeeReply::authenticate(&response, &original_request, &self.identity)?;
		self.check_record(&record, purpose, signed)?;
		Ok(record)
	}
	pub(crate) async fn revalidate(&self, purpose: PublicationPath, signed: &Transaction)
		-> anyhow::Result<(PublicationRecord, FinalFeeSource)> {
		let (request, response) = self.exchange(FeeRequestAction::Revalidate, purpose, signed).await?;
		let (record, source) = FeeReply::authenticate_current(&response, &request, &self.identity)?;
		self.check_record(&record, purpose, signed)?;
		Ok((record, source))
	}
	async fn exchange(&self, action: FeeRequestAction, purpose: PublicationPath, signed: &Transaction)
		-> anyhow::Result<(Vec<u8>, Vec<u8>)> {
		let authenticated_request = FeeRequest::sign(action, purpose, &self.identity, signed, self.server_key.leak_ref())?;
		let mut client = NativeFeeServiceClient::new(self.channel.clone())
			.max_decoding_message_size(4 * 1024 * 1024)
			.max_encoding_message_size(2 * 1024 * 1024 + 16);
		let original_request = authenticated_request.clone();
		let request = protos::NativeFeeRequest { authenticated_request };
		let response = match action {
			FeeRequestAction::Reserve => client.reserve(request).await,
			FeeRequestAction::Verify => client.verify(request).await,
			FeeRequestAction::Revalidate => client.revalidate(request).await,
		}.context("native fee owner did not acknowledge original candidate; retain its signed bytes/input exclusions")?
			.into_inner().signed_owner_response;
		ensure!(response.len() <= 4 * 1024 * 1024, "native fee response exceeds proof bound");
		Ok((original_request, response))
	}
	fn check_record(&self, record: &PublicationRecord, purpose: PublicationPath, signed: &Transaction) -> anyhow::Result<()> {
		record.matches(signed)?;
		ensure!(record.entry(self.policy)?.purpose() == match purpose { PublicationPath::Round => "round", PublicationPath::Pool => "pool" },
			"native fee response has another publication purpose");
		Ok(())
	}
}
