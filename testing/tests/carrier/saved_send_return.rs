//! Original saved sends must finish after actual confirmed direct settlement.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use bark::movement::MovementStatus;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::vtxo::VtxoStateKind;
use server_rpc::{protos, StatusExt};
use ark_testing::daemon::captaind::{proxy::ArkRpcProxy, ArkClient};

use super::*;

#[derive(Clone)]
struct InterruptRegistration {
	reached: Arc<Notify>,
	commit: bool,
	recipient: bitcoin::secp256k1::PublicKey,
}

#[async_trait::async_trait]
impl ArkRpcProxy for InterruptRegistration {
	async fn register_vtxo_transactions(
		&self, upstream: &mut ArkClient, request: protos::RegisterVtxoTransactionsRequest,
	) -> Result<protos::Empty, tonic::Status> {
		let destination = request.vtxos.iter().any(|bytes| ark::Vtxo::deserialize(bytes)
			.map(|v: ark::Vtxo| v.user_pubkey() == self.recipient).unwrap_or(false));
		if !destination { return Ok(upstream.register_vtxo_transactions(request).await?.into_inner()); }
		if self.commit { upstream.register_vtxo_transactions(request).await?; }
		self.reached.notify_one();
		Err(tonic::Status::unavailable("fixture interrupts registration request or reply"))
	}
}

struct SavedSendCase {
	fixture: AutomaticBoardFixture,
	recipient_seed: bip39::Mnemonic,
	recipient_script: ScriptBuf,
	signed: Vec<ark::Vtxo>,
	registered: bool,
	onchain_before: Amount,
}

#[tokio::test]
async fn original_unregistered_send_returns_after_automatic_direct_refund() {
	let case = Box::pin(saved_send_case(false)).await;
	Box::pin(confirm_and_return(case)).await;
}

#[tokio::test]
async fn original_registered_send_returns_after_automatic_direct_payment() {
	let case = Box::pin(saved_send_case(true)).await;
	Box::pin(confirm_and_return(case)).await;
}

async fn saved_send_case(registered: bool) -> SavedSendCase {
	let name = if registered { "saved_registered_send" } else { "saved_unregistered_send" };
	let fixture = automatic_board_fixture_with_fee(name, sat(1_000)).await;
	let sender = fixture.ctx.bark_sdk("absent", &fixture.srv).mnemonic(fixture.mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let recipient_seed = bip39::Mnemonic::generate(12).unwrap();
	let recipient = fixture.ctx.bark_sdk("recipient", &fixture.srv).mnemonic(recipient_seed.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let destination = recipient.new_address().await.unwrap();
	let recipient_script = recipient.fallback_destination().await.unwrap().spk;
	assert_ne!(recipient_script, fixture.record.spk);
	drop(recipient);
	let reached = Arc::new(Notify::new());
	let proxy = fixture.srv.start_proxy_no_mailbox(InterruptRegistration {
		reached: reached.clone(), commit: registered, recipient: destination.policy().user_pubkey(),
	}).await;
	let mut config = sender.config().clone();
	config.server_address = proxy.address.clone();
	drop(sender);
	let sender = bark::Wallet::open(Network::Regtest,
		bark::WalletSeed::new_from_mnemonic(Network::Regtest, &fixture.mnemonic), config,
		bark::OpenWalletArgs { datadir: Some(fixture.ctx.datadir.join("absent")), run_daemon: false, ..Default::default() },
	).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		tokio::select! {
			result = sender.send_arkoor_payment(&destination, sat(10_000)) => panic!("send passed interruption: {result:?}"),
			_ = reached.notified() => {},
		}
	}).await.expect("actual SDK send must save its registration checkpoint");
	let pending = sender.pending_arkoor_sends().await.unwrap();
	assert_eq!(pending.len(), 1);
	let bark::actions::arkoor_send::Progress::Registration {
		signed_destination_vtxos, signed_change_vtxos, ..
	} = &pending[0].progress else { panic!("interrupted registration must remain saved"); };
	assert_eq!(signed_destination_vtxos.iter().map(|v| v.amount()).sum::<Amount>(), sat(10_000));
	assert_eq!(signed_change_vtxos.iter().map(|v| v.amount()).sum::<Amount>(), sat(10_000));
	let signed = signed_destination_vtxos.iter().chain(signed_change_vtxos).cloned().collect::<Vec<_>>();
	assert_eq!(sender.get_vtxo_by_id(fixture.coin.id()).await.unwrap().state.kind(), VtxoStateKind::Locked);
	let db = Db::connect(&fixture.srv.config().postgres).await.unwrap();
	for coin in &signed {
		assert_eq!(db.read(async |t| t.get_user_vtxo_by_id(coin.id()).await).await.unwrap().spend_state,
			if registered { SpendState::Spendable } else { SpendState::Unregistered });
	}
	sender.sync_onchain().await.unwrap();
	let onchain_before = sender.onchain().unwrap().read().await.balance().await;
	drop(sender); drop(proxy);
	SavedSendCase { fixture, recipient_seed, recipient_script, signed, registered, onchain_before }
}

