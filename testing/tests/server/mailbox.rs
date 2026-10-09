use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bitcoin::secp256k1::{Keypair, rand::thread_rng};
use bitcoin_ext::AmountExt;
use futures::future::join_all;

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoPolicy, SECP};
use ark::lightning::PaymentHash;
use ark::mailbox::{MailboxAuthorization, MailboxIdentifier};
use ark::test_util::dummy::DummyTestVtxoSpec;
use ark::vtxo::raw::RawVtxo;

use server::database::{Db, MailboxPayload};
use server::database::tree::VtxoTreeUpdate;
use server_rpc::{protos, MAX_NB_MAILBOX_ARKOOR_VTXOS};
use server_rpc::protos::mailbox_server::mailbox_message::Message;

use ark::address::VtxoDelivery;
use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};

use ark_testing::{TestContext, btc, require_bark_version, sat};
use ark_testing::daemon::captaind::MailboxClient;

/// Regression test for the checkpoint visibility gap in concurrent mailbox writes.
///
/// When multiple writers call `PostVtxosMailbox` concurrently, without proper
/// serialization a higher checkpoint can become visible while a lower one is
/// still in flight, causing readers that advance their cursor to permanently
/// skip entries.
///
/// The fix uses `pg_advisory_xact_lock` to serialize all mailbox writes,
/// ensuring checkpoints are allocated and committed in strict order.
///
/// Each individual reader has a low probability of polling in the exact window
/// where a checkpoint is allocated but not yet inserted. Running 100 readers
/// in parallel turns this into 1-(1-p)^100, making the race near-certain to
/// be caught by at least one of them.
#[tokio::test]
async fn mailbox_checkpoint_visibility_gap() {
	let ctx = TestContext::new("server/mailbox_checkpoint_visibility_gap").await;
	let srv = ctx.captaind("server").cfg(|cfg| {
		cfg.postgres.max_connections = 100;
	}).create().await;

	let db = Db::connect(&srv.config().postgres).await.expect("connect to captaind's postgres");

	let mailbox_kp = Keypair::new(&SECP, &mut thread_rng());
	let mailbox_id = MailboxIdentifier::from_pubkey(mailbox_kp.public_key());
	let mailbox_pubkey = srv.ark_info().await.mailbox_pubkey;
	let ark_url = srv.ark_url();

	// Generate 100 unique VTXOs and seed them into the vtxo table (FK constraint).
	let mut anchors = Vec::new();
	let vtxo_pairs: Vec<_> = (0..100).map(|_| {
		let kp = Keypair::new(&SECP, &mut thread_rng());
		let (tx, vtxo) = DummyTestVtxoSpec {
			user_keypair: kp,
			..Default::default()
		}.build();
		anchors.push(tx);
		(kp, vtxo)
	}).collect();

	db.write(async |t| t.upsert_vtxos(
		vtxo_pairs.iter().map(|(_, v)| ServerVtxo::from(v.clone()))
	).await).await.expect("upsert vtxos");
	// A post validates each chain against its stored anchor.
	db.write(async |t| t.execute_vtxo_tree_update(VtxoTreeUpdate::new().upsert_signed_tx(anchors)).await)
		.await.expect("store anchor txs");

	let writers_done = Arc::new(AtomicBool::new(false));
	let expiry = chrono::Local::now() + Duration::from_secs(300);
	let auth_bytes = MailboxAuthorization::new(&mailbox_kp, expiry).serialize().to_vec();
	let unblinded_id = mailbox_id.serialize();

	// -- Readers: 100 tasks polling as fast as possible, advancing cursor --

	let reader_handles: Vec<_> = (0..100).map(|_| {
		let ark_url = ark_url.clone();
		let writers_done = writers_done.clone();
		let auth_bytes = auth_bytes.clone();
		let unblinded_id = unblinded_id.clone();

		tokio::spawn(async move {
			let mut client = MailboxClient::connect(ark_url).await.unwrap();
			let mut cursor: u64 = 0;
			let mut seen: usize = 0;
			let mut final_poll = false;

			loop {
				let resp = client.read_mailbox(protos::mailbox_server::MailboxRequest {
					mailbox_id: unblinded_id.clone(),
					authorization: Some(auth_bytes.clone()),
					checkpoint: cursor,
				}).await.unwrap().into_inner();

				for msg in &resp.messages {
					cursor = msg.checkpoint;
					seen += 1;
				}

				if writers_done.load(Ordering::Acquire) && resp.messages.is_empty() {
					if final_poll {
						break;
					}
					final_poll = true;
				}
			}

			seen
		})
	}).collect();

	// -- Writers: 100 tasks, each posting one VTXO to the same mailbox --

	let writer_handles: Vec<_> = vtxo_pairs.iter().map(|(kp, vtxo)| {
		let ark_url = ark_url.clone();
		let blinded_id = mailbox_id.to_blinded(mailbox_pubkey, kp).unwrap();
		let vtxo_bytes = ProtocolEncoding::serialize(vtxo).to_vec();

		tokio::spawn(async move {
			let mut client = MailboxClient::connect(ark_url).await.unwrap();
			client.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
				blinded_id: blinded_id.as_ref().to_vec(),
				vtxos: vec![vtxo_bytes],
			}).await.unwrap();
		})
	}).collect();

	join_all(writer_handles).await;
	writers_done.store(true, Ordering::Release);

	let reader_results: Vec<usize> = join_all(reader_handles).await
		.into_iter().map(|r| r.unwrap()).collect();

	// Sanity: all 100 writes landed in the database.
	let all = db.read(async |t| t.get_mailbox_entries(mailbox_id, 0, 10_000).await).await.unwrap();
	let total: usize = all.iter().map(|e| match &e.payload {
		MailboxPayload::Arkoor { vtxos } => vtxos.len(),
		_ => 0,
	}).sum();
	assert_eq!(total, 100, "all 100 VTXOs should be in the mailbox");

	// Every reader should have seen all 100 messages. If any reader missed
	// messages, the checkpoint visibility gap caused it to skip entries.
	for (i, seen) in reader_results.iter().enumerate() {
		assert_eq!(
			*seen, 100,
			"reader {i} saw only {seen}/100 messages — \
			 checkpoint visibility gap caused it to skip entries",
		);
	}
}

