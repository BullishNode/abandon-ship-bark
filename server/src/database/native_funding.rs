//! Durable provenance for the existing fee owner, never an independent allowance.
use anyhow::Context;
use bitcoin::Transaction;
use bitcoin::consensus::{deserialize, serialize};
use crate::watchman::ordinary_funding::FundingReservation;
use crate::watchman::ordinary_fee::FundingState;
use super::{Tx, AdvisoryLock};

impl Tx<'_> {
	pub(crate) async fn lock_native_funding(&self) -> anyhow::Result<()> {
		self.query_one(&format!("SELECT pg_advisory_xact_lock({})",AdvisoryLock::NativeFunding as i64),&[]).await?;Ok(())
	}
	pub(crate) async fn native_funding_states(&self) -> anyhow::Result<Vec<FundingState>> {
		let bounds=self.query_one("SELECT count(*)::bigint,COALESCE(sum(octet_length(reservation::text)+COALESCE(octet_length(signed_tx),0)),0)::bigint FROM native_funding_fee_intent",&[]).await?;
		ensure!(bounds.get::<_,i64>(0)<=4096 && bounds.get::<_,i64>(1)<=4*1024*1024,"native funding DB inventory bound");
		let mut result=Vec::new();
		for row in self.query("SELECT attempt_key,reservation::text,signed_tx FROM native_funding_fee_intent ORDER BY attempt_key",&[]).await? {
			let reservation:FundingReservation=serde_json::from_str(row.get(1))?;
			ensure!(reservation.candidate.key().to_string()==row.get::<_,String>(0),"native funding DB key differs");
			reservation.fee()?;
			let signed=row.get::<_,Option<Vec<u8>>>(2).map(|raw| -> anyhow::Result<Transaction> { let tx=deserialize(&raw)?;ensure!(serialize(&tx)==raw,"noncanonical native signed DB bytes");reservation.candidate.verify_signed(&tx)?;Ok(tx) }).transpose()?;
			let points=reservation.candidate.transaction()?.input.into_iter().map(|i|i.previous_output.to_string()).collect::<std::collections::BTreeSet<_>>();
			let stored=self.query("SELECT outpoint FROM native_funding_fee_input WHERE attempt_key=$1 ORDER BY outpoint",&[&reservation.candidate.key().to_string()]).await?.into_iter().map(|r|r.get::<_,String>(0)).collect::<std::collections::BTreeSet<_>>();
			ensure!(stored==points,"native funding input intent coverage differs");
			result.push(FundingState {reservation,signed});
		}
		Ok(result)
	}
	pub(crate) async fn stage_native_funding(&self,reservation:&FundingReservation)->anyhow::Result<()> {
		let old=self.native_funding_states().await?;
		let key=reservation.candidate.key().to_string();
		if let Some(state)=old.iter().find(|s|s.reservation.candidate.key()==reservation.candidate.key()) {
			ensure!(state.reservation==*reservation,"native funding DB attempt changed");return Ok(());
		}
		let points=reservation.candidate.transaction()?.input.into_iter().map(|i|i.previous_output).collect::<std::collections::BTreeSet<_>>();
		for state in old {if state.reservation.candidate.transaction()?.input.iter().any(|i|points.contains(&i.previous_output)) {
			ensure!(state.signed.is_none() && reservation.candidate.follows_unsigned_round(&state.reservation.candidate),"native funding DB input belongs to another or signed family");
		}}
		let value=serde_json::to_value(reservation)?;
		self.execute("INSERT INTO native_funding_fee_intent(attempt_key,reservation)VALUES($1,$2)",&[&key,&value]).await?;
		for point in points {self.execute("INSERT INTO native_funding_fee_input(outpoint,attempt_key)VALUES($1,$2)",&[&point.to_string(),&key]).await?;}
		Ok(())
	}
	pub(crate) async fn bind_native_funding(&self,reservation:&FundingReservation,tx:&Transaction)->anyhow::Result<()> {
		reservation.candidate.verify_signed(tx)?;let states=self.native_funding_states().await?;
		let own=states.iter().find(|s|s.reservation==*reservation).context("native funding signed intent missing")?;
		if let Some(old)=&own.signed {ensure!(old==tx,"native funding signed DB retry changed");return Ok(());}
		for state in &states {if state.reservation.index!=reservation.index && state.reservation.candidate.family==reservation.candidate.family {
			ensure!(state.signed.is_none() && state.reservation.candidate.generation<reservation.candidate.generation,"native funding competing signed/newer family");
		}}
		self.execute("UPDATE native_funding_fee_intent SET signed_tx=$2 WHERE attempt_key=$1",&[&reservation.candidate.key().to_string(),&serialize(tx)]).await?;Ok(())
	}
	pub(crate) async fn require_native_funding_binding(&self,reservation:&FundingReservation,tx:&Transaction)->anyhow::Result<()> {
		self.lock_native_funding().await?;
		let state=self.native_funding_states().await?.into_iter().find(|s|s.reservation==*reservation).context("native funding publication reservation missing")?;
		ensure!(state.signed.as_ref()==Some(tx),"native funding publication lacks original signed binding");Ok(())
	}
	pub(crate) async fn require_native_funding_history(&self,history:&[FundingState])->anyhow::Result<()> {
		self.lock_native_funding().await?;let states=self.native_funding_states().await?;
		ensure!(states.len()==history.len() && states.iter().all(|s|history.iter().any(|h|h.reservation==s.reservation && h.signed==s.signed)),"native funding DB/independent ledger diverged; retain original input intents");Ok(())
	}
}
