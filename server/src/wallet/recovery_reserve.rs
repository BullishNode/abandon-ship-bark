//! Admission quarantine for the shared rounds wallet's recovery cash.
//!
//! Unknown gross always fences financial work. Known physical bounds protect
//! confirmed liquidity; only a selected native settlement carries release credit.
//! Complete economic classification and operator residual closure are separate.

use std::time::Duration;

use anyhow::Context;
use bdk_wallet::Wallet;
use bitcoin::{Amount, OutPoint, ScriptBuf, Transaction};
use bitcoind_async_client::Client;
use tokio::time::Instant;

use crate::database::{Db, recovery_reserve::RecoveryReceipt};
use crate::sync::BlockData;
use crate::wallet::{BdkWalletExt, PersistedWallet};

const MAX_PROVEN_RECEIPTS_PER_BLOCK: usize = 64;
const MAX_INPUTS_PER_BLOCK: usize = 256;
const PROOF_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) struct RecoveryReserve {
	pub script: ScriptBuf,
	pub unknown_gross: bool,
	pub held: Amount,
	pub fee_headroom: Amount,
	/// Retained after a failed or ambiguous COMMIT, together with BDK's changeset.
	pub staged: Vec<RecoveryReceipt>,
}

impl RecoveryReserve {
	pub(super) async fn load(db: &Db, wallet: &Wallet) -> anyhow::Result<Option<Self>> {
		let state = db.read(async |t| t.recovery_reserve_state().await).await?;
		let Some(state) = state else { return Ok(None); };
		let script = state.script;
		let tip = wallet.latest_checkpoint().block_id();
		ensure!(state.height == tip.height && state.hash == tip.hash.to_string(),
			"recovery reserve and owning wallet checkpoints differ; reconciliation required");
		let transactions = wallet.tx_graph().full_txs().filter(|node| {
			node.tx.output.iter().any(|output| output.script_pubkey == script)
		}).map(|node| node.tx).collect::<Vec<_>>();
		ensure!(transactions.len() <= 10_000, "recovery startup receipt budget exceeded; reconciliation required");
		db.read(async |t| {
			for transaction in &transactions { t.check_recovery_receipt(transaction).await?; }
			Ok(())
		}).await?;
		Ok(Some(Self { script, held: state.held, fee_headroom: state.fee_headroom, unknown_gross: state.unknown_gross, staged: Vec::new() }))
	}

	pub(super) fn ensure_spending_allowed(&self) -> anyhow::Result<()> {
		ensure!(!self.unknown_gross && self.staged.is_empty(),
			"recovery capital quarantined: gross proof or native disposition/release is unresolved");
		Ok(())
	}
}

impl PersistedWallet {
	pub(crate) async fn enable_recovery_reserve(&mut self, script: ScriptBuf, fee_headroom: Amount) -> anyhow::Result<()> {
		if let Some(reserve) = &self.recovery_reserve {
			ensure!(reserve.script == script && reserve.fee_headroom == fee_headroom,
				"recovery reserve script or fee policy changed; declared maintenance transition required");
			return Ok(());
		}
		// Never initialize an old receipt history as an empty reserve. Backfill is
		// an explicit paused operation; absence of current payable coins proves nothing.
		ensure!(!self.wallet.tx_graph().full_txs().any(|node| {
			node.tx.output.iter().any(|output| output.script_pubkey == script)
		}), "existing sweep receipts require explicit recovery reserve backfill");
		self.recovery_reserve = Some(RecoveryReserve { script, unknown_gross: true, held: Amount::ZERO, fee_headroom, staged: Vec::new() });
		self.persist().await.context("initializing recovery reserve")
	}

	pub(super) fn ensure_recovery_spending_allowed(&self) -> anyhow::Result<()> {
		if let Some(reserve) = &self.recovery_reserve { reserve.ensure_spending_allowed()?; }
		Ok(())
	}

