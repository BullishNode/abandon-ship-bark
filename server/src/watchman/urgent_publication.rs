//! Exact urgent candidate/package binding for the existing owning process.
//! This validates package bytes and arithmetic; it creates NO fee authority.
//! Callers must retain the actual source/Watchman locks, same-ledger full-ceiling
//! attempt, canonical source eligibility, and durable nursery handoff. Until all
//! those hooks exist, the DIRECT claim/progression entry points remain held.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, Transaction, TxOut, Weight};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::{TransactionExt, P2TR_DUST};

use ark::{ProtocolEncoding, ServerVtxo};
use super::carrier::CarrierLimits;
use super::ordinary_fee::OrdinaryFeePolicy;
use super::ordinary_fee_auth::FeeServiceIdentity;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_INPUTS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UrgentPurpose { Claim, Progression }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceInput { point: OutPoint, output: TxOut, anchor: OutPoint, native_metadata_sha256: sha256::Hash }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrgentCandidate {
	version: u8,
	purpose: UrgentPurpose,
	server_pubkey: bitcoin::secp256k1::PublicKey,
	genesis: bitcoin::BlockHash,
	policy_sha256: sha256::Hash,
	opening_sha256: sha256::Hash,
	// Identity of a future SAME-ledger pre-sign reservation, not an allowance.
	attempt_index: u64,
	ceiling_sat: u64,
	max_fee_rate: FeeRate,
	max_weight_wu: u64,
	requested_fee_rate: FeeRate,
	operator_script: ScriptBuf,
	sources: Vec<SourceInput>,
	// Only the selected OWN confirmed/unreserved fee inputs, in canonical order.
	fee_inputs: BTreeMap<OutPoint, TxOut>,
	// Claim template or [EXACT already-signed native parent, child template].
	// Only the transaction that this attempt newly signs has empty witnesses.
	templates: Vec<Vec<u8>>,
	prevouts: Vec<Vec<TxOut>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundUrgentPublication {
	version: u8,
	candidate: UrgentCandidate,
	signed: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrgentReservationEntry {
	version: u8,
	pub attempt_index: u64,
	purpose: UrgentPurpose,
	server_pubkey: bitcoin::secp256k1::PublicKey,
	genesis: bitcoin::BlockHash,
	policy_sha256: sha256::Hash,
	pub opening_sha256: sha256::Hash,
	pub candidate_sha256: sha256::Hash,
	pub publication_txid: bitcoin::Txid,
	pub ceiling_sat: u64,
}
impl UrgentReservationEntry {
	pub(crate) fn filename(&self) -> String { format!("urgent-{}.candidate.json", self.attempt_index) }
	pub(crate) fn parse(raw: &str, policy: OrdinaryFeePolicy) -> anyhow::Result<Self> {
		let value: Self = serde_json::from_str(raw)?;
		ensure!(serde_json::to_string(&value)? == raw, "urgent pre-sign entry is not canonical");
		value.check(policy)?; Ok(value)
	}
	fn check(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<()> {
		ensure!(self.version == 1 && self.attempt_index > 0
			&& self.candidate_sha256 != sha256::Hash::all_zeros() && self.opening_sha256 != sha256::Hash::all_zeros()
			&& self.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?), "urgent pre-sign identity differs");
		policy.validate_ceiling(Amount::from_sat(self.ceiling_sat))
	}
	pub(crate) fn require_identity(&self, identity: &FeeServiceIdentity) -> anyhow::Result<()> {
		ensure!(self.server_pubkey == identity.server_pubkey && self.genesis == identity.genesis
			&& self.policy_sha256 == identity.policy_sha256 && self.opening_sha256 == identity.opening_sha256,
			"urgent pre-sign intent differs from actual activated owner");
		Ok(())
	}

	pub(crate) fn line(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<String> {
		self.check(policy)?; Ok(format!("u1\t{}\n", serde_json::to_string(self)?))
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrgentSignedEntry {
	version: u8,
	pub attempt_index: u64,
	pub candidate_sha256: sha256::Hash,
	pub record_sha256: sha256::Hash,
	pub publication_txid: bitcoin::Txid,
	publication_wtxid: bitcoin::Wtxid,
	actual_fee_sat: u64,
	signed_weight_wu: u64,
}
impl UrgentSignedEntry {
	pub(crate) fn parse(raw: &str, original: &UrgentReservationEntry) -> anyhow::Result<Self> {
		let value: Self = serde_json::from_str(raw)?;
		ensure!(serde_json::to_string(&value)? == raw, "urgent signed binding is not canonical");
		value.check(original)?; Ok(value)
	}
	fn check(&self, original: &UrgentReservationEntry) -> anyhow::Result<()> {
		ensure!(self.version == 1 && self.attempt_index == original.attempt_index
			&& self.candidate_sha256 == original.candidate_sha256 && self.publication_txid == original.publication_txid
			&& self.record_sha256 != sha256::Hash::all_zeros() && self.actual_fee_sat > 0
			&& self.actual_fee_sat <= original.ceiling_sat && self.signed_weight_wu > 0 && self.signed_weight_wu <= 400_000,
			"urgent signed binding differs from original full-ceiling attempt");
		Ok(())
	}
	pub(crate) fn line(&self, original: &UrgentReservationEntry) -> anyhow::Result<String> {
		self.check(original)?; Ok(format!("u2\t{}\n", serde_json::to_string(self)?))
	}
}

pub(crate) struct UrgentPackagePrice {
	pub fee: Amount,
	pub signed_weight: Weight,
	pub whole_fee_inputs: BTreeSet<OutPoint>,
}

fn money_sum(mut values: impl Iterator<Item = Amount>) -> anyhow::Result<Amount> {
	let total = values.try_fold(Amount::ZERO, |sum, value| {
		ensure!(value <= Amount::MAX_MONEY, "urgent value exceeds money range");
		sum.checked_add(value).context("urgent value overflow")
	})?;
	ensure!(total <= Amount::MAX_MONEY, "urgent sum exceeds money range");
	Ok(total)
}
fn decoded(raw: &[u8]) -> anyhow::Result<Transaction> {
	ensure!(raw.len() <= 400_000, "urgent transaction exceeds byte bound");
	let tx: Transaction = deserialize(raw)?;
	ensure!(serialize(&tx) == raw && !tx.is_coinbase() && !tx.input.is_empty()
		&& tx.input.len() <= MAX_INPUTS, "urgent transaction is not canonical bounded spending bytes");
	Ok(tx)
}
fn unsigned(mut tx: Transaction) -> Transaction {
	for input in &mut tx.input { input.witness.clear(); }
	tx
}

impl UrgentCandidate {
	/// Capture actual native source outputs plus selected owning fee outputs.
	/// The caller must independently authenticate these against current DB/Core
	/// while holding the existing locks. This constructor is not that proof.
	pub(crate) fn from_native_sources(purpose: UrgentPurpose, identity: &FeeServiceIdentity,
		attempt_index: u64, sources: &[ServerVtxo], transactions: &[Transaction],
		fee_inputs: BTreeMap<OutPoint, TxOut>, operator_script: ScriptBuf,
		requested_fee_rate: FeeRate, limits: CarrierLimits, policy: OrdinaryFeePolicy,
	) -> anyhow::Result<Self> {
		let sources = sources.iter().map(|source| SourceInput { point: source.point(),
			output: source.txout(), anchor: source.chain_anchor(),
			native_metadata_sha256: sha256::Hash::hash(&ProtocolEncoding::serialize(source)) }).collect::<Vec<_>>();
		Self::from_inputs(purpose, identity, attempt_index, sources, transactions,
			fee_inputs, operator_script, requested_fee_rate, limits, policy)
	}
	pub(crate) fn require_current_sources(&self, sources: &[ServerVtxo], selected_fee_inputs: &BTreeMap<OutPoint, TxOut>,
		operator_script: &ScriptBuf,
	) -> anyhow::Result<()> {
		let mut current = sources.iter().map(|source| SourceInput { point: source.point(), output: source.txout(),
			anchor: source.chain_anchor(), native_metadata_sha256: sha256::Hash::hash(&ProtocolEncoding::serialize(source)) }).collect::<Vec<_>>();
		current.sort_by_key(|source| source.point);
		ensure!(current == self.sources && selected_fee_inputs == &self.fee_inputs && operator_script == &self.operator_script,
			"urgent current native source metadata, whole fee inputs or operator destination differs");
		Ok(())
	}
	fn from_inputs(purpose: UrgentPurpose, identity: &FeeServiceIdentity, attempt_index: u64,
		mut sources: Vec<SourceInput>, transactions: &[Transaction], fee_inputs: BTreeMap<OutPoint, TxOut>,
		operator_script: ScriptBuf, requested_fee_rate: FeeRate, limits: CarrierLimits, policy: OrdinaryFeePolicy,
	) -> anyhow::Result<Self> {
		ensure!(transactions.len() == if purpose == UrgentPurpose::Claim { 1 } else { 2 }, "urgent purpose/package arity differs");
		sources.sort_by_key(|source| source.point);
		let templates = transactions.iter().enumerate().map(|(index, tx)| {
			serialize(&if index + 1 == transactions.len() { unsigned(tx.clone()) } else { tx.clone() })
		}).collect::<Vec<_>>();
		let mut value = Self { version: 2, purpose, server_pubkey: identity.server_pubkey, genesis: identity.genesis,
			policy_sha256: identity.policy_sha256, opening_sha256: identity.opening_sha256,
			attempt_index, ceiling_sat: limits.max_miner_fee.to_sat(), max_fee_rate: limits.max_fee_rate, max_weight_wu: limits.max_weight.to_wu(),
			requested_fee_rate, operator_script, sources, fee_inputs, templates, prevouts: Vec::new() };
		value.prevouts = value.expected_prevouts()?;
		value.check(identity, limits, policy)?;
		Ok(value)
	}
	fn expected_prevouts(&self) -> anyhow::Result<Vec<Vec<TxOut>>> {
		ensure!(!self.sources.is_empty() && self.sources.len() <= MAX_INPUTS && self.fee_inputs.len() <= MAX_INPUTS,
			"urgent complete source/input bound differs");
		let originals = self.sources.iter().map(|source| (source.point, source.output.clone())).collect::<BTreeMap<_, _>>();
		ensure!(originals.len() == self.sources.len() && originals.keys().all(|point| !self.fee_inputs.contains_key(point)),
			"urgent source/fee inputs duplicate or overlap");
		let parent = decoded(self.templates.first().context("urgent first transaction missing")?)?;
		let parent_points = parent.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>();
		ensure!(parent_points.len() == parent.input.len() && parent_points == originals.keys().copied().collect(),
			"urgent first transaction omits or adds a source input");
		let mut result = vec![parent.input.iter().map(|input| originals[&input.previous_output].clone()).collect()];
		match self.purpose {
			UrgentPurpose::Claim => {
				ensure!(self.templates.len() == 1 && self.fee_inputs.is_empty() && parent.output.len() == 1
					&& parent.output[0].script_pubkey == self.operator_script && parent.output[0].value >= P2TR_DUST,
					"urgent claim destination, dust or fee-input roles differ");
			},
			UrgentPurpose::Progression => {
				ensure!(self.templates.len() == 2 && !self.fee_inputs.is_empty(), "urgent progression requires exact parent and owning fee inputs");
				let (anchor, anchor_output) = parent.fee_anchor().context("urgent native parent has no fee anchor")?;
				let child = decoded(&self.templates[1])?;
				let child_points = child.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>();
				let mut expected = self.fee_inputs.keys().copied().collect::<BTreeSet<_>>();
				ensure!(expected.insert(anchor) && !originals.contains_key(&anchor), "urgent anchor aliases owning input");
				ensure!(child_points.len() == child.input.len() && child_points == expected,
					"urgent child omits, duplicates or adds a whole fee input/anchor");
				result.push(child.input.iter().map(|input| if input.previous_output == anchor { anchor_output.clone() }
					else { self.fee_inputs[&input.previous_output].clone() }).collect());
			},
		}
		Ok(result)
	}
	fn check(&self, identity: &FeeServiceIdentity, limits: CarrierLimits, policy: OrdinaryFeePolicy) -> anyhow::Result<()> {
		ensure!(self.version == 2 && self.attempt_index > 0 && self.server_pubkey == identity.server_pubkey && self.genesis == identity.genesis
			&& self.policy_sha256 == identity.policy_sha256 && self.opening_sha256 == identity.opening_sha256
			&& self.opening_sha256 != sha256::Hash::all_zeros()
			&& self.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?), "urgent actual policy/opening/chain/attempt differs");
		if policy.regtest_qualification.is_some() {
			ensure!(identity.genesis == bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash(),
				"urgent qualification authority requires actual REGTEST identity");
		} else { policy.validate()?; }
		policy.validate_ceiling(Amount::from_sat(self.ceiling_sat))?;
		ensure!(self.ceiling_sat == limits.max_miner_fee.to_sat() && self.max_fee_rate == limits.max_fee_rate
			&& limits.max_miner_fee <= Amount::MAX_MONEY && self.requested_fee_rate > FeeRate::ZERO && self.requested_fee_rate <= self.max_fee_rate
			&& self.max_weight_wu == limits.max_weight.to_wu() && limits.max_weight.to_wu() > 0 && limits.max_weight.to_wu() <= 400_000,
			"urgent configured full ceiling/rate/weight differs");
		ensure!(serde_json::to_vec(self)?.len() <= MAX_BYTES && self.expected_prevouts()? == self.prevouts,
			"urgent complete original prevouts or byte bound differs");
		let last = decoded(self.templates.last().context("urgent template missing")?)?;
		ensure!(last.input.iter().all(|input| input.witness.is_empty()), "new urgent signature was exposed before original candidate binding");
		Ok(())
	}
	pub(crate) fn identity(&self) -> FeeServiceIdentity {
		FeeServiceIdentity { server_pubkey: self.server_pubkey, genesis: self.genesis,
			policy_sha256: self.policy_sha256, opening_sha256: self.opening_sha256 }
	}
	pub(crate) fn limits(&self) -> CarrierLimits {
		CarrierLimits { max_fee_rate: self.max_fee_rate, max_miner_fee: Amount::from_sat(self.ceiling_sat), max_weight: Weight::from_wu(self.max_weight_wu) }
	}

	pub(crate) fn reservation_entry(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<UrgentReservationEntry> {
		let identity = FeeServiceIdentity { server_pubkey: self.server_pubkey, genesis: self.genesis,
			policy_sha256: self.policy_sha256, opening_sha256: self.opening_sha256 };
		let limits = CarrierLimits { max_fee_rate: self.max_fee_rate, max_miner_fee: Amount::from_sat(self.ceiling_sat), max_weight: Weight::from_wu(self.max_weight_wu) };
		self.check(&identity, limits, policy)?;
		Ok(UrgentReservationEntry { version: 1, attempt_index: self.attempt_index, purpose: self.purpose,
			server_pubkey: self.server_pubkey, genesis: self.genesis, policy_sha256: self.policy_sha256,
			opening_sha256: self.opening_sha256, candidate_sha256: self.id()?,
			publication_txid: decoded(self.templates.last().context("urgent template absent")?)?.compute_txid(), ceiling_sat: self.ceiling_sat })
	}
	pub(crate) fn decode_reserved(raw: &[u8], entry: &UrgentReservationEntry, policy: OrdinaryFeePolicy) -> anyhow::Result<Self> {
		ensure!(raw.len() <= MAX_BYTES && sha256::Hash::hash(raw) == entry.candidate_sha256, "urgent original candidate digest/bound differs");
		let value: Self = serde_json::from_slice(raw)?;
		ensure!(serde_json::to_vec(&value)? == raw && value.reservation_entry(policy)? == *entry,
			"urgent original candidate differs from retained pre-sign debit");
		Ok(value)
	}

	pub(crate) fn purpose_name(&self) -> &'static str {
		match self.purpose { UrgentPurpose::Claim => "claim", UrgentPurpose::Progression => "progression" }
	}
	/// Retain every EXTERNAL whole input. The parent-created P2A anchor is an
	/// internal package edge, never an independent wallet coin or fee source.
	pub(crate) fn external_inputs(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<BTreeSet<OutPoint>> {
		self.reservation_entry(policy)?;
		Ok(self.sources.iter().map(|source| source.point).chain(self.fee_inputs.keys().copied()).collect())
	}
	pub(crate) fn id(&self) -> anyhow::Result<sha256::Hash> { Ok(sha256::Hash::hash(&serde_json::to_vec(self)?)) }
	pub(crate) fn bind_signed(self, signed: &[Transaction], identity: &FeeServiceIdentity,
		limits: CarrierLimits, policy: OrdinaryFeePolicy,
	) -> anyhow::Result<BoundUrgentPublication> {
		let value = BoundUrgentPublication { version: 2, candidate: self, signed: signed.iter().map(serialize).collect() };
		value.validate(identity, limits, policy)?;
		Ok(value)
	}
}

impl BoundUrgentPublication {
	pub(crate) fn attempt_index(&self) -> u64 { self.candidate.attempt_index }
	pub(crate) fn decode(raw: &[u8], original_record_sha256: sha256::Hash, identity: &FeeServiceIdentity, limits: CarrierLimits,
		policy: OrdinaryFeePolicy,
	) -> anyhow::Result<Self> {
		ensure!(raw.len() <= MAX_BYTES && original_record_sha256 != sha256::Hash::all_zeros()
			&& sha256::Hash::hash(raw) == original_record_sha256, "urgent signed record differs from original durable binding or byte bound");
		let value: Self = serde_json::from_slice(raw)?;
		ensure!(serde_json::to_vec(&value)? == raw, "urgent signed record is not canonical");
		value.validate(identity, limits, policy)?;
		Ok(value)
	}
	pub(crate) fn digest(&self) -> anyhow::Result<sha256::Hash> { Ok(sha256::Hash::hash(&serde_json::to_vec(self)?)) }
	pub(crate) fn validate(&self, identity: &FeeServiceIdentity, limits: CarrierLimits,
		policy: OrdinaryFeePolicy,
	) -> anyhow::Result<UrgentPackagePrice> {
		ensure!(self.version == 2 && serde_json::to_vec(self)?.len() <= MAX_BYTES
			&& self.signed.len() == self.candidate.templates.len(), "urgent signed package record bound differs");
		self.candidate.check(identity, limits, policy)?;
		let mut fee = Amount::ZERO; let mut weight = Weight::ZERO; let mut ids = BTreeSet::new();
		for (index, (raw, prevouts)) in self.signed.iter().zip(&self.candidate.prevouts).enumerate() {
			let tx = decoded(raw)?;
			let original = if index + 1 == self.signed.len() { unsigned(tx.clone()) } else { tx.clone() };
			ensure!(serialize(&original) == self.candidate.templates[index] && ids.insert(tx.compute_txid()),
				"urgent signed body or existing parent witness differs from original candidate");
			ensure!(tx.input.len() == prevouts.len(), "urgent signed input coverage differs");
			if index + 1 == self.signed.len() {
				let anchor = if self.candidate.purpose == UrgentPurpose::Progression {
					Some(decoded(&self.signed[0])?.fee_anchor().context("urgent parent anchor absent")?.0)
				} else { None };
				ensure!(tx.input.iter().all(|input| input.script_sig.is_empty()
					&& (Some(input.previous_output) == anchor || !input.witness.is_empty())),
					"urgent newly signed input witness missing or scriptSig differs");
			}
			let input = money_sum(prevouts.iter().map(|output| output.value))?;
			let output = money_sum(tx.output.iter().map(|output| output.value))?;
			fee = fee.checked_add(input.checked_sub(output).context("urgent signed outputs exceed actual inputs")?).context("urgent package fee overflow")?;
			weight = weight.checked_add(tx.weight()).context("urgent package signed weight overflow")?;
		}
		ensure!(fee > Amount::ZERO && fee.to_sat() <= self.candidate.ceiling_sat && weight <= limits.max_weight
			&& self.candidate.max_fee_rate.fee_wu(weight).is_some_and(|maximum| fee <= maximum)
			&& self.candidate.requested_fee_rate.fee_wu(weight).is_some_and(|minimum| fee >= minimum),
			"urgent actual signed package fee/rate/weight exceeds original full-ceiling authority");
		Ok(UrgentPackagePrice { fee, signed_weight: weight, whole_fee_inputs: self.candidate.fee_inputs.keys().copied().collect() })
	}
	pub(crate) fn signed_entry(&self, identity: &FeeServiceIdentity, limits: CarrierLimits,
		policy: OrdinaryFeePolicy,
	) -> anyhow::Result<(UrgentReservationEntry, UrgentSignedEntry)> {
		let price = self.validate(identity, limits, policy)?;
		let original = self.candidate.reservation_entry(policy)?;
		let publication = decoded(self.signed.last().context("urgent signed publication absent")?)?;
		let entry = UrgentSignedEntry { version: 1, attempt_index: original.attempt_index,
			candidate_sha256: original.candidate_sha256, record_sha256: self.digest()?, publication_txid: publication.compute_txid(),
			publication_wtxid: publication.compute_wtxid(), actual_fee_sat: price.fee.to_sat(), signed_weight_wu: price.signed_weight.to_wu() };
		entry.check(&original)?;
		Ok((original, entry))
	}

	pub(crate) fn require_exact_bytes(&self, transactions: &[Transaction]) -> anyhow::Result<()> {
		ensure!(self.signed == transactions.iter().map(serialize).collect::<Vec<_>>(), "urgent replay/relay differs from original full signed bytes");
		Ok(())
	}
}

#[cfg(test)]
pub(crate) fn synthetic_urgent_vector(progression: bool, limits: CarrierLimits)
	-> (UrgentCandidate, Vec<Transaction>, FeeServiceIdentity, OrdinaryFeePolicy)
{
	tests::vector(progression, limits)
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::{absolute, transaction, Sequence, TxIn, Txid, Witness};
	use bitcoin::secp256k1::Keypair;
	use super::super::ordinary_fee::{OrdinaryFeeBudget, BUDGET_FILE};

	pub(super) fn vector(progression: bool, limits: CarrierLimits) -> (UrgentCandidate, Vec<Transaction>, FeeServiceIdentity, OrdinaryFeePolicy) {
		let key = Keypair::new(&crate::SECP, &mut bitcoin::secp256k1::rand::thread_rng());
		let operator = ScriptBuf::new_p2tr(&crate::SECP, key.x_only_public_key().0, None);
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		let identity = FeeServiceIdentity { server_pubkey: key.public_key(),
			genesis: bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash(),
			policy_sha256: sha256::Hash::hash(&serde_json::to_vec(&policy).unwrap()),
			opening_sha256: sha256::Hash::hash(b"SYNTHETIC original opening; no native authority") };
		let point = OutPoint::new(Txid::from_byte_array([7; 32]), 0);
		let source = SourceInput { point, anchor: point, output: TxOut { value: Amount::from_sat(10_000), script_pubkey: operator.clone() },
			native_metadata_sha256: sha256::Hash::hash(b"synthetic native input metadata; not ownership evidence") };
		let mut parent = Transaction { version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
			input: vec![TxIn { previous_output: point, script_sig: ScriptBuf::new(), sequence: Sequence::MAX,
				witness: Witness::from_slice(&[vec![0; 64]]) }],
			output: vec![TxOut { value: Amount::from_sat(9_000), script_pubkey: operator.clone() }] };
		let (txs, inputs) = if progression {
			parent.output.push(bitcoin_ext::fee::fee_anchor());
			let anchor = parent.fee_anchor().unwrap().0;
			let fee_point = OutPoint::new(Txid::from_byte_array([8; 32]), 1);
			let child = Transaction { version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
				input: vec![TxIn { previous_output: anchor, script_sig: ScriptBuf::new(), sequence: Sequence::MAX, witness: Witness::new() },
					TxIn { previous_output: fee_point, script_sig: ScriptBuf::new(), sequence: Sequence::MAX,
						witness: Witness::from_slice(&[vec![0; 64]]) }],
				output: vec![TxOut { value: Amount::from_sat(19_000), script_pubkey: operator.clone() }] };
			(vec![parent, child], BTreeMap::from([(fee_point, TxOut { value: Amount::from_sat(20_000), script_pubkey: operator.clone() })]))
		} else { (vec![parent], BTreeMap::new()) };
		let candidate = UrgentCandidate::from_inputs(if progression { UrgentPurpose::Progression } else { UrgentPurpose::Claim },
			&identity, 1, vec![source], &txs, inputs, operator, FeeRate::from_sat_per_vb_u32(1), limits, policy).unwrap();
		(candidate, txs, identity, policy)
	}
	fn limits() -> CarrierLimits {
		CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb_u32(100), max_miner_fee: Amount::from_sat(20_000), max_weight: Weight::from_wu(400_000) }
	}
	#[test]
	fn urgent_candidate_binds_body_complete_package_and_original_existing_parent_witness() {
		let limits = limits(); let (candidate, txs, identity, policy) = vector(true, limits);
		let publication = candidate.clone().bind_signed(&txs, &identity, limits, policy).unwrap();
		let price = publication.validate(&identity, limits, policy).unwrap();
		assert_eq!(price.fee.to_sat(), 2_000); assert_eq!(price.whole_fee_inputs.len(), 1);
		assert_eq!(price.signed_weight, txs[0].weight().checked_add(txs[1].weight()).unwrap());
		let mut changed = txs.clone(); changed[1].output[0].value = Amount::from_sat(18_999);
		assert!(candidate.clone().bind_signed(&changed, &identity, limits, policy).is_err());
		changed = txs.clone(); changed[0].input[0].witness = Witness::from_slice(&[vec![1; 64]]);
		assert!(candidate.bind_signed(&changed, &identity, limits, policy).is_err());
		assert!(publication.require_exact_bytes(&changed).is_err());
		assert!(publication.require_exact_bytes(&txs[..1]).is_err());
	}
	#[test]
	fn urgent_package_refuses_missing_whole_input_anchor_source_or_new_signature() {
		let limits = limits(); let (candidate, txs, identity, policy) = vector(true, limits);
		let mut missing = candidate.clone(); missing.fee_inputs.clear(); assert!(missing.check(&identity, limits, policy).is_err());
		missing = candidate.clone(); missing.sources.clear(); assert!(missing.check(&identity, limits, policy).is_err());
		let mut changed = txs.clone(); changed[1].input.remove(0);
		assert!(candidate.clone().bind_signed(&changed, &identity, limits, policy).is_err());
		changed = txs; changed[1].input[1].witness.clear();
		assert!(candidate.bind_signed(&changed, &identity, limits, policy).is_err());
	}
	#[test]
	fn urgent_signed_record_refuses_foreign_original_digest_and_changed_witness() {
		let limits = limits(); let (candidate, txs, identity, policy) = vector(false, limits);
		let publication = candidate.bind_signed(&txs, &identity, limits, policy).unwrap();
		let raw = serde_json::to_vec(&publication).unwrap(); let original = publication.digest().unwrap();
		assert_eq!(BoundUrgentPublication::decode(&raw, original, &identity, limits, policy).unwrap(), publication);
		assert!(BoundUrgentPublication::decode(&raw, sha256::Hash::hash(b"foreign durable witness"), &identity, limits, policy).is_err());
		let mut changed = publication.clone(); let mut body = txs; body[0].input[0].witness = Witness::from_slice(&[vec![2; 64]]);
		changed.signed = body.iter().map(serialize).collect();
		assert!(BoundUrgentPublication::decode(&serde_json::to_vec(&changed).unwrap(), original, &identity, limits, policy).is_err());
		assert!(publication.require_exact_bytes(&body).is_err());
	}
	#[test]
	fn urgent_actual_package_fee_weight_and_rate_do_not_use_fee_wallet_subsidy_as_fee() {
		let tight = CarrierLimits { max_miner_fee: Amount::from_sat(1_999), ..limits() };
		let (candidate, txs, identity, policy) = vector(true, tight);
		assert!(candidate.bind_signed(&txs, &identity, tight, policy).is_err());
		let tight = CarrierLimits { max_weight: Weight::from_wu(100), ..limits() };
		let (candidate, txs, identity, policy) = vector(false, tight);
		assert!(candidate.bind_signed(&txs, &identity, tight, policy).is_err());
		let tight = CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb_u32(1), ..limits() };
		let (candidate, txs, identity, policy) = vector(false, tight);
		assert!(candidate.bind_signed(&txs, &identity, tight, policy).is_err());
	}
	fn owner_directory(name: &str) -> std::path::PathBuf {
		let path = std::env::temp_dir().join(format!("c-urgent-ledger-{name}-{}-{}", std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&path).unwrap(); path
	}
	fn cleanup_owner(path: &std::path::Path) {
		let name = path.file_name().unwrap().to_str().unwrap();
		let witness = path.with_file_name(format!("{name}.ordinary-native-publications"));
		if witness.exists() { std::fs::remove_dir_all(witness).unwrap(); }
		std::fs::remove_dir_all(path).unwrap();
	}
	#[test]
	fn urgent_original_full_ceiling_shares_58k_with_carriers_and_survives_restart_without_resign_credit() {
		let (candidate, _, _, policy) = vector(false, limits()); let path = owner_directory("restart");
		policy.initialize(&path).unwrap();
		{
			let owner = OrdinaryFeeBudget::load(&path, policy).unwrap();
			owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
			owner.reserve_ceiling(Amount::from_sat(18_000)).unwrap();
			assert_eq!(owner.next_urgent_attempt_index().unwrap(), 1);
			owner.reserve_urgent_candidate(&candidate).unwrap();
			owner.require_retained_fees(Amount::from_sat(18_000)).unwrap();
			let original = std::fs::read(path.join(super::super::ordinary_fee::BUDGET_FILE)).unwrap();
			assert!(owner.reserve_urgent_candidate(&candidate).is_err());
			assert!(owner.reserve_ceiling(Amount::from_sat(1)).is_err());
			assert_eq!(std::fs::read(path.join(super::super::ordinary_fee::BUDGET_FILE)).unwrap(), original);
			assert_eq!(owner.reserved_urgent_candidates().unwrap(), vec![candidate.clone()]);
			assert!(owner.require_no_unreconciled_urgent().is_err());
		}
		let owner = OrdinaryFeeBudget::load(&path, policy).unwrap();
		owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
		assert_eq!(owner.reserved_urgent_candidates().unwrap(), vec![candidate.clone()]);
		assert!(owner.reserve_urgent_candidate(&candidate).is_err());
		let (mut second, _, _, _) = vector(true, limits()); second.attempt_index = 2;
		owner.reserve_urgent_candidate(&second).unwrap();
		let before = std::fs::read(path.join(super::super::ordinary_fee::BUDGET_FILE)).unwrap();
		let (mut third, _, _, _) = vector(false, limits()); third.attempt_index = 3;
		assert!(owner.reserve_urgent_candidate(&third).is_err(), "18k+20k+20k consumes original58k; no new urgent allowance");
		assert_eq!(std::fs::read(path.join(super::super::ordinary_fee::BUDGET_FILE)).unwrap(), before);
		drop(owner); cleanup_owner(&path);
	}
	#[test]
	fn urgent_same_original_candidate_concurrency_has_one_full_ceiling_and_no_second_signature_authority() {
		let (candidate, _, _, policy) = vector(false, limits()); let path = owner_directory("concurrent"); policy.initialize(&path).unwrap();
		let owner = std::sync::Arc::new(OrdinaryFeeBudget::load(&path, policy).unwrap());
		owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
		let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
		let threads = (0..2).map(|_| { let owner = owner.clone(); let barrier = barrier.clone(); let candidate = candidate.clone();
			std::thread::spawn(move || { barrier.wait(); owner.reserve_urgent_candidate(&candidate).is_ok() }) }).collect::<Vec<_>>();
		barrier.wait(); assert_eq!(threads.into_iter().map(|thread| thread.join().unwrap()).filter(|passed| *passed).count(), 1);
		assert_eq!(owner.reserved_urgent_candidates().unwrap().len(), 1);
		drop(owner); cleanup_owner(&path);
	}
	#[test]
	fn urgent_partial_or_replaced_candidate_witness_and_torn_history_hold_before_activation() {
		let (candidate, _, _, policy) = vector(false, limits()); let path = owner_directory("witness"); policy.initialize(&path).unwrap();
		{ let owner = OrdinaryFeeBudget::load(&path, policy).unwrap(); owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap(); owner.reserve_urgent_candidate(&candidate).unwrap(); }
		let name = path.file_name().unwrap().to_str().unwrap();
		let file = path.with_file_name(format!("{name}.ordinary-native-publications")).join("urgent-1.candidate.json");
		let original = std::fs::read(&file).unwrap(); std::fs::write(&file, b"{}").unwrap();
		assert!(OrdinaryFeeBudget::load(&path, policy).is_err()); std::fs::write(&file, &original).unwrap();
		let history = std::fs::read(path.join(BUDGET_FILE)).unwrap(); std::fs::write(path.join(BUDGET_FILE), &history[..history.len()-1]).unwrap();
		assert!(OrdinaryFeeBudget::load(&path, policy).is_err()); std::fs::write(path.join(BUDGET_FILE), &history).unwrap();
		std::fs::remove_file(&file).unwrap(); assert!(OrdinaryFeeBudget::load(&path, policy).is_err());
		cleanup_owner(&path);
	}

	#[test]
	fn urgent_signed_binding_replay_and_restart_retain_one_full_charge_and_exact_original_bytes() {
		let limit = limits(); let (candidate, transactions, identity, policy) = vector(true, limit);
		let signed = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let path = owner_directory("signed-restart"); policy.initialize(&path).unwrap();
		{
			let owner = OrdinaryFeeBudget::load(&path, policy).unwrap();
			owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
			owner.reserve_ceiling(Amount::from_sat(18_000)).unwrap();
			owner.reserve_urgent_candidate(&candidate).unwrap();
			owner.bind_urgent_signed(&signed, limit).unwrap();
			let history = std::fs::read(path.join(BUDGET_FILE)).unwrap();
			owner.bind_urgent_signed(&signed, limit).unwrap();
			assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(), history);
			assert_eq!(std::str::from_utf8(&history).unwrap().lines().filter(|line| line.starts_with("u1\t")).count(), 1);
			assert_eq!(std::str::from_utf8(&history).unwrap().lines().filter(|line| line.starts_with("u2\t")).count(), 1);
			owner.require_retained_fees(Amount::from_sat(18_000)).unwrap();
			assert!(owner.require_no_unreconciled_urgent().is_err(), "signed bytes alone do not establish the DB/source/nursery join");
		}
		let owner = OrdinaryFeeBudget::load(&path, policy).unwrap();
		owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
		let before = std::fs::read(path.join(BUDGET_FILE)).unwrap();
		owner.bind_urgent_signed(&signed, limit).unwrap();
		assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(), before);
		let name = path.file_name().unwrap().to_str().unwrap();
		let file = path.with_file_name(format!("{name}.ordinary-native-publications")).join("urgent-1.signed.json");
		assert_eq!(std::fs::read(file).unwrap(), serde_json::to_vec(&signed).unwrap());
		let (mut second, _, _, _) = vector(false, limit); second.attempt_index = 2;
		owner.reserve_urgent_candidate(&second).unwrap();
		let exhausted = std::fs::read(path.join(BUDGET_FILE)).unwrap();
		let (mut third, _, _, _) = vector(true, limit); third.attempt_index = 3;
		assert!(owner.reserve_urgent_candidate(&third).is_err(), "18k + two full 20k attempts exhaust the original 58k without a second signed-binding debit");
		assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(), exhausted);
		drop(owner); cleanup_owner(&path);
	}
	#[test]
	fn urgent_signed_binding_competing_witnesses_keep_one_original_without_resign_or_extra_debit() {
		let limit = limits(); let (candidate, transactions, identity, policy) = vector(false, limit);
		let original = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let mut changed = transactions; changed[0].input[0].witness = Witness::from_slice(&[vec![3; 64]]);
		let alternative = candidate.clone().bind_signed(&changed, &identity, limit, policy).unwrap();
		assert_ne!(original.digest().unwrap(), alternative.digest().unwrap());
		let path = owner_directory("signed-concurrent"); policy.initialize(&path).unwrap();
		let owner = std::sync::Arc::new(OrdinaryFeeBudget::load(&path, policy).unwrap());
		owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap(); owner.reserve_urgent_candidate(&candidate).unwrap();
		let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
		let threads = [original.clone(), alternative.clone()].into_iter().map(|publication| {
			let owner = owner.clone(); let barrier = barrier.clone();
			std::thread::spawn(move || { barrier.wait(); owner.bind_urgent_signed(&publication, limit).is_ok() })
		}).collect::<Vec<_>>();
		barrier.wait(); assert_eq!(threads.into_iter().map(|thread| thread.join().unwrap()).filter(|passed| *passed).count(), 1);
		let retained = std::fs::read(path.join(BUDGET_FILE)).unwrap();
		let name = path.file_name().unwrap().to_str().unwrap();
		let file = path.with_file_name(format!("{name}.ordinary-native-publications")).join("urgent-1.signed.json");
		let bytes = std::fs::read(&file).unwrap();
		let winner = if bytes == serde_json::to_vec(&original).unwrap() { &original } else { &alternative };
		assert_eq!(bytes, serde_json::to_vec(winner).unwrap());
		owner.bind_urgent_signed(winner, limit).unwrap();
		let loser = if winner == &original { &alternative } else { &original };
		assert!(owner.bind_urgent_signed(loser, limit).is_err());
		assert_eq!(std::fs::read(&file).unwrap(), bytes); assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(), retained);
		let (mut unreserved, txs, other_identity, _) = vector(true, limit); unreserved.attempt_index = 2;
		let unreserved = unreserved.bind_signed(&txs, &other_identity, limit, policy).unwrap();
		assert!(owner.bind_urgent_signed(&unreserved, limit).is_err());
		assert_eq!(std::fs::read(path.join(BUDGET_FILE)).unwrap(), retained);
		drop(owner); OrdinaryFeeBudget::load(&path, policy).unwrap(); cleanup_owner(&path);
	}
	#[test]
	fn urgent_signed_binding_missing_replaced_unindexed_or_torn_original_holds_before_activation() {
		let limit = limits(); let (candidate, transactions, identity, policy) = vector(false, limit);
		let signed = candidate.clone().bind_signed(&transactions, &identity, limit, policy).unwrap();
		let path = owner_directory("signed-corrupt"); policy.initialize(&path).unwrap();
		{
			let owner = OrdinaryFeeBudget::load(&path, policy).unwrap(); owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
			owner.reserve_urgent_candidate(&candidate).unwrap(); owner.bind_urgent_signed(&signed, limit).unwrap();
		}
		let history_path = path.join(BUDGET_FILE); let history = std::fs::read(&history_path).unwrap();
		let text = std::str::from_utf8(&history).unwrap();
		let u2 = text.lines().find(|line| line.starts_with("u2\t")).unwrap();
		for invalid in [format!("{u2}\n{text}"), format!("{text}{u2}\n"), text[..text.len()-1].to_owned(),
			text.lines().filter(|line| !line.starts_with("u2\t")).map(|line| format!("{line}\n")).collect::<String>()] {
			std::fs::write(&history_path, invalid).unwrap();
			assert!(OrdinaryFeeBudget::load(&path, policy).is_err());
		}
		std::fs::write(&history_path, &history).unwrap();
		let name = path.file_name().unwrap().to_str().unwrap();
		let file = path.with_file_name(format!("{name}.ordinary-native-publications")).join("urgent-1.signed.json");
		let bytes = std::fs::read(&file).unwrap(); std::fs::write(&file, b"{}").unwrap();
		assert!(OrdinaryFeeBudget::load(&path, policy).is_err()); std::fs::write(&file, &bytes).unwrap();
		OrdinaryFeeBudget::load(&path, policy).unwrap();
		std::fs::remove_file(file).unwrap(); assert!(OrdinaryFeeBudget::load(&path, policy).is_err());
		assert_eq!(std::fs::read(history_path).unwrap(), history); cleanup_owner(&path);
	}

}
