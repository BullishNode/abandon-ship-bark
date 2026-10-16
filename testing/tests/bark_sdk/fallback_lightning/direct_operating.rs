//! Genuine external-Lightning sources through the automatic C direct route.
//! Legacy journey callers keep their original settlement route and assertions.

use std::collections::{BTreeMap, BTreeSet};
use super::*;

#[tokio::test]
async fn operating_source_failed_send_automatic_direct() {
	Box::pin(absent_sender_route(false, true)).await;
}

#[tokio::test]
async fn operating_source_collected_receive_automatic_direct() {
	Box::pin(interrupted_receive_for(ReceiveOutcome::Collected, ReturnFault::None, false, true)).await;
}

pub(super) fn configure<'a>(
	builder: ark_testing::context::builders::CaptaindBuilder<'a>, direct: bool,
) -> ark_testing::context::builders::CaptaindBuilder<'a> {
	if !direct { return builder; }
	builder.cfg(configure_cap).watchmand_cfg(|c| {
		c.min_trusted_confs = 3;
		c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig {
			ordinary_fee_policy: Default::default(),
			history_dir: c.data_dir.parent().unwrap().join("server"), max_miner_fee: sat(10_000),
		});
		c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
		c.watchman.sweep_interval = Duration::from_secs(3600);
		c.watchman.reaction_interval = Duration::from_secs(3600);
	})
}

// CaptaindBuilder.cfg replaces its closure. A journey adding lifetime settings
// must call this same fixed setter inside its final configuration closure.
pub(super) fn configure_cap(c: &mut server::Config) {
	assert!(!c.expiry_payout.enabled);
	c.direct_expiry.enabled = true;
	c.direct_expiry.grace_blocks = 6;
	c.direct_expiry.interval = Duration::from_secs(3600);
	c.round_interval = Duration::from_secs(3600);
}

// Match native watchman/daemon.rs: the independent ledger lives in the
// witness subdirectory of the ACTUAL configured history root, not beside it.
fn fee_history(srv: &Captaind) -> std::path::PathBuf {
	let root = srv.watchmand().config().watchman.direct_expiry.as_ref().unwrap().history_dir.clone();
	assert_eq!(root, srv.config().data_dir, "both actual daemons share the declared independent history root");
	root.join("direct-expiry-witness")
}

pub(super) async fn fund_fee_wallet(ctx: &TestContext, srv: &Captaind) {
	let address = tokio::time::timeout(Duration::from_secs(30), srv.watchmand().wait_wallet_address())
		.await.expect("real owning fee wallet receive address");
	let mut funded = BTreeSet::new();
	for _ in 0..5 {
		let txid = ctx.bitcoind().fund_addr(address.clone(), sat(10_000)).await;
		ctx.await_transaction(txid).await;
		let tx = ctx.bitcoind().sync_client().get_raw_transaction(&txid, None).unwrap();
		let outputs = tx.output.iter().enumerate().filter(|(_, o)| o.script_pubkey == address.script_pubkey())
			.collect::<Vec<_>>();
		assert_eq!(outputs.len(), 1);
		assert_eq!(outputs[0].1.value, sat(10_000));
		assert!(funded.insert(OutPoint::new(txid, outputs[0].0 as u32).to_string()));
	}
	ctx.generate_blocks(3).await;
	let history = fee_history(srv);
	assert!(std::fs::read_to_string(history.join("ordinary-fee-budget")).unwrap().is_empty(),
		"no preparation may charge the source fixture before expiry");
	let policy: serde_json::Value = serde_json::from_slice(&std::fs::read(history.join("ordinary-fee-policy.json")).unwrap()).unwrap();
	assert_eq!(policy, serde_json::json!({"ordinary_budget_sat":60_000,"urgent_reserve_sat":40_000}));
	let inventory = owning_tip(ctx, srv).await;
	assert!(inventory.reserved_outpoints.is_empty());
	let actual = inventory.utxos.iter().map(|u| u.outpoint.clone()).collect::<BTreeSet<_>>();
	assert_eq!(actual, funded);
	assert_eq!(inventory.utxos.len(), 5);
	assert!(inventory.utxos.iter().all(|u| u.value_sat == 10_000 && u.confirmed_fee_eligible && !u.reserved));
}

