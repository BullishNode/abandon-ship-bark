//! The intended beneficiary of one validated Lightning receive claim.
//!
//! Unlike an ordinary undelivered OOR, collecting a Lightning receive already
//! commits the server to the claim's final destination. Persist that destination
//! before releasing the preimage, independently of checkpoint custody or delivery.

use anyhow::Context;
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::{BlockDelta, BlockHeight};
use prost::Message;

use ark::{ProtocolEncoding, Vtxo, VtxoId, VtxoPolicy};
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignRequest};
use ark::arkoor::state::ServerCanCosign;
use ark::lightning::{PaymentHash, Preimage};
use ark::vtxo::Full;
use server_rpc::protos;

use super::Tx;

const RECEIVE_REQUEST_ENCODING: i32 = 1;
// Match tonic's incoming request budget. Reject corrupt or unsupported stored
// representations before decoding rather than allocating an unbounded request.
const MAX_RECEIVE_REQUEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ReceiveClaimRequest {
	pub payment_hash: PaymentHash,
	pub preimage: Preimage,
	pub request: ArkoorPackageCosignRequest<VtxoId>,
	pub protocol_version: u64,
}

impl ReceiveClaimRequest {
	pub(crate) fn encode(&self) -> anyhow::Result<Vec<u8>> {
		ensure!(self.payment_hash == self.preimage.compute_payment_hash(), "receive request preimage differs");
		ensure!(self.protocol_version > 0 && self.protocol_version <= i64::MAX as u64,
			"unsupported receive request protocol version");
		let message = protos::ClaimLightningReceiveRequest {
			payment_hash: self.payment_hash.to_vec(),
			payment_preimage: self.preimage.to_vec(),
			cosign_request: Some(self.request.clone().into()),
		};
		ensure!(message.encoded_len() <= MAX_RECEIVE_REQUEST_BYTES, "receive request exceeds storage budget");
		Ok(message.encode_to_vec())
	}

