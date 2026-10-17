use std::collections::BTreeMap;

use ark::fallback::FallbackRecord;
use ark::mailbox::MailboxIdentifier;
use ark::SECP;
use ark_testing::TestContext;
use bitcoin::bip32::{ChildNumber, Xpriv, Xpub};
use bitcoin::secp256k1::{Keypair, PublicKey};
use bitcoin::{Network, ScriptBuf};
use server::database::expiry_attribution::{Attribution, OWNER_KEYSPACE_END};
use server::database::Db;

pub(super) fn key(account: &Xpriv, index: u32) -> PublicKey {
	Xpub::from_priv(&SECP, account).ckd_pub(&SECP, ChildNumber::from_normal_idx(index).unwrap()).unwrap().public_key
}

pub(super) async fn register(db: &Db, byte: u8) -> (Xpriv, MailboxIdentifier) {
	let account = Xpriv::new_master(Network::Regtest, &[byte; 64]).unwrap();
	let mailbox = Keypair::from_seckey_slice(&SECP, &[byte; 32]).unwrap();
	let spk = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
	let record = FallbackRecord::new(Network::Regtest, spk, &account, &mailbox).unwrap();
	db.write(async |tx| tx.set_fallback(mailbox.public_key(), &record, Network::Regtest).await).await.unwrap();
	(account, MailboxIdentifier::from_pubkey(mailbox.public_key()))
}

async fn owner(db: &Db, key: PublicKey) -> Attribution {
	db.read(async |tx| tx.expiry_owner(key, Network::Regtest).await).await.unwrap()
}

#[tokio::test]
async fn expiry_attribution_batches_keys_and_recovers_late_lower_keys_after_restart() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_batches_keys_and_recovers_late_lower_keys_after_restart").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_late_keys").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let (account, mailbox) = register(&db, 3).await;
	let keys = [key(&account, 17), key(&account, 25)];
	for key in keys { db.write(async |tx| tx.queue_expiry_owner(key, Some(mailbox)).await).await.unwrap(); }
	let stats = db.scan_expiry_owners(Network::Regtest, 32, 2).await.unwrap();
	assert_eq!(stats.children_derived, 32, "one pass compares every pending key for this record");
	assert_eq!(stats.keys_matched, 2);
	for key in keys { assert!(matches!(owner(&db, key).await, Attribution::Owned { .. })); }
	let remaining = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_scan", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(remaining, 0, "actual matches release their cross-record cursor storage");
	drop(db);
	let db = Db::connect(&config).await.unwrap();
	let late = key(&account, 8);
	db.write(async |tx| tx.queue_expiry_owner(late, Some(mailbox)).await).await.unwrap();
	assert_eq!(owner(&db, late).await, Attribution::Unresolved);
	assert_eq!(db.scan_expiry_owners(Network::Regtest, 32, 2).await.unwrap().children_derived, 32);
	assert!(matches!(owner(&db, late).await, Attribution::Owned { child_index: 8, .. }));
}

async fn coverage(db: &Db, key: PublicKey) -> BTreeMap<PublicKey, i64> {
	db.read(async |tx| {
		Ok(tx.query("SELECT mailbox_pk, scanned_up_to FROM expiry_scan WHERE user_pubkey=$1",
			&[&key.to_string()]).await?.into_iter().map(|row|
			(row.get::<_, &str>("mailbox_pk").parse().unwrap(), row.get("scanned_up_to"))).collect())
	}).await.unwrap()
}

