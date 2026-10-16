//! Early Round/Pool promises use the existing Watchman fee owner. Candidate
//! authentication is not wallet ownership; the broker independently checks Core
//! and the actual rounds descriptor before recording any authorization.
use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Amount, FeeRate, OutPoint, Psbt, Transaction, TxOut, Txid};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{schnorr, Keypair, Message, PublicKey, XOnlyPublicKey};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};

const MAX_BYTES: usize = 400_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FundingPurpose { Round, Pool }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) enum FundingFamily {
	Round { round_seq: u64, pinned_input: OutPoint },
	Pool { funding_txid: Txid },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FundingCandidate {
	pub purpose: FundingPurpose,
	pub family: FundingFamily,
	pub generation: u32,
	pub unsigned: Vec<u8>,
	pub prevouts: Vec<TxOut>,
}

impl FundingCandidate {
	pub(crate) fn from_psbt(purpose: FundingPurpose, family: FundingFamily, generation: u32, psbt: &Psbt) -> anyhow::Result<Self> {
		ensure!(psbt.inputs.len() == psbt.unsigned_tx.input.len(), "funding PSBT input coverage differs");
		let mut prevouts = Vec::with_capacity(psbt.inputs.len());
		for (input, source) in psbt.unsigned_tx.input.iter().zip(&psbt.inputs) {
			let output = source.witness_utxo.clone().or_else(|| source.non_witness_utxo.as_ref()
				.filter(|parent| parent.compute_txid() == input.previous_output.txid)
				.and_then(|parent| parent.output.get(input.previous_output.vout as usize)).cloned())
				.context("funding PSBT lacks original complete prevout")?;
			prevouts.push(output);
		}
		let candidate = Self { purpose, family, generation, unsigned: serialize(&psbt.unsigned_tx), prevouts };
		ensure!(candidate.fee()? == psbt.fee()?, "funding PSBT fee differs from complete prevouts");
		Ok(candidate)
	}
	pub(crate) fn key(&self) -> sha256::Hash {
		let mut engine = sha256::Hash::engine(); engine.input(b"bark/native-funding-attempt/v1");
		engine.input(&[match self.purpose { FundingPurpose::Round => 1, FundingPurpose::Pool => 2 }]);
		engine.input(&serde_json::to_vec(&self.family).expect("typed native family serializes")); engine.input(&self.generation.to_le_bytes());
		sha256::Hash::from_engine(engine)
	}
	pub(crate) fn transaction(&self) -> anyhow::Result<Transaction> {
		ensure!(self.unsigned.len() <= MAX_BYTES && self.prevouts.len() <= 4096, "native funding candidate bound");
		let tx: Transaction = deserialize(&self.unsigned)?;
		ensure!(serialize(&tx) == self.unsigned && !tx.is_coinbase() && !tx.input.is_empty()
			&& tx.input.len() == self.prevouts.len() && tx.weight().to_wu() <= 400_000,
			"canonical complete native funding candidate required");
		let points = tx.input.iter().map(|i| i.previous_output).collect::<BTreeSet<_>>();
		ensure!(points.len() == tx.input.len(), "native funding input identity differs");
		match &self.family {
			FundingFamily::Round { pinned_input, .. } => ensure!(self.purpose == FundingPurpose::Round && points.contains(pinned_input), "Round family lacks its actual pinned input"),
			FundingFamily::Pool { funding_txid } => ensure!(self.purpose == FundingPurpose::Pool && self.generation == 0 && tx.compute_txid() == *funding_txid, "Pool funding family differs"),
		}
		ensure!(tx.input.iter().all(|i| i.script_sig.is_empty() && i.witness.is_empty())
			&& self.prevouts.iter().all(|o| o.script_pubkey.is_p2tr()), "native funding requires unsigned P2TR key spends");
		Ok(tx)
	}
	pub(crate) fn follows_unsigned_round(&self, previous: &Self) -> bool {
		matches!((&self.family,&previous.family),(FundingFamily::Round { .. },FundingFamily::Round { .. }))
			&& self.purpose == FundingPurpose::Round && previous.purpose == FundingPurpose::Round
			&& self.family == previous.family && self.generation > previous.generation
	}
	pub(crate) fn fee(&self) -> anyhow::Result<Amount> {
		let tx = self.transaction()?;
		let add = |a: Amount, b: Amount| a.checked_add(b).filter(|n| *n <= Amount::MAX_MONEY).context("native funding money overflow");
		let input = self.prevouts.iter().try_fold(Amount::ZERO, |n,o| add(n,o.value))?;
		let output = tx.output.iter().try_fold(Amount::ZERO, |n,o| add(n,o.value))?;
		let fee = input.checked_sub(output).context("native funding outputs exceed complete inputs")?;
		ensure!(fee > Amount::ZERO, "native funding has zero fee"); Ok(fee)
	}
	pub(crate) fn require_rate(&self, maximum: FeeRate) -> anyhow::Result<()> {
		let mut tx = self.transaction()?;
		for input in &mut tx.input { input.witness.push([0;64]); }
		ensure!(maximum > FeeRate::ZERO && tx.weight().to_wu() <= 400_000
			&& self.fee()? <= maximum.fee_wu(tx.weight()).context("native funding rate overflow")?, "native funding exceeds fixed signed weight/rate ceiling");
		Ok(())
	}
	pub(crate) fn verify_signed(&self, signed: &Transaction) -> anyhow::Result<()> {
		let mut unsigned = signed.clone(); for input in &mut unsigned.input { input.witness.clear(); }
		ensure!(serialize(&unsigned) == self.unsigned, "native signed funding body differs from original reservation");
		self.transaction()?;
		let mut cache = SighashCache::new(signed);
		for (index,(input,prevout)) in signed.input.iter().zip(&self.prevouts).enumerate() {
			ensure!(input.witness.len() == 1, "native funding witness differs");
			let raw = input.witness.iter().next().context("native funding signature absent")?;
			ensure!(raw.len() == 64, "native funding requires default taproot signature");
			let key = XOnlyPublicKey::from_slice(&prevout.script_pubkey.as_bytes()[2..34])?;
			let hash = cache.taproot_key_spend_signature_hash(index, &Prevouts::All(&self.prevouts), TapSighashType::Default)?;
			crate::SECP.verify_schnorr(&schnorr::Signature::from_slice(raw)?, &Message::from_digest(hash.to_byte_array()), &key)?;
		}
		Ok(())
	}
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FundingReservation {
	pub policy_sha256: sha256::Hash,
	pub index: u64,
	pub candidate: FundingCandidate,
	pub max_fee_rate: FeeRate,
}
impl FundingReservation {
	pub(crate) fn fee(&self) -> anyhow::Result<Amount> { self.candidate.require_rate(self.max_fee_rate)?; self.candidate.fee() }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FundingRequest {
	version: u8,
	server: PublicKey,
	genesis: bitcoin::BlockHash,
	policy: sha256::Hash,
	opening: sha256::Hash,
	pub candidate: FundingCandidate,
	pub signed: Option<Vec<u8>>,
	signature: schnorr::Signature,
}
impl FundingRequest {
	fn message(&self) -> anyhow::Result<Message> {
		let bytes = serde_json::to_vec(&(self.version,self.server,self.genesis,self.policy,self.opening,&self.candidate,&self.signed))?;
		let mut engine = sha256::Hash::engine(); engine.input(b"bark/native-funding-owner/v1"); engine.input(&bytes);
		Ok(Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array()))
	}
	pub(crate) fn sign(candidate: FundingCandidate, signed: Option<&Transaction>, key: &Keypair,
		genesis: bitcoin::BlockHash, policy: sha256::Hash, opening: sha256::Hash,
	) -> anyhow::Result<Vec<u8>> {
		let mut request = Self { version:1,server:key.public_key(),genesis,policy,opening,candidate,
			signed:signed.map(serialize),signature:crate::SECP.sign_schnorr(&Message::from_digest([1;32]),key) };
		request.signature = crate::SECP.sign_schnorr(&request.message()?,key);
		let raw = serde_json::to_vec(&request)?;ensure!(raw.len() <= 2*1024*1024, "native funding request bound");Ok(raw)
	}
	pub(crate) fn authenticate(raw: &[u8], bind: bool, server: PublicKey, genesis: bitcoin::BlockHash,
		policy: sha256::Hash, opening: sha256::Hash,
	) -> anyhow::Result<Self> {
		ensure!(raw.len() <= 2*1024*1024, "native funding request bound");
		let request: Self = serde_json::from_slice(raw)?;
		ensure!(serde_json::to_vec(&request)? == raw && request.version == 1 && request.signed.is_some() == bind
			&& request.server == server && request.genesis == genesis && request.policy == policy && request.opening == opening,
			"native funding request method/server/chain/policy/opening differs");
		crate::SECP.verify_schnorr(&request.signature, &request.message()?, &server.x_only_public_key().0)?;
		request.candidate.fee()?;Ok(request)
	}
}

#[cfg(test)]
pub(crate) fn test_candidate(generation: u32, fee: u64) -> (FundingCandidate, Transaction) {
	use bitcoin::key::TapTweak;
	use ark::test_util::dummy::DUMMY_SERVER_KEY;
	let key=*DUMMY_SERVER_KEY;
	let point=OutPoint::new(Txid::from_byte_array([9;32]),0);
	let prevout=TxOut { value:Amount::from_sat(10_000),script_pubkey:bitcoin::ScriptBuf::new_p2tr(&crate::SECP,key.x_only_public_key().0,None) };
	let unsigned=Transaction { version:bitcoin::transaction::Version::TWO,lock_time:bitcoin::absolute::LockTime::ZERO,
		input:vec![bitcoin::TxIn { previous_output:point,script_sig:bitcoin::ScriptBuf::new(),sequence:bitcoin::Sequence::MAX,witness:bitcoin::Witness::new() }],
		output:vec![TxOut { value:Amount::from_sat(10_000-fee),script_pubkey:prevout.script_pubkey.clone() }] };
	let candidate=FundingCandidate { purpose:FundingPurpose::Round,family:FundingFamily::Round { round_seq:123,pinned_input:point },generation,unsigned:serialize(&unsigned),prevouts:vec![prevout] };
	let mut signed=unsigned;let hash=SighashCache::new(&signed).taproot_key_spend_signature_hash(0,&Prevouts::All(&candidate.prevouts),TapSighashType::Default).unwrap();
	let tweaked=key.tap_tweak(&crate::SECP,None).to_keypair();let sig=crate::SECP.sign_schnorr(&Message::from_digest(hash.to_byte_array()),&tweaked);signed.input[0].witness.push(sig.as_ref());(candidate,signed)
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn early_body_and_complete_prevouts_fix_fee_before_any_signature() {
		let (candidate,signed)=test_candidate(0,500);assert_eq!(candidate.fee().unwrap().to_sat(),500);candidate.verify_signed(&signed).unwrap();
		let mut changed=candidate.clone();changed.prevouts[0].value=Amount::from_sat(10_001);assert!(changed.verify_signed(&signed).is_err());
		let mut changed=signed;changed.output[0].value=Amount::from_sat(9_499);assert!(candidate.verify_signed(&changed).is_err());
		let mut incomplete=candidate;incomplete.prevouts.clear();assert!(incomplete.fee().is_err());
	}
	#[test]
	fn real_round_family_allows_only_strictly_later_unsigned_attempt() {
		let (old,_)=test_candidate(0,500);let (next,_)=test_candidate(1,600);assert!(next.follows_unsigned_round(&old));assert!(!old.follows_unsigned_round(&next));
		let mut foreign=next.clone();foreign.family=FundingFamily::Round { round_seq:124,pinned_input:old.transaction().unwrap().input[0].previous_output };assert!(!foreign.follows_unsigned_round(&old));
		let mut changed=next;changed.family=FundingFamily::Round { round_seq:123,pinned_input:OutPoint::null() };assert!(changed.transaction().is_err());
	}
	#[test]
	fn authenticated_owner_request_binds_method_family_prevouts_and_opening() {
		use ark::test_util::dummy::DUMMY_SERVER_KEY;
		let (candidate,_)=test_candidate(0,500);let key=*DUMMY_SERVER_KEY;let genesis=bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash();let hash=sha256::Hash::hash(b"testonly existing policy");
		let raw=FundingRequest::sign(candidate,None,&key,genesis,hash,hash).unwrap();FundingRequest::authenticate(&raw,false,key.public_key(),genesis,hash,hash).unwrap();
		assert!(FundingRequest::authenticate(&raw,true,key.public_key(),genesis,hash,hash).is_err());assert!(FundingRequest::authenticate(&raw,false,key.public_key(),genesis,hash,sha256::Hash::hash(b"different opening")).is_err());
		let mut changed: FundingRequest=serde_json::from_slice(&raw).unwrap();changed.candidate.generation+=1;assert!(FundingRequest::authenticate(&serde_json::to_vec(&changed).unwrap(),false,key.public_key(),genesis,hash,hash).is_err());
	}
}

#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct FundingReply { reservation:FundingReservation, signature:schnorr::Signature }
impl FundingReply {
	fn message(reservation:&FundingReservation,server:PublicKey,genesis:bitcoin::BlockHash)->anyhow::Result<Message> {
		let mut engine=sha256::Hash::engine();engine.input(b"bark/native-funding-reply/v1");engine.input(&serde_json::to_vec(&(server,genesis,reservation))?);Ok(Message::from_digest(sha256::Hash::from_engine(engine).to_byte_array()))
	}
}
pub(crate) fn sign_reply(reservation:FundingReservation,key:&Keypair,genesis:bitcoin::BlockHash)->anyhow::Result<Vec<u8>> {
	let signature=crate::SECP.sign_schnorr(&FundingReply::message(&reservation,key.public_key(),genesis)?,key);
	Ok(serde_json::to_vec(&FundingReply {reservation,signature})?)
}
pub(crate) fn verify_reply(raw:&[u8],candidate:&FundingCandidate,server:PublicKey,genesis:bitcoin::BlockHash,policy:sha256::Hash)->anyhow::Result<FundingReservation> {
	ensure!(raw.len()<=2*1024*1024,"native owner response bound");let reply:FundingReply=serde_json::from_slice(raw)?;
	ensure!(serde_json::to_vec(&reply)?==raw && reply.reservation.candidate==*candidate && reply.reservation.policy_sha256==policy,"native owner response candidate/policy differs");
	reply.reservation.fee()?;crate::SECP.verify_schnorr(&reply.signature,&FundingReply::message(&reply.reservation,server,genesis)?,&server.x_only_public_key().0)?;Ok(reply.reservation)
}

impl crate::Server {
	pub(crate) async fn native_funding_owner(&self,candidate:FundingCandidate,signed:Option<&Transaction>)->anyhow::Result<Option<FundingReservation>> {
		if !self.config.direct_expiry.enabled {return Ok(None);}
		let address=self.config.native_funding_owner_address.context("direct Round/Pool requires explicit owning Watchman fee endpoint")?;
		let (policy,opening)=super::ordinary_fee::installed_identity(&self.config.data_dir)?;
		let genesis=bitcoin::constants::genesis_block(self.config.network).block_hash();
		let raw=FundingRequest::sign(candidate.clone(),signed,self.server_key.leak_ref(),genesis,policy,opening)?;
		let maximum=self.config.fee_estimator.max_fee_rate.context("native funding needs explicit actual rate ceiling")?;
		candidate.require_rate(maximum)?;
		let endpoint=tonic::transport::Endpoint::from_shared(format!("http://{address}"))?
			.connect_timeout(std::time::Duration::from_secs(5)).timeout(std::time::Duration::from_secs(30));
		let mut client=server_rpc::admin::NativeFundingAdminServiceClient::new(endpoint.connect().await?);
		let request=server_rpc::protos::NativeFundingOwnerRequest {authenticated_request:raw};
		// A lost reply is uncertainty, never permission to retry a different body.
		let response=tokio::time::timeout(std::time::Duration::from_secs(30),async {
			if signed.is_some(){client.bind_native_funding(request).await}else{client.reserve_native_funding(request).await}
		}).await.context("native funding owner reply unknown; retain original candidate")??.into_inner();
		let reservation=verify_reply(&response.authenticated_response,&candidate,self.server_pubkey,genesis,policy)?;
		ensure!(reservation.max_fee_rate<=maximum,"native owner rate ceiling exceeds caller configuration");
		Ok(Some(reservation))
	}
}

#[cfg(test)]
mod reply_tests {
	use super::*;
	#[test]
	fn early_promise_requires_authentic_owner_reply_for_exact_attempt() {
		use ark::test_util::dummy::{DUMMY_SERVER_KEY,DUMMY_USER_KEY};
		let(candidate,_)=test_candidate(0,500);let key=*DUMMY_SERVER_KEY;let genesis=bitcoin::constants::genesis_block(bitcoin::Network::Regtest).block_hash();
		let policy=sha256::Hash::hash(b"existing actual policy fixture");
		let reservation=FundingReservation {policy_sha256:policy,index:7,candidate:candidate.clone(),max_fee_rate:FeeRate::from_sat_per_vb(10).unwrap()};
		let raw=sign_reply(reservation.clone(),&key,genesis).unwrap();assert_eq!(verify_reply(&raw,&candidate,key.public_key(),genesis,policy).unwrap(),reservation);
		assert!(verify_reply(&raw,&candidate,DUMMY_USER_KEY.public_key(),genesis,policy).is_err());
		let(mut different,_)=test_candidate(1,500);assert!(verify_reply(&raw,&different,key.public_key(),genesis,policy).is_err());
		different=candidate.clone();different.prevouts[0].value=Amount::from_sat(10001);assert!(verify_reply(&raw,&different,key.public_key(),genesis,policy).is_err());
		let mut reply:FundingReply=serde_json::from_slice(&raw).unwrap();reply.reservation.index+=1;assert!(verify_reply(&serde_json::to_vec(&reply).unwrap(),&candidate,key.public_key(),genesis,policy).is_err());
	}
}
