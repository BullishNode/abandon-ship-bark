//! Print the exact exit path of a hex-encoded VTXO read from stdin.
use std::io::Read;
use ark::{ProtocolEncoding, Vtxo};
use bitcoin::hex::FromHex;
fn main() -> anyhow::Result<()> {
	let mut raw = String::new();
	std::io::stdin().read_to_string(&mut raw)?;
	let vtxo: Vtxo = Vtxo::deserialize(&Vec::from_hex(raw.trim())?)?;
	println!("{}", vtxo.chain_anchor());
	for t in vtxo.transactions() {
		println!("{}:{}", t.tx.compute_txid(), t.output_idx);
	}
	Ok(())
}
