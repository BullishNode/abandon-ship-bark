//! Lifetime ordinary-fee reservations, independent of PostgreSQL. Charge the
//! complete ceiling before signing; failures and confirmations never replenish it.
//! Joint rollback of independent history and database is outside this contract.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use bitcoin::{Amount, Network, Transaction};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;

use crate::fs_perms;

pub(crate) const POLICY_FILE: &str = "ordinary-fee-policy.json";
pub(crate) const OPENING_FILE: &str = "ordinary-fee-opening.json";
pub(crate) const BUDGET_FILE: &str = "ordinary-fee-budget";
const MAX_HISTORY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryFeePolicy {
	pub urgent_reserve_sat: u64,
	pub ordinary_budget_sat: u64,
	/// Fixed prospective partition for fees outside this signer. No refund.
	pub external_future_reserved_sat: u64,
	/// Required for adoption of existing history; None permits fresh creation only.
	pub opening_sha256: Option<sha256::Hash>,
}

impl Default for OrdinaryFeePolicy {
	fn default() -> Self {
		Self { urgent_reserve_sat: 15_000, ordinary_budget_sat: 60_000,
			external_future_reserved_sat: 20_000, opening_sha256: None }
	}
}

impl OrdinaryFeePolicy {
	pub(crate) fn validate(self) -> anyhow::Result<()> {
		ensure!(self.urgent_reserve_sat > 0 && self.urgent_reserve_sat <= Amount::MAX_MONEY.to_sat(),
			"ordinary direct fees require a positive monetary urgent reserve");
		ensure!(self.ordinary_budget_sat > 0 && self.ordinary_budget_sat <= Amount::MAX_MONEY.to_sat(),
			"ordinary lifetime budget must be positive and within monetary bounds");
		ensure!(self.external_future_reserved_sat <= self.ordinary_budget_sat,
			"external future partition exceeds ordinary lifetime allowance");
		ensure!(self.opening_sha256 != Some(sha256::Hash::all_zeros()), "missing adopted opening digest");
		Ok(())
	}

	/// Called only by fresh server creation after proving the direct journal empty.
	pub(crate) fn initialize_fresh(self, directory: &Path, network: Network, key: PublicKey) -> anyhow::Result<()> {
		self.validate()?;
		ensure!(self.opening_sha256.is_none(), "existing-history adoption cannot initialize fresh history");
		let opening = OrdinaryFeeOpening { version: 1, network, genesis: bitcoin::blockdata::constants::genesis_block(network).block_hash(), server_pubkey: key,
			history_prefix_bytes: 0, history_prefix_sha256: sha256::Hash::hash(b""),
			native_spent_sat: 0, external_spent_sat: 0, accounting_receipt_sha256: None };
		let bytes = serde_json::to_vec(&opening)?;
		let stored = StoredPolicy { policy: self, opening_sha256: sha256::Hash::hash(&bytes) };
		fs_perms::create_new_owner_only(&directory.join(OPENING_FILE), &bytes)?;
		fs_perms::create_new_owner_only(&directory.join(POLICY_FILE), &serde_json::to_vec(&stored)?)?;
		fs_perms::create_new_owner_only(&directory.join(BUDGET_FILE), b"")?;
		for name in [OPENING_FILE, POLICY_FILE, BUDGET_FILE] { File::open(directory.join(name))?.sync_all()?; }
		File::open(directory)?.sync_all()?;
		Ok(())
	}
}

/// Operator-installed, create-new adoption checkpoint. Historical files remain
/// unchanged. Digest provenance is independently verified by the stopped operator;
/// a digest alone is not a native verification of the external accounting journal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrdinaryFeeOpening {
	pub version: u32,
	pub network: Network,
	pub genesis: bitcoin::BlockHash,
	pub server_pubkey: PublicKey,
	pub history_prefix_bytes: u64,
	pub history_prefix_sha256: sha256::Hash,
	pub native_spent_sat: u64,
	pub external_spent_sat: u64,
	pub accounting_receipt_sha256: Option<sha256::Hash>,
}

