//! Reconcile expired coins and collect historical coin-key payouts.
//!
//! Registered fallback payouts arrive directly in the BIP84 on-chain wallet.
//! Status adoption atomically records the Ark debit once, without claiming a
//! payout destination or transaction from the spent status alone.
//!
//! A server can choose to settle a VTXO that expired unrefreshed by paying its
//! value on-chain to the key-path address of the VTXO's own key,
//! `tr(user_pubkey)`, and marking the VTXO spent on its side. The wallet is not
//! told about this. These functions let the wallet:
//!
//! 1. adopt the server's spent state, so the VTXO leaves the balance and coin
//!    selection ([adopt_server_vtxo_status]);
//! 2. find the payout outputs on-chain ([find_expiry_payouts]);
//! 3. sweep them into the on-chain wallet and record a movement
//!    ([sweep_expiry_payouts]).
//!
//! Optional fee receipts come from `<server_address>/expiry-payouts/<txid>.json`:
//! `{"txid":"...","outputs":[{"vout":0,"amount_sat":10000,"fee_sat":150}]}`.
//! The txid, output index and received amount must match. Missing receipts leave
//! the fee unknown and do not prevent discovery or spending. A sweep records the
//! sum of known original fees in `payout_fee_sat` movement metadata, or null when
//! any fee is unknown. The sweep's own mining fee is `sweep_fee_sat`; its Ark
//! balance delta is zero because adoption already removed the expired coin.

use std::collections::BTreeMap;
use std::time::Duration;

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
use futures::StreamExt;

use serde::{Deserialize, Serialize};

use ark::{SECP, VtxoId};
use bitcoin_ext::{BlockHeight, P2TR_DUST};
use server_rpc::protos::VtxoSpendState;

use crate::{Wallet, WalletVtxo};
use crate::chain::ScriptUtxo;
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
/// A reused key does not identify which historical entitlement was settled.
/// Each output is listed once; its coin ID is absent when ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryPayout {
	pub vtxo_id: Option<VtxoId>,
	pub outpoint: OutPoint,
	pub amount: Amount,
	/// The operator's recorded deduction for this output, when available.
	pub fee: Option<Amount>,
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

/// Every VTXO the wallet has as spent whose expiry height the chain tip has
/// reached, including the ones [adopt_server_vtxo_status] marked spent.
async fn expired_spent_vtxos(wallet: &Wallet) -> anyhow::Result<Vec<WalletVtxo>> {
	let tip = wallet.chain().tip().await?;
	let mut vtxos = wallet.all_vtxos().await?;
	vtxos.retain(|v| v.state.kind() == VtxoStateKind::Spent && v.vtxo.expiry_height() <= tip);
	Ok(vtxos)
}

/// Ask the server for the state of each VTXO in `vtxo_ids` and adopt it, see
/// [Wallet::trust_and_adopt_server_vtxo_status]. A VTXO the server reports
/// spent is marked spent, so it leaves the balance and coin selection.
///
/// NOTE: Only call this with a server you trust.
pub async fn adopt_server_vtxo_status(
	wallet: &Wallet,
	vtxo_ids: Vec<VtxoId>,
) -> anyhow::Result<Vec<AdoptedVtxoStatus>> {
	let mut ret = Vec::with_capacity(vtxo_ids.len());
	for vtxo_id in vtxo_ids {
		let adoption = wallet.trust_and_adopt_server_vtxo_status(vtxo_id).await?;
		ret.push(AdoptedVtxoStatus {
			vtxo_id,
			state: AdoptedVtxoState::from_adoption(adoption),
		});
	}
	Ok(ret)
}

