//! The intended beneficiary of one validated Lightning receive claim.
//!
//! Unlike an ordinary undelivered OOR, collecting a Lightning receive already
//! commits the server to the claim's final destination. Persist that destination
//! before releasing the preimage, independently of checkpoint custody or delivery.

use anyhow::Context;
use bitcoin::secp256k1::PublicKey;

use ark::{ProtocolEncoding, Vtxo, VtxoId, VtxoPolicy};
use ark::arkoor::package::ArkoorPackageBuilder;
use ark::arkoor::state::ServerCanCosign;
use ark::lightning::PaymentHash;
use ark::vtxo::Full;

use super::Tx;

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
	/// Called before recording the preimage or collecting the invoice. A retry
	/// cannot redirect a previously validated claim, even before materialization.
	pub(crate) async fn prepare_receive_claims(&self, bindings: &[ReceiveClaimBinding]) -> anyhow::Result<()> {
		let mut ids = bindings.iter().map(|b| b.source).collect::<Vec<_>>();
		ids.sort_unstable();
		super::expiry_guard::guard_obligation_changes(self, &ids).await?;
		for binding in bindings {
			let row = self.query_opt("SELECT v.lightning_htlc_subscription_id AS subscription_id \
				FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id \
				JOIN lightning_htlc_subscription s ON s.id=v.lightning_htlc_subscription_id \
				WHERE v.vtxo_id=$1 AND h.direction='outgoing' AND h.payment_hash=$2 AND s.payment_hash=$2 \
				AND s.status IN ('htlcs-ready','settled')",
				&[&binding.source.to_string(), &binding.payment_hash.to_string()]).await?
				.context("receive claim source is not its subscription's grant")?;
			let subscription: i64 = row.get("subscription_id");
			let inserted = self.query_opt("INSERT INTO expiry_lightning_receive_claim \
				(source_vtxo_id,subscription_id,payment_hash,output_vtxo_id,output_vtxo) \
				VALUES($1,$2,$3,$4,$5) ON CONFLICT(source_vtxo_id) DO NOTHING RETURNING source_vtxo_id",
				&[&binding.source.to_string(), &subscription, &binding.payment_hash.to_string(),
					&binding.output.id().to_string(), &binding.output.serialize()]).await?.is_some();
			let existing = self.query_one("SELECT subscription_id,payment_hash,output_vtxo_id,output_vtxo \
				FROM expiry_lightning_receive_claim WHERE source_vtxo_id=$1", &[&binding.source.to_string()]).await?;
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
			let row = self.query_opt("SELECT c.output_vtxo,v.vtxo FROM expiry_lightning_receive_claim c \
				JOIN vtxo v ON v.vtxo_id=c.output_vtxo_id \
				JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id AND source.lightning_htlc_subscription_id=c.subscription_id \
				JOIN htlc_vtxo h ON h.id=source.id \
				WHERE c.source_vtxo_id=$1 AND c.output_vtxo_id=$2 AND c.payment_hash=$3 \
				AND h.direction='outgoing' AND h.payment_hash=c.payment_hash AND h.offchain_resolution='fulfilled'",
				&[&binding.source.to_string(), &binding.output.id().to_string(), &binding.payment_hash.to_string()]).await?
				.context("receive claim materialization lacks its original binding")?;
			let original = Vtxo::<Full>::deserialize(row.get("output_vtxo"))?;
			let actual = Vtxo::<Full>::deserialize(row.get("vtxo"))?;
			ensure!(same_claim_output(&original, &binding.output) && same_claim_output(&actual, &binding.output),
				"receive claim output differs from its durable destination");
			self.execute("UPDATE expiry_lightning_receive_claim SET materialized=TRUE \
				WHERE source_vtxo_id=$1 AND NOT materialized", &[&binding.source.to_string()]).await?;
		}
		Ok(())
	}

	pub(crate) async fn receive_claim_entitlement(&self, id: VtxoId) -> anyhow::Result<Option<ReceiveClaimEntitlement>> {
		let Some(row) = self.query_opt("SELECT c.source_vtxo_id,c.subscription_id,c.payment_hash, \
			c.output_vtxo_id,c.output_vtxo,c.materialized,s.status='settled' AS settled, \
			s.status='canceled' AS canceled, \
			EXISTS(SELECT 1 FROM expiry_lightning_internal_funding f WHERE f.subscription_id=s.id) AS internally_funded, \
			h.offchain_resolution::TEXT AS resolution \
			FROM expiry_lightning_receive_claim c \
			JOIN lightning_htlc_subscription s ON s.id=c.subscription_id AND s.payment_hash=c.payment_hash \
			JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id AND source.lightning_htlc_subscription_id=s.id \
			JOIN htlc_vtxo h ON h.id=source.id AND h.payment_hash=c.payment_hash AND h.direction='outgoing' \
			WHERE c.source_vtxo_id=$1 OR c.output_vtxo_id=$1", &[&id.to_string()]).await? else { return Ok(None); };
		let output = Vtxo::<Full>::deserialize(row.get("output_vtxo"))?;
		let output_id: VtxoId = row.get::<_, &str>("output_vtxo_id").parse()?;
		ensure!(output.id() == output_id, "receive binding output identity differs");
		let source: VtxoId = row.get::<_, &str>("source_vtxo_id").parse()?;
		if id == output_id {
			ensure!(row.get::<_, bool>("materialized") && row.get::<_, Option<&str>>("resolution") == Some("fulfilled"),
				"receive output has no committed claim materialization");
		}
		let recipient = match output.policy() { VtxoPolicy::Pubkey(p) => Some(p.user_pubkey), _ => None };
		Ok(Some(ReceiveClaimEntitlement {
			source, subscription_id: row.get("subscription_id"), payment_hash: row.get::<_, &str>("payment_hash").parse()?,
			output_id, recipient, settlement: receive_claim_settlement(
				row.get("settled"), row.get("canceled"), row.get("materialized"), row.get("internally_funded"),
				row.get::<_, Option<&str>>("resolution") == Some("fulfilled"),
			),
		}))
	}
}

// Canceling the native hold invoice is part of a successful intra-Ark payment,
// so it cannot erase that payment's actual source-specific funding obligation.
fn receive_claim_settlement(settled: bool, canceled: bool, materialized: bool, internally_funded: bool, fulfilled: bool) -> ReceiveClaimSettlement {
	if settled { ReceiveClaimSettlement::Settled }
	else if canceled && !materialized && !internally_funded && !fulfilled { ReceiveClaimSettlement::CancelledUncollected }
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
		assert_eq!(receive_claim_settlement(false, true, false, false, false), ReceiveClaimSettlement::CancelledUncollected);
		assert_eq!(receive_claim_settlement(false, false, false, false, false), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(false, true, true, false, true), ReceiveClaimSettlement::Pending);
		assert_eq!(receive_claim_settlement(false, true, false, true, false), ReceiveClaimSettlement::Pending);
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
