//! Prospective mining-fee allowances for one immutable server epoch.
//! Original parent bodies price exact signed candidates; replacements reserve
//! their full exposure and rebroadcasts preserve their original charge.
use std::collections::{BTreeMap, BTreeSet};
use anyhow::Context;
use bitcoin::{Amount, FeeRate, Transaction};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use crate::database::Tx;
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use bitcoind_async_client::traits::Reader;
use std::str::FromStr;
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Policy {
	pub epoch_id: String,
	pub genesis: String,
	pub server_pubkey: String,
	pub ordinary_sat: u64,
	pub urgent_sat: u64,
	pub ordinary_sat_per_kwu: u64,
	pub urgent_child_sat_per_kwu: u64,
	pub urgent_package_sat_per_kwu: u64,
	pub urgent_max_candidate_sat: u64,
	pub urgent_required_confirmed_sat: u64,
	pub watchman_descriptor_sha256: String,
	pub watchman_admin_address: String,
}
impl Policy {
	// Actual caller reads external bounded owner-only policy file, checks its
	// independently configured SHA and runtime genesis/key BEFORE calling this.
	// A DB row alone never selects a policy or silently starts a new epoch.
	pub(crate) fn validate(&self, genesis: &str, server_key: &str) -> anyhow::Result<()> {
		ensure!(self.genesis == genesis && self.server_pubkey == server_key,
			"fee epoch runtime identity differs");
		for id in [&self.epoch_id, &self.watchman_descriptor_sha256] {
			ensure!(id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()), "invalid fee policy identity");
		}
		for n in [self.ordinary_sat, self.urgent_sat, self.urgent_max_candidate_sat,
			self.urgent_required_confirmed_sat] {
			ensure!(n > 0 && n <= Amount::MAX_MONEY.to_sat(), "invalid fee allowance amount");
		}
		let endpoint:std::net::SocketAddr=self.watchman_admin_address.parse()?;
		ensure!(endpoint.ip().is_loopback(),"watchman fee source must be loopback");
		for n in [self.ordinary_sat_per_kwu,self.urgent_child_sat_per_kwu,self.urgent_package_sat_per_kwu] {
			ensure!(n > 0, "zero signed fee ceiling");
		}
		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Path { Round, Pool, Sweep, Payout, PayoutChild, Offboard, Claim, ProgressParent, ProgressChild }
impl Path {
	fn urgent(self) -> bool { matches!(self, Self::Claim | Self::ProgressParent | Self::ProgressChild) }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
	pub epoch_id: String,
	pub path: Path,
	pub raw: Vec<u8>,
	// Original native/Core/DB parent bodies, not caller-supplied fee integers.
	pub input_parents: Vec<Vec<u8>>,
	// Only ProgressChild: actual signed parent package and its original inputs.
	pub package_parents: Vec<Vec<u8>>,
	pub package_input_parents: Vec<Vec<u8>>,
}
impl Candidate {
	pub(crate) fn checked(&self, p: &Policy) -> anyhow::Result<(String,u64)> {
		ensure!(self.epoch_id == p.epoch_id, "candidate epoch differs");
		let bytes = self.raw.len().checked_add(self.input_parents.iter().map(Vec::len).sum::<usize>())
			.and_then(|n| n.checked_add(self.package_parents.iter().map(Vec::len).sum::<usize>()))
			.and_then(|n| n.checked_add(self.package_input_parents.iter().map(Vec::len).sum::<usize>()))
			.context("candidate proof length overflow")?;
		ensure!(bytes <= 4 * 1024 * 1024, "candidate proof exceeds private byte bound");
		let tx: Transaction = deserialize(&self.raw)?;
		ensure!(serialize(&tx) == self.raw, "noncanonical transaction encoding");
		let fee = measured_fee(&tx, &self.input_parents)?;
		let rate = if self.path.urgent() { p.urgent_child_sat_per_kwu } else { p.ordinary_sat_per_kwu };
		let limit = FeeRate::from_sat_per_kwu(rate).fee_wu(tx.weight()).context("signed fee ceiling overflow")?;
		ensure!(fee <= limit.to_sat(), "actual signed candidate fee exceeds epoch rate ceiling");
		if self.path.urgent() {
			ensure!(fee <= p.urgent_max_candidate_sat, "urgent signed candidate absolute fee exceeded");
		}
		if matches!(self.path,Path::ProgressChild) {
			ensure!(!self.package_parents.is_empty() && self.package_parents.len() <= 64,
				"progress child requires bounded real parent package");
			let mut seen = BTreeSet::new();
			let mut package_fee = fee;
			let mut package_weight = tx.weight();
			for raw in &self.package_parents {
				let parent: Transaction = deserialize(raw)?;
				ensure!(seen.insert(parent.compute_txid()), "duplicate package parent");
				ensure!(tx.input.iter().any(|i| i.previous_output.txid == parent.compute_txid()),
					"unrelated package parent");
				package_fee = package_fee.checked_add(measured_fee(&parent,&self.package_input_parents)?)
					.context("package fee overflow")?;
				package_weight = package_weight.checked_add(parent.weight()).context("package weight overflow")?;
			}
			let max = FeeRate::from_sat_per_kwu(p.urgent_package_sat_per_kwu).fee_wu(package_weight)
				.context("package ceiling overflow")?;
			ensure!(package_fee <= max.to_sat(), "actual package fee exceeds urgent ceiling");
		} else {
			ensure!(self.package_parents.is_empty() && self.package_input_parents.is_empty(),
				"unexpected package fee proof");
		}
		Ok((tx.compute_wtxid().to_string(),fee))
	}
}
fn measured_fee(tx: &Transaction, parents: &[Vec<u8>]) -> anyhow::Result<u64> {
	ensure!(!tx.is_coinbase() && !tx.input.is_empty() && tx.input.len() <= 4096,
		"not a bounded spending candidate");
	let mut source = BTreeMap::new();
	for raw in parents {
		let parent: Transaction = deserialize(raw)?;
		ensure!(source.insert(parent.compute_txid(),parent).is_none(), "duplicate source body");
	}
	let mut seen = BTreeSet::new();
	let mut gross = 0u64;
	for input in &tx.input {
		ensure!(seen.insert(input.previous_output), "duplicate candidate input");
		let parent = source.get(&input.previous_output.txid).context("original input parent missing")?;
		let out = parent.output.get(input.previous_output.vout as usize).context("original prevout missing")?;
		gross = gross.checked_add(out.value.to_sat()).context("input value overflow")?;
	}
	let outputs = tx.output.iter().try_fold(0u64,|n,o| n.checked_add(o.value.to_sat()))
		.context("output value overflow")?;
	gross.checked_sub(outputs).context("candidate spends more than original inputs")
}

impl Tx<'_> {
	// Caller holds actual monetary input/wallet/source locks and invokes this
	// inside the EXISTING exact publication transaction. Global advisory lock4
	// is proposed; native integrator must reserve an unused symbolic enum value.
	pub(crate) async fn reserve_fee_candidate(&self,p:&Policy,c:&Candidate) -> anyhow::Result<()> {
		self.lock_fee_allowance().await?;
		// Validate complete child+package before reserving any newly disclosed parent.
		c.checked(p)?;
		if matches!(c.path,Path::ProgressChild) {
			for raw in &c.package_parents {
				let parent=Candidate {epoch_id:c.epoch_id.clone(),path:Path::ProgressParent,
					raw:raw.clone(),input_parents:c.package_input_parents.clone(),
					package_parents:Vec::new(),package_input_parents:Vec::new()};
				let id=deserialize::<Transaction>(&parent.raw)?.compute_wtxid().to_string();
				let prior=self.query_opt("SELECT candidate_bytes FROM fee_allowance_candidate WHERE id=$1",&[&id]).await?;
				if let Some(prior)=prior {
					// A package can reference an already reserved original publication.
					// Preserve its original class/charge; never turn it into fresh U credit.
					let old:Candidate=serde_json::from_slice(&prior.get::<_,Vec<u8>>(0))?;
					ensure!(old.raw==parent.raw && old.epoch_id==p.epoch_id
						&& measured_fee(&deserialize::<Transaction>(&old.raw)?,&old.input_parents)?
							==measured_fee(&deserialize::<Transaction>(&parent.raw)?,&parent.input_parents)?,
						"prior parent fee reservation differs");
				} else { self.reserve_one_fee_candidate(p,&parent).await?; }
			}
		}
		self.reserve_one_fee_candidate(p,c).await
	}
	// Same surrounding SQL transaction and global lock; never recurse or COMMIT
	// a parent independently. First publication reserves its full actual fee;
	// exact original parent rebroadcast in a later child consumes no new amount.
	async fn reserve_one_fee_candidate(&self,p:&Policy,c:&Candidate) -> anyhow::Result<()> {
		self.lock_fee_allowance().await?;
		let policy = serde_json::to_vec(p)?;
		let row = self.query_one("SELECT policy_bytes FROM fee_allowance_policy WHERE id='active' FOR UPDATE",&[]).await?;
		ensure!(row.get::<_,Vec<u8>>(0) == policy,"native policy differs from external immutable declaration");
		let (id,fee) = c.checked(p)?;
		let raw = serde_json::to_vec(c)?;
		ensure!(raw.len()<=4*1024*1024,"serialized fee reservation exceeds private record bound");
		let old = self.query_opt("SELECT epoch_id,candidate_bytes,urgent,fee_sat FROM fee_allowance_candidate WHERE id=$1",&[&id]).await?;
		if let Some(old) = old {
			ensure!(old.get::<_,String>(0)==p.epoch_id && old.get::<_,Vec<u8>>(1)==raw
				&& old.get::<_,bool>(2)==c.path.urgent() && old.get::<_,i64>(3)==i64::try_from(fee)?,
				"same signed candidate has conflicting reservation");
			return Ok(());
		}
		let urgent=c.path.urgent();
		let used=self.query_one("SELECT COALESCE(sum(fee_sat),0)::bigint FROM fee_allowance_candidate WHERE epoch_id=$1 AND urgent=$2",
			&[&p.epoch_id,&urgent]).await?.get::<_,i64>(0);
		let total=u64::try_from(used)?.checked_add(fee).context("epoch fee sum overflow")?;
		let allowance=if urgent {p.urgent_sat} else {p.ordinary_sat};
		ensure!(total <= allowance,"durable epoch fee allowance exhausted");
		self.execute("INSERT INTO fee_allowance_candidate(id,epoch_id,candidate_bytes,urgent,fee_sat) VALUES($1,$2,$3,$4,$5)",
			&[&id,&p.epoch_id,&raw,&urgent,&i64::try_from(fee)?]).await?;
		Ok(())
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
	/// Explicit paused fresh opening only; never inferred from an empty DB.
	#[serde(default)]
	pub initialize_empty: bool,
	pub policy_file: std::path::PathBuf,
	pub policy_sha256: String,
	pub history_dir: std::path::PathBuf,
}

pub(crate) struct Gate {
	policy: Policy,
	history: crate::fee_allowance_history::FeeHistory,
	network: bitcoin::Network,
}

/// Created only after exact original fees and their history are durable.
pub(crate) struct Permit { candidate: Candidate }
impl Permit {
	pub(crate) fn matches(&self, tx: &Transaction) -> anyhow::Result<()> {
		ensure!(self.candidate.raw == serialize(tx), "fee permit signed bytes differ");
		Ok(())
	}
}

impl Gate {
	pub(crate) async fn load(
		config: Option<&Config>, datadir: &std::path::Path, network: bitcoin::Network,
		key: bitcoin::secp256k1::PublicKey, db: &crate::database::Db, core:&bitcoind_async_client::Client,
	) -> anyhow::Result<Option<Self>> {
		let mut row = db.read(async |t| Ok(t.query_opt(
			"SELECT policy_bytes FROM fee_allowance_policy WHERE id='active'", &[],
		).await?.map(|r| r.get::<_,Vec<u8>>(0)))).await?;
		let marker = datadir.join("fee-epoch-policy.json");
		let Some(config) = config else {
			ensure!(row.is_none() && !marker.exists(), "retained fee epoch requires its external policy");
			return Ok(None);
		};
		ensure!(config.policy_file.is_absolute() && config.history_dir.is_absolute(),
			"fee policy and shared history paths must be absolute");
		let directory = std::fs::symlink_metadata(&config.history_dir)?;
		ensure!(directory.is_dir(),"shared fee history root is not a real directory");
		let history_identity=format!("{}:{}",directory.dev(),directory.ino());
		let bytes = crate::operator_residual::read_bounded(&config.policy_file, 64 * 1024)?;
		ensure!(sha256::Hash::hash(&bytes).to_string() == config.policy_sha256,
			"external fee policy SHA differs");
		let policy: Policy = serde_json::from_slice(&bytes)?;
		let actual_genesis=core.get_block_hash(0).await?.to_string();
		ensure!(actual_genesis==bitcoin::constants::genesis_block(network).block_hash().to_string(),
			"actual Core genesis differs from configured network");
		policy.validate(&actual_genesis, &key.to_string())?;
		if row.is_none() && config.history_dir.join("fee-allowance-witness").exists() {
			// Retained identity is an ORIGINAL epoch, not permission to reset it.
			// Rebook every retained reservation below before any worker/replay.
			crate::fee_allowance_history::FeeHistory::load(&config.history_dir,&policy)?;
			db.write(async |t| {
				t.lock_fee_allowance().await?;
				t.execute("INSERT INTO fee_allowance_policy(id,policy_bytes,history_identity) VALUES('active',$1,$2) ON CONFLICT(id) DO NOTHING",
					&[&serde_json::to_vec(&policy)?,&history_identity]).await?;
				let actual=t.query_one("SELECT policy_bytes FROM fee_allowance_policy WHERE id='active'",&[]).await?.get::<_,Vec<u8>>(0);
				ensure!(actual==serde_json::to_vec(&policy)?,"restored fee epoch differs");
				Ok(())
			}).await?;
			row=Some(serde_json::to_vec(&policy)?);
		}
		if row.is_none() && config.initialize_empty {
			ensure!(!marker.exists() && !config.history_dir.join("fee-allowance-witness").exists(),
				"retained fee epoch cannot initialize an empty replacement");
			db.write(async |t| {
				t.lock_fee_allowance().await?;
				// Only a fresh, explicitly paused opening. No prior operator
				// publication/settlement can silently become zero fee history.
				t.execute("LOCK TABLE nursery_tx, round, virtual_transaction, offboards, vtxo_pool, expiry_settlement, recovery_receipt, recovery_capital_release IN SHARE MODE", &[]).await?;
				let existing=t.query_one("SELECT EXISTS(SELECT 1 FROM nursery_tx) OR EXISTS(SELECT 1 FROM offboards) OR EXISTS(SELECT 1 FROM vtxo_pool) OR EXISTS(SELECT 1 FROM expiry_settlement) OR EXISTS(SELECT 1 FROM recovery_receipt) OR EXISTS(SELECT 1 FROM recovery_capital_release) OR EXISTS(SELECT 1 FROM round r JOIN virtual_transaction v ON v.txid=r.funding_txid WHERE v.signed_tx IS NOT NULL)", &[]).await?.get::<_,bool>(0);
				ensure!(!existing,"existing operator publication history requires explicit original fee reservation import");
				ensure!(t.query_opt("SELECT id FROM fee_allowance_policy WHERE id='active'",&[]).await?.is_none(),
					"fee policy initialized concurrently; restart same original configuration");
				crate::fee_allowance_history::FeeHistory::initialize(&config.history_dir,&policy)?;
				t.execute("INSERT INTO fee_allowance_policy(id,policy_bytes,history_identity) VALUES('active',$1,$2)", &[&serde_json::to_vec(&policy)?,&history_identity]).await?;
				Ok(())
			}).await?;
			row=Some(serde_json::to_vec(&policy)?);
		}
		ensure!(row.as_deref() == Some(serde_json::to_vec(&policy)?.as_slice()),
			"fee epoch has no reconciled native policy; paused initialization required");
		let bound=db.read(async|t|Ok(t.query_one("SELECT history_identity FROM fee_allowance_policy WHERE id='active'",&[])
			.await?.get::<_,String>(0))).await?;
		ensure!(bound==history_identity,"captaind/watchmand do not share the same retained fee history root");
		let history = crate::fee_allowance_history::FeeHistory::load(&config.history_dir, &policy)?;
		// Both processes must reconcile before wallet replay, nursery or workers.
		history.reserve_and_witness(db, &policy, None).await?;
		crate::operator_residual::write_identical(&marker, &serde_json::to_vec(&serde_json::json!({
			"policy_sha256":config.policy_sha256,"history_dir":config.history_dir,
		}))?)?;
		Ok(Some(Self { policy, history, network }))
	}

