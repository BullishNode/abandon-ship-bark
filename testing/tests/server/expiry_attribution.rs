use std::sync::Arc;
use std::time::Duration;

use ark::arkoor::{ArkoorDestination, package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse}};
use ark::fallback::FallbackRecord;
use ark::lightning::PaymentHash;
use ark::mailbox::{MailboxAuthorization, MailboxIdentifier};
use ark::vtxo::Full;
use ark::{ProtocolEncoding, Vtxo, VtxoPolicy, SECP};
use ark_testing::{sat, Captaind, TestContext};
use ark_testing::constants::BOARD_CONFIRMATIONS;
use bitcoin::bip32::{ChildNumber, Xpriv};
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::secp256k1::{Keypair, PublicKey};
use bitcoin::{Amount, Network, OutPoint, Transaction, TxOut};
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use server::database::Db;
use server::database::expiry_attribution::Attribution;
use server_rpc::protos;

pub(super) struct Owner {
	pub(super) account: Xpriv,
	pub(super) mailbox: Keypair,
	pub(super) record: FallbackRecord,
}

impl Owner {
	pub(super) fn new(ctx: &TestContext) -> Self {
		Self::with_seed(ctx, 117)
	}

	pub(super) fn with_seed(ctx: &TestContext, seed: u8) -> Self {
		let account = Xpriv::new_master(Network::Regtest, &[seed; 64]).unwrap();
		Self::from_account(ctx, account)
	}

	pub(super) fn random(ctx: &TestContext) -> Self {
		let account = Xpriv::new_master(Network::Regtest, &rand::random::<[u8; 32]>()).unwrap();
		Self::from_account(ctx, account)
	}

	fn from_account(ctx: &TestContext, account: Xpriv) -> Self {
		let mailbox = account.to_keypair(&SECP);
		let record = FallbackRecord::new(Network::Regtest,
			ctx.bitcoind().get_new_address().script_pubkey(), &account, &mailbox).unwrap();
		Self { account, mailbox, record }
	}

	pub(super) fn key(&self, index: u32) -> Keypair {
		self.account.derive_priv(&SECP, &[ChildNumber::from_normal_idx(index).unwrap()]).unwrap().to_keypair(&SECP)
	}

	pub(super) fn proof(&self, index: u32, mailbox_pubkey: PublicKey) -> protos::OwnerProof {
		let blinded = MailboxIdentifier::from_pubkey(self.mailbox.public_key())
			.to_blinded(mailbox_pubkey, &self.key(index)).unwrap();
		protos::OwnerProof { blinded_mailbox_id: blinded.as_ref().to_vec(), child_index: index }
	}

	pub(super) fn prove_destinations(&self, request: &mut protos::ArkoorPackageCosignRequest,
		index: u32, mailbox_pubkey: PublicKey,
	) {
		let key = self.key(index).public_key();
		for part in &mut request.parts {
			for output in part.outputs.iter_mut().chain(&mut part.isolated_outputs) {
				let policy = VtxoPolicy::deserialize(&output.policy).unwrap();
				assert_eq!(policy.user_pubkey(), key, "fixture proof must match the actual output");
				output.owner_proof = Some(self.proof(index, mailbox_pubkey));
			}
		}
	}

	pub(super) async fn register(&self, srv: &Captaind) {
		let result = srv.get_public_rpc().await.set_fallback(protos::SetFallbackRequest {
			mailbox_pk: self.mailbox.public_key().serialize().to_vec(), record: self.record.serialize(),
		}).await.unwrap().into_inner();
		assert_eq!(result.record, self.record.serialize());
	}

	async fn post_mailbox(&self, srv: &Captaind, coin: &Vtxo<Full>) {
		let authorization = MailboxAuthorization::new(&self.mailbox,
			chrono::Local::now() + Duration::from_secs(60));
		srv.get_mailbox_public_rpc().await.post_recovery_vtxo_ids(protos::mailbox_server::PostRecoveryVtxoIdsRequest {
			mailbox_id: MailboxIdentifier::from_pubkey(self.mailbox.public_key()).serialize(),
			vtxo_ids: vec![coin.id().to_bytes().to_vec()], authorization: Some(authorization.serialize()),
		}).await.unwrap();
	}
}

pub(super) async fn server(ctx: &TestContext) -> Arc<Captaind> {
	server_with_lifetime(ctx, BlockDelta::new(24)).await
}

