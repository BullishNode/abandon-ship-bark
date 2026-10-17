

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use bitcoin::consensus::encode::deserialize_hex;
use clap;
use log::{debug, info, warn};

use ark::{ArkInfo, Vtxo, VtxoId};
use ark::encode::ProtocolEncoding;
use ark::vtxo::Full;
use bark::ImportVtxoArgs;
use bark::vtxo::VtxoStateKind;
use bark_json::primitives::{VtxoInfo, WalletVtxoInfo};
use server_rpc as rpc;

use bark_cli::wallet::open_wallet;

use bark_cli::util::{self, output_json};

#[derive(clap::Subcommand)]
pub enum DevCommand {
	// ** some general static dev commands

	/// play with vtxos
	#[command(subcommand)]
	Vtxo(VtxoCommand),

	/// Show the wallet's authenticated public fallback registration
	#[command()]
	FallbackRecord,

	/// inspect the `ArkInfo` of the given server (defaults to wallet server)
	#[command()]
	ArkInfo {
		/// the address of the Ark server to inspect (defaults to wallet server)
		ark_address: String,
	},
}

pub async fn execute_dev_command(
	command: DevCommand,
	datadir: PathBuf,
) -> anyhow::Result<()> {
	match command {
		DevCommand::Vtxo(c) => execute_vtxo_command(&datadir, c).await?,
		DevCommand::FallbackRecord => {
			let wallet = open_wallet(&datadir, crate::USER_AGENT).await
				.context("failed to open wallet")?.context("no wallet found")?;
			let record = wallet.fallback_destination().await?;
			let network = wallet.network().await?;
			let address = bitcoin::Address::from_script(&record.spk, network)
				.context("fallback script is not a supported address")?;
			output_json(&serde_json::json!({
				"network": network,
				"mailbox_pk": wallet.mailbox_identifier().as_pubkey(),
				"payout_address": address,
				"record_hex": record.serialize_hex(),
				"record": record,
			}));
		},
		DevCommand::ArkInfo { ark_address } => {
			let mut srv = connect_server(ark_address).await
				.context("failed to connect to server")?;
			let res = srv.get_ark_info(rpc::protos::Empty {}).await
				.context("ark_info request failed")?;
			let info = ArkInfo::try_from(res.into_inner())
				.context("invalid ark info from ark server")?;
			output_json(&bark_json::cli::ArkInfo::from(info));
		},
	}
	Ok(())
}

#[derive(clap::Subcommand)]
pub enum VtxoCommand {
	/// decode a serialized VTXO
	#[command()]
	Decode {
		/// VTXO encoded in hex
		vtxo: String,
	},

	/// Verify a VTXO's signed path against its funding transaction, without opening a wallet
	#[command()]
	Verify {
		/// VTXO encoded in hex
		vtxo: String,
		/// Raw funding transaction encoded in hex; confirmation is checked separately
		funding_tx: String,
		/// Also check that the signed coin pays this Ark address on its intended server
		#[arg(long)]
		address: Option<String>,
	},

	/// Drops a vtxo from the database (dangerous)
	#[command()]
	Drop {
		/// You must use this flag to acknowledge the danger of running this command
		#[arg(long = "dangerous")]
		dangerous: bool,
		/// Drop all vtxos
		#[arg(long = "all")]
		all: bool,
		/// Mention a specific vtxo. You can use it multiple times
		#[arg(long= "vtxo")]
		vtxo: Vec<VtxoId>,
	},

	/// Take the server's word for the state of VTXOs in this wallet (dangerous)
	///
	/// Asks the server about each VTXO and writes the answer into the wallet.
	/// The server is believed without question, so a VTXO it calls spent is
	/// marked spent here and disappears from your balance. A VTXO locked by a
	/// running operation keeps its lock.
	#[command()]
	TrustAndAdoptServerStatus {
		/// You must use this flag to acknowledge the danger of running this command
		#[arg(long = "dangerous")]
		dangerous: bool,
		/// Check every VTXO in the wallet that has not exited
		#[arg(long = "all", conflicts_with = "vtxos")]
		all: bool,
		/// The VTXOs to check
		vtxos: Vec<VtxoId>,
	},

