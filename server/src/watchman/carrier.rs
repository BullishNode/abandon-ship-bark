//! Construct one complete direct carrier without deciding ownership or broadcasting it.

use std::collections::BTreeSet;

use anyhow::Context;
use bdk_wallet::{TxOrdering, Wallet};
use bdk_wallet::coin_selection::{CoinSelectionAlgorithm, LargestFirstCoinSelection};
use bitcoin::{Amount, FeeRate, OutPoint, Psbt, Script, ScriptBuf, Sequence, Transaction, TxOut, Weight, sighash};
use bitcoin::psbt;

use ark::{ServerVtxo, ServerVtxoPolicy, VtxoId};
use ark::vtxo::policy::clause::VtxoClause;
use ark::vtxo::policy::signing::VtxoSigner;
use bitcoin_ext::BlockHeight;
use bitcoin_ext::bdk::{WithGuaranteedChange, KEYCHAIN};

use crate::utils::InstrumentedOwnedLockGuard;
use crate::wallet::{PersistedWallet, WalletKind, WalletUtxosGuard};
use crate::watchman::WatchmanSigner;

const MAX_BUILD_ATTEMPTS: usize = 16;
const MAX_STANDARD_WEIGHT: u64 = 400_000;

#[derive(Debug, Clone, Copy)]
pub struct CarrierLimits {
	pub max_fee_rate: FeeRate,
	pub max_miner_fee: Amount,
	pub max_weight: Weight,
}

/// Output roles follow construction order, even when scripts are identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierOutputRoles {
	pub recipients: Vec<u32>,
	pub operator: Option<u32>,
	pub fee_wallet_change: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct CarrierFeeInput {
	pub outpoint: OutPoint,
	pub txout: TxOut,
}

/// Keep the fee reservation alive until the caller's durable handoff is complete.
/// No transaction, PSBT or secret witness is emitted to logs by this builder.
pub struct DirectCarrier {
	pub tx: Transaction,
	pub backing_ids: Vec<VtxoId>,
	pub outputs: CarrierOutputRoles,
	pub fee_inputs: Vec<CarrierFeeInput>,
	pub fee_reservation: WalletUtxosGuard,
	pub signed_weight: Weight,
	pub miner_fee: Amount,
	/// Actual external fee-wallet funding consumed, distinct from fees beyond flat collections.
	pub funding_subsidy: Amount,
	pub operator_surplus: Amount,
}

#[derive(Clone)]
struct BackingInput {
	vtxo: ServerVtxo,
	clause: Option<VtxoClause>,
}

