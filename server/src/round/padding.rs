//! Creation-time facts for leaves which this round appends after all requests.
//! A public leaf policy or wallet funding alone is not an ownership proof.

use std::collections::BTreeSet;

use anyhow::Context;
use bitcoin::{Amount, OutPoint, Transaction, TxOut};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::secp256k1::XOnlyPublicKey;
use serde::{Deserialize, Serialize};

use ark::{ProtocolEncoding, VtxoId};
use ark::tree::signed::{CachedSignedVtxoTree, VtxoLeafSpec};

pub(crate) const MAX_ISSUANCE_BYTES: usize = 4_194_304;

/// Public outputs selected by the rounds wallet while its input guards are held.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FundingPrevout {
	pub outpoint: OutPoint,
	pub txout: TxOut,
}

/// This value exists only between accepted requests and the first round commit.
/// Keep full leaf specifications: policy and amount omit unlock and co-sign data.
pub(super) struct PaddingDraft {
	requested: Vec<VtxoLeafSpec>,
	appended: Vec<VtxoLeafSpec>,
	inputs: Vec<(VtxoId, Amount)>,
	refresh_surplus: Amount,
}

impl PaddingDraft {
	pub fn capture(
		requested: Vec<VtxoLeafSpec>,
		mut inputs: Vec<(VtxoId, Amount)>,
		admitted_inputs: Vec<VtxoId>,
		refresh_surplus: Amount,
	) -> anyhow::Result<Self> {
		inputs.sort_by_key(|(id, _)| *id);
		let unique = inputs.iter().map(|(id, _)| *id).collect::<BTreeSet<_>>();
		let admitted = admitted_inputs.iter().copied().collect::<BTreeSet<_>>();
		ensure!(!inputs.is_empty() && !requested.is_empty(), "empty padding issuance request cut");
		ensure!(unique.len() == inputs.len() && admitted.len() == admitted_inputs.len()
			&& unique == admitted, "padding issuance has an incomplete or repeated input role");
		let input_value = sum(inputs.iter().map(|(_, amount)| *amount))?;
		let request_value = sum(requested.iter().map(|leaf| leaf.vtxo.amount))?;
		ensure!(request_value.checked_add(refresh_surplus) == Some(input_value),
			"padding issuance differs from admitted request principal and surplus");
		Ok(Self { requested, appended: Vec::new(), inputs, refresh_surplus })
	}

	pub fn appended(&mut self, leaf: &VtxoLeafSpec) {
		self.appended.push(leaf.clone());
	}

	pub fn finish(
		&self,
		tree: &CachedSignedVtxoTree,
		funding: &Transaction,
		prevouts: &[FundingPrevout],
		funding_fee: Amount,
	) -> anyhow::Result<Option<PaddingIssuance>> {
		let leaves = &tree.spec.spec.vtxos;
		ensure!(leaves.len() == self.requested.len() + self.appended.len()
			&& leaves[..self.requested.len()] == self.requested
			&& leaves[self.requested.len()..] == self.appended,
			"padding issuance differs from the exact accepted prefix and appended suffix");
		ensure!(self.appended.len() == 2usize.saturating_sub(self.requested.len()),
			"padding issuance has an unexpected appended role count");
		if self.appended.is_empty() { return Ok(None); }
		let value = PaddingIssuance {
			version: 1,
			tree_sha256: sha256::Hash::hash(&tree.spec.serialize()),
			requested_leaf_count: self.requested.len(),
			input_vtxos: self.inputs.iter().map(|(id, _)| *id).collect(),
			input_value_sat: sum(self.inputs.iter().map(|(_, amount)| *amount))?.to_sat(),
			refresh_surplus_sat: self.refresh_surplus.to_sat(),
			funding_prevouts: prevouts.to_vec(),
			funding_fee_sat: funding_fee.to_sat(),
		};
		value.validate(tree, funding)?;
		ensure!(serde_json::to_vec(&value)?.len() <= MAX_ISSUANCE_BYTES,
			"padding issuance exceeds its storage limit");
		Ok(Some(value))
	}
}

/// Write once with the round and its complete tree. Old rows remain NULL.
/// The constructor is private to the successful round path, not an RPC setter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaddingIssuance {
	version: u32,
	tree_sha256: sha256::Hash,
	requested_leaf_count: usize,
	input_vtxos: Vec<VtxoId>,
	input_value_sat: u64,
	refresh_surplus_sat: u64,
	funding_prevouts: Vec<FundingPrevout>,
	funding_fee_sat: u64,
}

