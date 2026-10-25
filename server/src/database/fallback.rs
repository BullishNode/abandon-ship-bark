//! Immutable authenticated payout registrations, independent of upstream migrations.

use std::str::FromStr;

use anyhow::Context;
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::secp256k1::{schnorr, PublicKey};
use bitcoin::{Network, ScriptBuf};

use ark::fallback::FallbackRegistration;

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

	pub(super) async fn lock_fallback_registry_writer(&self) -> anyhow::Result<()> {
		self.execute("SELECT pg_advisory_xact_lock($1)", &[&REGISTRY_LOCK]).await?;
		Ok(())
	}

	/// The first valid mailbox binding wins. A same-binding retry returns the
	/// canonical signature even when the caller signs with new randomness.
	pub async fn set_fallback(
		&self, mailbox_pk: PublicKey, record: &FallbackRegistration, network: Network,
	) -> anyhow::Result<FallbackRegistration> {
		record.verify(network, mailbox_pk).badarg("invalid fallback record")?;
		self.lock_fallback_registry_writer().await?;
		if let Some(stored) = self.get_fallback(mailbox_pk).await? {
			stored.verify(network, mailbox_pk).context("invalid stored fallback registration")?;
			if !stored.same_binding(record) { return badarg!("immutable fallback destination differs"); }
			return Ok(stored);
		}
		self.query_one("
			INSERT INTO fallback_record (mailbox_pk, spk, mailbox_sig)
			VALUES ($1, $2, $3) RETURNING id
		", &[
			&mailbox_pk.to_string(), &record.spk.as_bytes().to_lower_hex_string(), &record.mailbox_signature.to_string(),
		]).await.context("failed to store fallback record")?;
		Ok(record.clone())
	}

	pub async fn get_fallback(&self, mailbox_pk: PublicKey) -> anyhow::Result<Option<FallbackRegistration>> {
		self.query_opt("
			SELECT spk, mailbox_sig FROM fallback_record
			WHERE mailbox_pk = $1
		", &[&mailbox_pk.to_string()]).await?
			.map(|row| -> anyhow::Result<_> { Ok(FallbackRegistration {
				spk: ScriptBuf::from(Vec::<u8>::from_hex(row.get::<_, &str>("spk"))?),
				mailbox_signature: schnorr::Signature::from_str(row.get("mailbox_sig"))?,
			}) }).transpose()
	}
}
