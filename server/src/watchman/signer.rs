
use bitcoin::hashes::sha256;
use bitcoin::secp256k1::schnorr;
use bitcoin::secp256k1::Keypair;
use bitcoin::taproot::ControlBlock;
use bitcoin::TapSighash;
use bitcoin::Witness;
use tracing::error;

use ark::vtxo::{HashDelaySignClause, HashDelaySignClause_v0, ServerVtxoPolicy, Vtxo};
use ark::vtxo::policy::clause::{TapScriptClause, VtxoClause, HashSignClause, HashSignClause_v0};
use ark::vtxo::policy::signing::VtxoSigner;
use ark::VtxoId;
use ark::lightning::PaymentHash;
use bitcoin_ext::KeypairExt;

use crate::database::{Db, Tx};
use crate::database::expiry_lightning::LightningSourceState;
use crate::database::htlc_vtxo::{self, HtlcDirection, HtlcResolution};
use crate::secret::Secret;
use crate::SECP;

/// Signer for server-side VTXO spending (claims).
///
/// Signs VTXOs where the server pubkey is authorized to spend.
/// This includes all [ServerVtxoPolicy](ark::vtxo::policy::ServerVtxoPolicy) variants:
/// - **Checkpoint**: Server can sweep after expiry height via `TimelockSign`
/// - **Expiry**: Server can sweep after expiry height via `TimelockSign`
/// - **ServerHtlcSend**: Server reveals preimage via `HashDelaySign`
/// - **ServerHtlcRecv**: Server claims after HTLC expiry via `DelayedTimelockSign`
///
/// For HashDelaySign clauses, preimages are retrieved from the database.
pub struct WatchmanSigner {
	server_keypair: Secret<Keypair>,
	ephemeral_master_key: Secret<Keypair>,
	db: Db,
}

impl WatchmanSigner {
	pub fn new(
		server_keypair: Secret<Keypair>,
		ephemeral_master_key: Secret<Keypair>,
		db: Db,
	) -> Self {
		Self { server_keypair, ephemeral_master_key, db }
	}

	/// Use the allocation transaction for first payout signing. This avoids a
	/// second pool connection while its allocation and chain-view locks are held.
	pub(super) fn with_tx<'a, 't>(&'a self, tx: &'a Tx<'t>) -> SigningContext<'a, 't> {
		SigningContext { signer: self, transaction: Some(tx) }
	}
}

pub(super) struct SigningContext<'a, 't> {
	signer: &'a WatchmanSigner,
	transaction: Option<&'a Tx<'t>>,
}

impl SigningContext<'_, '_> {
	async fn read<R, F>(&self, f: F) -> anyhow::Result<R>
	where
		F: AsyncFnOnce(&Tx<'_>) -> anyhow::Result<R>,
	{
		match self.transaction {
			Some(tx) => f(tx).await,
			None => self.signer.db.read(f).await,
		}
	}

	/// Get the preimage for a hash-locked clause from the database.
	///
	/// The same secret can exist in `htlc_settlement` (a settled LN payment)
	/// and `round_participation` (an hArk unlock preimage) at once through
	/// legitimate flows. Resolve strictly by clause domain: HashSign (hArk
	/// forfeit) uses the participation preimage, HashDelaySign (LN HTLC) the
	/// settlement preimage. Refusing on ambiguity let users plant that state
	/// and reclaim forfeits or refund paid HTLCs on-chain while we stood by.
	async fn get_preimage(&self, clause: &VtxoClause) -> Option<Secret<[u8; 32]>> {
		match clause {
			VtxoClause::HashSign(HashSignClause { hash, ..})
				| VtxoClause::HashSign_v0(HashSignClause_v0 { hash, ..}) =>
			{
				let hark = self.read(async |t| {
					if t.query_opt("
						SELECT participation_id FROM expiry_cancelled_participation
						WHERE unlock_hash=$1
					", &[&hash.to_string()]).await?.is_some() {
						return Ok(None);
					}
					Ok(t.get_round_participation_by_unlock_hash(*hash).await?
						.filter(|part| part.forfeited_at.is_some()))
				}).await.ok()??;
				self.log_if_duplicate_hash(*hash, true).await;
				Some(hark.unlock_preimage)
			},
			VtxoClause::HashDelaySign(HashDelaySignClause { hash, .. })
				| VtxoClause::HashDelaySign_v0(HashDelaySignClause_v0 { hash, .. }) =>
			{
				let settlement = self.read(async |t|
					t.get_htlc_settlement_by_payment_hash((*hash).into()).await
				).await.ok()??;
				self.log_if_duplicate_hash(*hash, false).await;
				Some(Secret::new(settlement.into()))
			},
			_ => None,
		}
	}

	/// Log when a hash exists in both secret domains. The state is reachable
	/// through legitimate flows and never blocks signing; the log exists for
	/// visibility only.
	async fn log_if_duplicate_hash(&self, hash: sha256::Hash, in_hark: bool) {
		let duplicate = if in_hark {
			self.read(async |t| t.get_htlc_settlement_by_payment_hash(hash.into()).await).await
				.ok().flatten().is_some()
		} else {
			self.read(async |t| t.get_round_participation_by_unlock_hash(hash).await).await
				.ok().flatten().is_some()
		};
		if duplicate {
			slog!(DuplicateSecretHash, hash);
		}
	}

	async fn source_was_discharged(&self, id: VtxoId, hash: PaymentHash) -> bool {
		self.read(async |t| {
			let Some(htlc) = htlc_vtxo::get_htlc_vtxo(t, id).await? else {
				return Ok(false);
			};
			if htlc.direction() != HtlcDirection::Incoming
				|| htlc.htlc.payment_hash != hash
				|| htlc.htlc.offchain_resolution == Some(HtlcResolution::Revoked)
				|| htlc.htlc.chain_resolution.is_some()
			{
				return Ok(false);
			}
			Ok(t.lightning_source_state(id, hash).await? == LightningSourceState::Discharged)
		}).await.unwrap_or(false)
	}
}

#[async_trait::async_trait]
impl VtxoSigner<ServerVtxoPolicy> for SigningContext<'_, '_> {
	async fn can_sign<G: Sync + Send>(
		&self, clause: &VtxoClause, vtxo: &Vtxo<G, ServerVtxoPolicy>,
	) -> bool {
		// first check for the key to sign
		let have_key = if clause.pubkey() == self.signer.server_keypair.leak_ref().public_key() {
			true
		} else {
			self.read(async |t| t.fetch_ephemeral_tweak(clause.pubkey()).await).await
				.ok().flatten().is_some()
		};
		if !have_key {
			return false;
		}

		// then check for other witness items
		match clause {
			VtxoClause::HashDelaySign(c) => self.source_was_discharged(vtxo.id(), c.hash.into()).await
				&& self.get_preimage(clause).await.is_some(),
			VtxoClause::HashDelaySign_v0(c) => self.source_was_discharged(vtxo.id(), c.hash.into()).await
				&& self.get_preimage(clause).await.is_some(),
			VtxoClause::HashSign(_) => self.get_preimage(clause).await.is_some(),
			VtxoClause::HashSign_v0(_) => self.get_preimage(clause).await.is_some(),
			// others are simple signatures
			VtxoClause::DelayedSign(_) => true,
			VtxoClause::DelayedTimelockSign(_) => true,
			VtxoClause::TimelockSign(_) => true,
		}
	}

