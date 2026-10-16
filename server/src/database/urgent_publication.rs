//! Authenticated attempt metadata on the existing signed nursery outbox.
//! This is not a second fee ledger; every debit belongs to the original file.

use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Transaction, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::sha256;
use bitcoin_ext::BlockHeight;

use crate::nursery::NurseryTxKind;
use crate::watchman::ordinary_fee::UrgentFeeAttempt;
use super::{Tx, carrier_snapshot::BackingSnapshotLimits};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrgentPublication {
	pub version: u32,
	pub policy_sha256: sha256::Hash,
	pub attempt_index: u64,
	pub ceiling_sat: u64,
	pub source_anchors: Vec<OutPoint>,
	pub txids: Vec<Txid>,
	/// Exact authenticated input outputs in transaction/input order.
	pub prevouts: Vec<Vec<TxOut>>,
}

impl UrgentPublication {
	pub fn attempt(&self) -> UrgentFeeAttempt {
		UrgentFeeAttempt { policy_sha256: self.policy_sha256, index: self.attempt_index }
	}

	pub fn validate(&self, txs: &[Transaction]) -> anyhow::Result<Amount> {
		ensure!(self.version == 1 && self.attempt_index > 0 && self.ceiling_sat > 0 &&
			self.ceiling_sat <= Amount::MAX_MONEY.to_sat(), "invalid urgent publication identity/ceiling");
		ensure!(!txs.is_empty() && txs.len() <= 2 && self.prevouts.len() == txs.len() &&
			self.txids == txs.iter().map(Transaction::compute_txid).collect::<Vec<_>>() &&
			self.txids.iter().collect::<BTreeSet<_>>().len() == txs.len(), "urgent package identity differs");
		ensure!(!self.source_anchors.is_empty() && self.source_anchors.iter().copied().collect::<BTreeSet<_>>()
			.into_iter().collect::<Vec<_>>() == self.source_anchors, "urgent source anchors are not canonical");
		let mut total_fee = Amount::ZERO;
		let mut total_weight = 0_u64;
		for (tx, prevouts) in txs.iter().zip(&self.prevouts) {
			ensure!(!tx.input.is_empty() && tx.input.len() == prevouts.len(), "urgent package prevouts are incomplete");
			let input = prevouts.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
				.filter(|v| *v <= Amount::MAX_MONEY).context("urgent input value overflow")?;
			let output = tx.output.iter().try_fold(Amount::ZERO, |sum, o| sum.checked_add(o.value))
				.context("urgent output value overflow")?;
			total_fee = total_fee.checked_add(input.checked_sub(output).context("urgent outputs exceed actual prevouts")?)
				.context("urgent package fee overflow")?;
			total_weight = total_weight.checked_add(tx.weight().to_wu()).context("urgent signed weight overflow")?;
		}
		ensure!(total_weight <= 400_000 && total_fee.to_sat() <= self.ceiling_sat,
			"urgent actual signed weight or prevout fee exceeds ceiling");
		Ok(total_fee)
	}
}

pub(crate) enum UrgentRelay {
	Unrelated,
	Unbound,
	Bound(Vec<Transaction>),
}

