//! Exact-candidate broker in the SAME owning Watchman process. Authentication
//! is followed by native wallet/Core source checks; it is never an R/F token.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

use anyhow::Context;
use bitcoin::{FeeRate, OutPoint, Transaction, TxOut};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoind_async_client::{Client, traits::Reader};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;

use crate::database::{Db, Tx};
use crate::utils::InstrumentedLock;
use crate::wallet::{PersistedWallet, WalletKind};
use super::ordinary_fee::OrdinaryFeeBudget;
use super::ordinary_fee_auth::{FeeRequest, FeeRequestAction};
use super::ordinary_fee_epoch::NativeFeeEpoch;
use super::ordinary_fee_publication::{PublicationPath, PublicationRecord};

pub(crate) struct NativeFeeBroker {
	db: Db,
	core: Client,
	budget: Arc<OrdinaryFeeBudget>,
	wallet: InstrumentedLock<PersistedWallet>,
	epoch: NativeFeeEpoch,
	max_fee_rate: FeeRate,
	// Supplied from the actual existing master seed's public descriptor,
	// never an RPC request, foreign graph, or an opening's self-assertion.
	rounds_descriptor_sha256: sha256::Hash,
	runtime: crate::system::RuntimeManager,
	requests: Arc<tokio::sync::Semaphore>,
	server_key: crate::secret::Secret<bitcoin::secp256k1::Keypair>,
}
impl NativeFeeBroker {
	pub(crate) fn new(db: Db, core: Client, budget: Arc<OrdinaryFeeBudget>,
		wallet: InstrumentedLock<PersistedWallet>, epoch: NativeFeeEpoch,
		max_fee_rate: FeeRate, rounds_descriptor_sha256: sha256::Hash,
		runtime: crate::system::RuntimeManager,
		server_key: bitcoin::secp256k1::Keypair,
	) -> anyhow::Result<Self> {
		ensure!(max_fee_rate > FeeRate::ZERO && rounds_descriptor_sha256 != sha256::Hash::all_zeros(),
			"native fee broker requires actual rate and owning descriptor identities");
		ensure!(epoch.identity().policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&budget.policy())?),
			"native fee owner policy differs from original active epoch");
		ensure!(server_key.public_key() == epoch.identity().server_pubkey, "native fee response key differs from actual epoch");
		Ok(Self { db, core, budget, wallet, epoch, max_fee_rate, rounds_descriptor_sha256,
			runtime, requests: Arc::new(tokio::sync::Semaphore::new(4)), server_key: crate::secret::Secret::new(server_key) })
	}
	pub(crate) fn response_bytes(&self, request: &[u8], record: PublicationRecord) -> anyhow::Result<Vec<u8>> {
		super::ordinary_fee_auth::FeeReply::sign(request, record, self.epoch.identity(), self.server_key.leak_ref())
	}
	fn retained(&self, tx: &Transaction, purpose: PublicationPath) -> anyhow::Result<Option<PublicationRecord>> {
		let records = self.budget.publication_records()?;
		let Some(record) = records.get(&tx.compute_wtxid()) else { return Ok(None); };
		record.matches(tx)?;
		ensure!(record.entry(self.budget.policy())?.purpose() == purpose_name(purpose),
			"retained native candidate has a different publication purpose");
		Ok(Some(record.clone()))
	}
	/// Repeating the SAME original signed candidate is read-only. It never
	/// turns an old source observation into permission for a new candidate.
	pub(crate) async fn verify(&self, bytes: &[u8]) -> anyhow::Result<PublicationRecord> {
		let request = FeeRequest::authenticate(bytes, FeeRequestAction::Verify, self.epoch.identity())?;
		let tx = request.transaction()?;
		let record = self.retained(&tx, request.purpose())?.context("native publication lacks its original reservation")?;
		self.db.write(async |t| {
			t.lock_native_fee_publication().await?;
			t.require_native_fee_intent(&record, self.budget.policy(), self.epoch.identity().opening_sha256, &tx).await?;
			self.budget.require_publication(&record, &tx)
		}).await?;
		Ok(record)
	}
	pub(crate) async fn reserve(self: Arc<Self>, bytes: Vec<u8>) -> anyhow::Result<PublicationRecord> {
		ensure!(!self.runtime.shutdown_requested(), "native fee owner is shutting down");
		ensure!(bytes.len() <= 2 * 1024 * 1024, "native fee request exceeds bound");
		let permit = self.requests.clone().try_acquire_owned().context("native fee owner request bound reached")?;
		let worker = self.runtime.spawn("NativeFeeReservation");
		// Dropping the HTTP/RPC response future does NOT cancel the financial
		// operation or its owning wallet guard during an unknown SQL COMMIT.
		tokio::spawn(async move {
			let _permit = permit; let _worker = worker;
			self.reserve_original(&bytes).await
		}).await.context("native fee owner reservation task interrupted; original history retained")?
	}
	async fn reserve_original(&self, bytes: &[u8]) -> anyhow::Result<PublicationRecord> {
		let request = FeeRequest::authenticate(bytes, FeeRequestAction::Reserve, self.epoch.identity())?;
		let tx = request.transaction()?;
		if let Some(record) = self.retained(&tx, request.purpose())? {
			self.db.write(async |t| {
				t.lock_native_fee_publication().await?;
				t.require_native_fee_intent(&record, self.budget.policy(), self.epoch.identity().opening_sha256, &tx).await?;
				self.budget.require_publication(&record, &tx)
			}).await?;
			return Ok(record);
		}
		// Every native urgent progress builder uses this SAME wallet guard.
		// Hold it across source checks, original witness/fsync and SQL COMMIT.
		// Never call the owner's own inventory RPC while holding this guard.
		let wallet = self.wallet.lock().await;
		ensure!(wallet.kind() == WalletKind::Watchman, "native fee broker does not own Watchman wallet");
		let debit_attempted = AtomicBool::new(false);
		let mut original = None;
		let result = self.db.write(async |t| {
			t.batch_execute("SET LOCAL statement_timeout='8s'; SET LOCAL lock_timeout='5s';").await?;
			t.lock_native_fee_publication().await?;
			let parents = tokio::time::timeout(Duration::from_secs(15), async {
				let inventory = wallet.inventory()?;
				let reserve = checked_urgent_source(&self.core, &inventory, self.epoch.identity().genesis).await?;
				ensure!(reserve.values().try_fold(0u64, |sum, amount| sum.checked_add(*amount).context("urgent source sum overflow"))?
					>= self.budget.urgent_reserve().to_sat(), "ordinary publication would lack confirmed unreserved urgent funds");
				ensure!(tx.input.iter().all(|input| !reserve.contains_key(&input.previous_output)
					&& !inventory.reserved_outpoints.contains(&input.previous_output.to_string())),
					"ordinary candidate selects a protected Watchman input");
				let parents = self.checked_rounds_parents(t, &tx).await?;
				let final_tip = crate::bitcoind::tip(&self.core).await?;
				ensure!(final_tip.height.to_u32() == inventory.tip_height && final_tip.hash.to_string() == inventory.tip_hash,
					"native fee preflight crossed the owning source tip");
				Ok::<_, anyhow::Error>(parents)
			}).await.context("native fee source preflight deadline exceeded before debit")??;
			let record = PublicationRecord::new_precommit(request.purpose(), self.budget.policy(), &tx, &parents, self.max_fee_rate)?;
			ensure!(serde_json::to_vec(&record)?.len() <= 4 * 1024 * 1024 - 1024,
				"native publication leaves no bounded authenticated response space");
			// The durable input intent and exact fee witness are checked together.
			// No timer/caller cancellation is introduced around the financial COMMIT.
			t.stage_native_fee_intent(&record, self.budget.policy(), self.epoch.identity().opening_sha256).await?;
			debit_attempted.store(true, Ordering::SeqCst);
			self.budget.reserve_publication(&record)?;
			original = Some(record);
			Ok(())
		}).await;
		if let Err(error) = result {
			if debit_attempted.load(Ordering::SeqCst) { self.budget.mark_uncertain()?; }
			return Err(error.context("native fee reservation not acknowledged; retain exact original candidate/input intent"));
		}
		drop(wallet);
		original.context("native reservation completed without original witness")
	}
	async fn checked_rounds_parents(&self, t: &Tx<'_>, tx: &Transaction) -> anyhow::Result<Vec<Transaction>> {
		let state = t.read_aggregate_changeset(WalletKind::Rounds).await?.context("native rounds source missing")?;
		ensure!(state.descriptor.as_ref().map(|d| sha256::Hash::hash(d.to_string().as_bytes()))
			== Some(self.rounds_descriptor_sha256), "native rounds descriptor differs from actual owning seed");
		let network = state.network.context("native rounds network missing")?;
		ensure!(bitcoin::constants::genesis_block(network).block_hash() == self.epoch.identity().genesis,
			"native rounds source network differs");
		let view = bdk_wallet::Wallet::load().check_network(network).load_wallet_no_persist(state)?
			.context("native rounds public graph unavailable")?;
		let mut parents = BTreeMap::new(); let mut bytes = 0usize;
		for input in &tx.input {
			if !parents.contains_key(&input.previous_output.txid) {
				let parent = crate::bitcoind::custom_get_raw_transaction_info(&self.core, input.previous_output.txid, None).await?
					.context("native original rounds input parent unavailable")?;
				bytes = bytes.checked_add(parent.hex.len()).context("native input proof overflow")?;
				ensure!(bytes <= 4 * 1024 * 1024, "native input proof exceeds byte bound");
				let full: Transaction = deserialize(&parent.hex)?;
				ensure!(full.compute_txid() == input.previous_output.txid && serialize(&full) == parent.hex,
					"native input parent differs from Core");
				parents.insert(input.previous_output.txid, full);
			}
			let output = parents[&input.previous_output.txid].output.get(input.previous_output.vout as usize)
				.context("native fee input prevout missing")?;
			ensure!(view.is_mine(output.script_pubkey.clone()), "native ordinary publication spends a foreign descriptor input");
			let live = self.core.try_get_tx_out(input.previous_output, true).await?
				.context("native ordinary input already spent or missing")?;
			ensure!(live.value == output.value && live.script_pub_key.script()? == output.script_pubkey,
				"native ordinary live Core input differs");
		}
		let prevouts = tx.input.iter().map(|input| parents[&input.previous_output.txid].output[input.previous_output.vout as usize].clone())
			.collect::<Vec<_>>();
		verify_native_signatures(tx, &prevouts)?;
		Ok(parents.into_values().collect())
	}
}

