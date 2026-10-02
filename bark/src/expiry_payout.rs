//! Collect coins that the server paid out on-chain after they expired.
//!
//! A server can choose to settle a VTXO that expired unrefreshed by paying its
//! value on-chain to the BIP86 key-path address of the VTXO's own key,
//! `tr(user_pubkey)`, and marking the VTXO spent on its side. The wallet is not
//! told about this. These functions let the wallet:
//!
//! 1. adopt the server's spent state, so the VTXO leaves the balance and coin
//!    selection ([adopt_server_vtxo_status]);
//! 2. find the payout outputs on-chain ([find_expiry_payouts]);
//! 3. sweep them into the on-chain wallet and record a movement
//!    ([sweep_expiry_payouts]).

use std::collections::{BTreeMap, HashMap};

use anyhow::Context;
use bitcoin::{
	absolute, transaction, Amount, FeeRate, OutPoint, ScriptBuf, Transaction, TxIn,
	TxOut, Txid, Witness,
};
use bitcoin::key::{Keypair, TapTweak};
use bitcoin::secp256k1::PublicKey;
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::taproot;
use log::{info, warn};

use serde::{Deserialize, Serialize};

use ark::{SECP, VtxoId};
use bitcoin_ext::{BlockHeight, P2TR_DUST};
use server_rpc::protos::VtxoSpendState;

use crate::{Wallet, WalletVtxo};
use crate::movement::{MovementDestination, MovementStatus};
use crate::movement::update::MovementUpdate;
use crate::subsystem::Subsystem;
use crate::vtxo::{ServerStatusAdoption, VtxoStateKind};

/// The subsystem of the movement recorded by [sweep_expiry_payouts].
pub const EXPIRY_PAYOUT_SUBSYSTEM: Subsystem = Subsystem::new("bark.expiry_payout");

/// The movement kind recorded by [sweep_expiry_payouts].
pub const EXPIRY_PAYOUT_MOVEMENT_KIND: &str = "expiry-payout";

/// The state a VTXO has after [adopt_server_vtxo_status].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdoptedVtxoState {
	/// The server considers the VTXO spent; the wallet now has it spent too.
	Spent,
	/// The server will let us spend the VTXO.
	Spendable,
	/// The server does not know the VTXO's transaction chain yet.
	Unregistered,
	/// Anything else: the VTXO is locked, or still in another flow.
	Other,
}

