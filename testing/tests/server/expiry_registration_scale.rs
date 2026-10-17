//! Explicit, unfunded registration/positive-attribution measurement. This is
//! not payout throughput or a measurement of exhaustive negative attribution.

use std::path::Path;
use std::time::{Duration, Instant};

use ark::fallback::FallbackRecord;
use ark::mailbox::MailboxIdentifier;
use ark::{ProtocolEncoding, SECP};
use ark_testing::TestContext;
use bitcoin::bip32::{ChildNumber, Xpriv, Xpub};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Network, ScriptBuf};
use serde_json::{json, Value};
use server::database::expiry_attribution::Attribution;
use server::database::Db;
use server_rpc::protos;

fn memory(pid: u32) -> Value {
	let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
	let field = |name: &str| status.lines().find_map(|line| line.strip_prefix(name))
		.and_then(|value| value.split_whitespace().next()).and_then(|value| value.parse::<u64>().ok());
	json!({"pid": pid, "rss_kib": field("VmRSS:"), "process_lifetime_peak_rss_kib": field("VmHWM:")})
}

fn server_pid(datadir: &Path) -> Option<u32> {
	// Inspect only process identity; never emit arguments, which may contain
	// credentials. The isolated harness owns this exact data directory.
	let config = datadir.join("config.toml").into_os_string().into_encoded_bytes();
	std::fs::read_dir("/proc").ok()?.filter_map(Result::ok).find_map(|entry| {
		let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
		let command = std::fs::read(entry.path().join("cmdline")).ok()?;
		command.split(|byte| *byte == 0).any(|argument| argument == config.as_slice()).then_some(pid)
	})
}

fn child(account: &Xpriv, index: u32) -> PublicKey {
	Xpub::from_priv(&SECP, account).ckd_pub(&SECP, ChildNumber::from_normal_idx(index).unwrap()).unwrap().public_key
}

async fn owner(db: &Db, key: PublicKey) -> Attribution {
	db.read(async |tx| tx.expiry_owner(key, Network::Regtest).await).await.unwrap()
}