	async fn sign_keyspend<G: Sync + Send>(
		&self,
		vtxo: &Vtxo<G, ServerVtxoPolicy>,
		sighash: TapSighash,
	) -> Option<schnorr::Signature> {
		let taproot = vtxo.output_taproot();
		let server_pubkey = self.signer.server_keypair.leak_ref().public_key();
		if taproot.internal_key() != server_pubkey.x_only_public_key().0 {
			error!("Watchman asked to sign VTXO {} which has internal key {} but our key is {}",
				vtxo.id(), taproot.internal_key(), server_pubkey,
			);
			return None;
		}
		let key = self.signer.server_keypair.leak_ref().for_keyspend(&*SECP, taproot.merkle_root());
		Some(ark::SECP.sign_schnorr(&sighash.into(), &key))
	}

	async fn witness(
		&self,
		clause: &VtxoClause,
		control_block: &ControlBlock,
		sighash: TapSighash,
	) -> Option<Witness> {
		// Only sign if the clause pubkey matches our server pubkey
		let key = if clause.pubkey() == self.signer.server_keypair.leak_ref().public_key() {
			self.signer.server_keypair.leak_owned()
		} else {
			let tweak = self.read(async |t| t.fetch_ephemeral_tweak(clause.pubkey()).await).await.ok()??;
			let seckey = self.signer.ephemeral_master_key.leak_ref().secret_key()
				.add_tweak(&tweak).expect("tweak error");
			Keypair::from_secret_key(&*SECP, &seckey)
		};

		// Sign the sighash
		let signature = ark::SECP.sign_schnorr(&sighash.into(), &key);

		// Construct witness based on clause type
		match clause {
			VtxoClause::TimelockSign(c) => Some(c.witness(&signature, control_block)),
			VtxoClause::DelayedSign(c) => Some(c.witness(&signature, control_block)),
			VtxoClause::DelayedTimelockSign(c) => Some(c.witness(&signature, control_block)),
			VtxoClause::HashDelaySign(c) => {
				let preimage = self.get_preimage(clause).await?;
				Some(c.witness(&(signature, *preimage.leak_ref()), control_block))
			},
			VtxoClause::HashDelaySign_v0(c) => {
				let preimage = self.get_preimage(clause).await?;
				Some(c.witness(&(signature, *preimage.leak_ref()), control_block))
			},
			VtxoClause::HashSign(c) => {
				let preimage = self.get_preimage(clause).await?;
				Some(c.witness(&(signature, *preimage.leak_ref()), control_block))
			},
			VtxoClause::HashSign_v0(c) => {
				let preimage = self.get_preimage(clause).await?;
				Some(c.witness(&(signature, *preimage.leak_ref()), control_block))
			},
		}
	}
}

#[async_trait::async_trait]
impl VtxoSigner<ServerVtxoPolicy> for WatchmanSigner {
	async fn can_sign<G: Sync + Send>(
		&self, clause: &VtxoClause, vtxo: &Vtxo<G, ServerVtxoPolicy>,
	) -> bool {
		SigningContext { signer: self, transaction: None }.can_sign(clause, vtxo).await
	}

	async fn sign_keyspend<G: Sync + Send>(
		&self, vtxo: &Vtxo<G, ServerVtxoPolicy>, sighash: TapSighash,
	) -> Option<schnorr::Signature> {
		SigningContext { signer: self, transaction: None }.sign_keyspend(vtxo, sighash).await
	}

	async fn witness(
		&self, clause: &VtxoClause, control_block: &ControlBlock, sighash: TapSighash,
	) -> Option<Witness> {
		SigningContext { signer: self, transaction: None }.witness(clause, control_block, sighash).await
	}
}
