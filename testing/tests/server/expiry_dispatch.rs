use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ark::arkoor::package::{ArkoorPackageBuilder, ArkoorPackageCosignResponse};
use ark::lightning::{Bolt12InvoiceExt, Invoice, PaymentHash, Preimage};
use ark::VtxoPolicy;
use bitcoin_ext::{BlockDelta, BlockHeight, TxOutExt};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::ln::LightningPaymentStatus;
use server_rpc::protos;
use cln_rpc::plugins::hold;
use bitcoin::constants::ChainHash;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey};
use bitcoin::{Amount, Network};
use lightning::blinded_path::{BlindedHop, Direction, IntroductionNode};
use lightning::blinded_path::payment::{BlindedPayInfo, BlindedPaymentPath};
use lightning::io::Cursor;
use lightning::ln::channelmanager::PaymentId;
use lightning::ln::inbound_payment::ExpandedKey;
use lightning::offers::invoice::{Bolt12Invoice, UnsignedBolt12Invoice};
use lightning::offers::nonce::Nonce;
use lightning::offers::offer::OfferBuilder;
use lightning::sign::EntropySource;
use lightning::types::features::BlindedHopFeatures;
use lightning::util::ser::{BigSize, Readable, Writeable};

use ark_testing::{btc, sat, TestContext};
use ark_testing::util::{FutureExt, poll_interval};

struct InvoiceEntropy;

impl EntropySource for InvoiceEntropy {
	fn get_secure_random_bytes(&self) -> [u8; 32] { rand::random() }
}

fn unresolved_introduction_invoice(
	payment_hash: PaymentHash, amount_msat: u64, unknown_scid: u64,
) -> Invoice {
	assert!(amount_msat > 0);
	assert_eq!(amount_msat % 1_000, 0, "registered HTLC principal must be whole sats");
	let principal = Amount::from_sat(amount_msat / 1_000);
	let secp = Secp256k1::new();
	let pubkey = |byte| PublicKey::from_secret_key(
		&secp, &SecretKey::from_slice(&[byte; 32]).unwrap(),
	);
	let issuer = Keypair::from_secret_key(
		&secp, &SecretKey::from_slice(&[43; 32]).unwrap(),
	);
	let introduction = pubkey(40);
	let path = BlindedPaymentPath::from_blinded_path_and_payinfo(
		introduction,
		pubkey(41),
		vec![
			BlindedHop { blinded_node_id: pubkey(43), encrypted_payload: vec![0; 43] },
			BlindedHop { blinded_node_id: pubkey(44), encrypted_payload: vec![0; 44] },
		],
		BlindedPayInfo {
			fee_base_msat: 1,
			fee_proportional_millionths: 1_000,
			cltv_expiry_delta: 42,
			htlc_minimum_msat: 100,
			htlc_maximum_msat: 1_000_000_000_000,
			features: BlindedHopFeatures::empty(),
		},
	);
	let offer = OfferBuilder::new(issuer.public_key())
		.chain(Network::Regtest)
		.amount_msats(amount_msat)
		.build().unwrap();
	let expanded_key = ExpandedKey::new(rand::random());
	let nonce = Nonce::from_entropy_source(&InvoiceEntropy);
	let request = offer.request_invoice(
		&expanded_key, nonce, &secp, PaymentId(rand::random()),
	).unwrap().chain(Network::Regtest).unwrap().build_and_sign().unwrap();
	assert_eq!(request.chain(), ChainHash::using_genesis_block(Network::Regtest));
	let ldk_hash = lightning::types::payment::PaymentHash(payment_hash.to_byte_array());
	let unsigned = request.respond_with(vec![path], ldk_hash).unwrap().build().unwrap();
	assert_eq!(unsigned.payment_paths().len(), 1);
	assert_eq!(unsigned.payment_paths()[0].introduction_node(), &IntroductionNode::NodeId(introduction));

	// Change the introduction in the unsigned stream, then sign the new invoice.
	let mut bytes = Vec::new();
	unsigned.write(&mut bytes).unwrap();
	let stream_len = u64::try_from(bytes.len()).unwrap();
	let mut cursor = Cursor::new(bytes.as_slice());
	let mut modified = Vec::new();
	let mut replaced = false;
	while cursor.position() < stream_len {
		let record_start = usize::try_from(cursor.position()).unwrap();
		let typ = BigSize::read(&mut cursor).unwrap().0;
		let len = usize::try_from(BigSize::read(&mut cursor).unwrap().0).unwrap();
		let value_start = usize::try_from(cursor.position()).unwrap();
		let value_end = value_start.checked_add(len).unwrap();
		let value = bytes.get(value_start..value_end).expect("complete local TLV value");
		if typ == 160 {
			assert!(!replaced, "one payment-path TLV");
			let prefix = value.get(..33).expect("compressed introduction key");
			assert!(matches!(prefix[0], 2 | 3));
			assert_eq!(prefix, introduction.serialize().as_slice());
			let replacement_len = len.checked_sub(33).unwrap().checked_add(9).unwrap();
			BigSize(typ).write(&mut modified).unwrap();
			BigSize(u64::try_from(replacement_len).unwrap()).write(&mut modified).unwrap();
			modified.push(0); // DirectedShortChannelId, NodeOne.
			modified.extend_from_slice(&unknown_scid.to_be_bytes());
			modified.extend_from_slice(value.get(33..).unwrap());
			replaced = true;
		} else {
			modified.extend_from_slice(bytes.get(record_start..value_end).unwrap());
		}
		cursor.set_position(u64::try_from(value_end).unwrap());
	}
	assert_eq!(cursor.position(), stream_len);
	assert!(replaced);
	let unsigned = UnsignedBolt12Invoice::try_from(modified).unwrap();
	let directed = IntroductionNode::DirectedShortChannelId(Direction::NodeOne, unknown_scid);
	assert_eq!(unsigned.payment_paths().len(), 1);
	assert_eq!(unsigned.payment_paths()[0].introduction_node(), &directed);
	assert_eq!(unsigned.chain(), ChainHash::using_genesis_block(Network::Regtest));
	assert_eq!(unsigned.payment_hash(), ldk_hash);
	assert_eq!(unsigned.amount_msats(), amount_msat);
	let invoice = unsigned.sign(|message: &UnsignedBolt12Invoice| {
		Ok(secp.sign_schnorr_no_aux_rand(message.as_ref().as_digest(), &issuer))
	}).unwrap();

	let mut signed_bytes = Vec::new();
	invoice.write(&mut signed_bytes).unwrap();
	let signed = Bolt12Invoice::try_from(signed_bytes).unwrap();
	assert_eq!(signed.payment_paths().len(), 1);
	assert_eq!(signed.payment_paths()[0].introduction_node(), &directed);
	assert_eq!(signed.chain(), ChainHash::using_genesis_block(Network::Regtest));
	assert_eq!(signed.payment_hash(), ldk_hash);
	assert_eq!(signed.amount_msats(), amount_msat);
	assert_eq!(signed.issuer_signing_pubkey(), Some(issuer.public_key()));
	signed.check_signature().unwrap();
	signed.validate_signing_key(&offer).unwrap();
	signed.validate_issuance(&offer, principal).unwrap();

	let invoice = Invoice::Bolt12(signed).to_string().parse::<Invoice>().unwrap();
	let Invoice::Bolt12(signed) = &invoice else { panic!("BOLT12 round trip") };
	assert_eq!(signed.payment_paths().len(), 1);
	assert_eq!(signed.payment_paths()[0].introduction_node(), &directed);
	assert_eq!(signed.chain(), ChainHash::using_genesis_block(Network::Regtest));
	assert_eq!(signed.payment_hash(), ldk_hash);
	assert_eq!(signed.amount_msats(), amount_msat);
	assert_eq!(signed.issuer_signing_pubkey(), Some(issuer.public_key()));
	signed.check_signature().unwrap();
	signed.validate_signing_key(&offer).unwrap();
	signed.validate_issuance(&offer, principal).unwrap();
	invoice
}

