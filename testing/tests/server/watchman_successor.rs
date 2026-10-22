use std::time::Duration;

use bitcoin::{absolute, transaction, Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid};
use bitcoin::hashes::{sha256, Hash};

use ark::{ServerVtxo, VtxoPolicy, VtxoRequest};
use ark::lightning::Preimage;
use ark::test_util::build_signed_tree;
use ark::test_util::dummy::{DUMMY_SERVER_KEY, DUMMY_USER_KEY};
use ark::tree::signed::HashlockVersion;
use ark::vtxo::Full;
use ark::vtxo::policy::ServerHtlcSend_v0_VtxoPolicy;
use ark_testing::{sat, TestContext};
use bitcoin_ext::BlockHeight;
use server::database::{Db, Tx};
use server::database::tree::VtxoTreeUpdate;

/// These synthetic VTXOs exercise the policy fetcher and database decoder.
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

async fn wait_for_backend_lock(t: &Tx<'_>, pid: i32) -> anyhow::Result<()> {
	tokio::time::timeout(Duration::from_secs(10), async {
		loop {
			let waiting = t.query_one("
				SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND NOT granted)
			", &[&pid]).await?.get::<_, bool>(0);
			if waiting { return Ok::<_, anyhow::Error>(()); }
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	}).await.expect("competing operation must reach the actual source lock")
}

fn claim_sources() -> Vec<ServerVtxo<Full>> {
	let mut result = sources(sat(1_000), Preimage::from([45; 32]));
	let (tree, _) = build_signed_tree(HashlockVersion::V1,
		[VtxoRequest {
			amount: sat(1_000), policy: VtxoPolicy::new_pubkey(DUMMY_USER_KEY.public_key()),
		}], &DUMMY_USER_KEY, &DUMMY_SERVER_KEY, &DUMMY_SERVER_KEY,
		sha256::Hash::hash(&[4; 32]),
	);
	result.extend(tree.into_cached_tree().output_vtxos().map(ServerVtxo::from));
	result
}

/// These controls exercise the same guard and tree writer as the claim path.
/// Synthetic sources and successor bytes are not chain or signature proof.
#[tokio::test]
async fn watchman_claim_fence_rechecks_after_a_committed_successor() {
	let mut ctx = TestContext::new_minimal("server/watchman_claim_successor_wins").await;
	ctx.init_central_postgres().await;
	let pg = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&pg).await.unwrap();
	let db = Db::connect(&pg).await.unwrap();
	for source in claim_sources() {
		db.write(async |t| t.upsert_vtxos([&source]).await).await.unwrap();
		let bare = source.to_bare();
		let next = successor(&source, None, sat(1_000));
		let txid = next.compute_txid();
		let claiming_db = db.clone();
		let claiming_source = bare.clone();
		let (pid_send, pid_recv) = tokio::sync::oneshot::channel();
		let claim = db.write(async |t| {
			// This is an accepted real writer while the source is unconfirmed.
			t.execute_vtxo_tree_update(VtxoTreeUpdate::new()
				.upsert_unsigned_tx([txid]).mark_vtxos_oor_spent([(source.id(), txid)])).await?;
			t.register_vtxo_confirmation(source.id(), BlockHeight::new(100)).await?;
			let claim = tokio::spawn(async move {
				claiming_db.write(async |tx| {
					let pid = tx.query_one("SELECT pg_backend_pid()", &[]).await?.get::<_, i32>(0);
					pid_send.send(pid).expect("lock observer remains alive");
					tx.lock_unallocated_watchman_inputs(&[claiming_source], true).await
				}).await
			});
			wait_for_backend_lock(t, pid_recv.await.unwrap()).await?;
			Ok(claim)
		}).await.unwrap();
		let error = claim.await.unwrap().expect_err("committed unsigned successor must refuse a stale claim");
		assert!(format!("{error:#}").contains("watchman claim source has a successor"), "{error:#}");
		let state = db.read(async |t| t.get_server_vtxo_by_id(source.id()).await).await.unwrap();
		assert_eq!(state.oor_spent_txid, Some(txid));
		assert_eq!(state.confirmed_height, Some(BlockHeight::new(100)));
		// Registration can complete without releasing the source's claim fence.
		db.write(async |t| t.execute_vtxo_tree_update(VtxoTreeUpdate::new()
			.upsert_signed_tx([next])).await).await.unwrap();
		let error = db.write(async |t| t.lock_unallocated_watchman_inputs(std::slice::from_ref(&bare), true).await).await
			.expect_err("a signed successor also takes priority over a claim");
		assert!(format!("{error:#}").contains("watchman claim source has a successor"), "{error:#}");
		for bytes in [Some(vec![0u8]), None] {
			db.write(async |t| {
				t.execute("UPDATE virtual_transaction SET signed_tx=$1, updated_at=NOW() WHERE txid=$2",
					&[&bytes, &txid.to_string()]).await?;
				Ok(())
			}).await.unwrap();
			let error = db.write(async |t| t.lock_unallocated_watchman_inputs(std::slice::from_ref(&bare), true).await).await
				.expect_err("unusable successor bytes must not authorize a claim");
			assert!(format!("{error:#}").contains("watchman claim source has a successor"), "{error:#}");
		}
		db.write(async |t| {
			t.execute("DELETE FROM virtual_transaction WHERE txid=$1", &[&txid.to_string()]).await?;
			Ok(())
		}).await.unwrap();
		let error = db.write(async |t| t.lock_unallocated_watchman_inputs(&[bare], true).await).await
			.expect_err("a missing linked transaction must not authorize a claim");
		assert!(format!("{error:#}").contains("watchman claim source has a successor"), "{error:#}");
	}
}