impl PaddingIssuance {
	pub fn validate_inputs(&self, inputs: impl IntoIterator<Item = (VtxoId, Amount)>) -> anyhow::Result<()> {
		let mut inputs = inputs.into_iter().collect::<Vec<_>>();
		inputs.sort_by_key(|(id, _)| *id);
		ensure!(inputs.iter().map(|(id, _)| *id).collect::<Vec<_>>() == self.input_vtxos
			&& sum(inputs.iter().map(|(_, amount)| *amount))?.to_sat() == self.input_value_sat,
			"padding issuance input cut changed before round commit");
		Ok(())
	}

	pub fn validate(&self, tree: &CachedSignedVtxoTree, funding: &Transaction) -> anyhow::Result<()> {
		ensure!(self.version == 1 && self.requested_leaf_count == 1
			&& tree.spec.spec.vtxos.len() == 2, "unsupported padding issuance roles or version");
		ensure!(self.tree_sha256 == sha256::Hash::hash(&tree.spec.serialize()),
			"padding issuance signed tree changed");
		ensure!(tree.spec.utxo.txid == funding.compute_txid()
			&& tree.spec.utxo.vout == ark::rounds::ROUND_TX_VTXO_TREE_VOUT,
			"padding issuance funding identity changed");
		let output = funding.output.get(tree.spec.utxo.vout as usize)
			.context("padding issuance funding output is absent")?;
		ensure!(output.script_pubkey == tree.spec.spec.funding_tx_script_pubkey()
			&& output.value == sum(tree.spec.spec.vtxos.iter().map(|leaf| leaf.vtxo.amount))?,
			"padding issuance tree funding differs");
		ensure!(!self.input_vtxos.is_empty()
			&& self.input_vtxos.windows(2).all(|ids| ids[0] < ids[1]),
			"padding issuance input roles are not unique and ordered");
		let requested = sum(tree.spec.spec.vtxos[..self.requested_leaf_count].iter().map(|leaf| leaf.vtxo.amount))?;
		ensure!(requested.to_sat().checked_add(self.refresh_surplus_sat) == Some(self.input_value_sat),
			"padding issuance request principal differs");
		ensure!(!funding.input.is_empty() && funding.input.len() == self.funding_prevouts.len(),
			"padding issuance lacks actual funding prevouts");
		let mut unique = BTreeSet::new();
		for (input, prevout) in funding.input.iter().zip(&self.funding_prevouts) {
			ensure!(input.previous_output == prevout.outpoint && unique.insert(prevout.outpoint),
				"padding issuance funding input differs or repeats");
		}
		let paid = sum(self.funding_prevouts.iter().map(|prevout| prevout.txout.value))?;
		let outputs = sum(funding.output.iter().map(|output| output.value))?;
		ensure!(outputs.to_sat().checked_add(self.funding_fee_sat) == Some(paid.to_sat()),
			"padding issuance funding principal, change and miner fee differ");
		let leaves = tree.output_vtxos().map(|leaf| leaf.id()).collect::<BTreeSet<_>>();
		ensure!(leaves.len() == tree.spec.spec.vtxos.len(), "padding issuance repeats an origin leaf");
		Ok(())
	}

	/// The rounds wallet uses P2TR key spends. Verify the actual signatures
	/// against every retained prevout; a changed prevout cannot reuse them.
	pub fn validate_signed_funding(&self, funding: &Transaction) -> anyhow::Result<()> {
		ensure!(!funding.input.is_empty() && funding.input.len() == self.funding_prevouts.len(),
			"padding signed funding lacks its complete prevout cut");
		let prevouts = self.funding_prevouts.iter().map(|prevout| &prevout.txout).collect::<Vec<_>>();
		let mut cache = SighashCache::new(funding);
		for (index, input) in funding.input.iter().enumerate() {
			let prevout = prevouts.get(index).context("padding signed funding prevout is absent")?;
			ensure!(input.witness.len() == 1 && prevout.script_pubkey.is_p2tr(),
				"unsupported padding funding witness or script");
			let signature = bitcoin::taproot::Signature::from_slice(input.witness.iter().next().unwrap())?;
			ensure!(matches!(signature.sighash_type, bitcoin::TapSighashType::Default | bitcoin::TapSighashType::All),
				"padding funding signature does not commit the complete funding cut");
			let sighash = cache.taproot_key_spend_signature_hash(index, &Prevouts::All(&prevouts), signature.sighash_type)?;
			let key = XOnlyPublicKey::from_slice(&prevout.script_pubkey.as_bytes()[2..])?;
			crate::SECP.verify_schnorr(&signature.signature, &sighash.into(), &key)?;
		}
		Ok(())
	}

	pub fn appended_leaf(&self, tree: &CachedSignedVtxoTree, id: VtxoId) -> Option<u32> {
		tree.output_vtxos().enumerate().skip(self.requested_leaf_count)
			.find(|(_, leaf)| leaf.id() == id).and_then(|(index, _)| u32::try_from(index).ok())
	}
}

