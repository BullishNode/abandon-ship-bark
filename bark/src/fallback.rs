//! Expiry fallback registration and keys linked before use.

#[cfg(feature = "onchain-bdk")]
use std::any::Any;
use std::cmp;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bitcoin::{Address, Network, Script, ScriptBuf};
use bitcoin::constants::ChainHash;
use bitcoin::secp256k1::{Keypair, PublicKey};
use tokio::sync::RwLock;

use ark::ProtocolEncoding;
use ark::attestations::{FallbackRecordAttestation, KeyLinkAttestation};
use server_rpc::{ServerConnection, protos};

use crate::{Wallet, WalletSeed};
use crate::onchain::OnchainWalletTrait;
#[cfg(feature = "onchain-bdk")]
use crate::onchain::OnchainWallet;
use crate::persist::BarkPersister;
use crate::persist::models::FallbackRecord;

const LINK_AHEAD: u32 = 8;

pub(crate) fn next_key_index(last: Option<u32>) -> anyhow::Result<u32> {
	let next = last.map(|i| i.checked_add(1).context("vtxo key index exhausted"))
		.transpose()?.unwrap_or(0);
	ensure!(next < (1 << 31), "vtxo key index exhausted");
	Ok(next)
}

fn record_bytes(
	record: &FallbackRecord,
	network: Network,
	server_pk: PublicKey,
	key: &Keypair,
) -> Vec<u8> {
	let chain = ChainHash::using_genesis_block_const(network);
	let mut bytes = record.spk.as_bytes().to_vec();
	bytes.extend(record.seq.to_le_bytes());
	bytes.extend(FallbackRecordAttestation::new(chain, server_pk, &record.spk, record.seq, key).serialize());
	bytes
}

/// Parse a record without checking its signature.
fn parse_record(
	bytes: &[u8],
	network: Network,
) -> anyhow::Result<(FallbackRecord, FallbackRecordAttestation)> {
	ensure!((73..=114).contains(&bytes.len()), "invalid fallback record length");
	let split = bytes.len() - 72;
	let spk = ScriptBuf::from_bytes(bytes[..split].to_vec());
	let seq = u64::from_le_bytes(bytes[split..split + 8].try_into()?);
	i64::try_from(seq).context("fallback sequence out of range")?;
	validate_script(&spk, network)?;
	let attestation = FallbackRecordAttestation::deserialize(&bytes[split + 8..])?;
	Ok((FallbackRecord { spk, seq }, attestation))
}

fn decode_record(
	bytes: &[u8],
	mailbox: PublicKey,
	network: Network,
	server_pk: PublicKey,
) -> anyhow::Result<FallbackRecord> {
	let (record, attestation) = parse_record(bytes, network)?;
	let chain = ChainHash::using_genesis_block_const(network);
	attestation.verify(chain, server_pk, &record.spk, record.seq, mailbox)
		.context("invalid fallback record signature")?;
	Ok(record)
}

fn validate_script(spk: &Script, network: Network) -> anyhow::Result<()> {
	ensure!(spk.is_p2pkh() || spk.is_p2sh() || spk.is_p2wpkh() || spk.is_p2wsh() || spk.is_p2tr(),
		"fallback destination must be a standard address");
	Address::from_script(spk, network).context("invalid fallback address")?;
	Ok(())
}

