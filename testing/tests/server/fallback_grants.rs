use std::sync::Arc;

use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;
use ark_testing::{btc, lightning_test, Captaind, TestContext};
use ark_testing::context::LightningPaymentSetup;
use ark_testing::util::FutureExt;
use server::database::Db;
use server_rpc::protos;

async fn grant_rows(db: &Db) -> i64 {
	db.read(async |tx| Ok(tx.query_one(
		"SELECT count(*) FROM vtxo WHERE spend_state='htlc-recv-unclaimed'", &[],
	).await?.get(0))).await.unwrap()
}

async fn server_grant_requires_origin(
	ctx: &TestContext, _lightning: &LightningPaymentSetup, srv: &Captaind,
	pay: impl AsyncFn(String),
) {
	srv.wait_for_vtxopool(ctx).await;
	let issuer = ctx.bark("issuer", srv).funded(btc(3)).create().await;
	issuer.board_and_confirm_and_register(ctx, btc(2)).await;
	let invoice = issuer.bolt11_invoice(btc(1)).await;
	let receive = issuer.lightning_receive_status(&invoice.invoice).await.unwrap();
	let mut rpc = srv.get_public_rpc().await;
	tokio::select! {
		_ = pay(invoice.invoice) => panic!("payment completed before granting any HTLC"),
		_ = async {
			rpc.check_lightning_receive(protos::CheckLightningReceiveRequest {
				hash: receive.payment_hash.to_vec(), wait: true,
			}).wait_millis(10_000).await.unwrap();
		} => {},
	}
	let wallet = issuer.client().await;
	let boarded_key = wallet.vtxos().await.unwrap()[0].user_pubkey();
	let (_, key) = wallet.pubkey_keypair(&boarded_key).await.unwrap().unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let before = grant_rows(&db).await;
	let error = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: receive.payment_hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: ctx.bitcoind().get_block_count().await as u32 + 1, lightning_receive_anti_dos: None, origin: None,
	}).await.expect_err("an unproven grant must not allocate customer coins");
	assert_eq!(error.code(), tonic::Code::InvalidArgument);
	assert!(error.message().contains("missing account child origin"), "{error}");
	assert_eq!(grant_rows(&db).await, before, "a refused grant must not allocate HTLCs");
}
lightning_test!(server_grant_requires_origin);

async fn server_grant_refuses_another_account(
	ctx: &TestContext, _lightning: &LightningPaymentSetup, srv: &Captaind,
	pay: impl AsyncFn(String),
) {
	srv.wait_for_vtxopool(ctx).await;
	let issuer = ctx.bark("issuer", srv).funded(btc(3)).create().await;
	issuer.board_and_confirm_and_register(ctx, btc(2)).await;
	let other = ctx.bark("other", srv).funded(btc(3)).create().await;
	other.board_and_confirm_and_register(ctx, btc(2)).await;
	let other_wallet = other.client().await;
	let other_pubkey = other_wallet.vtxos().await.unwrap()[0].user_pubkey();
	let (_, other_key) = other_wallet.pubkey_keypair(&other_pubkey).await.unwrap().unwrap();
	let other_origin = other_wallet.fallback_child_hint(other_pubkey).await.unwrap().into();
	let invoice = issuer.bolt11_invoice(btc(1)).await;
	let receive = issuer.lightning_receive_status(&invoice.invoice).await.unwrap();
	let mut rpc = srv.get_public_rpc().await;
	tokio::select! {
		_ = pay(invoice.invoice) => panic!("payment completed before granting any HTLC"),
		_ = async {
			rpc.check_lightning_receive(protos::CheckLightningReceiveRequest {
				hash: receive.payment_hash.to_vec(), wait: true,
			}).wait_millis(10_000).await.unwrap();
		} => {},
	}
	let request = protos::PrepareLightningReceiveClaimRequest {
		payment_hash: receive.payment_hash.to_vec(), user_pubkey: other_key.public_key().serialize().to_vec(),
		htlc_recv_expiry: ctx.bitcoind().get_block_count().await as u32 + 1, lightning_receive_anti_dos: None, origin: Some(other_origin),
	};
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let before = grant_rows(&db).await;
	let error = match rpc.prepare_lightning_receive_claim(request.clone()).await {
		Err(error) => error,
		Ok(_) => panic!("another registered account must not take the issuer's grant"),
	};
	assert_eq!(error.code(), tonic::Code::InvalidArgument);
	assert!(error.message().contains("claim account does not match invoice receiver"), "{error}");
	assert_eq!(grant_rows(&db).await, before);
	// A legitimate forwarder claims under its own invoice account. The final
	// phone destination belongs to the later claim, not to grant preparation.
	let wallet = issuer.client().await;
	let (key, _) = wallet.derive_store_next_keypair().await.unwrap();
	let origin = wallet.fallback_child_hint(key.public_key()).await.unwrap();
	let granted = rpc.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		payment_hash: receive.payment_hash.to_vec(), user_pubkey: key.public_key().serialize().to_vec(),
		htlc_recv_expiry: ctx.bitcoind().get_block_count().await as u32 + 1, lightning_receive_anti_dos: None, origin: Some(origin.into()),
	}).await.unwrap().into_inner();
	assert!(!granted.htlc_vtxos.is_empty());
	for raw in &granted.htlc_vtxos {
		let vtxo = Vtxo::<Full>::deserialize(raw).unwrap();
		assert_eq!(vtxo.user_pubkey(), key.public_key());
		assert!(vtxo.has_all_witnesses());
	}
	// The immutable issuer check must also run before an idempotent readback.
	let error = rpc.prepare_lightning_receive_claim(request).await.unwrap_err();
	assert_eq!(error.code(), tonic::Code::InvalidArgument);
	assert!(error.message().contains("claim account does not match invoice receiver"), "{error}");
}
lightning_test!(server_grant_refuses_another_account);
