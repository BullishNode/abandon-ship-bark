//! Replay only an already-canonical, unchanged user timeout authorization.
//! Core still decides current input availability, signatures, fees and relay.

use anyhow::Context;
use ark::{ProtocolEncoding, ServerVtxo};
use bitcoin::{Amount, Transaction, Txid};
use bitcoin::consensus::deserialize;
use bitcoin_ext::TxStatus;
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use tracing::{info, warn};

use crate::bitcoind as bcd;
use crate::bitcoind::BitcoindErrorExt;
use crate::database::expiry_chain::ExpiryViewToken;
use super::Watchman;

const REPLAY_BATCH: i64 = 16;

impl Watchman {
	pub(crate) async fn replay_user_timeout_claims(&self, view: ExpiryViewToken) -> anyhow::Result<()> {
		// Service bookkeeping is deliberately outside the financial revision
		// trigger. One permanently blocked batch cannot monopolize every pass.
		let pending=self.db.write(async |t| {
			let rows=t.query("SELECT txid,raw_tx FROM expiry_user_claim_tx WHERE confirmed_height IS NULL \
				ORDER BY last_replay_attempt_at NULLS FIRST,txid LIMIT $1 FOR UPDATE", &[&REPLAY_BATCH]).await?;
			let mut pending=Vec::new();
			for row in rows {
				let txid:Txid=row.get::<_,&str>("txid").parse()?;
				let claim:Transaction=deserialize(row.get("raw_tx"))?;
				ensure!(claim.compute_txid()==txid,"retained user claim raw identity differs");
				t.execute("UPDATE expiry_user_claim_tx SET last_replay_attempt_at=clock_timestamp() WHERE txid=$1",
					&[&txid.to_string()]).await?;
				pending.push(claim);
			}
			Ok(pending)
		}).await?;
		for claim in pending {
			let txid=claim.compute_txid();
			if let Err(error)=self.replay_user_timeout_claim(view,&claim).await {
				warn!(%txid,error=%format_args!("{error:#}"),"Retained user timeout replay remains pending");
			}
		}
		Ok(())
	}

	async fn replay_user_timeout_claim(&self, view: ExpiryViewToken, claim: &Transaction) -> anyhow::Result<()> {
		if matches!(bcd::tx_status(&self.bitcoind,claim.compute_txid()).await?,TxStatus::Confirmed(_)) {
			// Only the normal completed canonical rebuild publishes confirmation.
			return Ok(());
		}
		match self.bitcoind.call_raw::<serde_json::Value>("getmempoolentry", &[bcd::json_arg(claim.compute_txid())?]).await {
			Ok(_) => return Ok(()),
			Err(error) if error.is_not_found() => {},
			Err(error) => return Err(error.into()),
		}
		self.verify_expiry_view(view).await?;
		self.db.write(async |t| {
			t.lock_valid_expiry_view(view).await?;
			t.guard_user_claim_replay(claim).await?;
			let linked: Vec<ServerVtxo>=t.query("SELECT v.vtxo FROM expiry_user_claim_source s \
				JOIN vtxo v ON v.vtxo_id=s.source_vtxo_id WHERE s.claim_txid=$1",
				&[&claim.compute_txid().to_string()]).await?.into_iter()
				.map(|row|ServerVtxo::deserialize(row.get("vtxo"))).collect::<Result<Vec<_>,_>>()?;
			let mut total=Amount::ZERO;
			for input in &claim.input {
				let prevout=self.bitcoind.try_get_tx_out(input.previous_output,true).await?
					.context("retained user claim input is absent or already spent")?;
				ensure!(prevout.confirmations>0,"retained user claim input is not confirmed");
				for source in linked.iter().filter(|source|source.point()==input.previous_output) {
					ensure!(prevout.value==source.amount()
						&& prevout.script_pub_key.script().context("invalid user claim prevout script")?==source.txout().script_pubkey,
						"retained user claim source differs from actual canonical prevout");
				}
				total=total.checked_add(prevout.value).context("user claim input value overflow")?;
			}
			let paid=claim.output.iter().try_fold(Amount::ZERO,|sum,out|sum.checked_add(out.value).context("user claim output value overflow"))?;
			let actual_fee=total.checked_sub(paid).context("retained user claim outputs exceed actual inputs")?;
			let acceptance=bcd::test_mempool_accept(&self.bitcoind,&[claim]).await?;
			ensure!(acceptance.len()==1 && acceptance[0].txid==claim.compute_txid()
				&& acceptance[0].allowed,"Core did not admit retained user claim: {acceptance:?}");
			ensure!(acceptance[0].fees.as_ref().is_some_and(|fees|fees.base==actual_fee),
				"Core user claim fee differs from actual canonical inputs");
			self.verify_expiry_view(view).await?;
			bcd::broadcast_tx(&self.bitcoind,claim).await?;
			info!(txid=%claim.compute_txid(),fee_sat=actual_fee.to_sat(),"Rebroadcast exact retained user timeout claim");
			// Broadcasting is not payment. No financial or chain fact changes.
			Ok(())
		}).await
	}
}
