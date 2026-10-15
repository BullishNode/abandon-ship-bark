//! A lifetime ordinary-fee allowance, independent of the carrier PostgreSQL journal.
//! Reservations deliberately charge the complete fee ceiling, even if construction
//! fails. Neither confirmation, a new group id nor a restart replenishes it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use bitcoin::{Amount, Network};
use bitcoin::hashes::{sha256, Hash};
use crate::bitcoind::VerifiedFeeChain;

use crate::fs_perms;

pub(crate) const POLICY_FILE: &str = "ordinary-fee-policy.json";
pub(crate) const BUDGET_FILE: &str = "ordinary-fee-budget";
const MAX_HISTORY_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const DECLARATION_FILE: &str = "regtest-qualification-declaration.json";
const MAX_DECLARATION_BYTES: u64 = 4096;

/// Canonical public declaration bytes are independently frozen before a run.
/// Exact compact serde JSON, including field order, is the canonical encoding.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegtestFeeDeclaration {
	version: u8,
	purpose: String,
	network: Network,
	genesis: bitcoin::BlockHash,
	max_attempts: u32,
	per_attempt_ceiling_sat: u64,
	ordinary_budget_sat: u64,
	urgent_reserve_sat: u64,
	workload_sha256: sha256::Hash,
}

/// Explicit virtual qualification authority. A digest identifies the immutable
/// common declaration; none of these values has a production/default allowance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegtestQualificationBudget {
	pub max_attempts: u32,
	pub per_attempt_ceiling_sat: u64,
	pub declaration_sha256: sha256::Hash,
}

impl RegtestQualificationBudget {
	fn total(self) -> anyhow::Result<u64> {
		ensure!(self.max_attempts > 0 && self.per_attempt_ceiling_sat > 0,
			"REGTEST qualification requires positive finite attempts and fee ceiling");
		ensure!(self.declaration_sha256.to_byte_array().iter().any(|b| *b != 0),
			"REGTEST qualification declaration digest is missing");
		let bytes = u64::from(self.max_attempts).checked_mul(self.per_attempt_ceiling_sat.to_string().len() as u64 + 1)
			.context("REGTEST qualification history bound overflow")?;
		ensure!(bytes <= MAX_HISTORY_BYTES, "REGTEST qualification exceeds existing ledger byte bound");
		u64::from(self.max_attempts).checked_mul(self.per_attempt_ceiling_sat)
			.context("REGTEST qualification total charge overflow")
	}
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryFeePolicy {
	pub urgent_reserve_sat: u64,
	pub ordinary_budget_sat: u64,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub regtest_qualification: Option<RegtestQualificationBudget>,
}

impl Default for OrdinaryFeePolicy {
	fn default() -> Self { Self { urgent_reserve_sat: 40_000, ordinary_budget_sat: 60_000, regtest_qualification: None } }
}

impl OrdinaryFeePolicy {
	pub(crate) fn validate(self) -> anyhow::Result<()> {
		ensure!(self.regtest_qualification.is_none(), "qualification fee authority requires verified REGTEST chain");
		ensure!(self.urgent_reserve_sat >= 40_000 && self.urgent_reserve_sat <= Amount::MAX_MONEY.to_sat(),
			"ordinary direct fees require at least 40000 sats protected urgent reserve");
		ensure!(self.ordinary_budget_sat > 0 && self.ordinary_budget_sat <= 60_000,
			"ordinary direct lifetime budget must be positive and at most 60000 sats");
		Ok(())
	}

	pub(crate) fn validate_config(self, network: Network) -> anyhow::Result<()> {
		let Some(declaration) = self.regtest_qualification else { return self.validate(); };
		ensure!(network == Network::Regtest, "qualification fee policy is REGTEST only");
		ensure!(self.urgent_reserve_sat >= 40_000 && self.urgent_reserve_sat <= Amount::MAX_MONEY.to_sat(),
			"ordinary direct fees require at least 40000 sats protected urgent reserve");
		ensure!(self.ordinary_budget_sat == declaration.total()?,
			"qualification lifetime budget must equal finite attempts times configured ceiling");
		Ok(())
	}

	fn validate_verified(self, chain: &VerifiedFeeChain) -> anyhow::Result<()> {
		if self.regtest_qualification.is_some() {
			ensure!(chain.matches(Network::Regtest), "qualification fee authority requires actual REGTEST genesis");
			self.validate_config(Network::Regtest)
		} else { self.validate() }
	}

