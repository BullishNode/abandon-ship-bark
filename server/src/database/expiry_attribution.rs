//! Resumable attribution without payer hints or a bounded-search ownership guess.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::Context;
use bitcoin::bip32::{ChildNumber, Xpub};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Network, ScriptBuf};
use futures::stream::{self, StreamExt, TryStreamExt};

use ark::mailbox::MailboxIdentifier;
use ark::SECP;

use super::{Db, Tx};

/// The exclusive end of the wallet's non-hardened issuance contract.
pub const OWNER_KEYSPACE_END: u32 = 1 << 31;
// A mailbox candidate can use spare lanes, never the reserved oldest-work lane.
// This prefix is a scheduling preference, never a no-owner conclusion.
const MAILBOX_PRIORITY_PREFIX: i64 = 100_000;
/// A derivation pass never loads or advances an unbounded pending-key fanout.
pub const OWNER_SCAN_MAX_KEYS: usize = 256;

struct ScanAnchor {
	mailbox_pk: PublicKey,
	xpub: Xpub,
	start: u32,
}

/// Reserve one lane for the oldest cursor even under continuous mailbox work.
/// Duplicate account rows do not cause the same child range to be derived twice.
fn select_scan_anchors(mut fair: Vec<ScanAnchor>, preferred: Vec<ScanAnchor>, workers: usize) -> Vec<ScanAnchor> {
	let first = if fair.is_empty() { None } else { Some(fair.remove(0)) };
	let mut accounts = BTreeSet::new();
	first.into_iter().chain(preferred).chain(fair)
		.filter(|anchor| accounts.insert(anchor.mailbox_pk)).take(workers).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
	Owned { mailbox_pk: PublicKey, child_index: u32, spk: ScriptBuf },
	Unresolved,
	NoRegisteredOwner,
}

#[derive(Default, Debug)]
pub struct ScanStatistics {
	pub records_scanned: usize,
	pub children_derived: u64,
	pub keys_matched: usize,
}

pub struct OwnerScanBatch {
	mailbox_pk: PublicKey,
	xpub: Xpub,
	start: u32,
	end: u32,
	keys: BTreeMap<PublicKey, u32>,
}

pub struct OwnerScanResult {
	work: OwnerScanBatch,
	matches: BTreeMap<PublicKey, u32>,
}

impl OwnerScanBatch {
	pub fn derive(self) -> anyhow::Result<OwnerScanResult> {
		ensure!(self.start <= self.end && self.end <= OWNER_KEYSPACE_END, "invalid owner scan range");
		let mut matches = BTreeMap::new();
		for index in self.start..self.end {
			let child = self.xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(index)?)?.public_key;
			if self.keys.contains_key(&child) { matches.insert(child, index); }
		}
		Ok(OwnerScanResult { work: self, matches })
	}
}

