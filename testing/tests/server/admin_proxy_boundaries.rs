use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;

use chrono::Local;
use serde_json::{json, Value};
use tokio::process::Command;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use ark::integration::TokenType;
use ark_testing::{Captaind, TestContext};
use ark_testing::daemon::watchmand::Watchmand;
use ark_testing::util::wait_timeout;
use server::config::{self, Config};
use server::database::Db;
use server::filters::Filters;
use server_rpc::protos::intman::{
	self, integration_service_client::IntegrationServiceClient,
};

const BIND_ERROR: &str = "admin_address must use a loopback IP address";
const SOURCE_DENIED: &str = "Client's address is not allowed";
const SOURCE_MISSING: &str = "Client's address cannot be found";

type IntegrationClient = IntegrationServiceClient<Channel>;

// Public observations deliberately exclude API keys, tokens and full config contents.
fn record(ctx: &TestContext, observation: Value) {
	let mut file = OpenOptions::new().create(true).append(true)
		.open(ctx.datadir.join("boundary-observations.jsonl")).unwrap();
	serde_json::to_writer(&mut file, &observation).unwrap();
	file.write_all(b"\n").unwrap();
}

async fn native_config_command(
	ctx: &TestContext,
	mut command: Command,
	config: &Path,
	watchman: bool,
	start: bool,
	allowed: bool,
	address: Option<&str>,
) {
	let executable = command.as_std().get_program().to_string_lossy().into_owned();
	if start {
		command.arg("start").arg("--config").arg(config);
	} else {
		command.arg("check-config").arg(config);
		if watchman { command.arg("--watchman"); }
	}
	command.kill_on_drop(true);
	let output = tokio::time::timeout(wait_timeout(), command.output()).await
		.expect("native config command did not terminate").expect("native command spawn failed");
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	let refused_admin_bind = stderr.contains(BIND_ERROR);
	let valid_config = stdout.contains(if watchman { "Watchmand config is valid" } else { "Captaind config is valid" });
	record(ctx, json!({
		"case": "admin-bind", "daemon": if watchman { "watchmand" } else { "captaind" },
		"command": if start { "start" } else { "check-config" },
		"executable": executable, "config_file": config.file_name().unwrap().to_string_lossy(), "admin_address": address,
		"status": output.status.code(), "success": output.status.success(),
		"admin_bind_refusal": refused_admin_bind, "valid_config": valid_config,
	}));
	assert_eq!(output.status.success(), allowed, "unexpected native config command outcome");
	if !allowed { assert!(refused_admin_bind, "refusal must come from the admin bind guard"); }
	if allowed { assert!(valid_config, "success must come from the config validation command"); }
}

#[tokio::test]
async fn admin_bind_native_config_and_start_refuse_non_loopback() {
	let ctx = TestContext::new_minimal("server/admin_bind_native_config_and_start_refuse_non_loopback").await;
	let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
	let mut captain = Config::load(workspace.join("server/captaind.default.toml")).unwrap();
	let mut watchman = config::watchmand::Config::load(workspace.join("server/watchmand.default.toml")).unwrap();
	captain.data_dir = ctx.datadir.join("unused-captaind-data");
	watchman.data_dir = ctx.datadir.join("unused-watchmand-data");

	for (index, (address, allowed)) in [
		(None, true), (Some("127.0.0.1:0"), true), (Some("127.9.8.7:0"), true), (Some("[::1]:0"), true),
		(Some("0.0.0.0:0"), false), (Some("[::]:0"), false), (Some("192.168.1.2:0"), false),
		(Some("198.51.100.7:0"), false), (Some("[2001:db8::7]:0"), false), (Some("[::ffff:127.0.0.1]:0"), false),
	].into_iter().enumerate() {
		let parsed = address.map(|a| a.parse::<std::net::SocketAddr>().unwrap());
		captain.rpc.admin_address = parsed;
		watchman.admin_address = parsed;
		let captain_path = ctx.datadir.join(format!("captaind-check-{index}.toml"));
		let watchman_path = ctx.datadir.join(format!("watchmand-check-{index}.toml"));
		captain.write_into(&mut fs::File::create(&captain_path).unwrap()).unwrap();
		watchman.write_into(&mut fs::File::create(&watchman_path).unwrap()).unwrap();
		native_config_command(&ctx, Captaind::base_cmd(), &captain_path, false, false, allowed, address).await;
		native_config_command(&ctx, Captaind::base_cmd(), &watchman_path, true, false, allowed, address).await;
		if !allowed {
			// Invoke the actual binaries directly: daemon reservations must not rewrite the bad bind.
			native_config_command(&ctx, Captaind::base_cmd(), &captain_path, false, true, false, address).await;
			native_config_command(&ctx, Watchmand::base_cmd(), &watchman_path, true, true, false, address).await;
		}
	}
	assert!(!captain.data_dir.exists(), "invalid startup must fail before opening daemon state");
	assert!(!watchman.data_dir.exists(), "invalid startup must fail before opening daemon state");
}