async fn owning_tip(ctx: &TestContext, srv: &Captaind) -> protos::WalletInventory {
	let core = ctx.bitcoind().sync_client();
	let height = core.get_block_count().unwrap();
	let tip = core.get_best_block_hash().unwrap().to_string();
	let mut rpc = server_rpc::admin::WalletAdminServiceClient::connect(
		format!("http://{}", srv.watchmand().config().admin_address.unwrap()),
	).await.unwrap();
	let inventory = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let inventory = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().watchman.unwrap();
			if inventory.complete && u64::from(inventory.tip_height) == height && inventory.tip_hash == tip {
				break inventory;
			}
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("owning fee inventory must match the independent Core tip");
	assert_eq!(inventory.network, "regtest");
	assert_eq!(inventory.min_trusted_confirmations, 3);
	assert!(inventory.utxos.iter().filter(|u| u.confirmed_fee_eligible).map(|u| u.value_sat).sum::<u64>() >= 40_000,
		"confirmed unselected urgent floor survives ordinary publication");
	inventory
}

pub(super) async fn verify_retained(
	wallet: &bark::Wallet, db: &Db, coins: &[Vtxo], record: &ark::fallback::FallbackRecord,
) -> BTreeMap<String, Vtxo<Full>> {
	let mailbox = wallet.mailbox_keypair().public_key();
	record.verify(Network::Regtest, mailbox).unwrap();
	let mut ids = wallet.all_vtxos().await.unwrap().into_iter().map(|v| v.vtxo.id()).collect::<BTreeSet<_>>();
	ids.extend(coins.iter().map(|v| v.id()));
	let mut proofs = BTreeMap::new();
	for id in ids {
		let full = wallet.get_full_vtxo(id).await.unwrap();
		let link = wallet.fallback_key_link(full.user_pubkey()).await.unwrap();
		link.verify(Network::Regtest, mailbox, record).unwrap();
		let key = full.user_pubkey().serialize().to_vec();
		let row = db.read(async |t| Ok(t.query_one(
			"SELECT l.mailbox_pk,l.sig,l.recovery_index,r.spk,r.mailbox_sig FROM key_link l JOIN fallback_record r ON r.mailbox_pk=l.mailbox_pk WHERE l.user_pubkey=$1", &[&key],
		).await?)).await.unwrap();
		assert_eq!(row.get::<_, Vec<u8>>(0), mailbox.serialize());
		assert_eq!(row.get::<_, Vec<u8>>(1), link.signature.serialize());
		assert_eq!(row.get::<_, Option<i32>>(2), link.recovery_index.map(|i| i as i32));
		assert_eq!(row.get::<_, Vec<u8>>(3), record.spk.as_bytes());
		assert_eq!(row.get::<_, Vec<u8>>(4), record.mailbox_signature.serialize());
		assert!(proofs.insert(id.to_string(), full).is_none());
	}
	proofs
}

