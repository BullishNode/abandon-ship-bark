use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::ArkoorPackageBuilder;
use ark::lightning::PaymentHash;
use ark::VtxoPolicy;
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
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
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
