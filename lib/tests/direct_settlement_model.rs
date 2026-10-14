//! Independent symbolic reference for direct settlement. This never calls a
//! product resolver, database writer, signer or chain client. PASS here proves
//! model invariants only; concrete event producers and product adapters are separate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::io::Write;

use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;

const FLAT: u64 = 1_000;
const MAX_EVENTS: usize = 256;
const MAX_VERSIONS: usize = 8;
const MAX_MONEY: u64 = 2_100_000_000_000_000;

fn sum(values: impl IntoIterator<Item = u64>) -> u64 {
	values.into_iter().try_fold(0u64, u64::checked_add).expect("reference amount overflow")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case { Exchange, Intra, ExternalSend, ExternalReceive, Mixed }
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Root { OldA, OldB, Replacement, Sender, Grant }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outgoing { Unknown, TemporaryRefusal, Failed, Paid }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subscription { Pending, Collected, Canceled }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Right { Original, Replacement, Sender, Receiver, ExternalPayee }

#[derive(Clone, Debug, PartialEq, Eq)]
struct Allocation { right: Right, payee: u8, gross: u64, net: u64, flat: u64, forfeit: u64 }
#[derive(Clone, Debug, PartialEq, Eq)]
struct Freeze { epoch: u16, allocation: Allocation, roots: BTreeSet<Root> }
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version { id: u8, freeze: Freeze, miner: u64, subsidy: u64, beyond_flat: u64 }
#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt { allocation: Allocation, own_exit_fee: u64 }

/// All roots are physical assets. Old/new aliases share one economic right,
/// while creating a new tree/grant moves operator reserve rather than minting income.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Model {
	case: Case,
	gross: u64,
	refresh_fee: u64,
	dust: [u64; 2],
	roots: BTreeMap<Root, u64>,
	mature: BTreeSet<Root>,
	mempool: BTreeSet<Root>,
	retired: BTreeSet<Root>,
	reserve: u64,
	opening: u64,
	injections: u64,
	external_receipts: u64,
	withdrawals: u64,
	miner: u64,
	router: u64,
	router_recorded: bool,
	outgoing: Outgoing,
	subscription: Subscription,
	preimage: bool,
	late_fence: bool,
	forfeits_complete: bool,
	cosmetic_spent: bool,
	foreign_original: bool,
	bridge_unknown: bool,
	foreign_unclassified_net: u64,
	freeze: Option<Freeze>,
	versions: Vec<Version>,
	epoch: u16,
	winning_epoch: BTreeMap<u16, u8>,
	receipts: Vec<Receipt>,
}

#[derive(Clone, Debug)]
enum Event {
	CreateRepresentation,
	Mature(Root),
	CosmeticSpent,
	CompleteForfeits,
	Outgoing(Outgoing),
	PreimageKnown,
	Subscription(Subscription),
	CloseLateRequestWindow,
	LateNodeRequest,
	MempoolConflict(Root),
	RestartLoseMempool,
	ForeignOriginalExit(Root, u64),
	ConfirmedGrantTimeout(u64),
	ForeignGrantClaim(u64),
	Freeze { roots: BTreeSet<Root>, payee: u8 },
	Carrier { miner: u64, payee: u8, net: u64 },
	Confirm(u8),
	Inject(u64),
	Withdraw(u64),
	OperatorRecover(Root, u64),
	RouterFee(u64),
	ConflictingRecord,
}

impl Model {
	fn new(case: Case, gross: u64, refresh_fee: u64, dust: [u64; 2]) -> Self {
		assert!(gross > 0 && gross <= 200_000 && refresh_fee < gross);
		let roots: BTreeMap<Root, u64> = match case {
			Case::ExternalReceive => BTreeMap::new(),
			Case::Exchange => [(Root::OldA, gross / 2), (Root::OldB, gross.checked_sub(gross / 2).unwrap())].into_iter().collect(),
			_ => [(Root::Sender, gross)].into_iter().collect(),
		};
		let reserve = 1_000_000;
		let opening = sum(roots.values().copied().chain([reserve]));
		Self { case, gross, refresh_fee, dust, roots, mature: BTreeSet::new(), mempool: BTreeSet::new(),
			retired: BTreeSet::new(), reserve, opening, injections: 0, external_receipts: 0, withdrawals: 0,
			miner: 0, router: 0, router_recorded: false, outgoing: Outgoing::Unknown, subscription: Subscription::Pending, preimage: false,
			late_fence: false, forfeits_complete: false, cosmetic_spent: false, foreign_original: false, bridge_unknown: false, foreign_unclassified_net: 0,
			freeze: None, versions: Vec::new(), epoch: 0, winning_epoch: BTreeMap::new(), receipts: Vec::new() }
	}

	fn closed_gross(&self) -> u64 { sum(self.receipts.iter().map(|r| r.allocation.gross)) }
	fn remaining(&self) -> u64 {
		let budget = if self.receipts.iter().any(|r| r.allocation.right == Right::Replacement) {
			self.gross.checked_sub(self.refresh_fee).unwrap()
		} else { self.gross };
		budget.checked_sub(self.closed_gross()).unwrap()
	}
	fn held(&self) -> u64 {
		self.versions.iter().filter(|v| self.freeze.as_ref().is_some_and(|f| f.epoch == v.freeze.epoch))
			.map(|v| v.subsidy).max().unwrap_or(0)
	}