	/// Import serialized VTXOs into the wallet
	#[command()]
	Import {
		/// VTXOs encoded in hex
		vtxos: Vec<String>,
		/// (deprecated) VTXOs encoded in hex
		#[arg(long = "vtxo", hide = true)]
		vtxo_multi: Vec<String>,
		/// How many consecutive unused key indices to scan for each VTXO's user
		/// pubkey. Overrides the wallet's configured gap limit.
		#[arg(long = "gap-limit")]
		gap_limit: Option<u32>,
		/// Import as spendable without asking the server for each VTXO's state.
		///
		/// Use it when you already know it's spendable or when the server can't be reached,
		/// as it can leave the wallet in an inconsistent state.
		#[arg(long = "skip-status-check")]
		skip_status_check: bool,
		/// Keep the VTXOs that import successfully even when another one in the
		/// batch fails. Only the VTXOs that were kept are reported.
		#[arg(long = "allow-partial")]
		allow_partial: bool,
	},
}

fn verify_vtxo(vtxo: &str, funding_tx: &str, address: Option<&str>) -> anyhow::Result<Vtxo<Full>> {
	let vtxo = <Vtxo<Full>>::deserialize_hex(vtxo).context("invalid vtxo")?;
	let funding_tx: bitcoin::Transaction = deserialize_hex(funding_tx)
		.context("invalid funding transaction")?;
	// Validation checks the output and signatures. Bind the supplied transaction
	// to the declared anchor as well, including VTXOs with no genesis transitions.
	if funding_tx.compute_txid() != vtxo.chain_anchor().txid {
		bail!("funding transaction ID does not match the VTXO chain anchor");
	}
	vtxo.validate(&funding_tx).context("VTXO signed path failed validation")?;
	if let Some(address) = address {
		let address = ark::Address::from_str(address).context("invalid Ark address")?;
		ensure!(address.is_for_server(vtxo.server_pubkey()), "Ark address is for another server");
		ensure!(address.policy() == vtxo.policy(), "signed coin does not pay the Ark address policy");
	}
	Ok(vtxo)
}

