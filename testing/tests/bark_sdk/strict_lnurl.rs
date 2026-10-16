use std::path::{Path, PathBuf};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use bitcoin::{Amount, hashes::{sha256, Hash}, secp256k1::{Message, Secp256k1, SecretKey}};
use bark::lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use bark::lnurllib::lnurl::LnUrl;
use bark::persist::{BarkPersister, sqlite::SqliteClient};
use bark::{LightningSendOptions, actions::lightning::pay::LightningSendState};
use ark::lightning::Preimage;
use ark_testing::{TestContext, btc, util::FutureExt};
use cln_rpc::plugins::hold;

// A test-only TLS server. Certificates and private keys are generated inside
// this new fixture; no application key material or certificate override is used.
struct StrictTlsFixture { child: Child, dir: PathBuf, endpoint: String, ca: Vec<u8> }
impl Drop for StrictTlsFixture {
	fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}
impl StrictTlsFixture {
	async fn start(dir: PathBuf) -> Self {
		std::fs::create_dir(&dir).unwrap();
		let log = std::fs::File::create(dir.join("openssl.stderr.log")).unwrap();
		let openssl = |args: &[&str]| {
			let status = Command::new("openssl").current_dir(&dir).args(args)
				.stdout(Stdio::null()).stderr(log.try_clone().unwrap()).status().unwrap();
			assert!(status.success(), "actual openssl fixture command failed: {args:?}");
		};
		openssl(&[
			"req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
			"-days", "1", "-subj", "/CN=strict LNURL ephemeral test CA",
			"-addext", "basicConstraints=critical,CA:TRUE",
			"-addext", "keyUsage=critical,keyCertSign,cRLSign",
			"-keyout", "ca-key.pem", "-out", "ca.pem",
		]);
		openssl(&[
			"req", "-new", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
			"-subj", "/CN=localhost", "-keyout", "key.pem", "-out", "leaf.csr",
		]);
		std::fs::write(dir.join("leaf-extensions.cnf"),
			"basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nauthorityKeyIdentifier=keyid,issuer\n").unwrap();
		openssl(&[
			"x509", "-req", "-in", "leaf.csr", "-CA", "ca.pem", "-CAkey", "ca-key.pem",
			"-CAcreateserial", "-days", "1", "-extfile", "leaf-extensions.cnf", "-out", "cert.pem",
		]);
		openssl(&["verify", "-CAfile", "ca.pem", "-purpose", "sslserver", "-verify_hostname", "localhost", "cert.pem"]);
		openssl(&["verify", "-CAfile", "ca.pem", "-purpose", "sslserver", "-verify_ip", "127.0.0.1", "cert.pem"]);
		let script = dir.join("server.py");
		std::fs::write(&script, r#"import http.server,json,ssl,sys,pathlib,urllib.parse
p=pathlib.Path(sys.argv[1])
class H(http.server.BaseHTTPRequestHandler):
	def log_message(self,*args): pass
	def do_GET(self):
		with (p/'requests.jsonl').open('a') as f: f.write(json.dumps({'path':self.path})+'\n')
		mode=json.loads((p/'response.json').read_text())
		invoice='amount' in urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
		if mode.get('redirect_metadata') or (invoice and mode.get('redirect_invoice')):
			self.send_response(302); self.send_header('Location', 'https://localhost:'+str(self.server.server_port)+'/redirect-target'); self.send_header('Content-Length','0'); self.end_headers(); return
		body=(mode['invoice'] if invoice else mode['metadata']).encode()
		if mode.get('oversize_stream') and not invoice: body=b' '*65537
		self.send_response(200); self.send_header('Content-Type','application/json')
		if not mode.get('oversize_stream'): self.send_header('Content-Length',str(len(body)))
		self.end_headers(); self.wfile.write(body)
s=http.server.HTTPServer(('127.0.0.1',0),H)
c=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); c.load_cert_chain(p/'cert.pem',p/'key.pem'); s.socket=c.wrap_socket(s.socket,server_side=True)
(p/'port').write_text(str(s.server_port)); s.serve_forever()
"#).unwrap();
		let stderr = std::fs::File::create(dir.join("https-server.stderr.log")).unwrap();
		let child = Command::new("python3").arg(&script).arg(&dir)
			.stdout(Stdio::null()).stderr(stderr).spawn().unwrap();
		let mut fixture = Self { child, ca: std::fs::read(dir.join("ca.pem")).unwrap(), dir, endpoint: String::new() };
		async {
			while !fixture.dir.join("port").exists() {
				assert!(fixture.child.try_wait().unwrap().is_none(), "TLS fixture exited before binding");
				tokio::time::sleep(Duration::from_millis(20)).await;
			}
		}.wait_millis(5_000).await;
		let port: u16 = std::fs::read_to_string(fixture.dir.join("port")).unwrap().parse().unwrap();
		fixture.endpoint = format!("https://localhost:{port}/pay");
		fixture
	}
	fn metadata(&self, raw: &str, callback: Option<&str>) -> String {
		serde_json::json!({"tag":"payRequest","callback":callback.unwrap_or(&self.endpoint),
			"minSendable":1000,"maxSendable":100_000_000,"metadata":raw}).to_string()
	}
	fn configure(&self, metadata: String, invoice: String, mode: Option<&str>) {
		let mut config = serde_json::json!({"metadata":metadata,"invoice":serde_json::json!({"pr":invoice}).to_string()});
		if let Some(mode) = mode { config[mode] = true.into(); }
		let pending = self.dir.join("response.pending.json");
		std::fs::write(&pending, config.to_string()).unwrap();
		std::fs::rename(&pending, self.dir.join("response.json")).unwrap();
	}
	fn opts(&self) -> LightningSendOptions {
		LightningSendOptions::default().require_lnurl_metadata(true).lnurl_root_ca_pem(self.ca.clone())
	}
	fn requested_paths(&self) -> Vec<String> {
		std::fs::read_to_string(self.dir.join("requests.jsonl")).unwrap_or_default().lines()
			.map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["path"].as_str().unwrap().to_owned()).collect()
	}
}

