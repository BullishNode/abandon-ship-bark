// Shared test-only workload port of the preserved scale harness.
//
// The fixed declaration/native runner freezes these test sources and binaries.
// Parameters come from the predeclared `STAGE4_*` profile variables,
// raw measurements go to `STAGE4_OUT`. The runner evaluates the predeclared
// thresholds from those files; this test asserts correctness only.
//
// Every entitlement is genuinely funded: Core-funded boards (a finalized
// PSBT from an external wallet, signed into Ark by the owner), each owner's
// own delegated refresh of a boarded coin, real arkoor sends from funders,
// and real Lightning payments through two CLN nodes. No coin, sweep, payment or fee is written
// into the database by this harness.
//
// Each owner has its own seed and wallet, signs its own fallback record and
// individual policy-key evidence, and is absent (its wallet dropped) before its coins expire.
// Owners are not concurrent daemons; 32 separate active users refresh
// concurrently from before the cohort's expiry blocks are mined until every
// payout confirms, so direct backing claims and the payout backlog overlap them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use rand::SeedableRng;
use rand::seq::SliceRandom;
use serde_json::{json, Value};
use tokio::sync::Notify;

use ark::{ProtocolEncoding, Vtxo, VtxoId, VtxoRequest};
use ark::arkoor::ArkoorDestination;
use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::{PaymentHash, Preimage};
use ark_testing::{Bitcoind, Captaind, TestContext, sat};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use ark_testing::daemon::captaind::{ArkClient, proxy::ArkRpcProxy};
use bark::{BarkNetwork, OpenWalletArgs, WalletSeed};
use bark::onchain::OnchainWallet;
use bark::persist::BarkPersister;
use bark::persist::sqlite::SqliteClient;
use bark::round::RoundParticipation;
use bdk_wallet::{KeychainKind, SignOptions};
use bdk_wallet::template::Bip84;
use bitcoin::{Address, Amount, FeeRate, Network, OutPoint, Psbt, ScriptBuf, Transaction, Txid, Witness};
use bitcoin::bip32::{DerivationPath, Xpriv, Xpub};
use bitcoin::secp256k1::{PublicKey, Secp256k1};
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::RpcApi;
use cln_rpc::plugins::hold;
use server::database::Db;
use server_rpc::protos;
use ark_testing::util::FutureExt;

// ── parameters ────────────────────────────────────────────────────────────

fn env_u64(name: &str, default: u64) -> u64 {
	std::env::var(name).ok().map(|v| v.parse().unwrap_or_else(|_| panic!("{name}={v} is not a number")))
		.unwrap_or(default)
}

#[derive(Clone, Debug, serde::Serialize)]
struct Params {
	workload_seed:u64,
	/// Owners per profile: registered board, pending board, unregistered
	/// arkoor, failed Lightning send, Lightning receive with a lost claim reply.
	registered: usize,
	pending: usize,
	unregistered: usize,
	ln_send: usize,
	ln_receive: usize,
	active_users: usize,
	batch: usize,
	concurrency: usize,
	round_chunk: usize,
	baseline_cycles: usize,
	loaded_min_cycles: usize,
	block_interval_s: u64,
	round_interval_s: u64,
	payout_interval_s: u64,
	vtxo_lifetime: u32,
	server_funding_sat: u64,
	refresh_deadline_s: u64,
	funding_deadline_s: u64,
	loaded_deadline_s: u64,
	recovery_deadline_s: u64,
	// Entitlement amounts, distinct per source.
	round_sat: u64,
	arkoor_sat: u64,
	board_sat: u64,
	pending_sat: u64,
	unregistered_sat: u64,
	ln_send_board_sat: u64,
	ln_send_sat: u64,
	ln_receive_sat: u64,
	active_board_sat: u64,
}

