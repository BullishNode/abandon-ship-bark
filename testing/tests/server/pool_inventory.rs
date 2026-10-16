//! Actual memory-backed pool observations, never chain/readiness claims.

use std::time::Duration;

use ark::{ProtocolEncoding, Vtxo};
use ark::vtxo::Full;
use ark::lightning::PaymentHash;
use bark::actions::lightning::receive::LightningReceiveState;
use ark_testing::{btc, sat, TestContext};
use bitcoin::hex::DisplayHex;
use bitcoin_ext::{BlockDelta, rpc::RpcApi};
use server::database::Db;
use server::vtxopool::VtxoTarget;
use server_rpc::{admin::LightningAdminServiceClient, protos};

#[tokio::test]
async fn pool_inventory_empty_is_read_only_and_not_payment_ready() {
	let ctx = TestContext::new("server/pool_inventory_empty_is_read_only_and_not_payment_ready").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().create().await;
	let mut wallet = srv.get_wallet_rpc().await;
	let before = wallet.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
	let endpoint = format!("http://{}", srv.config().rpc.admin_address.unwrap());
	let mut rpc = LightningAdminServiceClient::connect(endpoint).await.unwrap();
	let first = rpc.pool_inventory(protos::Empty {}).await.unwrap().into_inner();
	assert_eq!(first.schema_version, 1);
	assert!(first.candidate_snapshot_complete && first.candidates.is_empty());
	assert!(!first.complete && !first.missing_sources.is_empty());
	assert_eq!(first.network, "regtest");
	let again = rpc.pool_inventory(protos::Empty {}).await.unwrap().into_inner();
	assert_eq!(again.candidates, first.candidates);
	let after = wallet.wallet_inventory(protos::Empty {}).await.unwrap().into_inner().rounds.unwrap();
	assert_eq!(before.last_revealed_index, after.last_revealed_index);
	assert_eq!(before.utxos, after.utxos);
	assert_eq!(before.reserved_outpoints, after.reserved_outpoints);
}

#[tokio::test]
async fn pool_inventory_real_issued_candidates_bind_core_paths_and_hide_tweaks() {
	let ctx = TestContext::new("server/pool_inventory_real_issued_candidates_bind_core_paths_and_hide_tweaks").await;
	let srv = ctx.captaind("server").funded(btc(1)).cfg(|cfg| {
		cfg.vtxopool.vtxo_targets = vec![VtxoTarget { amount: sat(10_000), count: 8 }];
		cfg.vtxopool.vtxo_lifetime = BlockDelta::new(128);
		cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(10);
	}).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	ctx.generate_blocks(1).await;
	let endpoint = format!("http://{}", srv.config().rpc.admin_address.unwrap());
	let mut rpc = LightningAdminServiceClient::connect(endpoint).await.unwrap();
	let core = ctx.bitcoind().sync_client();
	let source = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			if let Ok(source) = rpc.pool_inventory(protos::Empty {}).await {
				let source = source.into_inner();
				if u64::from(source.height) == core.get_block_count().unwrap() { break source; }
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}).await.expect("native source must reach the real confirmed pool tip");
	assert!(source.candidate_snapshot_complete && !source.complete);
	assert_eq!(source.candidates.len(), 8);
	assert_eq!(source.pool_pre_expiry_blocks, 10);
	assert_eq!(source.tip_hash, core.get_best_block_hash().unwrap().to_string());
	assert_eq!(source.genesis, core.get_block_hash(0).unwrap().to_string());
	let mut total = 0;
	for candidate in &source.candidates {
		assert!(candidate.runtime_member && candidate.key_present && candidate.key_matches);
		assert!(!candidate.in_flux && !candidate.pool_spent && !candidate.has_spend_reference);
		assert_eq!(candidate.spend_state, "pool");
		let coin = Vtxo::<Full>::deserialize(&candidate.signed_vtxo).unwrap();
		assert_eq!(coin.id().to_string(), candidate.id);
		assert_eq!(coin.server_pubkey().to_string(), source.server_pubkey);
		assert_eq!(coin.amount().to_sat(), candidate.amount_sat);
		assert_eq!(coin.expiry_height().to_u32(), candidate.expiry_height);
		assert_eq!(u32::from(coin.exit_depth()), candidate.exit_depth);
		assert!(candidate.exit_depth <= source.max_exit_depth);
		assert!(candidate.expiry_height > source.height + source.pool_pre_expiry_blocks);
		let anchor = coin.chain_anchor();
		let tx = core.get_raw_transaction(&anchor.txid, None).unwrap();
		coin.validate(&tx).expect("actual signed ancestry must validate against Core bytes");
		let backing = core.get_tx_out(&anchor.txid, anchor.vout, Some(true)).unwrap().unwrap();
		assert!(backing.confirmations > 0);
		total += candidate.amount_sat;
	}
	assert_eq!(total, 80_000);
	assert_eq!(rpc.pool_inventory(protos::Empty {}).await.unwrap().into_inner().candidates, source.candidates);
	let address = srv.config().rpc.admin_address.unwrap().to_string();
	let mut cli = srv.get_custom_command(&["rpc", "--addr", &address, "pool-inventory"]).await.unwrap();
	let output = tokio::time::timeout(Duration::from_secs(30), cli.output()).await.unwrap().unwrap();
	assert!(output.status.success(), "pool CLI failed: {}", String::from_utf8_lossy(&output.stderr));
	assert!(output.stdout.len() < 2 * 1024 * 1024);
	let private_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
	assert_eq!(private_json["complete"], false);
	assert_eq!(private_json["candidates"].as_array().unwrap().len(), 8);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let tweaks = db.read(async |t| Ok(t.query("SELECT tweak FROM ephemeral_tweak", &[]).await?
		.into_iter().map(|r| r.get::<_, Vec<u8>>(0)).collect::<Vec<_>>())).await.unwrap();
	assert!(!tweaks.is_empty());
	let text = String::from_utf8(output.stdout).unwrap();
	for tweak in tweaks { assert!(!text.contains(&tweak.as_hex().to_string()), "private tweak leaked by source"); }
}

