
use std::time::Instant;

use anyhow::Context;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::secp256k1::schnorr;
use bitcoin::secp256k1::Keypair;
use bitcoin::taproot::ControlBlock;
use bitcoin::TapSighash;
use bitcoin::Witness;
use tracing::error;

use ark::vtxo::{HashDelaySignClause, HashDelaySignClause_v0, ServerVtxoPolicy, Vtxo};
use ark::vtxo::policy::clause::{TapScriptClause, VtxoClause, HashSignClause, HashSignClause_v0};
use ark::vtxo::policy::signing::VtxoSigner;
use bitcoin_ext::KeypairExt;

use crate::database::Db;
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

	/// Private supported paid-send claim only. Uses the SAME already-locked SQL
	/// transaction for authoritative preimages, never another pool connection or
	/// a caller-supplied secret. All clauses/preimages are checked before signing.
	pub(super) async fn sign_paid_send_claim_locked(&self, t: &crate::database::Tx<'_>,
		sources: &[ark::ServerVtxo], once: super::urgent_owner::AcknowledgedPaidClaim<'_>,
		deadline: Instant,
	) -> anyhow::Result<bitcoin::Transaction> {
		let unsigned = once.transaction();
		ensure!(!sources.is_empty() && sources.len() == unsigned.input.len()
			&& unsigned.input.iter().all(|input| input.witness.is_empty() && input.script_sig.is_empty()),
			"paid urgent claim input coverage or unsigned shape differs");
		let key = self.server_keypair.leak_ref();
		let mut signing = Vec::with_capacity(sources.len());
		for (source,input) in sources.iter().zip(&unsigned.input) {
			ensure!(source.server_pubkey() == key.public_key() && source.point() == input.previous_output,
				"paid urgent claim input or actual signer differs");
			let (hash,clause) = super::urgent_owner::paid_send_clause(source)?;
			ensure!(clause.pubkey() == key.public_key() && input.sequence == clause.sequence().unwrap_or(bitcoin::Sequence::ZERO),
				"paid urgent claim does not use its original native clause");
			let preimage: [u8;32] = t.get_htlc_settlement_by_payment_hash(hash).await?
				.context("paid urgent claim authoritative settlement preimage disappeared")?.into();
			ensure!(sha256::Hash::hash(&preimage) == hash.to_sha256_hash(), "paid urgent claim settlement preimage hash differs");
			signing.push((clause,Secret::new(preimage)));
		}
		let mut signed = unsigned.clone();
		let prevouts = sources.iter().map(|source| source.txout()).collect::<Vec<_>>();
		let prevouts = Prevouts::All(&prevouts); let mut cache = SighashCache::new(&mut signed);
		for (index,(source,(clause,preimage))) in sources.iter().zip(&signing).enumerate() {
			ensure!(std::time::Instant::now() < deadline, "paid urgent private signing deadline exceeded; preserve original attempt");
			let control = clause.control_block(source);
			let leaf = bitcoin::taproot::TapLeafHash::from_script(&clause.tapscript(),bitcoin::taproot::LeafVersion::TapScript);
			let sighash = cache.taproot_script_spend_signature_hash(index,&prevouts,leaf,TapSighashType::Default)?;
			let signature = crate::SECP.sign_schnorr(&sighash.into(),key);
			crate::SECP.verify_schnorr(&signature,&sighash.into(),&key.x_only_public_key().0)?;
			let witness = match clause {
				VtxoClause::HashDelaySign(c) => c.witness(&(signature,*preimage.leak_ref()),&control),
				VtxoClause::HashDelaySign_v0(c) => c.witness(&(signature,*preimage.leak_ref()),&control),
				_ => bail!("unsupported urgent paid-send clause; no generic signing fallback"),
			};
			ensure!(witness.size() == clause.witness_size(source), "urgent paid-send signed weight differs from original plan");
			*cache.witness_mut(index).context("paid urgent signature input missing")? = witness;
		}
		Ok(signed)
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
				let hark = self.db.read(async |t|
					t.get_round_participation_by_unlock_hash(*hash).await
				).await.ok()??;
				self.log_if_duplicate_hash(*hash, true).await;
				Some(hark.unlock_preimage)
			},
			VtxoClause::HashDelaySign(HashDelaySignClause { hash, .. })
				| VtxoClause::HashDelaySign_v0(HashDelaySignClause_v0 { hash, .. }) =>
			{
				let settlement = self.db.read(async |t|
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
			self.db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash.into()).await).await
				.ok().flatten().is_some()
		} else {
			self.db.read(async |t| t.get_round_participation_by_unlock_hash(hash).await).await
				.ok().flatten().is_some()
		};
		if duplicate {
			slog!(DuplicateSecretHash, hash);
		}
	}
}

#[async_trait::async_trait]
impl VtxoSigner<ServerVtxoPolicy> for WatchmanSigner {
	async fn can_sign<G: Sync + Send>(
		&self, clause: &VtxoClause, _vtxo: &Vtxo<G, ServerVtxoPolicy>,
	) -> bool {
		// first check for the key to sign
		let have_key = if clause.pubkey() == self.server_keypair.leak_ref().public_key() {
			true
		} else {
			self.db.read(async |t| t.fetch_ephemeral_tweak(clause.pubkey()).await).await
				.ok().flatten().is_some()
		};
		if !have_key {
			return false;
		}

		// then check for other witness items
		match clause {
			VtxoClause::HashDelaySign(_) => self.get_preimage(clause).await.is_some(),
			VtxoClause::HashDelaySign_v0(_) => self.get_preimage(clause).await.is_some(),
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
		let server_pubkey = self.server_keypair.leak_ref().public_key();
		if taproot.internal_key() != server_pubkey.x_only_public_key().0 {
			error!("Watchman asked to sign VTXO {} which has internal key {} but our key is {}",
				vtxo.id(), taproot.internal_key(), server_pubkey,
			);
			return None;
		}
		let key = self.server_keypair.leak_ref().for_keyspend(&*SECP, taproot.merkle_root());
		Some(ark::SECP.sign_schnorr(&sighash.into(), &key))
	}

	async fn witness(
		&self,
		clause: &VtxoClause,
		control_block: &ControlBlock,
		sighash: TapSighash,
	) -> Option<Witness> {
		// Only sign if the clause pubkey matches our server pubkey
		let key = if clause.pubkey() == self.server_keypair.leak_ref().public_key() {
			self.server_keypair.leak_owned()
		} else {
			let tweak = self.db.read(async |t| t.fetch_ephemeral_tweak(clause.pubkey()).await).await.ok()??;
			let seckey = self.ephemeral_master_key.leak_ref().secret_key()
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