	pub(crate) fn validate_ceiling(self, ceiling: Amount) -> anyhow::Result<()> {
		ensure!(ceiling > Amount::ZERO && ceiling.to_sat() <= self.ordinary_budget_sat,
			"direct carrier fee ceiling exceeds lifetime ordinary allowance");
		if let Some(declaration) = self.regtest_qualification {
			ensure!(ceiling.to_sat() == declaration.per_attempt_ceiling_sat,
				"qualification carrier fee ceiling differs from immutable declaration");
		}
		Ok(())
	}

	pub(crate) fn declaration_bytes(self, directory: &Path, chain: &VerifiedFeeChain) -> anyhow::Result<Option<Vec<u8>>> {
		self.validate_verified(chain)?;
		let Some(expected) = self.regtest_qualification else { return Ok(None); };
		let file = File::open(directory.join(DECLARATION_FILE)).context("canonical REGTEST declaration missing")?;
		ensure!(file.metadata()?.len() <= MAX_DECLARATION_BYTES, "REGTEST declaration exceeds byte bound");
		let mut bytes = Vec::new();
		file.take(MAX_DECLARATION_BYTES + 1).read_to_end(&mut bytes)?;
		ensure!(bytes.len() as u64 <= MAX_DECLARATION_BYTES, "REGTEST declaration exceeds byte bound");
		ensure!(sha256::Hash::hash(&bytes) == expected.declaration_sha256, "REGTEST declaration digest mismatch");
		let declaration: RegtestFeeDeclaration = serde_json::from_slice(&bytes)?;
		ensure!(serde_json::to_vec(&declaration)? == bytes, "REGTEST declaration is not canonical JSON");
		ensure!(declaration.version == 1 && declaration.purpose == "common-regtest-scale-qualification",
			"unsupported REGTEST declaration purpose or version");
		ensure!(declaration.network == Network::Regtest && chain.matches(declaration.network)
			&& declaration.genesis == bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash(),
			"REGTEST declaration differs from actual verified chain");
		ensure!(declaration.max_attempts == expected.max_attempts
			&& declaration.per_attempt_ceiling_sat == expected.per_attempt_ceiling_sat
			&& declaration.ordinary_budget_sat == self.ordinary_budget_sat
			&& declaration.urgent_reserve_sat == self.urgent_reserve_sat,
			"REGTEST declaration differs from exact policy counts or amounts");
		ensure!(declaration.workload_sha256.to_byte_array().iter().any(|b| *b != 0), "REGTEST workload digest missing");
		Ok(Some(bytes))
	}

	/// Absence of a qualification marker preserves the actual immutable live
	/// allowance, including the deployed 58000 native + 2000 external split.
	pub(crate) fn require_live_stored(directory: &Path) -> anyhow::Result<Self> {
		let path = directory.join(POLICY_FILE);
		ensure!(path.metadata()?.len() <= 1024, "ordinary fee policy exceeds size limit");
		let stored: Self = serde_json::from_slice(&std::fs::read(path)?)?;
		stored.validate()?;
		Ok(stored)
	}

	pub(crate) fn require_stored(self, directory: &Path, chain: &VerifiedFeeChain) -> anyhow::Result<()> {
		self.declaration_bytes(directory, chain)?;
		let path = directory.join(POLICY_FILE);
		ensure!(path.metadata()?.len() <= 1024, "ordinary fee policy exceeds size limit");
		let stored: OrdinaryFeePolicy = serde_json::from_slice(&std::fs::read(path)?)?;
		ensure!(stored == self, "ordinary fee policy differs from immutable independent history");
		Ok(())
	}

	/// Explicit fresh-history initialization only. Never called by a loader.
	pub(crate) fn initialize(self, directory: &Path) -> anyhow::Result<()> {
		self.validate()?;
		self.initialize_checked(directory)
	}

	pub(crate) fn initialize_verified(self, directory: &Path, chain: &VerifiedFeeChain) -> anyhow::Result<()> {
		self.declaration_bytes(directory, chain)?;
		self.initialize_checked(directory)
	}