#[tokio::test]
async fn expiry_attribution_requeue_preserves_coverage_without_reinitializing_accounts() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_requeue_coverage").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_requeue_coverage").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let (account, mailbox) = register(&db, 21).await;
	let old = key(&account, 80);
	let future_account = Xpriv::new_master(Network::Regtest, &[22; 64]).unwrap();
	let future = key(&future_account, 7);
	for key in [old, future] {
		db.write(async |tx| tx.queue_expiry_owner(key, None).await).await.unwrap();
	}
	db.scan_expiry_owners(Network::Regtest, 32, 1).await.unwrap();
	let initial = BTreeMap::from([(mailbox.as_pubkey(), 32)]);
	assert_eq!(coverage(&db, old).await, initial);
	assert_eq!(coverage(&db, future).await, initial);

	db.write(async |tx| {
		// An already initialized key must not access the cross-account cursor
		// table. This real relation lock blocks the old INSERT...ON CONFLICT
		// path even when every proposed cursor would already exist.
		tx.execute("LOCK TABLE expiry_scan IN ACCESS EXCLUSIVE MODE", &[]).await?;
		let queued = db.clone();
		tokio::time::timeout(std::time::Duration::from_secs(5), async move {
			queued.write(async |tx| {
				for _ in 0..3 {
					tx.queue_expiry_owner(old, Some(mailbox)).await?;
					tx.queue_expiry_owner(future, None).await?;
				}
				Ok(())
			}).await
		}).await.expect("requeue must not reinitialize cross-account cursors")?;
		Ok(())
	}).await.unwrap();
	assert_eq!(coverage(&db, old).await, initial);
	assert_eq!(coverage(&db, future).await, initial);

	let (_, new_mailbox) = register(&db, 22).await;
	let appended = BTreeMap::from([(mailbox.as_pubkey(), 32), (new_mailbox.as_pubkey(), 0)]);
	assert_eq!(coverage(&db, old).await, appended);
	assert_eq!(coverage(&db, future).await, appended);
	// A later different hint cannot replace the first nonempty candidate or
	// reset either account's coverage. It is never an ownership assertion.
	db.write(async |tx| tx.queue_expiry_owner(old, Some(new_mailbox)).await).await.unwrap();
	let candidate = db.read(async |tx| Ok(tx.query_one(
		"SELECT mailbox_pk FROM expiry_pending_key WHERE user_pubkey=$1", &[&old.to_string()],
	).await?.get::<_, String>(0))).await.unwrap();
	assert_eq!(candidate, mailbox.as_pubkey().to_string());
	assert_eq!(coverage(&db, old).await, appended);

	let work = db.prepare_expiry_owner_scan(32, 2).await.unwrap();
	let late = key(&account, 8);
	db.write(async |tx| tx.queue_expiry_owner(late, None).await).await.unwrap();
	let zero = BTreeMap::from([(mailbox.as_pubkey(), 0), (new_mailbox.as_pubkey(), 0)]);
	assert_eq!(coverage(&db, late).await, zero);
	for work in work { db.commit_expiry_owner_scan(Network::Regtest, &work.derive().unwrap()).await.unwrap(); }
	assert_eq!(coverage(&db, late).await, zero, "older work must not advance a late key");
	assert!(matches!(owner(&db, future).await, Attribution::Owned { mailbox_pk, child_index: 7, .. }
		if mailbox_pk == new_mailbox.as_pubkey()));
	for _ in 0..3 { db.scan_expiry_owners(Network::Regtest, 32, 2).await.unwrap(); }
	assert!(matches!(owner(&db, late).await, Attribution::Owned { mailbox_pk, child_index: 8, .. }
		if mailbox_pk == mailbox.as_pubkey()));
	assert!(matches!(owner(&db, old).await, Attribution::Owned { mailbox_pk, child_index: 80, .. }
		if mailbox_pk == mailbox.as_pubkey()));
	// A cached match is not queued again, even when another record arrives.
	db.write(async |tx| tx.queue_expiry_owner(old, Some(new_mailbox)).await).await.unwrap();
	register(&db, 23).await;
	assert!(coverage(&db, old).await.is_empty());
}

#[tokio::test]
async fn expiry_attribution_duplicate_cursors_do_not_starve_other_accounts() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_duplicate_cursors").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_duplicate_cursors").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let mut wanted = Vec::new();
	for byte in [11, 12, 13] {
		let (account, mailbox) = register(&db, byte).await;
		for index in [17, 25] {
			let key = key(&account, index);
			db.write(async |tx| tx.queue_expiry_owner(key, None).await).await.unwrap();
			wanted.push((key, mailbox, index));
		}
	}
	let mut derived = 0;
	// The bounded scheduler may select one account more than once from its
	// first cursor rows. It must batch that work, then reach every other
	// account without re-deriving a prefix or stranding its pending keys.
	for _ in 0..3 {
		derived += db.scan_expiry_owners(Network::Regtest, 32, 2).await.unwrap().children_derived;
	}
	assert_eq!(derived, 3 * 32);
	for (key, mailbox, index) in wanted {
		assert!(matches!(owner(&db, key).await, Attribution::Owned { mailbox_pk, child_index, .. }
			if mailbox_pk==mailbox.as_pubkey() && child_index==index));
	}
}

