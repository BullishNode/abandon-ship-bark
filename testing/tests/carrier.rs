//! Real operator-owned backing proves construction/signing/relay only.
//! Exact supplied outputs here do not claim resolved customer entitlement.

#[path = "carrier/pool_retirement.rs"]
mod pool_retirement;


#[path = "carrier/returning_client.rs"]
mod returning_client;
#[path = "carrier/client_stack.rs"]
mod client_stack;
#[path = "carrier/cap_freeze.rs"]
mod cap_freeze;
#[path = "carrier/route_consistency.rs"]
mod route_consistency;
#[path = "carrier/pending_board_return.rs"]
mod pending_board_return;
#[path = "carrier/wallet_pending_inventory.rs"]
mod wallet_pending_inventory;
#[path = "carrier/saved_send_return.rs"]
mod saved_send_return;
#[path = "carrier/fee_acceleration.rs"]
mod fee_acceleration;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use bitcoin::{Address, Amount, FeeRate, Network, OutPoint, ScriptBuf, TxOut, Weight};
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::secp256k1::Keypair;
use bitcoincore_rpc::RpcApi;
use tokio::process::{Child, Command};

use ark::board::BoardBuilder;
use ark::{ProtocolEncoding, ServerVtxo};
use ark::vtxo::policy::clause::{TapScriptClause, VtxoClause};
use ark::vtxo::policy::signing::VtxoSigner;
use bitcoin_ext::{BlockDelta, BlockHeight, BlockRef, TxOutExt};
use bitcoin_ext::bdk::{WalletExt, KEYCHAIN};
use bitcoin_ext::rpc::BitcoinRpcExt;
use server::database::{Db, SpendState};
use server::database::expiry_ownership::OwnershipContext;
use server::database::carrier_snapshot::{BackingPartitionResidual, BackingSnapshot, BackingSnapshotLimits, SnapshotGenesis};
use server::secret::Secret;
use server::utils::InstrumentedLock;
use server::wallet::{self, BdkWalletExt, PersistedWallet, WalletKind};
use server::watchman::{WatchmanSigner, carrier::{build_direct_carrier, CarrierLimits}};

use ark_testing::{sat, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;

#[path = "carrier/direct_delivery.rs"]
mod direct_delivery;

fn operator_backing(
	operator: &Keypair, server: &Keypair, expiry: BlockHeight, amount: Amount, point: OutPoint,
) -> ServerVtxo {
	let user = BoardBuilder::new(operator.public_key(), expiry, server.public_key(), BlockDelta::new(144))
		.set_funding_details(amount, Amount::ZERO, point).unwrap().generate_user_nonces();
	BoardBuilder::new_for_cosign(operator.public_key(), expiry, server.public_key(), BlockDelta::new(144),
		amount, Amount::ZERO, point, *user.user_pub_nonce())
		.build_unsigned_vtxos().unwrap().0.into_bare()
}

fn sync_fee_wallet(wallet: &mut PersistedWallet, core: &impl RpcApi) {
	let checkpoint = wallet.latest_checkpoint();
	let mut emitter = bdk_bitcoind_rpc::Emitter::new(core, checkpoint, 0, wallet.unconfirmed_txs());
	while let Some(block) = emitter.next_block().unwrap() {
		wallet.apply_block_connected_to(&block.block, block.block_height(), block.connected_to()).unwrap();
	}
	wallet.apply_unconfirmed_txs(emitter.mempool().unwrap().update);
}

#[tokio::test]
async fn direct_carrier_actual_backing_mixed_signing_and_confirmed_reserve() {
	let ctx = TestContext::new("server/direct_carrier_actual_backing_mixed_signing_and_confirmed_reserve").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let data_dir = srv.config().data_dir.clone();
	// Operator-owned test keys stay in memory; no mnemonic, secret or raw PSBT logs.
	let mnemonic = wallet::read_mnemonic_from_datadir(&data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let server_key = key_at("m/2'/0'");
	let operator_key = key_at("m/30'");
	let signer = WatchmanSigner::new(Secret::new(server_key), Secret::new(operator_key), db.clone());
	// No watchmand process is launched: this test exclusively holds its standard wallet.
	let core = ctx.bitcoind().sync_client();
	let initial_height = core.get_block_count().unwrap() as u32;
	let tip = BlockRef { height: BlockHeight::new(initial_height), hash: core.get_block_hash(initial_height as u64).unwrap() };
	let mut fee_wallet = PersistedWallet::load_derive_from_master_xpriv(db, ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let reserve_address = fee_wallet.next_unused_address(KEYCHAIN).address;
	let expiry = BlockHeight::new(initial_height + 2);
	let template = operator_backing(&operator_key, &server_key, expiry, sat(20_000), OutPoint::null());
	let backing_address = Address::from_script(&template.txout().script_pubkey, Network::Regtest).unwrap();
	let funding = ctx.bitcoind().fund_addr(backing_address, sat(20_000)).await;
	let funding_tx = core.get_raw_transaction(&funding, None).unwrap();
	let vout = funding_tx.output.iter().position(|o| *o == template.txout()).unwrap() as u32;
	let backing = operator_backing(&operator_key, &server_key, expiry, sat(20_000), OutPoint::new(funding, vout));
	ctx.generate_blocks(3).await;
	sync_fee_wallet(&mut fee_wallet, &core);
	let recipient = TxOut { value: sat(19_000), script_pubkey: ctx.bitcoind().get_new_address().script_pubkey() };
	let operator_script = ctx.bitcoind().get_new_address().script_pubkey();
	let policy = CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb(50).unwrap(),
		max_miner_fee: sat(10_000), max_weight: Weight::from_wu(400_000) };
	let fee_rate = FeeRate::from_sat_per_vb(30).unwrap();
	let wallet = InstrumentedLock::new("test_carrier_fee_wallet", fee_wallet);
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	assert!(build_direct_carrier(&signer, wallet.lock_owned().await, &[backing.clone()], &[recipient.clone()],
		operator_script.clone(), height, fee_rate, policy).await.is_err(), "no reserve must defer fixed recipient");
	let reserve = ctx.bitcoind().fund_addr(reserve_address, sat(1_000_000)).await;
	{
		let mut held = wallet.lock_owned().await;
		sync_fee_wallet(&mut held, &core);
	}
	assert!(build_direct_carrier(&signer, wallet.lock_owned().await, &[backing.clone()], &[recipient.clone()],
		operator_script.clone(), height, fee_rate, policy).await.is_err(), "unconfirmed reserve must refuse");
	ctx.generate_blocks(1).await;
	{
		let mut held = wallet.lock_owned().await;
		sync_fee_wallet(&mut held, &core);
	}
	let one_confirmation_height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	assert!(build_direct_carrier(&signer, wallet.lock_owned().await, &[backing.clone()], &[recipient.clone()],
		operator_script.clone(), one_confirmation_height, fee_rate, policy).await.is_err(),
		"one confirmation cannot bypass configured three-confirmation reserve trust");
	ctx.generate_blocks(2).await;
	let mut held = wallet.lock_owned().await;
	sync_fee_wallet(&mut held, &core);
	let reserved_point = held.list_unspent().find(|u| u.outpoint.txid == reserve).unwrap().outpoint;
	let locked = held.lock_wallet_utxo(reserved_point).unwrap();
	drop(held);
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	assert!(build_direct_carrier(&signer, wallet.lock_owned().await, &[backing.clone()], &[recipient.clone()],
		operator_script.clone(), height, fee_rate, policy).await.is_err(), "locked reserve must refuse");
	drop(locked);
	let error = build_direct_carrier(&signer, wallet.lock_owned().await, &[backing.clone()], &[recipient.clone()],
		operator_script.clone(), BlockHeight::new(initial_height), fee_rate, policy).await.err()
		.expect("stale startup height must refuse before retaining an unsatisfiable signed transaction");
	assert!(error.to_string().contains("lock height is below its selected backing clause"));
	let (mut held, carrier) = build_direct_carrier(&signer, wallet.lock_owned().await, &[backing.clone()],
		&[recipient.clone()], operator_script, height, fee_rate, policy).await.unwrap();
	assert_eq!(carrier.backing_ids, vec![backing.id()]);
	assert_eq!(carrier.fee_inputs.len(), 1);
	assert_eq!(carrier.fee_inputs[0].outpoint, reserved_point);
	assert!(held.lock_wallet_utxo(reserved_point).is_err(), "signed fee input remains reserved before handoff");
	assert_eq!(carrier.tx.output[carrier.outputs.recipients[0] as usize], recipient);
	assert_eq!(carrier.outputs.operator, None);
	let change = &carrier.tx.output[carrier.outputs.fee_wallet_change.unwrap() as usize];
	assert!(held.is_mine(change.script_pubkey.clone()));
	assert!(change.value > sat(990_000));
	let input_total = carrier.tx.input.iter().map(|i| {
		core.get_raw_transaction(&i.previous_output.txid, None).unwrap().output[i.previous_output.vout as usize].value.to_sat()
	}).sum::<u64>();
	let actual_fee = input_total - carrier.tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
	assert_eq!(actual_fee, carrier.miner_fee.to_sat());
	assert_eq!(carrier.signed_weight, carrier.tx.weight());
	assert_eq!(carrier.funding_subsidy, carrier.miner_fee - sat(1_000));
	let accepted: serde_json::Value = core.call("testmempoolaccept", &[
		serde_json::json!([bitcoin::consensus::encode::serialize_hex(&carrier.tx)]),
	]).unwrap();
	assert_eq!(accepted[0]["allowed"], true, "actual node validates both signing domains and maturity");
	// Private signed proof is retained without printing witnesses.
	let proof_path = ctx.datadir.join("signed-carrier.tx");
	let mut proof = OpenOptions::new().write(true).create_new(true).mode(0o600).open(proof_path).unwrap();
	proof.write_all(&bitcoin::consensus::serialize(&carrier.tx)).unwrap();
	held.commit_tx(&carrier.tx);
	held.persist().await.unwrap();
	core.send_raw_transaction(&carrier.tx).unwrap();
	ctx.generate_blocks(1).await;
	assert!(core.get_raw_transaction_info(&carrier.tx.compute_txid(), None).unwrap().confirmations.unwrap() >= 1);
	assert!(core.get_tx_out(&backing.point().txid, backing.point().vout, Some(true)).unwrap().is_none());
	assert!(core.get_tx_out(&reserved_point.txid, reserved_point.vout, Some(true)).unwrap().is_none());
	println!("builder-only carrier confirmed: txid={}, weight={}, miner_fee={}, fee_subsidy={}, fee_change={}",
		carrier.tx.compute_txid(), carrier.signed_weight.to_wu(), actual_fee,
		carrier.funding_subsidy.to_sat(), change.value.to_sat());
}

struct AutomaticBoardFixture {
	ctx: TestContext,
	srv: std::sync::Arc<ark_testing::Captaind>,
	mnemonic: bip39::Mnemonic,
	coin: ark::Vtxo,
	record: ark::fallback::FallbackRecord,
}

async fn automatic_board_fixture(name: &str) -> AutomaticBoardFixture {
	automatic_board_fixture_with_fee(name, Amount::ZERO).await
}

async fn automatic_board_fixture_with_fee(name: &str, board_fee: Amount) -> AutomaticBoardFixture {
	automatic_board_fixture_with_funding(name, board_fee, ark_testing::btc(1)).await
}

async fn automatic_board_fixture_with_funding(name: &str, board_fee: Amount, rounds_funding: Amount) -> AutomaticBoardFixture {
	let ctx = TestContext::new(format!("carrier-builder/{name}")).await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(rounds_funding).cfg(move |c| {
			c.round_interval = std::time::Duration::from_secs(3600);
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fees.board.base_fee = board_fee;
			c.direct_expiry.enabled = true;
			c.direct_expiry.grace_blocks = 0;
			c.direct_expiry.interval = std::time::Duration::from_secs(3600);
		}).watchmand_cfg(move |c| {
			c.min_trusted_confs = 3;
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(),
				history_dir: c.data_dir.parent().unwrap().join("server"), max_miner_fee: sat(10_000),
			});
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
			c.watchman.sweep_interval = std::time::Duration::from_secs(3600);
			c.watchman.reaction_interval = std::time::Duration::from_secs(3600);
		}).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(20_000).checked_add(board_fee).unwrap()).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coins = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(coins.len(), 1, "actual native Board must register before route qualification");
	let coin = wallet.get_full_vtxo(coins[0].id()).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	let mailbox = wallet.mailbox_keypair().public_key().serialize().to_vec();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let stored = db.read(async |t| Ok(t.query_one(
		"SELECT spk, mailbox_sig FROM fallback_record WHERE mailbox_pk=$1", &[&mailbox],
	).await?)).await.unwrap();
	assert_eq!(stored.get::<_, Vec<u8>>("spk"), record.spk.as_bytes());
	assert_eq!(stored.get::<_, Vec<u8>>("mailbox_sig"), record.mailbox_signature.serialize());
	record.verify(Network::Regtest, wallet.mailbox_keypair().public_key()).unwrap();
	let retained = wallet.fallback_key_link(coin.user_pubkey()).await.unwrap();
	retained.verify(Network::Regtest, wallet.mailbox_keypair().public_key(), &record).unwrap();
	let coin_key = coin.user_pubkey().serialize().to_vec();
	let linked = db.read(async |t| Ok(t.query_one(
		"SELECT mailbox_pk,sig,recovery_index FROM key_link WHERE user_pubkey=$1", &[&coin_key],
	).await?)).await.unwrap();
	assert_eq!(linked.get::<_, Vec<u8>>("mailbox_pk"), mailbox);
	assert_eq!(linked.get::<_, Vec<u8>>("sig"), retained.signature.serialize());
	assert_eq!(linked.get::<_, Option<i32>>("recovery_index"), retained.recovery_index.map(|i| i as i32));
	let core = ctx.bitcoind().sync_client();
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(true)).unwrap().is_some());
	drop(wallet); // Original client stays absent through claim, confirmation and fresh recovery.
	AutomaticBoardFixture { ctx, srv, mnemonic, coin, record }
}

