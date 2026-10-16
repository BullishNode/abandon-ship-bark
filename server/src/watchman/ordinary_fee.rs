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
use super::ordinary_funding::{FundingCandidate, FundingReservation, FundingResolution};

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
	file: File,
	charged: u64,
	length: u64,
	attempts: u64,
	urgent_charged: u64,
	urgent: BTreeMap<u64, UrgentReservation>,
	funding: BTreeMap<sha256::Hash, FundingState>,
	funding_charged: u64,
	native_uncertain: bool,
	resolutions: BTreeMap<u64, FundingResolution>,
}

#[derive(Clone)]
pub(crate) struct FundingState {
	pub reservation: FundingReservation,
	pub signed: Option<Transaction>,
}

struct UrgentReservation { ceiling: u64, signed: Option<(sha256::Hash, Vec<Transaction>)> }

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
		ensure!(
			serde_json::to_vec(&stored)? == policy_bytes,
			"ordinary fee policy is not canonical"
		);
		ensure!(
			stored.policy == policy,
			"ordinary fee policy differs from immutable independent history"
		);
		let opening_bytes = read_bounded(&directory.join(OPENING_FILE), 2048)?;
		ensure!(
			sha256::Hash::hash(&opening_bytes) == stored.opening_sha256
				&& policy
					.opening_sha256
					.is_none_or(|h| h == stored.opening_sha256),
			"ordinary fee opening digest differs"
		);
		let opening: OrdinaryFeeOpening = serde_json::from_slice(&opening_bytes)?;
		ensure!(
			serde_json::to_vec(&opening)? == opening_bytes,
			"ordinary fee opening is not canonical"
		);
		opening.validate(policy)?;
		let mut file = OpenOptions::new()
			.read(true)
			.append(true)
			.open(directory.join(BUDGET_FILE))
			.context(
				"ordinary fee history missing; explicit adoption or fresh creation required",
			)?;
		file.try_lock()
			.context("ordinary fee history already owned by another process")?;
		let length = file.metadata()?.len();
		ensure!(
			length <= MAX_HISTORY_BYTES,
			"ordinary fee history exceeds size limit"
		);
		file.seek(SeekFrom::Start(0))?;
		let mut history = String::new();
		(&mut file)
			.take(MAX_HISTORY_BYTES + 1)
			.read_to_string(&mut history)?;
		ensure!(
			history.len() as u64 == length,
			"ordinary fee history changed during load"
		);
		let history = parse_history(&history, opening.initial_charge(policy)?, policy)?;
		ensure!(
			history
				.funding
				.values()
				.all(|s| s.reservation.policy_sha256 == sha256::Hash::hash(&policy_bytes)),
			"native funding stored policy identity differs"
		);
		Ok(Self {
			policy,
			opening,
			identity: sha256::Hash::hash(&policy_bytes),
			ledger: Mutex::new(Ledger {
				file,
				charged: history.charged,
				length,
				attempts: history.attempts,
				urgent_charged: history.urgent_charged,
				urgent: history.urgent,
				funding: history.funding,
				funding_charged: history.funding_charged,
				native_uncertain: false,
				resolutions: history.resolutions,
			}),
		})
	}

	/// Call after independent witness validation, before construction/recovery.
	pub(crate) fn require_retained_fees(&self, retained: Amount, original: Amount, history_bytes: u64) -> anyhow::Result<()> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(original.to_sat() == self.opening.native_spent_sat, "original signed history differs from opening native cost");
		ensure!(ledger.attempts > 0 || history_bytes == self.opening.history_prefix_bytes,
			"initial adoption prefix omits existing signed history");
		let native_charged = ledger.charged.checked_sub(ledger.urgent_charged)
			.and_then(|n| n.checked_sub(ledger.funding_charged))
			.and_then(|n| n.checked_sub(self.opening.external_spent_sat))
			.and_then(|n| n.checked_sub(self.policy.external_future_reserved_sat)).context("invalid native fee charge")?;
		ensure!(retained.to_sat() <= native_charged, "ordinary reservations do not cover all retained signed versions");
		Ok(())
	}

	pub(crate) fn urgent_reserve(&self) -> anyhow::Result<Amount> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed before cash reserve check");
		Ok(Amount::from_sat(self.policy.urgent_reserve_sat.checked_sub(ledger.urgent_charged)
			.context("urgent allowance already exceeded")?))
	}


	pub(crate) fn urgent_ceiling_limit(&self, maximum: Amount) -> anyhow::Result<Amount> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		let remaining = self.policy.urgent_reserve_sat.checked_sub(ledger.urgent_charged)
			.context("urgent lifetime budget exceeded")?;
		let shared = self.policy.ordinary_budget_sat.checked_sub(ledger.charged).context("lifetime budget exceeded")?;
		let limit = maximum.to_sat().min(remaining).min(shared);
		ensure!(limit > 0, "urgent lifetime allowance exhausted; funding cannot refill history");
		Ok(Amount::from_sat(limit))
	}

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
	pub(crate) fn bind_urgent_signed(&self, attempt: UrgentFeeAttempt, txs: &[Transaction], fee: Amount, source_binding: sha256::Hash) -> anyhow::Result<()> {
		ensure!(attempt.policy_sha256 == self.identity && source_binding != sha256::Hash::all_zeros(), "urgent attempt/source identity differs");
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed before urgent binding");
		let reservation = ledger.urgent.get(&attempt.index).context("urgent attempt was not reserved")?;
		ensure!(fee.to_sat() <= reservation.ceiling, "urgent signed fee exceeds reserved ceiling");
		let encoded = encode_package(txs)?;
		if let Some((binding, retained)) = &reservation.signed {
			ensure!(*binding == source_binding && encode_package(retained)? == encoded, "urgent exact-byte retry differs");
			return Ok(());
		}
		check_shared_package_bytes(&ledger.urgent, txs)?;
		append_entry(&mut ledger, format!("p:{}:{source_binding}:{encoded}\n", attempt.index).as_bytes())?;
		ledger.urgent.get_mut(&attempt.index).expect("reserved above").signed = Some((source_binding, txs.to_vec()));
		Ok(())
	}

	#[cfg(test)]
	pub(crate) fn retained_urgent(&self) -> anyhow::Result<Vec<(UrgentFeeAttempt, sha256::Hash, Vec<Transaction>)>> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed before urgent replay");
		Ok(ledger.urgent.iter().filter_map(|(&index, reservation)| reservation.signed.clone()
			.map(|(binding, txs)| (UrgentFeeAttempt { policy_sha256: self.identity, index }, binding, txs))).collect())
	}

	pub(crate) fn identity(&self) -> sha256::Hash {
		self.identity
	}
	pub(crate) fn opening_digest(&self) -> anyhow::Result<sha256::Hash> {
		Ok(sha256::Hash::hash(&serde_json::to_vec(&self.opening)?))
	}
	pub(crate) fn mark_native_uncertain(&self) -> anyhow::Result<()> {
		self.ledger
			.lock()
			.map_err(|_| anyhow!("fee ledger lock poisoned"))?
			.native_uncertain = true;
		Ok(())
	}
	pub(crate) fn funding_states(&self) -> anyhow::Result<Vec<FundingState>> {
		let ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(
			ledger.file.metadata()?.len() == ledger.length,
			"funding fee history changed"
		);
		Ok(ledger.funding.values().cloned().collect())
	}
	pub(crate) fn require_native_available(&self) -> anyhow::Result<()> {
		let ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("native fee lock poisoned"))?;
		ensure!(
			!ledger.native_uncertain && ledger.file.metadata()?.len() == ledger.length,
			"native funding state uncertain; retain original intents"
		);
		Ok(())
	}
	pub(crate) fn funding_resolutions(&self) -> anyhow::Result<Vec<FundingResolution>> {
		let ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("native fee lock poisoned"))?;
		ensure!(
			ledger.file.metadata()?.len() == ledger.length,
			"native funding history changed"
		);
		Ok(ledger.resolutions.values().cloned().collect())
	}
	pub(crate) fn resolve_funding(&self, resolution: FundingResolution) -> anyhow::Result<()> {
		let mut ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("native fee lock poisoned"))?;
		ensure!(!ledger.native_uncertain, "native funding state uncertain");
		validate_resolution(&resolution, &ledger.funding)?;
		if let Some(old) = ledger.resolutions.get(&resolution.winner_index) {
			ensure!(
				*old == resolution,
				"native funding winner block changed; current-chain reconciliation required"
			);
			return Ok(());
		}
		append_native_entry(
			&mut ledger,
			format!("r:{}\n", serde_json::to_string(&resolution)?).as_bytes(),
		)?;
		ledger
			.resolutions
			.insert(resolution.winner_index, resolution);
		Ok(())
	}

	pub(crate) fn reserve_funding(
		&self,
		candidate: FundingCandidate,
		maximum: bitcoin::FeeRate,
	) -> anyhow::Result<FundingReservation> {
		candidate.require_rate(maximum)?;
		let mut ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(
			!ledger.native_uncertain,
			"native funding outcome unknown; original inputs retained"
		);
		ensure!(
			ledger.file.metadata()?.len() == ledger.length,
			"native fee history changed before retry or append"
		);
		let key = candidate.key();
		if let Some(old) = ledger.funding.get(&key) {
			ensure!(
				old.reservation.candidate == candidate && old.reservation.max_fee_rate == maximum,
				"native attempt candidate or fee ceiling changed"
			);
			return Ok(old.reservation.clone());
		}
		ensure!(
			ledger.funding.len() < 4096,
			"native funding reservation count bound"
		);
		ensure!(
			ledger
				.funding
				.values()
				.all(|s| s.signed.is_none() || s.reservation.candidate.family != candidate.family),
			"signed native funding family is closed to new attempts"
		);
		let points = candidate
			.transaction()?
			.input
			.into_iter()
			.map(|i| i.previous_output)
			.collect::<std::collections::BTreeSet<_>>();
		for old in ledger.funding.values() {
			if old
				.reservation
				.candidate
				.transaction()?
				.input
				.iter()
				.any(|i| points.contains(&i.previous_output))
				&& !resolved_family(old, &ledger.funding, &ledger.resolutions)
			{
				ensure!(
					old.signed.is_none()
						&& candidate.follows_unsigned_round(&old.reservation.candidate),
					"native input belongs to a foreign, signed or noncurrent family"
				);
			}
		}
		let fee = candidate.fee()?.to_sat();
		let charged =
			discretionary_charge(ledger.charged, fee, ledger.urgent_charged, self.policy)?;
		let index = ledger
			.attempts
			.checked_add(1)
			.context("ordinary attempt count overflow")?;
		let reservation = FundingReservation {
			policy_sha256: self.identity,
			index,
			candidate,
			max_fee_rate: maximum,
		};
		append_native_entry(
			&mut ledger,
			format!("f:{}\n", serde_json::to_string(&reservation)?).as_bytes(),
		)?;
		ledger.charged = charged;
		ledger.attempts = index;
		ledger.funding_charged = ledger
			.funding_charged
			.checked_add(fee)
			.context("native funding fee overflow")?;
		ledger.funding.insert(
			key,
			FundingState {
				reservation: reservation.clone(),
				signed: None,
			},
		);
		Ok(reservation)
	}
	pub(crate) fn bind_funding(
		&self,
		reservation: &FundingReservation,
		tx: &Transaction,
	) -> anyhow::Result<()> {
		reservation.candidate.verify_signed(tx)?;
		let mut ledger = self
			.ledger
			.lock()
			.map_err(|_| anyhow!("fee ledger lock poisoned"))?;
		ensure!(
			!ledger.native_uncertain,
			"native funding outcome unknown; original inputs retained"
		);
		ensure!(
			ledger.file.metadata()?.len() == ledger.length,
			"native fee history changed before retry or append"
		);
		let key = reservation.candidate.key();
		let old = ledger
			.funding
			.get(&key)
			.context("native funding was never reserved")?;
		ensure!(
			old.reservation == *reservation && reservation.policy_sha256 == self.identity,
			"native funding reservation differs"
		);
		if let Some(signed) = &old.signed {
			ensure!(signed == tx, "native signed retry differs");
			return Ok(());
		}
		for (other, state) in &ledger.funding {
			if *other != key && state.reservation.candidate.family == reservation.candidate.family {
				ensure!(
					state.signed.is_none()
						&& state.reservation.candidate.generation
							< reservation.candidate.generation,
					"competing signed/newer native Round intent exists"
				);
			}
		}
		append_native_entry(
			&mut ledger,
			format!(
				"g:{}:{}\n",
				reservation.index,
				serde_json::to_string(&bitcoin::consensus::serialize(tx))?
			)
			.as_bytes(),
		)?;
		ledger.funding.get_mut(&key).expect("checked above").signed = Some(tx.clone());
		Ok(())
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

/// No-budget compatibility is only for a verifiably empty legacy installation.
/// The caller supplies the count from its actual bounded durable DB inventory.
pub(crate) fn urgent_budget_required(directory: &Path, retained_count: usize) -> anyhow::Result<bool> {
	let mut present = false;
	for name in [POLICY_FILE, OPENING_FILE, BUDGET_FILE] {
		match std::fs::symlink_metadata(directory.join(name)) {
			Ok(metadata) => {
				ensure!(metadata.file_type().is_file(), "fee budget metadata is not a regular file");
				present = true;
			},
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
			Err(error) => return Err(error).context("fee budget absence is not verifiable"),
		}
	}
	ensure!(present || retained_count == 0, "urgent DB metadata requires the missing independent budget");
	Ok(present)
}

/// Read-only admission for captaind, which must not acquire watchmand's FLOCK.
pub(crate) fn require_installed_policy(directory: &Path, expected: OrdinaryFeePolicy) -> anyhow::Result<OrdinaryFeeOpening> {
	let stored: StoredPolicy = serde_json::from_slice(&read_bounded(&directory.join(POLICY_FILE), 2048)?)?;
	ensure!(stored.policy == expected, "configured direct fee policy differs from immutable installation");
	urgent_snapshot(directory)?;
	Ok(serde_json::from_slice(&read_bounded(&directory.join(OPENING_FILE), 2048)?)?)
}

/// Read-only startup cross-check for BOTH captaind and watchmand. It grants no
/// signing authority and never repairs missing history from database contents.
pub(crate) fn urgent_snapshot(
	directory: &Path,
) -> anyhow::Result<(
	sha256::Hash,
	Vec<(
		UrgentFeeAttempt,
		u64,
		Option<(sha256::Hash, Vec<Transaction>)>,
	)>,
)> {
	let policy_raw = read_bounded(&directory.join(POLICY_FILE), 2048)?;
	let stored: StoredPolicy = serde_json::from_slice(&policy_raw)?;
	ensure!(
		serde_json::to_vec(&stored)? == policy_raw,
		"noncanonical immutable fee policy"
	);
	stored.policy.validate()?;
	let opening_raw = read_bounded(&directory.join(OPENING_FILE), 2048)?;
	ensure!(
		sha256::Hash::hash(&opening_raw) == stored.opening_sha256
			&& stored
				.policy
				.opening_sha256
				.is_none_or(|h| h == stored.opening_sha256),
		"urgent opening digest differs"
	);
	let opening: OrdinaryFeeOpening = serde_json::from_slice(&opening_raw)?;
	ensure!(
		serde_json::to_vec(&opening)? == opening_raw,
		"noncanonical immutable fee opening"
	);
	opening.validate(stored.policy)?;
	let path = directory.join(BUDGET_FILE);
	let before = std::fs::metadata(&path)?;
	let raw = read_bounded(&path, MAX_HISTORY_BYTES)?;
	let after = std::fs::metadata(&path)?;
	ensure!(
		before.len() == after.len()
			&& before.modified()? == after.modified()?
			&& raw.len() as u64 == after.len(),
		"fee history changed during startup observation"
	);
	let history = parse_history(
		std::str::from_utf8(&raw)?,
		opening.initial_charge(stored.policy)?,
		stored.policy,
	)?;
	ensure!(
		history
			.funding
			.values()
			.all(|s| s.reservation.policy_sha256 == sha256::Hash::hash(&policy_raw)),
		"native funding stored policy identity differs"
	);
	let policy_sha256 = sha256::Hash::hash(&policy_raw);
	Ok((
		policy_sha256,
		history
			.urgent
			.into_iter()
			.map(|(index, r)| {
				(
					UrgentFeeAttempt {
						policy_sha256,
						index,
					},
					r.ceiling,
					r.signed,
				)
			})
			.collect(),
	))
}

fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
	ensure!(std::fs::symlink_metadata(path)?.file_type().is_file(), "fee budget path is not a regular file");
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

fn append_native_entry(ledger: &mut Ledger, entry: &[u8]) -> anyhow::Result<()> {
	// Definitive no-write refusals leave exact retries available. Once actual
	// IO starts, any failure is uncertainty even if metadata still looks equal.
	ensure!(
		ledger.file.metadata()?.len() == ledger.length,
		"fee history changed; restart and reconcile"
	);
	ensure!(
		ledger
			.length
			.checked_add(entry.len() as u64)
			.is_some_and(|n| n <= MAX_HISTORY_BYTES),
		"fee history capacity exhausted"
	);
	if let Err(error) = ledger
		.file
		.write_all(entry)
		.and_then(|_| ledger.file.sync_all())
	{
		ledger.native_uncertain = true;
		return Err(error).context("native fee append outcome uncertain; retain inputs");
	}
	ledger.length += entry.len() as u64;
	Ok(())
}

fn append_entry(ledger: &mut Ledger, entry: &[u8]) -> anyhow::Result<()> {
	ensure!(ledger.file.metadata()?.len() == ledger.length, "fee history changed; restart and reconcile");
	ensure!(ledger.length.checked_add(entry.len() as u64).is_some_and(|n| n <= MAX_HISTORY_BYTES), "fee history capacity exhausted");
	ledger.file.write_all(entry)?; ledger.file.sync_all()?;
	ledger.length += entry.len() as u64;
	Ok(())
}

fn check_shared_package_bytes(retained: &BTreeMap<u64, UrgentReservation>, candidate: &[Transaction]) -> anyhow::Result<()> {
	for (_, previous) in retained.values().filter_map(|r| r.signed.as_ref()) {
		ensure!(previous.last().zip(candidate.last()).is_none_or(|(old,new)| old.compute_txid() != new.compute_txid()),
			"urgent publication transaction already belongs to another attempt");
		for old in previous { for new in candidate {
			ensure!(old.compute_txid() != new.compute_txid() || old == new, "shared urgent ancestor witness differs");
		} }
	}
	Ok(())
}

fn encode_package(txs: &[Transaction]) -> anyhow::Result<String> {
	ensure!(!txs.is_empty() && txs.len() <= 2 && txs.iter().all(|tx| tx.weight().to_wu() <= 400_000),
		"urgent signed package exceeds bounds");
	let ids = txs.iter().map(Transaction::compute_txid).collect::<std::collections::BTreeSet<_>>();
	ensure!(ids.len() == txs.len(), "urgent package duplicates a transaction");
	Ok(serde_json::to_string(&txs.iter().map(bitcoin::consensus::serialize).collect::<Vec<_>>())?)
}

struct ParsedHistory {
	charged: u64,
	attempts: u64,
	urgent_charged: u64,
	urgent: BTreeMap<u64, UrgentReservation>,
	funding: BTreeMap<sha256::Hash, FundingState>,
	funding_charged: u64,
	resolutions: BTreeMap<u64, FundingResolution>,
}

fn parse_history(
	history: &str,
	opening: u64,
	policy: OrdinaryFeePolicy,
) -> anyhow::Result<ParsedHistory> {
	ensure!(
		history.is_empty() || history.ends_with('\n'),
		"ordinary fee history has a torn append"
	);
	let (mut charged, mut attempts, mut urgent_charged) = (opening, 0_u64, 0_u64);
	let mut urgent = BTreeMap::<u64, UrgentReservation>::new();
	let mut funding = BTreeMap::<sha256::Hash, FundingState>::new();
	let mut funding_charged = 0u64;
	let mut resolutions = BTreeMap::<u64, FundingResolution>::new();
	for line in history.lines() {
		if let Some(raw) = line.strip_prefix("r:") {
			let resolution: FundingResolution = serde_json::from_str(raw)?;
			ensure!(
				serde_json::to_string(&resolution)? == raw,
				"noncanonical native funding resolution"
			);
			validate_resolution(&resolution, &funding)?;
			ensure!(
				resolutions
					.insert(resolution.winner_index, resolution)
					.is_none(),
				"duplicate native funding resolution"
			);
		} else if let Some(raw) = line.strip_prefix("f:") {
			let record: FundingReservation = serde_json::from_str(raw)?;
			ensure!(
				serde_json::to_string(&record)? == raw
					&& record.index == attempts + 1
					&& funding.len() < 4096,
				"noncanonical native funding reservation"
			);
			ensure!(
				funding.values().all(|s| s.signed.is_none()
					|| s.reservation.candidate.family != record.candidate.family),
				"retained signed native family cannot create new attempts"
			);
			let fee = record.fee()?.to_sat();
			let points = record
				.candidate
				.transaction()?
				.input
				.into_iter()
				.map(|i| i.previous_output)
				.collect::<std::collections::BTreeSet<_>>();
			for old in funding.values() {
				if old
					.reservation
					.candidate
					.transaction()?
					.input
					.iter()
					.any(|i| points.contains(&i.previous_output))
					&& !resolved_family(old, &funding, &resolutions)
				{
					ensure!(
						old.signed.is_none()
							&& record
								.candidate
								.follows_unsigned_round(&old.reservation.candidate),
						"retained native funding input/family conflict"
					);
				}
			}
			charged = checked_charge(charged, fee, policy)?;
			funding_charged = funding_charged
				.checked_add(fee)
				.context("native funding charge overflow")?;
			attempts += 1;
			ensure!(
				funding
					.insert(
						record.candidate.key(),
						FundingState {
							reservation: record,
							signed: None
						}
					)
					.is_none(),
				"duplicate native funding attempt"
			);
		} else if let Some(raw) = line.strip_prefix("g:") {
			let (index, bytes) = raw
				.split_once(':')
				.context("native signed funding binding missing")?;
			let index = index.parse::<u64>()?;
			let raw: Vec<u8> = serde_json::from_str(bytes)?;
			let tx: Transaction = bitcoin::consensus::deserialize(&raw)?;
			ensure!(
				line == format!(
					"g:{index}:{}",
					serde_json::to_string(&bitcoin::consensus::serialize(&tx))?
				),
				"noncanonical native signed binding"
			);
			let candidate = funding
				.values()
				.find(|s| s.reservation.index == index)
				.context("native signed binding was never reserved")?
				.reservation
				.candidate
				.clone();
			ensure!(
				funding
					.values()
					.filter(|s| s.reservation.index != index)
					.all(|s| s.reservation.candidate.family != candidate.family
						|| (s.signed.is_none()
							&& s.reservation.candidate.generation < candidate.generation)),
				"retained competing native funding family binding"
			);
			let state = funding
				.values_mut()
				.find(|s| s.reservation.index == index)
				.context("native signed binding was never reserved")?;
			ensure!(state.signed.is_none(), "duplicate native signed binding");
			state.reservation.candidate.verify_signed(&tx)?;
			state.signed = Some(tx);
		} else if let Some(record) = line.strip_prefix("u:") {
			let (index, amount) = record
				.split_once(':')
				.context("invalid urgent reservation")?;
			let index = index.parse::<u64>()?;
			let amount = amount.parse::<u64>()?;
			ensure!(
				line == format!("u:{index}:{amount}") && index == urgent.len() as u64 + 1,
				"noncanonical urgent index/amount"
			);
			charged = checked_charge(charged, amount, policy)?;
			urgent_charged = checked_urgent_charge(urgent_charged, amount, policy)?;
			urgent.insert(
				index,
				UrgentReservation {
					ceiling: amount,
					signed: None,
				},
			);
		} else if let Some(record) = line.strip_prefix("p:") {
			let (index, bytes) = record
				.split_once(':')
				.context("invalid urgent signed binding")?;
			let index = index.parse::<u64>()?;
			let (binding, bytes) = bytes
				.split_once(':')
				.context("urgent source binding missing")?;
			let binding = binding.parse::<sha256::Hash>()?;
			ensure!(
				binding != sha256::Hash::all_zeros(),
				"urgent source binding missing"
			);
			let raw: Vec<Vec<u8>> = serde_json::from_str(bytes)?;
			let txs = raw
				.iter()
				.map(|bytes| bitcoin::consensus::deserialize(bytes))
				.collect::<Result<Vec<Transaction>, _>>()?;
			ensure!(
				line == format!("p:{index}:{binding}:{}", encode_package(&txs)?),
				"noncanonical urgent signed binding"
			);
			check_shared_package_bytes(&urgent, &txs)?;
			let reservation = urgent
				.get_mut(&index)
				.context("signed urgent attempt has no prior reservation")?;
			ensure!(reservation.signed.is_none(), "duplicate urgent binding");
			reservation.signed = Some((binding, txs));
		} else {
			let amount = line
				.parse::<u64>()
				.context("invalid ordinary fee reservation")?;
			ensure!(
				line == amount.to_string(),
				"noncanonical ordinary fee reservation"
			);
			charged = checked_charge(charged, amount, policy)?;
			attempts = attempts
				.checked_add(1)
				.context("ordinary attempt count overflow")?;
		}
	}
	require_urgent_headroom(charged, urgent_charged, policy)?;
	Ok(ParsedHistory {
		charged,
		attempts,
		urgent_charged,
		urgent,
		funding,
		funding_charged,
		resolutions,
	})
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
		assert_eq!(budget.urgent_reserve().unwrap(), Amount::from_sat(15_000));
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
		budget.bind_urgent_signed(bound, &[tx.clone()], Amount::from_sat(500), sha256::Hash::hash(b"unit source binding")).unwrap();
		let bytes = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		budget.bind_urgent_signed(bound, &[tx.clone()], Amount::from_sat(500), sha256::Hash::hash(b"unit source binding")).unwrap();
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), bytes);
		assert!(budget.bind_urgent_signed(bound, &[urgent_tx(9)], Amount::from_sat(500), sha256::Hash::hash(b"unit source binding")).is_err());
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert!(budget.reserve_urgent_ceiling(Amount::from_sat(1)).is_err());
		assert_eq!(budget.retained_urgent().unwrap(), vec![(bound, sha256::Hash::hash(b"unit source binding"), vec![tx])]);
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
		assert!(budget.bind_urgent_signed(first, &[tx.clone()], Amount::from_sat(1_001), sha256::Hash::hash(b"unit source binding")).is_err());
		budget.bind_urgent_signed(first, &[tx.clone()], Amount::from_sat(1_000), sha256::Hash::hash(b"unit source binding")).unwrap();
		assert!(budget.bind_urgent_signed(second, &[tx], Amount::from_sat(1_000), sha256::Hash::hash(b"unit source binding")).is_err());
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


	#[test]
	fn urgent_shared_parent_keeps_every_version_and_cannot_change_witness() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let parent = urgent_tx(20); let first_child = urgent_tx(21); let second_child = urgent_tx(22);
		let first = budget.reserve_urgent_ceiling(Amount::from_sat(5_000)).unwrap();
		budget.bind_urgent_signed(first, &[parent.clone(),first_child], Amount::from_sat(500), sha256::Hash::hash(b"first source")).unwrap();
		let second = budget.reserve_urgent_ceiling(Amount::from_sat(5_000)).unwrap();
		assert_eq!(budget.urgent_reserve().unwrap(), Amount::from_sat(5_000));
		let mut changed = parent.clone(); changed.input[0].witness = bitcoin::Witness::from_slice(&[[2_u8;64]]);
		assert!(budget.bind_urgent_signed(second, &[changed,second_child.clone()], Amount::from_sat(500), sha256::Hash::hash(b"second source")).is_err());
		budget.bind_urgent_signed(second, &[parent,second_child], Amount::from_sat(500), sha256::Hash::hash(b"second source")).unwrap();
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert_eq!(budget.retained_urgent().unwrap().len(),2);
		assert_eq!(budget.urgent_reserve().unwrap(), Amount::from_sat(5_000));
		drop(budget); std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_legacy_absence_refuses_retained_metadata_and_partial_installation() {
		let dir = directory();
		assert!(!urgent_budget_required(&dir, 0).unwrap());
		assert!(urgent_budget_required(&dir, 1).is_err());
		std::fs::write(dir.join(POLICY_FILE), b"{}").unwrap();
		assert!(urgent_budget_required(&dir, 0).unwrap());
		assert!(urgent_snapshot(&dir).is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_legacy_absence_does_not_follow_dangling_or_unreadable_paths() {
		use std::os::unix::fs::{symlink, PermissionsExt};
		let dir = directory();
		symlink(dir.join("absent"), dir.join(BUDGET_FILE)).unwrap();
		assert!(urgent_budget_required(&dir, 0).is_err());
		std::fs::remove_file(dir.join(BUDGET_FILE)).unwrap();
		let (policy, _) = adopted(&dir);
		std::fs::set_permissions(dir.join(BUDGET_FILE), std::fs::Permissions::from_mode(0)).unwrap();
		assert!(require_installed_policy(&dir, policy).is_err());
		std::fs::set_permissions(dir.join(BUDGET_FILE), std::fs::Permissions::from_mode(0o600)).unwrap();
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn ordinary_cap_readonly_admission_requires_actual_immutable_policy() {
		let dir = directory(); let (policy, _) = adopted(&dir);
		assert!(urgent_budget_required(&dir, 0).unwrap());
		require_installed_policy(&dir, policy).unwrap();
		assert!(require_installed_policy(&dir, OrdinaryFeePolicy { ordinary_budget_sat: 60_001, ..policy }).is_err());
		std::fs::remove_file(dir.join(OPENING_FILE)).unwrap();
		assert!(require_installed_policy(&dir, policy).is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}


	#[test]
	fn early_funding_same_retry_free_later_round_full_fee_and_signed_family_park() {
		use super::super::ordinary_funding::test_candidate;
		let dir = directory();
		let (policy, history) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let rate = bitcoin::FeeRate::from_sat_per_vb(100).unwrap();
		let (first, _) = test_candidate(0, 500);
		let permit = budget.reserve_funding(first.clone(), rate).unwrap();
		let bytes = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		assert_eq!(budget.reserve_funding(first.clone(), rate).unwrap(), permit);
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), bytes);
		let (mut changed, _) = test_candidate(0, 501);
		assert!(budget.reserve_funding(changed.clone(), rate).is_err());
		changed.generation = 1;
		let permit2 = budget.reserve_funding(changed, rate).unwrap();
		assert_eq!(permit2.index, permit.index + 1);
		let (second, signed) = test_candidate(2, 600);
		let permit3 = budget.reserve_funding(second, rate).unwrap();
		budget.bind_funding(&permit3, &signed).unwrap();
		let bound = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		budget.bind_funding(&permit3, &signed).unwrap();
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), bound);
		assert!(
			budget
				.reserve_funding(test_candidate(3, 700).0, rate)
				.is_err()
		);
		assert!(
			budget
				.require_retained_fees(
					Amount::from_sat(4_986),
					Amount::from_sat(4_985),
					history.len() as u64
				)
				.is_err(),
			"Round funding fees cannot cover a missing direct debit"
		);
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert_eq!(budget.funding_states().unwrap().len(), 3);
		assert!(
			budget
				.reserve_funding(test_candidate(3, 700).0, rate)
				.is_err()
		);
		drop(budget);
		std::fs::remove_dir_all(dir).unwrap();
	}
	#[test]
	fn early_native_exhaustion_and_unknown_never_refill_or_authorize() {
		use super::super::ordinary_funding::test_candidate;
		let dir = directory();
		let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let rate = bitcoin::FeeRate::from_sat_per_vb(100).unwrap();
		budget.reserve_ceiling(Amount::from_sat(19_136)).unwrap();
		let before = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		assert!(
			budget
				.reserve_funding(test_candidate(0, 500).0, rate)
				.is_err()
		);
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), before);
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		assert!(
			budget
				.reserve_funding(test_candidate(0, 500).0, rate)
				.is_err()
		);
		budget.mark_native_uncertain().unwrap();
		assert!(
			budget
				.reserve_funding(test_candidate(1, 500).0, rate)
				.is_err()
		);
		budget
			.reserve_urgent_ceiling(Amount::from_sat(1000))
			.unwrap();
		drop(budget);
		std::fs::remove_dir_all(dir).unwrap();
	}
	#[test]
	fn native_replay_cannot_bind_a_superseded_unsigned_round_or_foreign_policy() {
		use super::super::ordinary_funding::test_candidate;
		let dir = directory();
		let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let (first, signed) = test_candidate(0, 500);
		let (next, _) = test_candidate(1, 600);
		let rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
		let old = budget.reserve_funding(first, rate).unwrap();
		budget.reserve_funding(next, rate).unwrap();
		assert!(budget.bind_funding(&old, &signed).is_err());
		drop(budget);
		let valid = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		let bad = format!(
			"g:{}:{}\n",
			old.index,
			serde_json::to_string(&bitcoin::consensus::serialize(&signed)).unwrap()
		);
		let mut corrupted = valid.clone();
		corrupted.extend(bad.as_bytes());
		std::fs::write(dir.join(BUDGET_FILE), corrupted).unwrap();
		assert!(
			OrdinaryFeeBudget::load(&dir, policy).is_err(),
			"restart must retain newer-family refusal"
		);
		std::fs::write(dir.join(BUDGET_FILE), &valid).unwrap();
		let mut lines = String::from_utf8(valid)
			.unwrap()
			.lines()
			.map(str::to_owned)
			.collect::<Vec<_>>();
		let mut reservation: FundingReservation =
			serde_json::from_str(lines[0].strip_prefix("f:").unwrap()).unwrap();
		reservation.policy_sha256 = sha256::Hash::hash(b"foreign policy");
		lines[0] = format!("f:{}", serde_json::to_string(&reservation).unwrap());
		std::fs::write(dir.join(BUDGET_FILE), lines.join("\n") + "\n").unwrap();
		assert!(
			urgent_snapshot(&dir).is_err(),
			"read-only startup cannot adopt foreign native policy"
		);
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn native_caller_reads_only_the_real_nested_direct_witness_installation() {
		let root = directory();
		let policy = OrdinaryFeePolicy::default();
		policy
			.initialize_fresh(&root, Network::Regtest, DUMMY_SERVER_KEY.public_key())
			.unwrap();
		assert!(
			installed_identity(&root).is_err(),
			"top-level lookalike files are not the installed witness"
		);
		let nested = root.join(crate::payout_witness::DIRECT_DIRECTORY);
		std::fs::create_dir(&nested).unwrap();
		policy
			.initialize_fresh(&nested, Network::Regtest, DUMMY_SERVER_KEY.public_key())
			.unwrap();
		let (identity, opening) = installed_identity(&root).unwrap();
		assert_eq!(
			identity,
			sha256::Hash::hash(&std::fs::read(nested.join(POLICY_FILE)).unwrap())
		);
		assert_eq!(
			opening,
			sha256::Hash::hash(&std::fs::read(nested.join(OPENING_FILE)).unwrap())
		);
		std::fs::remove_dir_all(root).unwrap();
	}
	#[test]
	fn native_budget_exhaustion_has_no_write_and_keeps_old_exact_retry_usable() {
		use super::super::ordinary_funding::test_candidate;
		let dir = directory();
		let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let (candidate, _) = test_candidate(0, 500);
		let rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
		let original = budget.reserve_funding(candidate.clone(), rate).unwrap();
		budget.reserve_ceiling(Amount::from_sat(18_636)).unwrap();
		let before = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		let (next, _) = test_candidate(1, 501);
		assert!(budget.reserve_funding(next, rate).is_err());
		assert_eq!(budget.reserve_funding(candidate, rate).unwrap(), original);
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), before);
		drop(budget);
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn native_failed_actual_append_parks_even_an_old_exact_retry() {
		use super::super::ordinary_funding::test_candidate;
		let dir = directory();
		let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let (first, _) = test_candidate(0, 500);
		let rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
		budget.reserve_funding(first.clone(), rate).unwrap();
		let before = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		// Local IO control: the actual append call fails on a read-only FD.
		budget.ledger.lock().unwrap().file = File::open(dir.join(BUDGET_FILE)).unwrap();
		let (next, _) = test_candidate(1, 500);
		assert!(budget.reserve_funding(next, rate).is_err());
		assert!(
			budget.reserve_funding(first, rate).is_err(),
			"IO uncertainty cannot masquerade as definitive no-write refusal"
		);
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), before);
		drop(budget);
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn native_unused_extra_is_released_only_by_original_signed_round_winner_proof_without_refund() {
		use super::super::ordinary_funding::{FundingFamily, FundingPurpose, test_candidate};
		let dir = directory();
		let (policy, _) = adopted(&dir);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		let rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
		let (mut old, _) = test_candidate(0, 500);
		let mut tx = old.transaction().unwrap();
		let extra = bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([8; 32]), 0);
		let mut input = tx.input[0].clone();
		input.previous_output = extra;
		tx.input.push(input);
		old.prevouts.push(old.prevouts[0].clone());
		tx.output[0].value += Amount::from_sat(10000);
		old.unsigned = bitcoin::consensus::serialize(&tx);
		let unsigned = budget.reserve_funding(old, rate).unwrap();
		let (winner, signed) = test_candidate(1, 600);
		let winning = budget.reserve_funding(winner, rate).unwrap();
		let (mut pool, _) = test_candidate(0, 500);
		let mut tx = pool.transaction().unwrap();
		tx.input[0].previous_output = extra;
		pool.unsigned = bitcoin::consensus::serialize(&tx);
		pool.purpose = FundingPurpose::Pool;
		pool.family = FundingFamily::Pool {
			funding_txid: tx.compute_txid(),
		};
		assert!(
			budget.reserve_funding(pool.clone(), rate).is_err(),
			"reservation or mempool is not a confirmed owning-wallet winner"
		);
		let mut proof = FundingResolution {
			winner_index: unsigned.index,
			block_height: 20,
			block_hash: bitcoin::BlockHash::from_byte_array([4; 32]),
		};
		assert!(
			budget.resolve_funding(proof.clone()).is_err(),
			"unsigned attempt cannot authorize release"
		);
		budget.bind_funding(&winning, &signed).unwrap();
		proof.winner_index = winning.index;
		let mut absent = proof.clone();
		absent.block_height = 0;
		assert!(budget.resolve_funding(absent).is_err());
		// Kernel-only source witness. Actual Core/BDK producer qualification is separate.
		budget.resolve_funding(proof.clone()).unwrap();
		let before = std::fs::read(dir.join(BUDGET_FILE)).unwrap();
		budget.resolve_funding(proof).unwrap();
		assert_eq!(std::fs::read(dir.join(BUDGET_FILE)).unwrap(), before);
		let (next, _) = test_candidate(2, 700);
		assert!(
			budget.reserve_funding(next, rate).is_err(),
			"confirmed family remains closed"
		);
		budget.reserve_funding(pool.clone(), rate).unwrap();
		assert_eq!(
			budget.ledger.lock().unwrap().funding_charged,
			1600,
			"release refunds no prior fee"
		);
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&dir, policy).unwrap();
		budget.reserve_funding(pool, rate).unwrap();
		assert_eq!(budget.ledger.lock().unwrap().funding_charged, 1600);
		drop(budget);
		std::fs::remove_dir_all(dir).unwrap();
	}

}