/// The caller supplies one authorized, currently consumable backing closure and
/// already grouped recipient amounts. It retains the returned exclusive wallet
/// guard while committing the signed carrier and its input reservations.
///
/// This never splits a closure or reduces a recipient to fund mining fees.
/// Chain eligibility and allocation durability remain the caller's responsibility.
pub async fn build_direct_carrier(
	signer: &WatchmanSigner,
	mut wallet: InstrumentedOwnedLockGuard<PersistedWallet>,
	backing: &[ServerVtxo],
	recipients: &[TxOut],
	operator_script: ScriptBuf,
	height: BlockHeight,
	fee_rate: FeeRate,
	limits: CarrierLimits,
) -> anyhow::Result<(InstrumentedOwnedLockGuard<PersistedWallet>, DirectCarrier)> {
	ensure!(wallet.kind() == WalletKind::Watchman, "carrier requires the existing watchman wallet");
	validate_request(backing, recipients, &operator_script, fee_rate, limits)?;
	let backing_amount = sum_amounts(backing.iter().map(|v| v.amount()))?;
	let recipient_amount = sum_amounts(recipients.iter().map(|o| o.value))?;
	let remainder = backing_amount.checked_sub(recipient_amount)
		.context("carrier recipients exceed supplied backing")?;
	let inputs = prepare_backing(signer, backing, height).await?;
	let mut fee = Amount::ZERO;

	for _ in 0..MAX_BUILD_ATTEMPTS {
		let build_inputs = inputs.clone();
		let build_recipients = recipients.to_vec();
		let build_operator = operator_script.clone();
		// BDK coin selection belongs on the blocking pool. Move the same owned
		// guard through the task so reservations cannot race between retries.
		let (held, built, available) = tokio::task::spawn_blocking(move || {
			let available = wallet.confirmed_unlocked_utxos();
			let built = build_candidate(&mut wallet, &available, &build_inputs,
				&build_recipients, &build_operator, height, remainder, fee);
			(wallet, built, available)
		}).await.context("carrier wallet build task failed")?;
		wallet = held;
		let (mut psbt, roles) = built?;
		sign_backing(signer, &inputs, &mut psbt).await?;
		let (held, signed) = tokio::task::spawn_blocking(move || {
			let signed = sign_wallet(&wallet, psbt);
			(wallet, signed)
		}).await.context("carrier wallet signing task failed")?;
		wallet = held;
		let (tx, prevouts) = signed?;
		let actual_fee = carrier_fee(&tx, &prevouts)?;
		let needed = validate_fee_policy(&tx, actual_fee, fee_rate, limits)?;
		if actual_fee < needed {
			ensure!(needed <= limits.max_miner_fee, "carrier required miner fee exceeds ceiling");
			fee = needed;
			continue;
		}
		validate_signed(&tx, &inputs, recipients, &operator_script, &roles)?;
		let backing_points = backing.iter().map(|v| v.point()).collect::<BTreeSet<_>>();
		let available = available.into_iter().collect::<BTreeSet<_>>();
		let fee_inputs = tx.input.iter().zip(&prevouts)
			.filter(|(i, _)| !backing_points.contains(&i.previous_output))
			.map(|(i, prev)| CarrierFeeInput { outpoint: i.previous_output, txout: prev.clone() })
			.collect::<Vec<_>>();
		ensure!(fee_inputs.iter().all(|i| available.contains(&i.outpoint)),
			"carrier supplemental input is not confirmed, trusted and unlocked");
		let fee_reservation = wallet.lock_wallet_utxos(fee_inputs.iter().map(|i| i.outpoint))?;
		let operator_surplus = roles.operator.map(|v| tx.output[v as usize].value).unwrap_or(Amount::ZERO);
		let funding_subsidy = actual_fee.checked_sub(remainder).unwrap_or(Amount::ZERO);
		let fee_input_amount = sum_amounts(fee_inputs.iter().map(|i| i.txout.value))?;
		let fee_change = roles.fee_wallet_change.map(|v| tx.output[v as usize].value).unwrap_or(Amount::ZERO);
		if let Some(idx) = roles.fee_wallet_change {
			ensure!(wallet.is_mine(tx.output[idx as usize].script_pubkey.clone()), "carrier fee change is not wallet owned");
		}
		ensure!(fee_input_amount.checked_sub(fee_change) == Some(funding_subsidy),
			"carrier fee-wallet conservation failed");
		ensure!(backing_amount.checked_add(fee_input_amount) ==
			recipient_amount.checked_add(operator_surplus).and_then(|v| v.checked_add(fee_change))
				.and_then(|v| v.checked_add(actual_fee)), "carrier conservation failed");
		let signed_weight = tx.weight();
		return Ok((wallet, DirectCarrier {
			tx, backing_ids: backing.iter().map(|v| v.id()).collect(), outputs: roles,
			fee_inputs, fee_reservation, signed_weight, miner_fee: actual_fee,
			funding_subsidy, operator_surplus,
		}));
	}
	bail!("carrier fee construction did not converge")
}

fn sum_amounts(amounts: impl IntoIterator<Item = Amount>) -> anyhow::Result<Amount> {
	amounts.into_iter().try_fold(Amount::ZERO, |sum, v| sum.checked_add(v).context("carrier amount overflow"))
}

fn standard_script(script: &Script) -> bool {
	script.is_p2pkh() || script.is_p2sh() || script.is_p2wpkh() || script.is_p2wsh() || script.is_p2tr()
}

