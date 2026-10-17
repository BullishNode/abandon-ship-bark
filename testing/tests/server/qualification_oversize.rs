//! Real-size native splitting. Run explicitly with the fallback-scale profile.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use ark::arkoor::{ArkoorDestination, package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse}};
use ark::fallback::FallbackRecord;
use ark::mailbox::MailboxIdentifier;
use ark::vtxo::Full;
use ark::{ProtocolEncoding, ServerVtxoPolicy, Vtxo, SECP};
use ark_testing::{sat, Captaind, TestContext};
use ark_testing::daemon::captaind::ArkClient;
use bitcoin::{Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid, WPubkeyHash};
use bitcoin::bip32::{ChildNumber, Xpriv};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::{Keypair, rand::{thread_rng, RngCore}};
use bitcoin_ext::{BlockDelta, TxOutExt};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use serde_json::{json, Value};
use server::database::Db;
use server_rpc::protos;

const PAYEES: usize = 327;
const LEAF_SAT: u64 = 5_000;

struct Owner {
	key: Keypair,
	mailbox: MailboxIdentifier,
	record: FallbackRecord,
}

struct Node {
	input: OutPoint,
	checkpoint: OutPoint,
	checkpoint_tx: Transaction,
	fanout_tx: Transaction,
	payees: usize,
}

fn owner() -> Owner {
	let mut seed = [0; 64];
	thread_rng().fill_bytes(&mut seed);
	let account = Xpriv::new_master(Network::Regtest, &seed).unwrap();
	let key = account.derive_priv(&SECP, &[ChildNumber::from_normal_idx(1).unwrap()]).unwrap().to_keypair(&SECP);
	let mailbox = account.derive_priv(&SECP, &[ChildNumber::from_hardened_idx(1).unwrap()]).unwrap().to_keypair(&SECP);
	thread_rng().fill_bytes(&mut seed);
	let payout = Xpriv::new_master(Network::Regtest, &seed).unwrap().to_keypair(&SECP);
	let script = ScriptBuf::new_p2wpkh(&WPubkeyHash::hash(&payout.public_key().serialize()));
	let record = FallbackRecord::new(Network::Regtest, script, &account, &mailbox).unwrap();
	Owner { key, mailbox: MailboxIdentifier::from_pubkey(mailbox.public_key()), record }
}

async fn save(ctx: &TestContext, report: &Value) {
	tokio::fs::write(ctx.datadir.join("automatic-oversize.json"), serde_json::to_vec_pretty(report).unwrap())
		.await.unwrap();
}

async fn native_node(
	client: &bark::Wallet, rpc: &mut ArkClient, input: Vtxo<Full>, key: Keypair,
	destinations: Vec<ArkoorDestination>, payees: usize,
) -> (Vec<Vtxo<Full>>, Node) {
	assert!(input.exit_depth() < 100);
	assert!(!destinations.is_empty() && destinations.len() <= 4);
	let input_point = input.point();
	let expected = destinations.iter().map(|d| (d.policy.clone(), d.total_amount)).collect::<Vec<_>>();
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input], destinations).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let response = rpc.request_arkoor_cosign(protos::ArkoorPackageCosignRequest::from(builder.cosign_request()))
		.await.unwrap().into_inner();
	let signed = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap();
	let checkpoints = signed.build_signed_internal_vtxos().into_iter()
		.filter(|(v, _)| matches!(v.policy(), ServerVtxoPolicy::Checkpoint(_))).collect::<Vec<_>>();
	assert_eq!(checkpoints.len(), 1, "non-dust checkpointed fanout has one actual checkpoint");
	let checkpoint = checkpoints[0].0.point();
	let transactions = signed.signed_virtual_transactions();
	assert_eq!(transactions.len(), 2);
	let checkpoint_tx = transactions.iter().find(|tx| tx.input[0].previous_output == input_point).unwrap().clone();
	let fanout_tx = transactions.iter().find(|tx| tx.input[0].previous_output == checkpoint).unwrap().clone();
	let outputs = signed.build_signed_vtxos();
	assert_eq!(outputs.len(), expected.len());
	for (vtxo, (policy, amount)) in outputs.iter().zip(expected) {
		assert_eq!(vtxo.policy(), &policy);
		assert_eq!(vtxo.amount(), amount);
		assert!(vtxo.exit_depth() < 100);
	}
	client.register_vtxo_transactions_with_server(&outputs).await.unwrap();
	(outputs, Node { input: input_point, checkpoint, checkpoint_tx, fanout_tx, payees })
}