#[tokio::test]
async fn expiry_attribution_match_serializes_with_requeue_and_registration() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_match_registry_race").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_match_registry_race").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let (account, mailbox) = register(&db, 3).await;
	let wanted = key(&account, 17);
	db.write(async |tx| tx.queue_expiry_owner(wanted, None).await).await.unwrap();
	let work = db.prepare_expiry_owner_scan(32, 1).await.unwrap().remove(0).derive().unwrap();
	let (commit, queue, registration) = db.write(async |tx| {
		// Stop the real commit at its cursor write, after it has acquired the
		// registry writer fence. No coverage or match is fabricated.
		tx.query_one("SELECT scanned_up_to FROM expiry_scan WHERE user_pubkey=$1 FOR UPDATE",
			&[&wanted.to_string()]).await?;
		let scan_db = db.clone();
		let commit = tokio::spawn(async move {
			scan_db.commit_expiry_owner_scan(Network::Regtest, &work).await.unwrap()
		});
		tokio::time::timeout(std::time::Duration::from_secs(5), async {
			loop {
				tx.query_one("SELECT pg_stat_clear_snapshot()", &[]).await?;
				let waiting = tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity
					WHERE datname=current_database() AND wait_event_type='Lock'
					AND query LIKE 'UPDATE expiry_scan SET scanned_up_to=%')", &[]).await?.get::<_, bool>(0);
				if waiting { return Ok::<_, anyhow::Error>(()); }
				tokio::time::sleep(std::time::Duration::from_millis(10)).await;
			}
		}).await.expect("owner commit never reached its real cursor lock")?;
		let queue_db = db.clone();
		let queue = tokio::spawn(async move {
			queue_db.write(async |tx| tx.queue_expiry_owner(wanted, Some(mailbox)).await).await.unwrap()
		});
		let registration_db = db.clone();
		let registration = tokio::spawn(async move { register(&registration_db, 7).await });
		tokio::time::timeout(std::time::Duration::from_secs(5), async {
			loop {
				let waiting = tx.query_one("SELECT COUNT(*) FROM pg_locks WHERE locktype='advisory'
					AND database=(SELECT oid FROM pg_database WHERE datname=current_database())
					AND objid=3 AND NOT granted", &[]).await?.get::<_, i64>(0);
				if waiting == 2 { return Ok::<_, anyhow::Error>(()); }
				tokio::time::sleep(std::time::Duration::from_millis(10)).await;
			}
		}).await.expect("requeue and registration must wait behind the actual match")?;
		Ok((commit, queue, registration))
	}).await.unwrap();
	assert_eq!(commit.await.unwrap(), 1);
	queue.await.unwrap();
	let (_, new_mailbox) = registration.await.unwrap();
	assert!(db.read(async |tx| tx.get_fallback(new_mailbox.as_pubkey()).await).await.unwrap().is_some());
	assert!(matches!(owner(&db, wanted).await, Attribution::Owned { child_index: 17, .. }));
	let queued = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_pending_key", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(queued, 0, "a concurrent queue must not recreate a resolved key and strand other work");
}

