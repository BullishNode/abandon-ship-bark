//! Actual scheduled source freezing, without manually calling the freeze API.

use std::time::Duration;

use ark::{ProtocolEncoding, VtxoPolicy};
use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use bitcoin_ext::BlockDelta;
use bitcoincore_rpc::RpcApi;
use server::database::Db;
use server_rpc::protos;

use ark_testing::{btc, sat, TestContext};

#[tokio::test]
async fn automatic_expiry_absent_registered_board_freezes_without_client_or_manual_call() {
	let ctx = TestContext::new("expiry_automatic/automatic_expiry_absent_registered_board_freezes_without_client_or_manual_call").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(128);
		c.fees.board.base_fee = sat(1_000);
		c.expiry_payout.grace_blocks = 0;
	}).create().await;
	let wallet = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(21_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coin = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let fallback = wallet.fallback_destination().await.unwrap().spk;
	drop(wallet);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	// The same operator config request is accepted by serde on the parent;
	// its missing scheduled boundary ignores this new section and never freezes.
	let mut config = serde_json::to_value(srv.config().clone()).unwrap();
	config["direct_expiry"] = serde_json::json!({"enabled":true,"interval":"100ms","grace_blocks":0,"max_groups_per_tick":1});
	let config: server::config::Config = serde_json::from_value(config).unwrap();
	srv.stop().await.unwrap();
	let mut competing = config.clone(); competing.expiry_payout.enabled = true;
	let conflict = server::Server::start(competing).await.err().expect("concurrent legacy route must be refused before workers");
	assert!(format!("{conflict:#}").contains("cannot run concurrently"));
	let server = server::Server::start(config).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		while server.chain_tip().height.to_u32() != core.get_block_count().unwrap() as u32 {
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	let row = tokio::time::timeout(Duration::from_secs(5), async {
		loop {
			if let Some(row) = db.read(async |t| t.query_opt("SELECT freeze_set,source,disposition,spk,gross_sat,fee_sat,payout_sat FROM expiry_allocation WHERE id=$1",
				&[&coin.id().to_string()]).await.map_err(Into::into)).await.unwrap() { break row; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("the configured scheduled source task must freeze an actual expired absent Board");
	assert_eq!(row.get::<_, &str>("source"), "registered");
	assert_eq!(row.get::<_, &str>("disposition"), "pay");
	assert_eq!(row.get::<_, Vec<u8>>("spk"), fallback.as_bytes());
	assert_eq!(row.get::<_, i64>("gross_sat"), 20_000);
	assert_eq!(row.get::<_, i64>("fee_sat"), 1_000);
	assert_eq!(row.get::<_, i64>("payout_sat"), 19_000);
	let frozen: String = row.get("freeze_set");
	tokio::time::sleep(Duration::from_millis(500)).await;
	let again: String = db.read(async |t| Ok(t.query_one("SELECT freeze_set FROM expiry_allocation WHERE id=$1", &[&coin.id().to_string()]).await?.get(0))).await.unwrap();
	assert_eq!(again, frozen, "later scheduled ticks must preserve the canonical allocation");
	let carriers: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_carrier", &[]).await?.get(0))).await.unwrap();
	assert_eq!(carriers, 0, "scheduled source freezing is pending allocation, not payment or signing");
}

async fn allocation(db: &Db, id: ark::VtxoId) -> Option<tokio_postgres::Row> {
	db.read(async |t| t.query_opt("SELECT freeze_set,source,disposition,spk,gross_sat,fee_sat,payout_sat FROM expiry_allocation WHERE id=$1",
		&[&id.to_string()]).await.map_err(Into::into)).await.unwrap()
}

async fn await_allocation(db: &Db, id: ark::VtxoId) -> tokio_postgres::Row {
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			if let Some(row) = allocation(db, id).await { break row; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual scheduled discovery must progress after complete prerequisites")
}

#[tokio::test]
async fn automatic_expiry_real_pending_board_preserves_pending_source() {
	let ctx = TestContext::new("expiry_automatic/automatic_expiry_real_pending_board_preserves_pending_source").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(128); c.fees.board.base_fee = sat(1_000);
	}).create().await;
	let wallet = ctx.bark_sdk("pending", &srv).cfg(|c| c.daemon_manual_sync = true).funded(btc(1)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let board = wallet.board_amount(sat(31_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	let coin = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
	let fallback = wallet.fallback_destination().await.unwrap().spk;
	drop(wallet); // Never sync_pending_boards: the actual source remains pending.
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert!(db.read(async |t| t.query_opt("SELECT vtxo_id FROM vtxo WHERE vtxo_id=$1", &[&coin.id().to_string()]).await.map_err(Into::into)).await.unwrap().is_none());
	let mut config = srv.config().clone();
	config.direct_expiry.enabled = true; config.direct_expiry.interval = Duration::from_millis(100);
	config.direct_expiry.grace_blocks = 0; config.direct_expiry.max_groups_per_tick = 1;
	srv.stop().await.unwrap();
	let _server = server::Server::start(config).await.unwrap();
	let height = ctx.bitcoind().sync_client().get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	let row = await_allocation(&db, coin.id()).await;
	assert_eq!(row.get::<_, &str>("source"), "pending-board");
	assert_eq!(row.get::<_, &str>("disposition"), "pay");
	assert_eq!(row.get::<_, Vec<u8>>("spk"), fallback.as_bytes());
	assert_eq!(row.get::<_, i64>("gross_sat"), 30_000);
	assert_eq!(row.get::<_, i64>("payout_sat"), 29_000);
}

#[tokio::test]
async fn automatic_expiry_registered_arkoor_waits_for_maturity_without_starving_board() {
	let ctx = TestContext::new("expiry_automatic/automatic_expiry_registered_arkoor_waits_for_maturity_without_starving_board").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(128); c.fees.board.base_fee = sat(1_000);
	}).create().await;
	let healthy = ctx.bark_sdk("healthy", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(21_000)).create().await;
	healthy.stop_daemon_wait().await.unwrap();
	let healthy_coin = healthy.get_full_vtxo(healthy.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	ctx.generate_blocks(10).await;
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(51_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let input = sender.get_full_vtxo(sender.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let address = receiver.new_address().await.unwrap();
	let fallback = receiver.fallback_destination().await.unwrap().spk;
	sender.send_arkoor_payment(&address, input.amount()).await.unwrap();
	receiver.sync_mailbox().await.unwrap();
	let received = receiver.spendable_vtxos().await.unwrap();
	assert_eq!(received.len(), 1);
	let coin = receiver.get_full_vtxo(received[0].id()).await.unwrap();
	assert!(coin.expiry_height().to_u32() > healthy_coin.expiry_height().to_u32() + 3);
	drop(sender); drop(receiver); drop(healthy);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let prior = db.read(async |t| Ok(t.query_one("SELECT spend_state::text,oor_spent_txid FROM vtxo WHERE vtxo_id=$1", &[&input.id().to_string()]).await?)).await.unwrap();
	let mut config = srv.config().clone();
	config.direct_expiry.enabled = true; config.direct_expiry.interval = Duration::from_millis(100);
	config.direct_expiry.grace_blocks = 0; config.direct_expiry.max_groups_per_tick = 1;
	srv.stop().await.unwrap();
	let _server = server::Server::start(config).await.unwrap();
	let height = ctx.bitcoind().sync_client().get_block_count().unwrap() as u32;
	ctx.generate_blocks(healthy_coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	await_allocation(&db, healthy_coin.id()).await;
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(allocation(&db, coin.id()).await.is_none(), "an actual immature cut must defer while its independent mature neighbour progresses");
	assert!(allocation(&db, input.id()).await.is_none(), "a consumed ancestor cannot be finalized independently");
	let after = db.read(async |t| Ok(t.query_one("SELECT spend_state::text,oor_spent_txid FROM vtxo WHERE vtxo_id=$1", &[&input.id().to_string()]).await?)).await.unwrap();
	assert_eq!(prior.get::<_, String>(0), after.get::<_, String>(0));
	assert_eq!(prior.get::<_, Option<String>>(1), after.get::<_, Option<String>>(1));
	let height = ctx.bitcoind().sync_client().get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	let row = await_allocation(&db, coin.id()).await;
	assert_eq!(row.get::<_, &str>("source"), "registered");
	assert_eq!(row.get::<_, &str>("disposition"), "pay");
	assert_eq!(row.get::<_, Vec<u8>>("spk"), fallback.as_bytes());
	assert_eq!(row.get::<_, i64>("gross_sat"), 50_000);
	assert_eq!(row.get::<_, i64>("payout_sat"), 49_000);
	assert!(allocation(&db, input.id()).await.is_none());
}

#[tokio::test]
async fn automatic_expiry_unregistered_arkoor_pays_exact_actual_input_owner() {
	let ctx = TestContext::new("expiry_automatic/automatic_expiry_unregistered_arkoor_pays_exact_actual_input_owner").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(btc(1)).cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(128); c.fees.board.base_fee = sat(1_000);
	}).create().await;
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(51_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let input = sender.get_full_vtxo(sender.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let fallback = sender.fallback_destination().await.unwrap().spk;
	let key = sender.pubkey_keypair(&input.user_pubkey()).await.unwrap().unwrap().1;
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let address = receiver.new_address().await.unwrap();
	assert_ne!(fallback, receiver.fallback_destination().await.unwrap().spk);
	let builder = ArkoorPackageBuilder::new_with_checkpoints(vec![input.clone()], vec![
		ArkoorDestination { total_amount: input.amount(), policy: address.policy().clone() },
	]).unwrap().generate_user_nonces(&[key]).unwrap();
	let mut request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	for part in &mut request.parts {
		for output in part.outputs.iter_mut().chain(part.isolated_outputs.iter_mut()) {
			assert_eq!(VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey(), address.policy().user_pubkey());
			output.origin = Some(address.account_child().unwrap().unwrap().into());
		}
	}
	let response = srv.get_public_rpc().await.request_arkoor_cosign(request).await.unwrap().into_inner();
	let outputs = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap().build_signed_vtxos();
	assert_eq!(outputs.len(), 1);
	let coin = outputs[0].clone();
	// Never call transaction registration or mailbox sync. The actual cosign
	// record reconstructs ancestry; ownership remains with the actual input.
	drop(sender); drop(receiver);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mut config = srv.config().clone();
	config.direct_expiry.enabled = true; config.direct_expiry.interval = Duration::from_millis(100);
	config.direct_expiry.grace_blocks = 0; config.direct_expiry.max_groups_per_tick = 1;
	srv.stop().await.unwrap();
	let _server = server::Server::start(config).await.unwrap();
	let height = ctx.bitcoind().sync_client().get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	let row = await_allocation(&db, coin.id()).await;
	assert_eq!(row.get::<_, &str>("source"), "unregistered");
	assert_eq!(row.get::<_, &str>("disposition"), "pay");
	assert_eq!(row.get::<_, Vec<u8>>("spk"), fallback.as_bytes());
	assert_eq!(row.get::<_, i64>("gross_sat"), input.amount().to_sat() as i64);
	assert_eq!(row.get::<_, i64>("fee_sat"), 1_000);
	assert_eq!(row.get::<_, i64>("payout_sat"), 49_000);
	assert!(allocation(&db, input.id()).await.is_none(), "consumed original is retirement closure, never independent customer or operator value");
}
