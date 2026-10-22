//! Authenticated payout owner matches.

use std::str::FromStr;

use anyhow::Context;
use bitcoin::bip32::ChildNumber;
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Network, ScriptBuf};

use ark::mailbox::MailboxIdentifier;
use ark::SECP;

use super::Tx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
	Owned { mailbox_pk: PublicKey, child_index: u32, spk: ScriptBuf },
	Unresolved,
}

impl Tx<'_> {
	pub(crate) async fn cache_expiry_owner(
		&self, user_pubkey: PublicKey, mailbox_pk: PublicKey, child_index: u32,
	) -> anyhow::Result<()> {
		let key = user_pubkey.to_string();
		let mailbox = mailbox_pk.to_string();
		let index = i64::from(child_index);
		self.execute("
			INSERT INTO expiry_owner_key (user_pubkey, mailbox_pk, child_index)
			VALUES ($1, $2, $3) ON CONFLICT (user_pubkey) DO NOTHING
		", &[&key, &mailbox, &index]).await?;
		let owner = self.query_one("
			SELECT mailbox_pk, child_index FROM expiry_owner_key WHERE user_pubkey=$1
		", &[&key]).await?;
		ensure!(owner.get::<_, &str>("mailbox_pk") == mailbox
			&& owner.get::<_, i64>("child_index") == index,
			"coin key derives from conflicting registered accounts");
		Ok(())
	}

	/// Try the mailbox's first 32 keys. The mailbox alone is not an owner proof.
	pub(crate) async fn queue_expiry_owner_with_candidate_proof(
		&self, user_pubkey: PublicKey, mailbox: Option<MailboxIdentifier>,
	) -> anyhow::Result<()> {
		if self.query_opt("SELECT user_pubkey FROM expiry_owner_key WHERE user_pubkey=$1",
			&[&user_pubkey.to_string()]).await?.is_some() { return Ok(()); }
		let Some(mailbox_pk) = mailbox.map(|id| id.as_pubkey()) else { return Ok(()); };
		let Some(record) = self.get_fallback(mailbox_pk).await? else { return Ok(()); };
		let index = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<u32>> {
			for index in 0..32 {
				let child = record.account_xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(index)?)?;
				if child.public_key == user_pubkey { return Ok(Some(index)); }
			}
			Ok(None)
		}).await.context("mailbox owner derivation task failed")??;
		if let Some(index) = index {
			self.cache_expiry_owner(user_pubkey, mailbox_pk, index).await?;
		}
		Ok(())
	}

	/// Recheck the immutable registration and its child key before each payout.
	pub async fn expiry_owner(&self, user_pubkey: PublicKey, network: Network) -> anyhow::Result<Attribution> {
		self.lock_fallback_registry().await?;
		let Some(row) = self.query_opt("
			SELECT mailbox_pk, child_index FROM expiry_owner_key WHERE user_pubkey=$1
		", &[&user_pubkey.to_string()]).await? else { return Ok(Attribution::Unresolved); };
		let mailbox_pk = PublicKey::from_str(row.get("mailbox_pk"))?;
		let child_index = u32::try_from(row.get::<_, i64>("child_index"))?;
		let record = self.get_fallback(mailbox_pk).await?.context("owner registration disappeared")?;
		record.verify(network, mailbox_pk)?;
		let child = record.account_xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(child_index)?)?;
		ensure!(child.public_key == user_pubkey, "cached fallback owner does not derive this key");
		Ok(Attribution::Owned { mailbox_pk, child_index, spk: record.spk })
	}
}