impl OrdinaryFeeOpening {
	fn validate(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<()> {
		ensure!(self.version == 1 && self.history_prefix_bytes % 65 == 0, "invalid ordinary fee opening version or history prefix");
		match policy.opening_sha256 {
			Some(_) => {
				ensure!(self.history_prefix_bytes > 0, "adoption requires the complete original signed history prefix");
				ensure!(self.native_spent_sat > 0, "adoption requires the original signed native fee debit");
				ensure!(self.accounting_receipt_sha256.is_some_and(|h| h != sha256::Hash::all_zeros()),
					"adoption requires independently authenticated accounting receipt digest");
			},
			None => ensure!(self.native_spent_sat == 0 && self.external_spent_sat == 0 && self.history_prefix_bytes == 0 &&
				self.history_prefix_sha256 == sha256::Hash::hash(b"") && self.accounting_receipt_sha256.is_none(),
				"missing adoption digest cannot authorize existing fees or history"),
		}
		self.initial_charge(policy)?;
		Ok(())
	}

	pub(crate) fn verify_prefix(&self, network: Network, key: PublicKey, history: &[u8]) -> anyhow::Result<usize> {
		ensure!(self.network == network && self.genesis == bitcoin::blockdata::constants::genesis_block(network).block_hash() &&
			self.server_pubkey == key, "ordinary fee opening server or chain identity differs");
		let length = usize::try_from(self.history_prefix_bytes)?;
		ensure!(length <= history.len() && sha256::Hash::hash(&history[..length]) == self.history_prefix_sha256,
			"ordinary fee opening signed-history prefix differs");
		Ok(length)
	}

	fn initial_charge(&self, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
		self.native_spent_sat.checked_add(self.external_spent_sat)
			.and_then(|n| n.checked_add(policy.external_future_reserved_sat))
			.filter(|n| *n <= policy.ordinary_budget_sat).context("opening fees and external partition exceed ordinary total")
	}
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPolicy { policy: OrdinaryFeePolicy, opening_sha256: sha256::Hash }

struct Ledger {
	file: File, charged: u64, length: u64, attempts: u64,
	urgent_charged: u64, urgent: BTreeMap<u64, UrgentReservation>,
}

struct UrgentReservation { ceiling: u64, signed: Option<Vec<Transaction>> }

/// Same immutable policy and journal as ordinary reservations. The kind is
/// explicit: an ordinary attempt index cannot authorize an urgent publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UrgentFeeAttempt { pub policy_sha256: sha256::Hash, pub index: u64 }

pub(crate) struct OrdinaryFeeBudget {
	policy: OrdinaryFeePolicy,
	pub(crate) opening: OrdinaryFeeOpening,
	identity: sha256::Hash,
	ledger: Mutex<Ledger>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OrdinaryFeeAttempt { pub policy_sha256: sha256::Hash, pub index: u64 }

impl OrdinaryFeeBudget {
	pub(crate) fn load(directory: &Path, policy: OrdinaryFeePolicy) -> anyhow::Result<Self> {
		policy.validate()?;
		let policy_bytes = read_bounded(&directory.join(POLICY_FILE), 2048)?;
		let stored: StoredPolicy = serde_json::from_slice(&policy_bytes)?;
		ensure!(serde_json::to_vec(&stored)? == policy_bytes, "ordinary fee policy is not canonical");
		ensure!(stored.policy == policy, "ordinary fee policy differs from immutable independent history");
		let opening_bytes = read_bounded(&directory.join(OPENING_FILE), 2048)?;
		ensure!(sha256::Hash::hash(&opening_bytes) == stored.opening_sha256 &&
			policy.opening_sha256.is_none_or(|h| h == stored.opening_sha256), "ordinary fee opening digest differs");
		let opening: OrdinaryFeeOpening = serde_json::from_slice(&opening_bytes)?;
		ensure!(serde_json::to_vec(&opening)? == opening_bytes, "ordinary fee opening is not canonical");
		opening.validate(policy)?;
		let mut file = OpenOptions::new().read(true).append(true).open(directory.join(BUDGET_FILE))
			.context("ordinary fee history missing; explicit adoption or fresh creation required")?;
		file.try_lock().context("ordinary fee history already owned by another process")?;
		let length = file.metadata()?.len();
		ensure!(length <= MAX_HISTORY_BYTES, "ordinary fee history exceeds size limit");
		file.seek(SeekFrom::Start(0))?;
		let mut history = String::new();
		(&mut file).take(MAX_HISTORY_BYTES + 1).read_to_string(&mut history)?;
		ensure!(history.len() as u64 == length, "ordinary fee history changed during load");
		let (charged, attempts, urgent_charged, urgent) = parse_history(&history, opening.initial_charge(policy)?, policy)?;
		Ok(Self { policy, opening, identity: sha256::Hash::hash(&policy_bytes),
			ledger: Mutex::new(Ledger { file, charged, length, attempts, urgent_charged, urgent }) })
	}

	/// Call after independent witness validation, before construction/recovery.
	pub(crate) fn require_retained_fees(&self, retained: Amount, original: Amount, history_bytes: u64) -> anyhow::Result<()> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(original.to_sat() == self.opening.native_spent_sat, "original signed history differs from opening native cost");
		ensure!(ledger.attempts > 0 || history_bytes == self.opening.history_prefix_bytes,
			"initial adoption prefix omits existing signed history");
		let native_charged = ledger.charged.checked_sub(ledger.urgent_charged)
			.and_then(|n| n.checked_sub(self.opening.external_spent_sat))
			.and_then(|n| n.checked_sub(self.policy.external_future_reserved_sat)).context("invalid native fee charge")?;
		ensure!(retained.to_sat() <= native_charged, "ordinary reservations do not cover all retained signed versions");
		Ok(())
	}

	pub(crate) fn urgent_reserve(&self) -> Amount { Amount::from_sat(self.policy.urgent_reserve_sat) }


	/// Urgent attempts consume the same lifetime total, with an additional urgent
	/// sub-limit. This deliberately cannot create a second independent allowance.
	/// Call before signing; an unbound or failed attempt never refunds its ceiling.
	pub(crate) fn reserve_urgent_ceiling(&self, ceiling: Amount) -> anyhow::Result<UrgentFeeAttempt> {
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		let charged = checked_charge(ledger.charged, ceiling.to_sat(), self.policy)?;
		let urgent = checked_urgent_charge(ledger.urgent_charged, ceiling.to_sat(), self.policy)?;
		let index = u64::try_from(ledger.urgent.len())?.checked_add(1).context("urgent attempt index overflow")?;
		append_entry(&mut ledger, format!("u:{index}:{}\n", ceiling.to_sat()).as_bytes())?;
		ledger.charged = charged; ledger.urgent_charged = urgent;
		ledger.urgent.insert(index, UrgentReservation { ceiling: ceiling.to_sat(), signed: None });
		Ok(UrgentFeeAttempt { policy_sha256: self.identity, index })
	}

	/// Retain the exact package independently BEFORE its DB publication/Core await.
	/// A retry of identical bytes is free; a different witness/package is refused.
	pub(crate) fn bind_urgent_signed(&self, attempt: UrgentFeeAttempt, txs: &[Transaction], fee: Amount) -> anyhow::Result<()> {
		ensure!(attempt.policy_sha256 == self.identity, "urgent attempt policy differs");
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed before urgent binding");
		let reservation = ledger.urgent.get(&attempt.index).context("urgent attempt was not reserved")?;
		ensure!(fee.to_sat() <= reservation.ceiling, "urgent signed fee exceeds reserved ceiling");
		let encoded = encode_package(txs)?;
		if let Some(retained) = &reservation.signed {
			ensure!(encode_package(retained)? == encoded, "urgent exact-byte retry differs");
			return Ok(());
		}
		ensure!(!ledger.urgent.values().filter_map(|r| r.signed.as_ref()).flatten()
			.any(|old| txs.iter().any(|new| old.compute_txid() == new.compute_txid())),
			"urgent transaction already belongs to another attempt");
		append_entry(&mut ledger, format!("p:{}:{encoded}\n", attempt.index).as_bytes())?;
		ledger.urgent.get_mut(&attempt.index).expect("reserved above").signed = Some(txs.to_vec());
		Ok(())
	}

	pub(crate) fn retained_urgent(&self) -> anyhow::Result<Vec<(UrgentFeeAttempt, Vec<Transaction>)>> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed before urgent replay");
		Ok(ledger.urgent.iter().filter_map(|(&index, reservation)| reservation.signed.clone()
			.map(|txs| (UrgentFeeAttempt { policy_sha256: self.identity, index }, txs))).collect())
	}

	/// One immutable identity per external prepare invocation. Failed attempts stay
	/// charged; exact retained-byte publication never calls this method.
	pub(crate) fn reserve_ceiling(&self, ceiling: Amount) -> anyhow::Result<OrdinaryFeeAttempt> {
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "ordinary fee history changed; restart and reconcile");
		let charged = discretionary_charge(ledger.charged, ceiling.to_sat(), ledger.urgent_charged, self.policy)?;
		let index = ledger.attempts.checked_add(1).context("ordinary attempt index overflow")?;
		let entry = format!("{}\n", ceiling.to_sat());
		ensure!(ledger.length + entry.len() as u64 <= MAX_HISTORY_BYTES, "ordinary fee history capacity exhausted");
		ledger.file.write_all(entry.as_bytes())?;
		ledger.file.sync_all()?;
		ledger.length += entry.len() as u64;
		ledger.charged = charged;
		ledger.attempts = index;
		Ok(OrdinaryFeeAttempt { policy_sha256: self.identity, index })
	}
}

fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
	let file = File::open(path)?;
	ensure!(file.metadata()?.len() <= limit, "ordinary fee metadata exceeds size limit");
	let mut bytes = Vec::new(); file.take(limit + 1).read_to_end(&mut bytes)?;
	ensure!(bytes.len() as u64 <= limit, "ordinary fee metadata grew beyond size limit");
	Ok(bytes)
}

fn checked_charge(charged: u64, ceiling: u64, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	ensure!(ceiling > 0, "ordinary fee reservation ceiling is zero");
	let next = charged.checked_add(ceiling).context("ordinary fee budget overflow")?;
	ensure!(next <= policy.ordinary_budget_sat, "ordinary lifetime budget exhausted; urgent funds remain protected");
	Ok(next)
}

fn discretionary_charge(charged: u64, ceiling: u64, urgent_charged: u64, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	let next = checked_charge(charged, ceiling, policy)?;
	require_urgent_headroom(next, urgent_charged, policy)?;
	Ok(next)
}

fn require_urgent_headroom(charged: u64, urgent_charged: u64, policy: OrdinaryFeePolicy) -> anyhow::Result<()> {
	let remaining = policy.urgent_reserve_sat.checked_sub(urgent_charged).context("urgent budget already exceeded")?;
	ensure!(charged.checked_add(remaining).is_some_and(|n| n <= policy.ordinary_budget_sat),
		"existing fee history leaves insufficient urgent authorization; preserve history and reconcile");
	Ok(())
}

