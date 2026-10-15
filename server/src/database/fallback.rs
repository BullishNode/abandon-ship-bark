use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use ark::fallback::{FallbackRecord as Record, KeyLink, KeyLinkOrigin};
use ark::ProtocolEncoding;
use bitcoin::secp256k1::{schnorr, PublicKey};
use bitcoin::{Network, ScriptBuf};

use super::Tx;

/// A mailbox-signed immutable destination authenticated at the RPC boundary.
pub(crate) struct FallbackRecord(Record);

impl FallbackRecord {
	pub fn from_bytes(bytes: &[u8], mailbox: PublicKey, network: Network) -> anyhow::Result<Self> {
		let record = Record::deserialize(bytes)?;
		record.verify(network, mailbox)?;
		Ok(Self(record))
	}
	pub fn to_bytes(&self) -> Vec<u8> { self.0.serialize() }
}

fn read_record(row: &tokio_postgres::Row) -> anyhow::Result<Record> {
	Ok(Record {
		spk: ScriptBuf::from(row.get::<_, Vec<u8>>("spk")),
		mailbox_signature: schnorr::Signature::from_slice(&row.get::<_, Vec<u8>>("mailbox_sig"))?,
	})
}

fn read_link(row: &tokio_postgres::Row) -> anyhow::Result<KeyLink> {
	Ok(KeyLink {
		user_pubkey: PublicKey::from_slice(&row.get::<_, Vec<u8>>("user_pubkey"))?,
		recovery_index: row.get::<_, Option<i32>>("recovery_index").map(u32::try_from).transpose()?,
		signature: schnorr::Signature::from_slice(&row.get::<_, Vec<u8>>("sig"))?,
	})
}