impl Wallet {
	/// Reconcile expired coins before automatic refresh. An expired coin may
	/// already have been paid on-chain while the wallet was offline. Uncertain
	/// or in-flight states stay in the wallet but are excluded from this attempt.
	pub(crate) async fn sync_expired_vtxos(&self) -> anyhow::Result<Vec<VtxoId>> {
		let tip = self.chain().tip().await?;
		let mut unavailable = Vec::new();
		for vtxo in self.spendable_vtxos().await? {
			if vtxo.expiry_height() > tip { continue; }
			match self.trust_and_adopt_server_vtxo_status(vtxo.id()).await {
				Ok(Some(ServerStatusAdoption::Spendable)) => {},
				Ok(_) => unavailable.push(vtxo.id()),
				Err(e) => {
					warn!("Expired VTXO {} status unavailable; deferring refresh: {e:#}", vtxo.id());
					unavailable.push(vtxo.id());
				},
			}
		}
		Ok(unavailable)
	}
}

/// Find the unspent on-chain outputs paying the [expiry_payout_script] of
/// every expired VTXO the wallet has as spent. A swept payout is spent, so it
/// is not listed.
///
/// Both confirmed and mempool payouts are included.
pub async fn find_expiry_payouts(wallet: &Wallet) -> anyhow::Result<Vec<ExpiryPayout>> {
	let vtxos = expired_spent_vtxos(wallet).await?;
	Ok(find_expiry_payouts_with_keys(wallet, &vtxos).await?
		.into_iter().map(|(payout, _)| payout).collect())
}

/// The payouts of [find_expiry_payouts], each with the key that spends it.
async fn find_expiry_payouts_with_keys(
	wallet: &Wallet,
	vtxos: &[WalletVtxo],
) -> anyhow::Result<Vec<(ExpiryPayout, Keypair)>> {
	let mut by_script = BTreeMap::<ScriptBuf, (Keypair, Vec<VtxoId>)>::new();
	for v in vtxos {
		let Some((_idx, keypair)) = wallet.pubkey_keypair(&v.vtxo.user_pubkey()).await? else {
			warn!("No key for vtxo {} in this wallet, cannot look for its payout", v.vtxo.id());
			continue;
		};
		by_script.entry(expiry_payout_script(keypair.public_key()))
			.or_insert_with(|| (keypair, Vec::new())).1.push(v.vtxo.id());
	}

	let scripts = by_script.keys().cloned().collect::<Vec<_>>();
	let utxos = wallet.chain().unspent_outputs_for_scripts(&scripts).await
		.context("failed to look up expiry payouts")?;
	let tip = wallet.chain().tip().await?;

	let mut payouts = payouts_for_scripts(&by_script, utxos, tip);
	attach_fee_receipts(wallet.config(), &mut payouts).await;
	Ok(payouts)
}

fn payouts_for_scripts(
	by_script: &BTreeMap<ScriptBuf, (Keypair, Vec<VtxoId>)>,
	utxos: Vec<ScriptUtxo>,
	tip: BlockHeight,
) -> Vec<(ExpiryPayout, Keypair)> {
	let mut ret = Vec::new();
	for utxo in utxos {
		let Some((keypair, vtxo_ids)) = by_script.get(&utxo.script_pubkey) else {
			continue;
		};
		let confirmations = confirmations(utxo.confirmed_height, tip);
		ret.push((ExpiryPayout {
			vtxo_id: match vtxo_ids.as_slice() { [id] => Some(*id), _ => None },
			outpoint: utxo.outpoint,
			amount: utxo.amount,
			fee: None,
			confirmations,
		}, *keypair));
	}
	ret
}

#[derive(Deserialize)]
struct FeeReceipt {
	txid: Txid,
	outputs: Vec<FeeReceiptOutput>,
}

#[derive(Deserialize)]
struct FeeReceiptOutput {
	vout: u32,
	amount_sat: u64,
	fee_sat: u64,
}

impl FeeReceipt {
	fn fee_for(&self, payout: &ExpiryPayout) -> Option<Amount> {
		if self.txid != payout.outpoint.txid { return None; }
		let mut matches = self.outputs.iter().filter(|o| o.vout == payout.outpoint.vout);
		let output = matches.next()?;
		if matches.next().is_some() || output.amount_sat != payout.amount.to_sat() { return None; }
		let gross = output.amount_sat.checked_add(output.fee_sat)?;
		(gross <= Amount::MAX_MONEY.to_sat()).then(|| Amount::from_sat(output.fee_sat))
	}
}

