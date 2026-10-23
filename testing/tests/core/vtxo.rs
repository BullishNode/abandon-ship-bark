//! Signed VTXO validation used by integration tests.

use std::str::FromStr;

use anyhow::{bail, ensure, Context};
use bitcoin::consensus::encode::deserialize_hex;

use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;

fn verify_vtxo(vtxo: &str, funding_tx: &str, address: Option<&str>) -> anyhow::Result<Vtxo<Full>> {
	let vtxo = <Vtxo<Full>>::deserialize_hex(vtxo).context("invalid vtxo")?;
	let funding_tx: bitcoin::Transaction = deserialize_hex(funding_tx)
		.context("invalid funding transaction")?;
	// Validation checks the output and signatures. Bind the supplied transaction
	// to the declared anchor as well, including VTXOs with no genesis transitions.
	if funding_tx.compute_txid() != vtxo.chain_anchor().txid {
		bail!("funding transaction ID does not match the VTXO chain anchor");
	}
	vtxo.validate(&funding_tx).context("VTXO signed path failed validation")?;
	if let Some(address) = address {
		let address = ark::Address::from_str(address).context("invalid Ark address")?;
		ensure!(address.is_for_server(vtxo.server_pubkey()), "Ark address is for another server");
		ensure!(address.policy() == vtxo.policy(), "signed coin does not pay the Ark address policy");
	}
	Ok(vtxo)
}

#[cfg(test)]
mod tests {
	use bitcoin::consensus::encode::serialize_hex;
	use ark::ProtocolEncoding;
	use ark::test_util::VTXO_VECTORS;

	use super::verify_vtxo;

	#[test]
	fn verify_vtxo_checks_signed_path_and_anchor_identity() {
		let vectors = &*VTXO_VECTORS;
		let funding = serialize_hex(&vectors.anchor_tx);
		let vtxo = &vectors.arkoor2_vtxo;
		assert_eq!(verify_vtxo(&vtxo.serialize_hex(), &funding, None).unwrap().id(), vtxo.id());

		let mut invalid = vtxo.clone();
		invalid.invalidate_final_sig();
		let error = verify_vtxo(&invalid.serialize_hex(), &funding, None).unwrap_err();
		assert!(format!("{error:#}").contains("invalid signature"));

		// Keep the funding outputs unchanged but change its transaction ID.
		let mut unrelated = vectors.anchor_tx.clone();
		unrelated.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
		let error = verify_vtxo(&vtxo.serialize_hex(), &serialize_hex(&unrelated), None).unwrap_err();
		assert!(error.to_string().contains("transaction ID does not match"));
	}

	#[test]
	fn verify_vtxo_binds_the_signed_recipient_and_server() {
		let vectors = &*VTXO_VECTORS;
		let funding = serialize_hex(&vectors.anchor_tx);
		let vtxo = &vectors.arkoor2_vtxo;
		let address = ark::Address::new(true, vtxo.server_pubkey(), vtxo.policy().clone(), vec![]);
		assert!(verify_vtxo(&vtxo.serialize_hex(), &funding, Some(&address.to_string())).is_ok());

		let wrong_payee = ark::Address::new(true, vtxo.server_pubkey(), vectors.board_vtxo.policy().clone(), vec![]);
		assert!(verify_vtxo(&vtxo.serialize_hex(), &funding, Some(&wrong_payee.to_string())).is_err());
		let wrong_server = ark::Address::new(true, vectors.arkoor3_user_key.public_key(), vtxo.policy().clone(), vec![]);
		assert!(verify_vtxo(&vtxo.serialize_hex(), &funding, Some(&wrong_server.to_string())).is_err());
	}
}
