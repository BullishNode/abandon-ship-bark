//! Successful receives deliver the actual recipient's signed value once.

use std::time::Duration;
use ark::lightning::PaymentHash;
use ark_testing::{btc, sat, TestContext};
use bark::actions::lightning::receive::LightningReceiveState;

#[tokio::test]
async fn receive_binding_normal_external_claim_keeps_the_actual_recipient() {
	let ctx = TestContext::new("bark_sdk/receive_binding_normal_external_claim").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let provider = ctx.bark_sdk("provider", &srv).boarded(sat(100_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let address = recipient.new_address().await.unwrap();
	let recipient_key = address.policy().user_pubkey();
	assert_ne!(provider.mailbox_identifier(), recipient.mailbox_identifier());
	let invoice = provider.bolt11_invoice_for_address(sat(50_000), address, None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	lightning.sync().await;
	let (paid, claimed) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(
			lightning.external.try_pay_bolt11(invoice.to_string()),
			provider.try_claim_lightning_receive(hash, true),
		)
	}).await.expect("normal receive did not complete");
	paid.expect("external payment failed");
	assert!(matches!(claimed.unwrap(), LightningReceiveState::Settled(_)));
	recipient.sync().await;
	let coins = recipient.spendable_vtxos().await.unwrap();
	assert!(!coins.is_empty(), "normal delivery must give the recipient its signed coins");
	let received = coins.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>();
	assert_eq!(received, sat(50_000));
	for coin in &coins {
		assert_eq!(coin.user_pubkey(), recipient_key);
		let full = recipient.get_full_vtxo(coin.id()).await.unwrap();
		let funding = ctx.bitcoind().await_transaction(full.chain_anchor().txid).await;
		full.validate(&funding).unwrap();
	}
	assert!(matches!(provider.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
	recipient.sync().await;
	assert_eq!(recipient.spendable_vtxos().await.unwrap().iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), received,
		"an identical claim retry cannot pay the recipient twice");
}

#[tokio::test]
async fn receive_binding_normal_internal_claim_and_reconciliation_are_idempotent() {
	let ctx = TestContext::new("bark_sdk/receive_binding_normal_internal_claim").await;
	let lightning = ctx.new_lightning_setup_no_channel("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(200_000))
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	let invoice = recipient.bolt11_invoice(sat(50_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let (paid, claimed) = tokio::time::timeout(Duration::from_secs(60), async {
		tokio::join!(
			sender.pay_lightning_invoice(invoice, None, true),
			recipient.try_claim_lightning_receive(hash, true),
		)
	}).await.expect("normal internal receive did not complete");
	paid.unwrap();
	assert!(matches!(claimed.unwrap(), LightningReceiveState::Settled(_)));
	assert!(matches!(recipient.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
	recipient.sync().await;
	let receipt = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(receipt.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(50_000));
	let mut external = lightning.internal.grpc_client().await;
	let pays = external.list_send_pays(cln_rpc::ListsendpaysRequest {
		payment_hash: Some(hash.to_vec()), ..Default::default()
	}).await.unwrap().into_inner();
	assert!(pays.payments.is_empty(), "actual internal receive must not be sent through external CLN");
}
