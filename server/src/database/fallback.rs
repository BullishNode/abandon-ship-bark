//! Immutable authenticated payout registrations, independent of upstream migrations.

use std::str::FromStr;

use anyhow::Context;
use bitcoin::bip32::Xpub;
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::secp256k1::{schnorr, PublicKey};
use bitcoin::{Network, ScriptBuf};

use ark::fallback::FallbackRecord;

use crate::error::{badarg, ContextExt};

use super::Tx;

/// Serializes registration with the short payout decision transaction. The lock
/// is shared for decisions and exclusive for a new immutable registration.
const REGISTRY_LOCK: i64 = 3;

impl Tx<'_> {
	pub async fn lock_fallback_registry(&self) -> anyhow::Result<()> {
		self.execute("SELECT pg_advisory_xact_lock_shared($1)", &[&REGISTRY_LOCK]).await?;
		Ok(())
	}

	pub async fn fallback_registry_version(&self) -> anyhow::Result<i64> {
		Ok(self.query_one("SELECT COALESCE(MAX(id), 0) FROM fallback_record", &[]).await?.get(0))
	}

	/// The first valid mailbox binding wins. A retry returns its exact canonical
	/// signatures; attempting to reuse an account with another mailbox is refused.
	pub async fn set_fallback(
		&self, mailbox_pk: PublicKey, record: &FallbackRecord, network: Network,
	) -> anyhow::Result<FallbackRecord> {
		record.verify(network, mailbox_pk).badarg("invalid fallback record")?;
		self.execute("SELECT pg_advisory_xact_lock($1)", &[&REGISTRY_LOCK]).await?;
		if let Some(stored) = self.get_fallback(mailbox_pk).await? { return Ok(stored); }
		let account_xpub = record.account_xpub.to_string();
		let account_pubkey = record.account_xpub.public_key.to_string();
		let account_chain_code = record.account_xpub.chain_code.to_string();
		let reused = self.query_opt("SELECT id FROM fallback_record WHERE account_pubkey = $1 AND account_chain_code = $2",
			&[&account_pubkey, &account_chain_code]).await?;
		if reused.is_some() { return badarg!("fallback account already registered to another mailbox"); }
		self.query_one("INSERT INTO fallback_record (mailbox_pk, spk, account_xpub, account_pubkey, account_chain_code, mailbox_sig, account_sig)
			VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id", &[
			&mailbox_pk.to_string(), &record.spk.as_bytes().to_lower_hex_string(), &account_xpub,
			&account_pubkey, &account_chain_code, &record.mailbox_signature.to_string(), &record.account_signature.to_string(),
		]).await.context("failed to store fallback record")?;
		self.add_expiry_scan_record(mailbox_pk).await?;
		Ok(record.clone())
	}

	pub async fn get_fallback(&self, mailbox_pk: PublicKey) -> anyhow::Result<Option<FallbackRecord>> {
		self.query_opt("SELECT spk, account_xpub, mailbox_sig, account_sig FROM fallback_record
			WHERE mailbox_pk = $1", &[&mailbox_pk.to_string()]).await?
			.map(|row| -> anyhow::Result<_> { Ok(FallbackRecord {
				spk: ScriptBuf::from(Vec::<u8>::from_hex(row.get::<_, &str>("spk"))?),
				account_xpub: Xpub::from_str(row.get("account_xpub"))?,
				mailbox_signature: schnorr::Signature::from_str(row.get("mailbox_sig"))?,
				account_signature: schnorr::Signature::from_str(row.get("account_sig"))?,
			}) }).transpose()
	}
}