	/// Spec decision table, expressed only in independently supplied typed facts.
	/// Mixed provenance and canceled claim-capable external grants remain gaps.
	fn unresolved(&self) -> bool {
		self.case == Case::Mixed || self.bridge_unknown ||
			(matches!(self.case, Case::Intra | Case::ExternalSend) && self.preimage && self.outgoing != Outgoing::Paid) ||
			(self.case == Case::ExternalReceive && (self.subscription != Subscription::Collected || self.retired.contains(&Root::Grant))) ||
			(self.case == Case::Intra && (
				(self.subscription == Subscription::Canceled && (self.preimage || self.outgoing == Outgoing::Paid)) ||
				(self.outgoing == Outgoing::Failed && (self.preimage || self.subscription == Subscription::Collected)) ||
				(self.retired.contains(&Root::Grant) && self.closed_gross() == 0 &&
					(self.preimage || self.subscription == Subscription::Collected || self.outgoing == Outgoing::Paid))))
	}

	fn candidate(&self) -> Option<(Right, u8, u64, BTreeSet<Root>)> {
		if self.remaining() == 0 || self.unresolved() { return None; }
		let (right, payee, gross, roots): (Right, u8, u64, BTreeSet<Root>) = match self.case {
			Case::Exchange => {
				if !self.roots.contains_key(&Root::Replacement) { return None; }
				if self.foreign_original {
					let gross = sum([Root::OldA, Root::OldB].iter().filter_map(|r| self.roots.get(r)).copied());
					(Right::Original, 0, gross, self.roots.keys().copied().collect())
				} else if self.forfeits_complete {
					(Right::Replacement, 0, self.gross.checked_sub(self.refresh_fee).unwrap(), [Root::Replacement].into_iter().collect())
				} else {
					(Right::Replacement, 0, self.gross.checked_sub(self.refresh_fee).unwrap(), self.roots.keys().copied().collect())
				}
			},
			Case::Intra => {
				if self.outgoing == Outgoing::Paid && self.subscription == Subscription::Collected {
					if !self.roots.contains_key(&Root::Grant) { return None; }
					(Right::Receiver, 1, self.gross, self.roots.keys().copied().collect())
				} else if self.outgoing == Outgoing::Failed && self.late_fence && !self.preimage && self.subscription != Subscription::Collected {
					(Right::Sender, 0, self.gross, self.roots.keys().copied().collect())
				} else { return None; }
			},
			Case::ExternalSend => {
				if self.outgoing != Outgoing::Failed || !self.late_fence || self.preimage { return None; }
				(Right::Sender, 0, self.gross, [Root::Sender].into_iter().collect())
			},
			Case::ExternalReceive => {
				if self.subscription != Subscription::Collected || !self.roots.contains_key(&Root::Grant) { return None; }
				(Right::Receiver, 1, self.gross, [Root::Grant].into_iter().collect())
			},
			Case::Mixed => return None,
		};
		if gross == 0 || roots.iter().any(|r| !self.mature.contains(r)) { return None; }
		Some((right, payee, gross, roots))
	}

	fn allocation(&self, right: Right, payee: u8, gross: u64) -> Allocation {
		match gross.checked_sub(FLAT).filter(|net| *net >= self.dust[usize::from(payee)]) {
			Some(net) => Allocation { right, payee, gross, net, flat: FLAT, forfeit: 0 },
			None => Allocation { right, payee, gross, net: 0, flat: 0, forfeit: gross },
		}
	}