fn sum(values: impl IntoIterator<Item = Amount>) -> anyhow::Result<Amount> {
	values.into_iter().try_fold(Amount::ZERO, |sum, value|
		sum.checked_add(value).context("padding issuance amount overflow"))
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use bitcoin::{ScriptBuf, Sequence, TxIn, Witness};
	use bitcoin::key::TapTweak;
	use bitcoin::secp256k1::{Keypair, SecretKey};
	use bitcoin_ext::{BlockDelta, BlockHeight};

	use ark::{musig, VtxoPolicy, VtxoRequest};
	use ark::tree::signed::VtxoTreeSpec;

	use super::*;

	fn key(byte: u8) -> Keypair {
		Keypair::from_secret_key(&crate::SECP, &SecretKey::from_slice(&[byte; 32]).unwrap())
	}

	fn public_tuple() -> VtxoLeafSpec {
		VtxoLeafSpec {
			vtxo: VtxoRequest {
				policy: VtxoPolicy::new_pubkey("02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c".parse().unwrap()),
				amount: Amount::from_sat(330),
			},
			cosign_pubkey: None,
			unlock_hash: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".parse().unwrap(),
		}
	}

	fn input_id() -> VtxoId { OutPoint::null().into() }

	/// A pure signed-tree fixture. It is not a round API or a funding execution.
	fn tree_fixture(leaves: Vec<VtxoLeafSpec>) -> (CachedSignedVtxoTree, Transaction, Vec<FundingPrevout>) {
		let cosigner = key(2);
		let funding_key = key(3).tap_tweak(&crate::SECP, None).to_keypair();
		let spec = VtxoTreeSpec::new(leaves, key(1).public_key(), BlockHeight::new(1000),
			BlockDelta::new(12), vec![cosigner.public_key()]);
		let prevout = FundingPrevout {
			outpoint: OutPoint::new(bitcoin::Txid::from_byte_array([7; 32]), 0),
			txout: TxOut { value: spec.total_required_value() + Amount::from_sat(1000),
				script_pubkey: ScriptBuf::new_p2tr_tweaked(key(3).tap_tweak(&crate::SECP, None).public_parts().0) },
		};
		let mut funding = Transaction {
			version: bitcoin::transaction::Version::TWO,
			lock_time: bitcoin::absolute::LockTime::ZERO,
			input: vec![TxIn { previous_output: prevout.outpoint, script_sig: ScriptBuf::new(),
				sequence: Sequence::ZERO, witness: Witness::new() }],
			output: vec![TxOut { value: spec.total_required_value(), script_pubkey: spec.funding_tx_script_pubkey() },
				TxOut { value: Amount::from_sat(900), script_pubkey: prevout.txout.script_pubkey.clone() }],
		};
		let unsigned = spec.into_unsigned_tree(OutPoint::new(funding.compute_txid(), 0));
		let (secs, pubs): (Vec<_>, Vec<_>) = (0..unsigned.spec.nb_internal_nodes())
			.map(|_| musig::nonce_pair(&cosigner)).unzip();
		let agg = unsigned.spec.calculate_cosign_agg_nonces(&HashMap::new(), &[&pubs]).unwrap();
		let partials = unsigned.cosign_tree(&agg, &cosigner, secs);
		let signatures = unsigned.combine_partial_signatures(&agg, &HashMap::new(), &[&partials]).unwrap();
		let tree = unsigned.into_signed_tree(signatures).into_cached_tree();
		let sighash = SighashCache::new(&funding).taproot_key_spend_signature_hash(
			0, &Prevouts::All(&[&prevout.txout]), bitcoin::TapSighashType::Default,
		).unwrap();
		let signature = crate::SECP.sign_schnorr_no_aux_rand(&sighash.into(), &funding_key);
		funding.input[0].witness.push(signature.as_ref());
		(tree, funding, vec![prevout])
	}

	#[test]
	fn padding_issuance_uses_creation_roles_even_for_identical_public_leaves() {
		let leaf = public_tuple();
		let mut draft = PaddingDraft::capture(vec![leaf.clone()], vec![(input_id(), leaf.vtxo.amount)],
			vec![input_id()], Amount::ZERO).unwrap();
		draft.appended(&leaf);
		let (tree, funding, prevouts) = tree_fixture(vec![leaf.clone(), leaf]);
		let issuance = draft.finish(&tree, &funding, &prevouts, Amount::from_sat(100)).unwrap().unwrap();
		issuance.validate_signed_funding(&funding).unwrap();
		let leaves = tree.output_vtxos().collect::<Vec<_>>();
		assert_ne!(leaves[0].id(), leaves[1].id());
		assert_eq!(issuance.appended_leaf(&tree, leaves[0].id()), None,
			"an accepted customer prefix leaf never becomes padding from its tuple");
		assert_eq!(issuance.appended_leaf(&tree, leaves[1].id()), Some(1));
		let encoded = serde_json::to_string(&issuance).unwrap();
		let decoded: PaddingIssuance = serde_json::from_str(&encoded).unwrap();
		decoded.validate(&tree, &funding).unwrap();
		decoded.validate_signed_funding(&funding).unwrap();
	}

	#[test]
	fn padding_issuance_rejects_incomplete_or_repeated_accepted_input_roles() {
		let request = public_tuple();
		for admitted in [vec![], vec![input_id(), input_id()]] {
			assert!(PaddingDraft::capture(vec![request.clone()], vec![(input_id(), request.vtxo.amount)],
				admitted, Amount::ZERO).is_err());
		}
		assert!(PaddingDraft::capture(vec![], vec![(input_id(), Amount::from_sat(330))],
			vec![input_id()], Amount::ZERO).is_err());
		assert!(PaddingDraft::capture(vec![request], vec![(input_id(), Amount::from_sat(331))],
			vec![input_id()], Amount::ZERO).is_err());
	}

	#[test]
	fn padding_issuance_rejects_reordered_prefix_and_nonminimal_append() {
		let request = public_tuple();
		let mut appended = request.clone();
		appended.vtxo.amount = Amount::from_sat(1000);
		let mut draft = PaddingDraft::capture(vec![request.clone()], vec![(input_id(), request.vtxo.amount)],
			vec![input_id()], Amount::ZERO).unwrap();
		draft.appended(&appended);
		let (tree, funding, prevouts) = tree_fixture(vec![appended.clone(), request.clone()]);
		assert!(draft.finish(&tree, &funding, &prevouts, Amount::from_sat(100)).is_err());
		draft.appended(&appended);
		let (tree, funding, prevouts) = tree_fixture(vec![request, appended.clone(), appended]);
		assert!(draft.finish(&tree, &funding, &prevouts, Amount::from_sat(100)).is_err());
	}

	#[test]
	fn padding_issuance_omits_metadata_when_all_leaves_are_requested() {
		for count in [2, 7] {
			let requested = vec![public_tuple(); count];
			let value = sum(requested.iter().map(|leaf| leaf.vtxo.amount)).unwrap();
			let draft = PaddingDraft::capture(requested.clone(), vec![(input_id(), value)],
				vec![input_id()], Amount::ZERO).unwrap();
			let (tree, funding, prevouts) = tree_fixture(requested);
			assert!(draft.finish(&tree, &funding, &prevouts, Amount::from_sat(100)).unwrap().is_none());
		}
	}

	#[test]
	fn padding_issuance_binds_full_funding_cut_and_signed_tree() {
		let leaf = public_tuple();
		let mut draft = PaddingDraft::capture(vec![leaf.clone()], vec![(input_id(), Amount::from_sat(430))],
			vec![input_id()], Amount::from_sat(100)).unwrap();
		draft.appended(&leaf);
		let (tree, funding, prevouts) = tree_fixture(vec![leaf.clone(), leaf]);
		let issuance = draft.finish(&tree, &funding, &prevouts, Amount::from_sat(100)).unwrap().unwrap();
		let mutations: [fn(&mut PaddingIssuance); 8] = [
			|value: &mut PaddingIssuance| value.version = 2,
			|value: &mut PaddingIssuance| value.requested_leaf_count = 0,
			|value: &mut PaddingIssuance| value.tree_sha256 = sha256::Hash::all_zeros(),
			|value: &mut PaddingIssuance| value.input_vtxos.push(input_id()),
			|value: &mut PaddingIssuance| value.input_value_sat += 1,
			|value: &mut PaddingIssuance| value.refresh_surplus_sat += 1,
			|value: &mut PaddingIssuance| value.funding_prevouts.clear(),
			|value: &mut PaddingIssuance| value.funding_fee_sat += 1,
		];
		for mutate in mutations {
			let mut changed = issuance.clone(); mutate(&mut changed);
			assert!(changed.validate(&tree, &funding).is_err());
		}
		let mut wrong_script = issuance.clone();
		wrong_script.funding_prevouts[0].txout.script_pubkey = ScriptBuf::new_p2tr(
			&crate::SECP, key(4).x_only_public_key().0, None);
		assert!(wrong_script.validate_signed_funding(&funding).is_err());
		let mut wrong_value = issuance.clone();
		wrong_value.funding_prevouts[0].txout.value += Amount::from_sat(1);
		wrong_value.funding_fee_sat += 1;
		assert!(wrong_value.validate_signed_funding(&funding).is_err());
		assert!(issuance.validate_inputs([(input_id(), Amount::from_sat(431))]).is_err());
	}
}