#[tokio::test]
async fn lightning_rejection_refunds_only_its_source() {
	tokio::time::timeout(Duration::from_secs(180), async {
		let ctx = TestContext::new("server/lightning_rejection_refunds_only_its_source").await;
		let lightning = ctx.new_lightning_setup("lightningd").await;
		let srv = ctx.captaind("server").lightningd(&lightning.internal).no_vtxo_pool().funded(btc(10))
			.cfg(|cfg| cfg.invoice_check_interval = Duration::from_secs(1)).watchmand().create().await;
		let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
		let boards = [sender.board_and_confirm_and_register(&ctx, sat(100_000)).await[0],
			sender.board_and_confirm_and_register(&ctx, sat(100_000)).await[0]];
		let wallet = sender.client().await;
		let hash = Preimage::from_slice(&[73;32]).unwrap().compute_payment_hash();
		let principal = sat(90_000);
		let unknown_scid = 0x00ff_ffffu64 << 40;
		assert!(ctx.bitcoind().get_block_count().await < unknown_scid >> 40);
		let invoice = unresolved_introduction_invoice(hash, principal.to_sat() * 1_000, unknown_scid);
		let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
		let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
		let mut rpc = srv.get_public_rpc().await;
		let mut sources = Vec::new();
		for id in boards {
			let input = wallet.get_full_vtxo(id).await.unwrap();
			let key = wallet.get_vtxo_key(&input).await.unwrap();
			let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input],
				VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline)).unwrap()
				.generate_user_nonces(&[key]).unwrap();
			let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
				parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
			}).await.unwrap().into_inner();
			let mut grants = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
				.unwrap().build_signed_vtxos();
			assert_eq!(grants.len(), 1);
			assert_eq!(grants[0].amount(), sat(100_000));
			wallet.register_vtxo_transactions_with_server(&grants).await.unwrap();
			sources.push((grants.remove(0), key));
		}
		lightning.sync().await;
		let mut payer = lightning.internal.grpc_client().await;
		let info = payer.getinfo(cln_rpc::GetinfoRequest {}).await.unwrap().into_inner();
		assert_eq!(info.network, "regtest");
		assert_eq!(info.version, "v26.06.6-abandon6");
		let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
		let node = db.read(async |t| Ok(t.query_one("SELECT id,pubkey FROM lightning_node WHERE pubkey=$1",
			&[&info.id]).await?)).await.unwrap();
		assert_eq!(node.get::<_, Vec<u8>>("pubkey"), info.id);
		let node_id = node.get::<_, i64>("id");
		assert!(payer.list_channels(cln_rpc::ListchannelsRequest {
			short_channel_id: Some("16777215x0x0".into()), ..Default::default()
		}).await.unwrap().into_inner().channels.is_empty());
		let probe = payer.xpay(cln_rpc::XpayRequest {
			invstring: invoice.to_string(), label: Some("unresolved-introduction-probe".into()),
			amount_msat: None, maxfee: None, layers: vec![], retry_for: Some(1),
			partial_msat: None, maxdelay: None, payer_note: None, localinvreqid: None, dev_use_shadow: None,
		}).await.unwrap_err();
		assert_eq!(probe.code(), tonic::Code::Unknown);
		assert_eq!(probe.metadata().get("abandon-cln-jsonrpc-error-code").unwrap().to_str().unwrap(), "-32602");
		assert!(probe.message().contains("Could not resolve any paths: unknown short_channel_id"));
		assert!(payer.list_send_pays(cln_rpc::ListsendpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().payments.is_empty());
		assert!(payer.list_pays(cln_rpc::ListpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().pays.is_empty());

		let mut returned = srv.subscribe_log::<server_log::XpayRpcReturned>();
		assert!(payer.list_channels(cln_rpc::ListchannelsRequest {
			short_channel_id: Some("16777215x0x0".into()), ..Default::default()
		}).await.unwrap().into_inner().channels.is_empty());
		rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
			invoice: invoice.to_string(), payment_amount_sat: principal.to_sat(),
			htlc_vtxo_ids: vec![sources[0].0.id().to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(1),
		}).await.unwrap();
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let result = returned.recv().await.unwrap();
				if result.payment_hash == hash {
					assert!(result.error.unwrap().contains("Could not resolve any paths: unknown short_channel_id"));
					break;
				}
			}
		}).await.expect("the product dispatch must reach the native unresolved-introduction branch");
		let rejected = tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				if let Some(attempt) = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap() {
					if attempt.status == LightningPaymentStatus::Failed { break attempt; }
				}
				tokio::time::sleep(poll_interval()).await;
			}
		}).await.expect("the exact typed rejection must close its own attempt");
		assert_eq!(rejected.lightning_node_id, node_id);
		assert_eq!(rejected.amount_msat, principal.to_sat() * 1_000);
		assert!(rejected.final_amount_msat.is_none());
		let receipt = db.read(async |t| Ok(t.query_one("SELECT d.label,r.code,f.jsonrpc_error_code, \
			f.created_index,f.updated_index,f.supervised_manifest IS NULL AND f.supervised_manifest_sha256 IS NULL AS unsupervised, \
			EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=d.id) AS success, \
			EXISTS(SELECT 1 FROM expiry_lightning_internal_funding WHERE id=d.id) AS funding \
			FROM expiry_lightning_dispatch d JOIN expiry_lightning_rpc_rejection r ON r.id=d.id \
			JOIN expiry_lightning_terminal_failure f ON f.id=d.id WHERE d.id=$1", &[&rejected.id]).await?)).await.unwrap();
		assert!(!receipt.get::<_, &str>("label").is_empty());
		assert_eq!(receipt.get::<_, i32>("code"), -32602);
		assert_eq!(receipt.get::<_, Option<i32>>("jsonrpc_error_code"), Some(-32602));
		assert!(receipt.get::<_, Option<i64>>("created_index").is_none());
		assert!(receipt.get::<_, Option<i64>>("updated_index").is_none());
		assert!(receipt.get::<_, bool>("unsupervised"));
		assert!(!receipt.get::<_, bool>("success") && !receipt.get::<_, bool>("funding"));
		let links = db.read(async |t| Ok(t.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
			WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&rejected.id]).await?
			.into_iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>())).await.unwrap();
		assert_eq!(links, vec![sources[0].0.id().to_string()]);
		assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
		assert!(payer.list_send_pays(cln_rpc::ListsendpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().payments.is_empty());
		assert!(payer.list_pays(cln_rpc::ListpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().pays.is_empty());

		// Only after the definite rejection, create the same-hash held sibling.
		let invoice = lightning.external.hold_client().await.invoice(hold::InvoiceRequest {
			payment_hash: hash.to_vec(), amount_msat: principal.to_sat() * 1_000,
			description: Some(hold::invoice_request::Description::Memo("same-hash pending source".into())),
			min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
		}).await.unwrap().into_inner().bolt11;
		rpc.initiate_lightning_payment(protos::InitiateLightningPaymentRequest {
			invoice, payment_amount_sat: principal.to_sat(),
			htlc_vtxo_ids: vec![sources[1].0.id().to_bytes().to_vec()], mailbox_id: None, retry_for_secs: Some(1),
		}).await.unwrap();
		lightning.external.wait_for_hold_invoice_accepted(hash).await;
		let pending = db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
		assert_ne!(pending.id, rejected.id);
		assert!(matches!(pending.status, LightningPaymentStatus::Requested | LightningPaymentStatus::Submitted));
		assert!(pending.final_amount_msat.is_none());
		let label = db.read(async |t| Ok(t.query_one("SELECT label FROM expiry_lightning_dispatch WHERE id=$1",
			&[&pending.id]).await?.get::<_, String>(0))).await.unwrap();
		let parts = payer.list_send_pays(cln_rpc::ListsendpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().payments;
		assert!(!parts.is_empty());
		assert!(parts.iter().all(|part| part.label.as_deref() == Some(label.as_str()) && part.payment_hash == hash.to_vec()
			&& part.status() == cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Pending && part.payment_preimage.is_none()));
		assert_eq!(parts.iter().map(|part| part.amount_msat.as_ref().unwrap().msat).sum::<u64>(), principal.to_sat() * 1_000);
		let aggregate = payer.list_pays(cln_rpc::ListpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().pays;
		assert_eq!(aggregate.len(), 1);
		assert_eq!(aggregate[0].label.as_deref(), Some(label.as_str()));
		assert_eq!(aggregate[0].status(), cln_rpc::listpays_pays::ListpaysPaysStatus::Pending);
		assert!(aggregate[0].preimage.is_none());
		for (index, (source, _key)) in sources.iter().enumerate() {
			let status = rpc.check_lightning_payment(protos::CheckLightningPaymentRequest {
				hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.id().to_bytes().to_vec()],
			}).await.unwrap().into_inner();
			assert_eq!(status.htlc_vtxo_ids, vec![source.id().to_bytes().to_vec()]);
			if index == 0 { assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Failed(_)))); }
			else { assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Pending(_)))); }
		}
		for (index, (source, key)) in sources.iter().enumerate() {
			let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([source.clone()],
				VtxoPolicy::new_pubkey(key.public_key())).unwrap().generate_user_nonces(&[*key]).unwrap();
			let response = rpc.request_lightning_pay_htlc_revocation(
				protos::ArkoorPackageCosignRequest::from(builder.cosign_request()),
			).await;
			if index == 0 {
				let internal = builder.build_unsigned_internal_vtxos();
				assert_eq!(internal.len(), 1);
				let txids = builder.virtual_transactions().collect::<Vec<_>>();
				let refund = builder.user_cosign(&[*key], ArkoorPackageCosignResponse::try_from(response.unwrap().into_inner()).unwrap())
					.unwrap().build_signed_vtxos();
				assert_eq!(refund.iter().map(|coin| coin.amount()).sum::<bitcoin::Amount>(), sat(100_000));
				wallet.register_vtxo_transactions_with_server(&refund).await.unwrap();
				let resolved = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, source.id()).await).await.unwrap().unwrap();
				assert_eq!(resolved.htlc.offchain_resolution, Some(server::database::htlc_vtxo::HtlcResolution::Revoked));
				// A different destination requests a second spend. Replaying the
				// identical signed transaction is an ordinary idempotent retry.
				let other_destination = sources[1].1.public_key();
				assert_ne!(other_destination, key.public_key());
				let repeat = ArkoorPackageBuilder::new_claim_all_with_checkpoints([source.clone()],
					VtxoPolicy::new_pubkey(other_destination)).unwrap().generate_user_nonces(&[*key]).unwrap();
				let repeated_internal = repeat.build_unsigned_internal_vtxos();
				assert_eq!(repeated_internal.len(), 1);
				assert_eq!(repeated_internal[0].0.id(), internal[0].0.id());
				assert_ne!(repeated_internal[0].1, internal[0].1);
				let repeated_outputs = repeat.build_unsigned_vtxos().collect::<Vec<_>>();
				assert_eq!(repeated_outputs.len(), 1);
				assert_ne!(repeated_outputs[0].id(), refund[0].id());
				let new_txids = repeat.virtual_transactions().filter(|txid| !txids.contains(txid)).collect::<Vec<_>>();
				assert_eq!(new_txids.len(), 1);
				for txid in &new_txids {
					assert!(db.read(async |t| t.get_virtual_transaction_by_txid(*txid).await).await.unwrap().is_none());
				}
				assert!(db.read(async |t| Ok(t.query_opt("SELECT vtxo_id FROM vtxo WHERE vtxo_id=$1",
					&[&repeated_outputs[0].id().to_string()]).await?.is_none())).await.unwrap());
				let affected_coins = [source.id(), internal[0].0.id(), refund[0].id(), repeated_outputs[0].id()]
					.map(|id| id.to_string()).to_vec();
				let affected_txs = txids.iter().chain(new_txids.iter()).map(ToString::to_string).collect::<Vec<_>>();
				let read_state = async || db.read(async |t| {
					let row = t.query_one("SELECT \
						(SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY v.vtxo_id),'[]'::jsonb)::TEXT FROM vtxo v WHERE v.vtxo_id=ANY($1)) AS coins, \
						(SELECT COALESCE(jsonb_agg(to_jsonb(h) ORDER BY h.id),'[]'::jsonb)::TEXT FROM htlc_vtxo h JOIN vtxo v ON v.id=h.id WHERE v.vtxo_id=ANY($1)) AS htlcs, \
						(SELECT COALESCE(jsonb_agg(to_jsonb(v) ORDER BY v.txid),'[]'::jsonb)::TEXT FROM virtual_transaction v WHERE v.txid=ANY($2)) AS txs, \
						(SELECT COUNT(*) FROM virtual_transaction_history WHERE txid=ANY($2)) AS tx_history",
						&[&affected_coins, &affected_txs]).await?;
					Ok((row.get::<_, String>("coins"), row.get::<_, String>("htlcs"),
						row.get::<_, String>("txs"), row.get::<_, i64>("tx_history")))
				}).await.unwrap();
				let before = read_state().await;
				assert!(srv.config().rpc_rich_errors);
				let refusal = rpc.request_lightning_pay_htlc_revocation(protos::ArkoorPackageCosignRequest::from(repeat.cosign_request()))
					.await.unwrap_err();
				assert_eq!(refusal.code(), tonic::Code::Internal);
				assert_eq!(refusal.message(), format!("tx body error: vtxo {} is already spent", internal[0].0.id()));
				assert_eq!(read_state().await, before);
			} else { assert_eq!(response.unwrap_err().code(), tonic::Code::InvalidArgument); }
		}
		let after = db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
		assert_eq!(after.id, pending.id);
		assert_eq!(after.status, pending.status);
		assert_eq!(after.updated_at, pending.updated_at);
		assert!(after.final_amount_msat.is_none());
		let held = db.read(async |t| server::database::htlc_vtxo::get_htlc_vtxo(t, sources[1].0.id()).await).await.unwrap().unwrap();
		assert!(held.htlc.offchain_resolution.is_none());
		let state = db.read(async |t| Ok(t.query_one("SELECT v.spend_state::TEXT, \
			EXISTS(SELECT 1 FROM expiry_lightning_terminal_failure WHERE id=$1) AS failure, \
			EXISTS(SELECT 1 FROM expiry_lightning_rpc_rejection WHERE id=$1) AS rejection, \
			EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=$1) AS success, \
			EXISTS(SELECT 1 FROM expiry_lightning_internal_funding WHERE id=$1) AS funding, \
			EXISTS(SELECT 1 FROM expiry_entitlement WHERE vtxo_id=ANY($3)) AS entitled \
			FROM vtxo v WHERE v.vtxo_id=$2", &[&pending.id,&sources[1].0.id().to_string(),
				&sources.iter().map(|(source,_)| source.id().to_string()).collect::<Vec<_>>()]).await?)).await.unwrap();
		assert_eq!(state.get::<_, &str>("spend_state"), "spendable");
		for flag in ["failure","rejection","success","funding","entitled"] { assert!(!state.get::<_, bool>(flag)); }
		assert_eq!(payer.list_send_pays(cln_rpc::ListsendpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner().payments, parts);
		assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	}).await.expect("typed rejection/source isolation exceeded the unchanged 180-second limit");
}

