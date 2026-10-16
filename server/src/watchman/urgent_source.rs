//! Current physical/native policy evidence under the caller's source locks.
//! No fee, signing or publication capability follows from this observation.
//! The publisher must still bind the exact paid attempt, private signed bytes,
//! retained inputs and nursery tuple. DIRECT entry points remain held.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Transaction, TxOut, Txid};
use bitcoind_async_client::{Client, traits::Reader};
use bitcoin_ext::{BlockHeight, BlockRef};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;

use ark::{ServerVtxo, ServerVtxoPolicy, VtxoId, VtxoPolicy};
use crate::database::{BlockTable, SpendState, Tx};
use crate::wallet::PersistedWallet;
use super::{Action, Config};
use super::ordinary_fee_auth::FeeServiceIdentity;
use super::policy::ActionContextFetcher;
use super::urgent_publication::UrgentPurpose;

/// Actual locked source cut, not a permission token. A later signing phase must
/// independently obtain a NEW cut while retaining the owning guard and SQL locks.
pub(crate) struct CheckedUrgentSourceCut { tip: BlockRef, lower: BlockHeight, deadline: BlockHeight }
impl CheckedUrgentSourceCut {
	pub(crate) fn tip(&self) -> BlockRef { self.tip }
	pub(crate) fn lower_height(&self) -> BlockHeight { self.lower }
	pub(crate) fn deadline(&self) -> BlockHeight { self.deadline }
}

fn require_action(action: &Action, purpose: UrgentPurpose, parent: Option<Txid>) -> anyhow::Result<()> {
	ensure!(match (purpose, action, parent) {
		(UrgentPurpose::Claim, Action::Claim { .. }, None) => true,
		(UrgentPurpose::Progression, Action::Progress { txid, .. }, Some(parent)) => *txid == parent,
		_ => false,
	}, "current native action is absent, changed, nonurgent or bound to another parent");
	Ok(())
}
fn require_unselected_floor(eligible: &BTreeMap<OutPoint, TxOut>, selected: &BTreeMap<OutPoint, TxOut>,
	floor: Amount,
) -> anyhow::Result<()> {
	for (point, output) in selected {
		ensure!(eligible.get(point) == Some(output), "urgent selected whole fee input is not current confirmed trusted and unreserved OWN inventory");
	}
	let retained = eligible.iter().filter(|(point, _)| !selected.contains_key(point))
		.try_fold(0u64, |sum, (_, output)| sum.checked_add(output.value.to_sat()).context("urgent unselected physical source overflow"))?;
	ensure!(retained >= floor.to_sat(), "urgent whole-input selection would consume the independently protected physical floor");
	Ok(())
}
fn checked_lower_height(core: BlockRef, cap: BlockRef, watch: BlockRef, confirmed: BlockHeight) -> anyhow::Result<BlockHeight> {
	ensure!(cap.height <= core.height && watch.height <= core.height, "urgent native source is ahead of independent Core");
	let lower = cap.height.min(watch.height).min(core.height);
	ensure!(confirmed <= lower, "urgent physical input has not reached both owning source checkpoints");
	Ok(lower)
}

