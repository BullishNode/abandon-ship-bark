//! Original observations for the native board/absence/recovery fixture.
//!
//! These are acquisition receipts, not a successful money-check result. In
//! particular, an absent wallet supplies no invented fallback movement debit.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bdk_wallet::{KeychainKind, Wallet};
use bdk_wallet::template::{Bip86, DescriptorTemplate};
use bitcoin::{Network, Transaction};
use bitcoin::bip32::{Xpriv, Xpub};
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::{sha256, Hash};
use chrono::Utc;
use serde_json::{json, Value};
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};
use uuid::Uuid;

use ark::{ProtocolEncoding, Vtxo, SECP};
use ark::vtxo::Full;
use ark_testing::{Bark, Barkd, Captaind, TestContext};
use bark::{OpenWalletArgs, WalletSeed};
use bark::lock_manager::memory::MemoryLockManager;
use bark::onchain::OnchainWallet;
use bark::persist::sqlite::SqliteClient;
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server::wallet::WalletKind;

const FINANCIAL: &str = include_str!("financial.sql");
const PATHS: &str = "SELECT COALESCE(jsonb_agg(jsonb_build_object(\
	'txid',txid,'hex',encode(signed_tx,'hex'))),'[]'::jsonb) \
	FROM public.virtual_transaction WHERE signed_tx IS NOT NULL";

pub(super) fn write_json(path: impl AsRef<Path>, value: &Value) {
	let mut file = OpenOptions::new().write(true).create_new(true).open(path)
		.expect("evidence receipt must be created once");
	serde_json::to_writer_pretty(&mut file, value).unwrap();
	file.write_all(b"\n").unwrap();
	file.sync_all().unwrap();
}

pub(super) fn tip(ctx: &TestContext) -> Value {
	ctx.bitcoind().sync_client().call("getblockchaininfo", &[]).unwrap()
}

fn public_derivation(ctx: &TestContext, descriptor: &str, high_water: Option<u32>) -> Value {
	let rpc = ctx.bitcoind().sync_client();
	let info: Value = rpc.call("getdescriptorinfo", &[json!(descriptor)]).unwrap();
	assert_eq!(info["hasprivatekeys"], false, "only public descriptors may leave the origin");
	let descriptor = info["descriptor"].as_str().unwrap();
	// The fixture creates only a bounded number of addresses. Retain both the
	// actual BDK creation index and the full scanned public range.
	let end = high_water.unwrap_or(0).max(1_023);
	let addresses: Value = rpc.call("deriveaddresses", &[json!(descriptor), json!([0, end])]).unwrap();
	let address_info = addresses.as_array().unwrap().iter().map(|address| {
		rpc.call::<Value>("getaddressinfo", &[address.clone()]).unwrap()
	}).collect::<Vec<_>>();
	json!({"descriptor": descriptor, "getdescriptorinfo": info,
		"deriveaddresses": addresses, "getaddressinfo": address_info,
		"range": [0, end], "creation_high_water_mark": high_water})
}

pub(super) struct Evidence {
	pub(super) dir: PathBuf,
	pub(super) source_incarnation: String,
	pub(super) observer_incarnation: String,
	debug: Option<Value>,
	captures: Vec<Value>,
	owned: BTreeSet<String>,
	configurations: Vec<Value>,
	baseline: Option<Value>,
	final_sql: Option<Value>,
	reader: Client,
}