/// Public process identity only. Never inspect process memory, environment,
/// private descriptors, node keys or payment preimages.
struct ProcessIdentity {
	pid: i32,
	parent: i32,
	uid: u32,
	start: u64,
	exe: std::path::PathBuf,
}

impl ProcessIdentity {
	fn parse_stat(stat:&str) -> Option<(char,i32,u64)> {
		let fields = stat.rsplit_once(") ")?.1.split_whitespace().collect::<Vec<_>>();
		Some((fields.first()?.chars().next()?,fields.get(1)?.parse().ok()?,fields.get(19)?.parse().ok()?))
	}
	fn stat(pid: i32) -> Result<Option<(char,i32,u64)>,()> {
		match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
			Ok(stat)=>Self::parse_stat(&stat).map(Some).ok_or(()),
			Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),
			Err(_)=>Err(()),
		}
	}
	fn read(pid: i32) -> Self {
		assert!(pid>1);
		let (_,parent,start) = Self::stat(pid).expect("owned process identity must be readable").expect("process must exist");
		let uid = std::fs::metadata(format!("/proc/{pid}")).unwrap().uid();
		assert_eq!(uid,unsafe { libc::geteuid() },"only this native test user's processes are eligible");
		let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
		Self { pid,parent,uid,start,exe }
	}
	fn remains(&self) -> bool {
		match Self::stat(self.pid) {
			Ok(Some((_,_,start)))=>start==self.start,
			Ok(None)=>false,
			Err(())=>panic!("cannot verify process extinction; refuse recovery"),
		}
	}
	fn signal(&self,signal:nix::sys::signal::Signal) {
		let (state,parent,start) = Self::stat(self.pid).expect("process identity is unreadable").expect("owned process vanished before its supervised signal");
		assert!(!matches!(state,'Z'|'X'));
		assert_eq!(parent,self.parent); assert_eq!(start,self.start);
		assert_eq!(std::fs::metadata(format!("/proc/{}",self.pid)).unwrap().uid(),self.uid);
		assert_eq!(std::fs::read_link(format!("/proc/{}/exe",self.pid)).unwrap(),self.exe);
		nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid),signal).unwrap();
	}
}

/// Stop the paused child if an assertion or timeout interrupts the test.
/// Check its PID, start time, parent, UID and executable before the signal.
struct PausedXpay {
	process: ProcessIdentity,
	parent_start: u64,
}

impl Drop for PausedXpay {
	fn drop(&mut self) {
		let p=&self.process;
		if ProcessIdentity::stat(p.pid).is_ok_and(|identity|identity.is_some_and(|(state,parent,start)| {
			let parent_gone=match ProcessIdentity::stat(p.parent) {
				Ok(None)=>true,
				Ok(Some((_,_,epoch)))=>epoch!=self.parent_start,
				Err(())=>false,
			};
			let expected_adoption=parent_gone && (parent==1 || parent==std::process::id() as i32);
			!matches!(state,'Z'|'X') && (parent==p.parent || expected_adoption) && start==p.start
		}))
			&& std::fs::metadata(format!("/proc/{}",p.pid)).is_ok_and(|meta|meta.uid()==p.uid)
			&& std::fs::read_link(format!("/proc/{}/exe",p.pid)).is_ok_and(|exe|exe==p.exe)
		{
			let _=nix::sys::signal::kill(nix::unistd::Pid::from_raw(p.pid),nix::sys::signal::Signal::SIGKILL);
		}
	}
}

fn lightning_process_tree(root:i32) -> Vec<ProcessIdentity> {
	let mut pending=vec![root];
	let mut processes=Vec::new();
	while let Some(pid)=pending.pop() {
		assert!(processes.len()<256,"unexpected fixture process tree; refuse unbounded supervision");
		let process=ProcessIdentity::read(pid);
		let mut children=std::collections::BTreeSet::new();
		for thread in std::fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
			let bytes=std::fs::read_to_string(thread.unwrap().path().join("children")).unwrap();
			children.extend(bytes.split_whitespace().map(|pid|pid.parse::<i32>().unwrap()));
		}
		pending.extend(children);
		processes.push(process);
	}
	processes
}

async fn pause_xpay(
	ctx:&TestContext,node:&ark_testing::daemon::lightningd::Lightningd,
) -> (ProcessIdentity,PausedXpay) {
	assert!(std::env::var_os("LIGHTNINGD_DOCKER_IMAGE").is_none(),"exact native PID supervision does not support Docker CLN");
	let mut rpc=node.grpc_client().await;
	let info=rpc.getinfo(cln_rpc::GetinfoRequest{}).await.unwrap().into_inner();
	assert_eq!(info.network,"regtest"); assert_eq!(info.version,"v26.06.6-abandon6");
	let config=rpc.list_configs(cln_rpc::ListconfigsRequest{config:Some("pid-file".into())}).await.unwrap().into_inner();
	let pid_file=std::path::PathBuf::from(config.configs.unwrap().pid_file.unwrap().value_str);
	assert!(pid_file.is_absolute() && pid_file.starts_with(&ctx.datadir),"node PID file must belong to this context");
	let root=ProcessIdentity::read(std::fs::read_to_string(pid_file).unwrap().trim().parse().unwrap());
	assert_eq!(root.exe.file_name().unwrap(),"lightningd");
	// plugin.c:1986 logs the pipecmd PID and executable. Do not infer a PID
	// from the process name or print the node logs.
	let logs=rpc.get_log(cln_rpc::GetlogRequest{level:Some(cln_rpc::getlog_request::GetlogLevel::Debug as i32)}).await.unwrap().into_inner();
	let mut candidates=Vec::new();
	for entry in logs.log {
		if entry.source.as_deref()!=Some("plugin-manager") { continue; }
		let Some((pid,path))=entry.log.as_deref().and_then(|line|line.strip_prefix("started(")).and_then(|line|line.split_once(") ")) else {continue;};
		let path=std::path::PathBuf::from(path);
		if path.file_name()!=Some(std::ffi::OsStr::new("cln-xpay")) {continue;}
		let process=ProcessIdentity::read(pid.parse().unwrap());
		assert_eq!(process.parent,root.pid); assert_eq!(process.uid,root.uid);
		assert_eq!(process.exe,std::fs::canonicalize(path).unwrap());
		candidates.push(process);
	}
	assert_eq!(candidates.len(),1,"require one unambiguous xpay startup identity");
	let paused=PausedXpay{process:candidates.pop().unwrap(),parent_start:root.start};
	paused.process.signal(nix::sys::signal::Signal::SIGSTOP);
	tokio::time::timeout(Duration::from_secs(5),async {
		loop {
			if matches!(ProcessIdentity::stat(paused.process.pid),Ok(Some(('T',_,start))) if start==paused.process.start) {break;}
			tokio::time::sleep(poll_interval()).await;
		}
	}).await.expect("the xpay process did not enter stopped state");
	(root,paused)
}

async fn wait_for_xpay_request(
	node:&ark_testing::daemon::lightningd::Lightningd,invoice:&str,label:&str,
) {
	tokio::time::timeout(Duration::from_secs(10),async {
		loop {
			// jsonrpc.c:1197 uses LOG_IO_IN for request bytes read by the
			// lightningd. log.c:1175 emits IO_IN; its source is
			// jsonrpc#<fd> (jsonrpc.c:1272), never the reverse response direction.
			let logs=node.grpc_client().await.get_log(cln_rpc::GetlogRequest{
				level:Some(cln_rpc::getlog_request::GetlogLevel::Io as i32),
			}).await.unwrap().into_inner();
			let mut streams=std::collections::BTreeMap::<String,Vec<u8>>::new();
			for entry in logs.log {
				if entry.item_type!=cln_rpc::getlog_log::GetlogLogType::IoIn as i32 {continue;}
				let Some(source)=entry.source.filter(|source|source.starts_with("jsonrpc#")) else {continue;};
				if let Some(data)=entry.data {streams.entry(source).or_default().extend(data);}
			}
			for bytes in streams.values() {
				for request in serde_json::Deserializer::from_slice(bytes).into_iter::<serde_json::Value>() {
					let Ok(request)=request else {break;};
					if request["jsonrpc"]=="2.0" && request["method"]=="xpay" && !request["id"].is_null()
						&& request["params"]["invstring"]==invoice && request["params"]["label"]==label {
						return;
					}
				}
			}
			tokio::time::sleep(poll_interval()).await;
		}
	}).await.expect("the node did not receive the xpay request")
}

/// Keep all recovery inputs alive through the command and remove them on drop.
pub(super) struct RecoveryManifest {
	_dir: tempfile::TempDir,
	path: PathBuf,
}

impl Deref for RecoveryManifest {
	type Target = Path;
	fn deref(&self) -> &Path { &self.path }
}

impl AsRef<Path> for RecoveryManifest {
	fn as_ref(&self) -> &Path { &self.path }
}