impl Tx<'_> {
	/// A newly arriving key gets its own zero-based coverage, even when an older
	/// key already exhausted this same registered account. Mailboxes prioritize
	/// candidate work but never constitute an ownership proof.
	pub async fn queue_expiry_owner(
		&self, user_pubkey: PublicKey, mailbox: Option<MailboxIdentifier>,
	) -> anyhow::Result<()> {
		self.queue_expiry_owner_inner(user_pubkey, mailbox, false).await
	}

	/// Readiness can prove a new key's candidate before materializing every
	/// account cursor. Explicit scanner callers retain their queued work boundary.
	pub(crate) async fn queue_expiry_owner_with_candidate_proof(
		&self, user_pubkey: PublicKey, mailbox: Option<MailboxIdentifier>,
	) -> anyhow::Result<()> {
		self.queue_expiry_owner_inner(user_pubkey, mailbox, true).await
	}

	async fn queue_expiry_owner_inner(
		&self, user_pubkey: PublicKey, mailbox: Option<MailboxIdentifier>, prove_candidate: bool,
	) -> anyhow::Result<()> {
		// Pair initialization cannot race a registration's pending-key query.
		self.lock_fallback_registry().await?;
		let key = user_pubkey.to_string();
		let candidate = mailbox.map(|id| id.as_pubkey().to_string());
		let inserted = self.query_opt("
			INSERT INTO expiry_pending_key (user_pubkey, mailbox_pk)
			SELECT $1, $2 WHERE NOT EXISTS (SELECT 1 FROM expiry_owner_key WHERE user_pubkey=$1)
			ON CONFLICT (user_pubkey) DO NOTHING RETURNING user_pubkey
		", &[&key, &candidate]).await?.is_some();
		if !inserted {
			if candidate.is_some() {
				self.execute("UPDATE expiry_pending_key SET mailbox_pk=$2 WHERE user_pubkey=$1 AND mailbox_pk IS NULL",
					&[&key, &candidate]).await?;
			}
			return Ok(());
		}
		// A concurrent candidate proof can cache the key and delete its pending
		// row while our INSERT waits. READ COMMITTED's earlier NOT EXISTS does
		// not recheck that new cache entry; use a fresh statement before queueing.
		if self.query_opt("SELECT user_pubkey FROM expiry_owner_key WHERE user_pubkey=$1", &[&key]).await?.is_some() {
			self.execute("DELETE FROM expiry_pending_key WHERE user_pubkey=$1", &[&key]).await?;
			return Ok(());
		}
		if prove_candidate {
			// Try the existing initial prefix before creating a key/account cursor
			// matrix. The stored record was authenticated at immutable insertion;
			// the mailbox hint alone is never proof. expiry_owner still verifies
			// that record for the network and derives the key before paying.
			if let Some(mailbox_pk) = mailbox.map(|id| id.as_pubkey()) {
				if let Some(record) = self.get_fallback(mailbox_pk).await? {
					let work = OwnerScanBatch {
						mailbox_pk, xpub: record.account_xpub, start: 0, end: 32,
						keys: BTreeMap::from([(user_pubkey, 0)]),
					};
					if let Ok(Ok(result)) = tokio::task::spawn_blocking(move || work.derive()).await {
						if let Some(index) = result.matches.get(&user_pubkey) {
							let mailbox = mailbox_pk.to_string();
							self.execute("
								INSERT INTO expiry_owner_key (user_pubkey, mailbox_pk, child_index)
								VALUES ($1, $2, $3) ON CONFLICT (user_pubkey) DO NOTHING
							", &[&key, &mailbox, &i64::from(*index)]).await?;
							let owner = self.query_one("
								SELECT mailbox_pk, child_index FROM expiry_owner_key WHERE user_pubkey=$1
							", &[&key]).await?;
							ensure!(owner.get::<_, &str>("mailbox_pk") == mailbox
								&& owner.get::<_, i64>("child_index") == i64::from(*index),
								"coin key derives from conflicting registered accounts");
							self.execute("DELETE FROM expiry_pending_key WHERE user_pubkey=$1", &[&key]).await?;
							return Ok(());
						}
					}
				}
			}
		}
		// A miss or failed derivation leaves every account's coverage at zero.
		// The pending row and its complete initial cursor set commit together.
		// Registrations append their own zero cursors under the writer fence,
		// so a repeated readiness pass need not conflict-check every account.
		self.execute("
			INSERT INTO expiry_scan (user_pubkey, mailbox_pk, scanned_up_to)
			SELECT $1, r.mailbox_pk, 0 FROM fallback_record r WHERE NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=$1)
			ON CONFLICT (user_pubkey, mailbox_pk) DO NOTHING
		", &[&user_pubkey.to_string()]).await?;
		Ok(())
	}

	/// Register fresh zero coverage for every unresolved key in the same
	/// transaction that inserts a new immutable account.
	pub(super) async fn add_expiry_scan_record(&self, mailbox_pk: PublicKey) -> anyhow::Result<()> {
		self.execute("
			INSERT INTO expiry_scan (user_pubkey, mailbox_pk, scanned_up_to)
			SELECT p.user_pubkey, $1, 0 FROM expiry_pending_key p WHERE NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=p.user_pubkey)
			ON CONFLICT (user_pubkey, mailbox_pk) DO NOTHING
		", &[&mailbox_pk.to_string()]).await?;
		Ok(())
	}

	/// A cached match is derived again and its immutable registration authenticated
	/// before returning a payout script. Missing or partial coverage is unresolved.
	pub async fn expiry_owner(&self, user_pubkey: PublicKey, network: Network) -> anyhow::Result<Attribution> {
		self.lock_fallback_registry().await?;
		let key = user_pubkey.to_string();
		if let Some(row) = self.query_opt("
			SELECT mailbox_pk, child_index FROM expiry_owner_key
			WHERE user_pubkey = $1
		", &[&key]).await? {
			let mailbox_pk = PublicKey::from_str(row.get("mailbox_pk"))?;
			let child_index = u32::try_from(row.get::<_, i64>("child_index"))?;
			let record = self.get_fallback(mailbox_pk).await?.context("owner registration disappeared")?;
			record.verify(network, mailbox_pk)?;
			let child = record.account_xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(child_index)?)?;
			ensure!(child.public_key == user_pubkey, "cached fallback owner does not derive this key");
			return Ok(Attribution::Owned { mailbox_pk, child_index, spk: record.spk });
		}
		let pending = self.query_opt("SELECT user_pubkey FROM expiry_pending_key WHERE user_pubkey=$1", &[&key]).await?;
		if pending.is_none() { return Ok(Attribution::Unresolved); }
		let incomplete = self.query_opt("
			SELECT r.id FROM fallback_record r
			LEFT JOIN expiry_scan s ON s.mailbox_pk=r.mailbox_pk AND s.user_pubkey=$1
			WHERE s.scanned_up_to IS NULL OR s.scanned_up_to < $2 LIMIT 1
		",
			&[&key, &i64::from(OWNER_KEYSPACE_END)]).await?;
		if incomplete.is_some() { return Ok(Attribution::Unresolved); }
		Ok(Attribution::NoRegisteredOwner)
	}

	async fn prepare_expiry_scans(&self, chunk_size: u32, workers: usize) -> anyhow::Result<Vec<OwnerScanBatch>> {
		// Tickets age only after compare-and-advance coverage. New keys
		// and registrations join behind existing waiting cursors, irrespective
		// of their prefix or an unverified mailbox candidate. Fetch bounded rows
		// before coalescing accounts; duplicate accounts can leave spare lanes.
		let limit = i64::try_from(workers)?;
		let fair = self.query("
			SELECT s.mailbox_pk, s.scanned_up_to, r.account_xpub
			FROM expiry_scan s JOIN fallback_record r ON r.mailbox_pk=s.mailbox_pk
			WHERE s.scanned_up_to < 2147483648 AND NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=s.user_pubkey)
			ORDER BY s.service_ticket, s.user_pubkey, s.mailbox_pk LIMIT $1
		", &[&limit]).await?;
		let preferred = if workers > 1 {
			self.query("
				SELECT s.mailbox_pk, s.scanned_up_to, r.account_xpub
				FROM expiry_scan s JOIN fallback_record r ON r.mailbox_pk=s.mailbox_pk
				JOIN expiry_pending_key p ON p.user_pubkey=s.user_pubkey AND p.mailbox_pk=s.mailbox_pk
				WHERE s.scanned_up_to < $1 AND NOT EXISTS
					(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=s.user_pubkey)
				ORDER BY s.service_ticket, s.user_pubkey, s.mailbox_pk LIMIT $2
			",
				&[&MAILBOX_PRIORITY_PREFIX, &limit]).await?
		} else { Vec::new() };
		let anchors = |rows: Vec<tokio_postgres::Row>| rows.into_iter().map(|row| -> anyhow::Result<_> {
			Ok(ScanAnchor { mailbox_pk: PublicKey::from_str(row.get("mailbox_pk"))?,
				xpub: Xpub::from_str(row.get("account_xpub"))?,
				start: u32::try_from(row.get::<_, i64>("scanned_up_to"))? })
		}).collect::<anyhow::Result<Vec<_>>>();
		let records = select_scan_anchors(anchors(fair)?, anchors(preferred)?, workers);
		let mut work = Vec::with_capacity(records.len());
		for anchor in records {
			let start = anchor.start;
			// Fresh zero-prefix arrivals cannot reset an older anchor. Coalesce
			// only its exact prefix, in ticket order, with an indexed fixed cap.
			let span = chunk_size.min(if start == 0 { 32 } else { start.saturating_mul(2) });
			let end = start.saturating_add(span).min(OWNER_KEYSPACE_END);
			let rows = self.query("
				SELECT s.user_pubkey, s.scanned_up_to FROM expiry_scan s
				WHERE s.mailbox_pk=$1 AND s.scanned_up_to=$2 AND s.scanned_up_to < 2147483648 AND NOT EXISTS
					(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=s.user_pubkey)
				ORDER BY s.service_ticket, s.user_pubkey LIMIT $3
			",
				&[&anchor.mailbox_pk.to_string(), &i64::from(start), &i64::try_from(OWNER_SCAN_MAX_KEYS)?]).await?;
			let keys = rows.into_iter().map(|row| -> anyhow::Result<_> { Ok((
				PublicKey::from_str(row.get("user_pubkey"))?, u32::try_from(row.get::<_, i64>("scanned_up_to"))?,
			)) }).collect::<anyhow::Result<BTreeMap<_, _>>>()?;
			// A concurrent completed pass may have consumed this exact prefix.
			if keys.is_empty() { continue; }
			work.push(OwnerScanBatch { mailbox_pk: anchor.mailbox_pk, xpub: anchor.xpub, start, end, keys });
		}
		Ok(work)
	}

	async fn commit_expiry_scan(&self, result: &OwnerScanResult) -> anyhow::Result<usize> {
		let mailbox = result.work.mailbox_pk.to_string();
		let mut matched = 0;
		for (key, prefix) in &result.work.keys {
			let encoded_key = key.to_string();
			// Compare-and-advance the key's own contiguous prefix. A late key
			// absent from this work item is never advanced by this result.
			let updated = self.execute("
				UPDATE expiry_scan SET scanned_up_to=$3, service_ticket=nextval('expiry_scan_service_ticket_seq')
				WHERE user_pubkey=$1 AND mailbox_pk=$2 AND scanned_up_to=$4
			",
				&[&encoded_key, &mailbox, &i64::from(result.work.end), &i64::from(*prefix)]).await?;
			if updated == 0 { continue; }
			if let Some(index) = result.matches.get(key) {
				self.execute("
					INSERT INTO expiry_owner_key (user_pubkey, mailbox_pk, child_index)
					VALUES ($1, $2, $3) ON CONFLICT (user_pubkey) DO NOTHING
				",
					&[&encoded_key, &mailbox, &i64::from(*index)]).await?;
				let row = self.query_one("
					SELECT mailbox_pk, child_index FROM expiry_owner_key
					WHERE user_pubkey=$1
				", &[&encoded_key]).await?;
				ensure!(row.get::<_, &str>("mailbox_pk") == mailbox && row.get::<_, i64>("child_index") == i64::from(*index),
					"coin key derives from conflicting registered accounts");
				// A verified match replaces the cross-record search state.
				// Retain only its sparse key mapping, rather than every completed
				// or unstarted registry cursor for the lifetime of this coin.
				self.execute("DELETE FROM expiry_pending_key WHERE user_pubkey=$1", &[&encoded_key]).await?;
				matched += 1;
			}
		}
		Ok(matched)
	}
}

impl Db {
	/// Prepare an immutable work snapshot without advancing coverage. A caller
	/// may drop it or restart before derivation/commit without skipping any key.
	pub async fn prepare_expiry_owner_scan(&self, chunk_size: u32, workers: usize) -> anyhow::Result<Vec<OwnerScanBatch>> {
		ensure!(chunk_size != 0 && workers != 0, "owner scan size and concurrency must be nonzero");
		self.write(async |tx| tx.prepare_expiry_scans(chunk_size, workers).await).await
	}

	/// Publish only completed work. Every pending-key cursor and match
	/// commits atomically; keys queued after preparation retain their own prefix.
	pub async fn commit_expiry_owner_scan(&self, network: Network, result: &OwnerScanResult) -> anyhow::Result<usize> {
		self.write(async |tx| {
			// Do not let queue initialization or a new registration race the
			// match's pending-row deletion and its cascading cursor cleanup.
			tx.lock_fallback_registry_writer().await?;
			tx.get_fallback(result.work.mailbox_pk).await?.context("scan registration disappeared")?
				.verify(network, result.work.mailbox_pk)?;
			tx.commit_expiry_scan(result).await
		}).await
	}

	/// One bounded pass. Derivation runs without a database transaction; only
	/// matches and contiguous coverage are persisted in the commit.
	pub async fn scan_expiry_owners(
		&self, network: Network, chunk_size: u32, workers: usize,
	) -> anyhow::Result<ScanStatistics> {
		let work = self.prepare_expiry_owner_scan(chunk_size, workers).await?;
		let results = stream::iter(work).map(|work| async move {
			tokio::task::spawn_blocking(move || work.derive()).await.context("owner derivation worker failed")?
		}).buffer_unordered(workers).try_collect::<Vec<_>>().await?;
		if results.is_empty() { return Ok(ScanStatistics::default()); }
		// Derivation is already complete. Commit this bounded pass together,
		// rather than paying a durable transaction for every account. The short
		// writer fence also keeps a late queue from recreating resolved work.
		self.write(async |tx| {
			tx.lock_fallback_registry_writer().await?;
			let mut stats = ScanStatistics::default();
			for result in results {
				tx.get_fallback(result.work.mailbox_pk).await?.context("scan registration disappeared")?
					.verify(network, result.work.mailbox_pk)?;
				stats.keys_matched += tx.commit_expiry_scan(&result).await?;
				stats.records_scanned += 1;
				stats.children_derived += u64::from(result.work.end - result.work.start);
			}
			Ok(stats)
		}).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::bip32::Xpriv;

	#[test]
	fn expiry_owner_scan_reserves_oldest_lane_ahead_of_mailbox_candidates() {
		let anchor = |byte: u8, start| {
			let account = Xpriv::new_master(Network::Regtest, &[byte; 64]).unwrap();
			let xpub = Xpub::from_priv(&SECP, &account);
			ScanAnchor { mailbox_pk: xpub.public_key, xpub, start }
		};
		let oldest = anchor(1, 64).mailbox_pk;
		let preferred = anchor(2, 0).mailbox_pk;
		let selected = select_scan_anchors(vec![anchor(1, 64), anchor(3, 0)], vec![anchor(2, 0)], 1);
		assert_eq!(selected.len(), 1);
		assert_eq!((selected[0].mailbox_pk, selected[0].start), (oldest, 64));
		let selected = select_scan_anchors(vec![anchor(1, 64), anchor(3, 0)], vec![anchor(2, 0)], 2);
		assert_eq!(selected.iter().map(|v| v.mailbox_pk).collect::<Vec<_>>(), vec![oldest, preferred]);
		let selected = select_scan_anchors(vec![anchor(1, 64), anchor(3, 0)], vec![anchor(1, 0)], 2);
		assert_eq!(selected.iter().map(|v| (v.mailbox_pk, v.start)).collect::<Vec<_>>(),
			vec![(oldest, 64), (anchor(3, 0).mailbox_pk, 0)]);
	}

	#[test]
	fn expiry_owner_scan_reaches_the_last_normal_child_without_storing_every_key() {
		let account = Xpriv::new_master(Network::Regtest, &[3; 64]).unwrap();
		let xpub = Xpub::from_priv(&SECP, &account);
		let key = xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(OWNER_KEYSPACE_END - 1).unwrap()).unwrap().public_key;
		let result = OwnerScanBatch { mailbox_pk: xpub.public_key, xpub,
			start: OWNER_KEYSPACE_END - 2, end: OWNER_KEYSPACE_END,
			keys: BTreeMap::from([(key, OWNER_KEYSPACE_END - 2)]),
		}.derive().unwrap();
		assert_eq!(result.matches, BTreeMap::from([(key, OWNER_KEYSPACE_END - 1)]));
		assert_eq!(result.matches.len(), 1);
	}
}
