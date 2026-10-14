//! Real pinned backend HTTP enrollment. No payments or dispatch qualification.
use ark::SECP;
use ark_testing::{TestContext, ports::pick_port};
use bitcoin::{
	hashes::{Hash, sha256},
	secp256k1::Message,
};
use serde_json::{Value, json};
use std::{
	fs,
	io::Write,
	os::unix::fs::{OpenOptionsExt, PermissionsExt},
	process::Stdio,
	time::Duration,
};
use tokio::{io::AsyncWriteExt, process::{Child, Command}};

const LOADER: &str = "/noah-host-libs/ld-linux-x86-64.so.2";
const BACKEND: &str = "/pinned/noah/noah-server";
const REDIS: &str = "/pinned/redis/redis-server";
const BACKEND_SHA: &str = "40319a6395ef5a255a36416ed3e22917a272a98efa3259eeed8696857de3d280";
const NOAH_PIN: &str = "8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0";

fn child(binary: &str, root: &std::path::Path, log_name: &str) -> Command {
	let log = fs::OpenOptions::new()
		.create_new(true)
		.write(true)
		.mode(0o600)
		.open(root.join(log_name))
		.unwrap();
	let mut command = Command::new(LOADER);
	command.args(["--library-path", "/noah-host-libs", binary]);
	command
		.current_dir(root)
		.stdin(Stdio::null())
		.stdout(log.try_clone().unwrap())
		.stderr(log)
		.kill_on_drop(true);
	command
}

fn start_backend(
	root: &std::path::Path,
	postgres: &server::config::Postgres,
	ark_url: &str,
	port: u16,
	redis_port: u16,
	forwarder: Option<(&str, &str)>,
) -> Child {
	let mut command = child(BACKEND, root, "backend-private.log");
	command
		.env("HOST", "127.0.0.1")
		.env("PORT", port.to_string())
		.env("SERVER_NETWORK", "regtest")
		.env(
			"POSTGRES_URL",
			format!(
				"postgres://postgres@{}:{}/{}",
				postgres.host, postgres.port, postgres.name
			),
		)
		.env("REDIS_URL", format!("redis://127.0.0.1:{redis_port}"))
		.env("ARK_SERVER_URL", ark_url)
		.env("LNURL_DOMAIN", "owned.invalid")
		.env("AUTH_JWT_SECRET", uuid::Uuid::new_v4().to_string())
		.env("EXPO_ACCESS_TOKEN", "unused-local-test-only")
		.env("S3_BUCKET_NAME", "owned-noah-zero-capital")
		.env("EMAIL_DEV_MODE", "true")
		.env("RUN_MAILBOX_WORKER", "false")
		.env("BARKD_FORWARDED_INVOICES_ENABLED", "false")
		.env("AWS_EC2_METADATA_DISABLED", "true")
		.env("AWS_ACCESS_KEY_ID", "unused-local-test-only")
		.env("AWS_SECRET_ACCESS_KEY", "unused-local-test-only")
		.env("AWS_REGION", "us-east-2")
		.env("AWS_ENDPOINT_URL", "http://127.0.0.1:9")
		.env("AWS_ENDPOINT_URL_S3", "http://127.0.0.1:9")
		.env("HTTP_PROXY", "http://127.0.0.1:9")
		.env("HTTPS_PROXY", "http://127.0.0.1:9")
		.env("ALL_PROXY", "http://127.0.0.1:9")
		.env("http_proxy", "http://127.0.0.1:9")
		.env("https_proxy", "http://127.0.0.1:9")
		.env("all_proxy", "http://127.0.0.1:9")
		.env("no_proxy", "127.0.0.1,localhost")
		.env("NO_PROXY", "127.0.0.1,localhost")
		.env_remove("SENTRY_URL")
		.env_remove("COINGECKO_DEMO_API_KEY")
		.env("RUST_LOG", "error")
		.env("TOKIO_WORKER_THREADS", "2");
	if let Some((url, token)) = forwarder {
		command
			.env("BARKD_FORWARDED_INVOICES_ENABLED", "true")
			.env("BARKD_URL", url)
			.env("BARKD_AUTH_TOKEN", token);
	}
	command.spawn().unwrap()
}

async fn stop(child: &mut Child) {
	if child.try_wait().unwrap().is_none() {
		child.kill().await.unwrap();
	}
	child.wait().await.unwrap();
}