fn validate_request(
	backing: &[ServerVtxo], recipients: &[TxOut], operator: &Script,
	rate: FeeRate, limits: CarrierLimits,
) -> anyhow::Result<()> {
	ensure!(!backing.is_empty(), "carrier has no supplied backing");
	ensure!(rate > FeeRate::ZERO && rate <= limits.max_fee_rate, "carrier requested fee rate exceeds ceiling or is zero");
	ensure!(limits.max_weight.to_wu() <= MAX_STANDARD_WEIGHT && limits.max_weight.to_wu() > 0,
		"carrier weight ceiling must be positive and at most standard weight");
	ensure!(backing.iter().map(|v| v.point()).collect::<BTreeSet<_>>().len() == backing.len(),
		"carrier contains duplicate backing outpoints");
	ensure!(standard_script(operator), "carrier operator script is unsupported");
	let mut scripts = BTreeSet::new();
	for recipient in recipients {
		ensure!(standard_script(&recipient.script_pubkey), "carrier recipient script is unsupported");
		ensure!(recipient.value >= recipient.script_pubkey.minimal_non_dust(), "carrier recipient is dust");
		ensure!(scripts.insert(recipient.script_pubkey.clone()), "carrier recipients are not grouped");
	}
	Ok(())
}

async fn prepare_backing<S: VtxoSigner<ServerVtxoPolicy> + Sync>(
	signer: &S, backing: &[ServerVtxo], height: BlockHeight,
) -> anyhow::Result<Vec<BackingInput>> {
	let mut inputs = Vec::with_capacity(backing.len());
	for vtxo in backing {
		let clause = if *vtxo.policy() == ServerVtxoPolicy::ServerOwned { None } else {
			Some(signer.find_signable_clause(vtxo).await.context("carrier backing has no signable clause")?)
		};
		let timelock = match &clause {
			Some(VtxoClause::TimelockSign(c)) => Some(c.timelock_height),
			Some(VtxoClause::DelayedTimelockSign(c)) => Some(c.timelock_height),
			_ => None,
		};
		ensure!(timelock.is_none_or(|required| height >= required),
			"carrier lock height is below its selected backing clause");
		inputs.push(BackingInput { vtxo: vtxo.clone(), clause });
	}
	Ok(inputs)
}

fn build_candidate(
	wallet: &mut Wallet, available: &[OutPoint], backing: &[BackingInput],
	recipients: &[TxOut], operator: &Script, height: BlockHeight, remainder: Amount, fee: Amount,
) -> anyhow::Result<(Psbt, CarrierOutputRoles)> {
	let available = available.iter().copied().collect::<BTreeSet<_>>();
	let unspendable = wallet.list_unspent().filter(|u| !available.contains(&u.outpoint))
		.map(|u| u.outpoint).collect::<Vec<_>>();
	ensure!(backing.iter().all(|i| wallet.get_utxo(i.vtxo.point()).is_none()),
		"carrier backing overlaps the fee wallet");
	let surplus = remainder.checked_sub(fee).unwrap_or(Amount::ZERO);
	let (operator_value, omitted_operator) = if surplus >= operator.minimal_non_dust() {
		(surplus, Amount::ZERO)
	} else { (Amount::ZERO, surplus) };
	let mut roles = CarrierOutputRoles {
		recipients: (0..recipients.len()).map(u32::try_from).collect::<Result<_, _>>()?,
		operator: None, fee_wallet_change: None,
	};
	let wallet_change = wallet.next_unused_address(KEYCHAIN).script_pubkey();
	let mut fixed = recipients.to_vec();
	if operator_value > Amount::ZERO {
		roles.operator = Some(u32::try_from(recipients.len())?);
		fixed.push(TxOut { script_pubkey: operator.to_owned(), value: operator_value });
	}
	let absolute_fee = fee.checked_add(omitted_operator).context("carrier fee overflow")?;
	let psbt = if fixed.is_empty() {
		finish_candidate(wallet, WithGuaranteedChange(LargestFirstCoinSelection), backing, &fixed,
			height, absolute_fee, wallet_change, unspendable)?
	} else {
		finish_candidate(wallet, LargestFirstCoinSelection, backing, &fixed,
			height, absolute_fee, wallet_change, unspendable)?
	};
	let fixed_outputs = recipients.len() + usize::from(roles.operator.is_some());
	ensure!(psbt.unsigned_tx.output.len() >= fixed_outputs && psbt.unsigned_tx.output.len() <= fixed_outputs + 1,
		"carrier has unexpected output roles");
	if psbt.unsigned_tx.output.len() > fixed_outputs {
		roles.fee_wallet_change = Some(u32::try_from(fixed_outputs)?);
	}
	Ok((psbt, roles))
}