	/// Gather immutable input proofs under the existing owning guard, before
	/// entering the short wallet/hold SQL transaction. No chain or LN RPC in SQL.
	/// The caller holds the guard throughout, so no revision can change meanwhile.
	pub(super) async fn stage_recovery_receipts(&mut self, block: &BlockData) -> anyhow::Result<()> {
		let Some(reserve) = &mut self.recovery_reserve else { return Ok(()); };
		let receipts = block.block.txdata.iter().filter(|tx| {
			tx.output.iter().any(|output| output.script_pubkey == reserve.script)
		}).cloned().collect::<Vec<_>>();
		if receipts.is_empty() { return Ok(()); }
		// This state survives errors after BDK mutation or an ambiguous COMMIT.
		// It fences OLD cash too, not only the newly received output.
		reserve.unknown_gross = true;
		let deadline = Instant::now() + PROOF_TIMEOUT;
		let mut proven_receipts = 0;
		let mut input_budget = MAX_INPUTS_PER_BLOCK;
		for transaction in receipts {
			let txid = transaction.compute_txid();
			if reserve.staged.iter().any(|r| r.transaction.compute_txid() == txid) { continue; }
			let result = if proven_receipts < MAX_PROVEN_RECEIPTS_PER_BLOCK
				&& transaction.input.len() <= input_budget && !transaction.is_coinbase() {
				proven_receipts += 1;
				input_budget -= transaction.input.len();
				tokio::time::timeout_at(deadline, input_proof(&self.bitcoind, &transaction)).await
					.context("recovery input proof deadline exceeded").and_then(|result| result)
			} else {
				Err(anyhow!("recovery input proof budget exceeded or coinbase receipt"))
			};
			let (sources, gap) = match result {
				Ok(proof) => (Some(proof), None),
				Err(_) => (None, Some("gross input proof unavailable; all rounds-wallet spending fenced".into())),
			};
			reserve.staged.push(RecoveryReceipt { transaction, block: block.block_ref, sources, gap });
		}
		Ok(())
	}
}

/// All values come from exact txid-validated prevouts, never the net drain output.
async fn input_proof(client: &Client, receipt: &Transaction) -> anyhow::Result<Vec<(OutPoint, Amount)>> {
	let mut sources = Vec::with_capacity(receipt.input.len());
	for input in &receipt.input {
		let point = input.previous_output;
		let previous = crate::bitcoind::custom_get_raw_transaction_info(client, point.txid, None).await?
			.context("recovery source transaction unavailable")?;
		let transaction: Transaction = bitcoin::consensus::deserialize(&previous.hex)?;
		ensure!(transaction.compute_txid() == point.txid, "recovery source txid mismatch");
		let output = transaction.output.get(point.vout as usize).context("recovery source output missing")?;
		sources.push((point, output.value));
	}
	let gross = sources.iter().try_fold(Amount::ZERO, |total, (_, value)| total.checked_add(*value))
		.context("recovery gross input amount overflow")?;
	let output = receipt.output.iter().try_fold(Amount::ZERO, |total, output| total.checked_add(output.value))
		.context("recovery output amount overflow")?;
	ensure!(gross >= output, "recovery receipt spends more than its proved inputs");
	Ok(sources)
}

#[derive(Debug)]
pub(super) struct RecoveryLiquidityShortfall {
	available: Amount,
	selected: Amount,
	remaining: Amount,
	held: Amount,
	credit: Amount,
	fee_headroom: Amount,
}

impl std::fmt::Display for RecoveryLiquidityShortfall {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "recovery capital quarantined: protected confirmed liquidity shortfall; available={}, selected_inputs={}, remaining={}, held={}, selected_release={}, fee_headroom={}",
			self.available, self.selected, self.remaining, self.held, self.credit, self.fee_headroom)
	}
}
impl std::error::Error for RecoveryLiquidityShortfall {}

