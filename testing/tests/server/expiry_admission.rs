use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::ArkoorPackageBuilder;
use ark::lightning::PaymentHash;
use ark::{ProtocolEncoding, VtxoPolicy};
use ark::test_util::VTXO_VECTORS;
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::{BlockDelta, BlockHeight};
use server_rpc::protos;

use ark_testing::{btc, sat, TestContext};

#[tokio::test]
async fn expiry_refuses_unbounded_requested_lightning_duration() {
	let ctx = TestContext::new("server/expiry_requested_htlc_bound").await;
	let srv = ctx.captaind("server").funded(btc(10)).create().await;
	let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
	sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let id = sender.vtxos_no_sync().await[0].vtxo.id;
	let client = sender.client().await;
	let vtxo = client.get_full_vtxo(id).await.unwrap();
	let key = client.get_vtxo_key(&vtxo).await.unwrap();
	let policy = VtxoPolicy::new_server_htlc_send(
		key.public_key(),
		PaymentHash::from(sha256::Hash::hash(b"bounded requested HTLC").to_byte_array()),
		BlockHeight::new(ctx.bitcoind().get_block_count().await as u32) + srv.config().max_htlc_expiry_delta + BlockDelta::new(1),
	);
	let builder = ArkoorPackageBuilder::new_with_checkpoints(
		[vtxo.clone()],
		vec![ArkoorDestination { policy, total_amount: vtxo.amount() }],
	).unwrap().generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let error = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: super::signed_package(&srv, &[&client], protos::ArkoorPackageCosignRequest::from(builder.cosign_request())).await.parts,
	}).await.expect_err("HTLC must not exceed configured duration");
	assert!(error.message().contains("at most"), "refusal: {error}");
	// The refusal must not persist the source as spent.
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let root = ark::ServerVtxo::from(vtxo.clone()).to_bare();
	let limits = server::database::expiry_inventory::BackingSnapshotLimits {
		max_rows: 10_000, max_serialized_bytes: 10_000_000,
	};
	let snapshot = db.read(async |t| t.backing_snapshot(&[root], limits).await).await.unwrap();
	assert!(snapshot.regular.iter().any(|fact| fact.state.vtxo_id == id));
	let states = db.read(async |t| t.get_user_vtxos_by_id(&[id]).await).await.unwrap();
	assert_eq!(states[0].spend_state, server::database::SpendState::Spendable);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	assert!(states[0].check_spendable(tip).is_ok());
}

/// Both early request rejection and the final writer fence must carry the same
/// stable reason. The durable decision here is an isolated fixture;
/// allocation-operation races have separate coverage.
#[tokio::test]
async fn expiry_native_rpc_refusals_have_stable_reasons() {
	let ctx = TestContext::new("server/expiry_typed_refusals").await;
	let srv = ctx.captaind("server").funded(btc(10)).no_vtxo_pool()
		.cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24)).create().await;
	let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx,sat(100_000)).await;
	let wallet = sender.client().await;
	let source = wallet.get_full_vtxo(ids[0]).await.unwrap();
	let key = wallet.get_vtxo_key(&source).await.unwrap();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let expired = ctx.generate_blocks(source.expiry_height().to_u32()+1-tip.to_u32()).await;
	srv.wait_for_sync_height(expired).await;
	let request = async || {
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([source.clone()],
			VtxoPolicy::new_server_htlc_send(key.public_key(),
				PaymentHash::from(sha256::Hash::hash(b"typed expiry refusal").to_byte_array()),
				expired+srv.config().htlc_send_expiry_delta+BlockDelta::new(1)),
		).unwrap().generate_user_nonces(&[key]).unwrap();
		protos::LightningPayHtlcCosignRequest {
			parts: super::signed_package(&srv, &[&wallet], protos::ArkoorPackageCosignRequest::from(builder.cosign_request())).await.parts,
		}
	};
	let mut rpc = srv.get_public_rpc().await;
	let error = rpc.request_lightning_pay_htlc_cosign(request().await).await.expect_err("new obligation on expired backing");
	assert_eq!(error.metadata().get("abandon-reason").unwrap().to_str().unwrap(), "expired_new_lightning");
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	db.write(async |t| {
		t.query_one("INSERT INTO expiry_entitlement(vtxo_id,gross_sat) VALUES($1,$2) RETURNING id",
			&[&source.id().to_string(),&100_000_i64]).await?;
		t.execute("UPDATE vtxo SET spend_state='spent',updated_at=NOW() WHERE vtxo_id=$1", &[&source.id().to_string()]).await?;
		Ok(())
	}).await.unwrap();
	let (full,bare,policy) = db.read(async |t| Ok((
		t.get_user_vtxo_by_id(source.id()).await?,
		t.get_bare_vtxo_by_id(source.id()).await?,
		t.get_vtxos_by_policy_page(source.policy(), None, 10).await?,
	))).await.unwrap();
	assert!(full.decided_for_fallback && bare.decided_for_fallback);
	assert!(policy.iter().find(|v| v.vtxo_id==source.id()).unwrap().decided_for_fallback);
	let error = rpc.request_lightning_pay_htlc_cosign(request().await).await.expect_err("already-decided source must have a specific refusal");
	assert_eq!(error.metadata().get("abandon-reason").unwrap().to_str().unwrap(), "decided_for_fallback");
	assert_eq!(full.vtxo.amount(),sat(100_000));
}