// Same finite fee-wallet opening as the original returning-source fixtures.
// Five confirmed inputs let the FULL10000 ceiling preflight leave the actual
// 40000 urgent floor unselected; unconfirmed prospective change is never credit.
async fn fund_confirmed_split_fee_reserve(ctx: &TestContext, srv: &ark_testing::Captaind) {
	let address = tokio::time::timeout(std::time::Duration::from_secs(30),
		srv.watchmand().wait_wallet_address()).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let mut points = BTreeSet::new();
	for _ in 0..5 {
		let txid = ctx.bitcoind().fund_addr(address.clone(), sat(10_000)).await;
		let tx = core.get_raw_transaction(&txid, None).unwrap();
		let vout = tx.output.iter().position(|o| o.script_pubkey == address.script_pubkey() && o.value == sat(10_000)).unwrap();
		assert!(points.insert(OutPoint::new(txid, vout as u32)));
	}
	ctx.generate_blocks(3).await;
	let inventory = wait_fee_inventory_tip(ctx,srv.watchmand().config().admin_address.unwrap()).await;
	assert_eq!(inventory.utxos.len(),5);
	assert_eq!(inventory.utxos.iter().map(|u|u.outpoint.parse::<OutPoint>().unwrap()).collect::<BTreeSet<_>>(),points);
	assert!(inventory.reserved_outpoints.is_empty());
	assert!(inventory.utxos.iter().all(|u|u.value_sat==10_000 && u.confirmed_fee_eligible && u.confirmations>=3));
}

async fn wait_fee_inventory_tip(ctx: &TestContext, address: std::net::SocketAddr) -> server_rpc::protos::WalletInventory {
	let core = ctx.bitcoind().sync_client();
	let height = core.get_block_count().unwrap();
	let tip = core.get_block_hash(height).unwrap().to_string();
	let inventory = tokio::time::timeout(std::time::Duration::from_secs(30), async {
		let mut client = server_rpc::admin::WalletAdminServiceClient::connect(
			format!("http://{address}")).await.unwrap();
		loop {
			let inventory = client.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner().watchman.unwrap();
			if inventory.complete && u64::from(inventory.tip_height)==height && inventory.tip_hash==tip { break inventory; }
			tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		}
	}).await.expect("actual owning fee wallet must reach the independent confirmed split-funding tip");
	assert!(inventory.network=="regtest" && inventory.min_trusted_confirmations==3);
	assert!(inventory.utxos.iter().filter(|u|u.confirmed_fee_eligible).map(|u|u.value_sat).sum::<u64>()>=40_000);
	inventory
}

#[tokio::test]
async fn automatic_board_creation_is_registered_before_absence() {
	let fixture = automatic_board_fixture("automatic_board_creation_is_registered_before_absence").await;
	assert_eq!(fixture.coin.amount(), sat(20_000));
}

// The recovery function receives only a mnemonic and a chain transport. It
// does not receive the original wallet, expected address, index, txid or outpoint.
async fn recover_all_bip84_and_spend(
	ctx: &TestContext, mnemonic: &bip39::Mnemonic,
) -> (Vec<(OutPoint, Amount)>, bitcoin::Transaction) {
	let core = ctx.bitcoind().sync_client();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let mut restored = bdk_wallet::Wallet::create(
		bdk_wallet::template::Bip84(master, bdk_wallet::KeychainKind::External),
		bdk_wallet::template::Bip84(master, bdk_wallet::KeychainKind::Internal),
	).network(Network::Regtest).lookahead(20).create_wallet_no_persist().unwrap();
	for height in 1..=core.get_block_count().unwrap() {
		let hash = core.get_block_hash(height).unwrap();
		restored.apply_block(&core.get_block(&hash).unwrap(), height as u32).unwrap();
	}
	let discovered = restored.list_unspent().map(|u| (u.outpoint, u.txout.value)).collect::<Vec<_>>();
	assert!(!discovered.is_empty(), "fresh standard BIP84 gap20 scan must discover funds");
	let destination = ctx.bitcoind().get_new_address();
	let mut builder = restored.build_tx();
	builder.drain_wallet().drain_to(destination.script_pubkey()).fee_rate(FeeRate::from_sat_per_vb(3).unwrap());
	let mut psbt = builder.finish().unwrap();
	assert!(restored.sign(&mut psbt, bdk_wallet::SignOptions::default()).unwrap());
	let spend = psbt.extract_tx().unwrap();
	core.send_raw_transaction(&spend).unwrap();
	ctx.generate_blocks(1).await;
	assert!(core.get_raw_transaction_info(&spend.compute_txid(), None).unwrap().confirmations.unwrap() >= 1);
	for (point, _) in &discovered {
		assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_none());
	}
	(discovered, spend)
}

