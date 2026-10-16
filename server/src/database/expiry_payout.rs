//! Exact signed payout parents and their independently replaceable fee children.

use anyhow::Context;
use bitcoin::{Amount, ScriptBuf, Transaction, TxOut};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hex::DisplayHex;
use ark::{ProtocolEncoding, ServerVtxo};

use super::expiry_chain::ExpiryViewToken;
use super::Tx;

pub struct PendingExpiryPayout {
	pub id: i64,
	pub inputs: Vec<ServerVtxo>,
	pub payouts: Vec<TxOut>,
	pub parent: Option<Transaction>,
	pub fee_child: Option<(Transaction, Amount)>,
}

impl Tx<'_> {
	/// A group is processed as a whole. Individual-input retries would lose its
	/// cross-tree output aggregation and could charge the output fee twice.
	pub async fn pending_expiry_payouts(&self) -> anyhow::Result<Vec<PendingExpiryPayout>> {
		let rows = self.query("SELECT id,sweep_tx FROM expiry_allocation WHERE status='pending' ORDER BY expiry_height,id", &[]).await?;
		let mut ret = Vec::with_capacity(rows.len());
		for row in rows {
			let id: i64 = row.get("id");
			let inputs = self.query("SELECT v.vtxo FROM expiry_allocation_input i JOIN vtxo v ON v.vtxo_id=i.vtxo_id WHERE i.allocation_id=$1 ORDER BY v.vtxo_id", &[&id]).await?
				.into_iter().map(|r| ServerVtxo::deserialize(r.get("vtxo"))).collect::<Result<Vec<_>, _>>()?;
			let payouts = self.query("SELECT spk,amount_sat FROM expiry_allocation_payout WHERE allocation_id=$1 ORDER BY output_index", &[&id]).await?
				.into_iter().map(|r| Ok(TxOut {
					script_pubkey: ScriptBuf::from_hex(r.get("spk"))?,
					value: Amount::from_sat(u64::try_from(r.get::<_, i64>("amount_sat"))?),
				})).collect::<anyhow::Result<Vec<_>>>()?;
			let parent = row.get::<_, Option<&[u8]>>("sweep_tx").map(deserialize).transpose()?;
			let fee_child = self.query_opt("SELECT raw_tx,fee_sat FROM expiry_fee_child WHERE allocation_id=$1 ORDER BY id DESC LIMIT 1", &[&id]).await?
				.map(|r| -> anyhow::Result<_> {
					let tx = deserialize::<Transaction>(r.get("raw_tx"))?;
					let fee = Amount::from_sat(u64::try_from(r.get::<_, i64>("fee_sat"))?);
					Ok((tx, fee))
				})
				.transpose()?;
			ret.push(PendingExpiryPayout { id, inputs, payouts, parent, fee_child });
		}
		Ok(ret)
	}

	/// Compare-and-set the exact signed parent before any network publication.
	/// Once present, a restart or fee bump must use precisely the same bytes.
	pub async fn store_expiry_parent(
		&self, view: ExpiryViewToken, id: i64, parent: &Transaction,
	) -> anyhow::Result<()> {
		self.lock_valid_expiry_view(view).await?;
		let row = self.query_one("SELECT status,sweep_tx FROM expiry_allocation WHERE id=$1 FOR UPDATE", &[&id]).await?;
		ensure!(row.get::<_, &str>("status") == "pending", "expiry allocation is no longer pending");
		let raw = serialize(parent);
		if let Some(old) = row.get::<_, Option<&[u8]>>("sweep_tx") {
			ensure!(old == raw.as_slice(), "attempt to replace immutable expiry parent");
			return Ok(());
		}
		let inputs = self.query("SELECT v.vtxo_id,v.amount FROM expiry_allocation_input i JOIN vtxo v ON v.vtxo_id=i.vtxo_id WHERE i.allocation_id=$1 ORDER BY v.vtxo_id", &[&id]).await?;
		let mut actual = parent.input.iter().map(|i| i.previous_output.to_string()).collect::<Vec<_>>();
		actual.sort();
		let expected = inputs.iter().map(|r| r.get::<_, String>("vtxo_id")).collect::<Vec<_>>();
		ensure!(actual == expected, "expiry parent does not spend precisely its allocated backing");
		let payouts = self.query("SELECT spk,amount_sat FROM expiry_allocation_payout WHERE allocation_id=$1 ORDER BY output_index", &[&id]).await?;
		ensure!(parent.output.len() >= payouts.len(), "expiry parent omits allocated payouts");
		for (row, output) in payouts.iter().zip(&parent.output) {
			ensure!(output.script_pubkey.as_bytes().to_lower_hex_string() == row.get::<_, &str>("spk")
				&& output.value.to_sat() == u64::try_from(row.get::<_, i64>("amount_sat"))?, "expiry parent changes a payee or amount");
		}
		let input_value = inputs.iter().try_fold(0_u64, |sum,r| sum.checked_add(u64::try_from(r.get::<_, i64>("amount"))?).context("expiry input amount overflow"))?;
		let output_value = parent.output.iter().try_fold(0_u64, |sum,o| sum.checked_add(o.value.to_sat()).context("expiry output amount overflow"))?;
		ensure!(input_value == output_value && parent.version.0 == 3 && parent.vsize() <= 10_000,
			"expiry parent violates zero-fee version-3 policy");
		self.execute("UPDATE expiry_allocation SET sweep_txid=$2,sweep_tx=$3 WHERE id=$1", &[&id,&parent.compute_txid().to_string(),&raw]).await?;
		Ok(())
	}

	pub async fn store_expiry_fee_child(
		&self, view: ExpiryViewToken, id: i64, parent: &Transaction,
		child: &Transaction, fee: Amount,
	) -> anyhow::Result<()> {
		self.lock_valid_expiry_view(view).await?;
		let row = self.query_one("SELECT status,sweep_tx FROM expiry_allocation WHERE id=$1 FOR UPDATE", &[&id]).await?;
		ensure!(row.get::<_, &str>("status") == "pending"
			&& row.get::<_, Option<&[u8]>>("sweep_tx") == Some(serialize(parent).as_slice()), "expiry parent changed before fee child storage");
		let anchor = parent.output.iter().enumerate()
			.find(|(_,o)| o.script_pubkey == *bitcoin_ext::fee::P2A_SCRIPT)
			.map(|(i,_)| bitcoin::OutPoint::new(parent.compute_txid(), i as u32))
			.context("expiry parent has no fee anchor")?;
		ensure!(child.input.iter().filter(|i| i.previous_output == anchor).count() == 1,
			"expiry fee child must spend its parent's anchor exactly once");
		ensure!(child.version.0 == 3, "expiry fee child is not version 3");
		let customer_outputs = usize::try_from(self.query_one("SELECT COUNT(*) FROM expiry_allocation_payout WHERE allocation_id=$1", &[&id]).await?.get::<_, i64>(0))?;
		let accounting = crate::watchman::settlement::payout_fee_accounting(parent,customer_outputs,fee)?;
		self.execute("INSERT INTO expiry_fee_child(allocation_id,txid,raw_tx,fee_sat,operator_remainder_sat) VALUES($1,$2,$3,$4,$5) ON CONFLICT(txid) DO NOTHING",
			&[&id,&child.compute_txid().to_string(),&serialize(child),&i64::try_from(fee.to_sat())?,&i64::try_from(accounting.operator_remainder.to_sat())?]).await?;
		Ok(())
	}
}