/// Call after the supervised shutdown and store reopen.
pub(super) async fn recovery_manifest(
	cfg: &server::Config, db: &server::database::Db, attempt: i64,
) -> RecoveryManifest {
	let root = tempfile::Builder::new().prefix("lightning-recovery-").tempdir().unwrap();
	let fence_path = root.path().join("supervisor.exclusion");
	std::fs::File::create(&fence_path).unwrap();
	let metadata = std::fs::metadata(&fence_path).unwrap();
	let row = db.read(async |tx| Ok(tx.query_one("SELECT a.lightning_node_id,a.payment_hash,a.amount_msat,n.pubkey,d.label \
		FROM lightning_payment_attempt a JOIN lightning_node n ON n.id=a.lightning_node_id \
		JOIN expiry_lightning_dispatch d ON d.id=a.id WHERE a.id=$1", &[&attempt]).await?)).await.unwrap();
	let sources = db.read(async |tx| Ok(tx.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
		WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&attempt]).await?
		.into_iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>())).await.unwrap();
	let supervisor = ProcessIdentity::read(std::process::id().try_into().unwrap());
	let node_id = row.get::<_, i64>("lightning_node_id");
	let binding = serde_json::json!({
		"incident": format!("lightning-attempt-{attempt}"), "operator": format!("uid-{}", supervisor.uid),
		"epoch": format!("pid-{}-start-{}", supervisor.pid, supervisor.start),
		"fence_path": fence_path, "fence_device": metadata.dev(), "fence_inode": metadata.ino(),
		"cln_config_index": 0, "attempt_id": attempt, "node_id": node_id,
		"node_pubkey": bitcoin::secp256k1::PublicKey::from_slice(&row.get::<_, Vec<u8>>("pubkey")).unwrap().to_string(),
		"network": cfg.network, "cln_version": "v26.06.6-abandon6", "payment_hash": row.get::<_, String>("payment_hash"),
		"dispatch_label": row.get::<_, String>("label"), "amount_msat": row.get::<_, i64>("amount_msat"),
		"source_count": sources.len(), "source_set_sha256": sha256::Hash::hash(&serde_json::to_vec(&sources).unwrap()),
	});
	let binding_digest = sha256::Hash::hash(&serde_json::to_vec(&binding).unwrap());
	let mut manifest = serde_json::json!({"version": 1, "binding": binding});
	for (role, events) in [
		("quiescence", vec!["original-producers-terminated", "transport-queues-extinguished", "lightningd-children-terminated"]),
		("history_retention", vec!["original-store-reopened", "no-history-rollback", "no-payment-pruning"]),
		("no_redelivery", vec!["supervisors-fenced", "old-request-redelivery-disabled"]),
	] {
		let records = events.iter().map(|event| serde_json::json!({
			"subject": format!("lightning-node-{node_id}"),
			"record_id": format!("supervisor-{}-node-{node_id}-{event}", supervisor.pid),
			"observed_at": chrono::Utc::now(), "event": event,
		})).collect::<Vec<_>>();
		let bytes = serde_json::to_vec(&serde_json::json!({"binding_sha256": binding_digest, "records": records})).unwrap();
		let path = root.path().join(format!("{role}.json"));
		std::fs::write(&path, &bytes).unwrap();
		manifest[role] = serde_json::json!({"path": path, "sha256": sha256::Hash::hash(&bytes)});
	}
	let path = root.path().join("manifest.json");
	std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
	RecoveryManifest { _dir: root, path }
}

/// Read the producer identities before dispatch for the recovery CLI input.
pub(super) async fn producer_qualification(
	ctx: &TestContext, srv: &ark_testing::daemon::captaind::Captaind,
	node: &ark_testing::daemon::lightningd::Lightningd,
) -> (serde_json::Value, i32, u64) {
	assert!(std::env::var_os("LIGHTNINGD_DOCKER_IMAGE").is_none());
	let mut previous = node.grpc_client().await;
	let config = previous.list_configs(cln_rpc::ListconfigsRequest {
		config: Some("pid-file".into()),
	}).await.unwrap().into_inner();
	let pid_file = std::path::PathBuf::from(config.configs.unwrap().pid_file.unwrap().value_str);
	assert!(pid_file.is_absolute() && pid_file.starts_with(&ctx.datadir));
	let root = ProcessIdentity::read(std::fs::read_to_string(pid_file).unwrap().trim().parse().unwrap());
	assert_eq!(root.exe.file_name().unwrap(), "lightningd");
	let tree = lightning_process_tree(root.pid);
	let pin = |name: &str| {
		let processes = tree.iter().filter(|process| process.exe.file_name() == Some(std::ffi::OsStr::new(name)))
			.collect::<Vec<_>>();
		assert_eq!(processes.len(), 1, "require one plugin process");
		let process = processes[0];
		assert_eq!(process.parent, root.pid); assert!(process.remains());
		serde_json::json!({"binary_sha256": sha256::Hash::hash(&std::fs::read(&process.exe).unwrap()),
			"observed_at": chrono::Utc::now(),
			"record_id": format!("{}-pid-{}-start-{}", name, process.pid, process.start)})
	};
	let dispatcher = srv.get_custom_command(&[]).await.unwrap();
	let dispatcher_binary = std::fs::canonicalize(dispatcher.as_std().get_program()).unwrap();
	let dispatcher_source = sha256::Hash::hash(include_bytes!("../../../server/src/ln/cln/xpay.rs"));
	let mut grpc = pin("cln-grpc");
	grpc["server_sha256"] = serde_json::json!("765b38372a479ce29900ac243ad2fb0cc339fc5907b8a1451084e22b405dbbcc");
	grpc["conversion_sha256"] = serde_json::json!("d4c01103a076cb8d1ad90c38940f0b9aef180aabca9e15c09eeaf51fd8535d60");
	grpc["model_sha256"] = serde_json::json!("36538a5e672b3fb0409cfb5b6424d0b71880d316cb3709b99354163183652cd8");
	let mut xpay = pin("cln-xpay");
	xpay["source_sha256"] = serde_json::json!("e65b19cef028b49acd3a8824dacf118206ed11074b2e72997074e3fabcd43d1c");
	let qualification = serde_json::json!({
		"original_epoch": format!("lightningd-pid-{}-start-{}", root.pid, root.start),
		"label_contract": "fork-native-xpay-label-abandon6-v1",
		"dispatcher": {"source_sha256": dispatcher_source,
			"binary_sha256": sha256::Hash::hash(&std::fs::read(dispatcher_binary).unwrap()),
			"observed_at": chrono::Utc::now(), "record_id": "captaind-before-dispatch"},
		"grpc": grpc, "xpay": xpay,
	});
	(qualification, root.pid, root.start)
}

pub(super) async fn add_producer_qualification(
	manifest: &std::path::Path, db: &server::database::Db, attempt: i64,
	mut qualification: serde_json::Value,
) {
	let mut body: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
	qualification["binding_sha256"] = serde_json::json!(sha256::Hash::hash(&serde_json::to_vec(&body["binding"]).unwrap()));
	let created = db.read(async |tx| Ok(tx.query_one("SELECT created_at FROM expiry_lightning_dispatch WHERE id=$1", &[&attempt])
		.await?.get::<_, chrono::DateTime<chrono::Utc>>(0))).await.unwrap();
	qualification["dispatch_created_at"] = serde_json::json!(created);
	let bytes = serde_json::to_vec(&qualification).unwrap();
	let path = manifest.with_file_name("producer.json");
	std::fs::write(&path, &bytes).unwrap();
	body["original_labelled_producer"] = serde_json::json!({"path": path, "sha256": sha256::Hash::hash(&bytes)});
	std::fs::write(manifest, serde_json::to_vec(&body).unwrap()).unwrap();
}

/// Recover an unsent dispatch after shutdown and store reopen.
/// Its signed source must receive one confirmed fallback payout.
#[tokio::test]
async fn recovery_refunds_unsent_dispatch_once() {
	assert_dispatch_recovery(false, false, false).await;
}

/// Stop xpay before CLN receives the dispatch, then recover without replay.
#[tokio::test]
async fn recovery_refunds_queued_dispatch_once() {
	assert_dispatch_recovery(true, false, false).await;
}

/// A foreign payment with the same hash cannot discharge an unsent source.
#[tokio::test]
async fn recovery_refunds_unsent_source_after_foreign_payment() {
	assert_dispatch_recovery(false, true, false).await;
}

/// Refuse recovery while a payment remains pending after store reopen.
/// A subsequent backend failure must release the source for one payout.
#[tokio::test]
async fn recovery_refuses_pending_payment_until_failure() {
	assert_dispatch_recovery(false, false, true).await;
}

