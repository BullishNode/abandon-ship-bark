//! A lifetime ordinary-fee allowance, independent of the carrier PostgreSQL journal.
//! Reservations deliberately charge the complete fee ceiling, even if construction
//! fails. Neither confirmation, a new group id nor a restart replenishes it.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;

use anyhow::Context;
use bitcoin::{Amount, Network, Transaction, Wtxid};
use bitcoin::hashes::{sha256, Hash};
use crate::bitcoind::VerifiedFeeChain;

use crate::fs_perms;
use super::ordinary_fee_publication::{self, PublicationEntry, PublicationRecord};
use super::urgent_publication::{BoundUrgentPublication, UrgentCandidate, UrgentReservationEntry, UrgentSignedEntry};

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
	carrier_charged: u64,
	publications: BTreeMap<Wtxid, PublicationEntry>,
	urgent: BTreeMap<u64, UrgentReservationEntry>,
	urgent_signed: BTreeMap<u64, UrgentSignedEntry>,
	length: u64,
	// An uncertain witness/append must poison this owner, including carrier
	// signing. Restart must reconcile the original bytes, never reset history.
	uncertain: bool,
	requires_epoch: bool,
	active_epoch: Option<sha256::Hash>,
	active_identity: Option<super::ordinary_fee_auth::FeeServiceIdentity>,
	// Runtime protection is derived from the actual enforced urgent ceiling.
	// It does not change stored allowance/history or create spend authority.
	urgent_headroom: Option<(u64, u32)>,
}

