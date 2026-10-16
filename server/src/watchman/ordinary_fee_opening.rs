//! Original native fee opening. This validates existing facts; it does not
//! mint an allowance, infer zero history, or reconstruct customer obligations.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use bitcoin::{BlockHash, Network, Transaction};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::PublicKey;
use bitcoind_async_client::{Client, traits::Reader};

use crate::database::Tx;
use crate::wallet::WalletKind;
use super::ordinary_fee::OrdinaryFeePolicy;
use super::ordinary_fee_publication::PublicationRecord;

const MAX_BYTES: usize = 4 * 1024 * 1024;
// No caller SQL or optional tables. Missing source tables are errors.
const TABLES: &[&str] = &["wallet_changeset", "nursery_tx", "round", "round_participation",
	"round_part_input", "round_part_output", "vtxo_pool", "vtxo", "virtual_transaction",
	"pending_board", "offboards", "sweep", "fallback_record", "key_link",
	"expiry_allocation", "expiry_carrier", "expiry_carrier_allocation", "vtxo_pool_retirement",
	"htlc_vtxo", "htlc_settlement", "lightning_htlc_subscription", "lightning_invoice",
	"lightning_payment_attempt", "lightning_payment_attempt_htlc_vtxo"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TableCut { rows: i64, bytes: i64, sha256: sha256::Hash }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletCut {
	descriptor_sha256: sha256::Hash,
	transaction_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalOpening {
	version: u8,
	server_pubkey: PublicKey,
	genesis: BlockHash,
	policy_sha256: sha256::Hash,
	created_at_unix: u64,
	tip_height: u32,
	tip_hash: BlockHash,
	minimum_confirmations: u32,
	// Exact original native journal prefix, including all old full-ceiling
	// carrier reservations. It is retained, never replaced by a net remainder.
	original_history: Vec<u8>,
	previously_charged_sat: u64,
	import_charge_sat: u64,
	remaining_ordinary_sat: u64,
	ordinary_total_sat: u64,
	urgent_floor_sat: u64,
	// Root-owned paused-source manifest and original publication logs must be
	// independently reviewed. Their exact private bytes are hash-bound here.
	source_manifest_sha256: sha256::Hash,
	publication_logs_sha256: BTreeSet<sha256::Hash>,
	tables: BTreeMap<String, TableCut>,
	wallets: BTreeMap<String, WalletCut>,
	external_funding_ids: BTreeSet<String>,
	publications: Vec<PublicationRecord>,
}

/// Constructed only after actual source validation under the DB locks. It is
/// not an R/F proof or authority to sign/publish any transaction.
pub(crate) struct CheckedOriginalOpening {
	sha256: sha256::Hash,
	publications: Vec<PublicationRecord>,
	original_history: Vec<u8>,
	canonical_bytes: Vec<u8>,
}

impl CheckedOriginalOpening {
	pub(crate) fn digest(&self) -> sha256::Hash { self.sha256 }
	pub(crate) fn publications(&self) -> &[PublicationRecord] { &self.publications }
	pub(crate) fn original_history(&self) -> &[u8] { &self.original_history }
	pub(crate) fn canonical_bytes(&self) -> &[u8] { &self.canonical_bytes }
}

/// Decode captured original bytes with the same canonical/accounting checks as
/// activation. These returned facts carry no DB/Core/source validation or
/// authority to import charges, activate an epoch, sign, or publish.
pub(crate) fn decode_captured_original(raw: &[u8], expected: sha256::Hash,
	policy: OrdinaryFeePolicy, server: PublicKey, network: Network,
) -> anyhow::Result<(Vec<u8>, Vec<PublicationRecord>)> {
	let opening = OriginalOpening::decode(raw, expected, policy, server, network)?;
	Ok((opening.original_history, opening.publications))
}

/// The caller supplies only independently protected original bytes/hash and
/// actual native runtime identity. A parsed document alone is not a capability.
pub(crate) async fn verify_original(raw: &[u8], expected: sha256::Hash,
	policy: OrdinaryFeePolicy, server: PublicKey, network: Network,
	t: &Tx<'_>, core: &Client, source_manifest: &[u8], publication_logs: &[Vec<u8>],
	replay: Option<&super::ordinary_fee_epoch::ValidatedOpeningJournal>,
) -> anyhow::Result<CheckedOriginalOpening> {
	let opening = OriginalOpening::decode(raw, expected, policy, server, network)?;
	let replay_original = replay.is_some_and(|proof| proof.matches(expected));
	ensure!(replay.is_none() || replay_original, "original opening replay journal identity differs");
	opening.validate_original(t, core, policy, source_manifest, publication_logs, replay_original).await
}

impl OriginalOpening {
	fn decode(raw: &[u8], expected: sha256::Hash, policy: OrdinaryFeePolicy,
		server: PublicKey, network: Network,
	) -> anyhow::Result<Self> {
		ensure!(raw.len() <= MAX_BYTES && sha256::Hash::hash(raw) == expected,
			"original native fee opening bytes or hash differ");
		let value: Self = serde_json::from_slice(raw)?;
		ensure!(serde_json::to_vec(&value)? == raw, "original native fee opening is not canonical");
		value.accounting(policy, server, network)?;
		Ok(value)
	}
	fn accounting(&self, policy: OrdinaryFeePolicy, server: PublicKey, network: Network) -> anyhow::Result<()> {
		policy.validate_config(network)?;
		ensure!(self.version == 1 && self.server_pubkey == server
			&& self.genesis == bitcoin::constants::genesis_block(network).block_hash()
			&& self.policy_sha256 == sha256::Hash::hash(&serde_json::to_vec(&policy)?),
			"original native fee opening identity differs");
		ensure!(self.ordinary_total_sat == policy.ordinary_budget_sat
			&& self.urgent_floor_sat == policy.urgent_reserve_sat,
			"original fee opening changes total allowance or urgent floor");
		ensure!(self.original_history.len() <= 2 * 1024 * 1024
			&& !self.original_history.windows(3).any(|bytes| bytes == b"p1\t"),
			"original opening cannot relabel another publication epoch");
		let original = super::ordinary_fee::opening_numeric_charges(&self.original_history, policy)?;
		ensure!(original == self.previously_charged_sat, "original carrier debits differ from opening");
		ensure!(self.publications.len() <= 4096, "original publication count exceeds bound");
		let mut ids = BTreeSet::new(); let mut total = 0u64;
		for publication in &self.publications {
			let entry = publication.entry(policy)?;
			ensure!(ids.insert(entry.id()), "duplicate original publication");
			total = total.checked_add(entry.charge(policy)?).context("opening charge overflow")?;
		}
		ensure!(total == self.import_charge_sat
			&& original.checked_add(total).and_then(|sum| sum.checked_add(self.remaining_ordinary_sat))
				== Some(policy.ordinary_budget_sat), "original debits/import/remaining do not conserve allowance");
		ensure!(self.minimum_confirmations > 0 && self.minimum_confirmations <= 10_000,
			"original opening lacks a finite positive confirmation policy");
		ensure!(self.source_manifest_sha256 != sha256::Hash::all_zeros()
			&& !self.publication_logs_sha256.is_empty() && self.publication_logs_sha256.len() <= 16
			&& !self.publication_logs_sha256.contains(&sha256::Hash::all_zeros()),
			"original source manifest or publication log binding missing");
		ensure!(self.tables.keys().map(String::as_str).collect::<BTreeSet<_>>()
			== TABLES.iter().copied().collect(), "original source table coverage differs");
		ensure!(self.wallets.keys().map(String::as_str).collect::<BTreeSet<_>>()
			== ["rounds", "watchman"].into_iter().collect(), "original opening requires both owning wallet graphs");
		Ok(())
	}

	/// Called before importing debits, while the server is externally paused.
	/// Fresh activation must not waive capture age; only an already committed
	/// original activation may later replay its authenticated retained binding.
	async fn validate_original(&self, t: &Tx<'_>, core: &Client,
		policy: OrdinaryFeePolicy, source_manifest: &[u8], publication_logs: &[Vec<u8>], replay_original: bool,
	) -> anyhow::Result<CheckedOriginalOpening> {
		tokio::time::timeout(std::time::Duration::from_secs(120),
			self.validate_original_inner(t, core, policy, source_manifest, publication_logs, replay_original)).await
			.context("original native fee opening absolute deadline exceeded")?
	}
	async fn validate_original_inner(&self, t: &Tx<'_>, core: &Client,
		policy: OrdinaryFeePolicy, source_manifest: &[u8], publication_logs: &[Vec<u8>], replay_original: bool,
	) -> anyhow::Result<CheckedOriginalOpening> {
		let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
		ensure!(replay_original || (now >= self.created_at_unix && now - self.created_at_unix <= 120),
			"original native fee opening requires a fresh paused source cut");
		ensure!(source_manifest.len() <= MAX_BYTES && sha256::Hash::hash(source_manifest) == self.source_manifest_sha256,
			"original paused source manifest differs");
		let mut log_bytes = 0usize; let mut log_ids = BTreeSet::new();
		for log in publication_logs {
			log_bytes = log_bytes.checked_add(log.len()).context("opening logs overflow")?;
			ensure!(log_bytes <= 64 * 1024 * 1024 && log_ids.insert(sha256::Hash::hash(log)),
				"opening publication logs duplicate or exceed bounds");
		}
		ensure!(log_ids == self.publication_logs_sha256, "original publication log set differs");
		t.batch_execute("SET LOCAL statement_timeout='10s'; SET LOCAL lock_timeout='5s';
			SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO,YMD'; SET LOCAL bytea_output='hex';
			SET LOCAL extra_float_digits=3;").await?;
		t.lock_native_fee_publication().await?;
		t.execute(&format!("LOCK TABLE {} IN SHARE MODE", TABLES.join(",")), &[]).await?;
		ensure!(snapshot(t).await? == self.tables, "original native/customer source cut differs");
		ensure!(core.get_block_hash(0).await? == self.genesis
			&& (replay_original || core.get_block_count().await? == u64::from(self.tip_height))
			&& core.get_block_hash(u64::from(self.tip_height)).await? == self.tip_hash,
			"original Core source identity or tip differs");
		self.validate_publications(t, core, policy).await?;
		ensure!((replay_original || core.get_block_count().await? == u64::from(self.tip_height))
			&& core.get_block_hash(u64::from(self.tip_height)).await? == self.tip_hash,
			"original Core source cut changed during validation");
		Ok(CheckedOriginalOpening { sha256: sha256::Hash::hash(&serde_json::to_vec(self)?),
			publications: self.publications.clone(), original_history: self.original_history.clone(),
			canonical_bytes: serde_json::to_vec(self)? })
	}

	async fn validate_publications(&self, t: &Tx<'_>, core: &Client,
		policy: OrdinaryFeePolicy,
	) -> anyhow::Result<()> {
		let mut publications = BTreeMap::new();
		for record in &self.publications {
			let entry = record.entry(policy)?; let tx = record.transaction()?;
			confirmed(core, &serialize(&tx), self.minimum_confirmations).await?;
			for parent in record.input_parents() { confirmed(core, parent, self.minimum_confirmations).await?; }
			ensure!(publications.insert(tx.compute_txid().to_string(), (entry.purpose(), tx)).is_none(),
				"original publication txid repeats");
		}
		let rows = t.query("SELECT txid,kind::TEXT,tx FROM nursery_tx ORDER BY txid LIMIT 4097", &[]).await?;
		ensure!(rows.len() <= 4096, "original nursery exceeds opening bound");
		let mut found = BTreeSet::new();
		for row in rows {
			let id: String = row.get(0); let kind: String = row.get(1); let raw: Vec<u8> = row.get(2);
			let (purpose, tx) = publications.get(&id).context("unsupported previous operator nursery publication")?;
			ensure!(kind == match *purpose { "round" => "round", "pool" => "vtxopool", _ => unreachable!() }
				&& serialize(tx) == raw && found.insert(id), "original nursery candidate differs");
		}
		ensure!(found.len() == publications.len(), "original native publications absent from nursery");
		let rounds = t.query("SELECT funding_txid,funding_tx FROM round ORDER BY id LIMIT 4097", &[]).await?;
		ensure!(rounds.len() <= 4096, "original round count exceeds bound");
		for row in rounds {
			let id: String = row.get(0); let raw: Vec<u8> = row.get(1);
			let (purpose, tx) = publications.get(&id).context("original signed/pending round has no fee provenance")?;
			ensure!(*purpose == "round" && serialize(tx) == raw, "original round differs from fee publication");
		}
		let pool = t.query("SELECT vtxo FROM vtxo_pool ORDER BY id LIMIT 10001", &[]).await?;
		ensure!(pool.len() <= 10_000, "original pool row bound exceeded");
		for row in pool {
			use ark::ProtocolEncoding;
			let vtxo: ark::Vtxo<ark::vtxo::Full> = ark::Vtxo::deserialize(row.get::<_, &[u8]>(0))?;
			let (purpose, _) = publications.get(&vtxo.chain_anchor().txid.to_string())
				.context("original pool output has no funding publication provenance")?;
			ensure!(*purpose == "pool", "original pool output fee purpose differs");
		}
		let mut graph = BTreeMap::new(); let mut owned_outputs = BTreeSet::new();
		let mut wallet_views = Vec::new();
		for kind in [WalletKind::Rounds, WalletKind::Watchman] {
			let state = t.read_aggregate_changeset(kind).await?.context("original owning wallet graph missing")?;
			ensure!(state.tx_graph.txouts.is_empty()
				&& state.locked_outpoints.outpoints.values().all(|locked| !locked),
				"original wallet has partial parents or unresolved input reservations");
			let network = state.network.context("original wallet network missing")?;
			ensure!(bitcoin::constants::genesis_block(network).block_hash() == self.genesis,
				"original wallet network differs");
			let view = bdk_wallet::Wallet::load().check_network(network).load_wallet_no_persist(state.clone())?
				.context("original public owning wallet graph cannot be reconstructed")?;
			let expected = self.wallets.get(kind.name()).context("owning wallet cut missing")?;
			ensure!(state.descriptor.as_ref().map(|d| sha256::Hash::hash(d.to_string().as_bytes()))
				== Some(expected.descriptor_sha256), "original owning descriptor differs");
			ensure!(state.tx_graph.txs.len() <= 4096, "original wallet transaction bound exceeded");
			let mut ids = BTreeSet::new(); let mut bytes = 0usize;
			for tx in &state.tx_graph.txs {
				let raw = serialize(tx.as_ref()); bytes = bytes.checked_add(raw.len()).context("original graph bytes overflow")?;
				ensure!(bytes <= MAX_BYTES, "original wallet graph exceeds byte bound");
				confirmed(core, &raw, self.minimum_confirmations).await?;
				let id = tx.compute_txid().to_string(); ids.insert(id.clone()); graph.insert(id, tx.clone());
				for (vout, output) in tx.output.iter().enumerate() {
					if view.is_mine(output.script_pubkey.clone()) {
						owned_outputs.insert(bitcoin::OutPoint::new(tx.compute_txid(), u32::try_from(vout)?));
					}
				}
			}
			ensure!(ids == expected.transaction_ids, "original owning graph is incomplete or changed");
			wallet_views.push(view);
		}
		let expected = self.external_funding_ids.iter().cloned().chain(publications.keys().cloned()).collect::<BTreeSet<_>>();
		ensure!(self.external_funding_ids.is_disjoint(&publications.keys().cloned().collect())
			&& expected == graph.keys().cloned().collect(), "original native graph classification incomplete");
		let mut parent_cache = BTreeMap::<bitcoin::Txid, Transaction>::new();
		let mut parent_bytes = 0usize;
		for id in &self.external_funding_ids {
			let tx = graph.get(id).context("original external funding absent")?;
			ensure!(tx.input.iter().all(|input| !owned_outputs.contains(&input.previous_output)),
				"original external funding spends an owned output");
			// Inspect actual prevout scripts as well; absence of a parent from
			// the stored graph must not relabel an owned spend as an injection.
			for input in &tx.input {
				if input.previous_output.is_null() { continue; } // external coinbase funding
				if !parent_cache.contains_key(&input.previous_output.txid) {
					ensure!(parent_cache.len() < 4096, "external funding parent count exceeds bound");
					let parent = crate::bitcoind::custom_get_raw_transaction_info(core, input.previous_output.txid, None).await?
						.context("external funding original parent unavailable")?;
					parent_bytes = parent_bytes.checked_add(parent.hex.len()).context("external funding proof overflow")?;
					ensure!(parent.hex.len() <= MAX_BYTES && parent_bytes <= 16 * 1024 * 1024,
						"external funding parent proof exceeds byte bound");
					let parent_tx: Transaction = deserialize(&parent.hex)?;
					ensure!(parent_tx.compute_txid() == input.previous_output.txid, "external funding parent differs");
					parent_cache.insert(input.previous_output.txid, parent_tx);
				}
				let output = parent_cache[&input.previous_output.txid].output.get(input.previous_output.vout as usize)
					.context("external funding prevout missing")?;
				ensure!(wallet_views.iter().all(|view| !view.is_mine(output.script_pubkey.clone())),
					"external funding classification hides an owning descriptor input");
			}
		}
		Ok(())
	}
}

async fn snapshot(t: &Tx<'_>) -> anyhow::Result<BTreeMap<String, TableCut>> {
	let mut cuts = BTreeMap::new(); let mut aggregate = 0i64;
	for table in TABLES {
		let stats = t.query_one(&format!("SELECT count(*),COALESCE(sum(octet_length(row_to_json(x)::text)),0)::bigint,
			COALESCE(max(octet_length(row_to_json(x)::text)),0)::bigint FROM {table} x"), &[]).await?;
		let rows: i64 = stats.get(0); let bytes: i64 = stats.get(1);
		aggregate = aggregate.checked_add(bytes).context("original source bytes overflow")?;
		ensure!(rows <= 10_000 && bytes <= 8 * 1024 * 1024 && stats.get::<_, i64>(2) <= 2 * 1024 * 1024
			&& aggregate <= 16 * 1024 * 1024, "original source cut exceeds bounds");
		let values = t.query(&format!("SELECT row_to_json(x)::text FROM {table} x ORDER BY (row_to_json(x)::text) COLLATE \"C\""), &[]).await?;
		let mut framed = Vec::new();
		for row in values { let raw: String = row.get(0); framed.extend_from_slice(&(raw.len() as u64).to_le_bytes()); framed.extend_from_slice(raw.as_bytes()); }
		cuts.insert((*table).into(), TableCut { rows, bytes, sha256: sha256::Hash::hash(&framed) });
	}
	Ok(cuts)
}
async fn confirmed(core: &Client, raw: &[u8], minimum: u32) -> anyhow::Result<()> {
	let tx: Transaction = deserialize(raw)?;
	ensure!(serialize(&tx) == raw, "original native publication is noncanonical");
	let actual = crate::bitcoind::custom_get_raw_transaction_info(core, tx.compute_txid(), None).await?
		.context("original signed transaction unavailable from Core")?;
	ensure!(actual.hex == raw && actual.confirmations.unwrap_or(0) >= minimum,
		"original native transaction differs or lacks confirmations");
	let block = actual.blockhash.context("original native transaction has no active block")?;
	let height = core.get_block_height(&block).await?;
	ensure!(core.get_block_hash(height).await? == block, "original native transaction block is not active");
	Ok(())
}

// Synthetic in-process ledger fixture only. Never compiled into a daemon and
// never establishes actual original-source admission or runtime authority.
#[cfg(test)]
pub(crate) fn synthetic_checked_opening(original_history: Vec<u8>, publications: Vec<PublicationRecord>) -> CheckedOriginalOpening {
	CheckedOriginalOpening { sha256: sha256::Hash::hash(b"synthetic opening, NOT admission"),
		publications, original_history, canonical_bytes: b"synthetic-only".to_vec() }
}

#[cfg(test)]
mod tests {
	use super::*;
	fn opening() -> (OriginalOpening, OrdinaryFeePolicy) {
		let policy = OrdinaryFeePolicy { ordinary_budget_sat: 58_000, urgent_reserve_sat: 40_000, regtest_qualification: None };
		let key = bitcoin::secp256k1::Keypair::new(&crate::SECP, &mut bitcoin::secp256k1::rand::thread_rng()).public_key();
		let value = OriginalOpening { version: 1, server_pubkey: key,
			genesis: bitcoin::constants::genesis_block(Network::Regtest).block_hash(),
			policy_sha256: sha256::Hash::hash(&serde_json::to_vec(&policy).unwrap()), created_at_unix: 1,
			tip_height: 1, tip_hash: BlockHash::all_zeros(), minimum_confirmations: 3,
			original_history: b"10000\n".to_vec(), previously_charged_sat: 10_000, import_charge_sat: 0,
			remaining_ordinary_sat: 48_000, ordinary_total_sat: 58_000, urgent_floor_sat: 40_000,
			source_manifest_sha256: sha256::Hash::hash(b"synthetic paused manifest, not authority"),
			publication_logs_sha256: [sha256::Hash::hash(b"synthetic original logs")].into_iter().collect(),
			tables: TABLES.iter().map(|name| ((*name).into(), TableCut { rows: 0, bytes: 0,
				sha256: sha256::Hash::hash(b"") })).collect(),
			wallets: ["rounds", "watchman"].into_iter().map(|name| (name.into(), WalletCut {
				descriptor_sha256: sha256::Hash::hash(name.as_bytes()), transaction_ids: BTreeSet::new() })).collect(),
			external_funding_ids: BTreeSet::new(), publications: Vec::new() };
		(value, policy)
	}
	#[test]
	fn original_opening_keeps_nonzero_carrier_history_and_58000_total() {
		let (value, policy) = opening();
		value.accounting(policy, value.server_pubkey, Network::Regtest).unwrap();
		for which in 0..4 {
			let mut bad = value.clone();
			match which { 0 => bad.original_history.clear(), 1 => bad.previously_charged_sat = 0,
				2 => bad.ordinary_total_sat = 60_000, _ => bad.remaining_ordinary_sat = 58_000 }
			assert!(bad.accounting(policy, value.server_pubkey, Network::Regtest).is_err());
		}
	}
	#[test]
	fn original_opening_requires_both_wallets_fixed_source_set_and_original_log_bindings() {
		let (value, policy) = opening();
		for which in 0..4 {
			let mut bad = value.clone();
			match which { 0 => { bad.wallets.remove("watchman"); }, 1 => { bad.tables.remove("round"); },
				2 => bad.source_manifest_sha256 = sha256::Hash::all_zeros(), _ => bad.publication_logs_sha256.clear() }
			assert!(bad.accounting(policy, value.server_pubkey, Network::Regtest).is_err());
		}
	}
	#[test]
	fn original_opening_rejects_changed_identity_noncanonical_and_torn_history() {
		let (value, policy) = opening(); let raw = serde_json::to_vec(&value).unwrap();
		OriginalOpening::decode(&raw, sha256::Hash::hash(&raw), policy, value.server_pubkey, Network::Regtest).unwrap();
		assert!(OriginalOpening::decode(&raw, sha256::Hash::hash(&raw), policy, value.server_pubkey, Network::Signet).is_err());
		let mut padded = raw; padded.push(b'\n');
		assert!(OriginalOpening::decode(&padded, sha256::Hash::hash(&padded), policy, value.server_pubkey, Network::Regtest).is_err());
		let mut torn = value; torn.original_history.pop();
		assert!(torn.accounting(policy, torn.server_pubkey, Network::Regtest).is_err());
	}
	#[test]
	fn captured_original_preserves_nonzero_history_and_refuses_rewritten_or_unbound_opening() {
		let (value, policy) = opening();
		let raw = serde_json::to_vec(&value).unwrap();
		let expected = sha256::Hash::hash(&raw);
		let (history, publications) = decode_captured_original(&raw, expected, policy,
			value.server_pubkey, Network::Regtest).unwrap();
		assert_eq!(history, b"10000\n");
		assert!(publications.is_empty());
		assert!(decode_captured_original(&raw, sha256::Hash::all_zeros(), policy,
			value.server_pubkey, Network::Regtest).is_err());
		assert!(decode_captured_original(&raw, expected, policy,
			value.server_pubkey, Network::Signet).is_err());
		for which in 0..5 {
			let mut bad = value.clone();
			match which {
				0 => { bad.original_history.clear(); bad.previously_charged_sat = 0; },
				1 => bad.remaining_ordinary_sat += 1,
				2 => { bad.wallets.remove("watchman"); },
				3 => bad.publication_logs_sha256.clear(),
				_ => { bad.original_history.pop(); },
			}
			let changed = serde_json::to_vec(&bad).unwrap();
			// Even a matching capture digest cannot authorize rewritten accounting
			// or omitted original source bindings.
			assert!(decode_captured_original(&changed, sha256::Hash::hash(&changed), policy,
				value.server_pubkey, Network::Regtest).is_err());
		}
	}
}
