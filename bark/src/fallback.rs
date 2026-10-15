//! One authenticated immutable expiry destination and durable individual key links.

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

fn authenticate_binding(record: &FallbackRecord, seed: &WalletSeed, network: Network, expected: &Script) -> anyhow::Result<()> {
	record.verify(network, seed.to_mailbox_keypair().public_key())?;
	ensure!(record.spk.as_script() == expected, "fallback record differs from the exact expected destination");
	Ok(())
}

pub(crate) async fn expected_destination(
	network: Network, seed: &WalletSeed, onchain: &Arc<RwLock<dyn OnchainWalletTrait>>,
	requested: Option<&ScriptBuf>,
) -> anyhow::Result<ScriptBuf> {
	#[cfg(feature = "onchain-bdk")]
	let native = {
		let guard = onchain.read().await;
		(&*guard as &dyn Any).is::<OnchainWallet>()
	};
	#[cfg(not(feature = "onchain-bdk"))]
	let native = false;
	let expected = if native {
		let spk = seed.fallback_script(network)?;
		ensure!(requested.is_none_or(|requested| *requested == spk), "native fallback must be this seed's BIP84 external index 0");
		spk
	} else {
		requested.cloned().context("an external callback wallet requires an explicit immutable fallback_spk")?
	};
	bitcoin::Address::from_script(&expected, network).context("unsupported fallback address script")?;
	ensure!(onchain.read().await.is_mine(&expected).await?, "fallback destination is not owned by the onchain wallet");
	Ok(expected)
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

async fn persist_native_identity(db: &dyn BarkPersister, seed: &WalletSeed, record: &FallbackRecord) -> anyhow::Result<()> {
	db.store_vtxo_key(0, seed.derive_vtxo_keypair(0).public_key()).await?;
	db.store_fallback_record(record).await
}

pub(crate) async fn register(
	network: Network, seed: &WalletSeed, db: &dyn BarkPersister,
	onchain: &Arc<RwLock<dyn OnchainWalletTrait>>, connection: &mut ServerConnection,
	expected: &Script,
) -> anyhow::Result<()> {
	ensure!(onchain.read().await.is_mine(expected).await?, "fallback destination is not owned by the onchain wallet");
	if let Some(stored) = db.get_fallback_record().await? { authenticate_binding(&stored, seed, network, expected)?; }
	let mailbox = seed.to_mailbox_keypair();
	let proposed = FallbackRecord::new(network, expected.to_owned(), &mailbox)?;
	let first = next_key_index(db.get_last_vtxo_key_index().await?)?.max(1);
	let end = first.checked_add(50).context("fallback key pool exhausted")?.min(1 << 31);
	let indexes = std::iter::once(0).chain(first..end);
	let mut requested = Vec::new();
	for index in indexes {
		let coin = seed.derive_vtxo_keypair(index);
		if let Some(link) = db.get_vtxo_key_link(&coin.public_key()).await? {
			link.verify(network, mailbox.public_key(), &proposed)?;
			ensure!(link.recovery_index == Some(index), "stored key link index differs from this seed");
			continue;
		}
		requested.push((index, ark::fallback::KeyLink::new(network, mailbox.public_key(), &proposed, &coin, Some(index))?));
	}
	let reply = connection.client.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: proposed.serialize(),
		key_links: requested.iter().map(|(_, link)| link.serialize()).collect(),
	}).await.context("failed to register fallback destination and key links")?.into_inner();
	let current = FallbackRecord::deserialize(&reply.record)?;
	authenticate_binding(&current, seed, network, expected)?;
	ensure!(reply.key_links.len() == requested.len(), "fallback acknowledgment has a different key set");
	let mut canonical = Vec::with_capacity(requested.len());
	for ((index, expected), bytes) in requested.iter().zip(&reply.key_links) {
		let link = ark::fallback::KeyLink::deserialize(bytes)?;
		link.verify(network, mailbox.public_key(), &current)?;
		ensure!(link.same_binding(expected), "fallback acknowledgment changes the requested key");
		canonical.push((*index, link));
	}
	persist_native_identity(db, seed, &current).await?;
	db.store_linked_vtxo_keys(&canonical).await?;
	mark_reserved(onchain, &current.spk).await
}

