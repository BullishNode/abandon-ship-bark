use bitcoin::hashes::Hash;

use ark::{ProtocolEncoding, VtxoPolicy, VtxoRequest};
use ark::arkoor::package::ArkoorPackageBuilder;
use ark::attestations::DelegatedRoundParticipationAttestation;
use ark::lightning::PaymentHash;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoin_ext::rpc::RpcApi;
use server_rpc::protos;

use ark_testing::{btc, sat, TestContext};


/// An actual singleton round records its complete requested prefix and the
/// operator-funded appended leaf. The tuple alone remains insufficient.
#[tokio::test]
async fn expiry_future_padding_has_creation_provenance() {
	let ctx = TestContext::new("server/expiry_future_padding_provenance").await;
	let srv = ctx.captaind("server").funded(btc(10)).no_vtxo_pool().create().await;
	let wallet = ctx.bark("wallet", &srv).funded(sat(500_000)).create().await;
	let originals = wallet.board_and_confirm_and_register(&ctx, sat(200_000)).await;
	ctx.refresh_all(&srv, &[&wallet]).await;
	ctx.generate_blocks(ark_testing::constants::ROUND_CONFIRMATIONS).await;
	wallet.sync().await;
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let round = db.read(async |t| {
		let id = t.get_last_round_id().await?.expect("refresh must issue a round");
		Ok(t.get_round(id).await?.unwrap())
	}).await.unwrap();
	let tree = round.into_cached_tree().unwrap();
	let leaves = tree.output_vtxos().collect::<Vec<_>>();
	assert_eq!(leaves.len(), 2);
	let padding = leaves.iter().find(|leaf| leaf.amount() == sat(330)).unwrap();
	assert_eq!(padding.user_pubkey().to_string(), "02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c");
	let hash = padding.unlock_hash().unwrap();
	assert_eq!(hash.to_string(), "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
	assert_eq!(leaves.iter().map(|leaf| leaf.amount()).sum::<bitcoin::Amount>(), sat(200_330));
	let evidence_rows = db.read(async |t| {
		Ok(t.query_one("
			SELECT
				(SELECT COUNT(*) FROM round_participation WHERE unlock_hash=$1) AS participations,
				(SELECT COUNT(*) FROM expiry_cancelled_participation WHERE unlock_hash=$1) AS cancellations
		", &[&hash.to_string()]).await?)
	}).await.unwrap();
	assert_eq!(evidence_rows.get::<_, i64>("participations"), 0);
	assert_eq!(evidence_rows.get::<_, i64>("cancellations"), 0);
	let funding = ctx.bitcoind().await_transaction(padding.chain_anchor().txid).await;
	padding.validate_unsigned(&funding).unwrap();
	let root = ark::ServerVtxo::from((*padding).clone()).to_bare();
	let limits = server::database::expiry_inventory::BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 10_000_000 };
	let padding_snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], limits).await).await.unwrap();
	let padding_fact = padding_snapshot.regular.iter().find(|f| f.state.vtxo_id == padding.id()).unwrap();
	assert_eq!(padding_fact.state.spend_state, server::database::SpendState::Unclaimed);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let proposal = db.write(async |t| t.propose_expiry_ownership(
		padding_fact, &funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert_eq!(proposal.id, padding.id());
	assert_eq!(proposal.gross, sat(330));
	assert!(matches!(proposal.disposition, server::database::expiry_ownership::OwnershipDisposition::Operator));
	assert_eq!(proposal.evidence, Some(server::database::expiry_ownership::OwnershipEvidence::RoundPaddingIssuance {
		funding_txid: funding.compute_txid(), leaf_index: 1,
	}));
	let issuance = db.read(async |t| Ok(t.query_one(
		"SELECT padding_issuance::TEXT AS issuance FROM round WHERE funding_txid=$1", &[&funding.compute_txid().to_string()],
	).await?.get::<_, String>("issuance"))).await.unwrap();
	let record: serde_json::Value = serde_json::from_str(&issuance).unwrap();
	assert_eq!(record["version"], 1);
	assert_eq!(record["requested_leaf_count"], 1);
	assert_eq!(record["input_value_sat"], 200_000);
	assert_eq!(record["refresh_surplus_sat"], 0);
	let core = ctx.bitcoind().sync_client();
	let mut input_total = sat(0);
	for (input, retained) in funding.input.iter().zip(record["funding_prevouts"].as_array().unwrap()) {
		let point: bitcoin::OutPoint = serde_json::from_value(retained["outpoint"].clone()).unwrap();
		let txout: bitcoin::TxOut = serde_json::from_value(retained["txout"].clone()).unwrap();
		assert_eq!(point, input.previous_output);
		let parent = core.get_raw_transaction(&point.txid, None).unwrap();
		assert_eq!(txout, parent.output[point.vout as usize]);
		input_total += txout.value;
	}
	assert_eq!(record["funding_prevouts"].as_array().unwrap().len(), funding.input.len());
	assert_eq!(input_total.to_sat(), funding.output.iter().map(|output| output.value.to_sat()).sum::<u64>()
		+ record["funding_fee_sat"].as_u64().unwrap());
	for replacement in [None, Some("{}".to_string())] {
		assert!(db.write(async |t| Ok(t.execute(
			"UPDATE round SET padding_issuance=$1::TEXT::JSONB WHERE funding_txid=$2",
			&[&replacement, &funding.compute_txid().to_string()],
		).await?)).await.is_err(), "creation metadata must be immutable, including erasure");
	}
	assert!(proposal.cancellations.is_empty());
	assert!(!proposal.needs_chain_progress);

	// The same real refresh did pay for its new customer output: its accepted
	// original forfeit remains operator capital, with the complete old amount.
	let inputs = db.read(async |t| t.get_user_vtxos_by_id(&originals).await).await.unwrap();
	assert_eq!(inputs.len(), 1);
	let original = &inputs[0];
	assert_eq!(original.spend_state, server::database::SpendState::Spent);
	assert!(original.spent_in_round.is_some());
	assert!(original.oor_spent_txid.is_some());
	assert_eq!(original.vtxo.amount(), sat(200_000));
	let original_funding = ctx.bitcoind().await_transaction(original.vtxo.chain_anchor().txid).await;
	original.vtxo.validate(&original_funding).unwrap();
	let root = ark::ServerVtxo::from(original.vtxo.clone()).to_bare();
	let snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], limits).await).await.unwrap();
	let fact = snapshot.regular.iter().find(|f| f.state.vtxo_id == original.vtxo_id).unwrap();
	let recovery = db.write(async |t| t.propose_expiry_ownership(
		fact, &original_funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert_eq!(recovery.gross, sat(200_000));
	assert!(matches!(recovery.disposition, server::database::expiry_ownership::OwnershipDisposition::Operator));
	assert!(matches!(recovery.evidence,
		Some(server::database::expiry_ownership::OwnershipEvidence::Exchange { forfeited: true, .. })
	));
	assert!(recovery.cancellations.is_empty());

	// Simulate the existing participation cleanup, then reopen both processes.
	// Issuance does not depend on the continued presence of those request rows.
	srv.stop().await.unwrap();
	srv.watchmand().stop().await.unwrap();
	db.write(async |t| {
		t.execute("DELETE FROM round_part_input WHERE participation_id IN (SELECT id FROM round_participation WHERE round_id=$1)", &[&funding.compute_txid().to_string()]).await?;
		t.execute("DELETE FROM round_part_output WHERE participation_id IN (SELECT id FROM round_participation WHERE round_id=$1)", &[&funding.compute_txid().to_string()]).await?;
		t.execute("DELETE FROM round_participation WHERE round_id=$1", &[&funding.compute_txid().to_string()]).await?;
		Ok(())
	}).await.unwrap();
	srv.start().await.unwrap();
	srv.watchmand().start().await.unwrap();
	let reopened = db.write(async |t| t.propose_expiry_ownership(
		padding_fact, &funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert_eq!(reopened.disposition, proposal.disposition);
	assert_eq!(reopened.evidence, proposal.evidence);
	let retained = db.read(async |t| Ok(t.query_one(
		"SELECT padding_issuance::TEXT AS issuance FROM round WHERE funding_txid=$1", &[&funding.compute_txid().to_string()],
	).await?.get::<_, String>("issuance"))).await.unwrap();
	assert_eq!(retained, issuance);
}


/// This authenticated customer request selects the padding key and 330 sats.
/// The API generates its unlock hash; the client cannot select that third field.
#[tokio::test]
async fn expiry_customer_padding_key_and_amount_are_not_an_issuance_role() {
	let ctx = TestContext::new("server/expiry_customer_padding_tuple").await;
	let srv = ctx.captaind("server").funded(btc(10)).no_vtxo_pool()
		.cfg(|config| config.round_interval = std::time::Duration::from_secs(3600)).create().await;
	let wallet = ctx.bark("wallet", &srv).funded(sat(500_000))
		.cfg(|config| config.daemon_manual_sync = true).create().await;
	let ids = wallet.board_and_confirm_and_register(&ctx, sat(200_000)).await;
	let client = wallet.client().await;
	let input = client.get_full_vtxo(ids[0]).await.unwrap();
	let key = client.get_vtxo_key(&input).await.unwrap();
	let request = VtxoRequest {
		policy: VtxoPolicy::new_pubkey("02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c".parse().unwrap()),
		amount: sat(330),
	};
	let mut rpc = srv.get_public_rpc().await;
	let admitted = rpc.submit_round_participation(protos::RoundParticipationRequest {
		input_vtxos: vec![protos::InputVtxo {
			vtxo_id: input.id().to_bytes().to_vec(),
			attestation: DelegatedRoundParticipationAttestation::new(input.id(), &[request.clone()], &key).serialize(),
		}],
		vtxo_requests: vec![protos::VtxoRequest { amount: 330, policy: request.policy.serialize() }],
		unblinded_mailbox_id: None, scheduled_height: None,
	}).await.unwrap().into_inner();
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	srv.trigger_round().await;
	let round = tokio::time::timeout(std::time::Duration::from_secs(30), async {
		loop {
			if let Some(id) = db.read(async |t| t.get_last_round_id().await).await.unwrap() {
				break db.read(async |t| t.get_round(id).await).await.unwrap().unwrap();
			}
			tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		}
	}).await.expect("the actual accepted request must produce a round");
	let funding = ctx.bitcoind().await_transaction(round.funding_txid.as_round_txid()).await;
	ctx.generate_blocks(ark_testing::constants::ROUND_CONFIRMATIONS).await;
	let tree = round.into_cached_tree().unwrap();
	let leaves = tree.output_vtxos().collect::<Vec<_>>();
	assert_eq!(leaves.len(), 2);
	assert_eq!(leaves.iter().map(|leaf| leaf.amount()).sum::<bitcoin::Amount>(), sat(660));
	assert!(leaves.iter().all(|leaf| leaf.policy() == &request.policy && leaf.amount() == sat(330)));
	assert_eq!(leaves[0].unlock_hash().unwrap().to_byte_array().to_vec(), admitted.unlock_hash);
	assert_eq!(leaves[1].unlock_hash().unwrap().to_string(), "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
	assert_ne!(leaves[0].id(), leaves[1].id());
	let issuance = db.read(async |t| Ok(t.query_one(
		"SELECT padding_issuance::TEXT AS issuance FROM round WHERE funding_txid=$1", &[&funding.compute_txid().to_string()],
	).await?.get::<_, String>("issuance"))).await.unwrap();
	let record: serde_json::Value = serde_json::from_str(&issuance).unwrap();
	assert_eq!(record["input_value_sat"], 200_000);
	assert_eq!(record["refresh_surplus_sat"], 199_670);
	assert_eq!(record["requested_leaf_count"], 1);
	let root = ark::ServerVtxo::from(leaves[0].clone()).to_bare();
	let limits = server::database::expiry_inventory::BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 10_000_000 };
	let snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], limits).await).await.unwrap();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	for (index, leaf) in leaves.iter().enumerate() {
		let fact = snapshot.regular.iter().find(|fact| fact.state.vtxo_id == leaf.id()).unwrap();
		let proposal = db.write(async |t| t.propose_expiry_ownership(
			fact, &funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
		).await).await.unwrap();
		assert_eq!(proposal.gross, sat(330));
		if index == 0 {
			assert!(matches!(proposal.evidence, Some(server::database::expiry_ownership::OwnershipEvidence::Exchange { forfeited: false, .. })),
				"an authenticated request retains its actual exchange evidence");
		} else {
			assert_eq!(proposal.evidence, Some(server::database::expiry_ownership::OwnershipEvidence::RoundPaddingIssuance {
				funding_txid: funding.compute_txid(), leaf_index: 1,
			}));
		}
	}
	// A missing participation is not an alternative padding admission path.
	// Keep the exact immutable creation record, and remove only request rows.
	srv.stop().await.unwrap();
	srv.watchmand().stop().await.unwrap();
	db.write(async |t| {
		t.execute("DELETE FROM round_part_input WHERE participation_id IN (SELECT id FROM round_participation WHERE round_id=$1)", &[&funding.compute_txid().to_string()]).await?;
		t.execute("DELETE FROM round_part_output WHERE participation_id IN (SELECT id FROM round_participation WHERE round_id=$1)", &[&funding.compute_txid().to_string()]).await?;
		t.execute("DELETE FROM round_participation WHERE round_id=$1", &[&funding.compute_txid().to_string()]).await?;
		Ok(())
	}).await.unwrap();
	let customer = snapshot.regular.iter().find(|fact| fact.state.vtxo_id == leaves[0].id()).unwrap();
	let proposal = db.write(async |t| t.propose_expiry_ownership(
		customer, &funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert!(matches!(proposal.disposition, server::database::expiry_ownership::OwnershipDisposition::Unresolved(_)));
	assert_eq!(proposal.evidence, None);
	assert!(proposal.cancellations.is_empty());
}


/// A registered grant is a real HTLC even when the sender never submits an
/// external payment. The preimage settler has no timeout event for this case.
#[tokio::test]
async fn expiry_abandoned_registered_send_needs_no_client_refund() {
	let ctx = TestContext::new("server/expiry_abandoned_registered_send").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24))
		.watchmand_cfg(|cfg| cfg.watchman.sweep_interval = std::time::Duration::from_secs(3600))
		.create().await;
	let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let wallet = sender.client().await;
	let input = wallet.get_full_vtxo(ids[0]).await.unwrap();
	let key = wallet.get_vtxo_key(&input).await.unwrap();
	let preimage_bytes = [12; 32];
	let invoice = lightning.external.invoice_with_preimage(Some(sat(100_000)), "abandoned", "Never initiated", preimage_bytes).await;
	let parsed: bark::lightning_invoice::Bolt11Invoice = invoice.parse().unwrap();
	let hash = PaymentHash::from(&parsed);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		[input.clone()], VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline),
	).unwrap().generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let response = ark::arkoor::package::ArkoorPackageCosignResponse::try_from(response).unwrap();
	let htlcs = builder.user_cosign(&[key], response).unwrap().build_signed_vtxos();
	wallet.register_vtxo_transactions_with_server(&htlcs).await.unwrap();
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let htlc = &htlcs[0];
	assert!(input.expiry_height() < deadline);
	let expired = ctx.generate_blocks(input.expiry_height().to_u32() + 1 - tip.to_u32()).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let error = rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
		invoice, payment_amount_sat: 100_000,
		htlc_vtxo_ids: htlcs.iter().map(|v| v.id().to_bytes().to_vec()).collect(),
		mailbox_id: None, retry_for_secs: None,
	}).await.expect_err("offline sender cannot start an external payment after backing expiry");
	assert!(error.message().contains("expired"), "{error}");
	let attempts = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap();
	assert!(attempts.is_none());
	let root = ark::ServerVtxo::from(input.clone()).to_bare();
	let funding = db.read(async |t| t.get_virtual_transaction_by_txid(input.chain_anchor().txid).await).await.unwrap().unwrap()
		.signed_tx.unwrap().into_owned();
	let limits = server::database::expiry_inventory::BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 10_000_000 };
	let snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], limits).await).await.unwrap();
	let fact = snapshot.regular.iter().find(|f| f.state.vtxo_id == htlc.id()).unwrap();
	let pending = db.write(async |t| t.propose_expiry_ownership(
		fact, &funding, expired, Some(expired), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert!(matches!(pending.disposition, server::database::expiry_ownership::OwnershipDisposition::Unresolved(_)));
	let tip = ctx.generate_blocks(deadline.to_u32() + 1 - expired.to_u32()).await;
	srv.watchmand().wait_for_sync_height(tip).await;
	let snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], limits).await).await.unwrap();
	let fact = snapshot.regular.iter().find(|f| f.state.vtxo_id == htlc.id()).unwrap();
	let ready = db.write(async |t| t.propose_expiry_ownership(
		fact, &funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert!(matches!(ready.disposition,
		server::database::expiry_ownership::OwnershipDisposition::Customer { user_pubkey, .. }
			if user_pubkey == key.public_key()
	));
	assert!(matches!(ready.evidence, Some(
		server::database::expiry_ownership::OwnershipEvidence::HtlcUnattemptedExpired { .. }
			| server::database::expiry_ownership::OwnershipEvidence::IrreversibleEntitlement { .. }
	)));
	// Readiness never invents an upstream off-chain revocation. A concurrent
	// background allocation may already have recorded the genuine spend fence.
	let upstream = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, htlc.id()).await).await.unwrap().unwrap();
	assert_eq!(upstream.htlc.resolution(), None);
}

