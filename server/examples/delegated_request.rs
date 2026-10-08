//! Regtest fixture: submit exact replacement requests with genuine input attestations.
//! Generated test mnemonic and serialized inputs arrive on stdin, never argv.
use std::io::Read;
use std::str::FromStr;

use ark::{ProtocolEncoding, Vtxo, VtxoPolicy, VtxoRequest};
use ark::attestations::DelegatedRoundParticipationAttestation;
use bitcoin::{Amount, Network};
use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::hex::FromHex;
use bitcoin::secp256k1::{Keypair, Secp256k1};
use serde::Deserialize;
use server_rpc::{ArkServiceClient, protos};

#[derive(Deserialize)]
struct Input { vtxo: String, index: u32 }
#[derive(Deserialize)]
struct Output { index: u32, amount: u64 }
#[derive(Deserialize)]
struct Request { mnemonic: String, inputs: Vec<Input>, outputs: Vec<Output> }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let mut text = String::new();
	std::io::stdin().read_to_string(&mut text)?;
	let request: Request = serde_json::from_str(&text)?;
	let seed = bip39::Mnemonic::parse(&request.mnemonic)?.to_seed("");
	let secp = Secp256k1::new();
	let keys = Xpriv::new_master(Network::Regtest, &seed)?
		.derive_priv(&secp, &DerivationPath::from_str("m/350'/0'")?)?;
	let key = |index| -> anyhow::Result<Keypair> {
		let child = keys.derive_priv(&secp, &[ChildNumber::from_normal_idx(index)?])?;
		Ok(Keypair::from_secret_key(&secp, &child.private_key))
	};
	let outputs = request.outputs.iter().map(|o| Ok(VtxoRequest {
		amount: Amount::from_sat(o.amount), policy: VtxoPolicy::new_pubkey(key(o.index)?.public_key()),
	})).collect::<anyhow::Result<Vec<_>>>()?;
	let inputs = request.inputs.iter().map(|i| {
		let v: Vtxo = Vtxo::deserialize(&Vec::from_hex(&i.vtxo)?)?;
		let key = key(i.index)?;
		anyhow::ensure!(v.user_pubkey() == key.public_key(), "wrong test input key");
		Ok(protos::InputVtxo { vtxo_id: v.id().to_bytes().to_vec(), attestation:
			DelegatedRoundParticipationAttestation::new(v.id(), &outputs, &key).serialize() })
	}).collect::<anyhow::Result<Vec<_>>>()?;
	let mut client = ArkServiceClient::connect("http://127.0.0.1:48535").await?;
	let mut rpc_request = tonic::Request::new(protos::RoundParticipationRequest {
		input_vtxos: inputs,
		vtxo_requests: outputs.iter().map(|o| protos::VtxoRequest {
			amount: o.amount.to_sat(), policy: o.policy.serialize(),
		}).collect(),
		unblinded_mailbox_id: None, scheduled_height: None,
	});
	rpc_request.metadata_mut().insert("pver", server_rpc::MAX_PROTOCOL_VERSION.into());
	let result = client.submit_round_participation(rpc_request).await?.into_inner();
	println!("{}", bitcoin::hex::DisplayHex::as_hex(&result.unlock_hash));
	Ok(())
}
