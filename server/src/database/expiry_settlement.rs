//! Atomic expiry payments. The nursery tx and the settlement rows share the coin-state commit.

use std::collections::BTreeMap;

use ark::{ProtocolEncoding, ServerVtxo, VtxoId, Vtxo};
use ark::lightning::PaymentHash;
use ark::vtxo::policy::ServerVtxoPolicy;
use ark::tree::signed::UnlockHash;
use bitcoin::{ScriptBuf, Transaction};
use bitcoin::consensus::deserialize;
use bitcoin::secp256k1::PublicKey;
use tokio_postgres::Row;

use crate::database::ln::LightningHtlcSubscriptionStatus;
use crate::nursery::NurseryTxKind;
use crate::SECP;
use super::model::SpendState;
use super::tree::VtxoTreeUpdate;
use super::Tx;

#[derive(Clone, Copy)]
pub(crate) struct ExpiryInput {
	pub id: VtxoId,
	pub source: ExpirySource,
	pub owner: PublicKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpirySource {
	Registered, PendingBoard, Unregistered, LightningReceive(PaymentHash), LightningSend(PaymentHash),
}

pub(crate) struct SettlementVtxo {
	pub id: VtxoId,
	pub vtxo: Vec<u8>,
	pub expiry: u32,
	pub unclaimed: bool,
	pub source: ExpirySource,
	pub input_owner: Option<PublicKey>,
	pub predecessors: Vec<Vec<u8>>,
}

impl SettlementVtxo {
	fn from_row(row: Row) -> anyhow::Result<Self> {
		let receive_hash = row.get::<_, Option<&str>>("receive_hash");
		let send_hash = row.get::<_, Option<&str>>("send_hash");
		Ok(Self {
			id: row.get::<_, &str>("vtxo_id").parse()?,
			unclaimed: row.get("unclaimed"), predecessors: Vec::new(),
			source: if let Some(hash) = receive_hash { ExpirySource::LightningReceive(hash.parse()?) }
				else if let Some(hash) = send_hash { ExpirySource::LightningSend(hash.parse()?) }
				else if row.get("pending_board") { ExpirySource::PendingBoard }
				else if row.get("unregistered") { ExpirySource::Unregistered }
				else { ExpirySource::Registered },
			input_owner: None,
			vtxo: row.get("vtxo"), expiry: u32::try_from(row.get::<_, i32>("expiry"))?,
		})
	}
}

pub(crate) fn payout_script(owner: PublicKey) -> ScriptBuf {
	ScriptBuf::new_p2tr(&SECP, owner.x_only_public_key().0, None)
}

/// Expired, unsettled coins and the source that decides their payee: `$1` is
/// the grace period and `$2` the tip. A coin that is spent, exited, offboarded
/// or in a live exchange is not a candidate. Scan and commit share this rule.
const CANDIDATES: &str = "SELECT v.vtxo_id, v.vtxo, v.expiry,
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
	pub(crate) async fn expiry_settlement_page(
		&self, tip: u32, grace: u32,
		after: (u32, String), limit: u32,
	) -> anyhow::Result<Vec<SettlementVtxo>> {
		let rows = self.query(&format!("SELECT * FROM ({CANDIDATES}) c
			WHERE (c.expiry::bigint, c.vtxo_id) > ($3::bigint, $4) ORDER BY c.expiry, c.vtxo_id LIMIT $5"),
			&[&(grace as i64), &(tip as i64), &(after.0 as i64), &after.1, &(limit as i64)]).await?;
		let mut candidates = rows.into_iter().map(SettlementVtxo::from_row).collect::<anyhow::Result<Vec<_>>>()?;
		for coin in &mut candidates {
			if coin.source == ExpirySource::Unregistered {
				coin.input_owner = self.unregistered_expiry_owner(&Vtxo::deserialize(&coin.vtxo)?).await?;
			}
			if !coin.unclaimed { continue; }
			let vtxo: Vtxo = Vtxo::deserialize(&coin.vtxo)?;
			let Some(hash) = vtxo.unlock_hash() else { continue; };
			coin.predecessors = self.query("SELECT v.vtxo FROM round_participation p
				JOIN round_part_input i ON i.participation_id=p.id
				LEFT JOIN vtxo v ON v.vtxo_id=i.vtxo_id
				WHERE p.unlock_hash=$1 AND p.round_id=$2 ORDER BY v.vtxo_id",
				&[&hash.to_string(), &vtxo.chain_anchor().txid.to_string()]).await?
				.into_iter().map(|row| row.get::<_, Option<Vec<u8>>>("vtxo"))
				.collect::<Option<Vec<_>>>().unwrap_or_default();
		}
		Ok(candidates)
	}

	/// Each arkoor transaction has one input, including checkpoint and dust
	/// isolation steps. A package with several inputs therefore allocates each
	/// output's own amount to the owner of its actual parent, not the recipient.
	async fn unregistered_expiry_owner(&self, vtxo: &Vtxo) -> anyhow::Result<Option<PublicKey>> {
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
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE",
			&[&ids]).await?;
		if self.query_opt("SELECT id FROM expiry_settlement WHERE id=ANY($1) LIMIT 1", &[&ids]).await?.is_some() {
			return badarg!("{}", server_rpc::EXPIRY_SETTLED_ERROR);
		}
		Ok(())
	}