impl Evidence {
	pub(super) async fn new(ctx: &TestContext, srv: &Captaind) -> Self {
		let dir = ctx.datadir.join("money-evidence");
		fs::create_dir(&dir).unwrap();
		let config = srv.config().postgres.clone();
		let (admin, connection) = Db::config(&config.name, &config).connect(NoTls).await.unwrap();
		tokio::spawn(async move { connection.await.expect("evidence setup connection failed"); });
		let role = format!("evidence_{}", Uuid::new_v4().simple());
		let password = Uuid::new_v4().simple().to_string();
		// These ACL changes are confined to this fixture's own database. The
		// daemon remains its owner. Credentials stay in memory and origin logs.
		admin.batch_execute(&format!("CREATE ROLE {role} LOGIN NOINHERIT NOSUPERUSER \
			NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '{password}'")).await.unwrap();
		let database = admin.query_one("SELECT quote_ident(current_database())", &[]).await.unwrap()
			.get::<_, String>(0);
		admin.batch_execute(&format!("REVOKE CREATE,TEMPORARY ON DATABASE {database} FROM PUBLIC; \
			REVOKE CREATE ON SCHEMA public FROM PUBLIC; \
			REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA public FROM PUBLIC; \
			GRANT CONNECT ON DATABASE {database} TO {role}; GRANT USAGE ON SCHEMA public TO {role}")).await.unwrap();
		let tables = FINANCIAL.lines().filter_map(|line| line.split("FROM public.").nth(1))
			.map(|part| part.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().unwrap())
			.chain(std::iter::once("virtual_transaction")).collect::<BTreeSet<_>>();
		assert_eq!(tables.len(), 29, "financial table allowlist changed");
		for table in tables {
			admin.batch_execute(&format!("GRANT SELECT ON public.{table} TO {role}")).await.unwrap();
		}
		drop(admin);
		let mut cfg = Db::config(&config.name, &config);
		cfg.user(&role).password(password);
		let (reader, connection) = cfg.connect(NoTls).await.expect("independent SELECT-only role must connect");
		tokio::spawn(async move { connection.await.expect("evidence reader connection failed"); });
		let mut ret = Self { dir, source_incarnation: Uuid::new_v4().to_string(),
			observer_incarnation: Uuid::new_v4().to_string(), debug: None, captures: vec![],
			owned: BTreeSet::new(), configurations: vec![], baseline: None, final_sql: None, reader };
		ret.configuration(srv, "baseline");
		ret
	}

	pub(super) fn action(&self, ctx: &TestContext, kind: &str, receipt: Value) {
		let observed = tip(ctx);
		let mut file = OpenOptions::new().create(true).append(true)
			.open(self.dir.join("actions.jsonl")).unwrap();
		serde_json::to_writer(&mut file, &json!({"id": Uuid::new_v4().to_string(),
			"kind": kind, "captured_at": Utc::now().to_rfc3339(),
			"height": observed["blocks"], "hash": observed["bestblockhash"], "receipt": receipt})).unwrap();
		file.write_all(b"\n").unwrap();
		file.sync_all().unwrap();
	}

	fn configuration(&mut self, srv: &Captaind, phase: &str) {
		let cfg = srv.config();
		let admitted = server::config::Config::load(cfg.data_dir.join("config.toml")).unwrap();
		assert!(admitted.cln_array.is_empty(), "this fixture admits no Lightning nodes");
		assert_eq!(admitted.network, cfg.network);
		let raw = json!({"network": admitted.network, "cln_array": []});
		let file = format!("public-config-{}.json", self.configurations.len());
		write_json(self.dir.join(&file), &raw);
		let bytes = fs::read(self.dir.join(&file)).unwrap();
		self.configurations.push(json!({"sequence": self.configurations.len(), "phase": phase,
			"captured_at": Utc::now().to_rfc3339(), "raw_public_config": raw,
			"source_receipt": {"file": file, "sha256": sha256::Hash::hash(&bytes).to_string()}}));
	}

	pub(super) async fn sql(&mut self, phase: &str) -> Value {
		let tx = self.reader.build_transaction().isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
			.read_only(true).start().await.unwrap();
		let privileges = query_json(&tx, include_str!("reader-privileges.sql")).await;
		for name in ["superuser", "createrole", "createdb", "inherit", "replication", "bypassrls",
			"database_create", "database_temp", "schema_create", "table_write", "sequence_rights",
			"owned_objects", "role_memberships", "public_function_execute"] {
			assert_eq!(privileges[name], false, "evidence role has effective privilege {name}");
		}
		assert_eq!(privileges["role"], privileges["session_role"], "SET ROLE is not independent admission");
		let financial = query_json(&tx, FINANCIAL).await;
		let paths = query_json(&tx, PATHS).await;
		let receipt = json!({"same_read_only_snapshot": true, "snapshot": financial["snapshot"],
			"captured_at": financial["captured_at"], "transaction_read_only": financial["transaction_read_only"],
			"isolation": financial["isolation"], "effective_privileges": privileges,
			"financial_sql_sha256": sha256::Hash::hash(FINANCIAL.as_bytes()).to_string(),
			"paths_sql_sha256": sha256::Hash::hash(PATHS.as_bytes()).to_string()});
		tx.rollback().await.unwrap();
		write_json(self.dir.join(format!("db-{phase}.json")), &financial);
		write_json(self.dir.join(format!("raw-paths-{phase}.json")), &paths);
		write_json(self.dir.join(format!("sql-acquisition-{phase}.json")), &receipt);
		if phase == "baseline" { self.baseline = Some(financial.clone()); }
		if phase == "final" { self.final_sql = Some(financial.clone()); }
		financial
	}