/// Source-checked financial handoff: real registration and backing, mature
/// ownership freeze, signed carrier and crash-style reload. Automatic daemon
/// dispatch and conditional dependency reconciliation remain separate gates.
#[tokio::test]
async fn direct_carrier_atomic_handoff_preserves_payee_and_wallet_reservation() {
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } =
		automatic_board_fixture_with_fee("direct_carrier_atomic_handoff_preserves_payee_and_wallet_reservation", sat(1_000)).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	// No daemon writes while this fixture holds the standard watchman wallet.
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap();
	let funding = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let roots = [root];
	let terminal = [ServerVtxo::from(coin.clone())];
	let residuals = partition_residuals(&roots, &terminal);
	let budget = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
	let inventory = snapshot(&db, &roots, budget.max_rows, budget.max_serialized_bytes).await.unwrap();
	let partition = inventory.check_partition(&terminal, &[funding.clone()], &residuals, budget).unwrap();
	let residual = residuals.iter().find(|r| r.txout.value > Amount::ZERO).unwrap();
	assert_eq!(partition.roots[0].terminal_amount, sat(20_000));
	assert_eq!(partition.roots[0].anchor_residual_amount, residual.txout.value);
	let funding = BTreeMap::from([(funding.compute_txid(), funding)]);
	let cancellations = BTreeSet::new();
	let send_evidence = BTreeMap::new();
	let early_context = OwnershipContext { tip: core.get_block_count().unwrap() as u32, grace: 0,
		max_dependencies: 100, server_pubkey: coin.server_pubkey(), funding: &funding, canceled_receives: &cancellations };
	assert!(db.write(async |t| t.freeze_expiry_sources(&roots, &terminal, &residuals,
		&early_context, &send_evidence, Network::Regtest, budget).await).await.is_err(),
		"an immature source cannot freeze or leave an allocation prefix");
	let count = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n FROM expiry_allocation", &[]).await?.get::<_, i64>("n"))).await.unwrap();
	assert_eq!(count, 0);
	let operator_mnemonic = wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &operator_mnemonic.to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	let initial_height = core.get_block_count().unwrap();
	let tip = BlockRef { height: BlockHeight::new(initial_height as u32), hash: core.get_block_hash(initial_height).unwrap() };
	let mut fee_wallet = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	ctx.bitcoind().fund_addr(fee_wallet.next_unused_address(KEYCHAIN).address, sat(1_000_000)).await;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(initial_height as u32) + 3).await;
	sync_fee_wallet(&mut fee_wallet, &core);
	let fee_wallet = InstrumentedLock::new("atomic_direct_carrier_fixture", fee_wallet);
	let recipient = TxOut { value: sat(19_000), script_pubkey: record.spk.clone() };
	let operator_script = ctx.bitcoind().get_new_address().script_pubkey();
	let policy = CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb(50).unwrap(),
		max_miner_fee: sat(10_000), max_weight: Weight::from_wu(400_000) };
	let rate = FeeRate::from_sat_per_vb(30).unwrap();
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	let context = OwnershipContext { tip: height.to_u32(), ..early_context };
	let refused: anyhow::Result<()> = db.write(async |t| {
		t.execute("UPDATE key_link SET child_index=child_index+1 WHERE user_pubkey=$1",
			&[&coin.user_pubkey().serialize().to_vec()]).await?;
		assert!(t.freeze_expiry_sources(&roots, &terminal, &residuals, &context,
			&send_evidence, Network::Regtest, budget).await.is_err(), "a mismatched account child cannot freeze");
		let count = t.query_one("SELECT count(*)::bigint AS n FROM expiry_allocation", &[]).await?.get::<_, i64>("n");
		assert_eq!(count, 0);
		anyhow::bail!("restore the declared invalid-child fixture by rollback");
	}).await;
	assert!(refused.is_err());
	let freeze = db.write(async |t| t.freeze_expiry_sources(&roots, &terminal, &residuals,
		&context, &send_evidence, Network::Regtest, budget).await).await.unwrap();
	let retry = db.write(async |t| t.freeze_expiry_sources(&roots, &terminal, &residuals,
		&context, &send_evidence, Network::Regtest, budget).await).await.unwrap();
	assert_eq!(freeze.id, retry.id);
	assert_eq!(freeze.allocation_ids, retry.allocation_ids);
	assert_eq!(freeze.backing_ids, roots.iter().map(|v| v.id()).collect::<Vec<_>>());
	assert_eq!(freeze.recipients, vec![recipient.clone()]);
	let ids = freeze.allocation_ids;
	let freeze_set = freeze.id.as_str();

	let (mut held, mut carrier) = build_direct_carrier(&signer, fee_wallet.lock_owned().await, &roots,
		&[recipient.clone()], operator_script.clone(), height, rate, policy).await.unwrap();
	assert!(!carrier.fee_inputs.is_empty(), "actual funding shortfall must exercise persistent fee reservation");
	let fee_point = carrier.fee_inputs[0].outpoint;
	let txid = carrier.tx.compute_txid();
	let previous_wallet = wallet_changeset_fingerprint(&db, WalletKind::Watchman).await;
	let store = async |t: &server::database::Tx<'_>, carrier: &server::watchman::carrier::DirectCarrier, held: &PersistedWallet| {
		t.store_frozen_expiry_carrier(freeze_set, &ids, &roots, carrier, held, &operator_script,
			Network::Regtest, rate, policy, budget).await
	};
	let saved = carrier.tx.output[carrier.outputs.recipients[0] as usize].clone();
	carrier.tx.output[carrier.outputs.recipients[0] as usize].script_pubkey = operator_script.clone();
	assert!(db.write(async |t| store(t, &carrier, &held).await).await.is_err(), "wrong payee must not publish a carrier");
	carrier.tx.output[carrier.outputs.recipients[0] as usize] = saved;
	assert!(db.write(async |t| t.store_frozen_expiry_carrier(freeze_set, &ids[..1], &roots, &carrier, &held,
		&operator_script, Network::Regtest, rate, policy, budget).await).await.is_err(), "a payable prefix cannot hide an allocation");
	held.commit_tx(&carrier.tx);
	let mut body_stored = false;
	let rolled_back: anyhow::Result<()> = db.write(async |t| {
		store(t, &carrier, &held).await?;
		body_stored = true;
		anyhow::bail!("declared rollback after signed carrier, mappings and wallet stage");
	}).await;
	assert!(rolled_back.is_err());
	assert!(body_stored, "declared rollback must follow successful handoff writes");
	let counts = db.read(async |t| Ok(t.query_one("SELECT (SELECT count(*) FROM expiry_carrier) AS carriers,
		(SELECT count(*) FROM expiry_carrier_allocation) AS mappings", &[]).await?)).await.unwrap();
	assert_eq!(counts.get::<_, i64>("carriers"), 0);
	assert_eq!(counts.get::<_, i64>("mappings"), 0);
	assert!(previous_wallet == wallet_changeset_fingerprint(&db, WalletKind::Watchman).await,
		"body failure must roll back wallet graph with the signed carrier");
	db.write(async |t| store(t, &carrier, &held).await).await.unwrap();
	let committed_wallet = wallet_changeset_fingerprint(&db, WalletKind::Watchman).await;
	assert_ne!(previous_wallet.0, committed_wallet.0);
	db.write(async |t| store(t, &carrier, &held).await).await.unwrap();
	assert!(committed_wallet == wallet_changeset_fingerprint(&db, WalletKind::Watchman).await,
		"idempotent readback must not append another wallet stage");
	let stored = db.read(async |t| Ok(t.query_one("SELECT signed_tx FROM expiry_carrier WHERE txid=$1", &[&txid.to_string()]).await?)).await.unwrap();
	assert!(stored.get::<_, Vec<u8>>("signed_tx") == bitcoin::consensus::serialize(&carrier.tx), "durable signed bytes differ");
	held.take_staged();
	drop(held);
	drop(carrier.fee_reservation);
	let reloaded = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	assert!(!reloaded.list_unspent().any(|u| u.outpoint == fee_point), "reload before broadcast must retain fee spend");
	assert!(!core.get_raw_mempool().unwrap().contains(&txid), "DB publication itself never broadcasts");
	let accepted: serde_json::Value = core.call("testmempoolaccept", &[
		serde_json::json!([bitcoin::consensus::encode::serialize_hex(&carrier.tx)]),
	]).unwrap();
	assert_eq!(accepted[0]["allowed"], true);
	let mut proof = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(ctx.datadir.join("manual-durable-direct-carrier.tx")).unwrap();
	proof.write_all(&bitcoin::consensus::serialize(&carrier.tx)).unwrap();
	core.send_raw_transaction(&carrier.tx).unwrap();
	ctx.generate_blocks(1).await;
	let payout = OutPoint::new(txid, carrier.outputs.recipients[0]);
	let (discovered, spend) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	assert!(discovered.contains(&(payout, sat(19_000))));
	assert!(spend.input.iter().any(|i| i.previous_output == payout));
}

/// A completed user exit leaves a user-only script, rather than Ark backing
/// the server can claim. Refuse freezing it and prove the owner's seed can spend.
/// This is user-initiated recovery, not absent-user BIP84 payout qualification.
#[tokio::test]
async fn confirmed_user_leaf_refuses_expiry_freeze_and_remains_owner_spendable() {
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } =
		automatic_board_fixture_with_fee("confirmed_user_leaf_refuses_expiry_freeze_and_remains_owner_spendable", sat(1_000)).await;
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let parent = coin.transactions().last().unwrap().tx.clone();
	assert_eq!(parent.compute_txid(), coin.point().txid);
	let anchor = parent.output.iter().position(|o| o.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT).unwrap();
	assert_eq!(parent.output[anchor].value, sat(1_000));
	let child = bitcoin::Transaction {
		version: parent.version, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: OutPoint::new(parent.compute_txid(), anchor as u32),
			script_sig: ScriptBuf::new(), sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
			witness: bitcoin::Witness::new() }],
		output: vec![TxOut { value: sat(330), script_pubkey: ctx.bitcoind().get_new_address().script_pubkey() }],
	};
	assert_eq!(core.submit_package(&[&parent, &child]).unwrap().package_msg, "success");
	ctx.generate_blocks(1).await;
	let confirmation = BlockHeight::new(core.get_block_count().unwrap() as u32);
	assert!(core.get_tx_out(&coin.point().txid, coin.point().vout, Some(true)).unwrap().is_some());
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(true)).unwrap().is_none());
	db.write(async |t| t.register_vtxo_confirmation(coin.id(), confirmation).await).await.unwrap();
	let root = db.read(async |t| t.get_bare_vtxo_by_id(coin.id()).await).await.unwrap();
	assert_eq!(root.confirmed_height, Some(confirmation));
	let roots = [root.vtxo];
	let terminals = [ServerVtxo::from(coin.clone())];
	let original = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let funding = BTreeMap::from([(original.compute_txid(), original)]);
	let canceled_receives = BTreeSet::new();
	let send_evidence = BTreeMap::new();
	let budget = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
	let operator_mnemonic = wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &operator_mnemonic.to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	assert!(signer.find_signable_clause(&roots[0]).await.is_none(),
		"a user-only leaf has no server claim even after Ark expiry");
	let maturity = coin.expiry_height().to_u32().max(confirmation.to_u32() + coin.exit_delta().to_u32()) + 1;
	ctx.generate_blocks(maturity.saturating_sub(core.get_block_count().unwrap() as u32)).await;
	let context = OwnershipContext { tip: maturity, grace: 0, max_dependencies: 100,
		server_pubkey: coin.server_pubkey(), funding: &funding, canceled_receives: &canceled_receives };
	let refused = db.write(async |t| t.freeze_expiry_sources(&roots, &terminals, &[],
		&context, &send_evidence, Network::Regtest, budget).await).await.err().expect("user-only leaf must not freeze");
	assert!(format!("{refused:#}").contains("ExitedOrConsumed"));
	let allocations = db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint AS n FROM expiry_allocation", &[])
		.await?.get::<_, i64>("n"))).await.unwrap();
	assert_eq!(allocations, 0, "the refusal must leave no impossible frozen obligation");
	// Re-derive the account from the mnemonic, without the original client or
	// its SQLite keys. This fixture's fresh Board is within a bounded scan.
	let user_master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let account = user_master.derive_priv(&ark::SECP, &DerivationPath::from_str("m/350'/0'").unwrap()).unwrap();
	let mailbox = user_master.derive_priv(&ark::SECP, &DerivationPath::from_str("m/350'/1'").unwrap()).unwrap().to_keypair(&ark::SECP);
	record.verify(Network::Regtest, mailbox.public_key()).unwrap();
	let user_key = (0..32).map(|i| account.derive_priv(&ark::SECP, &[bitcoin::bip32::ChildNumber::Normal { index: i }])
		.unwrap().to_keypair(&ark::SECP)).find(|k| k.public_key() == coin.user_pubkey()).unwrap();
	let claim_clause = coin.policy().as_pubkey().unwrap().user_pubkey_claim_clause(coin.exit_delta());
	let clause: VtxoClause = claim_clause.clone().into();
	let mut spend = bitcoin::Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: coin.point(), script_sig: ScriptBuf::new(),
			sequence: clause.sequence().unwrap(), witness: bitcoin::Witness::new() }],
		// This 1,000 sat is a miner fee for the user's exit spend, not an expiry charge.
		output: vec![TxOut { value: coin.amount() - sat(1_000), script_pubkey: record.spk }],
	};
	let leaf_hash = bitcoin::taproot::TapLeafHash::from_script(&clause.tapscript(), bitcoin::taproot::LeafVersion::TapScript);
	let hash = bitcoin::sighash::SighashCache::new(&spend).taproot_script_spend_signature_hash(0,
		&bitcoin::sighash::Prevouts::All(&[coin.txout()]), leaf_hash, bitcoin::sighash::TapSighashType::Default).unwrap();
	let signature = ark::SECP.sign_schnorr(&hash.into(), &user_key);
	spend.input[0].witness = claim_clause.witness(&signature, &clause.control_block(&coin));
	core.send_raw_transaction(&spend).unwrap();
	ctx.generate_blocks(1).await;
	let payout = OutPoint::new(spend.compute_txid(), 0);
	let (discovered, recovered_spend) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	assert!(discovered.contains(&(payout, sat(19_000))));
	assert!(recovered_spend.input.iter().any(|i| i.previous_output == payout));
	assert!(core.get_tx_out(&coin.point().txid, coin.point().vout, Some(true)).unwrap().is_none());
}