	fn initialize_checked(self, directory: &Path) -> anyhow::Result<()> {
		fs_perms::create_new_owner_only(&directory.join(POLICY_FILE), &serde_json::to_vec(&self)?)?;
		fs_perms::create_new_owner_only(&directory.join(BUDGET_FILE), b"")?;
		File::open(directory.join(POLICY_FILE))?.sync_all()?;
		File::open(directory.join(BUDGET_FILE))?.sync_all()?;
		File::open(directory)?.sync_all()?;
		Ok(())
	}
}

struct Ledger {
	file: File,
	charged: u64,
	length: u64,
}

pub(crate) struct OrdinaryFeeBudget {
	policy: OrdinaryFeePolicy,
	ledger: Mutex<Ledger>,
}

impl OrdinaryFeeBudget {
	pub(crate) fn load(directory: &Path, policy: OrdinaryFeePolicy) -> anyhow::Result<Self> {
		policy.validate()?;
		Self::load_checked(directory, policy)
	}

	pub(crate) fn load_verified(directory: &Path, policy: OrdinaryFeePolicy, chain: &VerifiedFeeChain) -> anyhow::Result<Self> {
		policy.require_stored(directory, chain)?;
		if policy.regtest_qualification.is_none() { Self::load(directory, policy) }
		else { Self::load_checked(directory, policy) }
	}

	fn load_checked(directory: &Path, policy: OrdinaryFeePolicy) -> anyhow::Result<Self> {
		let path = directory.join(POLICY_FILE);
		ensure!(path.metadata()?.len() <= 1024, "ordinary fee policy exceeds size limit");
		let stored: OrdinaryFeePolicy = serde_json::from_slice(&std::fs::read(path)?)?;
		ensure!(stored == policy, "ordinary fee policy differs from immutable independent history");
		let mut file = OpenOptions::new().read(true).append(true).open(directory.join(BUDGET_FILE))
			.context("ordinary fee history missing; explicit empty-history bootstrap required")?;
		file.try_lock().context("ordinary fee history is already owned by another process")?;
		let length = file.metadata()?.len();
		ensure!(length <= MAX_HISTORY_BYTES, "ordinary fee history exceeds size limit");
		let mut history = String::new();
		file.seek(SeekFrom::Start(0))?;
		(&mut file).take(MAX_HISTORY_BYTES + 1).read_to_string(&mut history)?;
		let charged = parse_history(&history, policy)?;
		Ok(Self { policy, ledger: Mutex::new(Ledger { file, charged, length }) })
	}

	/// Every retained version is charged conservatively, including conflicts.
	pub(crate) fn require_retained_fees(&self, retained: Amount) -> anyhow::Result<()> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(retained.to_sat() <= ledger.charged,
			"ordinary fee reservations do not cover retained signed carrier history");
		Ok(())
	}

	pub(crate) fn urgent_reserve(&self) -> Amount { Amount::from_sat(self.policy.urgent_reserve_sat) }

	/// Synchronous durable reservation BEFORE any backing or fee-wallet signature.
	/// An uncertain fsync/append leaves length mismatched and prevents retry signing.
	pub(crate) fn reserve_ceiling(&self, ceiling: Amount) -> anyhow::Result<()> {
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "ordinary fee history changed; restart and reconcile");
		let charged = checked_charge(ledger.charged, ceiling.to_sat(), self.policy)?;
		let entry = format!("{}\n", ceiling.to_sat());
		ensure!(ledger.length + entry.len() as u64 <= MAX_HISTORY_BYTES, "ordinary fee history capacity exhausted");
		ledger.file.write_all(entry.as_bytes())?;
		ledger.file.sync_all()?;
		ledger.length += entry.len() as u64;
		ledger.charged = charged;
		Ok(())
	}
}

fn checked_charge(charged: u64, ceiling: u64, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	ensure!(ceiling > 0, "ordinary fee reservation ceiling is zero");
	if let Some(declaration) = policy.regtest_qualification {
		ensure!(ceiling == declaration.per_attempt_ceiling_sat && charged % ceiling == 0,
			"qualification reservation differs from declared attempt ceiling");
		ensure!(charged / ceiling < u64::from(declaration.max_attempts),
			"qualification attempt allowance exhausted");
	}
	let next = charged.checked_add(ceiling).context("ordinary fee budget overflow")?;
	ensure!(next <= policy.ordinary_budget_sat, "ordinary lifetime fee budget exhausted; urgent funds remain protected");
	Ok(next)
}

