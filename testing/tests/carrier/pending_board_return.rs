//! Return to the original pending Board after actual automatic direct payment.

use std::sync::Arc;
use std::time::Duration;

use ark::attestations::VtxoStatusAttestation;
use bark::movement::MovementStatus;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoStateKind;
use server_rpc::protos;

use super::*;

struct PendingCase {
	ctx: TestContext,
	srv: Arc<ark_testing::Captaind>,
	mnemonic: bip39::Mnemonic,
	coin: ark::Vtxo,
	record: ark::fallback::FallbackRecord,
	status: protos::GetVtxoStatusRequest,
	movement: bark::movement::MovementId,
	onchain_before: Amount,
}

#[tokio::test]
async fn original_pending_board_returns_after_automatic_confirmed_payout() {
	let case = Box::pin(pending_case()).await;
	Box::pin(confirm_and_return(case)).await;
}

#[tokio::test]
async fn retained_pending_proof_preserves_registered_board_status() {
	let fixture = automatic_board_fixture("retained_pending_proof_preserves_registered_board_status").await;
	let owner = fixture.ctx.bark_sdk("absent", &fixture.srv).mnemonic(fixture.mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	owner.stop_daemon_wait().await.unwrap();
	let db = Db::connect(&fixture.srv.config().postgres).await.unwrap();
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM pending_board WHERE vtxo_id=$1",
		&[&fixture.coin.id().to_string()]).await?.get::<_, i64>(0))).await.unwrap(), 1);
	let (_, key) = owner.pubkey_keypair(&fixture.coin.user_pubkey()).await.unwrap().unwrap();
	let state = fixture.srv.get_public_rpc().await.get_vtxo_status(protos::GetVtxoStatusRequest {
		vtxo_id: fixture.coin.id().to_bytes().to_vec(),
		attestation: VtxoStatusAttestation::new(fixture.coin.id(), &key).serialize(),
	}).await.unwrap().into_inner();
	assert_eq!(state.spend_state, protos::VtxoSpendState::Spendable as i32,
		"retained pending proof must not override the registered ordinary row");
}

