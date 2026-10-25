use ark::address::VtxoDelivery;
use bark::movement::MovementStatus;

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

/// Old/absent ownership links are refused before allocating a change key or
/// reserving any input, including an otherwise valid registered recipient.
#[tokio::test]
async fn send_to_address_without_signed_link_is_refused() {
	let ctx = TestContext::new("bark_sdk/send_without_signed_link_refused").await;
	let srv = ctx.captaind("server").create().await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(400_000)).create().await;
	let receiver = ctx.bark_sdk("recipient", &srv).create().await;
	let address = receiver.new_address().await.unwrap();
	let blinded = address.delivery().iter().find_map(VtxoDelivery::mailbox_id).unwrap();
	let before = sender.peek_next_keypair().await.unwrap().1;
	for delivery in [vec![], vec![VtxoDelivery::ServerMailbox { blinded_id: blinded }]] {
		let old = ark::Address::new(address.is_testnet(), address.ark_id(), address.policy().clone(), delivery);
		let error = sender.send_arkoor_payment(&old, sat(100_000)).await.unwrap_err();
		assert_eq!(bark::FallbackRefusal::from_error(&error), Some(bark::FallbackRefusal::RecipientMustUpdate));
		assert_eq!(sender.peek_next_keypair().await.unwrap().1, before);
		assert!(sender.pending_arkoor_sends().await.unwrap().is_empty());
		assert_eq!(assert_balance_consistent(&sender, false).await.spendable, sat(400_000));
	}
}

/// Each arkoor send returns the movement that records it, also when two
/// payments have the same amount and destination.
#[tokio::test]
async fn send_returns_its_movement_id() {
	let ctx = TestContext::new("bark_sdk/send_returns_its_movement_id").await;
	let srv = ctx.captaind("server").create().await;

	let sender = ctx.bark_sdk("bark", &srv)
		.boarded(sat(400_000))
		.create().await;
	let receiver = ctx.bark_sdk("bark2", &srv).create().await;

	let address = receiver.new_address().await.expect("new address");
	let first = sender.send_arkoor_payment(&address, sat(10_000)).await.expect("first send");
	let second = sender.send_arkoor_payment(&address, sat(10_000)).await.expect("second send");
	assert_ne!(first, second);

	let history = sender.history().await.expect("history");
	let mut arkoor_ids = history.iter()
		.filter(|m| m.subsystem.name == "bark.arkoor")
		.map(|m| m.id)
		.collect::<Vec<_>>();
	arkoor_ids.sort();
	assert_eq!(arkoor_ids, vec![first, second]);
	for id in [first, second] {
		let movement = history.iter().find(|m| m.id == id).unwrap();
		assert_eq!(movement.status, MovementStatus::Successful);
	}
}

#[tokio::test]
async fn offline_signed_addresses_receive_split_and_isolated_outputs() {
	let ctx = TestContext::new("bark_sdk/offline_signed_addresses_split_isolated").await;
	let srv = ctx.captaind("server").create().await;
	let sender = ctx.bark_sdk("sender", &srv).boarded(sat(400_000))
		.cfg(|cfg| { cfg.daemon_manual_sync = true; cfg.change_vtxo_split_factor = 3; }).create().await;
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	sender.stop_daemon_wait().await.unwrap();
	recipient.stop_daemon_wait().await.unwrap();
	let recipient_mailbox = recipient.mailbox_identifier();
	let sender_mnemonic = tokio::fs::read_to_string(ctx.datadir.join("sender/mnemonic"))
		.await.unwrap().trim().parse::<bip39::Mnemonic>().unwrap();
	let recipient_mnemonic = tokio::fs::read_to_string(ctx.datadir.join("recipient/mnemonic"))
		.await.unwrap().trim().parse::<bip39::Mnemonic>().unwrap();
	srv.stop().await.unwrap();
	let first = recipient.new_address().await.unwrap();
	let second = recipient.new_address().await.unwrap();
	assert!(first.delivery().iter().all(|delivery| matches!(delivery, VtxoDelivery::ServerMailboxSigned { .. })));
	drop(recipient);
	drop(sender);
	srv.start().await.unwrap();
	// The fixture restart reserves a new RPC port. Reopen only the sender
	// against that endpoint, preserving its seed, keys and registered script.
	let sender = ctx.bark_sdk("sender", &srv).mnemonic(sender_mnemonic).cfg(|cfg| {
		cfg.daemon_manual_sync = true;
		cfg.change_vtxo_split_factor = 3;
	}).create().await;
	sender.stop_daemon_wait().await.unwrap();
	// The first payment makes three 100,000-sat change outputs; the second isolates change of 1 sat.
	sender.send_arkoor_payment(&first, sat(100_000)).await.unwrap();
	sender.send_arkoor_payment(&second, sat(99_999)).await.unwrap();
	assert_eq!(assert_balance_consistent(&sender, false).await.spendable, sat(200_001));
	assert!(sender.spendable_vtxos().await.unwrap().iter().any(|coin| coin.vtxo.amount() == sat(1)));
	let reopened = ctx.bark_sdk("recipient", &srv).mnemonic(recipient_mnemonic)
		.cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	reopened.sync_mailbox().await.unwrap();
	assert_eq!(assert_balance_consistent(&reopened, false).await.spendable, sat(199_999));
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	for address in [first, second] {
		let owner = db.read(async |tx| tx.expiry_owner(address.policy().user_pubkey(), bitcoin::Network::Regtest,
			&srv.mailbox_keypair()).await).await.unwrap();
		assert!(matches!(owner, server::database::expiry_attribution::Attribution::Owned { mailbox_pk, .. }
			if mailbox_pk == recipient_mailbox.as_pubkey()));
	}
}
