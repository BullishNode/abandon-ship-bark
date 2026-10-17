//! Resumable attribution without payer hints or a bounded-search ownership guess.

use std::collections::BTreeMap;
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
// This is a scheduling preference, never a no-owner conclusion. A mailbox
// candidate receives a short head start before exhaustive fair scheduling.
const MAILBOX_PRIORITY_PREFIX: i64 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
	Owned { mailbox_pk: PublicKey, child_index: u32, spk: ScriptBuf },
	Unresolved,
	/// Commit only under the registry's shared lock after checking this version.
	NoRegisteredOwner { registry_version: i64 },
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
		// Pair initialization cannot race a registration's pending-key query.
		self.lock_fallback_registry().await?;
		self.execute("INSERT INTO expiry_pending_key (user_pubkey, mailbox_pk)
			SELECT $1, $2 WHERE NOT EXISTS (SELECT 1 FROM expiry_owner_key WHERE user_pubkey=$1)
			ON CONFLICT (user_pubkey) DO UPDATE
			SET mailbox_pk = COALESCE(expiry_pending_key.mailbox_pk, EXCLUDED.mailbox_pk)",
			&[&user_pubkey.to_string(), &mailbox.map(|id| id.as_pubkey().to_string())]).await?;
		self.execute("INSERT INTO expiry_scan (user_pubkey, mailbox_pk, scanned_up_to)
			SELECT $1, r.mailbox_pk, 0 FROM fallback_record r WHERE NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=$1)
			ON CONFLICT (user_pubkey, mailbox_pk) DO NOTHING", &[&user_pubkey.to_string()]).await?;
		Ok(())
	}

	/// Register fresh zero coverage for every unresolved key in the same
	/// transaction that inserts a new immutable account.
	pub(super) async fn add_expiry_scan_record(&self, mailbox_pk: PublicKey) -> anyhow::Result<()> {
		self.execute("INSERT INTO expiry_scan (user_pubkey, mailbox_pk, scanned_up_to)
			SELECT p.user_pubkey, $1, 0 FROM expiry_pending_key p WHERE NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=p.user_pubkey)
			ON CONFLICT (user_pubkey, mailbox_pk) DO NOTHING", &[&mailbox_pk.to_string()]).await?;
		Ok(())
	}

	/// A cached match is derived again and its immutable registration authenticated
	/// before returning a payout script. Missing or partial coverage is unresolved.
	pub async fn expiry_owner(&self, user_pubkey: PublicKey, network: Network) -> anyhow::Result<Attribution> {
		self.lock_fallback_registry().await?;
		let key = user_pubkey.to_string();
		if let Some(row) = self.query_opt("SELECT mailbox_pk, child_index FROM expiry_owner_key
			WHERE user_pubkey = $1", &[&key]).await? {
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
		let incomplete = self.query_opt("SELECT r.id FROM fallback_record r
			LEFT JOIN expiry_scan s ON s.mailbox_pk=r.mailbox_pk AND s.user_pubkey=$1
			WHERE s.scanned_up_to IS NULL OR s.scanned_up_to < $2 LIMIT 1",
			&[&key, &i64::from(OWNER_KEYSPACE_END)]).await?;
		if incomplete.is_some() { return Ok(Attribution::Unresolved); }
		Ok(Attribution::NoRegisteredOwner { registry_version: self.fallback_registry_version().await? })
	}

	/// Call in the same short transaction that commits a no-owner decision.
	pub async fn revalidate_expiry_no_owner(
		&self, user_pubkey: PublicKey, registry_version: i64, network: Network,
	) -> anyhow::Result<bool> {
		self.lock_fallback_registry().await?;
		Ok(self.expiry_owner(user_pubkey, network).await? == Attribution::NoRegisteredOwner { registry_version })
	}

	async fn prepare_expiry_scans(&self, chunk_size: u32, workers: usize) -> anyhow::Result<Vec<OwnerScanBatch>> {
		// Bound the candidate accounts before joining their pending keys. A
		// matched key deletes its pending row and cascades every scan cursor,
		// so the indexed fair prefix contains only unfinished owner searches.
		// Several keys can select the same account; deriving it once still
		// compares all of its keys in the selected interval below.
		let records = self.query("WITH preferred AS MATERIALIZED (
			SELECT s.mailbox_pk FROM expiry_pending_key p JOIN expiry_scan s
				ON s.user_pubkey=p.user_pubkey AND s.mailbox_pk=p.mailbox_pk
			WHERE p.mailbox_pk IS NOT NULL AND s.scanned_up_to < $3
			ORDER BY s.scanned_up_to, s.mailbox_pk LIMIT $2
		), fair AS MATERIALIZED (
			SELECT s.mailbox_pk FROM expiry_scan s WHERE s.scanned_up_to < $1
			ORDER BY s.scanned_up_to, s.mailbox_pk LIMIT $2
		), candidates AS MATERIALIZED (
			SELECT mailbox_pk FROM preferred UNION SELECT mailbox_pk FROM fair
		)
			SELECT r.mailbox_pk, r.account_xpub, MIN(s.scanned_up_to) AS start_index,
			BOOL_OR(p.mailbox_pk=r.mailbox_pk) AS candidate
			FROM candidates c JOIN fallback_record r ON r.mailbox_pk=c.mailbox_pk
			JOIN expiry_scan s ON s.mailbox_pk=r.mailbox_pk
			JOIN expiry_pending_key p ON p.user_pubkey=s.user_pubkey
			WHERE s.scanned_up_to < $1 AND NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=s.user_pubkey)
			GROUP BY r.mailbox_pk, r.account_xpub
			ORDER BY CASE WHEN BOOL_OR(p.mailbox_pk=r.mailbox_pk) AND MIN(s.scanned_up_to) < $3
				THEN 0 ELSE 1 END, start_index, candidate DESC NULLS LAST, r.mailbox_pk LIMIT $2",
			&[&i64::from(OWNER_KEYSPACE_END), &i64::try_from(workers)?, &MAILBOX_PRIORITY_PREFIX]).await?;
		let mut work = Vec::with_capacity(records.len());
		for row in records {
			let mailbox: &str = row.get("mailbox_pk");
			let start = u32::try_from(row.get::<_, i64>("start_index"))?;
			// Small initial passes find ordinary low indices across a large
			// registry without deriving a full maximum chunk per wrong record.
			let span = chunk_size.min(if start == 0 { 32 } else { start.saturating_mul(2) });
			let end = start.saturating_add(span).min(OWNER_KEYSPACE_END);
			let rows = self.query("SELECT s.user_pubkey, s.scanned_up_to FROM expiry_scan s WHERE s.mailbox_pk=$1
				AND s.scanned_up_to >= $2 AND s.scanned_up_to < $3 AND NOT EXISTS
				(SELECT 1 FROM expiry_owner_key o WHERE o.user_pubkey=s.user_pubkey)",
				&[&mailbox, &i64::from(start), &i64::from(end)]).await?;
			let keys = rows.into_iter().map(|row| -> anyhow::Result<_> { Ok((
				PublicKey::from_str(row.get("user_pubkey"))?, u32::try_from(row.get::<_, i64>("scanned_up_to"))?,
			)) }).collect::<anyhow::Result<BTreeMap<_, _>>>()?;
			work.push(OwnerScanBatch { mailbox_pk: PublicKey::from_str(mailbox)?,
				xpub: Xpub::from_str(row.get("account_xpub"))?, start, end, keys });
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
			let updated = self.execute("UPDATE expiry_scan SET scanned_up_to=$3
				WHERE user_pubkey=$1 AND mailbox_pk=$2 AND scanned_up_to=$4",
				&[&encoded_key, &mailbox, &i64::from(result.work.end), &i64::from(*prefix)]).await?;
			if updated == 0 { continue; }
			if let Some(index) = result.matches.get(key) {
				self.execute("INSERT INTO expiry_owner_key (user_pubkey, mailbox_pk, child_index)
					VALUES ($1, $2, $3) ON CONFLICT (user_pubkey) DO NOTHING",
					&[&encoded_key, &mailbox, &i64::from(*index)]).await?;
				let row = self.query_one("SELECT mailbox_pk, child_index FROM expiry_owner_key
					WHERE user_pubkey=$1", &[&encoded_key]).await?;
				ensure!(row.get::<_, &str>("mailbox_pk") == mailbox && row.get::<_, i64>("child_index") == i64::from(*index),
					"coin key derives from conflicting registered accounts");
				// A verified actual match replaces the cross-record search state.
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

	/// Publish only completed work. Every pending-key cursor and actual match
	/// commits atomically; keys queued after preparation retain their own prefix.
	pub async fn commit_expiry_owner_scan(&self, network: Network, result: &OwnerScanResult) -> anyhow::Result<usize> {
		self.write(async |tx| {
			tx.get_fallback(result.work.mailbox_pk).await?.context("scan registration disappeared")?
				.verify(network, result.work.mailbox_pk)?;
			tx.commit_expiry_scan(result).await
		}).await
	}

	/// One bounded pass. Derivation runs without a database transaction; only
	/// actual matches and contiguous coverage are persisted in the commit.
	pub async fn scan_expiry_owners(
		&self, network: Network, chunk_size: u32, workers: usize,
	) -> anyhow::Result<ScanStatistics> {
		let work = self.prepare_expiry_owner_scan(chunk_size, workers).await?;
		let results = stream::iter(work).map(|work| async move {
			tokio::task::spawn_blocking(move || work.derive()).await.context("owner derivation worker failed")?
		}).buffer_unordered(workers).try_collect::<Vec<_>>().await?;
		let mut stats = ScanStatistics::default();
		for result in results {
			stats.keys_matched += self.commit_expiry_owner_scan(network, &result).await?;
			stats.records_scanned += 1;
			stats.children_derived += u64::from(result.work.end - result.work.start);
		}
		Ok(stats)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::bip32::Xpriv;

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