#[tokio::test]
async fn watchman_claim_fence_retains_confirmation_against_a_queued_writer() {
	let mut ctx = TestContext::new_minimal("server/watchman_claim_lock_wins").await;
	ctx.init_central_postgres().await;
	let pg = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&pg).await.unwrap();
	let db = Db::connect(&pg).await.unwrap();
	for source in claim_sources() {
		db.write(async |t| {
			t.upsert_vtxos([&source]).await?;
			t.register_vtxo_confirmation(source.id(), BlockHeight::new(100)).await
		}).await.unwrap();
		let bare = source.to_bare();
		let competing_db = db.clone();
		let id = source.id();
		let next = successor(&source, None, sat(1_000));
		let txid = next.compute_txid();
		let (pid_send, pid_recv) = tokio::sync::oneshot::channel();
		let writer = db.write(async |t| {
			t.lock_unallocated_watchman_inputs(std::slice::from_ref(&bare), true).await?;
			let writer = tokio::spawn(async move {
				competing_db.write(async |tx| {
					let pid = tx.query_one("SELECT pg_backend_pid()", &[]).await?.get::<_, i32>(0);
					pid_send.send(pid).expect("lock observer remains alive");
					tx.execute_vtxo_tree_update(VtxoTreeUpdate::new()
						.upsert_unsigned_tx([txid]).mark_vtxos_oor_spent([(id, txid)])).await
				}).await
			});
			wait_for_backend_lock(t, pid_recv.await.unwrap()).await?;
			// The successful authorization remains protected until commit.
			t.lock_unallocated_watchman_inputs(std::slice::from_ref(&bare), true).await?;
			Ok(writer)
		}).await.unwrap();
		let error = writer.await.unwrap().expect_err("fresh successor cannot spend a confirmed source after claim commit");
		assert!(format!("{error:#}").contains("has exited onchain"), "{error:#}");
		let state = db.read(async |t| t.get_server_vtxo_by_id(source.id()).await).await.unwrap();
		assert_eq!(state.oor_spent_txid, None);
		assert_eq!(state.confirmed_height, Some(BlockHeight::new(100)));
		assert!(db.read(async |t| t.get_virtual_transaction_by_txid(txid).await).await.unwrap().is_none(),
			"the refused writer's unsigned transaction must roll back");
		db.write(async |t| t.lock_unallocated_watchman_inputs(std::slice::from_ref(&bare), true).await).await.unwrap();
		// A reorg between scan and authorization must invalidate the claim.
		db.write(async |t| t.reorg_frontier(BlockHeight::new(99)).await).await.unwrap();
		let error = db.write(async |t| t.lock_unallocated_watchman_inputs(&[bare], true).await).await
			.expect_err("a formerly confirmed source cannot use its stale scan");
		assert!(format!("{error:#}").contains("watchman claim source is not confirmed and unspent"), "{error:#}");
	}
}