	async fn parents(
		db: &crate::database::Db, core: &bitcoind_async_client::Client, tx: &Transaction,
	) -> anyhow::Result<Vec<Vec<u8>>> {
		let ids = tx.input.iter().map(|i| i.previous_output.txid).collect::<BTreeSet<_>>();
		ensure!(ids.len() <= 4096, "fee input proof bound exceeded");
		let mut parents = Vec::with_capacity(ids.len());
		let mut bytes = 0usize;
		for id in ids {
			let stored = db.read(async |t| Ok(t.get_virtual_transaction_by_txid(id).await?
				.and_then(|v| v.signed_tx.map(|t| t.into_owned())))).await?;
			let parent = match stored {
				Some(parent) => parent,
				None => {
					let info = crate::bitcoind::custom_get_raw_transaction_info(core, id, None).await?
						.context("original fee input parent unavailable")?;
					deserialize::<Transaction>(&info.hex)?
				},
			};
			ensure!(parent.compute_txid() == id, "fee input parent txid differs");
			let raw = serialize(&parent);
			bytes = bytes.checked_add(raw.len()).context("fee parent byte sum overflow")?;
			ensure!(bytes <= 4 * 1024 * 1024, "fee parent byte bound exceeded");
			parents.push(raw);
		}
		Ok(parents)
	}