fn purpose_name(path: PublicationPath) -> &'static str {
	match path { PublicationPath::Round => "round", PublicationPath::Pool => "pool" }
}
async fn checked_urgent_source(core: &Client, wallet: &server_rpc::protos::WalletInventory,
	genesis: bitcoin::BlockHash,
) -> anyhow::Result<BTreeMap<OutPoint, u64>> {
	let tip = crate::bitcoind::tip(core).await?;
	let network: bitcoin::Network = wallet.network.parse()?;
	ensure!(bitcoin::constants::genesis_block(network).block_hash() == genesis, "urgent wallet network differs");
	ensure!(wallet.wallet == "watchman" && wallet.complete && wallet.unavailable_parent_txids.is_empty()
		&& wallet.utxos.len() <= 2000 && wallet.tip_height == tip.height.to_u32()
		&& wallet.tip_hash == tip.hash.to_string() && core.get_block_hash(0).await? == genesis,
		"urgent source is not complete owning Watchman at the actual Core tip");
	let mut result = BTreeMap::new(); let mut seen = BTreeSet::new();
	for output in &wallet.utxos {
		ensure!(seen.insert(output.outpoint.clone()), "urgent wallet source has duplicate outpoint");
		if !output.confirmed_fee_eligible { continue; }
		ensure!(output.trusted && output.mature && !output.reserved
			&& output.confirmations >= wallet.min_trusted_confirmations
			&& !wallet.reserved_outpoints.contains(&output.outpoint), "urgent input is not actually eligible");
		let point: OutPoint = output.outpoint.parse()?;
		let actual = core.try_get_tx_out(point, true).await?.context("urgent owning input spent or missing")?;
		ensure!(actual.value.to_sat() == output.value_sat && actual.script_pub_key.script()?.as_bytes() == output.script_pubkey
			&& actual.bestblock == tip.hash && actual.confirmations > 0
			&& actual.confirmations >= wallet.min_trusted_confirmations
			&& (!actual.coinbase || actual.confirmations >= bitcoin::constants::COINBASE_MATURITY),
			"urgent input differs from independent Core source");
		result.insert(point, output.value_sat);
	}
	ensure!(crate::bitcoind::tip(core).await? == tip, "urgent source proof crossed Core tip");
	Ok(result)
}