fn finish_candidate<C: CoinSelectionAlgorithm>(
	wallet: &mut Wallet, selection: C, backing: &[BackingInput], fixed: &[TxOut],
	height: BlockHeight, fee: Amount, wallet_change: ScriptBuf, unspendable: Vec<OutPoint>,
) -> anyhow::Result<Psbt> {
	let mut b = wallet.build_tx().coin_selection(selection);
	b.ordering(TxOrdering::Untouched);
	b.nlocktime(height.to_locktime().context("carrier lock height invalid")?);
	b.current_height(height.to_u32());
	b.unspendable(unspendable);
	b.fee_absolute(fee);
	b.drain_to(wallet_change);
	for input in backing {
		let sequence = input.clause.as_ref().and_then(|c| c.sequence()).unwrap_or(Sequence::ZERO);
		let weight = input.clause.as_ref().map(|c| c.witness_size(&input.vtxo)).unwrap_or(66);
		b.add_foreign_utxo_with_sequence(input.vtxo.point(), psbt::Input {
			witness_utxo: Some(input.vtxo.txout()), ..Default::default()
		}, Weight::from_wu(u64::try_from(weight)?), sequence)?;
	}
	for output in fixed { b.add_recipient(output.script_pubkey.clone(), output.value); }
	b.finish().context("carrier lacks usable confirmed fee reserve or cannot construct outputs")
}

async fn sign_backing<S: VtxoSigner<ServerVtxoPolicy> + Sync>(
	signer: &S, backing: &[BackingInput], psbt: &mut Psbt,
) -> anyhow::Result<()> {
	let prevouts = psbt.inputs.iter().map(|i| i.witness_utxo.clone().context("carrier missing prevout"))
		.collect::<Result<Vec<_>, _>>()?;
	let all_prevouts = sighash::Prevouts::All(&prevouts);
	let mut cache = sighash::SighashCache::new(&psbt.unsigned_tx);
	for (idx, input) in backing.iter().enumerate() {
		ensure!(psbt.unsigned_tx.input[idx].previous_output == input.vtxo.point(),
			"carrier changed mandatory backing order");
		let witness = if let Some(clause) = &input.clause {
			signer.sign_input_with_clause(&input.vtxo, clause, idx, &mut cache, &all_prevouts).await?
		} else {
			signer.sign_input_with_keyspend(&input.vtxo, idx, &mut cache, &all_prevouts).await?
		};
		psbt.inputs[idx].final_script_witness = Some(witness);
	}
	Ok(())
}

fn sign_wallet(wallet: &Wallet, mut psbt: Psbt) -> anyhow::Result<(Transaction, Vec<TxOut>)> {
	let prevouts = psbt.inputs.iter().map(|i| i.witness_utxo.clone().context("carrier missing prevout"))
		.collect::<Result<Vec<_>, _>>()?;
	#[allow(deprecated)]
	let opts = bdk_wallet::SignOptions { trust_witness_utxo: true, ..Default::default() };
	ensure!(wallet.sign(&mut psbt, opts).context("carrier wallet signing failed")?,
		"carrier has an unfinalized input");
	// Extraction's default fee guard is independent of our explicit ceilings.
	let tx = psbt.extract_tx().context("carrier extraction failed")?;
	Ok((tx, prevouts))
}

