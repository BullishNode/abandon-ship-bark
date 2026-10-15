//! Native Track C qualification: public per-key authorization and real money paths.

use ark::{ProtocolEncoding, SECP};
use ark::fallback::{FallbackRecord, KeyLink};
use ark_testing::{TestContext, btc, sat};
use bitcoin::secp256k1::{rand, Keypair};
use bitcoin::{Amount, Network, ScriptBuf};
use server::database::Db;
use server_rpc::protos;

#[tokio::test]
async fn per_key_registration_is_atomic_and_canonical() {
	let ctx = TestContext::new("per-key/registration_is_atomic_and_canonical").await;
	let srv = ctx.captaind("server").create().await;
	let mut rpc = srv.get_public_rpc().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mailbox = Keypair::new(&SECP, &mut rand::thread_rng());
	let coin = Keypair::new(&SECP, &mut rand::thread_rng());
	let script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let record = FallbackRecord::new(Network::Regtest, script.clone(), &mailbox).unwrap();
	let link = KeyLink::new(Network::Regtest, mailbox.public_key(), &record, &coin, None).unwrap();
	let request = |record: &FallbackRecord, links: &[KeyLink]| protos::SetFallbackRequest {
		mailbox_pk: mailbox.public_key().serialize().to_vec(), record: record.serialize(),
		key_links: links.iter().map(ProtocolEncoding::serialize).collect(),
	};
	let first = rpc.set_fallback(request(&record, &[link.clone()])).await.unwrap().into_inner();
	assert_eq!(first.record, record.serialize());
	assert_eq!(first.key_links, vec![link.serialize()]);
	let resigned_record = FallbackRecord::new(Network::Regtest, script, &mailbox).unwrap();
	let resigned_link = KeyLink::new(Network::Regtest, mailbox.public_key(), &resigned_record, &coin, None).unwrap();
	let retry = rpc.set_fallback(request(&resigned_record, &[resigned_link])).await.unwrap().into_inner();
	assert_eq!(retry, first, "randomized retries adopt the exact durable canonical acknowledgment");
	let other = Keypair::new(&SECP, &mut rand::thread_rng());
	let valid = KeyLink::new(Network::Regtest, mailbox.public_key(), &record, &other, None).unwrap();
	let mut invalid = link.clone(); invalid.recovery_index = Some(7);
	assert!(rpc.set_fallback(request(&record, &[valid, invalid])).await.is_err());
	let rows: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM key_link", &[]).await?.get(0))).await.unwrap();
	assert_eq!(rows, 1, "a batch with an invalid proof inserts no earlier valid member");
	let replacement = FallbackRecord::new(Network::Regtest,
		ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap(), &mailbox).unwrap();
	assert!(rpc.set_fallback(request(&replacement, &[])).await.is_err());
	assert_eq!(rpc.set_fallback(request(&record, &[])).await.unwrap().into_inner().record, first.record);
}

#[tokio::test]
async fn per_key_board_arkoor_preserves_owner_proofs_and_value() {
	let ctx = TestContext::new("per-key/board_arkoor_preserves_owner_proofs_and_value").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(100_000)).create().await;
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	sender.stop_daemon_wait().await.unwrap();
	receiver.stop_daemon_wait().await.unwrap();
	let initial: Amount = sender.spendable_vtxos().await.unwrap().iter().map(|v| v.amount()).sum();
	let address = receiver.new_address().await.unwrap();
	let key = address.policy().user_pubkey();
	let retained = receiver.fallback_key_link(key).await.unwrap();
	let record = receiver.fallback_destination().await.unwrap();
	retained.verify(Network::Regtest, receiver.mailbox_keypair().public_key(), &record).unwrap();
	assert!(address.delivery().iter().all(|d| matches!(d, ark::address::VtxoDelivery::ServerMailbox { .. })),
		"receiving addresses only advertise mailbox routing");
	sender.send_arkoor_payment(&address, sat(40_000)).await.unwrap();
	receiver.sync_mailbox().await.unwrap();
	let received = receiver.spendable_vtxos().await.unwrap();
	assert_eq!(received.iter().map(|v| v.amount()).sum::<Amount>(), sat(40_000));
	assert_eq!(sender.spendable_vtxos().await.unwrap().iter().map(|v| v.amount()).sum::<Amount>() + sat(40_000), initial);
	for coin in received {
		assert_eq!(coin.user_pubkey(), key);
		let proof = srv.get_public_rpc().await.get_vtxo(protos::GetVtxoRequest {
			vtxo_id: coin.id().to_bytes().to_vec(),
		}).await.unwrap().into_inner();
		assert_eq!(proof.fallback_record, record.serialize());
		assert_eq!(proof.key_link, retained.serialize(), "public source serves the original pre-use canonical acknowledgment");
	}
}