/// Stable read-only identities for the authenticated caller. No second FLOCK owner.
pub(crate) fn installed_identity(datadir: &Path) -> anyhow::Result<(sha256::Hash, sha256::Hash)> {
	let directory = datadir.join(crate::payout_witness::DIRECT_DIRECTORY);
	let raw = read_bounded(&directory.join(POLICY_FILE), 2048)?;
	let stored: StoredPolicy = serde_json::from_slice(&raw)?;
	ensure!(
		serde_json::to_vec(&stored)? == raw,
		"noncanonical native funding policy"
	);
	require_installed_policy(&directory, stored.policy)?;
	Ok((sha256::Hash::hash(&raw), stored.opening_sha256))
}

pub(crate) fn funding_snapshot(
	directory: &Path,
) -> anyhow::Result<(Vec<FundingState>, Vec<FundingResolution>)> {
	let policy_raw = read_bounded(&directory.join(POLICY_FILE), 2048)?;
	let stored: StoredPolicy = serde_json::from_slice(&policy_raw)?;
	ensure!(
		serde_json::to_vec(&stored)? == policy_raw,
		"native funding policy is not canonical"
	);
	let opening_raw = read_bounded(&directory.join(OPENING_FILE), 2048)?;
	ensure!(
		sha256::Hash::hash(&opening_raw) == stored.opening_sha256,
		"native funding opening differs"
	);
	let opening: OrdinaryFeeOpening = serde_json::from_slice(&opening_raw)?;
	opening.validate(stored.policy)?;
	let path = directory.join(BUDGET_FILE);
	let before = std::fs::metadata(&path)?;
	let raw = read_bounded(&path, MAX_HISTORY_BYTES)?;
	let after = std::fs::metadata(&path)?;
	ensure!(
		before.len() == after.len()
			&& before.modified()? == after.modified()?
			&& raw.len() as u64 == after.len(),
		"native fee history changed at startup"
	);
	let history = parse_history(
		std::str::from_utf8(&raw)?,
		opening.initial_charge(stored.policy)?,
		stored.policy,
	)?;
	ensure!(
		history
			.funding
			.values()
			.all(|s| s.reservation.policy_sha256 == sha256::Hash::hash(&policy_raw)),
		"native funding policy identity differs"
	);
	Ok((
		history.funding.into_values().collect(),
		history.resolutions.into_values().collect(),
	))
}

