//! Recovery helper: reads a Bark wallet mnemonic on stdin and prints the
//! private descriptor of its coin keys, `tr(<xprv>/350'/0'/*)`, so payouts
//! to `tr(coin_pubkey)` can be swept with any descriptor wallet.
//! Usage: echo "<mnemonic>" | cargo run --example coin_key_descriptor -- regtest

use std::io::Read;
use std::str::FromStr;

use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::secp256k1::Secp256k1;

fn main() -> anyhow::Result<()> {
	let network = bitcoin::Network::from_str(&std::env::args().nth(1).unwrap_or("bitcoin".into()))?;
	let mut words = String::new();
	std::io::stdin().read_to_string(&mut words)?;
	let seed = bip39::Mnemonic::parse(words.trim())?.to_seed("");
	let secp = Secp256k1::new();
	let master = Xpriv::new_master(network, &seed)?;
	let path = DerivationPath::from_str("m/350'/0'")?;
	let xprv = master.derive_priv(&secp, &path)?;
	// A fixed public key also lets the sparse regtest fixture advance its
	// valid derivation history without generating a million unused addresses.
	if let Some(index) = std::env::args().nth(2) {
		let child = xprv.derive_priv(&secp, &[ChildNumber::from_normal_idx(index.parse()?)?])?;
		println!("{}", bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &child.private_key));
		return Ok(());
	}
	println!("tr({xprv}/*)");
	Ok(())
}
