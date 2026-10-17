//! Admission of NEW receive grants. Existing HtlcsReady retries bypass this
//! module and retain their exact original grant set. This is a per-attempt
//! size/standardness gate, not connected-component discovery or recovery.

use std::collections::BTreeSet;

use anyhow::{Context, ensure};
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::PublicKey;

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, Vtxo, VtxoId, VtxoPolicy};
use ark::lightning::PaymentHash;
use ark::vtxo::{Bare, Full, TapScriptClause};
use ark::vtxo::policy::{MAX_BLOCK_DELTA, MAX_BLOCK_HEIGHT};
use bitcoin_ext::{BlockHeight, P2TR_DUST};

use crate::database::{self, SpendState};
use crate::database::htlc_vtxo::{self, HtlcDirection};
use crate::database::expiry_internal_refund::MAX_JOINT_GRANTS;

pub(super) struct ReceiveGrantBudget {
	subscription: i64,
	hash: PaymentHash,
	attempt: Option<i64>,
	// Exact admitted source/checkpoint bodies, not a current hash lookup.
	originals: Vec<(VtxoId, Vec<u8>)>,
	checkpoints: Vec<ServerVtxo<Full>>,
}

impl ReceiveGrantBudget {
	pub(super) fn source_ids(&self) -> impl Iterator<Item = VtxoId> + '_ {
		self.originals.iter().map(|v| v.0).chain(self.checkpoints.iter().map(|v| v.id()))
	}

	pub(super) async fn load(tx: &database::Tx<'_>, subscription: i64, policy: &VtxoPolicy) -> anyhow::Result<Self> {
		let hash = policy.as_server_htlc_recv().context("new receive grant has unsupported policy")?.payment_hash;
		let mut result = Self { subscription, hash, attempt: None, originals: Vec::new(), checkpoints: Vec::new() };
		let Some(funding) = tx.funded_internal_receive(subscription, hash).await? else { return Ok(result); };
		ensure!(!funding.claim_complete, "internal receive was already materialized");
		result.attempt = Some(funding.attempt.id);
		// No new grant may reopen a completed/ambiguous materialization or mix
		// externally dispatched obligations with a new internal refund budget.
		let safe = tx.query_one("SELECT \
			NOT EXISTS(SELECT 1 FROM vtxo WHERE lightning_htlc_subscription_id=$1) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_claim WHERE subscription_id=$1) \
			AND NOT EXISTS(SELECT 1 FROM expiry_lightning_receive_replay WHERE subscription_id=$1) \
			AND NOT EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$3) \
			AND NOT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo original \
				JOIN lightning_payment_attempt_htlc_vtxo other ON other.vtxo_id=original.vtxo_id \
				WHERE original.lightning_payment_attempt_id=$2 AND ( \
					EXISTS(SELECT 1 FROM expiry_lightning_dispatch d WHERE d.id=other.lightning_payment_attempt_id) \
					OR EXISTS(SELECT 1 FROM expiry_lightning_external_success s WHERE s.id=other.lightning_payment_attempt_id) \
					OR (other.lightning_payment_attempt_id<>$2 AND EXISTS(SELECT 1 FROM expiry_lightning_queued_attempt q \
						WHERE q.id=other.lightning_payment_attempt_id)))) AS safe",
			&[&subscription, &funding.attempt.id, &hash.to_string()]).await?.get::<_, bool>("safe");
		ensure!(safe, "internal receive no longer has an unmaterialized source scope");
		let rows = tx.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
			WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id LIMIT $2",
			&[&funding.attempt.id, &(MAX_JOINT_GRANTS+1)]).await?;
		ensure!(!rows.is_empty() && rows.len() <= MAX_JOINT_GRANTS as usize,
			"new receive's complete original funding set exceeds its refund budget");
		let mut checkpoint_ids = BTreeSet::new();
		for row in rows {
			let id: VtxoId = row.get::<_, &str>("vtxo_id").parse()?;
			let source = htlc_vtxo::get_htlc_vtxo(tx, id).await?.context("internal funding source missing")?;
			let signed = &source.vtxo.vtxo;
			let incoming = signed.policy().as_server_htlc_send().context("internal source is not a signed incoming HTLC")?;
			ensure!(incoming.payment_hash == hash && source.htlc.payment_hash == hash
				&& source.htlc.htlc_expiry == incoming.htlc_expiry && source.htlc.direction == HtlcDirection::Incoming
				&& source.htlc.resolution().is_none() && !source.vtxo.decided_for_fallback
				&& matches!(source.vtxo.spend_state, SpendState::Spendable | SpendState::LnSpent)
				&& source.vtxo.oor_spent_txid.is_none() && source.vtxo.offboarded_in.is_none()
				&& source.vtxo.spent_in_round.is_none(), "internal source is no longer unresolved");
			ensure!(tx.query_one("SELECT onchain_spent_height IS NULL AND onchain_spent_txid IS NULL AS unspent \
				FROM vtxo WHERE vtxo_id=$1", &[&id.to_string()]).await?.get::<_, bool>("unspent"),
				"internal source already has a chain spend");
			let funding_tx = tx.get_virtual_transaction_by_txid(signed.chain_anchor().txid).await?
				.context("internal source funding transaction missing")?;
			let funding_tx = funding_tx.signed_tx().context("internal source funding body missing")?;
			let final_step = signed.transactions().last().context("incoming source has no checkpoint transition")?;
			ensure!(final_step.tx.input.len() == 1, "incoming source transition is not single-input");
			let checkpoint_id = VtxoId::from(final_step.tx.input[0].previous_output);
			ensure!(checkpoint_ids.insert(checkpoint_id), "incoming sources do not have separate isolating checkpoints");
			let checkpoint = tx.get_server_vtxos_by_id(&[checkpoint_id]).await?.into_iter().next()
				.context("incoming source's actual checkpoint missing")?;
			ensure!(!checkpoint.decided_for_fallback && tx.query_one(
				"SELECT onchain_spent_height IS NULL AND onchain_spent_txid IS NULL AS unspent FROM vtxo WHERE vtxo_id=$1",
				&[&checkpoint_id.to_string()]).await?.get::<_, bool>("unspent"),
				"incoming recovery checkpoint is already consumed or reserved");
			let checkpoint = checkpoint.vtxo;
			check_isolating_checkpoint(signed, &checkpoint, funding_tx)?;
			result.originals.push((id, signed.serialize()));
			result.checkpoints.push(checkpoint);
		}
		Ok(result)
	}

	pub(super) async fn revalidate(&self, tx: &database::Tx<'_>, policy: &VtxoPolicy) -> anyhow::Result<()> {
		let current = Self::load(tx, self.subscription, policy).await?;
		ensure!(self.hash == current.hash && self.attempt == current.attempt
			&& self.originals == current.originals
			&& self.checkpoints.iter().map(ProtocolEncoding::serialize).collect::<Vec<_>>()
				== current.checkpoints.iter().map(ProtocolEncoding::serialize).collect::<Vec<_>>(),
			"original receive funding budget changed before grant commit");
		Ok(())
	}

	pub(super) fn maximum_grants(&self, policy: &VtxoPolicy, server: PublicKey) -> anyhow::Result<usize> {
		// All newly issued grants share this actual requested policy. The pool
		// may contain any accepted expiry/delta: use their protocol maxima,
		// real taproot tree and largest clause, never a guessed byte constant.
		let sample = Vtxo::<Bare>::new(OutPoint::null(), policy.clone(), P2TR_DUST,
			MAX_BLOCK_HEIGHT, server, MAX_BLOCK_DELTA, OutPoint::null());
		let witness = largest_user_witness(&sample);
		let base = self.checkpoints.iter().map(largest_server_witness).collect::<Vec<_>>();
		let customers = self.originals.len().max(1);
		let mut maximum = 0;
		for count in 1..=MAX_JOINT_GRANTS as usize {
			if parent_vbytes(&base, &vec![witness; count], customers)? <= 10_000 { maximum = count; } else { break; }
		}
		Ok(maximum)
	}

	pub(super) fn check_grants(&self, grants: impl Iterator<Item = Vtxo<Full>>) -> anyhow::Result<()> {
		let grants = grants.collect::<Vec<_>>();
		ensure!(!grants.is_empty() && grants.len() <= MAX_JOINT_GRANTS as usize, "receive grant set exceeds refund bound");
		let mut ids = BTreeSet::new();
		for grant in &grants {
			ensure!(ids.insert(grant.id()) && grant.amount() >= grant.output_script_pubkey().minimal_non_dust(),
				"new receive would create a duplicate or dust timeout dependency");
			let policy = grant.policy().as_server_htlc_recv().context("unsupported receive timeout dependency")?;
			ensure!(policy.payment_hash == self.hash, "receive dependency has a different payment hash");
			let last = grant.transactions().last().context("receive dependency has no signed-path template")?;
			ensure!(last.tx.input.len() == 1 && last.tx.output.iter().enumerate().all(|(index, output)|
				index == last.output_idx || *output == bitcoin_ext::fee::fee_anchor()),
				"new receive dependency is not isolated from sibling principal");
		}
		ensure!(parent_vbytes(&self.checkpoints.iter().map(largest_server_witness).collect::<Vec<_>>(),
			&grants.iter().map(largest_user_witness).collect::<Vec<_>>(), self.originals.len().max(1))? <= 10_000,
			"complete receive refund would exceed version-3 parent size");
		Ok(())
	}
}