	pub(super) async fn source(&mut self, ctx: &TestContext, wallet: &Bark, phase: &str) {
		let before = tip(ctx);
		let debug: Value = wallet.run_json(["debug-info"]).await;
		let movements: Value = wallet.run_json(["history", "--no-sync"]).await;
		let vtxos: Value = wallet.run_json(["vtxos", "--all", "--no-sync"]).await;
		let balance: Value = wallet.run_json(["balance", "--no-sync"]).await;
		let utxos: Value = wallet.run_json(["onchain", "utxos", "--no-sync"]).await;
		assert_eq!(before["bestblockhash"], tip(ctx)["bestblockhash"]);
		if let Some(previous) = &self.debug { assert_eq!(*previous, debug); }
		self.debug = Some(debug);
		for row in vtxos.as_array().unwrap() { self.owned.insert(row["id"].as_str().unwrap().to_owned()); }
		let capture = json!({"sequence": self.captures.len(), "phase": phase,
			"captured_at": Utc::now().to_rfc3339(), "height": before["blocks"], "hash": before["bestblockhash"],
			"history_complete": true, "vtxos_all": true, "movements": movements,
			"raw_vtxos": vtxos, "raw_balance": balance, "onchain_utxos": utxos});
		write_json(self.dir.join(format!("source-{}.json", self.captures.len())), &capture);
		self.captures.push(capture);
	}

	pub(super) fn source_history(&self) {
		write_json(self.dir.join("wallet-history.json"), &json!({"customer": {
			"origin": "original-source-wallet", "incarnation": self.source_incarnation,
			"raw_debug": self.debug, "history_complete": true, "vtxos_all": true,
			"owned_vtxo_ids": self.owned, "captures": self.captures}}));
	}

	pub(super) async fn right(&self, ctx: &TestContext, source: &Vtxo<Full>, movement_id: u32, child_index: u32) {
		let anchor = ctx.bitcoind().await_transaction(source.chain_anchor().txid).await;
		source.validate(&anchor).expect("original source signed lineage must validate before absence");
		let xpub: Xpub = serde_json::from_value(self.debug.as_ref().unwrap()["vtxo_xpub"].clone()).unwrap();
		assert_eq!(xpub.derive_pub(&SECP, &[child_index.into()]).unwrap().public_key,
			source.user_pubkey(), "the stored source child must belong to the original public identity");
		let observed = tip(ctx);
		write_json(self.dir.join("original-right.json"), &json!({"wallet": "customer",
			"source_incarnation": self.source_incarnation, "vtxo_id": source.id(),
			"captured_at": Utc::now().to_rfc3339(), "height": observed["blocks"], "hash": observed["bestblockhash"],
			"amount_sat": source.amount().to_sat(), "expiry_height": source.expiry_height().to_u32(),
			"board_movement_id": movement_id, "user_pubkey": source.user_pubkey(),
			"child_index": child_index, "original_vtxo_xpub": xpub,
			"child_index_origin": "linked SDK lookup of the original stored user key before absence; public derivation verified",
			"signed_vtxo_hex": source.serialize_hex(), "chain_anchor_hex": serialize_hex(&anchor),
			"transactions": source.transactions().map(|item| json!({"txid": item.tx.compute_txid(),
				"hex": serialize_hex(&item.tx)})).collect::<Vec<_>>() }));
	}

	pub(super) async fn source_descriptor(&self, ctx: &TestContext, wallet: &Bark) {
		let onchain = wallet.onchain_client().await;
		write_json(self.dir.join("original-onchain-derivation.json"), &public_derivation(ctx,
			&onchain.inner.public_descriptor(KeychainKind::External).to_string(),
			onchain.inner.derivation_index(KeychainKind::External)));
	}

	pub(super) async fn operator_descriptors(&self, ctx: &TestContext, srv: &Captaind, db: &Db) {
		let mnemonic = server::wallet::read_mnemonic_from_datadir(&srv.config().data_dir).unwrap();
		let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
		let mut records = vec![];
		for kind in [WalletKind::Rounds, WalletKind::Watchman] {
			let account = master.derive_priv(&SECP, &[kind.child_number()]).unwrap();
			let descriptor = format!("tr({}/0/*)", Xpub::from_priv(&SECP, &account));
			let changeset = db.read(async |tx| tx.read_aggregate_changeset(kind).await).await.unwrap()
				.expect("running operator wallet must have creation state");
			let wallet = Wallet::load().descriptor(KeychainKind::External, Some(descriptor.clone()))
				.check_network(Network::Regtest).load_wallet_no_persist(changeset).unwrap().unwrap();
			records.push(json!({"wallet": kind.name(), "derivation": public_derivation(ctx,
				&descriptor, wallet.derivation_index(KeychainKind::External))}));
		}
		write_json(self.dir.join("operator-derivations.json"), &json!(records));
	}

