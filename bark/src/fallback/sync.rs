//! Authenticated account history collection. No partial page set is published.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use anyhow::Context;
use bitcoin::{Amount, BlockHash, ScriptBuf, Transaction, Txid};
use bitcoin::hashes::Hash;

use ark::{ProtocolEncoding, VtxoId};
use ark::recovery::XpubRecoveryAuthorization;
use server_rpc::protos;

use crate::{Wallet, FallbackAttempt, FallbackBlock, FallbackDisposition, FallbackEntitlement,
	FallbackPayoutStage, FallbackTerminalClaim};
use crate::movement::{MovementId, MovementSubsystem};
use crate::vtxo::VtxoState;
use crate::movement::update::MovementUpdate;
use super::payouts::{FallbackSnapshot, FALLBACK_SUBSYSTEM};

// Raw parent transactions are repeated per association in the initial wire
// shape. Keep native client pages modest while the server accepts up to 1000.
const PAGE_SIZE: u32 = 32;

#[derive(Debug, thiserror::Error)]
#[error("fallback payout ledger view changed during collection")]
struct PayoutViewChanged;


fn block(value: protos::FallbackPayoutBlock) -> anyhow::Result<FallbackBlock> {
	Ok(FallbackBlock { height: value.height.into(), hash: BlockHash::from_slice(&value.hash)? })
}

fn receipt(raw: protos::FallbackEntitlementDetails) -> anyhow::Result<FallbackEntitlement> {
	let attempts = raw.attempts.into_iter().map(|attempt| -> anyhow::Result<_> {
		Ok(FallbackAttempt {
			allocation_id: attempt.allocation_id, active: attempt.active,
			stage: match protos::FallbackPayoutStage::try_from(attempt.stage)? {
				protos::FallbackPayoutStage::Committed => FallbackPayoutStage::Committed,
				protos::FallbackPayoutStage::Broadcast => FallbackPayoutStage::Broadcast,
				protos::FallbackPayoutStage::Confirmed => FallbackPayoutStage::Confirmed,
				protos::FallbackPayoutStage::Conflicted => FallbackPayoutStage::Conflicted,
				_ => bail!("server returned unspecified fallback payout stage"),
			},
			disposition: match protos::FallbackPayoutDisposition::try_from(attempt.disposition)? {
				protos::FallbackPayoutDisposition::CustomerPayout => FallbackDisposition::CustomerPayout,
				protos::FallbackPayoutDisposition::BelowDust => FallbackDisposition::BelowDust,
				protos::FallbackPayoutDisposition::Operator => FallbackDisposition::Operator,
				_ => bail!("server returned unspecified fallback payout disposition"),
			},
			output_index: attempt.output_index,
			script: (!attempt.script_pubkey.is_empty()).then(|| ScriptBuf::from_bytes(attempt.script_pubkey)),
			gross: Amount::from_sat(attempt.gross_sat), fee: Amount::from_sat(attempt.fee_sat),
			payout: Amount::from_sat(attempt.payout_sat), forfeited: Amount::from_sat(attempt.forfeited_sat),
			output_amount: attempt.output_amount_sat.map(Amount::from_sat),
			parent: if attempt.parent_tx.is_empty() { None } else { Some(bitcoin::consensus::deserialize::<Transaction>(&attempt.parent_tx)?) },
			confirmation: attempt.confirmation.map(block).transpose()?,
		})
	}).collect::<anyhow::Result<Vec<_>>>()?;
	Ok(FallbackEntitlement {
		entitlement_id: raw.entitlement_id, vtxo_id: VtxoId::from_slice(&raw.vtxo_id)?,
		gross: Amount::from_sat(raw.gross_sat), chain_view_valid: raw.chain_view_valid,
		attempts,
		terminal_claim: raw.terminal_claim.map(|claim| -> anyhow::Result<_> {
			Ok(FallbackTerminalClaim { txid: Txid::from_slice(&claim.txid)?,
				confirmation: block(claim.confirmation.context("terminal exit lacks confirmation")?)?, })
		}).transpose()?,
		first_seen: chrono::Local::now(), last_updated: chrono::Local::now(),
	})
}

/// Assemble only one server ledger view. The token is intentionally opaque;
/// the server owns its generation/revision semantics and must reject changes.
#[derive(Default)]
struct AccountPages {
	token: Vec<u8>,
	after: i64,
	entitlements: BTreeMap<i64, FallbackEntitlement>,
	coin_ids: HashSet<VtxoId>,
}