async fn server_with_lifetime(ctx: &TestContext, lifetime: BlockDelta) -> Arc<Captaind> {
	let srv = ctx.captaind("server").no_vtxo_pool()
		.cfg(move |cfg| cfg.vtxo_lifetime = lifetime).watchmand().create().await;
	ctx.bitcoind().fund_addr(srv.watchmand().wait_wallet_address().await, sat(100_000)).await;
	ctx.generate_blocks(2).await;
	srv
}

pub(super) struct PreparedBoard {
	pub(super) builder: ark::board::BoardBuilder<ark::board::state::CanFinish>,
	pub(super) funding: Transaction,
	pub(super) request: protos::BoardCosignRequest,
}

pub(super) async fn prepare_board(ctx: &TestContext, srv: &Captaind, owner: &Owner,
	child_index: u32,
) -> PreparedBoard {
	let key = owner.key(child_index);
	let core = ctx.bitcoind().sync_client();
	let address = ctx.bitcoind().get_new_address();
	let funding_id = ctx.bitcoind().fund_addr(&address, sat(200_000)).await;
	let funding = ctx.bitcoind().await_transaction(funding_id).await;
	let index = funding.output.iter().position(|output| output.script_pubkey == address.script_pubkey()).unwrap();
	assert_eq!(funding.output[index].value, sat(200_000));
	let tip = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let info = srv.ark_info().await;
	let expiry = tip + info.vtxo_lifetime;
	assert_eq!(info.fees.board.calculate(sat(100_000)).unwrap(), Amount::ZERO);
	let builder = ark::board::BoardBuilder::new(key.public_key(), expiry, info.server_pubkey, info.vtxo_exit_delta);
	let unsigned = Transaction {
		version: bitcoin::transaction::Version::TWO, lock_time: bitcoin::absolute::LockTime::ZERO,
		input: vec![bitcoin::TxIn { previous_output: OutPoint::new(funding_id, index as u32),
			script_sig: Default::default(), sequence: bitcoin::Sequence::MAX, witness: bitcoin::Witness::new() }],
		output: vec![TxOut { value: sat(100_000), script_pubkey: builder.funding_script_pubkey() },
			TxOut { value: sat(99_000), script_pubkey: ctx.bitcoind().get_new_address().script_pubkey() }],
	};
	let signed = core.sign_raw_transaction_with_wallet(&unsigned, None, None).unwrap();
	assert!(signed.complete);
	let funding = signed.transaction().unwrap();
	assert_eq!(funding.output, unsigned.output);
	assert_eq!(sat(200_000) - funding.output.iter().map(|output| output.value).sum::<Amount>(), sat(1_000));
	let point = OutPoint::new(funding.compute_txid(), 0);
	let builder = builder.set_funding_details(sat(100_000), Amount::ZERO, point).unwrap().generate_user_nonces();
	let request = protos::BoardCosignRequest {
		amount: 100_000, utxo: serialize(&point), expiry_height: expiry.into(),
		user_pubkey: key.public_key().serialize().to_vec(), pub_nonce: builder.user_pub_nonce().serialize().to_vec(),
		funding_tx: serialize(&funding),
		owner_proof: Some(owner.proof(child_index, info.mailbox_pubkey)),
	};
	PreparedBoard { builder, funding, request }
}

/// Use the public board protocol and an exact registered child, without relying on delivery.
pub(super) async fn board(ctx: &TestContext, srv: &Captaind, owner: &Owner, child_index: u32) -> Vtxo<Full> {
	let PreparedBoard { builder, funding, request } = prepare_board(ctx, srv, owner, child_index).await;
	let key = owner.key(child_index);
	let mut rpc = srv.get_public_rpc().await;
	let cosign = rpc.request_board_cosign(request).await.unwrap().into_inner().try_into().unwrap();
	assert!(builder.verify_cosign_response(&cosign));
	let coin = builder.build_vtxo(&cosign, &key).unwrap();
	coin.validate(&funding).unwrap();
	assert_eq!(coin.amount(), sat(100_000));
	assert_eq!(coin.user_pubkey(), key.public_key());
	ctx.bitcoind().sync_client().send_raw_transaction(&funding).unwrap();
	let tip = ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	rpc.register_board_vtxo(protos::BoardVtxoRequest { board_vtxo: coin.serialize() }).await.unwrap();
	coin
}