impl Tx<'_> {
	/// Request only an actual one-input signed tree step. No manufactured
	/// transaction or arbitrary txid can become a split instruction.
	pub async fn request_expiry_split(
		&self, view: ExpiryViewToken, root: &ServerVtxo,
	) -> anyhow::Result<bool> {
		self.lock_valid_expiry_view(view).await?;
		let row = self.query_one("SELECT oor_spent_txid FROM vtxo WHERE vtxo_id=$1 FOR UPDATE", &[&root.id().to_string()]).await?;
		ensure!(root.expiry_height() <= view.captaind.height && root.expiry_height() <= view.watchmand.height,
			"expiry split backing is not mature");
		let Some(txid) = row.get::<_, Option<&str>>("oor_spent_txid") else { return Ok(false); };
		let txid = txid.parse()?;
		let tx = self.get_virtual_transaction_by_txid(txid).await?.context("expiry split transaction missing")?
			.signed_tx.context("expiry split transaction is unsigned")?;
		ensure!(tx.input.len() == 1 && tx.input[0].previous_output == root.point(),
			"expiry split is not a single-input step on its backing path");
		ensure!(self.query_opt("SELECT i.vtxo_id FROM expiry_allocation_input i JOIN expiry_allocation a ON a.id=i.allocation_id WHERE i.vtxo_id=$1 AND a.status='pending' LIMIT 1", &[&root.id().to_string()]).await?.is_none(),
			"cannot split backing reserved for a pending payout");
		self.execute("INSERT INTO expiry_split_request(vtxo_id,progress_txid,generation) VALUES($1,$2,$3) ON CONFLICT(vtxo_id) DO UPDATE SET progress_txid=EXCLUDED.progress_txid,generation=EXCLUDED.generation",
			&[&root.id().to_string(),&txid.to_string(),&view.generation]).await?;
		Ok(true)
	}

	pub async fn pending_expiry_splits(&self) -> anyhow::Result<Vec<(bitcoin::Txid, ServerVtxo)>> {
		self.query("SELECT s.progress_txid,v.vtxo FROM expiry_split_request s JOIN vtxo v ON v.vtxo_id=s.vtxo_id WHERE v.onchain_spent_height IS NULL ORDER BY v.vtxo_id", &[]).await?
			.into_iter().map(|r| Ok((r.get::<_, &str>("progress_txid").parse()?,ServerVtxo::deserialize(r.get("vtxo"))?)))
			.collect()
	}
}

impl Tx<'_> {
	/// A reaction claim must not replace a financial payout already committed
	/// for its physical backing. Use the allocator's sorted coin lock order and
	/// keep these locks through broadcast, including when the root is not itself
	/// one of the economic customer terminals.
	pub async fn lock_unallocated_watchman_inputs(&self, ids: &[ark::VtxoId]) -> anyhow::Result<()> {
		super::expiry_guard::guard_obligation_changes(self, ids).await?;
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		ensure!(self.query_opt("SELECT vtxo_id FROM expiry_allocation_input WHERE vtxo_id=ANY($1) ORDER BY vtxo_id LIMIT 1", &[&ids]).await?.is_none(),
			"watchman claim backing is reserved for expiry payouts");
		Ok(())
	}
}