fn parse_history(history: &str, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	ensure!(history.is_empty() || history.ends_with('\n'), "ordinary fee history has a torn append");
	history.lines().try_fold(0, |charged, line| {
		let amount = line.parse::<u64>().context("invalid ordinary fee reservation")?;
		ensure!(line == amount.to_string(), "noncanonical ordinary fee reservation");
		checked_charge(charged, amount, policy)
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	// Synthetic finite values exercise admission/exhaustion, not authority for
	// the real common benchmark. No shared workload declaration exists here.
	fn synthetic_declaration(attempts: u32, ceiling: u64) -> Vec<u8> {
		serde_json::to_vec(&RegtestFeeDeclaration {
			version: 1, purpose: "common-regtest-scale-qualification".into(),
			network: Network::Regtest,
			genesis: bitcoin::blockdata::constants::genesis_block(Network::Regtest).block_hash(),
			max_attempts: attempts, per_attempt_ceiling_sat: ceiling,
			ordinary_budget_sat: u64::from(attempts).checked_mul(ceiling).unwrap(),
			urgent_reserve_sat: 40_000,
			workload_sha256: sha256::Hash::hash(b"SYNTHETIC UNIT WORKLOAD, NOT COMMON RUN AUTHORITY"),
		}).unwrap()
	}

	fn synthetic_qualification(attempts: u32, ceiling: u64) -> OrdinaryFeePolicy {
		OrdinaryFeePolicy {
			urgent_reserve_sat: 40_000,
			ordinary_budget_sat: u64::from(attempts).checked_mul(ceiling).unwrap(),
			regtest_qualification: Some(RegtestQualificationBudget {
				max_attempts: attempts, per_attempt_ceiling_sat: ceiling,
				declaration_sha256: sha256::Hash::hash(&synthetic_declaration(attempts, ceiling)),
			}),
		}
	}

	#[test]
	fn absent_qualification_preserves_real_58000_live_policy_without_writes() {
		let directory = std::env::temp_dir().join(format!("ark-live-58k-{}-{}",
			std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&directory).unwrap();
		let policy = OrdinaryFeePolicy { ordinary_budget_sat:58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let before_policy = std::fs::read(directory.join(POLICY_FILE)).unwrap();
		let before_ledger = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		assert_eq!(OrdinaryFeePolicy::require_live_stored(&directory).unwrap(),policy);
		assert_eq!(std::fs::read(directory.join(POLICY_FILE)).unwrap(),before_policy);
		assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(),before_ledger);
		std::fs::write(directory.join(POLICY_FILE),serde_json::to_vec(&OrdinaryFeePolicy {
			ordinary_budget_sat:60_001, ..Default::default()
		}).unwrap()).unwrap();
		assert!(OrdinaryFeePolicy::require_live_stored(&directory).is_err());
		std::fs::write(directory.join(POLICY_FILE),serde_json::to_vec(&synthetic_qualification(4,30_000)).unwrap()).unwrap();
		assert!(OrdinaryFeePolicy::require_live_stored(&directory).is_err(), "absence of opt-in cannot inherit qualification authority");
		std::fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn qualification_requires_actual_canonical_declaration_bytes_and_exact_fields() {
		let directory = std::env::temp_dir().join(format!("ark-regtest-declaration-{}-{}",
			std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&directory).unwrap();
		let policy = synthetic_qualification(4,30_000);
		let chain = crate::bitcoind::synthetic_fee_chain(Network::Regtest);
		let path = directory.join(DECLARATION_FILE);
		assert!(policy.declaration_bytes(&directory,&chain).is_err());
		let original = synthetic_declaration(4,30_000);
		std::fs::write(&path,&original).unwrap();
		assert_eq!(policy.declaration_bytes(&directory,&chain).unwrap(),Some(original.clone()));
		for field in ["max_attempts","per_attempt_ceiling_sat","ordinary_budget_sat","urgent_reserve_sat","network","genesis"] {
			let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
			value[field] = match field {
				"network" => serde_json::json!("signet"),
				"genesis" => serde_json::json!(bitcoin::blockdata::constants::genesis_block(Network::Signet).block_hash()),
				_ => serde_json::json!(value[field].as_u64().unwrap()+1),
			};
			let changed:RegtestFeeDeclaration=serde_json::from_value(value).unwrap();
			let bytes=serde_json::to_vec(&changed).unwrap();
			std::fs::write(&path,&bytes).unwrap();
			// Even a digest matching the supplied wrong document cannot bless its fields.
			let matching_hash=OrdinaryFeePolicy { regtest_qualification:Some(RegtestQualificationBudget {
				declaration_sha256:sha256::Hash::hash(&bytes), ..policy.regtest_qualification.unwrap()
			}), ..policy };
			assert!(matching_hash.declaration_bytes(&directory,&chain).is_err(),"wrong {field}");
		}
		let mut noncanonical=original.clone();noncanonical.push(b'\n');
		std::fs::write(&path,&noncanonical).unwrap();
		assert!(policy.declaration_bytes(&directory,&chain).is_err(),"wrong raw digest");
		let matching_hash=OrdinaryFeePolicy { regtest_qualification:Some(RegtestQualificationBudget {
			declaration_sha256:sha256::Hash::hash(&noncanonical), ..policy.regtest_qualification.unwrap()
		}), ..policy };
		assert!(matching_hash.declaration_bytes(&directory,&chain).is_err(),"noncanonical even with matching digest");
		std::fs::write(&path,vec![b' ';4097]).unwrap();
		assert!(policy.declaration_bytes(&directory,&chain).is_err(),"bounded complete document, no truncation");
		std::fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn live_fee_policy_keeps_original_serialization_and_rejects_larger_authority() {
		let policy = OrdinaryFeePolicy::default();
		assert_eq!(serde_json::to_value(policy).unwrap(),
			serde_json::json!({"urgent_reserve_sat":40000,"ordinary_budget_sat":60000}));
		assert_eq!(serde_json::from_str::<OrdinaryFeePolicy>(
			r#"{"urgent_reserve_sat":40000,"ordinary_budget_sat":60000}"#).unwrap(), policy);
		for network in [Network::Bitcoin, Network::Testnet, Network::Signet, Network::Regtest] {
			assert!(OrdinaryFeePolicy { ordinary_budget_sat: 60_001, ..policy }.validate_config(network).is_err());
		}
		let qualification = synthetic_qualification(4, 30_000);
		assert!(qualification.validate().is_err(), "ordinary live loader cannot accept a qualification marker");
		for network in [Network::Bitcoin, Network::Testnet, Network::Signet] {
			assert!(qualification.validate_config(network).is_err());
			assert!(qualification.validate_verified(&crate::bitcoind::synthetic_fee_chain(network)).is_err());
		}
		assert!(qualification.validate_config(Network::Regtest).is_ok());
	}

	#[test]
	fn qualification_declaration_requires_exact_finite_charge_ceiling_and_digest() {
		let policy = synthetic_qualification(4, 30_000);
		let declaration = policy.regtest_qualification.unwrap();
		assert_eq!(declaration.total().unwrap(), 120_000);
		policy.validate_ceiling(Amount::from_sat(30_000)).unwrap();
		assert!(policy.validate_ceiling(Amount::from_sat(29_999)).is_err());
		assert!(policy.validate_ceiling(Amount::from_sat(30_001)).is_err());
		assert!(OrdinaryFeePolicy { ordinary_budget_sat: 119_999, ..policy }.validate_config(Network::Regtest).is_err());
		assert!(RegtestQualificationBudget { max_attempts: 0, ..declaration }.total().is_err());
		assert!(RegtestQualificationBudget { per_attempt_ceiling_sat: 0, ..declaration }.total().is_err());
		assert!(RegtestQualificationBudget { declaration_sha256: sha256::Hash::from_byte_array([0;32]), ..declaration }.total().is_err());
		assert!(RegtestQualificationBudget { max_attempts: 2, per_attempt_ceiling_sat: u64::MAX, ..declaration }.total().is_err());
		assert!(RegtestQualificationBudget { max_attempts: u32::MAX, per_attempt_ceiling_sat: 1, ..declaration }.total().is_err());
		assert!(OrdinaryFeePolicy { urgent_reserve_sat: 39_999, ..policy }.validate_config(Network::Regtest).is_err());
	}

	#[test]
	fn finite_regtest_history_exhaustion_survives_restart_and_cannot_be_raised() {
		let directory = std::env::temp_dir().join(format!("ark-regtest-fees-{}-{}",
			std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&directory).unwrap();
		let policy = synthetic_qualification(4, 30_000);
		let regtest = crate::bitcoind::synthetic_fee_chain(Network::Regtest);
		let signet = crate::bitcoind::synthetic_fee_chain(Network::Signet);
		assert!(policy.initialize(&directory).is_err());
		assert!(policy.initialize_verified(&directory, &signet).is_err());
		assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0,
			"invalid network cannot create policy or ledger files");
		assert!(policy.initialize_verified(&directory, &regtest).is_err(), "bare marker is not a declaration");
		std::fs::write(directory.join(DECLARATION_FILE), synthetic_declaration(4,30_000)).unwrap();
		policy.initialize_verified(&directory, &regtest).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		assert!(OrdinaryFeeBudget::load_verified(&directory, policy, &signet).is_err());
		{
			let budget = OrdinaryFeeBudget::load_verified(&directory, policy, &regtest).unwrap();
			assert!(OrdinaryFeeBudget::load_verified(&directory, policy, &regtest).is_err());
			assert!(budget.reserve_ceiling(Amount::from_sat(29_999)).is_err());
			for _ in 0..3 { budget.reserve_ceiling(Amount::from_sat(30_000)).unwrap(); }
			budget.require_retained_fees(Amount::from_sat(90_000)).unwrap();
			assert!(budget.require_retained_fees(Amount::from_sat(90_001)).is_err());
		}
		{
			let budget = OrdinaryFeeBudget::load_verified(&directory, policy, &regtest).unwrap();
			budget.reserve_ceiling(Amount::from_sat(30_000)).unwrap();
			assert!(budget.reserve_ceiling(Amount::from_sat(30_000)).is_err());
		}
		assert_eq!(std::fs::read_to_string(directory.join(BUDGET_FILE)).unwrap(), "30000\n30000\n30000\n30000\n");
		assert!(OrdinaryFeeBudget::load_verified(&directory, synthetic_qualification(5,30_000), &regtest).is_err());
		let changed_digest = OrdinaryFeePolicy { regtest_qualification: Some(RegtestQualificationBudget {
			declaration_sha256: sha256::Hash::hash(b"different synthetic declaration"),
			..policy.regtest_qualification.unwrap()
		}), ..policy };
		assert!(OrdinaryFeeBudget::load_verified(&directory, changed_digest, &regtest).is_err());
		assert!(policy.initialize_verified(&directory, &regtest).is_err(), "existing ledger cannot bootstrap again");
		assert!(parse_history("30000\n30000\n30000\n30000\n30000\n", policy).is_err());
		assert!(parse_history("20000\n", policy).is_err());
		assert!(parse_history("30000\n300", policy).is_err());
		std::fs::remove_file(directory.join(BUDGET_FILE)).unwrap();
		assert!(OrdinaryFeeBudget::load_verified(&directory, policy, &regtest).is_err());
		std::fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn deliberately_small_regtest_budget_holds_next_attempt_without_reset() {
		let policy = synthetic_qualification(1, 30_000);
		policy.validate_verified(&crate::bitcoind::synthetic_fee_chain(Network::Regtest)).unwrap();
		assert_eq!(checked_charge(0,30_000,policy).unwrap(),30_000);
		assert!(checked_charge(30_000,30_000,policy).is_err());
		assert_eq!(parse_history("30000\n",policy).unwrap(),30_000);
		assert!(parse_history("30000\n30000\n",policy).is_err());
	}

	#[test]
	fn ordinary_fee_history_survives_restart_and_refuses_missing_or_changed_policy() {
		let directory = std::env::temp_dir().join(format!("ark-ordinary-fees-{}-{}",
			std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&directory).unwrap();
		let policy = OrdinaryFeePolicy::default();
		policy.initialize(&directory).unwrap();
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			assert!(OrdinaryFeeBudget::load(&directory, policy).is_err(), "second owner must not sign");
			for _ in 0..3 { budget.reserve_ceiling(Amount::from_sat(20_000)).unwrap(); }
			assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
			budget.require_retained_fees(Amount::from_sat(60_000)).unwrap();
			assert!(budget.require_retained_fees(Amount::from_sat(60_001)).is_err());
		}
		let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(20_000)).is_err(), "restart must not refill");
		drop(budget);
		assert!(OrdinaryFeeBudget::load(&directory, OrdinaryFeePolicy { ordinary_budget_sat: 59_999, ..policy }).is_err());
		std::fs::remove_file(directory.join(BUDGET_FILE)).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err(), "missing ledger must not initialize");
		assert!(policy.initialize(&directory).is_err(), "existing policy must not bootstrap again");
		std::fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn ordinary_fee_budget_is_cumulative_and_never_credits_confirmation() {
		let policy = OrdinaryFeePolicy::default();
		assert_eq!(parse_history("20000\n20000\n20000\n", policy).unwrap(), 60_000);
		assert!(parse_history("20000\n20000\n20000\n1\n", policy).is_err());
		assert!(parse_history("20000\n200", policy).is_err());
		assert!(parse_history("020000\n", policy).is_err());
		assert!(checked_charge(60_000, 1, policy).is_err());
	}
}
