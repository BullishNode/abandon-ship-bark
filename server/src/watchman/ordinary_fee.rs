//! A lifetime ordinary-fee allowance, independent of the carrier PostgreSQL journal.
//! Reservations deliberately charge the complete fee ceiling, even if construction
//! fails. Neither confirmation, a new group id nor a restart replenishes it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use bitcoin::Amount;

use crate::fs_perms;

pub(crate) const POLICY_FILE: &str = "ordinary-fee-policy.json";
pub(crate) const BUDGET_FILE: &str = "ordinary-fee-budget";
const MAX_HISTORY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryFeePolicy {
	pub urgent_reserve_sat: u64,
	pub ordinary_budget_sat: u64,
}

impl Default for OrdinaryFeePolicy {
	fn default() -> Self { Self { urgent_reserve_sat: 40_000, ordinary_budget_sat: 60_000 } }
}

impl OrdinaryFeePolicy {
	pub(crate) fn validate(self) -> anyhow::Result<()> {
		ensure!(self.urgent_reserve_sat >= 40_000 && self.urgent_reserve_sat <= Amount::MAX_MONEY.to_sat(),
			"ordinary direct fees require at least 40000 sats protected urgent reserve");
		ensure!(self.ordinary_budget_sat > 0 && self.ordinary_budget_sat <= 60_000,
			"ordinary direct lifetime budget must be positive and at most 60000 sats");
		Ok(())
	}

	/// Explicit fresh-history initialization only. Never called by a loader.
	pub(crate) fn initialize(self, directory: &Path) -> anyhow::Result<()> {
		self.validate()?;
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
