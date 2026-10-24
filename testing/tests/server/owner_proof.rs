use std::sync::Arc;
use std::time::Duration;

use ark::arkoor::{ArkoorDestination, package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse}};
use ark::address::VtxoDelivery;
use ark::integration::TokenStatus;
use ark::lightning::Preimage;
use ark::mailbox::BlindedMailboxIdentifier;
use ark::test_util::VTXO_VECTORS;
use ark::{ProtocolEncoding, ServerVtxo, Vtxo, VtxoPolicy, SECP};
use ark_testing::{btc, sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use bark::actions::arkoor_send::{ArkoorSend, Progress};
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::{VtxoLockHolder, VtxoState};
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::rpc::RpcApi;
use cln_rpc::plugins::hold;
use server::database::{Db, SpendState};
use server::database::expiry_attribution::Attribution;
use server::database::tree::VtxoTreeUpdate;
use server_rpc::protos;
use server_rpc::protos::prepare_lightning_receive_claim_request::LightningReceiveAntiDos;

use super::expiry_attribution::{Owner, PreparedBoard, board, prepare_board, server};

pub(super) async fn wallet_proof(wallet: &bark::Wallet, srv: &ark_testing::Captaind,
	pubkey: PublicKey,
) -> protos::OwnerProof {
	let (index, key) = wallet.pubkey_keypair(&pubkey).await.unwrap()
		.expect("the fixture must use an actual registered wallet child");
	let blinded = wallet.mailbox_identifier().to_blinded(srv.ark_info().await.mailbox_pubkey, &key).unwrap();
	protos::OwnerProof { blinded_mailbox_id: blinded.as_ref().to_vec(), child_index: index }
}

pub(super) async fn prove_wallet_destinations(wallet: &bark::Wallet, srv: &ark_testing::Captaind,
	mut request: protos::ArkoorPackageCosignRequest,
) -> protos::ArkoorPackageCosignRequest {
	for part in &mut request.parts {
		for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
			let key = VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
			output.owner_proof = Some(wallet_proof(wallet, srv, key).await);
		}
	}
	request
}

fn assert_owner_refusal(error: &tonic::Status) {
	let reason = error.metadata().get("abandon-reason")
		.expect("ownership refusal has a stable machine-readable reason").to_str().unwrap();
	assert!(matches!(reason, "owner_proof_required" | "invalid_owner_proof"), "{error:?}");
}

async fn assert_no_board(ctx: &TestContext, db: &Db, prepared: &PreparedBoard) {
	let key = PublicKey::from_slice(&prepared.request.user_pubkey).unwrap();
	let counts = db.read(async |tx| Ok((
		tx.query_one("SELECT COUNT(*) FROM expiry_owner_key WHERE user_pubkey=$1", &[&key.to_string()])
			.await?.get::<_, i64>(0),
		tx.query_one("SELECT COUNT(*) FROM vtxo", &[]).await?.get::<_, i64>(0),
	))).await.unwrap();
	assert_eq!(counts, (0, 0), "refused ownership cannot create a mapping or coin");
	assert!(ctx.bitcoind().sync_client().get_raw_transaction(&prepared.funding.compute_txid(), None).is_err(),
		"no refused request may broadcast the prepared funding transaction");
}