/// Exercise the trusted admission boundary directly: even if an upstream
/// caller hands it a spend transition, a committed expiry allocation must win.
/// This is not an independently signed malicious transaction or a live payment.
#[tokio::test]
async fn frozen_expiry_source_refuses_competing_virtual_spend() {
	let AutomaticBoardFixture { ctx, srv, coin, .. } =
		automatic_board_fixture("frozen_expiry_source_refuses_competing_virtual_spend").await;
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let roots = [db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap()];
	let terminal = [ServerVtxo::from(coin.clone())];
	let residuals = partition_residuals(&roots, &terminal);
	let funding = core.get_raw_transaction(&coin.chain_anchor().txid, None).unwrap();
	let funding = BTreeMap::from([(funding.compute_txid(), funding)]);
	let canceled_receives = BTreeSet::new();
	let send_evidence = BTreeMap::new();
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(core.get_block_count().unwrap() as u32) + 1).await;
	let context = OwnershipContext { tip: core.get_block_count().unwrap() as u32, grace: 0,
		max_dependencies: 100, server_pubkey: coin.server_pubkey(), funding: &funding, canceled_receives: &canceled_receives };
	let budget = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
	let plan = db.write(async |t| t.freeze_expiry_sources(&roots, &terminal, &residuals,
		&context, &send_evidence, Network::Regtest, budget).await).await.unwrap();
	assert!(plan.allocation_ids.contains(&coin.id()));
	let before = db.read(async |t| t.get_user_vtxo_by_id(coin.id()).await).await.unwrap();
	assert_eq!(before.spend_state, SpendState::Spendable);
	// Use a transaction already stored by the real Board so a foreign-key
	// error cannot masquerade as expiry arbitration.
	let attempt = db.write(async |t| t.execute_vtxo_tree_update(server::database::tree::VtxoTreeUpdate::new()
		.mark_vtxos_oor_spent([(coin.id(), coin.point().txid)])).await).await;
	assert!(attempt.is_err(), "a committed expiry freeze must refuse a competing money-state transition");
	let refusal = format!("{:#}", attempt.unwrap_err());
	assert!(refusal.contains(server_rpc::EXPIRY_FROZEN_ERROR), "reservation must not look paid or be a different SQL failure: {refusal}");
	let after = db.read(async |t| t.get_user_vtxo_by_id(coin.id()).await).await.unwrap();
	assert_eq!(after.spend_state, SpendState::Spendable);
	assert_eq!(after.oor_spent_txid, None);
}

