//! Read-only client details for stable entitlements and their payout attempts.

use std::collections::BTreeMap;

use anyhow::Context;
use bitcoin::{Amount, Network, ScriptBuf, Transaction, Txid};
use bitcoin::bip32::Xpub;
use bitcoin::secp256k1::PublicKey;
use bitcoin::consensus::deserialize;
use bitcoin::hashes::{sha256, Hash};
use bitcoin_ext::{BlockHeight, BlockRef};
use chrono::{DateTime, Utc};

use ark::VtxoId;

use super::Tx;

// Keep repeated parent transactions below the default RPC decode limit.
const HISTORY_BYTE_BUDGET: i64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackPayoutStage {
	Committed,
	Broadcast,
	Confirmed,
	Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackDisposition {
	CustomerPayout,
	BelowDust,
	Operator,
}

#[derive(Debug, Clone)]
pub struct FallbackChainPayment {
	pub txid: Txid,
	pub confirmation: BlockRef,
}

#[derive(Debug, Clone)]
pub struct FallbackAllocationAttempt {
	/// This identifies one attempt, not a forever-stable user payout output.
	pub allocation_id: i64,
	pub active: bool,
	pub stage: FallbackPayoutStage,
	pub disposition: FallbackDisposition,
	pub output_index: Option<u32>,
	pub script: Option<ScriptBuf>,
	pub gross: Amount,
	/// This coin's share of the fee and grouped payout.
	pub fee: Amount,
	pub payout: Amount,
	pub forfeited: Amount,
	/// The whole output may combine several coins or trees.
	pub output_amount: Option<Amount>,
	pub parent: Option<Transaction>,
	pub confirmation: Option<BlockRef>,
}

#[derive(Debug, Clone)]
pub struct FallbackEntitlementDetails {
	pub entitlement_id: i64,
	pub vtxo_id: VtxoId,
	pub gross: Amount,
	/// Confirmations and terminal claims are withheld during invalidation.
	pub chain_view_valid: bool,
	/// All prior associations are retained; clients project active groups and
	/// update the existing coin debit when an allocation merges or splits.
	pub attempts: Vec<FallbackAllocationAttempt>,
	/// A terminal user claim is an exit, never another fallback payout.
	pub terminal_claim: Option<FallbackChainPayment>,
}

pub struct FallbackEntitlementPage {
	pub entitlements: Vec<FallbackEntitlementDetails>,
	pub view_token: Vec<u8>,
	/// Pagination through stable IDs, never a stream of subsequent changes.
	pub next_after_entitlement_id: Option<i64>,
}

fn amount(row: &tokio_postgres::Row, name: &str) -> anyhow::Result<Amount> {
	Ok(Amount::from_sat(u64::try_from(row.get::<_, i64>(name))?))
}

fn point(height: Option<i64>, hash: Option<&str>) -> anyhow::Result<Option<BlockRef>> {
	match (height, hash) {
		(None, None) => Ok(None),
		(Some(height), Some(hash)) => Ok(Some(BlockRef { height: BlockHeight::try_from(height)?, hash: hash.parse()? })),
		_ => bail!("incomplete fallback confirmation evidence"),
	}
}

impl Tx<'_> {
	/// The public RPC first verifies the existing server-bound xpub recovery
	/// authorization. The record is matched by its actual derivation identity,
	/// so changing an xpub's header cannot discover another account's funds.
	pub async fn fallback_payout_details_by_account(
		&self, account: Xpub, network: Network, after_entitlement_id: i64, limit: u32,
		expected_view_token: Option<&[u8]>,
	) -> anyhow::Result<FallbackEntitlementPage> {
		let view_token = self.expiry_history_view_token().await?;
		if expected_view_token.is_some_and(|expected| expected!=view_token.as_slice()) {
			return Err(crate::error::FallbackHistoryViewChanged.into());
		}
		ensure!(account.network == bitcoin::NetworkKind::from(network), "fallback account network mismatch");
		ensure!(after_entitlement_id >= 0 && limit > 0 && limit <= 1_000, "invalid fallback account page");
		let Some(row) = self.query_opt("SELECT mailbox_pk FROM fallback_record \
			WHERE account_pubkey=$1 AND account_chain_code=$2",
			&[&account.public_key.to_string(),&account.chain_code.to_string()]).await? else {
			self.check_expiry_history_view(&view_token).await?;
			return Ok(FallbackEntitlementPage { entitlements: Vec::new(), view_token, next_after_entitlement_id: None });
		};
		let mailbox: PublicKey = row.get::<_, &str>("mailbox_pk").parse()?;
		let record = self.get_fallback(mailbox).await?.context("registered fallback account disappeared")?;
		record.verify(network,mailbox)?;
		ensure!(record.account_xpub.public_key == account.public_key && record.account_xpub.chain_code == account.chain_code,
			"registered fallback account derivation mismatch");
		// The allocation's authenticated economic owner can differ from the
		// unregistered child's recipient key. This discovers a lost cosign
		// response even when the sender knows only its prior input IDs.
		let rows = self.query("SELECT e.id,e.vtxo_id,256+COALESCE((SELECT SUM(768::BIGINT+COALESCE(octet_length(a.sweep_tx),0)+COALESCE(octet_length(c.spk),0))::BIGINT FROM expiry_allocation_coin c JOIN expiry_allocation a ON a.id=c.allocation_id WHERE c.entitlement_id=e.id),0) AS history_bytes FROM expiry_entitlement e WHERE e.id>$1 \
			AND EXISTS(SELECT 1 FROM expiry_allocation_coin c WHERE c.entitlement_id=e.id AND c.owner_mailbox=$2) \
			ORDER BY e.id LIMIT $3", &[&after_entitlement_id,&mailbox.to_string(),&(i64::from(limit)+1)]).await?;
		let mut page = Vec::new();
		let mut bytes = 0_i64;
		for row in rows.iter().take(limit as usize) {
			let size: i64 = row.get("history_bytes");
			ensure!(size > 0, "invalid fallback history size");
			if bytes.checked_add(size).context("fallback history size overflow")? > HISTORY_BYTE_BUDGET {
				if page.is_empty() { return Err(crate::error::FallbackHistoryTooLarge.into()); }
				break;
			}
			bytes += size;
			page.push(row);
		}
		let has_more = page.len() < rows.len();
		let next_after_entitlement_id = if has_more { page.last().map(|r| r.get::<_, i64>("id")) } else { None };
		let ids = page.into_iter().map(|r| r.get::<_, &str>("vtxo_id").parse::<VtxoId>()).collect::<Result<Vec<_>, _>>()?;
		let entitlements = self.fallback_payout_details_inner(&ids).await?;
		self.check_expiry_history_view(&view_token).await?;
		Ok(FallbackEntitlementPage { entitlements, view_token, next_after_entitlement_id })
	}

	/// Return only requested identifiers, with bounded request and history sizes.
	/// The RPC applies the same ownership/proof rules as GetVtxoStatus.
	pub async fn fallback_payout_details(&self, ids: &[VtxoId]) -> anyhow::Result<FallbackEntitlementPage> {
		let view_token = self.expiry_history_view_token().await?;
		let entitlements = self.fallback_payout_details_inner(ids).await?;
		self.check_expiry_history_view(&view_token).await?;
		Ok(FallbackEntitlementPage { entitlements, view_token, next_after_entitlement_id: None })
	}

	async fn check_expiry_history_view(&self, token: &[u8]) -> anyhow::Result<()> {
		if token!=self.expiry_history_view_token().await?.as_slice() {
			return Err(crate::error::FallbackHistoryViewChanged.into());
		}
		Ok(())
	}

	pub async fn expiry_history_view_token(&self) -> anyhow::Result<Vec<u8>> {
		let row = self.query_one("SELECT v.generation,v.invalid,v.rebuilt_captaind_height,v.rebuilt_captaind_hash, \
			v.rebuilt_watchmand_height,v.rebuilt_watchmand_hash, \
			(SELECT COUNT(*) FROM expiry_ledger_commit) AS ledger_count, \
			(SELECT COALESCE(MAX(revision),0) FROM expiry_ledger_commit) AS ledger_revision \
			FROM expiry_chain_view v WHERE v.id=1", &[]).await?;
		let payload = serde_json::json!([
			1, row.get::<_, i64>("generation"), row.get::<_, bool>("invalid"),
			row.get::<_, Option<i64>>("rebuilt_captaind_height"), row.get::<_, Option<&str>>("rebuilt_captaind_hash"),
			row.get::<_, Option<i64>>("rebuilt_watchmand_height"), row.get::<_, Option<&str>>("rebuilt_watchmand_hash"),
			row.get::<_, i64>("ledger_count"), row.get::<_, i64>("ledger_revision"),
		]);
		Ok(sha256::Hash::hash(&serde_json::to_vec(&payload)?).to_byte_array().to_vec())
	}

	async fn fallback_payout_details_inner(&self, ids: &[VtxoId]) -> anyhow::Result<Vec<FallbackEntitlementDetails>> {
		ensure!(ids.len() <= 1_024, "too many fallback payout coin identifiers");
		if ids.is_empty() { return Ok(Vec::new()); }
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		let size = self.query_one("SELECT COALESCE(SUM(256::BIGINT+512+COALESCE(octet_length(a.sweep_tx),0)+COALESCE(octet_length(c.spk),0)),0)::BIGINT FROM expiry_entitlement e LEFT JOIN expiry_allocation_coin c ON c.entitlement_id=e.id LEFT JOIN expiry_allocation a ON a.id=c.allocation_id WHERE e.vtxo_id=ANY($1)", &[&ids]).await?.get::<_, i64>(0);
		if size > HISTORY_BYTE_BUDGET { return Err(crate::error::FallbackHistoryTooLarge.into()); }
		let rows = self.query("SELECT view.invalid AS chain_invalid,e.id AS entitlement_id,e.vtxo_id,e.gross_sat AS entitlement_gross, \
			c.allocation_id,c.active,c.disposition,c.spk,c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat, \
			a.status,a.sweep_txid,a.sweep_tx,a.broadcast_at,a.confirmed_height,a.confirmed_hash, \
			p.output_index,p.amount_sat AS output_amount, \
			x.txid AS claim_txid,x.height AS claim_height,x.hash AS claim_hash \
			FROM expiry_entitlement e CROSS JOIN expiry_chain_view view \
			LEFT JOIN expiry_allocation_coin c ON c.entitlement_id=e.id \
			LEFT JOIN expiry_allocation a ON a.id=c.allocation_id \
			LEFT JOIN expiry_allocation_payout p ON p.allocation_id=c.allocation_id AND p.spk=c.spk \
			LEFT JOIN expiry_terminal_claim x ON x.entitlement_id=e.id \
			WHERE view.id=1 AND e.vtxo_id=ANY($1) ORDER BY e.id,c.allocation_id LIMIT 20001", &[&ids]).await?;
		ensure!(rows.len() <= 20_000, "fallback payout history exceeds one response; request fewer identifiers");
		let mut details = BTreeMap::<i64, FallbackEntitlementDetails>::new();
		for row in rows {
			let valid = !row.get::<_, bool>("chain_invalid");
			let id: i64 = row.get("entitlement_id");
			let terminal_claim = if valid {
				row.get::<_, Option<&str>>("claim_txid").map(|txid| -> anyhow::Result<_> {
					Ok(FallbackChainPayment {
						txid: txid.parse()?,
						confirmation: point(row.get("claim_height"),row.get("claim_hash"))?.context("terminal claim lacks confirmation")?,
					})
				}).transpose()?
			} else { None };
			let entry = details.entry(id).or_insert(FallbackEntitlementDetails {
				entitlement_id: id, vtxo_id: row.get::<_, &str>("vtxo_id").parse()?,
				gross: amount(&row,"entitlement_gross")?, chain_view_valid: valid,
				attempts: Vec::new(), terminal_claim,
			});
			let Some(allocation_id) = row.get::<_, Option<i64>>("allocation_id") else { continue; };
			let confirmation = if valid { point(row.get("confirmed_height"),row.get("confirmed_hash"))? } else { None };
			let stage = match row.get::<_, &str>("status") {
				"conflicted" => FallbackPayoutStage::Conflicted,
				"confirmed" if confirmation.is_some() => FallbackPayoutStage::Confirmed,
				_ if row.get::<_, Option<DateTime<Utc>>>("broadcast_at").is_some() => FallbackPayoutStage::Broadcast,
				_ => FallbackPayoutStage::Committed,
			};
			let payout = amount(&row,"payout_sat")?;
			let disposition = match row.get::<_, &str>("disposition") {
				"customer" if payout > Amount::ZERO => FallbackDisposition::CustomerPayout,
				"customer" => FallbackDisposition::BelowDust,
				"operator" => FallbackDisposition::Operator,
				_ => bail!("unknown stored fallback disposition"),
			};
			let output_index = row.get::<_, Option<i32>>("output_index").map(u32::try_from).transpose()?;
			let script = row.get::<_, Option<&str>>("spk").map(ScriptBuf::from_hex).transpose()?;
			let output_amount = row.get::<_, Option<i64>>("output_amount").map(|v| u64::try_from(v).map(Amount::from_sat)).transpose()?;
			let parent: Option<Transaction> = row.get::<_, Option<&[u8]>>("sweep_tx").map(deserialize).transpose()?;
			if let Some(parent) = &parent {
				ensure!(Some(parent.compute_txid().to_string().as_str()) == row.get::<_, Option<&str>>("sweep_txid"), "stored fallback transaction identifier mismatch");
				if let Some(index) = output_index {
					let output = parent.output.get(index as usize).context("fallback payout output is absent")?;
					ensure!(Some(&output.script_pubkey) == script.as_ref() && Some(output.value) == output_amount,
						"fallback payout output differs from committed destination or amount");
				}
			}
			entry.attempts.push(FallbackAllocationAttempt {
				allocation_id, active: row.get("active"), stage, disposition, output_index, script,
				gross: amount(&row,"gross_sat")?, fee: amount(&row,"fee_sat")?, payout,
				forfeited: amount(&row,"forfeited_sat")?, output_amount, parent, confirmation,
			});
		}
		Ok(details.into_values().collect())
	}
}