impl AccountPages {
	fn accept(&mut self, response: protos::GetFallbackPayoutsResponse, page_size: u32) -> anyhow::Result<bool> {
		ensure!(!response.view_token.is_empty(), "fallback history lacks a ledger view token");
		if self.token.is_empty() { self.token = response.view_token; }
		else if self.token != response.view_token { return Err(PayoutViewChanged.into()); }
		ensure!(response.entitlements.len() <= page_size as usize, "fallback history page exceeds request bound");
		let mut last = self.after;
		for raw in response.entitlements {
			let entitlement = receipt(raw)?;
			ensure!(entitlement.entitlement_id > last, "fallback history cursor is not strictly increasing");
			ensure!(self.coin_ids.insert(entitlement.vtxo_id), "duplicate fallback coin entitlement");
			last = entitlement.entitlement_id;
			self.entitlements.insert(entitlement.entitlement_id, entitlement);
		}
		if let Some(next) = response.next_after_entitlement_id {
			ensure!(next == last && next > self.after, "fallback history page cursor does not match its last entitlement");
			self.after = next;
			Ok(false)
		} else { Ok(true) }
	}
}

impl Wallet {
	async fn collect_fallback_pages_once(&self, page_size: u32) -> anyhow::Result<AccountPages> {
		let (mut server, info) = self.require_server().await?;
		let mut pages = AccountPages::default();
		loop {
			// Authorization is account-wide and freshly signed on each page;
			// cursor and page size are independently bounded server query options.
			let auth = XpubRecoveryAuthorization::new(&self.inner.seed.vtxo, info.server_pubkey,
				chrono::Local::now() + Duration::from_secs(60));
			let response = server.client.get_fallback_payouts(protos::GetFallbackPayoutsRequest {
				query: Some(protos::get_fallback_payouts_request::Query::Account(protos::FallbackPayoutAccountQuery {
					xpub: self.vtxo_xpub().encode().to_vec(), authorization: auth.serialize(),
					after_entitlement_id: pages.after, page_size, view_token: pages.token.clone(),
				})),
			}).await.map_err(|status| {
				if status.code() == tonic::Code::Aborted && status.metadata().get("abandon-reason")
					.and_then(|value| value.to_str().ok()) == Some("payout_view_changed") {
					anyhow::Error::new(status).context(PayoutViewChanged)
				} else { status.into() }
			})?.into_inner();
			if pages.accept(response, page_size)? { return Ok(pages); }
		}
	}

	async fn collect_fallback_pages(&self) -> anyhow::Result<AccountPages> {
		let mut page_size = PAGE_SIZE;
		let mut view_retries = 0;
		loop {
			match self.collect_fallback_pages_once(page_size).await {
				Ok(pages) => return Ok(pages),
				Err(error) if error.is::<PayoutViewChanged>() && view_retries < 2 => { view_retries += 1; },
				Err(error) if error.downcast_ref::<tonic::Status>().is_some_and(|status|
					status.code() == tonic::Code::ResourceExhausted) && page_size > 1 => {
					page_size /= 2;
				},
				Err(error) => return Err(error),
			}
		}
	}