/// Exercise the public board protocol against real funding and PostgreSQL.
/// The translation case uses the correct recipient key and a registered
/// attacker mailbox; only account-child equality can distinguish the forgery.
#[tokio::test]
async fn owner_proof_board_refuses_forgery_and_survives_registration_restart() {
	let ctx = TestContext::new("server/owner_proof_board_refuses_forgery_and_survives_registration_restart").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	let attacker = Owner::with_seed(&ctx, 119);
	attacker.register(&srv).await;
	let prepared = prepare_board(&ctx, &srv, &owner, 20_001).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mut rpc = srv.get_public_rpc().await;

	// A correct derivation without the signed registration is insufficient.
	let error = rpc.request_board_cosign(prepared.request.clone()).await.unwrap_err();
	assert_owner_refusal(&error);
	assert_no_board(&ctx, &db, &prepared).await;
	owner.register(&srv).await;

	let correct = prepared.request.owner_proof.clone().unwrap();
	let blinded = PublicKey::from_slice(&correct.blinded_mailbox_id).unwrap();
	let translated = PublicKey::combine_keys(&[
		&blinded, &owner.mailbox.public_key().negate(&SECP), &attacker.mailbox.public_key(),
	]).unwrap();
	let mut wrong_index = correct.clone();
	wrong_index.child_index += 1;
	let mut hardened = correct.clone();
	hardened.child_index = 1 << 31;
	let wrong_server_key = owner.proof(20_001, srv.ark_info().await.server_pubkey);
	let mut invalid_point = correct.clone();
	invalid_point.blinded_mailbox_id = vec![0; 33];
	let forgery = protos::OwnerProof {
		blinded_mailbox_id: BlindedMailboxIdentifier::from_pubkey(translated).as_ref().to_vec(),
		child_index: correct.child_index,
	};
	for proof in [None, Some(wrong_index), Some(hardened), Some(invalid_point), Some(forgery),
		Some(wrong_server_key)] {
		let mut request = prepared.request.clone();
		request.owner_proof = proof;
		let error = rpc.request_board_cosign(request).await.unwrap_err();
		assert_owner_refusal(&error);
		assert_no_board(&ctx, &db, &prepared).await;
	}

	let cosign = rpc.request_board_cosign(prepared.request.clone()).await.unwrap().into_inner();
	let replay = rpc.request_board_cosign(prepared.request).await.unwrap().into_inner();
	let replay = replay.try_into().unwrap();
	// Server nonces can differ. Both partial signatures must verify for the
	// same funding and exit transaction held by this unchanged builder.
	assert!(prepared.builder.verify_cosign_response(&replay));
	assert_eq!(db.read(async |tx| tx.expiry_owner(owner.key(20_001).public_key(), bitcoin::Network::Regtest).await)
		.await.unwrap(), Attribution::Owned { mailbox_pk: owner.mailbox.public_key(), child_index: 20_001,
			spk: owner.record.spk.clone() });
	let response = cosign.try_into().unwrap();
	assert!(prepared.builder.verify_cosign_response(&response));
	let coin = prepared.builder.build_vtxo(&response, &owner.key(20_001)).unwrap();
	coin.validate(&prepared.funding).unwrap();
	assert_eq!(coin.amount(), sat(100_000));
	drop(rpc);
	srv.stop().await.unwrap();
	srv.start().await.unwrap();
	ctx.bitcoind().sync_client().send_raw_transaction(&prepared.funding).unwrap();
	let tip = ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let request = protos::BoardVtxoRequest { board_vtxo: coin.serialize() };
	let mut rpc = srv.get_public_rpc().await;
	rpc.register_board_vtxo(request.clone()).await.unwrap();
	rpc.register_board_vtxo(request).await.unwrap();
	let stored = db.read(async |tx| tx.get_user_vtxo_by_id(coin.id()).await).await.unwrap();
	assert_eq!(stored.spend_state, SpendState::Spendable);
	assert_eq!(stored.vtxo.amount(), coin.amount());
}

/// Ownership metadata discovers a binding; it does not authorize spending.
/// An already verified binding is enough for a fresh output, but supplied
/// evidence must still be checked and a corrupted binding must fail closed.
#[tokio::test]
async fn owner_proof_known_mapping_accepts_fresh_output_without_metadata() {
	let ctx = TestContext::new("server/owner_proof_known_mapping_accepts_fresh_output_without_metadata").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let original = board(&ctx, &srv, &owner, 1).await;
	let prepared = prepare_board(&ctx, &srv, &owner, 1).await;
	assert_ne!(original.chain_anchor().txid, prepared.funding.compute_txid(),
		"this must create a fresh output, not merely replay the original board");
	let mut rpc = srv.get_public_rpc().await;
	let mut request = prepared.request.clone();
	request.owner_proof = None;
	let response = rpc.request_board_cosign(request.clone()).await.unwrap().into_inner();
	assert!(prepared.builder.verify_cosign_response(&response.try_into().unwrap()));

	let mut invalid = prepared.request;
	invalid.owner_proof.as_mut().unwrap().child_index += 1;
	let error = rpc.request_board_cosign(invalid).await.unwrap_err();
	assert_owner_refusal(&error);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(db.read(async |tx| tx.expiry_owner(owner.key(1).public_key(), bitcoin::Network::Regtest).await)
		.await.unwrap(), Attribution::Owned { mailbox_pk: owner.mailbox.public_key(), child_index: 1,
			spk: owner.record.spk.clone() });
	let before = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM vtxo", &[])
		.await?.get::<_, i64>(0))).await.unwrap();
	// A restored/corrupted mapping must not make missing metadata a bypass.
	db.write(async |tx| {
		tx.execute("UPDATE expiry_owner_key SET child_index=2 WHERE user_pubkey=$1",
			&[&owner.key(1).public_key().to_string()]).await?;
		Ok(())
	}).await.unwrap();
	rpc.request_board_cosign(request).await.expect_err("cached ownership must be re-derived before acceptance");
	assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM vtxo", &[])
		.await?.get::<_, i64>(0))).await.unwrap(), before);
	assert!(ctx.bitcoind().sync_client().get_raw_transaction(&prepared.funding.compute_txid(), None).is_err());
}

