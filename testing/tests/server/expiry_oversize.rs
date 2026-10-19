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
	checkpoint_tx: Transaction,
	branches: Vec<Branch>,
	payees: usize,
}

struct Branch {
	checkpoint: OutPoint,
	exit_tx: Transaction,
	output: OutPoint,
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

async fn native_node(
	client: &bark::Wallet, rpc: &mut ArkClient, input: Vtxo<Full>, key: Keypair,
	destinations: Vec<ArkoorDestination>, payees: usize,
) -> (Vec<Vtxo<Full>>, Node) {
	assert!(input.exit_depth() < 100);
	assert!(!destinations.is_empty() && destinations.len() <= 4);
	let input_point = input.point();
	let input_expiry = input.expiry_height();
	let input_exit_delta = input.exit_delta();
	let input_server = input.server_pubkey();
	let expected = destinations.iter().map(|d| (d.policy.clone(), d.total_amount)).collect::<Vec<_>>();
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input], destinations).unwrap()
		.generate_user_nonces(&[key]).unwrap();
	let response = rpc.request_arkoor_cosign(protos::ArkoorPackageCosignRequest::from(builder.cosign_request()))
		.await.unwrap().into_inner();
	let signed = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap();
	let checkpoints = signed.build_signed_internal_vtxos().into_iter()
		.filter(|(v, _)| matches!(v.policy(), ServerVtxoPolicy::Checkpoint(_))).collect::<Vec<_>>();
	// The native checkpoint transaction fans out into one independently
	// recoverable checkpoint per destination. Each has its own signed exit
	// transaction; there is no single checkpoint containing all destinations.
	assert_eq!(checkpoints.len(), expected.len());
	let transactions = signed.signed_virtual_transactions();
	assert_eq!(transactions.len(), expected.len() + 1);
	let checkpoint_tx = transactions.iter().find(|tx| tx.input[0].previous_output == input_point).unwrap().clone();
	assert_eq!(checkpoint_tx.input.len(), 1);
	assert_eq!(checkpoint_tx.output.len(), expected.len() + 1);
	assert_eq!(checkpoint_tx.output.iter().filter(|out| out.is_p2a_fee_anchor()).count(), 1);
	let outputs = signed.build_signed_vtxos();
	assert_eq!(outputs.len(), expected.len());
	let mut branches = Vec::new();
	for (index, (vtxo, (policy, amount))) in outputs.iter().zip(expected).enumerate() {
		assert_eq!(vtxo.policy(), &policy);
		assert_eq!(vtxo.amount(), amount);
		assert!(vtxo.exit_depth() < 100);
		let point = OutPoint::new(checkpoint_tx.compute_txid(), index as u32);
		let (checkpoint, exit_txid) = checkpoints.iter().find(|(v, _)| v.point() == point).unwrap();
		assert_eq!(checkpoint.policy(), &ServerVtxoPolicy::new_checkpoint(key.public_key()));
		assert_eq!(checkpoint.amount(), amount);
		assert_eq!(checkpoint.expiry_height(), input_expiry);
		assert_eq!(checkpoint.exit_delta(), input_exit_delta);
		assert_eq!(checkpoint.server_pubkey(), input_server);
		assert_eq!(checkpoint.txout(), checkpoint_tx.output[index]);
		let exit_tx = transactions.iter().find(|tx| tx.compute_txid() == *exit_txid).unwrap().clone();
		assert_eq!(exit_tx.input.len(), 1);
		assert_eq!(exit_tx.input[0].previous_output, point);
		assert_eq!(exit_tx.output.len(), 2);
		assert_eq!(vtxo.point(), OutPoint::new(exit_tx.compute_txid(), 0));
		assert_eq!(exit_tx.output[0], vtxo.txout());
		assert!(exit_tx.output[1].is_p2a_fee_anchor());
		branches.push(Branch { checkpoint: point, exit_tx, output: vtxo.point() });
	}
	client.register_vtxo_transactions_with_server(&outputs).await.unwrap();
	(outputs, Node { input: input_point, checkpoint_tx, branches, payees })
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
	graph: &mut BTreeMap<Txid, Transaction>,
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
}

