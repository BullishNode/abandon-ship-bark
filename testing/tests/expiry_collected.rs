//! Actual external collection before the recipient loses its cooperative reply.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::{ScriptBuf, Transaction, TxIn, TxOut, Witness};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::taproot::{LeafVersion, TapLeafHash};
use bitcoincore_rpc::RpcApi;

use ark::{ProtocolEncoding, VtxoId};
use ark::lightning::PaymentHash;
use ark::vtxo::policy::signing::VtxoSigner;
use ark::vtxo::policy::clause::HashDelaySignClause;
use bitcoin_ext::BlockDelta;
use cln_rpc::plugins::hold;
use server::database::Db;
use server::database::carrier_snapshot::BackingSnapshotLimits;
use server::database::ln::LightningHtlcSubscriptionStatus;
use server_rpc::protos;

use ark_testing::{btc, sat, TestContext};

fn retain_private(path: &std::path::Path, bytes: &[u8]) {
	OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap().write_all(bytes).unwrap();
}

#[tokio::test]
async fn collected_external_receive_lost_claim_reply_freezes_original_recipient() {
	let ctx = TestContext::new("expiry_collected/collected_external_receive_lost_claim_reply_freezes_original_recipient").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).lightningd(&lightning.internal).funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(512); c.vtxopool.vtxo_lifetime = BlockDelta::new(512);
		c.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget { count: 2, amount: sat(100_000) }];
		c.expiry_payout.grace_blocks = 0;
	}).create().await;
	tokio::time::timeout(Duration::from_secs(45), srv.wait_for_vtxopool(&ctx)).await.unwrap();
	lightning.sync().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let receiver = ctx.bark_sdk("absent_receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let fallback = receiver.fallback_destination().await.unwrap();
	let invoice = receiver.bolt11_invoice(sat(100_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let preimage = receiver.lightning_receive_checkpoint(hash).await.unwrap().unwrap().payment_preimage;
	let binding = db.read(async |t| t.get_lightning_receiver_mailbox_id(hash).await).await.unwrap().unwrap();
	let mut payer = lightning.external.grpc_client().await;
	let paying = tokio::spawn(async move { payer.xpay(cln_rpc::XpayRequest {
		invstring: invoice.to_string(), amount_msat: None, maxfee: None, layers: vec![], retry_for: None,
		partial_msat: None, maxdelay: None, payer_note: None, label: None, localinvreqid: None, dev_use_shadow: None,
	}).await });
	lightning.internal.wait_for_hold_invoice_accepted(hash).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		while db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap().status
			!= LightningHtlcSubscriptionStatus::Accepted { tokio::time::sleep(Duration::from_millis(50)).await; }
	}).await.unwrap();
	let foreign = ctx.bark_sdk("foreign", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	foreign.stop_daemon_wait().await.unwrap();
	let foreign_script = foreign.fallback_destination().await.unwrap().spk;
	assert_ne!(foreign_script, fallback.spk);
	let (foreign_key, _) = foreign.derive_store_next_keypair().await.unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	let refused = srv.get_public_rpc().await.prepare_lightning_receive_claim(protos::PrepareLightningReceiveClaimRequest {
		origin: Some(foreign.fallback_child_hint(foreign_key.public_key()).await.unwrap().into()),
		payment_hash: hash.to_vec(), user_pubkey: foreign_key.public_key().serialize().to_vec(),
		htlc_recv_expiry: sub.lowest_incoming_htlc_expiry.unwrap().saturating_sub(srv.config().htlc_expiry_delta).into(),
		lightning_receive_anti_dos: None,
	}).await.unwrap_err();
	assert!(refused.message().contains("claim account does not match invoice receiver"));
	drop(foreign);
	// This trigger blocks only the later cooperative spend. Upstream records
	// the real preimage and settles the actual hold before reaching this update;
	// server cosigning happens only after this transaction commits.
	db.write(async |t| {
		t.batch_execute("CREATE FUNCTION pause_collected_receive_spend() RETURNS trigger AS $$ BEGIN
			IF OLD.spend_state='htlc-recv-unclaimed' AND OLD.oor_spent_txid IS NULL AND NEW.oor_spent_txid IS NOT NULL THEN
				PERFORM pg_advisory_xact_lock(510014100::bigint);
				RAISE EXCEPTION 'test rolls back cooperative spend after actual external collection';
			END IF; RETURN NEW; END; $$ LANGUAGE plpgsql;
			CREATE TRIGGER pause_collected_receive_spend BEFORE UPDATE ON vtxo FOR EACH ROW EXECUTE FUNCTION pause_collected_receive_spend();").await?;
		Ok(())
	}).await.unwrap();
	let entered = Arc::new(tokio::sync::Notify::new()); let release = Arc::new(tokio::sync::Notify::new());
	let barrier_db = db.clone(); let enter = entered.clone(); let leave = release.clone();
	let barrier = tokio::spawn(async move { barrier_db.write(async |t| {
		t.execute("SELECT pg_advisory_xact_lock(510014100::bigint)", &[]).await?;
		enter.notify_one(); leave.notified().await; Ok(())
	}).await });
	entered.notified().await;
	let active = receiver.clone();
	let claiming = tokio::spawn(async move { active.try_claim_lightning_receive(hash, true).await });
	tokio::time::timeout(Duration::from_secs(45), async {
		loop {
			let waiting: bool = db.read(async |t| Ok(t.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory'
				AND classid::bigint=0 AND objid::bigint=510014100 AND NOT granted
				AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))", &[]).await?.get(0))).await.unwrap();
			if waiting { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual SDK claim must reach post-collection cooperative spend barrier");
	assert!(!claiming.is_finished(), "the recipient cannot have adopted a blocked claim reply");
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap() == Some(preimage),
		"the recipient's genuine preimage must be durable before the blocked spend");
	let invoices = lightning.internal.hold_client().await.list(hold::ListRequest {
		constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
	}).await.unwrap().into_inner().invoices;
	assert_eq!(invoices.len(), 1); assert_eq!(invoices[0].state(), hold::InvoiceState::Paid);
	tokio::time::timeout(Duration::from_secs(30), paying).await.unwrap().unwrap().unwrap();
	let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await).await.unwrap().unwrap();
	assert_eq!(sub.status, LightningHtlcSubscriptionStatus::Settled);
	assert!(db.read(async |t| t.get_payment_attempts_by_payment_hash(hash).await).await.unwrap().is_empty(), "external receive has no fabricated Ark sender attempt");
	assert_eq!(db.read(async |t| t.get_lightning_receiver_mailbox_id(hash).await).await.unwrap(), Some(binding));
	let grants = db.read(async |t| t.get_user_vtxos_by_id(&sub.htlc_vtxos).await).await.unwrap();
	assert_eq!(grants.len(), 1, "one genuine pool lot must give one receive grant");
	let grant = grants[0].vtxo.clone();
	let owner = grant.user_pubkey();
	assert_eq!(grant.amount(), sat(100_000));
	let core = ctx.bitcoind().sync_client();
	let funding = core.get_raw_transaction(&grant.chain_anchor().txid, None).unwrap();
	grant.validate(&funding).unwrap();
	assert!(grant.transactions().all(|item| item.tx.input.len() == 1), "no multiple-parent ancestry in this fixture");
	retain_private(&ctx.datadir.join("original-signed-grant.bin"), &grant.serialize());
	// Preserve a genuinely signed independent receiver claim, including its
	// preimage privately. Signature verification is not relay or final retirement;
	// its ancestor confirmation/delay still matters if it is later broadcast.
	let clause = receiver.find_signable_clause(&grant).await.unwrap();
	let mut user_claim = Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![TxIn { previous_output: grant.point(), sequence: clause.sequence().unwrap(), script_sig: ScriptBuf::new(), witness: Witness::new() }],
		output: vec![TxOut { value: grant.amount() - sat(2_000), script_pubkey: fallback.spk.clone() }],
	};
	user_claim.input[0].witness = receiver.sign_input(&grant, 0, &mut SighashCache::new(&user_claim), &Prevouts::All(&[grant.txout()])).await.unwrap();
	let witness = &user_claim.input[0].witness;
	let signature = bitcoin::secp256k1::schnorr::Signature::from_slice(witness.iter().next().unwrap()).unwrap();
	assert!(HashDelaySignClause::extract_preimage_from_witness(witness, hash) == Some(preimage));
	let sighash = SighashCache::new(&user_claim).taproot_script_spend_signature_hash(0,
		&Prevouts::All(&[grant.txout()]), TapLeafHash::from_script(&clause.tapscript(), LeafVersion::TapScript), TapSighashType::Default).unwrap();
	bitcoin::secp256k1::Secp256k1::verification_only().verify_schnorr(&signature, &sighash.into(), &owner.x_only_public_key().0).unwrap();
	retain_private(&ctx.datadir.join("original-signed-user-claim.tx"), &bitcoin::consensus::serialize(&user_claim));
	claiming.abort(); assert!(claiming.await.unwrap_err().is_cancelled()); drop(receiver);
	release.notify_one(); barrier.await.unwrap().unwrap();
	db.write(async |t| { t.batch_execute("DROP TRIGGER pause_collected_receive_spend ON vtxo; DROP FUNCTION pause_collected_receive_spend();").await?; Ok(()) }).await.unwrap();
	let saved = db.read(async |t| t.get_user_vtxos_by_id(&sub.htlc_vtxos).await).await.unwrap();
	assert_eq!(saved[0].spend_state, server::database::SpendState::HtlcRecvUnclaimed);
	assert!(saved[0].oor_spent_txid.is_none());
	let config = srv.config().clone(); srv.stop().await.unwrap();
	let server = server::Server::start(config).await.unwrap();
	let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(grant.chain_anchor().into()).await?.vtxo)).await.unwrap();
	let budget = BackingSnapshotLimits { max_rows: 1_000, max_serialized_bytes: 10_000_000 };
	let snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], budget).await).await.unwrap();
	let ancestors = snapshot.regular.iter().filter_map(|f| f.full_vtxo.as_ref()).flat_map(|v| v.transactions())
		.flat_map(|item| item.tx.input.into_iter().map(|i| VtxoId::from(i.previous_output))).collect::<BTreeSet<_>>();
	let terminals = snapshot.regular.iter().filter(|f| !ancestors.contains(&f.state.vtxo_id)).map(|f| {
		assert!(f.full_vtxo.is_some(), "complete source cut needs its actual stored full paths"); f.state.vtxo_id
	}).collect::<Vec<_>>();
	assert!(snapshot.pending_boards.is_empty()); assert!(terminals.contains(&grant.id()));
	let expiry = snapshot.regular.iter().map(|f| f.state.vtxo.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let frozen = server.freeze_current_expiry_backing(&[root.id()], &terminals, budget).await.unwrap_or_else(|e|
		panic!("actual externally collected grant must freeze for its immutable recipient: {e:#}"));
	let row = db.read(async |t| t.query_one("SELECT source,disposition,spk,owner_pubkey,gross_sat,fee_sat,payout_sat FROM expiry_allocation WHERE id=$1", &[&grant.id().to_string()]).await.map_err(Into::into)).await.unwrap();
	assert_eq!(row.get::<_, &str>("source"), format!("lightning-receive:{hash}"));
	assert_eq!(row.get::<_, &str>("disposition"), "pay");
	assert_eq!(row.get::<_, Vec<u8>>("spk"), fallback.spk.as_bytes());
	assert_ne!(row.get::<_, Vec<u8>>("spk"), foreign_script.as_bytes());
	assert_eq!(row.get::<_, Vec<u8>>("owner_pubkey"), owner.serialize());
	assert_eq!(row.get::<_, i64>("gross_sat"), 100_000); assert_eq!(row.get::<_, i64>("fee_sat"), 1_000); assert_eq!(row.get::<_, i64>("payout_sat"), 99_000);
	let wrong: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_allocation WHERE freeze_set=$1 AND source LIKE 'lightning-send:%'", &[&frozen.id]).await?.get(0))).await.unwrap();
	assert_eq!(wrong, 0, "collection cannot become a sender refund");
	assert_eq!(server.freeze_current_expiry_backing(&[root.id()], &terminals, budget).await.unwrap().id, frozen.id);
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_carrier", &[]).await?.get::<_, i64>(0))).await.unwrap(), 0);
	assert!(core.get_tx_out(&root.point().txid, root.point().vout, Some(true)).unwrap().is_some(), "freeze has not retired the preserved independent grant claim");
}