/// The free writer is also called directly by round persistence. Its guard
/// must cover all four customer birth states and roll the entire update back.
#[tokio::test]
async fn owner_proof_fence_rejects_each_unmapped_customer_state() {
	let mut ctx = TestContext::new_minimal("postgresd/owner_proof_fence_rejects_each_unmapped_customer_state").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let coin = ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone());
	for state in [SpendState::Spendable, SpendState::Unregistered, SpendState::Unclaimed, SpendState::HtlcRecvUnclaimed] {
		let update = VtxoTreeUpdate::new().insert_unspent_vtxos([coin.clone()], state);
		let error = db.write(async |tx| server::database::tree::execute_vtxo_tree_update(&tx, update).await)
			.await.expect_err("each unmapped customer state must fail at the shared writer");
		assert!(format!("{error:#}").contains("owner"), "{state:?}: {error:#}");
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM vtxo", &[]).await?.get::<_, i64>(0)))
			.await.unwrap(), 0);
	}
}

#[tokio::test]
async fn owner_proof_arkoor_invalid_sibling_rolls_back_all_outputs() {
	let ctx = TestContext::new("server/owner_proof_arkoor_invalid_sibling_rolls_back_all_outputs").await;
	let srv = server(&ctx).await;
	let sender = Owner::new(&ctx);
	let recipient = Owner::with_seed(&ctx, 121);
	sender.register(&srv).await;
	recipient.register(&srv).await;
	let input = board(&ctx, &srv, &sender, 1).await;
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input.clone()], vec![
		ArkoorDestination { policy: VtxoPolicy::new_pubkey(sender.key(2).public_key()), total_amount: sat(40_000) },
		ArkoorDestination { policy: VtxoPolicy::new_pubkey(recipient.key(3).public_key()), total_amount: sat(59_600) },
		ArkoorDestination { policy: VtxoPolicy::new_pubkey(recipient.key(4).public_key()), total_amount: sat(200) },
		ArkoorDestination { policy: VtxoPolicy::new_pubkey(recipient.key(5).public_key()), total_amount: sat(200) },
	]).unwrap().generate_user_nonces(&[sender.key(1)]).unwrap();
	let info = srv.ark_info().await;
	let mut correct = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &mut correct.parts {
		for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
			let key = VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
			output.owner_proof = Some(if key == sender.key(2).public_key() { sender.proof(2, info.mailbox_pubkey) }
				else {
					let index = (3..=5).find(|index| key == recipient.key(*index).public_key()).unwrap();
					recipient.proof(index, info.mailbox_pubkey)
				});
		}
	}
	assert_eq!(correct.parts.iter().map(|part| part.isolated_outputs.len()).sum::<usize>(), 2);
	let mut invalid = correct.clone();
	for part in &mut invalid.parts {
		for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
			if VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey() == recipient.key(4).public_key() {
				output.owner_proof.as_mut().unwrap().child_index += 1;
			}
		}
	}
	let mut rpc = srv.get_public_rpc().await;
	let error = rpc.request_arkoor_cosign(invalid).await.unwrap_err();
	assert_owner_refusal(&error);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let snapshot = db.read(async |tx| Ok((
		tx.get_user_vtxo_by_id(input.id()).await?.spend_state,
		tx.query_one("SELECT COUNT(*) FROM expiry_owner_key WHERE user_pubkey=ANY($1)",
			&[&vec![sender.key(2).public_key().to_string(), recipient.key(3).public_key().to_string(),
				recipient.key(4).public_key().to_string(), recipient.key(5).public_key().to_string()]])
			.await?.get::<_, i64>(0),
		tx.query_one("SELECT COUNT(*) FROM vtxo WHERE spend_state='unregistered'", &[]).await?.get::<_, i64>(0),
	))).await.unwrap();
	assert_eq!(snapshot, (SpendState::Spendable, 0, 0), "the bad sibling cannot strand the source or a partial proof batch");
	let response = rpc.request_arkoor_cosign(correct).await.unwrap().into_inner();
	let outputs = builder.user_cosign(&[sender.key(1)], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	assert_eq!(outputs.iter().map(|v| v.amount()).sum::<bitcoin::Amount>(), input.amount());
	assert_eq!(outputs.iter().filter(|v| (3..=5).any(|index| v.user_pubkey() == recipient.key(index).public_key()))
		.map(|v| v.amount()).sum::<bitcoin::Amount>(), sat(60_000));
	for index in [4, 5] {
		assert_eq!(outputs.iter().find(|coin| coin.user_pubkey() == recipient.key(index).public_key()).unwrap().amount(), sat(200));
	}
	rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
		vtxos: outputs.iter().map(|v| v.serialize()).collect(),
	}).await.unwrap();
	for output in outputs {
		assert_eq!(db.read(async |tx| tx.get_user_vtxo_by_id(output.id()).await).await.unwrap().spend_state,
			SpendState::Spendable);
	}
}