	/// The selected inputs that are still payable with the same source and owner.
	pub(crate) async fn expiry_inputs(
		&self, inputs: &[ExpiryInput], tip: u32, grace: u32,
	) -> anyhow::Result<Vec<Vtxo>> {
		let ids = inputs.iter().map(|i| i.id.to_string()).collect::<Vec<_>>();
		let expected = inputs.iter().map(|i| (i.id, (i.source, i.owner))).collect::<BTreeMap<_, _>>();
		let rows = self.query(&format!("SELECT * FROM ({CANDIDATES}) c WHERE c.vtxo_id=ANY($3)"),
			&[&(grace as i64), &(tip as i64), &ids]).await?;
		let mut coins = Vec::new();
		for row in rows {
			let coin = SettlementVtxo::from_row(row)?;
			let vtxo = Vtxo::deserialize(&coin.vtxo)?;
			let owner = if coin.source == ExpirySource::Unregistered { self.unregistered_expiry_owner(&vtxo).await? }
				else { Some(vtxo.user_pubkey()) };
			if owner.map(|o| (coin.source, o)).as_ref() == expected.get(&coin.id) { coins.push(vtxo); }
		}
		Ok(coins)
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

	pub(crate) async fn store_expiry_payment(
		&self, inputs: &[ExpiryInput], scripts: &[Vec<u8>], tx: &Transaction, fee: u64, tip: u32, grace: u32,
		confirm_target: bitcoin_ext::BlockHeight,
	) -> anyhow::Result<()> {
		let ids = inputs.iter().map(|i| i.id.to_string()).collect::<Vec<_>>();
		ensure!(ids.len() == scripts.len(), "expiry destination count mismatch");
		let sends = inputs.iter().filter_map(|i| match i.source {
			ExpirySource::LightningSend(hash) => Some((i.id.to_string(), hash)),
			_ => None,
		}).collect::<BTreeMap<_, _>>();
		// A refund must not commit after a settlement it did not see. Take the
		// settlement write lock first, as the sender's refund request does.
		if !sends.is_empty() { self.lock_htlc_settlements().await?; }
		self.lock_expiry_inputs(&ids).await?;
		let coins = self.expiry_inputs(inputs, tip, grace).await?;
		ensure!(coins.len() == ids.len(), "expiry inputs changed");
		let boards = inputs.iter().filter(|i| i.source == ExpirySource::PendingBoard).map(|i| i.id).collect::<Vec<_>>();
		let update = VtxoTreeUpdate::new().insert_unspent_vtxos(
			coins.into_iter().filter(|v| boards.contains(&v.id())).map(Into::into), SpendState::Spent,
		);
		let inserted = self.execute_vtxo_tree_update(update).await?;
		ensure!(inserted as usize == boards.len(), "pending boards changed during commit");
		let ordinary = inputs.iter().filter(|i| i.source != ExpirySource::PendingBoard).map(|i| i.id.to_string()).collect::<Vec<_>>();
		let n = self.execute("UPDATE vtxo SET spend_state='spent',updated_at=NOW()
			WHERE vtxo_id=ANY($1) AND spend_state IN ('spendable','unclaimed','unregistered','htlc-recv-unclaimed')", &[&ordinary]).await?;
		ensure!(n as usize == ordinary.len(), "expiry inputs changed during commit");
		let receives = inputs.iter().filter(|i| matches!(i.source, ExpirySource::LightningReceive(_)))
			.map(|i| i.id.to_string()).collect::<Vec<_>>();
		// The caller retains the payment guards through this commit. Keep the
		// resolution conditional as well, so another resolution cannot be overwritten.
		let fulfilled = self.execute("UPDATE htlc_vtxo h SET offchain_resolution='fulfilled'
			FROM vtxo v WHERE h.id=v.id AND v.vtxo_id=ANY($1) AND h.direction='outgoing'
			AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL
			AND EXISTS (SELECT 1 FROM htlc_settlement s WHERE s.payment_hash=h.payment_hash)
			AND EXISTS (SELECT 1 FROM lightning_htlc_subscription r
				WHERE r.payment_hash=h.payment_hash AND r.status='settled')", &[&receives]).await?;
		ensure!(fulfilled as usize == receives.len(), "expiry receive resolution changed during commit");
		let send_ids = sends.keys().cloned().collect::<Vec<_>>();
		let revoked = self.execute("UPDATE htlc_vtxo h SET offchain_resolution='revoked'
			FROM vtxo v WHERE h.id=v.id AND v.vtxo_id=ANY($1) AND h.direction='incoming'
			AND h.offchain_resolution IS NULL AND h.chain_resolution IS NULL
			AND NOT EXISTS (SELECT 1 FROM htlc_settlement s WHERE s.payment_hash=h.payment_hash)", &[&send_ids]).await?;
		ensure!(revoked as usize == send_ids.len(), "expiry send resolution changed during commit");
		// Cancel an intra-Ark receive for the same hash, as the refund request
		// does, so a later claim is refused. A committed receive vetoes the refund.
		for hash in sends.values().collect::<std::collections::BTreeSet<_>>() {
			if let Some(status) = self.cancel_revocable_htlc_subscription(*hash, tip.into()).await? {
				ensure!(!matches!(status, LightningHtlcSubscriptionStatus::HtlcsReady
					| LightningHtlcSubscriptionStatus::Settled), "intra-Ark receive committed during expiry refund");
			}
		}
		let txid = tx.compute_txid().to_string();
		self.upsert_nursery_tx(tx, NurseryTxKind::ExpiryPayout, confirm_target).await?;
		self.execute("INSERT INTO expiry_settlement (id,txid,fee_sat,spk)
			SELECT p.id,$2,$3,p.spk FROM UNNEST($1::text[],$4::bytea[]) AS p(id,spk)",
			&[&ids, &txid, &i64::try_from(fee)?, &scripts]).await?;
		Ok(())
	}

	/// Retire an irrecoverable delegated exchange and release only its swept
	/// originals. The caller holds all coin locks and proves every chain path.
	pub(crate) async fn cancel_expired_participation(
		&self, hash: UnlockHash, round_id: i64, inputs: &[VtxoId], outputs: &[VtxoId], exited: &[VtxoId],
	) -> anyhow::Result<bool> {
		// Forfeit registration takes these association locks before coin rows.
		// A cached registration must lose here if cancellation commits first.
		let associations = self.query("SELECT i.vtxo_id FROM round_part_input i
			JOIN round_participation p ON p.id=i.participation_id
			WHERE p.unlock_hash=$1 ORDER BY i.vtxo_id FOR UPDATE OF i", &[&hash.to_string()]).await?;
		let Some(part) = self.get_round_participation_by_unlock_hash(hash).await? else { return Ok(false); };
		if associations.len() != inputs.len() || part.forfeited_at.is_some()
			|| part.inputs.len() != inputs.len() || part.inputs.iter().any(|i|
				i.signed_forfeit_tx.is_some() || !inputs.contains(&i.vtxo_id)) { return Ok(false); }
		let Some(round) = part.round_id else { return Ok(false); };
		if self.get_round(round).await?.map(|r| r.id) != Some(round_id) { return Ok(false); }
		let keys: Vec<String> = inputs.iter().chain(outputs).map(ToString::to_string).collect();
		self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE", &[&keys]).await?;
		if !self.query("SELECT id FROM expiry_settlement WHERE id=ANY($1)", &[&keys]).await?.is_empty() {
			return Ok(false);
		}
		let old = self.get_user_vtxos_by_id(inputs).await?;
		let new = self.get_user_vtxos_by_id(outputs).await?;
		if old.len() != inputs.len() || new.len() != outputs.len()
			|| old.iter().any(|v| v.spend_state != SpendState::Spent || v.spent_in_round != Some(round_id)
				|| v.oor_spent_txid.is_some() || v.offboarded_in.is_some()
				|| (v.confirmed_height.is_some() && !exited.contains(&v.vtxo_id)))
			|| new.iter().any(|v| v.spend_state != SpendState::Unclaimed || v.spent_in_round.is_some()
				|| v.oor_spent_txid.is_some() || v.offboarded_in.is_some() || v.confirmed_height.is_some()) {
			return Ok(false);
		}
		let input_keys: Vec<String> = inputs.iter().map(ToString::to_string).collect();
		let output_keys: Vec<String> = outputs.iter().map(ToString::to_string).collect();
		let exited_keys: Vec<String> = exited.iter().map(ToString::to_string).collect();
		self.execute("INSERT INTO expiry_cancelled_participation (id,round_id,input_ids,output_ids,exited_ids)
			VALUES ($1,$2,$3,$4,$5)", &[&hash.to_string(), &round_id, &input_keys, &output_keys, &exited_keys]).await?;
		self.execute("UPDATE vtxo SET spend_state='spent',updated_at=NOW() WHERE vtxo_id=ANY($1)", &[&output_keys]).await?;
		self.execute("UPDATE vtxo SET spend_state='spendable',spent_in_round=NULL,updated_at=NOW()
			WHERE vtxo_id=ANY($1) AND NOT(vtxo_id=ANY($2))", &[&input_keys, &exited_keys]).await?;
		ensure!(self.remove_round_participation(hash).await?, "expired participation disappeared");
		Ok(true)
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
