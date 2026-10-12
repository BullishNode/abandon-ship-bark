//! Absent delegated refresh owner: payout versus a returning phone.
use std::time::Duration;

use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;
use bitcoin_ext::BlockDelta;
use server::database::Db;
use server_log::RoundFinished;

use ark_testing::{btc, sat, TestContext};
use ark_testing::util::FutureExt;

#[tokio::test]
async fn fallback_delegated_return_after_payout_terminates() {
    Box::pin(return_after_payout()).await;
}

async fn return_after_payout() {
    let name = "fallback_delegated_return_after_payout_terminates";
    let ctx = TestContext::new(format!("bark_sdk/{name}")).await;
    let srv = ctx.captaind("server").no_vtxo_pool().funded(btc(1)).watchmand().cfg(|c| {
        c.vtxo_lifetime = BlockDelta::new(200);
        c.round_interval = Duration::from_secs(3600);
    }).create().await;
    let mnemonic = bip39::Mnemonic::generate(12).unwrap();
    let wallet = ctx.bark_sdk("phone", &srv).mnemonic(mnemonic.clone())
        .cfg(|c| c.daemon_manual_sync = true).boarded(sat(300_000)).create().await;
    wallet.stop_daemon_wait().await.unwrap();
    let originals = wallet.spendable_vtxos().await.unwrap();
    assert_eq!(originals.len(), 1);
    let original = wallet.get_full_vtxo(originals[0].id()).await.unwrap();
    let record = wallet.fallback_destination().await.unwrap();
    let round = wallet.refresh_vtxos_delegated(vec![original.id()]).await.unwrap().unwrap();
    let unlock = round.state().unlock_hash().unwrap().to_string();
    let mut config = wallet.config().clone();
    drop(wallet);
    let mut finished = srv.subscribe_log::<RoundFinished>();
    srv.trigger_round().await;
    finished.recv().wait(Duration::from_secs(60)).await.unwrap();
    // No phone operation while the round issues, expires and settles.
    // The controller reads only the server DB and chain until the late return.
    let db = Db::connect(&srv.config().postgres).await.unwrap();
    let rows = db.read(async |t| Ok(t.query(
        "SELECT vtxo FROM vtxo WHERE spend_state='unclaimed'", &[],
    ).await?)).await.unwrap();
    let replacements = rows.iter().map(|r| Vtxo::<Full>::deserialize(&r.get::<_, Vec<u8>>(0)).unwrap())
        .filter(|v| v.unlock_hash().is_some_and(|h| h.to_string() == unlock)).collect::<Vec<_>>();
    assert!(!replacements.is_empty());
    let principal = replacements.iter().map(|v| v.amount().to_sat()).sum::<u64>();
    assert_eq!(principal, original.amount().to_sat(), "this case isolates terminal adoption with the default zero refresh fee");
    let ids = replacements.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
    let mut backing = replacements.clone();
    backing.push(original.clone());
    super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &backing).await;
    super::fallback_lightning::enable_payouts(&ctx, &srv).await;
    let (payout, _) = super::fallback_lightning::wait_and_reconcile_payout(
        &ctx, &db, &ids, &record.spk, principal,
    ).await;
    println!("delegated late return: original_expiry={}, replacement_expiry={}, confirmed_payout={payout}",
        original.expiry_height(), replacements.iter().map(|v| v.expiry_height()).max().unwrap());
    // Payout enablement allocates fresh test-server ports. Reopen the same
    // phone database with that endpoint; an unreachable old port proves nothing.
    config.server_address = srv.ark_url();
    let wallet = bark::Wallet::open(bitcoin::Network::Regtest,
        bark::WalletSeed::new_from_mnemonic(bitcoin::Network::Regtest, &mnemonic), config,
        bark::OpenWalletArgs { datadir: Some(ctx.datadir.join("phone")), run_daemon: false, ..Default::default() },
    ).await.unwrap();
    // Payout is confirmed before the phone returns. A generic retry error
    // alone is insufficient: its old coins and pending round must terminate.
    let status = wallet.sync_pending_rounds().await.unwrap();
    wallet.sync().await;
    wallet.sync_onchain().await.unwrap();
    let pending = wallet.pending_round_state_ids().await.unwrap();
    println!("late return: synced_status={status:?}, pending_rounds={pending:?}");
    let forfeited: bool = db.read(async |t| Ok(t.query_one(
        "SELECT forfeited_at IS NOT NULL FROM round_participation WHERE unlock_hash=$1", &[&unlock],
    ).await?.get(0))).await.unwrap();
    println!("late return: server released replacement unlock={forfeited}");
    assert!(!forfeited, "a settled replacement must not release its unlock preimage to a new late claim");
    assert!(!pending.contains(&round.id()), "expiry-paid delegated refresh must terminate, not retain a retryable round");
    assert!(wallet.spendable_vtxos().await.unwrap().is_empty(), "paid replacement coins must not appear spendable");
    assert!(wallet.pending_round_states().await.unwrap().is_empty());
    let movements = wallet.movements().await.unwrap().len();
    wallet.sync_pending_rounds().await.unwrap();
    wallet.sync().await;
    assert_eq!(wallet.movements().await.unwrap().len(), movements, "terminal payout adoption must be idempotent");
    drop(wallet);
    super::fallback_lightning::restore_and_spend(&ctx, &mnemonic, payout).await;
}