impl Tx<'_> {
	/// Bound the COMPLETE distinct package inventory before fetching any raw
	/// parent/member. Publication-row bytes alone omit shared signed parents.
	async fn check_urgent_package_bounds(&self, budget: BackingSnapshotLimits) -> anyhow::Result<()> {
		let metadata = self.query_one("SELECT count(*)::bigint AS rows,
			COALESCE(sum(octet_length(urgent_fee_attempt::text)),0)::bigint AS bytes
			FROM nursery_tx WHERE urgent_fee_attempt IS NOT NULL", &[]).await?;
		let metadata_bytes = usize::try_from(metadata.get::<_, i64>("bytes"))?;
		ensure!(usize::try_from(metadata.get::<_, i64>("rows"))? <= budget.max_rows &&
			metadata_bytes <= budget.max_serialized_bytes, "urgent metadata exceeds read bounds");
		ensure!(self.query_opt("SELECT id FROM nursery_tx WHERE urgent_fee_attempt IS NOT NULL AND
			(jsonb_typeof(urgent_fee_attempt->'txids') IS DISTINCT FROM 'array' OR
			jsonb_array_length(urgent_fee_attempt->'txids') NOT BETWEEN 1 AND 2) LIMIT 1", &[]).await?.is_none(),
			"urgent package membership is not bounded");
		let members = self.query_one("SELECT count(*)::bigint AS rows, COALESCE(sum(bytes),0)::bigint AS bytes,
			COALESCE(max(bytes),0)::bigint AS largest FROM (
			SELECT DISTINCT member.txid, octet_length(member.tx) AS bytes FROM nursery_tx publication
			CROSS JOIN LATERAL jsonb_array_elements_text(publication.urgent_fee_attempt->'txids') package(txid)
			JOIN nursery_tx member ON member.txid=package.txid WHERE publication.urgent_fee_attempt IS NOT NULL
			) complete_members", &[]).await?;
		let raw_bytes = usize::try_from(members.get::<_, i64>("bytes"))?;
		ensure!(usize::try_from(members.get::<_, i64>("rows"))? <= budget.max_rows &&
			usize::try_from(members.get::<_, i64>("largest"))? <= 400_000 &&
			metadata_bytes.checked_add(raw_bytes).is_some_and(|n| n <= budget.max_serialized_bytes),
			"complete urgent package bytes exceed read bounds");
		Ok(())
	}

	/// Called only after the SAME complete independent history binding is durable.
	pub(crate) async fn permit_bound_urgent_publication(&self, meta: &UrgentPublication) -> anyhow::Result<()> {
		let publication = meta.txids.last().context("urgent publication has no transaction")?;
		let expected = serde_json::to_value(meta)?;
		let row = self.query_one("SELECT urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE txid=$1 FOR UPDATE", &[&publication.to_string()]).await?;
		ensure!(serde_json::from_str::<serde_json::Value>(row.get::<_, Option<&str>>("attempt").context("urgent metadata missing")?)? == expected,
			"urgent permission differs from original durable attempt");
		self.execute("UPDATE nursery_tx SET urgent_fee_bound=TRUE WHERE txid=$1", &[&publication.to_string()]).await?;
		Ok(())
	}

	pub(crate) async fn urgent_publication_bound(&self, meta: &UrgentPublication) -> anyhow::Result<bool> {
		Ok(self.query_one("SELECT urgent_fee_bound FROM nursery_tx WHERE txid=$1", &[&meta.txids.last()
			.context("urgent package empty")?.to_string()]).await?.get("urgent_fee_bound"))
	}

	/// Every member, including a NULL-metadata shared parent, is gated through
	/// the immutable publication's package IDs. An older bound package may still
	/// relay its original parent while a new replacement remains unbound.
	pub(crate) async fn urgent_relay(&self, txid: Txid) -> anyhow::Result<UrgentRelay> {
		self.check_urgent_package_bounds(crate::payout_witness::DIRECT_HISTORY_LIMITS).await?;
		let rows = self.query("SELECT urgent_fee_attempt::text AS attempt,urgent_fee_bound FROM nursery_tx
			WHERE urgent_fee_attempt->'txids' ? $1 ORDER BY id DESC LIMIT 10001", &[&txid.to_string()]).await?;
		ensure!(rows.len() <= 10000, "urgent relay membership exceeds bound");
		if rows.is_empty() { return Ok(UrgentRelay::Unrelated); }
		for row in rows {
			if !row.get::<_, bool>("urgent_fee_bound") { continue; }
			let meta: UrgentPublication = serde_json::from_str(row.get("attempt"))?;
			let mut txs = Vec::new();
			for id in &meta.txids {
				let raw = self.get_nursery_raw_tx(*id).await?.context("bound urgent package member missing")?;
				txs.push(raw);
			}
			meta.validate(&txs)?;
			return Ok(UrgentRelay::Bound(txs));
		}
		Ok(UrgentRelay::Unbound)
	}


	pub(crate) async fn store_urgent_publication(&self, meta: &UrgentPublication, txs: &[Transaction], target: BlockHeight) -> anyhow::Result<()> {
		meta.validate(txs)?;
		let mut sources = Vec::new();
		for (input, expected) in txs[0].input.iter().zip(&meta.prevouts[0]) {
			let source = self.get_bare_vtxo_by_id(input.previous_output.into()).await?.vtxo;
			ensure!(source.txout() == *expected, "urgent source prevout differs from authenticated original row");
			sources.push(source);
		}
		ensure!(sources.iter().map(|v| v.chain_anchor()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>() == meta.source_anchors,
			"urgent physical source association differs");
		self.lock_unfrozen_urgent_sources(&sources).await?;
		let value = serde_json::to_value(meta)?;
		for (position, tx) in txs.iter().enumerate() {
			let publication = position + 1 == txs.len();
			self.upsert_nursery_tx_with_attempt(tx, NurseryTxKind::Internal, target, publication.then_some(&value)).await?;
			let row = self.query_one("SELECT tx,kind::text,urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE txid=$1 AND octet_length(tx)<=400000",
				&[&tx.compute_txid().to_string()]).await?;
			ensure!(row.get::<_, Vec<u8>>("tx") == serialize(tx) && row.get::<_, &str>("kind") == "internal" &&
				if publication {
					serde_json::from_str::<serde_json::Value>(row.get::<_, Option<&str>>("attempt").context("original nursery publication has no urgent metadata; relabel refused")?)? == value
				} else { row.get::<_, Option<&str>>("attempt").is_none() }, "urgent retained outbox conflicts with original attempt");
		}
		Ok(())
	}

	pub(crate) async fn urgent_publications(&self, budget: BackingSnapshotLimits) -> anyhow::Result<Vec<(UrgentPublication, Vec<Transaction>)>> {
		self.check_urgent_package_bounds(budget).await?;
		let mut result = Vec::new();
		for row in self.query("SELECT DISTINCT urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE urgent_fee_attempt IS NOT NULL ORDER BY attempt", &[]).await? {
			let text: &str = row.get("attempt"); let meta: UrgentPublication = serde_json::from_str(text)?;
			let mut txs = Vec::new();
			for (position, txid) in meta.txids.iter().enumerate() {
				let stored = self.query_opt("SELECT tx,kind::text,urgent_fee_attempt::text AS attempt FROM nursery_tx WHERE txid=$1", &[&txid.to_string()]).await?
					.context("urgent signed package member missing")?;
				ensure!(stored.get::<_, &str>("kind") == "internal" && if position + 1 == meta.txids.len() {
						serde_json::from_str::<UrgentPublication>(stored.get::<_, Option<&str>>("attempt").context("urgent publication metadata missing")?)? == meta
					} else { stored.get::<_, Option<&str>>("attempt").is_none() },
					"urgent package members have conflicting attempt identity");
				txs.push(deserialize(&stored.get::<_, Vec<u8>>("tx"))?);
			}
			meta.validate(&txs)?;
			let mut anchors = BTreeSet::new();
			for (input, prevout) in txs[0].input.iter().zip(&meta.prevouts[0]) {
				let source = self.get_bare_vtxo_by_id(input.previous_output.into()).await?.vtxo;
				ensure!(source.txout() == *prevout, "retained urgent source prevout differs");
				anchors.insert(source.chain_anchor());
			}
			ensure!(anchors.into_iter().collect::<Vec<_>>() == meta.source_anchors, "retained urgent source anchors differ");
			result.push((meta, txs));
		}
		Ok(result)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bitcoin::{absolute, transaction, ScriptBuf, Sequence, TxIn, Witness};
	use bitcoin::hashes::Hash;

	fn actual_package() -> (UrgentPublication, Vec<Transaction>) {
		let point = OutPoint { txid: Txid::from_byte_array([7; 32]), vout: 0 };
		let tx = Transaction { version: transaction::Version::TWO, lock_time: absolute::LockTime::ZERO,
			input: vec![TxIn { previous_output: point, script_sig: ScriptBuf::new(), sequence: Sequence::MAX, witness: Witness::new() }],
			output: vec![TxOut { value: Amount::from_sat(9_000), script_pubkey: ScriptBuf::new() }] };
		let meta = UrgentPublication { version: 1, policy_sha256: sha256::Hash::from_byte_array([9;32]), attempt_index: 1,
			ceiling_sat: 1_000, source_anchors: vec![point], txids: vec![tx.compute_txid()],
			prevouts: vec![vec![TxOut { value: Amount::from_sat(10_000), script_pubkey: ScriptBuf::new() }]] };
		(meta, vec![tx])
	}

	#[test]
	fn urgent_publication_charges_actual_prevouts_not_a_supplied_fee() {
		let (mut meta, txs) = actual_package();
		assert_eq!(meta.validate(&txs).unwrap(), Amount::from_sat(1_000));
		meta.ceiling_sat = 999;
		assert!(meta.validate(&txs).is_err());
		meta.ceiling_sat = 1_000;
		meta.prevouts[0][0].value = Amount::from_sat(9_999);
		assert_eq!(meta.validate(&txs).unwrap(), Amount::from_sat(999));
		// Prevout ownership is separately authenticated against original source
		// rows/Core by storage; this pure reducer never claims that proof.
	}

	#[test]
	fn urgent_publication_refuses_missing_prevout_and_changed_transaction() {
		let (meta, mut txs) = actual_package();
		txs[0].output[0].value = Amount::from_sat(8_999);
		assert!(meta.validate(&txs).is_err());
		let (mut meta, txs) = actual_package();
		meta.prevouts[0].clear();
		assert!(meta.validate(&txs).is_err());
	}

	#[test]
	fn urgent_publication_refuses_output_inflation_and_noncanonical_anchors() {
		let (mut meta, mut txs) = actual_package();
		txs[0].output[0].value = Amount::from_sat(10_001);
		meta.txids = txs.iter().map(Transaction::compute_txid).collect();
		assert!(meta.validate(&txs).is_err());
		let (mut meta, txs) = actual_package();
		meta.source_anchors.push(meta.source_anchors[0]);
		assert!(meta.validate(&txs).is_err());
	}
	/// PostgreSQL/witness seam only. The library's public dummy keys and
	/// synthetic anchor do not represent funded coins or a chain outcome.
	#[tokio::test]
	#[ignore = "requires an explicitly isolated Unix-socket PostgreSQL control fixture"]
	async fn urgent_pg_nursery_first_gap_exact_restart_and_reciprocal_overlap() {
		use ark::test_util::dummy::{DummyTestVtxoSpec, DUMMY_SERVER_KEY, DUMMY_USER_KEY};
		use ark::vtxo::Full;
		use ark::vtxo::policy::signing::VtxoSigner;
		use bitcoin::{Network, sighash};
		use crate::database::Db;
		use crate::payout_witness::{DirectPayoutWitness, DIRECT_DIRECTORY, DIRECT_HISTORY_LIMITS};
		use crate::watchman::{WatchmanSigner, ordinary_fee::{OrdinaryFeePolicy, OrdinaryFeeBudget}};
		use crate::secret::Secret;

		let path = std::path::PathBuf::from(std::env::var_os("ARK_TEST_URGENT_PG_CONFIG")
			.expect("explicit isolated PG configuration is required"));
		let cfg: crate::config::Postgres = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
		assert!(cfg.host.starts_with('/') && std::path::Path::new(&cfg.host).is_dir());
		assert_eq!(cfg.name, "urgent_publication_control");
		let directory = path.parent().unwrap().join("urgent-control-history");
		std::fs::create_dir(&directory).unwrap(); // create-new only; never reset.
		let db = Db::create(&cfg).await.unwrap();
		DirectPayoutWitness::initialize(&directory, Network::Regtest, DUMMY_SERVER_KEY.public_key(), &db).await.unwrap();
		let policy = OrdinaryFeePolicy::default();
		policy.initialize_fresh(&directory.join(DIRECT_DIRECTORY), Network::Regtest, DUMMY_SERVER_KEY.public_key()).unwrap();
		let budget = OrdinaryFeeBudget::load(&directory.join(DIRECT_DIRECTORY), policy).unwrap();
		// This PG-only fixture must actually own the ordinary policy key it signs.
		let (_, full) = DummyTestVtxoSpec { amount: Amount::from_sat(20_000), fee: Amount::ZERO,
			user_keypair: *DUMMY_SERVER_KEY, ..Default::default() }.build();
		let source: ark::ServerVtxo<Full> = full.into();
		db.write(async |t| t.upsert_vtxos([&source]).await).await.unwrap();
		let bare = source.to_bare();
		assert_eq!(bare.policy().user_pubkey(), Some(DUMMY_SERVER_KEY.public_key()));
		let signer = WatchmanSigner::new(Secret::new(*DUMMY_SERVER_KEY), Secret::new(*DUMMY_USER_KEY), db.clone());
		let clause = signer.find_signable_clause(&bare).await.unwrap();
		let mut tx = Transaction { version: transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::from_height(10_001).unwrap(),
			input: vec![TxIn { previous_output: bare.point(), script_sig: ScriptBuf::new(),
				sequence: clause.sequence().unwrap_or(Sequence::ZERO), witness: Witness::new() }],
			output: vec![TxOut { value: Amount::from_sat(19_000), script_pubkey:
				ScriptBuf::new_p2tr(&ark::SECP, DUMMY_SERVER_KEY.x_only_public_key().0, None) }] };
		let outputs = [bare.txout()]; let prevouts = sighash::Prevouts::All(&outputs);
		let mut cache = sighash::SighashCache::new(&mut tx);
		let witness = signer.sign_input_with_clause(&bare, &clause, 0, &mut cache, &prevouts).await.unwrap();
		cache.witness_mut(0).unwrap().clone_from(&witness);
		assert!(!tx.input[0].witness.is_empty());
		let attempt = budget.reserve_urgent_ceiling(Amount::from_sat(2_000)).unwrap();
		let meta = UrgentPublication { version: 1, policy_sha256: attempt.policy_sha256, attempt_index: attempt.index,
			ceiling_sat: 2_000, source_anchors: vec![bare.chain_anchor()], txids: vec![tx.compute_txid()],
			prevouts: vec![vec![bare.txout()]] };
		let txs = vec![tx]; let actual_fee = meta.validate(&txs).unwrap();
		assert_eq!(actual_fee, Amount::from_sat(1_000));
		db.write(async |t| t.store_urgent_publication(&meta, &txs, BlockHeight::new(10_001)).await).await.unwrap();
		assert!(matches!(db.read(async |t| t.urgent_relay(meta.txids[0]).await).await.unwrap(), UrgentRelay::Unbound));
		// Genuine DB-first/p-gap: cap-side witness validation sees the original
		// charged reservation, and the generic nursery cannot relay these bytes.
		DirectPayoutWitness::load(&directory, Network::Regtest, DUMMY_SERVER_KEY.public_key(), &db, DIRECT_HISTORY_LIMITS).await.unwrap();
		let binding = sha256::Hash::hash(&serde_json::to_vec(&meta).unwrap());
		budget.bind_urgent_signed(attempt, &txs, actual_fee, binding).unwrap();
		db.write(async |t| t.permit_bound_urgent_publication(&meta).await).await.unwrap();
		let retained = db.read(async |t| t.urgent_publications(DIRECT_HISTORY_LIMITS).await).await.unwrap();
		assert_eq!(retained, vec![(meta.clone(), txs.clone())]);
		assert!(matches!(db.read(async |t| t.urgent_relay(meta.txids[0]).await).await.unwrap(),
			UrgentRelay::Bound(ref actual) if actual == &txs));
		let history_path = directory.join(DIRECT_DIRECTORY).join("ordinary-fee-budget");
		let original_history = std::fs::read(&history_path).unwrap();
		budget.bind_urgent_signed(attempt, &txs, actual_fee, binding).unwrap();
		assert_eq!(std::fs::read(&history_path).unwrap(), original_history);
		drop(budget);
		let reopened = Db::connect(&cfg).await.unwrap();
		let budget = OrdinaryFeeBudget::load(&directory.join(DIRECT_DIRECTORY), policy).unwrap();
		assert_eq!(budget.urgent_reserve().unwrap(), Amount::from_sat(13_000));
		DirectPayoutWitness::load(&directory, Network::Regtest, DUMMY_SERVER_KEY.public_key(), &reopened, DIRECT_HISTORY_LIMITS).await.unwrap();
		assert_eq!(reopened.read(async |t| t.urgent_publications(DIRECT_HISTORY_LIMITS).await).await.unwrap(), retained);
		// Reciprocal freeze guard sees the actual retained signed input before
		// allocating any source. No artificial paid/settled record is inserted.
		let held = reopened.write(async |t| {
			t.lock_htlc_settlements().await?; t.lock_expiry_carriers().await?;
			let snapshot = t.backing_snapshot(std::slice::from_ref(&bare), DIRECT_HISTORY_LIMITS).await?;
			t.refuse_retained_urgent_overlap(&snapshot, DIRECT_HISTORY_LIMITS).await
		}).await.unwrap_err();
		assert!(held.to_string().contains("expiry freeze overlaps retained urgent signed inputs"), "unexpected guard failure: {held:#}");
		// A separate empty historical DB models DB-only rollback without deleting
		// any original nursery row, reservation, witness or signed byte.
		let mut old_cfg = cfg.clone(); old_cfg.name = "urgent_publication_control_old".into();
		let old_db = Db::create(&old_cfg).await.unwrap();
		let refused = DirectPayoutWitness::load(&directory, Network::Regtest, DUMMY_SERVER_KEY.public_key(), &old_db, DIRECT_HISTORY_LIMITS).await.err().expect("DB-only rollback was accepted");
		assert!(refused.to_string().contains("retained urgent signed intent missing from restored nursery"), "unexpected restore failure: {refused:#}");
		// Missing independent budget is tested against an ACTUAL populated DB.
		// Copy only the original immutable identity/empty direct history into a
		// separate directory; the real complete budget/journal remains untouched.
		let prebudget = path.parent().unwrap().join("prebudget-identity-copy");
		std::fs::create_dir(&prebudget).unwrap(); std::fs::create_dir(prebudget.join(DIRECT_DIRECTORY)).unwrap();
		for name in ["identity.json", "history"] {
			std::fs::copy(directory.join(DIRECT_DIRECTORY).join(name), prebudget.join(DIRECT_DIRECTORY).join(name)).unwrap();
		}
		let missing = DirectPayoutWitness::load(&prebudget, Network::Regtest, DUMMY_SERVER_KEY.public_key(), &reopened, DIRECT_HISTORY_LIMITS).await.err().expect("urgent metadata without independent budget was accepted");
		assert!(missing.to_string().contains("urgent DB metadata requires the missing independent budget"), "unexpected missing-budget failure: {missing:#}");
		assert_eq!(std::fs::read(history_path).unwrap(), original_history);
		println!("actual PG nursery-first gap, exact replay/reconnect and retained-input freeze refusal; chain/funding/automatic routing NOT qualified");
	}

}