/// The arkoor post endpoint is unauthenticated (senders aren't the
/// recipient), so the server must only accept vtxos it cosigned itself.
/// Unknown ids and posts whose content claims a pubkey different from the
/// server's records are rejected; without this anyone could grow the
/// mailbox table with arbitrary blobs.
#[tokio::test]
async fn mailbox_post_arkoor_requires_known_vtxos() {
	let ctx = TestContext::new("server/mailbox_post_arkoor_requires_known_vtxos").await;
	let srv = ctx.captaind("server").create().await;

	let db = Db::connect(&srv.config().postgres).await.expect("connect to captaind's postgres");
	let mut rpc = srv.get_mailbox_public_rpc().await;

	let mailbox_kp = Keypair::new(&SECP, &mut thread_rng());
	let mailbox_id = MailboxIdentifier::from_pubkey(mailbox_kp.public_key());
	let mailbox_pubkey = srv.ark_info().await.mailbox_pubkey;

	// Seed one vtxo into the vtxo table, as if the server cosigned it.
	let owner_kp = Keypair::new(&SECP, &mut thread_rng());
	let (anchor_tx, vtxo) = DummyTestVtxoSpec {
		user_keypair: owner_kp,
		..Default::default()
	}.build();
	db.write(async |t| t.upsert_vtxos([ServerVtxo::from(vtxo.clone())]).await).await
		.expect("upsert vtxo");
	// A post validates the chain against its stored anchor.
	db.write(async |t| t.execute_vtxo_tree_update(VtxoTreeUpdate::new().upsert_signed_tx([anchor_tx])).await)
		.await.expect("store anchor tx");

	// A vtxo the server never cosigned is rejected.
	let attacker_kp = Keypair::new(&SECP, &mut thread_rng());
	let (_tx, unknown_vtxo) = DummyTestVtxoSpec {
		user_keypair: attacker_kp,
		..Default::default()
	}.build();
	let err = rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: mailbox_id.to_blinded(mailbox_pubkey, &attacker_kp).unwrap().as_ref().to_vec(),
		vtxos: vec![ProtocolEncoding::serialize(&unknown_vtxo).to_vec()],
	}).await.unwrap_err();
	assert!(err.message().contains("does not exist"),
		"unexpected error for unknown vtxo: {}", err.message(),
	);

	// A known id whose content claims a different pubkey is rejected; it
	// would route the vtxo into a mailbox its owner doesn't watch.
	let mut raw = RawVtxo::deserialize(&ProtocolEncoding::serialize(&vtxo)).unwrap();
	raw.policy = ServerVtxoPolicy::User(VtxoPolicy::new_pubkey(attacker_kp.public_key()));
	let err = rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: mailbox_id.to_blinded(mailbox_pubkey, &attacker_kp).unwrap().as_ref().to_vec(),
		vtxos: vec![raw.serialize()],
	}).await.unwrap_err();
	assert!(err.message().contains("doesn't belong to the provided vtxo pubkey"),
		"unexpected error for pubkey mismatch: {}", err.message(),
	);

	// Nothing landed in the mailbox.
	let entries = db.read(async |t| t.get_mailbox_entries(mailbox_id, 0, 100).await).await.unwrap();
	assert!(entries.is_empty(), "rejected posts should not create mailbox entries");

	// The genuine vtxo still goes through.
	rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: mailbox_id.to_blinded(mailbox_pubkey, &owner_kp).unwrap().as_ref().to_vec(),
		vtxos: vec![ProtocolEncoding::serialize(&vtxo).to_vec()],
	}).await.expect("post of a known vtxo should succeed");

	let entries = db.read(async |t| t.get_mailbox_entries(mailbox_id, 0, 100).await).await.unwrap();
	assert_eq!(entries.len(), 1, "the genuine vtxo should be delivered");
}