async fn pending_case() -> PendingCase {
	let ctx = TestContext::new("pending-board-return/original_pending_board_returns_after_automatic_confirmed_payout").await;
	// Both actual daemons declare Direct on their first startup. Long setup
	// intervals leave the original client time to broadcast and disappear.
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(ark_testing::btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fees.board.base_fee = sat(1_000);
			c.round_interval = Duration::from_secs(3600);
			c.direct_expiry.enabled = true;
			c.direct_expiry.grace_blocks = 0;
			c.direct_expiry.interval = Duration::from_secs(3600);
		}).watchmand_cfg(|c| {
			c.min_trusted_confs = 3;
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(),
				history_dir: c.data_dir.parent().unwrap().join("server"), max_miner_fee: sat(10_000),
			});
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
			c.watchman.reaction_interval = Duration::from_secs(3600);
			c.watchman.sweep_interval = Duration::from_secs(3600);
		}).create().await;
	fund_confirmed_split_fee_reserve(&ctx, &srv).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let original = ctx.bark_sdk("original", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).funded(sat(121_000)).create().await;
	original.stop_daemon_wait().await.unwrap();
	let board = original.board_amount(sat(21_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	let coin = original.get_full_vtxo(board.vtxos[0]).await.unwrap();
	assert_eq!(coin.amount(), sat(20_000));
	assert_eq!(original.pending_boards().await.unwrap().len(), 1);
	assert_eq!(original.balance().await.unwrap().pending_board, coin.amount());
	assert_eq!(original.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Locked);
	let record = original.fallback_destination().await.unwrap();
	record.verify(bitcoin::Network::Regtest,original.mailbox_keypair().public_key()).unwrap();
	let link=original.fallback_key_link(coin.user_pubkey()).await.unwrap();
	link.verify(bitcoin::Network::Regtest,original.mailbox_keypair().public_key(),&record).unwrap();
	let status = {
		let (_, key) = original.pubkey_keypair(&coin.user_pubkey()).await.unwrap().unwrap();
		protos::GetVtxoStatusRequest { vtxo_id: coin.id().to_bytes().to_vec(),
			attestation: VtxoStatusAttestation::new(coin.id(), &key).serialize() }
	};
	original.sync_onchain().await.unwrap();
	let onchain_before = original.onchain().unwrap().read().await.balance().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_pending_only(&db, &coin).await;
	assert_not_spent(&srv, &status).await;
	drop(original); // No original seed+DB consumer runs until the carrier confirms.
	PendingCase { ctx, srv, mnemonic, coin, record, status, movement: board.movement_id, onchain_before }
}

async fn assert_pending_only(db: &Db, coin: &ark::Vtxo) {
	let counts = db.read(async |t| {
		let row = t.query_one("SELECT
			(SELECT count(*) FROM pending_board WHERE vtxo_id=$1) AS pending,
			(SELECT count(*) FROM vtxo WHERE vtxo_id=$1) AS ordinary", &[&coin.id().to_string()]).await?;
		Ok((row.get::<_, i64>("pending"), row.get::<_, i64>("ordinary")))
	}).await.unwrap();
	assert_eq!(counts, (1, 0), "a real pending Board must not become a fabricated ordinary coin");
}

async fn assert_not_spent(srv: &ark_testing::Captaind, req: &protos::GetVtxoStatusRequest) {
	let status = srv.get_public_rpc().await.get_vtxo_status(req.clone()).await
		.expect("the retained genuine Board proof must authenticate before retirement").into_inner();
	assert_eq!(status.spend_state, protos::VtxoSpendState::Unregistered as i32,
		"a pending, frozen or merely broadcast Board is not confirmed retirement");
}

async fn confirm_and_return(case: PendingCase) {
	let PendingCase { ctx, srv, mnemonic, coin, record, status, movement, onchain_before } = case;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	srv.stop().await.unwrap();
	let mut cap = srv.config().clone();
	cap.direct_expiry.interval = Duration::from_millis(100);
	let cap_path = ctx.datadir.join("scheduled-captaind.toml");
	write_automatic_config(&cap_path, &toml::to_string_pretty(&cap).unwrap());
	let mut cap_child = spawn_automatic_daemon("CAPTAIND_EXEC", &cap_path, ctx.datadir.join("scheduled-captaind.log"));
	let height = core.get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			assert!(cap_child.try_wait().unwrap().is_none());
			let frozen = db.read(async |t| Ok(t.query_one("SELECT EXISTS (SELECT 1 FROM expiry_allocation WHERE id=$1)",
				&[&coin.id().to_string()]).await?.get::<_, bool>(0))).await.unwrap();
			if frozen { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual captaind timer must discover and freeze the pending Board");
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_carrier", &[]).await?.get::<_, i64>(0))).await.unwrap(), 0);
	assert_pending_only(&db, &coin).await;
	assert_not_spent(&srv, &status).await;
	srv.watchmand().stop().await.unwrap();
	let mut watch = srv.watchmand().config().clone();
	watch.watchman.reaction_interval = Duration::from_secs(1);
	watch.watchman.sweep_interval = Duration::from_secs(1);
	let watch_path = ctx.datadir.join("scheduled-watchmand.toml");
	write_automatic_config(&watch_path, &toml::to_string_pretty(&watch).unwrap());
	let mut watch_child = spawn_automatic_daemon("WATCHMAND_EXEC", &watch_path, ctx.datadir.join("scheduled-watchmand.log"));
	let carrier = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			assert!(watch_child.try_wait().unwrap().is_none());
			if let Some(tx) = core.get_raw_mempool().unwrap().into_iter().map(|id| core.get_raw_transaction(&id, None).unwrap())
				.find(|tx| tx.input.iter().any(|i| i.previous_output == coin.chain_anchor())) { break tx; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("actual watchmand timer must spend the frozen physical Board backing");
	let expected = TxOut { value: sat(19_000), script_pubkey: record.spk };
	let vout = carrier.output.iter().position(|o| *o == expected).expect("exact rightful gross-minus1000 output");
	assert_eq!(carrier.output.iter().filter(|o| o.script_pubkey == expected.script_pubkey).count(), 1);
	std::fs::write(ctx.datadir.join("automatic-pending-board-carrier.tx"), bitcoin::consensus::serialize(&carrier)).unwrap();
	assert_not_spent(&srv, &status).await;
	ctx.generate_blocks(1).await;
	tokio::time::timeout(Duration::from_secs(15), async {
		loop {
			let confirmed = db.read(async |t| Ok(t.query_one("SELECT confirmed_height FROM expiry_carrier WHERE txid=$1",
				&[&carrier.compute_txid().to_string()]).await?.get::<_, Option<i32>>(0))).await.unwrap();
			if confirmed.is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.unwrap();
	assert!(core.get_raw_transaction_info(&carrier.compute_txid(), None).unwrap().confirmations.unwrap() >= 1);
	wait_fee_inventory_tip(&ctx,watch.admin_address.unwrap()).await;
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(true)).unwrap().is_none());
	assert_pending_only(&db, &coin).await;
	let returning = ctx.bark_sdk("original", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	returning.stop_daemon_wait().await.unwrap();
	assert_eq!(returning.pending_boards().await.unwrap().len(), 1, "reopen the original retained action");
	returning.sync_pending_boards().await.unwrap();
	assert!(returning.pending_boards().await.unwrap().is_empty(),
		"confirmed direct payout must finish the ORIGINAL pending Board action");
	let persister = SqliteClient::open(ctx.datadir.join("original/db.sqlite")).unwrap();
	assert!(persister.get_all_wallet_action_checkpoints().await.unwrap().is_empty());
	assert_eq!(returning.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Spent);
	assert!(!returning.all_vtxos().await.unwrap().iter().any(|v| v.state.kind() == VtxoStateKind::Locked));
	assert_eq!(returning.balance().await.unwrap().total(), Amount::ZERO);
	let history = returning.history().await.unwrap();
	let board = history.iter().find(|m| m.id == movement).unwrap();
	assert_eq!(board.status, MovementStatus::Successful);
	assert_eq!(board.effective_balance, coin.amount().to_signed().unwrap());
	assert_eq!(board.offchain_fee, sat(1_000));
	let debits = history.iter().filter(|m| m.subsystem.name == "bark.server_spend").collect::<Vec<_>>();
	assert_eq!(debits.len(), 1); assert_eq!(debits[0].effective_balance, -coin.amount().to_signed().unwrap());
	assert_eq!(debits[0].input_vtxos, vec![coin.id()]);
	assert!(debits[0].sent_to.is_empty(), "generic retirement must not invent a payout receipt");
	returning.sync_onchain().await.unwrap();
	assert_eq!(returning.onchain().unwrap().read().await.balance().await, onchain_before + expected.value);
	for _ in 0..3 {
		returning.sync().await;
		assert!(returning.pending_boards().await.unwrap().is_empty());
		assert!(persister.get_all_wallet_action_checkpoints().await.unwrap().is_empty());
		let next = returning.history().await.unwrap();
		assert_eq!(next.iter().filter(|m| m.id == movement).count(), 1);
		assert_eq!(next.iter().filter(|m| m.subsystem.name == "bark.server_spend").count(), 1);
		assert_eq!(returning.balance().await.unwrap().total(), Amount::ZERO);
	}
	let foreign = Keypair::new(&ark::SECP, &mut bitcoin::secp256k1::rand::thread_rng());
	let err = srv.get_public_rpc().await.get_vtxo_status(protos::GetVtxoStatusRequest {
		vtxo_id: coin.id().to_bytes().to_vec(), attestation: VtxoStatusAttestation::new(coin.id(), &foreign).serialize(),
	}).await.expect_err("confirmed mapping must still authenticate the pending Board owner");
	assert_eq!(err.code(), tonic::Code::InvalidArgument);
	assert!(err.message().contains("invalid VTXO status attestation"));
	let unknown = ark::VtxoId::from(OutPoint::null());
	assert_eq!(srv.get_public_rpc().await.get_vtxo_status(protos::GetVtxoStatusRequest {
		vtxo_id: unknown.to_bytes().to_vec(), attestation: VtxoStatusAttestation::new(unknown, &foreign).serialize(),
	}).await.unwrap_err().code(), tonic::Code::NotFound);
	assert_pending_only(&db, &coin).await;
	cap_child.kill().await.unwrap(); cap_child.wait().await.unwrap();
	watch_child.kill().await.unwrap(); watch_child.wait().await.unwrap();
	drop(returning);
	let (discovered, spent) = Box::pin(recover_all_bip84_and_spend(&ctx, &mnemonic)).await;
	let payout = OutPoint::new(carrier.compute_txid(), vout as u32);
	assert!(discovered.contains(&(payout, expected.value)));
	assert!(spent.input.iter().any(|i| i.previous_output == payout));
}
