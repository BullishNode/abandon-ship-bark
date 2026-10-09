//! Print the exact exit path of a hex-encoded VTXO read from stdin.
use std::io::Read;
use ark::{ProtocolEncoding, Vtxo};
use bitcoin::hex::{DisplayHex, FromHex};
fn main() -> anyhow::Result<()> {
	let mut raw = String::new();
	std::io::stdin().read_to_string(&mut raw)?;
	let vtxo: Vtxo = Vtxo::deserialize(&Vec::from_hex(raw.trim())?)?;
	if std::env::args().any(|a| a == "--json") {
		println!("{}", serde_json::json!({"id": vtxo.id(), "user_pubkey": vtxo.user_pubkey(),
			"amount_sat": vtxo.amount().to_sat(), "transactions": vtxo.transactions().map(|t|
				serde_json::json!({"outpoint": format!("{}:{}", t.tx.compute_txid(), t.output_idx),
					"raw": bitcoin::consensus::serialize(&t.tx).as_hex().to_string()})).collect::<Vec<_>>() }));
		return Ok(());
	}
	println!("{}", vtxo.chain_anchor());
	for t in vtxo.transactions() {
		println!("{}:{}", t.tx.compute_txid(), t.output_idx);
	}
	Ok(())
}
