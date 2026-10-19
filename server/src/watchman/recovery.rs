//! Rebuild chain observations from a completed, canonical sync generation.
//! Immutable ownership decisions and spend fences are never rolled back.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use bitcoin::{Transaction, Txid};
use anyhow::Context;
use bitcoin_ext::TransactionExt;
use tracing::warn;
use bitcoin::taproot::LeafVersion;
use bitcoin_ext::{BlockRef, TxStatus};
use bitcoind_async_client::traits::Reader;

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoPolicy};
use ark::vtxo::TapScriptClause;

use crate::bitcoind as bcd;
use crate::database::expiry_chain::ExpiryViewToken;
use super::Watchman;

/// An intermediate signed tree transaction never proves a terminal user exit.
fn terminal_user_claim(vtxo: &ServerVtxo, spend: &Transaction) -> bool {
	let Some(input) = spend.input.iter().find(|i| i.previous_output == vtxo.point()) else {
		return false;
	};
	let Some(leaf) = input.witness.taproot_leaf_script() else {
		return false;
	};
	if leaf.version != LeafVersion::TapScript {
		return false;
	}
	let script = match vtxo.policy() {
		ServerVtxoPolicy::User(VtxoPolicy::Pubkey(p)) => p.user_pubkey_claim_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(p)) => p.user_claim_after_expiry_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend_v0(p)) => p.user_claim_after_expiry_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcRecv(p)) => p.user_reveals_preimage_clause(vtxo.exit_delta()).tapscript(),
		ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcRecv_v0(p)) => p.user_reveals_preimage_clause(vtxo.exit_delta()).tapscript(),
		_ => return false,
	};
	leaf.script == script.as_script()
}