pub(crate) fn validate_resolution(
	resolution: &FundingResolution,
	states: &BTreeMap<sha256::Hash, FundingState>,
) -> anyhow::Result<()> {
	ensure!(
		resolution.block_height > 0 && resolution.block_hash != bitcoin::BlockHash::all_zeros(),
		"native funding resolution lacks actual confirmed block"
	);
	let winner = states
		.values()
		.find(|s| s.reservation.index == resolution.winner_index)
		.context("native resolution winner was never reserved")?;
	ensure!(
		matches!(
			winner.reservation.candidate.family,
			super::ordinary_funding::FundingFamily::Round { .. }
		),
		"only actual Round retry family may release extras"
	);
	winner.reservation.candidate.verify_signed(
		winner
			.signed
			.as_ref()
			.context("unsigned or mempool intent cannot authorize input release")?,
	)?;
	Ok(())
}
pub(crate) fn resolved_family(
	old: &FundingState,
	states: &BTreeMap<sha256::Hash, FundingState>,
	resolutions: &BTreeMap<u64, FundingResolution>,
) -> bool {
	resolutions.values().any(|r| {
		states.values().any(|winner| {
			winner.reservation.index == r.winner_index
				&& winner.signed.is_some()
				&& winner.reservation.candidate.family == old.reservation.candidate.family
		})
	})
}