async fn assert_dispatch_recovery(queued_request: bool, foreign_complete: bool, own_pending: bool) {
	tokio::time::timeout(Duration::from_secs(180), async {
		let ctx = TestContext::new(if queued_request {"server/recovery_refunds_queued_dispatch_once"}
			else if foreign_complete {"server/recovery_refunds_unsent_source_after_foreign_payment"}
			else if own_pending {"server/recovery_refuses_pending_payment_until_failure"}
			else {"server/recovery_refunds_unsent_dispatch_once"}).await;
		let lightning = ctx.new_lightning_setup("lightningd").await;
		let srv = ctx.captaind("server").lightningd(&lightning.internal).no_vtxo_pool().funded(btc(10))
			.cfg(move |cfg| {
				cfg.vtxo_lifetime=BlockDelta::new(24); cfg.vtxo_exit_delta=BlockDelta::new(3);
				cfg.htlc_send_expiry_delta=BlockDelta::new(40);
				// Keep the normal monitor enabled; the foreign case must really
				// observe its coarse outcome without direct status mutation.
				if foreign_complete || own_pending {cfg.invoice_check_interval=Duration::from_secs(1);}
			})
			.watchmand().create().await;
		let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
		let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
		let client = sender.client().await;
		let destination = client.fallback_destination().await.unwrap();
		let input = client.get_full_vtxo(ids[0]).await.unwrap();
		let key = client.get_vtxo_key(&input).await.unwrap();
		let invoice = if own_pending {
			lightning.external.hold_client().await.invoice(hold::InvoiceRequest {
				payment_hash: Preimage::from_slice(&[39;32]).unwrap().compute_payment_hash().to_vec(),
				amount_msat: 100_000_000,
				description: Some(hold::invoice_request::Description::Memo("own-pending-intact-reopen".into())),
				min_final_cltv_expiry: Some(30), expiry: Some(3600), routing_hints: vec![],
			}).await.unwrap().into_inner().bolt11
		} else {
			lightning.external.invoice_with_preimage(Some(sat(100_000)), "supervised-never-injected", "supervised pre-RPC", [39;32]).await
		};
		let invoice = Invoice::Bolt11(invoice.parse().unwrap());
		let hash = invoice.payment_hash();
		let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
		let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()],
			VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline)).unwrap()
			.generate_user_nonces(&[key]).unwrap();
		let response = srv.get_public_rpc().await.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
			parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
		}).await.unwrap().into_inner();
		let response = ark::arkoor::package::ArkoorPackageCosignResponse::try_from(response).unwrap();
		let grants = builder.user_cosign(&[key], response).unwrap().build_signed_vtxos();
		client.register_vtxo_transactions_with_server(&grants).await.unwrap();
		let source = grants[0].id();
		let cfg = srv.config().clone();
		let operator_script = {
			let watch = srv.watchmand().config();
			watch.sweep_address.clone().unwrap().require_network(watch.network).unwrap().script_pubkey()
		};
		let db = server::database::Db::connect(&cfg.postgres).await.unwrap();
		let node_id = db.read(async |tx| Ok(tx.query_one("SELECT id FROM lightning_node ORDER BY id LIMIT 1", &[]).await?.get::<_, i64>(0))).await.unwrap();
		// The recovery input identifies the producer before dispatch starts.
		let mut dispatch_node = None;
		let mut paused_xpay = None;
		let mut dispatch_tree = Vec::new();
		let mut dispatch_qualification = None;
		let mut accepted_parts = None;
		let mut accepted_pays = None;
		let foreign_label = "independent-operator-funded-foreign-payment";
		if foreign_complete || own_pending {
			let (qualification, pid, start) = producer_qualification(&ctx, &srv, &lightning.internal).await;
			let root = ProcessIdentity::read(pid);
			assert_eq!(root.start, start);
			dispatch_qualification = Some(qualification);
			dispatch_node = Some(root);
		}
		let attempt=if queued_request || own_pending {
			lightning.sync().await;
			if queued_request {
				let (root,paused)=pause_xpay(&ctx,&lightning.internal).await;
				dispatch_node=Some(root); paused_xpay=Some(paused);
			}
			// ONE public initiation; never call xpay from the test or
			// resubmit an invoice/label to manufacture a result.
			srv.get_public_rpc().await.initiate_lightning_payment(protos::InitiateLightningPaymentRequest{
				invoice:invoice.to_string(),payment_amount_sat:100_000,
				htlc_vtxo_ids:vec![source.to_bytes().to_vec()],mailbox_id:None,retry_for_secs:Some(1),
			}).await.unwrap();
			let attempt=tokio::time::timeout(Duration::from_secs(10),async {
				loop {
					if let Some(attempt)=db.read(async |tx|tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap() {
						let dispatched=db.read(async |tx|Ok(tx.query_opt("SELECT id FROM expiry_lightning_dispatch WHERE id=$1",&[&attempt.id]).await?.is_some())).await.unwrap();
						if dispatched {break attempt;}
					}
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("the one previous native payment must acquire its durable dispatch");
			let label=db.read(async |tx|Ok(tx.query_one("SELECT label FROM expiry_lightning_dispatch WHERE id=$1",&[&attempt.id]).await?.get::<_,String>(0))).await.unwrap();
			if queued_request {
				wait_for_xpay_request(&lightning.internal,&invoice.to_string(),&label).await;
				let paused=&paused_xpay.as_ref().unwrap().process;
				assert!(matches!(ProcessIdentity::stat(paused.pid),Ok(Some(('T',parent,start))) if parent==paused.parent && start==paused.start));
			} else {
				lightning.external.wait_for_hold_invoice_accepted(hash).await;
			}
			let mut previous=lightning.internal.grpc_client().await;
			let live=previous.getinfo(cln_rpc::GetinfoRequest{}).await.unwrap().into_inner();
			let key=db.read(async |tx|Ok(tx.query_one("SELECT pubkey FROM lightning_node WHERE id=$1",&[&attempt.lightning_node_id]).await?.get::<_,Vec<u8>>(0))).await.unwrap();
			assert_eq!(live.id,key);
			let history=previous.list_send_pays(cln_rpc::ListsendpaysRequest{payment_hash:Some(hash.to_vec()),..Default::default()}).await.unwrap().into_inner();
			if own_pending {
				assert!(!history.payments.is_empty());
				assert!(history.payments.iter().all(|pay| pay.payment_hash == hash.to_vec()
					&& pay.label.as_deref() == Some(label.as_str()) && pay.payment_preimage.is_none()
					&& pay.status() == cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Pending));
				assert_eq!(history.payments.iter().map(|pay| pay.amount_msat.as_ref().unwrap().msat).sum::<u64>(), 100_000_000);
				let aggregate = previous.list_pays(cln_rpc::ListpaysRequest {
					payment_hash: Some(hash.to_vec()), ..Default::default()
				}).await.unwrap().into_inner();
				assert_eq!(aggregate.pays.len(), 1);
				assert!(aggregate.pays.iter().all(|pay| pay.payment_hash == hash.to_vec()
					&& pay.label.as_deref() == Some(label.as_str()) && pay.preimage.is_none()
					&& pay.status() == cln_rpc::listpays_pays::ListpaysPaysStatus::Pending));
				assert_eq!(aggregate.pays[0].amount_msat.as_ref().unwrap().msat, 100_000_000);
				let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
					hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.to_bytes().to_vec()],
				}).await.unwrap().into_inner();
				assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Pending(_))));
				assert_eq!(status.htlc_vtxo_ids, vec![source.to_bytes().to_vec()]);
				accepted_parts = Some(history.payments);
				accepted_pays = Some(aggregate.pays);
			} else {
				assert!(history.payments.is_empty(),"a remotely received previous request must still have zero injected groups while xpay is stopped");
			}
			attempt
		} else {
			let attempt=db.write(async |tx| {
				tx.store_lightning_payment_start(node_id,&invoice,sat(100_000),None,&[source],None,tip,sat(0),None,Duration::from_secs(1), None).await?;
				Ok(tx.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.unwrap())
			}).await.unwrap();
			assert!(db.write(async |tx|tx.admit_lightning_dispatch(&attempt).await).await.unwrap());
			attempt
		};
		let (sync_done, mut sync_done_rx) = tokio::sync::mpsc::unbounded_channel();
		if foreign_complete {
			let prefix = format!("Lightning payment attempt ({}): Check ", attempt.id);
			srv.add_stdout_handler(move |line: &str| {
				if let Ok(record) = server_log::parse_record(line) {
					if record.message.starts_with(&prefix) && record.message.contains(" done, updated next check to ") {
						let _ = sync_done.send(record.timestamp);
					}
				}
				false
			});
			// The operator channel pays this real external invoice; no
			// Bark attempt/source is linked to that foreign payment. Never fake
			// a historical row, outcome, preimage or remote error.
			lightning.sync().await;
			let response = lightning.internal.grpc_client().await.xpay(cln_rpc::XpayRequest {
				invstring: invoice.to_string(), label: Some(foreign_label.into()),
				amount_msat: None, maxfee: None, layers: vec![], retry_for: Some(1),
				partial_msat: None, maxdelay: None, payer_note: None, localinvreqid: None, dev_use_shadow: None,
			}).await.unwrap().into_inner();
			assert_eq!(Preimage::from_slice(&response.payment_preimage).unwrap().compute_payment_hash(), hash);
			lightning.external.wait_invoice_paid("supervised-never-injected").await;
		}
		if foreign_complete {
			// No direct monitor call or pause: wait for the normal server loop
			// to retain the foreign preimage without closing the original dispatch.
			tokio::time::timeout(Duration::from_secs(35), async {
				loop {
					let recorded = db.read(async |tx| Ok(tx.query_opt(
						"SELECT created_at FROM htlc_settlement WHERE payment_hash=$1", &[&hash.to_string()],
					).await?.map(|row| row.get::<_, chrono::DateTime<chrono::Local>>(0)))).await.unwrap();
					if let Some(recorded) = recorded {
						// WAL retention precedes reconciliation. Observe the existing
						// end-of-check trace before asserting the resulting fields.
						while sync_done_rx.recv().await.unwrap() < recorded {}
						let observed = db.read(async |tx| tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
						assert_eq!(observed.id, attempt.id);
						assert!(matches!(observed.status, LightningPaymentStatus::Requested | LightningPaymentStatus::Submitted));
						assert!(observed.final_amount_msat.is_none());
						let source_state = db.read(async |tx| Ok(tx.query_one("SELECT v.spend_state::TEXT,h.offchain_resolution::TEXT, \
							EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=$1) AS success, \
							EXISTS(SELECT 1 FROM expiry_lightning_internal_funding WHERE id=$1) AS funding, \
							EXISTS(SELECT 1 FROM expiry_lightning_terminal_failure WHERE id=$1) AS failed, \
							EXISTS(SELECT 1 FROM mailbox WHERE payment_hash=$3) AS notified \
							FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=$2", &[&attempt.id,&source.to_string(),&hash.to_string()]).await?)).await.unwrap();
						assert_eq!(source_state.get::<_, &str>("spend_state"), "spendable",
							"foreign payment success must not consume the untouched own source");
						assert!(source_state.get::<_, Option<&str>>("offchain_resolution").is_none());
						assert!(!source_state.get::<_, bool>("success") && !source_state.get::<_, bool>("funding"));
						assert!(!source_state.get::<_, bool>("failed"));
						assert!(!source_state.get::<_, bool>("notified"));
						break;
					}
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("normal monitor must observe the foreign Complete before recovery");
		}
		// The pre-call component never submitted RPC. The separate queued case
		// instead has real previous-node IO admission and a paused previous
		// producer. In BOTH cases extinguish/reap before zero-history import.
		drop(client);
		srv.watchmand().stop().await.unwrap();
		srv.stop().await.unwrap();
		if let Some(paused)=&paused_xpay {
			let root=dispatch_node.as_ref().unwrap();
			assert!(root.remains());
			dispatch_tree=lightning_process_tree(root.pid);
			assert!(dispatch_tree.iter().any(|p|p.pid==paused.process.pid && p.start==paused.process.start));
			assert!(matches!(ProcessIdentity::stat(paused.process.pid),Ok(Some(('T',_,start))) if start==paused.process.start));
			paused.process.signal(nix::sys::signal::Signal::SIGKILL); // NEVER SIGCONT
		}
		if foreign_complete || own_pending {
			let root = dispatch_node.as_ref().unwrap();
			assert!(root.remains());
			dispatch_tree = lightning_process_tree(root.pid);
		}
		lightning.internal.stop().await.unwrap();
		if queued_request || foreign_complete || own_pending {
			tokio::time::timeout(Duration::from_secs(10),async {
				while dispatch_tree.iter().any(ProcessIdentity::remains) {tokio::time::sleep(poll_interval()).await;}
			}).await.expect("every captured previous CLN/plugin/channel process must be extinct and reaped");
		}
		drop(paused_xpay);
		lightning.internal.start().await.unwrap();
		// Restart reserves new ports, while identity, store and certificate
		// paths remain the same. Both the importer and restarted server must
		// address the reopened node.
		let grpc = lightning.internal.grpc_details().await;
		let hold = lightning.internal.hold_details().await;
		{
			let mut config = srv.config_mut();
			let node = &mut config.cln_array[0];
			assert_eq!(node.server_cert_path,grpc.server_cert_path);
			assert_eq!(node.client_cert_path,grpc.client_cert_path);
			assert_eq!(node.client_key_path,grpc.client_key_path);
			node.uri = grpc.uri.parse().unwrap();
			let plugin = node.hold_invoice.as_mut().unwrap();
			assert_eq!(plugin.server_cert_path,hold.server_cert_path);
			assert_eq!(plugin.client_cert_path,hold.client_cert_path);
			assert_eq!(plugin.client_key_path,hold.client_key_path);
			plugin.uri = hold.uri.parse().unwrap();
		}
		let cfg = srv.config().clone();
		// The CLI reads its endpoints from disk while the server is stopped.
		let config_path = cfg.data_dir.join(ark_testing::daemon::captaind::CAPTAIND_CONFIG_FILE);
		cfg.write_into(&mut std::fs::File::create(config_path).unwrap()).unwrap();
		let previous_status=db.read(async |tx|tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status;
		let manifest = recovery_manifest(&cfg, &db, attempt.id).await;
		if let Some(qualification) = dispatch_qualification {
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.is_err(),
				"nonempty history cannot pass the unqualified whole-hash-empty proof");
			add_producer_qualification(&manifest, &db, attempt.id, qualification).await;
		}
		let manifest_arg = manifest.to_str().unwrap();
		let previous: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
		if own_pending {
			let mut reopened = lightning.internal.grpc_client().await;
			let raw = reopened.list_send_pays(cln_rpc::ListsendpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			let aggregate = reopened.list_pays(cln_rpc::ListpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			// Exact rows, including labels/group/part/index, survive.
			// Do not print payment records or any possible private preimage.
			assert!(raw.payments == *accepted_parts.as_ref().unwrap(), "intact previous raw Pending history changed");
			assert!(aggregate.pays == *accepted_pays.as_ref().unwrap(), "intact previous aggregate Pending history changed");
			let error = server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.unwrap_err();
			assert!(format!("{error:#}").contains("original dispatch has own payment history"), "{error:#}");
			let unchanged = db.read(async |tx| Ok(tx.query_one("SELECT \
				EXISTS(SELECT 1 FROM expiry_lightning_terminal_failure WHERE id=$1) AS failure, \
				EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=$1) AS success, \
				EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$2) AS wal, \
				EXISTS(SELECT 1 FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
					WHERE e.vtxo_id=$3 AND c.active) AS allocated", &[&attempt.id,&hash.to_string(),&source.to_string()]).await?)).await.unwrap();
			for field in ["failure", "success", "wal", "allocated"] { assert!(!unchanged.get::<_, bool>(field)); }
		} else {
			let bytes = std::fs::read(&manifest).unwrap();
			let mut wrong: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
			wrong["binding"]["source_count"] = serde_json::json!(2);
			let wrong_path = manifest.with_file_name("wrong-sources.json");
			std::fs::write(&wrong_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &wrong_path).await.is_err());
			let artifact_path = previous["quiescence"]["path"].as_str().unwrap();
			let artifact_bytes = std::fs::read(artifact_path).unwrap();
			let mut changed = artifact_bytes.clone(); changed.push(b' ');
			std::fs::write(artifact_path,changed).unwrap();
			assert!(server::ln::resolve_uninjected_lightning(&cfg,&manifest).await.is_err(),"changed retained artifact bytes must refuse");
			std::fs::write(artifact_path,artifact_bytes).unwrap();
			let fence_path = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["binding"]["fence_path"].as_str().unwrap().to_owned();
			let held = std::fs::OpenOptions::new().read(true).write(true).open(fence_path).unwrap();
			held.try_lock().unwrap();
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.is_err(), "a supervisor-held competing exclusion cannot be ignored");
			drop(held);
			let mut missing_ack = srv.get_custom_command(&[
				"resolve-uninjected-lightning", "--manifest", manifest_arg,
			]).await.unwrap();
			missing_ack.kill_on_drop(true);
			let refused = missing_ack.output().await.unwrap();
			assert!(!refused.status.success(), "the privileged CLI must require explicit acknowledgment");
			assert!(String::from_utf8_lossy(&refused.stderr).contains("requires --acknowledge-trusted-operations"));
			assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_lightning_terminal_failure WHERE id=$1",
				&[&attempt.id]).await?.get::<_,i64>(0))).await.unwrap(),0,"acknowledgment refusal cannot import a receipt");
			let mut import = srv.get_custom_command(&[
				"resolve-uninjected-lightning", "--manifest", manifest_arg, "--acknowledge-trusted-operations",
			]).await.unwrap();
			import.kill_on_drop(true);
			let imported = import.output().await.unwrap();
			assert!(imported.status.success(), "CLI import failed: {}",String::from_utf8_lossy(&imported.stderr));
			assert!(!server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.unwrap());
			assert_eq!(db.read(async |tx| tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status, previous_status);
		}
		srv.start().await.unwrap(); srv.watchmand().start().await.unwrap();
		if own_pending {
			lightning.internal.connect(&lightning.external).await;
			lightning.sync().await;
			lightning.external.wait_for_hold_invoice_accepted(hash).await;
			// The source's real backing expires while its own HTLC is
			// still held. The new invoice's final CLTV30 fits maxdelay36 and
			// exceeds this root's remaining lifetime; do not mine to its timeout.
			let held_height = input.expiry_height() + BlockDelta::new(1);
			let current = ctx.bitcoind().get_block_count().await as u32;
			assert!(held_height < deadline);
			let held_height = ctx.generate_blocks(held_height.to_u32().checked_sub(current).unwrap()).await;
			srv.wait_for_sync_height(held_height).await; srv.watchmand().wait_for_sync_height(held_height).await;
			let held_hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(held_height.to_u32())).unwrap();
			tokio::time::timeout(Duration::from_secs(30), async {
				loop {
					if db.read(async |tx| {
						let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
						Ok(view.captaind.height == held_height && view.watchmand.height == held_height
							&& view.captaind.hash == held_hash && view.watchmand.hash == held_hash
							&& tx.expiry_view_is_rebuilt(view).await?)
					}).await.unwrap() { break; }
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("expired Pending sample requires the completed rebuilt canonical view");
			lightning.sync().await;
			lightning.external.wait_for_hold_invoice_accepted(hash).await;
			let mut previous = lightning.internal.grpc_client().await;
			let raw = previous.list_send_pays(cln_rpc::ListsendpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			let aggregate = previous.list_pays(cln_rpc::ListpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			assert!(raw.payments == *accepted_parts.as_ref().unwrap(), "backing expiry changed previous Pending parts");
			assert!(aggregate.pays == *accepted_pays.as_ref().unwrap(), "backing expiry changed previous Pending aggregate");
			assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=$1 AND c.active",
				&[&source.to_string()]).await?.get::<_, i64>(0))).await.unwrap(), 0,
				"expired backing with an own Pending cannot authorize any payout");
			let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
				hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.to_bytes().to_vec()],
			}).await.unwrap().into_inner();
			assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Pending(_))));
			assert_eq!(status.htlc_vtxo_ids, vec![source.to_bytes().to_vec()]);
			assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
			// Only the receiver changes the held HTLC outcome.
			lightning.external.hold_client().await.cancel(hold::CancelRequest { payment_hash: hash.to_vec() }).await.unwrap();
			tokio::time::timeout(Duration::from_secs(30), async {
				loop {
					let proof = db.read(async |tx| Ok(tx.query_opt("SELECT created_index,jsonrpc_error_code,supervised_manifest_sha256 \
						FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt.id]).await?)).await.unwrap();
					if let Some(proof) = proof {
						assert!(proof.get::<_, Option<i64>>("created_index").is_some());
						assert!(proof.get::<_, Option<i32>>("jsonrpc_error_code").is_none());
						assert!(proof.get::<_, Option<Vec<u8>>>("supervised_manifest_sha256").is_none());
						break;
					}
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("previous Pending must reconcile its backend cancellation");
		}
		let fee_address = srv.watchmand().wait_wallet_address().await;
		let mut sponsors = std::collections::BTreeSet::new();
		for _ in 0..2 {
			let funding_id = ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
			let funding = ctx.bitcoind().await_transaction(funding_id).await;
			let outputs = funding.output.iter().enumerate().filter(|(_,output)|
				output.script_pubkey==fee_address.script_pubkey() && output.value==sat(100_000)).collect::<Vec<_>>();
			assert_eq!(outputs.len(),1,"retain the exact sponsor output from each funding transaction");
			sponsors.insert(bitcoin::OutPoint::new(funding_id,outputs[0].0 as u32));
		}
		assert_eq!(sponsors.len(),2);
		ctx.generate_blocks(2).await;
		let height = ctx.bitcoind().get_block_count().await as u32;
		let expired = ctx.generate_blocks(deadline.to_u32()+1-height).await;
		srv.wait_for_sync_height(expired).await; srv.watchmand().wait_for_sync_height(expired).await;
		let parent = tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let row = db.read(async |tx| Ok(tx.query_opt("SELECT a.sweep_tx,f.raw_tx FROM expiry_allocation a \
					JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
					JOIN expiry_fee_child f ON f.allocation_id=a.id \
					WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL \
					ORDER BY f.id DESC LIMIT 1", &[&source.to_string()]).await?)).await.unwrap();
				if let Some(row) = row {
					let parent = bitcoin::consensus::deserialize::<bitcoin::Transaction>(row.get("sweep_tx")).unwrap();
					let child = bitcoin::consensus::deserialize::<bitcoin::Transaction>(row.get("raw_tx")).unwrap();
					// Publication is local to the publisher's Core. The miner
					// must know both exact transactions before its six-block batch.
					let core = ctx.bitcoind().sync_client();
					if let (Some(published_parent), Some(published_child)) = (
						core.custom_get_raw_transaction_info(parent.compute_txid(), None).unwrap(),
						core.custom_get_raw_transaction_info(child.compute_txid(), None).unwrap(),
					) {
						assert_eq!(published_parent.hex, bitcoin::consensus::serialize(&parent));
						assert_eq!(published_child.hex, bitcoin::consensus::serialize(&child));
						break parent;
					}
				}
				tokio::time::sleep(poll_interval()).await;
			}
		}).await.expect("supervised receipt must lead to a lawful fallback");
		let payouts = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
		assert_eq!(payouts.len(), 1); let (vout, payout) = payouts[0]; assert_eq!(payout.value, sat(99_000));
		let signed_path = std::iter::once(grants[0].chain_anchor()).chain(grants[0].transactions()
			.map(|step|bitcoin::OutPoint::new(step.tx.compute_txid(),step.output_idx as u32))).collect::<std::collections::BTreeSet<_>>();
		assert_eq!(parent.input.len(),1,"no_pool one-source payout cannot absorb unrelated roots");
		assert!(signed_path.contains(&parent.input[0].previous_output),"the only payout input belongs to the real signed source path");
		let backing = ctx.bitcoind().await_transaction(parent.input[0].previous_output.txid).await;
		assert_eq!(backing.output[parent.input[0].previous_output.vout as usize].value,sat(100_000));
		assert_eq!(parent.version.0,3);
		assert_eq!(parent.output.len(),3);
		assert_eq!(parent.output.iter().map(|output|output.value).sum::<bitcoin::Amount>(),sat(100_000),"payout parent has zero miner fee");
		let operator_outputs = parent.output.iter().filter(|output|output.script_pubkey==operator_script).collect::<Vec<_>>();
		assert_eq!(operator_outputs.len(),1); assert_eq!(operator_outputs[0].value,sat(1_000));
		let anchors = parent.output.iter().enumerate().filter(|(_,output)|output.is_p2a_fee_anchor()).collect::<Vec<_>>();
		assert_eq!(anchors.len(),1); assert_eq!(anchors[0].1.value,sat(0));
		let anchor = bitcoin::OutPoint::new(parent.compute_txid(),anchors[0].0 as u32);
		assert!(parent.output.iter().all(|output|output.script_pubkey==destination.spk || output.script_pubkey==operator_script || output.is_p2a_fee_anchor()),
			"every parent output pays the rightful user, legitimate configured operator, or zero anchor");
		let confirmed = ctx.generate_blocks(6).await;
		srv.wait_for_sync_height(confirmed).await; srv.watchmand().wait_for_sync_height(confirmed).await;
		let utxo = ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), vout as u32, Some(true)).unwrap().unwrap();
		assert!(utxo.confirmations >= 6); assert_eq!(utxo.value, sat(99_000));
		assert_eq!(utxo.script_pub_key.script().unwrap(), destination.spk);
		let chain = ctx.bitcoind().sync_client();
		let parent_info = chain.custom_get_raw_transaction_info(parent.compute_txid(), None).unwrap().unwrap();
		let block = chain.get_block(&parent_info.blockhash.unwrap()).unwrap();
		let children = block.txdata.iter().filter(|tx| tx.input.iter().any(|input|
			input.previous_output.txid == parent.compute_txid())).collect::<Vec<_>>();
		assert_eq!(children.len(),1,"payout must include its confirmed funded fee child");
		let child = children[0];
		assert_eq!(child.version.0,3);
		assert_eq!(child.input.iter().filter(|input|input.previous_output==anchor).count(),1);
		assert!(child.input.len()>1);
		for input in &child.input {
			if input.previous_output==anchor { continue; }
			assert!(sponsors.contains(&input.previous_output),"fee child may use only the two exact legitimate100k sponsor outputs");
			let funding = chain.custom_get_raw_transaction_info(input.previous_output.txid,None).unwrap().unwrap();
			let funding_height = chain.get_block_header_info(&funding.blockhash.unwrap()).unwrap().height;
			let payout_height = chain.get_block_header_info(&parent_info.blockhash.unwrap()).unwrap().height;
			assert!(funding_height<payout_height,"sponsor must be confirmed before payout construction");
		}
		let child_info = chain.custom_get_raw_transaction_info(child.compute_txid(), None).unwrap().unwrap();
		assert_eq!(child_info.blockhash,parent_info.blockhash); assert!(child_info.confirmations.unwrap()>=6);
		let mut boundary = std::collections::BTreeSet::new();
		let mut input_total = sat(0);
		for tx in [&parent, child] {
			for input in &tx.input {
				if input.previous_output.txid == parent.compute_txid() { continue; }
				assert!(boundary.insert(input.previous_output),"no repeated canonical input in the payout package");
				let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
				input_total += previous.output[input.previous_output.vout as usize].value;
			}
		}
		let mut terminal_total = sat(0);
		let consumed = child.input.iter().map(|input| input.previous_output).collect::<std::collections::BTreeSet<_>>();
		for (index, output) in parent.output.iter().enumerate() {
			if !consumed.contains(&bitcoin::OutPoint::new(parent.compute_txid(),index as u32)) { terminal_total += output.value; }
		}
		terminal_total += child.output.iter().map(|output|output.value).sum::<bitcoin::Amount>();
		let mut child_inputs = sat(0);
		for input in &child.input {
			let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
			child_inputs += previous.output[input.previous_output.vout as usize].value;
		}
		let child_fee = child_inputs-child.output.iter().map(|output|output.value).sum::<bitcoin::Amount>();
		assert!(child_fee>sat(0));
		assert_eq!(input_total,terminal_total+child_fee,"zero-fee parent plus sponsor cut must conserve satoshis");
		let recorded = db.read(async |tx| Ok(tx.query_one("SELECT f.raw_tx,f.fee_sat FROM expiry_fee_child f \
			JOIN expiry_allocation a ON a.id=f.allocation_id WHERE a.sweep_txid=$1 AND f.txid=$2",
			&[&parent.compute_txid().to_string(),&child.compute_txid().to_string()]).await?)).await.unwrap();
		assert_eq!(bitcoin::consensus::deserialize::<bitcoin::Transaction>(recorded.get("raw_tx")).unwrap(),*child);
		assert_eq!(u64::try_from(recorded.get::<_,i64>("fee_sat")).unwrap(),child_fee.to_sat());
		let admin = srv.watchmand().config().admin_address.unwrap();
		let mut wallet = ark_testing::daemon::captaind::WalletAdminClient::connect(format!("http://{admin}")).await.unwrap();
		tokio::time::timeout(Duration::from_secs(10),async {
			loop {
				let owned = wallet.wallet_status(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap().confirmed_utxos
					.into_iter().map(|point|point.parse::<bitcoin::OutPoint>().unwrap()).collect::<std::collections::BTreeSet<_>>();
				if child.output.iter().enumerate().all(|(index,output)| output.value==sat(0)
					|| owned.contains(&bitcoin::OutPoint::new(child.compute_txid(),index as u32))) { break; }
				tokio::time::sleep(poll_interval()).await;
			}
		}).await.expect("every real fee-child change output belongs to the watchman");
		for (index,output) in child.output.iter().enumerate().filter(|(_,output)|output.value>sat(0)) {
			let utxo = chain.get_tx_out(&child.compute_txid(),index as u32,Some(false)).unwrap().unwrap();
			assert_eq!(utxo.value,output.value); assert_eq!(utxo.script_pub_key.script().unwrap(),output.script_pubkey);
		}
		let mut known_payouts = std::collections::BTreeSet::new();
		for height in 0..=chain.get_block_count().unwrap() {
			for tx in chain.get_block(&chain.get_block_hash(height).unwrap()).unwrap().txdata {
				for (index,output) in tx.output.iter().enumerate().filter(|(_,output)|output.script_pubkey==destination.spk) {
					known_payouts.insert((bitcoin::OutPoint::new(tx.compute_txid(),index as u32),output.value));
				}
			}
		}
		assert_eq!(known_payouts,std::collections::BTreeSet::from([(bitcoin::OutPoint::new(parent.compute_txid(),vout as u32),sat(99_000))]),
			"only one canonical output may pay the known registered destination");
		if own_pending {
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.is_err(),
				"a refused absence incident must never replace the failure receipt after payout");
			let row = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) AS receipts, \
				COUNT(*) FILTER(WHERE created_index IS NOT NULL AND jsonrpc_error_code IS NULL \
					AND supervised_manifest_sha256 IS NULL) AS backend_failure \
				FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt.id]).await?)).await.unwrap();
			assert_eq!(row.get::<_, i64>("receipts"), 1); assert_eq!(row.get::<_, i64>("backend_failure"), 1);
		} else {
			let mut repeat = srv.get_custom_command(&[
				"resolve-uninjected-lightning", "--manifest", manifest_arg, "--acknowledge-trusted-operations",
			]).await.unwrap();
			repeat.kill_on_drop(true);
			let repeated = repeat.output().await.unwrap();
			assert!(repeated.status.success(), "post-payout CLI repeat failed: {}",String::from_utf8_lossy(&repeated.stderr));
			assert!(String::from_utf8_lossy(&repeated.stdout).contains("same incident already recorded"),
				"the CLI must return idempotently after payout, before the decided-source guard");
			let different = manifest.with_file_name("different-receipt-bytes.json");
			std::fs::write(&different,serde_json::to_vec_pretty(&previous).unwrap()).unwrap();
			let error = server::ln::resolve_uninjected_lightning(&cfg,&different).await.unwrap_err();
			assert!(format!("{error:#}").contains("different terminal receipt"),"{error:#}");
			let row = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) AS receipts, \
				COUNT(*) FILTER(WHERE created_index IS NOT NULL OR jsonrpc_error_code IS NOT NULL) AS fabricated \
				FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt.id]).await?)).await.unwrap();
			assert_eq!(row.get::<_, i64>("receipts"),1); assert_eq!(row.get::<_, i64>("fabricated"),0);
		}
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_lightning_dispatch WHERE id=$1", &[&attempt.id]).await?.get::<_, i64>(0))).await.unwrap(),1);
		let total=db.read(async |tx|Ok(tx.query_one("SELECT COUNT(*) AS attempts, \
			COUNT(*) FILTER(WHERE d.id IS NOT NULL) AS dispatches, COUNT(*) FILTER(WHERE s.id IS NOT NULL) AS success \
			FROM lightning_payment_attempt a LEFT JOIN expiry_lightning_dispatch d ON d.id=a.id \
			LEFT JOIN expiry_lightning_external_success s ON s.id=a.id WHERE a.payment_hash=$1",&[&hash.to_string()]).await?)).await.unwrap();
		assert_eq!(total.get::<_,i64>("attempts"),1); assert_eq!(total.get::<_,i64>("dispatches"),1); assert_eq!(total.get::<_,i64>("success"),0);
		let mut final_node = lightning.internal.grpc_client().await;
		let raw = final_node.list_send_pays(cln_rpc::ListsendpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner();
		let aggregate = final_node.list_pays(cln_rpc::ListpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner();
		if foreign_complete {
			assert_eq!(raw.payments.len(), 1); assert_eq!(aggregate.pays.len(), 1);
			let payment = &raw.payments[0];
			assert_eq!(payment.label.as_deref(), Some(foreign_label));
			assert_eq!(payment.status(), cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Complete);
			assert_eq!(payment.amount_msat.as_ref().unwrap().msat, 100_000_000);
			assert_eq!(aggregate.pays[0].label.as_deref(), Some(foreign_label));
			assert_eq!(aggregate.pays[0].status(), cln_rpc::listpays_pays::ListpaysPaysStatus::Complete);
			let received = lightning.external.grpc_client().await.list_invoices(cln_rpc::ListinvoicesRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			assert_eq!(received.invoices.len(), 1);
			assert_eq!(received.invoices[0].status(), cln_rpc::listinvoices_invoices::ListinvoicesInvoicesStatus::Paid);
			assert_eq!(received.invoices[0].amount_received_msat.as_ref().unwrap().msat, 100_000_000);
		} else if own_pending {
			let previous = accepted_parts.as_ref().unwrap();
			assert_eq!(raw.payments.len(), previous.len()); assert_eq!(aggregate.pays.len(), 1);
			for pay in &raw.payments {
				assert!(pay.payment_hash == hash.to_vec() && pay.payment_preimage.is_none()
					&& pay.status() == cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Failed);
				assert!(previous.iter().any(|before| before.created_index == pay.created_index
					&& before.groupid == pay.groupid && before.partid == pay.partid
					&& before.label == pay.label && before.amount_msat == pay.amount_msat),
					"only previous injected parts may remain; no new dispatch or replay");
			}
			assert_eq!(raw.payments.iter().map(|pay| pay.amount_msat.as_ref().unwrap().msat).sum::<u64>(), 100_000_000);
			let pay = &aggregate.pays[0];
			let before = &accepted_pays.as_ref().unwrap()[0];
			assert!(pay.payment_hash == hash.to_vec() && pay.preimage.is_none()
				&& pay.status() == cln_rpc::listpays_pays::ListpaysPaysStatus::Failed);
			assert!(pay.created_index == before.created_index && pay.label == before.label);
			// CLN tallies aggregate amounts only for Pending/Complete parts.
			// Failed retains the raw principal/identities checked above.
			assert!(pay.amount_msat.is_none());
			assert_eq!(pay.amount_sent_msat.as_ref().unwrap().msat, 0);
			let invoices = lightning.external.hold_client().await.list(hold::ListRequest {
				constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
			}).await.unwrap().into_inner().invoices;
			assert_eq!(invoices.len(), 1); assert_eq!(invoices[0].state(), hold::InvoiceState::Cancelled);
			assert!(invoices[0].preimage.is_none());
			assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
			let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
				hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.to_bytes().to_vec()],
			}).await.unwrap().into_inner();
			assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Failed(_))));
			assert_eq!(status.htlc_vtxo_ids, vec![source.to_bytes().to_vec()]);
		} else {
			assert!(raw.payments.is_empty() && aggregate.pays.is_empty(),
				"restart/repeat must never replay the extinguished previous request");
		}
	}).await.expect("supervised previous dispatch case exceeded its unchanged 180-second budget");
}

