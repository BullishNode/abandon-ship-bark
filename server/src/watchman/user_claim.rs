//! Retain confirmed user exit claims across shallow reorganizations.

use bitcoin::{Transaction, Witness};
use bitcoin::taproot::LeafVersion;
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use tracing::warn;

use ark::{ServerVtxo, ServerVtxoPolicy, VtxoPolicy};
use ark::vtxo::TapScriptClause;

use crate::bitcoind as bcd;

use super::Watchman;

pub(super) fn is_user_exit_claim(vtxo: &ServerVtxo, witness: &Witness) -> bool {
	let Some(leaf) = witness.taproot_leaf_script() else { return false; };
	if leaf.version != LeafVersion::TapScript { return false; }
	let script = match vtxo.policy() {
		ServerVtxoPolicy::User(VtxoPolicy::Pubkey(policy)) =>
			policy.user_pubkey_claim_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy)) =>
			policy.user_claim_after_expiry_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend_v0(policy)) =>
			policy.user_claim_after_expiry_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcRecv(policy)) =>
			policy.user_reveals_preimage_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcRecv_v0(policy)) =>
			policy.user_reveals_preimage_clause(vtxo.exit_delta()).tapscript(),
		_ => return false,
	};
	leaf.script == script.as_script()
}

impl Watchman {
	pub(crate) async fn rebroadcast_user_exit_claims(&self) -> anyhow::Result<()> {
		let claims = self.db.read(async |t| t.pending_user_exit_claims().await).await?;
		for claim in claims {
			if let Err(error) = self.rebroadcast_user_exit_claim(&claim).await {
				warn!(txid = %claim.compute_txid(), "Could not rebroadcast user exit claim: {error:#}");
			}
		}
		Ok(())
	}

	async fn rebroadcast_user_exit_claim(&self, claim: &Transaction) -> anyhow::Result<()> {
		// Include the mempool: an existing claim or a competing spend needs no replay.
		for input in &claim.input {
			if self.bitcoind.try_get_tx_out(input.previous_output, true).await?.is_none() {
				return Ok(());
			}
		}
		// Core checks the renewed CSV/CLTV maturity, scripts and all other inputs.
		let accepted = bcd::test_mempool_accept(&self.bitcoind, &[claim]).await?;
		if accepted.len() == 1 && accepted[0].txid == claim.compute_txid() && accepted[0].allowed {
			bcd::broadcast_tx(&self.bitcoind, claim).await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use bitcoin::Witness;
	use ark::test_util::dummy::DummyTestVtxoSpec;
	use super::*;

	#[test]
	fn retains_only_the_user_script_spend() {
		let (_, vtxo) = DummyTestVtxoSpec::default().build();
		let vtxo = ServerVtxo::from(vtxo).to_bare();
		let ServerVtxoPolicy::User(VtxoPolicy::Pubkey(policy)) = vtxo.policy() else { unreachable!(); };
		let script = policy.user_pubkey_claim_clause(vtxo.exit_delta()).tapscript();
		let control = vtxo.output_taproot().control_block(&(script.clone(), LeafVersion::TapScript))
			.unwrap().serialize();
		let signature = [0u8; 64];
		let witness = Witness::from_slice(&[signature.as_slice(), script.as_bytes(), &control]);
		assert!(is_user_exit_claim(&vtxo, &witness));
		assert!(!is_user_exit_claim(&vtxo, &Witness::from_slice(&[signature])));
		assert!(!is_user_exit_claim(&vtxo, &Witness::new()));

		let server_script = ark::vtxo::policy::clause::DelayedSignClause {
			pubkey: vtxo.server_pubkey(), block_delta: vtxo.exit_delta(),
		}.tapscript();
		let server_witness = Witness::from_slice(&[signature.as_slice(), server_script.as_bytes(), &control]);
		assert!(!is_user_exit_claim(&vtxo, &server_witness));
	}
}
