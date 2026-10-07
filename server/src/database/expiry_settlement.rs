//! Durable transfer of an expired entitlement to the operator's payout service.

use ark::VtxoId;
use tokio_postgres::Row;

use crate::flux::VtxosInFlux;

use super::{Db, Tx};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SettlementVtxo {
	pub id: VtxoId,
	pub vtxo: Vec<u8>,
	pub expiry: u32,
}

impl SettlementVtxo {
	fn from_row(row: Row) -> anyhow::Result<Self> {
		Ok(Self {
			id: row.get::<_, &str>("vtxo_id").parse()?,
			vtxo: row.get("vtxo"),
			expiry: u32::try_from(row.get::<_, i32>("expiry"))?,
		})
	}
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClaimResult {
	Claimed(SettlementVtxo),
	Busy,
	Ineligible,
}

impl Db {
	/// Replay a stopped payout service's durable IDs before any server workers
	/// start. A missing entitlement or conflicting spend requires restoring
	/// captaind's history; payout IDs cannot reconstruct that history.
	pub async fn restore_expiry_settlements(&self, ids: &[VtxoId]) -> anyhow::Result<()> {
		self.write(async |tx| {
			for id in ids {
				let id = id.to_string();
				let row = tx.query_opt("
					SELECT policy_type = 'pubkey'
					  AND spend_state IN ('spendable', 'unclaimed', 'spent')
					  AND spent_in_round IS NULL AND oor_spent_txid IS NULL
					  AND offboarded_in IS NULL AND confirmed_height IS NULL AS eligible,
					  EXISTS (SELECT 1 FROM round_part_input i
					    JOIN round_participation p ON p.id = i.participation_id
					    WHERE i.vtxo_id = $1 AND p.forfeited_at IS NULL) AS unfinished
					FROM vtxo WHERE vtxo_id = $1 FOR UPDATE
				", &[&id]).await?
					.ok_or_else(|| anyhow::anyhow!("settlement replay: missing VTXO {id}; restore captaind history"))?;
				anyhow::ensure!(row.get::<_, Option<bool>>("eligible") == Some(true), "settlement replay: conflicting spend for {id}");
				anyhow::ensure!(!row.get::<_, bool>("unfinished"), "settlement replay: unfinished round for {id}");
				tx.execute("UPDATE vtxo SET spend_state = 'spent', updated_at = NOW()
					WHERE vtxo_id = $1 AND spend_state <> 'spent'", &[&id]).await?;
				tx.execute("INSERT INTO expiry_settlement (id) VALUES ($1) ON CONFLICT DO NOTHING", &[&id]).await?;
			}
			Ok(())
		}).await
	}

	/// Hold the same lock as refresh, arkoor and offboard until the commit is durable.
	pub(crate) async fn claim_expired_vtxo(
		&self, flux: &VtxosInFlux, id: VtxoId, tip: u32, grace: u32,
	) -> anyhow::Result<ClaimResult> {
		let Ok(_guard) = flux.try_lock([id]) else { return Ok(ClaimResult::Busy) };
		self.write(async |tx| tx.claim_expired_vtxo(id, tip, grace).await).await
	}
}

impl Tx<'_> {
	pub(crate) async fn expiry_settlement_page(
		&self, claimed: bool, tip: u32, grace: u32, minimum: u64,
		after: (u32, String), limit: u32,
	) -> anyhow::Result<Vec<SettlementVtxo>> {
		if claimed {
			// Receipt IDs alone are enough for reconciliation. Page their primary
			// key rather than repeatedly sorting the full VTXO history by expiry.
			let rows = self.query("SELECT id AS vtxo_id, ''::bytea AS vtxo, 0::integer AS expiry
				FROM expiry_settlement WHERE id > $1 ORDER BY id LIMIT $2",
				&[&after.1, &(limit as i64)]).await?;
			return rows.into_iter().map(SettlementVtxo::from_row).collect();
		}
		let rows = self.query("
			SELECT v.vtxo_id, v.vtxo, v.expiry FROM vtxo v
			WHERE (v.expiry::bigint, v.vtxo_id) > ($1::bigint, $2)
			  AND NOT EXISTS (SELECT 1 FROM expiry_settlement s WHERE s.id = v.vtxo_id)
			  AND v.policy_type = 'pubkey'
			  AND v.spend_state IN ('spendable', 'unclaimed')
			  AND v.confirmed_height IS NULL
			  AND v.expiry::bigint + $3::bigint <= $4::bigint
			  AND v.amount >= $5::bigint
			ORDER BY v.expiry, v.vtxo_id LIMIT $6::bigint
		", &[&(after.0 as i64), &after.1, &(grace as i64),
			&(tip as i64), &i64::try_from(minimum)?, &(limit as i64)]).await?;
		rows.into_iter().map(SettlementVtxo::from_row).collect()
	}