async fn storage(db: &Db) -> Value {
	db.read(async |tx| {
		let row = tx.query_one("SELECT
			(SELECT COUNT(*) FROM fallback_record) AS records,
			(SELECT COUNT(*) FROM expiry_scan) AS cursors,
			(SELECT COUNT(*) FROM expiry_owner_key) AS matched_keys,
			pg_total_relation_size('fallback_record') AS record_bytes,
			pg_total_relation_size('expiry_scan') AS cursor_bytes", &[]).await?;
		Ok(json!({"records": row.get::<_, i64>("records"), "cursors": row.get::<_, i64>("cursors"),
			"matched_keys": row.get::<_, i64>("matched_keys"), "record_bytes": row.get::<_, i64>("record_bytes"),
			"cursor_bytes": row.get::<_, i64>("cursor_bytes")}))
	}).await.unwrap()
}

#[tokio::test]
#[ignore = "explicit signed RPC registration/positive attribution scale measurement"]
async fn expiry_registration_authenticated_scale() {
	let ctx = TestContext::new("server/expiry_registration_authenticated_scale").await;
	let mut report = json!({
		"scope": "real signed SetFallback RPC; real bounded owner scanner; no user/server funding or payouts",
		"not_measured": ["exhaustive 2^31-key negative search", "payout throughput", "concurrent user traffic", "Postgres/Core RSS"],
		"source_pin": option_env!("GIT_HASH"), "debug_assertions": cfg!(debug_assertions),
		"rss_scope": "test/scanner process and isolated captaind; HWM is process lifetime, not phase delta",
		"cases": [],
	});
	let report_path = ctx.datadir.join("authenticated-registration-scale.json");
	for count in [100_u32, 1_000, 10_000] {
		let srv = ctx.captaind(&format!("scale_{count}")).bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().create().await;
		let postgres = srv.config().postgres.clone();
		let db = Db::connect(&postgres).await.unwrap();
		let pid = server_pid(&srv.config().data_dir).expect("isolated captaind PID not found");
		let mut rpc = srv.get_public_rpc().await;
		let signing_start = Instant::now();
		let mut records = Vec::with_capacity(count as usize);
		for index in 0..count {
			let mut seed = [0_u8; 64];
			seed[..4].copy_from_slice(&index.to_le_bytes());
			seed[63] = 93;
			let account = Xpriv::new_master(Network::Regtest, &seed).unwrap();
			let mailbox = account.to_keypair(&SECP);
			let script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
			let record = FallbackRecord::new(Network::Regtest, script, &account, &mailbox).unwrap();
			records.push((account, mailbox.public_key(), record));
		}
		let signing_seconds = signing_start.elapsed().as_secs_f64();
		records.sort_by_key(|(_, mailbox, _)| mailbox.to_string());
		let registration_start = Instant::now();
		let mut latencies = Vec::with_capacity(count as usize);
		for (_, mailbox, record) in &records {
			let start = Instant::now();
			let encoded = record.serialize();
			let actual = rpc.set_fallback(protos::SetFallbackRequest {
				mailbox_pk: mailbox.serialize().to_vec(), record: encoded.clone(),
			}).await.unwrap().into_inner().record;
			latencies.push(start.elapsed().as_secs_f64());
			assert_eq!(actual, encoded, "server must authenticate and store the real signed record");
		}
		let registration_seconds = registration_start.elapsed().as_secs_f64();
		latencies.sort_by(f64::total_cmp);
		let registration_storage = storage(&db).await;
		assert_eq!(registration_storage["records"].as_i64(), Some(i64::from(count)));
		let registration = json!({"records": count, "case": "signed_public_rpc_registration",
			"client_signing_seconds": signing_seconds, "registration_seconds": registration_seconds,
			"records_per_second": f64::from(count) / registration_seconds,
			"rpc_p50_seconds": latencies[latencies.len()/2],
			"rpc_p95_seconds": latencies[latencies.len()*95/100],
			"rpc_p99_seconds": latencies[latencies.len()*99/100],
			"captaind_memory": memory(pid), "scanner_memory": memory(std::process::id()), "storage": registration_storage});
		report["cases"].as_array_mut().unwrap().push(registration);
		tokio::fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).await.unwrap();
		// Remove only this owned server's automatic scanner from the measurement.
		// The database remains real; the public RPC is no longer exercised below.
		drop(rpc);
		srv.stop().await.unwrap();
		let (account, mailbox, _) = records.last().unwrap();
		let wrong = MailboxIdentifier::from_pubkey(records.first().unwrap().1);
		for (label, index, candidate) in [
			("verified_mailbox_priority", 17, Some(MailboxIdentifier::from_pubkey(*mailbox))),
			("absent_mailbox_fallback", 18, None),
			("wrong_mailbox_fallback", 19, Some(wrong)),
		] {
			let key = child(account, index);
			let start = Instant::now();
			db.write(async |tx| tx.queue_expiry_owner(key, candidate).await).await.unwrap();
			let queued_storage = storage(&db).await;
			let mut passes = 0_u64;
			let mut children_derived = 0_u64;
			let mut records_scanned = 0_usize;
			let resolved = tokio::time::timeout(Duration::from_secs(120), async {
				loop {
					match owner(&db, key).await {
						Attribution::Owned { mailbox_pk, child_index, .. } => {
							assert_eq!((mailbox_pk, child_index), (*mailbox, index));
							break;
						},
						Attribution::NoRegisteredOwner { .. } => panic!("registered owner was misclassified"),
						Attribution::Unresolved => {},
					}
					let stats = db.scan_expiry_owners(Network::Regtest, 10_000, 2).await.unwrap();
					assert!(stats.children_derived > 0, "pending key made no progress");
					passes += 1;
					children_derived += stats.children_derived;
					records_scanned += stats.records_scanned;
				}
			}).await.is_ok();
			let elapsed = start.elapsed().as_secs_f64();
			let mut cached_seconds = None;
			if resolved {
				let cached_start = Instant::now();
				for _ in 0..100 { assert!(matches!(owner(&db, key).await, Attribution::Owned { .. })); }
				cached_seconds = Some(cached_start.elapsed().as_secs_f64());
			}
			report["cases"].as_array_mut().unwrap().push(json!({
				"records": count, "case": label, "resolved": resolved, "budget_seconds": 120,
				"elapsed_seconds": elapsed, "owner_sorted_position": count - 1, "child_index": index,
				"passes": passes, "children_derived": children_derived, "records_scanned": records_scanned,
				"chunk_size_max": 10_000, "workers": 2, "cached_100_lookup_seconds": cached_seconds,
				"scanner_memory": memory(std::process::id()), "queued_storage": queued_storage,
				"remaining_storage": storage(&db).await,
			}));
			tokio::fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).await.unwrap();
			// A timeout is retained as a measured failure. Stop this size rather
			// than contaminating another case with an unresolved pending key.
			if !resolved { break; }
		}
	}
	println!("{}", report_path.display());
	assert!(report["cases"].as_array().unwrap().iter()
		.all(|case| case.get("resolved").is_none_or(|value| value.as_bool() != Some(false))),
		"attribution exceeded its measured budget; partial report retained at {}", report_path.display());
}