	pub(crate) fn decode(encoding: i32, protocol_version: i64, bytes: &[u8]) -> anyhow::Result<Self> {
		ensure!(encoding == RECEIVE_REQUEST_ENCODING, "unsupported stored receive request encoding");
		ensure!(protocol_version > 0, "invalid stored receive protocol version");
		ensure!(bytes.len() <= MAX_RECEIVE_REQUEST_BYTES, "stored receive request exceeds storage budget");
		let message = protos::ClaimLightningReceiveRequest::decode(bytes)?;
		let payment_hash = PaymentHash::from_slice(&message.payment_hash)?;
		let preimage = Preimage::from_slice(&message.payment_preimage)?;
		ensure!(payment_hash == preimage.compute_payment_hash(), "stored receive preimage differs");
		let request = ArkoorPackageCosignRequest::try_from(message.cosign_request.context("stored receive request missing")?)?;
		ensure!(!request.requests.is_empty(), "stored receive package is empty");
		Ok(Self { payment_hash, preimage, request, protocol_version: protocol_version as u64 })
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiveClaimBinding {
	pub source: VtxoId,
	pub payment_hash: PaymentHash,
	pub output: Vtxo<Full>,
}

impl ReceiveClaimBinding {
	pub(crate) fn from_package(builder: &ArkoorPackageBuilder<ServerCanCosign>) -> anyhow::Result<Vec<Self>> {
		builder.builders.iter().filter_map(|part| {
			let source = part.input();
			let hash = match source.policy() {
				VtxoPolicy::ServerHtlcRecv(p) => p.payment_hash,
				VtxoPolicy::ServerHtlcRecv_v0(p) => p.payment_hash,
				_ => return None,
			};
			Some((|| {
				let mut outputs = part.build_unsigned_vtxos();
				let output = outputs.next().context("receive claim has no final output")?;
				ensure!(matches!(output.policy(), VtxoPolicy::Pubkey(_)), "receive claim destination must be a pubkey policy");
				ensure!(outputs.next().is_none(), "receive claim has multiple final outputs");
				ensure!(output.amount() == source.amount(), "receive claim does not preserve its source amount");
				Ok(Self { source: source.id(), payment_hash: hash, output })
			})())
		}).collect()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiveClaimSettlement {
	Settled,
	CancelledUncollected,
	Pending,
}

pub(crate) struct ReceiveClaimEntitlement {
	pub source: VtxoId,
	pub subscription_id: i64,
	pub payment_hash: PaymentHash,
	pub output_id: VtxoId,
	pub recipient: Option<PublicKey>,
	pub settlement: ReceiveClaimSettlement,
}

impl Tx<'_> {
	/// The original request and all its destinations commit together before the
	/// preimage reaches the settler. A retry can use fresh public signing nonces,
	/// but it cannot replace the first request needed by callerless recovery.
	pub(crate) async fn prepare_receive_request(
		&self, request: &ReceiveClaimRequest, bindings: &[ReceiveClaimBinding],
		collection_deadline: Option<(BlockHeight, BlockDelta)>,
	) -> anyhow::Result<()> {
		let bytes = request.encode()?;
		ensure!(!bindings.is_empty(), "receive request has no bindings");
		ensure!(request.request.inputs().copied().collect::<Vec<_>>()
			== bindings.iter().map(|b| b.source).collect::<Vec<_>>()
			&& bindings.iter().all(|b| b.payment_hash == request.payment_hash),
			"receive request and bound sources differ");
		self.prepare_receive_claims(bindings).await?;
		if let Some((expiry, margin)) = collection_deadline {
			// Admission checked before waiting for source locks is insufficient.
			// Recheck the completed tip while those obligation locks are held.
			let tip = self.query_one("SELECT COALESCE(MAX(height),0) FROM captaind_block", &[]).await?.get::<_, i64>(0);
			let last = BlockHeight::try_from(tip)?.checked_add(margin).context("receive collection margin overflows")?;
			ensure!(last <= expiry, "incoming receive expired while waiting for obligation locks");
		}
		let row = self.query_one("SELECT subscription_id FROM expiry_lightning_receive_claim WHERE source_vtxo_id=$1",
			&[&bindings[0].source.to_string()]).await?;
		let subscription_id: i64 = row.get("subscription_id");
		ensure!(self.query_opt("
			SELECT source_vtxo_id FROM expiry_lightning_receive_claim
			WHERE source_vtxo_id=ANY($1) AND subscription_id<>$2 LIMIT 1
		",
			&[&bindings.iter().map(|b| b.source.to_string()).collect::<Vec<_>>(), &subscription_id]).await?.is_none(),
			"receive request spans subscriptions");
		self.execute("
			INSERT INTO expiry_lightning_receive_replay
			(subscription_id,payment_hash,encoding_version,protocol_version,request) VALUES($1,$2,$3,$4,$5)
			ON CONFLICT(subscription_id) DO NOTHING
		", &[&subscription_id, &request.payment_hash.to_string(),
				&RECEIVE_REQUEST_ENCODING, &(request.protocol_version as i64), &bytes]).await?;
		// An admitted destination cannot acquire a contradictory first request.
		// Check the stored package against every original output.
		let stored = self.receive_claim_request(subscription_id).await?.context("receive request disappeared")?;
		ensure!(stored.payment_hash == request.payment_hash
			&& stored.request.inputs().copied().collect::<Vec<_>>() == request.request.inputs().copied().collect::<Vec<_>>(),
			"stored receive source identity differs");
		Ok(())
	}

	pub(crate) async fn receive_claim_request(&self, subscription_id: i64) -> anyhow::Result<Option<ReceiveClaimRequest>> {
		self.query_opt("
			SELECT encoding_version,protocol_version,request FROM expiry_lightning_receive_replay
			WHERE subscription_id=$1
		", &[&subscription_id]).await?.map(|row| {
			ReceiveClaimRequest::decode(row.get("encoding_version"), row.get("protocol_version"), row.get("request"))
		}).transpose()
	}

	/// Claim a bounded set of due recovery observations, not financial value.
	/// One unknown node cannot monopolize the ordered queue after a restart.
	pub(crate) async fn pending_receive_requests(&self, limit: i64) -> anyhow::Result<Vec<i64>> {
		ensure!((1..=8).contains(&limit), "invalid receive request recovery limit");
		Ok(self.query("
			WITH pending AS (
				SELECT r.subscription_id FROM expiry_lightning_receive_replay r
				WHERE r.uncollected_at IS NULL AND (r.last_attempt_at IS NULL OR r.last_attempt_at < NOW()-INTERVAL '5 seconds')
				AND EXISTS(SELECT 1 FROM expiry_lightning_receive_claim c WHERE c.subscription_id=r.subscription_id AND NOT c.materialized)
				AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim c JOIN expiry_entitlement d ON d.vtxo_id=c.source_vtxo_id
					WHERE c.subscription_id=r.subscription_id)
				ORDER BY r.last_attempt_at NULLS FIRST,r.subscription_id LIMIT $1 FOR UPDATE OF r SKIP LOCKED
			)
			UPDATE expiry_lightning_receive_replay r SET last_attempt_at=NOW() FROM pending p
			WHERE r.subscription_id=p.subscription_id RETURNING r.subscription_id
		", &[&limit]).await?
			.into_iter().map(|row| row.get("subscription_id")).collect())
	}

	pub(crate) async fn receive_request_materialized(&self, subscription_id: i64) -> anyhow::Result<bool> {
		Ok(self.query_one("
			SELECT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE subscription_id=$1)
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim c JOIN vtxo v ON v.vtxo_id=c.source_vtxo_id
			JOIN htlc_vtxo h ON h.id=v.id WHERE c.subscription_id=$1
			AND (NOT c.materialized OR h.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled'))
		", &[&subscription_id])
			.await?.get(0))
	}

	/// This proof concerns the original *incoming sender sources*. It must not
	/// inspect the outgoing receive grants that recovery is about to fulfill.
	/// LnSpent, a closed attempt and shared preimage knowledge are insufficient.
	pub(crate) async fn internal_receive_collected(&self, subscription_id: i64, hash: PaymentHash) -> anyhow::Result<bool> {
		Ok(self.query_one("
			SELECT EXISTS(SELECT 1 FROM expiry_lightning_internal_funding f
			JOIN lightning_payment_attempt a ON a.id=f.id AND a.lightning_htlc_subscription_id=f.subscription_id
			WHERE f.subscription_id=$1 AND a.payment_hash=$2
			AND EXISTS(SELECT 1 FROM htlc_settlement s WHERE s.payment_hash=a.payment_hash)
			AND EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l WHERE l.lightning_payment_attempt_id=a.id)
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l
			LEFT JOIN vtxo v ON v.vtxo_id=l.vtxo_id LEFT JOIN htlc_vtxo h ON h.id=v.id
			WHERE l.lightning_payment_attempt_id=a.id AND (h.direction::TEXT IS DISTINCT FROM 'incoming'
			OR h.payment_hash IS DISTINCT FROM a.payment_hash OR h.offchain_resolution::TEXT IS DISTINCT FROM 'fulfilled')))
		",
			&[&subscription_id, &hash.to_string()]).await?.get(0))
	}