fn signed_test_invoice(raw_metadata: &str, amount: Amount, currency: Currency, direct: bool) -> String {
	let key = SecretKey::from_slice(&[3; 32]).unwrap();
	let builder = InvoiceBuilder::new(currency);
	let builder = if direct { builder.description("direct description".into()) }
		else { builder.description_hash(sha256::Hash::hash(raw_metadata.as_bytes())) };
	builder.payment_hash(sha256::Hash::hash(b"strict TLS test only synthetic payment hash"))
		.payment_secret(PaymentSecret([5; 32]))
		.duration_since_epoch(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap())
		.min_final_cltv_expiry_delta(18).amount_milli_satoshis(amount.to_sat() * 1000)
		.build_signed(|message: &Message| Secp256k1::new().sign_ecdsa_recoverable(message, &key)).unwrap().to_string()
}

fn balance_fields(balance: &bark::Balance) -> [u64; 9] {
	[balance.spendable, balance.needs_refresh, balance.pending_lightning_send,
		balance.claimable_lightning_receive, balance.pending_in_round, balance.pending_exit,
		balance.pending_board, balance.pending_arkoor_send, balance.pending_offboard].map(|v| v.to_sat())
}

fn digest(value: impl AsRef<[u8]>) -> String {
	sha256::Hash::hash(value.as_ref()).to_string()
}

fn control_row(output: &mut std::fs::File, row: serde_json::Value) {
	serde_json::to_writer(&mut *output, &row).unwrap();
	output.write_all(b"\n").unwrap();
	output.sync_all().unwrap();
}

async fn checkpoints(db: &Path) -> String {
	let storage = SqliteClient::open(db).unwrap();
	serde_json::to_string(&storage.get_all_wallet_action_checkpoints().await.unwrap()).unwrap()
}

