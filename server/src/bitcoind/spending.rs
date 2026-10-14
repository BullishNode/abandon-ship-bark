//! Confirmed and mempool backing spends from Core's synchronized reverse index.

use std::collections::BTreeMap;

use anyhow::{ensure, Context};
use bitcoin::{BlockHash, OutPoint, Transaction, Txid};
use bitcoin::consensus::encode::deserialize_hex;
use bitcoind_async_client::Client;
use serde::Deserialize;

#[derive(Debug)]
pub struct BackingSpend {
	pub transaction: Transaction,
	pub block_hash: Option<BlockHash>,
}

#[derive(Debug, Deserialize)]
struct IndexStatus {
	synced: bool,
	best_block_height: u32,
}

#[derive(Debug, Deserialize)]
struct SpendRow {
	txid: Txid,
	vout: u32,
	spendingtxid: Option<Txid>,
	spendingtx: Option<String>,
	blockhash: Option<BlockHash>,
}

/// A missing or lagging index is a refusal, never evidence that backing is free.
pub async fn require_spender_index(client: &Client) -> anyhow::Result<()> {
	let tip: u32 = client.call_raw("getblockcount", &[]).await?;
	let indices: BTreeMap<String, IndexStatus> = client.call_raw(
		"getindexinfo", &["txospenderindex".into()],
	).await?;
	let index = indices.get("txospenderindex").context("Core txospenderindex is required")?;
	ensure!(index.synced && index.best_block_height >= tip, "Core txospenderindex is behind the chain tip");
	Ok(())
}

/// Retrieve actual spend bytes, including confirmed spends absent from the mempool.
/// Callers still arbitrate entitlement ownership and validate the backing path.
pub async fn backing_spends(
	client: &Client, points: &[OutPoint],
) -> anyhow::Result<Vec<Option<BackingSpend>>> {
	ensure!(!points.is_empty() && points.len() <= 1_000, "backing spend lookup must contain 1..=1000 outpoints");
	require_spender_index(client).await?;
	let rows: Vec<SpendRow> = client.call_raw("gettxspendingprevout", &[
		serde_json::Value::Array(points.iter().map(|point|
			serde_json::json!({ "txid": point.txid, "vout": point.vout })).collect()),
		serde_json::json!({ "mempool_only": false, "return_spending_tx": true }),
	]).await?;
	ensure!(rows.len() == points.len(), "Core backing spend response count differs");
	rows.into_iter().zip(points).map(|(row, point)| {
		ensure!(OutPoint::new(row.txid, row.vout) == *point, "Core backing spend response outpoint differs");
		match (row.spendingtxid, row.spendingtx) {
			(None, None) => {
				ensure!(row.blockhash.is_none(), "unspent backing has a spending block");
				Ok(None)
			},
			(Some(txid), Some(bytes)) => {
				let transaction: Transaction = deserialize_hex(&bytes)?;
				ensure!(transaction.compute_txid() == txid, "Core backing spender bytes differ from txid");
				ensure!(transaction.input.iter().any(|input| input.previous_output == *point),
					"Core backing spender does not consume the requested point");
				Ok(Some(BackingSpend { transaction, block_hash: row.blockhash }))
			},
			_ => anyhow::bail!("Core backing spender response is incomplete"),
		}
	}).collect()
}