pub(super) async fn settle(
	ctx: &TestContext, db: &Db, srv: &Captaind, coins: &[Vtxo], destination: &ScriptBuf,
	proofs: &BTreeMap<String, Vtxo<Full>>,
) -> Vec<OutPoint> {
	let core = ctx.bitcoind().sync_client();
	let history = fee_history(srv);
	let policy_before = std::fs::read(history.join("ordinary-fee-policy.json")).unwrap();
	let ledger_before = std::fs::read_to_string(history.join("ordinary-fee-budget")).unwrap();
	assert!(ledger_before.is_empty());
	let expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 6).await;
	train_fee_estimator(ctx).await;
	srv.stop().await.unwrap();
	{
		let mut c = srv.config_mut();
		assert!(!c.expiry_payout.enabled && c.direct_expiry.enabled);
		c.direct_expiry.interval = Duration::from_millis(100);
	}
	srv.watchmand().stop().await.unwrap();
	{
		let mut c = srv.watchmand().config_mut();
		c.watchman.sweep_interval = Duration::from_millis(100);
		c.watchman.reaction_interval = Duration::from_millis(100);
	}
	srv.watchmand().start().await.unwrap();
	srv.start().await.unwrap();
	let ids = coins.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	tokio::time::timeout(Duration::from_secs(180), async {
		loop {
			let count = db.read(async |t| Ok(t.query_one(
				"SELECT count(DISTINCT a.id) FROM expiry_allocation a JOIN expiry_carrier_allocation m ON m.allocation_id=a.id JOIN expiry_carrier c ON c.txid=m.txid WHERE a.id=ANY($1) AND a.disposition='pay' AND c.confirmed_height IS NOT NULL", &[&ids],
			).await?.get::<_, i64>(0))).await.unwrap();
			if count as usize == ids.len() { break; }
			ctx.generate_blocks(1).await;
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
	}).await.expect("real automatic direct source must reach confirmed payouts");
	let inventory = owning_tip(ctx, srv).await;
	let mut fee_change = inventory.utxos.iter().map(|u| u.outpoint.parse::<OutPoint>().unwrap()).collect::<BTreeSet<_>>();
	let operator_script = srv.watchmand().config().sweep_address.clone().unwrap().require_network(Network::Regtest).unwrap().script_pubkey();
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*) FROM expiry_settlement", &[]).await?.get::<_, i64>(0))).await.unwrap(), 0,
		"the legacy two-step settlement route never ran");
	let rows = db.read(async |t| Ok(t.query(
		"SELECT a.id,a.gross_sat,a.fee_sat,a.payout_sat,a.retirement_ids,m.txid,m.vout,c.backing_ids,c.miner_fee_sat,c.signed_tx,c.fee_inputs::TEXT,c.funding_subsidy_sat FROM expiry_allocation a JOIN expiry_carrier_allocation m ON m.allocation_id=a.id JOIN expiry_carrier c ON c.txid=m.txid WHERE a.spk=$1 AND a.disposition='pay' AND c.confirmed_height IS NOT NULL ORDER BY a.id", &[&destination.as_bytes()],
	).await?)).await.unwrap();
	let mut seen = BTreeSet::new();
	let mut outputs = BTreeMap::<OutPoint, (u64, u64, u64)>::new();
	let mut transactions = BTreeMap::<Txid, Transaction>::new();
	let mut funding = BTreeMap::<Txid, (u64, u64)>::new();
	for row in rows {
		let id = row.get::<_, String>(0);
		assert!(seen.insert(id.clone()), "exactly one confirmed canonical carrier per entitlement");
		let full = proofs.get(&id).expect("every recipient allocation belongs to an independently retained signed source");
		let funding_tx = core.get_raw_transaction(&full.chain_anchor().txid, None).unwrap();
		full.validate(&funding_tx).unwrap();
		let gross = u64::try_from(row.get::<_, i64>(1)).unwrap();
		let flat = u64::try_from(row.get::<_, i64>(2)).unwrap();
		let paid = u64::try_from(row.get::<_, i64>(3)).unwrap();
		assert_eq!(gross, full.amount().to_sat());
		assert_eq!(gross, flat.checked_add(paid).unwrap());
		let txid = row.get::<_, String>(5).parse::<Txid>().unwrap();
		let tx = core.get_raw_transaction(&txid, None).unwrap();
		assert_eq!(bitcoin::consensus::serialize(&tx), row.get::<_, Vec<u8>>(9));
		assert!(core.get_raw_transaction_info(&txid, None).unwrap().confirmations.unwrap_or(0) > 0);
		let backing = row.get::<_, Vec<String>>(7);
		let retirement = row.get::<_, Vec<String>>(4);
		assert!(!retirement.is_empty() && retirement.iter().all(|a| backing.contains(a)));
		let path = std::iter::once(full.chain_anchor().to_string()).chain(full.transactions().map(|v|
			OutPoint::new(v.tx.compute_txid(), v.output_idx as u32).to_string())).collect::<BTreeSet<_>>();
		assert!(retirement.iter().any(|a| path.contains(a)));
		for anchor in backing {
			let anchor = anchor.parse::<OutPoint>().unwrap();
			assert!(tx.input.iter().any(|i| i.previous_output == anchor));
			assert!(core.get_tx_out(&anchor.txid, anchor.vout, Some(true)).unwrap().is_none());
		}
		let input_sat = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		let miner = input_sat.checked_sub(tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>()).unwrap();
		assert_eq!(miner, u64::try_from(row.get::<_, i64>(8)).unwrap());
		assert!(miner > 0 && miner <= 10_000);
		assert!(tx.weight().to_wu() <= 400_000);
		assert!(sat(miner) <= FeeRate::from_sat_per_vb(50).unwrap().fee_wu(tx.weight()).unwrap());
		let fee_inputs: serde_json::Value = serde_json::from_str(&row.get::<_, String>(10)).unwrap();
		let mut fee_total = 0u64;
		let mut fee_points = BTreeSet::new();
		for item in fee_inputs.as_array().unwrap() {
			let point = item["outpoint"].as_str().unwrap().parse::<OutPoint>().unwrap();
			assert!(fee_points.insert(point));
			assert!(tx.input.iter().any(|i| i.previous_output == point));
			let prev = core.get_raw_transaction(&point.txid, None).unwrap();
			let prev = &prev.output[point.vout as usize];
			assert_eq!(item["value_sat"].as_u64(), Some(prev.value.to_sat()));
			assert_eq!(item["script_pubkey"].as_str(), Some(prev.script_pubkey.to_hex_string().as_str()));
			fee_total = fee_total.checked_add(prev.value.to_sat()).unwrap();
			fee_change.insert(point); // Also recognizes an earlier actual fee change later consumed.
		}
		funding.insert(txid, (fee_total, u64::try_from(row.get::<_, i64>(11)).unwrap()));
		let point = OutPoint::new(txid, u32::try_from(row.get::<_, Option<i32>>(6).unwrap()).unwrap());
		assert_eq!(tx.output[point.vout as usize].script_pubkey, *destination);
		let sums = outputs.entry(point).or_default();
		sums.0 += gross; sums.1 += flat; sums.2 += paid;
		transactions.insert(txid, tx);
	}
	assert!(ids.iter().all(|id| seen.contains(id)), "every original real Lightning entitlement is retired");
	for (point, (gross, flat, paid)) in &outputs {
		assert_eq!(*flat, 1_000, "one flat fee per actual canonical destination output");
		assert_eq!(*gross, paid.checked_add(*flat).unwrap());
		assert_eq!(core.get_raw_transaction(&point.txid, None).unwrap().output[point.vout as usize].value.to_sat(), *paid);
	}
	assert!(!outputs.is_empty());
	for (txid, tx) in &transactions {
		let mut change = 0u64;
		for (vout, output) in tx.output.iter().enumerate() {
			let point = OutPoint::new(*txid, vout as u32);
			assert!(!(outputs.contains_key(&point) && fee_change.contains(&point)), "customer and fee change roles are distinct");
			if fee_change.contains(&point) { change = change.checked_add(output.value.to_sat()).unwrap(); }
			if !outputs.contains_key(&point) && !fee_change.contains(&point) {
				assert_eq!(output.script_pubkey, operator_script,
					"every remaining output must be the configured operator destination");
			}
		}
		let (fee_input, subsidy) = funding[txid];
		assert_eq!(fee_input.checked_sub(change), Some(subsidy), "actual fee-wallet debit equals persisted subsidy");
	}
	let ledger_after = std::fs::read_to_string(history.join("ordinary-fee-budget")).unwrap();
	let charges = ledger_after.lines().map(|n| n.parse::<u64>().unwrap()).collect::<Vec<_>>();
	assert!(!charges.is_empty() && charges.len() <= 6);
	assert!(charges.iter().all(|n| *n == 10_000));
	assert!(charges.iter().sum::<u64>() <= 60_000);
	assert_eq!(std::fs::read(history.join("ordinary-fee-policy.json")).unwrap(), policy_before);
	let receipt = serde_json::json!({"source":"actual automatic direct external-Lightning source",
		"network":"regtest","source_ids":ids,"confirmed_entitlements":seen,
		"payouts":outputs.iter().map(|(p,(gross,flat,paid))|serde_json::json!({"outpoint":p.to_string(),"gross_sat":gross,"flat_fee_sat":flat,"paid_sat":paid})).collect::<Vec<_>>(),
		"confirmed_transactions":transactions.keys().map(ToString::to_string).collect::<Vec<_>>(),
		"full_charge_entries_sat":charges,"ledger_before":ledger_before,"ledger_after":ledger_after,
		"ordinary_budget_sat":60_000,"urgent_reserve_sat":40_000,"failed_build_attempts":serde_json::Value::Null,
		"ledger_reset":false,"qualified":false,"seed_spend_pending_at_this_frame":true});
	std::fs::write(ctx.datadir.join("direct-lightning-source-receipt.json"), serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
	outputs.into_keys().collect()
}
