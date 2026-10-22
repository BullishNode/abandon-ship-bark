//! Economic discharge of a specific signed Lightning source.
//!
//! A preimage is shared by every grant using its hash. It does not prove that
//! each of those grants funded a payment. Keep that cryptographic knowledge in
//! the upstream WAL and bind financial conclusions to attempt links.

use std::collections::BTreeMap;

use anyhow::Context;
use bitcoin::{Amount, ScriptBuf, Transaction};
use bitcoin::bip32::ChildNumber;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::taproot::LeafVersion;
use tokio_postgres::Row;

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoId, VtxoPolicy, SECP};
use ark::mailbox::MailboxType;
use ark::vtxo::TapScriptClause;
use ark::vtxo::policy::clause::{HashDelaySignClause, HashDelaySignClause_v0};
use ark::lightning::{PaymentHash, Preimage};
use bitcoin_ext::{BlockDelta, BlockHeight, BlockRef};

use super::{BlockTable, Tx};
use super::expiry_chain::ExpiryViewToken;
use super::ln::LightningPaymentAttempt;

/// Source-specific internal routing survives completion of the initiating
/// attempt. A current subscription sharing its hash is never routing proof.
pub(crate) struct InternalReceiveFunding {
	pub attempt: LightningPaymentAttempt,
	pub claim_complete: bool,
}

fn grant_reclaim_is_deep(height: u32, depth: u32, completed: u32) -> bool {
	depth.checked_sub(1).and_then(|delta| height.checked_add(delta))
		.is_some_and(|required| required <= completed)
}