fn check_isolating_checkpoint(source: &Vtxo<Full>, checkpoint: &ServerVtxo<Full>, funding: &Transaction) -> anyhow::Result<()> {
	ensure!(matches!(checkpoint.policy(), ServerVtxoPolicy::Checkpoint(_)), "incoming source lacks a server-expiry checkpoint");
	// Registration retains the full SIGNED final source and all signed
	// virtual transactions. Its intermediate VTXO row may still contain the
	// original unsigned body; validate its geometry against the real signed
	// source prefix rather than demanding an invented intermediate signature.
	source.validate(funding).context("invalid signed internal source path")?;
	checkpoint.validate_unsigned(funding).context("invalid incoming checkpoint path")?;
	let actual = source.transactions().find(|step| OutPoint::new(step.tx.compute_txid(), step.output_idx as u32)==checkpoint.point())
		.context("checkpoint is not on the actual signed original source path")?;
	ensure!(actual.tx.output.get(actual.output_idx)==Some(&checkpoint.txout()),
		"checkpoint policy differs from its actual signed source output");
	let last = source.transactions().last().context("incoming source has no final transition")?;
	ensure!(last.tx.input.len() == 1 && last.tx.input[0].previous_output == checkpoint.point()
		&& source.chain_anchor() == checkpoint.chain_anchor() && source.server_pubkey() == checkpoint.server_pubkey()
		&& source.amount() == checkpoint.amount() && source.exit_delta() == checkpoint.exit_delta()
		&& source.expiry_height() == checkpoint.expiry_height()
		&& checkpoint.amount() >= checkpoint.output_script_pubkey().minimal_non_dust()
		&& last.tx.output.get(last.output_idx) == Some(&source.txout())
		&& last.tx.output.iter().enumerate().all(|(index, output)| index == last.output_idx || *output == bitcoin_ext::fee::fee_anchor()),
		"incoming checkpoint does not isolate its complete original source");
	Ok(())
}

