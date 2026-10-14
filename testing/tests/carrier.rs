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