fn forwarded<T>(message: T, chain: Option<&str>) -> Request<T> {
	let mut request = Request::new(message);
	if let Some(chain) = chain {
		request.metadata_mut().insert("x-forwarded-for", chain.parse().unwrap());
	}
	request
}

async fn connect(srv: &Captaind) -> IntegrationClient {
	let channel = Channel::from_shared(srv.integration_url()).unwrap().connect().await.unwrap();
	IntegrationClient::new(channel)
}

// Provision genuine rows through the same database methods as integration CLI commands,
// keeping the random bearer credentials out of CLI arguments and trace output.
async fn provision(srv: &Captaind) -> (String, String, String) {
	let postgres = srv.config().postgres.clone();
	let db = Db::connect(&postgres).await.unwrap();
	let integration = db.write(async |t| t.store_integration("source-boundary").await).await.unwrap();
	let direct = uuid::Uuid::new_v4();
	let remote = uuid::Uuid::new_v4();
	for (name, key, ips) in [
		("direct", direct, vec!["127.0.0.1".to_owned()]),
		("remote", remote, vec!["198.51.100.7".to_owned(), "2001:db8::7".to_owned()]),
	] {
		db.write(async |t| t.store_integration_api_key(
			name, key, &Filters::init(ips, Vec::new()), integration.id,
			Local::now() + chrono::Duration::hours(1),
		).await).await.unwrap();
	}
	db.write(async |t| t.store_integration_token_config(
		TokenType::SingleUseBoard, 32, 3600, integration.id,
	).await).await.unwrap();
	let response = connect(srv).await.get_tokens(intman::TokensRequest {
		api_key: direct.to_string(), r#type: intman::TokenType::SingleUseBoard.into(), count: Some(1),
	}).await.expect("actual direct peer must be allowed by its genuine key").into_inner();
	assert_eq!(response.tokens.len(), 1);
	let token = response.tokens.into_iter().next().unwrap().token;
	(direct.to_string(), remote.to_string(), token)
}

fn assert_source_result<T>(
	ctx: &TestContext, label: &str, method: &str, result: Result<Response<T>, Status>,
	denial: Option<&str>,
) {
	// Do not use unwrap_err: an unexpected successful response may contain a live token.
	match result {
		Ok(_) => {
			record(ctx, json!({"case": label, "method": method, "code": "Ok"}));
			assert!(denial.is_none(), "source-restricted request was unexpectedly accepted: {label}/{method}");
		},
		Err(status) => {
			record(ctx, json!({
				"case": label, "method": method, "code": status.code().to_string(),
				"source_denied": status.message().contains(SOURCE_DENIED),
				"source_missing": status.message().contains(SOURCE_MISSING),
			}));
			assert_eq!(status.code(), tonic::Code::InvalidArgument, "unexpected refusal code: {label}/{method}");
			let expected = denial.expect("source-allowed request was unexpectedly refused");
			assert!(status.message().contains(expected), "refusal must come from the source filter: {label}/{method}");
		},
	}
}

