//! Actual supported paid-send claim producer. This path performs NO relay and
//! remains unscheduled until complete startup/input restoration and owner replay
//! are admitted. No generic claim/receive/forfeit signing fallback is exposed.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use bitcoin::{Amount, FeeRate, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Weight, Witness};
use bitcoin_ext::BlockHeight;
use ark::{ServerVtxo, ServerVtxoPolicy, VtxoPolicy};
use ark::lightning::PaymentHash;
use ark::vtxo::policy::clause::VtxoClause;

use super::Watchman;
use super::carrier::CarrierLimits;
use super::ordinary_fee::OrdinaryFeeBudget;
use super::urgent_publication::{BoundUrgentPublication, UrgentCandidate, UrgentPurpose};

// Unknown/cancelled SQL acknowledgement after a possible independent debit or
// private signature cannot release this owner or turn a retry into a new signer.
struct PendingStage<'a> { budget: &'a OrdinaryFeeBudget, armed: AtomicBool, acknowledged: bool }
impl<'a> PendingStage<'a> {
	fn new(budget: &'a OrdinaryFeeBudget) -> Self { Self { budget, armed: AtomicBool::new(false), acknowledged: false } }
	fn arm(&self) { self.armed.store(true,Ordering::SeqCst); }
	fn acknowledge(&mut self) { self.acknowledged = true; }
}
impl Drop for PendingStage<'_> {
	fn drop(&mut self) {
		if self.armed.load(Ordering::SeqCst) && !self.acknowledged { let _ = self.budget.mark_uncertain(); }
	}
}

pub(super) fn paid_send_clause(source: &ServerVtxo) -> anyhow::Result<(PaymentHash,VtxoClause)> {
	Ok(match source.policy() {
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy)) => (policy.payment_hash,
			VtxoClause::HashDelaySign(policy.server_reveals_preimage_clause(source.server_pubkey(),source.exit_delta()))),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend_v0(policy)) => (policy.payment_hash,
			VtxoClause::HashDelaySign_v0(policy.server_reveals_preimage_clause(source.server_pubkey(),source.exit_delta()))),
		_ => bail!("only actual paid-send native claim policy is supported; no receive/conditional/forfeit fallback"),
	})
}
// No public constructor or Clone. The owning hook creates this once only AFTER
// the original u1+whole-input SQL COMMIT was actually acknowledged, and consumes
// it at the private signer. A decoded request/cached R/F object cannot mint it.
pub(super) struct AcknowledgedPaidClaim<'a> { unsigned: &'a Transaction }
impl AcknowledgedPaidClaim<'_> {
	pub(super) fn transaction(&self) -> &Transaction { self.unsigned }
}
struct UnsignedPaidClaim { transaction: Transaction, signed_weight: Weight }
fn unsigned_paid_claim(sources: &[ServerVtxo], operator: &ScriptBuf, height: BlockHeight,
	rate: FeeRate, limits: CarrierLimits,
) -> anyhow::Result<UnsignedPaidClaim> {
	ensure!(!sources.is_empty() && sources.len() <= 4096 && rate > FeeRate::ZERO && rate <= limits.max_fee_rate,
		"paid urgent claim source/rate bound differs");
	let mut inputs = Vec::with_capacity(sources.len()); let mut witnesses = 0usize; let mut total = Amount::ZERO;
	let mut points = std::collections::BTreeSet::new();
	for source in sources {
		let (_,clause) = paid_send_clause(source)?;
		ensure!(points.insert(source.point()), "paid urgent claim repeats a physical source");
		witnesses = witnesses.checked_add(clause.witness_size(source)).context("paid urgent witness size overflow")?;
		total = total.checked_add(source.amount()).context("paid urgent claim value overflow")?;
		ensure!(total <= Amount::MAX_MONEY, "paid urgent claim exceeds monetary range");
		inputs.push(TxIn { previous_output: source.point(), script_sig: ScriptBuf::new(),
			sequence: clause.sequence().unwrap_or(Sequence::ZERO), witness: Witness::new() });
	}
	let mut transaction = Transaction { version:bitcoin::transaction::Version::TWO,
		lock_time:height.to_locktime().context("paid urgent claim lower height cannot be encoded")?,input:inputs,
		output:vec![TxOut { script_pubkey:operator.clone(),value:Amount::ZERO }] };
	let signed_weight = super::claim_weight(&transaction,witnesses)?;
	ensure!(signed_weight <= limits.max_weight && limits.max_weight.to_wu() <= 400_000,
		"paid urgent complete signed shape exceeds actual configured bound");
	let fee = rate.fee_wu(signed_weight).context("paid urgent claim requested fee overflow")?;
	ensure!(fee > Amount::ZERO && fee <= limits.max_miner_fee
		&& limits.max_fee_rate.fee_wu(signed_weight).is_some_and(|maximum| fee <= maximum),
		"paid urgent claim fee exceeds original configured full ceiling");
	let value = total.checked_sub(fee).context("paid urgent claim fee exceeds original operator inputs")?;
	ensure!(value >= operator.minimal_non_dust() && value >= bitcoin_ext::P2TR_DUST,
		"paid urgent claim operator output is dust; no fee-input or customer subsidy");
	transaction.output[0].value = value;
	Ok(UnsignedPaidClaim { transaction,signed_weight })
}