fn largest_server_witness(vtxo: &ServerVtxo<Full>) -> usize {
	vtxo.policy().clauses(vtxo.exit_delta(), vtxo.expiry_height(), vtxo.server_pubkey())
		.iter().map(|clause| clause.witness_size(vtxo)).max().unwrap_or(66).max(66)
}

fn largest_user_witness<G>(vtxo: &Vtxo<G>) -> usize {
	vtxo.policy().clauses(vtxo.exit_delta(), vtxo.expiry_height(), vtxo.server_pubkey())
		.iter().map(|clause| clause.witness_size(vtxo)).max().unwrap_or(66).max(66)
}

/// Same consensus serialization/largest-policy-witness approach as ReadyOutput.
/// Count every possible customer output (even if scripts later merge), plus
/// the operator drain and fee anchor; no wallet miner-fee cap is introduced.
fn parent_vbytes(incoming: &[usize], grants: &[usize], customers: usize) -> anyhow::Result<u64> {
	let script = ScriptBuf::new_p2wsh(&bitcoin::WScriptHash::all_zeros());
	let output_count = customers.checked_add(1).context("receive refund output budget overflow")?;
	let mut outputs = vec![TxOut { value: Amount::ZERO, script_pubkey: script }; output_count];
	outputs.push(bitcoin_ext::fee::fee_anchor());
	let tx = Transaction { version: bitcoin::transaction::Version::non_standard(3), lock_time: bitcoin::absolute::LockTime::ZERO,
		input: incoming.iter().chain(grants).map(|_| TxIn { previous_output: OutPoint::null(), script_sig: ScriptBuf::new(),
			sequence: Sequence::ZERO, witness: Witness::new() }).collect(), output: outputs };
	let mut weight = tx.weight().to_wu().checked_add(2).context("receive refund weight overflow")?;
	for witness in incoming.iter().chain(grants) {
		weight = weight.checked_add(u64::try_from(*witness)?).context("receive refund witness overflow")?;
	}
	Ok(weight.div_ceil(4))
}