async fn confirm_and_return(case: SavedSendCase) {
	let SavedSendCase { fixture, recipient_seed, recipient_script, signed, registered, onchain_before } = case;
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } = fixture;
	let fee_address = tokio::time::timeout(Duration::from_secs(30), srv.watchmand().wait_wallet_address()).await.unwrap();
	ctx.bitcoind().fund_addr(fee_address, sat(50_000)).await;
	ctx.generate_blocks(3).await;
	srv.watchmand().stop().await.unwrap(); srv.stop().await.unwrap();
	let mut cap = srv.config().clone();
	cap.direct_expiry.interval = Duration::from_millis(100);
	let cap_path = ctx.datadir.join("saved-send-captaind.toml");
	write_automatic_config(&cap_path, &toml::to_string_pretty(&cap).unwrap());
	let mut watch = srv.watchmand().config().clone();
	watch.watchman.reaction_interval = Duration::from_secs(1);
	watch.watchman.sweep_interval = Duration::from_secs(1);
	let watch_path = ctx.datadir.join("saved-send-watchmand.toml");
	write_automatic_config(&watch_path, &toml::to_string_pretty(&watch).unwrap());
	let mut cap_child = spawn_automatic_daemon("CAPTAIND_EXEC", &cap_path, ctx.datadir.join("saved-send-captaind.log"));
	let mut watch_child = spawn_automatic_daemon("WATCHMAND_EXEC", &watch_path, ctx.datadir.join("saved-send-watchmand.log"));
	let core = ctx.bitcoind().sync_client();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let height = core.get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	let carrier = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			assert!(cap_child.try_wait().unwrap().is_none());
			assert!(watch_child.try_wait().unwrap().is_none());
			if let Some(tx) = core.get_raw_mempool().unwrap().into_iter().map(|id| core.get_raw_transaction(&id, None).unwrap())
				.find(|tx| tx.input.iter().any(|i| i.previous_output == coin.chain_anchor())) { break tx; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("real daemon timers must settle the signed send's backing");
	let sender_net = if registered { sat(9_000) } else { sat(19_000) };
	let expected_sender = TxOut { value: sender_net, script_pubkey: record.spk.clone() };
	assert_eq!(carrier.output.iter().filter(|o| o.script_pubkey == record.spk).collect::<Vec<_>>(), vec![&expected_sender]);
	let expected_recipient = TxOut { value: sat(9_000), script_pubkey: recipient_script.clone() };
	let recipient_outputs = carrier.output.iter().filter(|o| o.script_pubkey == recipient_script).collect::<Vec<_>>();
	assert_eq!(recipient_outputs, if registered { vec![&expected_recipient] } else { vec![] },
		"the actual registration outcome chooses the payee; a lost reply changes no ownership");
	std::fs::write(ctx.datadir.join("saved-send-carrier.tx"), bitcoin::consensus::serialize(&carrier)).unwrap();
	let pending_refusal = srv.get_public_rpc().await.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest {
		vtxos: signed.iter().map(|v| v.serialize()).collect(),
	}).await.unwrap_err();
	assert!(pending_refusal.message().ends_with(server_rpc::EXPIRY_FROZEN_ERROR));
	assert!(!pending_refusal.is_expiry_settled(), "mempool publication must not finish saved actions");
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
	// Confirmation must not bypass the existing full signed-chain checks.
	let tx = signed[0].transactions().last().unwrap().tx;
	let signature = tx.input[0].witness.iter().find(|w| w.len() == 64).unwrap();
	let mut invalid = signed[0].serialize();
	let position = invalid.windows(64).position(|w| w == signature).unwrap();
	invalid[position] ^= 1;
	let invalid_coin: ark::Vtxo = ark::Vtxo::deserialize(&invalid).unwrap();
	assert_eq!(invalid_coin.id(), signed[0].id());
	assert!(invalid_coin.has_all_witnesses());
	let invalid_refusal = srv.get_public_rpc().await.register_vtxo_transactions(
		protos::RegisterVtxoTransactionsRequest { vtxos: vec![invalid] },
	).await.unwrap_err();
	assert!(invalid_refusal.message().contains("vtxo validation failed"));
	assert!(!invalid_refusal.is_expiry_settled());
	for vtxo in &signed {
		let before = db.read(async |t| t.get_user_vtxo_by_id(vtxo.id()).await).await.unwrap();
		assert_eq!(before.vtxo.has_all_witnesses(), registered);
		let request = protos::RegisterVtxoTransactionsRequest { vtxos: vec![vtxo.serialize()] };
		let current = srv.get_public_rpc().await.register_vtxo_transactions(request.clone()).await.unwrap_err();
		assert!(current.is_expiry_settled());
		let alias = srv.get_public_rpc().await.register_vtxos(request).await.unwrap_err();
		assert!(alias.is_expiry_settled(), "the deprecated registration alias must preserve the same terminal result");
		let after = db.read(async |t| t.get_user_vtxo_by_id(vtxo.id()).await).await.unwrap();
		assert_eq!(after.vtxo.serialize(), before.vtxo.serialize(), "terminal refusal preserves the actual registration outcome");
		assert_eq!(after.spend_state, before.spend_state);
	}
	let returning = ctx.bark_sdk("absent", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create().await;
	returning.stop_daemon_wait().await.unwrap();
	assert_eq!(returning.pending_arkoor_sends().await.unwrap().len(), 1);
	for _ in 0..3 { returning.sync_pending_arkoor_sends().await.unwrap(); }
	assert!(returning.pending_arkoor_sends().await.unwrap().is_empty(),
		"confirmed direct settlement must finish the ORIGINAL saved send and release its input lock");
	let persister = SqliteClient::open(ctx.datadir.join("absent/db.sqlite")).unwrap();
	assert!(persister.get_all_wallet_action_checkpoints().await.unwrap().is_empty());
	assert_eq!(returning.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Spent);
	assert_eq!(returning.balance().await.unwrap().total(), Amount::ZERO);
	let history = returning.history().await.unwrap();
	let sends = history.iter().filter(|m| m.subsystem.name == "bark.arkoor").collect::<Vec<_>>();
	assert_eq!(sends.len(), 1);
	assert_eq!(sends[0].status, if registered { MovementStatus::Successful } else { MovementStatus::Failed });
	assert_eq!(sends[0].effective_balance, -sat(20_000).to_signed().unwrap());
	assert!(sends[0].output_vtxos.is_empty(), "confirmed payout change must not become a new Ark coin");
	if !registered { assert!(sends[0].sent_to.is_empty()); }
	assert_eq!(history.iter().map(|m| m.effective_balance).sum::<bitcoin::SignedAmount>(), bitcoin::SignedAmount::ZERO);
	returning.sync_onchain().await.unwrap();
	assert_eq!(returning.onchain().unwrap().read().await.balance().await, onchain_before + sender_net);
	for _ in 0..3 {
		returning.sync().await;
		assert!(returning.pending_arkoor_sends().await.unwrap().is_empty());
		assert!(persister.get_all_wallet_action_checkpoints().await.unwrap().is_empty());
		assert_eq!(returning.history().await.unwrap().iter().filter(|m| m.subsystem.name == "bark.arkoor").count(), 1);
	}
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_carrier", &[]).await?.get::<_, i64>(0))).await.unwrap(), 1);
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM mailbox WHERE mailbox_type='arkoor-receive'", &[]).await?.get::<_, i64>(0))).await.unwrap(), 0);
	cap_child.kill().await.unwrap(); cap_child.wait().await.unwrap();
	watch_child.kill().await.unwrap(); watch_child.wait().await.unwrap();
	drop(returning);
	let (found, spend) = Box::pin(recover_all_bip84_and_spend(&ctx, &mnemonic)).await;
	let vout = carrier.output.iter().position(|o| *o == expected_sender).unwrap() as u32;
	let payout = OutPoint::new(carrier.compute_txid(), vout);
	assert!(found.contains(&(payout, sender_net)));
	assert!(spend.input.iter().any(|i| i.previous_output == payout));
	if registered {
		let (found, spend) = Box::pin(recover_all_bip84_and_spend(&ctx, &recipient_seed)).await;
		let vout = carrier.output.iter().position(|o| *o == expected_recipient).unwrap() as u32;
		let payout = OutPoint::new(carrier.compute_txid(), vout);
		assert!(found.contains(&(payout, sat(9_000))));
		assert!(spend.input.iter().any(|i| i.previous_output == payout));
	}
}
