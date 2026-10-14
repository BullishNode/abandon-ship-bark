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

fn authenticate_binding(record: &FallbackRecord, seed: &WalletSeed, network: Network, expected: &Script) -> anyhow::Result<()> {
	record.verify(network, seed.to_mailbox_keypair().public_key())?;
	ensure!(record.spk.as_script() == expected, "fallback record differs from the exact expected destination");
	ensure!(record.account_xpub == bitcoin::bip32::Xpub::from_priv(&crate::SECP, &seed.vtxo),
		"fallback record is not this seed's coin account");
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
	// Child zero is the existing stable phone-login identity and the account
	// possession signer. Reserve it once, not as a replenishable signing pool.
	db.store_vtxo_key(0, seed.derive_vtxo_keypair(0).public_key()).await?;
	db.store_fallback_record(record).await
}

pub(crate) async fn register(
	network: Network, seed: &WalletSeed, db: &dyn BarkPersister,
	onchain: &Arc<RwLock<dyn OnchainWalletTrait>>, connection: &mut ServerConnection,
	expected: &Script,
) -> anyhow::Result<()> {
	let spk = expected.to_owned();
	ensure!(onchain.read().await.is_mine(&spk).await?, "fallback destination is not owned by the onchain wallet");
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
	persist_native_identity(db, seed, &current).await?;
	mark_reserved(onchain, &current.spk).await
}

impl Wallet {
	pub(crate) async fn attach_arkoor_origins(
		&self, request: &mut protos::ArkoorPackageCosignRequest,
		recipient: Option<(bitcoin::secp256k1::PublicKey, ark::fallback::AccountChildHint)>,
	) -> anyhow::Result<()> {
		for part in &mut request.parts {
			for output in part.outputs.iter_mut().chain(part.isolated_outputs.iter_mut()) {
				let policy_key = ark::VtxoPolicy::deserialize(&output.policy)?.user_pubkey();
				let hint = match recipient {
					Some((key, hint)) if key == policy_key => hint,
					_ => self.fallback_child_hint(policy_key).await?,
				};
				output.origin = Some(hint.into());
			}
		}
		Ok(())
	}

	/// Public attribution for an already allocated native coin key. It carries
	/// no secret and never registers or replenishes a server-side key pool.
	pub async fn fallback_child_hint(&self, policy_key: bitcoin::secp256k1::PublicKey) -> anyhow::Result<ark::fallback::AccountChildHint> {
		self.fallback_destination().await?;
		let index = self.inner.db.get_public_key_idx(&policy_key).await?.context("coin key has no persisted derivation index")?;
		ensure!(index < (1 << 31), "hardened coin keys are not supported");
		let keypair = self.inner.seed.derive_vtxo_keypair(index);
		ensure!(keypair.public_key() == policy_key, "coin key does not match this seed");
		let properties = self.properties().await?;
		let server_mailbox_key = match properties.server_mailbox_pubkey {
			Some(key) => key,
			None => self.require_server().await?.1.mailbox_pubkey,
		};
		let blinded = self.mailbox_identifier().to_blinded(server_mailbox_key, &keypair)?;
		Ok(ark::fallback::AccountChildHint::new(blinded, index)?)
	}

	pub async fn fallback_destination(&self) -> anyhow::Result<FallbackRecord> {
		let record = self.inner.db.get_fallback_record().await?.context("no fallback account registered")?;
		authenticate_binding(&record, &self.inner.seed, self.network().await?, &self.inner.fallback_spk)?;
		Ok(record)
	}

	pub async fn set_fallback_destination(&self, spk: ScriptBuf) -> anyhow::Result<()> {
		ensure!(spk == self.inner.fallback_spk, "immutable fallback destination mismatch");
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let (mut connection, _) = self.require_server().await?;
		register(self.network().await?, &self.inner.seed, &*self.inner.db,
			self.inner.onchain.as_ref().context("onchain wallet required")?, &mut connection, &self.inner.fallback_spk).await
	}

	pub(crate) async fn open_fallback(&self, requested: Option<ScriptBuf>) -> anyhow::Result<()> {
		let onchain = self.inner.onchain.as_ref().context("onchain wallet required")?;
		if let Some(record) = self.inner.db.get_fallback_record().await? {
			authenticate_binding(&record, &self.inner.seed, self.network().await?, &self.inner.fallback_spk)?;
			ensure!(requested.as_ref().is_none_or(|spk| *spk == record.spk), "immutable fallback destination mismatch");
			persist_native_identity(&*self.inner.db, &self.inner.seed, &record).await?;
			return mark_reserved(onchain, &record.spk).await;
		}
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let (mut connection, _) = self.require_server().await?;
		register(self.network().await?, &self.inner.seed, &*self.inner.db, onchain, &mut connection, &self.inner.fallback_spk).await
	}

	pub(crate) async fn sync_fallback(&self) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
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
		let record = FallbackRecord::new(Network::Regtest, seed.fallback_script(Network::Regtest).unwrap(),
			&seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&record, &seed, Network::Regtest, &seed.fallback_script(Network::Regtest).unwrap()).is_ok());
		assert!(authenticate_binding(&record, &seed, Network::Signet, &seed.fallback_script(Network::Signet).unwrap()).is_err());
		let other_script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let wrong = FallbackRecord::new(Network::Regtest, other_script, &seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &seed.fallback_script(Network::Regtest).unwrap()).is_err());
		let other = WalletSeed::new_from_seed(Network::Regtest, &[18; 64]);
		let wrong = FallbackRecord::new(Network::Regtest, record.spk.clone(), &other.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &seed.fallback_script(Network::Regtest).unwrap()).is_err());
		assert_eq!(next_key_index(None).unwrap(), 0);
		assert_eq!(next_key_index(Some((1 << 31) - 2)).unwrap(), (1 << 31) - 1);
		assert!(next_key_index(Some((1 << 31) - 1)).is_err());
	}
}