/// Exercise the pre-dispatch database boundary with a natively
/// registered signed HTLC. The external xpay task is deliberately not run:
/// this proves admission fencing, not a complete payment-vs-payout RPC race.
#[tokio::test]
async fn expiry_dispatch_rechecks_registered_sources_and_closed_attempts() {
	let ctx = TestContext::new("server/expiry_dispatch_execution_boundary").await;
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
	let preimage_bytes = [12;32];
	let invoice = lightning.external.invoice_with_preimage(Some(sat(100_000)), "dispatch-boundary", "Registered source dispatch fence", preimage_bytes).await;
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
	let invoice = Invoice::Bolt11(parsed);
	let node_id = db.read(async |t| Ok(t.query_one("
		SELECT id
		FROM lightning_node
		ORDER BY id
		LIMIT 1
	", &[]).await?.get::<_, i64>("id"))).await.unwrap();
	let attempt = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, sat(100_000), None,
			&[htlc.id()], None, tip, sat(0), None, Duration::from_secs(1), None).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else( || anyhow::anyhow!("attempt disappeared"))
	}).await.unwrap();
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&attempt,
		LightningPaymentStatus::Failed, Some("closed before queued dispatch")).await).await.unwrap().is_some());
	let replacement = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, sat(100_000), None,
			&[htlc.id()], None, tip, sat(0), None, Duration::from_secs(1), None).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else( || anyhow::anyhow!("replacement disappeared"))
	}).await.unwrap();
	assert_ne!(replacement.id, attempt.id);
	assert!(!db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap(),
		"an obsolete queued task cannot dispatch a replacement attempt");
	assert!(db.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap());
	let separate_dispatcher = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	assert!(!separate_dispatcher.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap(),
		"a separate connection cannot dispatch the same durable attempt twice");
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&replacement,
		LightningPaymentStatus::Failed, Some("unproven failure after dispatch")).await).await.is_err());
	let expired = ctx.generate_blocks(input.expiry_height().to_u32() + 1 - tip.to_u32()).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let error = db.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap_err();
	let message = format!("{error:#}");
	assert!(message.contains("expired backing") && message.contains(&htlc.id().to_string()), "{message}");
	assert_eq!(db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().id,
		replacement.id, "refused dispatch does not create or replace an attempt");
}