	fn transition(&mut self, event: &Event) -> Result<(), &'static str> {
		match event {
			Event::CreateRepresentation => {
				if self.freeze.is_some() || self.late_fence || self.remaining() == 0 { return Err("issuance fence"); }
				if self.case == Case::ExternalSend { return Err("external outgoing has no local receive grant"); }
				let (root, value) = if self.case == Case::Exchange {
					(Root::Replacement, self.gross.checked_sub(self.refresh_fee).unwrap())
				} else { (Root::Grant, self.gross) };
				if self.roots.contains_key(&root) || self.retired.contains(&root) { return Ok(()); }
				self.reserve = self.reserve.checked_sub(value).ok_or("capital unavailable")?;
				self.roots.insert(root, value);
			},
			Event::Mature(root) => { if self.roots.contains_key(root) { self.mature.insert(*root); } },
			Event::CosmeticSpent => self.cosmetic_spent = true,
			Event::CompleteForfeits => {
				if self.case != Case::Exchange || self.foreign_original || self.freeze.is_some() || !self.roots.contains_key(&Root::Replacement) {
					return Err("completion loses arbitration");
				}
				self.forfeits_complete = true;
			},
			Event::Outgoing(fact) => {
				if self.case == Case::Exchange || self.case == Case::ExternalReceive { return Err("wrong node provenance"); }
				if self.remaining() == 0 { return Err("economic outcome already terminal"); }
				if self.outgoing == Outgoing::Paid && *fact != Outgoing::Paid { return Err("paid outgoing fact is immutable"); }
				if self.outgoing == Outgoing::Failed && self.late_fence && *fact != Outgoing::Failed { return Err("fenced final failure is immutable"); }
				if self.freeze.is_some() && *fact != self.outgoing { return Err("frozen node arbitration"); }
				if *fact == Outgoing::Failed && (self.preimage || self.outgoing == Outgoing::Paid) { return Err("success is not failure"); }
				if *fact == Outgoing::Paid && self.outgoing == Outgoing::Failed && self.late_fence { return Err("closed final failure"); }
				self.outgoing = *fact;
				if matches!(self.case, Case::ExternalSend | Case::Mixed) && *fact == Outgoing::Paid {
					self.reserve = self.reserve.checked_sub(self.gross).ok_or("outgoing reserve unavailable")?;
					self.receipts.push(Receipt { allocation: Allocation { right: Right::ExternalPayee, payee: 2,
						gross: self.gross, net: self.gross, flat: 0, forfeit: 0 }, own_exit_fee: 0 });
				}
			},
			Event::PreimageKnown => {
				if self.freeze.as_ref().is_some_and(|f| f.allocation.right == Right::Sender) { return Err("refund fence won"); }
				self.preimage = true;
			},
			Event::Subscription(fact) => {
				if self.case == Case::Exchange || self.case == Case::ExternalSend { return Err("wrong subscription provenance"); }
				if self.subscription == Subscription::Collected && *fact != Subscription::Collected { return Err("collected receipt is immutable"); }
				if self.freeze.is_some() && *fact != self.subscription { return Err("frozen collection arbitration"); }
				if self.subscription == Subscription::Canceled && *fact != Subscription::Canceled { return Err("terminal canceled subscription is immutable, not a receipt"); }
				if matches!(self.case, Case::ExternalReceive | Case::Mixed) && *fact == Subscription::Collected && self.subscription != *fact {
					// Actual external receipt, distinct from capital injection or preimage storage.
					self.external_receipts = self.external_receipts.checked_add(self.gross).ok_or("boundary receipt overflow")?;
					self.reserve = self.reserve.checked_add(self.gross).ok_or("receipt reserve overflow")?;
				}
				self.subscription = *fact;
			},
			Event::CloseLateRequestWindow => self.late_fence = true,
			Event::LateNodeRequest => {
				if self.late_fence || self.freeze.is_some() || self.remaining() == 0 { return Err("late request fenced"); }
			},
			Event::MempoolConflict(root) => { self.mempool.insert(*root); },
			Event::RestartLoseMempool => self.mempool.clear(),
			Event::ForeignOriginalExit(root, fee) => {
				if self.case != Case::Exchange || !matches!(root, Root::OldA | Root::OldB) || self.forfeits_complete { return Err("not a live original right"); }
				let gross = self.roots.remove(root).ok_or("already consumed")?;
				let net = gross.checked_sub(*fee).ok_or("exit fee exceeds principal")?;
				self.miner = self.miner.checked_add(*fee).ok_or("exit miner overflow")?;
				self.receipts.push(Receipt { allocation: Allocation { right: Right::Original, payee: 0, gross, net, flat: 0, forfeit: 0 }, own_exit_fee: *fee });
				self.retired.insert(*root); self.foreign_original = true;
				self.freeze = None; // Confirmed foreign consumption invalidates conditional proposal, not a payment.
			},
			Event::ConfirmedGrantTimeout(fee) => {
				if self.subscription == Subscription::Collected || self.outgoing == Outgoing::Paid { return Err("collection cannot be timeout recovery"); }
				let value = self.roots.remove(&Root::Grant).ok_or("grant absent or already retired")?;
				self.reserve = self.reserve.checked_add(value.checked_sub(*fee).ok_or("timeout fee exceeds grant")?).ok_or("reserve overflow")?;
				self.miner = self.miner.checked_add(*fee).ok_or("miner overflow")?;
				self.retired.insert(Root::Grant); self.freeze = None;
				// No assertion that canceled external + preimage resolves economic ownership.
			},
			Event::ForeignGrantClaim(fee) => {
				if self.case == Case::Exchange || self.case == Case::ExternalSend { return Err("no grant provenance"); }
				let gross = self.roots.remove(&Root::Grant).ok_or("grant already retired")?;
				let net = gross.checked_sub(*fee).ok_or("grant exit fee exceeds value")?;
				self.miner = self.miner.checked_add(*fee).ok_or("miner overflow")?;
				self.retired.insert(Root::Grant); self.freeze = None;
				if (self.case == Case::Intra && self.outgoing == Outgoing::Paid && self.subscription == Subscription::Collected) ||
					(self.case == Case::ExternalReceive && self.subscription == Subscription::Collected) {
					self.receipts.push(Receipt { allocation: Allocation { right: Right::Receiver, payee: 1,
						gross, net, flat: 0, forfeit: 0 }, own_exit_fee: *fee });
				} else {
					// Actual bridge principal left. Do not invent its economic relation
					// to a separately failed outgoing or canceled external invoice.
					self.foreign_unclassified_net = self.foreign_unclassified_net.checked_add(net).ok_or("bridge amount overflow")?;
					self.bridge_unknown = true;
				}
			},
			Event::Freeze { roots, payee } => {
				if let Some(frozen) = &self.freeze {
					return if frozen.roots == *roots && frozen.allocation.payee == *payee { Ok(()) } else { Err("immutable freeze") };
				}
				let (right, owner, gross, expected) = self.candidate().ok_or("ownership or maturity unresolved")?;
				if *roots != expected || *payee != owner { return Err("incomplete closure or wrong immutable payee"); }
				self.epoch = self.epoch.checked_add(1).ok_or("epoch bound")?;
				self.freeze = Some(Freeze { epoch: self.epoch, allocation: self.allocation(right, owner, gross), roots: expected });
			},
			Event::Carrier { miner, payee, net } => {
				let frozen = self.freeze.clone().ok_or("no allocation")?;
				if self.versions.iter().filter(|v| v.freeze.epoch == frozen.epoch).count() >= MAX_VERSIONS || *miner > 30_000 { return Err("carrier work or fee bound"); }
				if *payee != frozen.allocation.payee || *net != frozen.allocation.net { return Err("replacement changes customer allocation"); }
				let backing = sum(frozen.roots.iter().map(|r| self.roots.get(r).copied().unwrap_or(0)));
				if frozen.roots.iter().any(|r| !self.roots.contains_key(r)) || backing < *net { return Err("backing no longer complete"); }
				let spend = net.checked_add(*miner).ok_or("carrier total overflow")?;
				let subsidy = spend.saturating_sub(backing);
				if subsidy > self.reserve { return Err("fee capital unavailable, never forfeiture"); }
				self.versions.push(Version { id: u8::try_from(self.versions.len()).unwrap(), freeze: frozen.clone(), miner: *miner,
					subsidy, beyond_flat: miner.saturating_sub(frozen.allocation.flat) });
			},
			Event::Confirm(id) => {
				let version = self.versions.iter().find(|v| v.id == *id).cloned().ok_or("unknown carrier")?;
				if let Some(winner) = self.winning_epoch.get(&version.freeze.epoch) {
					return if winner == id { Ok(()) } else { Err("different carrier already confirmed") };
				}
				if self.freeze.as_ref() != Some(&version.freeze) { return Err("stale conditional carrier"); }
				let backing = sum(version.freeze.roots.iter().map(|r| self.roots.get(r).copied().unwrap_or(0)));
				if version.freeze.roots.iter().any(|r| !self.roots.contains_key(r)) { return Err("confirmed foreign winner invalidates carrier"); }
				let total = backing.checked_add(version.subsidy).ok_or("input amount overflow")?;
				let surplus = total.checked_sub(sum([version.freeze.allocation.net, version.miner])).ok_or("outputs exceed inputs")?;
				self.reserve = self.reserve.checked_sub(version.subsidy).and_then(|r| r.checked_add(surplus)).ok_or("reserve accounting overflow")?;
				self.miner = self.miner.checked_add(version.miner).ok_or("miner overflow")?;
				for root in &version.freeze.roots { self.roots.remove(root); self.retired.insert(*root); }
				self.receipts.push(Receipt { allocation: version.freeze.allocation.clone(), own_exit_fee: 0 });
				self.winning_epoch.insert(version.freeze.epoch, *id); self.freeze = None;
			},
			Event::Inject(value) => {
				let injections = self.injections.checked_add(*value).ok_or("injection overflow")?;
				if self.opening.checked_add(injections).and_then(|v| v.checked_add(self.external_receipts)).is_none_or(|v| v > MAX_MONEY) { return Err("declared boundary amount bound"); }
				self.injections = injections;
				self.reserve = self.reserve.checked_add(*value).ok_or("reserve overflow")?;
			},
			Event::Withdraw(value) => {
				if *value > self.reserve.checked_sub(self.held()).unwrap_or(0) { return Err("reserved fee funds cannot leave"); }
				self.reserve = self.reserve.checked_sub(*value).ok_or("reserve unavailable")?;
				self.withdrawals = self.withdrawals.checked_add(*value).ok_or("withdrawal overflow")?;
			},
			Event::OperatorRecover(root, fee) => {
				let authorized = self.remaining() == 0 || (self.case == Case::Exchange && self.forfeits_complete && matches!(root, Root::OldA | Root::OldB));
				if self.bridge_unknown || (self.case == Case::Mixed && *root == Root::Grant) || !authorized || !self.mature.contains(root) || self.freeze.as_ref().is_some_and(|f| f.roots.contains(root)) { return Err("operator ownership or complete retirement unresolved"); }
				let value = self.roots.remove(root).ok_or("already retired")?;
				self.reserve = self.reserve.checked_add(value.checked_sub(*fee).ok_or("recovery fee exceeds input")?).ok_or("reserve overflow")?;
				self.miner = self.miner.checked_add(*fee).ok_or("miner overflow")?;
				self.retired.insert(*root);
			},
			Event::RouterFee(value) => {
				if self.case != Case::ExternalSend || self.outgoing != Outgoing::Paid { return Err("no confirmed paid route cost"); }
				if self.router_recorded { return if self.router == *value { Ok(()) } else { Err("route cost receipt changed") }; }
				self.reserve = self.reserve.checked_sub(*value).ok_or("router capital unavailable")?;
				self.router = *value; self.router_recorded = true;
			},
			Event::ConflictingRecord => return Err("immutable authenticated destination"),
		}
		Ok(())
	}

	fn apply(&mut self, event: &Event) -> Result<(), &'static str> {
		let mut next = self.clone();
		next.transition(event)?;
		*self = next;
		Ok(())
	}

	/// Independent audits of receipts/assets, not candidate() as expected oracle.
	fn invariant(&self) -> Result<(), &'static str> {
		let assets = sum(self.roots.values().copied().chain([self.reserve]));
		let paid = sum(self.receipts.iter().map(|r| r.allocation.net));
		if sum([self.opening, self.injections, self.external_receipts]) != sum([assets, paid, self.foreign_unclassified_net, self.miner, self.router, self.withdrawals]) { return Err("capital does not conserve"); }
		let budget = if self.receipts.iter().any(|r| r.allocation.right == Right::Replacement) { self.gross.checked_sub(self.refresh_fee).unwrap() } else { self.gross };
		if self.closed_gross() > budget { return Err("economic entitlement paid twice"); }
		let rights = self.receipts.iter().map(|r| r.allocation.right).collect::<Vec<_>>();
		if self.case == Case::Exchange && !self.forfeits_complete && !self.foreign_original {
			let originals = [Root::OldA, Root::OldB];
			if rights.contains(&Right::Replacement) && originals.iter().any(|r| self.roots.contains_key(r)) {
				return Err("replacement paid before every original retired");
			}
			if self.freeze.as_ref().is_some_and(|f| f.allocation.right == Right::Replacement &&
				originals.iter().any(|r| !f.roots.contains(r))) {
				return Err("incomplete delegated retirement closure");
			}
			if !rights.contains(&Right::Replacement) && originals.iter().any(|r| self.retired.contains(r)) {
				return Err("incomplete exchange original reclaimed independently");
			}
		}
		if self.case == Case::Intra && self.freeze.as_ref().is_some_and(|f| f.allocation.right == Right::Sender &&
			self.roots.contains_key(&Root::Grant) && !f.roots.contains(&Root::Grant)) {
			return Err("refund omits live grant retirement");
		}

		if rights.iter().any(|r| *r == Right::Replacement) && rights.iter().any(|r| *r == Right::Original) { return Err("original and replacement both paid"); }
		if rights.iter().any(|r| *r == Right::Sender) && rights.iter().any(|r| matches!(r, Right::Receiver | Right::ExternalPayee)) { return Err("paid send also refunded"); }
		for receipt in &self.receipts {
			let a = &receipt.allocation;
			let owner = match a.right { Right::Receiver => 1, Right::ExternalPayee => 2, _ => 0 };
			if a.payee != owner { return Err("wrong immutable payee"); }
			if a.gross != sum([a.net, a.flat, a.forfeit, receipt.own_exit_fee]) { return Err("economic disposition does not conserve"); }
			if a.flat != 0 && a.flat != FLAT { return Err("flat charge not per actual output"); }
			if a.forfeit > 0 && (a.net != 0 || a.flat != 0 || a.gross.checked_sub(FLAT).is_some_and(|n| n >= self.dust[usize::from(a.payee)])) { return Err("unauthorized dust forfeiture"); }
		}
		for version in &self.versions {
			if let Some(frozen) = &self.freeze {
				if frozen.epoch == version.freeze.epoch && *frozen != version.freeze { return Err("carrier changes frozen allocation or closure"); }
			}
			if version.beyond_flat != version.miner.saturating_sub(version.freeze.allocation.flat) { return Err("economic fee beyond flat mismatch"); }
		}
		if self.roots.keys().any(|r| self.retired.contains(r)) { return Err("retired physical backing resurrected"); }
		Ok(())
	}
}