#[tokio::test]
async fn automatic_expired_board_pays_registered_recipient_from_actual_backing() {
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } =
		automatic_board_fixture_with_fee("automatic_expired_board_pays_registered_recipient_from_actual_backing", sat(1_000)).await;
	// Declare a confirmed fee reserve before expiry. The original wallet has
	// already stopped; neither daemon receives a manual freeze/sweep request.
	let fee_address = tokio::time::timeout(std::time::Duration::from_secs(30),
		srv.watchmand().wait_wallet_address()).await.expect("matching direct watchmand must start with a valid fee ceiling");
	// Same finite 50000-sat opening, split so selecting a complete fee input
	// can leave the native 40000-sat confirmed urgent floor untouched.
	let core = ctx.bitcoind().sync_client();
	let mut funding_points = BTreeSet::new();
	for _ in 0..5 {
		let txid = ctx.bitcoind().fund_addr(fee_address.clone(), sat(10_000)).await;
		let tx = core.get_raw_transaction(&txid, None).unwrap();
		let vout = tx.output.iter().position(|o| o.script_pubkey == fee_address.script_pubkey() && o.value == sat(10_000)).unwrap();
		assert!(funding_points.insert(OutPoint::new(txid, vout as u32)));
	}
	ctx.generate_blocks(3).await;
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let before = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap()))
		.await.unwrap().wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner().watchman.unwrap();
	assert!(before.complete && before.network == "regtest" && before.min_trusted_confirmations == 3);
	assert_eq!(before.utxos.len(), 5);
	assert!(before.reserved_outpoints.is_empty());
	assert_eq!(before.utxos.iter().map(|u|u.outpoint.parse::<OutPoint>().unwrap()).collect::<BTreeSet<_>>(),funding_points);
	assert!(before.utxos.iter().all(|u|u.value_sat==10_000 && u.confirmed_fee_eligible && u.confirmations>=3));
	let history = srv.config().data_dir.join("direct-expiry-witness");
	let policy_bytes = std::fs::read(history.join("ordinary-fee-policy.json")).unwrap();
	let policy:server::watchman::ordinary_fee::OrdinaryFeePolicy=serde_json::from_slice(&policy_bytes).unwrap();
	assert_eq!(policy,server::watchman::ordinary_fee::OrdinaryFeePolicy::default());
	let ledger_before = std::fs::read_to_string(history.join("ordinary-fee-budget")).unwrap();
	assert!(ledger_before.is_empty(),"fresh fixture must have its real empty history; never reset it");
	srv.watchmand().stop().await.unwrap();
	srv.stop().await.unwrap();
	let cap_cfg = srv.config().clone();
	let mut cap: toml::Value = toml::Value::try_from(&cap_cfg).unwrap();
	cap["expiry_payout"]["enabled"] = toml::Value::Boolean(false);
	cap.as_table_mut().unwrap().insert("direct_expiry".into(), toml::Value::Table(toml::map::Map::from_iter([
		("enabled".into(), toml::Value::Boolean(true)),
		("interval".into(), toml::Value::String("100ms".into())),
		("grace_blocks".into(), toml::Value::Integer(0)),
		("max_groups_per_tick".into(), toml::Value::Integer(2)),
	])));
	let cap_path = ctx.datadir.join("automatic-captaind.toml");
	write_automatic_config(&cap_path, &toml::to_string_pretty(&cap).unwrap());
	let mut watch = srv.watchmand().config().clone();
	watch.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(),
		history_dir: cap_cfg.data_dir.clone(), max_miner_fee: sat(10_000),
	});
	watch.watchman.reaction_interval = std::time::Duration::from_secs(1);
	watch.watchman.sweep_interval = std::time::Duration::from_secs(1);
	watch.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
	let watch_path = ctx.datadir.join("automatic-watchmand.toml");
	write_automatic_config(&watch_path, &toml::to_string_pretty(&watch).unwrap());
	let mut cap_daemon = spawn_automatic_daemon("CAPTAIND_EXEC", &cap_path, ctx.datadir.join("automatic-captaind.log"));
	let mut watch_daemon = spawn_automatic_daemon("WATCHMAND_EXEC", &watch_path, ctx.datadir.join("automatic-watchmand.log"));
	let core = ctx.bitcoind().sync_client();
	let height = core.get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	let backing = coin.chain_anchor();
	let carrier = tokio::time::timeout(std::time::Duration::from_secs(30), async {
		loop {
			assert!(cap_daemon.try_wait().unwrap().is_none(), "automatic captaind exited before payment");
			assert!(watch_daemon.try_wait().unwrap().is_none(), "automatic watchmand exited before payment");
			let spend = core.get_raw_mempool().unwrap().into_iter()
				.map(|id| core.get_raw_transaction(&id, None).unwrap())
				.find(|tx| tx.input.iter().any(|i| i.previous_output == backing));
			if let Some(tx) = spend { break tx; }
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		}
	}).await.expect("actual expired Board backing must have a real automatic spender");
	let expected = TxOut { value: coin.amount().checked_sub(sat(1_000)).unwrap(), script_pubkey: record.spk };
	let observed = carrier.output.iter().filter(|o| o.script_pubkey == expected.script_pubkey)
		.map(|o| o.value.to_sat()).collect::<Vec<_>>();
	let mut proof = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(ctx.datadir.join("automatic-backing-spender.tx")).unwrap();
	proof.write_all(&bitcoin::consensus::serialize(&carrier)).unwrap();
	println!("automatic route evidence: backing={}, spender={}, rightful_gross={}, expected_net={}, observed_recipient={observed:?}",
		backing, carrier.compute_txid(), coin.amount().to_sat(), expected.value.to_sat());
	let recipient_vout = carrier.output.iter().position(|o| *o == expected)
		.expect("the same transaction consuming actual Board funding must pay the exact registered gross-minus1000 output; a later two-step payment cannot earn direct-route credit");
	ctx.generate_blocks(1).await;
	assert!(core.get_raw_transaction_info(&carrier.compute_txid(), None).unwrap().confirmations.unwrap() >= 1);
	assert!(core.get_tx_out(&backing.txid, backing.vout, Some(true)).unwrap().is_none());
	let db = Db::connect(&cap_cfg.postgres).await.unwrap();
	tokio::time::timeout(std::time::Duration::from_secs(15), async {
		loop {
			let confirmed = db.read(async |t| Ok(t.query_one("SELECT confirmed_height FROM expiry_carrier WHERE txid=$1",
				&[&carrier.compute_txid().to_string()]).await?.get::<_, Option<i32>>(0))).await.unwrap();
			if confirmed.is_some() { break; }
			tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		}
	}).await.expect("actual daemon must reconcile the confirmed winner before recovery");
	assert_eq!(db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_carrier", &[]).await?
		.get::<_, i64>(0))).await.unwrap(), 1, "one automatic carrier, no second principal payout");
	let observed_height = core.get_block_count().unwrap();
	let observed_tip = core.get_block_hash(observed_height).unwrap().to_string();
	let after = tokio::time::timeout(std::time::Duration::from_secs(15), async {
		let mut client = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",watch.admin_address.unwrap())).await.unwrap();
		loop {
			let inventory = client.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner().watchman.unwrap();
			if inventory.complete && u64::from(inventory.tip_height)==observed_height && inventory.tip_hash==observed_tip { break inventory; }
			tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		}
	}).await.expect("owning fee wallet must actually observe the independent confirmed carrier tip");
	assert!(after.complete && after.network=="regtest" && after.min_trusted_confirmations==3);
	cap_daemon.kill().await.unwrap();
	cap_daemon.wait().await.unwrap();
	watch_daemon.kill().await.unwrap();
	watch_daemon.wait().await.unwrap();
	// Read the independent ledger only after both financial writers have stopped.
	// No reset, topup or reconstruction is performed to improve a measurement.
	let ledger_path = history.join("ordinary-fee-budget");
	assert!(std::fs::metadata(&ledger_path).unwrap().len()<=2*1024*1024);
	let ledger_after = std::fs::read_to_string(&ledger_path).unwrap();
	assert!(ledger_after.starts_with(&ledger_before) && ledger_after.ends_with('\n'));
	let charges = ledger_after.lines().map(|line| {
		let amount=line.parse::<u64>().unwrap();
		assert_eq!(line,amount.to_string());assert_eq!(amount,10_000);
		amount
	}).collect::<Vec<_>>();
	assert!(!charges.is_empty() && charges.len()<=6);
	assert!(charges.iter().sum::<u64>()<=60_000);
	assert_eq!(std::fs::read(history.join("ordinary-fee-policy.json")).unwrap(),policy_bytes);
	let versions = db.read(async |t| t.query("SELECT txid,generation,signed_tx,backing_ids,fee_inputs::text,miner_fee_sat,funding_subsidy_sat,confirmed_height FROM expiry_carrier ORDER BY generation",&[])
		.await.map_err(Into::into)).await.unwrap();
	assert_eq!(versions.len(),1,"preserve this original single-carrier fixture; RBF is not exercised here");
	assert!(versions.len()<=charges.len());
	let mut selected=BTreeSet::new();let mut measured=Vec::new();
	for row in &versions {
		let txid:bitcoin::Txid=row.get::<_,String>(0).parse().unwrap();
		assert_eq!(row.get::<_,Vec<String>>(3),vec![ark::VtxoId::from(backing).to_string()]);
		let signed:bitcoin::Transaction=bitcoin::consensus::deserialize(&row.get::<_,Vec<u8>>(2)).unwrap();
		let observed=core.get_raw_transaction(&txid,None).unwrap();
		assert_eq!(bitcoin::consensus::serialize(&signed),bitcoin::consensus::serialize(&observed));
		let input_sat=observed.input.iter().map(|i|core.get_raw_transaction(&i.previous_output.txid,None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		let output_sat=observed.output.iter().map(|o|o.value.to_sat()).sum::<u64>();
		let miner_fee=input_sat.checked_sub(output_sat).unwrap();
		assert_eq!(row.get::<_,i64>(5),miner_fee as i64);assert!(miner_fee<=10_000);
		assert!(observed.weight().to_wu()<=400_000);
		assert!(sat(miner_fee)<=FeeRate::from_sat_per_vb(50).unwrap().fee_wu(observed.weight()).unwrap());
		let fee_inputs:serde_json::Value=serde_json::from_str(&row.get::<_,String>(4)).unwrap();
		for item in fee_inputs.as_array().unwrap() {
			let point:OutPoint=item["outpoint"].as_str().unwrap().parse().unwrap();
			assert!(funding_points.contains(&point) && observed.input.iter().any(|i|i.previous_output==point));
			assert_eq!(item["value_sat"].as_u64(),Some(10_000));selected.insert(point);
		}
		assert_eq!(observed.input.iter().filter(|i|i.previous_output!=backing).map(|i|i.previous_output).collect::<BTreeSet<_>>(),selected);
		let fee_input_sat=fee_inputs.as_array().unwrap().iter().map(|i|i["value_sat"].as_u64().unwrap()).sum::<u64>();
		let change=after.utxos.iter().filter(|u|u.outpoint.parse::<OutPoint>().unwrap().txid==txid).collect::<Vec<_>>();
		let change_sat=change.iter().map(|u|u.value_sat).sum::<u64>();
		for u in &change {
			let point:OutPoint=u.outpoint.parse().unwrap();let out=&observed.output[point.vout as usize];
			assert_eq!(out.value.to_sat(),u.value_sat);assert_eq!(out.script_pubkey.as_bytes(),u.script_pubkey.as_slice());
		}
		let subsidy=u64::try_from(row.get::<_,i64>(6)).unwrap();
		assert_eq!(fee_input_sat.checked_sub(change_sat),Some(subsidy));
		let operator_script=watch.sweep_address.clone().unwrap().require_network(Network::Regtest).unwrap().script_pubkey();
		let change_indices=change.iter().map(|u|u.outpoint.parse::<OutPoint>().unwrap().vout as usize).collect::<BTreeSet<_>>();
		assert!(!change_indices.contains(&recipient_vout));
		let operator_sat=observed.output.iter().enumerate().filter(|(vout,_)| *vout!=recipient_vout && !change_indices.contains(vout))
			.map(|(_,out)| { assert_eq!(out.script_pubkey,operator_script);out.value.to_sat() }).sum::<u64>();
		assert_eq!(output_sat,expected.value.to_sat()+operator_sat+change_sat);
		let backing_sat=core.get_raw_transaction(&backing.txid,None).unwrap().output[backing.vout as usize].value.to_sat();
		assert_eq!(backing_sat+fee_input_sat,expected.value.to_sat()+operator_sat+change_sat+miner_fee);
		measured.push(serde_json::json!({"txid":txid,"generation":row.get::<_,i64>(1),
			"backing_ids":row.get::<_,Vec<String>>(3),"signed_weight_wu":observed.weight().to_wu(),
			"signed_vbytes":observed.vsize(),"actual_miner_fee_sat":miner_fee,
			"effective_sat_per_vbyte":{"numerator":miner_fee,"denominator":observed.vsize()},
			"funding_subsidy_sat":subsidy,"fee_wallet_change_sat":change_sat,"operator_surplus_sat":operator_sat,"confirmed_height":row.get::<_,Option<i32>>(7),
			"actual_fee_inputs":fee_inputs}));
	}
	let excluded=funding_points.difference(&selected).copied().collect::<Vec<_>>();
	assert!(excluded.len()>=4,"whole selected fee inputs must leave at least40000 confirmed unselected sats");
	for point in &excluded {
		let source=after.utxos.iter().find(|u|u.outpoint==point.to_string()).unwrap();
		assert!(source.confirmed_fee_eligible && source.value_sat==10_000 && source.confirmations>=3);
		let actual=core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap().unwrap();
		assert!(actual.confirmations>=3 && actual.value==sat(10_000));
	}
	let mappings=db.read(async |t| t.query("SELECT a.id,a.freeze_set,a.spk,a.gross_sat,a.fee_sat,a.payout_sat,m.txid,m.vout,a.disposition,a.owner_pubkey FROM expiry_allocation a JOIN expiry_carrier_allocation m ON m.allocation_id=a.id ORDER BY a.id,m.txid",&[])
		.await.map_err(Into::into)).await.unwrap();
	let paid=mappings.iter().filter(|r|r.get::<_,Option<i32>>(7).is_some()).collect::<Vec<_>>();
	assert_eq!(paid.len(),1);let map=paid[0];
	assert_eq!(map.get::<_,String>(8),"pay");
	assert_eq!(map.get::<_,Vec<u8>>(9),coin.user_pubkey().serialize());
	for residual in mappings.iter().filter(|r|r.get::<_,Option<i32>>(7).is_none()) {
		assert_eq!(residual.get::<_,String>(8),"operator");
		assert_eq!(residual.get::<_,Option<Vec<u8>>>(2),None);
		assert_eq!(residual.get::<_,Option<Vec<u8>>>(9),None);
		assert_eq!((residual.get::<_,i64>(4),residual.get::<_,i64>(5)),(0,0));
		assert_eq!(residual.get::<_,String>(1),map.get::<_,String>(1));
		assert_eq!(residual.get::<_,String>(6),carrier.compute_txid().to_string());
	}
	let frozen_gross=mappings.iter().map(|r|u64::try_from(r.get::<_,i64>(3)).unwrap()).sum::<u64>();
	assert_eq!(frozen_gross,core.get_raw_transaction(&backing.txid,None).unwrap().output[backing.vout as usize].value.to_sat());
	assert_eq!(map.get::<_,String>(0),coin.id().to_string());
	assert_eq!(map.get::<_,Vec<u8>>(2),expected.script_pubkey.as_bytes());
	assert_eq!((map.get::<_,i64>(3),map.get::<_,i64>(4),map.get::<_,i64>(5)),(20_000,1_000,19_000));
	assert_eq!(map.get::<_,String>(6),carrier.compute_txid().to_string());
	assert_eq!(map.get::<_,Option<i32>>(7),Some(recipient_vout as i32));
	let setup_fee_sat=funding_points.iter().map(|p|p.txid).collect::<BTreeSet<_>>().iter().map(|id| {
		let tx=core.get_raw_transaction(id,None).unwrap();
		let ins=tx.input.iter().map(|i|core.get_raw_transaction(&i.previous_output.txid,None).unwrap().output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		ins.checked_sub(tx.output.iter().map(|o|o.value.to_sat()).sum::<u64>()).unwrap()
	}).sum::<u64>();
	let estimate:serde_json::Value=core.call("estimatesmartfee",&[serde_json::json!(6)]).unwrap();
	let measurement=serde_json::json!({"scope":"one genuine registered Board/one direct carrier; not fullcase authority",
		"group_id":map.get::<_,String>(1),"allocation_id":coin.id().to_string(),
		"canonical_actual_output":{"txid":carrier.compute_txid(),"vout":recipient_vout,"spk":expected.script_pubkey.to_hex_string()},
		"gross_sat":20_000,"flat_actual_output_fee_sat":1_000,"payout_sat":19_000,
		"watchman_fee_wallet_opening_sat":50_000,"rounds_fixture_initial_funding_sat":ark_testing::btc(1).to_sat(),
		"setup_funding_miner_fee_sat":setup_fee_sat,"ordinary_policy":policy,
		"owning_before":{"wallet":before.wallet,"network":before.network,"tip_height":before.tip_height,"tip_hash":before.tip_hash,
			"reserved":before.reserved_outpoints,"utxos":before.utxos.iter().map(|u|serde_json::json!({"outpoint":u.outpoint,"value_sat":u.value_sat,"confirmations":u.confirmations,"confirmed_fee_eligible":u.confirmed_fee_eligible})).collect::<Vec<_>>()},
		"owning_after":{"wallet":after.wallet,"network":after.network,"tip_height":after.tip_height,"tip_hash":after.tip_hash,
			"reserved":after.reserved_outpoints,"utxos":after.utxos.iter().map(|u|serde_json::json!({"outpoint":u.outpoint,"value_sat":u.value_sat,"confirmations":u.confirmations,"confirmed_fee_eligible":u.confirmed_fee_eligible})).collect::<Vec<_>>()},
		"independent_Core":{"genesis":core.get_block_hash(0).unwrap(),"height":core.get_block_count().unwrap(),"tip":core.get_best_block_hash().unwrap()},
		"per_attempt_ceiling_sat":10_000,"source_derived_max_charged_attempts":6,
		"ledger_before":ledger_before,"ledger_after":ledger_after,"full_charge_entries_sat":charges,
		"charged_without_retained_version":charges.len()-versions.len(),"failed_build_attempts":serde_json::Value::Null,"failed_or_uncertain_attempt_classification":"requires original daemon logs; never infer failed-build count from signed rows alone",
		"retained_versions":measured,"observed_RBF_versions":0,
		"selected_fee_outpoints":selected.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"unselected_confirmed_eligible_outpoints":excluded.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"minimum_unselected_urgent_sat":40_000,"actual_unselected_sat":excluded.len()*10_000,
		"independent_Core_estimate6":estimate,"configured_max_rate_sat_vb":50,
		"fullcase_N":serde_json::Value::Null,"fullcase_C":serde_json::Value::Null,"fullcase_r":serde_json::Value::Null,
		"fixture_completion_pending_seed_recovery":true,"full_scale_or_stress_credit":false});
	let mut output=OpenOptions::new().write(true).create_new(true).mode(0o600).open(ctx.datadir.join("direct-fee-weight-ledger-measurement.json")).unwrap();
	output.write_all(&serde_json::to_vec_pretty(&measurement).unwrap()).unwrap();output.sync_all().unwrap();
	let (discovered, recovered_spend) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	// Compare only after the independent mnemonic-only scan and spend have finished.
	let payout = OutPoint::new(carrier.compute_txid(), recipient_vout as u32);
	assert!(discovered.contains(&(payout, expected.value)));
	assert!(recovered_spend.input.iter().any(|i| i.previous_output == payout));
}

fn write_automatic_config(path: &Path, text: &str) {
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();
	file.write_all(text.as_bytes()).unwrap();
	file.sync_all().unwrap();
}

fn spawn_automatic_daemon(exec: &str, config: &Path, log: PathBuf) -> Child {
	let output = OpenOptions::new().write(true).create_new(true).mode(0o600).open(log).unwrap();
	let child = Command::new(std::env::var_os(exec).unwrap()).arg("start").arg("--config").arg(config)
		.env_remove("RUST_MIN_STACK").stdout(output.try_clone().unwrap()).stderr(output).kill_on_drop(true).spawn().unwrap();
	let environment = std::fs::read(format!("/proc/{}/environ", child.id().unwrap())).unwrap();
	assert!(!environment.split(|v| *v == 0).any(|v| v.starts_with(b"RUST_MIN_STACK=")),
		"standalone daemon must use its ordinary production worker stack");
	child
}


async fn snapshot(db: &Db, roots: &[ServerVtxo], rows: usize, bytes: usize) -> anyhow::Result<BackingSnapshot> {
	db.read(async |t| t.backing_snapshot(roots, BackingSnapshotLimits {
		max_rows: rows, max_serialized_bytes: bytes,
	}).await).await
}

#[tokio::test]
async fn backing_snapshot_real_pending_registered_board_and_arkoor_inventory() {
	let ctx = TestContext::new("carrier-builder/backing_snapshot_real_pending_registered_board_and_arkoor_inventory").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().cfg(|c| c.fees.board.base_fee = sat(1_000)).watchmand_cfg(|c| {
		c.watchman.sweep_interval = std::time::Duration::from_secs(3600);
		c.watchman.reaction_interval = std::time::Duration::from_secs(3600);
	}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let sender = ctx.bark_sdk("sender", &srv).cfg(|c| c.daemon_manual_sync = true)
		.funded(sat(1_100_000)).create().await;
	sender.stop_daemon_wait().await.unwrap();
	let board = sender.board_amount(sat(100_000)).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	let coin = sender.get_full_vtxo(board.vtxos[0]).await.unwrap();
	let root = db.read(async |t| Ok(t.get_bare_vtxo_by_id(coin.chain_anchor().into()).await?.vtxo)).await.unwrap();
	let roots = [root];
	let pending = snapshot(&db, &roots, 100, 1_000_000).await.unwrap();
	let partition_limits = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
	let board_terminals = [ServerVtxo::from(coin.clone())];
	let board_residuals = partition_residuals(&roots, &board_terminals);
	let board_partition = pending.check_partition(&board_terminals, &[board.funding_tx.clone()], &board_residuals, partition_limits).unwrap();
	assert_eq!(board_partition.roots[0].terminal_amount, coin.amount());
	assert_eq!(board_partition.roots[0].anchor_residual_amount, sat(1_000));
	assert_eq!(coin.amount() + sat(1_000), roots[0].amount(), "priced Board anchor is explicit, never guessed operator money");
	assert!(pending.check_partition(&board_terminals, &[board.funding_tx.clone()], &[], partition_limits).is_err());
	let mut wrong_residual = board_residuals.clone(); wrong_residual[0].txout.value += sat(1);
	assert!(pending.check_partition(&board_terminals, &[board.funding_tx.clone()], &wrong_residual, partition_limits).is_err());

	assert_eq!(pending.regular.len(), 1, "only actual funding exists before Board registration");
	assert_eq!(pending.pending_boards.len(), 1);
	assert_eq!(pending.pending_boards[0].vtxo.id(), coin.id());
	assert_eq!(pending.pending_boards[0].anchor, coin.chain_anchor());
	assert_eq!(pending.pending_boards[0].genesis, SnapshotGenesis::EncodedPath);
	assert_eq!(pending.regular[0].genesis, SnapshotGenesis::Anchor);
	assert_eq!(pending.path_root(&ServerVtxo::from(coin.clone()), &board.funding_tx).unwrap(), Some(roots[0].id()));
	let mut wrong_funding = board.funding_tx.clone();
	wrong_funding.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
	assert!(pending.path_root(&ServerVtxo::from(coin.clone()), &wrong_funding).is_err(),
		"a transaction with the same funding output but different identity is not its backing");
	assert_eq!(pending.serialized_bytes, roots[0].serialize().len() + pending.pending_boards[0].vtxo.serialize().len());
	assert!(snapshot_error(snapshot(&db, &roots, 1, 1_000_000).await).contains("row budget"));
	assert!(snapshot_error(snapshot(&db, &roots, 2, pending.serialized_bytes - 1).await).contains("byte budget"));
	assert!(snapshot(&db, &roots, 2, pending.serialized_bytes).await.is_ok());
	let absent_source = ServerVtxo::from(pending.pending_boards[0].vtxo.to_bare());
	assert!(snapshot_error(snapshot(&db, &[absent_source], 100, 1_000_000).await).contains("no source row"),
		"an actual cosigned but unregistered user output is not a regular physical-root row");
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	sender.sync_onchain().await.unwrap();
	sender.sync_pending_boards().await.unwrap();
	let registered = snapshot(&db, &roots, 100, 1_000_000).await.unwrap();
	assert!(registered.pending_boards.is_empty(), "retained pending Board cannot duplicate registered user leaf");
	assert_eq!(registered.regular.len(), 2);
	assert_eq!(registered.regular.iter().find(|f| f.state.vtxo_id == coin.id()).unwrap().state.spend_state, SpendState::Spendable);
	let retained: i64 = db.read(async |t| Ok(t.query_one("SELECT count(*) FROM pending_board WHERE id=$1",
		&[&coin.chain_anchor().to_string()]).await?.get(0))).await.unwrap();
	assert_eq!(retained, 1, "deduplication must be real, not disappearance of pending source");

	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|c| c.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	sender.send_arkoor_payment(&address, sat(40_000)).await.unwrap();
	recipient.sync_mailbox().await.unwrap();
	let received = recipient.spendable_vtxos().await.unwrap();
	let change = sender.spendable_vtxos().await.unwrap();
	assert_eq!(received.len(), 1);
	assert!(!change.is_empty());
	assert_eq!(change.iter().map(|v| v.amount()).sum::<Amount>(), coin.amount() - sat(40_000));
	let inventory = snapshot(&db, &roots, 100, 1_000_000).await.unwrap();
	let mut terminal_sources = Vec::new();
	for v in &received { terminal_sources.push(ServerVtxo::from(recipient.get_full_vtxo(v.id()).await.unwrap())); }
	for v in &change { terminal_sources.push(ServerVtxo::from(sender.get_full_vtxo(v.id()).await.unwrap())); }
	let residuals = partition_residuals(&roots, &terminal_sources);
	let partition = inventory.check_partition(&terminal_sources, &[board.funding_tx.clone()], &residuals, partition_limits).unwrap();
	assert_eq!(partition.roots[0].terminal_amount, coin.amount());
	assert_eq!(partition.roots[0].anchor_residual_amount, sat(1_000));
	assert_eq!(partition.roots[0].terminals.len(), terminal_sources.len());
	assert!(residuals.len() > 1 && residuals.iter().any(|r| r.txout.value == Amount::ZERO), "all zero anchors are explicit too");
	for omitted in 0..terminal_sources.len() {
		let subset = terminal_sources.iter().enumerate().filter(|(i, _)| *i != omitted).map(|(_, v)| v.clone()).collect::<Vec<_>>();
		assert!(subset.iter().all(|v| inventory.path_root(v, &board.funding_tx).unwrap() == Some(roots[0].id())),
			"individual parent path matches do not prove missing-sibling completeness");
		assert!(inventory.check_partition(&subset, &[board.funding_tx.clone()], &residuals, partition_limits).is_err(), "missing terminal refuses the whole cut");
	}
	let mut overlapping = terminal_sources.clone(); overlapping.push(ServerVtxo::from(coin.clone()));
	assert!(inventory.check_partition(&overlapping, &[board.funding_tx.clone()], &residuals, partition_limits).is_err(), "ancestor and descendant cannot cover the same money");
	let mut duplicate = terminal_sources.clone(); duplicate.push(terminal_sources[0].clone());
	assert!(inventory.check_partition(&duplicate, &[board.funding_tx.clone()], &residuals, partition_limits).is_err());
	let mut wrong_identity = board.funding_tx.clone(); wrong_identity.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
	assert!(inventory.check_partition(&terminal_sources, &[wrong_identity], &residuals, partition_limits).is_err());
	let mut wrong_output = board.funding_tx.clone(); wrong_output.output[coin.chain_anchor().vout as usize].value += sat(1);
	assert!(inventory.check_partition(&terminal_sources, &[wrong_output], &residuals, partition_limits).is_err());

	assert!(inventory.pending_boards.is_empty());
	assert!(inventory.regular.len() > registered.regular.len());
	let old = inventory.regular.iter().find(|f| f.state.vtxo_id == coin.id()).unwrap();
	assert_eq!(old.state.spend_state, SpendState::Spent, "spent user source is retained");
	assert!(old.state.oor_spent_txid.is_some());
	for id in std::iter::once(received[0].id()).chain(change.iter().map(|v| v.id())) {
		let fact = inventory.regular.iter().find(|f| f.state.vtxo_id == id).unwrap();
		assert_eq!(fact.state.spend_state, SpendState::Spendable);
		assert_eq!(fact.genesis, SnapshotGenesis::EncodedPath);
		fact.full_vtxo.as_ref().unwrap().validate_unsigned(&board.funding_tx).unwrap();
		assert_eq!(inventory.path_root(fact.full_vtxo.as_ref().unwrap(), &board.funding_tx).unwrap(), Some(roots[0].id()));
	}
	// A source aggregate independently counts every state, not just the known
	// payable leaves; the reader must not suppress intermediates or spent rows.
	let aggregate = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) AS rows, sum(octet_length(vtxo))::bigint AS bytes FROM vtxo WHERE anchor_point=$1",
		&[&coin.chain_anchor().to_string()],
	).await?)).await.unwrap();
	let count = aggregate.get::<_, i64>("rows") as usize;
	let bytes = aggregate.get::<_, i64>("bytes") as usize;
	assert_eq!(inventory.regular.len(), count);
	assert_eq!(inventory.serialized_bytes, bytes);
	assert!(snapshot_error(snapshot(&db, &roots, count - 1, bytes).await).contains("row budget"));
	assert!(snapshot_error(snapshot(&db, &roots, count, bytes - 1).await).contains("byte budget"));
	let exact = snapshot(&db, &roots, count, bytes).await.unwrap();
	assert_eq!(exact.regular.iter().map(|f| f.state.vtxo_id).collect::<Vec<_>>(),
		inventory.regular.iter().map(|f| f.state.vtxo_id).collect::<Vec<_>>(), "stable order");
	assert!(exact.regular.windows(2).all(|pair| pair[0].state.vtxo_id.to_string() < pair[1].state.vtxo_id.to_string()));

	// A progressed output retains the same chain anchor. Its lookup deliberately
	// contains a sibling and the spent ancestor: neither is inferred as covered.
	let progressed = inventory.regular.iter().find(|f| f.state.vtxo_id == received[0].id()).unwrap();
	let progressed_roots = [progressed.state.vtxo.to_bare()];
	let superset = snapshot(&db, &progressed_roots, count, bytes).await.unwrap();
	assert!(change.iter().all(|v| superset.regular.iter().any(|f| f.state.vtxo_id == v.id())));
	let actual_path = progressed.full_vtxo.as_ref().unwrap().transactions().map(|item| OutPoint::new(item.tx.compute_txid(), item.output_idx as u32)).collect::<Vec<_>>();
	assert!(change.iter().all(|v| !actual_path.contains(&v.point())), "sibling outpoints are not on this validated full path");
	assert_eq!(superset.path_root(progressed.full_vtxo.as_ref().unwrap(), &board.funding_tx).unwrap(), Some(progressed_roots[0].id()));
	for sibling in &change {
		let fact = superset.regular.iter().find(|f| f.state.vtxo_id == sibling.id()).unwrap();
		assert_eq!(superset.path_root(fact.full_vtxo.as_ref().unwrap(), &board.funding_tx).unwrap(), None,
			"same-anchor sibling is outside the supplied progressed backing");
	}
	let overlap = snapshot(&db, &[roots[0].clone(), progressed_roots[0].clone()], count, bytes).await.unwrap();
	assert!(overlap.path_root(progressed.full_vtxo.as_ref().unwrap(), &board.funding_tx).is_err(),
		"an ancestor and its own descendant cannot count the same claim path twice");
	let sibling_root = inventory.regular.iter().find(|f| f.state.vtxo_id == change[0].id()).unwrap().state.vtxo.to_bare();
	let paired_roots = [progressed_roots[0].clone(), sibling_root];
	let progressed_source = [progressed.full_vtxo.clone().unwrap()];
	assert!(superset.check_partition(&progressed_source, &[board.funding_tx.clone()], &[], partition_limits).is_ok(),
		"the physical output itself is a geometric cut; this confers no current economic-terminal status");
	assert!(superset.check_partition(&terminal_sources, &[board.funding_tx.clone()], &[], partition_limits).is_err(), "same-anchor sibling is outside progressed backing");
	assert!(superset.check_partition(&progressed_source, &[board.funding_tx.clone()], &board_residuals, partition_limits).is_err(), "prior Board anchor is outside progressed backing");
	assert!(overlap.check_partition(&terminal_sources, &[board.funding_tx.clone()], &residuals, partition_limits).is_err(), "overlapping roots cannot partition a cut");
	let paired = snapshot(&db, &paired_roots, count, bytes).await.unwrap();
	let paired_sources = terminal_sources.iter().filter(|v| paired_roots.iter().any(|r| r.point() == v.point())).cloned().collect::<Vec<_>>();
	assert_eq!(paired_sources.len(), 2);
	if terminal_sources.len() > paired_sources.len() {
		assert!(paired.check_partition(&terminal_sources, &[board.funding_tx.clone()], &[], partition_limits).is_err(),
			"fragmented sender change outside selected roots must not be silently assigned to the pair");
	}
	let paired_partition = paired.check_partition(&paired_sources, &[board.funding_tx.clone()], &[], partition_limits).unwrap();
	assert_eq!(paired_partition.roots.len(), 2);
	assert!(paired_partition.roots.iter().all(|p| p.anchor_residual_amount == Amount::ZERO));

	assert_eq!(snapshot(&db, &paired_roots, count, bytes).await.unwrap().regular.len(), count, "shared anchors are inventoried once");
	assert!(snapshot_error(snapshot(&db, &[roots[0].clone(), roots[0].clone()], 100, 1_000_000).await).contains("duplicate"));
	assert!(snapshot_error(snapshot(&db, &roots, 0, bytes).await).contains("positive"));
	// Observe real chain facts through the normal watchman/indexed-spend path,
	// without editing source rows. First synchronize confirmed live funding,
	// then relay the client's actual signed Board transaction spending it.
	let core = ctx.bitcoind().sync_client();
	srv.watchmand().trigger_sweep().await;
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let live = snapshot(&db, &roots, count, bytes).await.unwrap();
	let funding_fact = live.regular.iter().find(|f| f.state.vtxo_id == roots[0].id()).unwrap();
	assert!(funding_fact.frontier_at.is_some());
	assert!(funding_fact.state.confirmed_height.is_some());
	assert_eq!(funding_fact.onchain_spent_txid, None);
	let board_exit = coin.transactions().next().unwrap().tx;
	assert!(board_exit.input.iter().any(|i| i.previous_output == roots[0].point()));
	ctx.broadcast_cpfp(&board_exit).await;
	ctx.generate_blocks(1).await;
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let retired = snapshot(&db, &roots, count, bytes).await.unwrap();
	let funding_fact = retired.regular.iter().find(|f| f.state.vtxo_id == roots[0].id()).unwrap();
	assert_eq!(funding_fact.onchain_spent_txid, Some(board_exit.compute_txid()));
	assert_eq!(funding_fact.onchain_spent_height, Some(BlockHeight::new(core.get_block_count().unwrap() as u32)));
	assert_eq!(retired.regular.len(), count, "physical retirement does not discard source history");
	let mut proof = OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(ctx.datadir.join("snapshot-board-exit.tx")).unwrap();
	proof.write_all(&bitcoin::consensus::serialize(&board_exit)).unwrap();
	let summary = serde_json::json!({
		"scope": "raw source snapshot, no rights/ownership/automatic-route qualification",
		"pending_boards": 1, "registered_rows": 2, "arkoor_rows": count, "serialized_vtxo_bytes": bytes,
		"geometry_only_partition": { "terminals": terminal_sources.len(), "explicit_anchors": residuals.len(),
			"terminal_sat": partition.roots[0].terminal_amount.to_sat(), "anchor_residual_sat": partition.roots[0].anchor_residual_amount.to_sat(),
			"missing_leaf_overlap_sibling_and_funding_refusal": true, "current_economic_cut_qualified": false },
		"observed_states": retired.regular.iter().map(|f| f.state.spend_state.as_str()).collect::<std::collections::BTreeSet<_>>(),
		"funding_point": roots[0].point().to_string(), "actual_confirmed_spender": board_exit.compute_txid().to_string(),
		"actual_spent_height": funding_fact.onchain_spent_height.unwrap().to_u32(),
	});
	std::fs::write(ctx.datadir.join("snapshot-summary.json"), serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
	println!("raw backing snapshot only: pending=1, registered=2, arkoor_rows={count}, serialized_bytes={bytes}; complete budgets, raw superset and real chain facts passed");
}