impl Params {
	fn from_env() -> Params {
		let p = Params {
			workload_seed:env_u64("STAGE5_SEED",20261014),
			registered: env_u64("STAGE4_REGISTERED", 12) as usize,
			pending: env_u64("STAGE4_PENDING", 3) as usize,
			unregistered: env_u64("STAGE4_UNREGISTERED", 3) as usize,
			ln_send: env_u64("STAGE4_LN_SEND", 1) as usize,
			ln_receive: env_u64("STAGE4_LN_RECEIVE", 1) as usize,
			active_users: env_u64("STAGE4_ACTIVE", 4) as usize,
			batch: env_u64("STAGE4_BATCH", 200) as usize,
			concurrency: env_u64("STAGE4_CONCURRENCY", 16) as usize,
			round_chunk: env_u64("STAGE4_ROUND_CHUNK", 250) as usize,
			baseline_cycles: env_u64("STAGE4_BASELINE_CYCLES", 3) as usize,
			loaded_min_cycles: env_u64("STAGE4_LOADED_MIN_CYCLES", 3) as usize,
			block_interval_s: env_u64("STAGE4_BLOCK_INTERVAL", 10),
			round_interval_s: env_u64("STAGE4_ROUND_INTERVAL", 10),
			payout_interval_s: env_u64("STAGE4_PAYOUT_INTERVAL", 1),
			vtxo_lifetime: env_u64("STAGE4_VTXO_LIFETIME", 432) as u32,
			server_funding_sat: env_u64("STAGE4_SERVER_FUNDING_SAT", 1_000_000_000),
			refresh_deadline_s: env_u64("STAGE4_REFRESH_DEADLINE", 300),
			funding_deadline_s: env_u64("STAGE4_FUNDING_DEADLINE", 3 * 3600),
			loaded_deadline_s: env_u64("STAGE4_LOADED_DEADLINE", 3600),
			recovery_deadline_s: env_u64("STAGE4_RECOVERY_DEADLINE", 3600),
			round_sat: 21_000,
			arkoor_sat: 13_000,
			board_sat: 25_000,
			pending_sat: 27_000,
			unregistered_sat: 17_000,
			ln_send_board_sat: 60_000,
			ln_send_sat: 40_000,
			ln_receive_sat: 30_000,
			active_board_sat: 200_000,
		};
		assert!(p.owners() >= 2, "at least two owners: an unregistered send pays another owner");
		p
	}

	fn owners(&self) -> usize {
		self.registered + self.pending + self.unregistered + self.ln_send + self.ln_receive
	}

	fn lightning(&self) -> bool {
		self.ln_send + self.ln_receive > 0
	}
}

// No numeric COMMON allowance is invented by this workload. The root-frozen
// document/policy and workload bytes must exist before the fresh native create;
// native944 independently verifies actual Core network/genesis and exact fields.
#[derive(Clone)]
struct ScaleFeeAuthority { bytes: Vec<u8>, policy: server::watchman::ordinary_fee::OrdinaryFeePolicy }
impl ScaleFeeAuthority {
	fn from_env() -> Self {
		use bitcoin::hashes::{Hash,sha256};
		let document=std::fs::read(std::env::var("SCALE_COMMON_REGTEST_DECLARATION").expect("finite COMMON document required")).unwrap();
		assert!(!document.is_empty() && document.len()<=4096);
		let policy_bytes=std::fs::read(std::env::var("SCALE_COMMON_REGTEST_POLICY").expect("exact COMMON policy required")).unwrap();
		assert!(policy_bytes.len()<=4096);
		let policy:server::watchman::ordinary_fee::OrdinaryFeePolicy=serde_json::from_slice(&policy_bytes).unwrap();
		let marker=policy.regtest_qualification.expect("no live-policy override or unlimited allowance");
		assert_eq!(marker.declaration_sha256,sha256::Hash::hash(&document));
		let declared:Value=serde_json::from_slice(&document).unwrap();
		let workload=std::fs::read(std::env::var("SCALE_COMMON_WORKLOAD_MANIFEST").expect("frozen COMMON workload required")).unwrap();
		assert!(workload.len()<=1024*1024);
		assert_eq!(declared["workload_sha256"].as_str().unwrap(),sha256::Hash::hash(&workload).to_string());
		Self{bytes:document,policy}
	}
	fn apply(&self,c:&mut server::Config) {
		assert!(!c.expiry_payout.enabled);
		c.direct_expiry.enabled=true; c.direct_expiry.interval=Duration::from_secs(3600);
		c.direct_expiry.grace_blocks=6;
		c.direct_expiry.regtest_qualification_fee_policy=Some(self.policy);
	}
}
fn scale_builder<'a>(builder:ark_testing::context::builders::CaptaindBuilder<'a>,ctx:&TestContext,authority:&ScaleFeeAuthority)->ark_testing::context::builders::CaptaindBuilder<'a> {
	let policy=authority.policy;let history=ctx.datadir.join("server");
	let ceiling=policy.regtest_qualification.unwrap().per_attempt_ceiling_sat;
	builder.regtest_fee_declaration(authority.bytes.clone()).watchmand_cfg(move|c| {
		c.watchman.direct_expiry=Some(server::watchman::DirectExpiryConfig{ordinary_fee_policy:policy,history_dir:history,max_miner_fee:sat(ceiling)});
	})
}

// ── output files ──────────────────────────────────────────────────────────

struct Out {
	dir: PathBuf,
	lock: Mutex<()>,
}

impl Out {
	fn new() -> Arc<Out> {
		let dir = PathBuf::from(std::env::var("STAGE4_OUT").expect("STAGE4_OUT must name the run's output directory"));
		std::fs::create_dir_all(&dir).unwrap();
		Arc::new(Out { dir, lock: Mutex::new(()) })
	}

	fn json(&self, name: &str, value: &Value) {
		std::fs::write(self.dir.join(name), serde_json::to_string_pretty(value).unwrap() + "\n").unwrap();
	}