const INTERNAL_RECEIVE_FUNDING: &str = "
	SELECT a.id,a.lightning_node_id,a.payment_hash,a.amount_msat,a.final_amount_msat,
		a.status,a.error,a.block_height,a.user_fee_sat,a.retry_for_secs,a.user_agent,
		a.lightning_htlc_subscription_id,a.max_routing_fee_msat,a.created_at,a.updated_at,
		f.subscription_id AS funded_subscription_id,
		(EXISTS(SELECT 1 FROM htlc_settlement hs WHERE hs.payment_hash=a.payment_hash)
		AND EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			WHERE v.lightning_htlc_subscription_id=f.subscription_id AND h.direction='outgoing')
		AND NOT EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			WHERE v.lightning_htlc_subscription_id=f.subscription_id AND h.direction='outgoing'
			AND (h.payment_hash<>a.payment_hash OR h.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled')))
		AS claim_complete
	FROM expiry_lightning_internal_funding f
	JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id
	JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LightningSourceState {
	Unattempted,
	Failed,
	Pending,
	/// Exactly one internal attempt funded by this source has issued grants.
	InternalCommitted { attempt_id: i64, subscription_id: i64 },
	InternalZeroGrantCancelled { attempt_id: i64, subscription_id: i64 },
	Discharged,
}

/// Chain-derived failure carries the exact completed view through to a
/// mailbox writer; a status snapshot does not make a durable notification safe.
pub(crate) struct LightningPaymentOutcome {
	pub sources: Vec<VtxoId>,
	pub state: LightningSourceState,
	pub refund_view: Option<ExpiryViewToken>,
}

impl Tx<'_> {
	/// Read before the current source guard: identical accepted incidents stay
	/// idempotent after payout. A different receipt must never overwrite one.
	pub(crate) async fn supervised_lightning_failure_recorded(
		&self, attempt: i64, digest: &[u8; 32],
	) -> anyhow::Result<bool> {
		let Some(row) = self.query_opt(
			"SELECT supervised_manifest_sha256 FROM expiry_lightning_terminal_failure WHERE id=$1",
			&[&attempt],
		).await? else { return Ok(false); };
		ensure!(row.get::<_, Option<Vec<u8>>>(0).as_deref() == Some(digest.as_slice()),
			"attempt already has a different terminal receipt");
		Ok(true)
	}

	/// Accept exactly the original externally dispatched source set under the
	/// same coin and WAL ordering as ordinary expiry decisions. This records a
	/// trusted supervised fact; no coarse Failed flag, preimage, dispatch reset
	/// or money movement is manufactured here.
	pub(crate) async fn record_supervised_lightning_failure(
		&self,
		binding: &crate::ln::SupervisedLightningBinding,
		digest: &[u8; 32],
		receipt: &serde_json::Value,
		absence: &crate::ln::SupervisedOriginalAbsence,
	) -> anyhow::Result<bool> {
		let id = binding.attempt_id;
		if self.supervised_lightning_failure_recorded(id, digest).await? { return Ok(false); }
		let original = self.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
			WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&id]).await?
			.into_iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>();
		ensure!(original.len() as u64 == binding.source_count
			&& sha256::Hash::hash(&serde_json::to_vec(&original)?) == binding.source_set_sha256,
			"incident does not bind the complete original source set");
		let sources = original.iter().map(|source| source.parse::<VtxoId>())
			.collect::<Result<Vec<_>, _>>()?;
		// Lock first, then repeat the idempotence check before testing decided
		// fences. A simultaneous import/payout cannot turn SAME receipt into a
		// spurious refusal or create a second entitlement.
		self.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE",
			&[&original]).await?;
		if self.supervised_lightning_failure_recorded(id, digest).await? { return Ok(false); }
		super::expiry_guard::guard_obligation_changes(self, &sources).await?;
		let attempt = self.query_opt("
			SELECT a.payment_hash,a.lightning_node_id,a.amount_msat,a.final_amount_msat,
				a.status::TEXT,a.lightning_htlc_subscription_id,n.pubkey,d.label,d.created_at AS dispatch_created_at,
				EXISTS(SELECT 1 FROM expiry_lightning_queued_attempt q WHERE q.id=a.id) AS queued,
				EXISTS(SELECT 1 FROM expiry_lightning_external_success s WHERE s.id=a.id) AS success,
				EXISTS(SELECT 1 FROM expiry_lightning_internal_funding f WHERE f.id=a.id) AS internal
			FROM lightning_payment_attempt a JOIN expiry_lightning_dispatch d ON d.id=a.id
			JOIN lightning_node n ON n.id=a.lightning_node_id WHERE a.id=$1 FOR UPDATE OF a,d
		", &[&id]).await?.context("original external dispatch is missing")?;
		self.lock_htlc_settlements().await?;
		ensure!(attempt.get::<_, &str>("payment_hash") == binding.payment_hash.to_string()
			&& attempt.get::<_, i64>("lightning_node_id") == binding.node_id
			&& attempt.get::<_, Vec<u8>>("pubkey") == binding.node_pubkey.serialize().to_vec()
			&& attempt.get::<_, i64>("amount_msat") == i64::try_from(binding.amount_msat)?
			&& attempt.get::<_, &str>("label") == binding.dispatch_label,
			"incident does not match the original dispatch");
		let qualified_original = match absence {
			crate::ln::SupervisedOriginalAbsence::WholeHashEmpty => false,
			crate::ln::SupervisedOriginalAbsence::QualifiedOwnLabel { dispatch_created_at } => {
				ensure!(attempt.get::<_, chrono::DateTime<chrono::Utc>>("dispatch_created_at") == *dispatch_created_at,
					"producer provenance describes a different original dispatch epoch");
				true
			},
		};
		// The ordinary monitor can infer Succeeded/final_amount from a foreign
		// same-hash group. Only qualified ORIGINAL own-label absence supersedes
		// those coarse fields. own commitments/resolutions remain fenced.
		let coarse_uncommitted = attempt.get::<_, Option<i64>>("final_amount_msat").is_none()
			&& attempt.get::<_, &str>("status") != "succeeded";
		ensure!(attempt.get::<_, bool>("queued") && !attempt.get::<_, bool>("success")
			&& !attempt.get::<_, bool>("internal")
			&& attempt.get::<_, Option<i64>>("lightning_htlc_subscription_id").is_none()
			&& (qualified_original || coarse_uncommitted),
			"original attempt has a contradictory economic commitment");
		let rows = self.query("
			SELECT l.vtxo_id,h.direction::TEXT,h.payment_hash,h.offchain_resolution::TEXT,
				h.chain_resolution::TEXT,v.spend_state::TEXT,v.oor_spent_txid,v.spent_in_round,
				v.offboarded_in,v.onchain_spent_txid
			FROM lightning_payment_attempt_htlc_vtxo l
			LEFT JOIN vtxo v ON v.vtxo_id=l.vtxo_id LEFT JOIN htlc_vtxo h ON h.id=v.id
			WHERE l.lightning_payment_attempt_id=$1 ORDER BY l.vtxo_id
		", &[&id]).await?;
		let actual = rows.iter().map(|row| row.get::<_, String>("vtxo_id")).collect::<Vec<_>>();
		ensure!(actual == original, "original complete source set changed");
		for row in rows {
			ensure!(row.get::<_, Option<&str>>("direction") == Some("incoming")
				&& row.get::<_, Option<&str>>("payment_hash") == Some(binding.payment_hash.to_string().as_str())
				&& row.get::<_, Option<&str>>("spend_state").is_some_and(|state| matches!(state,"spendable"|"ln-spent"))
				&& row.get::<_, Option<&str>>("offchain_resolution").is_none()
				&& row.get::<_, Option<&str>>("chain_resolution").is_none()
				&& row.get::<_, Option<String>>("oor_spent_txid").is_none()
				&& row.get::<_, Option<i64>>("spent_in_round").is_none()
				&& row.get::<_, Option<String>>("offboarded_in").is_none()
				&& row.get::<_, Option<String>>("onchain_spent_txid").is_none(),
				"source has a contradictory claim, refund, or spend");
		}
		// A preimage shared by an unrelated hash sibling is not proof this
		// particular source paid. Check funding/success on its links.
		ensure!(self.query_opt("
			SELECT l.vtxo_id FROM lightning_payment_attempt_htlc_vtxo l
			WHERE l.vtxo_id=ANY($1) AND (
				EXISTS(SELECT 1 FROM expiry_lightning_external_success s WHERE s.id=l.lightning_payment_attempt_id)
				OR EXISTS(SELECT 1 FROM expiry_lightning_internal_funding f WHERE f.id=l.lightning_payment_attempt_id))
			LIMIT 1
		", &[&original]).await?.is_none(), "source funded a different economic commitment");
		let receipt = serde_json::to_string(&receipt)?;
		self.execute("
			INSERT INTO expiry_lightning_terminal_failure(id,supervised_manifest_sha256,supervised_manifest)
			VALUES($1,$2,$3::TEXT::JSONB)
		", &[&id, &&digest[..], &receipt]).await?;
		Ok(true)
	}

	/// An internal promotion never dispatches xpay. Once its exact receive is
	/// cancelled without EVER granting a receive coin, no recipient commitment
	/// exists. Do not infer this from the payer's coarse Failed state, a shared
	/// preimage, or expiration of signed grants (which is a different case).
	pub(crate) async fn internal_zero_grant_cancellation(
		&self, attempt_id: i64, hash: PaymentHash,
	) -> anyhow::Result<Option<i64>> {
		Ok(self.query_opt("
			SELECT f.subscription_id FROM expiry_lightning_internal_funding f
			JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id
			JOIN lightning_htlc_subscription s ON s.id=f.subscription_id AND s.payment_hash=a.payment_hash
			WHERE f.id=$1 AND a.payment_hash=$2 AND s.status='canceled'
			AND NOT EXISTS(SELECT 1 FROM vtxo WHERE lightning_htlc_subscription_id=s.id)
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE subscription_id=s.id)
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_replay WHERE subscription_id=s.id)
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_dispatch WHERE id=a.id)
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=a.id)
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original
				JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id
				JOIN expiry_lightning_dispatch d ON d.id=other.lightning_payment_attempt_id
				WHERE original.lightning_payment_attempt_id=a.id)
			AND EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo WHERE lightning_payment_attempt_id=a.id)
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l
				LEFT JOIN vtxo v ON v.vtxo_id=l.vtxo_id LEFT JOIN htlc_vtxo h ON h.id=v.id
				WHERE l.lightning_payment_attempt_id=a.id AND (h.direction::TEXT IS DISTINCT FROM 'incoming'
				OR h.payment_hash IS DISTINCT FROM a.payment_hash OR h.offchain_resolution='fulfilled'))
		",
			&[&attempt_id, &hash.to_string()]).await?.map(|r| r.get("subscription_id")))
	}

	/// Lock the funding source and recipient grants together before a
	/// NEW internal claim publishes its preimage. A global WAL or LnSpent flag
	/// cannot replace this source-specific, still-collectible signed grant.
	pub(crate) async fn lock_internal_receive_claim(
		&self,
		subscription_id: i64,
		preimage: Preimage,
		receive_inputs: &[VtxoId],
		margin: BlockDelta,
	) -> anyhow::Result<()> {
		let hash = preimage.compute_payment_hash();
		let funding = self.funded_internal_receive(subscription_id, hash).await?
			.context("internal receive has no exact funding attempt")?;
		let rows = self.query("
			SELECT v.vtxo_id FROM lightning_payment_attempt_htlc_vtxo l
			JOIN vtxo v ON v.vtxo_id=l.vtxo_id WHERE l.lightning_payment_attempt_id=$1 ORDER BY v.vtxo_id
		",
			&[&funding.attempt.id]).await?;
		let incoming = rows.into_iter().map(|r| r.get::<_, &str>("vtxo_id").parse())
			.collect::<Result<Vec<VtxoId>, _>>()?;
		ensure!(!incoming.is_empty() && !receive_inputs.is_empty(), "internal receive lacks signed sources");
		let mut all = incoming.iter().copied().chain(receive_inputs.iter().copied()).collect::<Vec<_>>();
		all.sort_unstable();
		all.dedup();
		super::expiry_guard::guard_obligation_changes(self, &all).await?;
		let current = self.funded_internal_receive(subscription_id, hash).await?
			.context("internal receive funding disappeared")?;
		ensure!(current.attempt.id == funding.attempt.id, "internal funding attempt changed");
		if current.claim_complete || self.internal_receive_collected(subscription_id, hash).await? {
			return Ok(()); // Actual prior commitment, never a coarse attempt flag.
		}
		let rows = self.query("
			SELECT v.vtxo_id,v.spend_state::TEXT,h.direction::TEXT,h.payment_hash,
			h.htlc_expiry,h.offchain_resolution::TEXT,v.onchain_spent_height,v.onchain_spent_txid
			FROM lightning_payment_attempt_htlc_vtxo l JOIN vtxo v ON v.vtxo_id=l.vtxo_id
			JOIN htlc_vtxo h ON h.id=v.id WHERE l.lightning_payment_attempt_id=$1 ORDER BY v.vtxo_id
		",
			&[&current.attempt.id]).await?;
		let actual = rows.iter().map(|r| r.get::<_, &str>("vtxo_id").parse())
			.collect::<Result<Vec<VtxoId>, _>>()?;
		ensure!(actual == incoming, "internal signed source set changed");
		let tip = self.query_one("SELECT COALESCE(MAX(height),0) FROM captaind_block", &[]).await?.get::<_, i64>(0);
		let last = BlockHeight::try_from(tip)?.checked_add(margin).context("internal receive collection margin overflows")?;
		for row in rows {
			ensure!(row.get::<_, &str>("direction") == "incoming" && row.get::<_, &str>("payment_hash") == hash.to_string(),
				"internal funding has a different signed source");
			ensure!(matches!(row.get::<_, &str>("spend_state"), "spendable"|"ln-spent")
				&& row.get::<_, Option<&str>>("offchain_resolution") != Some("revoked")
				&& row.get::<_, Option<i32>>("onchain_spent_height").is_none()
				&& row.get::<_, Option<&str>>("onchain_spent_txid").is_none(),
				"internal incoming source is not safely collectible");
			ensure!(last <= BlockHeight::try_from(row.get::<_, i32>("htlc_expiry"))?,
				"internal incoming source expired before claim materialization");
		}
		Ok(())
	}

	/// Off-chain fulfillment or a confirmed user preimage spend must
	/// cover the entire issued set. Shared preimage knowledge alone says nothing
	/// about which subscription paid its recipient.
	async fn internal_receive_grants_claimed(&self, subscription_id: i64, hash: PaymentHash) -> anyhow::Result<bool> {
		let Some(preimage) = self.get_htlc_settlement_by_payment_hash(hash).await? else { return Ok(false); };
		let initial_view = self.expiry_rebuild_token().await?;
		let rows = self.query("
			SELECT v.vtxo,v.vtxo_id,v.onchain_spent_txid,v.onchain_spent_height,
				h.payment_hash,h.offchain_resolution::TEXT,h.chain_resolution::TEXT,h.chain_resolution_height,
				c.raw_tx,c.confirmed_height
			FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			LEFT JOIN user_exit_claim c ON c.id=v.onchain_spent_txid
			WHERE v.lightning_htlc_subscription_id=$1 AND h.direction='outgoing'
			ORDER BY v.vtxo_id
		", &[&subscription_id]).await?;
		if rows.is_empty() { return Ok(false); }
		let mut chain_view = None;
		for row in rows {
			if row.get::<_, &str>("payment_hash") != hash.to_string() { return Ok(false); }
			if row.get::<_, Option<&str>>("offchain_resolution") == Some("fulfilled") { continue; }
			if row.get::<_, Option<&str>>("chain_resolution") != Some("fulfilled") { return Ok(false); }
			let Some(height) = row.get::<_, Option<i32>>("chain_resolution_height") else { return Ok(false); };
			if row.get::<_, Option<i32>>("onchain_spent_height") != Some(height)
				|| row.get::<_, Option<i32>>("confirmed_height") != Some(height) { return Ok(false); }
			let Some(txid) = row.get::<_, Option<&str>>("onchain_spent_txid") else { return Ok(false); };
			let Some(raw) = row.get::<_, Option<&[u8]>>("raw_tx") else { return Ok(false); };
			let claim: Transaction = bitcoin::consensus::deserialize(raw)?;
			if claim.compute_txid().to_string() != txid { return Ok(false); }
			let grant: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo"))?;
			if grant.id().to_string() != row.get::<_, &str>("vtxo_id") { return Ok(false); }
			let mut inputs = claim.input.iter().filter(|i| i.previous_output == grant.point());
			let Some(input) = inputs.next() else { return Ok(false); };
			if inputs.next().is_some() { return Ok(false); }
			let (policy_hash, script, revealed) = match grant.policy() {
				ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcRecv(policy)) => (
					policy.payment_hash, policy.user_reveals_preimage_clause(grant.exit_delta()).tapscript(),
					HashDelaySignClause::extract_preimage_from_witness(&input.witness, hash),
				),
				ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcRecv_v0(policy)) => (
					policy.payment_hash, policy.user_reveals_preimage_clause(grant.exit_delta()).tapscript(),
					HashDelaySignClause_v0::extract_preimage_from_witness(&input.witness, hash),
				),
				_ => return Ok(false),
			};
			let Some(leaf) = input.witness.taproot_leaf_script() else { return Ok(false); };
			if policy_hash != hash || revealed != Some(preimage)
				|| leaf.version != LeafVersion::TapScript || leaf.script != script.as_script() { return Ok(false); }
			// Only the confirmed-block frontier writes this raw claim and spend
			// linkage. Reorg processing clears all three confirmation heights.
			// Require both current completed views, not a stale fulfillment flag.
			let view = match chain_view {
				Some(view) => view,
				None => {
					let Some(view) = initial_view else { return Ok(false); };
					chain_view = Some(view);
					view
				},
			};
			let height = BlockHeight::try_from(height)?;
			if height > view.captaind.height || height > view.watchmand.height { return Ok(false); }
			let block = self.get_block_by_height(BlockTable::Watchmand, height).await?;
			if block.is_none() || self.get_block_by_height(BlockTable::Captaind, height).await? != block { return Ok(false); }
		}
		// ReadCommitted callers must not combine rows from opposite sides of a
		// reorg. Completed views are inputs to expiry reconstruction, so do not
		// require the financial reconstruction itself to have finished here.
		Ok(chain_view.is_none() || self.expiry_rebuild_token().await? == chain_view)
	}

	/// The WAL consumer may already have observed an unrelated shared preimage.
	/// Maintenance must also finish attempts after their own real claim
	/// commits, even without a new WAL item or the original caller.
	pub(crate) async fn completed_internal_attempts(&self, limit: i64) -> anyhow::Result<Vec<InternalReceiveFunding>> {
		ensure!((1..=128).contains(&limit), "invalid completed internal attempt limit");
		let query = format!("
			WITH funding AS ({INTERNAL_RECEIVE_FUNDING})
			SELECT id,lightning_node_id,payment_hash,
			amount_msat,final_amount_msat,status,error,block_height,user_fee_sat,retry_for_secs,user_agent,max_routing_fee_msat,
			lightning_htlc_subscription_id,created_at,updated_at,claim_complete
			FROM funding
			WHERE (claim_complete OR EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
					WHERE v.lightning_htlc_subscription_id=funding.funded_subscription_id AND h.direction='outgoing'
					AND h.chain_resolution='fulfilled')
					AND NOT EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
						WHERE v.lightning_htlc_subscription_id=funding.funded_subscription_id AND h.direction='outgoing'
						AND (h.payment_hash<>funding.payment_hash OR (h.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled'
							AND h.chain_resolution::TEXT IS DISTINCT FROM 'fulfilled'))))
				AND (status NOT IN ('failed','succeeded') OR EXISTS(
					SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l JOIN vtxo source ON source.vtxo_id=l.vtxo_id
					WHERE l.lightning_payment_attempt_id=funding.id AND source.spend_state='spendable'))
				ORDER BY id LIMIT $1
		");
		let mut completed = Vec::new();
		for row in self.query(&query, &[&limit]).await? {
			let cooperative = row.get::<_, bool>("claim_complete");
			let attempt: LightningPaymentAttempt = row.try_into()?;
			let subscription = attempt.lightning_htlc_subscription_id.context("internal funding lacks subscription")?;
			if cooperative || self.internal_receive_grants_claimed(subscription, attempt.payment_hash).await? {
				completed.push(InternalReceiveFunding { attempt, claim_complete: true });
			}
		}
		Ok(completed)
	}
	pub(crate) async fn funded_internal_receive(
		&self, subscription_id: i64, hash: PaymentHash,
	) -> anyhow::Result<Option<InternalReceiveFunding>> {
		let query = format!("
			WITH funding AS ({INTERNAL_RECEIVE_FUNDING})
			SELECT id,lightning_node_id,payment_hash,
			amount_msat,final_amount_msat,status,error,block_height,user_fee_sat,retry_for_secs,user_agent,max_routing_fee_msat,
			lightning_htlc_subscription_id,created_at,updated_at,claim_complete
			FROM funding
			WHERE funded_subscription_id=$1 AND payment_hash=$2
		");
		let Some(row) = self.query_opt(&query, &[&subscription_id, &hash.to_string()]).await? else { return Ok(None); };
		let claim_complete = row.get::<_, bool>("claim_complete")
			|| self.internal_receive_grants_claimed(subscription_id, hash).await?;
		Ok(Some(InternalReceiveFunding { attempt: row.try_into()?, claim_complete }))
	}

	/// Replay completed internal commitments after a missed status write. The
	/// bounded maintenance pass can run without the original client, new WAL
	/// entries, or the intentionally canceled native hold invoice.
	/// No row changes on an identical pass, so status/history remain idempotent.
	pub async fn reconcile_completed_internal_receives(&self, limit: i64) -> anyhow::Result<Vec<PaymentHash>> {
		ensure!((1..=128).contains(&limit), "invalid internal receive reconciliation limit");
		let query = format!("
			WITH funding AS ({INTERNAL_RECEIVE_FUNDING})
			SELECT s.id,s.payment_hash,f.claim_complete FROM lightning_htlc_subscription s
			JOIN funding f ON f.funded_subscription_id=s.id
			WHERE s.status<>'settled' AND (f.claim_complete OR EXISTS(
				SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
				WHERE v.lightning_htlc_subscription_id=s.id AND h.direction='outgoing' AND h.chain_resolution='fulfilled')
				AND NOT EXISTS(SELECT 1 FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
					WHERE v.lightning_htlc_subscription_id=s.id AND h.direction='outgoing'
					AND (h.payment_hash<>s.payment_hash OR (h.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled'
						AND h.chain_resolution::TEXT IS DISTINCT FROM 'fulfilled'))))
			ORDER BY s.id LIMIT $1 FOR UPDATE OF s SKIP LOCKED
		");
		let mut completed = Vec::new();
		for row in self.query(&query, &[&limit]).await? {
			let subscription: i64 = row.get("id");
			let hash: PaymentHash = row.get::<_, &str>("payment_hash").parse()?;
			if !row.get::<_, bool>("claim_complete") && !self.internal_receive_grants_claimed(subscription, hash).await? { continue; }
			if self.execute("UPDATE lightning_htlc_subscription SET status='settled',updated_at=NOW() WHERE id=$1 AND status<>'settled'",
				&[&subscription]).await? != 0 { completed.push(hash); }
		}
		Ok(completed)
	}

	pub(crate) async fn lightning_source_state(
		&self, source: VtxoId, hash: PaymentHash,
	) -> anyhow::Result<LightningSourceState> {
		let attempts = self.query("
			SELECT a.id,a.status='failed' AS failed,
			q.id IS NOT NULL AS queued, d.id IS NOT NULL AS dispatched,
			f.id IS NOT NULL AS terminal_failure, s.id IS NOT NULL AS external_success,
			i.id IS NOT NULL AS internal_funding,
			i.subscription_id AS internal_subscription
			FROM lightning_payment_attempt_htlc_vtxo l
			JOIN lightning_payment_attempt a ON a.id=l.lightning_payment_attempt_id
			LEFT JOIN expiry_lightning_queued_attempt q ON q.id=a.id
			LEFT JOIN expiry_lightning_dispatch d ON d.id=a.id
			LEFT JOIN expiry_lightning_terminal_failure f ON f.id=a.id
			LEFT JOIN expiry_lightning_external_success s ON s.id=a.id
			LEFT JOIN expiry_lightning_internal_funding i ON i.id=a.id
			WHERE l.vtxo_id=$1 AND a.payment_hash=$2
		", &[&source.to_string(), &hash.to_string()]).await?;
		self.lightning_attempts_outcome(hash, attempts).await
	}

	/// Resolve only a complete signed source-set identity, never the newest
	/// same-hash attempt. Mailbox callers constrain the attempt ID;
	/// hash-only callers get no proof when several source sets are possible.
	/// Invalid/partial/duplicate/unlinked scopes return None, not a failure or
	/// a success inferred from someone else's preimage.
	pub(crate) async fn lightning_payment_outcome(
		&self, hash: PaymentHash, source_ids: &[VtxoId], actual_attempt_id: Option<i64>,
	) -> anyhow::Result<Option<LightningPaymentOutcome>> {
		let mut requested = source_ids.to_vec();
		requested.sort_unstable();
		requested.dedup();
		if requested.len() != source_ids.len() { return Ok(None); }
		let rows = self.query("
			SELECT a.id,l.vtxo_id,h.direction::TEXT,h.payment_hash AS source_hash
			FROM lightning_payment_attempt a
			LEFT JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=a.id
			LEFT JOIN vtxo v ON v.vtxo_id=l.vtxo_id LEFT JOIN htlc_vtxo h ON h.id=v.id
			WHERE a.payment_hash=$1 AND ($2::BIGINT IS NULL OR a.id=$2)
			ORDER BY a.id,l.vtxo_id
		", &[&hash.to_string(), &actual_attempt_id]).await?;
		let mut groups = BTreeMap::<i64, Option<Vec<VtxoId>>>::new();
		for row in rows {
			let sources = groups.entry(row.get("id")).or_insert_with(|| Some(Vec::new()));
			let Some(current) = sources.as_mut() else { continue; };
			if row.get::<_, Option<&str>>("direction") != Some("incoming")
				|| row.get::<_, Option<&str>>("source_hash") != Some(hash.to_string().as_str()) {
				*sources = None;
				continue;
			}
			let Some(source) = row.get::<_, Option<&str>>("vtxo_id") else {
				*sources = None;
				continue;
			};
			current.push(source.parse()?);
		}
		let mut canonical = if requested.is_empty() { None } else { Some(requested) };
		let mut attempts = Vec::new();
		for (id, sources) in groups {
			let Some(mut sources) = sources else {
				if source_ids.is_empty() || actual_attempt_id.is_some() { return Ok(None); }
				continue;
			};
			sources.sort_unstable();
			let count = sources.len();
			sources.dedup();
			if sources.is_empty() || count != sources.len() { return Ok(None); }
			if let Some(expected) = &canonical {
				if *expected != sources {
					if source_ids.is_empty() { return Ok(None); }
					continue;
				}
			} else {
				canonical = Some(sources);
			}
			attempts.push(id);
		}
		let Some(canonical) = canonical else { return Ok(None); };
		if attempts.is_empty() { return Ok(None); }
		let states = self.query("
			SELECT a.id,a.status='failed' AS failed,
			q.id IS NOT NULL AS queued, d.id IS NOT NULL AS dispatched,
			f.id IS NOT NULL AS terminal_failure, s.id IS NOT NULL AS external_success,
			i.id IS NOT NULL AS internal_funding, i.subscription_id AS internal_subscription
			FROM lightning_payment_attempt a
			LEFT JOIN expiry_lightning_queued_attempt q ON q.id=a.id
			LEFT JOIN expiry_lightning_dispatch d ON d.id=a.id
			LEFT JOIN expiry_lightning_terminal_failure f ON f.id=a.id
			LEFT JOIN expiry_lightning_external_success s ON s.id=a.id
			LEFT JOIN expiry_lightning_internal_funding i ON i.id=a.id
			WHERE a.id=ANY($1) AND a.payment_hash=$2 ORDER BY a.id
		", &[&attempts, &hash.to_string()]).await?;
		if states.len() != attempts.len() { return Ok(None); }
		let mut outcome = self.lightning_attempts_outcome(hash, states).await?;
		if outcome != LightningSourceState::Discharged {
			// A native cooperative refund can finish an internal payment even
			// though its attempt still says Succeeded/Committed. Only a real
			// committed, signed refund spend of EVERY source proves that return;
			// neither cancellation/N6 recovery nor a payout decision suffices.
			let ids = canonical.iter().map(ToString::to_string).collect::<Vec<_>>();
			let refunds = self.query("
				SELECT v.vtxo_id,v.vtxo,v.spend_state::TEXT,v.oor_spent_txid,
					h.direction::TEXT,h.payment_hash,h.offchain_resolution::TEXT,t.signed_tx
				FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
				LEFT JOIN virtual_transaction t ON t.txid=v.oor_spent_txid
				WHERE v.vtxo_id=ANY($1) ORDER BY v.vtxo_id
			", &[&ids]).await?;
			let mut complete_refund = refunds.len() == canonical.len();
			for row in refunds {
				if row.get::<_, &str>("direction") != "incoming"
					|| row.get::<_, &str>("payment_hash") != hash.to_string()
					|| row.get::<_, Option<&str>>("offchain_resolution") != Some("revoked")
					|| row.get::<_, &str>("spend_state") != "spent" {
					complete_refund = false;
					break;
				}
				let (Some(spend), Some(raw)) = (
					row.get::<_, Option<&str>>("oor_spent_txid"), row.get::<_, Option<&[u8]>>("signed_tx"),
				) else { complete_refund = false; break; };
				let source: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo"))?;
				let refund: Transaction = bitcoin::consensus::deserialize(raw)?;
				let mut inputs = refund.input.iter().filter(|input| input.previous_output == source.point());
				let Some(input) = inputs.next() else { complete_refund = false; break; };
				if source.id().to_string() != row.get::<_, &str>("vtxo_id")
					|| !matches!(source.policy(), ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy)) if policy.payment_hash==hash)
					|| refund.compute_txid().to_string() != spend || inputs.next().is_some() || input.witness.is_empty() {
					complete_refund = false;
					break;
				}
			}
			if complete_refund { outcome = LightningSourceState::Failed; }
		}
		let refund_view = if outcome != LightningSourceState::Discharged {
			self.native_lightning_refund_view(hash, &canonical).await?
		} else { None };
		if refund_view.is_some() { outcome = LightningSourceState::Failed; }
		Ok(Some(LightningPaymentOutcome { sources: canonical, state: outcome, refund_view }))
	}

	/// Every original incoming HTLC must have one confirmed customer
	/// payout. A grant reclaim, allocation decision or partial refund is not
	/// a terminal payment outcome. Reuse the immutable allocation/registration
	/// authorities and the watchman's completed canonical reconstruction.
	async fn native_lightning_refund_view(
		&self, hash: PaymentHash, sources: &[VtxoId],
	) -> anyhow::Result<Option<ExpiryViewToken>> {
		if sources.is_empty() { return Ok(None); }
		let Some(view) = self.expiry_rebuild_token().await? else { return Ok(None); };
		if !self.expiry_view_is_rebuilt(view).await? { return Ok(None); }
		let mut remaining = sources.iter().map(|id| (id.to_string(), *id)).collect::<BTreeMap<_, _>>();
		if remaining.len() != sources.len() { return Ok(None); }
		let ids = remaining.keys().cloned().collect::<Vec<_>>();
		let rows = self.query("
			SELECT v.vtxo_id,v.vtxo,h.direction::TEXT,h.payment_hash,
				e.gross_sat AS entitlement_gross,c.user_pubkey,c.owner_mailbox,c.spk,
				c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat,
				o.child_index,a.sweep_txid,a.sweep_tx,a.confirmed_height,a.confirmed_hash,
				p.output_index,p.amount_sat,
				(SELECT SUM(g.payout_sat)::BIGINT FROM expiry_allocation_coin g
					WHERE g.allocation_id=c.allocation_id AND g.spk=c.spk) AS grouped_payout
			FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			JOIN expiry_entitlement e ON e.vtxo_id=v.vtxo_id
			JOIN expiry_allocation_coin c ON c.entitlement_id=e.id AND c.active AND c.disposition='customer'
			JOIN expiry_allocation a ON a.id=c.allocation_id AND a.status='confirmed'
			JOIN expiry_allocation_payout p ON p.allocation_id=a.id AND p.spk=c.spk
			JOIN expiry_owner_key o ON o.user_pubkey=c.user_pubkey AND o.mailbox_pk=c.owner_mailbox
			WHERE v.vtxo_id=ANY($1) ORDER BY v.vtxo_id
		", &[&ids]).await?;
		if rows.len() != remaining.len() { return Ok(None); }
		for row in rows {
			let Some(id) = remaining.remove(row.get::<_, &str>("vtxo_id")) else { return Ok(None); };
			let source: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo"))?;
			let ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy)) = source.policy()
				else { return Ok(None); };
			if source.id() != id || policy.payment_hash != hash
				|| row.get::<_, &str>("direction") != "incoming" || row.get::<_, &str>("payment_hash") != hash.to_string()
				|| row.get::<_, &str>("user_pubkey") != policy.user_pubkey.to_string() { return Ok(None); }
			let mailbox = row.get::<_, &str>("owner_mailbox").parse()?;
			let Some(record) = self.get_fallback(mailbox).await? else { return Ok(None); };
			let child = ChildNumber::from_normal_idx(u32::try_from(row.get::<_, i64>("child_index"))?)?;
			let script = ScriptBuf::from_hex(row.get("spk"))?;
			// The registration was authenticated at insertion and allocation;
			// derive its cached owner again and require that same destination.
			if record.account_xpub.ckd_pub(&SECP, child)?.public_key != policy.user_pubkey || record.spk != script {
				return Ok(None);
			}
			let gross = i64::try_from(source.amount().to_sat())?;
			let fee: i64 = row.get("fee_sat");
			let payout: i64 = row.get("payout_sat");
			if row.get::<_, i64>("gross_sat") != gross || row.get::<_, i64>("entitlement_gross") != gross
				|| row.get::<_, i64>("forfeited_sat") != 0 || payout <= 0 || fee < 0
				|| payout.checked_add(fee) != Some(gross) { return Ok(None); }
			let (Some(raw), Some(height), Some(block_hash)) = (
				row.get::<_, Option<&[u8]>>("sweep_tx"), row.get::<_, Option<i64>>("confirmed_height"),
				row.get::<_, Option<&str>>("confirmed_hash"),
			) else { return Ok(None); };
			let parent: Transaction = bitcoin::consensus::deserialize(raw)?;
			let index = usize::try_from(row.get::<_, i32>("output_index"))?;
			let Some(output) = parent.output.get(index) else { return Ok(None); };
			if row.get::<_, Option<&str>>("sweep_txid") != Some(parent.compute_txid().to_string().as_str())
				|| parent.input.is_empty() || parent.input.iter().any(|input| input.witness.is_empty())
				|| output.script_pubkey != script || output.value.to_sat() < u64::try_from(payout)?
				|| output.value != Amount::from_sat(u64::try_from(row.get::<_, i64>("amount_sat"))?)
				|| output.value.to_sat() != u64::try_from(row.get::<_, i64>("grouped_payout"))? { return Ok(None); }
			let block = BlockRef { height: BlockHeight::try_from(height)?, hash: block_hash.parse()? };
			if block.height > view.captaind.height || block.height > view.watchmand.height
				|| self.get_block_by_height(BlockTable::Captaind, block.height).await? != Some(block)
				|| self.get_block_by_height(BlockTable::Watchmand, block.height).await? != Some(block) { return Ok(None); }
		}
		if !remaining.is_empty() || self.expiry_rebuild_token().await? != Some(view)
			|| !self.expiry_view_is_rebuilt(view).await? { return Ok(None); }
		Ok(Some(view))
	}

	/// Bounded maintenance candidates only; the shared outcome rechecks every
	/// original signed source and canonical refund before notifying its sender.
	pub(crate) async fn native_refund_attempts(&self, limit: i64) -> anyhow::Result<Vec<LightningPaymentAttempt>> {
		ensure!((1..=128).contains(&limit), "invalid native refund attempt limit");
		let finished = String::from(MailboxType::LnSendFinished);
		let rows = self.query("
			SELECT a.id,a.lightning_node_id,a.payment_hash,a.amount_msat,a.final_amount_msat,
				a.status,a.error,a.block_height,a.user_fee_sat,a.retry_for_secs,a.user_agent,
				a.max_routing_fee_msat,a.lightning_htlc_subscription_id,a.created_at,a.updated_at
			FROM lightning_payment_attempt a
			WHERE a.sender_mailbox_id IS NOT NULL
				AND NOT EXISTS(SELECT 1 FROM mailbox m WHERE m.payment_hash=a.payment_hash AND m.mailbox_type::TEXT=$2)
				AND EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l WHERE l.lightning_payment_attempt_id=a.id)
				AND NOT EXISTS(
					SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l
					LEFT JOIN expiry_entitlement e ON e.vtxo_id=l.vtxo_id
					LEFT JOIN expiry_allocation_coin c ON c.entitlement_id=e.id AND c.active AND c.disposition='customer'
					LEFT JOIN expiry_allocation p ON p.id=c.allocation_id AND p.status='confirmed'
					WHERE l.lightning_payment_attempt_id=a.id AND p.id IS NULL)
			ORDER BY a.id LIMIT $1
		", &[&limit, &finished]).await?;
		rows.into_iter().map(TryInto::try_into).collect()
	}

	/// Shared source-specific public proof for scoped status and the
	/// attempt's mailbox. The existing expiry spending guard is unchanged;
	/// coarse final fields never discharge this public outcome either.
	async fn lightning_attempts_outcome(
		&self, hash: PaymentHash, attempts: Vec<Row>,
	) -> anyhow::Result<LightningSourceState> {
		if attempts.is_empty() {
			return Ok(LightningSourceState::Unattempted);
		}
		let preimage_known = self.get_htlc_settlement_by_payment_hash(hash).await?.is_some();
		let mut pending = false;
		let mut internal = Vec::new();
		let mut cancelled_internal = None;
		for attempt in attempts {
			if preimage_known {
				if attempt.get::<_, bool>("external_success") { return Ok(LightningSourceState::Discharged); }
				if let Some(subscription) = attempt.get::<_, Option<i64>>("internal_subscription") {
					if self.funded_internal_receive(subscription, hash).await?.is_some_and(|funding|
						funding.attempt.id == attempt.get::<_, i64>("id") && funding.claim_complete)
					{ return Ok(LightningSourceState::Discharged); }
				}
			}
			if attempt.get::<_, bool>("internal_funding") {
				let attempt_id = attempt.get::<_, i64>("id");
				if let Some(subscription_id) = self.internal_zero_grant_cancellation(attempt_id, hash).await? {
					cancelled_internal = Some((attempt_id, subscription_id));
					continue;
				}
				internal.push(attempt_id);
				continue;
			}
			let never_dispatched = attempt.get::<_, bool>("queued")
				&& attempt.get::<_, bool>("failed") && !attempt.get::<_, bool>("dispatched");
			if !attempt.get::<_, bool>("terminal_failure") && !never_dispatched {
				pending = true;
			}
		}
		Ok(if pending {
			LightningSourceState::Pending
		} else if let [attempt_id] = internal[..] {
			let subscription_id = self.query_one(
				"SELECT subscription_id FROM expiry_lightning_internal_funding WHERE id=$1",
				&[&attempt_id]).await?.get("subscription_id");
			LightningSourceState::InternalCommitted { attempt_id, subscription_id }
		} else if !internal.is_empty() {
			// Several internal attempts on one source never resolve automatically.
			LightningSourceState::Pending
		} else if let Some((attempt_id, subscription_id)) = cancelled_internal {
			LightningSourceState::InternalZeroGrantCancelled { attempt_id, subscription_id }
		} else {
			LightningSourceState::Failed
		})
	}

	/// The block by which every receive grant of an internal receive had been
	/// taken back by the server, at least `depth` blocks deep in both completed
	/// views: either its own timeout spend, or a confirmed operator payout that
	/// swept it with its tree output. Any grant still open, collected off-chain,
	/// or claimed with the preimage keeps the sender's source unresolved.
	pub(crate) async fn internal_grants_reclaimed(
		&self,
		subscription_id: i64,
		hash: PaymentHash,
		tip: BlockHeight,
		watchman_tip: BlockHeight,
		depth: u32,
	) -> anyhow::Result<Option<BlockRef>> {
		let grants = self.query("
			SELECT h.payment_hash,h.direction::TEXT,h.offchain_resolution::TEXT,
				h.chain_resolution::TEXT,h.chain_resolution_height,
				(SELECT MIN(a.confirmed_height) FROM expiry_allocation a
					JOIN expiry_allocation_coin c ON c.allocation_id=a.id
					JOIN expiry_entitlement e ON e.id=c.entitlement_id
					WHERE e.vtxo_id=v.vtxo_id AND c.disposition='operator' AND a.status='confirmed') AS swept_height
			FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
			WHERE v.lightning_htlc_subscription_id=$1 ORDER BY v.vtxo_id
		", &[&subscription_id]).await?;
		if grants.is_empty() {
			return Ok(None);
		}
		let completed = tip.min(watchman_tip).to_u32();
		let mut last = 0u32;
		for grant in &grants {
			let ours = grant.get::<_, &str>("payment_hash") == hash.to_string()
				&& grant.get::<_, &str>("direction") == "outgoing"
				&& grant.get::<_, Option<&str>>("offchain_resolution").is_none();
			let revoked = grant.get::<_, Option<&str>>("chain_resolution") == Some("revoked");
			let resolved = grant.get::<_, Option<&str>>("chain_resolution").is_some();
			let height = if revoked {
				grant.get::<_, Option<i32>>("chain_resolution_height")
			} else if !resolved {
				grant.get::<_, Option<i64>>("swept_height").map(i32::try_from).transpose()?
			} else {
				None
			};
			let Some(height) = height else {
				return Ok(None);
			};
			let height = u32::try_from(height)?;
			if !ours || !grant_reclaim_is_deep(height, depth, completed) {
				return Ok(None);
			}
			last = last.max(height);
		}
		let block = self.get_block_by_height(super::BlockTable::Watchmand, BlockHeight::new(last)).await?
			.context("grant reclaim block is not in the completed watchman view")?;
		Ok(Some(block))
	}

	/// Return only subscriptions the requested source coins actually funded.
	/// A payment-hash lookup alone can refer to an unrelated external receive.
	pub(crate) async fn lightning_source_subscriptions(&self, sources: &[VtxoId]) -> anyhow::Result<Vec<i64>> {
		let sources = sources.iter().map(ToString::to_string).collect::<Vec<_>>();
		Ok(self.query("
			SELECT DISTINCT i.subscription_id FROM expiry_lightning_internal_funding i
			JOIN lightning_payment_attempt_htlc_vtxo l ON l.lightning_payment_attempt_id=i.id
			WHERE l.vtxo_id=ANY($1) ORDER BY i.subscription_id
		", &[&sources]).await?
			.into_iter().map(|row| row.get("subscription_id")).collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn internal_grant_recovery_requires_full_depth_without_overflow() {
		assert!(!grant_reclaim_is_deep(100, 6, 104));
		assert!(grant_reclaim_is_deep(100, 6, 105));
		assert!(!grant_reclaim_is_deep(100, 0, 105));
		assert!(!grant_reclaim_is_deep(100, u32::MAX, u32::MAX));
		assert!(!grant_reclaim_is_deep(u32::MAX, 6, u32::MAX));
	}
}
