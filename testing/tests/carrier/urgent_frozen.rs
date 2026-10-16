//! Fresh ordinary Board/Arkoor freeze: physical-ancestor refusal, no exit.
use super::*;
use bitcoin::hashes::{sha256, Hash};

#[tokio::test]
async fn frozen_ordinary_board_ancestor_refuses_urgent_source_without_money_change() {
	Box::pin(frozen_ancestor()).await;
}

async fn frozen_ancestor() {
	let ctx = Box::pin(TestContext::new("carrier/frozen_ordinary_board_ancestor")).await;
	let core = ctx.bitcoind().sync_client();
	let baseline = core.get_block_count().unwrap();
	let srv = Box::pin(ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|c| {
		c.vtxo_lifetime = BlockDelta::new(128);
		c.fees.board.base_fee = sat(1_000);
		c.direct_expiry.enabled = false;
	}).create()).await;
	let sender = Box::pin(ctx.bark_sdk("fresh-sender", &srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(100_000)).boarded(sat(21_000)).create()).await;
	let recipient = Box::pin(ctx.bark_sdk("fresh-recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create()).await;
	let mut funding = Vec::new();
	for height in baseline..=core.get_block_count().unwrap() {
		for tx in core.get_block(&core.get_block_hash(height).unwrap()).unwrap().txdata {
			if !tx.is_coinbase() && tx.output.iter().any(|o|o.value == sat(100_000)) {
				funding.push(("fresh-sender".to_owned(), tx.compute_txid(), 100_000));
			}
		}
	}
	assert_eq!(funding.len(), 1, "exact explicit SDK funding, no automatic fee buffer");
	let ledger = srv.config().data_dir.join("FROZEN-ORDINARY-PUBLIC-CHAIN-LEDGER.json");
	let fees = super::urgent_progress::chain_receipt_bounded(&core, baseline, &funding, &ledger, 2048);
	assert!(fees <= 10_000);
	let _keep_chain = super::urgent_progress::KeepChain { core: &core, baseline, funding: &funding, path: ledger.clone(), max_blocks: 2048 };
	let original = sender.get_full_vtxo(sender.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let sender_fallback = sender.fallback_destination().await.unwrap().spk;
	let recipient_fallback = recipient.fallback_destination().await.unwrap().spk;
	sender.send_arkoor_payment(&recipient.new_address().await.unwrap(), sat(5_000)).await.unwrap();
	sender.sync().await; recipient.sync().await;
	let mut terminals = sender.spendable_vtxos().await.unwrap().into_iter().map(|v|v.id()).collect::<Vec<_>>();
	terminals.extend(recipient.spendable_vtxos().await.unwrap().into_iter().map(|v|v.id()));
	assert_eq!(terminals.len(), 3, "retain both actual upstream change leaves and the recipient");
	assert!(!terminals.contains(&original.id()));
	let mut expected = Vec::new();
	for (wallet, fallback) in [(&sender, sender_fallback), (&recipient, recipient_fallback)] {
		let coins = wallet.spendable_vtxos().await.unwrap();
		let mut gross = Amount::ZERO;
		for coin in coins {
			let full = wallet.get_full_vtxo(coin.id()).await.unwrap();
			assert_eq!(full.chain_anchor(), original.chain_anchor());
			gross += full.amount();
		}
		// Existing policy charges once per actual canonical fallback output,
		// including when this owner has multiple actual change leaves.
		expected.push(TxOut { value: gross - sat(1_000), script_pubkey: fallback });
	}
	assert_eq!(expected.iter().map(|o|o.value + sat(1_000)).sum::<Amount>(), sat(20_000));
	sender.stop_daemon_wait().await.unwrap(); recipient.stop_daemon_wait().await.unwrap();
	drop(sender); drop(recipient);
	let config = srv.config().clone();
	let grace = config.expiry_payout.grace_blocks;
	let target = original.expiry_height().to_u32() + grace + 3;
	assert!(u64::from(target).saturating_sub(baseline) <= 2048, "actual configured expiry exceeds this finite capture");
	ctx.generate_blocks(target.saturating_sub(core.get_block_count().unwrap() as u32)).await;
	srv.stop().await.unwrap();
	let server = server::Server::start(config.clone()).await.unwrap();
	let plan = server.freeze_current_expiry_backing(&[original.chain_anchor().into()], &terminals,
		BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 }).await.unwrap();
	let mut actual = plan.recipients.clone();
	actual.sort_by_key(|o|(o.script_pubkey.clone(), o.value));
	expected.sort_by_key(|o|(o.script_pubkey.clone(), o.value));
	assert_eq!(actual, expected, "wrong canonical recipients, multiplicity or payout amounts");
	assert!(!plan.allocation_ids.contains(&original.id()), "probe must be a retirement/physical ancestor, not a pay allocation");
	let config_path = config.data_dir.join("FROZEN-GUARD-PG.toml");
	let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&config_path).unwrap();
	f.write_all(toml::to_string(&config.postgres).unwrap().as_bytes()).unwrap(); f.sync_all().unwrap();
	let result_path = config.data_dir.join("FROZEN-GUARD-RESULT.json");
	let status = Command::new(std::env::var_os("ARK_TEST_FROZEN_GUARD_ELF").expect("preserved actual in-crate guard ELF"))
		.arg("--exact").arg("database::urgent_publication::tests::urgent_pg_actual_frozen_ancestor_refuses_without_money_change")
		.arg("--ignored").arg("--nocapture")
		.env("ARK_TEST_FROZEN_GUARD_PG", &config_path).env("ARK_TEST_FROZEN_GUARD_ID", original.id().to_string())
		.env("ARK_TEST_FROZEN_GUARD_ANCHOR", original.chain_anchor().to_string()).env("ARK_TEST_FROZEN_GUARD_RESULT", &result_path)
		.status().await.unwrap();
	assert!(status.success(), "actual in-crate frozen-ancestor guard failed");
	let result: serde_json::Value = serde_json::from_slice(&std::fs::read(&result_path).unwrap()).unwrap();
	assert_eq!(result["probe_id"], original.id().to_string());
	assert_eq!(result["money_unchanged"], true);
	let history = config.data_dir.join("direct-expiry-witness/ordinary-fee-budget");
	assert!(std::fs::read(history).unwrap().is_empty(), "refusal charged a fee or signed an urgent publication");
	assert_eq!(super::urgent_progress::chain_receipt_bounded(&core, baseline, &funding, &ledger, 2048), fees);
	println!("actual mature ordinary freeze; canonical owners preserved; authenticated non-pay ancestor refused for frozen physical/retirement overlap; no money change; originalFull={}", sha256::Hash::hash(&original.serialize()));
}