	fn line(&self, name: &str, value: &Value) {
		let _guard = self.lock.lock().unwrap();
		let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join(name)).unwrap();
		writeln!(f, "{}", serde_json::to_string(value).unwrap()).unwrap();
	}

	fn event(&self, what: &str, data: Value) {
		let mut v = json!({ "utc": utc(), "event": what });
		if let (Some(m), Value::Object(d)) = (v.as_object_mut(), data) { m.extend(d); }
		println!("stage4 {}", serde_json::to_string(&v).unwrap());
		self.line("events.jsonl", &v);
	}
}

fn utc() -> f64 {
	SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}



// ── environment shared by spawned tasks ──────────────────────────────────

#[derive(Clone)]
struct Env {
	bitcoind: Arc<Bitcoind>,
	datadir: PathBuf,
	cfg: bark::Config,
	db: Db,
	out: Arc<Out>,
	ln_client: Option<cln_rpc::ClnGrpcClient>,
	hold_client:Option<hold::hold_client::HoldClient<tonic::transport::Channel>>,
}

impl Env {
	fn core(&self) -> bitcoin_ext::rpc::BitcoinRpcClient { self.bitcoind.sync_client() }

	/// Open (or create) a wallet with its own seed and sqlite database, like
	/// the SDK builder, without a background daemon.
	async fn open_wallet(&self, name: &str, mnemonic: &bip39::Mnemonic, server: &str, create: bool) -> bark::Wallet {
		let mut cfg = self.cfg.clone();
		cfg.server_address = server.to_owned();
		cfg.daemon_manual_sync = true;
		let datadir = self.datadir.join(name);
		std::fs::create_dir_all(&datadir).unwrap();
		if create {
			if !name.starts_with("owner-") {
				let master=Xpriv::new_master(Network::Regtest,&mnemonic.to_seed("")).unwrap();
				let account=master.derive_priv(&Secp256k1::new(),&"m/84'/1'/0'".parse::<DerivationPath>().unwrap()).unwrap();
				let xpub=Xpub::from_priv(&Secp256k1::new(),&account);
				self.out.line("native-public-descriptors.jsonl",&json!({"account":name,"utc":utc(),"descriptors":[format!("wpkh({xpub}/0/*)"),format!("wpkh({xpub}/1/*)")],"range":[0,19]}));
			}
			std::fs::write(datadir.join("mnemonic"), mnemonic.to_string()).unwrap();
			std::fs::write(datadir.join("config.toml"), toml::to_string_pretty(&cfg).unwrap()).unwrap();
		}
		let network = BarkNetwork::Regtest.as_bitcoin();
		let db: Arc<dyn BarkPersister> = Arc::new(SqliteClient::open(datadir.join("db.sqlite")).unwrap());
		let onchain = Arc::new(tokio::sync::RwLock::new(
			OnchainWallet::load_or_create(network, mnemonic.to_seed(""), db.clone()).await.unwrap()));
		bark::Wallet::open(network, WalletSeed::new_from_mnemonic(network, mnemonic), cfg, OpenWalletArgs {
			persister: Some(db), datadir: Some(datadir), create_if_not_exists: create, onchain: Some(onchain),
			run_daemon: false, ..Default::default()
		}).await.unwrap_or_else(|e| panic!("open wallet {name}: {e:#}"))
	}

	/// An external wallet (Core) funds `address` with a finalized transaction.
	fn funded_psbt(&self, address: &Address, amount: Amount) -> Psbt {
		let core = self.core();
		let mut outputs = serde_json::Map::new();
		outputs.insert(address.to_string(), json!(amount.to_btc()));
		let funded: Value = core.call("walletcreatefundedpsbt", &[
			json!([]), json!([outputs]), 0.into(),
			json!({ "fee_rate": 3, "lockUnspents": true, "minconf": 1 }),
		]).expect("Core funds the board");
		let signed: Value = core.call("walletprocesspsbt", &[funded["psbt"].clone(), true.into()]).unwrap();
		let finalized: Value = core.call("finalizepsbt", &[signed["psbt"].clone(), true.into()]).unwrap();
		assert_eq!(finalized["complete"], true);
		let tx: Transaction = bitcoin::consensus::encode::deserialize_hex(finalized["hex"].as_str().unwrap()).unwrap();
		let mut unsigned = tx.clone();
		for input in &mut unsigned.input { input.witness = Witness::new(); input.script_sig = ScriptBuf::new(); }
		let mut psbt = Psbt::from_unsigned_tx(unsigned).unwrap();
		for (i, input) in tx.input.iter().enumerate() {
			let prev: Transaction = core.get_raw_transaction(&input.previous_output.txid, None).unwrap();
			psbt.inputs[i].witness_utxo = Some(prev.output[input.previous_output.vout as usize].clone());
			psbt.inputs[i].final_script_witness = Some(input.witness.clone());
		}
		psbt
	}