/// An actual issued refresh can be cancelled before its forfeit binds. The
/// product's participation-removal method must not erase the old unclaimed
/// output's ownership. This tests settlement evidence, not a payout race.
#[tokio::test]
async fn expiry_cancelled_unclaimed_output_keeps_owner_after_participation_removed() {
	let ctx = TestContext::new("server/expiry_cancelled_unclaimed_tombstone").await;
	let srv = ctx.captaind("server").funded(btc(10)).no_vtxo_pool().create().await;
	let wallet = ctx.bark("wallet", &srv).funded(sat(500_000)).create().await;
	let originals = wallet.board_and_confirm_and_register(&ctx, sat(200_000)).await;
	ctx.refresh_all(&srv, &[&wallet]).await;
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let (participation, hash, leaf) = db.read(async |t| {
		let row = t.query_one("
			SELECT id, unlock_hash, forfeited_at IS NOT NULL AS forfeited
			FROM round_participation
			WHERE round_id IS NOT NULL
			ORDER BY id DESC
			LIMIT 1
		", &[]).await?;
		assert!(!row.get::<_, bool>("forfeited"));
		let hash: ark::tree::signed::UnlockHash = row.get::<_, &str>("unlock_hash").parse()?;
		let ids = t.query("
			SELECT vtxo_id
			FROM vtxo
			WHERE spend_state = 'unclaimed'
			ORDER BY id
		", &[]).await?
			.into_iter().map(|r| r.get::<_, &str>(0).parse::<ark::VtxoId>()).collect::<Result<Vec<_>, _>>()?;
		let states = t.get_server_vtxos_by_id(&ids).await?;
		let leaf = states.into_iter().find(|state| state.vtxo.clone().try_into_user_vtxo()
			.is_ok_and(|user| user.unlock_hash() == Some(hash))).expect("actual round must store its unclaimed output").vtxo;
		Ok((row.get::<_, i64>("id"), hash, leaf))
	}).await.unwrap();
	assert!(db.write(async |t| t.cancel_expiry_participations(&[participation]).await).await.unwrap());
	assert!(db.write(async |t| t.remove_round_participation(hash).await).await.unwrap());
	let remaining = db.read(async |t| Ok(t.query_one("
		SELECT COUNT(*)
		FROM round_participation
		WHERE id = $1
	", &[&participation]).await?.get::<_, i64>(0))).await.unwrap();
	assert_eq!(remaining, 0);
	let funding = ctx.bitcoind().await_transaction(leaf.chain_anchor().txid).await;
	let limits = server::database::expiry_inventory::BackingSnapshotLimits { max_rows: 10_000, max_serialized_bytes: 10_000_000 };
	let root = leaf.to_bare();
	let snapshot = db.read(async |t| t.backing_snapshot(&[root.clone()], limits).await).await.unwrap();
	let fact = snapshot.regular.iter().find(|f| f.state.vtxo_id == root.id()).unwrap();
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let proposal = db.write(async |t| t.propose_expiry_ownership(
		fact, &funding, tip, Some(tip), srv.config().expiry_internal_refund_depth,
	).await).await.unwrap();
	assert!(matches!(proposal.disposition, server::database::expiry_ownership::OwnershipDisposition::Operator));
	assert!(matches!(proposal.evidence,
		Some(server::database::expiry_ownership::OwnershipEvidence::Exchange {
			participation_id, ref unlock_hash, forfeited: false
		}) if participation_id == participation && unlock_hash == &hash.to_string()
	));
	let inputs = db.read(async |t| t.get_user_vtxos_by_id(&originals).await).await.unwrap();
	assert!(inputs.iter().all(|input| input.spend_state == server::database::SpendState::Spendable));
	assert_eq!(inputs.iter().map(|input| input.vtxo.amount().to_sat()).sum::<u64>(), 200_000);
}