async fn expire(ctx: &TestContext, srv: &Captaind, height: BlockHeight) {
	let current = ctx.bitcoind().get_block_count().await as u32;
	let tip = ctx.generate_blocks(height.to_u32() + 1 - current).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
}

async fn assert_paid(ctx: &TestContext, srv: &Captaind, owner: &Owner, coin: &Vtxo<Full>, index: u32) {
	assert_paid_to_owner(ctx, srv, owner, coin, index, coin.user_pubkey()).await;
}

async fn assert_paid_to_owner(
	ctx: &TestContext, srv: &Captaind, owner: &Owner, coin: &Vtxo<Full>, index: u32, owner_key: PublicKey,
) {
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let parent = tokio::time::timeout(Duration::from_secs(45), async {
		for _ in 0..24 {
			let row = db.read(async |tx| Ok(tx.query_opt("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND a.broadcast_at IS NOT NULL", &[&coin.id().to_string()]).await?)).await.unwrap();
			if let Some(row) = row { return deserialize::<Transaction>(row.get("sweep_tx")).unwrap(); }
			srv.watchmand().trigger_sweep().await;
			tokio::time::sleep(Duration::from_millis(750)).await;
			let tip = ctx.generate_blocks(1).await;
			srv.wait_for_sync_height(tip).await;
			srv.watchmand().wait_for_sync_height(tip).await;
		}
		panic!("owner payout did not appear within 24 progress blocks");
	}).await.expect("owner payout exceeded 45 seconds");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == owner.record.spk).collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	assert_eq!(outputs[0].1.value, sat(99_000));
	let tip = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(tip).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let core = ctx.bitcoind().sync_client();
	let confirmed = core.get_tx_out(&parent.compute_txid(), outputs[0].0 as u32, Some(true)).unwrap().unwrap();
	assert!(confirmed.confirmations >= 6);
	assert_eq!(confirmed.value, sat(99_000));
	assert_eq!(db.read(async |tx| tx.expiry_owner(owner_key, Network::Regtest).await).await.unwrap(),
		Attribution::Owned { mailbox_pk: owner.mailbox.public_key(), child_index: index, spk: owner.record.spk.clone() });
	let row = db.read(async |tx| Ok(tx.query_one("SELECT c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat \
		FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
		WHERE e.vtxo_id=$1 AND c.active", &[&coin.id().to_string()]).await?)).await.unwrap();
	for (field, amount) in [("gross_sat", 100_000), ("fee_sat", 1_000), ("payout_sat", 99_000), ("forfeited_sat", 0)] {
		assert_eq!(row.get::<_, i64>(field), amount);
	}
}

#[tokio::test]
async fn expiry_owner_finds_mailbox_key() {
	let ctx = TestContext::new("server/expiry_owner_finds_mailbox_key").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let coin = board(&ctx, &srv, &owner, 17).await;
	owner.post_mailbox(&srv, &coin).await;
	expire(&ctx, &srv, coin.expiry_height()).await;
	assert_paid(&ctx, &srv, &owner, &coin, 17).await;
}

#[tokio::test]
async fn expiry_owner_finds_key_without_mailbox() {
	let ctx = TestContext::new("server/expiry_owner_finds_key_without_mailbox").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let coin = board(&ctx, &srv, &owner, 500).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM mailbox WHERE vtxo_id=$1",
		&[&coin.id().to_string()]).await?.get::<_, i64>(0))).await.unwrap(), 0);
	expire(&ctx, &srv, coin.expiry_height()).await;
	assert_paid(&ctx, &srv, &owner, &coin, 500).await;
}

#[tokio::test]
async fn expiry_owner_finds_old_key_used_after_recovery() {
	let ctx = TestContext::new("server/expiry_owner_finds_old_key_used_after_recovery").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let first = board(&ctx, &srv, &owner, 500).await;
	expire(&ctx, &srv, first.expiry_height()).await;
	assert_paid(&ctx, &srv, &owner, &first, 500).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	assert_eq!(db.read(async |tx| tx.expiry_owner(owner.key(17).public_key(), Network::Regtest).await).await.unwrap(), Attribution::Unresolved);
	let late = board(&ctx, &srv, &owner, 17).await;
	assert!(late.expiry_height() > first.expiry_height());
	expire(&ctx, &srv, late.expiry_height()).await;
	assert_paid(&ctx, &srv, &owner, &late, 17).await;
}