/// A new C route check: changing only a valid payment's mailbox must not
/// register that payment as a side effect of refusing the delivery.
#[tokio::test]
async fn per_key_wrong_mailbox_refuses_before_signed_chain_registration() {
	let ctx = TestContext::new("per-key/wrong_mailbox_refuses_before_signed_chain_registration").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(100_000)).create().await;
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	sender.stop_daemon_wait().await.unwrap();
	receiver.stop_daemon_wait().await.unwrap();
	let input_id = sender.spendable_vtxos().await.unwrap()[0].id();
	let input = sender.get_full_vtxo(input_id).await.unwrap();
	let principal = input.amount();
	let input_keys = [sender.get_vtxo_key(&input).await.unwrap()];
	let mut rpc = srv.get_public_rpc().await;
	rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
		vtxos: vec![input.serialize()],
	}).await.unwrap();
	let (address, index) = receiver.new_address_with_index().await.unwrap();
	let receiver_key = receiver.peek_keypair(index).await.unwrap();
	let retained = receiver.fallback_key_link(receiver_key.public_key()).await.unwrap();
	let builder = ark::arkoor::package::ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		vec![input], address.policy().clone(),
	).unwrap().generate_user_nonces(&input_keys).unwrap();
	let response = rpc.request_arkoor_cosign(protos::ArkoorPackageCosignRequest::from(builder.cosign_request()))
		.await.unwrap().into_inner();
	let response = ark::arkoor::package::ArkoorPackageCosignResponse::try_from(response).unwrap();
	let signed = builder.user_cosign(&input_keys, response).unwrap().build_signed_vtxos();
	assert!(!signed.is_empty());
	let ids = signed.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let before = db.read(async |t| Ok(t.query("SELECT vtxo_id,spend_state::text AS state,vtxo FROM vtxo
		WHERE vtxo_id=ANY($1) ORDER BY vtxo_id", &[&ids]).await?.into_iter()
		.map(|row| (row.get::<_, String>("vtxo_id"), row.get::<_, String>("state"), row.get::<_, Vec<u8>>("vtxo")))
		.collect::<Vec<_>>())).await.unwrap();
	assert_eq!(before.len(), signed.len());
	assert!(!before.is_empty());
	assert!(before.iter().all(|(_, state, _)| state == "unregistered"));
	let info = rpc.get_ark_info(protos::Empty {}).await.unwrap().into_inner();
	let server_mailbox = bitcoin::secp256k1::PublicKey::from_slice(&info.mailbox_pubkey).unwrap();
	let wrong = Keypair::new(&SECP, &mut rand::thread_rng());
	let blinded = ark::mailbox::MailboxIdentifier::from_pubkey(wrong.public_key())
		.to_blinded(server_mailbox, &receiver_key).unwrap();
	let mut request = tonic::Request::new(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: blinded.serialize(), vtxos: signed.iter().map(ProtocolEncoding::serialize).collect(),
	});
	server_rpc::RequestExt::set_pver(&mut request, 6);
	assert!(srv.get_mailbox_public_rpc().await.post_arkoor_message(request).await.is_err());
	let after = db.read(async |t| Ok(t.query("SELECT vtxo_id,spend_state::text AS state,vtxo FROM vtxo
		WHERE vtxo_id=ANY($1) ORDER BY vtxo_id", &[&ids]).await?.into_iter()
		.map(|row| (row.get::<_, String>("vtxo_id"), row.get::<_, String>("state"), row.get::<_, Vec<u8>>("vtxo")))
		.collect::<Vec<_>>())).await.unwrap();
	assert_eq!(before, after, "a refused route must not promote or rewrite the signed source");
	let delivered: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM mailbox WHERE vtxo_id=ANY($1)", &[&ids])
		.await?.get(0))).await.unwrap();
	assert_eq!(delivered, 0);
	assert_eq!(receiver.fallback_key_link(receiver_key.public_key()).await.unwrap(), retained);
	let blinded = receiver.mailbox_identifier().to_blinded(server_mailbox, &receiver_key).unwrap();
	let mut request = tonic::Request::new(protos::mailbox_server::PostArkoorMessageRequest {
		blinded_id: blinded.serialize(), vtxos: signed.iter().map(ProtocolEncoding::serialize).collect(),
	});
	server_rpc::RequestExt::set_pver(&mut request, 6);
	srv.get_mailbox_public_rpc().await.post_arkoor_message(request).await.unwrap();
	receiver.sync_mailbox().await.unwrap();
	assert_eq!(receiver.spendable_vtxos().await.unwrap().iter().map(|v| v.amount()).sum::<Amount>(), principal);
}