async fn check_source(
	ctx: &TestContext, client: &mut IntegrationClient, label: &str,
	key: &str, token: &str, chain: Option<&str>, denial: Option<&str>,
) {
	let issued = client.get_tokens(forwarded(intman::TokensRequest {
		api_key: key.to_owned(), r#type: intman::TokenType::SingleUseBoard.into(), count: Some(1),
	}, chain)).await;
	assert_source_result(ctx, label, "GetTokens", issued, denial);
	let info = client.get_token_info(forwarded(intman::TokenInfoRequest {
		api_key: key.to_owned(), token: token.to_owned(),
	}, chain)).await;
	assert_source_result(ctx, label, "GetTokenInfo", info, denial);
	let updated = client.update_token(forwarded(intman::UpdateTokenRequest {
		api_key: key.to_owned(), token: token.to_owned(), status: intman::TokenStatus::Disabled.into(),
	}, chain)).await;
	assert_source_result(ctx, label, "UpdateToken", updated, denial);
}

fn record_started_config(ctx: &TestContext, srv: &Captaind, label: &str, expected: &[IpAddr]) {
	let path = srv.config().data_dir.join("config.toml");
	let actual = Config::load(path).unwrap();
	assert_eq!(actual.rpc.integration_trusted_proxies, expected);
	record(ctx, json!({
		"case": label, "transport": "tonic TCP", "integration_address": actual.rpc.integration_address,
		"integration_trusted_proxies": actual.rpc.integration_trusted_proxies,
		"key_source_filters": {"direct": ["127.0.0.1"], "remote": ["198.51.100.7", "2001:db8::7"]},
	}));
}

#[tokio::test]
async fn integration_native_source_filters_follow_explicit_proxy_trust_after_restart() {
	let ctx = TestContext::new("server/integration_native_source_filters_follow_explicit_proxy_trust_after_restart").await;
	let direct = ctx.captaind("direct").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool().create().await;
	record_started_config(&ctx, &direct, "direct-start", &[]);
	let (direct_key, remote_key, token) = provision(&direct).await;
	let mut client = connect(&direct).await;
	check_source(&ctx, &mut client, "untrusted-peer-spoof", &remote_key, &token,
		Some("198.51.100.7"), Some(SOURCE_DENIED)).await;
	check_source(&ctx, &mut client, "actual-direct-peer", &direct_key, &token, None, None).await;
	drop(client);
	direct.stop().await.unwrap();

	let trusted: Vec<IpAddr> = vec!["127.0.0.1".parse().unwrap(), "192.0.2.10".parse().unwrap()];
	let builder_trust = trusted.clone();
	let proxy = ctx.captaind("proxy").bitcoind(ctx.bitcoind_arc()).no_vtxo_pool()
		.cfg(move |cfg| cfg.rpc.integration_trusted_proxies = builder_trust).create().await;
	record_started_config(&ctx, &proxy, "trusted-proxy-first-start", &trusted);
	let (_, remote_key, token) = provision(&proxy).await;
	let mut client = connect(&proxy).await;
	for (label, chain) in [
		("trusted-ipv4", "198.51.100.7"), ("trusted-ipv6", "2001:db8::7"),
		("trusted-multi-hop", "198.51.100.7, 192.0.2.10"),
	] {
		check_source(&ctx, &mut client, label, &remote_key, &token, Some(chain), None).await;
	}
	check_source(&ctx, &mut client, "malformed-trusted-chain", &remote_key, &token,
		Some("not-an-ip, 198.51.100.7"), Some(SOURCE_MISSING)).await;
	check_source(&ctx, &mut client, "untrusted-hop-blocks-forged-prefix", &remote_key, &token,
		Some("198.51.100.7, 198.51.100.8, 192.0.2.10"), Some(SOURCE_DENIED)).await;
	check_source(&ctx, &mut client, "trusted-peer-without-attestation", &remote_key, &token,
		None, Some(SOURCE_DENIED)).await;
	drop(client);
	proxy.stop().await.unwrap();
	proxy.start().await.unwrap();
	record_started_config(&ctx, &proxy, "trusted-proxy-restart", &trusted);
	let mut client = connect(&proxy).await;
	check_source(&ctx, &mut client, "trusted-ipv4-after-restart", &remote_key, &token,
		Some("198.51.100.7"), None).await;
	check_source(&ctx, &mut client, "malformed-after-restart", &remote_key, &token,
		Some("198.51.100.7, bad-hop"), Some(SOURCE_MISSING)).await;
	drop(client);
	proxy.stop().await.unwrap();
}