#[tokio::test]
async fn expiry_owner_finds_htlc_key() {
	let ctx = TestContext::new("server/expiry_owner_finds_htlc_key").await;
	// Native send expiry plus its existing 30-block margin needs ordinary backing.
	let srv = server_with_lifetime(&ctx, BlockDelta::new(432)).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let input_key = owner.key(1);
	let input = board(&ctx, &srv, &owner, 1).await;
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let policy = VtxoPolicy::new_server_htlc_send(owner.key(500).public_key(), PaymentHash::from([19; 32]), deadline);
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input.clone()], vec![
		ArkoorDestination { total_amount: sat(100_000), policy: policy.clone() },
	]).unwrap().generate_user_nonces(&[input_key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let mut request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	owner.prove_destinations(&mut request, 500, srv.ark_info().await.mailbox_pubkey);
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: request.parts,
	}).await.unwrap().into_inner();
	let package = builder.user_cosign(&[input_key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap();
	let leaves = package.build_signed_vtxos();
	assert_eq!(leaves.len(), 1);
	let coin = &leaves[0];
	assert_eq!(coin.policy(), &policy);
	assert_eq!(coin.amount(), sat(100_000));
	let funding = ctx.bitcoind().await_transaction(input.chain_anchor().txid).await;
	coin.validate(&funding).unwrap();
	rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest { vtxos: vec![coin.serialize()] }).await.unwrap();
	expire(&ctx, &srv, deadline.max(coin.expiry_height())).await;
	assert_paid(&ctx, &srv, &owner, coin, 500).await;
}

#[tokio::test]
async fn expiry_owner_finds_revoked_htlc_input_key() {
	let ctx = TestContext::new("server/expiry_owner_finds_revoked_htlc_input_key").await;
	// Native send expiry plus its existing 30-block margin needs ordinary backing.
	let srv = server_with_lifetime(&ctx, BlockDelta::new(432)).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let input_key = owner.key(1);
	let input = board(&ctx, &srv, &owner, 1).await;
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let policy = VtxoPolicy::new_server_htlc_send(owner.key(500).public_key(), PaymentHash::from([19; 32]), deadline);
	let builder = ArkoorPackageBuilder::new_with_checkpoints([input.clone()], vec![
		ArkoorDestination { total_amount: sat(100_000), policy: policy.clone() },
	]).unwrap().generate_user_nonces(&[input_key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let mut request = protos::ArkoorPackageCosignRequest::from(builder.cosign_request());
	owner.prove_destinations(&mut request, 500, srv.ark_info().await.mailbox_pubkey);
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: request.parts,
	}).await.unwrap().into_inner();
	let package = builder.user_cosign(&[input_key], ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap();
	let leaves = package.build_signed_vtxos();
	assert_eq!(leaves.len(), 1);
	let coin = &leaves[0];
	assert_eq!(coin.policy(), &policy);
	assert_eq!(coin.amount(), sat(100_000));
	let funding = ctx.bitcoind().await_transaction(input.chain_anchor().txid).await;
	coin.validate(&funding).unwrap();
	rpc.register_vtxo_transactions(protos::RegisterVtxoTransactionsRequest { vtxos: vec![coin.serialize()] }).await.unwrap();
	let refund_owner = Owner::with_seed(&ctx, 118);
	refund_owner.register(&srv).await;
	let refund_key = refund_owner.key(1);
	assert_ne!(refund_key.public_key(), owner.key(500).public_key());
	let revoke = ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		vec![coin.clone()], VtxoPolicy::new_pubkey(refund_key.public_key()),
	).unwrap().generate_user_nonces(&[owner.key(500)]).unwrap();
	let mut request = protos::ArkoorPackageCosignRequest::from(revoke.cosign_request());
	refund_owner.prove_destinations(&mut request, 1, srv.ark_info().await.mailbox_pubkey);
	let response = rpc.request_lightning_pay_htlc_revocation(request).await.unwrap().into_inner();
	let refunds = revoke.user_cosign(&[owner.key(500)],
		ArkoorPackageCosignResponse::try_from(response).unwrap()).unwrap().build_signed_vtxos();
	assert_eq!(refunds.len(), 1);
	let refund = &refunds[0];
	assert_eq!(refund.amount(), sat(100_000));
	assert_eq!(refund.policy(), &VtxoPolicy::new_pubkey(refund_key.public_key()));
	refund.validate(&funding).unwrap();
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let stored = db.read(async |tx| tx.get_bare_vtxo_by_id(refund.id()).await).await.unwrap();
	assert_eq!(stored.spend_state, server::database::SpendState::Unregistered);
	assert!(db.read(async |tx| tx.get_virtual_transaction_by_txid(refund.point().txid).await)
		.await.unwrap().unwrap().signed_tx.is_none());
	assert_eq!(db.read(async |tx| tx.expiry_owner(owner.key(500).public_key(), Network::Regtest).await)
		.await.unwrap(), Attribution::Owned { mailbox_pk: owner.mailbox.public_key(),
			child_index: 500, spk: owner.record.spk.clone() });
	// Keep the signed refund local, so attribution must use the committed input owner.
	expire(&ctx, &srv, deadline.max(refund.expiry_height())).await;
	assert_paid_to_owner(&ctx, &srv, &owner, refund, 500, owner.key(500).public_key()).await;
}