fn checked_urgent_charge(charged: u64, ceiling: u64, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	let next = charged.checked_add(ceiling).context("urgent fee budget overflow")?;
	ensure!(ceiling > 0 && next <= policy.urgent_reserve_sat, "urgent lifetime budget exhausted");
	Ok(next)
}

fn append_entry(ledger: &mut Ledger, entry: &[u8]) -> anyhow::Result<()> {
	ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed; restart and reconcile");
	ensure!(ledger.length.checked_add(entry.len() as u64).is_some_and(|n| n <= MAX_HISTORY_BYTES), "fee history capacity exhausted");
	ledger.file.write_all(entry)?; ledger.file.sync_all()?;
	ledger.length += entry.len() as u64;
	Ok(())
}

fn encode_package(txs: &[Transaction]) -> anyhow::Result<String> {
	ensure!(!txs.is_empty() && txs.len() <= 2 && txs.iter().all(|tx| tx.weight().to_wu() <= 400_000),
		"urgent signed package exceeds bounds");
	let ids = txs.iter().map(Transaction::compute_txid).collect::<std::collections::BTreeSet<_>>();
	ensure!(ids.len() == txs.len(), "urgent package duplicates a transaction");
	Ok(serde_json::to_string(&txs.iter().map(bitcoin::consensus::serialize).collect::<Vec<_>>())?)
}