impl Watchman {
	pub(crate) async fn rebuild_expiry_state(&self) -> anyhow::Result<Option<ExpiryViewToken>> {
		let Some(view) = self.db.read(async |t| t.expiry_rebuild_token().await).await? else {
			return Ok(None);
		};
		self.verify_expiry_view(view).await?;
		if self.db.read(async |t| t.expiry_view_is_rebuilt(view).await).await? {
			return Ok(Some(view));
		}
		let (ids, txids, claims) = self.db.read(async |t| {
			let allocations = t.query("
				SELECT id, sweep_txid
				FROM expiry_allocation
				ORDER BY id
			", &[]).await?;
			let ids = allocations.iter().map(|r| r.get::<_, i64>("id")).collect::<Vec<_>>();
			let mut txids = allocations.iter().filter_map(|r| r.get::<_, Option<&str>>("sweep_txid"))
				.map(Txid::from_str).collect::<Result<BTreeSet<_>, _>>()?;
			for row in t.query("
				SELECT txid
				FROM expiry_fee_child
			", &[]).await? {
				txids.insert(row.get::<_, &str>("txid").parse()?);
			}
			let mut claims = Vec::new();
			for row in t.query("
				SELECT e.id, v.vtxo, v.onchain_spent_txid
				FROM expiry_entitlement e
				JOIN vtxo v ON v.vtxo_id = e.vtxo_id
				WHERE v.onchain_spent_height IS NOT NULL
				AND v.onchain_spent_height <= $1
			", &[&i32::try_from(view.captaind.height.min(view.watchmand.height).to_u32())?]).await? {
				let txid: Txid = row.get::<_, &str>("onchain_spent_txid").parse()?;
				// The persisted virtual transaction index distinguishes known
				// intermediate steps from a terminal claimant transaction.
				if t.get_virtual_transaction_by_txid(txid).await?.is_some() {
					continue;
				}
				txids.insert(txid);
				claims.push((row.get::<_, i64>("id"), ServerVtxo::deserialize(row.get("vtxo"))?, txid));
			}
			Ok((ids, txids, claims))
		}).await?;
		let mut confirmed = BTreeMap::<Txid, BlockRef>::new();
		for txid in txids {
			if let TxStatus::Confirmed(point) = bcd::tx_status(&self.bitcoind, txid).await? {
				if point.height <= view.captaind.height.min(view.watchmand.height) {
					confirmed.insert(txid, point);
				}
			}
		}
		// The ordinary package's child may confirm before any sweep observes
		// it in the mempool. Recover its actual body from the parent's canonical
		// block, including after restart; no anchor-discovery hint marks it paid.
		let parents = self.db.read(async |t| Ok(t.query("
			SELECT a.id, a.sweep_tx, (SELECT COUNT(*)
				FROM expiry_allocation_payout p
				WHERE p.allocation_id = a.id) AS customer_outputs, ARRAY(SELECT f.txid
				FROM expiry_fee_child f
				WHERE f.allocation_id = a.id) AS known_children
			FROM expiry_allocation a
			WHERE a.sweep_tx IS NOT NULL
			ORDER BY a.id
		", &[]).await?)).await?;
		let mut observations = Vec::new();
		let mut current_block = None;
		for row in parents {
			let id: i64 = row.get("id");
			let parent: Transaction = bitcoin::consensus::deserialize(row.get("sweep_tx"))?;
			let Some(point) = confirmed.get(&parent.compute_txid()).copied() else {
				continue;
			};
			let known = row.get::<_, Vec<String>>("known_children").into_iter()
				.map(|id| id.parse::<Txid>()).collect::<Result<Vec<_>, _>>()?;
			if known.iter().any(|id| confirmed.contains_key(id)) {
				continue;
			}
			let (anchor, _) = parent.fee_anchor().context("confirmed expiry parent has no anchor")?;
			if current_block.as_ref().is_none_or(|block: &bitcoin::Block| block.block_hash() != point.hash) {
				current_block = Some(self.bitcoind.get_block(&point.hash).await?);
			}
			let block = current_block.as_ref().context("missing canonical parent block")?;
			ensure!(block.block_hash() == point.hash && block.txdata.iter().any(|tx| tx == &parent),
				"confirmed expiry parent body differs from actual canonical block");
			for child in block.txdata.iter().filter(|tx| tx.input.iter().any(|input| input.previous_output == anchor)) {
				if self.db.read(async |t| Ok(t.query_opt("
					SELECT id
					FROM expiry_fee_child
					WHERE txid = $1
				",
					&[&child.compute_txid().to_string()]).await?.is_some())).await? {
					continue;
				}
				let observed = async {
					let prevouts = self.observed_fee_prevouts(view, &parent, child).await?;
					let wallet = self.watchman_wallet.lock().await;
					super::fee_sponsor::recognize_fee_only(
						&parent,
						usize::try_from(row.get::<_, i64>("customer_outputs"))?,
						child,
						&prevouts,
						|script| script == self.drain_spk.as_script() || wallet.is_mine(script.to_owned())
					)
				}.await;
				match observed {
					Ok(observed) => {
						confirmed.insert(child.compute_txid(), point);
						observations.push((id, parent.clone(), observed));
					},
					Err(error) => warn!(
						allocation_id = id,
						child = %child.compute_txid(),
						error = %format_args!("{error:#}"),
						"Canonical payout sponsor accounting remains unclassified"
					),
				}
			}
		}

		let mut paid_claims = Vec::new();
		for (id, vtxo, txid) in claims {
			let Some(point) = confirmed.get(&txid) else {
				continue;
			};
			let spend = self.bitcoind.get_raw_transaction_verbosity_zero(&txid).await?.0;
			if terminal_user_claim(&vtxo, &spend) {
				paid_claims.push((id, txid, *point));
			}
		}
		self.verify_expiry_view(view).await?;
		let rebuilt = self.db.write(async |t| {
			t.query_one("
				SELECT generation
				FROM expiry_chain_view
				WHERE id = 1
				FOR UPDATE
			", &[]).await?;
			if t.expiry_rebuild_token().await? != Some(view) {
				return Ok(false);
			}
			let current = t.query("
				SELECT id
				FROM expiry_allocation
				ORDER BY id
				FOR UPDATE
			", &[]).await?
				.into_iter().map(|r| r.get::<_, i64>("id")).collect::<Vec<_>>();
			ensure!(current == ids, "expiry allocations changed during chain reconstruction");
			for (id, parent, observed) in &observations {
				t.store_observed_expiry_fee_child(view, *id, parent, observed).await?;
			}
			t.execute("
				UPDATE expiry_allocation
				SET confirmed_height = NULL, confirmed_hash = NULL, status = 'pending'
			", &[]).await?;
			t.execute("
				UPDATE expiry_fee_child
				SET confirmed_height = NULL, confirmed_hash = NULL
			", &[]).await?;
			t.execute("
				UPDATE expiry_allocation_coin
				SET paying_txid = NULL
			", &[]).await?;
			t.execute("
				DELETE
				FROM expiry_terminal_claim
			", &[]).await?;
			for (txid, point) in &confirmed {
				t.execute("
					UPDATE expiry_allocation
					SET status = 'confirmed', confirmed_height = $2, confirmed_hash = $3
					WHERE sweep_txid = $1
				", &[
					&txid.to_string(),
					&i64::from(point.height),
					&point.hash.to_string()
				]).await?;
				t.execute("
					UPDATE expiry_fee_child
					SET confirmed_height = $2, confirmed_hash = $3
					WHERE txid = $1
				", &[
					&txid.to_string(),
					&i64::from(point.height),
					&point.hash.to_string()
				]).await?;
			}
			// Only an observed spend in the completed prefix invalidates a
			// parent. A mempool conflict is not a final discharge or replan.
			t.execute("
				UPDATE expiry_allocation a
				SET status = 'conflicted'
				WHERE a.status = 'pending'
				AND EXISTS(SELECT 1
					FROM expiry_allocation_input i
					JOIN vtxo v ON v.vtxo_id = i.vtxo_id
					WHERE i.allocation_id = a.id
					AND v.onchain_spent_height <= $1
					AND v.onchain_spent_txid IS DISTINCT FROM a.sweep_txid)
			", &[&i32::try_from(view.captaind.height.min(view.watchmand.height).to_u32())?]).await?;
			// A chain-only ownership premise may have disappeared even when
			// another input in that group remains unspent. It needs a fresh
			// derived allocation before this process may publish its parent.
			t.execute("
				UPDATE expiry_allocation a
				SET status = 'conflicted'
				WHERE a.status = 'pending'
				AND EXISTS(SELECT 1
					FROM expiry_allocation_coin c
					WHERE c.allocation_id = a.id
					AND c.evidence->>'kind' IN ('htlc_chain', 'htlc_internal_grants_reclaimed')
					AND NOT EXISTS(SELECT 1
						FROM watchmand_block b
						WHERE b.height = (c.evidence->>'height')::BIGINT
						AND b.hash = c.evidence->>'hash'))
			", &[]).await?;
			t.execute("
				UPDATE expiry_allocation_coin c
				SET paying_txid = a.sweep_txid
				FROM expiry_allocation a
				WHERE a.id = c.allocation_id
				AND a.status = 'confirmed'
			", &[]).await?;
			for (id, txid, point) in &paid_claims {
				t.execute("
					INSERT INTO expiry_terminal_claim(entitlement_id, txid, height, hash)
					VALUES($1, $2, $3, $4)
				", &[
					id,
					&txid.to_string(),
					&i64::from(point.height),
					&point.hash.to_string()
				]).await?;
				t.execute("
					UPDATE expiry_allocation_coin
					SET paying_txid = $2
					WHERE entitlement_id = $1
				", &[
					id,
					&txid.to_string(),
				]).await?;
			}
			let duplicated = t.query_one("
				SELECT EXISTS(SELECT 1
					FROM expiry_allocation_coin c
					JOIN expiry_allocation a ON a.id = c.allocation_id
					WHERE a.status = 'confirmed'
					GROUP BY c.entitlement_id
					HAVING COUNT(DISTINCT a.sweep_txid) > 1) AS duplicated
			", &[]).await?.get::<_, bool>("duplicated");
			ensure!(!duplicated, "one expiry entitlement has multiple canonical payout transactions");
			// A reorg may restore an earlier immutable parent and remove a
			// replacement's input branch. The canonical payment wins globally,
			// including when that older allocation link was inactive.
			t.execute("
				UPDATE expiry_allocation a
				SET status = 'conflicted'
				WHERE a.status = 'pending'
				AND EXISTS(SELECT 1
					FROM expiry_allocation_coin c
					WHERE c.allocation_id = a.id
					AND (EXISTS(SELECT 1
							FROM expiry_terminal_claim p
							WHERE p.entitlement_id = c.entitlement_id)
						OR EXISTS(SELECT 1
							FROM expiry_allocation_coin paid
							JOIN expiry_allocation winner ON winner.id = paid.allocation_id
							WHERE paid.entitlement_id = c.entitlement_id
							AND winner.status = 'confirmed'
							AND winner.id <> a.id)))
			", &[]).await?;
			t.execute("
				UPDATE expiry_allocation_coin c
				SET active = FALSE
				WHERE EXISTS(SELECT 1
					FROM expiry_terminal_claim p
					WHERE p.entitlement_id = c.entitlement_id)
				OR EXISTS(SELECT 1
					FROM expiry_allocation_coin paid
					JOIN expiry_allocation winner ON winner.id = paid.allocation_id
					WHERE paid.entitlement_id = c.entitlement_id
					AND winner.status = 'confirmed')
			", &[]).await?;
			t.execute("
				UPDATE expiry_allocation_coin c
				SET active = TRUE
				FROM expiry_allocation a
				WHERE a.id = c.allocation_id
				AND a.status = 'confirmed'
				AND NOT EXISTS(SELECT 1
					FROM expiry_terminal_claim p
					WHERE p.entitlement_id = c.entitlement_id)
			", &[]).await?;
			// Spent fences survive every rebuild. A later wallet sync must not
			// turn an allocated obligation back into a spendable source coin.
			t.execute("
				UPDATE vtxo
				SET spend_state = 'spent', updated_at = NOW()
				WHERE spend_state <> 'spent'
				AND EXISTS(SELECT 1
					FROM expiry_decided_coin d
					WHERE d.vtxo_id = vtxo.vtxo_id)
			", &[]).await?;
			t.finish_expiry_rebuild(view).await
		}).await?;
		Ok(rebuilt.then_some(view))
	}
}

#[cfg(test)]
mod tests {
	use bitcoin::{absolute, transaction, Sequence, TxIn, Witness};
	use ark::test_util::VTXO_VECTORS;

	use super::*;

	/// Script recognition is intentionally narrower than transaction validity;
	/// the production caller first requires Core's canonical confirmation.
	#[test]
	fn terminal_user_clause_requires_the_exact_source_outpoint() {
		let vtxo = ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone()).to_bare();
		let ServerVtxoPolicy::User(VtxoPolicy::Pubkey(policy)) = vtxo.policy() else {
			panic!("board vector must be ordinary");
		};
		let script = policy.user_pubkey_claim_clause(vtxo.exit_delta()).tapscript();
		let info = vtxo.policy().taproot(vtxo.server_pubkey(), vtxo.exit_delta(), vtxo.expiry_height());
		let control = info.control_block(&(script.clone(), LeafVersion::TapScript)).unwrap().serialize();
		let mut tx = Transaction {
			version: transaction::Version::TWO,
			lock_time: absolute::LockTime::ZERO,
			input: vec![TxIn {
				previous_output: vtxo.point(),
				script_sig: bitcoin::ScriptBuf::new(),
				sequence: Sequence::ZERO,
				witness: Witness::from_slice(&[vec![0; 64], script.into_bytes(), control])
			}],
			output: Vec::new(),
		};
		assert!(terminal_user_claim(&vtxo, &tx));
		tx.input[0].previous_output.vout += 1;
		assert!(!terminal_user_claim(&vtxo, &tx));
		tx.input[0].previous_output = vtxo.point();
		tx.input[0].witness = Witness::new();
		assert!(!terminal_user_claim(&vtxo, &tx));
		// A known signed tree step spending an ancestor never consumes this
		// leaf; matching its expiry or funding anchor is insufficient.
		tx.input[0].previous_output = vtxo.chain_anchor();
		assert!(!terminal_user_claim(&vtxo, &tx));
	}
}
