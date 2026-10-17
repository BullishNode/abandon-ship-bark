//! One authenticated, immutable expiry destination and public coin account.

pub(crate) mod payouts;
mod refusal;
mod sync;
mod reconcile;
mod protection;
mod act_by;
mod adoption;
mod spend_proof;
pub use act_by::{ExitProtectionOutcome, ExitProtectionSchedule, ExitProtectionReport,
	ExitProtectionIssue, ExitProtectionIncomplete};
pub use refusal::FallbackRefusal;

use std::time::Duration;

use anyhow::Context;
use bitcoin::{Network, Script, ScriptBuf};

use ark::ProtocolEncoding;
use server_rpc::{ServerConnection, protos};
use server_rpc::client::ConnectError;
#[cfg(feature = "native")]
use server_rpc::client::CreateEndpointError;

use crate::{Wallet, WalletSeed};
use crate::persist::BarkPersister;
use crate::persist::models::FallbackRecord;

pub(crate) fn next_key_index(last: Option<u32>) -> anyhow::Result<u32> {
	let next = last.map(|i| i.checked_add(1).context("vtxo key index exhausted"))
		.transpose()?.unwrap_or(0);
	ensure!(next < (1 << 31), "vtxo key index exhausted");
	Ok(next)
}

fn authenticate_binding(record: &FallbackRecord, seed: &WalletSeed, network: Network, expected: &Script) -> anyhow::Result<()> {
	record.verify(network, seed.to_mailbox_keypair().public_key())?;
	ensure!(record.spk.as_script() == expected, "fallback record differs from the exact expected destination");
	ensure!(record.account_xpub == bitcoin::bip32::Xpub::from_priv(&crate::SECP, &seed.vtxo),
		"fallback record is not this seed's coin account");
	Ok(())
}

fn validate_destination(network: Network, script: &Script) -> anyhow::Result<()> {
	bitcoin::Address::from_script(script, network).context("unsupported fallback address script")?;
	Ok(())
}

/// Only a failed connection/temporary handshake can defer registration.
/// Configuration, protocol, server identity and local storage failures must
/// still stop open; a pending local signature cannot override those checks.
fn registration_server_unavailable(error: &anyhow::Error) -> bool {
	match error.downcast_ref::<ConnectError>() {
		Some(ConnectError::Handshake(status) | ConnectError::GetArkInfo(status)
			| ConnectError::GetOffboardFeeRate(status)) => matches!(status.code(),
				tonic::Code::Unavailable | tonic::Code::DeadlineExceeded),
		#[cfg(feature = "native")]
		Some(ConnectError::CreateEndpoint(CreateEndpointError::Transport(_))) => {
			// A transport variant can also wrap invalid TLS configuration.
			// Require a typed network I/O cause, not arbitrary error text.
			error.chain().filter_map(|cause| cause.downcast_ref::<std::io::Error>())
				.any(|io| matches!(io.kind(), std::io::ErrorKind::ConnectionRefused
					| std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
					| std::io::ErrorKind::NotConnected | std::io::ErrorKind::TimedOut
					| std::io::ErrorKind::NetworkDown | std::io::ErrorKind::NetworkUnreachable
					| std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::BrokenPipe))
		},
		_ => false,
	}
}

async fn persist_native_identity(db: &dyn BarkPersister, seed: &WalletSeed, record: &FallbackRecord) -> anyhow::Result<()> {
	// Child zero is the existing stable wallet identity and the account
	// possession signer. Reserve it once, not as a replenishable signing pool.
	db.store_vtxo_key(0, seed.derive_vtxo_keypair(0).public_key()).await?;
	db.store_fallback_record(record).await
}