impl Tx<'_> {
	pub(crate) async fn set_fallback(&self, mailbox: PublicKey, record: &FallbackRecord,
		links: &[KeyLink], network: Network,
	) -> anyhow::Result<(FallbackRecord, Vec<KeyLink>)> {
		ensure!(links.len() <= 64, "too many fallback key links");
		record.0.verify(network, mailbox)?;
		let mut distinct = BTreeSet::new();
		for link in links {
			ensure!(distinct.insert(link.user_pubkey), "duplicate fallback key link");
			link.verify(network, mailbox, &record.0)?;
		}
		let mb = mailbox.serialize().to_vec();
		self.execute("INSERT INTO fallback_record(mailbox_pk,spk,mailbox_sig) VALUES($1,$2,$3)
			ON CONFLICT(mailbox_pk) DO NOTHING", &[&mb, &record.0.spk.as_bytes(),
			&record.0.mailbox_signature.serialize().as_slice()]).await?;
		let row = self.query_one("SELECT spk,mailbox_sig FROM fallback_record WHERE mailbox_pk=$1", &[&mb]).await?;
		let current = read_record(&row)?;
		current.verify(network, mailbox)?;
		ensure!(current.same_binding(&record.0), "immutable fallback destination mismatch");
		// Stable key order bounds conflicting batch lock order. Return request order.
		let mut ordered = links.iter().collect::<Vec<_>>();
		ordered.sort_by_key(|link| link.user_pubkey);
		for link in ordered {
			let key = link.user_pubkey.serialize().to_vec();
			let index = link.recovery_index.map(i32::try_from).transpose()?;
			self.execute("INSERT INTO key_link(user_pubkey,mailbox_pk,sig,recovery_index) VALUES($1,$2,$3,$4)
				ON CONFLICT(user_pubkey) DO NOTHING", &[&key,&mb,&link.signature.serialize().as_slice(),&index]).await?;
			let row = self.query_one("SELECT user_pubkey,mailbox_pk,sig,recovery_index FROM key_link WHERE user_pubkey=$1", &[&key]).await?;
			ensure!(row.get::<_, Vec<u8>>("mailbox_pk") == mb, "coin key already belongs to another mailbox");
			let stored = read_link(&row)?;
			stored.verify(network, mailbox, &current)?;
			ensure!(stored.same_binding(link), "conflicting fallback recovery hint");
		}
		let mut canonical = Vec::with_capacity(links.len());
		for link in links {
			let row = self.query_one("SELECT user_pubkey,sig,recovery_index FROM key_link WHERE user_pubkey=$1",
				&[&link.user_pubkey.serialize().to_vec()]).await?;
			canonical.push(read_link(&row)?);
		}
		Ok((FallbackRecord(current), canonical))
	}

	pub(crate) async fn fallback_origin(&self, key: PublicKey) -> anyhow::Result<Option<KeyLinkOrigin>> {
		self.query_opt("SELECT mailbox_pk FROM key_link WHERE user_pubkey=$1", &[&key.serialize().to_vec()]).await?
			.map(|row| Ok(KeyLinkOrigin::new(PublicKey::from_slice(&row.get::<_, Vec<u8>>("mailbox_pk"))?))).transpose()
	}

	pub(crate) async fn fallback_proof(&self, key: PublicKey, network: Network) -> anyhow::Result<(PublicKey, Record, KeyLink)> {
		let row = self.query_opt("SELECT l.user_pubkey,l.mailbox_pk,l.sig,l.recovery_index,r.spk,r.mailbox_sig
			FROM key_link l JOIN fallback_record r USING(mailbox_pk) WHERE l.user_pubkey=$1",
			&[&key.serialize().to_vec()]).await?.context("missing signed fallback key link")?;
		let mailbox = PublicKey::from_slice(&row.get::<_, Vec<u8>>("mailbox_pk"))?;
		let record = read_record(&row)?;
		let link = read_link(&row)?;
		link.verify(network, mailbox, &record)?;
		ensure!(link.user_pubkey == key, "fallback policy key mismatch");
		Ok((mailbox, record, link))
	}

	pub(crate) async fn verify_fallback_link(&self, origin: KeyLinkOrigin, key: PublicKey, network: Network) -> anyhow::Result<()> {
		let (mailbox, _, _) = self.fallback_proof(key, network).await?;
		ensure!(mailbox == origin.mailbox_pk(), "fallback mailbox differs from linked owner");
		Ok(())
	}

	pub(crate) async fn fallback_scripts(&self, keys: &[PublicKey]) -> anyhow::Result<BTreeMap<PublicKey, ScriptBuf>> {
		let keys = keys.iter().map(|key| key.serialize().to_vec()).collect::<Vec<_>>();
		self.query("SELECT l.user_pubkey,r.spk FROM key_link l JOIN fallback_record r USING(mailbox_pk)
			WHERE l.user_pubkey=ANY($1::bytea[])", &[&keys]).await?.into_iter().map(|row| Ok((
			PublicKey::from_slice(&row.get::<_, Vec<u8>>("user_pubkey"))?, ScriptBuf::from(row.get::<_, Vec<u8>>("spk")),
		))).collect()
	}

	pub(crate) async fn authenticated_fallback_scripts(&self, keys: &[PublicKey], network: Network) -> anyhow::Result<BTreeMap<PublicKey, ScriptBuf>> {
		let expected = keys.iter().copied().collect::<BTreeSet<_>>();
		let raw = expected.iter().map(|key| key.serialize().to_vec()).collect::<Vec<_>>();
		let rows = self.query("SELECT l.user_pubkey,l.mailbox_pk,l.sig,l.recovery_index,r.spk,r.mailbox_sig
			FROM key_link l JOIN fallback_record r USING(mailbox_pk) WHERE l.user_pubkey=ANY($1::bytea[])", &[&raw]).await?;
		let mut scripts = BTreeMap::new();
		for row in rows {
			let mailbox = PublicKey::from_slice(&row.get::<_, Vec<u8>>("mailbox_pk"))?;
			let record = read_record(&row)?;
			let link = read_link(&row)?;
			link.verify(network, mailbox, &record)?;
			scripts.insert(link.user_pubkey, record.spk);
		}
		ensure!(scripts.len() == expected.len(), "missing signed fallback key link");
		Ok(scripts)
	}

	pub(crate) async fn require_fallback(&self, keys: &[PublicKey], network: Network) -> anyhow::Result<()> {
		self.authenticated_fallback_scripts(keys, network).await?;
		Ok(())
	}
}