	async fn claim_expired_vtxo(
		&self, id: VtxoId, tip: u32, grace: u32,
	) -> anyhow::Result<ClaimResult> {
		// The row lock also serializes retries at the database boundary. A receipt
		// is sufficient after a lost RPC response; an ordinary spent coin is not.
		let id = id.to_string();
		let Some(row) = self.query_opt("
			SELECT v.vtxo_id, v.vtxo, v.expiry, s.id IS NOT NULL AS claimed
			FROM vtxo v LEFT JOIN expiry_settlement s ON s.id = v.vtxo_id
			WHERE v.vtxo_id = $1 FOR UPDATE OF v
		", &[&id]).await? else { return Ok(ClaimResult::Ineligible) };
		if row.get::<_, bool>("claimed") {
			return Ok(ClaimResult::Claimed(SettlementVtxo::from_row(row)?));
		}
		let row = self.query_opt("
			UPDATE vtxo SET spend_state = 'spent', updated_at = NOW()
			WHERE vtxo_id = $1 AND policy_type = 'pubkey'
			  AND spend_state IN ('spendable', 'unclaimed') AND confirmed_height IS NULL
			  AND expiry::bigint + $2::bigint <= $3::bigint
			  AND NOT EXISTS (
			    SELECT 1 FROM round_part_input i
			    JOIN round_participation p ON p.id = i.participation_id
			    WHERE i.vtxo_id = $1 AND p.forfeited_at IS NULL
			  )
			RETURNING vtxo_id, vtxo, expiry
		", &[&id, &(grace as i64), &(tip as i64)]).await?;
		let Some(row) = row else { return Ok(ClaimResult::Ineligible) };
		self.execute("INSERT INTO expiry_settlement (id) VALUES ($1)", &[&id]).await?;
		Ok(ClaimResult::Claimed(SettlementVtxo::from_row(row)?))
	}

	/// Return hints in request order, including missing outpoints. The sidecar
	/// must verify a spender spends this exact outpoint on this coin's exit path.
	pub(crate) async fn expiry_settlement_spenders(
		&self, ids: &[VtxoId],
	) -> anyhow::Result<Vec<Option<String>>> {
		let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		Ok(self.query("
			SELECT v.onchain_spent_txid
			FROM UNNEST($1::text[]) WITH ORDINALITY AS requested(id, position)
			LEFT JOIN vtxo v ON v.vtxo_id = requested.id ORDER BY requested.position
		", &[&ids]).await?.into_iter().map(|row| row.get(0)).collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ark::ServerVtxo;
	use ark::test_util::VTXO_VECTORS;
	use bitcoin::{hashes::Hash, Txid};
	use bitcoin_ext::BlockHeight;
	use crate::config::Postgres;
	use crate::database::{tree::VtxoTreeUpdate, SpendState};

	// Each ignored test creates a fresh database on the explicitly selected,
	// isolated Postgres. These are ledger tests, not a live Ark/Bitcoin journey.
	async fn database() -> (Db, Postgres) {
		let port = std::env::var("EXPIRY_TEST_POSTGRES_PORT")
			.expect("set EXPIRY_TEST_POSTGRES_PORT to an isolated Postgres port");
		let config = Postgres {
			host: "127.0.0.1".into(), port: port.parse().unwrap(),
			name: format!("expiry_{}", uuid::Uuid::new_v4().simple()),
			user: Some("postgres".into()), password: None,
			max_connections: 4, connection_timeout_secs: 10, idle_timeout_secs: 90,
		};
		let db = Db::create(&config).await.unwrap();
		db.write(async |tx| {
			tx.batch_execute(include_str!("../../../contrib/expiry-settlement.sql")).await?;
			Ok(())
		}).await.unwrap();
		(db, config)
	}

	async fn board(db: &Db) -> VtxoId {
		let vtxo = ServerVtxo::from(VTXO_VECTORS.board_vtxo.clone());
		let id = vtxo.id();
		db.write(async |tx| tx.execute_vtxo_tree_update(
			VtxoTreeUpdate::new().insert_unspent_vtxos([vtxo], SpendState::Spendable),
		).await).await.unwrap();
		id
	}

	async fn receipt_count(db: &Db) -> i64 {
		db.read(async |tx| Ok(tx.query_one("SELECT count(*) FROM expiry_settlement", &[])
			.await?.get(0))).await.unwrap()
	}

	async fn claim(db: &Db, flux: &VtxosInFlux, id: VtxoId) -> ClaimResult {
		db.claim_expired_vtxo(flux, id, 200_000, 144).await.unwrap()
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_receipt_pages_ignore_expiry_and_find_late_ids() {
		let (db, _) = database().await;
		let original = board(&db).await;
		// Synthetic aliases test receipt paging only; no payment is made.
		db.write(async |tx| {
			tx.execute("INSERT INTO vtxo (vtxo_id,vtxo,expiry,created_at,updated_at,
				vtxo_txid,exit_delta,policy_type,policy,server_pubkey,amount,anchor_point,spend_state)
				SELECT '10'||lpad(to_hex(n),62,'0')||':0',vtxo,258-n,NOW(),NOW(),
				vtxo_txid,exit_delta,policy_type,policy,server_pubkey,amount,anchor_point,'spent'::spend_state
				FROM vtxo CROSS JOIN generate_series(1,257) n WHERE vtxo_id=$1",
				&[&original.to_string()]).await?;
			tx.execute("INSERT INTO expiry_settlement(id) SELECT vtxo_id FROM vtxo
				WHERE vtxo_id <> $1 AND expiry <> 257", &[&original.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		let page = db.read(async |tx| tx.expiry_settlement_page(
			true, 0, 0, 0, (u32::MAX, String::new()), 256,
		).await).await.unwrap();
		assert_eq!(page.len(), 256);
		assert!(page.iter().all(|v| v.expiry == 0 && v.vtxo.is_empty()));
		assert!(page.windows(2).all(|w| w[0].id.to_string() < w[1].id.to_string()));
		let after = (0, page.last().unwrap().id.to_string());
		// A receipt commits after an ID cursor has passed its position.
		db.write(async |tx| {
			tx.execute("INSERT INTO expiry_settlement(id) SELECT vtxo_id FROM vtxo
				WHERE expiry=257 AND vtxo_id <> $1", &[&original.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		assert!(db.read(async |tx| tx.expiry_settlement_page(
			true, 0, 0, 0, after.clone(), 256,
		).await).await.unwrap().is_empty());
		let fresh = db.read(async |tx| tx.expiry_settlement_page(
			true, 0, 0, 0, (0, String::new()), 256,
		).await).await.unwrap();
		assert_eq!(fresh.len(), 256);
		assert!(fresh[0].id.to_string() < page[0].id.to_string());
		let tail = db.read(async |tx| tx.expiry_settlement_page(
			true, 0, 0, 0, (0, fresh.last().unwrap().id.to_string()), 256,
		).await).await.unwrap();
		assert_eq!(tail.len(), 1);
		assert_eq!(tail[0].id, page.last().unwrap().id);
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_startup_replay_is_atomic_and_idempotent() {
		let (db, _) = database().await;
		let id = board(&db).await;
		let missing = VtxoId::from_slice(&[8; 36]).unwrap();
		let err = db.restore_expiry_settlements(&[id, missing]).await.unwrap_err();
		assert!(format!("{err:#}").contains("missing VTXO"), "{err:#}");
		assert_eq!(receipt_count(&db).await, 0);
		let live = db.read(async |tx| tx.get_user_vtxos_by_id(&[id]).await).await.unwrap();
		assert!(live[0].check_spendable(BlockHeight::new(200_000)).is_ok());
		db.restore_expiry_settlements(&[id, id]).await.unwrap();
		db.restore_expiry_settlements(&[id]).await.unwrap();
		assert_eq!(receipt_count(&db).await, 1);
		assert!(matches!(claim(&db, &VtxosInFlux::new(), id).await, ClaimResult::Claimed(_)));
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_startup_replay_refuses_unfinished_or_conflicting_spend() {
		let (db, _) = database().await;
		let id = board(&db).await;
		db.write(async |tx| tx.try_store_round_participation(
			BlockHeight::new(200_000), [7; 32], &[id], &[], None,
		).await).await.unwrap();
		let err = db.restore_expiry_settlements(&[id]).await.unwrap_err();
		assert!(format!("{err:#}").contains("unfinished round"), "{err:#}");
		assert_eq!(receipt_count(&db).await, 0);
		db.write(async |tx| {
			tx.execute("UPDATE round_participation SET forfeited_at = NOW()", &[]).await?;
			tx.execute_vtxo_tree_update(VtxoTreeUpdate::new()
				.mark_vtxos_oor_spent([(id, Txid::from_byte_array([6; 32]))])).await
		}).await.unwrap();
		let err = db.restore_expiry_settlements(&[id]).await.unwrap_err();
		assert!(format!("{err:#}").contains("conflicting spend"), "{err:#}");
		assert_eq!(receipt_count(&db).await, 0);
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_receipt_survives_reconnect() {
		let (db, config) = database().await;
		let id = board(&db).await;
		let first = claim(&db, &VtxosInFlux::new(), id).await;
		assert!(matches!(first, ClaimResult::Claimed(_)));
		drop(db); // No client receipt is persisted before the reconnect.
		let db = Db::connect(&config).await.unwrap();
		assert_eq!(first, claim(&db, &VtxosInFlux::new(), id).await);
		assert_eq!(receipt_count(&db).await, 1);
		let page = db.read(async |tx| tx.expiry_settlement_page(
			true, 0, u32::MAX, u64::MAX >> 1, (0, String::new()), 256,
		).await).await.unwrap();
		assert_eq!(page.len(), 1);
		assert_eq!(page[0].id, id);
		assert!(page[0].vtxo.is_empty()); // Reconciliation need not re-fetch known histories.
		let live = db.read(async |tx| tx.get_user_vtxos_by_id(&[id]).await).await.unwrap();
		assert!(live[0].check_spendable(BlockHeight::new(200_000)).is_err());
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_user_lock_and_unfinished_round() {
		let (db, _) = database().await;
		let id = board(&db).await;
		let flux = VtxosInFlux::new();
		let user = flux.try_lock([id]).unwrap();
		assert_eq!(claim(&db, &flux, id).await, ClaimResult::Busy);
		assert_eq!(receipt_count(&db).await, 0);
		drop(user);
		// The actual delegated-registration DB operation creates the pending input.
		db.write(async |tx| tx.try_store_round_participation(
			BlockHeight::new(200_000), [9; 32], &[id], &[], None,
		).await).await.unwrap();
		assert_eq!(claim(&db, &flux, id).await, ClaimResult::Ineligible);
		db.write(async |tx| {
			tx.execute("UPDATE round_participation SET forfeited_at = NOW()", &[]).await?;
			Ok(())
		}).await.unwrap();
		assert!(matches!(claim(&db, &flux, id).await, ClaimResult::Claimed(_)));
		// A refresh registered after the handoff is refused by the existing code.
		assert!(db.write(async |tx| tx.try_store_round_participation(
			BlockHeight::new(200_000), [8; 32], &[id], &[], None,
		).await).await.is_err());
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_failed_receipt_rolls_back_spend() {
		let (db, _) = database().await;
		let id = board(&db).await;
		db.write(async |tx| {
			tx.batch_execute("
				CREATE FUNCTION refuse_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
				BEGIN RAISE EXCEPTION 'injected receipt failure'; END $$;
				CREATE TRIGGER refuse_receipt BEFORE INSERT ON expiry_settlement
				FOR EACH ROW EXECUTE FUNCTION refuse_receipt();
			").await?;
			Ok(())
		}).await.unwrap();
		assert!(db.claim_expired_vtxo(&VtxosInFlux::new(), id, 200_000, 144).await.is_err());
		assert_eq!(receipt_count(&db).await, 0);
		let live = db.read(async |tx| tx.get_user_vtxos_by_id(&[id]).await).await.unwrap();
		assert!(live[0].check_spendable(BlockHeight::new(200_000)).is_ok());
		db.write(async |tx| {
			tx.batch_execute("DROP TRIGGER refuse_receipt ON expiry_settlement").await?;
			Ok(())
		}).await.unwrap();
		assert!(matches!(claim(&db, &VtxosInFlux::new(), id).await, ClaimResult::Claimed(_)));
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_cancelled_claim_rolls_back_and_releases_lock() {
		let (db, _) = database().await;
		let id = board(&db).await;
		db.write(async |tx| {
			tx.batch_execute("
				CREATE FUNCTION slow_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
				BEGIN PERFORM pg_sleep(30); RETURN NEW; END $$;
				CREATE TRIGGER slow_receipt BEFORE INSERT ON expiry_settlement
				FOR EACH ROW EXECUTE FUNCTION slow_receipt();
			").await?;
			Ok(())
		}).await.unwrap();
		let flux = VtxosInFlux::new();
		let task_db = db.clone();
		let task_flux = flux.clone();
		let task = tokio::spawn(async move { claim(&task_db, &task_flux, id).await });
		// Observe the in-flight INSERT, then abort the request and cancel that
		// backend query so the test does not wait out the injected 30-second delay.
		let conn = db.raw_conn().await.unwrap();
		let pid = tokio::time::timeout(std::time::Duration::from_secs(5), async {
			loop {
				if let Some(row) = conn.query_opt("
					SELECT pid FROM pg_stat_activity WHERE datname = current_database()
					AND wait_event = 'PgSleep' AND pid <> pg_backend_pid()
				", &[]).await.unwrap() {
					break row.get::<_, i32>(0);
				}
				tokio::task::yield_now().await;
			}
		}).await.unwrap();
		task.abort();
		assert!(task.await.unwrap_err().is_cancelled());
		conn.execute("SELECT pg_cancel_backend($1)", &[&pid]).await.unwrap();
		drop(conn);
		let _guard = flux.try_lock([id]).unwrap();
		assert_eq!(receipt_count(&db).await, 0);
		let live = db.read(async |tx| tx.get_user_vtxos_by_id(&[id]).await).await.unwrap();
		assert!(live[0].check_spendable(BlockHeight::new(200_000)).is_ok());
		drop(_guard);
		tokio::time::timeout(std::time::Duration::from_secs(5), async {
			db.write(async |tx| {
				tx.batch_execute("DROP TRIGGER slow_receipt ON expiry_settlement").await?;
				Ok(())
			}).await.unwrap();
			assert!(matches!(claim(&db, &flux, id).await, ClaimResult::Claimed(_)));
		}).await.unwrap();
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_concurrent_retries() {
		let (db, _) = database().await;
		let id = board(&db).await;
		let flux = VtxosInFlux::new();
		let (a, b) = tokio::join!(claim(&db, &flux, id), claim(&db, &flux, id));
		assert!(matches!(a, ClaimResult::Claimed(_)) || matches!(b, ClaimResult::Claimed(_)));
		assert!(!matches!(a, ClaimResult::Ineligible));
		assert!(!matches!(b, ClaimResult::Ineligible));
		assert!(matches!(claim(&db, &flux, id).await, ClaimResult::Claimed(_)));
		assert_eq!(receipt_count(&db).await, 1);
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_competes_with_existing_arkoor_update() {
		let (db, _) = database().await;
		let id = board(&db).await;
		let flux = VtxosInFlux::new();
		let update = VtxoTreeUpdate::new()
			.mark_vtxos_oor_spent([(id, Txid::from_byte_array([6; 32]))]);
		let (payout, arkoor) = tokio::join!(
			claim(&db, &flux, id),
			db.write(async |tx| tx.execute_vtxo_tree_update(update).await),
		);
		assert_ne!(matches!(payout, ClaimResult::Claimed(_)), arkoor.is_ok());
		assert_eq!(receipt_count(&db).await, i64::from(arkoor.is_err()));
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_replay_after_restored_rows() {
		let (db, config) = database().await;
		let id = board(&db).await;
		let original = claim(&db, &VtxosInFlux::new(), id).await;
		// Simulate a consistent pre-handoff snapshot. This is not pg_restore.
		db.write(async |tx| {
			tx.execute("DELETE FROM expiry_settlement WHERE id = $1", &[&id.to_string()]).await?;
			tx.execute("UPDATE vtxo SET spend_state = 'spendable', updated_at = NOW()
				WHERE vtxo_id = $1", &[&id.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		drop(db);
		let db = Db::connect(&config).await.unwrap();
		assert_eq!(claim(&db, &VtxosInFlux::new(), id).await, original);
		assert_eq!(receipt_count(&db).await, 1);
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_eligibility_and_pagination() {
		let (db, _) = database().await;
		let id = board(&db).await;
		let expiry = VTXO_VECTORS.board_vtxo.expiry_height().to_u32();
		let flux = VtxosInFlux::new();
		assert_eq!(db.claim_expired_vtxo(&flux, id, expiry + 143, 144).await.unwrap(),
			ClaimResult::Ineligible);
		let page = db.read(async |tx| tx.expiry_settlement_page(
			false, 200_000, 144, 10_001, (0, String::new()), 1,
		).await).await.unwrap();
		assert!(page.is_empty());
		let page = db.read(async |tx| tx.expiry_settlement_page(
			false, 200_000, 144, 1, (0, String::new()), 1,
		).await).await.unwrap();
		assert_eq!(page.len(), 1);
		assert_eq!(page[0].id, id);
		let next = db.read(async |tx| tx.expiry_settlement_page(
			false, 200_000, 144, 1, (expiry, id.to_string()), 1,
		).await).await.unwrap();
		assert!(next.is_empty());
		db.write(async |tx| {
			tx.execute("UPDATE vtxo SET confirmed_height = 100, updated_at = NOW()", &[]).await?;
			Ok(())
		}).await.unwrap();
		assert_eq!(claim(&db, &flux, id).await, ClaimResult::Ineligible);
		db.write(async |tx| {
			tx.execute("UPDATE vtxo SET confirmed_height = NULL, spend_state = 'unclaimed',
				updated_at = NOW()", &[]).await?;
			Ok(())
		}).await.unwrap();
		assert!(matches!(claim(&db, &flux, id).await, ClaimResult::Claimed(_)));
	}

	#[tokio::test]
	#[ignore = "requires isolated EXPIRY_TEST_POSTGRES_PORT"]
	async fn expiry_settlement_spender_hints_keep_output_index() {
		let (db, _) = database().await;
		let id = board(&db).await;
		let mut sibling = id.to_bytes();
		sibling[35] ^= 1;
		let sibling = VtxoId::from_slice(&sibling).unwrap();
		let txid = Txid::from_byte_array([5; 32]).to_string();
		db.write(async |tx| {
			tx.execute("UPDATE vtxo SET onchain_spent_txid = $1, updated_at = NOW()",
				&[&txid]).await?;
			Ok(())
		}).await.unwrap();
		let hints = db.read(async |tx| tx.expiry_settlement_spenders(&[sibling, id, sibling]).await)
			.await.unwrap();
		assert_eq!(hints, vec![None, Some(txid), None]);
	}
}