pub(crate) async fn register(
	network: Network, seed: &WalletSeed, db: &dyn BarkPersister,
	connection: &mut ServerConnection,
	expected: &Script,
) -> anyhow::Result<()> {
	let spk = expected.to_owned();
	validate_destination(network, &spk)?;
	if let Some(stored) = db.get_fallback_record().await? { authenticate_binding(&stored, seed, network, expected)?; }
	if let Some(pending) = db.get_pending_fallback_record().await? { authenticate_binding(&pending, seed, network, expected)?; }
	let key = seed.to_mailbox_keypair();
	let proposed = FallbackRecord::new(network, spk, &seed.vtxo, &key)?;
	// Deterministic binding makes a lost reply safe to retry, including from a
	// different device. Adopt its canonical signatures only after authenticating
	// the exact seed-derived fields; a mismatch never overwrites local state.
	let reply = connection.client.set_fallback(protos::SetFallbackRequest {
		mailbox_pk: key.public_key().serialize().to_vec(), record: proposed.serialize(),
	}).await.context("failed to register immutable fallback account")?.into_inner();
	let current = FallbackRecord::deserialize(&reply.record)?;
	authenticate_binding(&current, seed, network, expected)?;
	persist_native_identity(db, seed, &current).await
}

impl Wallet {
	/// Return the authenticated registration, if this wallet has one.
	pub async fn fallback_registration(&self) -> anyhow::Result<Option<FallbackRecord>> {
		let Some(record) = self.inner.db.get_fallback_record().await? else { return Ok(None); };
		record.verify(self.network().await?, self.inner.seed.to_mailbox_keypair().public_key())?;
		ensure!(record.account_xpub == self.vtxo_xpub(), "fallback record is not this seed's coin account");
		Ok(Some(record))
	}

	/// The authenticated immutable destination, suitable for recovery evidence.
	pub async fn fallback_destination(&self) -> anyhow::Result<FallbackRecord> {
		self.fallback_registration().await?.context("no fallback account registered")
	}

	pub(crate) async fn retry_pending_fallback_registration(&self) -> anyhow::Result<()> {
		if self.inner.db.get_pending_fallback_record().await?.is_some() {
			self.open_fallback(None, true).await?;
		}
		Ok(())
	}

	pub(crate) async fn open_fallback(&self, requested: Option<ScriptBuf>, allow_offline: bool) -> anyhow::Result<()> {
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback", self.fingerprint()), Duration::from_secs(30)).await?;
		let stored = self.inner.db.get_fallback_record().await?;
		let network = self.network().await?;
		if let Some(record) = stored {
			let expected = requested.as_deref().unwrap_or(record.spk.as_script());
			validate_destination(network, expected)?;
			authenticate_binding(&record, &self.inner.seed, network, expected)?;
			persist_native_identity(&*self.inner.db, &self.inner.seed, &record).await?;
			// A cached authenticated binding permits offline/seed-only status
			// opens. An app that supplies its destination retries registration
			// when connected, including after a server restore.
			if requested.is_some() {
				match self.require_server().await {
					Ok((mut connection, _)) => register(network, &self.inner.seed, &*self.inner.db, &mut connection, expected).await?,
					Err(error) if registration_server_unavailable(&error) => {},
					Err(error) => return Err(error),
				}
			}
			return Ok(());
		}
		// Keep the app's exact choice locally even when --force creates the
		// wallet offline. A local signature is not a server acknowledgement:
		// registration/destination getters and coin issuance remain fenced.
		let pending = self.inner.db.get_pending_fallback_record().await?;
		let reopening_pending = pending.is_some();
		let expected = requested.or_else(|| pending.as_ref().map(|record| record.spk.clone()));
		let Some(expected) = expected else { return Ok(()); };
		validate_destination(network, &expected)?;
		let proposed = if let Some(record) = pending {
			authenticate_binding(&record, &self.inner.seed, network, &expected)?;
			record
		} else {
			FallbackRecord::new(network, expected.clone(), &self.inner.seed.vtxo,
				&self.inner.seed.to_mailbox_keypair())?
		};
		if allow_offline || reopening_pending {
			self.inner.db.store_vtxo_key(0, self.inner.seed.derive_vtxo_keypair(0).public_key()).await?;
			self.inner.db.store_pending_fallback_record(&proposed).await?;
		}
		let (mut connection, _) = match self.require_server().await {
			Ok(server) => server,
			Err(error) if (allow_offline || reopening_pending) && registration_server_unavailable(&error) => return Ok(()),
			Err(error) => return Err(error),
		};
		register(network, &self.inner.seed, &*self.inner.db, &mut connection, &expected).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn offline_registration_defers_only_typed_temporary_connection_errors() {
		for status in [tonic::Status::unavailable("temporary"), tonic::Status::deadline_exceeded("temporary")] {
			let error = anyhow::Error::from(ConnectError::Handshake(status)).context("wallet connection failed");
			assert!(registration_server_unavailable(&error));
		}
		for error in [
			anyhow::Error::from(ConnectError::Handshake(tonic::Status::invalid_argument("invalid client"))),
			anyhow::Error::from(ConnectError::ProtocolVersionMismatchClientTooOld { client_max: 1, server_min: 2 }),
			anyhow::Error::from(ConnectError::NetworkMismatch { expected: Network::Regtest, got: Network::Bitcoin }),
			anyhow::Error::from(server_rpc::client::CreateEndpointError::NoTransportBackend),
			anyhow::Error::from(ConnectError::CreateEndpoint(server_rpc::client::CreateEndpointError::InvalidScheme("file".into()))),
			anyhow::anyhow!("server public key changed"),
			anyhow::anyhow!("unsafe server exit margin"),
			anyhow::anyhow!("wallet properties read failed"),
			anyhow::Error::from(tonic::Status::unavailable("not a connection error")).context("local persister failed"),
		] {
			assert!(!registration_server_unavailable(&error), "must preserve {error:#}");
		}
	}

	#[cfg(feature = "native")]
	#[test]
	fn offline_registration_preserves_endpoint_configuration_errors() {
		let transport = tonic::transport::Endpoint::from_shared("http://[".to_owned()).err().unwrap();
		let error = anyhow::Error::from(ConnectError::CreateEndpoint(CreateEndpointError::Transport(transport)));
		assert!(!registration_server_unavailable(&error));
	}

	#[cfg(feature = "native")]
	#[tokio::test]
	async fn offline_registration_recognizes_an_actual_refused_loopback_connection() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let address = format!("http://{}", listener.local_addr().unwrap());
		drop(listener);
		let error = ServerConnection::builder().address(&address).network(Network::Regtest)
			.connect().await.err().expect("closed loopback endpoint must refuse the connection");
		assert!(registration_server_unavailable(&anyhow::Error::from(error).context("wallet connection failed")));
	}

