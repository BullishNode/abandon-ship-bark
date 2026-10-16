//! One authenticated, immutable expiry destination and public coin account.

pub(crate) mod payouts;
mod refusal;
mod sync;
mod reconcile;
pub use refusal::FallbackRefusal;

use std::time::Duration;

use anyhow::Context;
use bitcoin::{Network, Script, ScriptBuf};

use ark::ProtocolEncoding;
use server_rpc::{ServerConnection, protos};

use crate::{Wallet, WalletSeed};
use crate::persist::BarkPersister;
use crate::persist::models::FallbackRecord;

pub(crate) fn next_key_index(last: Option<u32>) -> anyhow::Result<u32> {
	let next = last.map(|i| i.checked_add(1).context("vtxo key index exhausted"))
		.transpose()?.unwrap_or(0);
	ensure!(next < (1 << 31), "vtxo key index exhausted");
	Ok(next)
}

fn authenticate_binding(record: &FallbackRecord, seed: &WalletSeed, network: Network, expected: &Script) -> anyhow::Result<()> {
	record.verify(network, seed.to_mailbox_keypair().public_key())?;
	ensure!(record.spk.as_script() == expected, "fallback record differs from the exact expected destination");
	ensure!(record.account_xpub == bitcoin::bip32::Xpub::from_priv(&crate::SECP, &seed.vtxo),
		"fallback record is not this seed's coin account");
	Ok(())
}

fn validate_destination(network: Network, script: &Script) -> anyhow::Result<()> {
	bitcoin::Address::from_script(script, network).context("unsupported fallback address script")?;
	Ok(())
}

async fn persist_native_identity(db: &dyn BarkPersister, seed: &WalletSeed, record: &FallbackRecord) -> anyhow::Result<()> {
	// Child zero is the existing stable wallet identity and the account
	// possession signer. Reserve it once, not as a replenishable signing pool.
	db.store_vtxo_key(0, seed.derive_vtxo_keypair(0).public_key()).await?;
	db.store_fallback_record(record).await
}

pub(crate) async fn register(
	network: Network, seed: &WalletSeed, db: &dyn BarkPersister,
	connection: &mut ServerConnection,
	expected: &Script,
) -> anyhow::Result<()> {
	let spk = expected.to_owned();
	validate_destination(network, &spk)?;
	if let Some(stored) = db.get_fallback_record().await? { authenticate_binding(&stored, seed, network, expected)?; }
	let key = seed.to_mailbox_keypair();
	let proposed = FallbackRecord::new(network, spk, &seed.vtxo, &key)?;
	// Deterministic binding makes a lost reply safe to retry, including from a
	// different device. Adopt its canonical signatures only after authenticating
	// the exact seed-derived fields; a mismatch never overwrites local state.
	let reply = connection.client.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: key.public_key().serialize().to_vec(), record: proposed.serialize(),
	}).await.context("failed to register immutable fallback account")?.into_inner();
	let current = FallbackRecord::deserialize(&reply.record)?;
	authenticate_binding(&current, seed, network, expected)?;
	persist_native_identity(db, seed, &current).await
}

impl Wallet {
	/// Return the authenticated registration, if this wallet has one.
	pub async fn fallback_registration(&self) -> anyhow::Result<Option<FallbackRecord>> {
		let Some(record) = self.inner.db.get_fallback_record().await? else { return Ok(None); };
		record.verify(self.network().await?, self.inner.seed.to_mailbox_keypair().public_key())?;
		ensure!(record.account_xpub == self.vtxo_xpub(), "fallback record is not this seed's coin account");
		Ok(Some(record))
	}

	/// The authenticated immutable destination, suitable for recovery evidence.
	pub async fn fallback_destination(&self) -> anyhow::Result<FallbackRecord> {
		self.fallback_registration().await?.context("no fallback account registered")
	}

	pub(crate) async fn open_fallback(&self, requested: Option<ScriptBuf>) -> anyhow::Result<()> {
		let stored = self.inner.db.get_fallback_record().await?;
		let network = self.network().await?;
		if let Some(record) = stored {
			let expected = requested.as_deref().unwrap_or(record.spk.as_script());
			validate_destination(network, expected)?;
			authenticate_binding(&record, &self.inner.seed, network, expected)?;
			persist_native_identity(&*self.inner.db, &self.inner.seed, &record).await?;
			// A cached authenticated binding permits offline/seed-only status
			// opens. An app that supplies its destination retries registration
			// when connected, including after a server restore.
			if requested.is_some() {
				if let Ok((mut connection, _)) = self.require_server().await {
					register(network, &self.inner.seed, &*self.inner.db, &mut connection, expected).await?;
				}
			}
			return Ok(());
		}
		// Choosing and restoring the destination belongs to the app. Opening
		// without one remains useful for read-only recovery/status, while the
		// existing registration fence prevents issuing or spending coin keys.
		let Some(expected) = requested else { return Ok(()); };
		validate_destination(network, &expected)?;
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let (mut connection, _) = self.require_server().await?;
		register(network, &self.inner.seed, &*self.inner.db, &mut connection, &expected).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fallback_accepts_application_standard_scripts_without_derivation_rules() {
		let seed = WalletSeed::new_from_seed(Network::Regtest, &[17; 64]);
		for hex in [
			format!("76a914{}88ac", "11".repeat(20)),
			format!("a914{}87", "22".repeat(20)),
			format!("0014{}", "33".repeat(20)),
			format!("0020{}", "44".repeat(32)),
			format!("5120{}", "55".repeat(32)),
		] {
			let script = ScriptBuf::from_hex(&hex).unwrap();
			validate_destination(Network::Regtest, &script).unwrap();
			let record = FallbackRecord::new(Network::Regtest, script.clone(),
				&seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
			authenticate_binding(&record, &seed, Network::Regtest, &script).unwrap();
		}
		assert!(validate_destination(Network::Regtest, &ScriptBuf::from_bytes(vec![0x6a])).is_err());
	}

	#[test]
	fn restore_requires_exact_seed_binding_and_network() {
		let seed = WalletSeed::new_from_seed(Network::Regtest, &[17; 64]);
		let expected = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let record = FallbackRecord::new(Network::Regtest, expected.clone(),
			&seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&record, &seed, Network::Regtest, &expected).is_ok());
		assert!(authenticate_binding(&record, &seed, Network::Signet, &expected).is_err());
		let other_script = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
		let wrong = FallbackRecord::new(Network::Regtest, other_script, &seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &expected).is_err());
		let other = WalletSeed::new_from_seed(Network::Regtest, &[18; 64]);
		let wrong = FallbackRecord::new(Network::Regtest, record.spk.clone(), &other.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &expected).is_err());
		assert_eq!(next_key_index(None).unwrap(), 0);
		assert_eq!(next_key_index(Some((1 << 31) - 2)).unwrap(), (1 << 31) - 1);
		assert!(next_key_index(Some((1 << 31) - 1)).is_err());
	}
}
