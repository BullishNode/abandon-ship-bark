//! Reconcile expired coins with the server.
//!
//! Registered fallback payouts arrive directly in the BIP84 on-chain wallet.
//! Status adoption atomically records the Ark debit once, without claiming a
//! payout destination or transaction from the spent status alone.

use log::warn;
use serde::{Deserialize, Serialize};

use ark::VtxoId;
use server_rpc::protos::VtxoSpendState;

use crate::Wallet;
use crate::vtxo::ServerStatusAdoption;

/// The state a VTXO has after [adopt_server_vtxo_status].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdoptedVtxoState {
	/// The server considers the VTXO spent; the wallet now has it spent too.
	Spent,
	/// The server will let us spend the VTXO.
	Spendable,
	/// The server does not know the VTXO's transaction chain yet.
	Unregistered,
	/// Anything else: the VTXO is locked, or still in another flow.
	Other,
}

impl AdoptedVtxoState {
	fn from_adoption(adoption: Option<ServerStatusAdoption>) -> Self {
		match adoption {
			Some(ServerStatusAdoption::Spent) => AdoptedVtxoState::Spent,
			Some(ServerStatusAdoption::Spendable) => AdoptedVtxoState::Spendable,
			Some(ServerStatusAdoption::InFlight(VtxoSpendState::Unregistered)) => {
				AdoptedVtxoState::Unregistered
			},
			Some(ServerStatusAdoption::InFlight(_)) | None => AdoptedVtxoState::Other,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedVtxoStatus {
	pub vtxo_id: VtxoId,
	pub state: AdoptedVtxoState,
}

/// Ask the server for the state of each VTXO in `vtxo_ids` and adopt it, see
/// [Wallet::trust_and_adopt_server_vtxo_status]. A VTXO the server reports
/// spent is marked spent, so it leaves the balance and coin selection.
///
/// NOTE: Only call this with a server you trust.
pub async fn adopt_server_vtxo_status(
	wallet: &Wallet,
	vtxo_ids: Vec<VtxoId>,
) -> anyhow::Result<Vec<AdoptedVtxoStatus>> {
	let mut ret = Vec::with_capacity(vtxo_ids.len());
	for vtxo_id in vtxo_ids {
		let adoption = wallet.trust_and_adopt_server_vtxo_status(vtxo_id).await?;
		ret.push(AdoptedVtxoStatus {
			vtxo_id,
			state: AdoptedVtxoState::from_adoption(adoption),
		});
	}
	Ok(ret)
}

impl Wallet {
	/// Reconcile expired coins before automatic refresh. An expired coin may
	/// already have been paid on-chain while the wallet was offline. Uncertain
	/// or in-flight states stay in the wallet but are excluded from this attempt.
	pub(crate) async fn sync_expired_vtxos(&self) -> anyhow::Result<Vec<VtxoId>> {
		let tip = self.chain().tip().await?;
		let mut unavailable = Vec::new();
		for vtxo in self.spendable_vtxos().await? {
			if vtxo.expiry_height() > tip { continue; }
			match self.trust_and_adopt_server_vtxo_status(vtxo.id()).await {
				Ok(Some(ServerStatusAdoption::Spendable)) => {},
				Ok(_) => unavailable.push(vtxo.id()),
				Err(e) => {
					warn!("Expired VTXO {} status unavailable; deferring refresh: {e:#}", vtxo.id());
					unavailable.push(vtxo.id());
				},
			}
		}
		Ok(unavailable)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn adopted_state_names() {
		assert_eq!(AdoptedVtxoState::from_adoption(None), AdoptedVtxoState::Other);
		assert_eq!(
			AdoptedVtxoState::from_adoption(Some(ServerStatusAdoption::InFlight(VtxoSpendState::Unregistered))),
			AdoptedVtxoState::Unregistered,
		);
		assert_eq!(serde_json::to_string(&AdoptedVtxoState::Spent).unwrap(), "\"spent\"");
	}
}