	/// Board `amount` from Core into `wallet`, with a genuinely allocated individually linked policy key.
	async fn board(&self, wallet: &bark::Wallet, amount: u64) -> anyhow::Result<bark::persist::models::PendingBoard> {
		let (key, index) = wallet.derive_store_next_keypair().await?;
		let (address, expiry) = wallet.board_funding_address(&key).await?;
		let psbt = self.funded_psbt(&address, sat(amount));
		self.out.line("funding-intents.jsonl",&json!({"utc":utc(),"fingerprint":wallet.fingerprint().to_string(),
			"txid":psbt.unsigned_tx.compute_txid(),"user_pubkey":key.public_key().to_string(),"child_index":index,"amount_sat":amount,"bank":"included Core fixture bank","destination":"included actual protocol backing"}));
		wallet.board_psbt(psbt, key, expiry).await
	}

	async fn db_size(&self) -> i64 {
		self.db.read(async |t| Ok(t.query_one("SELECT pg_database_size(current_database())", &[])
			.await?.get::<_, i64>(0))).await.unwrap()
	}

	async fn pg_stats(&self) -> Value {
		let row = self.db.read(async |t| Ok(t.query_one(
			"SELECT xact_commit, xact_rollback, tup_returned, tup_fetched, tup_inserted, tup_updated,
				tup_deleted, blks_read, blks_hit FROM pg_stat_database WHERE datname=current_database()", &[],
		).await?)).await.unwrap();
		let names = ["xact_commit", "xact_rollback", "tup_returned", "tup_fetched", "tup_inserted",
			"tup_updated", "tup_deleted", "blks_read", "blks_hit"];
		let mut db = serde_json::Map::new();
		for (i, n) in names.iter().enumerate() { db.insert(n.to_string(), json!(row.get::<_, i64>(i))); }
		let tables = self.db.read(async |t| Ok(t.query(
			"SELECT relname::TEXT, coalesce(seq_scan,0), coalesce(seq_tup_read,0), coalesce(idx_scan,0),
				coalesce(n_tup_ins,0), coalesce(n_tup_upd,0), n_live_tup FROM pg_stat_user_tables", &[],
		).await?)).await.unwrap();
		let mut t = serde_json::Map::new();
		for r in tables {
			t.insert(r.get::<_, String>(0), json!({
				"seq_scan": r.get::<_, i64>(1), "seq_tup_read": r.get::<_, i64>(2), "idx_scan": r.get::<_, i64>(3),
				"n_tup_ins": r.get::<_, i64>(4), "n_tup_upd": r.get::<_, i64>(5), "n_live_tup": r.get::<_, i64>(6),
			}));
		}
		json!({ "database": db, "tables": t })
	}
}

/// Phase boundaries: time, tip, database size, SQL counters, capital.
async fn checkpoint(env: &Env, srv: Option<&Captaind>, phase: &str, edge: &str) {
	let core = env.core();
	let balance: Value = core.call("getbalances", &[]).unwrap();
	let v = json!({
		"utc": utc(), "phase": phase, "edge": edge,
		"tip": core.get_block_count().unwrap(),
		"db_bytes": env.db_size().await,
		"pg": env.pg_stats().await,
		"rounds_wallet": match srv { Some(s) => read_owning_wallets(s).await, None => Value::Null },
		"core_wallet": balance["mine"],
	});
	env.out.line("phases.jsonl", &v);
	env.out.event("phase", json!({ "phase": phase, "edge": edge }));
}

// ── owners and their entitlements ────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
enum Profile { Registered, Pending, Unregistered, LnSend, LnReceive }

#[derive(Clone, Debug, serde::Serialize)]
struct Coin {
	id: String,
	amount: u64,
	source: &'static str,
	user_pubkey: String,
}

struct Owner {
	index: usize,
	profile: Profile,
	cohort: &'static str,
	name: String,
	spk: ScriptBuf,
	mailbox_pk: String,
	/// Addresses for the arkoor receive, and a spare one that another
	/// owner's unregistered send pays.
	arkoor_address: ark::Address,
	gift_address: ark::Address,
	coins: Vec<Coin>,
	/// Full coins whose actual funding paths must be consumed by direct claims.
	anchors: Vec<Vtxo>,
}

impl Owner {
	fn public(&self) -> Value {
		json!({
			"index": self.index, "profile": self.profile, "cohort":self.cohort,"wallet": self.name,
			"spk": self.spk.to_hex_string(), "mailbox_pk": self.mailbox_pk,
			"keys": [self.arkoor_address.policy().user_pubkey().to_string(),
				self.gift_address.policy().user_pubkey().to_string()],
			"coins": self.coins,
			"physical_paths":self.anchors.iter().map(|v|json!({"id":v.id(),"root":v.chain_anchor(),"expiry_height":v.expiry_height().to_u32(),
				"nodes":v.transactions().map(|n|json!({"outpoint":OutPoint::new(n.tx.compute_txid(),u32::try_from(n.output_idx).unwrap()),"inputs":n.tx.input.iter().map(|i|i.previous_output.to_string()).collect::<Vec<_>>(),"outputs":n.tx.output.iter().map(|o|json!({"value_sat":o.value.to_sat(),"script":o.script_pubkey.to_hex_string()})).collect::<Vec<_>>() })).collect::<Vec<_>>() })).collect::<Vec<_>>(),
		})
	}
}

