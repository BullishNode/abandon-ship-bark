use std::str::FromStr;
use std::time::Duration;

use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};

use ark::ServerVtxo;
use ark::lightning::Invoice;
use ark::test_util::VTXO_VECTORS;
use ark::vtxo::Full;
use ark_testing::{sat, TestContext};
use bark::lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use bitcoin_ext::BlockHeight;
use server::database::{Db, Tx};
use server::database::htlc_vtxo::{self, HtlcDirection};
use server::database::ln::{LightningPaymentAttempt, LightningPaymentStatus};

async fn queued_attempt(name: &str) -> (TestContext, Db, LightningPaymentAttempt, ServerVtxo<Full>) {
	let mut ctx = TestContext::new_minimal(name).await;
	ctx.init_central_postgres().await;
	let config = ctx.new_postgres(&ctx.test_name).await;
	Db::create(&config).await.unwrap();
	let db = Db::connect(&config).await.unwrap();
	let node = PublicKey::from_str(super::DUMMY_PUBKEY).unwrap();
	let (node_id, _) = db.write(async |t| t.register_lightning_node(&node).await).await.unwrap();
	let source = ServerVtxo::from(VTXO_VECTORS.arkoor_htlc_out_vtxo.clone());
	let policy = VTXO_VECTORS.arkoor_htlc_out_vtxo.policy().as_server_htlc_send().unwrap();
	let hash = policy.payment_hash;
	db.write(async |t| {
		t.upsert_vtxos([&source]).await?;
		htlc_vtxo::create_htlc_vtxos(t, &[(source.id(), hash, policy.htlc_expiry)], HtlcDirection::Incoming).await
	}).await.unwrap();
	let key = SecretKey::from_slice(&[41; 32]).unwrap();
	let invoice = Invoice::Bolt11(InvoiceBuilder::new(Currency::Regtest)
		.description("payment attempt source".into()).payment_hash(hash.to_sha256_hash())
		.payment_secret(PaymentSecret([42; 32])).current_timestamp().min_final_cltv_expiry_delta(18)
		.amount_milli_satoshis(source.amount().to_sat() * 1_000)
		.build_signed(|message| Secp256k1::new().sign_ecdsa_recoverable(message, &key)).unwrap());
	let attempt = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, source.amount(), None, &[source.id()], None,
			BlockHeight::new(1), sat(1), None, Duration::from_secs(60), None).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else(|| anyhow::anyhow!("attempt missing"))
	}).await.unwrap();
	(ctx, db, attempt, source)
}

async fn current(db: &Db, attempt: &LightningPaymentAttempt) -> LightningPaymentAttempt {
	let current = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(attempt.payment_hash).await)
		.await.unwrap().unwrap();
	assert_eq!(current.id, attempt.id);
	current
}

async fn unchanged(db: &Db, attempt: &LightningPaymentAttempt, source: &ServerVtxo<Full>) {
	let current = current(db, attempt).await;
	assert_eq!(current.status, attempt.status);
	assert_eq!(current.final_amount_msat, attempt.final_amount_msat);
	assert_eq!(current.error, attempt.error);
	assert_eq!(current.updated_at, attempt.updated_at);
	let rows = db.read(async |t| Ok(t.query("SELECT v.vtxo_id,v.spend_state::TEXT FROM vtxo v \
		JOIN lightning_payment_attempt_htlc_vtxo l ON l.vtxo_id=v.vtxo_id \
		WHERE l.lightning_payment_attempt_id=$1", &[&attempt.id]).await?)).await.unwrap();
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].get::<_, &str>("vtxo_id"), source.id().to_string());
	assert_eq!(rows[0].get::<_, &str>("spend_state"), "spendable");
}

async fn update(
	t: &Tx<'_>, attempt: &LightningPaymentAttempt, status: LightningPaymentStatus, status_only: bool,
) -> anyhow::Result<bool> {
	Ok(if status_only {
		t.update_lightning_payment_attempt_status(attempt, status, None).await?
	} else {
		t.update_lightning_payment_attempt_result(attempt, status, None, None).await?
	}.is_some())
}