/// A refused grant must leave its one-use token available. A refused
/// forwarding destination must leave the external payer held, so the same
/// receive can still complete once all output owners are proven.
#[tokio::test]
async fn owner_proof_receive_refusal_preserves_token_and_uncollected_payer() {
	let ctx = TestContext::new("server/owner_proof_receive_refusal_preserves_token_and_uncollected_payer").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10))
		.cfg(|cfg| cfg.ln_receive_anti_dos_required = true).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let owner = Owner::new(&ctx);
	let recipient = Owner::with_seed(&ctx, 120);
	owner.register(&srv).await;
	recipient.register(&srv).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let configured = srv.integration_cmd(&["configure-token-type", "captaind", "single-use-board", "1", "1000"]).await;
	assert_ne!(configured.trim().parse::<i64>().unwrap(), 0);
	let token_reply = srv.integration_cmd(&["generate-token", "captaind", "single-use-board"]).await;
	let mut token_parts = token_reply.split_whitespace();
	assert_eq!(token_parts.next(), Some("Token:"));
	let token = token_parts.next().unwrap().to_owned();
	let preimage = Preimage::random();
	let hash = preimage.compute_payment_hash();
	let mut rpc = srv.get_public_rpc().await;
	let invoice = rpc.start_lightning_receive(protos::StartLightningReceiveRequest {
		payment_hash: hash.to_vec(), amount_sat: 100_000,
		min_cltv_delta: srv.config().max_user_invoice_cltv_delta.into(),
		mailbox_id: Some(owner.mailbox.public_key().serialize().to_vec()), description: None,
	}).await.unwrap().into_inner().bolt11;
	let payer = Arc::new(lightning.external);
	let payment = {
		let payer = Arc::clone(&payer);
		tokio::spawn(async move { payer.try_pay_bolt11(invoice).await })
	};
	srv.wait_for_lightning_receive_accepted(hash).await;
	let subscription = db.read(async |tx| tx.get_htlc_subscription_by_payment_hash(hash).await)
		.await.unwrap().unwrap();
	let expiry = subscription.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta);
	let info = srv.ark_info().await;
	let correct = owner.proof(17, info.mailbox_pubkey);
	let mut wrong = correct.clone();
	wrong.child_index += 1;
	let mut request = protos::PrepareLightningReceiveClaimRequest {
		payment_hash: hash.to_vec(), user_pubkey: owner.key(17).public_key().serialize().to_vec(),
		htlc_recv_expiry: expiry.to_u32(), lightning_receive_anti_dos: Some(LightningReceiveAntiDos::Token(token.clone())),
		owner_proof: Some(wrong),
	};
	let error = rpc.prepare_lightning_receive_claim(request.clone()).await.unwrap_err();
	assert_owner_refusal(&error);
	let unused = db.read(async |tx| tx.get_integration_token(&token).await).await.unwrap().unwrap();
	assert_eq!(unused.status, TokenStatus::Unused);
	let refused = db.read(async |tx| tx.get_htlc_subscription_by_payment_hash(hash).await)
		.await.unwrap().unwrap();
	assert_eq!(refused.status, subscription.status);
	assert!(refused.htlc_vtxos.is_empty(), "invalid proof cannot allocate pool grants");
	assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());

	request.owner_proof = Some(correct);
	let grants = rpc.prepare_lightning_receive_claim(request).await.unwrap().into_inner().htlc_vtxos
		.into_iter().map(|bytes| Vtxo::deserialize(&bytes).unwrap()).collect::<Vec<_>>();
	assert!(!grants.is_empty());
	assert_eq!(db.read(async |tx| tx.get_integration_token(&token).await).await.unwrap().unwrap().status, TokenStatus::Used);
	let keys = vec![owner.key(17); grants.len()];
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		grants.clone(), VtxoPolicy::new_pubkey(recipient.key(23).public_key()),
	).unwrap().generate_user_nonces(&keys).unwrap();
	let mut cosign = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	recipient.prove_destinations(&mut cosign, 23, info.mailbox_pubkey);
	let mut invalid_claim = cosign.clone();
	for part in &mut invalid_claim.parts {
		for output in &mut part.outputs { output.owner_proof.as_mut().unwrap().child_index += 1; }
	}
	let error = rpc.claim_lightning_receive(protos::ClaimLightningReceiveRequest {
		payment_hash: hash.to_vec(), payment_preimage: preimage.to_vec(), cosign_request: Some(invalid_claim),
	}).await.unwrap_err();
	assert_owner_refusal(&error);
	assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	let bindings = db.read(async |tx| Ok(tx.query_one(
		"SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE payment_hash=$1", &[&hash.to_string()],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(bindings, 0, "refused destination must not bind a receive");
	let held = lightning.internal.hold_client().await.list(hold::ListRequest {
		constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
	}).await.unwrap().into_inner().invoices;
	assert_eq!(held.len(), 1);
	assert_eq!(held[0].state(), hold::InvoiceState::Accepted);
	assert!(held[0].preimage.is_none());
	assert!(!payment.is_finished(), "invalid owner must not collect the payer");

	let response = rpc.claim_lightning_receive(protos::ClaimLightningReceiveRequest {
		payment_hash: hash.to_vec(), payment_preimage: preimage.to_vec(), cosign_request: Some(cosign),
	}).await.unwrap().into_inner();
	let outputs = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	assert_eq!(outputs.iter().map(|v| v.amount()).sum::<bitcoin::Amount>(),
		grants.iter().map(|v| v.amount()).sum::<bitcoin::Amount>());
	assert!(outputs.iter().all(|v| v.user_pubkey() == recipient.key(23).public_key()));
	rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
		vtxos: outputs.iter().map(|v| v.serialize()).collect(),
	}).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), payment).await
		.expect("correct proof must let the held payer finish").unwrap().unwrap();
	assert_eq!(db.read(async |tx| tx.expiry_owner(recipient.key(23).public_key(), bitcoin::Network::Regtest).await)
		.await.unwrap(), Attribution::Owned { mailbox_pk: recipient.mailbox.public_key(), child_index: 23,
			spk: recipient.record.spk.clone() });
}

