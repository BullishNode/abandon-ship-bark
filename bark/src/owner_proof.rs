//! Evidence that an output key belongs to a registered account.

use anyhow::Context;
use bitcoin::secp256k1::PublicKey;

use ark::{Address, ArkInfo, ProtocolEncoding, VtxoPolicy};
use ark::fallback::OwnerProof;
use server_rpc::client::ServerConnection;
use server_rpc::protos;

use crate::Wallet;
use crate::round::RoundParticipation;

/// Stable ownership refusals from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OwnerProofRefusal {
	#[error("This output needs an owner proof. Obtain an updated recipient address.")]
	Required,
	#[error("The server could not verify the output's registered owner.")]
	Invalid,
}

impl OwnerProofRefusal {
	pub(crate) fn from_status(status: &tonic::Status) -> Option<Self> {
		if status.code() != tonic::Code::InvalidArgument { return None; }
		match status.metadata().get("abandon-reason")?.to_str().ok()? {
			"owner_proof_required" => Some(Self::Required),
			"invalid_owner_proof" => Some(Self::Invalid),
			_ => None,
		}
	}

	pub(crate) fn from_error(error: &anyhow::Error) -> Option<Self> {
		if let Some(refusal) = error.downcast_ref::<Self>() { return Some(*refusal); }
		error.chain().find_map(|cause| cause.downcast_ref::<tonic::Status>()
			.and_then(Self::from_status))
	}
}

impl Wallet {
	/// Only fresh issuance requires upgraded enforcement. Saved actions and
	/// recovery keep using the ordinary connection and their retained evidence.
	pub(crate) async fn require_owner_proof_server(&self) -> anyhow::Result<(ServerConnection, ArkInfo)> {
		let (server, info) = self.require_server().await?;
		require_owner_proof_version(server.protocol_version().await)?;
		Ok((server, info))
	}

	/// Construct evidence for a key already retained in this wallet.
	pub(crate) async fn own_owner_proof(&self, key: PublicKey) -> anyhow::Result<OwnerProof> {
		self.fallback_destination().await?;
		let (index, keypair) = self.pubkey_keypair(&key).await?
			.with_context(|| format!("wallet has no derivation index for output key {}", key))?;
		let mailbox_key = match self.properties().await?.server_mailbox_pubkey {
			Some(key) => key,
			None => self.require_server().await?.1.mailbox_pubkey,
		};
		let blinded = self.mailbox_identifier().to_blinded(mailbox_key, &keypair)?;
		Ok(OwnerProof::new(blinded, index)?)
	}

	/// Attach metadata after the builder has split and ordered its outputs.
	/// Saved legacy actions can omit recipient metadata. The server then requires
	/// an authenticated cached binding. New payments validate the address first.
	pub(crate) async fn attach_package_owner_proofs(
		&self,
		request: &mut protos::ArkoorPackageCosignRequest,
		recipient: Option<(PublicKey, Option<OwnerProof>)>,
	) -> anyhow::Result<()> {
		for part in &mut request.parts {
			for output in part.outputs.iter_mut().chain(part.isolated_outputs.iter_mut()) {
				let policy = VtxoPolicy::deserialize(&output.policy)?;
				let key = policy.user_pubkey();
				let proof = match recipient {
					Some((recipient_key, proof)) if recipient_key == key => proof,
				_ => Some(self.own_owner_proof(key).await?),
				};
				output.owner_proof = proof.map(Into::into);
			}
		}
		Ok(())
	}

	pub(crate) async fn round_output_owner_proof(
		&self, participation: &RoundParticipation, key: PublicKey,
	) -> anyhow::Result<OwnerProof> {
		if let Some(proof) = participation.owner_proofs.get(&key) {
			proof.validate()?;
			return Ok(*proof);
		}
		self.own_owner_proof(key).await
	}

	pub(crate) async fn validate_round_owner_proofs(
		&self, participation: &RoundParticipation,
	) -> anyhow::Result<()> {
		for key in participation.owner_proofs.keys() {
			ensure!(participation.outputs.iter().any(|output| output.policy.user_pubkey() == *key),
				"owner proof does not identify a requested round output: {}", key);
		}
		for output in &participation.outputs {
			self.round_output_owner_proof(participation, output.policy.user_pubkey()).await?;
		}
		Ok(())
	}
}

fn require_owner_proof_version(version: u64) -> anyhow::Result<()> {
	ensure!(version >= server_rpc::pver::PROTOCOL_VERSION_OWNER_PROOF,
		"Upgrade the Ark server and reconnect the wallet before creating new coins.");
	Ok(())
}

pub(crate) fn recipient_owner_proof(address: &Address) -> anyhow::Result<OwnerProof> {
	address.owner_proof()?.context(OwnerProofRefusal::Required)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn new_issuance_requires_owner_enforcement_without_raising_recovery_minimum() {
		let required = server_rpc::pver::PROTOCOL_VERSION_OWNER_PROOF;
		assert!(server_rpc::MIN_PROTOCOL_VERSION < required);
		for version in server_rpc::MIN_PROTOCOL_VERSION..required {
			let error = require_owner_proof_version(version).unwrap_err().to_string();
			assert!(error.contains("Upgrade the Ark server"));
			assert!(error.contains("reconnect the wallet"));
		}
		require_owner_proof_version(required).unwrap();
		require_owner_proof_version(required + 1).unwrap();
	}

	#[test]
	fn ownership_refusal_requires_stable_metadata_and_rejection_status() {
		let mut status = tonic::Status::invalid_argument("diagnostic text can change");
		assert_eq!(OwnerProofRefusal::from_status(&status), None);
		status.metadata_mut().insert("abandon-reason", "owner_proof_required".parse().unwrap());
		assert_eq!(OwnerProofRefusal::from_status(&status), Some(OwnerProofRefusal::Required));
		let error = anyhow::Error::new(status).context("payment context");
		assert_eq!(OwnerProofRefusal::from_error(&error), Some(OwnerProofRefusal::Required));
		let mut unavailable = tonic::Status::unavailable("connection lost");
		unavailable.metadata_mut().insert("abandon-reason", "invalid_owner_proof".parse().unwrap());
		assert_eq!(OwnerProofRefusal::from_status(&unavailable), None);
	}
}