	/// Called only after the original authenticated hold backend reports this
	/// exact invoice Cancelled. Coarse cancellation alone never drops work.
	pub(crate) async fn stop_uncollected_receive_request(
		&self, subscription_id: i64,
		internal_deadline: Option<(super::expiry_chain::ExpiryViewToken, BlockDelta)>,
	) -> anyhow::Result<bool> {
		if let Some((view, _)) = internal_deadline { self.lock_valid_expiry_view(view).await?; }
		let ids = self.query("
			SELECT source_vtxo_id FROM expiry_lightning_receive_claim
			WHERE subscription_id=$1 ORDER BY source_vtxo_id
		", &[&subscription_id]).await?
			.into_iter().map(|r| r.get::<_, &str>("source_vtxo_id").parse()).collect::<Result<Vec<VtxoId>, _>>()?;
		super::expiry_guard::guard_obligation_changes(self, &ids).await?;
		if internal_deadline.is_some() { self.lock_htlc_settlements().await?; }
		let subscription = self.get_htlc_subscription_by_id(subscription_id).await?
			.context("uncollected receive subscription disappeared")?;
		let funding = self.funded_internal_receive(subscription_id, subscription.payment_hash).await?;
		ensure!(funding.is_some() == internal_deadline.is_some(), "receive funding changed before terminal recovery");
		if let Some((view, margin)) = internal_deadline {
			let funding = funding.context("internal receive funding disappeared")?;
			let attempt_id = funding.attempt.id;
			let incoming = self.query_one("SELECT COUNT(*) AS count,MIN(h.htlc_expiry) AS expiry, \
				BOOL_AND(COALESCE(h.direction::TEXT='incoming' AND h.payment_hash=$2,FALSE)) AS valid \
				FROM lightning_payment_attempt_htlc_vtxo l LEFT JOIN vtxo v ON v.vtxo_id=l.vtxo_id \
				LEFT JOIN htlc_vtxo h ON h.id=v.id WHERE l.lightning_payment_attempt_id=$1",
				&[&attempt_id, &subscription.payment_hash.to_string()]).await?;
			ensure!(incoming.get::<_, i64>("count") > 0 && incoming.get::<_, Option<bool>>("valid") == Some(true),
				"internal receive lacks its complete original incoming sources");
			let expiry = BlockHeight::try_from(incoming.get::<_, i32>("expiry"))?;
			let last = view.captaind.height.checked_add(margin).context("internal receive collection margin overflows")?;
			if last <= expiry || funding.claim_complete
				|| self.internal_receive_collected(subscription_id, subscription.payment_hash).await? {
				return Ok(false);
			}
		}
		// Keep actual terminal cancellation independent of a missed coarse-state
		// update. Cancellation must not override an already materialized or
		// fulfilled destination; entitlement recovery proves collection separately.
		let changed = self.execute("
			UPDATE expiry_lightning_receive_replay r SET uncollected_at=NOW()
			FROM lightning_htlc_subscription s WHERE r.subscription_id=$1 AND s.id=r.subscription_id
			AND r.payment_hash=s.payment_hash AND r.uncollected_at IS NULL
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim c JOIN vtxo v ON v.vtxo_id=c.source_vtxo_id
			JOIN htlc_vtxo h ON h.id=v.id WHERE c.subscription_id=s.id AND (c.materialized OR h.offchain_resolution='fulfilled'))
		",
			&[&subscription_id]).await? != 0;
		if changed {
			self.execute("UPDATE vtxo SET updated_at=NOW() WHERE vtxo_id=ANY($1)",
				&[&ids.iter().map(ToString::to_string).collect::<Vec<_>>()]).await?;
		}
		Ok(changed)
	}