#[tokio::test]
async fn queued_attempt_refuses_success_without_dispatch() {
	let (_ctx, db, attempt, source) = queued_attempt("postgresd/queued_attempt_refuses_success_without_dispatch").await;
	let snapshot = async || db.read(async |t| Ok(serde_json::from_str::<serde_json::Value>(&t.query_one("SELECT jsonb_build_array( \
		jsonb_build_array(a.id,a.lightning_node_id,a.payment_hash,a.amount_msat,a.final_amount_msat, \
			a.sender_mailbox_id,a.status,a.error,a.block_height,a.user_fee_sat,a.user_agent,a.retry_for_secs, \
			a.max_routing_fee_msat,a.lightning_htlc_subscription_id,a.created_at,a.updated_at), \
		(SELECT jsonb_build_array(q.id,q.created_at) FROM expiry_lightning_queued_attempt q WHERE q.id=a.id), \
		(SELECT COUNT(*) FROM lightning_payment_attempt_history h WHERE h.id=a.id), \
		(SELECT COUNT(*) FROM expiry_lightning_dispatch d WHERE d.id=a.id), \
		(SELECT COUNT(*) FROM expiry_lightning_external_success s WHERE s.id=a.id), \
		(SELECT COUNT(*) FROM expiry_lightning_terminal_failure f WHERE f.id=a.id), \
		(SELECT COUNT(*) FROM expiry_lightning_rpc_rejection r WHERE r.id=a.id), \
		(SELECT COUNT(*) FROM expiry_lightning_internal_funding i WHERE i.id=a.id), \
		(SELECT jsonb_agg(jsonb_build_array(l.vtxo_id,v.id,v.vtxo,v.expiry,v.oor_spent_txid,v.spent_in_round, \
			v.created_at,v.updated_at,v.lightning_htlc_subscription_id,v.offboarded_in,v.vtxo_txid,v.exit_delta, \
			v.policy_type,v.policy,v.server_pubkey,v.amount,v.anchor_point,v.banned_until_height,v.spend_state, \
			v.frontier_at,v.confirmed_height,v.onchain_spent_height,v.onchain_spent_txid, \
			h.id,h.payment_hash,h.htlc_expiry,h.direction,h.offchain_resolution,h.chain_resolution, \
			h.chain_resolution_height,h.created_at) ORDER BY l.vtxo_id) \
			FROM lightning_payment_attempt_htlc_vtxo l JOIN vtxo v ON v.vtxo_id=l.vtxo_id \
			LEFT JOIN htlc_vtxo h ON h.id=v.id WHERE l.lightning_payment_attempt_id=a.id))::TEXT \
		FROM lightning_payment_attempt a WHERE a.id=$1", &[&attempt.id]).await?.get::<_, String>(0))?))
		.await.unwrap();
	let before = snapshot().await;
	assert!(before[1].is_array(), "the current attempt must retain its queue marker");
	for index in 2..=7 { assert_eq!(before[index].as_i64(), Some(0)); }
	assert_eq!(before[8].as_array().unwrap().len(), 1);
	for (status_only, amount) in [(true, None), (false, None),
		(false, Some(attempt.amount_msat)), (false, Some(attempt.amount_msat + 2_345))]
	{
		let result = db.write(async |t| {
			if status_only {
				t.update_lightning_payment_attempt_status(&attempt, LightningPaymentStatus::Succeeded,
					Some("unproven success")).await
			} else {
				t.update_lightning_payment_attempt_result(&attempt, LightningPaymentStatus::Succeeded,
					Some("unproven success"), amount).await
			}
		}).await;
		let error = result.expect_err("a queued external source has no own success before dispatch");
		assert!(format!("{error:#}").contains("queued external payment has no own success receipt"));
		unchanged(&db, &attempt, &source).await;
		assert!(snapshot().await == before, "refusal must preserve the attempt, history, queue and complete source");
	}
}