struct Trace { seed: u64, events: Vec<String>, model: Model }
impl Trace {
	fn new(seed: u64, model: Model) -> Self { Self { seed, events: vec![format!("initial={model:?}")], model } }
	fn step(&mut self, event: Event) {
		assert!(self.events.len() <= MAX_EVENTS, "bounded reference work");
		let before = self.model.clone();
		let result = self.model.apply(&event);
		self.events.push(format!("{event:?} => {result:?}"));
		if result.is_err() { assert_eq!(self.model, before, "rejected event changed business state"); }
		if let Err(error) = self.model.invariant() { self.fail(error); }
	}
	fn retain(&self, label: &str) -> Option<PathBuf> {
		let folder = std::env::var_os("DIRECT_MODEL_TRACE_DIR")?;
		let folder = PathBuf::from(folder); std::fs::create_dir_all(&folder).unwrap();
		for sequence in 0..10_000u32 {
			let path = folder.join(format!("seed-{}-{label}-{sequence}.trace", self.seed));
			match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
				Ok(mut file) => {
					write!(file, "seed={}\n{}\nfinal={:#?}\n", self.seed, self.events.join("\n"), self.model).unwrap();
					return Some(path);
				},
				Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
				Err(error) => panic!("cannot retain model trace: {error}"),
			}
		}
		panic!("trace retention bound exceeded")
	}
	fn fail(&self, error: &str) -> ! {
		let path = self.retain("failure");
		panic!("seed={} invariant={error}; trace={path:?}\n{}", self.seed, self.events.join("\n"));
	}
	fn freeze(&mut self) {
		if let Some((_, payee, _, roots)) = self.model.candidate() { self.step(Event::Freeze { roots, payee }); }
	}
	fn carrier(&mut self, fee: u64) {
		if let Some(f) = &self.model.freeze { self.step(Event::Carrier { miner: fee, payee: f.allocation.payee, net: f.allocation.net }); }
	}
}

