//! Normal successful receive-for-address retains the intended recipient in the
//! server's durable receive binding, independently of the provider's key.

use std::time::Duration;

use ark::{ProtocolEncoding, ServerVtxo, Vtxo};
use ark::lightning::PaymentHash;
use ark::vtxo::Full;
use ark_testing::{btc, sat, TestContext};
use bark::actions::lightning::receive::LightningReceiveState;
use server::database::Db;

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
	assert_ne!(provider.fallback_destination().await.unwrap().account_xpub,
		recipient.fallback_destination().await.unwrap().account_xpub);
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
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let bindings = db.read(async |t| Ok(t.query("SELECT c.source_vtxo_id,c.output_vtxo_id,c.output_vtxo, \
		c.materialized,source.vtxo AS source_vtxo,actual.vtxo AS actual_vtxo, \
		h.offchain_resolution::TEXT AS resolution,s.status::TEXT AS status \
		FROM expiry_lightning_receive_claim c \
		JOIN vtxo source ON source.vtxo_id=c.source_vtxo_id \
		JOIN htlc_vtxo h ON h.id=source.id \
		JOIN vtxo actual ON actual.vtxo_id=c.output_vtxo_id \
		JOIN lightning_htlc_subscription s ON s.id=c.subscription_id \
		WHERE c.payment_hash=$1", &[&hash.to_string()]).await?)).await.unwrap();
	assert!(!bindings.is_empty());
	let mut received = bitcoin::Amount::ZERO;
	for row in &bindings {
		let original = Vtxo::<Full>::deserialize(row.get("output_vtxo")).unwrap();
		let actual = Vtxo::<Full>::deserialize(row.get("actual_vtxo")).unwrap();
		let source = ServerVtxo::<Full>::deserialize(row.get("source_vtxo")).unwrap().try_into_user_vtxo().unwrap();
		assert!(row.get::<_, bool>("materialized"));
		assert_eq!(row.get::<_, &str>("resolution"), "fulfilled");
		assert_eq!(row.get::<_, &str>("status"), "settled");
		assert_eq!(original.id(), actual.id());
		assert_eq!(original.user_pubkey(), recipient_key);
		assert_eq!(actual.user_pubkey(), recipient_key);
		assert_ne!(source.user_pubkey(), recipient_key);
		assert_eq!(source.amount(), original.amount());
		assert!(coins.iter().any(|coin| coin.id() == actual.id()));
		received += actual.amount();
	}
	assert_eq!(coins.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), received);
	// A completed normal action is idempotent; it cannot mint another binding.
	assert!(matches!(provider.try_claim_lightning_receive(hash, true).await.unwrap(), LightningReceiveState::Settled(_)));
	let count = db.read(async |t| Ok(t.query_one("SELECT COUNT(*) FROM expiry_lightning_receive_claim WHERE payment_hash=$1", &[&hash.to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(count, bindings.len() as i64);
}
