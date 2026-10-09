use anyhow::Context;
use ark::attestations::{FallbackRecordAttestation, KeyLinkAttestation};
use ark::ProtocolEncoding;
use bitcoin::{Address, Network, ScriptBuf};
use bitcoin::secp256k1::PublicKey;
use tokio_postgres::types::Type;

use super::Tx;

/// A mailbox-signed destination, validated before it enters a write transaction.
pub(crate) struct FallbackRecord {
	spk: ScriptBuf,
	seq: i64,
	attestation: FallbackRecordAttestation,
}

impl FallbackRecord {
	pub fn from_bytes(bytes: &[u8], mailbox_pk: PublicKey, network: Network) -> anyhow::Result<Self> {
		// Standard address scripts are at most 42 bytes. The suffix is seq || signature.
		ensure!((73..=114).contains(&bytes.len()), "invalid fallback record length");
		let (spk, suffix) = bytes.split_at(bytes.len() - 72);
		let spk = ScriptBuf::from(spk.to_vec());
		ensure!(spk.is_p2pkh() || spk.is_p2sh() || spk.is_p2wpkh() || spk.is_p2wsh() || spk.is_p2tr(),
			"fallback destination is not a standard address");
		Address::from_script(&spk, network).context("fallback destination is not a standard address")?;
		let seq = u64::from_le_bytes(suffix[..8].try_into()?);
		let stored_seq = i64::try_from(seq).context("fallback sequence exceeds database range")?;
		let attestation = FallbackRecordAttestation::deserialize(&suffix[8..])?;
		attestation.verify(&spk, seq, mailbox_pk).context("invalid fallback record signature")?;
		Ok(Self { spk, seq: stored_seq, attestation })
	}

	pub fn to_bytes(&self) -> Vec<u8> {
		let mut bytes = self.spk.as_bytes().to_vec();
		bytes.extend_from_slice(&self.seq.to_le_bytes());
		bytes.extend_from_slice(&self.attestation.serialize());
		bytes
	}
}

pub(crate) struct FallbackKeyLink {
	user_pubkey: PublicKey,
	attestation: KeyLinkAttestation,
}

impl FallbackKeyLink {
	pub fn from_bytes(bytes: &[u8], mailbox_pk: PublicKey) -> anyhow::Result<Self> {
		ensure!(bytes.len() == 97, "invalid fallback key link length");
		let user_pubkey = PublicKey::from_slice(&bytes[..33]).context("invalid fallback coin key")?;
		let attestation = KeyLinkAttestation::deserialize(&bytes[33..])?;
		attestation.verify(mailbox_pk, user_pubkey).context("invalid fallback key link signature")?;
		Ok(Self { user_pubkey, attestation })
	}
}

impl Tx<'_> {
	pub(crate) async fn set_fallback(
		&self,
		mailbox_pk: PublicKey,
		record: Option<&FallbackRecord>,
		key_links: &[FallbackKeyLink],
	) -> anyhow::Result<Option<FallbackRecord>> {
		let mailbox_bytes = mailbox_pk.serialize().to_vec();
		if let Some(record) = record {
			self.execute("
				INSERT INTO fallback_record (mailbox_pk, spk, seq, sig) VALUES ($1, $2, $3, $4)
				ON CONFLICT (mailbox_pk) DO UPDATE SET spk = EXCLUDED.spk, seq = EXCLUDED.seq, sig = EXCLUDED.sig
				WHERE fallback_record.seq < EXCLUDED.seq
			", &[&mailbox_bytes, &record.spk.as_bytes(), &record.seq, &record.attestation.serialize()])
				.await.context("failed to store fallback record")?;
		}
		if !key_links.is_empty() {
			let keys = key_links.iter().map(|l| l.user_pubkey.serialize().to_vec()).collect::<Vec<_>>();
			let signatures = key_links.iter().map(|l| l.attestation.serialize()).collect::<Vec<_>>();
			self.execute("
				INSERT INTO key_link (user_pubkey, mailbox_pk, sig)
				SELECT u.user_pubkey, $1, u.sig FROM UNNEST($2::bytea[], $3::bytea[]) AS u(user_pubkey, sig)
				ON CONFLICT (user_pubkey) DO NOTHING
			", &[&mailbox_bytes, &keys, &signatures]).await.context("failed to store fallback key links")?;
		}
		let row = self.query_opt(
			"SELECT spk, seq, sig FROM fallback_record WHERE mailbox_pk = $1", &[&mailbox_bytes],
		).await.context("failed to read fallback record")?;
		row.map(|row| Ok(FallbackRecord {
			spk: ScriptBuf::from(row.get::<_, Vec<u8>>("spk")),
			seq: row.get("seq"),
			attestation: FallbackRecordAttestation::deserialize(&row.get::<_, Vec<u8>>("sig"))?,
		})).transpose()
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
	use ark::SECP;
	use bitcoin::secp256k1::Keypair;

	use super::*;

	fn mailbox() -> Keypair {
		Keypair::from_seckey_slice(&SECP, &[7; 32]).unwrap()
	}

	fn record_bytes(spk: &ScriptBuf, seq: u64) -> Vec<u8> {
		let mut bytes = spk.as_bytes().to_vec();
		bytes.extend_from_slice(&seq.to_le_bytes());
		bytes.extend_from_slice(&FallbackRecordAttestation::new(spk, seq, &mailbox()).serialize());
		bytes
	}

	#[test]
	fn fallback_record_wire_validation() {
		let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let bytes = record_bytes(&spk, 1791475200);
		let decode = |b: &[u8]| FallbackRecord::from_bytes(b, mailbox().public_key(), Network::Regtest);
		assert_eq!(decode(&bytes).unwrap().to_bytes(), bytes);
		for len in 0..bytes.len() {
			assert!(decode(&bytes[..len]).is_err());
		}
		let mut changed = bytes.clone();
		changed[2] ^= 1;
		assert!(decode(&changed).is_err());
		let mut changed = bytes.clone();
		changed[spk.len()] ^= 1;
		assert!(decode(&changed).is_err());
		assert!(decode(&record_bytes(&spk, i64::MAX as u64)).is_ok());
		assert!(decode(&record_bytes(&spk, u64::MAX)).is_err());
		assert!(decode(&record_bytes(&ScriptBuf::from_hex("6a").unwrap(), 1)).is_err());
		assert!(FallbackRecord::from_bytes(&bytes, Keypair::from_seckey_slice(&SECP, &[8; 32])
			.unwrap().public_key(), Network::Regtest).is_err());
		let mut extended = bytes;
		extended.push(0);
		assert!(decode(&extended).is_err());
	}

	#[test]
	fn fallback_link_wire_validation() {
		let key = Keypair::from_seckey_slice(&SECP, &[3; 32]).unwrap();
		let mut bytes = key.public_key().serialize().to_vec();
		bytes.extend_from_slice(&KeyLinkAttestation::new(mailbox().public_key(), &key).serialize());
		let decode = |b: &[u8]| FallbackKeyLink::from_bytes(b, mailbox().public_key());
		assert_eq!(decode(&bytes).unwrap().user_pubkey, key.public_key());
		for len in 0..bytes.len() {
			assert!(decode(&bytes[..len]).is_err());
		}
		assert!(FallbackKeyLink::from_bytes(&bytes, key.public_key()).is_err());
		let mut changed = bytes.clone();
		changed[..33].copy_from_slice(&mailbox().public_key().serialize());
		assert!(decode(&changed).is_err());
		let mut extended = bytes;
		extended.push(0);
		assert!(decode(&extended).is_err());
	}
}