/// Each public issuance path rejects a missing link while its original request
/// metadata is still available, before canonical conversion or money handling.
#[tokio::test]
async fn ownership_links_are_required_at_all_eight_admissions() {
	let ctx = TestContext::new("server/owner_links_all_admissions").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let key = VTXO_VECTORS.board_vtxo.user_pubkey();
	let policy = VtxoPolicy::new_pubkey(key).serialize();
	let destination = protos::ArkoorDestination {
		total_amount: 100_000, policy: policy.clone(), owner_proof: None,
	};
	let part = protos::ArkoorCosignRequest { outputs: vec![destination], ..Default::default() };
	let package = || protos::ArkoorPackageCosignRequest { parts: vec![part.clone()] };
	let refusal = |status: tonic::Status| {
		assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
		assert_eq!(status.metadata().get("abandon-reason").unwrap().to_str().unwrap(), "owner_proof_required");
		assert_eq!(status.metadata().get("identifiers").unwrap().to_str().unwrap(), key.to_string());
	};
	refusal(rpc.request_board_cosign(protos::BoardCosignRequest {
		user_pubkey: key.serialize().to_vec(), ..Default::default()
	}).await.unwrap_err());
	refusal(rpc.request_arkoor_cosign(package()).await.unwrap_err());
	refusal(rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: vec![part.clone()],
	}).await.unwrap_err());
	refusal(rpc.request_lightning_pay_htlc_revocation(package()).await.unwrap_err());
	refusal(rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: vec![1; 32], user_pubkey: key.serialize().to_vec(), ..Default::default()
	}).await.unwrap_err());
	refusal(rpc.claim_lightning_receive(protos::ClaimLightningReceiveRequest {
		payment_hash: vec![1; 32], payment_preimage: vec![2; 32], cosign_request: Some(package()),
	}).await.unwrap_err());
	let output = protos::VtxoRequest { amount: 100_000, policy, owner_proof: None };
	refusal(rpc.submit_payment(protos::SubmitPaymentRequest {
		vtxo_requests: vec![protos::SignedVtxoRequest { vtxo: Some(output.clone()), ..Default::default() }],
		..Default::default()
	}).await.unwrap_err());
	refusal(rpc.submit_round_participation(protos::RoundParticipationRequest {
		vtxo_requests: vec![output], ..Default::default()
	}).await.unwrap_err());
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let counts = db.read(async |tx| Ok(tx.query_one("SELECT
		(SELECT count(*) FROM expiry_owner_key),
		(SELECT count(*) FROM lightning_payment_attempt),
		(SELECT count(*) FROM lightning_htlc_subscription)", &[]).await?)).await.unwrap();
	assert_eq!((counts.get::<_, i64>(0), counts.get::<_, i64>(1), counts.get::<_, i64>(2)), (0, 0, 0));
}
