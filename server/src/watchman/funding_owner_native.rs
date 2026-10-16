//! Opt-in, owned-regtest component fixture. No stock/live endpoint is accepted.
use super::*;
use crate::watchman::ordinary_fee::OrdinaryFeePolicy;
use crate::watchman::ordinary_funding::{
	FundingCandidate, FundingFamily, FundingPurpose, FundingRequest, verify_reply,
};
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::Keypair;
use bitcoin::{Amount, Psbt, ScriptBuf, Txid};
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

fn evidence(root: &Path, stage: &str, facts: Value) -> anyhow::Result<()> {
	let mut file = OpenOptions::new()
		.create(true)
		.append(true)
		.open(root.join("native-events.jsonl"))?;
	writeln!(file, "{}", json!({"stage":stage,"facts":facts}))?;
	file.sync_all()?;
	Ok(())
}
fn ledger(root: &Path, stage: &str) -> anyhow::Result<()> {
	let path = root
		.join("data")
		.join(crate::payout_witness::DIRECT_DIRECTORY)
		.join("ordinary-fee-budget");
	evidence(
		root,
		stage,
		json!({"actual_public_ledger":std::fs::read_to_string(path)?}),
	)
}
async fn rpc(core: &Client, method: &str, params: &[Value]) -> anyhow::Result<Value> {
	Ok(core.call_raw(method, params).await?)
}
async fn sync(wallet: &mut PersistedWallet, core: &Client) -> anyhow::Result<()> {
	let end = u32::try_from(core.get_block_count().await?)?;
	for height in wallet.latest_checkpoint().height() + 1..=end {
		let hash = core.get_block_hash(u64::from(height)).await?;
		let block = core.get_block(&hash).await?;
		wallet.apply_block(&block, height)?;
	}
	wallet.persist().await?;
	Ok(())
}
fn build(
	wallet: &mut PersistedWallet,
	points: &[OutPoint],
	script: ScriptBuf,
	value: u64,
	fee: u64,
) -> anyhow::Result<Psbt> {
	let mut builder = wallet.build_tx();
	builder.manually_selected_only();
	builder.add_utxos(points)?;
	builder.add_recipient(script, Amount::from_sat(value));
	builder.fee_absolute(Amount::from_sat(fee));
	Ok(builder.finish()?)
}
async fn request(
	owner: &Arc<FundingOwner>,
	candidate: FundingCandidate,
	signed: Option<&Transaction>,
	key: &Keypair,
	genesis: bitcoin::BlockHash,
	policy: sha256::Hash,
	opening: sha256::Hash,
) -> anyhow::Result<FundingReservation> {
	let raw = FundingRequest::sign(candidate.clone(), signed, key, genesis, policy, opening)?;
	let reply = owner.clone().execute(raw, signed.is_some()).await?;
	verify_reply(&reply, &candidate, key.public_key(), genesis, policy)
}

