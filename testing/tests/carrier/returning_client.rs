//! Real SDK freeze status and confirmed direct delivery; no simulated paid rows.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use bark::expiry_payout::{adopt_server_vtxo_status, AdoptedVtxoState};
use bark::vtxo::VtxoStateKind;
use server::watchman::direct_delivery::{DirectDelivery, DeliveryState};
use server_rpc::{protos, StatusExt};
use ark_testing::daemon::captaind::{proxy::ArkRpcProxy, ArkClient};

use super::*;

#[derive(Clone, Default)]
struct DelayedSpend {
	entered: Arc<Notify>,
	release: Arc<Notify>,
	refusal: Arc<Mutex<Option<tonic::Status>>>,
}

#[async_trait::async_trait]
impl ArkRpcProxy for DelayedSpend {
	async fn request_arkoor_cosign(&self, upstream: &mut ArkClient,
		req: protos::ArkoorPackageCosignRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		self.entered.notify_one();
		self.release.notified().await;
		let result = upstream.request_arkoor_cosign(req).await;
		if let Err(status) = &result { *self.refusal.lock().unwrap() = Some(status.clone()); }
		Ok(result?.into_inner())
	}
}

#[tokio::test]
async fn returning_real_sdk_preserves_frozen_value_then_adopts_confirmed_carrier() {
	let ctx = TestContext::new("returning-client/returning_real_sdk_preserves_frozen_value_then_adopts_confirmed_carrier").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(ark_testing::btc(1))
		.cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128); c.fees.board.base_fee = sat(1_000);
			// Exercise the reservation boundary with a real delayed SDK request.
			// Default expired-input admission is a separate earlier refusal.
			c.allow_expired_arkoor = true;
		}).create().await;
	let delayed = DelayedSpend::default();
	let proxy = srv.start_proxy_no_mailbox(delayed.clone()).await;
	let returning = ctx.bark_sdk("returning", &proxy.address).cfg(|c| c.daemon_manual_sync = true)
		.boarded(sat(21_000)).create().await;
	returning.stop_daemon_wait().await.unwrap();
	let receiver = ctx.bark_sdk("receiver", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	receiver.stop_daemon_wait().await.unwrap();
	let destination = receiver.new_address().await.unwrap();
	let coin = returning.get_full_vtxo(returning.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	assert_eq!(coin.amount(), sat(20_000));
	let fallback = returning.fallback_destination().await.unwrap();
	returning.sync_onchain().await.unwrap();
	let chain_before = returning.onchain().unwrap().read().await.balance().await;
	let active = returning.clone();
	let requested_destination = destination.clone();
	let spend = tokio::spawn(async move { active.send_arkoor_payment(&requested_destination, sat(10_000)).await });
	tokio::time::timeout(Duration::from_secs(30), delayed.entered.notified()).await.unwrap();
	assert_eq!(returning.balance().await.unwrap().pending_arkoor_send, sat(20_000));
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let roots = [db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap()];
	let terminals = [ServerVtxo::from(coin.clone())];
	let residuals = partition_residuals(&roots, &terminals);
	let funding_tx = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let funding = BTreeMap::from([(funding_tx.compute_txid(), funding_tx)]);
	let master_mnemonic = wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &master_mnemonic.to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	let initial = core.get_block_count().unwrap();
	let tip = BlockRef { height: BlockHeight::new(initial as u32), hash: core.get_block_hash(initial).unwrap() };
	let mut fee_wallet = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	ctx.bitcoind().fund_addr(fee_wallet.next_unused_address(KEYCHAIN).address, sat(1_000_000)).await;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(initial as u32) + 3).await;
	sync_fee_wallet(&mut fee_wallet, &core);
	let budget = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
	let context = OwnershipContext { tip: core.get_block_count().unwrap() as u32, grace: 0,
		max_dependencies: 100, server_pubkey: coin.server_pubkey(), funding: &funding, canceled_receives: &BTreeSet::new() };
	let freeze = db.write(async |t| t.freeze_expiry_sources(&roots, &terminals, &residuals,
		&context, &BTreeMap::new(), Network::Regtest, budget).await).await.unwrap();
	returning.sync().await;
	assert_eq!(returning.balance().await.unwrap().total(), sat(20_000));
	assert_eq!(returning.balance().await.unwrap().pending_arkoor_send, sat(20_000));
	assert!(!returning.history().await.unwrap().iter().any(|m| m.subsystem.name == "bark.server_spend"));
	delayed.release.notify_one();
	assert!(tokio::time::timeout(Duration::from_secs(30), spend).await.unwrap().unwrap().is_err());
	let status = delayed.refusal.lock().unwrap().clone().expect("native SDK must reach actual server admission");
	assert!(status.message().ends_with(server_rpc::EXPIRY_FROZEN_ERROR), "{status}");
	assert!(!status.is_expiry_settled(), "a real freeze is not a payment receipt");
	returning.sync().await;
	assert_eq!(returning.balance().await.unwrap().total(), sat(20_000));
	assert_eq!(returning.get_vtxo_by_id(coin.id()).await.unwrap().state.kind(), VtxoStateKind::Spendable);
	let before = adopt_server_vtxo_status(&returning, vec![coin.id()]).await.unwrap();
	assert_ne!(before[0].state, AdoptedVtxoState::Spent);
	assert!(!returning.history().await.unwrap().iter().any(|m| m.subsystem.name == "bark.server_spend"));

	let policy = CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb(50).unwrap(), max_miner_fee: sat(10_000),
		max_weight: Weight::from_wu(400_000) };
	let rate = FeeRate::from_sat_per_vb(30).unwrap();
	let operator_script = ctx.bitcoind().get_new_address().script_pubkey();
	let held_wallet = InstrumentedLock::new("returning-client-fee", fee_wallet);
	let (mut held, carrier) = build_direct_carrier(&signer, held_wallet.lock_owned().await, &roots,
		&freeze.recipients, operator_script.clone(), BlockHeight::new(context.tip), rate, policy).await.unwrap();
	held.commit_tx(&carrier.tx);
	db.write(async |t| t.store_frozen_expiry_carrier(&freeze.id, &freeze.allocation_ids, &roots,
		&carrier, &held, &operator_script, Network::Regtest, rate, policy, budget).await).await.unwrap();
	held.take_staged(); drop(held); drop(carrier.fee_reservation);
	let reloaded = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let delivery = DirectDelivery::recover_group(db.clone(), &reloaded, &freeze.id, Network::Regtest, budget, policy).await.unwrap();
	let txid = carrier.tx.compute_txid();
	assert_eq!(delivery.observe_and_submit(&ctx.bitcoind().async_client()).await.unwrap(), DeliveryState::Pending { txid });
	returning.sync().await;
	assert_eq!(returning.balance().await.unwrap().total(), sat(20_000), "broadcast must not be adopted as paid");
	assert!(!returning.history().await.unwrap().iter().any(|m| m.subsystem.name == "bark.server_spend"));
	ctx.generate_blocks(1).await;
	assert_ne!(adopt_server_vtxo_status(&returning, vec![coin.id()]).await.unwrap()[0].state,
		AdoptedVtxoState::Spent, "mining without the authoritative confirmation record is insufficient");
	let block = match delivery.observe_and_submit(&ctx.bitcoind().async_client()).await.unwrap() {
		DeliveryState::Confirmed { txid: winner, block } => { assert_eq!(winner, txid); block },
		other => panic!("expected actual confirmed winner, got {other:?}"),
	};
	// A mismatching confirmation record is a deterministic negative control.
	// Do not invalidate Core's tip under the live upstream SyncManager: a
	// backward-height observation deliberately terminates that process.
	let wrong_block = core.get_block_hash(block.height.to_u32() as u64 - 1).unwrap();
	db.write(async |t| {
		t.execute("UPDATE expiry_carrier SET confirmed_block=$2 WHERE txid=$1",
			&[&txid.to_string(), &wrong_block.to_string()]).await?;
		Ok(())
	}).await.unwrap();
	assert_ne!(adopt_server_vtxo_status(&returning, vec![coin.id()]).await.unwrap()[0].state,
		AdoptedVtxoState::Spent, "a recorded winner outside the active chain must retain Ark value");
	assert_eq!(returning.balance().await.unwrap().total(), sat(20_000));
	db.write(async |t| {
		t.execute("UPDATE expiry_carrier SET confirmed_block=$2 WHERE txid=$1",
			&[&txid.to_string(), &block.hash.to_string()]).await?;
		Ok(())
	}).await.unwrap();
	let payout = carrier.tx.output.iter().find(|o| o.script_pubkey == fallback.spk).unwrap();
	assert_eq!(payout.value, sat(19_000));
	returning.sync_onchain().await.unwrap();
	assert_eq!(returning.onchain().unwrap().read().await.balance().await, chain_before + sat(19_000));
	returning.sync().await;
	let adopted = adopt_server_vtxo_status(&returning, vec![coin.id()]).await.unwrap();
	assert_eq!(adopted[0].state, AdoptedVtxoState::Spent,
		"actual recorded confirmed winner must remove the stale Ark entitlement exactly once");
	assert_eq!(returning.balance().await.unwrap().total(), Amount::ZERO);
	let debits = returning.history().await.unwrap().into_iter().filter(|m| m.subsystem.name == "bark.server_spend").collect::<Vec<_>>();
	assert_eq!(debits.len(), 1); assert_eq!(debits[0].effective_balance, -sat(20_000).to_signed().unwrap());
	returning.sync().await;
	assert_eq!(returning.history().await.unwrap().iter().filter(|m| m.subsystem.name == "bark.server_spend").count(), 1);
	assert!(returning.send_arkoor_payment(&destination, sat(1_000)).await.is_err(), "a returned stale client cannot spend paid backing");
}