#[tokio::test]
async fn expiry_owner_proof_pays_key_beyond_former_recovery_gap() {
	let ctx = TestContext::new("server/expiry_owner_proof_pays_key_beyond_former_recovery_gap").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let coin = board(&ctx, &srv, &owner, 20_001).await;
	expire(&ctx, &srv, coin.expiry_height()).await;
	assert_paid(&ctx, &srv, &owner, &coin, 20_001).await;
}

/// A legacy database can contain signed customer coins without an issuance
/// mapping. Model that restored state without adding a creation bypass: issue
/// through the real proof-bearing board path, then restore only the old rows.
#[tokio::test]
async fn expiry_owner_holds_legacy_coin_without_issuance_mapping() {
	let ctx = TestContext::new("server/expiry_owner_holds_legacy_coin_without_issuance_mapping").await;
	let srv = server(&ctx).await;
	let owner = Owner::new(&ctx);
	owner.register(&srv).await;
	let coin = board(&ctx, &srv, &owner, 20_001).await;
	let funding = ctx.bitcoind().await_transaction(coin.chain_anchor().txid).await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	db.write(async |tx| {
		tx.execute("DELETE FROM expiry_owner_key WHERE user_pubkey=$1", &[&coin.user_pubkey().to_string()]).await?;
		tx.execute("DELETE FROM fallback_record WHERE mailbox_pk=$1", &[&owner.mailbox.public_key().to_string()]).await?;
		Ok(())
	}).await.unwrap();
	expire(&ctx, &srv, coin.expiry_height()).await;
	for _ in 0..3 {
		srv.watchmand().trigger_sweep().await;
		tokio::time::sleep(Duration::from_millis(750)).await;
		let tip = ctx.generate_blocks(1).await;
		srv.wait_for_sync_height(tip).await;
		srv.watchmand().wait_for_sync_height(tip).await;
	}
	assert_eq!(db.read(async |tx| tx.expiry_owner(coin.user_pubkey(), Network::Regtest).await)
		.await.unwrap(), Attribution::Unresolved);
	let stored = db.read(async |tx| tx.get_user_vtxo_by_id(coin.id()).await).await.unwrap();
	assert_eq!(stored.spend_state, server::database::SpendState::Spendable,
		"an unknown owner cannot be retired without a payment obligation");
	assert_eq!(stored.vtxo.serialize(), coin.serialize());
	let allocations = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_entitlement WHERE vtxo_id=$1",
		&[&coin.id().to_string()]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(allocations, 0, "neither customer payout nor operator income may be fabricated");
	let backing = ctx.bitcoind().sync_client().get_tx_out(&coin.chain_anchor().txid,
		coin.chain_anchor().vout, Some(true)).unwrap().unwrap();
	assert_eq!(backing.value, funding.output[coin.chain_anchor().vout as usize].value);
	assert_eq!(backing.script_pub_key.script().unwrap(), funding.output[coin.chain_anchor().vout as usize].script_pubkey);
	assert!(backing.confirmations >= 6, "the signed legacy recovery backing stays unspent");
	coin.validate(&funding).unwrap();
}