impl Watchman {
	/// This private owner hook is NOT connected to the DIRECT scheduler yet.
	/// Only an actual acknowledged u1+SQL tuple may reach the private signer;
	/// u2+SQL+nursery retain the same body once. Nursery still holds all relay.
	pub(super) async fn retain_supported_paid_claim(&self, sources: &[ServerVtxo], rate: FeeRate)
		-> anyhow::Result<BoundUrgentPublication> {
		ensure!(self.direct_expiry.is_some(), "paid urgent owner requires actual DIRECT route");
		let budget = self.ordinary_fee_budget.as_ref().context("paid urgent actual fee owner missing")?;
		let identity = budget.actual_owner_identity()?;
		let limits = self.urgent_fee_limits.context("paid urgent actual configured limits missing")?;
		let direct = self.config.direct_expiry.as_ref().context("paid urgent actual DIRECT config missing")?;
		ensure!(limits.max_miner_fee == direct.max_miner_fee && budget.policy() == direct.ordinary_fee_policy,
			"paid urgent actual owning configuration differs from immutable policy");
		// SAME owning guard is retained through BOTH acknowledged commits. No
		// expected change, foreign wallet, or first-phase cached R/F token exists.
		let wallet = self.watchman_wallet.lock_owned().await;
		let mut reservation = PendingStage::new(budget);
		let (candidate,unsigned) = tokio::time::timeout(Duration::from_secs(30),self.db.write(async |t| {
			let current = super::urgent_source::require_current_sources(t,&self.bitcoind,&wallet,&self.db,&self.config,
				budget,&identity,sources,&BTreeMap::new(),UrgentPurpose::Claim,None).await?;
			let unsigned = unsigned_paid_claim(sources,&self.drain_spk,current.lower_height(),rate,limits)?;
			let candidate = UrgentCandidate::from_native_sources(UrgentPurpose::Claim,&identity,budget.next_urgent_attempt()?,
				sources,&[unsigned.transaction.clone()],BTreeMap::new(),self.drain_spk.clone(),rate,limits,budget.policy())?;
			reservation.arm(); // BEFORE possible fsync, no cancellation gap.
			budget.reserve_urgent_candidate(&candidate)?;
			t.stage_native_urgent_intent(&candidate,budget,&identity).await?;
			t.retained_native_urgent_inputs(budget,&identity).await?;
			Ok((candidate,unsigned))
		})).await.context("urgent pre-sign COMMIT deadline unknown; preserve original debit and inputs")??;
		reservation.acknowledge(); // ONLY after actual COMMIT acknowledgement.

		let deadline = Instant::now().checked_add(Duration::from_secs(30)).context("paid urgent signing deadline overflow")?;
		let mut signing = PendingStage::new(budget);
		let publication = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline),self.db.write(async |t| {
			// A new current-source cut, not the first transaction's R/F evidence.
			let current = super::urgent_source::require_current_sources(t,&self.bitcoind,&wallet,&self.db,&self.config,
				budget,&identity,sources,&BTreeMap::new(),UrgentPurpose::Claim,None).await?;
			candidate.require_current_sources(sources,&BTreeMap::new(),&self.drain_spk)?;
			t.require_native_urgent_unsigned(&candidate,budget,&identity).await?;
			ensure!(unsigned.transaction.lock_time <= current.lower_height().to_locktime().context("paid urgent current height invalid")?,
				"paid urgent original claim locktime is ahead of current lower owning checkpoint");
			signing.arm(); // A lost future after ANY signature must never re-sign.
			let once = AcknowledgedPaidClaim { unsigned: &unsigned.transaction };
			let signed = self.signer.sign_paid_send_claim_locked(t,sources,once,deadline).await?;
			ensure!(signed.weight() == unsigned.signed_weight, "paid urgent actual signature weight differs from exact pre-sign shape");
			let publication = candidate.clone().bind_signed(&[signed],&identity,limits,budget.policy())?;
			budget.bind_urgent_signed(&publication,limits)?; // Original private bytes first.
			ensure!(crate::bitcoind::tip(&self.bitcoind).await? == current.tip(),
				"paid urgent independent source crossed a tip while signing; retain original private package");
			t.publish_native_urgent_intent(&candidate,&publication,budget,&identity,current.deadline()).await?;
			t.retained_native_urgent_inputs(budget,&identity).await?;
			Ok(publication)
		})).await.context("urgent signed COMMIT deadline unknown; preserve exact original package and inputs")??;
		signing.acknowledge();
		Ok(publication)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::os::unix::fs::DirBuilderExt;
	use bitcoin::hashes::{sha256,Hash};
	use bitcoin::secp256k1::{Keypair,SecretKey};
	use ark::vtxo::policy::clause::TapScriptClause;
	use super::super::ordinary_fee::{OrdinaryFeePolicy,BUDGET_FILE};

	fn source(legacy: bool) -> (ServerVtxo,Keypair,[u8;32]) {
		let key=Keypair::from_secret_key(&crate::SECP,&SecretKey::from_slice(&[3;32]).unwrap());
		let user=Keypair::from_secret_key(&crate::SECP,&SecretKey::from_slice(&[4;32]).unwrap());
		let preimage=[7;32];let hash:PaymentHash=sha256::Hash::hash(&preimage).into();
		let policy=if legacy {VtxoPolicy::ServerHtlcSend_v0(ark::vtxo::ServerHtlcSend_v0_VtxoPolicy {
			user_pubkey:user.public_key(),payment_hash:hash,htlc_expiry:BlockHeight::new(200)})}
			else {VtxoPolicy::new_server_htlc_send(user.public_key(),hash,BlockHeight::new(200))};
		let point=bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([9;32]),0);
		(ark::ServerVtxo::new(point,policy.into(),Amount::from_sat(50_000),BlockHeight::new(300),
			key.public_key(),12u16.into(),point),key,preimage)
	}
	fn limits() -> CarrierLimits {CarrierLimits {max_fee_rate:FeeRate::from_sat_per_vb(50).unwrap(),
		max_miner_fee:Amount::from_sat(20_000),max_weight:Weight::from_wu(400_000)}}
	#[test]
	fn paid_claim_plan_matches_genuine_v0_v1_witness_weight_and_operator_conservation() {
		for legacy in [false,true] {
			let (source,key,preimage)=source(legacy);let operator=ScriptBuf::new_p2tr(&crate::SECP,key.x_only_public_key().0,None);
			let rate=FeeRate::from_sat_per_vb(2).unwrap();
			let plan=unsigned_paid_claim(&[source.clone()],&operator,BlockHeight::new(100),rate,limits()).unwrap();
			let (_,clause)=paid_send_clause(&source).unwrap();let mut signed=plan.transaction.clone();
			let prevouts=[source.txout()];let mut cache=bitcoin::sighash::SighashCache::new(&signed);
			let leaf=bitcoin::taproot::TapLeafHash::from_script(&clause.tapscript(),bitcoin::taproot::LeafVersion::TapScript);
			let message=cache.taproot_script_spend_signature_hash(0,&bitcoin::sighash::Prevouts::All(&prevouts),
				leaf,bitcoin::sighash::TapSighashType::Default).unwrap();
			let signature=crate::SECP.sign_schnorr(&message.into(),&key);
			crate::SECP.verify_schnorr(&signature,&message.into(),&key.x_only_public_key().0).unwrap();
			let control=clause.control_block(&source);
			signed.input[0].witness=match &clause {VtxoClause::HashDelaySign(c)=>c.witness(&(signature,preimage),&control),
				VtxoClause::HashDelaySign_v0(c)=>c.witness(&(signature,preimage),&control),_=>unreachable!()};
			assert_eq!(plan.signed_weight,signed.weight());
			assert_eq!(source.amount().checked_sub(signed.output[0].value),rate.fee_wu(signed.weight()));
			assert_eq!(signed.output.len(),1);assert_eq!(signed.output[0].script_pubkey,operator);
			assert_eq!(signed.input[0].previous_output,source.point());
		}
	}
	#[test]
	fn paid_claim_plan_refuses_other_ownership_duplicate_sources_and_over_ceiling_before_signing() {
		let (source,key,_)=source(false);let operator=ScriptBuf::new_p2tr(&crate::SECP,key.x_only_public_key().0,None);
		let rate=FeeRate::from_sat_per_vb(2).unwrap();
		assert!(unsigned_paid_claim(&[source.clone(),source.clone()],&operator,BlockHeight::new(100),rate,limits()).is_err());
		let foreign=ark::ServerVtxo::new(source.point(),VtxoPolicy::new_pubkey(key.public_key()).into(),source.amount(),
			source.expiry_height(),source.server_pubkey(),source.exit_delta(),source.chain_anchor());
		assert!(paid_send_clause(&foreign).is_err());
		assert!(unsigned_paid_claim(&[foreign],&operator,BlockHeight::new(100),rate,limits()).is_err());
		let tiny=CarrierLimits{max_miner_fee:Amount::from_sat(1),..limits()};
		assert!(unsigned_paid_claim(&[source.clone()],&operator,BlockHeight::new(100),rate,tiny).is_err());
		assert!(unsigned_paid_claim(&[source],&operator,BlockHeight::new(100),FeeRate::from_sat_per_vb(51).unwrap(),limits()).is_err());
	}
	fn budget() -> (std::path::PathBuf,OrdinaryFeeBudget) {
		let path=std::env::temp_dir().join(format!("c-paid-urgent-once-{}-{}",std::process::id(),
			bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
		let policy=OrdinaryFeePolicy{ordinary_budget_sat:58_000,..Default::default()};policy.initialize(&path).unwrap();
		let budget=OrdinaryFeeBudget::load(&path,policy).unwrap();(path,budget)
	}
	#[test]
	fn possible_independent_write_or_signature_without_ack_poison_owner_without_refund_or_rewrite() {
		let (path,budget)=budget();let before=std::fs::read(path.join(BUDGET_FILE)).unwrap();
		{let stage=PendingStage::new(&budget);stage.arm();}
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
		assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(),before);
		drop(budget);std::fs::remove_dir_all(path).unwrap();
	}
	#[test]
	fn unarmed_preflight_or_acknowledged_commit_does_not_mint_or_poison_existing_allowance() {
		let (path,budget)=budget();let before=std::fs::read(path.join(BUDGET_FILE)).unwrap();
		{let _stage=PendingStage::new(&budget);}
		{let mut stage=PendingStage::new(&budget);stage.arm();stage.acknowledge();}
		assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(),before);
		budget.reserve_ceiling(Amount::from_sat(1)).unwrap();
		assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(),b"1\n");
		drop(budget);std::fs::remove_dir_all(path).unwrap();
	}
}
