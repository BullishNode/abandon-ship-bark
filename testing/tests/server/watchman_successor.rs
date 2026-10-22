use std::num::NonZeroUsize;
use std::time::Duration;

use bitcoin::{absolute, transaction, Amount, FeeRate, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid};
use bitcoin::consensus::serialize;
use bitcoin::hashes::{sha256, Hash};

use ark::{ServerVtxo, VtxoPolicy, VtxoRequest};
use ark::lightning::Preimage;
use ark::test_util::build_signed_tree;
use ark::test_util::dummy::{DUMMY_SERVER_KEY, DUMMY_USER_KEY};
use ark::tree::signed::HashlockVersion;
use ark::vtxo::Full;
use ark::vtxo::policy::ServerHtlcSend_v0_VtxoPolicy;
use ark_testing::{sat, TestContext};
use bitcoin_ext::{BlockDelta, BlockHeight};
use server::database::Db;
use server::watchman::{Action, ActionContextFetcher, Config};

fn config() -> Config {
	Config {
		reaction_interval: Duration::from_secs(1),
		sweep_interval: Duration::from_secs(1),
		progress_grace_period: BlockDelta::new(6),
		claim_chunksize: NonZeroUsize::new(10).unwrap(),
		incremental_relay_fee: FeeRate::from_sat_per_vb(1).unwrap(),
		min_cpfp_amount: sat(1_000),
		expiry_fee_warning: sat(10_000),
	}
}

/// These synthetic VTXOs exercise the actual policy fetcher and database decoder.
/// They do not establish claim authorization, chain settlement, or monetary safety.
fn sources(amount: Amount, preimage: Preimage) -> Vec<ServerVtxo<Full>> {
	let key = DUMMY_USER_KEY.public_key();
	let hash = preimage.compute_payment_hash();
	let expiry = BlockHeight::new(500);
	let policies = [
		VtxoPolicy::new_server_htlc_send(key, hash, expiry),
		VtxoPolicy::ServerHtlcSend_v0(ServerHtlcSend_v0_VtxoPolicy {
			user_pubkey: key, payment_hash: hash, htlc_expiry: expiry,
		}),
	];
	let (tree, _) = build_signed_tree(HashlockVersion::V1,
		policies.into_iter().map(|policy| VtxoRequest { amount, policy }),
		&DUMMY_USER_KEY, &DUMMY_SERVER_KEY, &DUMMY_SERVER_KEY,
		sha256::Hash::hash(&[3; 32]),
	);
	tree.into_cached_tree().output_vtxos().map(ServerVtxo::from).collect()
}

fn successor(source: &ServerVtxo<Full>, connector: Option<Txid>, amount: Amount) -> Transaction {
	let mut input = vec![TxIn { previous_output: source.point(), ..Default::default() }];
	if let Some(txid) = connector {
		input.push(TxIn { previous_output: OutPoint::new(txid, 0), ..Default::default() });
	}
	Transaction {
		version: transaction::Version::TWO,
		lock_time: absolute::LockTime::ZERO,
		input,
		output: vec![TxOut {
			value: amount,
			script_pubkey: ScriptBuf::from_hex(
				"51201234123412341234123412341234123412341234123412341234123412341234",
			).unwrap(),
		}],
	}
}

async fn store_successor(db: &Db, source: &ServerVtxo<Full>, tx: &Transaction, signed: bool) {
	let txid = tx.compute_txid().to_string();
	let bytes = signed.then(|| serialize(tx));
	db.write(async |t| {
		t.execute("INSERT INTO virtual_transaction(txid, signed_tx, is_funding, created_at, updated_at)
			VALUES($1, $2, false, NOW(), NOW())
			ON CONFLICT(txid) DO UPDATE SET signed_tx = EXCLUDED.signed_tx",
			&[&txid, &bytes]).await?;
		t.execute("UPDATE vtxo SET oor_spent_txid = $1 WHERE vtxo_id = $2",
			&[&txid, &source.id().to_string()]).await?;
		Ok(())
	}).await.unwrap();
}