#[tokio::test]
async fn expiry_attribution_prepared_work_never_advances_a_new_pending_key() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_prepared_work_never_advances_a_new_pending_key").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_mid_batch").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let (account, mailbox) = register(&db, 4).await;
	let old = key(&account, 80);
	db.write(async |tx| tx.queue_expiry_owner(old, None).await).await.unwrap();
	let batch = db.prepare_expiry_owner_scan(32, 1).await.unwrap().remove(0);
	let stale = db.prepare_expiry_owner_scan(32, 1).await.unwrap().remove(0);
	let late = key(&account, 8);
	db.write(async |tx| tx.queue_expiry_owner(late, Some(mailbox)).await).await.unwrap();
	let result = batch.derive().unwrap();
	db.commit_expiry_owner_scan(Network::Regtest, &result).await.unwrap();
	assert_eq!(owner(&db, late).await, Attribution::Unresolved);
	// Drop a newly prepared batch to model interruption before a completed
	// derivation/commit. Restart must derive its whole untested prefix again.
	drop(db.prepare_expiry_owner_scan(32, 1).await.unwrap());
	drop(db);
	let db = Db::connect(&config).await.unwrap();
	db.scan_expiry_owners(Network::Regtest, 32, 1).await.unwrap();
	assert!(matches!(owner(&db, late).await, Attribution::Owned { child_index: 8, .. }));
	db.scan_expiry_owners(Network::Regtest, 32, 1).await.unwrap();
	assert_eq!(db.commit_expiry_owner_scan(Network::Regtest, &stale.derive().unwrap()).await.unwrap(), 0);
	let prefix = db.read(async |tx| Ok(tx.query_one("SELECT scanned_up_to FROM expiry_scan WHERE user_pubkey=$1",
		&[&old.to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(prefix, 64, "a stale result must not roll back a later completed prefix");
	db.scan_expiry_owners(Network::Regtest, 32, 1).await.unwrap();
	assert!(matches!(owner(&db, old).await, Attribution::Owned { child_index: 80, .. }));
}

#[tokio::test]
async fn expiry_attribution_wrong_mailbox_is_not_a_payee_and_registration_invalidates_no_owner() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_wrong_mailbox_is_not_a_payee_and_registration_invalidates_no_owner").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_wrong_mailbox").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let (actual, actual_mailbox) = register(&db, 5).await;
	let (_, wrong_mailbox) = register(&db, 6).await;
	let customer = key(&actual, 7);
	db.write(async |tx| tx.queue_expiry_owner(customer, Some(wrong_mailbox)).await).await.unwrap();
	db.scan_expiry_owners(Network::Regtest, 16, 2).await.unwrap();
	assert!(matches!(owner(&db, customer).await, Attribution::Owned { mailbox_pk, .. } if mailbox_pk == actual_mailbox.as_pubkey()));

	let future_account = Xpriv::new_master(Network::Regtest, &[7; 64]).unwrap();
	let unknown = key(&future_account, 1);
	db.write(async |tx| tx.queue_expiry_owner(unknown, None).await).await.unwrap();
	db.scan_expiry_owners(Network::Regtest, 16, 2).await.unwrap();
	assert_eq!(owner(&db, unknown).await, Attribution::Unresolved);
	// Fixture: the previous prefix was completed already. Exercise actual
	// derivation of the final two normal indices and the terminal coverage check
	// without making every test derive billions of keys.
	db.write(async |tx| {
		tx.execute("UPDATE expiry_scan SET scanned_up_to=$2 WHERE user_pubkey=$1",
			&[&unknown.to_string(), &i64::from(OWNER_KEYSPACE_END - 2)]).await?;
		Ok(())
	}).await.unwrap();
	db.scan_expiry_owners(Network::Regtest, 16, 2).await.unwrap();
	let Attribution::NoRegisteredOwner { registry_version } = owner(&db, unknown).await else { panic!("complete coverage must be terminal"); };
	// Another pending key completed every registered account. A later key
	// still starts at zero and must not inherit that terminal coverage.
	let late = key(&actual, 8);
	db.write(async |tx| tx.queue_expiry_owner(late, None).await).await.unwrap();
	db.scan_expiry_owners(Network::Regtest, 16, 2).await.unwrap();
	assert!(matches!(owner(&db, late).await, Attribution::Owned { child_index: 8, .. }));
	register(&db, 7).await;
	assert!(!db.write(async |tx| tx.revalidate_expiry_no_owner(unknown, registry_version, Network::Regtest).await).await.unwrap());
	assert_eq!(owner(&db, unknown).await, Attribution::Unresolved);
	db.scan_expiry_owners(Network::Regtest, 16, 2).await.unwrap();
	assert!(matches!(owner(&db, unknown).await, Attribution::Owned { child_index: 1, .. }));
}

/// Run explicitly on the intended production build profile. These measurements
/// resolve real registered low-index keys; they do not claim to time an exhaustive
/// no-owner search through R * 2^31 children.
#[tokio::test]
#[ignore = "measures attribution at 100, 1,000 and 10,000 registered accounts"]
async fn expiry_attribution_resolution_scale() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_resolution_scale").await;
	ctx.init_central_postgres().await;
	let mut report = Vec::new();
	for count in [100_u32, 1_000, 10_000] {
		let config = ctx.new_postgres(&format!("expiry_scale_{count}")).await;
		Db::create(&config).await.unwrap();
		let db = Db::connect(&config).await.unwrap();
		let mut records = Vec::new();
		for index in 0..count {
			let mut seed = [0_u8; 64];
			seed[..4].copy_from_slice(&index.to_le_bytes());
			seed[63] = 71;
			let account = Xpriv::new_master(Network::Regtest, &seed).unwrap();
			let mailbox = account.to_keypair(&SECP);
			let record = FallbackRecord::new(Network::Regtest,
				ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap(), &account, &mailbox).unwrap();
			records.push((account, mailbox.public_key(), record));
		}
		// The real owner sorts last among non-candidate records, so this is
		// not a favorable first-record lookup disguised as a registry scan.
		records.sort_by_key(|(_, mailbox, _)| mailbox.to_string());
		for batch in records.chunks(100) {
			db.write(async |tx| {
				for (_, mailbox, record) in batch {
					tx.set_fallback(*mailbox, record, Network::Regtest).await?;
				}
				Ok(())
			}).await.unwrap();
		}
		let (account, mailbox, _) = records.last().unwrap();
		let wrong = MailboxIdentifier::from_pubkey(records.first().unwrap().1);
		for (label, child_index, candidate) in [("wrong-mailbox", 17, Some(wrong)), ("absent-mailbox", 18, None)] {
			let wanted = key(account, child_index);
			let start = std::time::Instant::now();
			db.write(async |tx| tx.queue_expiry_owner(wanted, candidate).await).await.unwrap();
			let mut passes = 0_u64;
			let mut derived = 0_u64;
			loop {
				match owner(&db, wanted).await {
					Attribution::Owned { mailbox_pk, child_index: found, .. } => {
						assert_eq!(mailbox_pk, *mailbox);
						assert_eq!(found, child_index);
						break;
					},
					Attribution::NoRegisteredOwner { .. } => panic!("registered key was misclassified"),
					Attribution::Unresolved => {},
				}
				let stats = db.scan_expiry_owners(Network::Regtest, 10_000, 2).await.unwrap();
				assert!(stats.children_derived > 0, "unresolved attribution made no progress");
				derived += stats.children_derived;
				passes += 1;
			}
			let result = serde_json::json!({
				"records": count, "case": label, "owner_child_index": child_index,
				"owner_sorted_position": count - 1, "chunk_size_max": 10_000, "workers": 2,
				"elapsed_seconds": start.elapsed().as_secs_f64(), "children_derived": derived,
				"passes": passes, "debug_assertions": cfg!(debug_assertions),
			});
			println!("{result}");
			report.push(result);
			tokio::fs::write(ctx.datadir.join("expiry-attribution-resolution-scale.json"),
				serde_json::to_vec_pretty(&report).unwrap()).await.unwrap();
		}
	}
}

