//! Real PostgreSQL service-state durability, not a financial source fixture.
//! No VTXO, entitlement, chain observation or transaction body is synthesized.

use ark_testing::TestContext;
use server::database::Db;

async fn state(db: &Db) -> (i64,i64,i64,i64) {
	db.read(async |t| {
		let row=t.query_one("SELECT after_id,through_id,(SELECT COUNT(*) FROM expiry_ledger_commit) AS count, \
			(SELECT COALESCE(MAX(revision),0) FROM expiry_ledger_commit) AS revision \
			FROM expiry_user_claim_backfill_service WHERE id=1", &[]).await?;
		Ok((row.get("after_id"),row.get("through_id"),row.get("count"),row.get("revision")))
	}).await.unwrap()
}

#[tokio::test]
async fn user_claim_backfill_service_reopens_without_invalidating_financial_history() {
	let mut ctx=TestContext::new_minimal("postgresd/user_claim_backfill_service_reopens").await;
	ctx.init_central_postgres().await;
	let config=ctx.new_postgres("user_claim_backfill_service").await;
	Db::create(&config).await.unwrap();
	let db=Db::connect(&config).await.unwrap();
	let initial=state(&db).await;
	assert_eq!((initial.0,initial.1),(0,0));
	db.write(async |t| {
		t.execute("UPDATE expiry_user_claim_backfill_service SET after_id=16,through_id=48 WHERE id=1", &[]).await?;
		Ok(())
	}).await.unwrap();
	assert_eq!(state(&db).await,(16,48,initial.2,initial.3),"service progress is not a financial revision");
	drop(db);
	let db=Db::connect(&config).await.unwrap();
	assert_eq!(state(&db).await,(16,48,initial.2,initial.3),"restart retains the service epoch instead of retrying its first prefix");
	let invalid=db.write(async |t| {
		t.execute("UPDATE expiry_user_claim_backfill_service SET after_id=49 WHERE id=1", &[]).await?;
		Ok(())
	}).await;
	assert!(invalid.is_err(),"a position beyond the closed epoch must fail atomically");
	assert_eq!(state(&db).await,(16,48,initial.2,initial.3));
	db.write(async |t| {
		t.execute("UPDATE expiry_user_claim_backfill_service SET after_id=0,through_id=96 WHERE id=1", &[]).await?;
		Ok(())
	}).await.unwrap();
	assert_eq!(state(&db).await,(0,96,initial.2,initial.3),"a wrapped epoch revisits old rows without changing payment history");
}