async fn reserve(onchain: &Arc<RwLock<dyn OnchainWalletTrait>>) -> anyhow::Result<ScriptBuf> {
	#[cfg(feature = "onchain-bdk")]
	{
		let mut guard = onchain.write().await;
		let wallet = (&mut *guard as &mut dyn Any).downcast_mut::<OnchainWallet>()
			.context("fallback_spk is required for an external onchain wallet")?;
		wallet.reserve_fallback_address().await
	}
	#[cfg(not(feature = "onchain-bdk"))]
	{
		let _ = onchain;
		bail!("fallback_spk is required without onchain-bdk")
	}
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

// Caller holds the creation or fallback lock. Persist before the request so a
// crash or lost reply retries the same address and sequence after reopening.
pub(crate) async fn register(
	network: Network,
	server_pubkey: PublicKey,
	seed: &WalletSeed,
	db: &dyn BarkPersister,
	onchain: &Arc<RwLock<dyn OnchainWalletTrait>>,
	connection: &mut ServerConnection,
	requested: Option<ScriptBuf>,
) -> anyhow::Result<()> {
	let stored = db.get_fallback_record().await?;
	let spk = match requested {
		Some(spk) => spk,
		None => match &stored {
			Some(record) if onchain.read().await.is_mine(&record.spk).await? => record.spk.clone(),
			_ => reserve(onchain).await?,
		},
	};
	validate_script(&spk, network)?;
	ensure!(onchain.read().await.is_mine(&spk).await?, "fallback destination is not owned by this wallet");
	let seq = match &stored {
		Some(record) if record.spk == spk => record.seq,
		_ => next_sequence(stored.as_ref().map(|r| r.seq))?,
	};
	let mut proposed = FallbackRecord { spk, seq };
	let key = seed.to_mailbox_keypair();
	let chain = ChainHash::using_genesis_block_const(network);
	// One more round than an unowned reply needs, for replacing an unbound record.
	for _ in 0..3 {
		db.store_fallback_record(&proposed).await?;
		mark_reserved(onchain, &proposed.spk).await?;
		let reply = connection.client.set_fallback(protos::SetFallbackRequest {
			mailbox_pk: key.public_key().serialize().to_vec(),
			record: Some(record_bytes(&proposed, network, server_pubkey, &key)), key_links: vec![],
		}).await.context("failed to register expiry fallback destination")?.into_inner();
		let (current, attestation) = parse_record(&reply.record, network)?;
		if attestation.verify(chain, server_pubkey, &current.spk, current.seq, key.public_key()).is_err() {
			// The server kept a record signed before records were bound to a
			// chain and server, or one for another of them. Sign ours above it.
			proposed.seq = next_sequence(Some(cmp::max(current.seq, proposed.seq)))?;
			continue;
		}
		ensure!(current.seq >= proposed.seq, "server returned an older fallback record");
		if onchain.read().await.is_mine(&current.spk).await? {
			db.store_fallback_record(&current).await?;
			mark_reserved(onchain, &current.spk).await?;
			return Ok(());
		}
		// A restored/replaced external wallet must not adopt an unowned script.
		proposed.seq = next_sequence(Some(current.seq))?;
	}
	bail!("fallback destination changed concurrently to an unowned address; retry registration")
}

fn next_sequence(previous: Option<u64>) -> anyhow::Result<u64> {
	let now = bark_runtime::timestamp_secs();
	let seq = match previous {
		Some(seq) => now.max(seq.checked_add(1).context("fallback sequence exhausted")?),
		None => now,
	};
	i64::try_from(seq).context("fallback sequence exhausted")?;
	Ok(seq)
}

impl Wallet {
	/// The current expiry destination, including a pending registration after a lost reply.
	pub async fn fallback_destination(&self) -> anyhow::Result<FallbackRecord> {
		self.inner.db.get_fallback_record().await?.context("no fallback destination registered")
	}

	/// Register an owned onchain destination for this wallet's expiry payouts.
	pub async fn set_fallback_destination(&self, spk: ScriptBuf) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(
			&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let (mut connection, ark_info) = self.require_server().await?;
		register(self.network().await?, ark_info.server_pubkey, &self.inner.seed, &*self.inner.db,
			self.inner.onchain.as_ref().context("onchain wallet required")?,
			&mut connection, Some(spk),
		).await
	}

	pub(crate) async fn open_fallback(&self, requested: Option<ScriptBuf>) -> anyhow::Result<()> {
		let onchain = self.inner.onchain.as_ref().context("onchain wallet required")?;
		if let Some(record) = self.inner.db.get_fallback_record().await? {
			if requested.as_ref().is_none_or(|spk| *spk == record.spk)
				&& onchain.read().await.is_mine(&record.spk).await?
			{
				mark_reserved(onchain, &record.spk).await?;
				return Ok(());
			}
		}
		let _guard = self.inner.lock_manager.lock(
			&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let (mut connection, ark_info) = self.require_server().await?;
		register(self.network().await?, ark_info.server_pubkey, &self.inner.seed, &*self.inner.db,
			onchain, &mut connection, requested,
		).await
	}

	pub(crate) async fn sync_fallback(&self) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(
			&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30),
		).await?;
		let (mut connection, ark_info) = self.require_server().await?;
		let server_pubkey = ark_info.server_pubkey;
		let onchain = self.inner.onchain.as_ref().context("onchain wallet required")?;
		let network = self.network().await?;
		register(network, server_pubkey, &self.inner.seed, &*self.inner.db, onchain, &mut connection, None).await?;
		let first = next_key_index(self.inner.db.get_last_vtxo_key_index().await?)?;
		let end = first.saturating_add(LINK_AHEAD).min(1 << 31);
		let mailbox = self.mailbox_keypair().public_key();
		let mut keys = Vec::new();
		let mut links = Vec::new();
		for index in first..end {
			let key = self.inner.seed.derive_vtxo_keypair(index);
			if self.inner.db.is_vtxo_key_linked(&key.public_key()).await? { continue; }
			let mut bytes = key.public_key().serialize().to_vec();
			bytes.extend(KeyLinkAttestation::new(mailbox, &key).serialize());
			keys.push((index, key.public_key()));
			links.push(bytes);
		}
		if keys.is_empty() { return Ok(()); }
		let reply = connection.client.set_fallback(protos::SetFallbackRequest {
			mailbox_pk: mailbox.serialize().to_vec(), record: None, key_links: links,
		}).await.context("failed to link next coin keys")?.into_inner();
		let current = decode_record(&reply.record, mailbox, network, server_pubkey)?;
		ensure!(onchain.read().await.is_mine(&current.spk).await?,
			"fallback changed to an unowned address while linking coin keys");
		self.inner.db.store_fallback_record(&current).await?;
		mark_reserved(onchain, &current.spk).await?;
		self.inner.db.store_linked_vtxo_keys(&keys).await
	}

	pub(crate) async fn rotate_fallback_if_paid(&self) -> anyhow::Result<()> {
		#[cfg(feature = "onchain-bdk")]
		{
			let _guard = self.inner.lock_manager.lock(
				&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30),
			).await?;
			let onchain = self.inner.onchain.as_ref().context("onchain wallet required")?;
			let record = self.fallback_destination().await?;
			let replacement = {
				let mut guard = onchain.write().await;
				let Some(wallet) = (&mut *guard as &mut dyn Any).downcast_mut::<OnchainWallet>()
					else { return Ok(()); };
				if !wallet.fallback_received(&record.spk) { return Ok(()); }
				wallet.reserve_fallback_address().await?
			};
			let (mut connection, ark_info) = self.require_server().await?;
			register(self.network().await?, ark_info.server_pubkey, &self.inner.seed, &*self.inner.db,
				onchain, &mut connection, Some(replacement),
			).await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use crate::SECP;

	use super::*;

	#[test]
	fn record_reply_authentication_and_key_index_bounds() {
		let key = Keypair::from_seckey_slice(&SECP, &[1; 32]).unwrap();
		let other = Keypair::from_seckey_slice(&SECP, &[2; 32]).unwrap();
		let record = FallbackRecord {
			spk: ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap(),
			seq: 100,
		};
		let server = Keypair::from_seckey_slice(&SECP, &[3; 32]).unwrap().public_key();
		let mut wire = record_bytes(&record, Network::Regtest, server, &key);
		assert_eq!(decode_record(&wire, key.public_key(), Network::Regtest, server).unwrap(), record);
		assert!(decode_record(&wire, other.public_key(), Network::Regtest, server).is_err());
		// A record signed for another chain or server is not ours.
		assert!(decode_record(&wire, key.public_key(), Network::Regtest, other.public_key()).is_err());
		let signet = record_bytes(&record, Network::Signet, server, &key);
		assert!(decode_record(&signet, key.public_key(), Network::Regtest, server).is_err());
		// It still parses, so registration can replace it.
		assert_eq!(parse_record(&signet, Network::Regtest).unwrap().0, record);
		wire[2] ^= 1;
		assert!(decode_record(&wire, key.public_key(), Network::Regtest, server).is_err());
		for len in [0, 72, 115] {
			assert!(decode_record(&vec![0; len], key.public_key(), Network::Regtest, server).is_err());
		}
		assert_eq!(next_key_index(None).unwrap(), 0);
		assert_eq!(next_key_index(Some((1 << 31) - 2)).unwrap(), (1 << 31) - 1);
		assert!(next_key_index(Some((1 << 31) - 1)).is_err());
		assert!(next_key_index(Some(u32::MAX)).is_err());
		assert!(next_sequence(Some(i64::MAX as u64)).is_err());
	}
}
