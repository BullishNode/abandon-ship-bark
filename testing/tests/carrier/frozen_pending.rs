//! Observe one original Board through automatic freeze, publication and return.
//! All phase observations survive a client-state assertion failure.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use ark::attestations::VtxoStatusAttestation;
use bark::onchain::OnchainWallet;
use server_rpc::protos;

use super::*;

struct Phase {
	status: i32,
	selectable: bool,
	ark: bark::Balance,
	onchain: bdk_wallet::Balance,
	debits: usize,
	receipts: usize,
}

async fn observe(
	wallet: &bark::Wallet, srv: &ark_testing::Captaind, coin: &ark::Vtxo,
	payout: Option<bitcoin::Txid>,
) -> Phase {
	wallet.sync().await;
	wallet.sync_onchain().await.unwrap();
	let (_, key) = wallet.pubkey_keypair(&coin.user_pubkey()).await.unwrap().unwrap();
	let status = srv.get_public_rpc().await.get_vtxo_status(protos::GetVtxoStatusRequest {
		vtxo_id: coin.id().to_bytes().to_vec(),
		attestation: VtxoStatusAttestation::new(coin.id(), &key).serialize(),
	}).await.unwrap().into_inner().spend_state;
	let onchain = wallet.onchain().unwrap();
	let guard = onchain.read().await;
	let bdk = (&*guard as &dyn Any).downcast_ref::<OnchainWallet>().unwrap();
	Phase {
		status,
		selectable: wallet.spendable_vtxos().await.unwrap().iter().any(|v| v.id() == coin.id()),
		ark: wallet.balance().await.unwrap(),
		onchain: bdk.balance(),
		debits: wallet.history().await.unwrap().iter().filter(|m| m.subsystem.name == "bark.server_spend").count(),
		receipts: bdk.list_transaction_infos().unwrap().iter().filter(|t| Some(t.txid) == payout).count(),
	}
}

fn record(ctx: &TestContext, name: &str, phase: &Phase) {
	let value = serde_json::json!({
		"phase": name, "authenticated_spend_state": phase.status,
		"original_coin_in_spendable_vtxos": phase.selectable,
		"ark_balance": bark_json::cli::Balance::from(phase.ark.clone()),
		"ark_total_sat": phase.ark.total().to_sat(),
		"ark_summary_pending_sat": phase.ark.summary().pending.to_sat(),
		"onchain_balance": bark_json::cli::onchain::OnchainBalance::from(phase.onchain.clone()),
		"naive_ark_plus_onchain_total_sat": (phase.ark.total() + phase.onchain.total()).to_sat(),
		"server_spend_movement_count": phase.debits,
		"actual_payout_transaction_count": phase.receipts,
		"scope": "actual SDK and chain observations; no browser DOM qualification",
	});
	let path = ctx.datadir.join(format!("c14-{name}.json"));
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();
	file.write_all(&serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

#[tokio::test]
async fn returning_registered_board_pending_phases() {
	Box::pin(registered_board_phases()).await;
}

async fn registered_board_phases() {
	// Keep debug poll frames separate without changing any runtime stack size.
	let (ctx, srv, watch_funding, mnemonic, wallet) = Box::pin(board_setup()).await;
	Box::pin(observe_board_phases(ctx, srv, watch_funding, mnemonic, wallet)).await;
}

async fn board_setup() -> (
	TestContext, Arc<ark_testing::Captaind>, bitcoin::Txid, bip39::Mnemonic, bark::Wallet,
) {
	let ctx = TestContext::new("c14/returning_registered_board_pending_phases").await;
	let history = ctx.datadir.join("server");
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(sat(200_000)).cfg(|c| {
			c.round_interval = Duration::from_secs(3600);
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fees.board.base_fee = Amount::ZERO;
			c.expiry_payout.enabled = false;
			c.direct_expiry.enabled = true;
			c.direct_expiry.grace_blocks = 0;
			c.direct_expiry.interval = Duration::from_millis(100);
			c.direct_expiry.max_groups_per_tick = 1;
		}).watchmand_cfg(move |c| {
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig {
				history_dir: history, max_miner_fee: sat(10_000),
			});
			c.watchman.sweep_interval = Duration::from_secs(1);
			c.watchman.reaction_interval = Duration::from_secs(1);
			c.fee_estimator.fallback_fee_rate_regular = FeeRate::from_sat_per_vb(7).unwrap();
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
		}).create().await;
	let watch_funding = ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await, sat(50_000)).await;
	ctx.generate_blocks(3).await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = Box::pin(ctx.bark_sdk("original", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).funded(sat(100_000)).boarded(sat(20_000)).create()).await;
	(ctx, srv, watch_funding, mnemonic, wallet)
}