async fn execute_vtxo_command(datadir: &Path, command: VtxoCommand) -> anyhow::Result<()> {
	match command {
		VtxoCommand::Decode { vtxo } => {
			let vtxo = <Vtxo<Full>>::deserialize_hex(&vtxo).context("invalid vtxo")?;
			// for --verbose print the debug format as well
			debug!("{:#?}", vtxo);
			let info = VtxoInfo::from(vtxo);
			output_json(&info);
		},
		VtxoCommand::Verify { vtxo, funding_tx, address } => {
			let vtxo = verify_vtxo(&vtxo, &funding_tx, address.as_deref())?;
			let mut path_outpoints = vec![vtxo.chain_anchor()];
			for step in vtxo.transactions() {
				path_outpoints.push(bitcoin::OutPoint::new(
					step.tx.compute_txid(), u32::try_from(step.output_idx)?,
				));
			}
			output_json(&serde_json::json!({
				"verified": true,
				"address_verified": address.is_some(),
				"anchor_txid": vtxo.chain_anchor().txid,
				"path_outpoints": path_outpoints,
				"vtxo": VtxoInfo::from(vtxo),
			}));
		},
		VtxoCommand::Drop { dangerous, all, vtxo} => {
			if !dangerous {
				bail!("You must acknowledge the danger. Run again with --dangerous")
			}

			let wallet = open_wallet(&datadir, crate::USER_AGENT).await
				.context("Failed to open wallet")?
				.context("No wallet found")?;

			if all {
				log::info!("Dropping all vtxos");
				wallet.dangerous_drop_all_vtxos().await
					.context("Failed to drop vtxos")?;
			}

			for v in vtxo {
				log::info!("Dropping vtxo {}", v);
				wallet.dangerous_drop_vtxo(v).await
					.context("Failed to drop vtxo")?;
			}
		}
		VtxoCommand::TrustAndAdoptServerStatus { dangerous, all, vtxos } => {
			if !dangerous {
				bail!("You must acknowledge the danger. Run again with --dangerous")
			}
			if !all && vtxos.is_empty() {
				bail!("Pass either --all or a list of vtxo ids");
			}

			let wallet = open_wallet(&datadir, crate::USER_AGENT).await
				.context("Failed to open wallet")?
				.context("No wallet found")?;

			let ids = if all {
				wallet.all_vtxos().await.context("Failed to list vtxos")?
					.iter()
					.filter(|v| v.state.kind() != VtxoStateKind::Exited)
					.map(|v| v.id())
					.collect()
			} else {
				vtxos
			};

			let mut updated = Vec::with_capacity(ids.len());
			let mut failed = 0;
			for vtxo_id in ids {
				match wallet.trust_and_adopt_server_vtxo_status(vtxo_id).await {
					Ok(Some(adoption)) => info!("Server reports vtxo {} as {:?}", vtxo_id, adoption),
					Ok(None) => info!("Vtxo {} is locked, leaving it alone", vtxo_id),
					Err(e) => {
						warn!("Failed to check vtxo {}: {:#}", vtxo_id, e);
						failed += 1;
						continue;
					},
				}

				match wallet.get_vtxo_by_id(vtxo_id).await {
					Ok(wallet_vtxo) => updated.push(WalletVtxoInfo::from(&wallet_vtxo)),
					Err(e) => {
						warn!("Failed to get vtxo {}: {:#}", vtxo_id, e);
						failed += 1;
					},
				}
			}
			output_json(&updated);
			if failed > 0 {
				bail!("Failed to update {} vtxos", failed);
			}
		},
		VtxoCommand::Import { vtxos, vtxo_multi, gap_limit, skip_status_check, allow_partial } => {
			if vtxos.is_empty() && vtxo_multi.is_empty() {
				bail!("No VTXOs provided. Add raw VTXO arguments to import");
			}

			if !vtxo_multi.is_empty() {
				warn!("The --vtxo flag is deprecated. You can pass arguments \
					directly without the flag.");
			}


			// first try to parse all
			let mut to_import = Vec::with_capacity(vtxos.len() + vtxo_multi.len());
			for vtxo_hex in vtxos.into_iter().chain(vtxo_multi) {
				let vtxo = Vtxo::deserialize_hex(&vtxo_hex)
					.with_context(|| format!("invalid vtxo: {}", vtxo_hex))?;
				to_import.push(vtxo);
			}

			let wallet = open_wallet(&datadir, crate::USER_AGENT).await
				.context("Failed to open wallet")?
				.context("No wallet found")?;

			info!("Importing {} VTXOs...", to_import.len());
			let args = ImportVtxoArgs {
				gap_limit, skip_status_check, allow_partial,
			};
			let ids = wallet.import_vtxos(&to_import, args).await
				.context("Failed to import vtxos")?;

			let mut imported = Vec::with_capacity(ids.len());
			for vtxo_id in ids {
				let wallet_vtxo = wallet.get_vtxo_by_id(vtxo_id).await
					.with_context(|| format!("Failed to get imported vtxo {}", vtxo_id))?;
				imported.push(WalletVtxoInfo::from(&wallet_vtxo));
			}
			output_json(&imported);
		}
	}
	Ok(())
}