fn parse_history(history: &str, opening: u64, policy: OrdinaryFeePolicy)
	-> anyhow::Result<(u64, u64, u64, BTreeMap<u64, UrgentReservation>)>
{
	ensure!(history.is_empty() || history.ends_with('\n'), "ordinary fee history has a torn append");
	let (mut charged, mut attempts, mut urgent_charged) = (opening, 0_u64, 0_u64);
	let mut urgent = BTreeMap::<u64, UrgentReservation>::new();
	for line in history.lines() {
		if let Some(record) = line.strip_prefix("u:") {
			let (index, amount) = record.split_once(':').context("invalid urgent reservation")?;
			let index = index.parse::<u64>()?; let amount = amount.parse::<u64>()?;
			ensure!(line == format!("u:{index}:{amount}") && index == urgent.len() as u64 + 1, "noncanonical urgent index/amount");
			charged = checked_charge(charged, amount, policy)?;
			urgent_charged = checked_urgent_charge(urgent_charged, amount, policy)?;
			urgent.insert(index, UrgentReservation { ceiling: amount, signed: None });
		} else if let Some(record) = line.strip_prefix("p:") {
			let (index, bytes) = record.split_once(':').context("invalid urgent signed binding")?;
			let index = index.parse::<u64>()?;
			let raw: Vec<Vec<u8>> = serde_json::from_str(bytes)?;
			let txs = raw.iter().map(|bytes| bitcoin::consensus::deserialize(bytes))
				.collect::<Result<Vec<Transaction>, _>>()?;
			ensure!(line == format!("p:{index}:{}", encode_package(&txs)?), "noncanonical urgent signed binding");
			ensure!(!urgent.values().filter_map(|r| r.signed.as_ref()).flatten()
				.any(|old| txs.iter().any(|new| old.compute_txid() == new.compute_txid())), "duplicate urgent signed transaction");
			let reservation = urgent.get_mut(&index).context("signed urgent attempt has no prior reservation")?;
			ensure!(reservation.signed.is_none(), "duplicate urgent binding");
			reservation.signed = Some(txs);
		} else {
			let amount = line.parse::<u64>().context("invalid ordinary fee reservation")?;
			ensure!(line == amount.to_string(), "noncanonical ordinary fee reservation");
			charged = checked_charge(charged, amount, policy)?;
			attempts = attempts.checked_add(1).context("ordinary attempt count overflow")?;
		}
	}
	require_urgent_headroom(charged, urgent_charged, policy)?;
	Ok((charged, attempts, urgent_charged, urgent))
}

#[cfg(test)]
mod tests {
	use super::*;
	use ark::test_util::dummy::{DUMMY_SERVER_KEY, DUMMY_USER_KEY};

