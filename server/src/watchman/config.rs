use std::{num::NonZeroUsize, time::Duration};
use std::path::PathBuf;

use bitcoin::{Amount, FeeRate};

use bitcoin_ext::BlockDelta;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectExpiryConfig {
	/// Immutable lifetime ordinary allowance and confirmed urgent reserve.
	#[serde(default)]
	pub ordinary_fee_policy: super::ordinary_fee::OrdinaryFeePolicy,
	/// Captaind's independently backed-up datadir, shared with this daemon.
	pub history_dir: PathBuf,
	/// Absolute mining-fee ceiling for each signed direct claim.
	#[serde(with = "crate::utils::serde::string")]
	pub max_miner_fee: Amount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
	/// Direct settlement of source-frozen groups. No legacy claim/progression
	/// may consume their backing when this route is configured.
	#[serde(default)]
	pub direct_expiry: Option<DirectExpiryConfig>,
	/// Interval at which to react to on-chain exit activity: progressing exit
	/// chains and claiming VTXOs before a counterparty's timelock matures.
	/// This work races other parties, so it should run often.
	#[serde(with = "crate::utils::serde::duration")]
	pub reaction_interval: Duration,

	/// Interval at which to sweep VTXOs that only we can spend. Sweeps have
	/// no deadline, so this can be much longer than [Self::reaction_interval].
	#[serde(with = "crate::utils::serde::duration")]
	pub sweep_interval: Duration,

	/// This grace period allows users who are performing an exit to complete
	/// their on-chain transaction before the server interferes by attempting
	/// to progress the VTXO.
	pub progress_grace_period: BlockDelta,

	/// Maximum number of VTXOs to process in a single claim transaction.
	pub claim_chunksize: NonZeroUsize,

	/// Minimum fee bump required for RBF in sat/kvb.
	#[serde(with = "crate::utils::serde::fee_rate")]
	pub incremental_relay_fee: FeeRate,

	/// Minimum confirmed funds in sats required to create a CPFP transaction.
	#[serde(with = "crate::utils::serde::string")]
	pub min_cpfp_amount: Amount,
}
