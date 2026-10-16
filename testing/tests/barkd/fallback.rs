use std::str::FromStr;

use ark_testing::TestContext;
use bitcoin::{Address, Network};

#[tokio::test]
async fn fallback_registration_is_exposed_through_the_actual_native_barkd() {
	let ctx = TestContext::new("barkd/fallback_registration_status").await;
	let srv = ctx.captaind("server").create().await;
	let barkd = ctx.barkd("wallet", &srv).create().await;
	let config = barkd.client_config();
	let get = || config.client.get(format!("{}/api/v1/wallet/fallback", config.base_path));
	let first: bark_json::web::FallbackRegistrationInfo = get().send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
	assert!(first.registered);
	let address = Address::from_str(first.payout_address.as_ref().unwrap()).unwrap().require_network(Network::Regtest).unwrap();
	assert_ne!(address, barkd.onchain_address().await, "native app reserves its stable fallback address");
	let second: bark_json::web::FallbackRegistrationInfo = get().send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
	assert_eq!(first.payout_address, second.payout_address, "status reads never change the immutable destination");
}
