//! Authenticated account-to-output ownership at admission.

use std::collections::{BTreeMap, BTreeSet};

use bitcoin::bip32::ChildNumber;
use bitcoin::secp256k1::PublicKey;

use ark::{ProtocolEncoding, VtxoPolicy};
use ark::fallback::OwnerProof;
use server_rpc::protos;

use crate::database::{Tx, expiry_attribution::Attribution};
use crate::error::OwnerProofRefusal;
use crate::error::ContextExt;
use crate::{Server, SECP};

/// Transport metadata, kept outside the transaction and attestation encodings.
#[derive(Debug, Default, Clone)]
pub struct OwnerProofs(BTreeMap<PublicKey, OwnerProof>);

impl OwnerProofs {
	pub fn new() -> Self { Self::default() }

	pub fn insert(&mut self, key: PublicKey, proof: Option<OwnerProof>) -> anyhow::Result<()> {
		let Some(proof) = proof else { return Ok(()); };
		if let Some(previous) = self.0.get(&key) {
			if *previous != proof { return Err(OwnerProofRefusal::Invalid(key).into()); }
		} else {
			self.0.insert(key, proof);
		}
		Ok(())
	}

	pub fn from_arkoor_parts(parts: &[protos::ArkoorCosignRequest]) -> anyhow::Result<Self> {
		let mut ret = Self::new();
		for output in parts.iter().flat_map(|p| p.outputs.iter().chain(&p.isolated_outputs)) {
			let policy = VtxoPolicy::deserialize(&output.policy).badarg("invalid output policy")?;
			ret.insert_proto(policy.user_pubkey(), output.owner_proof.clone())?;
		}
		Ok(ret)
	}

	pub fn insert_proto(&mut self, key: PublicKey, proof: Option<protos::OwnerProof>) -> anyhow::Result<()> {
		let proof = parse_proof(key, proof)?;
		self.insert(key, proof)
	}
}

pub(crate) fn parse_proof(key: PublicKey, proof: Option<protos::OwnerProof>) -> anyhow::Result<Option<OwnerProof>> {
	proof.map(OwnerProof::try_from).transpose().map_err(|_| OwnerProofRefusal::Invalid(key).into())
}

impl Server {
	/// Verify the entire batch in one transaction. No partial new batch survives refusal.
	pub(crate) async fn verify_output_owners(
		&self,
		tx: &Tx<'_>,
		keys: impl IntoIterator<Item = PublicKey>,
		proofs: &OwnerProofs,
	) -> anyhow::Result<()> {
		let keys = keys.into_iter().collect::<BTreeSet<_>>();
		if let Some(key) = proofs.0.keys().find(|k| !keys.contains(*k)) {
			return Err(OwnerProofRefusal::Invalid(*key).into());
		}
		tx.lock_fallback_registry().await?;
		for key in keys {
			let Some(proof) = proofs.0.get(&key).copied() else {
				if matches!(tx.expiry_owner(key, self.config.network).await?, Attribution::Owned { .. }) {
					continue;
				}
				return Err(OwnerProofRefusal::Required(key).into());
			};
			proof.validate().map_err(|_| OwnerProofRefusal::Invalid(key))?;
			let child = ChildNumber::from_normal_idx(proof.child_index)
				.map_err(|_| OwnerProofRefusal::Invalid(key))?;
			let mailbox = self.unblind_mailbox_id(proof.blinded_mailbox_id, key)
				.map_err(|_| OwnerProofRefusal::Invalid(key))?.as_pubkey();
			let record = tx.get_fallback(mailbox).await?
				.ok_or(OwnerProofRefusal::Invalid(key))?;
			record.verify(self.config.network, mailbox).map_err(|_| OwnerProofRefusal::Invalid(key))?;
			let derived = record.account_xpub.ckd_pub(&SECP, child)
				.map_err(|_| OwnerProofRefusal::Invalid(key))?;
			if derived.public_key != key { return Err(OwnerProofRefusal::Invalid(key).into()); }
			tx.cache_expiry_owner(key, mailbox, proof.child_index).await?;
		}
		Ok(())
	}
}

/// The shared free-function tree fence also covers callers bypassing the Tx wrapper.
pub(crate) async fn require_mappings(
	tx: &tokio_postgres::Transaction<'_>, keys: &[PublicKey],
) -> anyhow::Result<()> {
	if keys.is_empty() { return Ok(()); }
	let keys = keys.iter().map(ToString::to_string).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
	if let Some(row) = tx.query_opt("SELECT k.user_pubkey FROM UNNEST($1::text[]) AS k(user_pubkey)
		LEFT JOIN expiry_owner_key o ON o.user_pubkey=k.user_pubkey WHERE o.user_pubkey IS NULL LIMIT 1", &[&keys]).await? {
		return Err(OwnerProofRefusal::Required(row.get::<_, &str>("user_pubkey").parse()?).into());
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use ark::mailbox::BlindedMailboxIdentifier;
	use ark::test_util::VTXO_VECTORS;

	#[test]
	fn isolated_outputs_and_repeated_keys_keep_the_same_evidence() {
		let key = VTXO_VECTORS.board_vtxo.user_pubkey();
		let proof = OwnerProof::new(BlindedMailboxIdentifier::from_pubkey(key), 1).unwrap();
		let output = protos::ArkoorDestination {
			total_amount: 1, policy: VtxoPolicy::new_pubkey(key).serialize(), owner_proof: Some(proof.into()),
		};
		let parts = [protos::ArkoorCosignRequest {
			outputs: vec![output.clone()], isolated_outputs: vec![output], ..Default::default()
		}];
		let parsed = OwnerProofs::from_arkoor_parts(&parts).unwrap();
		assert_eq!(parsed.0.len(), 1);
		assert_eq!(parsed.0[&key], proof);

		let mut conflicting = parts.clone();
		conflicting[0].isolated_outputs[0].owner_proof.as_mut().unwrap().child_index = 2;
		assert!(OwnerProofs::from_arkoor_parts(&conflicting).unwrap_err().is::<OwnerProofRefusal>());
		conflicting[0].isolated_outputs[0].owner_proof = None;
		assert_eq!(OwnerProofs::from_arkoor_parts(&conflicting).unwrap().0[&key], proof);
	}

	#[test]
	fn malformed_proof_has_an_output_key_refusal() {
		let key = VTXO_VECTORS.board_vtxo.user_pubkey();
		for proof in [
			protos::OwnerProof { blinded_mailbox_id: vec![0; 33], child_index: 1 },
			protos::OwnerProof { blinded_mailbox_id: key.serialize().to_vec(), child_index: 1 << 31 },
		] {
			let err = parse_proof(key, Some(proof)).unwrap_err();
			let refusal = err.downcast_ref::<OwnerProofRefusal>().unwrap();
			assert_eq!(refusal.reason(), "invalid_owner_proof");
			assert_eq!(refusal.key(), key);
		}
	}
}