// Independent fixture inventory from actual full transactions, not a replacement
// for production coverage validation. Include only anchors below supplied roots.
fn partition_residuals(roots: &[ServerVtxo], sources: &[ServerVtxo<ark::vtxo::Full>]) -> Vec<BackingPartitionResidual> {
	let root_points = roots.iter().map(|v| v.point()).collect::<std::collections::BTreeSet<_>>();
	let mut anchors = std::collections::BTreeMap::new();
	for source in sources {
		let mut inside = false;
		for item in source.transactions() {
			inside |= root_points.contains(&item.tx.input[0].previous_output);
			if inside {
				let txid = item.tx.compute_txid();
				for (vout, txout) in item.tx.output.into_iter().enumerate() {
					if txout.is_p2a_fee_anchor() { anchors.insert(OutPoint::new(txid, vout as u32), txout); }
				}
			}
		}
	}
	anchors.into_iter().map(|(outpoint, txout)| BackingPartitionResidual { outpoint, txout }).collect()
}

fn snapshot_error(result: anyhow::Result<BackingSnapshot>) -> String {
	match result { Ok(_) => panic!("expected whole-snapshot refusal"), Err(err) => format!("{err:#}") }
}


async fn wallet_changeset_fingerprint(db: &Db, kind: WalletKind) -> (i64, Option<String>) {
	db.read(async |t| {
		let row = t.query_one("SELECT count(*) AS count, md5(string_agg(md5(content), ',' ORDER BY id)) AS digest FROM wallet_changeset WHERE kind=$1::text::wallet_kind", &[&kind.name()]).await?;
		Ok((row.get("count"), row.get("digest")))
	}).await.unwrap()
}