async fn challenge(client: &reqwest::Client, origin: &str) -> String {
	let response = client
		.get(format!("{origin}/v0/getk1"))
		.send()
		.await
		.unwrap();
	assert!(
		response.status().is_success(),
		"real challenge endpoint refused"
	);
	let body: Value = response.json().await.unwrap();
	body["k1"]
		.as_str()
		.expect("actual challenge missing")
		.to_owned()
}

const INVOICE_VERIFIER: &str = "/build/targets/sweep-simulator-native/debug/noah-public-proof";
const INVOICE_VERIFIER_SHA: &str =
	"d15d058dccc1ad38ca1fe60a5f05daa4ffa0890d63678817b2af6f4068904dcd";

async fn verify_actual_invoice(invoice: &str, metadata: &str) -> std::process::Output {
	assert_eq!(
		sha256::Hash::hash(&fs::read(INVOICE_VERIFIER).unwrap()).to_string(),
		INVOICE_VERIFIER_SHA
	);
	let mut child = Command::new(INVOICE_VERIFIER)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.unwrap();
	let request = json!({"invoice":invoice,"metadata":metadata,"amount_msat":12_000_000,"network":"regtest","now":chrono::Utc::now().timestamp() as u64});
	child
		.stdin
		.take()
		.unwrap()
		.write_all(&serde_json::to_vec(&request).unwrap())
		.await
		.unwrap();
	tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
		.await
		.unwrap()
		.unwrap()
}

