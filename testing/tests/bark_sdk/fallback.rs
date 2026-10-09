use std::time::Duration;

use bitcoin::{Address, Network};
use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
use bitcoin::hashes::{sha256, Hash};
use bark::lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};

use ark_testing::{TestContext, btc, sat};
use server::database::Db;

#[tokio::test]
async fn fallback_register_board_rotate_and_offline_pool() {
	let ctx = TestContext::new("bark_sdk/fallback_register_board_rotate_and_offline_pool").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let wallet = ctx.bark_sdk("wallet", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	assert!(record.spk.is_p2wpkh());
	let mailbox = wallet.mailbox_keypair().public_key().serialize().to_vec();
	let stored = db.read(async |t| Ok(t.query_one(
		"SELECT spk, seq FROM fallback_record WHERE mailbox_pk = $1", &[&mailbox],
	).await?)).await.unwrap();
	assert_eq!(stored.get::<_, Vec<u8>>(0), record.spk.as_bytes());
	assert_eq!(stored.get::<_, i64>(1), record.seq as i64);
	for vtxo in wallet.spendable_vtxos().await.unwrap() {
		let pk = vtxo.user_pubkey().serialize().to_vec();
		let linked = db.read(async |t| Ok(t.query_one(
			"SELECT mailbox_pk FROM key_link WHERE user_pubkey = $1", &[&pk],
		).await?)).await.unwrap();
		assert_eq!(linked.get::<_, Vec<u8>>(0), mailbox);
	}

	// The rotation trigger is an actual chain receive. S5 separately proves
	// the automatic expiry transaction; this test isolates client registration.
	let address = Address::from_script(&record.spk, Network::Regtest).unwrap();
	ctx.bitcoind().fund_addr(address, sat(20_000)).await;
	ctx.generate_blocks(1).await;
	wallet.sync_onchain().await.unwrap();
	let rotated = wallet.fallback_destination().await.unwrap();
	assert_ne!(rotated.spk, record.spk);
	assert!(rotated.seq > record.seq);
	let foreign = ctx.bitcoind().get_new_address().script_pubkey();
	assert!(wallet.set_fallback_destination(foreign).await.is_err());
	assert_eq!(wallet.fallback_destination().await.unwrap(), rotated);

	wallet.sync().await;
	srv.stop().await.unwrap();
	let mut keys = Vec::<PublicKey>::new();
	for _ in 0..8 {
		let (address, _) = wallet.new_address_with_index().await.unwrap();
		let pk = address.policy().user_pubkey();
		assert!(!keys.contains(&pk));
		keys.push(pk);
	}
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.new_address()).await
		.expect("offline exhaustion returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"));
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.board_amount(sat(10_000))).await
		.expect("offline board returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"));
	assert!(wallet.pending_boards().await.unwrap().is_empty());

	// An empty pool must fail before Lightning send locks any input. No LN
	// node is needed: the valid invoice must never reach an HTLC cosign step.
	let secp = Secp256k1::new();
	let key = SecretKey::from_slice(&[42; 32]).unwrap();
	let invoice = InvoiceBuilder::new(Currency::Regtest)
		.description("offline linking failure".into())
		.payment_hash(sha256::Hash::hash(b"fallback offline send"))
		.payment_secret(PaymentSecret([43; 32]))
		.current_timestamp().min_final_cltv_expiry_delta(144)
		.amount_milli_satoshis(10_000_000)
		.build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &key)).unwrap();
	let before = wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<Vec<_>>();
	assert!(!before.is_empty());
	let err = tokio::time::timeout(Duration::from_secs(15), wallet.pay_lightning_invoice(invoice, None, false))
		.await.expect("offline LN send returns promptly").unwrap_err();
	assert!(format!("{err:#}").contains("no linked keys available"), "{err:#}");
	assert_eq!(wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<Vec<_>>(), before);
	assert!(wallet.pending_lightning_sends().await.unwrap().is_empty());
}