fn profile_of(p: &Params, i: usize) -> Profile {
	let mut profiles=Vec::new();
	for (profile,n) in [(Profile::Registered,p.registered),(Profile::Pending,p.pending),
		(Profile::Unregistered,p.unregistered),(Profile::LnSend,p.ln_send),(Profile::LnReceive,p.ln_receive)] {
		profiles.extend(std::iter::repeat_n(profile,n));
	}
	profiles.shuffle(&mut rand::rngs::StdRng::seed_from_u64(p.workload_seed));
	profiles[i]
}

/// Create an owner with one immutable signed record and separately linked policy keys.
async fn create_owner(env: Env, index: usize, profile: Profile, server: String) -> (Owner, bark::Wallet) {
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let name = format!("owner-{index:05}");
	let wallet = env.open_wallet(&name, &mnemonic, &server, true).await;
	let record = wallet.fallback_destination().await.unwrap();
	let seed_master=Xpriv::new_master(Network::Regtest,&mnemonic.to_seed("")).unwrap();
	let onchain_account=seed_master.derive_priv(&Secp256k1::new(),&"m/84'/1'/0'".parse::<DerivationPath>().unwrap()).unwrap();
	let public_onchain=Xpub::from_priv(&Secp256k1::new(),&onchain_account);
	env.out.line("native-public-descriptors.jsonl",&json!({"owner":index,"account":format!("actor-{index}"),
		"descriptors":[format!("wpkh({public_onchain}/0/*)"),format!("wpkh({public_onchain}/1/*)")],"range":[0,19],"allocated_payout_index":0}));
	record.verify(Network::Regtest,wallet.mailbox_keypair().public_key()).unwrap();
	env.out.line("per-key-bindings.jsonl", &json!({"owner":index,"utc":utc(),"wallet":name,"fingerprint":wallet.fingerprint().to_string(),"mailbox_pk":wallet.mailbox_keypair().public_key().to_string(),"spk":record.spk.to_hex_string(),"immutable":true,"record_version":4,"record":record.serialize_hex()}));
	let arkoor_address = wallet.new_address().await.unwrap();
	let gift_address = wallet.new_address().await.unwrap();
	for address in [&arkoor_address,&gift_address] {
		let key=address.policy().user_pubkey();let link=wallet.fallback_key_link(key).await.unwrap();
		link.verify(Network::Regtest,wallet.mailbox_keypair().public_key(),&record).unwrap();
		assert_eq!(link.user_pubkey,key);
		let stored=env.db.read(async |t|Ok(t.query_one("SELECT l.sig,l.recovery_index,r.spk,r.mailbox_sig FROM key_link l JOIN fallback_record r ON r.mailbox_pk=l.mailbox_pk WHERE l.user_pubkey=$1 AND l.mailbox_pk=$2",&[&key.serialize().as_slice(),&wallet.mailbox_keypair().public_key().serialize().as_slice()]).await?)).await.unwrap();
		assert_eq!(stored.get::<_,Vec<u8>>(0),link.signature.serialize());
		assert_eq!(stored.get::<_,Option<i32>>(1).map(|i|u32::try_from(i).unwrap()),link.recovery_index);
		assert_eq!(stored.get::<_,Vec<u8>>(2),record.spk.as_bytes());
		assert_eq!(stored.get::<_,Vec<u8>>(3),record.mailbox_signature.serialize());
		env.out.line("per-key-links.jsonl",&json!({"owner":index,"utc":utc(),"user_pubkey":key.to_string(),"link_version":1,"key_link":link.serialize_hex(),"pre_use":true,"durable_bytes_match":true}));
	}
	let owner = Owner {
		index, profile, cohort:"main", name, spk: record.spk,
		mailbox_pk: wallet.mailbox_keypair().public_key().to_string(),
		arkoor_address, gift_address, coins: Vec::new(), anchors: Vec::new(),
	};
	(owner, wallet)
}