#[tokio::test]
async fn dispatched_attempt_refuses_unproven_results() {
	let (_ctx, db, attempt, source) = queued_attempt("postgresd/dispatched_attempt_refuses_unproven_results").await;
	assert!(db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap());
	for status in [LightningPaymentStatus::Failed, LightningPaymentStatus::Succeeded] {
		for status_only in [true, false] {
			assert!(db.write(async |t| update(t, &attempt, status, status_only).await).await.is_err());
			unchanged(&db, &attempt, &source).await;
		}
	}
	for amount in [attempt.amount_msat, u64::MAX] {
		assert!(db.write(async |t| t.update_lightning_payment_attempt_result(&attempt,
			LightningPaymentStatus::Submitted, Some("unqualified amount"), Some(amount)).await).await.is_err());
		unchanged(&db, &attempt, &source).await;
	}
}

#[tokio::test]
async fn dispatched_success_uses_its_receipt_amount() {
	for (status_only, foreign_amount, name) in [
		(true, false, "postgresd/dispatched_success_status_amount"),
		(false, false, "postgresd/dispatched_success_result_amount"),
		(true, true, "postgresd/dispatched_success_status_replaces_amount"),
		(false, true, "postgresd/dispatched_success_result_replaces_amount"),
	]
	{
		let (_ctx, db, attempt, source) = queued_attempt(name).await;
		assert!(db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap());
		let amount = i64::try_from(attempt.amount_msat + 2_345).unwrap();
		// This is a database input fixture, not a native payment result.
		db.write(async |t| {
			t.query_one("SELECT vtxo_id FROM vtxo WHERE vtxo_id=$1 FOR UPDATE", &[&source.id().to_string()]).await?;
			t.query_one("SELECT id FROM lightning_payment_attempt WHERE id=$1 FOR UPDATE", &[&attempt.id]).await?;
			if foreign_amount {
				t.execute("UPDATE lightning_payment_attempt SET final_amount_msat=$2,updated_at=NOW() WHERE id=$1",
					&[&attempt.id, &i64::try_from(attempt.amount_msat + 999)?]).await?;
			}
			t.execute("INSERT INTO expiry_lightning_external_success(id,created_index,updated_index,amount_sent_msat) \
				VALUES($1,1,2,$2)", &[&attempt.id, &amount]).await?;
			Ok(())
		}).await.unwrap();
		let attempt = current(&db, &attempt).await;
		assert!(db.write(async |t| t.update_lightning_payment_attempt_result(&attempt,
			LightningPaymentStatus::Succeeded, None, Some(attempt.amount_msat)).await).await.is_err());
		unchanged(&db, &attempt, &source).await;
		assert!(db.write(async |t| update(t, &attempt, LightningPaymentStatus::Succeeded, status_only).await).await.unwrap());
		let succeeded = current(&db, &attempt).await;
		assert_eq!(succeeded.status, LightningPaymentStatus::Succeeded);
		assert_eq!(succeeded.final_amount_msat, Some(u64::try_from(amount).unwrap()));
		assert!(db.write(async |t| t.update_lightning_payment_attempt_result(&attempt,
			LightningPaymentStatus::Succeeded, Some("stale update"), None).await).await.unwrap().is_none());
		assert_eq!(current(&db, &attempt).await.updated_at, succeeded.updated_at);
	}
}

#[tokio::test]
async fn dispatched_failure_clears_unqualified_amount() {
	for (status_only, name) in [(true, "postgresd/dispatched_failure_status_amount"),
		(false, "postgresd/dispatched_failure_result_amount")]
	{
		let (_ctx, db, attempt, source) = queued_attempt(name).await;
		assert!(db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap());
		db.write(async |t| {
			t.query_one("SELECT vtxo_id FROM vtxo WHERE vtxo_id=$1 FOR UPDATE", &[&source.id().to_string()]).await?;
			t.query_one("SELECT id FROM lightning_payment_attempt WHERE id=$1 FOR UPDATE", &[&attempt.id]).await?;
			t.execute("UPDATE lightning_payment_attempt SET final_amount_msat=$2,error='old diagnostic',updated_at=NOW() \
				WHERE id=$1", &[&attempt.id, &i64::try_from(attempt.amount_msat + 999)?]).await?;
			t.execute("INSERT INTO expiry_lightning_terminal_failure(id,jsonrpc_error_code) VALUES($1,205)", &[&attempt.id]).await?;
			Ok(())
		}).await.unwrap();
		let attempt = current(&db, &attempt).await;
		assert!(db.write(async |t| t.update_lightning_payment_attempt_result(&attempt,
			LightningPaymentStatus::Failed, Some("refused update"), Some(attempt.amount_msat)).await).await.is_err());
		unchanged(&db, &attempt, &source).await;
		assert!(db.write(async |t| update(t, &attempt, LightningPaymentStatus::Failed, status_only).await).await.unwrap());
		let failed = current(&db, &attempt).await;
		assert_eq!(failed.status, LightningPaymentStatus::Failed);
		assert!(failed.final_amount_msat.is_none());
		assert_eq!(failed.error.as_deref(), Some("old diagnostic"));
	}
}