impl Drop for Trace {
	fn drop(&mut self) { if std::thread::panicking() { self.retain("panic"); } }
}

fn prepare(trace: &mut Trace) {
	trace.step(Event::CreateRepresentation);
	for root in [Root::OldA, Root::OldB, Root::Replacement, Root::Sender, Root::Grant] { trace.step(Event::Mature(root)); }
}

fn drain(trace: &mut Trace) {
	if trace.model.unresolved() {
		return; // Unproved provenance/collection is no liveness premise.
	}
	trace.step(Event::Inject(100_000));
	for root in [Root::OldA, Root::OldB, Root::Replacement, Root::Sender, Root::Grant] { trace.step(Event::Mature(root)); }
	if trace.model.freeze.is_none() && trace.model.remaining() > 0 {
		match trace.model.case {
			Case::Intra | Case::ExternalSend if trace.model.outgoing != Outgoing::Paid => {
				trace.step(Event::CloseLateRequestWindow); trace.step(Event::Outgoing(Outgoing::Failed));
			},
			Case::Intra => trace.step(Event::Subscription(Subscription::Collected)),
			_ => (),
		}
		trace.freeze();
	}
	trace.carrier(100);
	if let Some(id) = trace.model.versions.last().map(|v| v.id) { trace.step(Event::Confirm(id)); }
	if trace.model.remaining() != 0 { trace.fail("restored proven prerequisites did not terminate"); }
}