/// The distinct-key window and reusable-key capacity are different quantities.
/// All fifty keys are advertised before absence; ten distinct keys receive
/// twelve genuine payments, including keys 25/50 and three uses of key 1.
#[tokio::test]
async fn per_key_fifty_offline_keys_receive_25_50_and_eight_reuse() {
	let ctx = TestContext::new("per-key/fifty_offline_keys_receive_25_50_and_eight_reuse").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let recipient = ctx.bark_sdk("recipient", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let record = recipient.fallback_destination().await.unwrap();
	let mailbox = recipient.mailbox_keypair().public_key();
	let mut config = recipient.config().clone();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let key_count: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM key_link WHERE mailbox_pk=$1",
		&[&mailbox.serialize().to_vec()]).await?.get(0))).await.unwrap();
	assert_eq!(key_count, 51, "identity plus all fifty receiving proofs are durable before absence");
	srv.stop().await.unwrap();
	let mut addresses = Vec::new();
	let mut retained = Vec::new();
	for expected_index in 1..=50 {
		let (address, index) = recipient.new_address_with_index().await.unwrap();
		assert_eq!(index, expected_index);
		let link = recipient.fallback_key_link(address.policy().user_pubkey()).await.unwrap();
		link.verify(Network::Regtest, mailbox, &record).unwrap();
		assert_eq!(link.recovery_index, Some(index));
		addresses.push(address);
		retained.push(link);
		if expected_index == 25 {
			assert_eq!(addresses.iter().map(|a| a.policy().user_pubkey()).collect::<std::collections::BTreeSet<_>>().len(), 25);
		}
	}
	assert_eq!(addresses.iter().map(|a| a.policy().user_pubkey()).collect::<std::collections::BTreeSet<_>>().len(), 50);
	let refusal = tokio::time::timeout(std::time::Duration::from_secs(15), recipient.new_address()).await
		.expect("unavailable refill must return a clean error within fifteen seconds").unwrap_err();
	assert!(format!("{refusal:#}").contains("no linked keys available"));
	assert!(recipient.peek_keypair(51).await.is_err(), "an unsupported new key is not persisted or exposed");
	assert_eq!(recipient.peek_address(1).await.unwrap(), addresses[0], "existing linked addresses remain reusable offline");
	assert_eq!(recipient.fallback_destination().await.unwrap(), record);
	assert!(recipient.pending_boards().await.unwrap().is_empty());
	assert!(recipient.pending_lightning_sends().await.unwrap().is_empty());
	drop(recipient);
	srv.start().await.unwrap();
	config.server_address = srv.ark_url();
	// Payer receives only these public addresses. It has its own seed and DB;
	// no recipient wallet is open during these genuine payments.
	let targets = (0..8).chain([24, 49, 0, 0]).collect::<Vec<_>>();
	assert_eq!(targets.iter().copied().collect::<std::collections::BTreeSet<_>>().len(), 10);
	let mut payer = ctx.bark_sdk("payer", &srv).cfg(|c| c.daemon_manual_sync = true);
	for _ in &targets { payer = payer.boarded(sat(20_000)); }
	let payer = payer.create().await;
	payer.stop_daemon_wait().await.unwrap();
	for target in &targets { payer.send_arkoor_payment(&addresses[*target], sat(20_000)).await.unwrap(); }
	drop(payer);
	// Every acknowledged proof still has its original owner and exact signature.
	for link in &retained {
		let row = db.read(async |t| Ok(t.query_one("SELECT mailbox_pk,sig,recovery_index FROM key_link WHERE user_pubkey=$1",
			&[&link.user_pubkey.serialize().to_vec()]).await?)).await.unwrap();
		assert_eq!(row.get::<_, Vec<u8>>("mailbox_pk"), mailbox.serialize().to_vec());
		assert_eq!(row.get::<_, Vec<u8>>("sig"), link.signature.serialize().to_vec());
		assert_eq!(row.get::<_, Option<i32>>("recovery_index"), link.recovery_index.map(|i| i as i32));
	}
	let recipient = bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("recipient")), run_daemon: false, ..Default::default() },
	).await.unwrap();
	recipient.sync_mailbox().await.unwrap();
	let received = recipient.spendable_vtxos().await.unwrap();
	assert_eq!(received.len(), 12);
	assert_eq!(received.iter().map(|v| v.amount()).sum::<Amount>(), sat(240_000));
	assert_eq!(received.iter().map(|v| v.user_pubkey()).collect::<std::collections::BTreeSet<_>>().len(), 10);
	for target in [24, 49] {
		assert_eq!(received.iter().filter(|v| v.user_pubkey() == addresses[target].policy().user_pubkey()).count(), 1);
	}
	for target in 0..8 {
		let expected = if target == 0 { 3 } else { 1 };
		assert_eq!(received.iter().filter(|v| v.user_pubkey() == addresses[target].policy().user_pubkey()).count(), expected);
	}
	assert_eq!(recipient.fallback_destination().await.unwrap(), record);
	let (next, index) = recipient.new_address_with_index().await.unwrap();
	assert_eq!(index, 51, "online return refills after the failed offline refill, without reissuing old keys");
	assert!(addresses.iter().all(|address| address.policy().user_pubkey() != next.policy().user_pubkey()));
	let link = recipient.fallback_key_link(next.policy().user_pubkey()).await.unwrap();
	link.verify(Network::Regtest, mailbox, &record).unwrap();
}