/// A pre-upgrade Cosigning checkpoint can have a legacy mailbox-only address.
/// If its first cosign committed before the reply was lost, the persisted owner
/// binding must let that exact spend finish after reopening the wallet.
#[tokio::test]
async fn owner_proof_legacy_cosigning_checkpoint_replays_committed_spend_after_reopen() {
	let ctx = TestContext::new("server/owner_proof_legacy_cosigning_checkpoint_replays_committed_spend_after_reopen").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let sender = ctx.bark("sender", &srv).cfg(|cfg| cfg.daemon_manual_sync = true)
		.funded(sat(200_000)).create().await;
	let recipient = ctx.bark("recipient", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let [source] = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await.try_into().unwrap();
	let wallet = sender.client().await;
	let recipient_wallet = recipient.client().await;
	let address = recipient_wallet.new_address().await.unwrap();
	let proof = address.owner_proof().unwrap().unwrap();
	let legacy = ark::Address::new(address.is_testnet(), address.ark_id(), address.policy().clone(),
		vec![VtxoDelivery::ServerMailbox { blinded_id: proof.blinded_mailbox_id }]);
	assert!(legacy.owner_proof().unwrap().is_none());
	assert_eq!(legacy.policy(), address.policy());
	let input = wallet.get_full_vtxo(source).await.unwrap();
	let input_key = wallet.get_vtxo_key(&input).await.unwrap();
	let (change_key, change_key_index) = wallet.derive_store_next_keypair().await.unwrap();
	let action = ArkoorSend {
		id: "owner-proof-legacy-cosign".into(), destination: legacy, amount: sat(60_000),
		input_vtxo_ids: vec![source], change_key_index,
		change_pieces: None, progress: Progress::Cosigning,
	};
	let local = SqliteClient::open(sender.datadir().join("db.sqlite")).unwrap();
	local.upsert_wallet_action_checkpoint(&action.id, &action.clone().into()).await.unwrap();
	wallet.lock_vtxos([source], Some(VtxoLockHolder::Action { id: action.id.clone() })).await.unwrap();
	wallet.register_vtxo_transactions_with_server([&input]).await.unwrap();
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input.clone()], vec![
		ArkoorDestination { total_amount: sat(60_000), policy: address.policy().clone() },
		ArkoorDestination { total_amount: sat(40_000), policy: VtxoPolicy::new_pubkey(change_key.public_key()) },
	]).unwrap().generate_user_nonces(&[input_key]).unwrap();
	let expected = builder.build_unsigned_vtxos().map(|coin| (coin.id(), coin.user_pubkey(), coin.amount()))
		.collect::<Vec<_>>();
	assert_eq!(expected.len(), 2);
	let mut request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &mut request.parts {
		for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
			let key = VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
			output.owner_proof = Some(if key == address.policy().user_pubkey() { proof.into() }
				else { wallet_proof(&wallet, &srv, key).await });
		}
	}
	// The server accepts the original indexed proof. The response is never
	// applied to this wallet, which stays durably locked at Cosigning.
	drop(srv.get_public_rpc().await.request_arkoor_cosign(request).await.unwrap());
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let committed = db.read(async |tx| tx.get_user_vtxo_by_id(source).await).await.unwrap();
	assert_eq!(committed.spend_state, SpendState::Spent);
	assert!(committed.oor_spent_txid.is_some());
	assert_eq!(wallet.pending_arkoor_sends().await.unwrap(), vec![action.clone()]);
	assert!(local.get_movement_id_by_action(&action.id).await.unwrap().is_none());
	assert_eq!(local.get_wallet_vtxo(source).await.unwrap().unwrap().state,
		VtxoState::Locked { holder: Some(VtxoLockHolder::Action { id: action.id.clone() }) });
	drop(builder);
	drop(local);
	drop(wallet);
	drop(recipient_wallet);
	// Reopening uses the original seed, keystore, checkpoints and database;
	// no signed response or derived output is supplied to the new wallet.
	let reopened = sender.client().await;
	assert_eq!(reopened.pending_arkoor_sends().await.unwrap(), vec![action.clone()]);
	tokio::time::timeout(Duration::from_secs(30), reopened.sync_pending_arkoor_sends()).await.unwrap().unwrap();
	assert!(reopened.pending_arkoor_sends().await.unwrap().is_empty());
	reopened.sync_pending_arkoor_sends().await.unwrap();
	let recipient_wallet = recipient.client().await;
	recipient_wallet.sync().await;
	let sender_coins = reopened.spendable_vtxos().await.unwrap();
	let recipient_coins = recipient_wallet.spendable_vtxos().await.unwrap();
	assert_eq!(sender_coins.len(), 1);
	assert_eq!(recipient_coins.len(), 1);
	assert_eq!(sender_coins[0].vtxo.amount(), sat(40_000));
	assert_eq!(recipient_coins[0].vtxo.amount(), sat(60_000));
	assert_eq!(recipient_coins[0].vtxo.user_pubkey(), address.policy().user_pubkey());
	assert_eq!(sender_coins.iter().chain(&recipient_coins).map(|coin| coin.vtxo.id()).collect::<std::collections::BTreeSet<_>>(),
		expected.iter().map(|(id, _, _)| *id).collect::<std::collections::BTreeSet<_>>(),
		"replay must register exactly the outputs of the committed spend");
	assert_eq!(sender_coins[0].vtxo.amount() + recipient_coins[0].vtxo.amount(), input.amount());
	let replayed = db.read(async |tx| tx.get_user_vtxo_by_id(source).await).await.unwrap();
	assert_eq!(replayed.oor_spent_txid, committed.oor_spent_txid);
	assert_eq!(replayed.spend_state, SpendState::Spent);
	assert_eq!(reopened.get_vtxo_by_id(source).await.unwrap().state, VtxoState::Spent);
	let history = reopened.history().await.unwrap().into_iter()
		.filter(|movement| movement.subsystem.name == "bark.arkoor" && movement.subsystem.kind == "send")
		.collect::<Vec<_>>();
	assert_eq!(history.len(), 1, "the accepted payment is debited once across both recovery drives");
	assert_eq!(history[0].effective_balance.to_sat(), -60_000);
	assert_eq!(history[0].input_vtxos, vec![source]);
	assert!(SqliteClient::open(sender.datadir().join("db.sqlite")).unwrap()
		.get_all_wallet_action_checkpoints().await.unwrap().is_empty());
}

