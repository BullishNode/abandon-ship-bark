//! Durable provenance for the existing fee owner, never an independent allowance.
use super::{AdvisoryLock, Tx};
use crate::watchman::ordinary_fee::FundingState;
use crate::watchman::ordinary_funding::{FundingReservation, FundingResolution};
use anyhow::Context;
use bitcoin::Transaction;
use bitcoin::consensus::{deserialize, serialize};

impl Tx<'_> {
	pub(crate) async fn lock_native_funding(&self) -> anyhow::Result<()> {
		self.query_one(
			&format!(
				"SELECT pg_advisory_xact_lock({})",
				AdvisoryLock::NativeFunding as i64
			),
			&[],
		)
		.await?;
		Ok(())
	}
	pub(crate) async fn native_funding_states(&self) -> anyhow::Result<Vec<FundingState>> {
		let bounds=self.query_one("SELECT count(*)::bigint,COALESCE(sum(octet_length(reservation::text)+COALESCE(octet_length(signed_tx),0)),0)::bigint FROM native_funding_fee_intent",&[]).await?;
		ensure!(
			bounds.get::<_, i64>(0) <= 4096 && bounds.get::<_, i64>(1) <= 4 * 1024 * 1024,
			"native funding DB inventory bound"
		);
		let mut result = Vec::new();
		for row in self.query("SELECT attempt_key,reservation::text,signed_tx FROM native_funding_fee_intent ORDER BY attempt_key",&[]).await? {
			let state = decode_funding_row(row.get(0), row.get(1), row.get::<_,Option<Vec<u8>>>(2).as_deref())?;
			let stored=self.query("SELECT outpoint FROM native_funding_fee_input WHERE attempt_key=$1 ORDER BY outpoint",&[&state.reservation.candidate.key().to_string()]).await?.into_iter().map(|r|r.get::<_,String>(0)).collect::<std::collections::BTreeSet<_>>();
			require_funding_inputs(&state, &stored)?;
			result.push(state);
		}
		Ok(result)
	}
	pub(crate) async fn stage_native_funding(
		&self,
		reservation: &FundingReservation,
	) -> anyhow::Result<()> {
		let old = self.native_funding_states().await?;
		let records = old
			.iter()
			.cloned()
			.map(|s| (s.reservation.candidate.key(), s))
			.collect::<std::collections::BTreeMap<_, _>>();
		let resolutions = self
			.native_funding_resolutions()
			.await?
			.into_iter()
			.map(|r| (r.winner_index, r))
			.collect::<std::collections::BTreeMap<_, _>>();
		let key = reservation.candidate.key().to_string();
		if let Some(state) = old
			.iter()
			.find(|s| s.reservation.candidate.key() == reservation.candidate.key())
		{
			ensure!(
				state.reservation == *reservation,
				"native funding DB attempt changed"
			);
			return Ok(());
		}
		ensure!(
			old.iter().all(|s| s.signed.is_none()
				|| s.reservation.candidate.family != reservation.candidate.family),
			"signed native DB family is closed"
		);
		for proof in resolutions.values() {
			crate::watchman::ordinary_fee::validate_resolution(proof, &records)?;
		}
		let points = reservation
			.candidate
			.transaction()?
			.input
			.into_iter()
			.map(|i| i.previous_output)
			.collect::<std::collections::BTreeSet<_>>();
		for state in old {
			if state
				.reservation
				.candidate
				.transaction()?
				.input
				.iter()
				.any(|i| points.contains(&i.previous_output))
				&& !crate::watchman::ordinary_fee::resolved_family(&state, &records, &resolutions)
			{
				ensure!(
					state.signed.is_none()
						&& reservation
							.candidate
							.follows_unsigned_round(&state.reservation.candidate),
					"native funding DB input belongs to another or signed family"
				);
			}
		}
		let value = serde_json::to_string(reservation)?;
		self.execute(
			"INSERT INTO native_funding_fee_intent(attempt_key,reservation)VALUES($1,$2::text::jsonb)",
			&[&key, &value],
		)
		.await?;
		for point in points {
			self.execute(
				"INSERT INTO native_funding_fee_input(outpoint,attempt_key)VALUES($1,$2)",
				&[&point.to_string(), &key],
			)
			.await?;
		}
		Ok(())
	}
	pub(crate) async fn bind_native_funding(
		&self,
		reservation: &FundingReservation,
		tx: &Transaction,
	) -> anyhow::Result<()> {
		reservation.candidate.verify_signed(tx)?;
		let states = self.native_funding_states().await?;
		let own = states
			.iter()
			.find(|s| s.reservation == *reservation)
			.context("native funding signed intent missing")?;
		if let Some(old) = &own.signed {
			ensure!(old == tx, "native funding signed DB retry changed");
			return Ok(());
		}
		for state in &states {
			if state.reservation.index != reservation.index
				&& state.reservation.candidate.family == reservation.candidate.family
			{
				ensure!(
					state.signed.is_none()
						&& state.reservation.candidate.generation
							< reservation.candidate.generation,
					"native funding competing signed/newer family"
				);
			}
		}
		self.execute(
			"UPDATE native_funding_fee_intent SET signed_tx=$2 WHERE attempt_key=$1",
			&[&reservation.candidate.key().to_string(), &serialize(tx)],
		)
		.await?;
		Ok(())
	}
	pub(crate) async fn require_native_funding_binding(
		&self,
		reservation: &FundingReservation,
		tx: &Transaction,
	) -> anyhow::Result<()> {
		self.lock_native_funding().await?;
		let state = self
			.native_funding_states()
			.await?
			.into_iter()
			.find(|s| s.reservation == *reservation)
			.context("native funding publication reservation missing")?;
		ensure!(
			state.signed.as_ref() == Some(tx),
			"native funding publication lacks original signed binding"
		);
		Ok(())
	}
	pub(crate) async fn require_native_funding_history(
		&self,
		history: &[FundingState],
		resolutions: &[FundingResolution],
	) -> anyhow::Result<()> {
		self.lock_native_funding().await?;
		let states = self.native_funding_states().await?;
		let actual = self.native_funding_resolutions().await?;
		require_funding_history(&states, &actual, history, resolutions)?;
		Ok(())
	}
	pub(crate) async fn native_funding_resolutions(
		&self,
	) -> anyhow::Result<Vec<FundingResolution>> {
		let rows=self.query("SELECT winner_index,proof::text FROM native_funding_fee_resolution ORDER BY winner_index LIMIT 4097",&[]).await?;
		ensure!(rows.len() <= 4096, "native funding resolution row bound");
		rows.into_iter()
			.map(|row| {
				let raw: &str = row.get(1);
				ensure!(raw.len() <= 1024, "native funding resolution byte bound");
				let proof: FundingResolution = serde_json::from_str(raw)?;
				ensure!(
					u64::try_from(row.get::<_, i64>(0))? == proof.winner_index,
					"native funding resolution index differs"
				);
				Ok(proof)
			})
			.collect()
	}
	pub(crate) async fn store_native_funding_resolution(
		&self,
		proof: &FundingResolution,
	) -> anyhow::Result<()> {
		let states = self
			.native_funding_states()
			.await?
			.into_iter()
			.map(|s| (s.reservation.candidate.key(), s))
			.collect();
		crate::watchman::ordinary_fee::validate_resolution(proof, &states)?;
		let index = i64::try_from(proof.winner_index)?;
		let raw = serde_json::to_string(proof)?;
		if let Some(row) = self
			.query_opt(
				"SELECT proof::text FROM native_funding_fee_resolution WHERE winner_index=$1",
				&[&index],
			)
			.await?
		{
			ensure!(
				serde_json::from_str::<FundingResolution>(row.get(0))? == *proof,
				"native funding resolution cannot change"
			);
			return Ok(());
		}
		self.execute(
			"INSERT INTO native_funding_fee_resolution(winner_index,proof)VALUES($1,$2::text::jsonb)",
			&[&index, &raw],
		)
		.await?;
		Ok(())
	}
}