/// Simulate the existing ascending-input package allocation. The actual
/// unsigned builder is checked again before cosigning, including policies.
pub(super) fn cover_fits(inputs: &[(VtxoId, BlockHeight, Amount)], amount: Amount, maximum: usize) -> bool {
	if inputs.is_empty() || inputs.len() > maximum || amount == Amount::ZERO { return false; }
	let mut values = inputs.iter().map(|v| v.2).collect::<Vec<_>>();
	let Some(total) = values.iter().try_fold(Amount::ZERO, |total, value| total.checked_add(*value)) else { return false; };
	let Some(change) = total.checked_sub(amount) else { return false; };
	// A tiny operator change also triggers dust isolation: it takes value
	// from the recipient into a shared, unrelayable fanout. Prefer another
	// standard cover before constructing/signing that new obligation.
	if change != Amount::ZERO && change < P2TR_DUST { return false; }
	values.sort_unstable();
	let mut remaining = amount;
	for value in values {
		let piece = value.min(remaining);
		if piece != Amount::ZERO && piece < P2TR_DUST { return false; }
		remaining = remaining.checked_sub(piece).unwrap_or(Amount::ZERO);
	}
	remaining == Amount::ZERO
}

#[cfg(test)]
mod tests {
	use bitcoin::secp256k1::{Keypair, SecretKey};
	use ark::arkoor::{ArkoorDestination, package::ArkoorPackageBuilder};
	use ark::lightning::Preimage;
	use ark::test_util::dummy::{DummyTestVtxoSpec, DUMMY_SERVER_KEY, DUMMY_USER_KEY};
	use bitcoin_ext::BlockDelta;

	use super::*;

	fn board(amount: u64, seed: u8) -> (Transaction, Vtxo<Full>, Keypair) {
		let user = Keypair::from_secret_key(&crate::SECP, &SecretKey::from_slice(&[seed; 32]).unwrap());
		let (funding, input) = DummyTestVtxoSpec {
			amount: Amount::from_sat(amount), fee: Amount::ZERO, user_keypair: user,
			..DummyTestVtxoSpec::default()
		}.build();
		(funding, input, user)
	}

	fn external(policy: &VtxoPolicy) -> ReceiveGrantBudget {
		ReceiveGrantBudget { subscription: 1, hash: policy.as_server_htlc_recv().unwrap().payment_hash,
			attempt: None, originals: Vec::new(), checkpoints: Vec::new() }
	}

	fn receive() -> VtxoPolicy {
		VtxoPolicy::new_server_htlc_recv(DUMMY_USER_KEY.public_key(), Preimage::random().compute_payment_hash(),
			BlockHeight::new(10_100), BlockDelta::new(12))
	}

	#[test]
	fn receive_admission_accepts_actual_two_grant_package_before_cosign() {
		let policy = receive();
		let (_, first, _) = board(50_000, 11);
		let (_, second, _) = board(50_000, 12);
		let builder = ArkoorPackageBuilder::new_with_checkpoints([first, second], vec![
			ArkoorDestination { policy: policy.clone(), total_amount: Amount::from_sat(90_000) },
			ArkoorDestination { policy: VtxoPolicy::new_pubkey(DUMMY_USER_KEY.public_key()), total_amount: Amount::from_sat(10_000) },
		]).unwrap();
		let grants = builder.build_unsigned_vtxos().filter(|v| *v.policy() == policy).collect::<Vec<_>>();
		assert_eq!(grants.len(), 2);
		assert_eq!(grants.iter().map(Vtxo::amount).sum::<Amount>(), Amount::from_sat(90_000));
		external(&policy).check_grants(grants.into_iter()).unwrap();
	}

	#[test]
	fn receive_admission_refuses_real_90001_dust_tail_but_accepts_large_cover() {
		let policy = receive();
		let inputs = (21..24).map(|seed| board(45_000, seed).1).collect::<Vec<_>>();
		let builder = ArkoorPackageBuilder::new_with_checkpoints(inputs, vec![
			ArkoorDestination { policy: policy.clone(), total_amount: Amount::from_sat(90_001) },
			ArkoorDestination { policy: VtxoPolicy::new_pubkey(DUMMY_USER_KEY.public_key()), total_amount: Amount::from_sat(44_999) },
		]).unwrap();
		let grants = builder.build_unsigned_vtxos().filter(|v| *v.policy() == policy).collect::<Vec<_>>();
		assert!(grants.iter().any(|v| v.amount() == Amount::from_sat(1)));
		assert!(external(&policy).check_grants(grants.into_iter()).is_err());
		let (_, input, _) = board(100_000, 24);
		let safe = ArkoorPackageBuilder::new_with_checkpoints([input], vec![
			ArkoorDestination { policy: policy.clone(), total_amount: Amount::from_sat(90_001) },
			ArkoorDestination { policy: VtxoPolicy::new_pubkey(DUMMY_USER_KEY.public_key()), total_amount: Amount::from_sat(9_999) },
		]).unwrap();
		external(&policy).check_grants(safe.build_unsigned_vtxos().filter(|v| *v.policy() == policy)).unwrap();
	}