/// Fail the selected real query once. A temporary sequence survives rollback
/// and permits `nextval` in the read-only transaction. Use one pooled connection.
/// The view preserves the real table, constraints and rows. The projection runs
/// only after the query selects its txid, so dependency failures stay distinct.
async fn fail_next_transaction_read(db: &Db, txid: Txid) {
	db.write(async |t| {
		t.batch_execute(&format!("
			ALTER TABLE virtual_transaction RENAME TO fixture_virtual_transaction;
			CREATE TEMP SEQUENCE fixture_successor_read;
			CREATE FUNCTION fixture_successor_bytes(id text, bytes bytea) RETURNS bytea
			LANGUAGE plpgsql VOLATILE AS $$
			BEGIN
				IF id = '{}' AND nextval('fixture_successor_read') = 1 THEN
					RAISE EXCEPTION 'injected successor query failure';
				END IF;
				RETURN bytes;
			END $$;
			CREATE VIEW virtual_transaction AS
			SELECT txid, fixture_successor_bytes(txid, signed_tx) AS signed_tx, is_funding
			FROM fixture_virtual_transaction;
		", txid)).await?;
		Ok(())
	}).await.unwrap();
}

async fn restore_transaction_table(db: &Db) {
	db.write(async |t| {
		let row = t.query_one("SELECT last_value, is_called FROM fixture_successor_read", &[]).await?;
		assert!(row.get::<_, bool>("is_called"), "the targeted query must reach its injected error");
		assert_eq!(row.get::<_, i64>("last_value"), 2, "the same query must recover on its second read");
		t.batch_execute("DROP VIEW virtual_transaction;
			ALTER TABLE fixture_virtual_transaction RENAME TO virtual_transaction;
			DROP FUNCTION fixture_successor_bytes(text, bytea);
			DROP SEQUENCE fixture_successor_read;").await?;
		Ok(())
	}).await.unwrap();
}

#[tokio::test]
async fn htlc_send_fetcher_holds_unknown_successors_and_recovers() {
	let mut ctx = TestContext::new_minimal("server/htlc_send_successor_lookup").await;
	ctx.init_central_postgres().await;
	ctx.init_central_bitcoind().await;
	let mut pg = ctx.new_postgres(&ctx.test_name).await;
	pg.max_connections = 1;
	Db::create(&pg).await.unwrap();
	let db = Db::connect(&pg).await.unwrap();
	let preimage = Preimage::from([43; 32]);
	db.write(async |t| t.store_htlc_settlement(preimage).await).await.unwrap();
	let config = config();
	let rpc = ctx.bitcoind().async_client();
	let fetcher = ActionContextFetcher {
		config: &config, db: &db, bitcoind: &rpc, chain_tip_height: BlockHeight::new(300),
	};
	let confirmed = Some(BlockHeight::new(100));
	let claim = Action::Claim { deadline: BlockHeight::new(148) };
	for source in sources(sat(1_000), preimage) {
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "missing VTXO is unknown");
		db.write(async |t| t.upsert_vtxos([&source]).await).await.unwrap();
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, claim, "healthy absence permits the original claim");
		db.write(async |t| {
			t.batch_execute("ALTER TABLE vtxo RENAME COLUMN vtxo TO fixture_vtxo").await?;
			Ok(())
		}).await.unwrap();
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "VTXO query failure is unknown");
		db.write(async |t| {
			t.batch_execute("ALTER TABLE vtxo RENAME COLUMN fixture_vtxo TO vtxo").await?;
			Ok(())
		}).await.unwrap();
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, claim);
		let tx = successor(&source, None, sat(1_000));
		let txid = tx.compute_txid();
		store_successor(&db, &source, &tx, false).await;
		fail_next_transaction_read(&db, txid).await;
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "one failed successor query cannot claim");
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "recovery preserves unsigned hold");
		restore_transaction_table(&db).await;
		assert_eq!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(preimage.compute_payment_hash()).await)
			.await.unwrap(), Some(preimage), "a readable preimage must not bypass the hold");
		db.write(async |t| {
			t.execute("DELETE FROM virtual_transaction WHERE txid = $1", &[&txid.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "missing linked row is unknown");
		store_successor(&db, &source, &tx, false).await;
		db.write(async |t| {
			t.execute("UPDATE virtual_transaction SET signed_tx = $1 WHERE txid = $2",
				&[&vec![0u8], &txid.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "invalid signed bytes are unknown");
		store_successor(&db, &source, &tx, true).await;
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await,
			Action::Progress { txid, deadline: BlockHeight::new(148) });

		let parent = successor(&source, None, sat(999));
		let parent_txid = parent.compute_txid();
		let child = successor(&source, Some(parent_txid), sat(1_000));
		store_successor(&db, &source, &child, true).await;
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "missing dependency is unknown");
		// Store the dependency without changing the child's spend link.
		db.write(async |t| {
			t.execute("INSERT INTO virtual_transaction(txid, signed_tx, is_funding, created_at, updated_at)
				VALUES($1, $2, false, NOW(), NOW())", &[&parent_txid.to_string(), &serialize(&parent)]).await?;
			Ok(())
		}).await.unwrap();
		fail_next_transaction_read(&db, parent_txid).await;
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "dependency query failure is unknown");
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await,
			Action::Progress { txid: parent_txid, deadline: BlockHeight::new(148) }, "dependency query recovers");
		restore_transaction_table(&db).await;
		for bytes in [None, Some(vec![0u8])] {
			db.write(async |t| {
				t.execute("UPDATE virtual_transaction SET signed_tx = $1 WHERE txid = $2",
					&[&bytes, &parent_txid.to_string()]).await?;
				Ok(())
			}).await.unwrap();
			assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await,
				Action::Wait, "an unsigned or malformed dependency cannot authorize a claim");
		}
		db.write(async |t| {
			t.execute("UPDATE virtual_transaction SET signed_tx = $1 WHERE txid = $2",
				&[&serialize(&parent), &parent_txid.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		let rejected_rpc = bitcoind_async_client::Client::new(ctx.bitcoind().rpc_url(),
			bitcoind_async_client::Auth::UserPass("rejected-fixture".into(), "rejected-fixture".into()),
			Some(0), None, Some(1),
		).unwrap();
		let failed_rpc_fetcher = ActionContextFetcher { bitcoind: &rejected_rpc, ..fetcher };
		assert_eq!(failed_rpc_fetcher.get_action(&source.to_bare(), confirmed).await, Action::Wait, "RPC failure is unknown");
		assert_eq!(fetcher.get_action(&source.to_bare(), confirmed).await,
			Action::Progress { txid: parent_txid, deadline: BlockHeight::new(148) });
	}
}

#[tokio::test]
async fn htlc_send_fetcher_dust_filter_does_not_establish_absence() {
	let mut ctx = TestContext::new_minimal("server/htlc_send_successor_dust").await;
	ctx.init_central_postgres().await;
	ctx.init_central_bitcoind().await;
	let pg = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&pg).await.unwrap();
	let db = Db::connect(&pg).await.unwrap();
	let preimage = Preimage::from([44; 32]);
	db.write(async |t| t.store_htlc_settlement(preimage).await).await.unwrap();
	let config = config();
	let rpc = ctx.bitcoind().async_client();
	let fetcher = ActionContextFetcher {
		config: &config, db: &db, bitcoind: &rpc, chain_tip_height: BlockHeight::new(300),
	};
	for source in sources(sat(648), preimage) {
		db.write(async |t| t.upsert_vtxos([&source]).await).await.unwrap();
		assert_eq!(fetcher.get_action(&source.to_bare(), Some(BlockHeight::new(100))).await,
			Action::Claim { deadline: BlockHeight::new(148) }, "healthy absence control");
		let tx = successor(&source, None, sat(308));
		store_successor(&db, &source, &tx, true).await;
		assert_eq!(fetcher.get_action(&source.to_bare(), Some(BlockHeight::new(100))).await,
			Action::Wait, "filtered dust is an existing successor");
	}
}
