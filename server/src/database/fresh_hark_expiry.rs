//! Narrow same-carrier retirement proof for one fresh original and one hArk replacement.
//! No participation is marked forfeited/completed and no unlock material is disclosed.
use super::carrier_snapshot::{BackingSnapshot, BackingSnapshotLimits};
use super::expiry_ownership::{
	DependencySource, OperatorRecovery, OwnershipContext, OwnershipDecision, OwnershipDisposition,
	RetirementRule,
};
use super::{BlockTable, SpendState, Tx};
use anyhow::Context;
use ark::tree::signed::UnlockHash;
use ark::vtxo::Full;
use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoId};
use bitcoin::Network;
use bitcoin::hashes::{Hash, sha256};
use std::collections::BTreeSet;

pub(super) struct FreshHarkRetirement {
	hash: UnlockHash,
	original: VtxoId,
	replacement: VtxoId,
	roots: BTreeSet<VtxoId>,
	pub evidence: serde_json::Value,
}

impl FreshHarkRetirement {
	/// Dependencies remain mandatory: only these exact two source decisions may
	/// discharge their original set in this exact multi-input carrier.
	pub fn covers(&self, decision: &OwnershipDecision, roots: &BTreeSet<VtxoId>) -> bool {
		roots == &self.roots
			&& ((decision.id == self.original
				&& matches!(decision.disposition, OwnershipDisposition::ConditionalOriginalRecovery(h) if h == self.hash))
				|| (decision.id == self.replacement
					&& matches!(decision.disposition, OwnershipDisposition::Customer(_))))
			&& decision.dependencies.len() == 1
			&& decision.dependencies.iter().all(|d| {
				d.source == DependencySource::Exchange(self.hash)
					&& d.ids == [self.original]
					&& match d.rule {
						RetirementRule::CompleteExchangeClosure => decision.id == self.original,
						RetirementRule::ConfirmedOrSameCarrier => decision.id == self.replacement,
					}
			})
	}
	pub fn operator_reason(&self) -> OperatorRecovery {
		OperatorRecovery::SameCarrierFreshExchange(self.hash)
	}
}

