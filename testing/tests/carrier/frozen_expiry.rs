//! Actual source freeze, component-process restart and unknown COMMIT evidence.
//! Automatic service startup remains a separate qualification gate.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use server::watchman::frozen_expiry::FrozenExpiryProcessor;

#[derive(Serialize, Deserialize)]
struct ProcessorRecipe {
	source: CommitRecipe,
	prepared: PathBuf,
	permit_submission: PathBuf,
	submitted: PathBuf,
	released: PathBuf,
}

fn marker(path: &PathBuf, txid: bitcoin::Txid) {
	let bytes = serde_json::to_vec(&txid).unwrap();
	if path.exists() { assert_eq!(std::fs::read(path).unwrap(), bytes); return; }
	let temporary = path.with_extension("partial");
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary).unwrap();
	file.write_all(&bytes).unwrap();
	file.sync_all().unwrap();
	std::fs::rename(temporary, path).unwrap();
}

#[tokio::test]
#[ignore = "owned component worker; never a production daemon readiness bypass"]
async fn frozen_expiry_component_worker() {
	let path = std::env::var_os("FROZEN_EXPIRY_RECIPE").expect("owned recipe is required");
	let recipe: ProcessorRecipe = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&recipe.source.config_path).unwrap()).unwrap();
	let db = Db::connect(&config.postgres).await.unwrap();
	let node = server::bitcoind::build_client(&config.bitcoind.url, config.bitcoind.auth()).unwrap();
	let core = bitcoincore_rpc::Client::new(&config.bitcoind.url, config.bitcoind.auth()).unwrap();
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	let tip = server::bitcoind::get_block_by_height(&node, height).await.unwrap();
	let mnemonic = wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap();
	let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	let wallet = PersistedWallet::load_derive_from_master_xpriv(db.clone(), node.clone(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let wallet = InstrumentedLock::new("frozen_expiry_component", wallet);
	let mut processor = FrozenExpiryProcessor::new(db.clone(), wallet.clone(), Network::Regtest,
		recipe.source.operator_script, budget(), limits());
	processor.recover_durable_groups().await.unwrap();
	let existing = db.read(async |t| Ok(t.query_opt("SELECT c.txid FROM expiry_carrier c JOIN expiry_carrier_allocation m ON m.txid=c.txid
		JOIN expiry_allocation a ON a.id=m.allocation_id WHERE a.freeze_set=$1 ORDER BY generation DESC LIMIT 1",
		&[&recipe.source.freeze_set]).await?)).await.unwrap();
	let txid = if let Some(row) = existing { row.get::<_, &str>("txid").parse().unwrap() } else {
		processor.prepare_group(&signer, &node, &recipe.source.freeze_set, height, FeeRate::from_sat_per_vb(30).unwrap()).await.unwrap()
	};
	marker(&recipe.prepared, txid);
	while !recipe.permit_submission.exists() { tokio::time::sleep(Duration::from_millis(25)).await; }
	loop {
		match processor.observe_and_submit(&node, &recipe.source.freeze_set).await.unwrap() {
			DeliveryState::Pending { txid: actual } => { assert_eq!(actual, txid); marker(&recipe.submitted, txid); },
			DeliveryState::Confirmed { txid: actual, .. } => {
				assert_eq!(actual, txid);
				let mut held = wallet.lock().await;
				sync_fee_wallet(&mut held, &core);
				held.persist().await.unwrap();
				drop(held);
				processor.release_confirmed_group(&node, &recipe.source.freeze_set).await.unwrap();
				marker(&recipe.released, txid);
				return;
			},
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

async fn wait_marker(worker: &mut tokio::process::Child, path: &PathBuf) -> bitcoin::Txid {
	tokio::time::timeout(Duration::from_secs(30), async {
		while !path.exists() {
			assert!(worker.try_wait().unwrap().is_none(), "owned worker exited before the financial barrier");
			tokio::time::sleep(Duration::from_millis(25)).await;
		}
	}).await.unwrap();
	serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn frozen_expiry_real_process_restart_delivers_frozen_group_once() {
	// Zero carrier rates creates a real fully source-authorized freeze only.
	// Existing helper's control child is killed without constructing a carrier.
	let case = crash_case("frozen_expiry_real_process_restart_delivers_frozen_group_once", vec![]).await;
	assert!(case.versions.is_empty());
	let recipe = ProcessorRecipe { source: case.recipe.clone(), prepared: case.ctx.datadir.join("processor-prepared"),
		permit_submission: case.ctx.datadir.join("permit-component-submission"), submitted: case.ctx.datadir.join("processor-submitted"),
		released: case.ctx.datadir.join("processor-released") };
	let path = case.ctx.datadir.join("processor-recipe.json");
	let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
	file.write_all(&serde_json::to_vec(&recipe).unwrap()).unwrap();
	file.sync_all().unwrap();
	let spawn = || Command::new(std::env::current_exe().unwrap())
		.args(["--ignored", "--exact", "direct_delivery::frozen_expiry::frozen_expiry_component_worker", "--nocapture"])
		.env("FROZEN_EXPIRY_RECIPE", &path).kill_on_drop(true).spawn().unwrap();
	let mut worker = spawn();
	let txid = wait_marker(&mut worker, &recipe.prepared).await;
	worker.kill().await.unwrap();
	assert_eq!(std::os::unix::process::ExitStatusExt::signal(&worker.wait().await.unwrap()), Some(9));
	let bytes = case.db.read(async |t| Ok(t.query_one("SELECT signed_tx FROM expiry_carrier WHERE txid=$1", &[&txid.to_string()]).await?.get::<_, Vec<u8>>("signed_tx"))).await.unwrap();
	assert!(!case.ctx.bitcoind().sync_client().get_raw_mempool().unwrap().contains(&txid));
	let signed: bitcoin::Transaction = bitcoin::consensus::deserialize(&bytes).unwrap();
	assert_eq!(signed.compute_txid(), txid);
	std::fs::write(&recipe.permit_submission, []).unwrap();
	let mut restarted = spawn();
	assert_eq!(wait_marker(&mut restarted, &recipe.submitted).await, txid);
	case.ctx.generate_blocks(3).await;
	assert_eq!(wait_marker(&mut restarted, &recipe.released).await, txid);
	assert!(restarted.wait().await.unwrap().success());
	let retained = case.db.read(async |t| Ok(t.query("SELECT signed_tx,confirmed_height FROM expiry_carrier", &[]).await?)).await.unwrap();
	assert_eq!(retained.len(), 1);
	assert_eq!(retained[0].get::<_, Vec<u8>>("signed_tx"), bytes);
	assert!(retained[0].get::<_, Option<i32>>("confirmed_height").is_some());
	let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
	assert_eq!(outputs.iter().filter(|(p,_)| p.txid == txid).map(|(_,v)| *v).collect::<Vec<_>>(), vec![sat(19_000)]);
	assert!(outputs.iter().filter(|(p,_)| p.txid != txid).all(|(p,_)| p.txid == case.board_funding_txid));
}

// Reuse the existing fallback_faults relay's exact simple-protocol COMMIT
// barriers. This relay targets only the owned retained PostgreSQL fixture.
const COMMIT_QUERY: &[u8] = b"Q\0\0\0\x0bCOMMIT\0";
const COMMIT_DONE: &[u8] = b"C\0\0\0\x0bCOMMIT\0";
const PUBLICATION_MARKER: &[u8] = b"direct-publication-marker";

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommitLoss { Request, Reply, BeforeReply }

async fn publication_relay(upstream: &server::config::Postgres, loss: CommitLoss) -> (u16, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
	assert!(upstream.host.parse::<std::net::IpAddr>().unwrap().is_loopback());
	let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let armed = Arc::new(AtomicBool::new(true));
	let fired = Arc::new(AtomicBool::new(false));
	let host = upstream.host.clone();
	let upstream_port = upstream.port;
	let receipt = fired.clone();
	let task = tokio::spawn(async move {
		while let Ok((mut client, _)) = listener.accept().await {
			let mut server = TcpStream::connect((host.as_str(), upstream_port)).await.unwrap();
			let armed = armed.clone();
			let fired = fired.clone();
			tokio::spawn(async move {
				let (mut client_rx, mut client_tx) = client.split();
				let (mut server_rx, mut server_tx) = server.split();
				let (mut up, mut down) = (vec![0; 65_536], vec![0; 65_536]);
				let (mut up_tail, mut down_tail) = (Vec::new(), Vec::new());
				let (mut marked, mut commit_sent) = (false, false);
				let contains = |bytes: &[u8], needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
				loop {
					tokio::select! {
						n = client_rx.read(&mut up) => {
							let n = match n { Ok(0) | Err(_) => return, Ok(n) => n };
							let window = [up_tail.as_slice(), &up[..n]].concat();
							if marked && contains(&window, COMMIT_QUERY) && armed.swap(false, Ordering::SeqCst) {
								if loss == CommitLoss::Request { fired.store(true, Ordering::SeqCst); return; }
								commit_sent = true;
								if loss == CommitLoss::BeforeReply {
									if server_tx.write_all(&up[..n]).await.is_err() { return; }
									let _ = client_tx.shutdown().await;
									fired.store(true, Ordering::SeqCst);
									// Keep the original backend connection alive while its
									// deferred COMMIT waits for the owned test lock.
									loop { if !matches!(server_rx.read(&mut down).await, Ok(n) if n > 0) { return; } }
								}
							}
							if server_tx.write_all(&up[..n]).await.is_err() { return; }
							up_tail = window[window.len().saturating_sub(64)..].to_vec();
						},
						n = server_rx.read(&mut down) => {
							let n = match n { Ok(0) | Err(_) => return, Ok(n) => n };
							let window = [down_tail.as_slice(), &down[..n]].concat();
							marked |= contains(&window, PUBLICATION_MARKER);
							if commit_sent && contains(&window, COMMIT_DONE) { fired.store(true, Ordering::SeqCst); return; }
							if client_tx.write_all(&down[..n]).await.is_err() { return; }
							down_tail = window[window.len().saturating_sub(64)..].to_vec();
						},
					}
				}
			});
		}
	});
	(port, receipt, task)
}

async fn unknown_publication(name: &str, lose_reply: bool) {
	let case = crash_case(name, vec![]).await;
	case.db.write(async |t| {
		t.batch_execute("CREATE FUNCTION direct_publication_notice() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN RAISE NOTICE 'direct-publication-marker'; RETURN NEW; END $$;
			CREATE TRIGGER direct_publication_notice AFTER INSERT ON expiry_carrier
			FOR EACH ROW EXECUTE FUNCTION direct_publication_notice();").await?;
		Ok(())
	}).await.unwrap();
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let (port, fired, relay) = publication_relay(&config.postgres, if lose_reply { CommitLoss::Reply } else { CommitLoss::Request }).await;
	let mut pg = config.postgres.clone();
	pg.host = "127.0.0.1".into();
	pg.port = port;
	let db = Db::connect(&pg).await.unwrap();
	let node = case.ctx.bitcoind().async_client();
	let core = case.ctx.bitcoind().sync_client();
	let master = Xpriv::new_master(Network::Regtest, &wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap().to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	let wallet = InstrumentedLock::new("unknown_direct_publication", case.wallet);
	let mut processor = FrozenExpiryProcessor::new(db, wallet.clone(), Network::Regtest,
		case.recipe.operator_script.clone(), budget(), limits());
	processor.recover_durable_groups().await.unwrap();
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	let outcome = processor.prepare_group(&signer, &node, &case.recipe.freeze_set, height, FeeRate::from_sat_per_vb(30).unwrap()).await;
	assert!(outcome.is_err(), "actual lost COMMIT must not be reported as successful publication");
	let original_id = processor.retained_publication_id().expect("the original signed carrier remains held");
	assert!(fired.load(Ordering::SeqCst), "the actual publication COMMIT barrier must fire");
	assert!(processor.prepare_group(&signer, &node, &case.recipe.freeze_set, height, FeeRate::from_sat_per_vb(30).unwrap()).await.is_err());
	assert!(tokio::time::timeout(Duration::from_millis(100), wallet.lock()).await.is_err(), "unknown outcome retains the exclusive fee wallet");
	assert!(core.get_raw_mempool().unwrap().is_empty());
	let rows = case.db.read(async |t| Ok(t.query("SELECT txid,signed_tx FROM expiry_carrier", &[]).await?)).await.unwrap();
	if lose_reply {
		assert_eq!(rows.len(), 1, "reply lost only after actual durable COMMIT");
		let txid: bitcoin::Txid = rows[0].get::<_, &str>("txid").parse().unwrap();
		let original: bitcoin::Transaction = bitcoin::consensus::deserialize(&rows[0].get::<_, Vec<u8>>("signed_tx")).unwrap();
		processor.probe_publication().await.unwrap();
		assert_eq!(processor.observe_and_submit(&node, &case.recipe.freeze_set).await.unwrap(), DeliveryState::Pending { txid });
		assert_eq!(core.get_raw_transaction(&txid, None).unwrap(), original);
		case.ctx.generate_blocks(3).await;
		let mut held = wallet.lock().await;
		sync_fee_wallet(&mut held, &core);
		held.persist().await.unwrap();
		drop(held);
		processor.release_confirmed_group(&node, &case.recipe.freeze_set).await.unwrap();
		let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
		assert_eq!(outputs.iter().filter(|(p,_)| p.txid == txid).map(|(_,v)| *v).collect::<Vec<_>>(), vec![sat(19_000)]);
		assert!(outputs.iter().filter(|(p,_)| p.txid != txid).all(|(p,_)| p.txid == case.board_funding_txid));
	} else {
		assert!(rows.is_empty(), "request lost before COMMIT leaves no durable carrier");
		for encoded in &case.recipe.backing {
			let backing: ServerVtxo = ServerVtxo::deserialize(encoded).unwrap();
			let point = backing.point();
			assert!(core.get_tx_out(&point.txid, point.vout, Some(true)).unwrap().is_some());
		}
		processor.probe_publication().await.expect("same retained publication must recover without rebuilding principal");
		assert!(processor.retained_publication_id().is_none());
		let retained = case.db.read(async |t| Ok(t.query("SELECT txid,signed_tx FROM expiry_carrier", &[]).await?)).await.unwrap();
		assert_eq!(retained.len(), 1);
		let signed: bitcoin::Transaction = bitcoin::consensus::deserialize(&retained[0].get::<_, Vec<u8>>("signed_tx")).unwrap();
		assert_eq!((signed.compute_txid(), signed.compute_wtxid()), original_id, "recovery must preserve the original body and witness bytes");
		assert_eq!(processor.observe_and_submit(&node, &case.recipe.freeze_set).await.unwrap(), DeliveryState::Pending { txid: original_id.0 });
		assert_eq!(core.get_raw_transaction(&original_id.0, None).unwrap(), signed);
		case.ctx.generate_blocks(3).await;
		let mut held = wallet.lock().await;
		sync_fee_wallet(&mut held, &core);
		held.persist().await.unwrap();
		drop(held);
		processor.release_confirmed_group(&node, &case.recipe.freeze_set).await.unwrap();
		let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
		assert_eq!(outputs.iter().filter(|(p,_)| p.txid == original_id.0).map(|(_,v)| *v).collect::<Vec<_>>(), vec![sat(19_000)]);
		assert!(outputs.iter().filter(|(p,_)| p.txid != original_id.0).all(|(p,_)| p.txid == case.board_funding_txid));
	}
	drop(processor);
	relay.abort();
	let _ = relay.await;
}

#[tokio::test]
async fn frozen_expiry_lost_commit_reply_probes_same_bytes_before_submission() {
	unknown_publication("frozen_expiry_lost_commit_reply_probes_same_bytes_before_submission", true).await;
}

#[tokio::test]
async fn frozen_expiry_lost_commit_request_recovers_identical_publication() {
	unknown_publication("frozen_expiry_lost_commit_request_recovers_identical_publication", false).await;
}

#[tokio::test]
async fn frozen_expiry_late_generation_requires_complete_reservation_reload() {
	let case = crash_case("frozen_expiry_late_generation_requires_complete_reservation_reload", vec![30]).await;
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let node = case.ctx.bitcoind().async_client();
	let core = case.ctx.bitcoind().sync_client();
	let master = Xpriv::new_master(Network::Regtest, &wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap().to_seed("")).unwrap();
	let wallet = InstrumentedLock::new("stale_processor", case.wallet);
	let mut processor = FrozenExpiryProcessor::new(case.db.clone(), wallet.clone(), Network::Regtest,
		case.recipe.operator_script.clone(), budget(), limits());
	processor.recover_durable_groups().await.unwrap();
	let mut held = wallet.lock().await;
	case.ctx.bitcoind().fund_addr(held.next_unused_address(KEYCHAIN).address, sat(900_000)).await;
	case.ctx.generate_blocks(3).await;
	sync_fee_wallet(&mut held, &core);
	held.persist().await.unwrap();
	drop(held);
	let mut later = case.recipe.clone();
	later.fee_rates = vec![40];
	later.committed_marker = case.ctx.datadir.join("later-generation-committed");
	let txids = run_commit_worker(&later, case.ctx.datadir.join("later-generation-recipe.json")).await;
	assert_eq!(txids.len(), 1);
	let refused = processor.recover_durable_groups().await.expect_err("existing processor cannot claim hydration of a newer generation");
	assert!(format!("{refused:#}").contains("generation frontier changed"));
	let backing_points = case.recipe.backing.iter().map(|bytes| {
		let vtxo: ServerVtxo = ServerVtxo::deserialize(bytes).unwrap();
		vtxo.point()
	}).collect::<BTreeSet<_>>();
	assert!(wallet.lock().await.lock_wallet_utxo(case.versions[0].input.iter()
		.map(|i| i.previous_output).find(|p| !backing_points.contains(p)).unwrap()).is_err());
	assert!(core.get_raw_mempool().unwrap().is_empty());
	drop(processor);
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	let tip = server::bitcoind::get_block_by_height(&node, height).await.unwrap();
	let restored = PersistedWallet::load_derive_from_master_xpriv(case.db.clone(), node.clone(),
		Network::Regtest, &master, WalletKind::Watchman, tip, 3).await.unwrap();
	let restored = InstrumentedLock::new("reloaded_processor", restored);
	let mut recovered = FrozenExpiryProcessor::new(case.db.clone(), restored.clone(), Network::Regtest,
		case.recipe.operator_script.clone(), budget(), limits());
	recovered.recover_durable_groups().await.unwrap();
	assert_eq!(recovered.observe_and_submit(&node, &case.recipe.freeze_set).await.unwrap(), DeliveryState::Pending { txid: txids[0] });
	case.ctx.generate_blocks(3).await;
	assert!(recovered.release_confirmed_group(&node, &case.recipe.freeze_set).await.is_err(), "confirmation without owning-wallet sync keeps reservations");
	let mut held = restored.lock().await;
	sync_fee_wallet(&mut held, &core);
	held.persist().await.unwrap();
	drop(held);
	recovered.release_confirmed_group(&node, &case.recipe.freeze_set).await.unwrap();
	let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
	assert_eq!(outputs.iter().filter(|(p,_)| p.txid == txids[0]).map(|(_,v)| *v).collect::<Vec<_>>(), vec![sat(19_000)]);
	assert!(outputs.iter().filter(|(p,_)| p.txid != txids[0]).all(|(p,_)| p.txid == case.board_funding_txid));
}

async fn delayed_commit(name: &str, cancel_prepare: bool) {
	let case = crash_case(name, vec![]).await;
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let (control, connection) = Db::config(&config.postgres.name, &config.postgres)
		.connect(tokio_postgres::NoTls).await.unwrap();
	let control_task = tokio::spawn(connection);
	control.execute("SELECT pg_advisory_lock(936472819::bigint)", &[]).await.unwrap();
	case.db.write(async |t| {
		t.batch_execute("CREATE FUNCTION delayed_carrier_commit() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN PERFORM pg_advisory_xact_lock(936472819::bigint); RETURN NEW; END $$;
			CREATE CONSTRAINT TRIGGER delayed_carrier_commit AFTER INSERT ON expiry_carrier
			DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION delayed_carrier_commit();
			CREATE FUNCTION publication_marker() RETURNS trigger LANGUAGE plpgsql AS $$
			BEGIN RAISE NOTICE 'direct-publication-marker'; RETURN NEW; END $$;
			CREATE TRIGGER publication_marker AFTER INSERT ON expiry_carrier FOR EACH ROW EXECUTE FUNCTION publication_marker();").await?;
		Ok(())
	}).await.unwrap();
	let (port, fired, relay) = publication_relay(&config.postgres, CommitLoss::BeforeReply).await;
	let mut pg = config.postgres.clone();
	pg.host = "127.0.0.1".into();
	pg.port = port;
	let db = if cancel_prepare { case.db.clone() } else { Db::connect(&pg).await.unwrap() };
	let node = case.ctx.bitcoind().async_client();
	let core = case.ctx.bitcoind().sync_client();
	let master = Xpriv::new_master(Network::Regtest, &wallet::read_mnemonic_from_datadir(&config.data_dir).unwrap().to_seed("")).unwrap();
	let key_at = |path| Keypair::from_secret_key(&ark::SECP,
		&master.derive_priv(&ark::SECP, &DerivationPath::from_str(path).unwrap()).unwrap().private_key);
	let signer = WatchmanSigner::new(Secret::new(key_at("m/2'/0'")), Secret::new(key_at("m/30'")), db.clone());
	let wallet = InstrumentedLock::new("delayed_publication", case.wallet);
	let mut processor = FrozenExpiryProcessor::new(db, wallet.clone(), Network::Regtest,
		case.recipe.operator_script.clone(), budget(), limits());
	processor.recover_durable_groups().await.unwrap();
	let height = BlockHeight::new(core.get_block_count().unwrap() as u32);
	if cancel_prepare {
		// Dropping this actual future while COMMIT waits must not drop the signed
		// carrier or release its exclusive wallet from the surviving processor.
		let pending = processor.prepare_group(&signer, &node, &case.recipe.freeze_set, height, FeeRate::from_sat_per_vb(30).unwrap());
		tokio::pin!(pending);
		tokio::time::timeout(Duration::from_secs(10), async {
			loop {
				assert!(tokio::time::timeout(Duration::from_millis(25), &mut pending).await.is_err(), "prepare must still be waiting for the original COMMIT");
				let waiting = control.query("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND query='COMMIT' AND wait_event_type='Lock'", &[]).await.unwrap();
				if waiting.len() == 1 { break; }
			}
		}).await.unwrap();
	} else {
		assert!(processor.prepare_group(&signer, &node, &case.recipe.freeze_set, height, FeeRate::from_sat_per_vb(30).unwrap()).await.is_err());
		assert!(fired.load(Ordering::SeqCst));
	}
	let waiting = control.query("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND query='COMMIT' AND wait_event_type='Lock'", &[]).await.unwrap();
	assert_eq!(waiting.len(), 1, "the original real COMMIT remains alive behind the deferred lock");
	let original = processor.retained_publication_id().expect("cancelled or disconnected publication retains the original signed carrier");
	assert!(tokio::time::timeout(Duration::from_millis(100), wallet.lock()).await.is_err());
	assert!(core.get_raw_mempool().unwrap().is_empty());
	if !cancel_prepare {
		let pending = processor.probe_publication();
		tokio::pin!(pending);
		assert!(tokio::time::timeout(Duration::from_millis(100), &mut pending).await.is_err(), "an absent read cannot skip the previous COMMIT completion fence");
		let rows: i64 = control.query_one("SELECT count(*)::bigint AS rows FROM expiry_carrier", &[]).await.unwrap().get("rows");
		assert_eq!(rows, 0);
		control.execute("SELECT pg_advisory_unlock(936472819::bigint)", &[]).await.unwrap();
		tokio::time::timeout(Duration::from_secs(10), &mut pending).await.unwrap().unwrap();
	} else {
		control.execute("SELECT pg_advisory_unlock(936472819::bigint)", &[]).await.unwrap();
		processor.probe_publication().await.unwrap();
	}
	let rows = case.db.read(async |t| Ok(t.query("SELECT signed_tx,generation FROM expiry_carrier", &[]).await?)).await.unwrap();
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].get::<_, i64>("generation"), 1, "the original late commit keeps its generation");
	let signed: bitcoin::Transaction = bitcoin::consensus::deserialize(&rows[0].get::<_, Vec<u8>>("signed_tx")).unwrap();
	assert_eq!((signed.compute_txid(), signed.compute_wtxid()), original);
	assert_eq!(processor.observe_and_submit(&node, &case.recipe.freeze_set).await.unwrap(), DeliveryState::Pending { txid: original.0 });
	case.ctx.generate_blocks(3).await;
	let mut held = wallet.lock().await;
	sync_fee_wallet(&mut held, &core);
	held.persist().await.unwrap();
	drop(held);
	processor.release_confirmed_group(&node, &case.recipe.freeze_set).await.unwrap();
	let (outputs, _) = recover_all_bip84_and_spend(&case.ctx, &case.mnemonic).await;
	assert_eq!(outputs.iter().filter(|(p,_)| p.txid == original.0).map(|(_,v)| *v).collect::<Vec<_>>(), vec![sat(19_000)]);
	assert!(outputs.iter().filter(|(p,_)| p.txid != original.0).all(|(p,_)| p.txid == case.board_funding_txid));
	relay.abort();
	let _ = relay.await;
	drop(control);
	control_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn frozen_expiry_delayed_commit_completion_is_fenced_before_same_byte_retry() {
	delayed_commit("frozen_expiry_delayed_commit_completion_is_fenced_before_same_byte_retry", false).await;
}

#[tokio::test]
async fn frozen_expiry_cancelled_prepare_retains_original_before_commit_await() {
	delayed_commit("frozen_expiry_cancelled_prepare_retains_original_before_commit_await", true).await;
}