impl PersistedWallet {
	/// Preserve immediately spendable confirmed liquidity, rather than crediting
	/// hypothetical unconfirmed change or double-excluding this transaction's locks.
	pub(super) fn check_recovery_spend(
		&self,
		psbt: &bitcoin::Psbt,
		credit: Option<&crate::database::recovery_reserve::PayoutCapitalCredit>,
	) -> anyhow::Result<()> {
		self.ensure_recovery_spending_allowed()?;
		let Some(reserve) = &self.recovery_reserve else {
			ensure!(credit.is_none(), "payout capital credit requires an active native reserve");
			return Ok(());
		};
		let credit = credit.map(|credit| credit.total()).unwrap_or(Amount::ZERO);
		let protected = reserve.held.checked_sub(credit).context("payout credit exceeds held capital")?
			.checked_add(reserve.fee_headroom).context("protected capital overflow")?;
		let canon = bitcoin_ext::bdk::TrustedCanonicalization::from_wallet(&self.wallet, self.min_trusted_confs);
		let selected = psbt.unsigned_tx.input.iter().map(|input| input.previous_output).collect::<std::collections::HashSet<_>>();
		ensure!(selected.len() == psbt.unsigned_tx.input.len(), "duplicate wallet transaction input");
		let locked = self.locked_outputs.utxos();
		let mut seen = std::collections::HashSet::new();
		let mut available = Amount::ZERO;
		let mut spent_confirmed = Amount::ZERO;
		for output in canon.list_unspent() {
			let is_selected = selected.contains(&output.outpoint);
			if is_selected { seen.insert(output.outpoint); }
			if !output.is_trusted || !output.chain_position.is_confirmed() { continue; }
			let transaction = canon.get(output.outpoint.txid).context("canonical input transaction missing")?;
			let height = output.chain_position.confirmation_height_upper_bound().context("confirmed input has no height")?;
			let confirmations = self.wallet.latest_checkpoint().height().saturating_sub(height) + 1;
			if transaction.tx.is_coinbase() && confirmations < bitcoin::constants::COINBASE_MATURITY { continue; }
			if locked.contains(&output.outpoint) && !is_selected { continue; }
			available = available.checked_add(output.txout.value).context("confirmed availability overflow")?;
			if is_selected {
				spent_confirmed = spent_confirmed.checked_add(output.txout.value).context("selected confirmed input overflow")?;
			}
		}
		ensure!(seen == selected, "selected wallet input is not currently canonical and unspent");
		let remaining = available.checked_sub(spent_confirmed).context("selected inputs exceed confirmed availability")?;
		if remaining < protected {
			return Err(RecoveryLiquidityShortfall {
				available, selected: spent_confirmed, remaining, held: reserve.held, credit,
				fee_headroom: reserve.fee_headroom,
			}.into());
		}
		Ok(())
	}
}

impl PersistedWallet {
	/// Before workers, restore exact already-signed owning-wallet input spends.
	/// Foreign watchman publications must not become rounds-wallet debits. The
	/// normal sync still establishes confirmations; replay does not invent them.
	pub(crate) async fn replay_signed_wallet_publications(&mut self) -> anyhow::Result<()> {
		let height = self.wallet.latest_checkpoint().height();
		let mut cursor = String::new();
		loop {
			let transactions = self.db.read(async |t| t.signed_wallet_publication_page(&cursor, height).await).await?;
			if transactions.is_empty() { break; }
			for transaction in transactions {
				cursor = transaction.compute_txid().to_string();
				let mut owning = false;
				for input in &transaction.input {
					let point = input.previous_output;
					let parent = if let Some(parent) = self.wallet.tx_graph().get_tx(point.txid) {
						parent.as_ref().clone()
					} else if let Some(parent) = self.db.read(async |t| t.signed_wallet_publication(point.txid).await).await? {
						parent
					} else {
						let info = crate::bitcoind::custom_get_raw_transaction_info(&self.bitcoind, point.txid, None).await?
							.context("signed publication input provenance unavailable; startup reconciliation required")?;
						let parent: Transaction = bitcoin::consensus::deserialize(&info.hex)?;
						ensure!(parent.compute_txid() == point.txid, "publication input parent txid mismatch");
						parent
					};
					let output = parent.output.get(point.vout as usize).context("publication parent output missing")?;
					if self.wallet.is_mine(output.script_pubkey.clone()) { owning = true; break; }
				}
				// Every supported native rounds spend selects from the owning BDK
				// wallet. Its imported input/derivation state must already exist.
				// A signed round/pool/offboard missing such provenance cannot be
				// silently treated as a foreign security-wallet transaction.
				let native_funding = self.db.read(async |t| Ok(t.query_one("
					SELECT EXISTS (SELECT 1 FROM round WHERE funding_txid = $1)
						OR EXISTS (SELECT 1 FROM offboards WHERE txid = $1)
						OR EXISTS (SELECT 1 FROM nursery_tx WHERE txid = $1
							AND kind::text IN ('round', 'offboard', 'vtxopool', 'expiry-payout'))
				", &[&cursor]).await?.get::<_, bool>(0))).await?;
				ensure!(owning || !native_funding,
					"signed rounds-wallet publication lacks owning input provenance; reconciliation required");
				if owning { self.commit_tx(&transaction); }
			}
		}
		self.persist().await.context("replaying durable signed owning-wallet publications")
	}
}