/// A real external receiver settles after the coarse attempt row was made
/// stale. The preimage must still reach the monetary settlement WAL
/// and the public payment status must reflect it.
#[tokio::test]
async fn expiry_xpay_preserves_late_preimage_after_coarse_failure() {
	let ctx = TestContext::new("server/expiry_xpay_late_preimage").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	let wallet = ctx.bark_sdk("sender", &srv).boarded(btc(2)).create().await;
	lightning.sync().await;
	let preimage = Preimage::random();
	let hash = preimage.compute_payment_hash();
	let mut receiver = lightning.external.hold_client().await;
	let invoice = receiver.invoice(hold::InvoiceRequest {
		payment_hash: hash.to_vec(), amount_msat: 100_000_000,
		description: Some(hold::invoice_request::Description::Memo("late-preimage-after-stale-status".into())),
		min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	wallet.pay_lightning_invoice(invoice, None, false).await.unwrap();
	lightning.external.wait_for_hold_invoice_accepted(hash).await;
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let attempt = db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
	let source_ids = db.read(async |tx| Ok(tx.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
		WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&attempt.id]).await?
		.into_iter().map(|row| row.get::<_, &str>(0).parse::<ark::VtxoId>()).collect::<Result<Vec<_>, _>>()?))
		.await.unwrap();
	assert!(!source_ids.is_empty(), "the real native payment must bind its complete signed source set");
	let source_expiries = db.read(async |tx| Ok(tx.query("SELECT v.expiry,h.htlc_expiry \
		FROM lightning_payment_attempt_htlc_vtxo l JOIN vtxo v ON v.vtxo_id=l.vtxo_id \
		JOIN htlc_vtxo h ON h.id=v.id WHERE l.lightning_payment_attempt_id=$1", &[&attempt.id]).await?
		.into_iter().map(|row| (row.get::<_, i32>("expiry"), row.get::<_, i32>("htlc_expiry"))).collect::<Vec<_>>()))
		.await.unwrap();
	assert_eq!(source_expiries.len(), source_ids.len());
	// The ordinary writer refuses an unproven terminal update.
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&attempt,
		LightningPaymentStatus::Failed, Some("unproven coarse reconciliation")).await).await.is_err());
	// Retain a legacy coarse-state fixture under the same source/attempt
	// exclusion. The actual native held part and outcome history stay intact.
	db.write(async |t| {
		let sources = source_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
		t.query("SELECT vtxo_id FROM vtxo WHERE vtxo_id=ANY($1) ORDER BY vtxo_id FOR UPDATE", &[&sources]).await?;
		t.query_one("SELECT id FROM lightning_payment_attempt WHERE id=$1 FOR UPDATE", &[&attempt.id]).await?;
		t.execute("UPDATE lightning_payment_attempt SET status='failed',error='legacy coarse reconciliation', \
			updated_at=NOW() WHERE id=$1", &[&attempt.id]).await?;
		Ok(())
	}).await.unwrap();
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	receiver.settle(hold::SettleRequest { payment_preimage: preimage.to_vec() }).await.unwrap();
	async {
		loop {
			let proven = db.read(async |tx| Ok(tx.get_htlc_settlement_by_payment_hash(hash).await? == Some(preimage)
								&& tx.query_opt("SELECT id FROM expiry_lightning_external_success WHERE id=$1", &[&attempt.id]).await?.is_some()
				&& !tx.query_one("SELECT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l \
					JOIN vtxo v ON v.vtxo_id=l.vtxo_id WHERE l.lightning_payment_attempt_id=$1 \
					AND v.spend_state<>'ln-spent')", &[&attempt.id]).await?.get::<_, bool>(0)))
				.await.unwrap();
			if proven { break; }
			tokio::time::sleep(poll_interval()).await;
		}
	}.wait_millis(30_000).await;
	let receipt_tip = ctx.bitcoind().get_block_count().await;
	assert!(source_expiries.iter().all(|(backing, htlc)|
		receipt_tip < u64::try_from(*backing).unwrap() && receipt_tip < u64::try_from(*htlc).unwrap()),
		"success receipt must arrive before every backing/HTLC expiry, not wait for fallback eligibility");
	let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
		hash: hash.to_vec(), wait: false,
		htlc_vtxo_ids: source_ids.iter().map(|source| source.to_bytes().to_vec()).collect(),
	}).await.unwrap().into_inner();
	let mut expected_sources = source_ids.iter().map(|source| source.to_bytes().to_vec()).collect::<Vec<_>>();
	expected_sources.sort_unstable();
	let mut echoed_sources = status.htlc_vtxo_ids.clone();
	echoed_sources.sort_unstable();
	assert_eq!(echoed_sources, expected_sources, "success must echo the complete signed payment scope");
	assert!(matches!(status.payment_status,
		Some(protos::lightning_payment_status::PaymentStatus::Success(ref success))
		if success.preimage == preimage.to_vec()), "{status:?}");
	let success_tip = ctx.bitcoind().get_block_count().await;
	assert!(source_expiries.iter().all(|(backing, htlc)|
		success_tip < u64::try_from(*backing).unwrap() && success_tip < u64::try_from(*htlc).unwrap()),
		"public Success must be usable before source expiry too");
}