impl Wallet {
	/// Return the exact canonical proof acknowledged before this key was used.
	pub async fn fallback_key_link(&self, key: bitcoin::secp256k1::PublicKey) -> anyhow::Result<ark::fallback::KeyLink> {
		let record = self.fallback_destination().await?;
		let link = self.inner.db.get_vtxo_key_link(&key).await?.context("coin key is not linked to its fallback destination")?;
		link.verify(self.network().await?, self.mailbox_keypair().public_key(), &record)?;
		ensure!(link.user_pubkey == key, "fallback link is for another policy key");
		Ok(link)
	}

	pub async fn fallback_destination(&self) -> anyhow::Result<FallbackRecord> {
		let record = self.inner.db.get_fallback_record().await?.context("no fallback destination registered")?;
		authenticate_binding(&record, &self.inner.seed, self.network().await?, &self.inner.fallback_spk)?;
		Ok(record)
	}

	pub async fn set_fallback_destination(&self, spk: ScriptBuf) -> anyhow::Result<()> {
		ensure!(spk == self.inner.fallback_spk, "immutable fallback destination mismatch");
		self.sync_fallback().await
	}

	pub(crate) async fn open_fallback(&self, requested: Option<ScriptBuf>) -> anyhow::Result<()> {
		let onchain = self.inner.onchain.as_ref().context("onchain wallet required")?;
		if let Some(record) = self.inner.db.get_fallback_record().await? {
			authenticate_binding(&record, &self.inner.seed, self.network().await?, &self.inner.fallback_spk)?;
			ensure!(requested.as_ref().is_none_or(|spk| *spk == record.spk), "immutable fallback destination mismatch");
			persist_native_identity(&*self.inner.db, &self.inner.seed, &record).await?;
			return mark_reserved(onchain, &record.spk).await;
		}
		self.sync_fallback().await
	}

	pub(crate) async fn sync_fallback(&self) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(&format!("{}.key-pool", self.fingerprint()), Duration::from_secs(30)).await?;
		self.sync_fallback_locked().await
	}

	pub(crate) async fn sync_fallback_locked(&self) -> anyhow::Result<()> {
		let (mut connection, _) = self.require_server().await?;
		register(self.network().await?, &self.inner.seed, &*self.inner.db,
			self.inner.onchain.as_ref().context("onchain wallet required")?, &mut connection, &self.inner.fallback_spk).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn restore_requires_exact_seed_binding_and_network() {
		let seed = WalletSeed::new_from_seed(Network::Regtest, &[17; 64]);
		let expected = seed.fallback_script(Network::Regtest).unwrap();
		let record = FallbackRecord::new(Network::Regtest, expected.clone(), &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&record, &seed, Network::Regtest, &expected).is_ok());
		assert!(authenticate_binding(&record, &seed, Network::Signet, &expected).is_err());
		let wrong_script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let wrong = FallbackRecord::new(Network::Regtest, wrong_script, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &expected).is_err());
		let other = WalletSeed::new_from_seed(Network::Regtest, &[18; 64]);
		let wrong = FallbackRecord::new(Network::Regtest, expected.clone(), &other.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &expected).is_err());
	}

	#[test]
	fn native_recovery_index_boundary_is_non_hardened() {
		assert_eq!(next_key_index(None).unwrap(), 0);
		assert_eq!(next_key_index(Some((1 << 31) - 2)).unwrap(), (1 << 31) - 1);
		assert!(next_key_index(Some((1 << 31) - 1)).is_err());
		assert!(next_key_index(Some(u32::MAX)).is_err());
	}
}
