//! Owning graph facts preserve confirmed-tip inputs without counting change twice.

use std::time::Duration;

use bdk_wallet::coin_selection::DefaultCoinSelectionAlgorithm;
use bitcoin::hex::FromHex;

use super::*;

async fn cli_inventory(srv: &ark_testing::Captaind) -> serde_json::Value {
	let out = Command::new(std::env::var("CAPTAIND_EXEC").unwrap())
		.args(["rpc", "--addr", &srv.config().rpc.admin_address.unwrap().to_string(), "wallet-inventory"])
		.output().await.unwrap();
	assert!(out.status.success(), "actual owning CLI must return a bounded inventory");
	assert!(out.stdout.len() < 2 * 1024 * 1024);
	serde_json::from_slice(&out.stdout).unwrap()
}

fn decoded_body(row: &serde_json::Value) -> bitcoin::Transaction {
	assert_eq!(row["encoding"], "bitcoin-stripped-body-v1");
	let body = Vec::<u8>::from_hex(row["transaction_body"].as_str().unwrap()).unwrap();
	let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&body).unwrap();
	assert!(tx.input.iter().all(|i| i.witness.is_empty() && i.script_sig.is_empty()));
	assert_eq!(tx.compute_txid().to_string(), row["txid"].as_str().unwrap());
	assert_eq!(bitcoin::consensus::serialize(&tx), body, "exact stripped Bitcoin body, no altered inputs");
	tx
}