#[tokio::test]
async fn noah_first_original_invoice_commits_discovery_metadata() {
	let ctx =
		TestContext::new("noah-metadata/first_original_invoice_commits_discovery_metadata").await;
	fs::set_permissions(&ctx.datadir, fs::Permissions::from_mode(0o700)).unwrap();
	assert_eq!(
		sha256::Hash::hash(&fs::read(BACKEND).unwrap()).to_string(),
		BACKEND_SHA
	);
	// Deliberately don't use new_lightning_setup_no_channel: it funds 12 BTC.
	let node = ctx.lightningd("owned_unfunded_invoice_node").create().await;
	let before = node
		.grpc_client()
		.await
		.list_funds(cln_rpc::ListfundsRequest { spent: Some(false) })
		.await
		.unwrap()
		.into_inner();
	assert!(
		before.outputs.is_empty() && before.channels.is_empty(),
		"unpaid compatibility fixture must have no node capital/channel"
	);
	let srv = ctx
		.captaind("server")
		.lightningd(&node)
		.no_vtxo_pool()
		.create()
		.await;
	let wallet = ctx
		.bark_sdk("fresh-forwarded-phone", &srv)
		.cfg(|c| c.daemon_manual_sync = true)
		.create()
		.await;
	wallet.stop_daemon_wait().await.unwrap();
	let address = wallet.peek_address(0).await.unwrap().to_string();
	let key = wallet.peek_keypair(0).await.unwrap();
	let forwarder = ctx
		.barkd("owned_forwarder", &srv)
		.env("BARKD_NOAH_LNURL_DOMAIN", "owned.invalid")
		.cfg(|c| c.daemon_manual_sync = true)
		.create()
		.await;
	let token = fs::read_to_string(forwarder.datadir().join("auth_token")).unwrap();
	let noah_pg = ctx.new_postgres("owned_noah_unpaid_forwarder").await;
	let redis_port = pick_port();
	let port = pick_port();
	let mut redis = child(REDIS, &ctx.datadir, "redis-private.log")
		.args([
			"--bind",
			"127.0.0.1",
			"--port",
			&redis_port.to_string(),
			"--save",
			"",
			"--appendonly",
			"no",
		])
		.spawn()
		.unwrap();
	let mut backend = start_backend(
		&ctx.datadir,
		&noah_pg,
		&srv.ark_url(),
		port,
		redis_port,
		Some((&forwarder.base_url(), token.trim())),
	);
	let origin = format!("http://127.0.0.1:{port}");
	let client = reqwest::Client::builder()
		.no_proxy()
		.timeout(Duration::from_secs(15))
		.build()
		.unwrap();
	let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
	loop {
		if client
			.get(format!("{origin}/health"))
			.send()
			.await
			.is_ok_and(|r| r.status().is_success())
		{
			break;
		}
		assert!(
			tokio::time::Instant::now() < deadline && backend.try_wait().unwrap().is_none(),
			"real forwarder backend failed health; private logs retained"
		);
		tokio::time::sleep(Duration::from_millis(200)).await;
	}
	let issued = challenge(&client, &origin).await;
	let digest = bitcoin::sign_message::signed_msg_hash(&issued);
	let response = client.post(format!("{origin}/v0/auth/login")).json(&json!({"key":key.public_key().to_string(),"sig":SECP.sign_ecdsa(&Message::from_digest(digest.to_byte_array()),&key.secret_key()).to_string(),"k1":issued})).send().await.unwrap();
	assert!(response.status().is_success(), "real phone auth failed");
	let reply: Value = response.json().await.unwrap();
	let bearer = reply["access_token"].as_str().unwrap();
	let response = client
		.post(format!("{origin}/v0/register"))
		.bearer_auth(bearer)
		.json(&json!({"ln_address":"forwardedphone@owned.invalid","ark_address":address}))
		.send()
		.await
		.unwrap();
	assert!(
		response.status().is_success(),
		"real phone registration failed"
	);
	let discovery: Value = client
		.get(format!("{origin}/.well-known/lnurlp/forwardedphone"))
		.send()
		.await
		.unwrap()
		.json()
		.await
		.unwrap();
	let metadata = discovery["metadata"].as_str().unwrap();
	assert_eq!(
		discovery["callback"],
		"https://owned.invalid/.well-known/lnurlp/forwardedphone"
	);
	assert!(node.grpc_client().await.list_invoices(cln_rpc::ListinvoicesRequest::default()).await.unwrap().into_inner().invoices.is_empty());
	// One callback only, no Ark negotiation and no payment/retry dispatch.
	let response = client
		.get(format!("{origin}/.well-known/lnurlp/forwardedphone"))
		.query(&[("amount", "12000000")])
		.send()
		.await
		.unwrap();
	assert!(
		response.status().is_success(),
		"real Noah -> fork forwarder unpaid invoice refused; not payment-qualified"
	);
	let body: Value = response.json().await.unwrap();
	let invoice = body["pr"].as_str().expect("real forwarding invoice absent");
	let mut original = fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(ctx.datadir.join("original-invoice-private.json")).unwrap();
	original.write_all(serde_json::to_vec(&json!({"invoice":invoice,"metadata":metadata,"address":address,"amount_msat":12_000_000})).unwrap().as_slice()).unwrap();
	let result = verify_actual_invoice(invoice, metadata).await;
	assert!(result.status.success(), "first original Noah invoice must commit exact discovery metadata: {}", String::from_utf8_lossy(&result.stderr));
	let created = node.grpc_client().await.list_invoices(cln_rpc::ListinvoicesRequest::default()).await.unwrap().into_inner();
	assert_eq!(created.invoices.len(), 1, "first callback must create exactly one original invoice");
	let endpoint = format!("{}/api/v1/lightning/receives/invoice/for-address", forwarder.base_url());
	for label in ["ordinary memo", "Paying satoshis to forwardedphone@other.invalid", "Paying satoshis to Forwardedphone@owned.invalid", "Paying satoshis to forwardedphone@owned.invalid "] {
		let refused = client.post(&endpoint).bearer_auth(token.trim())
			.json(&json!({"address":address,"amount_sat":12_000,"description":label})).send().await.unwrap();
		assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
	}
	let unauthorized = client.post(&endpoint).json(&json!({"address":address,"amount_sat":12_000,"description":"Paying satoshis to forwardedphone@owned.invalid"})).send().await.unwrap();
	assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
	let count = node.grpc_client().await.list_invoices(cln_rpc::ListinvoicesRequest::default()).await.unwrap().into_inner().invoices.len();
	assert_eq!(count, 1, "refused requests must not issue another invoice");
	let public = json!({"backend_pin":NOAH_PIN,"backend_sha256":BACKEND_SHA,"fixture_head":env!("GIT_HASH"),"first_original_invoice_sha256":sha256::Hash::hash(invoice.as_bytes()).to_string(),"metadata_sha256":sha256::Hash::hash(metadata.as_bytes()).to_string(),"node_invoice_count":count,"strict_original_verification":true,"payments_dispatched":0,"wallet_funding_sat":0,"paid_journey_qualified":false});
	fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(ctx.datadir.join("public-result.json")).unwrap().write_all(&serde_json::to_vec_pretty(&public).unwrap()).unwrap();
	let after = node.grpc_client().await.list_funds(cln_rpc::ListfundsRequest { spent: Some(false) }).await.unwrap().into_inner();
	assert!(after.outputs.is_empty() && after.channels.is_empty());
	assert_eq!(wallet.balance().await.unwrap().total().to_sat(), 0);
	stop(&mut backend).await;
	stop(&mut redis).await;
	forwarder.stop().await.unwrap();
	node.stop().await.unwrap();
}