	#[test]
	fn fallback_accepts_application_standard_scripts_without_derivation_rules() {
		let seed = WalletSeed::new_from_seed(Network::Regtest, &[17; 64]);
		for hex in [
			format!("76a914{}88ac", "11".repeat(20)),
			format!("a914{}87", "22".repeat(20)),
			format!("0014{}", "33".repeat(20)),
			format!("0020{}", "44".repeat(32)),
			format!("5120{}", "55".repeat(32)),
		] {
			let script = ScriptBuf::from_hex(&hex).unwrap();
			validate_destination(Network::Regtest, &script).unwrap();
			let record = FallbackRecord::new(Network::Regtest, script.clone(),
				&seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
			authenticate_binding(&record, &seed, Network::Regtest, &script).unwrap();
		}
		assert!(validate_destination(Network::Regtest, &ScriptBuf::from_bytes(vec![0x6a])).is_err());
	}

	#[test]
	fn restore_requires_exact_seed_binding_and_network() {
		let seed = WalletSeed::new_from_seed(Network::Regtest, &[17; 64]);
		let expected = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
		let record = FallbackRecord::new(Network::Regtest, expected.clone(),
			&seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&record, &seed, Network::Regtest, &expected).is_ok());
		assert!(authenticate_binding(&record, &seed, Network::Signet, &expected).is_err());
		let other_script = ScriptBuf::from_hex("00142222222222222222222222222222222222222222").unwrap();
		let wrong = FallbackRecord::new(Network::Regtest, other_script, &seed.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &expected).is_err());
		let other = WalletSeed::new_from_seed(Network::Regtest, &[18; 64]);
		let wrong = FallbackRecord::new(Network::Regtest, record.spk.clone(), &other.vtxo, &seed.to_mailbox_keypair()).unwrap();
		assert!(authenticate_binding(&wrong, &seed, Network::Regtest, &expected).is_err());
		assert_eq!(next_key_index(None).unwrap(), 0);
		assert_eq!(next_key_index(Some((1 << 31) - 2)).unwrap(), (1 << 31) - 1);
		assert!(next_key_index(Some((1 << 31) - 1)).is_err());
	}
}