/// Caller holds the actual Watchman wallet guard BEFORE this first SQL call.
/// All network and database work while holding financial locks has one aggregate
/// deadline. Error/timeout is not source absence or permission to try signing.
/// Only the current paid-send operator claim and exact existing native progress
/// action are represented here. Receive cancellation and conditional exchanges
/// require their separate genuine financial closure and remain unsupported.
pub(crate) async fn require_current_sources(t: &Tx<'_>, core: &Client, wallet: &PersistedWallet,
	db: &crate::database::Db, config: &Config, budget: &super::ordinary_fee::OrdinaryFeeBudget, identity: &FeeServiceIdentity,
	sources: &[ServerVtxo], fee_inputs: &BTreeMap<OutPoint, TxOut>,
	purpose: UrgentPurpose, parent: Option<&Transaction>,
) -> anyhow::Result<CheckedUrgentSourceCut> {
	tokio::time::timeout(Duration::from_secs(15), require_bounded(t, core, wallet, db, config, budget,
		identity, sources, fee_inputs, purpose, parent)).await.context("aggregate urgent source SQL/Core deadline exceeded")?
}
async fn require_bounded(t: &Tx<'_>, core: &Client, wallet: &PersistedWallet,
	db: &crate::database::Db, config: &Config, budget: &super::ordinary_fee::OrdinaryFeeBudget, identity: &FeeServiceIdentity,
	sources: &[ServerVtxo], fee_inputs: &BTreeMap<OutPoint, TxOut>, purpose: UrgentPurpose, parent: Option<&Transaction>,
) -> anyhow::Result<CheckedUrgentSourceCut> {
	t.lock_final_fee_sources().await?;
	ensure!(budget.actual_owner_identity()? == *identity, "urgent actual owner opening identity differs");
	ensure!(!sources.is_empty() && sources.len() <= 4096 && fee_inputs.len() <= 4096,
		"urgent complete native source/input set is empty or exceeds bound");
	let ids = sources.iter().map(|source| source.id().to_string()).collect::<BTreeSet<_>>();
	ensure!(ids.len() == sources.len(), "urgent current source repeats a native input");
	let ids = ids.into_iter().collect::<Vec<_>>();
	// Preflight the complete stored Full bodies BEFORE the native decoder reads
	// them. No row is omitted or replaced by the caller's earlier scan.
	let bounds = t.query_one("SELECT count(*),COALESCE(sum(octet_length(vtxo)),0)::bigint,COALESCE(max(octet_length(vtxo)),0)
		FROM vtxo WHERE vtxo_id=ANY($1)", &[&ids]).await?;
	ensure!(bounds.get::<_, i64>(0) == i64::try_from(sources.len())? && bounds.get::<_, i64>(1) <= 16 * 1024 * 1024
		&& bounds.get::<_, i32>(2) <= 4 * 1024 * 1024, "urgent original native rows are missing or exceed complete byte bound");
	t.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE", &[&ids]).await?;
	let mut frozen = sources.iter().flat_map(|source| [source.id(), VtxoId::from(source.chain_anchor())]).collect::<Vec<_>>();
	frozen.sort(); frozen.dedup(); t.check_not_expiry_frozen(&frozen).await?;
	let before = crate::bitcoind::tip(core).await?;
	ensure!(core.get_block_hash(0).await? == identity.genesis, "urgent actual Core genesis differs from owner opening");
	let cap = t.get_highest_block(BlockTable::Captaind).await?.context("urgent cap source checkpoint missing")?;
	let watch = t.get_highest_block(BlockTable::Watchmand).await?.context("urgent Watchman source checkpoint missing")?;
	let lower = cap.height.min(watch.height).min(before.height);
	let hash = core.get_block_hash(u64::from(lower.to_u32())).await?;
	for table in [BlockTable::Captaind, BlockTable::Watchmand] {
		let actual = t.get_block_by_height(table, lower).await?.context("urgent lower checkpoint ancestry missing")?;
		ensure!(actual.hash == hash, "urgent lower source checkpoint is not on independent Core chain");
	}
	let inventory = wallet.inventory()?;
	let eligible = super::ordinary_fee_broker::checked_urgent_source(core, &inventory, identity.genesis).await?;
	require_unselected_floor(&eligible, fee_inputs, budget.urgent_reserve())?;
	if purpose == UrgentPurpose::Claim { ensure!(fee_inputs.is_empty() && parent.is_none(), "urgent claim has foreign fee/input roles"); }
	if purpose == UrgentPurpose::Progression {
		let parent = parent.context("urgent progress source lacks original signed parent")?;
		let stored = t.get_virtual_transaction_by_txid(parent.compute_txid()).await?.context("urgent original native signed parent missing")?;
		ensure!(stored.signed_tx.as_deref() == Some(parent), "urgent progress parent differs from native original signed bytes");
	}
	let mut deadline: Option<BlockHeight> = None;
	for source in sources {
		let current = t.get_server_vtxo_by_id(source.id()).await?;
		ensure!(current.vtxo.to_bare() == *source && source.server_pubkey() == identity.server_pubkey
			&& current.offboarded_in.is_none() && current.spent_in_round.is_none(),
			"urgent original native metadata, signer or financial route changed");
		ensure!(!matches!(current.spend_state, SpendState::Unregistered | SpendState::Unclaimed
			| SpendState::RoundForfeit | SpendState::OffboardForfeit | SpendState::OffboardConnector),
			"urgent conditional or unregistered financial source remains unsupported");
		let physical = core.try_get_tx_out(source.point(), true).await?.context("urgent physical source is spent or unknown")?;
		ensure!(physical.value == source.amount() && physical.script_pub_key.script()? == source.txout().script_pubkey
			&& physical.bestblock == before.hash && physical.confirmations > 0 && physical.confirmations <= before.height.to_u32().checked_add(1).context("urgent Core height overflow")?
			&& (!physical.coinbase || physical.confirmations >= bitcoin::constants::COINBASE_MATURITY),
			"urgent independent current physical value/script/confirmation differs");
		let confirmed = BlockHeight::new(before.height.to_u32().checked_add(1).and_then(|height| height.checked_sub(physical.confirmations))
			.context("urgent actual confirmation height underflow")?);
		let lower = checked_lower_height(before, cap, watch, confirmed)?;
		ensure!(current.confirmed_height == Some(confirmed), "urgent native confirmation does not match independent Core");
		let funding = core.get_raw_transaction_verbosity_zero(&source.chain_anchor().txid).await?.0;
		current.vtxo.validate(&funding).context("urgent current original signed native path is invalid or incomplete")?;
		let context = ActionContextFetcher { config, db, bitcoind: core, chain_tip_height: lower };
		let action = context.get_action_locked(t, source, confirmed).await?;
		require_action(&action, purpose, parent.map(Transaction::compute_txid))?;
		let bound = match action { Action::Claim { deadline } | Action::Progress { deadline, .. } => deadline,
			_ => bail!("urgent current source has no actual native deadline"), };
		deadline = Some(deadline.map_or(bound, |old| old.min(bound)));
		if purpose == UrgentPurpose::Claim {
			let hash = match source.policy() {
				ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend(policy)) => policy.payment_hash,
				ServerVtxoPolicy::User(VtxoPolicy::ServerHtlcSend_v0(policy)) => policy.payment_hash,
				_ => bail!("urgent claim lacks the supported current paid-send financial ownership proof"),
			};
			// Same exact paid-send operator predicate as current C ownership:
			// actual incoming HTLC association, no revoked/chain-resolved right,
			// authoritative settlement preimage and no unfinalized exchange.
			let association = t.query_opt("SELECT direction::text,offchain_resolution::text,chain_resolution::text
				FROM htlc_vtxo WHERE id=$1 AND payment_hash=$2", &[&current.id,&hash.to_string()]).await?
				.context("urgent paid-send source lacks native HTLC association")?;
			ensure!(matches!(current.spend_state, SpendState::Spendable | SpendState::LnSpent)
				&& association.get::<_, String>(0) == "incoming" && association.get::<_, Option<String>>(1).as_deref() != Some("revoked")
				&& association.get::<_, Option<String>>(2).is_none() && t.get_htlc_settlement_by_payment_hash(hash).await?.is_some(),
				"urgent claim has no current paid-send operator entitlement");
		}
	}
	ensure!(crate::bitcoind::tip(core).await? == before, "urgent actual source crossed independent Core tip");
	Ok(CheckedUrgentSourceCut { tip: before, lower, deadline: deadline.context("urgent source deadline missing")? })
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::hashes::Hash;
	#[test]
	fn urgent_whole_input_floor_uses_unselected_confirmed_coins_without_change_or_budget_credit() {
		let first = OutPoint::new(Txid::from_byte_array([3;32]), 0);
		let second = OutPoint::new(Txid::from_byte_array([4;32]), 0);
		let output = |value| TxOut { value: Amount::from_sat(value), script_pubkey: bitcoin::ScriptBuf::new() };
		let two = BTreeMap::from([(first, output(50_000)), (second, output(50_000))]);
		let selected = BTreeMap::from([(first, output(50_000))]);
		require_unselected_floor(&two, &selected, Amount::from_sat(40_000)).unwrap();
		assert!(require_unselected_floor(&BTreeMap::from([(first, output(100_000))]),
			&BTreeMap::from([(first, output(100_000))]), Amount::from_sat(40_000)).is_err(),
			"a single whole 100k input does not protect 40k via anticipated change");
		assert!(require_unselected_floor(&two, &BTreeMap::from([(first, output(49_999))]), Amount::from_sat(40_000)).is_err());
		assert!(require_unselected_floor(&selected, &BTreeMap::from([(second, output(50_000))]), Amount::from_sat(40_000)).is_err());
		assert!(require_unselected_floor(&BTreeMap::from([(second, output(39_999))]), &BTreeMap::new(), Amount::from_sat(40_000)).is_err());
	}
	#[test]
	fn urgent_lower_source_refuses_missing_confirmation_coverage_and_uses_lesser_checkpoint() {
		let block = |height| BlockRef { height: BlockHeight::new(height), hash: bitcoin::BlockHash::all_zeros() };
		assert_eq!(checked_lower_height(block(200), block(200), block(150), BlockHeight::new(100)).unwrap(), BlockHeight::new(150));
		assert!(checked_lower_height(block(200), block(200), block(150), BlockHeight::new(151)).is_err());
		assert!(checked_lower_height(block(200), block(201), block(200), BlockHeight::new(100)).is_err());
		assert!(checked_lower_height(block(200), block(200), block(201), BlockHeight::new(100)).is_err());
	}
	#[test]
	fn urgent_action_refuses_wait_sweep_and_changed_parent_instead_of_reusing_old_scan() {
		let parent = Txid::from_byte_array([1;32]); let other = Txid::from_byte_array([2;32]);
		let deadline = BlockHeight::new(200);
		require_action(&Action::Claim { deadline }, UrgentPurpose::Claim, None).unwrap();
		require_action(&Action::Progress { txid: parent, deadline }, UrgentPurpose::Progression, Some(parent)).unwrap();
		for action in [Action::Wait, Action::Sweep, Action::Claim { deadline }, Action::Progress { txid: other, deadline }] {
			assert!(require_action(&action, UrgentPurpose::Progression, Some(parent)).is_err());
		}
		assert!(require_action(&Action::Progress { txid: parent, deadline }, UrgentPurpose::Claim, None).is_err());
		assert!(require_action(&Action::Claim { deadline }, UrgentPurpose::Claim, Some(parent)).is_err());
	}
}
