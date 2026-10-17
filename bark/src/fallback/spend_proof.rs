//! An ordinary signed transfer is evidence; a server Spent flag is not.

use std::collections::HashSet;

use anyhow::Context;
use bitcoin::{Amount, Transaction};

use ark::{ProtocolEncoding, ServerVtxoPolicy, Vtxo, VtxoPolicy};
use ark::attestations::VtxoStatusAttestation;
use ark::vtxo::{Full, Policy};
use server_rpc::{protos, MAX_ORDINARY_SPEND_PROOF_BYTES, MAX_ORDINARY_SPEND_PROOF_VTXOS};

use crate::Wallet;

/// Check the entire current-format ordinary transfer, including change. All
/// VTXO signatures must also be validated against the actual funding chain by
/// the caller. Unsupported isolation/conditional branches deliberately fail.
fn verify_ordinary_branches(source: &Vtxo<Full>, leaves: &[Vtxo<Full>]) -> anyhow::Result<()> {
	ensure!(matches!(source.policy(), VtxoPolicy::Pubkey(_)), "conditional source cannot prove ordinary discharge");
	ensure!(!leaves.is_empty() && leaves.len() <= MAX_ORDINARY_SPEND_PROOF_VTXOS,
		"ordinary proof has an invalid branch count");
	let prefix = source.transactions().collect::<Vec<_>>();
	let checkpoint_script = ServerVtxoPolicy::new_checkpoint(source.user_pubkey()).script_pubkey(
		source.server_pubkey(), source.exit_delta(), source.expiry_height(),
	);
	let mut checkpoint: Option<Transaction> = None;
	let mut covered = HashSet::new();
	let mut total = Amount::ZERO;
	for leaf in leaves {
		ensure!(matches!(leaf.policy(), VtxoPolicy::Pubkey(_)) && leaf.chain_anchor() == source.chain_anchor()
			&& leaf.server_pubkey() == source.server_pubkey() && leaf.exit_delta() == source.exit_delta()
			&& leaf.expiry_height() == source.expiry_height(), "ordinary proof changes source policy domain");
		let path = leaf.transactions().collect::<Vec<_>>();
		ensure!(path.len() == prefix.len().checked_add(2).context("ordinary path length overflow")?
			&& path[..prefix.len()] == prefix, "ordinary proof is not a direct source descendant");
		let first = &path[prefix.len()].tx;
		ensure!(first.input.len() == 1 && first.input[0].previous_output == source.point()
			&& first.output.len() == leaves.len().checked_add(1).context("ordinary branch count overflow")?
			&& first.output.last() == Some(&bitcoin_ext::fee::fee_anchor())
			&& first.output[..leaves.len()].iter().all(|output| output.script_pubkey == checkpoint_script),
			"ordinary source has conditional, missing or unsupported branches");
		if let Some(prior) = &checkpoint {
			ensure!(prior == first, "ordinary branches disagree on the source spend");
		} else {
			checkpoint = Some(first.clone());
		}
		let last = &path[prefix.len() + 1].tx;
		ensure!(last.input.len() == 1 && last.input[0].previous_output.txid == first.compute_txid()
			&& last.output.len() == 2 && last.output[0] == leaf.txout()
			&& last.output[1] == bitcoin_ext::fee::fee_anchor()
			&& leaf.point().txid == last.compute_txid() && leaf.point().vout == 0,
			"ordinary final branch is not the claimed leaf");
		let index = usize::try_from(last.input[0].previous_output.vout)?;
		let output = first.output.get(index).context("ordinary branch lies outside its checkpoint")?;
		ensure!(index < leaves.len() && covered.insert(index) && output.value == leaf.amount(),
			"ordinary branch is repeated or fails to conserve its value");
		total = total.checked_add(leaf.amount()).context("ordinary proof amount overflow")?;
	}
	ensure!(covered.len() == leaves.len() && total == source.amount(),
		"ordinary proof does not cover all original principal");
	Ok(())
}

fn decode_ordinary_proof(encoded: &[Vec<u8>]) -> anyhow::Result<Vec<Vtxo<Full>>> {
	ensure!(!encoded.is_empty() && encoded.len() <= MAX_ORDINARY_SPEND_PROOF_VTXOS,
		"ordinary proof exceeds branch budget");
	let bytes = encoded.iter().try_fold(0usize, |size, bytes| size.checked_add(bytes.len()))
		.context("ordinary proof size overflow")?;
	ensure!(bytes <= MAX_ORDINARY_SPEND_PROOF_BYTES, "ordinary proof exceeds byte budget");
	Ok(encoded.iter().map(|bytes| Vtxo::<Full>::deserialize(bytes)).collect::<Result<Vec<_>, _>>()?)
}

impl Wallet {
	pub(crate) async fn verify_reported_ordinary_spend(&self, source: &Vtxo<Full>) -> anyhow::Result<()> {
		let key = self.get_vtxo_key(source).await?;
		let (mut server, _) = self.require_server().await?;
		let request = protos::GetVtxoStatusRequest {
			vtxo_id: source.id().to_bytes().to_vec(),
			attestation: VtxoStatusAttestation::new(source.id(), &key).serialize(),
		};
		let response = server.client.get_vtxo_spend_proof(request).await?.into_inner();
		let leaves = decode_ordinary_proof(&response.vtxos)?;
		// One actual funding lookup bounds chain I/O independently of branch count.
		let funding = self.inner.chain.get_tx(&source.chain_anchor().txid).await?
			.context("ordinary proof funding transaction is unavailable")?;
		source.validate(&funding)?;
		for leaf in &leaves { leaf.validate(&funding)?; }
		verify_ordinary_branches(source, &leaves)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ordinary_proof_rejects_empty_and_excess_branches_before_decoding() {
		assert!(decode_ordinary_proof(&[]).unwrap_err().to_string().contains("branch budget"));
		assert!(decode_ordinary_proof(&vec![vec![]; MAX_ORDINARY_SPEND_PROOF_VTXOS + 1])
			.unwrap_err().to_string().contains("branch budget"));
	}

	#[test]
	fn ordinary_proof_rejects_excess_bytes_before_decoding() {
		assert!(decode_ordinary_proof(&[vec![0; MAX_ORDINARY_SPEND_PROOF_BYTES + 1]])
			.unwrap_err().to_string().contains("byte budget"));
	}

	#[test]
	fn ordinary_proof_rejects_malformed_body_without_retirement_authority() {
		assert!(decode_ordinary_proof(&[vec![0]]).is_err());
	}
}