#[test]
fn direct_settlement_model_exchange_both_expiry_orders_and_foreign_winner() {
	for first in [Root::OldA, Root::Replacement] {
		let mut trace = Trace::new(10, Model::new(Case::Exchange, 20_000, 300, [330, 294]));
		trace.step(Event::CreateRepresentation); trace.step(Event::Mature(first)); trace.step(Event::CosmeticSpent);
		assert!(trace.model.candidate().is_none());
		trace.step(Event::OperatorRecover(Root::OldA, 1)); assert!(!trace.model.retired.contains(&Root::OldA));
		prepare(&mut trace); trace.freeze(); trace.carrier(2_000);
		let old = trace.model.versions.last().unwrap().id;
		trace.step(Event::MempoolConflict(Root::OldB));
		assert_eq!(trace.model.closed_gross(), 0); assert!(trace.model.freeze.is_some());
		trace.step(Event::RestartLoseMempool); trace.carrier(3_000);
		assert_eq!(trace.model.versions[0].freeze, trace.model.versions[1].freeze);
		trace.step(Event::ForeignOriginalExit(Root::OldB, 50));
		assert!(trace.model.freeze.is_none()); trace.step(Event::Confirm(old));
		assert_eq!(trace.model.closed_gross(), 10_000);
		drain(&mut trace);
		assert!(trace.model.receipts.iter().all(|r| r.allocation.right == Right::Original));
		trace.retain("exchange-foreign-green");
	}
}

#[test]
fn direct_settlement_model_old_or_new_carrier_wins_without_repricing() {
	for winner in [0, 1] {
		let mut trace = Trace::new(u64::from(winner), Model::new(Case::Exchange, 50_000, 200, [330, 294]));
		prepare(&mut trace); trace.freeze(); trace.carrier(2_000); trace.carrier(10_000);
		assert_eq!(trace.model.versions[0].subsidy, 0);
		assert!(trace.model.versions[0].beyond_flat > 0, "physical subsidy and economic mining excess differ");
		let frozen = trace.model.freeze.clone().unwrap();
		trace.step(Event::Carrier { miner: 1_000, payee: 1, net: frozen.allocation.net });
		trace.step(Event::Carrier { miner: 1_000, payee: 0, net: frozen.allocation.net.checked_sub(1).unwrap() });
		assert_eq!(trace.model.versions.len(), 2);
		trace.step(Event::RestartLoseMempool); trace.step(Event::CosmeticSpent);
		trace.step(Event::Confirm(winner)); trace.step(Event::Confirm(1u8.checked_sub(winner).unwrap()));
		trace.step(Event::Confirm(winner));
		assert_eq!(trace.model.receipts.len(), 1); assert_eq!(trace.model.receipts[0].allocation, frozen.allocation);
		assert_eq!(trace.model.remaining(), 0);
		trace.retain("carrier-winner-green");
	}
}

#[test]
fn direct_settlement_model_same_node_joint_refund_success_and_late_fence() {
	for success in [false, true] {
		let mut trace = Trace::new(20, Model::new(Case::Intra, 15_000, 0, [330, 294]));
		prepare(&mut trace); trace.step(Event::Outgoing(Outgoing::TemporaryRefusal));
		assert!(trace.model.candidate().is_none());
		if success {
			trace.step(Event::PreimageKnown); assert!(trace.model.candidate().is_none());
			trace.step(Event::Outgoing(Outgoing::Failed)); assert_ne!(trace.model.outgoing, Outgoing::Failed);
			trace.step(Event::Outgoing(Outgoing::Paid)); trace.step(Event::Outgoing(Outgoing::TemporaryRefusal));
			assert_eq!(trace.model.outgoing, Outgoing::Paid); assert!(trace.model.candidate().is_none());
			trace.step(Event::Subscription(Subscription::Collected));
		} else {
			trace.step(Event::Outgoing(Outgoing::Failed)); assert!(trace.model.candidate().is_none());
			trace.step(Event::CloseLateRequestWindow);
		}
		trace.freeze(); let frozen = trace.model.freeze.clone().unwrap();
		assert_eq!(frozen.roots, [Root::Sender, Root::Grant].into_iter().collect());
		let before = trace.model.clone(); trace.step(Event::LateNodeRequest); assert_eq!(trace.model, before);
		trace.carrier(100); trace.step(Event::Confirm(0));
		assert_eq!(trace.model.receipts.len(), 1);
		assert_eq!(trace.model.receipts[0].allocation.payee, if success { 1 } else { 0 });
		assert!(trace.model.retired.contains(&Root::Grant));
		trace.retain("same-node-green");
	}
}