	/// Called before recording the preimage or collecting the invoice. A retry
	/// cannot redirect a previously validated claim, even before materialization.
	pub(crate) async fn prepare_receive_claims(&self, bindings: &[ReceiveClaimBinding]) -> anyhow::Result<()> {
		let mut ids = bindings.iter().map(|b| b.source).collect::<Vec<_>>();
		ids.sort_unstable();
		super::expiry_guard::guard_obligation_changes(self, &ids).await?;
		for binding in bindings {
			let row = self.query_opt("
				SELECT v.lightning_htlc_subscription_id AS subscription_id,s.status::TEXT AS status
				FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id
				JOIN lightning_htlc_subscription s ON s.id=v.lightning_htlc_subscription_id
				WHERE v.vtxo_id=$1 AND h.direction='outgoing' AND h.payment_hash=$2 AND s.payment_hash=$2
				AND (s.status IN ('htlcs-ready','settled') OR (s.status='canceled'
				AND EXISTS(SELECT 1 FROM expiry_lightning_receive_replay r WHERE r.subscription_id=s.id)))
			",
				&[&binding.source.to_string(), &binding.payment_hash.to_string()]).await?
				.context("receive claim source is not its subscription's grant")?;
			let subscription: i64 = row.get("subscription_id");
			if row.get::<_, &str>("status") == "canceled" {
				ensure!(self.internal_receive_collected(subscription, binding.payment_hash).await?,
					"cancelled internal receive lacks permanent original-source collection");
			}
			let inserted = self.query_opt("
				INSERT INTO expiry_lightning_receive_claim
				(source_vtxo_id,subscription_id,payment_hash,output_vtxo_id,output_vtxo)
				VALUES($1,$2,$3,$4,$5) ON CONFLICT(source_vtxo_id) DO NOTHING RETURNING source_vtxo_id
			",
				&[&binding.source.to_string(), &subscription, &binding.payment_hash.to_string(),
					&binding.output.id().to_string(), &binding.output.serialize()]).await?.is_some();
			let existing = self.query_one("
				SELECT subscription_id,payment_hash,output_vtxo_id,output_vtxo
				FROM expiry_lightning_receive_claim WHERE source_vtxo_id=$1
			", &[&binding.source.to_string()]).await?;
			let output = Vtxo::<Full>::deserialize(existing.get("output_vtxo"))?;
			ensure!(existing.get::<_, i64>("subscription_id") == subscription
				&& existing.get::<_, &str>("payment_hash") == binding.payment_hash.to_string()
				&& existing.get::<_, &str>("output_vtxo_id") == binding.output.id().to_string()
				&& same_claim_output(&output, &binding.output), "receive claim conflicts with its durable destination");
			if inserted {
				// Invalidate any ownership snapshot prepared before this intent.
				self.execute("UPDATE vtxo SET updated_at=NOW() WHERE vtxo_id=$1", &[&binding.source.to_string()]).await?;
			}
		}
		Ok(())
	}

	/// Must share the actual tree-update/Fulfilled transaction. A missing intent
	/// is an error; this method never silently invents a post-settlement payee.
	pub(crate) async fn materialize_receive_claims(&self, bindings: &[ReceiveClaimBinding]) -> anyhow::Result<()> {
		for binding in bindings {
			let row = self.query_opt("
				SELECT c.output_vtxo,v.vtxo FROM expiry_lightning_receive_claim c
				JOIN vtxo v ON v.vtxo_id=c.output_vtxo_id
				JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id AND source.lightning_htlc_subscription_id=c.subscription_id
				JOIN htlc_vtxo h ON h.id=source.id
				WHERE c.source_vtxo_id=$1 AND c.output_vtxo_id=$2 AND c.payment_hash=$3
				AND h.direction='outgoing' AND h.payment_hash=c.payment_hash AND h.offchain_resolution='fulfilled'
			",
				&[&binding.source.to_string(), &binding.output.id().to_string(), &binding.payment_hash.to_string()]).await?
				.context("receive claim materialization lacks its original binding")?;
			let original = Vtxo::<Full>::deserialize(row.get("output_vtxo"))?;
			let actual = Vtxo::<Full>::deserialize(row.get("vtxo"))?;
			ensure!(same_claim_output(&original, &binding.output) && same_claim_output(&actual, &binding.output),
				"receive claim output differs from its durable destination");
			self.execute("
				UPDATE expiry_lightning_receive_claim SET materialized=TRUE
				WHERE source_vtxo_id=$1 AND NOT materialized
			", &[&binding.source.to_string()]).await?;
		}
		Ok(())
	}

	pub(crate) async fn receive_claim_entitlement(&self, id: VtxoId) -> anyhow::Result<Option<ReceiveClaimEntitlement>> {
		let Some(row) = self.query_opt("
			SELECT c.source_vtxo_id,c.subscription_id,c.payment_hash,
			c.output_vtxo_id,c.output_vtxo,c.materialized,s.status='settled' AS settled,
			(s.status='canceled' OR replay.uncollected_at IS NOT NULL) AS canceled,
			EXISTS(SELECT 1 FROM expiry_lightning_internal_funding f WHERE f.subscription_id=s.id) AS internally_funded,
			h.offchain_resolution::TEXT AS resolution
			FROM expiry_lightning_receive_claim c
			JOIN lightning_htlc_subscription s ON s.id=c.subscription_id AND s.payment_hash=c.payment_hash
			LEFT JOIN expiry_lightning_receive_replay replay ON replay.subscription_id=s.id AND replay.payment_hash=s.payment_hash
			JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id AND source.lightning_htlc_subscription_id=s.id
			JOIN htlc_vtxo h ON h.id=source.id AND h.payment_hash=c.payment_hash AND h.direction='outgoing'
			WHERE c.source_vtxo_id=$1 OR c.output_vtxo_id=$1
		", &[&id.to_string()]).await? else { return Ok(None); };
		let output = Vtxo::<Full>::deserialize(row.get("output_vtxo"))?;
		let output_id: VtxoId = row.get::<_, &str>("output_vtxo_id").parse()?;
		ensure!(output.id() == output_id, "receive binding output identity differs");
		let source: VtxoId = row.get::<_, &str>("source_vtxo_id").parse()?;
		if id == output_id {
			ensure!(row.get::<_, bool>("materialized") && row.get::<_, Option<&str>>("resolution") == Some("fulfilled"),
				"receive output has no committed claim materialization");
		}
		let recipient = match output.policy() { VtxoPolicy::Pubkey(p) => Some(p.user_pubkey), _ => None };
		let subscription_id = row.get("subscription_id");
		let payment_hash = row.get::<_, &str>("payment_hash").parse()?;
		let internally_collected = row.get::<_, bool>("internally_funded")
			&& self.internal_receive_collected(subscription_id, payment_hash).await?;
		Ok(Some(ReceiveClaimEntitlement {
			source, subscription_id, payment_hash,
			output_id, recipient, settlement: receive_claim_settlement(
				row.get("settled"), row.get("canceled"), row.get("materialized"), internally_collected,
				row.get::<_, Option<&str>>("resolution") == Some("fulfilled"),
			),
		}))
	}
}

// Canceling the native hold invoice is part of a successful intra-Ark payment,
// so it cannot erase actual source-specific collection. Funding alone is not
// collection: the original claim can expire after its destination was recorded.
fn receive_claim_settlement(settled: bool, canceled: bool, materialized: bool, internally_collected: bool, fulfilled: bool) -> ReceiveClaimSettlement {
	if settled { ReceiveClaimSettlement::Settled }
	else if canceled && !materialized && !internally_collected && !fulfilled { ReceiveClaimSettlement::CancelledUncollected }
	else { ReceiveClaimSettlement::Pending }
}

// The validated output identity commits to its policy and amount. Signatures
// supplied later and client nonce retransmission do not change its beneficiary.
fn same_claim_output(a: &Vtxo<Full>, b: &Vtxo<Full>) -> bool {
	a.id() == b.id() && a.policy() == b.policy() && a.amount() == b.amount()
		&& a.chain_anchor() == b.chain_anchor() && a.server_pubkey() == b.server_pubkey()
		&& a.expiry_height() == b.expiry_height() && a.exit_delta() == b.exit_delta()
}

#[cfg(test)]
mod tests {
	use super::*;
	use ark::test_util::VTXO_VECTORS;
	use ark::ServerVtxo;
	use bitcoin::secp256k1::Keypair;
	use std::str::FromStr;
	use ark::vtxo::raw::RawVtxo;

	#[test]
	fn receive_request_roundtrip_preserves_original_nonce_attestation_and_transaction_identity() {
		for version in [2, 5] {
			let input = VTXO_VECTORS.arkoor3_vtxo.clone();
			let destination = VtxoPolicy::new_pubkey(VTXO_VECTORS.arkoor3_user_key.public_key());
			let builder = if version < server_rpc::pver::PROTOCOL_VERSION_LN_RECEIVE_CHECKPOINT {
				ArkoorPackageBuilder::new_claim_all_without_checkpoints([input.clone()], destination)
			} else {
				ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()], destination)
			}.unwrap().generate_user_nonces(&[VTXO_VECTORS.arkoor3_user_key]).unwrap();
			let full = builder.cosign_request();
			let preimage = Preimage::from([7; 32]);
			let original = ReceiveClaimRequest { payment_hash: preimage.compute_payment_hash(), preimage,
				request: full.clone().convert_vtxo(|v| v.id()), protocol_version: version };
			let bytes = original.encode().unwrap();
			let decoded = ReceiveClaimRequest::decode(RECEIVE_REQUEST_ENCODING, version as i64, &bytes).unwrap();
			assert_eq!(decoded.protocol_version, version);
			assert_eq!(decoded.request.requests, original.request.requests);
			assert_eq!(decoded.preimage, original.preimage);
			assert_eq!(decoded.encode().unwrap(), bytes);
			let expected = builder.build_unsigned_vtxos().map(|coin|
				(coin.id(), coin.amount(), coin.policy().clone())).collect::<Vec<_>>();
			let resumed = decoded.request.set_vtxos([input]).unwrap();
			let response = ArkoorPackageBuilder::from_cosign_request(resumed).unwrap()
				.server_cosign(&VTXO_VECTORS.server_key).unwrap().cosign_response();
			// Upstream partial signing includes fresh randomness. Verify the
			// resumed response with the original user's single nonce session;
			// do not require identical response bytes or reuse a secret nonce.
			let completed = builder.user_cosign(&[VTXO_VECTORS.arkoor3_user_key], response)
				.unwrap().build_signed_vtxos();
			let actual = completed.iter().map(|coin|
				(coin.id(), coin.amount(), coin.policy().clone())).collect::<Vec<_>>();
			assert_eq!(actual, expected, "stored request must preserve the original transaction identities and destinations");
		}
	}

	#[test]
	fn receive_request_rejects_unsupported_or_corrupt_storage_before_replay() {
		assert!(ReceiveClaimRequest::decode(2, 5, &[]).is_err());
		assert!(ReceiveClaimRequest::decode(1, 0, &[]).is_err());
		assert!(ReceiveClaimRequest::decode(1, 5, &[255]).is_err());
		assert!(ReceiveClaimRequest::decode(1, 5, &vec![0; MAX_RECEIVE_REQUEST_BYTES + 1]).is_err());
		let preimage = Preimage::from([8; 32]);
		let message = protos::ClaimLightningReceiveRequest {
			payment_hash: preimage.compute_payment_hash().to_vec(),
			payment_preimage: Preimage::from([9; 32]).to_vec(), cosign_request: None,
		};
		assert!(ReceiveClaimRequest::decode(1, 5, &message.encode_to_vec()).err().unwrap().to_string().contains("preimage differs"));
	}

	#[test]
	fn receive_claim_uses_final_recipient_instead_of_provider_checkpoint() {
		let input = VTXO_VECTORS.round2_vtxo.clone();
		let provider = Keypair::from_str("c0b645b01cac427717a18b30c7c9238dee2b3885f659930144fbe05061ad6166").unwrap();
		let recipient = VTXO_VECTORS.board_vtxo.user_pubkey();
		assert_ne!(provider.public_key(), recipient);
		let user = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()], VtxoPolicy::new_pubkey(recipient))
			.unwrap().generate_user_nonces(&[provider]).unwrap();
		let server = ArkoorPackageBuilder::from_cosign_request(user.cosign_request()).unwrap();
		let bindings = ReceiveClaimBinding::from_package(&server).unwrap();
		assert_eq!(bindings.len(), 1);
		assert_eq!(bindings[0].source, input.id());
		assert_eq!(bindings[0].output.user_pubkey(), recipient);
		assert_eq!(bindings[0].output.amount(), input.amount());
		assert_eq!(bindings[0].output.id(), server.build_unsigned_vtxos().next().unwrap().id());
		assert!(!server.build_unsigned_internal_vtxos().is_empty(), "real checkpoint is part of the source path");
	}

	#[test]
	fn receive_claim_rejects_conditional_destination_before_intent_or_settlement() {
		let input = VTXO_VECTORS.round2_vtxo.clone();
		let provider = Keypair::from_str("c0b645b01cac427717a18b30c7c9238dee2b3885f659930144fbe05061ad6166").unwrap();
		let hash = PaymentHash::from([9; 32]);
		let destination = VtxoPolicy::new_server_htlc_send(provider.public_key(), hash, bitcoin_ext::BlockHeight::new(100));
		let user = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input], destination)
			.unwrap().generate_user_nonces(&[provider]).unwrap();
		let server = ArkoorPackageBuilder::from_cosign_request(user.cosign_request()).unwrap();
		let err = ReceiveClaimBinding::from_package(&server).unwrap_err();
		assert!(err.to_string().contains("destination must be a pubkey policy"));
	}

	#[test]
	fn receive_claim_does_not_bind_ordinary_unregistered_transfers() {
		let key = VTXO_VECTORS.arkoor3_user_key;
		let user = ArkoorPackageBuilder::new_claim_all_with_checkpoints(
			[VTXO_VECTORS.arkoor3_vtxo.clone()], VtxoPolicy::new_pubkey(key.public_key()),
		).unwrap().generate_user_nonces(&[key]).unwrap();
		let server = ArkoorPackageBuilder::from_cosign_request(user.cosign_request()).unwrap();
		assert!(ReceiveClaimBinding::from_package(&server).unwrap().is_empty());
	}

	#[test]
	fn receive_claim_cancelled_uncollected_intent_preserves_original_grant_recovery() {
		// A funded but uncollected internal request has the same recovery
		// classification as an uncollected external request.
		assert_eq!(receive_claim_settlement(false, true, false, false, false), ReceiveClaimSettlement::CancelledUncollected);
		assert_eq!(receive_claim_settlement(false, false, false, false, false), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(false, true, true, false, true), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(false, true, false, true, false), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(false, true, true, false, false), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(false, true, false, false, true), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(true, true, false, false, false), ReceiveClaimSettlement::Settled);
		assert_eq!(receive_claim_settlement(true, false, true, false, true), ReceiveClaimSettlement::Settled);
	}

	#[test]
	fn receive_claim_retry_ignores_signature_upload_but_not_destination() {
		let signed = VTXO_VECTORS.arkoor2_vtxo.clone();
		let mut other = signed.clone();
		other.invalidate_final_sig();
		assert!(same_claim_output(&signed, &other));
		assert!(!same_claim_output(&signed, &VTXO_VECTORS.board_vtxo));
		let mut raw: RawVtxo = ServerVtxo::from(signed.clone()).into();
		raw.amount += bitcoin::Amount::from_sat(1);
		let changed: ServerVtxo<Full> = raw.into();
		let changed = changed.try_into_user_vtxo().unwrap();
		assert!(!same_claim_output(&signed, &changed));
	}
}
