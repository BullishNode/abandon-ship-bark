//! Shared expiry source checks and the removed two-step payout selector.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use ark::{Vtxo, VtxoId};
use bitcoin::{OutPoint, Transaction, Txid};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use serde::{Deserialize, Serialize};

use crate::Server;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
	/// Retained selector; enabling the removed two-step producer is refused.
	pub enabled: bool,
	/// Obsolete scheduler value, parsed without starting a payout worker.
	#[serde(with = "crate::utils::serde::duration")]
	pub interval: Duration,
	/// Shared source grace, set from direct_expiry when that route starts.
	pub grace_blocks: u32,
	/// Shared swept-path confirmation depth used by receive-refund guards.
	pub sweep_min_confs: u32,
	/// Obsolete two-step minimum; direct payouts use their script's dust limit.
	pub min_payout_sat: u64,
	/// Obsolete estimator target, parsed without setting direct fee policy.
	pub conf_target_blocks: u16,
	/// Obsolete two-step watchman path; direct history has its own configuration.
	pub watchman_config: Option<PathBuf>,
}

impl Default for Config {
	fn default() -> Self {
		Self { enabled: false, interval: Duration::from_secs(60), grace_blocks: 1008,
			sweep_min_confs: 100, min_payout_sat: 1_000,
			conf_target_blocks: 6, watchman_config: None }
	}
}

impl Config {
	pub fn validate(&self, _network: bitcoin::Network) -> anyhow::Result<()> {
		ensure!(!self.enabled, "legacy expiry_payout producer is unavailable; use direct_expiry");
		Ok(())
	}
}

impl Server {
	/// Whether a confirmed sweep spent this coin's own backing path into the
	/// rounds wallet, at the configured depth.
	pub(crate) async fn expiry_path_swept(&self, vtxo: &Vtxo) -> anyhow::Result<bool> {
		let anchor = vtxo.chain_anchor();
		if self.bitcoind.try_get_tx_out(anchor, true).await?.is_some() { return Ok(false); }
		// A crash can leave a persisted unsigned round that Core never saw.
		// Keep that candidate waiting without starving later funded coins.
		let Some(anchor_info) = crate::bitcoind::custom_get_raw_transaction_info(
			&self.bitcoind, anchor.txid, None,
		).await? else { return Ok(false); };
		let anchor_tx = bitcoin::consensus::deserialize(&anchor_info.hex)?;
		vtxo.validate_unsigned(&anchor_tx).context("invalid expiry path")?;
		let path = std::iter::once(anchor).chain(vtxo.transactions()
			.map(|t| OutPoint::new(t.tx.compute_txid(), t.output_idx as u32))).collect::<Vec<_>>();
		let ids = path.iter().map(|op| op.to_string().parse::<VtxoId>()).collect::<Result<Vec<_>,_>>()?;
		let hints = self.db.read(async |t| t.expiry_settlement_spenders(&ids).await).await?;
		for (outpoint, spender) in path.into_iter().zip(hints) {
			if outpoint != anchor && self.bitcoind.try_get_tx_out(outpoint, true).await?.is_some() { break; }
			let Some(spender) = spender else { continue; };
			let spender = Txid::from_str(&spender)?;
			let Some(info) = crate::bitcoind::custom_get_raw_transaction_info(&self.bitcoind, spender, None).await? else { continue; };
			if info.confirmations.unwrap_or(0) < self.config.expiry_payout.sweep_min_confs { continue; }
			let tx: Transaction = bitcoin::consensus::deserialize(&info.hex)?;
			if !tx.input.iter().any(|i| i.previous_output == outpoint) { continue; }
			let wallet = self.rounds_wallet.lock().await;
			let outputs = tx.output.iter().filter(|o| !o.script_pubkey.is_op_return()
				&& o.script_pubkey.as_bytes() != [0x51, 0x02, 0x4e, 0x73]).collect::<Vec<_>>();
			if !outputs.is_empty() && outputs.iter().all(|o| wallet.is_mine(o.script_pubkey.clone())) { return Ok(true); }
		}
		Ok(false)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn legacy_payout_selector_is_disabled_and_cannot_be_enabled() {
		let mut cfg = Config::default();
		assert!(!cfg.enabled);
		for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Signet, bitcoin::Network::Regtest] {
			cfg.validate(network).unwrap();
		}
		cfg.enabled = true;
		for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Signet, bitcoin::Network::Regtest] {
			assert!(cfg.validate(network).unwrap_err().to_string().contains("legacy expiry_payout producer is unavailable"));
		}
	}
}