#[tokio::test]
async fn owning_wallet_pending_spend_preserves_confirmed_tip_backing_and_read_only_state() {
	let ctx = TestContext::new("wallet-pending-inventory/owning_wallet_pending_spend_preserves_confirmed_tip_backing_and_read_only_state").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(sat(1_000_000))
		.cfg(|c| c.round_interval = Duration::from_secs(3600)).watchmand_cfg(|c| {
			c.watchman.sweep_interval = Duration::from_secs(3600);
			c.watchman.reaction_interval = Duration::from_secs(3600);
		}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let initial = cli_inventory(&srv).await;
	let source = &initial["rounds"];
	assert_eq!(source["utxos"].as_array().unwrap().len(), 1);
	let original = OutPoint::from_str(source["utxos"][0]["outpoint"].as_str().unwrap()).unwrap();
	assert_eq!(source["utxos"][0]["value_sat"], 1_000_000);
	assert_eq!(source["utxos"][0]["confirmed_fee_eligible"], true);
	let core = ctx.bitcoind().sync_client();
	let funding = core.get_raw_transaction(&original.txid, None).unwrap();
	assert!(funding.input.iter().all(|i| i.script_sig.is_empty()), "native positive needs a safely exportable retained parent");
	let mnemonic = wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let height = core.get_block_count().unwrap();
	let tip = BlockRef { height: BlockHeight::new(height as u32), hash: core.get_block_hash(height).unwrap() };
	// Stop the sole rounds owner before loading its actual persisted graph.
	srv.stop().await.unwrap();
	let loaded = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Rounds, tip, source["min_trusted_confirmations"].as_u64().unwrap() as u32).await.unwrap();
	let owning = InstrumentedLock::new("native-pending-inventory", loaded);
	let mut held = owning.lock_owned().await;
	let recipient = ctx.bitcoind().get_new_address().script_pubkey();
	let psbt = held.build_tx_at_chunk_feerate(DefaultCoinSelectionAlgorithm::default(), FeeRate::from_sat_per_vb(10).unwrap(), |b| {
		b.add_recipient(recipient.clone(), sat(300_000)); Ok(())
	}).unwrap();
	let spend = held.finish_tx_with_max_fee_rate(psbt, Some(FeeRate::from_sat_per_vb(50).unwrap())).unwrap();
	assert_eq!(spend.input.len(), 1); assert_eq!(spend.input[0].previous_output, original);
	assert!(!spend.input[0].witness.is_empty());
	held.commit_tx(&spend); held.persist().await.unwrap();
	core.send_raw_transaction(&spend).unwrap();
	assert!(core.get_raw_mempool().unwrap().contains(&spend.compute_txid()));
	let change_vout = spend.output.iter().position(|o| held.is_mine(o.script_pubkey.clone())).unwrap();
	let change = OutPoint::new(spend.compute_txid(), change_vout as u32);
	let confirmed_original = core.get_tx_out(&original.txid, original.vout, Some(false)).unwrap().unwrap();
	assert_eq!(confirmed_original.value, sat(1_000_000));
	assert!(core.get_tx_out(&original.txid, original.vout, Some(true)).unwrap().is_none(), "mempool-spent backing is not fee cash");
	assert!(core.get_tx_out(&change.txid, change.vout, Some(false)).unwrap().is_none(), "unconfirmed change is not another confirmed asset");
	assert!(core.get_tx_out(&change.txid, change.vout, Some(true)).unwrap().is_some());
	let reservation = held.lock_wallet_utxo(original).unwrap();
	let stage = held.staged().cloned(); let index = held.derivation_index(KEYCHAIN);
	let fingerprint = wallet_changeset_fingerprint(&db, WalletKind::Rounds).await;
	let first = held.inventory().unwrap();
	assert!(!first.utxos.iter().any(|u| u.outpoint == original.to_string()), "canonical list_unspent really omits the confirmed input");
	assert_eq!(first.reserved_outpoints, vec![original.to_string()]);
	assert!(first.utxos.iter().all(|u| !u.confirmed_fee_eligible));
	for _ in 0..3 { assert_eq!(held.inventory().unwrap(), first); }
	assert!(held.staged() == stage.as_ref()); assert_eq!(held.derivation_index(KEYCHAIN), index);
	assert_eq!(wallet_changeset_fingerprint(&db, WalletKind::Rounds).await, fingerprint);
	assert!(held.lock_wallet_utxo(original).is_err(), "reads must preserve the actual live reservation");
	drop(reservation); drop(held); drop(owning);
	srv.start().await.unwrap();
	let public = cli_inventory(&srv).await;
	let source = &public["rounds"];
	assert!(!source["utxos"].as_array().unwrap().iter().any(|u| u["outpoint"] == original.to_string()));
	assert_eq!(source["utxos"].as_array().unwrap().iter().filter(|u| !u["confirmed_height_upper_bound"].is_null())
		.map(|u| u["value_sat"].as_u64().unwrap()).sum::<u64>(), 0);
	std::fs::write(ctx.datadir.join("pending-wallet-source.json"), serde_json::to_vec_pretty(&public).unwrap()).unwrap();
	let pending = source["pending_transactions"].as_array()
		.expect("missing pending_transactions: current inventory discarded still-confirmed wallet backing");
	assert_eq!(source["pending_transaction_scope"], "canonical_wallet_graph_only");
	assert_eq!(source["complete"], true);
	assert_eq!(pending.len(), 1);
	assert_eq!(decoded_body(&pending[0]).compute_txid(), spend.compute_txid());
	assert_eq!(pending[0]["signed_weight_wu"], spend.weight().to_wu());
	let parents = source["pending_parents"].as_array().unwrap();
	assert_eq!(parents.len(), 1); assert_eq!(decoded_body(&parents[0]).compute_txid(), original.txid);
	assert_eq!(parents[0]["signed_weight_wu"], funding.weight().to_wu());
	assert_eq!(source["unavailable_parent_txids"].as_array().unwrap().len(), 0);
	let parent = decoded_body(&parents[0]);
	assert_eq!(parent.output[original.vout as usize].value, confirmed_original.value);
	assert_eq!(parent.output[original.vout as usize].script_pubkey, funding.output[original.vout as usize].script_pubkey);
	let descriptor: serde_json::Value = core.call("getdescriptorinfo", &[serde_json::json!(source["public_descriptor"])]).unwrap();
	assert_eq!(descriptor["hasprivatekeys"], false);
	let index = initial["rounds"]["utxos"][0]["derivation_index"].as_u64().unwrap();
	let addresses: Vec<String> = core.call("deriveaddresses", &[source["public_descriptor"].clone(), serde_json::json!([index, index])]).unwrap();
	let address = bitcoin::Address::from_str(&addresses[0]).unwrap().require_network(Network::Regtest).unwrap();
	assert_eq!(address.script_pubkey(), parent.output[original.vout as usize].script_pubkey);
	assert_eq!(confirmed_original.value, sat(1_000_000), "confirmed-tip projection restores the input once, never adds unconfirmed change");
	ctx.generate_blocks(1).await;
	srv.wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let after = cli_inventory(&srv).await;
	assert_eq!(after["rounds"]["pending_transactions"].as_array().unwrap().len(), 0);
	assert_eq!(after["rounds"]["pending_parents"].as_array().unwrap().len(), 0);
	assert!(core.get_tx_out(&original.txid, original.vout, Some(false)).unwrap().is_none());
	let now = core.get_tx_out(&change.txid, change.vout, Some(false)).unwrap().unwrap();
	let realized_fee = confirmed_original.value - spend.output.iter().map(|o| o.value).sum::<Amount>();
	assert_eq!(now.value + sat(300_000) + realized_fee, confirmed_original.value);
	std::fs::write(ctx.datadir.join("confirmed-wallet-source.json"), serde_json::to_vec_pretty(&after).unwrap()).unwrap();
}