fn carrier_fee(tx: &Transaction, prevouts: &[TxOut]) -> anyhow::Result<Amount> {
	ensure!(tx.input.len() == prevouts.len(), "carrier prevout count mismatch");
	let input = sum_amounts(prevouts.iter().map(|o| o.value))?;
	let output = sum_amounts(tx.output.iter().map(|o| o.value))?;
	ensure!(input <= Amount::MAX_MONEY && prevouts.iter().all(|o| o.value <= Amount::MAX_MONEY),
		"carrier input amount exceeds Bitcoin money range");
	input.checked_sub(output).context("carrier outputs exceed inputs")
}

pub(crate) fn validate_fee_policy(
	tx: &Transaction, actual_fee: Amount, rate: FeeRate, limits: CarrierLimits,
) -> anyhow::Result<Amount> {
	let needed = rate.fee_wu(tx.weight()).context("carrier fee computation overflow")?;
	ensure!(tx.weight() <= limits.max_weight, "complete carrier exceeds maximum signed weight");
	ensure!(actual_fee <= limits.max_miner_fee, "carrier actual miner fee exceeds ceiling");
	if actual_fee >= needed {
		ensure!(actual_fee <= limits.max_fee_rate.fee_wu(tx.weight()).context("carrier fee ceiling overflow")?,
			"carrier actual fee rate exceeds ceiling, including omitted dust");
	}
	Ok(needed)
}

