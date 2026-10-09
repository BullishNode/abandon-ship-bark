use ark::address::VtxoDelivery;

use ark_testing::{TestContext, sat};
use ark_testing::balance::assert_balance_consistent;

/// An address that lists no delivery mechanism this bark can use is refused
/// before the arkoor is built, so the sender keeps its funds.
#[tokio::test]
async fn send_to_unsupported_delivery_is_refused() {
	let ctx = TestContext::new("bark_sdk/send_to_unsupported_delivery_is_refused").await;
	let srv = ctx.captaind("server").create().await;

	let sender = ctx.bark_sdk("bark", &srv)
		.boarded(sat(400_000))
		.create().await;
	let receiver = ctx.bark_sdk("bark2", &srv).create().await;

	// Same server and policy as a payable address, but the only way it offers
	// to hand over the VTXO is a mechanism this bark doesn't know.
	let address = receiver.new_address().await.expect("new address");
	let address = ark::Address::new(
		address.is_testnet(),
		address.ark_id(),
		address.policy().clone(),
		vec![VtxoDelivery::Unknown { delivery_type: 0xff, data: vec![1, 2, 3] }],
	);

	let err = sender.send_arkoor_payment(&address, sat(100_000)).await
		.expect_err("send to an undeliverable address must fail");
	assert!(format!("{:#}", err).contains("Unknown delivery mechanism"), "err: {err:#}");

	// The send never started: no action is pending and no VTXO was spent.
	assert!(sender.pending_arkoor_sends().await.unwrap().is_empty());
	assert_eq!(assert_balance_consistent(&sender, false).await.spendable, sat(400_000));
}

/// An address that lists no delivery mechanism at all is the receiver's
/// choice to pick up the VTXO out-of-band: the payment succeeds and bark
/// never attempts a delivery.
#[tokio::test]
async fn send_to_address_without_delivery_succeeds() {
	let ctx = TestContext::new("bark_sdk/send_to_address_without_delivery_succeeds").await;
	let srv = ctx.captaind("server").create().await;

	let sender = ctx.bark_sdk("bark", &srv)
		.boarded(sat(400_000))
		.create().await;

	// The recipient has a fallback record but opts out of mailbox delivery.
	let receiver = ctx.bark_sdk("receiver", &srv).create().await;
	let recipient = receiver.new_address().await.expect("linked recipient address");
	let address = ark::Address::new(
		recipient.is_testnet(),
		recipient.ark_id(),
		recipient.policy().clone(),
		vec![],
	);

	sender.send_arkoor_payment(&address, sat(100_000)).await
		.expect("send to a delivery-less address must succeed");

	// The send ran to completion and only the change remains spendable.
	assert!(sender.pending_arkoor_sends().await.unwrap().is_empty());
	assert_eq!(assert_balance_consistent(&sender, false).await.spendable, sat(300_000));
}