#[tokio::test]
async fn per_key_two_devices_converge_without_duplicate_principal() {
	let ctx = TestContext::new("per-key/two_devices_converge_without_duplicate_principal").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let first = ctx.bark_sdk("first", &srv).mnemonic(mnemonic.clone()).cfg(|c| c.daemon_manual_sync = true);
	let second = ctx.bark_sdk("second", &srv).mnemonic(mnemonic).cfg(|c| c.daemon_manual_sync = true);
	let (first, second) = tokio::join!(first.create(), second.create());
	first.stop_daemon_wait().await.unwrap();
	second.stop_daemon_wait().await.unwrap();
	let record = first.fallback_destination().await.unwrap();
	assert_eq!(record, second.fallback_destination().await.unwrap());
	assert_eq!(first.mailbox_keypair().public_key(), second.mailbox_keypair().public_key());
	for index in 0..=50 {
		let a = first.peek_keypair(index).await.unwrap().public_key();
		let b = second.peek_keypair(index).await.unwrap().public_key();
		assert_eq!(a, b);
		assert_eq!(first.fallback_key_link(a).await.unwrap(), second.fallback_key_link(b).await.unwrap());
	}
	let (a, b) = tokio::join!(first.new_address_with_index(), second.new_address_with_index());
	let (a, first_index) = a.unwrap();
	let (b, second_index) = b.unwrap();
	assert_eq!((first_index, second_index), (1, 1));
	assert_eq!(a, b, "separate devices may reuse a genuinely linked receiving key");
	let canonical = first.fallback_key_link(a.policy().user_pubkey()).await.unwrap();
	let payer = ctx.bark_sdk("payer", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(20_000)).boarded(sat(20_000)).create().await;
	payer.stop_daemon_wait().await.unwrap();
	payer.send_arkoor_payment(&a, sat(20_000)).await.unwrap();
	payer.send_arkoor_payment(&b, sat(20_000)).await.unwrap();
	let (one, two) = tokio::join!(first.sync_mailbox(), second.sync_mailbox());
	one.unwrap(); two.unwrap();
	let one = first.spendable_vtxos().await.unwrap();
	let two = second.spendable_vtxos().await.unwrap();
	let one_ids = one.iter().map(|v| v.id()).collect::<std::collections::BTreeSet<_>>();
	let two_ids = two.iter().map(|v| v.id()).collect::<std::collections::BTreeSet<_>>();
	assert_eq!(one_ids.len(), 2);
	assert_eq!(one_ids, two_ids, "two views describe the same two entitlements");
	assert_eq!(one.iter().map(|v| v.amount()).sum::<Amount>(), sat(40_000));
	assert_eq!(two.iter().map(|v| v.amount()).sum::<Amount>(), sat(40_000));
	let ids = one_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let principal: i64 = db.read(async |t| Ok(t.query_one("SELECT SUM(amount)::bigint FROM vtxo WHERE vtxo_id=ANY($1)",
		&[&ids]).await?.get(0))).await.unwrap();
	assert_eq!(principal, 40_000, "duplicate device views never create another economic obligation");
	let links: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM key_link WHERE mailbox_pk=$1",
		&[&first.mailbox_keypair().public_key().serialize().to_vec()]).await?.get(0))).await.unwrap();
	assert_eq!(links, 51, "concurrent initialization retains one canonical proof per key");
	assert_eq!(first.fallback_destination().await.unwrap(), record);
	assert_eq!(second.fallback_destination().await.unwrap(), record);
	assert_eq!(first.fallback_key_link(a.policy().user_pubkey()).await.unwrap(), canonical);
}