impl AdoptedVtxoState {
	fn from_adoption(adoption: Option<ServerStatusAdoption>) -> Self {
		match adoption {
			Some(ServerStatusAdoption::Spent) => AdoptedVtxoState::Spent,
			Some(ServerStatusAdoption::Spendable) => AdoptedVtxoState::Spendable,
			Some(ServerStatusAdoption::InFlight(VtxoSpendState::Unregistered)) => {
				AdoptedVtxoState::Unregistered
			},
			Some(ServerStatusAdoption::InFlight(_)) | None => AdoptedVtxoState::Other,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedVtxoStatus {
	pub vtxo_id: VtxoId,
	pub state: AdoptedVtxoState,
}

/// An on-chain output paying the expiry payout address of a VTXO.
///
/// VTXOs that share a key share a payout address, so one output can be listed
/// for several VTXOs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryPayout {
	pub vtxo_id: VtxoId,
	pub outpoint: OutPoint,
	pub amount: Amount,
	/// 0 while the output is in the mempool.
	pub confirmations: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryPayoutSweep {
	pub txid: Txid,
	/// The amount the sweep pays to the on-chain wallet, after its fee.
	pub swept: Amount,
}

/// The script a server pays an expired VTXO to: BIP86 `tr(user_pubkey)`,
/// key-path only.
pub fn expiry_payout_script(user_pubkey: PublicKey) -> ScriptBuf {
	ScriptBuf::new_p2tr(&SECP, user_pubkey.x_only_public_key().0, None)
}

/// Build and sign a transaction that spends the key-path `inputs` to
/// `destination`, paying `fee_rate`.
///
/// Each input carries the untweaked VTXO key whose [expiry_payout_script] the
/// output pays; the BIP86 tweak is applied here.
pub fn build_signed_expiry_payout_sweep(
	inputs: &[(OutPoint, TxOut, Keypair)],
	destination: ScriptBuf,
	fee_rate: FeeRate,
) -> anyhow::Result<Transaction> {
	if inputs.is_empty() {
		bail!("no expiry payouts to sweep");
	}

	let total = inputs.iter().map(|(_, o, _)| o.value).sum::<Amount>();
	let mut tx = Transaction {
		version: transaction::Version::TWO,
		lock_time: absolute::LockTime::ZERO,
		input: inputs.iter().map(|(outpoint, _, _)| TxIn {
			previous_output: *outpoint,
			sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
			// A placeholder signature of the final size, for the fee.
			witness: Witness::from_slice(&[[0u8; 64]]),
			..Default::default()
		}).collect(),
		output: vec![TxOut { value: Amount::ZERO, script_pubkey: destination }],
	};
	let fee = fee_rate.fee_wu(tx.weight()).context("fee overflowed")?;
	let output_amount = total.checked_sub(fee)
		.with_context(|| format!("expiry payouts of {} do not cover the fee of {}", total, fee))?;
	if output_amount < P2TR_DUST {
		bail!("sweeping {} at a fee of {} leaves {}, which is below dust",
			total, fee, output_amount);
	}
	tx.output[0].value = output_amount;

	let prevouts = inputs.iter().map(|(_, o, _)| o.clone()).collect::<Vec<_>>();
	let mut shc = SighashCache::new(&mut tx);
	for (idx, (_, _, keypair)) in inputs.iter().enumerate() {
		let sighash = shc.taproot_key_spend_signature_hash(
			idx, &Prevouts::All(&prevouts), TapSighashType::Default,
		).expect("provided all prevouts");
		let tweaked = keypair.tap_tweak(&SECP, None).to_keypair();
		let signature = SECP.sign_schnorr_with_aux_rand(
			&sighash.into(), &tweaked, &rand::random(),
		);
		*shc.witness_mut(idx).expect("input exists") = Witness::p2tr_key_spend(
			&taproot::Signature { signature, sighash_type: TapSighashType::Default },
		);
	}

	Ok(tx)
}

/// Every VTXO of `wallet` in one of `states` whose expiry height the chain tip
/// has reached.
async fn expired_vtxos(
	wallet: &Wallet,
	states: &[VtxoStateKind],
) -> anyhow::Result<Vec<WalletVtxo>> {
	let tip = wallet.chain().tip().await?;
	let mut vtxos = wallet.all_vtxos().await?;
	vtxos.retain(|v| states.contains(&v.state.kind()) && v.vtxo.expiry_height() <= tip);
	Ok(vtxos)
}

async fn vtxos_by_id(wallet: &Wallet, ids: Vec<VtxoId>) -> anyhow::Result<Vec<WalletVtxo>> {
	let mut ret = Vec::with_capacity(ids.len());
	for id in ids {
		ret.push(wallet.get_vtxo_by_id(id).await?);
	}
	Ok(ret)
}

/// Ask the server for the state of each VTXO in `vtxo_ids` and adopt it, see
/// [Wallet::trust_and_adopt_server_vtxo_status]. A VTXO the server reports
/// spent is marked spent, so it leaves the balance and coin selection.
///
/// When `vtxo_ids` is `None`, this checks every unspent VTXO that has expired.
///
/// NOTE: Only call this with a server you trust.
pub async fn adopt_server_vtxo_status(
	wallet: &Wallet,
	vtxo_ids: Option<Vec<VtxoId>>,
) -> anyhow::Result<Vec<AdoptedVtxoStatus>> {
	let ids = match vtxo_ids {
		Some(ids) => ids,
		None => expired_vtxos(wallet, VtxoStateKind::UNSPENT_STATES).await?
			.into_iter().map(|v| v.vtxo.id()).collect(),
	};

	let mut ret = Vec::with_capacity(ids.len());
	for vtxo_id in ids {
		let adoption = wallet.trust_and_adopt_server_vtxo_status(vtxo_id).await?;
		ret.push(AdoptedVtxoStatus {
			vtxo_id,
			state: AdoptedVtxoState::from_adoption(adoption),
		});
	}
	Ok(ret)
}

/// Find the unspent on-chain outputs paying the [expiry_payout_script] of each
/// VTXO in `vtxo_ids`. A swept payout is spent, so it is not listed.
///
/// When `vtxo_ids` is `None`, this looks at every expired VTXO the wallet has
/// as spent, which includes the ones [adopt_server_vtxo_status] marked spent.
///
/// With a bitcoind chain source only confirmed payouts are found.
pub async fn find_expiry_payouts(
	wallet: &Wallet,
	vtxo_ids: Option<Vec<VtxoId>>,
) -> anyhow::Result<Vec<ExpiryPayout>> {
	let vtxos = match vtxo_ids {
		Some(ids) => vtxos_by_id(wallet, ids).await?,
		None => expired_vtxos(wallet, &[VtxoStateKind::Spent]).await?,
	};
	Ok(find_expiry_payouts_with_keys(wallet, &vtxos).await?
		.into_iter().map(|(payout, _)| payout).collect())
}

/// The payouts of [find_expiry_payouts], each with the key that spends it.
async fn find_expiry_payouts_with_keys(
	wallet: &Wallet,
	vtxos: &[WalletVtxo],
) -> anyhow::Result<Vec<(ExpiryPayout, Keypair)>> {
	let mut vtxos_by_script = BTreeMap::<ScriptBuf, Vec<VtxoId>>::new();
	let mut keys_by_script = HashMap::<ScriptBuf, Keypair>::new();
	for v in vtxos {
		let Some((_idx, keypair)) = wallet.pubkey_keypair(&v.vtxo.user_pubkey()).await? else {
			warn!("No key for vtxo {} in this wallet, cannot look for its payout", v.vtxo.id());
			continue;
		};
		let script = expiry_payout_script(keypair.public_key());
		vtxos_by_script.entry(script.clone()).or_default().push(v.vtxo.id());
		keys_by_script.insert(script, keypair);
	}

	let scripts = vtxos_by_script.keys().cloned().collect::<Vec<_>>();
	let utxos = wallet.chain().unspent_outputs_for_scripts(&scripts).await
		.context("failed to look up expiry payouts")?;
	let tip = wallet.chain().tip().await?;

	let mut ret = Vec::new();
	for utxo in utxos {
		let (Some(vtxo_ids), Some(keypair)) = (
			vtxos_by_script.get(&utxo.script_pubkey), keys_by_script.get(&utxo.script_pubkey),
		) else {
			continue;
		};
		let confirmations = confirmations(utxo.confirmed_height, tip);
		for vtxo_id in vtxo_ids {
			ret.push((ExpiryPayout {
				vtxo_id: *vtxo_id,
				outpoint: utxo.outpoint,
				amount: utxo.amount,
				confirmations,
			}, *keypair));
		}
	}
	Ok(ret)
}

/// Sweep every payout [find_expiry_payouts] finds by default to a fresh
/// address of the wallet's on-chain wallet, and record a finished movement of
/// kind [EXPIRY_PAYOUT_MOVEMENT_KIND].
///
/// `fee_rate` defaults to the chain source's regular fee rate.
pub async fn sweep_expiry_payouts(
	wallet: &Wallet,
	fee_rate: Option<FeeRate>,
) -> anyhow::Result<ExpiryPayoutSweep> {
	let onchain = wallet.onchain().context("sweeping expiry payouts needs an onchain wallet")?;

	let vtxos = expired_vtxos(wallet, &[VtxoStateKind::Spent]).await?;
	let payouts = find_expiry_payouts_with_keys(wallet, &vtxos).await?;
	if payouts.is_empty() {
		bail!("no expiry payouts to sweep");
	}

	let mut inputs = BTreeMap::<OutPoint, (TxOut, Keypair)>::new();
	for (payout, keypair) in &payouts {
		inputs.entry(payout.outpoint).or_insert_with(|| (TxOut {
			value: payout.amount,
			script_pubkey: expiry_payout_script(keypair.public_key()),
		}, *keypair));
	}
	let inputs = inputs.into_iter()
		.map(|(outpoint, (txout, keypair))| (outpoint, txout, keypair))
		.collect::<Vec<_>>();

	let fee_rate = match fee_rate {
		Some(r) => r,
		None => wallet.chain().fee_rates().await.regular,
	};
	let address = onchain.write().await.address().await
		.context("failed to get an onchain address")?;
	let tx = build_signed_expiry_payout_sweep(&inputs, address.script_pubkey(), fee_rate)?;
	let txid = tx.compute_txid();
	let swept = tx.output[0].value;

	wallet.chain().broadcast_tx(&tx).await.context("failed to broadcast the sweep")?;
	info!("Swept {} expiry payout(s) for {} in tx {}", inputs.len(), swept, txid);
	if let Err(e) = onchain.write().await.register_tx(&tx).await {
		warn!("Failed to register sweep tx {} with the onchain wallet: {:#}", txid, e);
	}

	let mut swept_vtxos = HashMap::new();
	for v in &vtxos {
		if payouts.iter().any(|(p, _)| p.vtxo_id == v.vtxo.id()) {
			swept_vtxos.insert(v.vtxo.id(), v.vtxo.amount());
		}
	}
	let vtxo_total = swept_vtxos.values().copied().sum::<Amount>();
	let payout_txids = {
		let mut txids = inputs.iter().map(|(o, _, _)| o.txid).collect::<Vec<_>>();
		txids.dedup();
		txids
	};
	wallet.movements_mgr().new_finished_movement(
		EXPIRY_PAYOUT_SUBSYSTEM,
		EXPIRY_PAYOUT_MOVEMENT_KIND,
		MovementStatus::Successful,
		MovementUpdate::new()
			.consumed_vtxos(swept_vtxos.keys().copied())
			.intended_and_effective_balance(-vtxo_total.to_signed()?)
			.sent_to([MovementDestination::bitcoin(address, swept)])
			.metadata([
				("payout_txids".into(), serde_json::to_value(&payout_txids)?),
				("sweep_txid".into(), serde_json::to_value(txid)?),
				("swept_sat".into(), swept.to_sat().into()),
			]),
	).await?;

	Ok(ExpiryPayoutSweep { txid, swept })
}

fn confirmations(confirmed_height: Option<BlockHeight>, tip: BlockHeight) -> u32 {
	confirmed_height
		.and_then(|h| tip.checked_blocks_since(h))
		.map(|blocks| blocks.saturating_add(1))
		.unwrap_or(0)
}

#[cfg(test)]
mod test {
	use super::*;

	use std::str::FromStr;

	use bitcoin::{Address, Network};
	use bitcoin::bip32::{ChildNumber, Xpriv};
	use bitcoin::hashes::Hash;
	use bitcoin::secp256k1::{schnorr, Message, SecretKey, XOnlyPublicKey};

	/// The first test vector of BIP86: `m/86'/0'/0'/0/0` of the "abandon … about"
	/// mnemonic, which `tr(KEY)` pays this way.
	#[test]
	fn expiry_payout_script_is_bip86() {
		let internal = XOnlyPublicKey::from_str(
			"cc8a4bc64d897bddc5fbc2f670f7a8ba0b386779106cf1223c6fc5d7cd6fc115",
		).unwrap();
		let pubkey = internal.public_key(bitcoin::secp256k1::Parity::Even);
		let script = expiry_payout_script(pubkey);
		assert_eq!(
			Address::from_script(&script, Network::Bitcoin).unwrap().to_string(),
			"bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr",
		);
	}

	/// The payout key is the wallet's VTXO key at `m/350'/0'/i`, so a descriptor
	/// wallet with `tr(xprv/350'/0'/*)` would find the same outputs.
	#[test]
	fn expiry_payout_script_matches_descriptor_derivation() {
		let master = Xpriv::new_master(Network::Regtest, &[7u8; 64]).unwrap();
		let path = [
			ChildNumber::from_hardened_idx(350).unwrap(),
			ChildNumber::from_hardened_idx(0).unwrap(),
			ChildNumber::from_normal_idx(3).unwrap(),
		];
		let key = master.derive_priv(&SECP, &path).unwrap().to_keypair(&SECP);
		let script = expiry_payout_script(key.public_key());

		let (xonly, _) = key.x_only_public_key();
		let tweaked = xonly.tap_tweak(&SECP, None).0;
		assert_eq!(script, ScriptBuf::new_p2tr_tweaked(tweaked));
		assert!(script.is_p2tr());
	}

	fn keypair(byte: u8) -> Keypair {
		Keypair::from_secret_key(&SECP, &SecretKey::from_slice(&[byte; 32]).unwrap())
	}

	fn payout(byte: u8, key: &Keypair, sat: u64) -> (OutPoint, TxOut, Keypair) {
		let outpoint = OutPoint::new(Txid::from_byte_array([byte; 32]), byte as u32);
		let txout = TxOut {
			value: Amount::from_sat(sat),
			script_pubkey: expiry_payout_script(key.public_key()),
		};
		(outpoint, txout, *key)
	}

	#[test]
	fn sweep_signatures_verify_against_tweaked_keys() {
		let a = keypair(1);
		let b = keypair(2);
		let inputs = vec![payout(1, &a, 50_000), payout(2, &b, 20_000)];
		let destination = expiry_payout_script(keypair(3).public_key());
		let fee_rate = FeeRate::from_sat_per_vb_u32(2);

		let tx = build_signed_expiry_payout_sweep(&inputs, destination.clone(), fee_rate).unwrap();

		assert_eq!(tx.input.len(), 2);
		assert_eq!(tx.output.len(), 1);
		assert_eq!(tx.output[0].script_pubkey, destination);
		let fee = Amount::from_sat(70_000) - tx.output[0].value;
		assert_eq!(fee, fee_rate.fee_wu(tx.weight()).unwrap());

		let prevouts = inputs.iter().map(|(_, o, _)| o.clone()).collect::<Vec<_>>();
		let mut shc = SighashCache::new(&tx);
		for (idx, (_, txout, _)) in inputs.iter().enumerate() {
			let witness = &tx.input[idx].witness;
			assert_eq!(witness.len(), 1, "key-path spend carries only a signature");
			let sig = schnorr::Signature::from_slice(witness.nth(0).unwrap()).unwrap();
			let sighash = shc.taproot_key_spend_signature_hash(
				idx, &Prevouts::All(&prevouts), TapSighashType::Default,
			).unwrap();
			let output_key = XOnlyPublicKey::from_slice(&txout.script_pubkey.as_bytes()[2..]).unwrap();
			SECP.verify_schnorr(&sig, &Message::from(sighash), &output_key)
				.expect("signature verifies against the tweaked output key");
		}
	}

	#[test]
	fn sweep_refuses_to_produce_dust() {
		let err = build_signed_expiry_payout_sweep(
			&[payout(1, &keypair(1), 400)],
			expiry_payout_script(keypair(3).public_key()),
			FeeRate::from_sat_per_vb_u32(2),
		).unwrap_err();
		assert!(err.to_string().contains("below dust"), "{err}");
	}

	#[test]
	fn confirmations_count_the_confirming_block() {
		let tip = BlockHeight::new(100);
		assert_eq!(confirmations(None, tip), 0);
		assert_eq!(confirmations(Some(BlockHeight::new(100)), tip), 1);
		assert_eq!(confirmations(Some(BlockHeight::new(91)), tip), 10);
	}

	#[test]
	fn adopted_state_names() {
		assert_eq!(AdoptedVtxoState::from_adoption(None), AdoptedVtxoState::Other);
		assert_eq!(
			AdoptedVtxoState::from_adoption(Some(ServerStatusAdoption::InFlight(VtxoSpendState::Unregistered))),
			AdoptedVtxoState::Unregistered,
		);
		assert_eq!(serde_json::to_string(&AdoptedVtxoState::Spent).unwrap(), "\"spent\"");
	}
}
