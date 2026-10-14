//! Actual in-process captaind freeze against current Core backing.

use super::*;

async fn board(name: &str) -> (TestContext, std::sync::Arc<server::Server>, ark::Vtxo, ScriptBuf) {
	let ctx = TestContext::new(format!("cap-freeze/{name}")).await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(ark_testing::btc(1))
		.cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fees.board.base_fee = sat(1_000);
			c.expiry_payout.grace_blocks = 0;
		}).create().await;
	let client = ctx.bark_sdk("absent", &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(21_000)).create().await;
	client.stop_daemon_wait().await.unwrap();
	let coin = client.get_full_vtxo(client.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let fallback = client.fallback_destination().await.unwrap().spk;
	drop(client);
	let config = srv.config().clone();
	srv.stop().await.unwrap();
	let server = server::Server::start(config).await.unwrap();
	(ctx, server, coin, fallback)
}

async fn freeze(server: &server::Server, coin: &ark::Vtxo)
	-> anyhow::Result<server::database::expiry_freeze::FrozenExpiryPlan>
{
	server.freeze_current_expiry_backing(&[coin.chain_anchor().into()], &[coin.id()],
		BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 },
	).await
}

#[tokio::test]
async fn cap_freeze_actual_board_uses_current_node_tip_and_canonical_handoff() {
	let (ctx, server, coin, fallback) = board("cap_freeze_actual_board_uses_current_node_tip_and_canonical_handoff").await;
	assert!(freeze(&server, &coin).await.is_err(), "actual immature source must not freeze");
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let plan = freeze(&server, &coin).await.unwrap();
	assert_eq!(plan.backing_ids, vec![coin.chain_anchor().into()]);
	assert!(plan.allocation_ids.contains(&coin.id()));
	assert_eq!(plan.recipients.len(), 1);
	assert_eq!(plan.recipients[0].value, sat(19_000));
	assert_eq!(plan.recipients[0].script_pubkey, fallback);
	let rows = server.database().read(async |t| t.query("SELECT id,source,disposition,gross_sat,fee_sat,payout_sat FROM expiry_allocation WHERE freeze_set=$1 ORDER BY id", &[&plan.id]).await.map_err(Into::into)).await.unwrap();
	assert_eq!(rows.len(), 2, "complete actual Board plus transition fee anchor");
	let customer = rows.iter().find(|r| r.get::<_, &str>("id") == coin.id().to_string()).unwrap();
	assert_eq!(customer.get::<_, &str>("source"), "registered");
	assert_eq!(customer.get::<_, &str>("disposition"), "pay");
	assert_eq!(customer.get::<_, i64>("gross_sat"), 20_000);
	assert_eq!(customer.get::<_, i64>("fee_sat"), 1_000);
	assert_eq!(customer.get::<_, i64>("payout_sat"), 19_000);
	assert_eq!(freeze(&server, &coin).await.unwrap().id, plan.id, "unchanged complete handoff is canonical");
	let carriers: i64 = server.database().read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n FROM expiry_carrier", &[]).await?.get("n"))).await.unwrap();
	assert_eq!(carriers, 0, "freeze must not fabricate delivery or payment");
}

#[tokio::test]
async fn cap_freeze_actual_mempool_spent_backing_refuses_without_allocation() {
	let (ctx, server, coin, _) = board("cap_freeze_actual_mempool_spent_backing_refuses_without_allocation").await;
	let core = ctx.bitcoind().sync_client();
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let parent = coin.transactions().last().unwrap().tx;
	let anchor = parent.output.iter().position(|o| o.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT).unwrap();
	let child = bitcoin::Transaction {
		version: parent.version, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: OutPoint::new(parent.compute_txid(), anchor as u32),
			script_sig: ScriptBuf::new(), sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
			witness: bitcoin::Witness::new() }],
		output: vec![TxOut { value: sat(330), script_pubkey: ctx.bitcoind().get_new_address().script_pubkey() }],
	};
	assert_eq!(core.submit_package(&[&parent, &child]).unwrap().package_msg, "success");
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(false)).unwrap().is_some());
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(true)).unwrap().is_none());
	let refused = freeze(&server, &coin).await.err().expect("actual mempool consumption must refuse a fresh freeze");
	assert!(format!("{refused:#}").contains("backing is spent or unavailable"), "{refused:#}");
	let n: i64 = server.database().read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n FROM expiry_allocation", &[]).await?.get("n"))).await.unwrap();
	assert_eq!(n, 0);
}
