//! Bounded proof for an ordinary transfer, never a replacement obligation.

use anyhow::Context;
use bitcoin::OutPoint;

use ark::{ProtocolEncoding, ServerVtxoPolicy, VtxoId, VtxoPolicy};

use super::{Tx, VtxoState};

impl Tx<'_> {
	/// Only return a complete current-format ordinary OOR proof. A forfeit,
	/// conditional payment, partial registration or oversized branch set is not
	/// proof that the old user's financial claim has been discharged.
	pub(crate) async fn ordinary_spend_proof(&self, source: &VtxoState) -> anyhow::Result<Vec<Vec<u8>>> {
		ensure!(matches!(source.vtxo.policy(), VtxoPolicy::Pubkey(_))
			&& source.spent_in_round.is_none() && source.offboarded_in.is_none(), "source is not an ordinary OOR spend");
		let txid = source.oor_spent_txid.context("no ordinary source transaction")?;
		let stored = self.get_virtual_transaction_by_txid(txid).await?
			.context("ordinary source transaction is missing")?;
		let checkpoint = stored.signed_tx().context("ordinary source transaction is not signed")?;
		ensure!(checkpoint.input.len() == 1 && checkpoint.input[0].previous_output == source.vtxo.point(),
			"ordinary source linkage is incomplete");
		let count = checkpoint.output.len().checked_sub(1).context("empty ordinary checkpoint")?;
		ensure!(count > 0 && count <= server_rpc::MAX_ORDINARY_SPEND_PROOF_VTXOS,
			"ordinary proof exceeds branch budget");
		ensure!(checkpoint.output.last() == Some(&bitcoin_ext::fee::fee_anchor()), "ordinary checkpoint lacks its anchor");
		let script = ServerVtxoPolicy::new_checkpoint(source.vtxo.user_pubkey()).script_pubkey(
			source.vtxo.server_pubkey(), source.vtxo.exit_delta(), source.vtxo.expiry_height(),
		);
		ensure!(checkpoint.output[..count].iter().all(|output| output.script_pubkey == script),
			"ordinary source has conditional or unsupported branches");
		let mut proof = Vec::with_capacity(count);
		let mut bytes = 0usize;
		for index in 0..count {
			let point = OutPoint::new(txid, u32::try_from(index)?);
			let intermediate = self.get_bare_vtxo_by_id(VtxoId::from(point)).await?;
			ensure!(intermediate.spent_in_round.is_none() && intermediate.offboarded_in.is_none(), "checkpoint has another obligation");
			let spender = intermediate.oor_spent_txid.context("ordinary branch is incomplete")?;
			let transaction = self.get_virtual_transaction_by_txid(spender).await?
				.context("ordinary branch transaction is missing")?;
			let transaction = transaction.signed_tx().context("ordinary branch is not fully signed")?;
			ensure!(transaction.input.len() == 1 && transaction.input[0].previous_output == point
				&& transaction.output.len() == 2 && transaction.output[1] == bitcoin_ext::fee::fee_anchor(),
				"ordinary branch has unsupported geometry");
			let leaf = self.get_user_vtxo_by_id(VtxoId::from(OutPoint::new(spender, 0))).await?;
			ensure!(matches!(leaf.vtxo.policy(), VtxoPolicy::Pubkey(_)),
				"conditional branch is not an ordinary discharge");
			let encoded = leaf.vtxo.serialize();
			bytes = bytes.checked_add(encoded.len()).context("ordinary proof size overflow")?;
			ensure!(bytes <= server_rpc::MAX_ORDINARY_SPEND_PROOF_BYTES, "ordinary proof exceeds byte budget");
			proof.push(encoded);
		}
		Ok(proof)
	}
}