/// Real TLS and signed responses enter the public SDK resolver. Every refused
/// response must leave the actual persisted action rows, movements and funds
/// unchanged. The positive response then pays an actual external CLN invoice.
/// This uses an opaque LNURL on an ephemeral port: address/identifier routing
/// remains separately qualified against the owned Noah endpoint.
#[tokio::test]
async fn strict_lnurl_private_tls_refusals_preserve_checkpoint_then_real_payment_settles() {
	let ctx = TestContext::new("bark_sdk/strict_lnurl_private_tls_refusals_preserve_checkpoint_then_real_payment_settles").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	let board_amount = btc(0.02);
	let wallet = ctx.bark_sdk("payer", &srv).boarded(board_amount).create().await;
	lightning.sync().await;
	let fixture = StrictTlsFixture::start(ctx.datadir.join("lnurl-tls")).await;
	let lnurl = LnUrl::from_url(fixture.endpoint.clone());
	let db = ctx.datadir.join("payer/db.sqlite");
	let opening_cp = checkpoints(&db).await;
	let opening_movements = serde_json::to_string(&wallet.movements().await.unwrap()).unwrap();
	let opening_balance = balance_fields(&ark_testing::balance::assert_balance_consistent(&wallet, false).await);
	let mut control_evidence = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
		.open(ctx.datadir.join("strict-lnurl-controls.private.jsonl")).unwrap();
	let amount = Amount::from_sat(2_000);
	let metadata = "[[\"text/plain\",\"strict native TLS test\"]]";
	let valid = signed_test_invoice(metadata, amount, Currency::Regtest, false);
	let normal = fixture.metadata(metadata, None);
	let cases = vec![
		("wrong-metadata-hash", normal.clone(), signed_test_invoice("other bytes", amount, Currency::Regtest, false), None),
		("direct-description", normal.clone(), signed_test_invoice(metadata, amount, Currency::Regtest, true), None),
		("wrong-amount", normal.clone(), signed_test_invoice(metadata, Amount::from_sat(1_999), Currency::Regtest, false), None),
		("wrong-network", normal.clone(), signed_test_invoice(metadata, amount, Currency::Bitcoin, false), None),
		("simnet-is-not-regtest", normal.clone(), signed_test_invoice(metadata, amount, Currency::Simnet, false), None),
		("invalid-invoice", normal.clone(), "not a signed invoice".into(), None),
		("malformed-metadata", fixture.metadata("invalid JSON", None), valid.clone(), None),
		("foreign-callback", fixture.metadata(metadata, Some("https://foreign.invalid/pay")), valid.clone(), None),
		("http-callback", fixture.metadata(metadata, Some("http://localhost/pay")), valid.clone(), None),
		("metadata-redirect", normal.clone(), valid.clone(), Some("redirect_metadata")),
		("invoice-redirect", normal.clone(), valid.clone(), Some("redirect_invoice")),
		("actual-stream-too-large", normal.clone(), valid.clone(), Some("oversize_stream")),
	];
	for (name, response, invoice, mode) in cases {
		fixture.configure(response, invoice, mode);
		let http_before = fixture.requested_paths().len();
		let result = wallet.pay_lnurl_with(&lnurl, amount, None::<&str>, fixture.opts()).await;
		assert!(result.is_err(), "{name} unexpectedly dispatched");
		let error = format!("{:#}", result.unwrap_err());
		let expected_refusal = match name {
			"wrong-metadata-hash" | "direct-description" => "does not commit to the exact advertised metadata",
			"wrong-amount" => "but we requested",
			"wrong-network" | "simnet-is-not-regtest" => "Invoice is for wrong network",
			"invalid-invoice" => "received invalid invoice",
			"malformed-metadata" => "invalid strict lnurl metadata",
			"foreign-callback" => "callback does not match the requested recipient endpoint",
			"http-callback" => "requires an HTTPS endpoint",
			"metadata-redirect" | "invoice-redirect" => "strict lnurl endpoint redirected",
			"actual-stream-too-large" => "strict lnurl response exceeds size limit",
			other => panic!("undeclared native refusal control: {other}"),
		};
		assert!(error.contains(expected_refusal), "{name} failed outside its required guard: {error}");
		let after_cp = checkpoints(&db).await;
		let after_movements = serde_json::to_string(&wallet.movements().await.unwrap()).unwrap();
		let after_balance = balance_fields(&ark_testing::balance::assert_balance_consistent(&wallet, false).await);
		let http_after = fixture.requested_paths().len();
		let minimum_http = if matches!(name, "malformed-metadata" | "foreign-callback" | "http-callback" | "metadata-redirect" | "actual-stream-too-large") { 1 } else { 2 };
		assert!(http_after >= http_before + minimum_http, "{name} did not reach the real TLS HTTP response");
		assert_eq!(after_cp, opening_cp, "{name} changed actual checkpoint rows");
		assert_eq!(after_movements, opening_movements, "{name} changed movements");
		assert_eq!(after_balance, opening_balance, "{name} changed balances");
		control_row(&mut control_evidence, serde_json::json!({"control":name,"outcome":"REFUSED_BEFORE_CHECKPOINT","validated_refusal_guard":expected_refusal,
			"http_path_count_before":http_before,"http_path_count_after":http_after,
			"checkpoint_before_sha256":digest(&opening_cp),"checkpoint_after_sha256":digest(&after_cp),
			"movement_before_sha256":digest(&opening_movements),"movement_after_sha256":digest(&after_movements),
			"balance_before_sha256":digest(serde_json::to_vec(&opening_balance).unwrap()),
			"balance_after_sha256":digest(serde_json::to_vec(&after_balance).unwrap())}));
		assert!(wallet.pending_lightning_sends().await.unwrap().is_empty());
	}
	assert!(fixture.requested_paths().iter().all(|path| !path.starts_with("/redirect-target")));
	fixture.configure(normal, valid, None);
	let before = fixture.requested_paths().len();
	let without_ca = LightningSendOptions::default().require_lnurl_metadata(true);
	assert!(wallet.pay_lnurl_with(&lnurl, amount, None::<&str>, without_ca).await.is_err());
	assert_eq!(fixture.requested_paths().len(), before, "untrusted TLS certificate reached HTTP handler");
	let after_untrusted = checkpoints(&db).await;
	assert_eq!(after_untrusted, opening_cp);
	control_row(&mut control_evidence, serde_json::json!({"control":"untrusted-local-certificate","outcome":"TLS_REFUSED_BEFORE_HTTP_AND_CHECKPOINT",
		"http_path_count_before":before,"http_path_count_after":fixture.requested_paths().len(),
		"checkpoint_before_sha256":digest(&opening_cp),"checkpoint_after_sha256":digest(&after_untrusted)}));

	let preimage = Preimage::random();
	let payment_hash = preimage.compute_payment_hash();
	let mut hold_client = lightning.external.hold_client().await;
	let invoice = hold_client.invoice(hold::InvoiceRequest {
		payment_hash: payment_hash.as_ref().to_vec(), amount_msat: amount.to_sat() * 1_000,
		description: Some(hold::invoice_request::Description::Hash(sha256::Hash::hash(metadata.as_bytes()).to_byte_array().to_vec())),
		min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	fixture.configure(fixture.metadata(metadata, None), invoice, None);
	let paid_invoice = wallet.pay_lnurl_with(&lnurl, amount, None::<&str>, fixture.opts()).await.unwrap();
	assert_eq!(paid_invoice.payment_hash(), payment_hash);
	async {
		loop {
			let invoices = hold_client.list(hold::ListRequest { constraint: Some(hold::list_request::Constraint::PaymentHash(payment_hash.as_ref().to_vec())) }).await.unwrap().into_inner().invoices;
			if invoices.first().is_some_and(|invoice| invoice.state() == hold::InvoiceState::Accepted) { break; }
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}.wait_millis(30_000).await;
	hold_client.settle(hold::SettleRequest { payment_preimage: preimage.as_ref().to_vec() }).await.unwrap();
	let status = wallet.check_lightning_payment(payment_hash, true).wait_millis(30_000).await.unwrap();
	match status { LightningSendState::Paid(paid) => assert_eq!(paid.preimage, preimage), other => panic!("actual external CLN payment not settled: {other:?}") }
	let closing = ark_testing::balance::assert_balance_consistent(&wallet, false).await;
	assert_eq!(closing.spendable, board_amount - amount);
	assert!(wallet.pending_lightning_sends().await.unwrap().is_empty());
	control_row(&mut control_evidence, serde_json::json!({"control":"real-external-cln-positive","outcome":"PAID_AND_SETTLED",
		"amount_sat":amount.to_sat(),"spendable_before_sat":board_amount.to_sat(),"spendable_after_sat":closing.spendable.to_sat(),
		"http_path_count_after":fixture.requested_paths().len(),"checkpoint_after_sha256":digest(checkpoints(&db).await)}));
}