/// Exact row checks shared with the offline private source codec.
pub(crate) fn decode_funding_row(key: &str, raw: &str, signed: Option<&[u8]>) -> anyhow::Result<FundingState> {
	let reservation: FundingReservation = serde_json::from_str(raw)?;
	ensure!(reservation.candidate.key().to_string() == key, "native funding DB key differs");
	reservation.fee()?;
	let signed = signed.map(|raw| -> anyhow::Result<Transaction> {
		let tx = deserialize(raw)?;
		ensure!(serialize(&tx) == raw, "noncanonical native signed DB bytes");
		reservation.candidate.verify_signed(&tx)?;
		Ok(tx)
	}).transpose()?;
	Ok(FundingState { reservation, signed })
}

pub(crate) fn require_funding_inputs(state: &FundingState, stored: &std::collections::BTreeSet<String>) -> anyhow::Result<()> {
	let points = state.reservation.candidate.transaction()?.input.into_iter()
		.map(|i| i.previous_output.to_string()).collect::<std::collections::BTreeSet<_>>();
	ensure!(*stored == points, "native funding input intent coverage differs");
	Ok(())
}

pub(crate) fn require_funding_history(states: &[FundingState], actual: &[FundingResolution], history: &[FundingState], resolutions: &[FundingResolution]) -> anyhow::Result<()> {
	ensure!(states.len() == history.len() && states.iter().all(|s| history.iter()
		.any(|h| h.reservation == s.reservation && h.signed == s.signed)),
		"native funding DB/independent ledger diverged; retain original input intents");
	ensure!(actual.len() == resolutions.len() && actual.iter().all(|r| resolutions.contains(r)),
		"native funding resolution DB/history diverged");
	Ok(())
}
