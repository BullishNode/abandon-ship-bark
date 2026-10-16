//! Baseline-compatible regression for the fixed read-only CLI seam.

use std::time::Duration;

use ark_testing::{btc, TestContext};
use serde_json::Value;

#[tokio::test]
async fn wallet_inventory_fixed_cli_returns_public_owning_source() {
	let ctx = TestContext::new("server/wallet_inventory_fixed_cli_returns_public_owning_source").await;
	let srv = ctx.captaind("server").funded(btc(1)).no_vtxo_pool().create().await;
	let address = srv.config().rpc.admin_address.unwrap().to_string();
	let mut command = srv.get_custom_command(&["rpc", "--addr", &address, "wallet-inventory"]).await.unwrap();
	let output = tokio::time::timeout(Duration::from_secs(30), command.output()).await.unwrap().unwrap();
	assert!(output.status.success(), "fixed wallet inventory unavailable: {}", String::from_utf8_lossy(&output.stderr));
	assert!(output.stdout.len() < 2 * 1024 * 1024);
	let source: Value = serde_json::from_slice(&output.stdout).unwrap();
	assert_eq!(source["complete"], false);
	assert!(source["watchman"].is_null());
	assert_eq!(source["rounds"]["wallet"], "rounds");
	let descriptor = source["rounds"]["public_descriptor"].as_str().unwrap();
	assert!(!descriptor.contains("tprv") && !descriptor.contains("xprv"));
	assert_eq!(source["rounds"]["pending_transaction_scope"], "canonical_wallet_graph_only");
	assert!(source["rounds"]["pending_transactions"].is_array());
	assert!(source["rounds"]["pending_parents"].is_array());
}
