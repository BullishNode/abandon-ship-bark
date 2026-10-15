//! A stale phone resubmits a signed refresh after the server issued its outputs.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;
use ark_testing::{btc, sat, TestContext};
use ark_testing::daemon::captaind::{ArkClient, proxy::ArkRpcProxy};
use ark_testing::util::FutureExt;
use bitcoin_ext::{BlockDelta, BlockHeight};
use bitcoincore_rpc::RpcApi;
use server::database::Db;
use server_log::RoundFinished;
use server_rpc::protos;

#[derive(Clone)]
struct CaptureSubmission(Arc<Mutex<Option<protos::RoundParticipationRequest>>>);

#[async_trait::async_trait]
impl ArkRpcProxy for CaptureSubmission {
	async fn submit_round_participation(&self, upstream: &mut ArkClient,
		req: protos::RoundParticipationRequest,
	) -> Result<protos::RoundParticipationResponse, tonic::Status> {
		let response = upstream.submit_round_participation(req.clone()).await?.into_inner();
		*self.0.lock().unwrap() = Some(req);
		Ok(response)
	}
}

#[tokio::test]
async fn fallback_post_issued_requests_cannot_multiply_outputs() {
	Box::pin(post_issued()).await;
}

async fn post_issued() {
	let ctx = TestContext::new("bark_sdk/fallback_post_issued_requests_cannot_multiply_outputs").await;
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.funded(btc(1)).watchmand().recovery_fee_headroom(sat(20_000)).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(300);
			c.round_interval = Duration::from_secs(3600);
		}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let captured = Arc::new(Mutex::new(None));
	let proxy = srv.start_proxy_no_mailbox(CaptureSubmission(captured.clone())).await;
	let endpoint = proxy.address.clone();
	let mnemonic = bip39::Mnemonic::generate(12).unwrap();
	let wallet = ctx.bark_sdk("stale-phone", &srv).mnemonic(mnemonic.clone()).cfg(move |c| {
		c.daemon_manual_sync = true;
		c.server_address = endpoint;
	}).boarded(sat(300_000)).create().await;
	wallet.stop_daemon_wait().await.unwrap();
	let original = wallet.get_full_vtxo(wallet.spendable_vtxos().await.unwrap()[0].id()).await.unwrap();
	let record = wallet.fallback_destination().await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let scheduled = core.get_block_count().unwrap() as u32 + 140;
	let request = wallet.refresh_vtxos_scheduled(vec![original.id()], BlockHeight::new(scheduled))
		.await.unwrap().unwrap();
	let unlock = request.state().unlock_hash().unwrap().to_string();
	let signed = captured.lock().unwrap().clone().expect("capture a genuinely accepted signed request");
	drop(request);
	// The phone never finishes its accepted exchange. The controller replays
	// its genuine request over gRPC, independently of client-side filtering.
	drop(wallet);
	ctx.generate_blocks(scheduled - core.get_block_count().unwrap() as u32).await;
	let mut finished = srv.subscribe_log::<RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(60)).await.unwrap();
	let rows = db.read(async |t| Ok(t.query("SELECT vtxo FROM vtxo WHERE spend_state='unclaimed'", &[]).await?)).await.unwrap();
	let initial = rows.iter().map(|r| Vtxo::<Full>::deserialize(&r.get::<_, Vec<u8>>(0)).unwrap()).collect::<Vec<_>>();
	let initial_unclaimed = initial.iter().map(|v| v.id().to_string()).collect::<std::collections::BTreeSet<_>>();
	let replacements = initial.into_iter()
		.filter(|v| v.unlock_hash().is_some_and(|h| h.to_string() == unlock)).collect::<Vec<_>>();
	assert!(!replacements.is_empty());
	assert_eq!(replacements.iter().map(|v| v.amount().to_sat()).sum::<u64>(), 300_000);
	let ids = replacements.iter().map(|v| v.id().to_string()).collect::<Vec<_>>();
	let mut refused = Vec::new();
	for round in 0..3 {
		let mut rpc = srv.get_public_rpc().await;
		let error = rpc.submit_round_participation(signed.clone()).await.unwrap_err();
		assert_eq!(error.code(), tonic::Code::InvalidArgument);
		assert!(error.message().contains("unusable"), "must refuse the spent original, not a malformed request: {error}");
		refused.push(serde_json::json!({"round":round,"message":error.message()}));
		srv.trigger_round().await;
		ctx.generate_blocks(1).await;
		tokio::time::sleep(Duration::from_secs(1)).await;
		let associations: i64 = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM round_part_input WHERE vtxo_id=$1", &[&original.id().to_string()],
		).await?.get(0))).await.unwrap();
		assert_eq!(associations, 1, "stale requests must not create a second participation");
		let rows = db.read(async |t| Ok(t.query("SELECT vtxo_id FROM vtxo WHERE spend_state='unclaimed'", &[]).await?)).await.unwrap();
		let actual = rows.iter().map(|r| r.get::<_, String>(0)).collect::<std::collections::BTreeSet<_>>();
		assert_eq!(actual, initial_unclaimed, "intervening rounds must not add to the complete initial unclaimed set");
	}
	let mut backing = replacements.clone();
	backing.push(original.clone());
	super::fallback_lightning::expire_and_confirm_sweeps(&ctx, &db, &backing).await;
	super::fallback_lightning::enable_payouts(&ctx, &srv).await;
	let (payout, fee) = super::fallback_lightning::wait_and_reconcile_payout(&ctx, &db,
		&ids, &record.spk, 300_000).await;
	let old_paid: i64 = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=$1", &[&original.id().to_string()],
	).await?.get(0))).await.unwrap();
	assert_eq!(old_paid, 0, "the original and replacement must never both be paid");
	let evidence = serde_json::json!({"refused_replays":refused,"original":original.id().to_string(),
		"unlock_hash":unlock,"replacement_ids":ids,"gross_sat":300_000,"recipient_fee_sat":fee,
		"payout":payout.to_string(),"source":"genuine signed submission replayed after issuance across three rounds"});
	std::fs::write(ctx.datadir.join("postissued.json"), serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
	super::fallback_lightning::restore_and_spend(&ctx, &mnemonic, payout).await;
}
