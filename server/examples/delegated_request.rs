//! Regtest fixture: submit exact replacement requests with genuine input attestations.
//! Generated test mnemonic and serialized inputs arrive on stdin, never argv.
use std::io::Read;
use std::str::FromStr;

use ark::{ProtocolEncoding, Vtxo, VtxoPolicy, VtxoRequest};
use ark::attestations::DelegatedRoundParticipationAttestation;
use ark::forfeit::HashLockedForfeitBundle;
use ark::tree::signed::UnlockHash;
use bitcoin::{Amount, Network};
use bitcoin::hashes::Hash;
use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::hex::FromHex;
use bitcoin::secp256k1::{Keypair, Secp256k1};
use serde::Deserialize;
use server_rpc::{ArkServiceClient, TryFromBytes, protos};

#[derive(Deserialize)]
struct Input { vtxo: String, index: u32 }
#[derive(Deserialize)]
struct Output { index: u32, amount: u64, pubkey: Option<bitcoin::secp256k1::PublicKey> }
#[derive(Deserialize)]
struct Request {
	mnemonic: String, inputs: Vec<Input>, outputs: Vec<Output>,
	forfeit_unlock_hash: Option<String>,
}

fn rpc<T>(message: T) -> tonic::Request<T> {
	let mut request = tonic::Request::new(message);
	request.metadata_mut().insert("pver", server_rpc::MAX_PROTOCOL_VERSION.into());
	request
}

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
	let mut client = ArkServiceClient::connect("http://127.0.0.1:48535").await?;
	if let Some(hash) = request.forfeit_unlock_hash {
		let hash = UnlockHash::from_str(&hash)?;
		let inputs = request.inputs.iter().map(|i| {
			let vtxo: Vtxo = Vtxo::deserialize(&Vec::from_hex(&i.vtxo)?)?;
			let key = key(i.index)?;
			anyhow::ensure!(vtxo.user_pubkey() == key.public_key(), "wrong forfeit test key");
			Ok((vtxo, key))
		}).collect::<anyhow::Result<Vec<_>>>()?;
		let nonces = client.request_forfeit_nonces(rpc(protos::ForfeitNoncesRequest {
			unlock_hash: hash.to_byte_array().to_vec(),
			vtxo_ids: inputs.iter().map(|(v, _)| v.id().to_bytes().to_vec()).collect(),
		})).await?.into_inner().public_nonces;
		anyhow::ensure!(nonces.len() == inputs.len(), "wrong forfeit nonce count");
		let bundles = inputs.iter().zip(nonces).map(|((v, key), nonce)| {
			Ok(HashLockedForfeitBundle::new(v, hash, key,
				&ark::musig::PublicNonce::from_bytes(nonce)?).serialize())
		}).collect::<anyhow::Result<Vec<_>>>()?;
		client.forfeit_vtxos(rpc(protos::ForfeitVtxosRequest { forfeit_bundles: bundles })).await?;
		println!("forfeit accepted");
		return Ok(());
	}
	let outputs = request.outputs.iter().map(|o| Ok(VtxoRequest {
		amount: Amount::from_sat(o.amount),
		policy: VtxoPolicy::new_pubkey(o.pubkey.unwrap_or(key(o.index)?.public_key())),
	})).collect::<anyhow::Result<Vec<_>>>()?;
	let inputs = request.inputs.iter().map(|i| {
		let v: Vtxo = Vtxo::deserialize(&Vec::from_hex(&i.vtxo)?)?;
		let key = key(i.index)?;
		anyhow::ensure!(v.user_pubkey() == key.public_key(), "wrong test input key");
		Ok(protos::InputVtxo { vtxo_id: v.id().to_bytes().to_vec(), attestation:
			DelegatedRoundParticipationAttestation::new(v.id(), &outputs, &key).serialize() })
	}).collect::<anyhow::Result<Vec<_>>>()?;
	let result = client.submit_round_participation(rpc(protos::RoundParticipationRequest {
		input_vtxos: inputs,
		vtxo_requests: outputs.iter().map(|o| protos::VtxoRequest {
			amount: o.amount.to_sat(), policy: o.policy.serialize(),
		}).collect(),
		unblinded_mailbox_id: None, scheduled_height: None,
	})).await?.into_inner();
	println!("{}", bitcoin::hex::DisplayHex::as_hex(&result.unlock_hash));
	Ok(())
}
