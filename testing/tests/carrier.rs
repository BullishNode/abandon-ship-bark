//! Real operator-owned backing proves construction/signing/relay only.
//! Exact supplied outputs here do not claim resolved customer entitlement.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::str::FromStr;

use bitcoin::{Address, Amount, FeeRate, Network, OutPoint, TxOut, Weight};
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::secp256k1::Keypair;
use bitcoincore_rpc::RpcApi;

use ark::board::BoardBuilder;
use ark::ServerVtxo;
use bitcoin_ext::{BlockDelta, BlockHeight, BlockRef};
use bitcoin_ext::bdk::{WalletExt, KEYCHAIN};
use server::database::Db;
use server::secret::Secret;
use server::utils::InstrumentedLock;
use server::wallet::{self, BdkWalletExt, PersistedWallet, WalletKind};
use server::watchman::{WatchmanSigner, carrier::{build_direct_carrier, CarrierLimits}};

use ark_testing::{sat, TestContext};

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
	let ctx = TestContext::new(format!("carrier-builder/{name}")).await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(ark_testing::btc(1)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
		}).watchmand_cfg(|c| {
			c.watchman.sweep_interval = std::time::Duration::from_secs(3600);
			c.watchman.reaction_interval = std::time::Duration::from_secs(3600);
		}).create().await;
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("absent", &srv).mnemonic(mnemonic.clone())
		.cfg(|c| c.daemon_manual_sync = true).boarded(sat(20_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let coins = wallet.spendable_vtxos().await.unwrap();
	assert_eq!(coins.len(), 1, "actual native Board must register before route qualification");
	let coin = wallet.get_full_vtxo(coins[0].id()).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	let mailbox = wallet.mailbox_keypair().public_key().serialize().to_vec();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let stored = db.read(async |t| Ok(t.query_one(
		"SELECT spk, account_xpub FROM fallback_record WHERE mailbox_pk=$1", &[&mailbox],
	).await?)).await.unwrap();
	assert_eq!(stored.get::<_, Vec<u8>>("spk"), record.spk.as_bytes());
	assert_eq!(stored.get::<_, Vec<u8>>("account_xpub"), record.account_xpub.encode());
	let coin_key = coin.user_pubkey().serialize().to_vec();
	let linked = db.read(async |t| Ok(t.query_one(
		"SELECT mailbox_pk FROM key_link WHERE user_pubkey=$1", &[&coin_key],
	).await?)).await.unwrap();
	assert_eq!(linked.get::<_, Vec<u8>>("mailbox_pk"), mailbox);
	let core = ctx.bitcoind().sync_client();
	assert!(core.get_tx_out(&coin.chain_anchor().txid, coin.chain_anchor().vout, Some(true)).unwrap().is_some());
	drop(wallet); // Original client stays absent through claim, confirmation and fresh recovery.
	// Keep the existing cross-signer recovery guard: watchmand and its config
	// must exist before the server can enable the legacy expiry task.
	srv.stop().await.unwrap();
	{
		let mut config = srv.config_mut();
		config.expiry_payout.enabled = true;
		config.expiry_payout.grace_blocks = 0;
		config.expiry_payout.sweep_min_confs = 1;
		config.expiry_payout.interval = std::time::Duration::from_secs(1);
		config.expiry_payout.watchman_config = Some(srv.watchmand().config().data_dir.join("config.toml"));
	}
	srv.start().await.unwrap();
	AutomaticBoardFixture { ctx, srv, mnemonic, coin, record }
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

#[tokio::test]
async fn automatic_expired_board_pays_registered_recipient_from_actual_backing() {
	let AutomaticBoardFixture { ctx, srv, mnemonic, coin, record } =
		automatic_board_fixture("automatic_expired_board_pays_registered_recipient_from_actual_backing").await;
	let core = ctx.bitcoind().sync_client();
	let height = core.get_block_count().unwrap() as u32;
	ctx.generate_blocks(coin.expiry_height().to_u32().saturating_sub(height) + 3).await;
	srv.watchmand().wait_for_sync_height(BlockHeight::new(core.get_block_count().unwrap() as u32)).await;
	srv.watchmand().trigger_sweep().await;
	let backing = coin.chain_anchor();
	let carrier = tokio::time::timeout(std::time::Duration::from_secs(30), async {
		loop {
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
	let (discovered, recovered_spend) = recover_all_bip84_and_spend(&ctx, &mnemonic).await;
	// Compare only after the independent mnemonic-only scan and spend have finished.
	let payout = OutPoint::new(carrier.compute_txid(), recipient_vout as u32);
	assert!(discovered.contains(&(payout, expected.value)));
	assert!(recovered_spend.input.iter().any(|i| i.previous_output == payout));
}