/// Neither send issuance nor an unattempted-source refund may retire its
/// input before the new output owner is verified.
#[tokio::test]
async fn owner_proof_htlc_send_and_fresh_refund_refuse_atomically_then_retry() {
	let ctx = TestContext::new("server/owner_proof_htlc_send_and_fresh_refund_refuse_atomically_then_retry").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let actor = ctx.bark("owner", &srv).funded(sat(200_000)).create().await;
	let [source] = actor.board_and_confirm_and_register(&ctx, sat(100_000)).await.try_into().unwrap();
	let wallet = actor.client().await;
	let input = wallet.get_full_vtxo(source).await.unwrap();
	let input_key = wallet.get_vtxo_key(&input).await.unwrap();
	let (htlc_key, _) = wallet.derive_store_next_keypair().await.unwrap();
	let tip = bitcoin_ext::BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + bitcoin_ext::BlockDelta::new(2);
	let hash = Preimage::random().compute_payment_hash();
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()],
		VtxoPolicy::new_server_htlc_send(htlc_key.public_key(), hash, deadline)).unwrap()
		.generate_user_nonces(&[input_key]).unwrap();
	let correct = prove_wallet_destinations(&wallet, &srv,
		protos::ArkoorPackageCosignRequest::from(builder.cosign_request())).await;
	let mut invalid = correct.clone();
	for part in &mut invalid.parts {
		for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
			output.owner_proof.as_mut().unwrap().child_index += 1;
		}
	}
	let mut rpc = srv.get_public_rpc().await;
	let error = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: invalid.parts,
	}).await.unwrap_err();
	assert_owner_refusal(&error);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(db.read(async |tx| tx.get_user_vtxo_by_id(source).await).await.unwrap().spend_state, SpendState::Spendable);
	assert_eq!(db.read(async |tx| tx.expiry_owner(htlc_key.public_key(), bitcoin::Network::Regtest).await)
		.await.unwrap(), Attribution::Unresolved);
	assert!(srv.htlc_vtxos(hash).await.is_empty());
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: correct.parts,
	}).await.unwrap().into_inner();
	let grants = builder.user_cosign(&[input_key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	assert_eq!(grants.len(), 1);
	assert_eq!(grants[0].amount(), input.amount());
	wallet.register_vtxo_transactions_with_server(&grants).await.unwrap();
	let before = db.read(async |tx| server::database::htlc_vtxo::get_htlc_vtxo(tx, grants[0].id()).await)
		.await.unwrap().unwrap();
	assert!(before.htlc.offchain_resolution.is_none());
	let (refund_key, _) = wallet.derive_store_next_keypair().await.unwrap();
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(grants.clone(),
		VtxoPolicy::new_pubkey(refund_key.public_key())).unwrap().generate_user_nonces(&[htlc_key]).unwrap();
	let correct = prove_wallet_destinations(&wallet, &srv,
		protos::ArkoorPackageCosignRequest::from(builder.cosign_request())).await;
	let mut invalid = correct.clone();
	for part in &mut invalid.parts {
		for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
			output.owner_proof.as_mut().unwrap().child_index += 1;
		}
	}
	assert_owner_refusal(&rpc.request_lightning_pay_htlc_revocation(invalid).await.unwrap_err());
	let after = db.read(async |tx| server::database::htlc_vtxo::get_htlc_vtxo(tx, grants[0].id()).await)
		.await.unwrap().unwrap();
	assert_eq!(after.vtxo.spend_state, before.vtxo.spend_state);
	assert_eq!(after.vtxo.oor_spent_txid, before.vtxo.oor_spent_txid);
	assert!(after.htlc.offchain_resolution.is_none(), "refusal cannot revoke the source");
	assert_eq!(db.read(async |tx| tx.expiry_owner(refund_key.public_key(), bitcoin::Network::Regtest).await)
		.await.unwrap(), Attribution::Unresolved);
	let response = rpc.request_lightning_pay_htlc_revocation(correct).await.unwrap().into_inner();
	let refunds = builder.user_cosign(&[htlc_key], ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	wallet.register_vtxo_transactions_with_server(&refunds).await.unwrap();
	assert_eq!(refunds.len(), 1);
	assert_eq!(refunds[0].amount(), input.amount());
	assert_eq!(refunds[0].user_pubkey(), refund_key.public_key());
	assert_eq!(db.read(async |tx| tx.get_user_vtxo_by_id(refunds[0].id()).await).await.unwrap().spend_state,
		SpendState::Spendable);
}

/// Simulate an isolated server registry loss while retaining the wallet's
/// authentic registration and all financial history. The rejected key is the
/// sender's new change, so the action must release its input without payment.
#[tokio::test]
async fn owner_proof_own_change_refusal_unlocks_input_without_movement() {
	let ctx = TestContext::new("server/owner_proof_own_change_refusal_unlocks_input_without_movement").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let sender = ctx.bark("sender", &srv).funded(sat(200_000)).create().await;
	let recipient = ctx.bark("recipient", &srv).create().await;
	let [source] = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await.try_into().unwrap();
	let wallet = sender.client().await;
	let recipient_wallet = recipient.client().await;
	let address = recipient_wallet.new_address().await.unwrap();
	let (change, _) = wallet.peek_next_keypair().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mailbox = wallet.mailbox_identifier().as_pubkey().to_string();
	db.write(async |tx| {
		tx.execute("DELETE FROM expiry_owner_key WHERE mailbox_pk=$1", &[&mailbox]).await?;
		tx.execute("DELETE FROM fallback_record WHERE mailbox_pk=$1", &[&mailbox]).await?;
		Ok(())
	}).await.unwrap();
	let error = tokio::time::timeout(Duration::from_secs(30), wallet.send_arkoor_payment(&address, sat(60_000)))
		.await.unwrap().unwrap_err();
	assert_eq!(error.downcast_ref::<bark::OwnerProofRefusal>(), Some(&bark::OwnerProofRefusal::Invalid));
	let status = error.downcast_ref::<tonic::Status>().unwrap();
	assert_owner_refusal(status);
	assert_eq!(status.metadata().get("output-key").unwrap().to_str().unwrap(), change.public_key().to_string());
	assert_eq!(wallet.get_vtxo_by_id(source).await.unwrap().state, VtxoState::Spendable);
	assert!(wallet.pending_arkoor_sends().await.unwrap().is_empty());
	assert!(!wallet.history().await.unwrap().iter().any(|movement|
		movement.subsystem.name == "bark.arkoor" && movement.subsystem.kind == "send"));
	assert_eq!(wallet.balance().await.unwrap().spendable, sat(100_000));
	assert_eq!(recipient_wallet.balance().await.unwrap().spendable, sat(0));
	assert_eq!(db.read(async |tx| tx.get_user_vtxo_by_id(source).await).await.unwrap().spend_state, SpendState::Spendable);
	assert_eq!(db.read(async |tx| tx.expiry_owner(address.policy().user_pubkey(), bitcoin::Network::Regtest).await)
		.await.unwrap(), Attribution::Unresolved, "the valid sibling proof must also roll back");
}