	pub(super) async fn observer(
		&self, ctx: &TestContext, restored: &Barkd, seed: &bip39::Mnemonic,
		destination: &ark::fallback::FallbackRecord,
	) {
		let before = tip(ctx);
		let config = restored.client_config();
		let raw_debug_origin = "linked SDK open of the stopped fresh REST-created database; not a GNU debug endpoint";
		let raw_response = async |path: &str, name: &str| -> Value {
			let response = config.client.get(format!("{}{}", config.base_path, path))
				.send().await.unwrap().error_for_status().unwrap();
			let bytes = response.bytes().await.unwrap();
			let mut file = OpenOptions::new().write(true).create_new(true)
				.open(self.dir.join(format!("fresh-rest-{name}.json"))).unwrap();
			file.write_all(&bytes).unwrap();
			file.sync_all().unwrap();
			serde_json::from_slice(&bytes).unwrap()
		};
		let raw_balance = raw_response("/api/v1/wallet/balance", "balance").await;
		let raw_vtxos = raw_response("/api/v1/wallet/vtxos?all=true", "vtxos").await;
		let movements = raw_response("/api/v1/history", "history").await;
		let onchain_utxos = raw_response("/api/v1/onchain/utxos", "utxos").await;
		let registration = raw_response("/api/v1/wallet/fallback", "fallback").await;
		restored.stop().await.unwrap();
		let cfg: bark::Config = toml::from_str(&fs::read_to_string(restored.datadir().join("config.toml")).unwrap()).unwrap();
		let reader = bark::Wallet::open(Network::Regtest, WalletSeed::new_from_mnemonic(Network::Regtest, seed), cfg,
			OpenWalletArgs { datadir: Some(restored.datadir()), run_daemon: false,
				lock_manager: Some(Box::new(MemoryLockManager::new())), ..Default::default() }).await.unwrap();
		let debug = serde_json::to_value(bark_json::cli::DebugInfo::from(reader.debug_info().await.unwrap())).unwrap();
		assert_eq!(self.debug.as_ref().unwrap(), &debug, "fresh seed identity must match original identity");
		let fallback = reader.fallback_destination().await.unwrap();
		assert_eq!(&fallback, destination);
		fallback.verify(Network::Regtest, serde_json::from_value(debug["mailbox_id"].clone()).unwrap()).unwrap();
		drop(reader);
		let db = Arc::new(SqliteClient::open(restored.datadir().join("db.sqlite")).unwrap());
		let onchain = OnchainWallet::load_or_create(Network::Regtest, seed.to_seed(""), db).await.unwrap();
		// Build the same BIP86 public template independently from the retained
		// seed, then compare it to the fresh database's actual public descriptor.
		let master = Xpriv::new_master(Network::Regtest, &seed.to_seed("")).unwrap();
		let (descriptor, _private_keys, _) = Bip86(master, KeychainKind::External).build(Network::Regtest.into()).unwrap();
		assert_eq!(descriptor, *onchain.inner.public_descriptor(KeychainKind::External));
		let derivation = public_derivation(ctx, &descriptor.to_string(), onchain.inner.derivation_index(KeychainKind::External));
		assert_eq!(before["bestblockhash"], tip(ctx)["bestblockhash"]);
		write_json(self.dir.join("fresh-custody-before-drain.json"), &json!({"origin": "fresh-own-seed",
			"incarnation": self.observer_incarnation, "captured_at": Utc::now().to_rfc3339(),
			"birth_height": 0, "height": before["blocks"], "hash": before["bestblockhash"],
			"history_complete": true, "vtxos_all": true, "raw_debug": debug,
			"raw_debug_origin": raw_debug_origin, "raw_balance": raw_balance,
			"raw_vtxos": raw_vtxos, "movements": movements, "onchain_utxos": onchain_utxos,
			"original_owned_vtxo_ids": self.owned, "onchain_derivation": derivation,
			"fallback_registration": registration, "signed_fallback_record": fallback}));
	}