/// Build a tonic endpoint from a server address, configuring timeouts and TLS if required.
///
/// - Supports `http` and `https` URIs. Any other scheme results in an error.
/// - Uses a 10-minute keep-alive and overall request timeout to accommodate long-running RPCs.
/// - When `https` is used, the crate-configured root CAs are enabled and the SNI domain is set.
fn create_server_endpoint(address: &str) -> anyhow::Result<tonic::transport::Endpoint> {
	let uri = tonic::transport::Uri::from_str(address)
		.context("failed to parse Ark server as a URI")?;

	let scheme = uri.scheme_str().unwrap_or("");
	if scheme != "http" && scheme != "https" {
		bail!("Ark server scheme must be either http or https. Found: {}", scheme);
	}

	let mut endpoint = tonic::transport::Channel::builder(uri.clone())
		.keep_alive_timeout(Duration::from_secs(600))
		.timeout(Duration::from_secs(600));

	if scheme == "https" {
		info!("Connecting to Ark server at {} using TLS...", address);
		let uri_auth = uri.clone().into_parts().authority
			.context("Ark server URI is missing an authority part")?;
		let domain = uri_auth.host();

		let tls_config = tonic::transport::ClientTlsConfig::new()
			.with_enabled_roots()
			.domain_name(domain);
		endpoint = endpoint.tls_config(tls_config)?
	} else {
		info!("Connecting to Ark server at {} without TLS...", address);
	};
	Ok(endpoint)
}

/// connect to an Ark server
pub async fn connect_server(
	address: String,
) -> anyhow::Result<rpc::ArkServiceClient<tonic::transport::Channel>> {
	let address = util::default_scheme("https", address)?;
	let endpoint = create_server_endpoint(&address)?;
	let channel = endpoint.connect().await
		.context("couldn't connect to Ark server")?;
	Ok(rpc::ArkServiceClient::new(channel))
}

#[cfg(test)]
mod tests {
	use bitcoin::consensus::encode::serialize_hex;
	use ark::ProtocolEncoding;
	use ark::test_util::VTXO_VECTORS;

	use super::verify_vtxo;

	#[test]
	fn verify_vtxo_checks_signed_path_and_anchor_identity() {
		let vectors = &*VTXO_VECTORS;
		let funding = serialize_hex(&vectors.anchor_tx);
		let vtxo = &vectors.arkoor2_vtxo;
		assert_eq!(verify_vtxo(&vtxo.serialize_hex(), &funding, None).unwrap().id(), vtxo.id());

		let mut invalid = vtxo.clone();
		invalid.invalidate_final_sig();
		let error = verify_vtxo(&invalid.serialize_hex(), &funding, None).unwrap_err();
		assert!(format!("{error:#}").contains("invalid signature"));

		// Keep the funding outputs unchanged but change its transaction ID.
		let mut unrelated = vectors.anchor_tx.clone();
		unrelated.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
		let error = verify_vtxo(&vtxo.serialize_hex(), &serialize_hex(&unrelated), None).unwrap_err();
		assert!(error.to_string().contains("transaction ID does not match"));
	}

	#[test]
	fn verify_vtxo_binds_the_signed_recipient_and_server() {
		let vectors = &*VTXO_VECTORS;
		let funding = serialize_hex(&vectors.anchor_tx);
		let vtxo = &vectors.arkoor2_vtxo;
		let address = ark::Address::new(true, vtxo.server_pubkey(), vtxo.policy().clone(), vec![]);
		assert!(verify_vtxo(&vtxo.serialize_hex(), &funding, Some(&address.to_string())).is_ok());

		let wrong_payee = ark::Address::new(true, vtxo.server_pubkey(), vectors.board_vtxo.policy().clone(), vec![]);
		assert!(verify_vtxo(&vtxo.serialize_hex(), &funding, Some(&wrong_payee.to_string())).is_err());
		let wrong_server = ark::Address::new(true, vectors.arkoor3_user_key.public_key(), vtxo.policy().clone(), vec![]);
		assert!(verify_vtxo(&vtxo.serialize_hex(), &funding, Some(&wrong_server.to_string())).is_err());
	}
}