pub(crate) struct OrdinaryFeeBudget {
	directory: PathBuf,
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
		ensure!(history.len() as u64 == length, "ordinary fee history changed during bounded read");
		let parsed = parse_joint_history(&history, policy)?;
		validate_joint_witnesses(directory, policy, &parsed.publications, &parsed.urgent, &parsed.urgent_signed)?;
		Ok(Self { directory: directory.into(), policy, ledger: Mutex::new(Ledger {
			file, charged: parsed.charged, carrier_charged: parsed.carrier_charged,
			publications: parsed.publications, urgent: parsed.urgent, urgent_signed: parsed.urgent_signed, length, uncertain: false, requires_epoch: false, active_epoch: None, active_identity: None, urgent_headroom: None,
		}) })
	}

	/// Every retained version is charged conservatively, including conflicts.
	pub(crate) fn require_retained_fees(&self, retained: Amount) -> anyhow::Result<()> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length,
			"ordinary fee history uncertain or changed; reconcile original history");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		ensure!(retained.to_sat() <= ledger.carrier_charged,
			"ordinary carrier reservations do not cover retained signed carrier history");
		Ok(())
	}

	pub(crate) fn urgent_reserve(&self) -> Amount { Amount::from_sat(self.policy.urgent_reserve_sat) }

	/// Actual DIRECT startup requires an original reconciled owner epoch before
	/// any carrier/native debit, including setup/read-only captaind configurations.
	pub(crate) fn require_native_epoch(&self) -> anyhow::Result<()> {
		self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?.requires_epoch = true;
		Ok(())
	}
	/// One claim and one progression can represent distinct required exposures.
	/// Their FULL configured ceilings remain inside this SAME native allowance,
	/// independently of the physical urgent wallet floor. No unused debit is
	/// refunded. More campaign exposures still require an independently priced
	/// complete source; this minimum is not a full-cohort coverage certificate.
	pub(crate) fn protect_urgent_attempts(&self, ceiling: Amount) -> anyhow::Result<()> {
		const REQUIRED_ATTEMPTS: u32 = 2;
		self.policy.validate_ceiling(ceiling)?;
		let headroom = ceiling.to_sat().checked_mul(u64::from(REQUIRED_ATTEMPTS)).context("urgent headroom overflow")?;
		ensure!(headroom <= self.policy.ordinary_budget_sat,
			"native allowance cannot cover one full claim and one full progression attempt");
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		let protection = (ceiling.to_sat(), REQUIRED_ATTEMPTS);
		ensure!(ledger.urgent_headroom.is_none_or(|old| old == protection), "active urgent protection cannot be changed");
		ledger.urgent_headroom = Some(protection);
		Ok(())
	}
	fn require_discretionary_headroom(&self, ledger: &Ledger, next_charge: u64) -> anyhow::Result<()> {
		ensure!(ledger.urgent.is_empty(), "retained urgent inputs require exact DB/package reconciliation before new discretionary construction");
		ensure!(!ledger.requires_epoch || ledger.urgent_headroom.is_some(), "actual DIRECT urgent fee headroom is not configured");
		if let Some((ceiling, attempts)) = ledger.urgent_headroom {
			let headroom = ceiling.checked_mul(u64::from(attempts)).context("urgent headroom overflow")?;
			ensure!(next_charge.checked_add(headroom).is_some_and(|total| total <= self.policy.ordinary_budget_sat),
				"discretionary publication would consume protected SAME-ledger urgent fee authority");
		}
		Ok(())
	}
	pub(crate) fn actual_owner_identity(&self) -> anyhow::Result<super::ordinary_fee_auth::FeeServiceIdentity> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length && ledger.urgent_headroom.is_some(),
			"urgent actual owner history or configured headroom missing");
		let identity = ledger.active_identity.as_ref().context("urgent actual original opening is not activated")?;
		ensure!(ledger.active_epoch == Some(identity.opening_sha256), "urgent original owner epoch differs");
		Ok(identity.clone())
	}
	pub(crate) fn require_active_owner(&self) -> anyhow::Result<()> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && (!ledger.requires_epoch || (ledger.active_epoch.is_some() && ledger.urgent_headroom.is_some())),
			"direct fee owner lacks its actual reconciled native epoch");
		Ok(())
	}
	pub(crate) fn bind_native_epoch(&self, epoch: &super::ordinary_fee_epoch::NativeFeeEpoch) -> anyhow::Result<()> {
		ensure!(epoch.identity().policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&self.policy)?),
			"native epoch policy differs from existing owner allowance");
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.active_epoch.is_none_or(|old| old == epoch.identity().opening_sha256),
			"native owner epoch changed or history remains uncertain");
		ledger.active_epoch = Some(epoch.identity().opening_sha256);
		ledger.active_identity = Some(epoch.identity().clone());
		Ok(())
	}

	/// Synchronous durable reservation BEFORE any backing or fee-wallet signature.
	/// An uncertain fsync/append leaves length mismatched and prevents retry signing.
	pub(crate) fn reserve_ceiling(&self, ceiling: Amount) -> anyhow::Result<()> {
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length,
			"ordinary fee history uncertain or changed; restart and reconcile");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		ensure!(!ledger.requires_epoch || ledger.active_epoch.is_some(), "direct carrier construction requires native fee activation");
		let charged = checked_charge(ledger.charged, ceiling.to_sat(), self.policy)?;
		self.require_discretionary_headroom(&ledger, charged)?;
		let entry = format!("{}\n", ceiling.to_sat());
		ensure!(ledger.length + entry.len() as u64 <= MAX_HISTORY_BYTES, "ordinary fee history capacity exhausted");
		ledger.uncertain = true;
		ledger.file.write_all(entry.as_bytes())?;
		ledger.file.sync_all()?;
		ledger.length += entry.len() as u64;
		ledger.charged = charged;
		ledger.carrier_charged = ledger.carrier_charged.checked_add(ceiling.to_sat())
			.context("carrier reservation sum overflow")?;
		ledger.uncertain = false;
		Ok(())
	}
	/// The actual carrier caller retains its Watchman wallet guard. The SAME
	/// independent full-C debit now serializes with final publication fences.
	/// Cancellation/unknown COMMIT poisons the owner even when fsync completed;
	/// no physical signature follows a missing acknowledgement.
	pub(crate) async fn reserve_carrier_ceiling(&self, db: &crate::database::Db, ceiling: Amount) -> anyhow::Result<()> {
		struct Pending<'a> { owner: &'a OrdinaryFeeBudget, written: std::sync::atomic::AtomicBool, acknowledged: bool }
		impl Drop for Pending<'_> {
			fn drop(&mut self) {
				if self.written.load(std::sync::atomic::Ordering::SeqCst) && !self.acknowledged { let _ = self.owner.mark_uncertain(); }
			}
		}
		let mut pending = Pending { owner: self, written: std::sync::atomic::AtomicBool::new(false), acknowledged: false };
		db.write(async |t| {
			t.lock_native_fee_publication().await?;
			self.reserve_ceiling(ceiling)?;
			pending.written.store(true, std::sync::atomic::Ordering::SeqCst);
			Ok(())
		}).await?;
		pending.acknowledged = true;
		Ok(())
	}
	pub(crate) fn current_source_journal(&self) -> anyhow::Result<super::ordinary_fee_source::JournalStamp> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.urgent.is_empty() && ledger.active_epoch.is_some() && ledger.file.metadata()?.len() == ledger.length,
			"native final source journal is uncertain or lacks active epoch");
		let owner = std::os::unix::fs::MetadataExt::uid(&std::fs::symlink_metadata(&self.directory)?);
		super::ordinary_fee_source::JournalStamp::read(&self.directory.join(BUDGET_FILE), owner)
	}

	/// Accounting primitive for the SAME owning Watchman process. The broker
	/// must first hold its actual wallet guard and verify physical urgent funds.
	/// This method alone does not authorize signing, publication or a cached R/F token.
	pub(crate) fn reserve_publication(&self, record: &PublicationRecord) -> anyhow::Result<()> {
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.requires_epoch || ledger.active_epoch.is_some(), "native publication requires actual fee activation");
		self.reserve_publication_locked(&mut ledger, record, false)
	}
	fn reserve_publication_locked(&self, ledger: &mut Ledger, record: &PublicationRecord, validated_historical_import: bool) -> anyhow::Result<()> {
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length,
			"ordinary fee history uncertain or changed; reconcile original reservation");
		let entry = record.entry(self.policy)?;
		if let Some(old) = ledger.publications.get(&entry.id()) {
			ensure!(*old == entry, "same signed ordinary publication has conflicting reservation");
			validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
			return Ok(());
		}
		let charged = checked_charge(ledger.charged, entry.charge(self.policy)?, self.policy)?;
		if !validated_historical_import { self.require_discretionary_headroom(ledger, charged)?; }
		let line = entry.line()?;
		ensure!(ledger.length + line.len() as u64 <= MAX_HISTORY_BYTES,
			"ordinary fee history capacity exhausted");
		// The immutable intent precedes the indexed charge. A crash in between
		// leaves an unindexed original witness and blocks the next native loader.
		ledger.uncertain = true;
		let actual = ordinary_fee_publication::write_witness_with_urgent(&self.directory, self.policy,
			&ledger.publications, &urgent_candidate_hashes(&ledger.urgent), &urgent_signed_hashes(&ledger.urgent_signed), record)?;
		ensure!(actual == entry, "ordinary publication witness differs");
		ledger.file.write_all(line.as_bytes())?;
		ledger.file.sync_all()?;
		ledger.length += line.len() as u64;
		ledger.charged = charged;
		ledger.publications.insert(entry.id(), entry);
		ledger.uncertain = false;
		Ok(())
	}

	pub(crate) fn next_urgent_attempt_index(&self) -> anyhow::Result<u64> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length, "urgent fee source is uncertain");
		u64::try_from(ledger.urgent.len())?.checked_add(1).context("urgent attempt index overflow")
	}
	/// SAME-ledger full-C accounting primitive. The actual caller MUST first
	/// hold Watchman then financial SQL4 plus source guards, and retain every
	/// original input on a lost acknowledgement. This method is NOT an urgent
	/// signing/relay capability; those entry points remain explicitly held.
	/// Used only while the actual owning wallet and Native4 are held. The
	/// reservation itself still checks this sequence and all retained bytes.
	pub(crate) fn next_urgent_attempt(&self) -> anyhow::Result<u64> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length
			&& ledger.active_identity.is_some() && ledger.urgent_headroom.is_some(),
			"urgent attempt lacks actual active owner or exact original history");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		u64::try_from(ledger.urgent.len())?.checked_add(1).context("urgent attempt index overflow")
	}
	pub(crate) fn reserve_urgent_candidate(&self, candidate: &UrgentCandidate) -> anyhow::Result<()> {
		let entry = candidate.reservation_entry(self.policy)?;
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length
			&& (!ledger.requires_epoch || ledger.active_epoch == Some(entry.opening_sha256)),
			"urgent original owner/epoch or history differs");
		if ledger.requires_epoch {
			entry.require_identity(ledger.active_identity.as_ref().context("urgent actual activated owner identity missing")?)?;
		}
		let (ceiling, _) = ledger.urgent_headroom.context("actual inherited urgent full-ceiling policy missing")?;
		ensure!(entry.ceiling_sat == ceiling && entry.attempt_index == u64::try_from(ledger.urgent.len())?.checked_add(1).context("urgent attempt index overflow")?
			&& ledger.urgent.values().all(|old| old.publication_txid != entry.publication_txid),
			"urgent attempt already reserved or differs from actual full configured ceiling; retain original, never re-sign");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		let charged = checked_charge(ledger.charged, entry.ceiling_sat, self.policy)?;
		let line = entry.line(self.policy)?;
		ensure!(ledger.length.checked_add(line.len() as u64).is_some_and(|length| length <= MAX_HISTORY_BYTES), "urgent pre-sign history capacity exhausted");
		ledger.uncertain = true;
		ordinary_fee_publication::write_urgent_candidate(&self.directory, self.policy, &ledger.publications,
			&urgent_candidate_hashes(&ledger.urgent), &urgent_signed_hashes(&ledger.urgent_signed), &entry, &serde_json::to_vec(candidate)?)?;
		ledger.file.write_all(line.as_bytes())?; ledger.file.sync_all()?;
		ledger.length += line.len() as u64; ledger.charged = charged;
		ledger.urgent.insert(entry.attempt_index, entry); ledger.uncertain = false;
		Ok(())
	}
	/// Bind private original signed bytes to their already-paid full ceiling.
	/// No new debit, permission to re-sign, source eligibility or nursery relay
	/// follows. Actual source locks and reciprocal DB publication are still
	/// required by the later owner hook; that path remains held in this child.
	pub(crate) fn bind_urgent_signed(&self, publication: &BoundUrgentPublication,
		limits: super::carrier::CarrierLimits,
	) -> anyhow::Result<()> {
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length, "urgent signed binding history is uncertain");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		let original = ledger.urgent.get(&publication.attempt_index()).context("urgent signed package lacks original paid attempt")?.clone();
		let candidate = UrgentCandidate::decode_reserved(&ordinary_fee_publication::read_urgent_candidate(
			&self.directory, original.attempt_index)?, &original, self.policy)?;
		let identity = match &ledger.active_identity {
			Some(identity) => identity.clone(),
			None => { ensure!(!ledger.requires_epoch, "urgent actual activated owner missing"); candidate.identity() },
		};
		let (matched, signed) = publication.signed_entry(&identity, limits, self.policy)?;
		ensure!(matched == original, "urgent signed package differs from original pre-sign full ceiling");
		if let Some(existing) = ledger.urgent_signed.get(&original.attempt_index) {
			ensure!(*existing == signed, "urgent original signed binding differs; retain original and do not re-sign");
			return Ok(());
		}
		let line = signed.line(&original)?;
		ensure!(ledger.length.checked_add(line.len() as u64).is_some_and(|length| length <= MAX_HISTORY_BYTES), "urgent signed history capacity exhausted");
		ledger.uncertain = true;
		ordinary_fee_publication::write_urgent_signed(&self.directory, self.policy, &ledger.publications,
			&urgent_candidate_hashes(&ledger.urgent), &urgent_signed_hashes(&ledger.urgent_signed), &signed, &serde_json::to_vec(publication)?)?;
		ledger.file.write_all(line.as_bytes())?; ledger.file.sync_all()?;
		ledger.length += line.len() as u64;
		ledger.urgent_signed.insert(original.attempt_index, signed); ledger.uncertain = false;
		Ok(())
	}

	/// Read-only paid-original checks for SQL retention. These expose no signing
	/// or relay capability; the actual source locks must surround the caller.
	pub(crate) fn require_urgent_candidate(&self, candidate: &UrgentCandidate) -> anyhow::Result<()> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length, "urgent original history uncertain");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		let expected = candidate.reservation_entry(self.policy)?;
		ensure!(ledger.urgent.get(&expected.attempt_index) == Some(&expected), "urgent SQL intent lacks original paid candidate");
		if ledger.requires_epoch {
			expected.require_identity(ledger.active_identity.as_ref().context("urgent actual activated owner missing")?)?;
		}
		Ok(())
	}
	pub(crate) fn retained_urgent_publications(&self) -> anyhow::Result<Vec<(UrgentCandidate, Option<BoundUrgentPublication>)>> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length, "urgent retained history uncertain");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		ledger.urgent.values().map(|entry| {
			let candidate = UrgentCandidate::decode_reserved(&ordinary_fee_publication::read_urgent_candidate(
				&self.directory, entry.attempt_index)?, entry, self.policy)?;
			let signed = ledger.urgent_signed.get(&entry.attempt_index).map(|signed| BoundUrgentPublication::decode(
				&ordinary_fee_publication::read_urgent_signed(&self.directory, entry.attempt_index)?, signed.record_sha256,
				&candidate.identity(), candidate.limits(), self.policy)).transpose()?;
			Ok((candidate, signed))
		}).collect()
	}
	pub(crate) fn require_urgent_signed(&self, publication: &BoundUrgentPublication) -> anyhow::Result<()> {
		let retained = self.retained_urgent_publications()?;
		ensure!(retained.iter().any(|(_, signed)| signed.as_ref() == Some(publication)),
			"urgent SQL signed binding lacks exact original retained u2 record");
		Ok(())
	}

	pub(crate) fn reserved_urgent_candidates(&self) -> anyhow::Result<Vec<UrgentCandidate>> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length, "urgent original history is uncertain");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		ledger.urgent.values().map(|entry| UrgentCandidate::decode_reserved(
			&ordinary_fee_publication::read_urgent_candidate(&self.directory, entry.attempt_index)?, entry, self.policy)).collect()
	}
	/// The DB/nursery signed-package join is not implemented in this partial
	/// child. Do not activate a restored owner by ignoring typed urgent intents.
	pub(crate) fn require_no_unreconciled_urgent(&self) -> anyhow::Result<()> {
		ensure!(self.reserved_urgent_candidates()?.is_empty(), "retained urgent pre-sign inputs require exact DB/package reconciliation before activation");
		Ok(())
	}

	pub(crate) fn mark_uncertain(&self) -> anyhow::Result<()> {
		self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?.uncertain = true;
		Ok(())
	}
	pub(crate) fn policy(&self) -> OrdinaryFeePolicy { self.policy }
	pub(crate) fn directory(&self) -> &Path { &self.directory }

	/// Paused startup only. Retain the original numeric prefix and serialize
	/// the entire import against every existing ordinary/carrier reservation.
	/// A half-completed import may replay exact originals, never new credit.
	pub(crate) fn import_original(&self, opening: &super::ordinary_fee_opening::CheckedOriginalOpening) -> anyhow::Result<()> {
		let mut expected = opening.original_history().to_vec();
		for record in opening.publications() { expected.extend_from_slice(record.entry(self.policy)?.line()?.as_bytes()); }
		ensure!(expected.len() as u64 <= MAX_HISTORY_BYTES, "original import exceeds native journal bound");
		parse_joint_history(std::str::from_utf8(&expected)?, self.policy)?;
		let mut ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length,
			"original import found uncertain native fee history");
		ledger.file.seek(SeekFrom::Start(0))?;
		let mut actual = Vec::new(); (&mut ledger.file).take(MAX_HISTORY_BYTES + 1).read_to_end(&mut actual)?;
		let parsed = parse_joint_history(std::str::from_utf8(&actual)?, self.policy)?;
		ensure!(parsed.charged == ledger.charged && parsed.carrier_charged == ledger.carrier_charged
			&& parsed.publications == ledger.publications && parsed.urgent == ledger.urgent && parsed.urgent_signed == ledger.urgent_signed, "original import cached ledger differs from exact file");
		ensure!(actual.len() as u64 == ledger.length && actual.starts_with(opening.original_history())
			&& expected.starts_with(&actual), "native fee history is not the exact original import prefix");
		for record in opening.publications() { self.reserve_publication_locked(&mut ledger, record, true)?; }
		ensure!(ledger.length == expected.len() as u64, "original import has an unexpected retained debit");
		Ok(())
	}

	pub(crate) fn publication_records(&self) -> anyhow::Result<BTreeMap<Wtxid, PublicationRecord>> {
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length,
			"native publication history uncertain or changed");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)
	}

	/// Exact original-candidate check for a caller's publication transaction.
	/// It does not grant a reusable balance token and does not free its debit.
	pub(crate) fn require_publication(&self, record: &PublicationRecord, signed: &Transaction) -> anyhow::Result<()> {
		record.matches(signed)?;
		let ledger = self.ledger.lock().map_err(|_| anyhow!("ordinary fee ledger lock poisoned"))?;
		ensure!(!ledger.uncertain && ledger.file.metadata()?.len() == ledger.length,
			"ordinary publication history uncertain or changed");
		let entry = record.entry(self.policy)?;
		ensure!(ledger.publications.get(&entry.id()) == Some(&entry),
			"ordinary publication lacks its exact durable reservation");
		validate_joint_witnesses(&self.directory, self.policy, &ledger.publications, &ledger.urgent, &ledger.urgent_signed)?;
		Ok(())
	}
}