#[tokio::test]
#[ignore = "327 native payees; declare a serial 600-second regtest slot and use fallback-scale profile"]
async fn expiry_automatic_real_limit_multilevel_split_pays_every_owner() {
	tokio::time::timeout(Duration::from_secs(600), automatic_multilevel_split()).await
		.expect("automatic size-limit splitting exceeded 600 seconds");
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
	// Give every owner a separate random account and payout destination.
	let owners = (0..PAYEES).map(|_| owner()).collect::<Vec<_>>();
	assert_eq!(owners.iter().map(|o| o.record.spk.clone()).collect::<BTreeSet<_>>().len(), PAYEES);
	// This lower bound uses real consensus serialization, without any input,
	// witness, drain, anchor or transaction-header overhead.
	let minimum_outputs = owners[..324].iter().map(|o|
		serialize(&TxOut { value: sat(LEAF_SAT - 1_000), script_pubkey: o.record.spk.clone() }).len()
	).sum::<usize>();
	assert_eq!(minimum_outputs, 10_044);
	assert!(minimum_outputs > 10_000);
	let mut rpc = srv.get_public_rpc().await;
	for owner in &owners {
		let bytes = owner.record.serialize();
		let reply = rpc.set_fallback(protos::SetFallbackRequest {
			mailbox_pk: owner.mailbox.as_pubkey().serialize().to_vec(), record: bytes.clone(),
		}).await.unwrap().into_inner();
		assert_eq!(reply.record, bytes);
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

	let mut destinations = Vec::new();
	let mut jobs = VecDeque::new();
	let root_groups = [Vec::from_iter(0..324), vec![324], vec![325], vec![326]];
	let mut child_keys = Vec::new();
	for group in &root_groups {
		let key = if group.len() == 1 { owners[group[0]].key }
			else { client.derive_store_next_keypair().await.unwrap().0 };
		destinations.push(ArkoorDestination {
			total_amount: sat(group.len() as u64 * LEAF_SAT), policy: ark::VtxoPolicy::new_pubkey(key.public_key()),
		});
		child_keys.push((group.len() > 1).then_some(key));
	}
	let (outputs, root_node) = native_node(&client, &mut rpc, source.clone(), source_key, destinations, PAYEES).await;
	let mut terminals = Vec::new();
	for ((output, key), group) in outputs.into_iter().zip(child_keys).zip(root_groups) {
		if let Some(key) = key { jobs.push_back((output, key, group)); }
		else { terminals.push((group[0], output)); }
	}
	let mut nodes = vec![root_node];
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
	}
	assert_eq!(nodes.len(), 154);
	assert_eq!(terminals.len(), PAYEES);
	assert_eq!(terminals.iter().map(|(i, _)| *i).collect::<BTreeSet<_>>().len(), PAYEES);
	let mut signed_paths = BTreeMap::from([(board_exit.compute_txid(), board_exit.clone())]);
	for node in &nodes {
		signed_paths.insert(node.checkpoint_tx.compute_txid(), node.checkpoint_tx.clone());
		for branch in &node.branches { signed_paths.insert(branch.exit_tx.compute_txid(), branch.exit_tx.clone()); }
	}
	assert_eq!(signed_paths.len(), 635, "154 native operations sign 634 path transactions, plus the board exit");
	let mut mailbox = srv.get_mailbox_public_rpc().await;
	let mailbox_pubkey = srv.ark_info().await.mailbox_pubkey;
	for (index, leaf) in &terminals {
		let owner = &owners[*index];
		let blinded = owner.mailbox.to_blinded(mailbox_pubkey, &owner.key).unwrap();
		mailbox.post_arkoor_message(protos::mailbox_server::PostArkoorMessageRequest {
			blinded_id: blinded.as_ref().to_vec(), vtxos: vec![leaf.serialize()],
		}).await.unwrap();
	}
	drop(client);
	drop(wallet);
	drop(rpc);
	drop(mailbox);
	let current = ctx.bitcoind().get_block_count().await as u32;
	assert!(current < source.expiry_height().to_u32());
	let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - current).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let mut graph = BTreeMap::new();
	confirm_package(&ctx, &srv, &board_exit, &mut graph).await;
	let grace = srv.watchmand().config().watchman.progress_grace_period.to_u32();
	ctx.generate_blocks(grace).await;
	// The source's ordinary pubkey output is progressed by the native
	// watchtower, after the real grace period. Its one checkpoint transaction
	// creates all four independently recoverable branch outputs together.
	confirm_package(&ctx, &srv, &nodes[0].checkpoint_tx, &mut graph).await;
	let large = nodes.iter().find(|node| node.payees == 324).unwrap();
	let large_branch = nodes[0].branches.iter().find(|branch| branch.output == large.input).unwrap();
	assert_eq!(large_branch.checkpoint.txid, nodes[0].checkpoint_tx.compute_txid());
	// This physical checkpoint still represents 324 distinct recipients and
	// cannot fit a payout at the production size limit. Only the automatic
	// readiness/split path can authorize its actual signed branch exit.
	confirm_package(&ctx, &srv, &large_branch.exit_tx, &mut graph).await;
	ctx.generate_blocks(grace).await;
	confirm_package(&ctx, &srv, &large.checkpoint_tx, &mut graph).await;
	let ready_nodes = nodes.iter().filter(|node| node.payees == 81).collect::<Vec<_>>();
	assert_eq!(ready_nodes.len(), 4);
	for node in ready_nodes {
		assert!(large.branches.iter().any(|branch| branch.output == node.input));
	}
	let required_splits = [
		(source.chain_anchor(), board_exit.compute_txid()),
		(large_branch.checkpoint, large_branch.exit_tx.compute_txid()),
	];
	let requests = db.read(async |tx| Ok(tx.query("SELECT s.vtxo_id,s.progress_txid \
		FROM expiry_split_request s JOIN vtxo v ON v.vtxo_id=s.vtxo_id WHERE v.anchor_point=$1",
		&[&source.chain_anchor().to_string()],
	).await?)).await.unwrap();
	let mut observed_requests = BTreeSet::new();
	for request in requests {
		let backing = request.get::<_, String>("vtxo_id");
		let progress: Txid = request.get::<_, &str>("progress_txid").parse().unwrap();
		let path = signed_paths.get(&progress).expect("each automatic request must name a genuine signed native path");
		assert_eq!(path.input.len(), 1);
		assert_eq!(ark::VtxoId::from(path.input[0].previous_output).to_string(), backing);
		observed_requests.insert((backing, progress));
	}
	for (point, progress) in required_splits {
		assert!(observed_requests.contains(&(ark::VtxoId::from(point).to_string(), progress)),
			"the daemon must create both actual oversized backing requests, independently of extra legitimate substeps");
	}
	let terminal_ids = terminals.iter().map(|(_, leaf)| leaf.id().to_string()).collect::<Vec<_>>();
	let payout_rows = loop {
		let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,a.id,a.sweep_tx,c.gross_sat,c.fee_sat, \
			c.payout_sat,c.forfeited_sat,c.spk,c.owner_mailbox,c.user_pubkey FROM expiry_entitlement e \
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
	for parent in payouts.values() { confirm_package(&ctx, &srv, parent, &mut graph).await; }
	let final_height = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(final_height).await;
	srv.watchmand().wait_for_sync_height(final_height).await;
	let core = ctx.bitcoind().sync_client();
	// Native progression can publish additional signed substeps alongside
	// packages already observed. Include every canonical path transaction and
	// its actual sponsor, so change/drain accounting never omits a real edge.
	for (txid, path) in &signed_paths {
		if graph.contains_key(txid) { continue; }
		let Some(info) = core.custom_get_raw_transaction_info(*txid, None).unwrap() else { continue; };
		if info.confirmations.unwrap_or(0) > 0 {
			confirm_package(&ctx, &srv, path, &mut graph).await;
		}
	}
	for (index, leaf) in &terminals {
		let owner = &owners[*index];
		let matches = graph.values().flat_map(|tx| tx.output.iter().enumerate().filter_map(move |(vout, out)|
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
		assert_eq!(row.get::<_, String>("owner_mailbox"), owner.mailbox.as_pubkey().to_string());
		assert_eq!(row.get::<_, String>("user_pubkey"), owner.key.public_key().to_string());
		let associated_parent = deserialize::<Transaction>(row.get("sweep_tx")).unwrap();
		assert_eq!(associated_parent.compute_txid(), txid,
			"each terminal's stored parent must contain that owner's actual payout output");
	}
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
}