/// The number of vtxos per arkoor post is capped so a single request can't
/// carry an arbitrary amount of decode and database work. The cap is checked
/// before any vtxo is deserialized.
#[tokio::test]
async fn mailbox_post_arkoor_caps_vtxos_per_request() {
	let ctx = TestContext::new("server/mailbox_post_arkoor_caps_vtxos_per_request").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_mailbox_public_rpc().await;

	let mailbox_kp = Keypair::new(&SECP, &mut thread_rng());
	let mailbox_id = MailboxIdentifier::from_pubkey(mailbox_kp.public_key());
	let mailbox_pubkey = srv.ark_info().await.mailbox_pubkey;

	let err = rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: mailbox_id.to_blinded(mailbox_pubkey, &mailbox_kp).unwrap().as_ref().to_vec(),
		vtxos: vec![vec![0u8]; MAX_NB_MAILBOX_ARKOOR_VTXOS + 1],
	}).await.unwrap_err();
	assert!(err.message().contains("too many vtxos"),
		"unexpected error for over-cap post: {}", err.message(),
	);
}

/// Test that an incoming lightning payment posts an IncomingLightningPayment
/// notification to the receiver's mailbox with the payment hash.
#[tokio::test]
async fn mailbox_lightning_receive_pending() {
	require_bark_version!(> "0.5.0");

	let ctx = TestContext::new("server/mailbox_lightning_receive_pending").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;

	// Server must be linked to the receiver CLN to generate hold invoices
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;

	let bark = Arc::new(ctx.bark("bark", &srv).funded(btc(3)).create().await);
	bark.board_and_confirm_and_register(&ctx, btc(2)).await;

	let mut mb_rpc = srv.get_mailbox_public_rpc().await;
	let bark_wallet = bark.client().await;

	// Create an invoice and have the external sender pay it
	let pay_amount = btc(1);
	let invoice_info = bark.bolt11_invoice(pay_amount).await;

	let cloned_invoice = invoice_info.invoice.clone();
	let pay_handle = tokio::spawn(async move {
		lightning.external.pay_bolt11(cloned_invoice).await
	});

	// Read the receiver's main mailbox, retrying until the notification arrives.
	let mailbox_kp = bark_wallet.mailbox_keypair();
	let mailbox_id = MailboxIdentifier::from_pubkey(mailbox_kp.public_key());

	let incoming = tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let expiry = chrono::Local::now() + Duration::from_secs(60);
			let mailbox_auth = MailboxAuthorization::new(&mailbox_kp, expiry);

			let read_req = protos::mailbox_server::MailboxRequest {
				authorization: Some(mailbox_auth.serialize().to_vec()),
				mailbox_id: mailbox_id.serialize(),
				checkpoint: 0,
			};

			let mailbox_msgs = mb_rpc.read_mailbox(read_req).await.unwrap().into_inner();

			let found = mailbox_msgs.messages.iter().find_map(|msg| {
				match msg.message.as_ref()? {
					Message::IncomingLightningPayment(m) => Some(m.clone()),
					_ => None,
				}
			});

			if let Some(msg) = found {
				break msg;
			}
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	}).await.expect("IncomingLightningPayment notification should arrive within 15s");

	// Verify the payment hash is valid
	PaymentHash::try_from(incoming.payment_hash.clone())
		.expect("valid payment hash");
	assert_eq!(incoming.amount_msat, pay_amount.to_msat());

	// We don't need to claim or await the payment — we only care that
	// the notification arrived. Drop the handle to avoid a panic from
	// the sender timing out on the unsettled hold invoice.
	drop(pay_handle);
}

