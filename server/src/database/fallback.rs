use std::collections::BTreeMap;

use anyhow::Context;
use ark::fallback::{AccountChild, FallbackRecord as AccountRecord};
use ark::ProtocolEncoding;
use bitcoin::bip32::Xpub;
use bitcoin::secp256k1::{schnorr, PublicKey};
use bitcoin::{Network, ScriptBuf};
use tokio_postgres::types::Type;

use super::Tx;

/// A fully authenticated immutable mailbox/account/destination binding.
pub(crate) struct FallbackRecord(AccountRecord);

impl FallbackRecord {
	pub fn from_bytes(bytes: &[u8], mailbox_pk: PublicKey, network: Network) -> anyhow::Result<Self> {
		let record = AccountRecord::deserialize(bytes)?;
		record.verify(network, mailbox_pk).context("invalid fallback record")?;
		Ok(Self(record))
	}

	pub fn to_bytes(&self) -> Vec<u8> { self.0.serialize() }
}

impl Tx<'_> {
	pub(crate) async fn set_fallback(
		&self, mailbox_pk: PublicKey, record: &FallbackRecord,
	) -> anyhow::Result<FallbackRecord> {
		let mailbox_bytes = mailbox_pk.serialize().to_vec();
		let FallbackRecord(record) = record;
		{
			self.execute("
				INSERT INTO fallback_record (mailbox_pk, spk, account_xpub, mailbox_sig, account_sig)
				VALUES ($1, $2, $3, $4, $5) ON CONFLICT (mailbox_pk) DO NOTHING
			", &[&mailbox_bytes, &record.spk.as_bytes(), &record.account_xpub.encode().as_slice(),
				&record.mailbox_signature.serialize().as_slice(), &record.account_signature.serialize().as_slice()])
				.await.context("failed to store fallback record")?;
		}
		let row = self.query_opt(
			"SELECT spk, account_xpub, mailbox_sig, account_sig FROM fallback_record WHERE mailbox_pk = $1",
			&[&mailbox_bytes],
		).await.context("failed to read fallback record")?;
		row.map(|row| -> anyhow::Result<_> { Ok(FallbackRecord(AccountRecord {
			spk: ScriptBuf::from(row.get::<_, Vec<u8>>("spk")),
			account_xpub: Xpub::decode(&row.get::<_, Vec<u8>>("account_xpub"))?,
			mailbox_signature: schnorr::Signature::from_slice(&row.get::<_, Vec<u8>>("mailbox_sig"))?,
			account_signature: schnorr::Signature::from_slice(&row.get::<_, Vec<u8>>("account_sig"))?,
		})) }).transpose()?.context("fallback record disappeared during registration")
	}

	pub(crate) async fn fallback_origin(&self, policy_key: PublicKey) -> anyhow::Result<Option<AccountChild>> {
		let row = self.query_opt("SELECT mailbox_pk, child_index FROM key_link WHERE user_pubkey=$1",
			&[&policy_key.serialize().to_vec()]).await?;
		row.map(|row| Ok(AccountChild::new(PublicKey::from_slice(&row.get::<_, Vec<u8>>("mailbox_pk"))?,
			u32::try_from(row.get::<_, i32>("child_index"))?)?)).transpose()
	}

	/// A cheap read-only preflight, also repeated inside the decisive write.
	pub(crate) async fn verify_fallback_child(&self, origin: AccountChild, policy_key: PublicKey) -> anyhow::Result<()> {
		let mailbox = origin.mailbox_pk().serialize().to_vec();
		let row = self.query_opt("SELECT account_xpub FROM fallback_record WHERE mailbox_pk=$1", &[&mailbox]).await?
			.context("missing immutable fallback account record")?;
		origin.verify_pubkey(&Xpub::decode(&row.get::<_, Vec<u8>>("account_xpub"))?, policy_key)?;
		Ok(())
	}

	/// Retain exact key attribution in the same transaction as coin admission.
	pub(crate) async fn admit_fallback_child(&self, origin: AccountChild, policy_key: PublicKey) -> anyhow::Result<()> {
		self.verify_fallback_child(origin, policy_key).await?;
		let mailbox = origin.mailbox_pk().serialize().to_vec();
		let index = i32::try_from(origin.child_index())?;
		let key = policy_key.serialize().to_vec();
		self.execute("INSERT INTO key_link (user_pubkey, mailbox_pk, child_index) VALUES ($1,$2,$3)
			ON CONFLICT (user_pubkey) DO NOTHING", &[&key, &mailbox, &index]).await?;
		let row = self.query_one("SELECT mailbox_pk, child_index FROM key_link WHERE user_pubkey=$1", &[&key]).await?;
		ensure!(row.get::<_, Vec<u8>>("mailbox_pk") == mailbox && row.get::<_, i32>("child_index") == index,
			"coin key already belongs to a different account child");
		Ok(())
	}

	/// Resolve a page of coin keys with one lookup. Missing entries have no admitted
	/// account provenance and cannot establish an automatic payout destination.
	pub(crate) async fn fallback_scripts(
		&self, keys: &[PublicKey],
	) -> anyhow::Result<BTreeMap<PublicKey, ScriptBuf>> {
		let keys = keys.iter().map(|key| key.serialize().to_vec()).collect::<Vec<_>>();
		self.query("SELECT l.user_pubkey, r.spk FROM key_link l
			JOIN fallback_record r ON r.mailbox_pk=l.mailbox_pk
			WHERE l.user_pubkey=ANY($1::bytea[])", &[&keys]).await?
			.into_iter().map(|row| Ok((
				PublicKey::from_slice(&row.get::<_, Vec<u8>>("user_pubkey"))?,
				ScriptBuf::from(row.get::<_, Vec<u8>>("spk")),
			))).collect()
	}

	/// Refuse a new user output unless its key has an immutable link to a record.
	pub(crate) async fn require_fallback(&self, user_pubkeys: &[PublicKey]) -> anyhow::Result<()> {
		if user_pubkeys.is_empty() {
			return Ok(());
		}
		let keys = user_pubkeys.iter().map(|k| k.serialize().to_vec()).collect::<Vec<_>>();
		let stmt = self.prepare_typed("
			SELECT u.user_pubkey FROM UNNEST($1::bytea[]) AS u(user_pubkey)
			LEFT JOIN key_link l ON l.user_pubkey = u.user_pubkey
			LEFT JOIN fallback_record r ON r.mailbox_pk = l.mailbox_pk
			WHERE r.mailbox_pk IS NULL LIMIT 1
		", &[Type::BYTEA_ARRAY]).await?;
		if let Some(row) = self.query_opt(&stmt, &[&keys]).await? {
			let key = PublicKey::from_slice(&row.get::<_, Vec<u8>>("user_pubkey"))?;
			return badarg!("missing fallback link or wallet record for coin key {}; call SetFallback before creating coins", key);
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use bitcoin::bip32::{ChildNumber, Xpriv, Xpub};
	use bitcoin::constants::ChainHash;
	use ark::SECP;
	use bitcoin::hashes::{sha256, Hash, HashEngine};
	use bitcoin::secp256k1::{Keypair, Message};
	use super::*;
	fn mailbox() -> Keypair { Keypair::from_seckey_slice(&SECP, &[7; 32]).unwrap() }

	#[test]
	fn immutable_account_record_accepts_portable_full_field_proofs() {
		let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let account = Xpriv::new_master(Network::Regtest, &[3; 64]).unwrap();
		let xpub = Xpub::from_priv(&SECP, &account);
		let child0 = account.derive_priv(&SECP, &[ChildNumber::from_normal_idx(0).unwrap()]).unwrap().to_keypair(&SECP);
		let mailbox = mailbox();
		let mut wire = vec![3, u8::try_from(spk.len()).unwrap()];
		wire.extend_from_slice(spk.as_bytes());
		wire.extend_from_slice(&xpub.encode());
		for (prefix, key) in [
			(b"Ark expiry fallback record v3   ", &mailbox),
			(b"Ark expiry fallback account v3  ", &child0),
		] {
			let mut engine = sha256::Hash::engine();
			engine.input(prefix);
			engine.input(ChainHash::REGTEST.as_bytes());
			engine.input(&mailbox.public_key().serialize());
			engine.input(&[u8::try_from(spk.len()).unwrap()]);
			engine.input(spk.as_bytes());
			engine.input(&xpub.encode());
			let message = Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array());
			wire.extend_from_slice(&SECP.sign_schnorr_with_aux_rand(&message, key, &[0; 32]).serialize());
		}
		assert!(FallbackRecord::from_bytes(&wire, mailbox.public_key(), Network::Regtest).is_ok(),
			"a valid immutable xpub record must register without per-key signatures");
		assert!(FallbackRecord::from_bytes(&wire, mailbox.public_key(), Network::Signet).is_err());
	}

}
