//! Shared expiry source eligibility, registration locks and retained payment recovery.

use ark::{ProtocolEncoding, ServerVtxo, VtxoId, Vtxo};
use ark::lightning::PaymentHash;
use ark::vtxo::policy::ServerVtxoPolicy;
use bitcoin::Transaction;
use bitcoin::consensus::deserialize;
use bitcoin::secp256k1::PublicKey;

use super::Tx;

#[derive(Debug, Clone, Copy)]
pub struct ExpiryInput {
	pub id: VtxoId,
	pub source: ExpirySource,
	pub owner: PublicKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpirySource {
	Registered, PendingBoard, Unregistered, LightningReceive(PaymentHash), LightningSend(PaymentHash),
}

/// Expired, unsettled coins and the source that decides their payee: `$1` is
/// the grace period and `$2` the tip. A coin that is spent, exited, offboarded
/// or in a live exchange is not a candidate. Scan and commit share this rule.
pub(super) const CANDIDATES: &str = "SELECT v.vtxo_id, v.vtxo, v.expiry,
		v.spend_state='unclaimed' AS unclaimed, false AS pending_board,
		v.spend_state='unregistered' AS unregistered,
		CASE WHEN v.spend_state='htlc-recv-unclaimed' THEN h.payment_hash END AS receive_hash,
		CASE WHEN v.spend_state='spendable' AND v.policy_type IN ('server-htlc-send','server-htlc-send-v1')
			THEN h.payment_hash END AS send_hash
	FROM vtxo v LEFT JOIN htlc_vtxo h ON h.id=v.id
	WHERE NOT EXISTS (SELECT 1 FROM expiry_settlement s WHERE s.id = v.vtxo_id)
	AND ((v.policy_type = 'pubkey' AND v.spend_state IN ('spendable', 'unclaimed'))
		OR (v.spend_state='unregistered' AND h.id IS NULL)
		-- A recorded preimage does not prove the payer paid: it is
		-- stored before the hold invoice settles. Only a settled
		-- subscription shows the incoming payment was collected.
		OR (v.spend_state='htlc-recv-unclaimed'
			AND v.policy_type IN ('server-htlc-receive','server-htlc-receive-v1')
			AND h.direction='outgoing' AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL
			AND EXISTS (SELECT 1 FROM htlc_settlement s WHERE s.payment_hash=h.payment_hash)
			AND EXISTS (SELECT 1 FROM lightning_htlc_subscription r
				WHERE r.payment_hash=h.payment_hash AND r.status='settled'))
		-- A prepared intra-Ark receive is a candidate: the refund
		-- decision waits until its recipient can no longer claim.
		OR (v.spend_state='spendable'
			AND v.policy_type IN ('server-htlc-send','server-htlc-send-v1')
			AND h.direction='incoming' AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL
			AND NOT EXISTS (SELECT 1 FROM htlc_settlement s WHERE s.payment_hash=h.payment_hash)
			AND NOT EXISTS (SELECT 1 FROM lightning_htlc_subscription r
				WHERE r.payment_hash=h.payment_hash AND r.status='settled')))
	AND v.confirmed_height IS NULL AND v.expiry::bigint + $1 <= $2
	AND v.spent_in_round IS NULL AND v.oor_spent_txid IS NULL AND v.offboarded_in IS NULL
	AND NOT EXISTS (SELECT 1 FROM round_part_input i JOIN round_participation p ON p.id=i.participation_id
		WHERE i.vtxo_id=v.vtxo_id AND p.forfeited_at IS NULL)
	UNION ALL
	SELECT p.vtxo_id, p.vtxo, p.expiry, false, true, false, NULL, NULL FROM pending_board p
	WHERE p.expiry::bigint + $1 <= $2
	AND NOT EXISTS (SELECT 1 FROM vtxo v WHERE v.vtxo_id=p.vtxo_id)";

impl Tx<'_> {
	/// Each arkoor transaction has one input, including checkpoint and dust
	/// isolation steps. A package with several inputs therefore allocates each
	/// output's own amount to the owner of its actual parent, not the recipient.
	pub(super) async fn unregistered_expiry_owner(&self, vtxo: &Vtxo) -> anyhow::Result<Option<PublicKey>> {
		let Some(last) = vtxo.transactions().last() else { return Ok(None); };
		let [input] = last.tx.input.as_slice() else { return Ok(None); };
		let parent = input.previous_output.to_string();
		let Some(row) = self.query_opt("SELECT vtxo FROM vtxo
			WHERE vtxo_id=$1 AND oor_spent_txid=$2", &[&parent, &vtxo.point().txid.to_string()]).await?
			else { return Ok(None); };
		let parent: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo"))?;
		Ok(match parent.policy() {
			ServerVtxoPolicy::User(policy) => Some(policy.user_pubkey()),
			ServerVtxoPolicy::Checkpoint(policy) => Some(policy.user_pubkey),
			_ => None,
		})
	}

	/// Registration changes an unregistered output's entitlement from its
	/// input owner to its recipient. Arbitrate that change with payout commit.
	pub(crate) async fn lock_vtxo_registration(&self, ids: &[VtxoId]) -> anyhow::Result<()> {
		self.check_not_expiry_frozen(ids).await?;
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE",
			&[&ids]).await?;
		if self.query_opt("SELECT id FROM expiry_settlement WHERE id=ANY($1) LIMIT 1", &[&ids]).await?.is_some() {
			return badarg!("{}", server_rpc::EXPIRY_SETTLED_ERROR);
		}
		Ok(())
	}

	/// Pending rows survive both registration and payout. Lock them first in
	/// both the commit and its outcome probe, including when no leaf row exists.
	pub(crate) async fn lock_expiry_inputs(&self, ids: &[String]) -> anyhow::Result<()> {
		self.query("SELECT id FROM pending_board WHERE vtxo_id=ANY($1) ORDER BY id FOR UPDATE",
			&[&ids]).await?;
		self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE",
			&[&ids]).await?;
		Ok(())
	}

	pub(crate) async fn pending_expiry_payments(&self, wallet_height: u32) -> anyhow::Result<Vec<Transaction>> {
		self.query("SELECT tx FROM nursery_tx WHERE kind::text='expiry-payout'
			AND (confirmed_at_height IS NULL OR confirmed_at_height::bigint > $1) ORDER BY id", &[&(wallet_height as i64)]).await?.into_iter()
			.map(|r| Ok(deserialize(r.get("tx"))?)).collect()
	}

	pub(crate) async fn expiry_settlement_spenders(&self, ids: &[VtxoId]) -> anyhow::Result<Vec<Option<String>>> {
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		Ok(self.query("SELECT v.onchain_spent_txid FROM UNNEST($1::text[]) WITH ORDINALITY AS requested(id,position)
			LEFT JOIN vtxo v ON v.vtxo_id=requested.id ORDER BY requested.position", &[&ids]).await?
			.into_iter().map(|r| r.get(0)).collect())
	}
}