// Existing native rounds wallets are P2TR key-spend/default-sighash only.
// Validate each actual signature against the complete independently fetched
// prevout vector before its weight or fee can authorize a publication debit.
fn verify_native_signatures(tx: &Transaction, prevouts: &[TxOut]) -> anyhow::Result<()> {
	use bitcoin::secp256k1::{schnorr::Signature, Message, XOnlyPublicKey};
	use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
	ensure!(!tx.input.is_empty() && tx.input.len() == prevouts.len(), "native signature prevout coverage differs");
	let mut cache = SighashCache::new(tx);
	for (index, (input, output)) in tx.input.iter().zip(prevouts).enumerate() {
		ensure!(input.script_sig.is_empty() && input.witness.len() == 1 && output.script_pubkey.is_p2tr(),
			"native publication is not an owned P2TR key spend");
		let raw = input.witness.iter().next().context("native funding signature absent")?;
		ensure!(raw.len() == 64, "native publication signature or sighash type differs");
		let key = XOnlyPublicKey::from_slice(&output.script_pubkey.as_bytes()[2..34])?;
		let message = cache.taproot_key_spend_signature_hash(index, &Prevouts::All(prevouts), TapSighashType::Default)?;
		crate::SECP.verify_schnorr(&Signature::from_slice(raw)?, &Message::from_digest(message.to_byte_array()), &key)?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::key::TapTweak;
	use bitcoin::{absolute, transaction, Amount, ScriptBuf, Sequence, TxIn, Txid, Witness};
	use bitcoin::secp256k1::{Keypair, Message};
	use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
	fn signed() -> (Transaction, Vec<TxOut>) {
		let key = Keypair::new(&crate::SECP, &mut bitcoin::secp256k1::rand::thread_rng());
		let prevouts = vec![TxOut { value: Amount::from_sat(10_000), script_pubkey: ScriptBuf::new_p2tr(&crate::SECP, key.x_only_public_key().0, None) }];
		let mut tx = Transaction { version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
			input: vec![TxIn { previous_output: OutPoint::new(Txid::all_zeros(), 2), script_sig: ScriptBuf::new(),
				sequence: Sequence::MAX, witness: Witness::new() }],
			output: vec![TxOut { value: Amount::from_sat(9_000), script_pubkey: ScriptBuf::new() }] };
		let sighash = SighashCache::new(&tx).taproot_key_spend_signature_hash(0, &Prevouts::All(&prevouts), TapSighashType::Default).unwrap();
		let tweaked = key.tap_tweak(&crate::SECP, None).to_keypair();
		let signature = crate::SECP.sign_schnorr(&Message::from_digest(sighash.to_byte_array()), &tweaked);
		tx.input[0].witness.push(signature.as_ref());
		(tx, prevouts)
	}
	#[test]
	fn native_publication_requires_exact_signed_body_and_complete_actual_prevouts() {
		let (tx, prevouts) = signed(); verify_native_signatures(&tx, &prevouts).unwrap();
		let mut changed = tx.clone(); changed.output[0].value = Amount::from_sat(8_999);
		assert!(verify_native_signatures(&changed, &prevouts).is_err());
		let mut wrong = prevouts.clone(); wrong[0].value = Amount::from_sat(10_001);
		assert!(verify_native_signatures(&tx, &wrong).is_err());
		assert!(verify_native_signatures(&tx, &[]).is_err());
	}
	#[test]
	fn native_publication_refuses_unsigned_foreign_key_and_non_native_witness() {
		let (tx, prevouts) = signed();
		let mut unsigned = tx.clone(); unsigned.input[0].witness.clear();
		assert!(verify_native_signatures(&unsigned, &prevouts).is_err());
		let (_, foreign) = signed(); assert!(verify_native_signatures(&tx, &foreign).is_err());
		let mut extra = tx; extra.input[0].witness.push([1]);
		assert!(verify_native_signatures(&extra, &prevouts).is_err());
	}
}
