//! Measure authenticated registration and positive ownership lookup for unfunded accounts.

use std::time::Duration;

use ark::fallback::FallbackRecord;
use ark::mailbox::MailboxIdentifier;
use ark::{ProtocolEncoding, SECP};
use ark_testing::TestContext;
use bitcoin::bip32::{ChildNumber, Xpriv, Xpub};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Network, ScriptBuf};
use serde_json::{json, Value};
use server::database::expiry_attribution::Attribution;
use server::database::Db;
use server_rpc::protos;

fn child(account: &Xpriv, index: u32) -> PublicKey {
	Xpub::from_priv(&SECP, account).ckd_pub(&SECP, ChildNumber::from_normal_idx(index).unwrap()).unwrap().public_key
}

async fn owner(db: &Db, key: PublicKey) -> Attribution {
	db.read(async |tx| tx.expiry_owner(key, Network::Regtest).await).await.unwrap()
}

async fn storage(db: &Db) -> Value {
	db.read(async |tx| {
		let row = tx.query_one("SELECT COUNT(*) FROM fallback_record", &[]).await?;
		Ok(json!({"records": row.get::<_, i64>(0)}))
	}).await.unwrap()
}

#[tokio::test]
#[ignore = "explicit signed RPC registration/positive attribution scale measurement"]
async fn expiry_registration_authenticated_scale() {
	let ctx = TestContext::new("server/expiry_registration_authenticated_scale").await;
	let mut all_resolved = true;
	for count in [100_u32, 1_000, 10_000] {
		let srv = ctx.captaind(&format!("scale_{count}")).bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().create().await;
		let postgres = srv.config().postgres.clone();
		let db = Db::connect(&postgres).await.unwrap();
		let mut rpc = srv.get_public_rpc().await;
		let mut records = Vec::with_capacity(count as usize);
		for index in 0..count {
			let mut seed = [0_u8; 64];
			seed[..4].copy_from_slice(&index.to_le_bytes());
			seed[63] = 93;
			let account = Xpriv::new_master(Network::Regtest, &seed).unwrap();
			let mailbox = account.to_keypair(&SECP);
			let script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111").unwrap();
			let record = FallbackRecord::new(Network::Regtest, script, &account, &mailbox).unwrap();
			records.push((account, mailbox.public_key(), record));
		}
		records.sort_by_key(|(_, mailbox, _)| mailbox.to_string());
		for (_, mailbox, record) in &records {
			let encoded = record.serialize();
			let actual = rpc.set_fallback(protos::SetFallbackRequest {
				mailbox_pk: mailbox.serialize().to_vec(), record: encoded.clone(),
			}).await.unwrap().into_inner().record;
			assert_eq!(actual, encoded, "server must authenticate and store the signed record");
		}
		let registration_storage = storage(&db).await;
		assert_eq!(registration_storage["records"].as_i64(), Some(i64::from(count)));

		// Remove only this owned server's automatic scanner from the measurement.
		// The database remains real; the public RPC is no longer exercised below.
		drop(rpc);
		srv.stop().await.unwrap();
		let (account, mailbox, _) = records.last().unwrap();
		let wrong = MailboxIdentifier::from_pubkey(records.first().unwrap().1);
		for (index, candidate) in [
			(17, Some(MailboxIdentifier::from_pubkey(*mailbox))),
			(18, None),
			(19, Some(wrong)),
		] {
			let key = child(account, index);
			db.write(async |tx| tx.queue_expiry_owner(key, candidate).await).await.unwrap();
			let resolved = tokio::time::timeout(Duration::from_secs(120), async {
				loop {
					match owner(&db, key).await {
						Attribution::Owned { mailbox_pk, child_index, .. } => {
							assert_eq!((mailbox_pk, child_index), (*mailbox, index));
							break;
						},
						Attribution::NoRegisteredOwner => panic!("registered owner was misclassified"),
						Attribution::Unresolved => {},
					}
					let stats = db.scan_expiry_owners(Network::Regtest, 10_000, 2).await.unwrap();
					assert!(stats.children_derived > 0, "pending key made no progress");
				}
			}).await.is_ok();
			if resolved {
				for _ in 0..100 { assert!(matches!(owner(&db, key).await, Attribution::Owned { .. })); }
			}

			// Stop this size after a timeout so a pending key does not affect the next case.
			if !resolved { all_resolved = false; break; }
		}
	}
	assert!(all_resolved, "attribution exceeded its 120-second lookup budget");
}