	#[test]
	fn receive_admission_refuses_dust_operator_change_before_cosign() {
		let policy = receive();
		let (_, input, _) = board(90_000, 25);
		let change_policy = VtxoPolicy::new_pubkey(DUMMY_USER_KEY.public_key());
		let builder = ArkoorPackageBuilder::new_with_checkpoints([input], vec![
			ArkoorDestination { policy: policy.clone(), total_amount: Amount::from_sat(89_999) },
			ArkoorDestination { policy: change_policy.clone(), total_amount: Amount::from_sat(1) },
		]).unwrap();
		let actual = builder.build_unsigned_vtxos().collect::<Vec<_>>();
		assert!(actual.iter().any(|v| *v.policy()==change_policy && v.amount()==Amount::from_sat(1)));
		assert!(actual.iter().any(|v| *v.policy()==policy && v.amount()==Amount::from_sat(329)),
			"operator dust change moves recipient principal into the real dust fanout");
		assert!(external(&policy).check_grants(actual.into_iter().filter(|v| *v.policy()==policy)).is_err());
	}

	#[test]
	fn receive_admission_counts_original_checkpoints_outputs_and_policy_witnesses() {
		let user = DUMMY_USER_KEY.public_key();
		let cp = Vtxo::<Bare, ServerVtxoPolicy>::new(OutPoint::null(), ServerVtxoPolicy::new_checkpoint(user),
			P2TR_DUST, MAX_BLOCK_HEIGHT, DUMMY_SERVER_KEY.public_key(), MAX_BLOCK_DELTA, OutPoint::null());
		let policy = VtxoPolicy::new_server_htlc_recv(user, Preimage::random().compute_payment_hash(), MAX_BLOCK_HEIGHT, MAX_BLOCK_DELTA);
		let recv = Vtxo::<Bare>::new(OutPoint::null(), policy, P2TR_DUST, MAX_BLOCK_HEIGHT,
			DUMMY_SERVER_KEY.public_key(), MAX_BLOCK_DELTA, OutPoint::null());
		let cp_witness = cp.policy().clauses(cp.exit_delta(), cp.expiry_height(), cp.server_pubkey())
			.iter().map(|clause| clause.witness_size(&cp)).max().unwrap();
		let receive_witness = largest_user_witness(&recv);
		assert_eq!(cp_witness, 142);
		assert_eq!(receive_witness, 232);
		assert!(parent_vbytes(&vec![cp_witness; 64], &vec![receive_witness; 23], 64).unwrap() <= 10_000);
		assert!(parent_vbytes(&vec![cp_witness; 64], &vec![receive_witness; 24], 64).unwrap() > 10_000);
		assert!(parent_vbytes(&[cp_witness], &vec![receive_witness; 64], 1).unwrap() <= 10_000);
	}

	#[test]
	fn receive_admission_requires_actual_signed_isolating_incoming_checkpoint() {
		let (funding, input, key) = board(100_000, 31);
		let hash = Preimage::random().compute_payment_hash();
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
			VtxoPolicy::new_server_htlc_send(key.public_key(), hash, BlockHeight::new(10_200))).unwrap();
		// The normal RPC stores this unsigned intermediate row; the client's
		// later registration provides the actual signed FINAL source body.
		let internals = builder.build_unsigned_internal_vtxos();
		let unsigned = builder.build_unsigned_vtxos().next().unwrap();
		let builder = builder.cosign_both(&[key], &DUMMY_SERVER_KEY).unwrap();
		let source = builder.build_signed_vtxos().remove(0);
		let checkpoint = &internals.iter().find(|(v, _)| matches!(v.policy(), ServerVtxoPolicy::Checkpoint(_))).unwrap().0;
		check_isolating_checkpoint(&source, checkpoint, &funding).unwrap();
		assert!(check_isolating_checkpoint(&unsigned, checkpoint, &funding).is_err());
		let (_, other, _) = board(100_000, 32);
		assert!(check_isolating_checkpoint(&source, &ServerVtxo::from(other), &funding).is_err());
	}
}