/// Wait for the real server query to be blocked on our native table barrier.
/// This observes pg_locks; it does not insert or alter financial rows.
async fn wait_for_pool_query_lock(db: &Db, pattern: &str) {
	tokio::time::timeout(Duration::from_secs(3), async {
		loop {
			let blocked: bool = db.read(async |t| Ok(t.query_one(
				"SELECT EXISTS (SELECT 1 FROM pg_stat_activity a JOIN pg_locks l ON l.pid=a.pid \
				WHERE a.datname=current_database() AND l.relation='vtxo_pool'::regclass \
				AND NOT l.granted AND a.query LIKE $1)", &[&pattern],
			).await?.get(0))).await.unwrap();
			if blocked { break; }
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
	}).await.expect("actual inventory/receive query did not reach the native table barrier");
}

#[tokio::test]
async fn pool_inventory_refuses_membership_cut_during_real_lightning_receive() {
	let ctx = TestContext::new("server/pool_inventory_refuses_membership_cut_during_real_lightning_receive").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(1)).cfg(|cfg| {
		cfg.vtxopool.vtxo_targets = vec![VtxoTarget { amount: sat(10_000), count: 8 }];
		cfg.vtxopool.vtxo_lifetime = BlockDelta::new(128);
		cfg.vtxopool.vtxo_pre_expiry = BlockDelta::new(10);
	}).create().await;
	srv.wait_for_vtxopool(&ctx).await;
	ctx.generate_blocks(1).await;
	let wallet = ctx.bark_sdk("receiver", &srv).cfg(|cfg| cfg.daemon_manual_sync = true).create().await;
	lightning.sync().await;
	let invoice = wallet.bolt11_invoice(sat(2_000), None, None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let endpoint = format!("http://{}", srv.config().rpc.admin_address.unwrap());
	let mut rpc = LightningAdminServiceClient::connect(endpoint).await.unwrap();
	let before = rpc.pool_inventory(protos::Empty {}).await.unwrap().into_inner();
	assert_eq!(before.candidates.len(), 8);

	let (payment, observation) = tokio::join!(
		lightning.external.try_pay_bolt11(invoice.to_string()),
		async {
			let mut ark = srv.get_public_rpc().await;
			tokio::time::timeout(Duration::from_secs(30), ark.check_lightning_receive(
				protos::CheckLightningReceiveRequest { hash: hash.to_vec(), wait: true },
			)).await.unwrap().unwrap();
			let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
			let (claim_tx, claim_rx) = tokio::sync::oneshot::channel();
			let (release_tx, release_rx) = tokio::sync::oneshot::channel();
			let barrier = async {
				db.write(async |t| {
					t.execute("LOCK TABLE vtxo_pool IN ACCESS EXCLUSIVE MODE", &[]).await?;
					locked_tx.send(()).unwrap();
					release_rx.await.unwrap();
					Ok(())
				}).await.unwrap();
			};
			let inventory = async {
				locked_rx.await.unwrap();
				rpc.pool_inventory(protos::Empty {}).await
			};
			let claim = async {
				claim_rx.await.unwrap();
				wallet.try_claim_lightning_receive(hash, false).await
			};
			let controller = async {
				// Inventory already copied the original memory IDs when its SQL blocks.
				wait_for_pool_query_lock(&db, "%SELECT p.vtxo_id,%").await;
				claim_tx.send(()).unwrap();
				// The real receive has now taken its IDs out of memory, then blocked
				// on its own SELECT. Releasing the table cannot turn our old cut valid.
				wait_for_pool_query_lock(&db, "%SELECT p.vtxo FROM vtxo_pool p JOIN%").await;
				release_tx.send(()).unwrap();
			};
			let (_, source, claim, _) = tokio::join!(barrier, inventory, claim, controller);
			let error = source.expect_err("stale membership must not become a successful complete source");
			assert_eq!(error.code(), tonic::Code::FailedPrecondition);
			assert!(error.message().contains("pool membership changed"), "{error}");
			assert!(matches!(claim.unwrap(), LightningReceiveState::Settled(_)), "real receive must settle after releasing the barrier");
			error.message().to_owned()
		},
	);
	payment.expect("real external payment must settle, not remain an unknown setup");
	let after = rpc.pool_inventory(protos::Empty {}).await.unwrap().into_inner();
	let spent: Vec<String> = db.read(async |t| Ok(t.query(
		"SELECT vtxo_id FROM vtxo_pool WHERE spent_at IS NOT NULL ORDER BY vtxo_id", &[],
	).await?.into_iter().map(|r| r.get(0)).collect())).await.unwrap();
	assert!(!spent.is_empty(), "the real payment must consume actual original pool coins");
	for id in &spent {
		assert!(before.candidates.iter().any(|c| &c.id == id));
		assert!(!after.candidates.iter().any(|c| &c.id == id));
	}
	assert!(!after.complete, "native observation still does not imply independent Core readiness");
	assert_eq!(wallet.spendable_vtxos().await.unwrap().iter().map(|v| v.amount()).sum::<bitcoin::Amount>(), sat(2_000));
	assert_eq!(observation, "pool membership changed during observation");
}