/// Optional static receipts at the Ark server's HTTP origin. An outage must
/// neither hide funds nor prevent spending them; the entire lookup has a bound.
async fn attach_fee_receipts(config: &crate::Config, payouts: &mut [(ExpiryPayout, Keypair)]) {
	if payouts.is_empty() { return; }
	let builder = reqwest::Client::builder();
	#[cfg(feature = "socks5-proxy")]
	let builder = if let Some(proxy) = &config.socks5_proxy {
		let Ok(proxy) = reqwest::Proxy::all(proxy) else { return; };
		builder.proxy(proxy)
	} else { builder };
	let Ok(client) = builder.build() else { return; };
	let txids = payouts.iter().map(|(p, _)| p.outpoint.txid)
		.collect::<std::collections::BTreeSet<_>>();
	let mut requests = futures::stream::iter(txids).map(|txid| {
		let request = client.get(format!("{}/expiry-payouts/{txid}.json", config.server_address.trim_end_matches('/')));
		async move {
			let response = request.send().await?.error_for_status()?;
			response.json::<FeeReceipt>().await
		}
	}).buffer_unordered(4);
	let _ = bark_runtime::timeout(Duration::from_secs(3), async {
		while let Some(result) = requests.next().await {
			if let Ok(receipt) = result {
				for (payout, _) in payouts.iter_mut().filter(|(p, _)| p.outpoint.txid == receipt.txid) {
					payout.fee = receipt.fee_for(payout);
				}
			}
		}
	}).await;
}