#[test]
fn direct_settlement_model_external_preimage_cancellation_and_mixed_stay_distinct() {
	for case in [Case::ExternalReceive, Case::Mixed] {
		let mut trace = Trace::new(30, Model::new(case, 20_000, 0, [330, 294]));
		prepare(&mut trace); trace.step(Event::PreimageKnown);
		assert_eq!(trace.model.external_receipts, 0); assert!(trace.model.candidate().is_none());
		trace.step(Event::Subscription(Subscription::Canceled)); trace.step(Event::Outgoing(Outgoing::Failed));
		trace.step(Event::CloseLateRequestWindow);
		assert!(trace.model.candidate().is_none()); assert_eq!(trace.model.closed_gross(), 0);
		trace.step(Event::ForeignGrantClaim(20));
		assert!(trace.model.bridge_unknown); assert!(trace.model.candidate().is_none());
		assert_eq!(trace.model.foreign_unclassified_net, 19_980);
		assert_eq!(trace.model.external_receipts, 0); assert_eq!(trace.model.closed_gross(), 0);
		trace.retain("claim-capable-unqualified");
	}
	let mut collected = Trace::new(31, Model::new(Case::ExternalReceive, 20_000, 0, [330, 294]));
	prepare(&mut collected); collected.step(Event::Subscription(Subscription::Collected));
	collected.step(Event::Subscription(Subscription::Collected));
	assert_eq!(collected.model.external_receipts, 20_000); assert_eq!(collected.model.injections, 0);
	collected.freeze(); collected.carrier(100); collected.step(Event::Confirm(0));
	assert_eq!(collected.model.receipts[0].allocation.payee, 1);
	let mut paid = Trace::new(32, Model::new(Case::ExternalSend, 20_000, 0, [330, 294]));
	paid.step(Event::Outgoing(Outgoing::Paid)); paid.step(Event::RouterFee(75)); paid.step(Event::RouterFee(75)); paid.step(Event::Outgoing(Outgoing::Failed));
	assert_eq!(paid.model.router, 75);
	assert_eq!(paid.model.remaining(), 0); assert!(paid.model.candidate().is_none());
	assert_eq!(paid.model.receipts[0].allocation.right, Right::ExternalPayee);
}

#[test]
fn direct_settlement_model_script_dust_and_fee_funding_are_different() {
	for dust in [294u64, 330] {
		for gross in [1, FLAT, FLAT.checked_add(dust).unwrap().checked_sub(1).unwrap(), FLAT.checked_add(dust).unwrap()] {
			let mut trace = Trace::new(gross, Model::new(Case::ExternalSend, gross, 0, [dust, dust]));
			trace.step(Event::Mature(Root::Sender)); trace.step(Event::Outgoing(Outgoing::Failed));
			trace.step(Event::CloseLateRequestWindow); trace.freeze();
			let plan = trace.model.freeze.clone().unwrap();
			trace.step(Event::Withdraw(trace.model.reserve)); trace.carrier(2_000);
			assert!(trace.model.versions.is_empty()); assert_eq!(trace.model.closed_gross(), 0);
			assert_eq!(trace.model.freeze.as_ref(), Some(&plan));
			trace.step(Event::Inject(u64::MAX)); assert_eq!(trace.model.reserve, 0);
			trace.step(Event::Inject(10_000)); trace.carrier(2_000); trace.step(Event::Confirm(0));
			let receipt = &trace.model.receipts[0].allocation;
			if gross == FLAT.checked_add(dust).unwrap() { assert_eq!(receipt.net, dust); assert_eq!(receipt.flat, FLAT); }
			else { assert_eq!(receipt.forfeit, gross); assert_eq!(receipt.flat, 0); }
			assert_eq!(trace.model.remaining(), 0);
		}
	}
}

fn generated_event(rng: &mut StdRng, model: &Model) -> Event {
	let root = [Root::OldA, Root::OldB, Root::Replacement, Root::Sender, Root::Grant][rng.random_range(0..5)];
	match rng.random_range(0..25) {
		0 => Event::CreateRepresentation,
		1 => Event::Mature(root),
		2 => Event::CosmeticSpent,
		3 => Event::CompleteForfeits,
		4 => Event::Outgoing(Outgoing::TemporaryRefusal),
		5 => Event::Outgoing(Outgoing::Failed),
		6 => Event::Outgoing(Outgoing::Paid),
		7 => Event::PreimageKnown,
		8 => Event::Subscription(Subscription::Collected),
		9 => Event::Subscription(Subscription::Canceled),
		10 => Event::CloseLateRequestWindow,
		11 => Event::LateNodeRequest,
		12 => Event::MempoolConflict(root),
		13 => Event::RestartLoseMempool,
		14 => Event::ForeignOriginalExit(root, rng.random_range(0..100)),
		15 => Event::ConfirmedGrantTimeout(rng.random_range(0..100)),
		16 => Event::ForeignGrantClaim(rng.random_range(0..100)),
		17 => Event::Freeze { roots: model.roots.keys().copied().collect(), payee: rng.random_range(0..2) },
		18 => Event::Freeze { roots: [root].into_iter().collect(), payee: rng.random_range(0..2) },
		19 => Event::Carrier { miner: rng.random_range(0..30_001), payee: model.freeze.as_ref().map(|f| f.allocation.payee).unwrap_or(0),
			net: model.freeze.as_ref().map(|f| f.allocation.net).unwrap_or(0) },
		20 => Event::Confirm(rng.random_range(0..8)),
		21 => Event::Inject(rng.random_range(0..50_000)),
		22 => Event::Withdraw(rng.random_range(0..50_000)),
		23 => Event::OperatorRecover(root, rng.random_range(0..100)),
		_ => Event::ConflictingRecord,
	}
}

