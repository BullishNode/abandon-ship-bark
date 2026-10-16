//! Bounded original fee reservations retained independently of PostgreSQL.
//! Recovery rebooks exact original reservations, never broadcasts transactions
//! or reconstructs customer settlements from fee evidence.
use std::{collections::{BTreeMap,BTreeSet},fs::{self,File,OpenOptions},io::Write,path::{Path,PathBuf}};
use anyhow::Context;
use bitcoin::Wtxid;
use crate::database::{Db,Tx};
use crate::fee_allowance::{Candidate,Policy};
use crate::operator_residual::{read_bounded,write_identical};
const MAX_RECORDS:usize=100_000;
const MAX_BYTES:usize=64*1024*1024;
const MAX_RECORD_BYTES:usize=4*1024*1024;
const DIRECTORY:&str="fee-allowance-witness";

pub(crate) struct FeeHistory { directory:PathBuf }
// Loaded once under the same global SQL advisory lock, then maintained for
// this operation. Never reused across calls/processes or retained as a cache.
struct HistorySnapshot {
	originals:BTreeMap<String,Candidate>,
	indexed:BTreeSet<String>,
	bytes:usize,
}
impl FeeHistory {
	// ONLY the existing genuine server-create path may call this. Enabling an
	// existing server is a separately reconciled paused operation, not auto-init.
	pub(crate) fn initialize(datadir:&Path,p:&Policy)->anyhow::Result<()> {
		let directory=datadir.join(DIRECTORY);fs::create_dir(&directory)?;
		crate::fs_perms::harden(&directory,0o700)?;
		write_identical(&directory.join("identity.json"),&serde_json::to_vec(p)?)?;
		write_identical(&directory.join("history"),b"")?;
		File::open(datadir)?.sync_all()?;Ok(())
	}
	pub(crate) fn load(datadir:&Path,p:&Policy)->anyhow::Result<Self> {
		let directory=datadir.join(DIRECTORY);
		ensure!(fs::symlink_metadata(&directory)?.is_dir(),"fee history directory absent");
		let bytes=read_bounded(&directory.join("identity.json"),64*1024)?;
		let stored:Policy=serde_json::from_slice(&bytes)?;
		ensure!(stored==*p,"fee history differs from immutable external policy");
		Ok(Self{directory})
	}
	fn indexed(&self)->anyhow::Result<BTreeSet<String>> {
		let raw=read_bounded(&self.directory.join("history"),MAX_RECORDS*65)?;
		ensure!(raw.is_empty()||raw.last()==Some(&b'\n'),"torn fee reservation history");
		let mut ids=BTreeSet::new();
		for line in std::str::from_utf8(&raw)?.lines() {
			let id:Wtxid=line.parse()?;
			ensure!(id.to_string()==line&&ids.insert(line.to_string()),"invalid/duplicate fee index");
		}Ok(ids)
	}
	fn snapshot_locked(&self,p:&Policy)->anyhow::Result<HistorySnapshot> {
		let mut bytes=0usize;let mut originals=BTreeMap::new();
		for (entry_number,entry) in fs::read_dir(&self.directory)?.enumerate() {
			ensure!(entry_number<=MAX_RECORDS+3,"fee history directory entry bound exceeded");
			let entry=entry?;let name=entry.file_name();let name=name.to_str().context("invalid fee filename")?;
			if name=="identity.json"||name=="history"||(name.starts_with('.')&&name.ends_with(".tmp")){continue}
			ensure!(originals.len()<MAX_RECORDS,"fee history record bound exceeded");
			let id=name.strip_suffix(".json").context("unknown fee history file")?;
			let raw=read_bounded(&entry.path(),MAX_RECORD_BYTES)?;
			bytes=bytes.checked_add(raw.len()).context("fee history byte overflow")?;
			ensure!(bytes<=MAX_BYTES,"fee history byte bound exceeded");
			let c:Candidate=serde_json::from_slice(&raw)?;
			ensure!(c.checked(p)?.0==id&&originals.insert(id.to_string(),c).is_none(),"fee history identity mismatch");
		}
		let indexed=self.indexed()?;
		ensure!(indexed.iter().all(|id| originals.contains_key(id)),"indexed fee reservation file missing");
		Ok(HistorySnapshot{originals,indexed,bytes})
	}
	// Caller holds GLOBAL fee SQL advisory lock, not only an in-process mutex.
	// Exact-file verification remains bounded; do not rescan other records.
	fn record_locked(&self,p:&Policy,c:&Candidate,snapshot:&mut HistorySnapshot)->anyhow::Result<()> {
		let (id,_)=c.checked(p)?;
		let raw=serde_json::to_vec(c)?;
		ensure!(raw.len()<=MAX_RECORD_BYTES,"serialized fee history record exceeds bound");
		let is_new=match snapshot.originals.get(&id) {
			Some(old)=>{ensure!(old==c,"conflicting exact fee history");false},
			None=>true,
		};
		let total=if is_new {
			ensure!(snapshot.originals.len()<MAX_RECORDS,"fee history record capacity exhausted");
			snapshot.bytes.checked_add(raw.len()).context("fee history sum overflow")?
		}else{snapshot.bytes};
		ensure!(total<=MAX_BYTES,"fee history capacity exhausted");
		write_identical(&self.directory.join(format!("{id}.json")),&raw)?;
		if !snapshot.indexed.contains(&id) {
			let mut history=OpenOptions::new().append(true).open(self.directory.join("history"))?;
			ensure!(history.metadata()?.len()==snapshot.indexed.len()as u64*65,"fee history changed during append");
			writeln!(history,"{id}")?;history.sync_all()?;
			snapshot.indexed.insert(id.clone());
		}
		if is_new {snapshot.originals.insert(id,c.clone());snapshot.bytes=total;}
		Ok(())
	}
	// reserve_fee_candidate has already inserted parent+child atomically under
	// this transaction/lock. Witness those exact native parent rows before the
	// child, using the same operation snapshot (not another full reconciliation).
	async fn record_reserved_locked(&self,t:&Tx<'_>,p:&Policy,c:&Candidate,snapshot:&mut HistorySnapshot)->anyhow::Result<()> {
		for raw in &c.package_parents {
			let parent:bitcoin::Transaction=bitcoin::consensus::deserialize(raw)?;
			let id=parent.compute_wtxid().to_string();
			let row=t.query_one("SELECT candidate_bytes FROM fee_allowance_candidate WHERE id=$1",&[&id]).await?;
			let bytes:Vec<u8>=row.get(0);
			ensure!(bytes.len()<=MAX_RECORD_BYTES,"native parent reservation exceeds byte bound");
			let original:Candidate=serde_json::from_slice(&bytes)?;
			ensure!(original.raw==*raw,"native reserved parent bytes differ");
			self.record_locked(p,&original,snapshot)?;
		}
		self.record_locked(p,c,snapshot)
	}
	// Replay bounded original reservations only. NEVER broadcast or reconstruct
	// missing native customer obligations from a fee record.
	async fn assert_ledger_bounds(t:&Tx<'_>)->anyhow::Result<()> {
		let row=t.query_one("SELECT count(*),COALESCE(sum(octet_length(candidate_bytes)),0)::bigint,COALESCE(max(octet_length(candidate_bytes)),0)::bigint FROM fee_allowance_candidate",&[]).await?;
		ensure!(row.get::<_,i64>(0)<=MAX_RECORDS as i64 && row.get::<_,i64>(1)<=MAX_BYTES as i64
			&& row.get::<_,i64>(2)<=MAX_RECORD_BYTES as i64,"native fee ledger exceeds bounded read admission");
		Ok(())
	}