#[tokio::test]
#[ignore = "requires declared NEW owned Core/PG and <=400000sat funding; no live endpoints"]
async fn ordinary_owner_regtest_retry_bind_restart_and_confirmed_unused_extra() -> anyhow::Result<()>
{
	let root = std::path::PathBuf::from(std::env::var("B_OWNER_CASE_DIR")?);
	ensure!(root.is_dir(), "fresh proof namespace missing");
	OpenOptions::new()
		.write(true)
		.create_new(true)
		.open(root.join("native-case-started"))?
		.sync_all()?;
	let url = std::env::var("B_OWNER_CORE_URL")?;
	ensure!(
		url.starts_with("http://127.0.0.1:"),
		"only owned loopback regtest fixture allowed"
	);
	let cookie = std::path::PathBuf::from(std::env::var("B_OWNER_CORE_COOKIE")?);
	ensure!(cookie.starts_with(&root), "foreign Core cookie refused");
	let core = crate::bitcoind::build_client(&url, bitcoin_ext::rpc::Auth::CookieFile(cookie))?;
	ensure!(
		core.network().await? == Network::Regtest && core.get_block_count().await? == 0,
		"only genuinely fresh regtest chain is admitted"
	);
	let cfg: crate::config::Postgres = serde_json::from_str(&std::env::var("B_OWNER_PG_CONFIG")?)?;
	ensure!(
		cfg.host == root.join("socket").to_string_lossy() && cfg.name == "b_owner_case",
		"foreign PG refused"
	);
	let db = Db::create(&cfg).await?;
	let master = bitcoin::bip32::Xpriv::new_master(Network::Regtest, &[42u8; 64])?;
	let server_derived = master.derive_priv(
		&crate::SECP,
		&"m/2'/0'".parse::<bitcoin::bip32::DerivationPath>()?,
	)?;
	let key = Keypair::from_secret_key(&crate::SECP, &server_derived.private_key);
	let genesis = core.get_block_hash(0).await?;
	let first = bitcoin_ext::BlockRef {
		height: bitcoin_ext::BlockHeight::new(0),
		hash: genesis,
	};
	let mut rounds = PersistedWallet::load_derive_from_master_xpriv(
		db.clone(),
		core.clone(),
		Network::Regtest,
		&master,
		WalletKind::Rounds,
		first,
		1,
	)
	.await?;
	let mut watch = PersistedWallet::load_derive_from_master_xpriv(
		db.clone(),
		core.clone(),
		Network::Regtest,
		&master,
		WalletKind::Watchman,
		first,
		1,
	)
	.await?;
	let round_a = rounds
		.reveal_next_address(bdk_wallet::KeychainKind::External)
		.address;
	let round_b = rounds
		.reveal_next_address(bdk_wallet::KeychainKind::External)
		.address;
	let change = rounds
		.reveal_next_address(bdk_wallet::KeychainKind::External)
		.address;
	let watch_a = watch
		.reveal_next_address(bdk_wallet::KeychainKind::External)
		.address;
	rounds.persist().await?;
	watch.persist().await?;
	evidence(
		&root,
		"before_funding",
		json!({"treasury_members":["rounds","watchman"],"external_faucet":true,"principal_sat":400000,"funding_outputs":[{"role":"rounds_pinned","amount_sat":100000,"script":round_a.script_pubkey().as_bytes()},{"role":"rounds_extra","amount_sat":200000,"script":round_b.script_pubkey().as_bytes()},{"role":"watchman","amount_sat":100000,"script":watch_a.script_pubkey().as_bytes()}],"ordinary_budget_sat":60000,"urgent_reserve_sat":15000,"external_future_reserved_sat":20000,"miner_ceiling_sat":10000,"network_rpc_credit":false,"round_pool_cosign_credit":false}),
	)?;
	rpc(&core, "createwallet", &[json!("owned_faucet")]).await?;
	let faucet = rpc(&core, "getnewaddress", &[])
		.await?
		.as_str()
		.context("faucet address missing")?
		.to_owned();
	rpc(&core, "generatetoaddress", &[json!(101), json!(faucet)]).await?;
	rpc(&core, "settxfee", &[json!(0.00001000)]).await?;
	let outputs = std::collections::BTreeMap::from([
		(round_a.to_string(), 0.001),
		(round_b.to_string(), 0.002),
		(watch_a.to_string(), 0.001),
	]);
	let funding: Txid = rpc(&core, "sendmany", &[json!(""), json!(outputs)])
		.await?
		.as_str()
		.context("funding txid missing")?
		.parse()?;
	rpc(&core, "generatetoaddress", &[json!(1), json!(faucet)]).await?;
	let funding_raw = crate::bitcoind::custom_get_raw_transaction_info(&core, funding, None)
		.await?
		.context("funding body missing")?;
	let funding_tx: Transaction = bitcoin::consensus::deserialize(&funding_raw.hex)?;
	let mut funding_inputs = Amount::ZERO;
	for input in &funding_tx.input {
		let parent = crate::bitcoind::custom_get_raw_transaction_info(
			&core,
			input.previous_output.txid,
			None,
		)
		.await?
		.context("faucet input parent missing")?;
		let tx: Transaction = bitcoin::consensus::deserialize(&parent.hex)?;
		funding_inputs = funding_inputs
			.checked_add(
				tx.output
					.get(input.previous_output.vout as usize)
					.context("funding parent vout missing")?
					.value,
			)
			.context("funding input sum overflow")?;
	}
	let funding_outputs = funding_tx
		.output
		.iter()
		.try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
		.context("funding output sum overflow")?;
	let funding_fee = funding_inputs
		.checked_sub(funding_outputs)
		.context("funding fee negative")?
		.to_sat();
	ensure!(
		funding_fee + 1100 <= 10000,
		"declared actual miner cap would be exceeded"
	);
	evidence(
		&root,
		"external_faucet_receipt",
		json!({"txid":funding,"external_receipts_sat":400000,"faucet_miner_cost_sat":funding_fee,"faucet_fee_outside_treasury":true,"planned_native_miner_sat":1100}),
	)?;

	let find = |script: &ScriptBuf, value: u64| -> anyhow::Result<OutPoint> {
		let positions = funding_tx
			.output
			.iter()
			.enumerate()
			.filter(|(_, o)| o.script_pubkey == *script && o.value.to_sat() == value)
			.map(|(i, _)| i)
			.collect::<Vec<_>>();
		ensure!(positions.len() == 1, "funding output identity ambiguous");
		Ok(OutPoint::new(funding, u32::try_from(positions[0])?))
	};
	let pinned = find(&round_a.script_pubkey(), 100000)?;
	let extra = find(&round_b.script_pubkey(), 200000)?;
	sync(&mut rounds, &core).await?;
	sync(&mut watch, &core).await?;
	let data = root.join("data");
	std::fs::create_dir(&data)?;
	crate::payout_witness::DirectPayoutWitness::initialize(
		&data,
		Network::Regtest,
		key.public_key(),
		&db,
	)
	.await?;
	let directory = data.join(crate::payout_witness::DIRECT_DIRECTORY);
	let policy = OrdinaryFeePolicy::default();
	policy.initialize_fresh(&directory, Network::Regtest, key.public_key())?;
	let budget = Arc::new(OrdinaryFeeBudget::load(&directory, policy)?);
	let policy_id = budget.identity();
	let opening = budget.opening_digest()?;
	let watch = InstrumentedLock::new("native_fixture_watch", watch);
	let runtime = crate::system::RuntimeManager::new();
	let rate = FeeRate::from_sat_per_vb(20).context("rate")?;
	let owner = Arc::new(
		FundingOwner::new(
			db.clone(),
			core.clone(),
			budget.clone(),
			watch.clone(),
			key,
			Network::Regtest,
			&master,
			rate,
			runtime.clone(),
		)
		.await?,
	);
	let old_psbt = build(
		&mut rounds,
		&[pinned, extra],
		change.script_pubkey(),
		299500,
		500,
	)?;
	let old = FundingCandidate::from_psbt(
		FundingPurpose::Round,
		FundingFamily::Round {
			round_seq: 123,
			pinned_input: pinned,
		},
		0,
		&old_psbt,
	)?;
	let first_res = request(&owner, old.clone(), None, &key, genesis, policy_id, opening).await?;
	ledger(&root, "old_unsigned_reserved")?;
	let before = std::fs::read(directory.join("ordinary-fee-budget"))?;
	ensure!(
		request(&owner, old.clone(), None, &key, genesis, policy_id, opening).await? == first_res,
		"same unsigned retry changed reservation"
	);
	ensure!(
		std::fs::read(directory.join("ordinary-fee-budget"))? == before,
		"same retry appended/debited"
	);
	let winner_psbt = build(&mut rounds, &[pinned], change.script_pubkey(), 99400, 600)?;
	let winner =
		FundingCandidate::from_psbt(FundingPurpose::Round, old.family.clone(), 1, &winner_psbt)?;
	let winning = request(
		&owner,
		winner.clone(),
		None,
		&key,
		genesis,
		policy_id,
		opening,
	)
	.await?;
	let signed = rounds.finish_tx_with_max_fee_rate(winner_psbt, Some(rate))?;
	ensure!(
		request(
			&owner,
			winner.clone(),
			Some(&signed),
			&key,
			genesis,
			policy_id,
			opening
		)
		.await? == winning,
		"signed winner changed original reservation"
	);
	ledger(&root, "winner_signed_bound")?;
	let pool_psbt = build(&mut rounds, &[extra], change.script_pubkey(), 199500, 500)?;
	let pool = FundingCandidate::from_psbt(
		FundingPurpose::Pool,
		FundingFamily::Pool {
			funding_txid: pool_psbt.unsigned_tx.compute_txid(),
		},
		0,
		&pool_psbt,
	)?;
	ensure!(
		request(
			&owner,
			pool.clone(),
			None,
			&key,
			genesis,
			policy_id,
			opening
		)
		.await
		.is_err(),
		"unconfirmed winner released another family input"
	);
	ledger(&root, "pre_confirmation_refusal")?;
	let txid: Txid = rpc(
		&core,
		"sendrawtransaction",
		&[json!(
			bitcoin::consensus::serialize(&signed).to_lower_hex_string()
		)],
	)
	.await?
	.as_str()
	.context("winner broadcast reply missing")?
	.parse()?;
	ensure!(
		txid == signed.compute_txid(),
		"winner broadcast identity changed"
	);
	rpc(&core, "generatetoaddress", &[json!(1), json!(faucet)]).await?;
	{
		let mut guard = watch.lock().await;
		sync(&mut guard, &core).await?;
	}
	ensure!(
		request(
			&owner,
			pool.clone(),
			None,
			&key,
			genesis,
			policy_id,
			opening
		)
		.await
		.is_err(),
		"lagging rounds wallet released unused extra"
	);
	ledger(&root, "confirmed_but_rounds_wallet_lag_refusal")?;
	sync(&mut rounds, &core).await?;
	let pooled = request(
		&owner,
		pool.clone(),
		None,
		&key,
		genesis,
		policy_id,
		opening,
	)
	.await?;
	ledger(&root, "confirmed_wallet_winner_released_unused_extra")?;
	let pool_signed = rounds.finish_tx_with_max_fee_rate(pool_psbt, Some(rate))?;
	ensure!(
		request(
			&owner,
			pool.clone(),
			Some(&pool_signed),
			&key,
			genesis,
			policy_id,
			opening
		)
		.await? == pooled,
		"Pool binding changed reservation"
	);
	let actual: Txid = rpc(
		&core,
		"sendrawtransaction",
		&[json!(
			bitcoin::consensus::serialize(&pool_signed).to_lower_hex_string()
		)],
	)
	.await?
	.as_str()
	.context("Pool broadcast reply missing")?
	.parse()?;
	ensure!(
		actual == pool_signed.compute_txid(),
		"Pool broadcast identity differs"
	);
	rpc(&core, "generatetoaddress", &[json!(1), json!(faucet)]).await?;
	sync(&mut rounds, &core).await?;
	{
		let mut guard = watch.lock().await;
		sync(&mut guard, &core).await?;
	}
	let native_fees = 600u64 + 500;
	let closing = rounds.balance().total().to_sat() + watch.lock().await.balance().total().to_sat();
	ensure!(
		closing == 400000 - native_fees,
		"native treasury conservation differs"
	);
	let snapshot = std::fs::read(directory.join("ordinary-fee-budget"))?;
	drop(owner);
	drop(budget);
	let budget = Arc::new(OrdinaryFeeBudget::load(&directory, policy)?);
	let owner = Arc::new(
		FundingOwner::new(
			db.clone(),
			core.clone(),
			budget.clone(),
			watch.clone(),
			key,
			Network::Regtest,
			&master,
			rate,
			runtime.clone(),
		)
		.await?,
	);
	ensure!(
		request(
			&owner,
			pool.clone(),
			Some(&pool_signed),
			&key,
			genesis,
			policy_id,
			opening
		)
		.await? == pooled,
		"restart exact signed retry changed original"
	);
	ensure!(
		std::fs::read(directory.join("ordinary-fee-budget"))? == snapshot,
		"restart retried fee or changed history"
	);
	ensure!(
		budget.funding_resolutions()?.len() == 1 && budget.funding_states()?.len() == 3,
		"original intent or winner history lost"
	);
	ledger(&root, "restart_exact_signed_retry_no_debit")?;
	evidence(
		&root,
		"PASS",
		json!({"opening_assets_sat":0,"external_receipts_sat":400000,"closing_assets_sat":closing,"actual_native_miner_sat":native_fees,"actual_all_fixture_miner_sat":funding_fee+native_fees,"faucet_fee_sat":funding_fee,"native_reserved_fee_sat":1600,"old_unsigned_reservation_sat":500,"urgent_reserve_sat":budget.urgent_reserve()?.to_sat(),"funding_txid":funding,"winner_txid":txid,"pool_txid":actual,"ordinary_policy_unchanged":true,"failed_unsigned_pool_recovery":"OPEN","network_rpc_credit":false,"full_round_pool_credit":false}),
	)?;
	drop(owner);
	drop(budget);
	runtime.shutdown();
	Ok(())
}