#[test]
fn direct_settlement_model_seeded_property_sequences() {
	let selected = std::env::var("DIRECT_MODEL_SEED").ok().map(|s| s.parse::<u64>().unwrap());
	let seeds = selected.map(|seed| vec![seed]).unwrap_or_else(|| (0..512).collect());
	let mut steps = 0usize;
	let mut unresolved = 0usize;
	for seed in seeds {
		let mut rng = StdRng::seed_from_u64(seed);
		let case = [Case::Exchange, Case::Intra, Case::ExternalSend, Case::ExternalReceive, Case::Mixed][rng.random_range(0..5)];
		let gross = rng.random_range(1..200_001);
		let refresh = rng.random_range(0..gross);
		let mut trace = Trace::new(seed, Model::new(case, gross, refresh, [330, 294]));
		prepare(&mut trace);
		for _ in 0..128 {
			let event = generated_event(&mut rng, &trace.model); trace.step(event);
			steps = steps.checked_add(1).unwrap();
		}
		if trace.model.unresolved() {
			unresolved = unresolved.checked_add(1).unwrap();
		} else { drain(&mut trace); }
		if selected.is_some() || seed % 64 == 0 { trace.retain("replay-green"); }
	}
	println!("independent model only: random_steps={steps}; explicitly_unresolved_traces={unresolved}; no product qualification");
}

#[test]
fn direct_settlement_model_invariant_audit_detects_wrong_payment_and_missing_capital() {
	let mut trace = Trace::new(900, Model::new(Case::ExternalSend, 20_000, 0, [330, 294]));
	trace.step(Event::Mature(Root::Sender)); trace.step(Event::Outgoing(Outgoing::Failed));
	trace.step(Event::CloseLateRequestWindow); trace.freeze(); trace.carrier(100); trace.step(Event::Confirm(0));
	let mut wrong = trace.model.clone(); wrong.receipts[0].allocation.payee = 1;
	assert_eq!(wrong.invariant(), Err("wrong immutable payee"));
	let mut duplicate = trace.model.clone(); duplicate.receipts.push(duplicate.receipts[0].clone());
	duplicate.reserve = duplicate.reserve.checked_sub(duplicate.receipts[0].allocation.net).unwrap();
	assert_eq!(duplicate.invariant(), Err("economic entitlement paid twice"));
	let mut missing = trace.model.clone(); missing.reserve = missing.reserve.checked_add(1).unwrap();
	assert_eq!(missing.invariant(), Err("capital does not conserve"));
	for (name, model) in [("mutant-wrong-payee", wrong), ("mutant-double-economic-pay", duplicate), ("mutant-missing-capital", missing)] {
		let mutant = Trace { seed: trace.seed, events: trace.events.clone(), model };
		mutant.retain(name);
	}
}

#[test]
fn direct_settlement_model_independent_audit_detects_split_exchange_and_refund_closure() {
	let mut exchange = Trace::new(901, Model::new(Case::Exchange, 20_000, 100, [330, 294]));
	prepare(&mut exchange); exchange.freeze(); exchange.carrier(100);
	let mut split = exchange.model.clone();
	split.freeze.as_mut().unwrap().roots.remove(&Root::OldB);
	for v in &mut split.versions { v.freeze.roots.remove(&Root::OldB); }
	assert_eq!(split.invariant(), Err("incomplete delegated retirement closure"));
	let mut refund = Trace::new(902, Model::new(Case::Intra, 20_000, 0, [330, 294]));
	prepare(&mut refund); refund.step(Event::Outgoing(Outgoing::Failed)); refund.step(Event::CloseLateRequestWindow);
	refund.freeze(); refund.carrier(100);
	let mut missing = refund.model.clone(); missing.freeze.as_mut().unwrap().roots.remove(&Root::Grant);
	for v in &mut missing.versions { v.freeze.roots.remove(&Root::Grant); }
	assert_eq!(missing.invariant(), Err("refund omits live grant retirement"));
	Trace { seed: 901, events: exchange.events.clone(), model: split }.retain("mutant-split-exchange");
	Trace { seed: 902, events: refund.events.clone(), model: missing }.retain("mutant-refund-omits-grant");
}

#[test]
fn direct_settlement_model_terminal_facts_do_not_reopen() {
	let mut node = Trace::new(903, Model::new(Case::ExternalSend, 20_000, 0, [330, 294]));
	prepare(&mut node); node.step(Event::Outgoing(Outgoing::Failed)); node.step(Event::CloseLateRequestWindow);
	node.step(Event::Outgoing(Outgoing::TemporaryRefusal)); node.step(Event::Outgoing(Outgoing::Paid));
	let failure_closed = node.model.outgoing == Outgoing::Failed && node.model.closed_gross() == 0;
	let mut receive = Trace::new(904, Model::new(Case::ExternalReceive, 20_000, 0, [330, 294]));
	prepare(&mut receive); receive.step(Event::PreimageKnown); receive.step(Event::Subscription(Subscription::Canceled));
	receive.step(Event::Subscription(Subscription::Pending)); receive.step(Event::Subscription(Subscription::Collected));
	let canceled_closed = receive.model.subscription == Subscription::Canceled && receive.model.external_receipts == 0 && receive.model.candidate().is_none();
	assert_eq!((failure_closed, canceled_closed), (true, true), "terminal typed facts cannot be reset into success");
	node.retain("terminal-failure-green"); receive.retain("terminal-cancel-unqualified");
}