pub(crate) fn opening_numeric_charges(raw: &[u8], policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	let history = std::str::from_utf8(raw)?;
	let parsed = parse_joint_history(history, policy)?;
	ensure!(parsed.publications.is_empty() && parsed.urgent.is_empty(), "original opening contains another native publication epoch");
	Ok(parsed.carrier_charged)
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

fn urgent_candidate_hashes(entries: &BTreeMap<u64, UrgentReservationEntry>) -> BTreeMap<u64, sha256::Hash> {
	entries.iter().map(|(&index, entry)| (index, entry.candidate_sha256)).collect()
}

fn urgent_signed_hashes(entries: &BTreeMap<u64, UrgentSignedEntry>) -> BTreeMap<u64, sha256::Hash> {
	entries.iter().map(|(&index, entry)| (index, entry.record_sha256)).collect()
}
fn validate_joint_witnesses(history: &Path, policy: OrdinaryFeePolicy,
	publications: &BTreeMap<Wtxid, PublicationEntry>, urgent: &BTreeMap<u64, UrgentReservationEntry>,
	urgent_signed: &BTreeMap<u64, UrgentSignedEntry>,
) -> anyhow::Result<BTreeMap<Wtxid, PublicationRecord>> {
	let records = ordinary_fee_publication::validate_witnesses_with_urgent(history, policy, publications,
		&urgent_candidate_hashes(urgent), &urgent_signed_hashes(urgent_signed))?;
	for entry in urgent.values() {
		let candidate = UrgentCandidate::decode_reserved(&ordinary_fee_publication::read_urgent_candidate(history, entry.attempt_index)?, entry, policy)?;
		if let Some(signed) = urgent_signed.get(&entry.attempt_index) {
			let identity = candidate.identity(); let limits = candidate.limits();
			let publication = BoundUrgentPublication::decode(&ordinary_fee_publication::read_urgent_signed(history, entry.attempt_index)?,
				signed.record_sha256, &identity, limits, policy)?;
			let (actual_original, actual_signed) = publication.signed_entry(&identity, limits, policy)?;
			ensure!(actual_original == *entry && actual_signed == *signed, "urgent signed witness differs from original journal binding");
		}
	}
	ensure!(urgent_signed.keys().all(|index| urgent.contains_key(index)), "urgent signed witness has no original paid attempt");
	Ok(records)
}

struct ParsedHistory {
	charged: u64,
	carrier_charged: u64,
	publications: BTreeMap<Wtxid, PublicationEntry>,
	urgent: BTreeMap<u64, UrgentReservationEntry>,
	urgent_signed: BTreeMap<u64, UrgentSignedEntry>,
}
fn parse_joint_history(history: &str, policy: OrdinaryFeePolicy) -> anyhow::Result<ParsedHistory> {
	ensure!(history.is_empty() || history.ends_with('\n'), "ordinary fee history has a torn append");
	let mut parsed = ParsedHistory { charged: 0, carrier_charged: 0, publications: BTreeMap::new(), urgent: BTreeMap::new(), urgent_signed: BTreeMap::new() };
	for line in history.lines() {
		if let Some(bytes) = line.strip_prefix("u2\t") {
			let index = serde_json::from_str::<UrgentSignedEntry>(bytes)?.attempt_index;
			let original = parsed.urgent.get(&index).context("urgent signed binding lacks original pre-sign reservation")?;
			let signed = UrgentSignedEntry::parse(bytes, original)?;
			ensure!(parsed.urgent_signed.insert(index, signed).is_none(), "duplicate urgent signed binding");
			continue; // Original u1 retained the FULL charge; binding never charges twice.
		}
		let amount = if let Some(bytes) = line.strip_prefix("u1\t") {
			let entry = UrgentReservationEntry::parse(bytes, policy)?;
			ensure!(entry.attempt_index == u64::try_from(parsed.urgent.len())?.checked_add(1).context("urgent index overflow")?
				&& parsed.urgent.values().all(|old| old.publication_txid != entry.publication_txid),
				"urgent pre-sign attempt is nonsequential or repeats original publication");
			let amount = entry.ceiling_sat;
			parsed.urgent.insert(entry.attempt_index, entry);
			amount
		} else if let Some(bytes) = line.strip_prefix("p1\t") {
			let entry = PublicationEntry::parse(bytes)?;
			let charge = entry.charge(policy)?;
			ensure!(parsed.publications.insert(entry.id(), entry).is_none(),
				"duplicate ordinary publication reservation");
			charge
		} else {
			let amount = line.parse::<u64>().context("invalid ordinary fee reservation")?;
			ensure!(line == amount.to_string(), "noncanonical ordinary fee reservation");
			parsed.carrier_charged = parsed.carrier_charged.checked_add(amount)
				.context("carrier reservation sum overflow")?;
			amount
		};
		parsed.charged = checked_charge(parsed.charged, amount, policy)?;
	}
	Ok(parsed)
}
/// Read-only complete original snapshot under the caller's NativeFeePublication
/// SQL lock. This never takes Watchman's exclusive file lock or creates an owner.
pub(crate) fn original_publications_locked(directory: &Path, policy: OrdinaryFeePolicy)
	-> anyhow::Result<BTreeMap<Wtxid, PublicationRecord>> {
	let owner = std::fs::symlink_metadata(directory)?.uid();
	let before = super::ordinary_fee_source::JournalStamp::read(&directory.join(BUDGET_FILE), owner)?;
	let raw = ordinary_fee_publication::read_record(&directory.join(BUDGET_FILE), owner)?;
	ensure!(raw.len() as u64 <= MAX_HISTORY_BYTES, "original fee snapshot exceeds journal bound");
	let parsed = parse_joint_history(std::str::from_utf8(&raw)?, policy)?;
	let records = validate_joint_witnesses(directory, policy, &parsed.publications, &parsed.urgent, &parsed.urgent_signed)?;
	ensure!(before == super::ordinary_fee_source::JournalStamp::read(&directory.join(BUDGET_FILE), owner)?,
		"original fee snapshot changed under global lock");
	Ok(records)
}

/// Native arithmetic/binding result ONLY. The protected collector separately
/// proves source ownership, whole fixed file/SQL cut, chain and declaration.
/// This cannot activate an owner or authorize signing/publication/readiness.
pub(crate) struct CapturedJointHistory {
	pub(crate) charged_sat: u64,
	pub(crate) carrier_charged_sat: u64,
	pub(crate) publication_entries: BTreeMap<Wtxid, PublicationEntry>,
	pub(crate) publications: BTreeMap<Wtxid, PublicationRecord>,
	pub(crate) urgent_entries: BTreeMap<u64, UrgentReservationEntry>,
	pub(crate) urgent_signed_entries: BTreeMap<u64, UrgentSignedEntry>,
	pub(crate) urgent: BTreeMap<u64, (UrgentCandidate, Option<BoundUrgentPublication>)>,
}

/// Read-only captured counterpart to the SAME original parser/witness checks.
/// `publications` is ONLY the complete original fixed publication sibling:
/// canonical filename -> exact UTF8 JSON bytes, not rights/header/source flags.
/// No path, owner, allowance, SQL row, or missing witness is created/repaired.
/// Caller independently validates actual network/declaration and source pins.
pub(crate) fn validate_captured_joint_history(history: &str, policy: OrdinaryFeePolicy,
	publications: &BTreeMap<String,String>,
) -> anyhow::Result<CapturedJointHistory> {
	policy.validate_config(if policy.regtest_qualification.is_some() { Network::Regtest } else { Network::Signet })?;
	ensure!(history.len() as u64 <= MAX_HISTORY_BYTES && publications.len() <= 4096,
		"captured joint original source exceeds count or history bound");
	let mut bytes = 0usize;
	for raw in publications.values() {
		ensure!(raw.len() <= 4 * 1024 * 1024, "captured original witness exceeds per-record bound");
		bytes = bytes.checked_add(raw.len()).context("captured original witness size overflow")?;
		ensure!(bytes <= 64 * 1024 * 1024, "captured original witness aggregate exceeds existing bound");
	}
	let parsed = parse_joint_history(history,policy)?;
	let mut used = std::collections::BTreeSet::new(); let mut originals = BTreeMap::new();
	for (id,entry) in &parsed.publications {
		let name = format!("{id}.json"); let raw = publications.get(&name).context("captured original p1 witness absent")?;
		let record: PublicationRecord = serde_json::from_slice(raw.as_bytes())?;
		ensure!(serde_json::to_vec(&record)? == raw.as_bytes() && record.entry(policy)? == *entry,
			"captured p1 original record differs from retained exact native entry");
		used.insert(name); originals.insert(*id,record);
	}
	let mut urgent = BTreeMap::new();
	for (index,entry) in &parsed.urgent {
		let name = format!("urgent-{index}.candidate.json");
		let raw = publications.get(&name).context("captured original u1 candidate absent")?;
		let candidate = UrgentCandidate::decode_reserved(raw.as_bytes(),entry,policy)?; used.insert(name);
		let signed = match parsed.urgent_signed.get(index) {
			None => None,
			Some(signed) => {
				let name = format!("urgent-{index}.signed.json");
				let raw = publications.get(&name).context("captured original u2 signed package absent")?;
				let identity = candidate.identity(); let limits = candidate.limits();
				let publication = BoundUrgentPublication::decode(raw.as_bytes(),signed.record_sha256,&identity,limits,policy)?;
				let (original,actual) = publication.signed_entry(&identity,limits,policy)?;
				ensure!(original == *entry && actual == *signed, "captured urgent original package differs from retained u1/u2");
				used.insert(name); Some(publication)
			},
		};
		urgent.insert(*index,(candidate,signed));
	}
	ensure!(used.len() == publications.len() && publications.keys().all(|name| used.contains(name)),
		"captured witness set contains unknown, unindexed or unrelated original files");
	Ok(CapturedJointHistory { charged_sat:parsed.charged,carrier_charged_sat:parsed.carrier_charged,
		publication_entries:parsed.publications,publications:originals,urgent_entries:parsed.urgent,
		urgent_signed_entries:parsed.urgent_signed,urgent })
}

fn parse_history(history: &str, policy: OrdinaryFeePolicy) -> anyhow::Result<u64> {
	Ok(parse_joint_history(history, policy)?.charged)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::os::unix::fs::PermissionsExt;

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
	fn publication_directory(name: &str) -> std::path::PathBuf {
		let path = std::env::temp_dir().join(format!("c-native-publication-{name}-{}-{}",
			std::process::id(), bitcoin::secp256k1::rand::random::<u64>()));
		std::fs::create_dir(&path).unwrap(); path
	}
	fn remove_publication_directory(path: &Path) {
		let name = path.file_name().unwrap().to_str().unwrap();
		let witnesses = path.with_file_name(format!("{name}.ordinary-native-publications"));
		if witnesses.exists() { std::fs::remove_dir_all(witnesses).unwrap(); }
		std::fs::remove_dir_all(path).unwrap();
	}
	fn publication_transaction(fee: u64, height: u32) -> (Transaction, Transaction) {
		let parent = Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint::null(),
				script_sig: bitcoin::ScriptBuf::new(), sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::default() }],
			output: vec![bitcoin::TxOut { value: Amount::from_sat(10_000), script_pubkey: bitcoin::ScriptBuf::new() }] };
		let tx = Transaction { version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::from_height(height).unwrap(),
			input: vec![bitcoin::TxIn { previous_output: bitcoin::OutPoint::new(parent.compute_txid(), 0),
				script_sig: bitcoin::ScriptBuf::new(), sequence: bitcoin::Sequence::MAX,
				witness: bitcoin::Witness::from_slice(&[vec![0; 64]]) }],
			output: vec![bitcoin::TxOut { value: Amount::from_sat(10_000 - fee), script_pubkey: bitcoin::ScriptBuf::new() }] };
		(parent, tx)
	}
	fn publication_record(policy: OrdinaryFeePolicy, path: super::ordinary_fee_publication::PublicationPath,
		fee: u64, height: u32,
	) -> (PublicationRecord, Transaction) {
		let (parent, tx) = publication_transaction(fee, height);
		let record = PublicationRecord::new(path, policy, &tx, &[parent], bitcoin::FeeRate::from_sat_per_vb_u32(100)).unwrap();
		(record, tx)
	}

	#[test]
	fn exact_publication_restart_keeps_native_58000_and_carrier_coverage_separate() {
		let directory = publication_directory("restart");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (record, tx) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Round, 5_000, 1);
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			budget.reserve_ceiling(Amount::from_sat(50_000)).unwrap();
			assert!(budget.require_publication(&record, &tx).is_err());
			budget.reserve_publication(&record).unwrap();
			budget.require_publication(&record, &tx).unwrap();
			let before = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
			budget.reserve_publication(&record).unwrap();
			assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), before, "same candidate cannot renew or double debit");
			budget.require_retained_fees(Amount::from_sat(50_000)).unwrap();
			assert!(budget.require_retained_fees(Amount::from_sat(50_001)).is_err(), "pool/round charge cannot conceal missing carrier reservation");
		}
		let before = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			budget.require_publication(&record, &tx).unwrap();
			let (next, _) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Pool, 5_000, 2);
			assert!(budget.reserve_publication(&next).is_err());
			let (wrong_class, _) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Pool, 5_000, 1);
			assert!(budget.reserve_publication(&wrong_class).is_err());
			let mut changed = tx.clone(); changed.input[0].witness.push([1]);
			assert!(budget.require_publication(&record, &changed).is_err());
			assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), before);
		}
		remove_publication_directory(&directory);
	}

	#[test]
	fn concurrent_publications_share_the_existing_native_remainder() {
		let directory = publication_directory("concurrent");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let budget = std::sync::Arc::new(OrdinaryFeeBudget::load(&directory, policy).unwrap());
		budget.reserve_ceiling(Amount::from_sat(50_000)).unwrap();
		let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
		let workers = [super::ordinary_fee_publication::PublicationPath::Round, super::ordinary_fee_publication::PublicationPath::Pool]
			.into_iter().enumerate().map(|(index, path)| {
				let budget = budget.clone(); let barrier = barrier.clone();
				std::thread::spawn(move || {
					let (record, _) = publication_record(policy, path, 5_000, index as u32 + 1);
					barrier.wait(); budget.reserve_publication(&record).is_ok()
				})
			}).collect::<Vec<_>>();
		assert_eq!(workers.into_iter().map(|worker| worker.join().expect("native ledger worker panicked"))
			.filter(|ok| *ok).count(), 1);
		assert_eq!(parse_history(&std::fs::read_to_string(directory.join(BUDGET_FILE)).unwrap(), policy).unwrap(), 55_000);
		drop(budget);
		OrdinaryFeeBudget::load(&directory, policy).unwrap();
		remove_publication_directory(&directory);
	}

	#[test]
	fn unindexed_original_publication_intent_blocks_restart_without_reset() {
		let directory = publication_directory("pending-intent");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (record, _) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Pool, 5_000, 1);
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			budget.reserve_ceiling(Amount::from_sat(50_000)).unwrap();
			// Exact filesystem boundary after original witness fsync and before
			// charge append. This is not claimed as a full daemon crash test.
			ordinary_fee_publication::write_witness(&directory, policy, &BTreeMap::new(), &record).unwrap();
		}
		let before = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), before);
		assert_eq!(before, b"50000\n");
		remove_publication_directory(&directory);
	}

	#[test]
	fn exact_publication_witness_torn_and_unknown_records_cannot_authorize() {
		let directory = publication_directory("corruption");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (record, tx) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Round, 500, 1);
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			budget.reserve_publication(&record).unwrap();
		}
		let complete = std::fs::read_to_string(directory.join(BUDGET_FILE)).unwrap();
		assert!(parse_joint_history(complete.trim_end_matches('\n'), policy).is_err());
		assert!(parse_joint_history(&format!("{complete}{complete}"), policy).is_err());
		let name = directory.file_name().unwrap().to_str().unwrap();
		let witness = directory.with_file_name(format!("{name}.ordinary-native-publications"))
			.join(format!("{}.json", tx.compute_wtxid()));
		let original = std::fs::read(&witness).unwrap();
		std::fs::write(&witness, b"{}").unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		std::fs::write(&witness, &original).unwrap();
		let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		budget.require_publication(&record, &tx).unwrap(); drop(budget);
		remove_publication_directory(&directory);
	}

	#[test]
	fn qualified_publication_uses_existing_full_ceiling_without_extra_allowance() {
		let directory = publication_directory("qualification");
		let policy = synthetic_qualification(2, 10_000);
		let chain = crate::bitcoind::synthetic_fee_chain(Network::Regtest);
		std::fs::write(directory.join(DECLARATION_FILE), synthetic_declaration(2, 10_000)).unwrap();
		policy.initialize_verified(&directory, &chain).unwrap();
		let (record, tx) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Round, 500, 1);
		{
			let budget = OrdinaryFeeBudget::load_verified(&directory, policy, &chain).unwrap();
			budget.reserve_ceiling(Amount::from_sat(10_000)).unwrap();
			budget.reserve_publication(&record).unwrap();
			budget.require_publication(&record, &tx).unwrap();
			let (next, _) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Pool, 500, 2);
			assert!(budget.reserve_publication(&next).is_err());
			budget.require_retained_fees(Amount::from_sat(10_000)).unwrap();
			assert!(budget.require_retained_fees(Amount::from_sat(10_001)).is_err());
		}
		let budget = OrdinaryFeeBudget::load_verified(&directory, policy, &chain).unwrap();
		budget.require_publication(&record, &tx).unwrap(); drop(budget);
		remove_publication_directory(&directory);
	}

	#[test]
	fn publication_prices_original_parents_and_actual_signed_weight() {
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		let (parent, tx) = publication_transaction(500, 1);
		let path = super::ordinary_fee_publication::PublicationPath::Round;
		let rate = bitcoin::FeeRate::from_sat_per_vb_u32(100);
		assert!(PublicationRecord::new(path, policy, &tx, &[], rate).is_err());
		let mut wrong = parent.clone(); wrong.output[0].value = Amount::from_sat(20_000);
		assert!(PublicationRecord::new(path, policy, &tx, &[wrong], rate).is_err());
		let mut duplicate = tx.clone(); duplicate.input.push(duplicate.input[0].clone());
		assert!(PublicationRecord::new(path, policy, &duplicate, &[parent.clone()], rate).is_err());
		assert!(PublicationRecord::new(path, policy, &tx, &[parent], bitcoin::FeeRate::from_sat_per_vb_u32(1)).is_err());
	}

	#[test]
	fn publication_witness_refuses_links_and_nonprivate_permissions() {
		let directory = publication_directory("private-witness");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (record, tx) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Round, 500, 1);
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			budget.reserve_publication(&record).unwrap();
		}
		let name = directory.file_name().unwrap().to_str().unwrap();
		let root = directory.with_file_name(format!("{name}.ordinary-native-publications"));
		let witness = root.join(format!("{}.json", tx.compute_wtxid()));
		std::fs::set_permissions(&witness, std::fs::Permissions::from_mode(0o644)).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		std::fs::set_permissions(&witness, std::fs::Permissions::from_mode(0o600)).unwrap();
		let alias = directory.join("extra-link");
		std::fs::hard_link(&witness, &alias).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		std::fs::remove_file(&alias).unwrap();
		let saved = directory.join("original-witness");
		std::fs::rename(&witness, &saved).unwrap();
		std::os::unix::fs::symlink(&saved, &witness).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		std::fs::remove_file(&witness).unwrap(); std::fs::rename(&saved, &witness).unwrap();
		std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
		assert!(OrdinaryFeeBudget::load(&directory, policy).is_err());
		std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
		OrdinaryFeeBudget::load(&directory, policy).unwrap().require_publication(&record, &tx).unwrap();
		remove_publication_directory(&directory);
	}

	#[test]
	fn original_import_keeps_prior_debits_across_replay_restart_and_refused_future_reset() {
		let directory = publication_directory("opening-replay");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..OrdinaryFeePolicy::default() };
		policy.initialize(&directory).unwrap();
		let (record, signed) = publication_record(policy, super::super::ordinary_fee_publication::PublicationPath::Pool, 5_000, 91);
		let opening = super::super::ordinary_fee_opening::synthetic_checked_opening(b"10000\n".to_vec(), vec![record.clone()]);
		{
			let owner = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			owner.reserve_ceiling(Amount::from_sat(10_000)).unwrap();
			owner.import_original(&opening).unwrap();
			let exact = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
			owner.import_original(&opening).unwrap();
			assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), exact);
			owner.require_publication(&record, &signed).unwrap();
			owner.require_retained_fees(Amount::from_sat(10_000)).unwrap();
			assert!(owner.require_retained_fees(Amount::from_sat(15_000)).is_err());
		}
		{
			let owner = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			owner.import_original(&opening).unwrap();
			owner.reserve_ceiling(Amount::from_sat(43_000)).unwrap();
			assert!(owner.reserve_ceiling(Amount::from_sat(1)).is_err());
			let full = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
			assert!(owner.import_original(&opening).is_err());
			assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), full);
		}
		remove_publication_directory(&directory);
	}
	#[test]
	fn original_import_cannot_erase_a_competing_carrier_reservation() {
		let directory = publication_directory("opening-concurrent");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..OrdinaryFeePolicy::default() };
		policy.initialize(&directory).unwrap();
		let (record, _) = publication_record(policy, super::super::ordinary_fee_publication::PublicationPath::Round, 5_000, 92);
		let opening = super::super::ordinary_fee_opening::synthetic_checked_opening(Vec::new(), vec![record]);
		let owner = std::sync::Arc::new(OrdinaryFeeBudget::load(&directory, policy).unwrap());
		let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
		let other = owner.clone(); let start = barrier.clone();
		let worker = std::thread::spawn(move || { start.wait(); other.reserve_ceiling(Amount::from_sat(10_000)) });
		barrier.wait(); let imported = owner.import_original(&opening).is_ok();
		worker.join().unwrap().unwrap();
		owner.require_retained_fees(Amount::from_sat(10_000)).unwrap();
		assert_eq!(owner.ledger.lock().unwrap().charged, if imported { 15_000 } else { 10_000 });
		drop(owner); remove_publication_directory(&directory);
	}

	#[test]
	fn precommit_v2_ceiling_preserves_original_v1_actual_fee_history() {
		let directory = publication_directory("v2-original");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (legacy, _) = publication_record(policy, super::ordinary_fee_publication::PublicationPath::Round, 1_000, 1);
		let (parent, tx) = publication_transaction(1_000, 2);
		let rate = bitcoin::FeeRate::from_sat_per_vb_u32(100);
		let current = PublicationRecord::new_precommit(super::ordinary_fee_publication::PublicationPath::Pool,
			policy, &tx, &[parent], rate).unwrap();
		let ceiling = rate.fee_wu(tx.weight()).unwrap().to_sat();
		assert!(ceiling > 1_000);
		let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		budget.reserve_publication(&legacy).unwrap();
		let original = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		budget.reserve_publication(&current).unwrap();
		let after = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		assert!(after.starts_with(&original));
		assert_eq!(budget.ledger.lock().unwrap().charged, 1_000 + ceiling);
		budget.reserve_publication(&current).unwrap();
		assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), after);
		drop(budget);
		let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		assert_eq!(budget.ledger.lock().unwrap().charged, 1_000 + ceiling);
		budget.require_publication(&current, &tx).unwrap();
		drop(budget); remove_publication_directory(&directory);
	}

	#[test]
	fn precommit_unused_full_ceiling_survives_restart_without_spend_or_refund_credit() {
		let directory = publication_directory("v2-unused");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (parent, tx) = publication_transaction(1_000, 1);
		let rate = bitcoin::FeeRate::from_sat_per_vb_u32(100);
		let record = PublicationRecord::new_precommit(super::ordinary_fee_publication::PublicationPath::Round,
			policy, &tx, &[parent], rate).unwrap();
		let charge = rate.fee_wu(tx.weight()).unwrap().to_sat();
		{
			let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
			budget.reserve_ceiling(Amount::from_sat(58_000 - charge)).unwrap();
			budget.reserve_publication(&record).unwrap();
			assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
		}
		let budget = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		assert_eq!(budget.ledger.lock().unwrap().charged, 58_000);
		budget.reserve_publication(&record).unwrap();
		assert!(budget.reserve_ceiling(Amount::from_sat(1)).is_err());
		let mut other = tx.clone(); other.lock_time = bitcoin::absolute::LockTime::from_height(2).unwrap();
		assert!(budget.require_publication(&record, &other).is_err());
		drop(budget); remove_publication_directory(&directory);
	}

	#[test]
	fn precommit_concurrent_candidates_share_existing_full_ceiling_remainder() {
		let directory = publication_directory("v2-concurrent");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let records = [1,2].into_iter().map(|height| {
			let (parent, tx) = publication_transaction(1_000, height);
			PublicationRecord::new_precommit(super::ordinary_fee_publication::PublicationPath::Round,
				policy, &tx, &[parent], bitcoin::FeeRate::from_sat_per_vb_u32(100)).unwrap()
		}).collect::<Vec<_>>();
		let ceiling = records[0].entry(policy).unwrap().charge(policy).unwrap();
		let budget = std::sync::Arc::new(OrdinaryFeeBudget::load(&directory, policy).unwrap());
		budget.reserve_ceiling(Amount::from_sat(58_000 - ceiling)).unwrap();
		let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
		let tasks = records.into_iter().map(|record| {
			let budget = budget.clone(); let barrier = barrier.clone();
			std::thread::spawn(move || { barrier.wait(); budget.reserve_publication(&record).is_ok() })
		}).collect::<Vec<_>>();
		assert_eq!(tasks.into_iter().map(|t| t.join().unwrap()).filter(|ok| *ok).count(), 1);
		assert_eq!(budget.ledger.lock().unwrap().charged, 58_000);
		drop(budget); remove_publication_directory(&directory);
	}

	#[test]
	fn urgent_two_exposure_headroom_uses_same_58k_and_refuses_20k_discretionary_without_writes() {
		let directory = publication_directory("urgent-headroom");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let owner = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
		let original = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		assert!(owner.reserve_ceiling(Amount::from_sat(20_000)).is_err());
		assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), original);
		owner.reserve_ceiling(Amount::from_sat(18_000)).unwrap();
		assert!(owner.reserve_ceiling(Amount::from_sat(1)).is_err());
		let (next_native, _) = publication_record(policy, super::super::ordinary_fee_publication::PublicationPath::Round, 1, 142);
		let charged = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		assert!(owner.reserve_publication(&next_native).is_err());
		assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), charged);
		assert_eq!(owner.ledger.lock().unwrap().charged, 18_000);
		assert_eq!(owner.urgent_reserve().to_sat(), 40_000);
		drop(owner);
		let restored = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		restored.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
		assert!(restored.reserve_ceiling(Amount::from_sat(1)).is_err());
		drop(restored); remove_publication_directory(&directory);
	}
	#[test]
	fn urgent_headroom_preserves_historical_import_without_granting_new_ordinary_credit() {
		let directory = publication_directory("urgent-historical");
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, ..Default::default() };
		policy.initialize(&directory).unwrap();
		let (record, signed) = publication_record(policy, super::super::ordinary_fee_publication::PublicationPath::Pool, 5_000, 141);
		let opening = super::super::ordinary_fee_opening::synthetic_checked_opening(b"14000\n".to_vec(), vec![record.clone()]);
		let owner = OrdinaryFeeBudget::load(&directory, policy).unwrap();
		owner.reserve_ceiling(Amount::from_sat(14_000)).unwrap();
		owner.protect_urgent_attempts(Amount::from_sat(20_000)).unwrap();
		// Already-committed original fees remain charged even when past the new
		// discretionary frontier. Exact original replay adds no new debit.
		owner.import_original(&opening).unwrap(); owner.require_publication(&record, &signed).unwrap();
		let historical = std::fs::read(directory.join(BUDGET_FILE)).unwrap();
		owner.import_original(&opening).unwrap();
		assert_eq!(std::fs::read(directory.join(BUDGET_FILE)).unwrap(), historical);
		assert!(owner.reserve_ceiling(Amount::from_sat(1)).is_err());
		assert_eq!(owner.ledger.lock().unwrap().charged, 19_000);
		assert!(owner.protect_urgent_attempts(Amount::from_sat(19_999)).is_err());
		drop(owner); remove_publication_directory(&directory);
	}

	#[test]
	fn captured_native_history_preserves_v1_v2_numeric_and_single_urgent_debit_without_io() {
		let policy = OrdinaryFeePolicy {ordinary_budget_sat:58_000,..Default::default()};
		let (old,_) = publication_record(policy,super::ordinary_fee_publication::PublicationPath::Round,1000,1);
		let (parent,tx) = publication_transaction(1000,2);
		let new = PublicationRecord::new_precommit(super::ordinary_fee_publication::PublicationPath::Pool,policy,&tx,&[parent],
			bitcoin::FeeRate::from_sat_per_vb_u32(100)).unwrap();
		let limits = super::super::carrier::CarrierLimits {max_fee_rate:bitcoin::FeeRate::from_sat_per_vb_u32(100),
			max_miner_fee:Amount::from_sat(20_000),max_weight:bitcoin::Weight::from_wu(400_000)};
		let (candidate,signed,identity,actual_policy) = super::super::urgent_publication::synthetic_urgent_vector(false,limits);
		assert_eq!(actual_policy,policy);
		let publication = candidate.clone().bind_signed(&signed,&identity,limits,policy).unwrap();
		let (urgent,signed) = publication.signed_entry(&identity,limits,policy).unwrap();
		let history = format!("100\n{}{}{}{}",old.entry(policy).unwrap().line().unwrap(),new.entry(policy).unwrap().line().unwrap(),
			urgent.line(policy).unwrap(),signed.line(&urgent).unwrap());
		let files = BTreeMap::from([(format!("{}.json",old.entry(policy).unwrap().id()),serde_json::to_string(&old).unwrap()),
			(format!("{}.json",new.entry(policy).unwrap().id()),serde_json::to_string(&new).unwrap()),
			(urgent.filename(),serde_json::to_string(&candidate).unwrap()),
			(format!("urgent-{}.signed.json",urgent.attempt_index),serde_json::to_string(&publication).unwrap())]);
		let checked = validate_captured_joint_history(&history,policy,&files).unwrap();
		assert_eq!(checked.charged_sat,100+1000+new.entry(policy).unwrap().charge(policy).unwrap()+20_000);
		assert_eq!(checked.carrier_charged_sat,100);assert_eq!(checked.publications.len(),2);
		assert_eq!(checked.urgent.len(),1);assert_eq!(checked.urgent_signed_entries.len(),1);
		let mut absent=files.clone();absent.remove(&urgent.filename());assert!(validate_captured_joint_history(&history,policy,&absent).is_err());
		let mut extra=files.clone();extra.insert("unindexed.json".into(),"{}".into());assert!(validate_captured_joint_history(&history,policy,&extra).is_err());
		let mut changed=files;changed.insert(urgent.filename(),"{}".into());assert!(validate_captured_joint_history(&history,policy,&changed).is_err());
	}
	#[test]
	fn captured_native_history_rejects_torn_duplicate_or_orphan_sources_without_zero_alias() {
		let policy = OrdinaryFeePolicy {ordinary_budget_sat:58_000,..Default::default()};
		let (record,_) = publication_record(policy,super::ordinary_fee_publication::PublicationPath::Round,1000,1);
		let line=record.entry(policy).unwrap().line().unwrap();
		let files=BTreeMap::from([(format!("{}.json",record.entry(policy).unwrap().id()),serde_json::to_string(&record).unwrap())]);
		assert!(validate_captured_joint_history("",policy,&files).is_err());
		assert!(validate_captured_joint_history(line.trim_end(),policy,&files).is_err());
		assert!(validate_captured_joint_history(&(line.clone()+&line),policy,&files).is_err());
		assert!(validate_captured_joint_history("58001\n",policy,&BTreeMap::new()).is_err());
		assert!(validate_captured_joint_history("unknown\n",policy,&BTreeMap::new()).is_err());
		assert!(validate_captured_joint_history(&line,policy,&BTreeMap::new()).is_err());
	}

}
