//! One authenticated, immutable expiry destination and public coin account.

#[cfg(feature = "onchain-bdk")]
use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bitcoin::{Network, Script, ScriptBuf};
use tokio::sync::RwLock;

use ark::ProtocolEncoding;
use server_rpc::{ServerConnection, protos};

use crate::{Wallet, WalletSeed};
use crate::onchain::OnchainWalletTrait;
#[cfg(feature = "onchain-bdk")]
use crate::onchain::OnchainWallet;
use crate::persist::BarkPersister;
use crate::persist::models::FallbackRecord;

pub(crate) fn next_key_index(last: Option<u32>) -> anyhow::Result<u32> {
	let next = last.map(|i| i.checked_add(1).context("vtxo key index exhausted"))
		.transpose()?.unwrap_or(0);
	ensure!(next < (1 << 31), "vtxo key index exhausted");
	Ok(next)
}

fn authenticate_native(record: &FallbackRecord, seed: &WalletSeed, network: Network) -> anyhow::Result<()> {
	record.verify(network, seed.to_mailbox_keypair().public_key())?;
	ensure!(record.spk == seed.fallback_script(network)?, "fallback record is not this seed's BIP84 external index 0");
	ensure!(record.account_xpub == bitcoin::bip32::Xpub::from_priv(&crate::SECP, &seed.vtxo),
		"fallback record is not this seed's coin account");
	Ok(())
}

async fn mark_reserved(onchain: &Arc<RwLock<dyn OnchainWalletTrait>>, spk: &Script) -> anyhow::Result<()> {
	#[cfg(feature = "onchain-bdk")]
	{
		let mut guard = onchain.write().await;
		if let Some(wallet) = (&mut *guard as &mut dyn Any).downcast_mut::<OnchainWallet>() {
			wallet.mark_fallback_used(spk).await?;
		}
	}
	#[cfg(not(feature = "onchain-bdk"))]
	let _ = (onchain, spk);
	Ok(())
}

pub(crate) async fn register(
	network: Network, seed: &WalletSeed, db: &dyn BarkPersister,
	onchain: &Arc<RwLock<dyn OnchainWalletTrait>>, connection: &mut ServerConnection,
	requested: Option<ScriptBuf>,
) -> anyhow::Result<()> {
	let spk = seed.fallback_script(network)?;
	ensure!(requested.as_ref().is_none_or(|requested| *requested == spk),
		"native fallback destination must be this seed's BIP84 external index 0");
	ensure!(onchain.read().await.is_mine(&spk).await?, "fallback destination is not owned by the onchain wallet");
	if let Some(stored) = db.get_fallback_record().await? { authenticate_native(&stored, seed, network)?; }
	let key = seed.to_mailbox_keypair();
	let proposed = FallbackRecord::new(network, spk, &seed.vtxo, &key)?;
	// Deterministic binding makes a lost reply safe to retry, including from a
	// different device. Adopt its canonical signatures only after authenticating
	// the exact seed-derived fields; a mismatch never overwrites local state.
	let reply = connection.client.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: key.public_key().serialize().to_vec(), record: proposed.serialize(),
	}).await.context("failed to register immutable fallback account")?.into_inner();
	let current = FallbackRecord::deserialize(&reply.record)?;
	authenticate_native(&current, seed, network)?;
	db.store_fallback_record(&current).await?;
	mark_reserved(onchain, &current.spk).await
}

impl Wallet {
	pub async fn fallback_destination(&self) -> anyhow::Result<FallbackRecord> {
		let record = self.inner.db.get_fallback_record().await?.context("no fallback account registered")?;
		authenticate_native(&record, &self.inner.seed, self.network().await?)?;
		Ok(record)
	}

	pub async fn set_fallback_destination(&self, spk: ScriptBuf) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let (mut connection, _) = self.require_server().await?;
		register(self.network().await?, &self.inner.seed, &*self.inner.db,
			self.inner.onchain.as_ref().context("onchain wallet required")?, &mut connection, Some(spk)).await
	}

	pub(crate) async fn open_fallback(&self, requested: Option<ScriptBuf>) -> anyhow::Result<()> {
		let onchain = self.inner.onchain.as_ref().context("onchain wallet required")?;
		if let Some(record) = self.inner.db.get_fallback_record().await? {
			authenticate_native(&record, &self.inner.seed, self.network().await?)?;
			ensure!(requested.as_ref().is_none_or(|spk| *spk == record.spk), "immutable fallback destination mismatch");
			return mark_reserved(onchain, &record.spk).await;
		}
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let (mut connection, _) = self.require_server().await?;
		register(self.network().await?, &self.inner.seed, &*self.inner.db, onchain, &mut connection, requested).await
	}

	pub(crate) async fn sync_fallback(&self) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let (mut connection, _) = self.require_server().await?;
		register(self.network().await?, &self.inner.seed, &*self.inner.db,
			self.inner.onchain.as_ref().context("onchain wallet required")?, &mut connection, None).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn restore_requires_exact_seed_binding_and_network() {
		let seed = WalletSeed::new_from_seed(Network::Regtest, &[17; 64]);
		let record = FallbackRecord::new(Network::Regtest, seed.fallback_script(Network::Regtest).unwrap(),
			&seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_native(&record, &seed, Network::Regtest).is_ok());
		assert!(authenticate_native(&record, &seed, Network::Signet).is_err());
		let other_script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let wrong = FallbackRecord::new(Network::Regtest, other_script, &seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_native(&wrong, &seed, Network::Regtest).is_err());
		let other = WalletSeed::new_from_seed(Network::Regtest, &[18; 64]);
		let wrong = FallbackRecord::new(Network::Regtest, record.spk.clone(), &other.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_native(&wrong, &seed, Network::Regtest).is_err());
		assert_eq!(next_key_index(None).unwrap(), 0);
		assert_eq!(next_key_index(Some((1 << 31) - 2)).unwrap(), (1 << 31) - 1);
		assert!(next_key_index(Some((1 << 31) - 1)).is_err());
	}
}
