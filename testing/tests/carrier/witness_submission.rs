//! Actual signed carriers must not touch Core before independent history is durable.

use super::*;

#[derive(Clone, Copy)]
enum Failure { Missing, Inaccessible, Conflicting, Identity }

async fn refuses_unrecordable_history(name: &str, failure: Failure) {
	let case = crash_case(name, vec![30]).await;
	let config: server::Config = toml::from_str(&std::fs::read_to_string(&case.recipe.config_path).unwrap()).unwrap();
	let root: ServerVtxo = ServerVtxo::deserialize(&case.recipe.backing[0]).unwrap();
	let _history = server::payout_witness::DirectPayoutWitness::load(&config.data_dir,
		Network::Regtest, root.server_pubkey(), &case.db, budget()).await.unwrap();
	let delivery = recovered(&case).await;
	let txid = case.versions[0].compute_txid();
	let directory = config.data_dir.join("direct-expiry-witness");
	let path = directory.join(format!("{txid}.json"));
	match failure {
		Failure::Missing => std::fs::rename(&directory, config.data_dir.join("retained-missing-history")).unwrap(),
		Failure::Inaccessible => std::fs::create_dir(&path).unwrap(),
		Failure::Conflicting => std::fs::write(&path, b"conflicting independent identity").unwrap(),
		Failure::Identity => {
			let identity = directory.join("identity.json");
			std::fs::rename(&identity, config.data_dir.join("retained-original-identity")).unwrap();
			let mut changed: serde_json::Value = serde_json::from_slice(&std::fs::read(config.data_dir.join("retained-original-identity")).unwrap()).unwrap();
			changed["server_pubkey"] = serde_json::Value::String(Keypair::new(&ark::SECP, &mut bitcoin::secp256k1::rand::thread_rng()).public_key().to_string());
			std::fs::write(identity, serde_json::to_vec(&changed).unwrap()).unwrap();
		},
	}
	let result = delivery.observe_and_submit(&case.ctx.bitcoind().async_client()).await;
	assert!(result.is_err(), "unrecordable independent history must prevent transaction observation/submission");
	assert!(case.ctx.bitcoind().sync_client().get_raw_mempool().unwrap().is_empty());
	let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
	let unused_node = server::bitcoind::build_client(&format!("http://{}", listener.local_addr().unwrap()), config.bitcoind.auth()).unwrap();
	assert!(tokio::time::timeout(Duration::from_millis(500), delivery.observe_and_submit(&unused_node)).await
		.expect("history refusal occurs before any Core request").is_err());
	assert!(tokio::time::timeout(Duration::from_millis(25), listener.accept()).await.is_err(), "history refusal must not connect to Core");
	let paid = case.db.read(async |t| Ok(t.query_one("SELECT count(*)::bigint FROM expiry_carrier WHERE confirmed_height IS NOT NULL", &[]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(paid, 0);
	for point in delivery.reserved_fee_inputs() { assert!(case.wallet.lock_wallet_utxo(*point).is_err()); }
	// Restore only the deliberately changed owning fixture paths; retain every
	// conflicting file/directory separately, never overwrite or erase evidence.
	match failure {
		Failure::Missing => std::fs::rename(config.data_dir.join("retained-missing-history"), &directory).unwrap(),
		Failure::Identity => {
			std::fs::rename(directory.join("identity.json"), config.data_dir.join("retained-changed-identity")).unwrap();
			std::fs::rename(config.data_dir.join("retained-original-identity"), directory.join("identity.json")).unwrap();
		},
		_ => std::fs::rename(&path, config.data_dir.join("retained-conflicting-history")).unwrap(),
	}
	confirm_once(&case, &delivery, &case.versions[0]).await;
	let recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
	let bytes: Vec<u8> = serde_json::from_value(recorded["tx"].clone()).unwrap();
	assert_eq!(bytes, bitcoin::consensus::serialize(&case.versions[0]));
	let index = std::fs::read_to_string(directory.join("history")).unwrap();
	assert_eq!(index.lines().collect::<Vec<_>>(), vec![txid.to_string()]);
}

#[tokio::test]
async fn direct_submission_missing_history_refuses_and_retains_fee_inputs() {
	refuses_unrecordable_history("direct_submission_missing_history_refuses_and_retains_fee_inputs", Failure::Missing).await;
}

#[tokio::test]
async fn direct_submission_inaccessible_history_refuses_and_retains_fee_inputs() {
	refuses_unrecordable_history("direct_submission_inaccessible_history_refuses_and_retains_fee_inputs", Failure::Inaccessible).await;
}

#[tokio::test]
async fn direct_submission_conflicting_history_refuses_and_retains_fee_inputs() {
	refuses_unrecordable_history("direct_submission_conflicting_history_refuses_and_retains_fee_inputs", Failure::Conflicting).await;
}

#[tokio::test]
async fn direct_submission_changed_identity_refuses_before_core() {
	refuses_unrecordable_history("direct_submission_changed_identity_refuses_before_core", Failure::Identity).await;
}

#[tokio::test]
async fn direct_submission_unrelated_empty_server_history_is_not_accepted() {
	let case = crash_case("direct_submission_unrelated_empty_server_history_is_not_accepted", vec![30]).await;
	let unrelated_ctx = TestContext::new("unrelated-witness/server-key").await;
	let unrelated = unrelated_ctx.captaind("server").bitcoind(case.ctx.bitcoind_arc()).no_vtxo_pool().create().await;
	let unrelated_key = unrelated.ark_info().await.server_pubkey;
	unrelated.stop().await.unwrap();
	let history = history_at(&unrelated.config().data_dir, &case.db, unrelated_key).await;
	let error = DirectDelivery::recover_group(case.db.clone(), &case.wallet, &case.recipe.freeze_set,
		Network::Regtest, budget(), limits(), history).await.err().expect("an unrelated empty independent history is not this carrier's witness");
	assert!(format!("{error:#}").contains("does not belong to backing server/network"));
	assert!(case.ctx.bitcoind().sync_client().get_raw_mempool().unwrap().is_empty());
	let delivery = recovered(&case).await;
	confirm_once(&case, &delivery, &case.versions[0]).await;
}

#[tokio::test]
async fn direct_submission_actual_delivery_on_default_size_tokio_worker() {
	let mut case = crash_case("direct_submission_actual_delivery_on_default_size_tokio_worker", vec![30]).await;
	let witness = history_for(&case.recipe, &case.db).await;
	let db = case.db.clone();
	let freeze_set = case.recipe.freeze_set.clone();
	let node = case.ctx.bitcoind().async_client();
	let wallet = case.wallet;
	// Tokio 1.52.3 documents 2 MiB as its default worker stack. Set that exact
	// size here so an outer fixture allowance cannot enlarge this operation.
	let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1)
		.thread_stack_size(2 * 1024 * 1024).enable_all().build().unwrap();
	let (wallet, delivery, state) = runtime.spawn(async move {
		let recovery = DirectDelivery::recover_group(db, &wallet, &freeze_set, Network::Regtest, budget(), limits(), witness);
		eprintln!("default-worker production recovery future bytes: {}", std::mem::size_of_val(&recovery));
		let delivery = recovery.await.unwrap();
		let observation = delivery.observe_and_submit(&node);
		eprintln!("default-worker production observation future bytes: {}", std::mem::size_of_val(&observation));
		let state = observation.await.unwrap();
		(wallet, delivery, state)
	}).await.unwrap();
	runtime.shutdown_background();
	case.wallet = wallet;
	assert_eq!(state, DeliveryState::Pending { txid: case.versions[0].compute_txid() });
	confirm_once(&case, &delivery, &case.versions[0]).await;
}