/// Fresh Ark-wallet recovery is separate from mnemonic-only on-chain payout
/// discovery. Key 50 is inside the normal VTXO gap; its signed hint is not
/// claimed to be necessary for discovery in this case.
#[tokio::test]
async fn per_key_fresh_seed_key50_preserves_entitlements_and_original_proof() {
	let ctx = TestContext::new("per-key/fresh_seed_key50_preserves_entitlements_and_original_proof").await;
	let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1))
		.cfg(|c| c.fees.board.min_fee = sat(330)).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let original = ctx.bark_sdk("original", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	original.stop_daemon_wait().await.unwrap();
	let record = original.fallback_destination().await.unwrap();
	let mailbox = original.mailbox_identifier();
	let config = original.config().clone();
	let mut address = None;
	for index in 1..=50 {
		let (next, actual_index) = original.new_address_with_index().await.unwrap();
		assert_eq!(actual_index, index);
		address = Some(next);
	}
	let address = address.unwrap();
	let key = address.policy().user_pubkey();
	let canonical = original.fallback_key_link(key).await.unwrap();
	assert_eq!(canonical.recovery_index, Some(50));
	drop(original);
	let payer = ctx.bark_sdk("payer", &srv).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(30_000)).boarded(sat(30_000)).create().await;
	payer.stop_daemon_wait().await.unwrap();
	assert_eq!(payer.estimate_board_offchain_fee(sat(30_000)).await.unwrap().fee, sat(330));
	let payer_before = payer.spendable_vtxos().await.unwrap();
	assert_eq!(payer_before.len(), 2);
	assert!(payer_before.iter().all(|coin| coin.amount() == sat(29_670)));
	let payer_principal = payer_before.iter().map(|coin| coin.amount()).sum::<Amount>();
	assert_eq!(payer_principal, sat(60_000 - 2 * 330));
	payer.send_arkoor_payment(&address, sat(25_000)).await.unwrap();
	payer.send_arkoor_payment(&address, sat(25_000)).await.unwrap();
	let payer_after = payer.spendable_vtxos().await.unwrap();
	let payer_change = payer_after.iter().map(|coin| coin.amount()).sum::<Amount>();
	assert_eq!(payer_change, sat(9_340));
	assert!(payer_after.iter().all(|coin| coin.user_pubkey() != key));
	let payer_change_ids = payer_after.iter().map(|coin| coin.id()).collect::<std::collections::BTreeSet<_>>();
	drop(payer);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let expected_ids = db.read(async |t| Ok(t.query("SELECT DISTINCT vtxo_id FROM mailbox
		WHERE unblinded_mailbox_id=$1 AND vtxo_id IS NOT NULL ORDER BY vtxo_id", &[&mailbox.to_string()]).await?
		.into_iter().map(|row| row.get::<_, String>(0).parse::<ark::VtxoId>().unwrap())
		.collect::<std::collections::BTreeSet<_>>())).await.unwrap();
	assert_eq!(expected_ids.len(), 2);
	assert!(expected_ids.is_disjoint(&payer_change_ids));
	assert_eq!(payer_change + sat(50_000), payer_principal,
		"two exact recipient entitlements plus payer change conserve net Board principal");
	let mut rpc = srv.get_public_rpc().await;
	for id in &expected_ids {
		let public = rpc.get_vtxo(protos::GetVtxoRequest { vtxo_id: id.to_bytes().to_vec() }).await.unwrap().into_inner();
		assert_eq!(public.child_index, Some(50));
		assert_eq!(public.fallback_record, record.serialize());
		assert_eq!(public.key_link, canonical.serialize());
		let coin = ark::Vtxo::<ark::vtxo::Full>::deserialize(&public.vtxo).unwrap();
		assert_eq!(coin.id(), *id);
		assert_eq!(coin.user_pubkey(), key);
		assert_eq!(coin.amount(), sat(25_000));
	}
	let fresh_datadir = ctx.datadir.join("fresh-seed");
	assert!(!fresh_datadir.exists(), "recovery starts from a genuinely new dataset");
	// Only the mnemonic/config enter this open. No original DB, coin IDs or
	// public response bytes are passed to the recovering wallet.
	let restored = bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(fresh_datadir), create_if_not_exists: true,
			run_daemon: false, ..Default::default() },
	).await.unwrap();
	let coins = restored.spendable_vtxos().await.unwrap();
	assert_eq!(coins.iter().map(|v| v.id()).collect::<std::collections::BTreeSet<_>>(), expected_ids);
	assert_eq!(coins.len(), 2);
	assert_eq!(coins.iter().map(|v| v.amount()).sum::<Amount>(), sat(50_000));
	assert!(coins.iter().all(|v| v.user_pubkey() == key));
	assert_eq!(restored.fallback_destination().await.unwrap(), record);
	assert_eq!(restored.fallback_key_link(key).await.unwrap(), canonical);
	assert_eq!(restored.peek_next_keypair().await.unwrap().1, 51,
		"a genuinely recovered key50 is issued, even when fresh prelinking already made its public mapping known");
	// The automatic pool chooses the lowest linked UNUSED key, independently
	// of peek_next_keypair's max-issued frontier. Recovery marks only actual
	// key50 used, preserving unused1..49 and skipping50 during allocation.
	for expected in 1..=49 {
		let (_, index) = restored.new_address_with_index().await.unwrap();
		assert_eq!(index, expected);
	}
	let (_, index) = restored.new_address_with_index().await.unwrap();
	assert_eq!(index, 51, "automatic allocation must never reissue recovered key50");
	assert_eq!(restored.peek_address(50).await.unwrap(), address,
		"explicit reuse of an original immutable receiving address remains valid");
	assert_eq!(restored.fallback_key_link(key).await.unwrap(), canonical);
	let unchanged = restored.spendable_vtxos().await.unwrap();
	assert_eq!(unchanged.iter().map(|v| v.id()).collect::<std::collections::BTreeSet<_>>(), expected_ids);
	assert_eq!(unchanged.iter().map(|v| v.amount()).sum::<Amount>(), sat(50_000));
	// Separately exercise native GetVtxo signed-hint verification. This is
	// an integrity follow-up, not evidence of seed-only discovery using IDs.
	let report = restored.recover_vtxos(expected_ids.iter().copied(), None).await.unwrap();
	assert!(report.is_complete());
	assert_eq!(restored.peek_next_keypair().await.unwrap().1, 52, "idempotent recovery never unissues the newly allocated51");
	assert_eq!(restored.spendable_vtxos().await.unwrap().iter().map(|v| v.id()).collect::<std::collections::BTreeSet<_>>(), expected_ids);
}