async fn wait_spendable(wallet: &bark::Wallet, n: usize, deadline: Duration) -> anyhow::Result<Vec<bark::WalletVtxo>> {
	let end = Instant::now() + deadline;
	loop {
		wallet.sync().await;
		let coins = wallet.spendable_vtxos().await?;
		if coins.len() >= n { return Ok(coins); }
		anyhow::ensure!(Instant::now() < end, "only {} of {n} coins spendable before the deadline", coins.len());
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
}

#[derive(Clone)]
struct DropClaimReply {
	reached: Arc<Notify>,
}

#[async_trait::async_trait]
impl ArkRpcProxy for DropClaimReply {
	/// The claim commits at the server; its reply never reaches the owner,
	/// who keeps its `HtlcsReady` checkpoint and never registers the claim
	/// outputs. The test drops the owner while the call is pending.
	async fn claim_lightning_receive(
		&self, upstream: &mut ArkClient, request: protos::ClaimLightningReceiveRequest,
	) -> Result<protos::ArkoorPackageCosignResponse, tonic::Status> {
		let claimed = upstream.claim_lightning_receive(request).await;
		self.reached.notify_one();
		match claimed {
			Ok(_) => std::future::pending().await,
			Err(e) => Err(e),
		}
	}
}

async fn unregistered_pubkey_outputs(db: &Db) -> BTreeMap<String, u64> {
	db.read(async |t| Ok(t.query(
		"SELECT vtxo_id, amount FROM vtxo WHERE spend_state::TEXT='unregistered' AND policy_type='pubkey'", &[],
	).await?)).await.unwrap().into_iter().map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1) as u64)).collect()
}

async fn full_coins(db: &Db, ids: &[String]) -> Vec<Vtxo> {
	let mut out = Vec::new();
	for chunk in ids.chunks(500) {
		let ids = chunk.iter().map(|s| s.parse::<VtxoId>().unwrap()).collect::<Vec<_>>();
		out.extend(db.read(async |t| t.get_user_vtxos_by_id(&ids).await).await.unwrap().into_iter().map(|v| v.vtxo));
	}
	out
}

// ── the run ───────────────────────────────────────────────────────────────

/// Every active user refreshes `min_cycles` times, then keeps going until
/// `done`. Each refresh is recorded; an error is counted, never retried.
async fn run_refreshes(
	env: &Env, users: &[bark::Wallet], phase: &'static str, min_cycles: usize,
	done: Arc<AtomicBool>, errors: &Arc<AtomicUsize>, p: &Params,
) {
	let tasks = users.iter().enumerate().map(|(u, wallet)| {
		let (wallet, out, done, errors) = (wallet.clone(), env.out.clone(), done.clone(), errors.clone());
		let deadline = Duration::from_secs(p.refresh_deadline_s);
		tokio::spawn(async move {
			let mut cycle = 0;
			while cycle < min_cycles || !done.load(Ordering::Relaxed) {
				let record = refresh_once(&wallet, deadline).await;
				let ok = record["ok"] == true;
				let mut record = record;
				record["phase"] = json!(phase);
				record["user"] = json!(u);
				record["cycle"] = json!(cycle);
				out.line("refreshes.jsonl", &record);
				if !ok {
					errors.fetch_add(1, Ordering::Relaxed);
					// A user without spendable coins cannot continue.
					if wallet.spendable_vtxos().await.map(|v| v.is_empty()).unwrap_or(true) { break; }
				}
				cycle += 1;
			}
		})
	}).collect::<Vec<_>>();
	for t in tasks { t.await.unwrap(); }
}