	pub(crate) async fn ordinary(
		&self, db: &crate::database::Db, core: &bitcoind_async_client::Client,
		path: Path, tx: &Transaction,
	) -> anyhow::Result<Permit> {
		ensure!(!path.urgent(), "urgent fee source proof is not implemented; financial work held");
		let candidate = Candidate { epoch_id: self.policy.epoch_id.clone(), path, raw: serialize(tx),
			input_parents: Self::parents(db,core,tx).await?,
			package_parents: Vec::new(), package_input_parents: Vec::new() };
		candidate.checked(&self.policy)?;
		// No Boolean ready flag: owning watchman inventory and independent Core.
		let readiness = self.stage_urgent_inputs(db, core).await?;
		self.history.reserve_with_urgent_source(db, &self.policy, &candidate,
			readiness.generation, &readiness.inputs).await?;
		Ok(Permit { candidate })
	}
}

struct UrgentInputs { generation: i64, inputs: BTreeMap<bitcoin::OutPoint,u64> }
impl Gate {
	async fn generation(db: &crate::database::Db) -> anyhow::Result<i64> {
		db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM fee_allowance_candidate WHERE urgent=true", &[],
		).await?.get::<_,i64>(0))).await
	}

	async fn checked_urgent_inventory(
		&self, core: &bitcoind_async_client::Client, wallet: &server_rpc::protos::WalletInventory,
	) -> anyhow::Result<BTreeMap<bitcoin::OutPoint,u64>> {
		let before = crate::bitcoind::tip(core).await?;
		ensure!(wallet.wallet == "watchman" && wallet.network == self.network.to_string()
			&& wallet.tip_height == before.height.to_u32() && wallet.tip_hash == before.hash.to_string(),
			"urgent owning wallet identity or tip differs");
		ensure!(sha256::Hash::hash(wallet.public_descriptor.as_bytes()).to_string()
			== self.policy.watchman_descriptor_sha256, "urgent owning descriptor differs");
		ensure!(wallet.utxos.len() <= 4096 && wallet.unavailable_parent_txids.is_empty(),
			"urgent owning source bound or parent proof unavailable");
		let mut inputs = BTreeMap::new();
		for output in wallet.utxos.iter().filter(|u| u.confirmed_fee_eligible) {
			let point = bitcoin::OutPoint::from_str(&output.outpoint)?;
			ensure!(output.confirmations >= wallet.min_trusted_confirmations && output.mature
				&& output.trusted && !output.reserved && !wallet.reserved_outpoints.contains(&output.outpoint),
				"urgent native input is not confirmed eligible");
			let live = core.try_get_tx_out(point,true).await?.context("urgent input spent or missing")?;
			ensure!(live.value.to_sat() == output.value_sat
				&& live.script_pub_key.script()?.as_bytes() == output.script_pubkey
				&& live.confirmations >= wallet.min_trusted_confirmations,
				"independent urgent Core prevout differs");
			ensure!(inputs.insert(point, output.value_sat).is_none(), "duplicate urgent input");
		}
		ensure!(crate::bitcoind::tip(core).await? == before, "urgent proof crossed Core tip");
		Ok(inputs)
	}

	async fn stage_urgent_inputs(
		&self, db: &crate::database::Db, core: &bitcoind_async_client::Client,
	) -> anyhow::Result<UrgentInputs> {
		let generation = Self::generation(db).await?;
		let endpoint: std::net::SocketAddr = self.policy.watchman_admin_address.parse()?;
		ensure!(endpoint.ip().is_loopback(), "urgent source endpoint must be loopback");
		let observe = async {
			let mut client = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{endpoint}")).await?;
			let frame = client.wallet_inventory(server_rpc::protos::Empty {}).await?.into_inner();
			let wallet = frame.watchman.context("owning urgent watchman inventory missing")?;
			let inputs = self.checked_urgent_inventory(core,&wallet).await?;
			Ok::<_,anyhow::Error>(UrgentInputs { generation, inputs })
		};
		tokio::time::timeout(std::time::Duration::from_secs(15), observe).await
			.context("urgent source proof deadline exceeded")?
	}

	pub(crate) async fn urgent_claim(
		&self, db:&crate::database::Db, core:&bitcoind_async_client::Client, tx:&Transaction,
	) -> anyhow::Result<Permit> {
		// Called only for native scan_frontier deadline claims after WatchmanSigner
		// constructed the actual owned backing spend, not from a public class flag.
		let generation=Self::generation(db).await?;
		let candidate=Candidate {epoch_id:self.policy.epoch_id.clone(),path:Path::Claim,raw:serialize(tx),
			input_parents:Self::parents(db,core,tx).await?,package_parents:Vec::new(),package_input_parents:Vec::new()};
		self.history.reserve_urgent(db,&self.policy,&candidate,generation,&BTreeSet::new()).await?;
		Ok(Permit {candidate})
	}

	pub(crate) async fn urgent_progress(
		&self,db:&crate::database::Db,core:&bitcoind_async_client::Client,
		child:&Transaction,parent:&Transaction,owning:&server_rpc::protos::WalletInventory,
	) -> anyhow::Result<Permit> {
		let generation=Self::generation(db).await?;
		// Uses the inventory made under this SAME actual watchman-wallet guard;
		// calling our own admin RPC while holding that guard would deadlock.
		let inputs=self.checked_urgent_inventory(core,owning).await?;
		let (anchor,_) = bitcoin_ext::TransactionExt::fee_anchor(parent).context("urgent parent has no actual fee anchor")?;
		ensure!(child.input.iter().any(|i|i.previous_output==anchor),"urgent child does not spend original anchor");
		let own_selected=child.input.iter().filter(|i|i.previous_output!=anchor)
			.map(|i|i.previous_output).collect::<BTreeSet<_>>();
		ensure!(!own_selected.is_empty()&&own_selected.iter().all(|p|inputs.contains_key(p)),
			"urgent child selects inputs outside actual confirmed owning source");
		let candidate=Candidate {epoch_id:self.policy.epoch_id.clone(),path:Path::ProgressChild,raw:serialize(child),
			input_parents:Self::parents(db,core,child).await?,package_parents:vec![serialize(parent)],
			package_input_parents:Self::parents(db,core,parent).await?};
		self.history.reserve_urgent(db,&self.policy,&candidate,generation,&own_selected).await?;
		Ok(Permit {candidate})
	}
}