impl Tx<'_> {
	/// Scheduling hint only. It confers no ownership; the complete association is
	/// independently reread under decisive source locks before allocation.
	pub(crate) async fn fresh_hark_expiry_anchors(
		&self,
		anchor: &str,
	) -> anyhow::Result<Vec<String>> {
		let rows = self.query("SELECT DISTINCT p.unlock_hash FROM round_participation p JOIN round_part_input i ON i.participation_id=p.id JOIN vtxo v ON v.vtxo_id=i.vtxo_id WHERE p.forfeited_at IS NULL AND p.round_id IS NOT NULL AND (v.anchor_point=$1 OR p.round_id=split_part($1,':',1)) ORDER BY p.unlock_hash LIMIT 2", &[&anchor]).await?;
		ensure!(
			rows.len() <= 1,
			"fresh hArk scheduling refuses multiple participations"
		);
		let Some(row) = rows.first() else {
			return Ok(vec![anchor.to_owned()]);
		};
		let hash: &str = row.get("unlock_hash");
		let inputs = self.query("SELECT v.anchor_point FROM round_part_input i JOIN round_participation p ON p.id=i.participation_id JOIN vtxo v ON v.vtxo_id=i.vtxo_id WHERE p.unlock_hash=$1 ORDER BY i.vtxo_id LIMIT 2", &[&hash]).await?;
		ensure!(
			inputs.len() == 1,
			"fresh hArk scheduling requires one economic original"
		);
		let original: String = inputs[0].get("anchor_point");
		let round = self.query_one("SELECT r.signed_tree, octet_length(r.signed_tree)::bigint AS bytes FROM round r JOIN round_participation p ON p.round_id=r.funding_txid WHERE p.unlock_hash=$1 AND octet_length(r.signed_tree)<=1048576", &[&hash]).await?;
		let tree = ark::tree::signed::SignedVtxoTreeSpec::deserialize(
			round.get::<_, &[u8]>("signed_tree"),
		)?;
		ensure!(
			tree.spec.vtxos.len() == 2,
			"fresh hArk scheduling requires replacement and padding only"
		);
		let replacement = tree.utxo.to_string();
		ensure!(
			original != replacement && (anchor == original || anchor == replacement),
			"fresh hArk physical anchor association differs"
		);
		Ok(BTreeSet::from([original, replacement])
			.into_iter()
			.collect())
	}

	/// Caller already holds every source flux/row lock, settlement lock and
	/// carrier lock. No partial receipt, source-state edit or allocation occurs.
	pub(super) async fn fresh_hark_retirement(
		&self,
		snapshot: &BackingSnapshot,
		terminals: &[ServerVtxo<Full>],
		decisions: &[OwnershipDecision],
		context: &OwnershipContext<'_>,
		budget: BackingSnapshotLimits,
		network: Network,
		actual_core_tip: Option<bitcoin_ext::BlockRef>,
	) -> anyhow::Result<Option<FreshHarkRetirement>> {
		let conditional = decisions
			.iter()
			.filter_map(|d| match d.disposition {
				OwnershipDisposition::ConditionalOriginalRecovery(hash) => Some((d, hash)),
				_ => None,
			})
			.collect::<Vec<_>>();
		if conditional.is_empty() {
			return Ok(None);
		}
		ensure!(
			conditional.len() == 1 && snapshot.pending_boards.is_empty() && terminals.len() == 3,
			"fresh hArk retirement requires one original, replacement and padding; no mixed closure"
		);
		let (original_decision, hash) = conditional[0];
		let row = self.query_one("SELECT id, round_id, forfeited_at IS NULL AS unfinished, unlock_preimage IS NOT NULL AS has_unlock FROM round_participation WHERE unlock_hash=$1 FOR UPDATE", &[&hash.to_string()]).await?;
		ensure!(
			row.get::<_, bool>("unfinished") && row.get::<_, bool>("has_unlock"),
			"fresh hArk participation is not an issued unfinished source"
		);
		let part_id: i64 = row.get("id");
		let inputs = self.query("SELECT vtxo_id, signed_forfeit_tx IS NOT NULL AS forfeited FROM round_part_input WHERE participation_id=$1 ORDER BY vtxo_id LIMIT 2 FOR UPDATE", &[&part_id]).await?;
		let outputs = self.query("SELECT participation_id, octet_length(policy)::bigint AS bytes FROM round_part_output WHERE participation_id=$1 LIMIT 2 FOR UPDATE", &[&part_id]).await?;
		ensure!(
			inputs.len() == 1
				&& outputs.len() == 1
				&& outputs[0].get::<_, i64>("bytes") <= 4096
				&& !inputs[0].get::<_, bool>("forfeited")
				&& inputs[0].get::<_, &str>("vtxo_id") == original_decision.id.to_string(),
			"fresh hArk exact unsigned original/output association required"
		);
		let round_id: ark::rounds::RoundId = row
			.get::<_, Option<&str>>("round_id")
			.context("fresh hArk round not issued")?
			.parse()?;
		let sizes = self.query_one("SELECT (octet_length(signed_tree)::bigint + octet_length(funding_tx)::bigint) AS bytes FROM round WHERE funding_txid=$1", &[&round_id.to_string()]).await?;
		ensure!(
			usize::try_from(sizes.get::<_, i64>("bytes"))?
				<= budget.max_serialized_bytes.min(1048576),
			"fresh hArk signed tree exceeds bounded source budget"
		);
		let part = self
			.get_round_participation_by_unlock_hash(hash)
			.await?
			.context("fresh hArk participation vanished")?;
		ensure!(
			part.inputs.len() == 1
				&& part.outputs.len() == 1
				&& part.forfeited_at.is_none()
				&& part.inputs[0].signed_forfeit_tx.is_none(),
			"fresh hArk source changed"
		);
		ensure!(
			UnlockHash::hash(part.unlock_preimage.leak_ref()) == hash,
			"fresh hArk unlock binding differs"
		);
		let round = self
			.get_round(round_id)
			.await?
			.context("fresh hArk round missing")?;
		ensure!(round.swept_at.is_none(), "fresh hArk round already retired");
		let round_db_id = round.id;
		let tree = round.into_cached_tree()?;
		ensure!(
			tree.spec.spec.vtxos.len() == 2,
			"fresh hArk refuses shared round participation"
		);
		let indexes = tree
			.spec
			.spec
			.leaf_idxs_for_participation(hash, part.outputs.iter().map(|o| &o.vtxo_request))
			.context("fresh hArk request not in signed tree")?;
		ensure!(indexes.len() == 1, "fresh hArk replacement mapping differs");
		let replacement = ServerVtxo::from(tree.build_vtxo(indexes[0]));
		let original = terminals
			.iter()
			.find(|v| v.id() == original_decision.id)
			.context("fresh hArk original path missing")?;
		ensure!(
			original.amount() == replacement.amount()
				&& matches!(original.policy(),ServerVtxoPolicy::User(p) if p.as_pubkey().is_some())
				&& matches!(replacement.policy(),ServerVtxoPolicy::User(p) if p.as_pubkey().is_some()),
			"fresh hArk requires equal-value plain pubkey exchange"
		);
		let original_user = original
			.clone()
			.try_into_user_vtxo()
			.ok()
			.context("fresh hArk original is not a user source")?;
		let replacement_user = replacement
			.clone()
			.try_into_user_vtxo()
			.ok()
			.context("fresh hArk replacement is not a user source")?;
		let (old_mailbox, old_record, old_link) = self
			.fallback_proof(original_user.user_pubkey(), network)
			.await?;
		let (new_mailbox, new_record, new_link) = self
			.fallback_proof(replacement_user.user_pubkey(), network)
			.await?;
		ensure!(
			old_mailbox == new_mailbox && old_record.spk == new_record.spk,
			"fresh hArk requires the same canonical mailbox owner and immutable payout destination"
		);
		let original_fact = snapshot
			.regular
			.iter()
			.find(|f| f.state.vtxo_id == original.id())
			.context("fresh hArk original metadata missing")?;
		let replacement_fact = snapshot
			.regular
			.iter()
			.find(|f| f.state.vtxo_id == replacement.id())
			.context("fresh hArk replacement metadata missing")?;
		ensure!(
			original_fact.state.spend_state == SpendState::Spent
				&& original_fact.state.spent_in_round == Some(round_db_id)
				&& replacement_fact.state.spend_state == SpendState::Unclaimed
				&& replacement_fact.state.spent_in_round.is_none(),
			"fresh hArk current source states differ"
		);
		for fact in [original_fact, replacement_fact] {
			ensure!(
				fact.state.confirmed_height.is_none()
					&& fact.state.oor_spent_txid.is_none()
					&& fact.state.offboarded_in.is_none()
					&& fact.onchain_spent_txid.is_none()
					&& fact.lightning_htlc_subscription_id.is_none(),
				"fresh hArk source is exited, consumed or mixed with Lightning"
			);
		}
		let actual_replacement = terminals
			.iter()
			.find(|v| v.id() == replacement.id())
			.context("fresh hArk replacement path absent")?;
		ensure!(
			actual_replacement.serialize() == replacement.serialize(),
			"fresh hArk full replacement differs from signed request/tree"
		);
		let padding = terminals
			.iter()
			.filter(|v| v.id() != original.id() && v.id() != replacement.id())
			.collect::<Vec<_>>();
		ensure!(
			padding.len() == 1
				&& super::expiry_ownership::is_padding(padding[0])
				&& tree
					.output_vtxos()
					.any(|v| ServerVtxo::from(v).serialize() == padding[0].serialize()),
			"fresh hArk exact signed padding missing"
		);
		let roots = snapshot
			.roots
			.iter()
			.map(|r| r.id())
			.collect::<BTreeSet<_>>();
		let anchors = BTreeSet::from([
			VtxoId::from(original.chain_anchor()),
			VtxoId::from(replacement.chain_anchor()),
		]);
		ensure!(
			roots.len() == 2 && roots == anchors,
			"fresh hArk requires both complete independent physical roots"
		);
		let cap = self
			.get_highest_block(BlockTable::Captaind)
			.await?
			.context("fresh hArk cap tip missing")?;
		let watch = self
			.get_highest_block(BlockTable::Watchmand)
			.await?
			.context("fresh hArk owning watch tip missing")?;
		let actual_core_tip =
			actual_core_tip.context("fresh hArk requires actual native Core tip provenance")?;
		ensure!(
			cap == watch
				&& watch == actual_core_tip
				&& actual_core_tip.height.to_u32() == context.tip,
			"fresh hArk awaits matching owning watch and Core-height source"
		);
		let proof = FreshHarkRetirement {
			hash,
			original: original.id(),
			replacement: replacement.id(),
			roots,
			evidence: serde_json::json!({"version":1,"kind":"fresh-single-hark-same-carrier-retirement","unlock_hash":hash.to_string(),
                "original":original.id().to_string(),"replacement":replacement.id().to_string(),"round":round_id.to_string(),
                "original_path_sha256":sha256::Hash::hash(&original.serialize()).to_string(),
                "replacement_path_sha256":sha256::Hash::hash(&replacement.serialize()).to_string(),
                "original_record_sha256":sha256::Hash::hash(&old_record.serialize()).to_string(),
                "replacement_record_sha256":sha256::Hash::hash(&new_record.serialize()).to_string(),
                "original_link_sha256":sha256::Hash::hash(&old_link.serialize()).to_string(),
                "replacement_link_sha256":sha256::Hash::hash(&new_link.serialize()).to_string(),
                "retirement":"both-physical-roots-in-every-version; payment-only-on-canonical-confirmation", "participation_marked_complete":false}),
		};
		ensure!(
			decisions.iter().all(|d| if d.dependencies.is_empty() {
				matches!(
					d.disposition,
					OwnershipDisposition::Operator(OperatorRecovery::Padding)
				)
			} else {
				proof.covers(d, &proof.roots)
			}),
			"fresh hArk refuses other conditional or customer sources"
		);
		ensure!(
			decisions.iter().any(|d| d.id == proof.replacement
				&& matches!(d.disposition, OwnershipDisposition::Customer(_))),
			"fresh hArk sole replacement customer required"
		);
		Ok(Some(proof))
	}
}