#[tokio::test]
async fn wallet_inventory_actual_owner_rpc_and_helper_do_not_mutate_wallet() {
	let ctx = TestContext::new("carrier-builder/wallet_inventory_actual_owner_rpc_and_helper_do_not_mutate_wallet").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(sat(1_000_000)).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let mut rpc = srv.get_wallet_rpc().await;
	let first = rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner();
	assert!(!first.complete, "captaind rounds inventory cannot stand in for watchmand's actual fee wallet");
	assert!(first.watchman.is_none());
	assert!(!first.missing_sources.is_empty());
	let rounds = first.rounds.as_ref().unwrap();
	assert!(rounds.complete);
	assert_eq!(rounds.wallet, "rounds");
	assert_eq!(rounds.network, "regtest");
	assert!(!rounds.utxos.is_empty());
	assert!(rounds.reserved_outpoints.is_empty());
	let core = ctx.bitcoind().sync_client();
	let descriptor_info: serde_json::Value = core.call("getdescriptorinfo", &[serde_json::json!(rounds.public_descriptor)]).unwrap();
	assert_eq!(descriptor_info["hasprivatekeys"], false);
	assert_eq!(descriptor_info["isrange"], true);
	assert_eq!(rounds.tip_height as u64, core.get_block_count().unwrap());
	assert_eq!(rounds.tip_hash, core.get_block_hash(rounds.tip_height as u64).unwrap().to_string());
	for output in &rounds.utxos {
		let point = OutPoint::from_str(&output.outpoint).unwrap();
		let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
		assert_eq!(output.value_sat, actual.value.to_sat());
		assert_eq!(output.script_pubkey, actual.script_pub_key.hex);
		assert!(output.confirmed_height_upper_bound.is_some());
		assert!(output.trusted && output.mature && output.transaction_eligible && output.confirmed_fee_eligible);
		assert!(!output.reserved && !output.coinbase);
	}
	let before_db = wallet_changeset_fingerprint(&db, WalletKind::Rounds).await;
	for _ in 0..3 {
		let next = rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner();
		assert_eq!(next, first, "repeated reads must not reveal or change the canonical inventory");
	}
	let cli = tokio::process::Command::new(std::env::var("CAPTAIND_EXEC").unwrap())
		.args(["rpc", "--addr", &srv.config().rpc.admin_address.unwrap().to_string(), "wallet-inventory"])
		.output().await.unwrap();
	assert!(cli.status.success(), "fixed inventory CLI must succeed");
	assert!(cli.stdout.len() <= 2 * 1024 * 1024);
	let cli_json: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
	assert_eq!(cli_json["complete"], false);
	assert!(cli_json["watchman"].is_null());
	assert_eq!(cli_json["rounds"]["public_descriptor"], rounds.public_descriptor);
	assert_eq!(cli_json["rounds"]["last_revealed_index"], serde_json::json!(rounds.last_revealed_index));
	std::fs::write(ctx.datadir.join("rounds-wallet-inventory.json"), &cli.stdout).unwrap();
	let after_db = wallet_changeset_fingerprint(&db, WalletKind::Rounds).await;
	assert!(before_db == after_db, "owning-daemon inventory must not append or rewrite persisted wallet state");

	// Real immature coinbase must not become eligible merely because canonical
	// trust meets the configured confirmation threshold.
	let mining_address = Address::from_script(&ScriptBuf::from_bytes(rounds.utxos[0].script_pubkey.clone()), Network::Regtest).unwrap();
	core.generate_to_address(1, &mining_address).unwrap();
	srv.wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let immature_source = rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner();
	let rounds = immature_source.rounds.as_ref().unwrap();
	let immature = rounds.utxos.iter().find(|u| u.coinbase).unwrap();
	assert_eq!(immature.confirmations, 1);
	assert!(immature.trusted);
	assert!(!immature.mature && !immature.transaction_eligible && !immature.confirmed_fee_eligible);
	let before_db = wallet_changeset_fingerprint(&db, WalletKind::Rounds).await;

	// Stop its sole owner, then reload the same existing rounds wallet to inspect
	// staged changes and live guard behavior directly. No second wallet is created.
	srv.stop().await.unwrap();
	let mnemonic = wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let tip = BlockRef { height: BlockHeight::new(rounds.tip_height),
		hash: core.get_block_hash(rounds.tip_height as u64).unwrap() };
	let existing = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(),
		Network::Regtest, &master, WalletKind::Rounds, tip, rounds.min_trusted_confirmations).await.unwrap();
	let wallet = InstrumentedLock::new("test_existing_rounds_inventory", existing);
	let held = wallet.lock_owned().await;
	let staged_before = held.staged().cloned();
	let index_before = held.derivation_index(KEYCHAIN);
	let snapshot = held.inventory().unwrap();
	assert_eq!(snapshot, *rounds);
	let point = OutPoint::from_str(&snapshot.utxos.iter().find(|u| u.confirmed_fee_eligible).unwrap().outpoint).unwrap();
	let reservation = held.lock_wallet_utxo(point).unwrap();
	let reserved = held.inventory().unwrap();
	let row = reserved.utxos.iter().find(|u| u.outpoint == point.to_string()).unwrap();
	assert!(row.reserved);
	assert!(!row.transaction_eligible && !row.confirmed_fee_eligible);
	assert_eq!(reserved.reserved_outpoints, vec![point.to_string()]);
	assert!(held.lock_wallet_utxo(point).is_err(), "read preserves the existing reservation");
	assert!(held.staged() == staged_before.as_ref(), "inventory must not mutate staged BDK state");
	assert_eq!(held.derivation_index(KEYCHAIN), index_before);
	assert!(before_db == wallet_changeset_fingerprint(&db, WalletKind::Rounds).await, "direct helper must not persist");
	drop(reservation);
	let released = held.inventory().unwrap();
	assert_eq!(released, snapshot);
	std::fs::write(ctx.datadir.join("wallet-inventory-summary.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"scope": "actual rounds-wallet source only; watchmand unavailable; financial readiness incomplete",
		"utxos": rounds.utxos.len(), "value_sat": rounds.utxos.iter().map(|u| u.value_sat).sum::<u64>(),
		"tip_height": rounds.tip_height, "tip_hash": rounds.tip_hash,
		"public_descriptor_core_checked": true, "private_keys_exported": false,
		"owner_rpc_repeated_reads": 4, "fixed_cli_json_checked": true, "real_immature_coinbase_refused": true, "address_index_unchanged": true,
		"staged_state_unchanged": true, "persisted_state_unchanged": true, "reservations_preserved": true,
		"watchman_inventory": null, "complete": false,
	})).unwrap()).unwrap();
}