#[tokio::test]
async fn dispatched_attempt_refuses_contradictory_receipts() {
	let (_ctx, db, attempt, source) = queued_attempt("postgresd/dispatched_attempt_refuses_contradictory_receipts").await;
	assert!(db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap());
	db.write(async |t| {
		t.execute("INSERT INTO expiry_lightning_external_success(id,created_index,amount_sent_msat) VALUES($1,1,$2)",
			&[&attempt.id, &i64::try_from(attempt.amount_msat)?]).await?;
		t.execute("INSERT INTO expiry_lightning_terminal_failure(id,jsonrpc_error_code) VALUES($1,205)", &[&attempt.id]).await?;
		Ok(())
	}).await.unwrap();
	for status in [LightningPaymentStatus::Failed, LightningPaymentStatus::Succeeded] {
		for status_only in [true, false] {
			assert!(db.write(async |t| update(t, &attempt, status, status_only).await).await.is_err());
			unchanged(&db, &attempt, &source).await;
		}
	}
}

#[tokio::test]
async fn payment_attempt_closure_serializes_with_dispatch() {
	for (dispatch_first, status_only, name) in [
		(true, true, "postgresd/dispatch_before_status"), (true, false, "postgresd/dispatch_before_result"),
		(false, true, "postgresd/status_before_dispatch"), (false, false, "postgresd/result_before_dispatch"),
	] {
		let (_ctx, db, attempt, source) = queued_attempt(name).await;
		let competing_db = db.clone();
		let competing_attempt = attempt.clone();
		let (send_pid, receive_pid) = tokio::sync::oneshot::channel();
		let writer = db.write(async |t| {
			t.query_one("SELECT vtxo_id FROM vtxo WHERE vtxo_id=$1 FOR UPDATE", &[&source.id().to_string()]).await?;
			let writer = tokio::spawn(async move {
				competing_db.write(async |other| {
					let pid = other.query_one("SELECT pg_backend_pid()", &[]).await?.get::<_, i32>(0);
					send_pid.send(pid).expect("race observer alive");
					if dispatch_first {
						update(other, &competing_attempt, LightningPaymentStatus::Failed, status_only).await
					} else { other.admit_lightning_dispatch(&competing_attempt).await }
				}).await
			});
			let pid = receive_pid.await.unwrap();
			tokio::time::timeout(Duration::from_secs(10), async {
				loop {
					if t.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND NOT granted)",
						&[&pid]).await?.get::<_, bool>(0) { break; }
					tokio::time::sleep(Duration::from_millis(10)).await;
				}
				Ok::<_, anyhow::Error>(())
			}).await.expect("competing writer must reach an actual row lock")?;
			if dispatch_first { assert!(t.admit_lightning_dispatch(&attempt).await?); }
			else { assert!(update(t, &attempt, LightningPaymentStatus::Failed, status_only).await?); }
			Ok(writer)
		}).await.unwrap();
		let result = writer.await.unwrap();
		if dispatch_first {
			assert!(result.is_err());
			unchanged(&db, &attempt, &source).await;
		} else {
			assert!(!result.unwrap());
			assert_eq!(current(&db, &attempt).await.status, LightningPaymentStatus::Failed);
		}
		let dispatched = db.read(async |t| Ok(t.query_opt("SELECT id FROM expiry_lightning_dispatch WHERE id=$1",
			&[&attempt.id]).await?.is_some())).await.unwrap();
		assert_eq!(dispatched, dispatch_first);
	}
}
