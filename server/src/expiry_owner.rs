//! Payout owner lookup through wallet recovery.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use bitcoin::bip32::ChildNumber;
use bitcoin::secp256k1::PublicKey;
use tracing::warn;

use ark::mailbox::MailboxIdentifier;
use ark::SECP;

use crate::Server;
use crate::database::expiry_attribution::Attribution;

pub(crate) enum OwnerLookup {
	Running,
	NoMatch { registrations: i64, checked_at: Instant },
}

impl OwnerLookup {
	fn retain_current(lookups: &mut HashMap<PublicKey, Self>, registrations: i64) {
		lookups.retain(|_, lookup| !lookup.needs_retry(registrations));
	}

	fn needs_retry(&self, registrations: i64) -> bool {
		match self {
			Self::Running => false,
			Self::NoMatch { registrations: previous, checked_at } =>
				*previous != registrations || checked_at.elapsed() >= Duration::from_secs(86_400),
		}
	}
}

impl Server {
	/// A miss must not delay ready siblings. The next readiness pass consumes
	/// the cached match after this task completes.
	pub(crate) async fn queue_expiry_owner_lookup(
		self: &Arc<Self>, key: PublicKey, mailbox: Option<MailboxIdentifier>, htlc: bool,
	) -> anyhow::Result<()> {
		let registrations = self.db.read(async |tx| Ok(tx.query_one(
			"SELECT COALESCE(MAX(id), 0) FROM fallback_record", &[],
		).await?.get::<_, i64>(0))).await?;
		{
			let mut lookups = self.expiry_owner_lookups.lock();
			OwnerLookup::retain_current(&mut lookups, registrations);
			if lookups.get(&key).is_some_and(|lookup| !lookup.needs_retry(registrations)) {
				return Ok(());
			}
			lookups.insert(key, OwnerLookup::Running);
		}
		let srv = Arc::clone(self);
		tokio::spawn(async move {
			let result = async {
				// Share both the upstream queue bound and its CPU admission limit.
				let _queued = srv.xpub_recovery_queue.try_acquire()
					.context("xpub recovery queue is full")?;
				let _permit = srv.xpub_recovery_permits.acquire().await
					.context("xpub recovery semaphore closed")?;
				srv.find_expiry_owner(key, mailbox, htlc).await
			};
			let result = tokio::select! {
				_ = srv.rtmgr.shutdown_signal() => None,
				result = result => Some(result),
			};
			let mut lookups = srv.expiry_owner_lookups.lock();
			if let Some(Ok(false)) = result {
				lookups.insert(key, OwnerLookup::NoMatch { registrations, checked_at: Instant::now() });
			} else {
				lookups.remove(&key);
				if let Some(Err(error)) = result {
					warn!(%key, %error, "Expiry owner lookup remains unresolved");
				}
			}
		});
		Ok(())
	}

	async fn find_expiry_owner(
		&self, key: PublicKey, mailbox: Option<MailboxIdentifier>, htlc: bool,
	) -> anyhow::Result<bool> {
		if matches!(self.db.read(async |tx| tx.expiry_owner(key, self.config.network).await).await?,
			Attribution::Owned { .. }) { return Ok(true); }
		let candidate = mailbox.map(|id| id.as_pubkey().to_string());
		let mailboxes = self.db.read(async |tx| {
			tx.query("
				SELECT mailbox_pk FROM fallback_record
				ORDER BY (mailbox_pk = $1) DESC NULLS LAST, id
			", &[&candidate]).await?.into_iter().map(|row|
				PublicKey::from_str(row.get("mailbox_pk")).map_err(Into::into)
			).collect::<anyhow::Result<Vec<_>>>()
		}).await?;
		for mailbox in mailboxes {
			let record = self.db.read(async |tx| tx.get_fallback(mailbox).await).await?
				.context("owner registration disappeared")?;
			record.verify(self.config.network, mailbox)?;
			// Always begin at zero. An earlier unused address can receive later.
			let mut matched = false;
			let mut start_index = 0;
			let mut cursor_vtxo_id = None;
			loop {
				let page = self.recover_vtxos_by_xpub(record.account_xpub, start_index, cursor_vtxo_id).await?;
				let mut keys = page.vtxos.iter().map(|found|
					(found.vtxo.user_pubkey(), found.derivation_index)
				).collect::<BTreeMap<_, _>>();
				let next_index = start_index.checked_add(page.nb_keys_scanned)
					.context("owner recovery index overflow")?;
				if htlc {
					// Recovery returns plain-key policies. Compare an HTLC key only
					// over this page's covered range, including a split cursor key.
					let end = next_index.checked_add(u32::from(page.cursor_vtxo_id.is_some()))
						.context("owner recovery range overflow")?;
					let xpub = record.account_xpub;
					let matched = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<u32>> {
						for index in start_index..end {
							let child = xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(index)?)?;
							if child.public_key == key { return Ok(Some(index)); }
						}
						Ok(None)
					}).await.context("HTLC owner derivation task failed")??;
					if let Some(index) = matched { keys.insert(key, index); }
				}
				matched |= keys.contains_key(&key);
				self.db.write(async |tx| {
					for (key, index) in keys { tx.cache_expiry_owner(key, mailbox, index).await?; }
					Ok(())
				}).await?;
				if !page.have_more { break; }
				ensure!(next_index > start_index || page.cursor_vtxo_id != cursor_vtxo_id,
					"owner recovery page made no progress");
				start_index = next_index;
				cursor_vtxo_id = page.cursor_vtxo_id;
			}
			if matched { return Ok(true); }
		}
		Ok(false)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn owner_lookup_discards_stale_unrelated_misses() {
		let key = |value| PublicKey::from_secret_key(&SECP,
			&bitcoin::secp256k1::SecretKey::from_slice(&[value; 32]).unwrap());
		let running = key(1);
		let recent = key(2);
		let expired = key(3);
		let old_registration = key(4);
		let mut lookups = HashMap::from([
			(running, OwnerLookup::Running),
			(recent, OwnerLookup::NoMatch { registrations: 2, checked_at: Instant::now() }),
			(expired, OwnerLookup::NoMatch {
				registrations: 2, checked_at: Instant::now() - Duration::from_secs(86_400),
			}),
			(old_registration, OwnerLookup::NoMatch { registrations: 1, checked_at: Instant::now() }),
		]);
		OwnerLookup::retain_current(&mut lookups, 2);
		assert_eq!(lookups.len(), 2);
		assert!(lookups.contains_key(&running));
		assert!(lookups.contains_key(&recent));
		assert!(!lookups.contains_key(&expired));
		assert!(!lookups.contains_key(&old_registration));
		OwnerLookup::retain_current(&mut lookups, 3);
		assert_eq!(lookups.len(), 1);
		assert!(lookups.contains_key(&running));
	}

	#[test]
	fn owner_lookup_retries_after_registration_or_one_day() {
		let recent = OwnerLookup::NoMatch { registrations: 2, checked_at: Instant::now() };
		assert!(!recent.needs_retry(2));
		assert!(recent.needs_retry(3));
		let yesterday = OwnerLookup::NoMatch {
			registrations: 2, checked_at: Instant::now() - Duration::from_secs(86_400),
		};
		assert!(yesterday.needs_retry(2));
		assert!(!OwnerLookup::Running.needs_retry(3));
	}
}