#[cfg(test)]
mod test {
	use super::super::expiry_ownership::{ExpiryInput, ExpirySource, RetirementDependency};
	use super::*;
	fn fixture() -> (FreshHarkRetirement, OwnershipDecision, OwnershipDecision) {
		let original = ark::test_util::VTXO_VECTORS.board_vtxo.id();
		let replacement = ark::test_util::VTXO_VECTORS.arkoor2_vtxo.id();
		let hash = UnlockHash::hash(b"synthetic-fresh-hark-dependency");
		let roots = BTreeSet::from([original, replacement]);
		let dependency = |rule| {
			vec![RetirementDependency {
				source: DependencySource::Exchange(hash),
				ids: vec![original],
				rule,
			}]
		};
		let old = OwnershipDecision {
			id: original,
			gross: bitcoin::Amount::from_sat(10000),
			disposition: OwnershipDisposition::ConditionalOriginalRecovery(hash),
			dependencies: dependency(RetirementRule::CompleteExchangeClosure),
		};
		let new = OwnershipDecision {
			id: replacement,
			gross: bitcoin::Amount::from_sat(10000),
			disposition: OwnershipDisposition::Customer(ExpiryInput {
				id: replacement,
				source: ExpirySource::Registered,
				owner: ark::test_util::VTXO_VECTORS.arkoor2_vtxo.user_pubkey(),
			}),
			dependencies: dependency(RetirementRule::ConfirmedOrSameCarrier),
		};
		(
			FreshHarkRetirement {
				hash,
				original,
				replacement,
				roots,
				evidence: serde_json::Value::Null,
			},
			old,
			new,
		)
	}
	#[test]
	fn same_carrier_hark_dependency_requires_both_roots_and_exact_rule() {
		let (proof, mut old, mut new) = fixture();
		assert!(proof.covers(&old, &proof.roots) && proof.covers(&new, &proof.roots));
		assert!(!proof.covers(&old, &BTreeSet::from([proof.original])));
		assert!(!proof.covers(&new, &BTreeSet::from([proof.replacement])));
		let extra = VtxoId::from(bitcoin::OutPoint::null());
		assert!(!proof.covers(
			&new,
			&BTreeSet::from([proof.original, proof.replacement, extra])
		));
		old.dependencies[0].rule = RetirementRule::ConfirmedOrSameCarrier;
		new.dependencies[0].rule = RetirementRule::CompleteExchangeClosure;
		assert!(!proof.covers(&old, &proof.roots) && !proof.covers(&new, &proof.roots));
	}
	#[test]
	fn same_carrier_hark_dependency_refuses_cross_exchange_subset_and_unknown() {
		let (proof, mut old, mut new) = fixture();
		old.dependencies[0].source =
			DependencySource::Exchange(UnlockHash::hash(b"another-exchange"));
		assert!(!proof.covers(&old, &proof.roots));
		new.dependencies[0].ids = vec![proof.replacement];
		assert!(!proof.covers(&new, &proof.roots));
		let (_, mut old, _) = fixture();
		old.dependencies[0].ids.clear();
		assert!(!proof.covers(&old, &proof.roots));
		let (_, mut old, _) = fixture();
		old.dependencies[0].ids.push(proof.original);
		assert!(!proof.covers(&old, &proof.roots));
		let (_, mut old, _) = fixture();
		old.dependencies.push(old.dependencies[0].clone());
		assert!(!proof.covers(&old, &proof.roots));
		let (_, mut old, _) = fixture();
		old.disposition = OwnershipDisposition::Pending(
			super::super::expiry_ownership::OwnershipPending::MissingPath,
		);
		assert!(!proof.covers(&old, &proof.roots));
	}
}