	pub(super) fn chain(&self, ctx: &TestContext, phase: &str) {
		let rpc = ctx.bitcoind().sync_client();
		let before = tip(ctx);
		let mut blocks = vec![];
		for height in 0..=before["blocks"].as_u64().unwrap() {
			let hash: Value = rpc.call("getblockhash", &[json!(height)]).unwrap();
			let mut block: Value = rpc.call("getblock", &[hash.clone(), json!(2)]).unwrap();
			let header: Value = rpc.call("getblockheader", &[hash, json!(false)]).unwrap();
			block["header"] = header;
			if height == 0 { block["previousblockhash"] = json!("00".repeat(32)); }
			assert!(block["tx"].as_array().unwrap().iter().all(|tx| tx["hex"].is_string()));
			blocks.push(block);
		}
		let after = tip(ctx);
		assert_eq!(before["bestblockhash"], after["bestblockhash"], "restart acquisition if canonical tip moved");
		let mempool: Vec<bitcoin::Txid> = rpc.get_raw_mempool().unwrap();
		let transactions = mempool.into_iter().map(|txid|
			rpc.call::<Value>("getrawtransaction", &[json!(txid), json!(true)]).unwrap()).collect::<Vec<_>>();
		write_json(self.dir.join(format!("chain-{phase}.json")), &json!({"before": before, "after": after,
			"scan_start_height": 0, "blocks": blocks, "previous_transactions": [], "mempool_transactions": transactions}));
	}

	pub(super) fn transaction(&self, ctx: &TestContext, phase: &str, tx: &Transaction) {
		let rpc = ctx.bitcoind().sync_client();
		let raw: Value = rpc.call("getrawtransaction", &[json!(tx.compute_txid()), json!(true)]).unwrap();
		let previous = tx.input.iter().map(|input| rpc.call::<Value>("getrawtransaction",
			&[json!(input.previous_output.txid), json!(true)]).unwrap()).collect::<Vec<_>>();
		write_json(self.dir.join(format!("transaction-{phase}.json")), &json!({"raw": raw, "previous_transactions": previous}));
	}

	pub(super) fn finish(&mut self, ctx: &TestContext, srv: &Captaind) {
		self.configuration(srv, "final");
		let mut executable_receipts = vec![];
		collect_executables(&ctx.datadir, &ctx.datadir, &mut executable_receipts);
		write_json(self.dir.join("actual-executable-receipts.json"), &json!(executable_receipts));
		write_json(self.dir.join("lightning-admission.json"), &json!({"schema": 1,
			"origin": "original-admitted-public-configuration", "network": "regtest",
			"configuration_history_complete": true, "baseline_snapshot": self.baseline.as_ref().unwrap()["snapshot"],
			"final_snapshot": self.final_sql.as_ref().unwrap()["snapshot"], "configurations": self.configurations}));
		write_json(self.dir.join("cln.json"), &json!({}));
		write_json(self.dir.join("acquisition-status.json"), &json!({"schema": 1,
			"status": "EVIDENCE_INCOMPLETE", "scope": "board through absence, fallback and fresh seed custody; diagnostic acquisitions",
			"unsupported": ["absent original source has no native fallback debit; original-right-to-payout adapter remains unsupported",
				"signed ownership and operator financial reconciliation require independent checking",
				"acquired raw receipts are not an assembled complete accounting qualification bundle",
				"binary candidate/build-kind admission must be bound to actual retained executable observations; unavailable is incomplete"],
			"original_source_reopened_after_absence": false}));
	}
}

fn collect_executables(root: &Path, directory: &Path, receipts: &mut Vec<Value>) {
	let mut entries = fs::read_dir(directory).unwrap().map(|entry| entry.unwrap()).collect::<Vec<_>>();
	entries.sort_by_key(|entry| entry.file_name());
	for entry in entries {
		let path = entry.path();
		let kind = entry.file_type().unwrap();
		if kind.is_dir() { collect_executables(root, &path, receipts); }
		if kind.is_file() && (entry.file_name() == "executable-receipt.json"
			|| path.parent().unwrap().file_name().is_some_and(|name| name == "executable-receipts")) {
			let raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
			receipts.push(json!({"origin_receipt": path.strip_prefix(root).unwrap(), "raw": raw}));
		}
	}
}

async fn query_json(tx: &tokio_postgres::Transaction<'_>, sql: &str) -> Value {
	let rows = tx.simple_query(sql).await.unwrap();
	let values = rows.iter().filter_map(|row| match row {
		SimpleQueryMessage::Row(row) => row.get(0), _ => None,
	}).collect::<Vec<_>>();
	assert_eq!(values.len(), 1, "one original SQL result is required");
	serde_json::from_str(values[0]).context("public SQL result is JSON").unwrap()
}