	fn directory() -> std::path::PathBuf {
		let path = std::env::temp_dir().join(format!("ark-b-ordinary-{}-{}",
			std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&path).unwrap(); path
	}

	fn adopted(directory: &Path) -> (OrdinaryFeePolicy, Vec<u8>) {
		let history = format!("{}\n", bitcoin::Txid::from_byte_array([7; 32])).into_bytes();
		let opening = OrdinaryFeeOpening { version: 1, network: Network::Signet,
			genesis: bitcoin::blockdata::constants::genesis_block(Network::Signet).block_hash(),
			server_pubkey: DUMMY_SERVER_KEY.public_key(), history_prefix_bytes: history.len() as u64,
			history_prefix_sha256: sha256::Hash::hash(&history), native_spent_sat: 4_985,
			external_spent_sat: 879, accounting_receipt_sha256: Some(sha256::Hash::hash(b"synthetic unit receipt")) };
		let bytes = serde_json::to_vec(&opening).unwrap();
		let digest = sha256::Hash::hash(&bytes);
		let policy = OrdinaryFeePolicy { opening_sha256: Some(digest), ..Default::default() };
		std::fs::write(directory.join(OPENING_FILE), bytes).unwrap();
		std::fs::write(directory.join(POLICY_FILE), serde_json::to_vec(&StoredPolicy { policy, opening_sha256: digest }).unwrap()).unwrap();
		std::fs::write(directory.join(BUDGET_FILE), b"").unwrap();
		(policy, history)
	}

	#[test]
	fn ordinary_opening_preserves_all_old_fees_and_external_partition() {
		let dir = directory(); let (policy, history) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert_eq!(budget.opening.initial_charge(policy).unwrap(), 25_864);
		assert_eq!(policy.ordinary_budget_sat - budget.opening.initial_charge(policy).unwrap(), 34_136);
		budget.require_retained_fees(Amount::from_sat(4_985), Amount::from_sat(4_985), history.len() as u64).unwrap();
		assert!(budget.require_retained_fees(Amount::from_sat(4_986), Amount::from_sat(4_985), history.len() as u64).is_err());
		assert!(budget.require_retained_fees(Amount::from_sat(4_985), Amount::from_sat(4_984), history.len() as u64).is_err());
		assert!(budget.require_retained_fees(Amount::from_sat(4_985), Amount::from_sat(4_985), history.len() as u64 + 65).is_err());
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_failed_charges_and_attempt_identity_survive_restart_without_credit() {
		let dir = directory(); let (policy, history) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err(), "second owner refuses");
		let first = budget.reserve_ceiling(Amount::from_sat(10_000)).unwrap();
		assert_eq!(first.index, 1);
		// Failed construction creates no carrier and still retains its charge.
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let second = budget.reserve_ceiling(Amount::from_sat(9_136)).unwrap();
		assert_eq!(second.policy_sha256, first.policy_sha256); assert_eq!(second.index, 2);
		let before = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		for _ in 0..3 {
			budget.require_retained_fees(Amount::from_sat(4_985), Amount::from_sat(4_985), history.len() as u64).unwrap();
		}
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), before, "retained-byte reconciliation charges zero");
		// All remaining 15k authorization is ring-fenced for urgent work.
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
		budget.reserve_urgent_ceiling(Amount::from_sat(15_000)).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err()); drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err(), "restart cannot replenish exhaustion");
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_missing_changed_or_torn_history_refuses() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		assert!(OrdinaryFeeBudget::load(&dir, OrdinaryFeePolicy { opening_sha256: None, ..policy }).is_err());
		assert!(OrdinaryFeeBudget::load(&dir, OrdinaryFeePolicy { external_future_reserved_sat: 0, ..policy }).is_err());
		assert!(OrdinaryFeeBudget::load(&dir, OrdinaryFeePolicy { ordinary_budget_sat: 60_001, ..policy }).is_err());
		let original_policy = std::fs::read(dir.join(POLICY_FILE)).unwrap();
		let mut noncanonical = original_policy.clone(); noncanonical.push(b'\n');
		std::fs::write(dir.join(POLICY_FILE), noncanonical).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err(), "policy identity cannot change through formatting");
		std::fs::write(dir.join(POLICY_FILE), original_policy).unwrap();
		std::fs::write(dir.join(BUDGET_FILE), b"10000\n100").unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err());
		std::fs::write(dir.join(BUDGET_FILE), b"010000\n").unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err());
		std::fs::remove_file(dir.join(BUDGET_FILE)).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err(), "no automatic initialization");
		assert!(policy.initialize_fresh(&dir, Network::Signet, DUMMY_SERVER_KEY.public_key()).is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_opening_identity_prefix_and_receipt_cannot_be_substituted() {
		let dir = directory(); let (policy, history) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap(); let opening = &budget.opening;
		opening.verify_prefix(Network::Signet, DUMMY_SERVER_KEY.public_key(), &history).unwrap();
		assert!(opening.verify_prefix(Network::Regtest, DUMMY_SERVER_KEY.public_key(), &history).is_err());
		assert!(opening.verify_prefix(Network::Signet, DUMMY_USER_KEY.public_key(), &history).is_err());
		assert!(opening.verify_prefix(Network::Signet, DUMMY_SERVER_KEY.public_key(), &[]).is_err());
		let mut changed = history.clone(); changed[0] ^= 1;
		assert!(opening.verify_prefix(Network::Signet, DUMMY_SERVER_KEY.public_key(), &changed).is_err());
		let mut changed = opening.clone(); changed.external_spent_sat = 0;
		std::fs::write(dir.join(OPENING_FILE), serde_json::to_vec(&changed).unwrap()).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err(), "explicit historical amounts are digest-bound");
		changed = opening.clone(); changed.accounting_receipt_sha256 = None;
		assert!(changed.validate(policy).is_err());
		std::fs::write(dir.join(OPENING_FILE), serde_json::to_vec(&changed).unwrap()).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err());
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_fresh_initialization_is_distinct_and_never_overwrites() {
		let dir = directory(); let policy = OrdinaryFeePolicy::default();
		policy.initialize_fresh(&dir, Network::Regtest, DUMMY_SERVER_KEY.public_key()).unwrap();
		assert!(policy.initialize_fresh(&dir, Network::Regtest, DUMMY_SERVER_KEY.public_key()).is_err());
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert_eq!(budget.urgent_reserve(), Amount::from_sat(15_000));
		let finite = OrdinaryFeePolicy { ordinary_budget_sat: 40_960_000, urgent_reserve_sat: 40_000,
			external_future_reserved_sat: 0, opening_sha256: None };
		finite.validate().unwrap();
		OrdinaryFeePolicy { ordinary_budget_sat: 500, ..finite }.validate().unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, finite).is_err(), "new declaration cannot raise an existing lifetime allowance");
		assert!(OrdinaryFeePolicy { urgent_reserve_sat: 0, ..finite }.validate().is_err());
		assert!(OrdinaryFeePolicy { ordinary_budget_sat: Amount::MAX_MONEY.to_sat() + 1, ..finite }.validate().is_err());

		budget.require_retained_fees(Amount::ZERO, Amount::ZERO, 0).unwrap();
		assert!(budget.require_retained_fees(Amount::ZERO, Amount::ZERO, 65).is_err());
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

	fn urgent_tx(tag: u8) -> Transaction {
		Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint { txid: bitcoin::Txid::from_byte_array([tag; 32]), vout: 0 },
				script_sig: bitcoin::ScriptBuf::new(), sequence: bitcoin::Sequence::ZERO,
				witness: bitcoin::Witness::from_slice(&[[1_u8; 64]]) }],
			output: vec![bitcoin::TxOut { value: Amount::from_sat(1000), script_pubkey: bitcoin::ScriptBuf::new() }] }
	}

	#[test]
	fn urgent_failed_reservation_and_exact_byte_retry_survive_restart() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let failed = budget.reserve_urgent_ceiling(Amount::from_sat(5_000)).unwrap();
		let bound = budget.reserve_urgent_ceiling(Amount::from_sat(10_000)).unwrap();
		assert_ne!(failed.index, bound.index);
		let tx = urgent_tx(8);
		budget.bind_urgent_signed(bound, &[tx.clone()], Amount::from_sat(500)).unwrap();
		let bytes = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		budget.bind_urgent_signed(bound, &[tx.clone()], Amount::from_sat(500)).unwrap();
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), bytes);
		assert!(budget.bind_urgent_signed(bound, &[urgent_tx(9)], Amount::from_sat(500)).is_err());
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert!(budget.reserve_urgent_ceiling(Amount::from_sat(1)).is_err());
		assert_eq!(budget.retained_urgent().unwrap(), vec![(bound, vec![tx])]);
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), bytes);
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn urgent_headroom_is_reserved_before_discretionary_and_no_replenishment() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		budget.reserve_ceiling(Amount::from_sat(19_136)).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
		budget.reserve_urgent_ceiling(Amount::from_sat(15_000)).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
		assert!(budget.reserve_urgent_ceiling(Amount::from_sat(1)).is_err());
		drop(budget);
		assert!(OrdinaryFeeBudget::load(&dir, policy).unwrap().reserve_ceiling(Amount::from_sat(1)).is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn urgent_old_history_cannot_be_reset_to_create_missing_headroom() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		std::fs::write(dir.join(BUDGET_FILE), b"20000\n").unwrap();
		let original = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err());
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), original);
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn urgent_unreserved_duplicate_torn_and_over_ceiling_binding_refuse() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let first = budget.reserve_urgent_ceiling(Amount::from_sat(1_000)).unwrap();
		let second = budget.reserve_urgent_ceiling(Amount::from_sat(1_000)).unwrap();
		let tx = urgent_tx(10);
		assert!(budget.bind_urgent_signed(first, &[tx.clone()], Amount::from_sat(1_001)).is_err());
		budget.bind_urgent_signed(first, &[tx.clone()], Amount::from_sat(1_000)).unwrap();
		assert!(budget.bind_urgent_signed(second, &[tx], Amount::from_sat(1_000)).is_err());
		drop(budget);
		let valid = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		let mut torn = valid.clone(); torn.extend_from_slice(b"p:2:");
		std::fs::write(dir.join(BUDGET_FILE), torn).unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err());
		std::fs::write(dir.join(BUDGET_FILE), b"p:1:[]\n").unwrap();
		assert!(OrdinaryFeeBudget::load(&dir, policy).is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}


	#[test]
	fn ordinary_discretionary_fees_must_leave_urgent_authorization() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		budget.reserve_ceiling(Amount::from_sat(19_136)).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err(),
			"discretionary signing consumed authorization reserved for urgent defense");
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

}