async fn refresh_once(wallet: &bark::Wallet, deadline: Duration) -> Value {
	let before = match wallet.spendable_vtxos().await {
		Ok(v) => v.into_iter().map(|w| w.vtxo.id()).collect::<Vec<_>>(),
		Err(e) => return json!({ "ok": false, "error": format!("list: {e:#}") }),
	};
	if before.is_empty() { return json!({ "ok": false, "error": "no spendable coins" }); }
	let start_utc = utc();
	let start = Instant::now();
	let result = tokio::time::timeout(deadline, wallet.refresh_vtxos(before.clone())).await;
	let seconds = start.elapsed().as_secs_f64();
	let mut record = json!({ "start_utc": start_utc, "seconds": seconds, "inputs": before.len() });
	match result {
		Ok(Ok(Some(status))) => { record["status"] = json!(format!("{status:?}")); },
		Ok(Ok(None)) => { record["ok"] = json!(false); record["error"] = json!("refresh was a no-op"); return record; },
		Ok(Err(e)) => { record["ok"] = json!(false); record["error"] = json!(format!("{e:#}")); return record; },
		Err(_) => { record["ok"] = json!(false); record["error"] = json!("refresh deadline"); return record; },
	}
	// The cycle completes when the new coin is spendable.
	let end = Instant::now() + deadline;
	loop {
		wallet.sync().await;
		let after = wallet.spendable_vtxos().await.map(|v| v.into_iter().map(|w| w.vtxo.id()).collect::<Vec<_>>())
			.unwrap_or_default();
		if !after.is_empty() && after.iter().all(|id| !before.contains(id)) { record["outputs"]=json!(after.len()); break; }
		if Instant::now() > end {
			record["ok"] = json!(false);
			record["error"] = json!("refreshed coin not spendable before the deadline");
			return record;
		}
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
	record["complete_seconds"] = json!(start.elapsed().as_secs_f64());
	record["ok"] = json!(true);
	record
}

/// Reconcile owner, principal, outputs and fees independently: the
/// harness's own ledger of what each owner was given, the server's
/// settlement rows, and the confirmed transactions on chain.
/// From each owner's seed phrase alone: derive its BIP84 account, find its
/// payouts in the chain's UTXO set (gap 20 on both chains), and spend them all
/// with keys derived from the seed. Every expected payout must be found.
async fn recover(ctx: &TestContext, env: &Env, owners: &[Owner], p: &Params) -> Value {
	let core = env.core();
	let secp = Secp256k1::new();
	let mut failures = Vec::<String>::new();
	let deadline = Instant::now() + Duration::from_secs(p.recovery_deadline_s);
	let destination = env.bitcoind.get_new_address();
	let mut spends = Vec::new();
	let mut found_total = 0usize;
	let mut independently_scanned = HashMap::<usize,BTreeSet<String>>::new();
	let t0 = Instant::now();
	for group in owners.chunks(50) {
		assert!(Instant::now() < deadline, "recovery deadline");
		// Seed → account xpub; spks derived independently to map results back.
		let mut descriptors = Vec::new();
		let mut spk_owner = HashMap::<ScriptBuf, usize>::new();
		for (k, o) in group.iter().enumerate() {
			let mnemonic: bip39::Mnemonic = std::fs::read_to_string(env.datadir.join(&o.name).join("mnemonic")).unwrap()
				.trim().parse().unwrap();
			let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
			let account: DerivationPath = "m/84'/1'/0'".parse().unwrap();
			let xpub = Xpub::from_priv(&secp, &master.derive_priv(&secp, &account).unwrap());
			for chain in 0..2u32 {
				descriptors.push(json!({ "desc": format!("wpkh({xpub}/{chain}/*)"), "range": [0, 19] }));
				for i in 0..20u32 {
					let path: DerivationPath = format!("m/{chain}/{i}").parse().unwrap();
					let pk = xpub.derive_pub(&secp, &path).unwrap().public_key;
					let spk = ScriptBuf::new_p2wpkh(&bitcoin::CompressedPublicKey(pk).wpubkey_hash());
					spk_owner.insert(spk, k);
				}
			}
		}
		let scan: Value = core.call("scantxoutset", &["start".into(), json!(descriptors)]).unwrap();
		assert_eq!(scan["success"], true);
		let mut found = HashMap::<usize, Vec<(OutPoint, u64, String)>>::new();
		for u in scan["unspents"].as_array().unwrap() {
			let spk = ScriptBuf::from_hex(u["scriptPubKey"].as_str().unwrap()).unwrap();
			let k = *spk_owner.get(&spk).expect("scan result maps to a derived script");
			let op = OutPoint::new(u["txid"].as_str().unwrap().parse().unwrap(), u["vout"].as_u64().unwrap() as u32);
			let amount = Amount::from_btc(u["amount"].as_f64().unwrap()).unwrap().to_sat();
			found.entry(k).or_default().push((op, amount, spk.to_hex_string()));
		}
		for (k, o) in group.iter().enumerate() {
			let utxos = found.remove(&k).unwrap_or_default();
			let got = utxos.iter().map(|(op, _, _)| op.txid.to_string()).collect::<BTreeSet<_>>();
			independently_scanned.insert(o.index,got);
			env.out.line("seed-scans.jsonl",&json!({"owner":o.index,"utc":utc(),"source":"mnemonic-only fresh BIP84 gap20","gap":20,"original_wallet_opened":false,"expected_lookup_performed":false,"utxos":utxos.iter().map(|(p,n,s)|json!({"outpoint":p.to_string(),"value_sat":n,"script":s})).collect::<Vec<_>>() }));
			if utxos.is_empty() { continue; }
			found_total += utxos.len();
			// Sign from the seed with a plain BIP84 wallet.
			let mnemonic: bip39::Mnemonic = std::fs::read_to_string(env.datadir.join(&o.name).join("mnemonic")).unwrap()
				.trim().parse().unwrap();
			let master = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
			let mut wallet = bdk_wallet::Wallet::create(Bip84(master, KeychainKind::External), Bip84(master, KeychainKind::Internal))
				.network(Network::Regtest).lookahead(20).create_wallet_no_persist().unwrap();
			let txs = utxos.iter().map(|(op, _, _)| op.txid).collect::<BTreeSet<_>>().into_iter()
				.map(|txid| (core.get_raw_transaction(&txid, None).unwrap(), 0u64)).collect::<Vec<_>>();
			wallet.apply_unconfirmed_txs(txs);
			let mut builder = wallet.build_tx();
			for (op, _, _) in &utxos { builder.add_utxo(*op).unwrap(); }
			builder.manually_selected_only().drain_to(destination.script_pubkey()).fee_rate(FeeRate::from_sat_per_vb(3).unwrap());
			let mut psbt = builder.finish().unwrap();
			assert!(wallet.sign(&mut psbt, SignOptions::default()).unwrap(), "owner {}: seed keys sign every input", o.index);
			let tx = psbt.extract_tx().unwrap();
			let txid = core.send_raw_transaction(&tx).unwrap_or_else(|e| panic!("owner {} spend: {e}", o.index));
			spends.push((o.index, txid, utxos.iter().map(|(_, a, _)| a).sum::<u64>()));
		}
	}
	env.out.event("all-seed-scans-complete",json!({"owners":owners.len(),"before_expected_lookup":true}));
	// Auditor learns expected payouts only AFTER every mnemonic-only scan/sign.
	let expected = env.db.read(async |t| Ok(t.query("SELECT DISTINCT c.txid,a.spk FROM expiry_allocation a JOIN expiry_carrier_allocation m ON m.allocation_id=a.id JOIN expiry_carrier c ON c.txid=m.txid WHERE a.disposition='pay' AND c.confirmed_height IS NOT NULL", &[]).await?))
		.await.unwrap().into_iter().map(|r| (r.get::<_, Vec<u8>>(1), r.get::<_, String>(0)))
		.fold(HashMap::<Vec<u8>, BTreeSet<String>>::new(), |mut m, (s, t)| { m.entry(s).or_default().insert(t); m });
	for o in owners {
		let got = independently_scanned.get(&o.index).unwrap();
		let want = expected.get(o.spk.as_bytes()).cloned().unwrap_or_default();
		if *got != want { failures.push(format!("owner {}: seed scan differs from post-scan audited expectation",o.index)); }
	}
	let scan_seconds = t0.elapsed().as_secs_f64();
	// Confirm every spend.
	loop {
		let unconfirmed = spends.iter().filter(|(_, txid, _)| core.get_raw_transaction_info(txid, None).ok()
			.and_then(|i| i.confirmations).unwrap_or(0) == 0).count();
		if unconfirmed == 0 { break; }
		assert!(Instant::now() < deadline, "{unconfirmed} seed spends unconfirmed at the deadline");
		ctx.generate_blocks(1).await;
	}
	json!({
		"failures": failures, "owners": owners.len(), "spends": spends.len(), "utxos_found": found_total,
		"spent_sat": spends.iter().map(|(_, _, a)| a).sum::<u64>(),
		"scan_and_sign_seconds": scan_seconds, "seconds": t0.elapsed().as_secs_f64(),
		"spend_txids": spends.iter().map(|(i, t, a)| json!([i, t.to_string(), a])).collect::<Vec<_>>(),
	})
}



async fn soak_restart(srv: &Captaind, enabled: bool) {
	srv.stop().await.unwrap();
	{
		let mut cfg = srv.config_mut();
		assert!(!cfg.expiry_payout.enabled, "legacy route must remain disabled");
		cfg.direct_expiry.enabled = true; // immutable qualification marker requires the route
		cfg.direct_expiry.interval = Duration::from_secs(if enabled {1} else {3600});
		cfg.direct_expiry.grace_blocks = 6;
	}
	srv.start().await.unwrap();
}

async fn soak_wait_paid(env: &Env, owners: &[Owner], deadline: Instant) {
	let ids = owners.iter().flat_map(|o| o.coins.iter().map(|c| c.id.clone())).collect::<Vec<_>>();
	loop {
		let rows = env.db.read(async |t| Ok(t.query(
			"SELECT a.id,c.txid FROM expiry_allocation a JOIN expiry_carrier_allocation m ON m.allocation_id=a.id JOIN expiry_carrier c ON c.txid=m.txid WHERE a.id=ANY($1) AND a.disposition IN ('pay','forfeit') AND c.confirmed_height IS NOT NULL", &[&ids],
		).await?)).await.unwrap();
		let settled = rows.iter().map(|r|r.get::<_,String>(0)).collect::<BTreeSet<_>>().len();
		let mut unconfirmed = 0;
		for txid in rows.iter().map(|r| r.get::<_, String>(1)).collect::<BTreeSet<_>>() {
			match env.core().get_raw_transaction_info(&txid.parse().unwrap(), None) {
				Ok(tx) if tx.confirmations.unwrap_or(0) > 0 => {},
				Ok(_) => unconfirmed += 1,
				// The database commit precedes direct carrier publication. A payout
				// not yet visible to Core is pending, not confirmed.
				Err(bitcoin_ext::rpc::Error::JsonRpc(bitcoin_ext::rpc::jsonrpc::Error::Rpc(e)))
					if e.code == -5 => unconfirmed += 1,
				Err(e) => panic!("cannot verify payout {txid}: {e}"),
			}
		}
		env.out.line("soak-payouts.jsonl", &json!({ "utc": utc(), "settled": settled,
			"expected": ids.len(), "unconfirmed": unconfirmed, "tip": env.core().get_block_count().unwrap() }));
		if settled == ids.len() && unconfirmed == 0 { return; }
		assert!(Instant::now() < deadline, "non-policy entitlement did not drain with float and low fees");
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
}


include!("sweep_direct_observers.rs");