pub(crate) async fn ordinary_permit(
	gate: Option<&Gate>, db: &crate::database::Db, core: &bitcoind_async_client::Client,
	path: Path, tx: &Transaction,
) -> anyhow::Result<Option<Permit>> {
	match gate {
		Some(gate) => Ok(Some(gate.ordinary(db,core,path,tx).await?)),
		None => {
			let active = db.read(async |t| Ok(t.query_one(
				"SELECT EXISTS(SELECT 1 FROM fee_allowance_policy)", &[],
			).await?.get::<_,bool>(0))).await?;
			ensure!(!active, "native fee epoch appeared without its external policy");
			Ok(None)
		},
	}
}

pub(crate) async fn require_permit(
	t: &Tx<'_>, tx: &Transaction, permit: Option<&Permit>,
) -> anyhow::Result<()> {
	if let Some(permit) = permit {
		permit.matches(tx)?;
		let id = tx.compute_wtxid().to_string();
		let raw = serde_json::to_vec(&permit.candidate)?;
		let row = t.query_one("SELECT candidate_bytes FROM fee_allowance_candidate WHERE id=$1", &[&id]).await?;
		ensure!(row.get::<_,Vec<u8>>(0) == raw, "durable fee reservation differs before publication");
	} else {
		ensure!(!t.query_one("SELECT EXISTS(SELECT 1 FROM fee_allowance_policy)", &[]).await?.get::<_,bool>(0),
			"signed publication lacks its enforced epoch permit");
	}
	Ok(())
}

pub(crate) async fn require_record(t: &Tx<'_>, tx: &Transaction) -> anyhow::Result<()> {
	if !t.query_one("SELECT EXISTS(SELECT 1 FROM fee_allowance_policy)",&[]).await?.get::<_,bool>(0) {
		return Ok(());
	}
	let row=t.query_opt("SELECT candidate_bytes FROM fee_allowance_candidate WHERE id=$1",
		&[&tx.compute_wtxid().to_string()]).await?.context("enforced epoch has no exact fee reservation")?;
	let candidate:Candidate=serde_json::from_slice(&row.get::<_,Vec<u8>>(0))?;
	ensure!(candidate.raw==serialize(tx),"reserved original signed bytes differ");
	Ok(())
}