async fn observe_board_phases(
	ctx: TestContext, srv: Arc<ark_testing::Captaind>, watch_funding: bitcoin::Txid,
	mnemonic: bip39::Mnemonic, wallet: bark::Wallet,
) {
	let core = ctx.bitcoind().sync_client();
	wallet.stop_daemon_wait().await.unwrap();
	let coins = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(coins.len(), 1);
	let coin = wallet.get_full_vtxo(coins[0].id()).await.unwrap();
	assert_eq!(coin.amount(), sat(20_000));
	let fallback = wallet.fallback_destination().await.unwrap();
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	// All350k is funded before absence. This owned process barrier makes the
	// frozen phase stable; no freeze or publication endpoint is invoked.
	srv.watchmand().stop().await.unwrap();
	let pre = observe(&wallet, &srv, &coin, None).await;
	record(&ctx, "pre", &pre);
	let original_change = pre.onchain.total();
	let funding = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let inputs = funding.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
		.output[i.previous_output.vout as usize].value).sum::<Amount>();
	let board_miner = inputs - funding.output.iter().map(|o| o.value).sum::<Amount>();
	assert_eq!(sat(100_000), original_change + coin.amount() + board_miner);
	drop(wallet);
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let frozen = db.read(async |t| Ok(t.query_opt("SELECT id FROM expiry_allocation WHERE id=$1",
				&[&coin.id().to_string()]).await?.is_some())).await.unwrap();
			if frozen { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("normal captaind must freeze the original registered Board");
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_carrier", &[]).await?
		.get::<_, i64>(0))).await.unwrap(), 0);
	let wallet = Box::pin(ctx.bark_sdk("original", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).create()).await;
	wallet.stop_daemon_wait().await.unwrap();
	let frozen = observe(&wallet, &srv, &coin, None).await;
	record(&ctx, "frozen", &frozen);
	srv.watchmand().start().await.unwrap();
	let carrier = tokio::time::timeout(Duration::from_secs(45), async {
		loop {
			let tx = core.get_raw_mempool().unwrap().into_iter().map(|id| core.get_raw_transaction(&id, None).unwrap())
				.find(|tx| tx.input.iter().any(|i| i.previous_output == coin.chain_anchor()));
			if let Some(tx) = tx { break tx; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("normal watchmand must publish the original automatic carrier");
	let txid = carrier.compute_txid();
	let payouts = carrier.output.iter().filter(|o| o.script_pubkey == fallback.spk).collect::<Vec<_>>();
	assert_eq!(payouts.len(), 1); assert_eq!(payouts[0].value, sat(19_000));
	assert!(carrier.input.iter().all(|i| i.previous_output == coin.chain_anchor() || i.previous_output.txid == watch_funding),
		"the carrier may use only its original backing and declared watchman reserve");
	let carrier_inputs = carrier.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
		.output[i.previous_output.vout as usize].value).sum::<Amount>();
	let carrier_miner = carrier_inputs - carrier.output.iter().map(|o| o.value).sum::<Amount>();
	assert!(carrier_miner <= sat(10_000));
	assert!(carrier_miner <= FeeRate::from_sat_per_vb(50).unwrap().fee_wu(carrier.weight()).unwrap());
	let published = observe(&wallet, &srv, &coin, Some(txid)).await;
	record(&ctx, "published", &published);
	assert!(core.get_raw_mempool().unwrap().contains(&txid));
	ctx.generate_blocks(1).await;
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let confirmed = db.read(async |t| Ok(t.query_one("SELECT confirmed_height FROM expiry_carrier WHERE txid=$1",
				&[&txid.to_string()]).await?.get::<_, Option<i32>>(0))).await.unwrap();
			if confirmed.is_some() { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("normal daemon must record the actual confirmed carrier");
	let confirmed = observe(&wallet, &srv, &coin, Some(txid)).await;
	record(&ctx, "confirmed", &confirmed);
	let repeated = observe(&wallet, &srv, &coin, Some(txid)).await;
	record(&ctx, "repeated", &repeated);
	drop(wallet);
	let wallet = Box::pin(ctx.bark_sdk("original", &srv).mnemonic(mnemonic)
		.cfg(|c| c.daemon_manual_sync = true).create()).await;
	wallet.stop_daemon_wait().await.unwrap();
	let reopened = observe(&wallet, &srv, &coin, Some(txid)).await;
	record(&ctx, "reopened", &reopened);
	let debit = wallet.history().await.unwrap().into_iter().find(|m| m.subsystem.name == "bark.server_spend").unwrap();
	assert_eq!(debit.effective_balance, -sat(20_000).to_signed().unwrap());
	let mut failures = Vec::new();
	let mut check = |ok: bool, message: &'static str| { if !ok { failures.push(message); } };
	check(pre.status == protos::VtxoSpendState::Spendable as i32 && pre.selectable, "pre: ordinary registered Board must be available");
	check(pre.ark.spendable == sat(20_000) && pre.ark.pending() == Amount::ZERO, "pre: usable20k without pending");
	check(frozen.status != protos::VtxoSpendState::Spendable as i32 && frozen.status != protos::VtxoSpendState::Spent as i32,
		"frozen: authenticated status must distinguish reservation from available and retired");
	check(!frozen.selectable, "frozen: original coin must leave raw spendable selection");
	check(frozen.ark.summary().pending == sat(20_000) && frozen.ark.needs_refresh == Amount::ZERO,
		"frozen: gross20k belongs in pending reservation, not needs-refresh");
	for phase in [&frozen, &published] {
		check(phase.ark.total() == sat(20_000) && phase.ark.spendable == Amount::ZERO && phase.debits == 0,
			"frozen/published: preserve gross Ark without usable balance or invented terminal debit");
	}
	check(published.onchain.untrusted_pending == sat(19_000) && published.receipts == 1,
		"published: actual original BDK must discover exactly one19k unconfirmed receipt");
	// Both raw ledgers legitimately overlap before retirement. Their sum is
	// recorded only as diagnostic evidence; a combined presenter needs an
	// authenticated relation and separate browser qualification.
	for phase in [&confirmed, &repeated, &reopened] {
		check(phase.status == protos::VtxoSpendState::Spent as i32 && !phase.selectable && phase.ark.total() == Amount::ZERO,
			"confirmed/repeat/reopen: original Ark entitlement must retire");
		check(phase.onchain.total() == original_change + sat(19_000) && phase.debits == 1 && phase.receipts == 1,
			"confirmed/repeat/reopen: one19k receipt and one20k Ark debit");
	}
	let path = ctx.datadir.join("c14-assertion-failures.json");
	std::fs::write(&path, serde_json::to_vec_pretty(&failures).unwrap()).unwrap();
	std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
	assert!(failures.is_empty(), "C14 phase assertions failed: {failures:?}");
}