fn validate_signed(
	tx: &Transaction, backing: &[BackingInput], recipients: &[TxOut], operator: &Script,
	roles: &CarrierOutputRoles,
) -> anyhow::Result<()> {
	ensure!(tx.input.len() >= backing.len(), "carrier omitted backing");
	ensure!(tx.input.iter().map(|i| i.previous_output).collect::<BTreeSet<_>>().len() == tx.input.len(),
		"carrier contains duplicate inputs");
	ensure!(tx.input.iter().all(|i| !i.witness.is_empty()), "carrier is not fully signed");
	ensure!(roles.recipients.len() == recipients.len(), "carrier recipient roles mismatch");
	for (output, idx) in recipients.iter().zip(&roles.recipients) {
		ensure!(tx.output.get(*idx as usize) == Some(output), "carrier changed exact recipient output");
	}
	if let Some(idx) = roles.operator {
		ensure!(tx.output[idx as usize].script_pubkey.as_script() == operator, "carrier operator role mismatch");
	}
	ensure!(tx.output.iter().all(|o| standard_script(&o.script_pubkey) && o.value >= o.script_pubkey.minimal_non_dust()),
		"carrier has a nonstandard or dust output");
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	use bdk_wallet::chain::BlockId;
	use bdk_wallet::test_utils::{get_test_tr_single_sig_xprv, insert_checkpoint, receive_output_in_latest_block};
	use bitcoin::{BlockHash, Network, TapSighash, Txid, Witness};
	use bitcoin::hashes::Hash;
	use bitcoin::secp256k1::{Keypair, schnorr};
	use bitcoin::taproot::ControlBlock;

	use ark::test_util::dummy::{DUMMY_SERVER_KEY, DUMMY_USER_KEY};
	use ark::board::BoardBuilder;
	use ark::vtxo::Vtxo;
	use ark::vtxo::policy::clause::TapScriptClause;
	use bitcoin_ext::{BlockDelta, KeypairExt};

	// Actual secp signatures and clause witnesses, without a database-backed
	// preimage source. Node integration separately exercises WatchmanSigner.
	struct ExpirySigner(Keypair);

	#[async_trait::async_trait]
	impl VtxoSigner<ServerVtxoPolicy> for ExpirySigner {
		async fn sign_keyspend<G: Sync + Send>(&self, v: &Vtxo<G, ServerVtxoPolicy>, hash: TapSighash) -> Option<schnorr::Signature> {
			let key = self.0.for_keyspend(&ark::SECP, v.output_taproot().merkle_root());
			Some(ark::SECP.sign_schnorr(&hash.into(), &key))
		}

		async fn witness(&self, clause: &VtxoClause, cb: &ControlBlock, hash: TapSighash) -> Option<Witness> {
			if clause.pubkey() != self.0.public_key() { return None; }
			let sig = ark::SECP.sign_schnorr(&hash.into(), &self.0);
			match clause { VtxoClause::TimelockSign(c) => Some(c.witness(&sig, cb)), _ => None }
		}
	}

	fn fixture_backing(count: usize, amount: Amount) -> Vec<ServerVtxo> {
		(0..count).map(|n| {
			let point = OutPoint::new(Txid::from_byte_array([7; 32]), n.try_into().unwrap());
			let user = BoardBuilder::new(DUMMY_USER_KEY.public_key(), BlockHeight::new(999),
				DUMMY_SERVER_KEY.public_key(), BlockDelta::new(144))
				.set_funding_details(amount, Amount::ZERO, point).unwrap().generate_user_nonces();
			let server = BoardBuilder::new_for_cosign(DUMMY_USER_KEY.public_key(), BlockHeight::new(999),
				DUMMY_SERVER_KEY.public_key(), BlockDelta::new(144), amount, Amount::ZERO, point,
				*user.user_pub_nonce());
			server.build_unsigned_vtxos().unwrap().0.into_bare()
		}).collect()
	}

	fn script(n: usize) -> ScriptBuf {
		let mut bytes = [0; 20]; bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
		ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array(bytes))
	}

	fn fixture_wallet(coins: &[Amount]) -> (Wallet, Vec<OutPoint>) {
		let mut wallet = Wallet::create_single(get_test_tr_single_sig_xprv())
			.network(Network::Regtest).create_wallet_no_persist().unwrap();
		insert_checkpoint(&mut wallet, BlockId { height: 1_000, hash: BlockHash::all_zeros() });
		let points = coins.iter().map(|v| receive_output_in_latest_block(&mut wallet, *v)).collect();
		(wallet, points)
	}

	fn limits() -> CarrierLimits {
		CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb(1_000).unwrap(),
			max_miner_fee: Amount::ONE_BTC, max_weight: Weight::from_wu(400_000) }
	}

	async fn construct(
		wallet: &mut Wallet, available: &[OutPoint], backing: &[ServerVtxo], recipients: &[TxOut],
		operator: &Script, rate: FeeRate, policy: CarrierLimits,
	) -> anyhow::Result<(Transaction, CarrierOutputRoles, Amount)> {
		validate_request(backing, recipients, operator, rate, policy)?;
		let signer = ExpirySigner(*DUMMY_SERVER_KEY);
		let inputs = prepare_backing(&signer, backing, BlockHeight::new(1_000)).await?;
		let remainder = sum_amounts(backing.iter().map(|v| v.amount()))?
			.checked_sub(sum_amounts(recipients.iter().map(|o| o.value))?).context("test recipients exceed backing")?;
		let mut fee = Amount::ZERO;
		for _ in 0..MAX_BUILD_ATTEMPTS {
			let (mut psbt, roles) = build_candidate(wallet, available, &inputs, recipients, operator,
				BlockHeight::new(1_000), remainder, fee)?;
			sign_backing(&signer, &inputs, &mut psbt).await?;
			let (tx, prevouts) = sign_wallet(wallet, psbt)?;
			let actual_fee = carrier_fee(&tx, &prevouts)?;
			let needed = validate_fee_policy(&tx, actual_fee, rate, policy)?;
			if actual_fee >= needed {
				validate_signed(&tx, &inputs, recipients, operator, &roles)?;
				return Ok((tx, roles, actual_fee));
			}
			// Update only the next construction's operator fee, never a recipient.
			if needed > policy.max_miner_fee { bail!("test fee exceeds ceiling"); }
			fee = needed;
		}
		bail!("test fee did not converge")
	}

	#[tokio::test]
	async fn signed_carrier_preserves_recipients_across_252_253_counts() {
		for inputs in [252, 253] {
			for outputs in [252, 253] {
				let backing = fixture_backing(inputs, Amount::from_sat(20_000));
				let recipients = (0..outputs - 1).map(|n| TxOut { value: Amount::from_sat(19_000), script_pubkey: script(n) }).collect::<Vec<_>>();
				let (mut wallet, available) = fixture_wallet(&[]);
				let (tx, roles, fee) = construct(&mut wallet, &available, &backing, &recipients, &script(999),
					FeeRate::from_sat_per_vb(2).unwrap(), limits()).await.unwrap();
				assert_eq!(tx.input.len(), inputs);
				assert_eq!(tx.output.len(), outputs);
				assert_eq!(&tx.output[..outputs - 1], recipients.as_slice());
				assert_eq!(roles.operator, Some((outputs - 1) as u32));
				assert_eq!(roles.fee_wallet_change, None);
				assert_eq!(fee, FeeRate::from_sat_per_vb(2).unwrap().fee_wu(tx.weight()).unwrap());
				assert_eq!(inputs as u64 * 20_000, tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>() + fee.to_sat());
			}
		}
	}

	#[tokio::test]
	async fn signed_carrier_retains_large_fee_reserve_change() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let recipient = TxOut { value: Amount::from_sat(19_000), script_pubkey: script(0) };
		let (mut wallet, available) = fixture_wallet(&[Amount::from_sat(1_000_000)]);
		let (tx, roles, fee) = construct(&mut wallet, &available, &backing, &[recipient.clone()], &script(999),
			FeeRate::from_sat_per_vb(30).unwrap(), limits()).await.unwrap();
		assert_eq!(tx.output[roles.recipients[0] as usize], recipient);
		assert_eq!(roles.operator, None);
		let change = tx.output[roles.fee_wallet_change.unwrap() as usize].clone();
		assert!(wallet.is_mine(change.script_pubkey));
		assert_eq!(change.value.to_sat(), 1_001_000 - fee.to_sat());
		assert!(change.value.to_sat() > 990_000);
		assert_eq!(tx.input.len(), 2);
	}

	#[tokio::test]
	async fn signed_carrier_separates_equal_script_output_roles() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let recipient = TxOut { value: Amount::from_sat(10_000), script_pubkey: script(0) };
		let (mut wallet, available) = fixture_wallet(&[]);
		let (tx, roles, fee) = construct(&mut wallet, &available, &backing, &[recipient.clone()], &recipient.script_pubkey,
			FeeRate::from_sat_per_vb(2).unwrap(), limits()).await.unwrap();
		assert_eq!(roles.recipients, vec![0]);
		assert_eq!(roles.operator, Some(1));
		assert_eq!(tx.output[0], recipient);
		assert_eq!(tx.output[1].script_pubkey, tx.output[0].script_pubkey);
		assert_eq!(tx.output[1].value.to_sat(), 10_000 - fee.to_sat());
	}

	#[tokio::test]
	async fn signed_carrier_refuses_insufficient_fragmented_reserve() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(19_000), script_pubkey: script(0) }];
		let (mut wallet, available) = fixture_wallet(&vec![Amount::from_sat(400); 8]);
		let result = construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(100).unwrap(), limits()).await;
		assert!(result.is_err());
		assert_eq!(wallet.list_unspent().count(), 8, "failed construction does not consume reserve");
		assert_eq!(recipients[0].value.to_sat(), 19_000);
	}

	#[tokio::test]
	async fn signed_carrier_uses_multiple_confirmed_fee_inputs() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(19_000), script_pubkey: script(0) }];
		let (mut wallet, available) = fixture_wallet(&vec![Amount::from_sat(2_000); 8]);
		let (tx, roles, fee) = construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(20).unwrap(), limits()).await.unwrap();
		assert!(tx.input.len() >= 3);
		assert_eq!(tx.output[0], recipients[0]);
		let local_value = (tx.input.len() - 1) as u64 * 2_000;
		let change = roles.fee_wallet_change.map(|i| tx.output[i as usize].value.to_sat()).unwrap_or(0);
		assert_eq!(local_value - change, fee.to_sat() - 1_000);
	}

	#[tokio::test]
	async fn signed_carrier_waits_at_high_fees_then_builds_at_low_fees() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(18_000), script_pubkey: script(0) }];
		let (mut wallet, available) = fixture_wallet(&[]);
		assert!(construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(100).unwrap(), limits()).await.is_err());
		let (tx, _, _) = construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(2).unwrap(), limits()).await.unwrap();
		assert_eq!(tx.output[0], recipients[0]);
	}

	#[tokio::test]
	async fn signed_carrier_omitted_dust_obeys_actual_fee_ceiling() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(19_600), script_pubkey: script(0) }];
		let (mut wallet, available) = fixture_wallet(&[]);
		let (tx, roles, fee) = construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(1).unwrap(), limits()).await.unwrap();
		assert_eq!(roles.operator, None);
		assert_eq!(roles.fee_wallet_change, None);
		assert_eq!(tx.output, recipients);
		assert_eq!(fee, Amount::from_sat(400));
		let tight = CarrierLimits { max_fee_rate: FeeRate::from_sat_per_vb(2).unwrap(), ..limits() };
		assert!(construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(1).unwrap(), tight).await.is_err());
		let tight = CarrierLimits { max_miner_fee: Amount::from_sat(399), ..limits() };
		assert!(construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(1).unwrap(), tight).await.is_err());
	}

	#[tokio::test]
	async fn signed_carrier_refuses_whole_oversize_closure() {
		let backing = fixture_backing(253, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(253 * 19_000), script_pubkey: script(0) }];
		let (mut wallet, available) = fixture_wallet(&[]);
		let tight = CarrierLimits { max_weight: Weight::from_wu(10_000), ..limits() };
		assert!(construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(2).unwrap(), tight).await.is_err());
		assert_eq!(backing.len(), 253, "no fallback input splitting");
	}
	#[tokio::test]
	async fn signed_carrier_retires_operator_only_backing_with_reserve_change() {
		let backing = fixture_backing(1, Amount::from_sat(600));
		let (mut wallet, available) = fixture_wallet(&[Amount::from_sat(1_000_000)]);
		let (tx, roles, fee) = construct(&mut wallet, &available, &backing, &[], &script(999),
			FeeRate::from_sat_per_vb(5).unwrap(), limits()).await.unwrap();
		assert!(roles.recipients.is_empty());
		assert_eq!(roles.operator, None);
		let change = tx.output[roles.fee_wallet_change.unwrap() as usize].clone();
		assert!(wallet.is_mine(change.script_pubkey));
		assert_eq!(change.value.to_sat() + fee.to_sat(), 1_000_600);
	}

	#[test]
	fn carrier_refuses_invalid_group_or_request_before_construction() {
		let backing = fixture_backing(1, Amount::from_sat(20_000));
		let good = TxOut { value: Amount::from_sat(19_000), script_pubkey: script(0) };
		let rate = FeeRate::from_sat_per_vb(2).unwrap();
		assert!(validate_request(&[], &[good.clone()], &script(999), rate, limits()).is_err());
		assert!(validate_request(&[backing[0].clone(), backing[0].clone()], &[good.clone()], &script(999), rate, limits()).is_err());
		assert!(validate_request(&backing, &[good.clone(), good.clone()], &script(999), rate, limits()).is_err());
		assert!(validate_request(&backing, &[TxOut { value: Amount::ZERO, ..good.clone() }], &script(999), rate, limits()).is_err());
		assert!(validate_request(&backing, &[good], &ScriptBuf::new(), rate, limits()).is_err());
		assert!(validate_request(&backing, &[], &script(999), rate,
			CarrierLimits { max_weight: Weight::from_wu(400_001), ..limits() }).is_err());
	}

	#[tokio::test]
	async fn signed_carrier_enforces_actual_standard_weight_boundary() {
		let (mut wallet, available) = fixture_wallet(&[]);
		let backing = fixture_backing(1_300, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(1_300 * 19_000), script_pubkey: script(0) }];
		let (tx, _, _) = construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(2).unwrap(), limits()).await.unwrap();
		assert!(tx.weight().to_wu() > 380_000 && tx.weight().to_wu() <= 400_000);
		let backing = fixture_backing(1_500, Amount::from_sat(20_000));
		let recipients = [TxOut { value: Amount::from_sat(1_500 * 19_000), script_pubkey: script(0) }];
		assert!(construct(&mut wallet, &available, &backing, &recipients, &script(999),
			FeeRate::from_sat_per_vb(2).unwrap(), limits()).await.is_err());
	}

}