	async fn reconcile_locked(&self,t:&Tx<'_>,p:&Policy)->anyhow::Result<HistorySnapshot> {
		Self::assert_ledger_bounds(t).await?;
		let mut snapshot=self.snapshot_locked(p)?;
		// First recover every original reservation. Parent reservations disclosed
		// by a child are inserted by the same native method, under this same lock.
		// Preserve every recorded parent's ORIGINAL class before a child can
		// disclose that same raw parent as ProgressParent. A restored DB must
		// not depend on filesystem or wtxid order to reconstruct accounting.
		for c in snapshot.originals.values().filter(|c| !matches!(c.path,crate::fee_allowance::Path::ProgressChild)) {
			t.reserve_fee_candidate(p,c).await?;
		}
		for c in snapshot.originals.values().filter(|c| matches!(c.path,crate::fee_allowance::Path::ProgressChild)) {
			t.reserve_fee_candidate(p,c).await?;
		}
		Self::assert_ledger_bounds(t).await?;
		let rows=t.query("SELECT candidate_bytes FROM fee_allowance_candidate WHERE epoch_id=$1 ORDER BY id LIMIT 100001",&[&p.epoch_id]).await?;
		ensure!(rows.len()<=MAX_RECORDS,"native fee ledger row bound exceeded");
		let mut bytes=0usize;
		for row in rows {
			let raw:Vec<u8>=row.get(0);bytes=bytes.checked_add(raw.len()).context("native fee ledger size overflow")?;
			ensure!(raw.len()<=MAX_RECORD_BYTES&&bytes<=MAX_BYTES,"native fee ledger byte bound exceeded");
			let c:Candidate=serde_json::from_slice(&raw)?;
			t.reserve_fee_candidate(p,&c).await?;self.record_reserved_locked(t,p,&c,&mut snapshot).await?;
		}Ok(snapshot)
	}
	// A retained history intent survives failed/unknown COMMIT; the next call
	// conservatively rebooks that SAME reservation, never renews allowance.
	// Must also run before starting financial workers at ordinary restart.
	pub(crate) async fn reserve_with_urgent_source(
		&self,db:&Db,p:&Policy,c:&Candidate,generation:i64,
		inputs:&std::collections::BTreeMap<bitcoin::OutPoint,u64>,
	)->anyhow::Result<()> {
		db.write(async|t| {
			t.lock_fee_allowance().await?;
			let mut snapshot=self.reconcile_locked(t,p).await?;
			ensure!(t.query_one("SELECT count(*) FROM fee_allowance_candidate WHERE urgent=true",&[])
				.await?.get::<_,i64>(0)==generation,"urgent reservation generation changed; restage native proof");
			Self::assert_ledger_bounds(t).await?;
			let rows=t.query("SELECT candidate_bytes FROM fee_allowance_candidate WHERE urgent=true ORDER BY id LIMIT 100001",&[]).await?;
			ensure!(rows.len()<=MAX_RECORDS,"urgent reservation row bound exceeded");
			let mut reserved=BTreeSet::new();let mut bytes=0usize;
			for row in rows {
				let raw:Vec<u8>=row.get(0);bytes=bytes.checked_add(raw.len()).context("urgent reservation bytes overflow")?;
				ensure!(raw.len()<=MAX_RECORD_BYTES&&bytes<=MAX_BYTES,"urgent reservation byte bound exceeded");
				let old:Candidate=serde_json::from_slice(&raw)?;
				let tx:bitcoin::Transaction=bitcoin::consensus::deserialize(&old.raw)?;
				reserved.extend(tx.input.iter().map(|i|i.previous_output));
			}
			let available=inputs.iter().filter(|(id,_)|!reserved.contains(id))
				.try_fold(0u64,|total,(_,value)|total.checked_add(*value)).context("urgent liquidity overflow")?;
			ensure!(available>=p.urgent_required_confirmed_sat,"confirmed unreserved urgent inputs below declared floor");
			let tx:bitcoin::Transaction=bitcoin::consensus::deserialize(&c.raw)?;
			ensure!(tx.input.iter().all(|i|!inputs.contains_key(&i.previous_output)),
				"ordinary transaction selects declared urgent input");
			t.reserve_fee_candidate(p,c).await?;self.record_reserved_locked(t,p,c,&mut snapshot).await?;
			Ok(())
		}).await
	}
	pub(crate) async fn reserve_urgent(
		&self,db:&Db,p:&Policy,c:&Candidate,generation:i64,selected:&BTreeSet<bitcoin::OutPoint>,
	)->anyhow::Result<()> {
		db.write(async|t| {
			t.lock_fee_allowance().await?;
			let mut snapshot=self.reconcile_locked(t,p).await?;
			ensure!(t.query_one("SELECT count(*) FROM fee_allowance_candidate WHERE urgent=true",&[])
				.await?.get::<_,i64>(0)==generation,"urgent generation changed; restage original source");
			Self::assert_ledger_bounds(t).await?;
			let rows=t.query("SELECT candidate_bytes FROM fee_allowance_candidate WHERE urgent=true ORDER BY id LIMIT 100001",&[]).await?;
			ensure!(rows.len()<=MAX_RECORDS,"urgent candidate row bound exceeded");
			let mut bytes=0usize;
			for row in rows {
				let raw:Vec<u8>=row.get(0);bytes=bytes.checked_add(raw.len()).context("urgent candidate size overflow")?;
				ensure!(raw.len()<=MAX_RECORD_BYTES&&bytes<=MAX_BYTES,"urgent candidate bytes exceeded");
				let old:Candidate=serde_json::from_slice(&raw)?;
				let tx:bitcoin::Transaction=bitcoin::consensus::deserialize(&old.raw)?;
				if tx.input.iter().any(|i|selected.contains(&i.previous_output)) && old.raw!=c.raw {
					// Replacements of the SAME anchor package are mutually exclusive and
					// each new child still consumes its full fee. Different urgent jobs
					// cannot reserve the same fee cash even before broadcast reaches Core.
					ensure!(matches!(old.path,crate::fee_allowance::Path::ProgressChild)
						&& old.package_parents==c.package_parents,"urgent input reserved by a different original package");
				}
			}
			t.reserve_fee_candidate(p,c).await?;self.record_reserved_locked(t,p,c,&mut snapshot).await?;
			Ok(())
		}).await
	}
	pub(crate) async fn reserve_and_witness(&self,db:&Db,p:&Policy,c:Option<&Candidate>)->anyhow::Result<()> {
		db.write(async|t| {
			t.lock_fee_allowance().await?;
			let mut snapshot=self.reconcile_locked(t,p).await?;
			if let Some(c)=c {t.reserve_fee_candidate(p,c).await?;self.record_reserved_locked(t,p,c,&mut snapshot).await?;}
			Ok(())
		}).await
	}
}