#[tokio::test]
async fn expiry_attribution_registry_fence_blocks_registration_until_decision_commits() {
	let mut ctx = TestContext::new_minimal("postgresd/expiry_attribution_registry_fence").await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres("expiry_registry_fence").await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	register(&db, 3).await;
	let future = Xpriv::new_master(Network::Regtest, &[7; 64]).unwrap();
	let wanted = key(&future, 1);
	db.write(async |tx| {
		tx.queue_expiry_owner(wanted, None).await?;
		// Fixture for prior completed exhaustive work; the lock protocol is
		// independent of its CPU cost, which the scale test reports separately.
		tx.execute("UPDATE expiry_scan SET scanned_up_to=2147483648 WHERE user_pubkey=$1", &[&wanted.to_string()]).await?;
		Ok(())
	}).await.unwrap();
	let Attribution::NoRegisteredOwner { registry_version } = owner(&db, wanted).await else { panic!("fixture is incomplete"); };
	let (held_tx, held_rx) = tokio::sync::oneshot::channel();
	let (release_tx, release_rx) = tokio::sync::oneshot::channel();
	let decision_db = db.clone();
	let decision = tokio::spawn(async move {
		decision_db.write(async |tx| {
			assert!(tx.revalidate_expiry_no_owner(wanted, registry_version, Network::Regtest).await?);
			held_tx.send(()).unwrap();
			release_rx.await.unwrap();
			Ok(())
		}).await.unwrap();
	});
	held_rx.await.unwrap();
	let registration_db = db.clone();
	let registration = tokio::spawn(async move { register(&registration_db, 7).await });
	// Observe PostgreSQL's actual waiting advisory lock, rather than assuming
	// that a sleep means the registration reached the protected boundary.
	tokio::time::timeout(std::time::Duration::from_secs(5), async {
		loop {
			let waiting = db.read(async |tx| Ok(tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks
				WHERE locktype='advisory' AND database=(SELECT oid FROM pg_database WHERE datname=current_database())
				AND objid=3 AND NOT granted)", &[]).await?.get::<_, bool>(0))).await.unwrap();
			if waiting { break; }
			tokio::time::sleep(std::time::Duration::from_millis(10)).await;
		}
	}).await.unwrap();
	release_tx.send(()).unwrap();
	decision.await.unwrap();
	registration.await.unwrap();
	assert_eq!(owner(&db, wanted).await, Attribution::Unresolved);
}
