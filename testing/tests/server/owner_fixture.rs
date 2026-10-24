//! Registered storage fixtures and explicitly historical signed commitments.
//! New financial issuance uses the public proof-bearing RPCs.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, ensure};

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignRequest};
use ark::fallback::{FallbackRecord, OwnerProof};
use ark::mailbox::MailboxIdentifier;
use ark::test_util::vectors::{generate_vtxo_vectors_for_account, VtxoTestVectors};
use ark::{ProtocolEncoding, ServerVtxo, VtxoId, VtxoPolicy, SECP};
use ark_testing::Captaind;
use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Address, Network};
use server::database::{Db, SpendState};
use server::database::tree::VtxoTreeUpdate;
use server_rpc::protos;

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

/// Represent a commitment signed before the send-source expiry guard existed.
/// This first proves that today's public RPC refuses the same shape without
/// retiring an input or creating an output. Inputs retain real Core funding,
/// all user attestations and output owner proofs are checked, and only unsigned
/// tree/spend rows are loaded.
/// The caller receives the matching server response and still signs/uploads it.
pub(super) async fn legacy_send_commitment(
	srv: &Captaind, request: protos::ArkoorPackageCosignRequest,
) -> protos::ArkoorPackageCosignResponse {
	let network = srv.config().network;
	let mnemonic = server::wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(network, &mnemonic.to_seed("")).unwrap();
	// These are the existing server identity paths, not fixture signing keys.
	let server_key = master.derive_priv(&SECP, &DerivationPath::from_str("m/2'/0'").unwrap()).unwrap().to_keypair(&SECP);
	let mailbox_key = master.derive_priv(&SECP, &DerivationPath::from_str("m/2'/1'").unwrap()).unwrap().to_keypair(&SECP);
	let info = srv.ark_info().await;
	assert_eq!(server_key.public_key(), info.server_pubkey);
	assert_eq!(mailbox_key.public_key(), info.mailbox_pubkey);
	let mut proofs = BTreeMap::new();
	for output in request.parts.iter().flat_map(|part| part.outputs.iter().chain(&part.isolated_outputs)) {
		let key = VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
		let proof = OwnerProof::try_from(output.owner_proof.clone().expect("legacy setup must authenticate every output owner")).unwrap();
		if let Some(previous) = proofs.insert(key, proof) { assert_eq!(previous, proof); }
	}
	let admission = protos::LightningPayHtlcCosignRequest { parts: request.parts.clone() };
	let parsed = ArkoorPackageCosignRequest::<VtxoId>::try_from(request).unwrap();
	let ids = parsed.inputs().copied().collect::<Vec<_>>();
	assert!(!ids.is_empty());
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let inputs = db.read(async |tx| {
		let mut inputs = Vec::with_capacity(ids.len());
		for id in &ids { inputs.push(tx.get_user_vtxo_by_id(*id).await?); }
		Ok(inputs)
	}).await.unwrap();
	for source in &inputs {
		assert_eq!(source.spend_state, SpendState::Spendable);
		assert!(!source.decided_for_fallback && source.confirmed_height.is_none());
		assert!(matches!(source.vtxo.policy(), VtxoPolicy::Pubkey(_)));
		assert_eq!(source.vtxo.server_pubkey(), info.server_pubkey);
		let funding = srv.bitcoind().await_transaction(source.vtxo.chain_anchor().txid).await;
		source.vtxo.validate(&funding).unwrap();
	}
	let request = parsed.set_vtxos(inputs.iter().map(|source| source.vtxo.clone())).unwrap();
	let builder = ArkoorPackageBuilder::from_cosign_request(request).unwrap();
	assert!(builder.builders.iter().any(|part| part.all_outputs().any(|output| matches!(&output.policy,
		VtxoPolicy::ServerHtlcSend(policy) if policy.htlc_expiry > part.input().expiry_height()))),
		"this fixture is only for preguard contracts whose HTLC outlives its backing");
	let refused = srv.get_public_rpc().await.request_lightning_pay_htlc_cosign(admission).await
		.expect_err("current admission must refuse an HTLC outliving its backing");
	assert_eq!(refused.code(), tonic::Code::InvalidArgument);
	assert!(refused.message().contains("funding coin expiry"), "{refused}");
	db.read(async |tx| {
		for source in &inputs {
			let current = tx.get_user_vtxo_by_id(source.vtxo_id).await?;
			assert_eq!(current.spend_state, SpendState::Spendable, "refusal cannot retire a source");
			assert!(!current.decided_for_fallback && current.confirmed_height.is_none());
		}
		for output in builder.build_unsigned_vtxos() {
			assert_eq!(tx.query_one("SELECT COUNT(*) FROM vtxo WHERE vtxo_id=$1",
				&[&output.id().to_string()]).await?.get::<_, i64>(0), 0,
				"refusal cannot create an output");
		}
		Ok(())
	}).await.unwrap();
	let update = VtxoTreeUpdate::new()
		.upsert_unsigned_tx(builder.virtual_transactions())
		.insert_oor_spent_vtxos(builder.build_unsigned_internal_vtxos())
		.insert_unregistered_vtxos(builder.build_unsigned_vtxos().map(ServerVtxo::from))
		.mark_vtxos_oor_spent(builder.input_spend_info());
	db.write(async |tx| {
		tx.lock_fallback_registry().await?;
		for (key, proof) in proofs {
			proof.validate()?;
			let mailbox = MailboxIdentifier::from_blinded(proof.blinded_mailbox_id, key, &mailbox_key)?.as_pubkey();
			let record = tx.get_fallback(mailbox).await?.context("legacy output has no canonical registration")?;
			record.verify(network, mailbox)?;
			let child = record.account_xpub.ckd_pub(&SECP, ChildNumber::from_normal_idx(proof.child_index)?)?;
			ensure!(child.public_key == key, "legacy proof must derive the actual output key");
			tx.execute("INSERT INTO expiry_owner_key(user_pubkey,mailbox_pk,child_index) \
				VALUES($1,$2,$3) ON CONFLICT(user_pubkey) DO NOTHING",
				&[&key.to_string(), &mailbox.to_string(), &i64::from(proof.child_index)]).await?;
			let stored = tx.query_one("SELECT mailbox_pk,child_index FROM expiry_owner_key WHERE user_pubkey=$1",
				&[&key.to_string()]).await?;
			ensure!(stored.get::<_, String>(0) == mailbox.to_string()
				&& stored.get::<_, i64>(1) == i64::from(proof.child_index), "legacy setup cannot replace an owner binding");
		}
		for source in &inputs {
			tx.query_one("SELECT vtxo_id FROM vtxo WHERE vtxo_id=$1 FOR UPDATE", &[&source.vtxo_id.to_string()]).await?;
			let current = tx.get_user_vtxo_by_id(source.vtxo_id).await?;
			ensure!(current.spend_state == SpendState::Spendable && !current.decided_for_fallback
				&& current.confirmed_height.is_none(), "legacy source changed before snapshot import");
			ensure!(current.vtxo.serialize() == source.vtxo.serialize(), "legacy source bytes changed");
		}
		tx.execute_vtxo_tree_update(update).await?;
		Ok(())
	}).await.unwrap();
	builder.server_cosign(&server_key).unwrap().cosign_response().into()
}