/// Sweep every payout [find_expiry_payouts] finds to a fresh
/// address of the wallet's on-chain wallet, and record a finished movement of
/// kind [EXPIRY_PAYOUT_MOVEMENT_KIND].
///
/// `fee_rate` defaults to the chain source's regular fee rate.
pub async fn sweep_expiry_payouts(
	wallet: &Wallet,
	fee_rate: Option<FeeRate>,
) -> anyhow::Result<ExpiryPayoutSweep> {
	let onchain = wallet.onchain().context("sweeping expiry payouts needs an onchain wallet")?;

	let vtxos = expired_spent_vtxos(wallet).await?;
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

	let payout_total = inputs.iter().map(|(_, output, _)| output.value).sum::<Amount>();
	let payout_fee_sat = payouts.iter().map(|(p, _)| p.fee.map(|f| f.to_sat())).sum::<Option<u64>>();
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
			// Adoption already debited the Ark coin. This transaction only moves
			// on-chain outputs, including when a spent coin was imported.
			.intended_and_effective_balance(bitcoin::SignedAmount::ZERO)
			.sent_to([MovementDestination::bitcoin(address, swept)])
			.metadata([
				("payout_fee_sat".into(), serde_json::to_value(payout_fee_sat)?),
				("payout_txids".into(), serde_json::to_value(&payout_txids)?),
				("sweep_txid".into(), serde_json::to_value(txid)?),
				("swept_sat".into(), swept.to_sat().into()),
				("payout_total_sat".into(), payout_total.to_sat().into()),
				("sweep_fee_sat".into(), (payout_total - swept).to_sat().into()),
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
	fn reused_key_does_not_attribute_a_payout_to_every_old_coin() {
		let key = keypair(1);
		let script = expiry_payout_script(key.public_key());
		let old = VtxoId::from(OutPoint::new(Txid::from_byte_array([1; 32]), 0));
		let later = VtxoId::from(OutPoint::new(Txid::from_byte_array([2; 32]), 0));
		// The old coin was refreshed; the later coin expired. Both are spent
		// and share a key. A restored wallet only has those facts.
		let by_script = BTreeMap::from([(script.clone(), (key, vec![old, later]))]);
		let output = ScriptUtxo {
			outpoint: OutPoint::new(Txid::from_byte_array([3; 32]), 0),
			script_pubkey: script,
			amount: Amount::from_sat(49_000),
			confirmed_height: Some(BlockHeight::new(100)),
		};
		let payouts = payouts_for_scripts(&by_script, vec![output], BlockHeight::new(100));
		assert_eq!(payouts.len(), 1, "one UTXO, not one payout per historical coin");
		assert_eq!(payouts[0].0.vtxo_id, None, "a shared key cannot identify the paid coin");
		assert_eq!(payouts[0].0.amount.to_sat(), 49_000);
	}

	#[test]
	fn receipt_fee_belongs_to_the_exact_output() {
		let txid = Txid::from_byte_array([3; 32]);
		let payout = ExpiryPayout {
			vtxo_id: None, outpoint: OutPoint::new(txid, 2),
			amount: Amount::from_sat(49_850), fee: None, confirmations: 1,
		};
		let mut receipt = FeeReceipt { txid, outputs: vec![
			FeeReceiptOutput { vout: 0, amount_sat: 69_849, fee_sat: 151 },
			FeeReceiptOutput { vout: 2, amount_sat: 49_850, fee_sat: 150 },
		] };
		assert_eq!(receipt.fee_for(&payout), Some(Amount::from_sat(150)));
		receipt.txid = Txid::from_byte_array([4; 32]);
		assert_eq!(receipt.fee_for(&payout), None);
		receipt.txid = txid;
		receipt.outputs[1].amount_sat += 1;
		assert_eq!(receipt.fee_for(&payout), None);
		receipt.outputs[1].amount_sat -= 1;
		receipt.outputs.push(FeeReceiptOutput { vout: 2, amount_sat: 49_850, fee_sat: 151 });
		assert_eq!(receipt.fee_for(&payout), None);
	}

	#[cfg(not(target_arch = "wasm32"))]
	#[tokio::test]
	async fn receipt_lookup_preserves_outputs_when_unavailable() {
		use std::io::{Read, Write};
		let txid = Txid::from_byte_array([5; 32]);
		for (status, body, fee) in [
			("200 OK", format!(r#"{{"txid":"{txid}","outputs":[{{"vout":0,"amount_sat":10000,"fee_sat":151}},{{"vout":1,"amount_sat":20000,"fee_sat":150}}]}}"#), Some(151)),
			("404 Not Found", String::new(), None),
			("200 OK", "unavailable".to_owned(), None),
		] {
			let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
			let config = crate::Config {
				server_address: format!("http://{}", listener.local_addr().unwrap()),
				..crate::Config::network_default(Network::Regtest)
			};
			let server = std::thread::spawn(move || {
				let (mut socket, _) = listener.accept().unwrap();
				socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
				let mut request = [0; 4096];
				let n = socket.read(&mut request).unwrap();
				assert!(String::from_utf8_lossy(&request[..n]).starts_with(&format!("GET /expiry-payouts/{txid}.json ")));
				write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
			});
			let mut payouts = (0..2).map(|vout| (ExpiryPayout {
				vtxo_id: None, outpoint: OutPoint::new(txid, vout),
				amount: Amount::from_sat(10_000 * (vout as u64 + 1)), fee: None, confirmations: 0,
			}, keypair(1))).collect::<Vec<_>>();
			attach_fee_receipts(&config, &mut payouts).await;
			server.join().unwrap();
			assert_eq!(payouts.len(), 2);
			assert_eq!(payouts[0].0.fee.map(|f| f.to_sat()), fee);
			assert_eq!(payouts[1].0.fee.map(|f| f.to_sat()), fee.map(|_| 150));
			assert_eq!(payouts[0].0.amount.to_sat(), 10_000);
			assert_eq!(payouts[1].0.amount.to_sat(), 20_000);
		}
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