/// Test that a successful lightning send posts a LightningSendFinished
/// notification to the sender's mailbox with the preimage.
#[tokio::test]
async fn mailbox_lightning_send_finished() {
	require_bark_version!(> "0.5.0");

	let ctx = TestContext::new("server/mailbox_lightning_send_finished").await;

	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	srv.wait_for_vtxopool(&ctx).await;

	let bark = ctx.bark("bark", &srv).funded(btc(3)).create().await;
	bark.board_and_confirm_and_register(&ctx, btc(2)).await;

	lightning.sync().await;

	let mut mb_rpc = srv.get_mailbox_public_rpc().await;
	let bark_wallet = bark.client().await;

	// Pay a lightning invoice
	let invoice = lightning.external.invoice(Some(btc(1)), "test_payment", "test").await;
	bark.pay_lightning_wait(invoice, None).await;

	// Read the sender's main mailbox, retrying until the notification arrives.
	// The send-finished notification is posted asynchronously after the payment
	// status is written to the DB, so there is a small race window.
	let mailbox_kp = bark_wallet.mailbox_keypair();
	let mailbox_id = MailboxIdentifier::from_pubkey(mailbox_kp.public_key());

	let send_finished = tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let expiry = chrono::Local::now() + Duration::from_secs(60);
			let mailbox_auth = MailboxAuthorization::new(&mailbox_kp, expiry);

			let read_req = protos::mailbox_server::MailboxRequest {
				authorization: Some(mailbox_auth.serialize().to_vec()),
				mailbox_id: mailbox_id.serialize(),
				checkpoint: 0,
			};

			let mailbox_msgs = mb_rpc.read_mailbox(read_req).await.unwrap().into_inner();

			let found = mailbox_msgs.messages.iter().find_map(|msg| {
				match msg.message.as_ref()? {
					Message::LightningSendFinished(m) => Some(m.clone()),
					_ => None,
				}
			});

			if let Some(msg) = found {
				break msg;
			}
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	}).await.expect("LightningSendFinished notification should arrive within 10s");

	// Verify the payment hash is valid
	PaymentHash::try_from(send_finished.payment_hash.clone())
		.expect("valid payment hash");

	// On success the preimage must be present
	assert!(send_finished.preimage.is_some(), "preimage should be present on successful payment");
}

/// A post registers the signed chain it delivers: the recipient then holds
/// it, so the vtxos are the recipient's. An unsigned chain is refused.
#[tokio::test]
async fn mailbox_post_arkoor_registers_and_rejects_unsigned() {
	let ctx = TestContext::new("server/mailbox_post_arkoor_registers_and_rejects_unsigned").await;
	let srv = ctx.captaind("server").no_vtxo_pool().create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(100_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	let blinded_id = address.delivery().iter().find_map(|d| match d {
		VtxoDelivery::ServerMailbox { blinded_id } => Some(blinded_id.as_ref().to_vec()),
		_ => None,
	}).unwrap();

	// A real cosign whose registration never happens.
	let mut inputs = Vec::new();
	let mut keys = Vec::new();
	for coin in sender.spendable_vtxos().await.unwrap() {
		inputs.push(sender.get_full_vtxo(coin.id()).await.unwrap());
	}
	for input in &inputs {
		keys.push(sender.pubkey_keypair(&input.user_pubkey()).await.unwrap().unwrap().1);
	}
	let builder = ArkoorPackageBuilder::new_with_checkpoints(inputs, vec![ArkoorDestination {
		total_amount: sat(100_000), policy: address.policy().clone(),
	}]).unwrap().generate_user_nonces(&keys).unwrap();
	let response = srv.get_public_rpc().await.request_arkoor_cosign(
		protos::ArkoorPackageCosignRequest::from(builder.cosign_request()),
	).await.unwrap().into_inner();
	let outputs = builder.user_cosign(&keys, ArkoorPackageCosignResponse::try_from(response).unwrap())
		.unwrap().build_signed_vtxos();
	let ids = outputs.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let states = || async {
		db.read(async |t| Ok(t.query("SELECT spend_state::TEXT FROM vtxo WHERE vtxo_id=ANY($1)", &[&ids])
			.await?)).await.unwrap().iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>()
	};
	let posts = || async {
		db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM mailbox WHERE mailbox_type='arkoor-receive' AND vtxo_id=ANY($1)", &[&ids],
		).await?.get::<_, i64>(0))).await.unwrap()
	};
	assert!(states().await.iter().all(|s| s == "unregistered"));

	let mut rpc = srv.get_mailbox_public_rpc().await;
	let mut unsigned = outputs.clone();
	unsigned[0].invalidate_final_sig();
	let err = rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: blinded_id.clone(), vtxos: unsigned.iter().map(|v| v.serialize()).collect(),
	}).await.unwrap_err();
	assert_eq!(err.code(), tonic::Code::InvalidArgument, "{err}");
	assert!(states().await.iter().all(|s| s == "unregistered"), "a refused post registers nothing");
	assert_eq!(posts().await, 0, "a refused post stores nothing");

	rpc.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id, vtxos: outputs.iter().map(|v| v.serialize()).collect(),
	}).await.expect("the signed chain is accepted");
	assert!(states().await.iter().all(|s| s == "spendable"), "the post registered the chain");
	assert_eq!(posts().await as usize, outputs.len());
}
