//! Signed account-derived coins for database storage fixtures. Financial
//! integration tests establish the same relation through the public RPCs.

use anyhow::ensure;

use ark::fallback::FallbackRecord;
use ark::test_util::vectors::{generate_vtxo_vectors_for_account, VtxoTestVectors};
use ark::SECP;
use bitcoin::bip32::{ChildNumber, Xpriv};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Address, Network};
use server::database::Db;

pub(super) struct RegisteredVectors {
	pub(super) account: Xpriv,
	vectors: VtxoTestVectors,
}

impl std::ops::Deref for RegisteredVectors {
	type Target = VtxoTestVectors;
	fn deref(&self) -> &Self::Target { &self.vectors }
}

lazy_static::lazy_static! {
	pub(super) static ref REGISTERED_VTXO_VECTORS: RegisteredVectors = {
		let account = Xpriv::new_master(Network::Regtest, &[134; 32]).unwrap();
		let vectors = generate_vtxo_vectors_for_account(&account);
		RegisteredVectors { account, vectors }
	};
}

pub(super) async fn register_account_children(
	db: &Db, account: Xpriv, children: impl IntoIterator<Item = (PublicKey, u32)>,
) {
	let children = children.into_iter().collect::<Vec<_>>();
	let mailbox = account.to_keypair(&SECP);
	let record = FallbackRecord::new(Network::Regtest,
		Address::p2tr(&SECP, mailbox.x_only_public_key().0, None, Network::Regtest).script_pubkey(),
		&account, &mailbox,
	).unwrap();
	record.verify(Network::Regtest, mailbox.public_key()).unwrap();
	db.write(async |tx| {
		let canonical = tx.set_fallback(mailbox.public_key(), &record, Network::Regtest).await?;
		ensure!(canonical.same_binding(&record), "fixture registration must be canonical");
		for (key, index) in children {
			let child = account.derive_priv(&SECP, &[ChildNumber::from_normal_idx(index)?])?;
			ensure!(child.to_keypair(&SECP).public_key() == key, "fixture key must derive from its registered account");
			tx.execute("INSERT INTO expiry_owner_key(user_pubkey,mailbox_pk,child_index) \
				VALUES($1,$2,$3) ON CONFLICT(user_pubkey) DO NOTHING",
				&[&key.to_string(), &mailbox.public_key().to_string(), &i64::from(index)]).await?;
			let stored = tx.query_one("SELECT mailbox_pk,child_index FROM expiry_owner_key WHERE user_pubkey=$1",
				&[&key.to_string()]).await?;
			ensure!(stored.get::<_, String>(0) == mailbox.public_key().to_string()
				&& stored.get::<_, i64>(1) == i64::from(index), "fixture must not overwrite ownership");
		}
		Ok(())
	}).await.unwrap();
}

pub(super) async fn register_vector_owners(db: &Db) {
	let v = &*REGISTERED_VTXO_VECTORS;
	register_account_children(db, v.account, [
		(v.board_vtxo.user_pubkey(), 1),
		(v.arkoor_htlc_out_vtxo.user_pubkey(), 2),
		(v.arkoor2_vtxo.user_pubkey(), 3),
		(v.round1_vtxo.user_pubkey(), 4),
		(v.round2_vtxo.user_pubkey(), 5),
		(v.arkoor3_vtxo.user_pubkey(), 6),
	]).await;
}