async fn actual_fee(ctx: &TestContext, tx: &Transaction) -> u64 {
	let mut inputs = 0;
	for input in &tx.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		inputs += previous.output[input.previous_output.vout as usize].value.to_sat();
	}
	inputs - tx.output.iter().map(|out| out.value.to_sat()).sum::<u64>()
}

/// Observe actual publication, then mine its package. A package already mined
/// alongside another observed level is checked through its canonical block.
async fn confirm_package(
	ctx: &TestContext, srv: &Captaind, parent: &Transaction,
	graph: &mut BTreeMap<Txid, Transaction>, report: &mut Value,
) {
	let rpc = ctx.bitcoind().sync_client();
	let txid = parent.compute_txid();
	loop {
		if let Some(info) = rpc.custom_get_raw_transaction_info(txid, None).unwrap() {
			if info.confirmations.unwrap_or(0) > 0 { break; }
			assert!(rpc.get_mempool_entry(&txid).is_ok(), "unconfirmed publication must really be in Core's mempool");
			let height = ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(height).await;
			srv.watchmand().wait_for_sync_height(height).await;
		} else {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}
	assert_eq!(ctx.bitcoind().await_transaction(txid).await, *parent);
	assert_eq!(parent.version.0, 3);
	assert!(parent.vsize() <= 10_000);
	assert_eq!(actual_fee(ctx, parent).await, 0);
	let info = rpc.custom_get_raw_transaction_info(txid, None).unwrap().unwrap();
	let hash = info.blockhash.unwrap();
	let height = rpc.get_block_header_info(&hash).unwrap().height;
	let block = rpc.get_block(&hash).unwrap();
	let anchors = parent.output.iter().enumerate().filter(|(_, out)| out.is_p2a_fee_anchor())
		.map(|(vout, _)| OutPoint::new(txid, vout as u32)).collect::<Vec<_>>();
	assert_eq!(anchors.len(), 1);
	let sponsors = block.txdata.iter().filter(|tx|
		tx.input.iter().any(|input| input.previous_output == anchors[0])
	).collect::<Vec<_>>();
	assert_eq!(sponsors.len(), 1, "the actual canonical package must have exactly one anchor spender");
	let sponsor = sponsors[0];
	assert_eq!(sponsor.version.0, 3);
	assert!(sponsor.vsize() <= 1_000, "actual TRUC child size must satisfy Core's production bound");
	let fee = actual_fee(ctx, sponsor).await;
	assert!(fee > 0);
	let child_info = rpc.custom_get_raw_transaction_info(sponsor.compute_txid(), None).unwrap().unwrap();
	assert_eq!(child_info.blockhash, Some(hash));
	assert!(child_info.confirmations.unwrap_or(0) > 0);
	for input in &parent.input {
		let previous = rpc.custom_get_raw_transaction_info(input.previous_output.txid, None).unwrap().unwrap();
		let previous_height = rpc.get_block_header_info(&previous.blockhash.unwrap()).unwrap().height;
		assert!(previous_height < height, "each new physical level must follow already confirmed backing");
	}
	graph.insert(txid, parent.clone());
	graph.insert(sponsor.compute_txid(), sponsor.clone());
	report["packages"].as_array_mut().unwrap().push(json!({
		"parent_txid": txid, "parent_vsize": parent.vsize(), "parent_weight_wu": parent.weight().to_wu(),
		"confirmed_height": height,
		"confirmed_hash": hash, "child_txid": sponsor.compute_txid(),
		"child_vsize": sponsor.vsize(), "child_weight_wu": sponsor.weight().to_wu(), "child_miner_fee_sat": fee,
	}));
	save(ctx, report).await;
}

#[tokio::test]
#[ignore = "327 native payees; declare a serial 600-second regtest slot and use fallback-scale profile"]
async fn expiry_automatic_real_limit_multilevel_split_pays_every_owner() {
	tokio::time::timeout(Duration::from_secs(600), automatic_multilevel_split()).await
		.expect("predeclared 600-second automatic real-limit split deadline exceeded; retain the partial receipt");
}

async fn automatic_multilevel_split() {
	let ctx = TestContext::new("server/expiry_automatic_real_limit_multilevel_split").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|cfg| {
		cfg.vtxo_lifetime = BlockDelta::new(4_320);
		cfg.vtxo_exit_delta = BlockDelta::new(144);
		cfg.max_vtxo_exit_depth = 100;
		cfg.max_arkoor_fanout = 4;
	}).watchmand().create().await;
	assert_eq!(srv.config().expiry_owner_scan_chunk_size, 10_000);
	assert_eq!(srv.config().expiry_owner_scan_workers, 2);
	assert_eq!(srv.watchmand().config().watchman.progress_grace_period, BlockDelta::new(2));
	let mut report = json!({
		"deadline_seconds": 600, "payees": PAYEES, "leaf_gross_sat": LEAF_SAT,
		"source_pin": option_env!("GIT_HASH"), "outcome": "RUNNING",
		"production_limits": {"payout_vsize": 10_000, "child_vsize": 1_000,
			"fanout": 4, "exit_depth": 100, "lifetime_blocks": 4_320, "exit_delta": 144,
			"owner_scan_chunk": 10_000, "owner_scan_workers": 2},
		"harness_cadence_seconds": {"sweep": srv.watchmand().config().watchman.sweep_interval.as_secs(),
			"reaction": srv.watchmand().config().watchman.reaction_interval.as_secs()},
		"production_timer_defaults_seconds": {"sweep": 7_200, "reaction": 600},
		"source_actors": 1, "source_funding_sat": 2_000_000, "watchman_funding_sat": 3_200_000,
		"total_reserved_funding_sat": 5_200_000, "payee_records_registered": 0,
		"completed_native_cosign_registration_pairs": 0, "mailbox_posts": 0,
		"expected_explicit_rpc_workload": {"payee_registration": 327, "native_cosign": 157,
			"native_output_registration": 157, "initial_source_path_registration": 1, "mailbox_post": 327},
		"packages": [], "payouts": [], "automatic_split_requests": [], "phase": "registration",
	});
	save(&ctx, &report).await;
	// Geometry is reproducible by owner index; private account and payout
	// material is freshly random and never included in the public receipt.
	let owners = (0..PAYEES).map(|_| owner()).collect::<Vec<_>>();
	assert_eq!(owners.iter().map(|o| o.record.spk.clone()).collect::<BTreeSet<_>>().len(), PAYEES);
	// This lower bound uses real consensus serialization, without any input,
	// witness, drain, anchor or transaction-header overhead.
	let minimum_outputs = owners[..324].iter().map(|o|
		serialize(&TxOut { value: sat(LEAF_SAT - 1_000), script_pubkey: o.record.spk.clone() }).len()
	).sum::<usize>();
	assert_eq!(minimum_outputs, 10_044);
	assert!(minimum_outputs > 10_000);
	report["324_payout_output_bytes_alone"] = json!(minimum_outputs);
	let mut rpc = srv.get_public_rpc().await;
	for (index, owner) in owners.iter().enumerate() {
		let bytes = owner.record.serialize();
		let reply = rpc.set_fallback(protos::SetFallbackRequest {
			mailbox_pk: owner.mailbox.as_pubkey().serialize().to_vec(), record: bytes.clone(),
		}).await.unwrap().into_inner();
		assert_eq!(reply.record, bytes);
		report["payee_records_registered"] = json!(index + 1);
		if (index + 1) % 32 == 0 { save(&ctx, &report).await; }
	}
	let wallet = ctx.bark("source", &srv).cfg(|cfg| cfg.daemon_manual_sync = true)
		.funded(sat(2_000_000)).create().await;
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(PAYEES as u64 * LEAF_SAT)).await;
	assert_eq!(ids.len(), 1);
	let client = wallet.client().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let registry_count = db.read(async |tx| Ok(tx.query_one(
		"SELECT COUNT(*) FROM fallback_record", &[],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(registry_count, PAYEES as i64 + 1, "the source wallet adds its own real fallback registration");
	report["actual_registry_count"] = json!(registry_count);
	let source = client.get_full_vtxo(ids[0]).await.unwrap();
	let source_key = client.get_vtxo_key(&source).await.unwrap();
	let board_exit = source.transactions().find(|step| step.tx.input[0].previous_output == source.chain_anchor())
		.expect("the genuine board supplies its signed first exit step").tx;
	client.register_vtxo_transactions_with_server([&source]).await.unwrap();
	let mut sponsor_funding = BTreeSet::new();
	let fee_address = srv.watchmand().wait_wallet_address().await;
	for _ in 0..16 {
		let txid = ctx.bitcoind().fund_addr(fee_address.clone(), sat(200_000)).await;
		let tx = ctx.bitcoind().await_transaction(txid).await;
		let outputs = tx.output.iter().enumerate().filter(|(_, out)| out.script_pubkey == fee_address.script_pubkey())
			.collect::<Vec<_>>();
		assert_eq!(outputs.len(), 1);
		assert_eq!(outputs[0].1.value, sat(200_000));
		sponsor_funding.insert(OutPoint::new(txid, outputs[0].0 as u32));
	}
	ctx.generate_blocks(2).await;
	report["watchman_funding_outpoints"] = json!(sponsor_funding.iter().map(ToString::to_string).collect::<Vec<_>>());
	report["phase"] = json!("native_fanout");
	save(&ctx, &report).await;

	let mut destinations = Vec::new();
	let mut jobs = VecDeque::new();
	let root_groups = [Vec::from_iter(0..324), vec![324], vec![325], vec![326]];
	let mut child_keys = Vec::new();
	for group in &root_groups {
		let key = client.derive_store_next_keypair().await.unwrap().0;
		destinations.push(ArkoorDestination {
			total_amount: sat(group.len() as u64 * LEAF_SAT), policy: ark::VtxoPolicy::new_pubkey(key.public_key()),
		});
		child_keys.push(key);
	}
	let (outputs, root_node) = native_node(&client, &mut rpc, source.clone(), source_key, destinations, PAYEES).await;
	for ((output, key), group) in outputs.into_iter().zip(child_keys).zip(root_groups) {
		jobs.push_back((output, key, group));
	}
	let mut nodes = vec![root_node];
	let mut terminals = Vec::new();
	while let Some((input, key, group)) = jobs.pop_front() {
		let mut destinations = Vec::new();
		let mut children = Vec::new();
		if group.len() <= 4 {
			for &index in &group {
				destinations.push(ArkoorDestination {
					total_amount: sat(LEAF_SAT), policy: ark::VtxoPolicy::new_pubkey(owners[index].key.public_key()),
				});
				children.push((vec![index], None));
			}
		} else {
			let base = group.len() / 4;
			let extra = group.len() % 4;
			let mut offset = 0;
			for part in 0..4 {
				let count = base + usize::from(part < extra);
				let indices = group[offset..offset + count].to_vec();
				offset += count;
				let child_key = if count == 1 { owners[indices[0]].key }
					else { client.derive_store_next_keypair().await.unwrap().0 };
				destinations.push(ArkoorDestination {
					total_amount: sat(count as u64 * LEAF_SAT), policy: ark::VtxoPolicy::new_pubkey(child_key.public_key()),
				});
				children.push((indices, (count > 1).then_some(child_key)));
			}
		}
		let (outputs, node) = native_node(&client, &mut rpc, input, key, destinations, group.len()).await;
		nodes.push(node);
		for (output, (indices, child_key)) in outputs.into_iter().zip(children) {
			if let Some(key) = child_key { jobs.push_back((output, key, indices)); }
			else { terminals.push((indices[0], output)); }
		}
		report["completed_native_cosign_registration_pairs"] = json!(nodes.len());
		if nodes.len() % 16 == 0 { save(&ctx, &report).await; }
	}
	assert_eq!(nodes.len(), 157);
	assert_eq!(terminals.len(), PAYEES);
	assert_eq!(terminals.iter().map(|(i, _)| *i).collect::<BTreeSet<_>>().len(), PAYEES);
	report["native_path_nodes"] = json!(nodes.iter().map(|node| json!({
		"input": node.input, "checkpoint": node.checkpoint, "payees": node.payees,
		"checkpoint_txid": node.checkpoint_tx.compute_txid(), "fanout_txid": node.fanout_tx.compute_txid(),
	})).collect::<Vec<_>>());
	let mut mailbox = srv.get_mailbox_public_rpc().await;
	let mailbox_pubkey = srv.ark_info().await.mailbox_pubkey;
	for (posted, (index, leaf)) in terminals.iter().enumerate() {
		let owner = &owners[*index];
		let blinded = owner.mailbox.to_blinded(mailbox_pubkey, &owner.key).unwrap();
		mailbox.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
			blinded_id: blinded.as_ref().to_vec(), vtxos: vec![leaf.serialize()],
		}).await.unwrap();
		report["mailbox_posts"] = json!(posted + 1);
		if (posted + 1) % 32 == 0 { save(&ctx, &report).await; }
	}
	drop(client);
	drop(wallet);
	drop(rpc);
	drop(mailbox);
	save(&ctx, &report).await;
	let current = ctx.bitcoind().get_block_count().await as u32;
	assert!(current < source.expiry_height().to_u32());
	let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - current).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	report["phase"] = json!("automatic_multilevel_progress");
	save(&ctx, &report).await;
	let mut graph = BTreeMap::new();
	confirm_package(&ctx, &srv, &board_exit, &mut graph, &mut report).await;
	let grace = srv.watchmand().config().watchman.progress_grace_period.to_u32();
	ctx.generate_blocks(grace).await;
	confirm_package(&ctx, &srv, &nodes[0].checkpoint_tx, &mut graph, &mut report).await;
	confirm_package(&ctx, &srv, &nodes[0].fanout_tx, &mut graph, &mut report).await;
	ctx.generate_blocks(grace).await;
	for node in nodes.iter().filter(|n| n.payees == 324 || n.payees == 1) {
		confirm_package(&ctx, &srv, &node.checkpoint_tx, &mut graph, &mut report).await;
	}
	let large = nodes.iter().find(|node| node.payees == 324).unwrap();
	confirm_package(&ctx, &srv, &large.fanout_tx, &mut graph, &mut report).await;
	ctx.generate_blocks(grace).await;
	let ready_nodes = nodes.iter().filter(|node| node.payees == 81).collect::<Vec<_>>();
	assert_eq!(ready_nodes.len(), 4);
	for node in ready_nodes {
		confirm_package(&ctx, &srv, &node.checkpoint_tx, &mut graph, &mut report).await;
	}
	for (point, progress) in [
		(source.chain_anchor(), board_exit.compute_txid()),
		(nodes[0].checkpoint, nodes[0].fanout_tx.compute_txid()),
		(large.checkpoint, large.fanout_tx.compute_txid()),
	] {
		let actual = db.read(async |tx| Ok(tx.query_one(
			"SELECT progress_txid FROM expiry_split_request WHERE vtxo_id=$1", &[&ark::VtxoId::from(point).to_string()],
		).await?.get::<_, String>(0))).await.unwrap();
		assert_eq!(actual, progress.to_string(), "the daemon must create each actual oversized split instruction");
		report["automatic_split_requests"].as_array_mut().unwrap().push(json!({
			"backing": point, "progress_txid": progress,
		}));
	}
	assert_eq!(large.input.txid, nodes[0].fanout_tx.compute_txid());
	report["phase"] = json!("all_owner_payouts");
	save(&ctx, &report).await;
	let terminal_ids = terminals.iter().map(|(_, leaf)| leaf.id().to_string()).collect::<Vec<_>>();
	let payout_rows = loop {
		let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,a.id,a.sweep_tx,c.gross_sat,c.fee_sat, \
			c.payout_sat,c.forfeited_sat,c.spk FROM expiry_entitlement e \
			JOIN expiry_allocation_coin c ON c.entitlement_id=e.id \
			JOIN expiry_allocation a ON a.id=c.allocation_id \
			WHERE e.vtxo_id=ANY($1) AND c.active AND a.broadcast_at IS NOT NULL", &[&terminal_ids],
		).await?)).await.unwrap();
		assert!(rows.len() <= PAYEES);
		if rows.len() == PAYEES { break rows; }
		tokio::time::sleep(Duration::from_millis(100)).await;
	};
	let mut payouts = BTreeMap::new();
	for row in &payout_rows {
		assert_eq!(row.get::<_, i64>("gross_sat"), LEAF_SAT as i64);
		assert_eq!(row.get::<_, i64>("fee_sat"), 1_000);
		assert_eq!(row.get::<_, i64>("payout_sat"), (LEAF_SAT - 1_000) as i64);
		assert_eq!(row.get::<_, i64>("forfeited_sat"), 0);
		let parent = deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
		payouts.insert(parent.compute_txid(), parent);
	}
	for parent in payouts.values() { confirm_package(&ctx, &srv, parent, &mut graph, &mut report).await; }
	let final_height = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(final_height).await;
	srv.watchmand().wait_for_sync_height(final_height).await;
	let core = ctx.bitcoind().sync_client();
	for (index, leaf) in &terminals {
		let owner = &owners[*index];
		let matches = payouts.values().flat_map(|tx| tx.output.iter().enumerate().filter_map(move |(vout, out)|
			(out.script_pubkey == owner.record.spk).then_some((tx.compute_txid(), vout, out))
		)).collect::<Vec<_>>();
		assert_eq!(matches.len(), 1, "each actual owner must have exactly one economic payout output");
		let (txid, vout, expected) = matches[0];
		assert_eq!(expected.value, sat(LEAF_SAT - 1_000));
		let actual = core.get_tx_out(&txid, vout as u32, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6);
		assert_eq!(actual.value, expected.value);
		assert_eq!(actual.script_pub_key.script().unwrap(), owner.record.spk);
		let row = payout_rows.iter().find(|r| r.get::<_, String>("vtxo_id") == leaf.id().to_string()).unwrap();
		assert_eq!(row.get::<_, String>("spk"), owner.record.spk.as_bytes().to_lower_hex_string());
		let associated_parent = deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
		assert_eq!(associated_parent.compute_txid(), txid,
			"each terminal's stored parent must contain that owner's actual payout output");
		report["payouts"].as_array_mut().unwrap().push(json!({
			"owner": index, "vtxo_id": leaf.id(), "outpoint": OutPoint::new(txid, vout as u32),
			"amount_sat": actual.value.to_sat(), "confirmations": actual.confirmations,
		}));
	}
	save(&ctx, &report).await;
	loop {
		let row = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) AS n,COUNT(DISTINCT e.vtxo_id) AS coins \
			FROM expiry_entitlement e JOIN expiry_allocation_coin c ON c.entitlement_id=e.id \
			JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=ANY($1) \
			AND c.active AND a.status='confirmed' AND c.paying_txid=a.sweep_txid AND c.paying_hash=a.confirmed_hash",
			&[&terminal_ids],
		).await?)).await.unwrap();
		assert!(row.get::<_, i64>("n") <= PAYEES as i64);
		if row.get::<_, i64>("n") == PAYEES as i64 && row.get::<_, i64>("coins") == PAYEES as i64 { break; }
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	for txid in payouts.keys() {
		let canonical = core.custom_get_raw_transaction_info(*txid, None).unwrap().unwrap().blockhash.unwrap();
		let stored = db.read(async |tx| Ok(tx.query_one(
			"SELECT confirmed_hash FROM expiry_allocation WHERE sweep_txid=$1", &[&txid.to_string()],
		).await?.get::<_, String>(0))).await.unwrap();
		assert_eq!(stored, canonical.to_string(), "payout history must name the actual canonical block");
	}
	let entitlements = db.read(async |tx| Ok(tx.query_one(
		"SELECT COUNT(*) FROM expiry_allocation_coin WHERE disposition='customer' AND active", &[],
	).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(entitlements, PAYEES as i64, "intermediate coins cannot acquire extra customer payouts");
	let mut spent = BTreeSet::new();
	let mut external_inputs = 0;
	let mut fees = 0;
	for tx in graph.values() {
		let info = core.custom_get_raw_transaction_info(tx.compute_txid(), None).unwrap().unwrap();
		assert!(info.confirmations.unwrap_or(0) >= 6);
		fees += actual_fee(&ctx, tx).await;
		for input in &tx.input {
			assert!(spent.insert(input.previous_output), "no backing or fee outpoint may be spent twice");
			if !graph.contains_key(&input.previous_output.txid) {
				assert!(input.previous_output == source.chain_anchor() || sponsor_funding.contains(&input.previous_output),
					"the observed transaction DAG can consume only declared real backing and watchman sponsors");
				let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
				external_inputs += previous.output[input.previous_output.vout as usize].value.to_sat();
			}
		}
	}
	assert!(spent.contains(&source.chain_anchor()));
	let mut remaining = 0;
	for (txid, tx) in &graph {
		for (vout, expected) in tx.output.iter().enumerate() {
			if spent.contains(&OutPoint::new(*txid, vout as u32)) { continue; }
			let actual = core.get_tx_out(txid, vout as u32, Some(true)).unwrap().unwrap();
			assert_eq!(actual.value, expected.value);
			assert_eq!(actual.script_pub_key.script().unwrap(), expected.script_pubkey);
			remaining += actual.value.to_sat();
		}
	}
	assert_eq!(external_inputs, remaining + fees, "actual canonical DAG must conserve principal and sponsor funds");
	report["conservation"] = json!({"external_input_sat": external_inputs,
		"remaining_output_sat": remaining, "canonical_miner_fees_sat": fees,
		"customer_payout_sat": PAYEES as u64 * (LEAF_SAT - 1_000), "unique_transactions": graph.len()});
	report["outcome"] = json!("PASS");
	report["phase"] = json!("complete");
	save(&ctx, &report).await;
}