#[tokio::test]
async fn wallet_inventory_watchmand_actual_owner_and_existing_admin_socket() {
	let ctx = TestContext::new("carrier-builder/wallet_inventory_watchmand_actual_owner_and_existing_admin_socket").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().watchmand_cfg(|c| {
		c.min_trusted_confs = 3;
		c.watchman.sweep_interval = std::time::Duration::from_secs(3600);
		c.watchman.reaction_interval = std::time::Duration::from_secs(3600);
	}).create().await;
	let watchmand = srv.watchmand();
	let addr = watchmand.config().admin_address.unwrap().to_string();
	let mut rpc = tokio::time::timeout(std::time::Duration::from_secs(20), async {
		loop {
			if let Ok(client) = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{addr}")).await {
				break client;
			}
			tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		}
	}).await.expect("existing watchmand admin listener must become ready");
	assert_eq!(rpc.wallet_status(server_rpc::protos::Empty {}).await.unwrap_err().code(), tonic::Code::Unimplemented,
		"legacy address-revealing wallet status must not be activated on watchmand");
	let address = watchmand.wait_wallet_address().await;
	ctx.bitcoind().fund_addr(address, sat(1_000_000)).await;
	ctx.generate_blocks(1).await;
	let core = ctx.bitcoind().sync_client();
	watchmand.wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let one_conf = rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner();
	let one_conf_wallet = one_conf.watchman.as_ref().unwrap();
	assert_eq!(one_conf_wallet.min_trusted_confirmations, 3);
	assert_eq!(one_conf_wallet.utxos.len(), 1);
	assert_eq!(one_conf_wallet.utxos[0].confirmations, 1);
	assert!(!one_conf_wallet.utxos[0].trusted && !one_conf_wallet.utxos[0].confirmed_fee_eligible);
	ctx.generate_blocks(2).await;
	watchmand.wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	let first = rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner();
	assert!(first.rounds.is_none() && !first.complete && !first.missing_sources.is_empty());
	let fee_wallet = first.watchman.as_ref().unwrap();
	assert!(fee_wallet.complete);
	assert_eq!(fee_wallet.wallet, "watchman");
	assert_eq!(fee_wallet.tip_height as u64, core.get_block_count().unwrap());
	assert_eq!(fee_wallet.tip_hash, core.get_block_hash(fee_wallet.tip_height as u64).unwrap().to_string());
	let info: serde_json::Value = core.call("getdescriptorinfo", &[serde_json::json!(fee_wallet.public_descriptor)]).unwrap();
	assert_eq!(info["hasprivatekeys"], false);
	let row = &fee_wallet.utxos[0];
	assert_eq!(row.confirmations, 3);
	assert!(row.trusted && row.mature && row.transaction_eligible && row.confirmed_fee_eligible && !row.reserved);
	let point = OutPoint::from_str(&row.outpoint).unwrap();
	let actual = core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().unwrap();
	assert_eq!(row.value_sat, actual.value.to_sat());
	assert_eq!(row.script_pubkey, actual.script_pub_key.hex);
	let rounds_source = srv.get_wallet_rpc().await.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner();
	assert_ne!(rounds_source.rounds.as_ref().unwrap().public_descriptor, fee_wallet.public_descriptor);
	assert!(rounds_source.watchman.is_none() && !rounds_source.complete,
		"separate owner responses remain partial; matching tips do not make them atomic");
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let before = wallet_changeset_fingerprint(&db, WalletKind::Watchman).await;
	for _ in 0..3 {
		assert_eq!(rpc.wallet_inventory(server_rpc::protos::Empty {}).await.unwrap().into_inner(), first);
	}
	let cli = tokio::process::Command::new(std::env::var("CAPTAIND_EXEC").unwrap())
		.args(["rpc", "--addr", &addr, "wallet-inventory"]).output().await.unwrap();
	assert!(cli.status.success());
	assert!(cli.stdout.len() <= 2 * 1024 * 1024);
	let json: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
	assert!(json["rounds"].is_null());
	assert_eq!(json["watchman"]["public_descriptor"], fee_wallet.public_descriptor);
	assert_eq!(json["complete"], false);
	assert!(before == wallet_changeset_fingerprint(&db, WalletKind::Watchman).await,
		"actual watchmand RPC and CLI cannot mutate persisted wallet state");
	std::fs::write(ctx.datadir.join("watchman-wallet-inventory.json"), &cli.stdout).unwrap();
	watchmand.stop().await.unwrap();
	srv.stop().await.unwrap();
	let mnemonic = wallet::read_mnemonic_from_datadir(&watchmand.config().data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let tip = BlockRef { height: BlockHeight::new(fee_wallet.tip_height), hash: core.get_block_hash(fee_wallet.tip_height as u64).unwrap() };
	let existing = PersistedWallet::load_derive_from_master_xpriv(db.clone(), ctx.bitcoind().async_client(), Network::Regtest,
		&master, WalletKind::Watchman, tip, fee_wallet.min_trusted_confirmations).await.unwrap();
	let lock = InstrumentedLock::new("test_existing_watchman_inventory", existing);
	let held = lock.lock_owned().await;
	let staged = held.staged().cloned();
	let index = held.derivation_index(KEYCHAIN);
	assert_eq!(held.inventory().unwrap(), *fee_wallet);
	let reservation = held.lock_wallet_utxo(point).unwrap();
	let reserved = held.inventory().unwrap();
	assert_eq!(reserved.reserved_outpoints, vec![point.to_string()]);
	assert!(reserved.utxos[0].reserved && !reserved.utxos[0].confirmed_fee_eligible);
	assert!(held.lock_wallet_utxo(point).is_err());
	assert!(held.staged() == staged.as_ref());
	assert_eq!(held.derivation_index(KEYCHAIN), index);
	assert!(before == wallet_changeset_fingerprint(&db, WalletKind::Watchman).await);
	drop(reservation);
	assert_eq!(held.inventory().unwrap(), *fee_wallet);
	std::fs::write(ctx.datadir.join("watchman-inventory-summary.json"), serde_json::to_vec_pretty(&serde_json::json!({
		"scope": "actual watchmand-owned fee-wallet source; separate daemon frames are not globally atomic",
		"utxos": fee_wallet.utxos.len(), "value_sat": fee_wallet.utxos.iter().map(|u| u.value_sat).sum::<u64>(),
		"tip_height": fee_wallet.tip_height, "tip_hash": fee_wallet.tip_hash,
		"minimum_confirmations": 3, "one_confirmation_refused": true, "three_confirmations_eligible": true,
		"owner_rpc_repeated_reads": 4, "private_keys_exported": false, "legacy_wallet_status_unimplemented": true,
		"existing_admin_socket": true, "staged_state_unchanged": true, "address_index_unchanged": true,
		"persisted_state_unchanged": true, "reservations_preserved": true, "complete": false,
	})).unwrap()).unwrap();
}

#[tokio::test]
async fn complete_pending_group_listing_survives_more_groups_than_its_query_page() {
	let ctx = TestContext::new("carrier-builder/complete_pending_group_listing_survives_more_groups_than_its_query_page").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().funded(ark_testing::btc(1))
		.cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fees.board.base_fee = sat(1_000);
			c.direct_expiry.enabled = true;
			c.direct_expiry.grace_blocks = 0;
			c.direct_expiry.interval = std::time::Duration::from_secs(3600);
		}).watchmand_cfg(|c| {
			c.watchman.direct_expiry = Some(server::watchman::DirectExpiryConfig { ordinary_fee_policy: Default::default(),
				history_dir: c.data_dir.parent().unwrap().join("server"), max_miner_fee: sat(10_000),
			});
			c.fee_estimator.max_fee_rate = Some(FeeRate::from_sat_per_vb(50).unwrap());
			c.watchman.sweep_interval = std::time::Duration::from_secs(3600);
			c.watchman.reaction_interval = std::time::Duration::from_secs(3600);
		}).create().await;
	let mut coins = Vec::new();
	for name in ["absent-a", "absent-b"] {
		let wallet = ctx.bark_sdk(name, &srv).cfg(|c| c.daemon_manual_sync = true).boarded(sat(21_000)).create().await;
		wallet.stop_daemon_wait().await.unwrap();
		coins.push(wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap());
		drop(wallet);
	}
	let config = srv.config().clone();
	srv.stop().await.unwrap();
	let server = server::Server::start(config).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let expiry = coins.iter().map(|c| c.expiry_height().to_u32()).max().unwrap();
	ctx.generate_blocks(expiry.saturating_sub(core.get_block_count().unwrap() as u32) + 3).await;
	let budget = BackingSnapshotLimits { max_rows: 100, max_serialized_bytes: 1_000_000 };
	let mut expected = BTreeSet::new();
	for coin in &coins {
		let plan = server.freeze_current_expiry_backing(&[coin.chain_anchor().into()], &[coin.id()], budget).await.unwrap();
		expected.insert(plan.id);
	}
	assert_eq!(expected.len(), 2);
	// A queue page is not an ownership group: all groups must be returned so
	// startup can restore every durable fee reservation before new selection.
	let listed = server.database().frozen_expiry_group_ids(1).await.unwrap();
	assert_eq!(listed.iter().cloned().collect::<BTreeSet<_>>(), expected);
	assert_eq!(listed.len(), 2, "the bounded reader must not silently return a prefix");
	for id in listed {
		let group = server.database().read_frozen_expiry_group(&id, Network::Regtest, budget).await.unwrap();
		assert_eq!(group.plan.recipients.len(), 1);
		assert_eq!(group.plan.recipients[0].value, sat(19_000));
	}
}