	/// Reconcile the entire registered account, including spent IDs no longer
	/// present after seed restore and unregistered OOR descendants whose leaf
	/// keys belong to a different recipient. A partial or changing page set
	/// never replaces the last complete local receipt snapshot.
	pub async fn sync_fallback_payouts(&self) -> anyhow::Result<()> {
		let Some(record) = self.fallback_registration().await? else { return Ok(()); };
		let collection_base = self.inner.db.get_fallback_snapshot().await?.view_token;
		// Remote pagination must not hold the local protection lock. A
		// stalled server cannot prevent cached chain checks or act-by work.
		let collected = self.collect_fallback_pages().await;
		let _guard = self.inner.lock_manager.lock(&format!("{}.fallback-history", self.fingerprint()), Duration::from_secs(30)).await?;
		let previous = self.inner.db.get_fallback_snapshot().await?;
		let result: anyhow::Result<()> = async {
			ensure!(previous.view_token == collection_base, "concurrent fallback collection changed the local ledger view");
			let pages = collected?;
			ensure!(previous.entitlements.keys().all(|id| pages.entitlements.contains_key(id)),
				"complete fallback history dropped a permanent account entitlement");
			let mut snapshot = FallbackSnapshot {
				entitlements: pages.entitlements, view_token: pages.token,
				local_debits: previous.local_debits.clone(), movement_ids: previous.movement_ids.clone(),
				awaiting_sync: false, awaiting_settlement: previous.awaiting_settlement.clone(),
				verified_payouts: previous.verified_payouts.clone(),
				payout_observations: previous.payout_observations.clone(),
				adopted_payout_vtxos: previous.adopted_payout_vtxos.clone(),
			};
			for entitlement in snapshot.entitlements.values_mut() {
				entitlement.verify(&record.spk)?;
				if let Some(old) = previous.entitlements.get(&entitlement.entitlement_id) {
					ensure!(old.vtxo_id == entitlement.vtxo_id && old.gross == entitlement.gross,
						"server changed an immutable fallback entitlement");
					entitlement.first_seen = old.first_seen;
					if old.attempts == entitlement.attempts && old.chain_view_valid == entitlement.chain_view_valid
						&& old.terminal_claim == entitlement.terminal_claim { entitlement.last_updated = old.last_updated; }
				}
				if let Some(coin) = self.inner.db.get_wallet_vtxo(entitlement.vtxo_id).await? {
					// Seed recovery also keeps historical Spent rows and locks
					// uncertain rows without an action holder. Mere presence does
					// not prove another local Ark balance was consumed. A debit
					// already recorded before adoption remains in local_debits.
					if matches!(coin.state, VtxoState::Spendable | VtxoState::Exited | VtxoState::Locked { holder: Some(_) }) {
						snapshot.local_debits.entry(entitlement.entitlement_id).or_insert(entitlement.gross);
					}
				}
			}
			self.verify_fallback_chain_evidence(&mut snapshot, &record.spk).await?;
			// IDs are existing action-indexed movement IDs. Interrupted reservation
			// leaves only hidden placeholders; publishing the one snapshot makes
			// all current output groups visible together.
			let keys = snapshot.groups()?.into_keys().collect::<Vec<_>>();
			for key in keys {
				if snapshot.movement_ids.contains_key(&key) { continue; }
				let (id, _) = self.inner.db.get_or_create_movement_for_action(
					&MovementSubsystem { name: FALLBACK_SUBSYSTEM.to_owned(), kind: "payout".to_owned() },
					chrono::Local::now(), &format!("fallback/{}", key), MovementUpdate::new(),
				).await?;
				snapshot.movement_ids.insert(key, id);
			}
			// Reserve IDs without emitting zero-value placeholder notifications.
			// Actual receipt notifications follow the atomic snapshot commit.
			let stored = self.inner.db.get_all_movements().await?;
			let network = self.network().await?;
			let old = previous.project(stored.clone(), network)?.into_iter()
				.map(|movement| (movement.id, movement)).collect::<std::collections::HashMap<MovementId, _>>();
			let projected = snapshot.project(stored, network)?;
			self.inner.db.store_fallback_snapshot(&snapshot).await?;
			if let Err(error) = self.apply_fallback_chain_states(&snapshot).await {
				// The durable recovery marker already committed. Never roll it
				// back after a partial state update; next sync retries safely.
				log::warn!("Fallback coin states await retry: {:#}", error);
			}
			for movement in projected.into_iter().filter(|m| m.subsystem.name == FALLBACK_SUBSYSTEM) {
				match old.get(&movement.id) {
					None => self.inner.notifications.dispatch_movement_created(movement),
					Some(previous) if previous != &movement => self.inner.notifications.dispatch_movement_updated(movement),
					Some(_) => {},
				}
			}
			Ok(())
		}.await;
		if let Err(error) = result {
			let mut stale = previous;
			stale.awaiting_sync = true;
			// Server availability must not suppress independent reorg checks.
			if let Err(chain_error) = self.verify_fallback_chain_evidence(&mut stale, &record.spk).await {
				log::warn!("Fallback chain evidence could not be refreshed: {:#}", chain_error);
			}
			self.inner.db.store_fallback_snapshot(&stale).await?;
			self.apply_fallback_chain_states(&stale).await?;
			return Err(error).context("fallback history is awaiting a coherent server view");
		}
		Ok(())
	}

	pub async fn fallback_payouts(&self) -> anyhow::Result<Vec<FallbackEntitlement>> {
		Ok(self.inner.db.get_fallback_snapshot().await?.entitlements.into_values().collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn raw(id: i64) -> protos::FallbackEntitlementDetails {
		use std::str::FromStr;
		protos::FallbackEntitlementDetails {
			entitlement_id: id,
			vtxo_id: VtxoId::from_str(&format!("{:064x}:0", id)).unwrap().to_bytes().to_vec(),
			gross_sat: 5_000, chain_view_valid: true, attempts: vec![], terminal_claim: None,
		}
	}

	#[test]
	fn changed_reorg_or_regroup_view_never_yields_a_publishable_page_set() {
		let mut pages = AccountPages::default();
		assert!(!pages.accept(protos::GetFallbackPayoutsResponse {
			entitlements: vec![raw(1)], next_after_entitlement_id: Some(1), view_token: vec![1; 32],
		}, PAGE_SIZE).unwrap());
		assert!(pages.accept(protos::GetFallbackPayoutsResponse {
			entitlements: vec![raw(2)], next_after_entitlement_id: None, view_token: vec![2; 32],
		}, PAGE_SIZE).is_err());
		assert_eq!(pages.entitlements.len(), 1, "the second view cannot enter the first page set");
		assert!(AccountPages::default().accept(protos::GetFallbackPayoutsResponse {
			entitlements: vec![], next_after_entitlement_id: Some(0), view_token: vec![1; 32],
		}, PAGE_SIZE).is_err(), "a nonadvancing cursor cannot loop forever");
	}

}
