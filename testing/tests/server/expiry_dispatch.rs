use std::time::Duration;

use ark::arkoor::package::ArkoorPackageBuilder;
use ark::lightning::{Invoice, PaymentHash, Preimage};
use ark::VtxoPolicy;
use bitcoin_ext::{BlockDelta, BlockHeight, TxOutExt};
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::ln::LightningPaymentStatus;
use server_rpc::protos;
use cln_rpc::plugins::hold;
use bitcoin::hashes::{sha256, Hash};
use std::os::unix::fs::MetadataExt;

use ark_testing::{btc, sat, TestContext};
use ark_testing::util::{FutureExt, poll_interval};

/// Public process identity only. Never inspect process memory, environment,
/// private descriptors, node keys or payment preimages.
struct OriginalXpayProcess {
	pid: i32,
	parent: i32,
	uid: u32,
	start: u64,
	exe: std::path::PathBuf,
}

impl OriginalXpayProcess {
	fn parse_stat(stat:&str) -> Option<(char,i32,u64)> {
		let fields = stat.rsplit_once(") ")?.1.split_whitespace().collect::<Vec<_>>();
		Some((fields.first()?.chars().next()?,fields.get(1)?.parse().ok()?,fields.get(19)?.parse().ok()?))
	}
	fn stat(pid: i32) -> Result<Option<(char,i32,u64)>,()> {
		match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
			Ok(stat)=>Self::parse_stat(&stat).map(Some).ok_or(()),
			Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),
			Err(_)=>Err(()),
		}
	}
	fn capture(pid: i32) -> Self {
		assert!(pid>1);
		let (_,parent,start) = Self::stat(pid).expect("original owned process identity must be readable").expect("original process must exist");
		let uid = std::fs::metadata(format!("/proc/{pid}")).unwrap().uid();
		assert_eq!(uid,unsafe { libc::geteuid() },"only this native test user's processes are eligible");
		let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
		Self { pid,parent,uid,start,exe }
	}
	fn remains(&self) -> bool {
		match Self::stat(self.pid) {
			Ok(Some((_,_,start)))=>start==self.start,
			Ok(None)=>false,
			Err(())=>panic!("cannot verify original process extinction; refuse the recovery premise"),
		}
	}
	fn signal(&self,signal:nix::sys::signal::Signal) {
		let (state,parent,start) = Self::stat(self.pid).expect("original process identity is unreadable").expect("original owned process vanished before its supervised signal");
		assert!(!matches!(state,'Z'|'X'));
		assert_eq!(parent,self.parent); assert_eq!(start,self.start);
		assert_eq!(std::fs::metadata(format!("/proc/{}",self.pid)).unwrap().uid(),self.uid);
		assert_eq!(std::fs::read_link(format!("/proc/{}/exe",self.pid)).unwrap(),self.exe);
		nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid),signal).unwrap();
	}
}

/// Sole purpose: a paused child must never survive an assertion/timeout or be
/// resumed into its queued original payment. Cleanup signals only the exact
/// captured PID/epoch/parent/UID/executable; no name-based or broad kill.
struct PausedOriginalXpay {
	process: OriginalXpayProcess,
	parent_start: u64,
}

impl Drop for PausedOriginalXpay {
	fn drop(&mut self) {
		let p=&self.process;
		if OriginalXpayProcess::stat(p.pid).is_ok_and(|identity|identity.is_some_and(|(state,parent,start)| {
			let original_parent_gone=match OriginalXpayProcess::stat(p.parent) {
				Ok(None)=>true,
				Ok(Some((_,_,epoch)))=>epoch!=self.parent_start,
				Err(())=>false,
			};
			let expected_adoption=original_parent_gone && (parent==1 || parent==std::process::id() as i32);
			!matches!(state,'Z'|'X') && (parent==p.parent || expected_adoption) && start==p.start
		}))
			&& std::fs::metadata(format!("/proc/{}",p.pid)).is_ok_and(|meta|meta.uid()==p.uid)
			&& std::fs::read_link(format!("/proc/{}/exe",p.pid)).is_ok_and(|exe|exe==p.exe)
		{
			let _=nix::sys::signal::kill(nix::unistd::Pid::from_raw(p.pid),nix::sys::signal::Signal::SIGKILL);
		}
	}
}

fn original_ln_tree(root:i32) -> Vec<OriginalXpayProcess> {
	let mut pending=vec![root];
	let mut captured=Vec::new();
	while let Some(pid)=pending.pop() {
		assert!(captured.len()<256,"unexpected native fixture process tree; refuse unbounded supervision");
		let process=OriginalXpayProcess::capture(pid);
		let mut children=std::collections::BTreeSet::new();
		for thread in std::fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
			let bytes=std::fs::read_to_string(thread.unwrap().path().join("children")).unwrap();
			children.extend(bytes.split_whitespace().map(|pid|pid.parse::<i32>().unwrap()));
		}
		pending.extend(children);
		captured.push(process);
	}
	captured
}

async fn pause_original_xpay(
	ctx:&TestContext,node:&ark_testing::daemon::lightningd::Lightningd,
) -> (OriginalXpayProcess,PausedOriginalXpay) {
	assert!(std::env::var_os("LIGHTNINGD_DOCKER_IMAGE").is_none(),"exact native PID supervision does not support Docker CLN");
	let mut rpc=node.grpc_client().await;
	let info=rpc.getinfo(cln_rpc::GetinfoRequest{}).await.unwrap().into_inner();
	assert_eq!(info.network,"regtest"); assert_eq!(info.version,"v26.06.6-abandon6");
	let config=rpc.list_configs(cln_rpc::ListconfigsRequest{config:Some("pid-file".into())}).await.unwrap().into_inner();
	let pid_file=std::path::PathBuf::from(config.configs.unwrap().pid_file.unwrap().value_str);
	assert!(pid_file.is_absolute() && pid_file.starts_with(&ctx.datadir),"original-node PID file must belong to this private context");
	let root=OriginalXpayProcess::capture(std::fs::read_to_string(pid_file).unwrap().trim().parse().unwrap());
	assert_eq!(root.exe.file_name().unwrap(),"lightningd");
	// plugin.c:1986 logs the actual pipecmd PID and exact executable, not a
	// guessed process name. getlog data is inspected privately and never logged.
	let logs=rpc.get_log(cln_rpc::GetlogRequest{level:Some(cln_rpc::getlog_request::GetlogLevel::Debug as i32)}).await.unwrap().into_inner();
	let mut candidates=Vec::new();
	for entry in logs.log {
		if entry.source.as_deref()!=Some("plugin-manager") { continue; }
		let Some((pid,path))=entry.log.as_deref().and_then(|line|line.strip_prefix("started(")).and_then(|line|line.split_once(") ")) else {continue;};
		let path=std::path::PathBuf::from(path);
		if path.file_name()!=Some(std::ffi::OsStr::new("cln-xpay")) {continue;}
		let process=OriginalXpayProcess::capture(pid.parse().unwrap());
		assert_eq!(process.parent,root.pid); assert_eq!(process.uid,root.uid);
		assert_eq!(process.exe,std::fs::canonicalize(path).unwrap());
		candidates.push(process);
	}
	assert_eq!(candidates.len(),1,"require one unambiguous original xpay startup identity");
	let paused=PausedOriginalXpay{process:candidates.pop().unwrap(),parent_start:root.start};
	paused.process.signal(nix::sys::signal::Signal::SIGSTOP);
	tokio::time::timeout(Duration::from_secs(5),async {
		loop {
			if matches!(OriginalXpayProcess::stat(paused.process.pid),Ok(Some(('T',_,start))) if start==paused.process.start) {break;}
			tokio::time::sleep(poll_interval()).await;
		}
	}).await.expect("the exact original xpay process did not enter stopped state");
	(root,paused)
}

async fn original_node_received_xpay(
	node:&ark_testing::daemon::lightningd::Lightningd,invoice:&str,label:&str,
) -> sha256::Hash {
	tokio::time::timeout(Duration::from_secs(10),async {
		loop {
			// jsonrpc.c:1197 uses LOG_IO_IN for request bytes read by the
			// ORIGINAL lightningd. log.c:1175 emits IO_IN; its source is
			// jsonrpc#<fd> (jsonrpc.c:1272), never the reverse response direction.
			let logs=node.grpc_client().await.get_log(cln_rpc::GetlogRequest{
				level:Some(cln_rpc::getlog_request::GetlogLevel::Io as i32),
			}).await.unwrap().into_inner();
			let mut streams=std::collections::BTreeMap::<String,Vec<u8>>::new();
			for entry in logs.log {
				if entry.item_type!=cln_rpc::getlog_log::GetlogLogType::IoIn as i32 {continue;}
				let Some(source)=entry.source.filter(|source|source.starts_with("jsonrpc#")) else {continue;};
				if let Some(data)=entry.data {streams.entry(source).or_default().extend(data);}
			}
			for bytes in streams.values() {
				for request in serde_json::Deserializer::from_slice(bytes).into_iter::<serde_json::Value>() {
					let Ok(request)=request else {break;};
					if request["jsonrpc"]=="2.0" && request["method"]=="xpay" && !request["id"].is_null()
						&& request["params"]["invstring"]==invoice && request["params"]["label"]==label {
						// Retain only a digest of the exact matched public request;
						// NEVER print/publish the private original-node IO inventory.
						return sha256::Hash::hash(&serde_json::to_vec(&request).unwrap());
					}
				}
			}
			tokio::time::sleep(poll_interval()).await;
		}
	}).await.expect("no exact original-node incoming xpay admission marker; setup is unproven")
}

/// Caller must first perform the actual supervised stop/reopen procedure.
/// These are retained TEST supervisor observations, not a forged CLN failure
/// or a substitute for production evidence. No preimage/config secrets enter.
pub(super) async fn retained_supervised_manifest(
	ctx: &TestContext, cfg: &server::Config, db: &server::database::Db,
	attempt: i64, incident: &str,
) -> std::path::PathBuf {
	let root = ctx.datadir.join(incident);
	std::fs::create_dir_all(&root).unwrap();
	let fence_path = root.join("supervisor.exclusion");
	std::fs::File::create(&fence_path).unwrap();
	let metadata = std::fs::metadata(&fence_path).unwrap();
	let row = db.read(async |tx| Ok(tx.query_one("SELECT a.lightning_node_id,a.payment_hash,a.amount_msat,n.pubkey,d.label \
		FROM lightning_payment_attempt a JOIN lightning_node n ON n.id=a.lightning_node_id \
		JOIN expiry_lightning_dispatch d ON d.id=a.id WHERE a.id=$1", &[&attempt]).await?)).await.unwrap();
	let sources = db.read(async |tx| Ok(tx.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
		WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&attempt]).await?
		.into_iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>())).await.unwrap();
	let binding = serde_json::json!({
		"incident": incident, "operator": "native-test-supervisor", "epoch": uuid::Uuid::new_v4().to_string(),
		"fence_path": fence_path, "fence_device": metadata.dev(), "fence_inode": metadata.ino(),
		"cln_config_index": 0, "attempt_id": attempt, "node_id": row.get::<_, i64>("lightning_node_id"),
		"node_pubkey": bitcoin::secp256k1::PublicKey::from_slice(&row.get::<_, Vec<u8>>("pubkey")).unwrap().to_string(),
		"network": cfg.network, "cln_version": "v26.06.6-abandon6", "payment_hash": row.get::<_, String>("payment_hash"),
		"dispatch_label": row.get::<_, String>("label"), "amount_msat": row.get::<_, i64>("amount_msat"),
		"source_count": sources.len(), "source_set_sha256": sha256::Hash::hash(&serde_json::to_vec(&sources).unwrap()),
	});
	let binding_digest = sha256::Hash::hash(&serde_json::to_vec(&binding).unwrap());
	let mut manifest = serde_json::json!({"version": 1, "binding": binding});
	for (role, events) in [
		("quiescence", vec!["original-producers-terminated", "transport-queues-extinguished", "lightningd-children-terminated"]),
		("history_retention", vec!["original-store-reopened", "no-history-rollback", "no-payment-pruning"]),
		("no_redelivery", vec!["supervisors-fenced", "old-request-redelivery-disabled"]),
	] {
		let records = events.iter().map(|event| serde_json::json!({
			"subject": format!("{incident}:original-test-daemons-and-controller"),
			"record_id": format!("{incident}:{event}:completed-native-call"),
			"observed_at": chrono::Utc::now(), "event": event,
		})).collect::<Vec<_>>();
		let bytes = serde_json::to_vec(&serde_json::json!({"binding_sha256": binding_digest, "records": records})).unwrap();
		let path = root.join(format!("{role}.json"));
		std::fs::write(&path, &bytes).unwrap();
		manifest[role] = serde_json::json!({"path": path, "sha256": sha256::Hash::hash(&bytes)});
	}
	let path = root.join("manifest.json");
	std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
	path
}

/// Retain ORIGINAL process/binary identities before the actual dispatch. Source
/// correspondence is the framework's pinned-build premise, not a rebuild proof.
pub(super) async fn capture_original_labelled_producer(
	ctx: &TestContext, srv: &ark_testing::daemon::captaind::Captaind,
	node: &ark_testing::daemon::lightningd::Lightningd,
) -> (serde_json::Value, i32, u64) {
	assert!(std::env::var_os("LIGHTNINGD_DOCKER_IMAGE").is_none());
	let mut original = node.grpc_client().await;
	let config = original.list_configs(cln_rpc::ListconfigsRequest {
		config: Some("pid-file".into()),
	}).await.unwrap().into_inner();
	let pid_file = std::path::PathBuf::from(config.configs.unwrap().pid_file.unwrap().value_str);
	assert!(pid_file.is_absolute() && pid_file.starts_with(&ctx.datadir));
	let root = OriginalXpayProcess::capture(std::fs::read_to_string(pid_file).unwrap().trim().parse().unwrap());
	assert_eq!(root.exe.file_name().unwrap(), "lightningd");
	let tree = original_ln_tree(root.pid);
	let pin = |name: &str| {
		let processes = tree.iter().filter(|process| process.exe.file_name() == Some(std::ffi::OsStr::new(name)))
			.collect::<Vec<_>>();
		assert_eq!(processes.len(), 1, "require the exact original plugin runtime");
		let process = processes[0];
		assert_eq!(process.parent, root.pid); assert!(process.remains());
		serde_json::json!({"binary_sha256": sha256::Hash::hash(&std::fs::read(&process.exe).unwrap()),
			"observed_at": chrono::Utc::now(),
			"record_id": format!("native-original-{}-{}-{}", name, process.pid, process.start)})
	};
	let dispatcher = srv.get_custom_command(&[]).await.unwrap();
	let dispatcher_binary = std::fs::canonicalize(dispatcher.as_std().get_program()).unwrap();
	let dispatcher_source = sha256::Hash::hash(include_bytes!("../../../server/src/ln/cln/xpay.rs"));
	assert_eq!(dispatcher_source.to_string(), "7743ce9d33087319cb47f30a59a9719b6856857b3c452eb76c9cddda36f30d99");
	let mut grpc = pin("cln-grpc");
	grpc["server_sha256"] = serde_json::json!("765b38372a479ce29900ac243ad2fb0cc339fc5907b8a1451084e22b405dbbcc");
	grpc["conversion_sha256"] = serde_json::json!("d4c01103a076cb8d1ad90c38940f0b9aef180aabca9e15c09eeaf51fd8535d60");
	grpc["model_sha256"] = serde_json::json!("36538a5e672b3fb0409cfb5b6424d0b71880d316cb3709b99354163183652cd8");
	let mut xpay = pin("cln-xpay");
	xpay["source_sha256"] = serde_json::json!("e65b19cef028b49acd3a8824dacf118206ed11074b2e72997074e3fabcd43d1c");
	let qualification = serde_json::json!({
		"original_epoch": format!("native-original-cln-{}-{}", root.pid, root.start),
		"label_contract": "fork-native-xpay-label-abandon6-v1",
		"dispatcher": {"source_sha256": dispatcher_source,
			"binary_sha256": sha256::Hash::hash(&std::fs::read(dispatcher_binary).unwrap()),
			"observed_at": chrono::Utc::now(), "record_id": "native-original-pinned-captaind"},
		"grpc": grpc, "xpay": xpay,
	});
	(qualification, root.pid, root.start)
}

pub(super) async fn attach_original_labelled_producer(
	manifest: &std::path::Path, db: &server::database::Db, attempt: i64,
	mut qualification: serde_json::Value,
) {
	let mut body: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
	qualification["binding_sha256"] = serde_json::json!(sha256::Hash::hash(&serde_json::to_vec(&body["binding"]).unwrap()));
	let created = db.read(async |tx| Ok(tx.query_one("SELECT created_at FROM expiry_lightning_dispatch WHERE id=$1", &[&attempt])
		.await?.get::<_, chrono::DateTime<chrono::Utc>>(0))).await.unwrap();
	qualification["dispatch_created_at"] = serde_json::json!(created);
	let evidence = serde_json::to_vec(&qualification).unwrap();
	let path = manifest.with_file_name("original-labelled-producer.json");
	std::fs::write(&path, &evidence).unwrap();
	body["original_labelled_producer"] = serde_json::json!({"path": path, "sha256": sha256::Hash::hash(&evidence)});
	std::fs::write(manifest, serde_json::to_vec(&body).unwrap()).unwrap();
}

/// Real signed source, committed one-shot fence BEFORE any remote send. The
/// controlled supervisor stops the only original producer and original CLN,
/// reopens its intact store, imports the receipt and proves actual Core6
/// fallback. No xpay replay, fake error/index or changed attempt status.
#[tokio::test]
async fn expiry_supervised_pre_rpc_dispatch_refunds_once_after_real_quiescence() {
	supervised_dispatch_refund(false, false, false).await;
}

/// Different premise from the pre-call component: the real native dispatcher
/// sends its original xpay into the ORIGINAL CLN while its exact xpay child
/// is stopped, and private incoming IO proves remote admission before group1.
#[tokio::test]
async fn expiry_supervised_original_xpay_queued_before_first_group_refunds_once() {
	supervised_dispatch_refund(true, false, false).await;
}

/// A genuine operator-funded foreign payment is observed by the normal monitor
/// BEFORE import. Its hash-level success cannot discharge this never-sent source.
#[tokio::test]
async fn expiry_supervised_own_label_absent_after_foreign_complete_refunds_own_source_once() {
	supervised_dispatch_refund(false, true, false).await;
}

/// AcceptedPending is a real injected own payment, not zero-history absence.
/// Reopen the original intact CLN, refuse that receipt, and let the original
/// held payment genuinely fail before the existing full native-refund oracle.
#[tokio::test]
async fn expiry_supervised_own_pending_survives_reopen_then_actual_failure_refunds_once() {
	supervised_dispatch_refund(false, false, true).await;
}

async fn supervised_dispatch_refund(queued_original: bool, foreign_complete: bool, own_pending: bool) {
	tokio::time::timeout(Duration::from_secs(180), async {
		let ctx = TestContext::new(if queued_original {"server/expiry_supervised_original_xpay_queued"}
			else if foreign_complete {"server/expiry_supervised_foreign_complete"}
			else if own_pending {"server/expiry_supervised_own_pending_reopen"}
			else {"server/expiry_supervised_pre_rpc_dispatch"}).await;
		let lightning = ctx.new_lightning_setup("lightningd").await;
		let srv = ctx.captaind("server").lightningd(&lightning.internal).no_vtxo_pool().funded(btc(10))
			.cfg(move |cfg| {
				cfg.vtxo_lifetime=BlockDelta::new(24); cfg.vtxo_exit_delta=BlockDelta::new(3);
				cfg.htlc_send_expiry_delta=BlockDelta::new(40);
				// Keep the normal monitor enabled; the foreign case must really
				// observe its coarse outcome without direct status mutation.
				if foreign_complete || own_pending {cfg.invoice_check_interval=Duration::from_secs(1);}
			})
			.watchmand().create().await;
		let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
		let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
		let client = sender.client().await;
		let destination = client.fallback_destination().await.unwrap();
		let input = client.get_full_vtxo(ids[0]).await.unwrap();
		let key = client.get_vtxo_key(&input).await.unwrap();
		let invoice = if own_pending {
			lightning.external.hold_client().await.invoice(hold::InvoiceRequest {
				payment_hash: Preimage::from_slice(&[39;32]).unwrap().compute_payment_hash().to_vec(),
				amount_msat: 100_000_000,
				description: Some(hold::invoice_request::Description::Memo("own-pending-intact-reopen".into())),
				min_final_cltv_expiry: Some(30), expiry: Some(3600), routing_hints: vec![],
			}).await.unwrap().into_inner().bolt11
		} else {
			lightning.external.invoice_with_preimage(Some(sat(100_000)), "supervised-never-injected", "supervised pre-RPC", [39;32]).await
		};
		let invoice = Invoice::Bolt11(invoice.parse().unwrap());
		let hash = invoice.payment_hash();
		let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
		let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
		let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints([input.clone()],
			VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline)).unwrap()
			.generate_user_nonces(&[key]).unwrap();
		let response = srv.get_public_rpc().await.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
			parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
		}).await.unwrap().into_inner();
		let response = ark::arkoor::package::ArkoorPackageCosignResponse::try_from(response).unwrap();
		let grants = builder.user_cosign(&[key], response).unwrap().build_signed_vtxos();
		client.register_vtxo_transactions_with_server(&grants).await.unwrap();
		let source = grants[0].id();
		let cfg = srv.config().clone();
		let operator_script = {
			let watch = srv.watchmand().config();
			watch.sweep_address.clone().unwrap().require_network(watch.network).unwrap().script_pubkey()
		};
		let db = server::database::Db::connect(&cfg.postgres).await.unwrap();
		let node_id = db.read(async |tx| Ok(tx.query_one("SELECT id FROM lightning_node ORDER BY id LIMIT 1", &[]).await?.get::<_, i64>(0))).await.unwrap();
		// Capture actual ORIGINAL runtime identities before creating the own
		// dispatch. Build/source correspondence is this test's pinned-build
		// premise; these observations do not pretend to prove a reproducible build.
		let mut original_node = None;
		let mut paused_xpay = None;
		let mut admission = None;
		let mut original_tree = Vec::new();
		let mut original_qualification = None;
		let mut accepted_parts = None;
		let mut accepted_pays = None;
		let foreign_label = "independent-operator-funded-foreign-payment";
		if foreign_complete || own_pending {
			let (qualification, pid, start) = capture_original_labelled_producer(&ctx, &srv, &lightning.internal).await;
			let root = OriginalXpayProcess::capture(pid);
			assert_eq!(root.start, start);
			original_qualification = Some(qualification);
			original_node = Some(root);
		}
		let attempt=if queued_original || own_pending {
			lightning.sync().await;
			if queued_original {
				let (root,paused)=pause_original_xpay(&ctx,&lightning.internal).await;
				original_node=Some(root); paused_xpay=Some(paused);
			}
			// ONE authentic public initiation; never call xpay from the test or
			// resubmit an invoice/label to manufacture a result.
			srv.get_public_rpc().await.initiate_lightning_payment(protos::InitiateLightningPaymentRequest{
				invoice:invoice.to_string(),payment_amount_sat:100_000,
				htlc_vtxo_ids:vec![source.to_bytes().to_vec()],mailbox_id:None,retry_for_secs:Some(1),
			}).await.unwrap();
			let attempt=tokio::time::timeout(Duration::from_secs(10),async {
				loop {
					if let Some(attempt)=db.read(async |tx|tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap() {
						let dispatched=db.read(async |tx|Ok(tx.query_opt("SELECT id FROM expiry_lightning_dispatch WHERE id=$1",&[&attempt.id]).await?.is_some())).await.unwrap();
						if dispatched {break attempt;}
					}
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("the one original native payment must acquire its genuine durable dispatch");
			let label=db.read(async |tx|Ok(tx.query_one("SELECT label FROM expiry_lightning_dispatch WHERE id=$1",&[&attempt.id]).await?.get::<_,String>(0))).await.unwrap();
			if queued_original {
				admission=Some(original_node_received_xpay(&lightning.internal,&invoice.to_string(),&label).await);
				let paused=&paused_xpay.as_ref().unwrap().process;
				assert!(matches!(OriginalXpayProcess::stat(paused.pid),Ok(Some(('T',parent,start))) if parent==paused.parent && start==paused.start));
			} else {
				lightning.external.wait_for_hold_invoice_accepted(hash).await;
			}
			let mut original=lightning.internal.grpc_client().await;
			let live=original.getinfo(cln_rpc::GetinfoRequest{}).await.unwrap().into_inner();
			let key=db.read(async |tx|Ok(tx.query_one("SELECT pubkey FROM lightning_node WHERE id=$1",&[&attempt.lightning_node_id]).await?.get::<_,Vec<u8>>(0))).await.unwrap();
			assert_eq!(live.id,key);
			let history=original.list_send_pays(cln_rpc::ListsendpaysRequest{payment_hash:Some(hash.to_vec()),..Default::default()}).await.unwrap().into_inner();
			if own_pending {
				assert!(!history.payments.is_empty());
				assert!(history.payments.iter().all(|pay| pay.payment_hash == hash.to_vec()
					&& pay.label.as_deref() == Some(label.as_str()) && pay.payment_preimage.is_none()
					&& pay.status() == cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Pending));
				assert_eq!(history.payments.iter().map(|pay| pay.amount_msat.as_ref().unwrap().msat).sum::<u64>(), 100_000_000);
				let aggregate = original.list_pays(cln_rpc::ListpaysRequest {
					payment_hash: Some(hash.to_vec()), ..Default::default()
				}).await.unwrap().into_inner();
				assert_eq!(aggregate.pays.len(), 1);
				assert!(aggregate.pays.iter().all(|pay| pay.payment_hash == hash.to_vec()
					&& pay.label.as_deref() == Some(label.as_str()) && pay.preimage.is_none()
					&& pay.status() == cln_rpc::listpays_pays::ListpaysPaysStatus::Pending));
				assert_eq!(aggregate.pays[0].amount_msat.as_ref().unwrap().msat, 100_000_000);
				let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
					hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.to_bytes().to_vec()],
				}).await.unwrap().into_inner();
				assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Pending(_))));
				assert_eq!(status.htlc_vtxo_ids, vec![source.to_bytes().to_vec()]);
				accepted_parts = Some(history.payments);
				accepted_pays = Some(aggregate.pays);
			} else {
				assert!(history.payments.is_empty(),"a remotely received original request must still have zero injected groups while xpay is stopped");
			}
			attempt
		} else {
			let attempt=db.write(async |tx| {
				tx.store_lightning_payment_start(node_id,&invoice,sat(100_000),None,&[source],None,tip,sat(0),None,Duration::from_secs(1), None).await?;
				Ok(tx.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.unwrap())
			}).await.unwrap();
			assert!(db.write(async |tx|tx.admit_lightning_dispatch(&attempt).await).await.unwrap());
			attempt
		};
		if foreign_complete {
			// The actual operator channel pays this real external invoice; no
			// Bark attempt/source is linked to that foreign payment. Never fake
			// a historical row, outcome, preimage or remote error.
			lightning.sync().await;
			let response = lightning.internal.grpc_client().await.xpay(cln_rpc::XpayRequest {
				invstring: invoice.to_string(), label: Some(foreign_label.into()),
				amount_msat: None, maxfee: None, layers: vec![], retry_for: Some(1),
				partial_msat: None, maxdelay: None, payer_note: None, localinvreqid: None, dev_use_shadow: None,
			}).await.unwrap().into_inner();
			assert_eq!(Preimage::from_slice(&response.payment_preimage).unwrap().compute_payment_hash(), hash);
			lightning.external.wait_invoice_paid("supervised-never-injected").await;
		}
		if foreign_complete {
			// No direct monitor call or pause: wait for the normal server loop
			// to observe the foreign group and commit the misleading coarse fields.
			tokio::time::timeout(Duration::from_secs(35), async {
				loop {
					let observed = db.read(async |tx| tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
					if observed.status == LightningPaymentStatus::Succeeded {
						assert!(observed.final_amount_msat.is_some());
						let source_state = db.read(async |tx| Ok(tx.query_one("SELECT v.spend_state::TEXT,h.offchain_resolution::TEXT, \
							EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=$1) AS success, \
							EXISTS(SELECT 1 FROM expiry_lightning_internal_funding WHERE id=$1) AS funding \
							FROM vtxo v JOIN htlc_vtxo h ON h.id=v.id WHERE v.vtxo_id=$2", &[&attempt.id,&source.to_string()]).await?)).await.unwrap();
						assert_eq!(source_state.get::<_, &str>("spend_state"), "spendable",
							"foreign payment success must not consume the untouched own source");
						assert!(source_state.get::<_, Option<&str>>("offchain_resolution").is_none());
						assert!(!source_state.get::<_, bool>("success") && !source_state.get::<_, bool>("funding"));
						break;
					}
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("normal monitor must observe the actual foreign Complete before recovery");
		}
		// The pre-call component never submitted RPC. The separate queued case
		// instead has real original-node IO admission and a paused original
		// producer. In BOTH cases extinguish/reap before zero-history import.
		drop(client);
		srv.watchmand().stop().await.unwrap();
		srv.stop().await.unwrap();
		if let Some(paused)=&paused_xpay {
			let root=original_node.as_ref().unwrap();
			assert!(root.remains());
			original_tree=original_ln_tree(root.pid);
			assert!(original_tree.iter().any(|p|p.pid==paused.process.pid && p.start==paused.process.start));
			assert!(matches!(OriginalXpayProcess::stat(paused.process.pid),Ok(Some(('T',_,start))) if start==paused.process.start));
			paused.process.signal(nix::sys::signal::Signal::SIGKILL); // NEVER SIGCONT
		}
		if foreign_complete || own_pending {
			let root = original_node.as_ref().unwrap();
			assert!(root.remains());
			original_tree = original_ln_tree(root.pid);
		}
		lightning.internal.stop().await.unwrap();
		if queued_original || foreign_complete || own_pending {
			tokio::time::timeout(Duration::from_secs(10),async {
				while original_tree.iter().any(OriginalXpayProcess::remains) {tokio::time::sleep(poll_interval()).await;}
			}).await.expect("every captured original CLN/plugin/channel process must actually be extinct and reaped");
		}
		drop(paused_xpay);
		lightning.internal.start().await.unwrap();
		// Restart reserves new ports, while identity, store and certificate
		// paths remain the same. Both the importer and restarted server must
		// address the actual reopened original node.
		let grpc = lightning.internal.grpc_details().await;
		let hold = lightning.internal.hold_details().await;
		{
			let mut config = srv.config_mut();
			let node = &mut config.cln_array[0];
			assert_eq!(node.server_cert_path,grpc.server_cert_path);
			assert_eq!(node.client_cert_path,grpc.client_cert_path);
			assert_eq!(node.client_key_path,grpc.client_key_path);
			node.uri = grpc.uri.parse().unwrap();
			let plugin = node.hold_invoice.as_mut().unwrap();
			assert_eq!(plugin.server_cert_path,hold.server_cert_path);
			assert_eq!(plugin.client_cert_path,hold.client_cert_path);
			assert_eq!(plugin.client_key_path,hold.client_key_path);
			plugin.uri = hold.uri.parse().unwrap();
		}
		let cfg = srv.config().clone();
		// The CLI reads its endpoints from disk while the server is stopped.
		let config_path = cfg.data_dir.join(ark_testing::daemon::captaind::CAPTAIND_CONFIG_FILE);
		cfg.write_into(&mut std::fs::File::create(config_path).unwrap()).unwrap();
		let original_status=db.read(async |tx|tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status;
		let manifest = retained_supervised_manifest(&ctx, &cfg, &db, attempt.id,
			if queued_original {"queued-original-incident"} else if foreign_complete {"foreign-complete-incident"}
			else if own_pending {"own-pending-reopen-incident"} else {"pre-rpc-incident"}).await;
		if let Some(admission)=admission {
			// Retain actual public process epochs and only the matched admission
			// digest inside the existing artifact; never raw private IO records.
			let mut body:serde_json::Value=serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
			let path=std::path::PathBuf::from(body["quiescence"]["path"].as_str().unwrap());
			let mut evidence:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
			let records=evidence["records"].as_array_mut().unwrap();
			records.push(serde_json::json!({"subject":"original-node-exact-xpay-request", "record_id":admission.to_string(),
				"observed_at":chrono::Utc::now(),"event":"original-request-admission"}));
			for process in &original_tree {
				records.push(serde_json::json!({"subject":format!("pid={} parent={} uid={} start={} exe={}",process.pid,process.parent,process.uid,process.start,process.exe.display()),
					"record_id":format!("native-original-epoch-{}-{}-reaped",process.pid,process.start),
					"observed_at":chrono::Utc::now(),"event":"original-producers-terminated"}));
			}
			let bytes=serde_json::to_vec(&evidence).unwrap(); std::fs::write(&path,&bytes).unwrap();
			body["quiescence"]["sha256"]=serde_json::json!(sha256::Hash::hash(&bytes));
			std::fs::write(&manifest,serde_json::to_vec(&body).unwrap()).unwrap();
		}
		if let Some(qualification) = original_qualification {
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.is_err(),
				"nonempty history cannot pass the unqualified whole-hash-empty proof");
			attach_original_labelled_producer(&manifest, &db, attempt.id, qualification).await;
		}
		let manifest_arg = manifest.to_str().unwrap();
		let original: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
		if own_pending {
			let mut reopened = lightning.internal.grpc_client().await;
			let raw = reopened.list_send_pays(cln_rpc::ListsendpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			let aggregate = reopened.list_pays(cln_rpc::ListpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			// Exact original rows, including labels/group/part/index, survive.
			// Do not print payment records or any possible private preimage.
			assert!(raw.payments == *accepted_parts.as_ref().unwrap(), "intact original raw Pending history changed");
			assert!(aggregate.pays == *accepted_pays.as_ref().unwrap(), "intact original aggregate Pending history changed");
			let error = server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.unwrap_err();
			assert!(format!("{error:#}").contains("original dispatch has own payment history"), "{error:#}");
			let unchanged = db.read(async |tx| Ok(tx.query_one("SELECT \
				EXISTS(SELECT 1 FROM expiry_lightning_terminal_failure WHERE id=$1) AS failure, \
				EXISTS(SELECT 1 FROM expiry_lightning_external_success WHERE id=$1) AS success, \
				EXISTS(SELECT 1 FROM htlc_settlement WHERE payment_hash=$2) AS wal, \
				EXISTS(SELECT 1 FROM expiry_allocation_coin c JOIN expiry_entitlement e ON e.id=c.entitlement_id \
					WHERE e.vtxo_id=$3 AND c.active) AS allocated", &[&attempt.id,&hash.to_string(),&source.to_string()]).await?)).await.unwrap();
			for field in ["failure", "success", "wal", "allocated"] { assert!(!unchanged.get::<_, bool>(field)); }
		} else {
			let bytes = std::fs::read(&manifest).unwrap();
			let mut wrong: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
			wrong["binding"]["source_count"] = serde_json::json!(2);
			let wrong_path = manifest.with_file_name("wrong-sources.json");
			std::fs::write(&wrong_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &wrong_path).await.is_err());
			let artifact_path = original["quiescence"]["path"].as_str().unwrap();
			let artifact_bytes = std::fs::read(artifact_path).unwrap();
			let mut changed = artifact_bytes.clone(); changed.push(b' ');
			std::fs::write(artifact_path,changed).unwrap();
			assert!(server::ln::resolve_uninjected_lightning(&cfg,&manifest).await.is_err(),"changed retained artifact bytes must refuse");
			std::fs::write(artifact_path,artifact_bytes).unwrap();
			let fence_path = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["binding"]["fence_path"].as_str().unwrap().to_owned();
			let held = std::fs::OpenOptions::new().read(true).write(true).open(fence_path).unwrap();
			held.try_lock().unwrap();
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.is_err(), "a supervisor-held competing exclusion cannot be ignored");
			drop(held);
			let mut missing_ack = srv.get_custom_command(&[
				"resolve-uninjected-lightning", "--manifest", manifest_arg,
			]).await.unwrap();
			missing_ack.kill_on_drop(true);
			let refused = missing_ack.output().await.unwrap();
			assert!(!refused.status.success(), "the actual privileged CLI must require explicit acknowledgment");
			assert!(String::from_utf8_lossy(&refused.stderr).contains("requires --acknowledge-trusted-operations"));
			assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_lightning_terminal_failure WHERE id=$1",
				&[&attempt.id]).await?.get::<_,i64>(0))).await.unwrap(),0,"acknowledgment refusal cannot import a receipt");
			let mut import = srv.get_custom_command(&[
				"resolve-uninjected-lightning", "--manifest", manifest_arg, "--acknowledge-trusted-operations",
			]).await.unwrap();
			import.kill_on_drop(true);
			let imported = import.output().await.unwrap();
			assert!(imported.status.success(), "actual CLI import failed: {}",String::from_utf8_lossy(&imported.stderr));
			assert!(String::from_utf8_lossy(&imported.stdout).contains("recorded supervised zero-history evidence"));
			assert!(!server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.unwrap());
			assert_eq!(db.read(async |tx| tx.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status, original_status);
		}
		srv.start().await.unwrap(); srv.watchmand().start().await.unwrap();
		if own_pending {
			lightning.internal.connect(&lightning.external).await;
			lightning.sync().await;
			lightning.external.wait_for_hold_invoice_accepted(hash).await;
			// The source's real backing expires while its actual own HTLC is
			// still held. The new invoice's final CLTV30 fits maxdelay36 and
			// exceeds this root's remaining lifetime; do not mine to its timeout.
			let held_height = input.expiry_height() + BlockDelta::new(1);
			let current = ctx.bitcoind().get_block_count().await as u32;
			assert!(held_height < deadline);
			let held_height = ctx.generate_blocks(held_height.to_u32().checked_sub(current).unwrap()).await;
			srv.wait_for_sync_height(held_height).await; srv.watchmand().wait_for_sync_height(held_height).await;
			let held_hash = ctx.bitcoind().sync_client().get_block_hash(u64::from(held_height.to_u32())).unwrap();
			tokio::time::timeout(Duration::from_secs(30), async {
				loop {
					if db.read(async |tx| {
						let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
						Ok(view.captaind.height == held_height && view.watchmand.height == held_height
							&& view.captaind.hash == held_hash && view.watchmand.hash == held_hash
							&& tx.expiry_view_is_rebuilt(view).await?)
					}).await.unwrap() { break; }
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("expired Pending sample requires the completed rebuilt canonical view");
			lightning.sync().await;
			lightning.external.wait_for_hold_invoice_accepted(hash).await;
			let mut original = lightning.internal.grpc_client().await;
			let raw = original.list_send_pays(cln_rpc::ListsendpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			let aggregate = original.list_pays(cln_rpc::ListpaysRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			assert!(raw.payments == *accepted_parts.as_ref().unwrap(), "backing expiry changed genuine original Pending parts");
			assert!(aggregate.pays == *accepted_pays.as_ref().unwrap(), "backing expiry changed genuine original Pending aggregate");
			assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_allocation_coin c \
				JOIN expiry_entitlement e ON e.id=c.entitlement_id WHERE e.vtxo_id=$1 AND c.active",
				&[&source.to_string()]).await?.get::<_, i64>(0))).await.unwrap(), 0,
				"expired backing with a genuine own Pending cannot authorize any payout");
			let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
				hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.to_bytes().to_vec()],
			}).await.unwrap().into_inner();
			assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Pending(_))));
			assert_eq!(status.htlc_vtxo_ids, vec![source.to_bytes().to_vec()]);
			assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
			// Only the genuine receiver changes the actual held HTLC outcome.
			lightning.external.hold_client().await.cancel(hold::CancelRequest { payment_hash: hash.to_vec() }).await.unwrap();
			tokio::time::timeout(Duration::from_secs(30), async {
				loop {
					let proof = db.read(async |tx| Ok(tx.query_opt("SELECT created_index,jsonrpc_error_code,supervised_manifest_sha256 \
						FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt.id]).await?)).await.unwrap();
					if let Some(proof) = proof {
						assert!(proof.get::<_, Option<i64>>("created_index").is_some());
						assert!(proof.get::<_, Option<i32>>("jsonrpc_error_code").is_none());
						assert!(proof.get::<_, Option<Vec<u8>>>("supervised_manifest_sha256").is_none());
						break;
					}
					tokio::time::sleep(poll_interval()).await;
				}
			}).await.expect("genuine original Pending must reconcile its actual backend cancellation");
		}
		let fee_address = srv.watchmand().wait_wallet_address().await;
		let mut sponsors = std::collections::BTreeSet::new();
		for _ in 0..2 {
			let funding_id = ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await;
			let funding = ctx.bitcoind().await_transaction(funding_id).await;
			let outputs = funding.output.iter().enumerate().filter(|(_,output)|
				output.script_pubkey==fee_address.script_pubkey() && output.value==sat(100_000)).collect::<Vec<_>>();
			assert_eq!(outputs.len(),1,"retain the exact authentic sponsor output from each funding transaction");
			sponsors.insert(bitcoin::OutPoint::new(funding_id,outputs[0].0 as u32));
		}
		assert_eq!(sponsors.len(),2);
		ctx.generate_blocks(2).await;
		let height = ctx.bitcoind().get_block_count().await as u32;
		let expired = ctx.generate_blocks(deadline.to_u32()+1-height).await;
		srv.wait_for_sync_height(expired).await; srv.watchmand().wait_for_sync_height(expired).await;
		let parent = tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let row = db.read(async |tx| Ok(tx.query_opt("SELECT a.sweep_tx,f.raw_tx FROM expiry_allocation a \
					JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
					JOIN expiry_fee_child f ON f.allocation_id=a.id \
					WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL \
					ORDER BY f.id DESC LIMIT 1", &[&source.to_string()]).await?)).await.unwrap();
				if let Some(row) = row {
					let parent = bitcoin::consensus::deserialize::<bitcoin::Transaction>(row.get("sweep_tx")).unwrap();
					let child = bitcoin::consensus::deserialize::<bitcoin::Transaction>(row.get("raw_tx")).unwrap();
					// Publication is local to the publisher's Core. The actual miner
					// must know both exact transactions before its six-block batch.
					let core = ctx.bitcoind().sync_client();
					if let (Some(actual_parent), Some(actual_child)) = (
						core.custom_get_raw_transaction_info(parent.compute_txid(), None).unwrap(),
						core.custom_get_raw_transaction_info(child.compute_txid(), None).unwrap(),
					) {
						assert_eq!(actual_parent.hex, bitcoin::consensus::serialize(&parent));
						assert_eq!(actual_child.hex, bitcoin::consensus::serialize(&child));
						break parent;
					}
				}
				tokio::time::sleep(poll_interval()).await;
			}
		}).await.expect("supervised receipt must lead to an actual lawful fallback");
		let payouts = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk).collect::<Vec<_>>();
		assert_eq!(payouts.len(), 1); let (vout, payout) = payouts[0]; assert_eq!(payout.value, sat(99_000));
		let signed_path = std::iter::once(grants[0].chain_anchor()).chain(grants[0].transactions()
			.map(|step|bitcoin::OutPoint::new(step.tx.compute_txid(),step.output_idx as u32))).collect::<std::collections::BTreeSet<_>>();
		assert_eq!(parent.input.len(),1,"no_pool one-source payout cannot absorb unrelated roots");
		assert!(signed_path.contains(&parent.input[0].previous_output),"the only payout input belongs to the real signed source path");
		let backing = ctx.bitcoind().await_transaction(parent.input[0].previous_output.txid).await;
		assert_eq!(backing.output[parent.input[0].previous_output.vout as usize].value,sat(100_000));
		assert_eq!(parent.version.0,3);
		assert_eq!(parent.output.len(),3);
		assert_eq!(parent.output.iter().map(|output|output.value).sum::<bitcoin::Amount>(),sat(100_000),"payout parent has zero miner fee");
		let operator_outputs = parent.output.iter().filter(|output|output.script_pubkey==operator_script).collect::<Vec<_>>();
		assert_eq!(operator_outputs.len(),1); assert_eq!(operator_outputs[0].value,sat(1_000));
		let anchors = parent.output.iter().enumerate().filter(|(_,output)|output.is_p2a_fee_anchor()).collect::<Vec<_>>();
		assert_eq!(anchors.len(),1); assert_eq!(anchors[0].1.value,sat(0));
		let anchor = bitcoin::OutPoint::new(parent.compute_txid(),anchors[0].0 as u32);
		assert!(parent.output.iter().all(|output|output.script_pubkey==destination.spk || output.script_pubkey==operator_script || output.is_p2a_fee_anchor()),
			"every parent output pays the rightful user, legitimate configured operator, or zero anchor");
		let confirmed = ctx.generate_blocks(6).await;
		srv.wait_for_sync_height(confirmed).await; srv.watchmand().wait_for_sync_height(confirmed).await;
		let actual = ctx.bitcoind().sync_client().get_tx_out(&parent.compute_txid(), vout as u32, Some(true)).unwrap().unwrap();
		assert!(actual.confirmations >= 6); assert_eq!(actual.value, sat(99_000));
		assert_eq!(actual.script_pub_key.script().unwrap(), destination.spk);
		let chain = ctx.bitcoind().sync_client();
		let parent_info = chain.custom_get_raw_transaction_info(parent.compute_txid(), None).unwrap().unwrap();
		let block = chain.get_block(&parent_info.blockhash.unwrap()).unwrap();
		let children = block.txdata.iter().filter(|tx| tx.input.iter().any(|input|
			input.previous_output.txid == parent.compute_txid())).collect::<Vec<_>>();
		assert_eq!(children.len(),1,"actual payout must include its confirmed funded fee child");
		let child = children[0];
		assert_eq!(child.version.0,3);
		assert_eq!(child.input.iter().filter(|input|input.previous_output==anchor).count(),1);
		assert!(child.input.len()>1);
		for input in &child.input {
			if input.previous_output==anchor { continue; }
			assert!(sponsors.contains(&input.previous_output),"fee child may use only the two exact legitimate100k sponsor outputs");
			let funding = chain.custom_get_raw_transaction_info(input.previous_output.txid,None).unwrap().unwrap();
			let funding_height = chain.get_block_header_info(&funding.blockhash.unwrap()).unwrap().height;
			let payout_height = chain.get_block_header_info(&parent_info.blockhash.unwrap()).unwrap().height;
			assert!(funding_height<payout_height,"actual sponsor must be confirmed before payout construction");
		}
		let child_info = chain.custom_get_raw_transaction_info(child.compute_txid(), None).unwrap().unwrap();
		assert_eq!(child_info.blockhash,parent_info.blockhash); assert!(child_info.confirmations.unwrap()>=6);
		let mut boundary = std::collections::BTreeSet::new();
		let mut input_total = sat(0);
		for tx in [&parent, child] {
			for input in &tx.input {
				if input.previous_output.txid == parent.compute_txid() { continue; }
				assert!(boundary.insert(input.previous_output),"no repeated canonical input in the payout package");
				let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
				input_total += previous.output[input.previous_output.vout as usize].value;
			}
		}
		let mut terminal_total = sat(0);
		let consumed = child.input.iter().map(|input| input.previous_output).collect::<std::collections::BTreeSet<_>>();
		for (index, output) in parent.output.iter().enumerate() {
			if !consumed.contains(&bitcoin::OutPoint::new(parent.compute_txid(),index as u32)) { terminal_total += output.value; }
		}
		terminal_total += child.output.iter().map(|output|output.value).sum::<bitcoin::Amount>();
		let mut child_inputs = sat(0);
		for input in &child.input {
			let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
			child_inputs += previous.output[input.previous_output.vout as usize].value;
		}
		let child_fee = child_inputs-child.output.iter().map(|output|output.value).sum::<bitcoin::Amount>();
		assert!(child_fee>sat(0));
		assert_eq!(input_total,terminal_total+child_fee,"zero-fee parent plus actual sponsor cut must conserve satoshis");
		let recorded = db.read(async |tx| Ok(tx.query_one("SELECT f.raw_tx,f.fee_sat FROM expiry_fee_child f \
			JOIN expiry_allocation a ON a.id=f.allocation_id WHERE a.sweep_txid=$1 AND f.txid=$2",
			&[&parent.compute_txid().to_string(),&child.compute_txid().to_string()]).await?)).await.unwrap();
		assert_eq!(bitcoin::consensus::deserialize::<bitcoin::Transaction>(recorded.get("raw_tx")).unwrap(),*child);
		assert_eq!(u64::try_from(recorded.get::<_,i64>("fee_sat")).unwrap(),child_fee.to_sat());
		let admin = srv.watchmand().config().admin_address.unwrap();
		let mut wallet = ark_testing::daemon::captaind::WalletAdminClient::connect(format!("http://{admin}")).await.unwrap();
		tokio::time::timeout(Duration::from_secs(10),async {
			loop {
				let owned = wallet.wallet_status(protos::Empty{}).await.unwrap().into_inner().watchman.unwrap().confirmed_utxos
					.into_iter().map(|point|point.parse::<bitcoin::OutPoint>().unwrap()).collect::<std::collections::BTreeSet<_>>();
				if child.output.iter().enumerate().all(|(index,output)| output.value==sat(0)
					|| owned.contains(&bitcoin::OutPoint::new(child.compute_txid(),index as u32))) { break; }
				tokio::time::sleep(poll_interval()).await;
			}
		}).await.expect("every real fee-child change output belongs to the watchman");
		for (index,output) in child.output.iter().enumerate().filter(|(_,output)|output.value>sat(0)) {
			let actual = chain.get_tx_out(&child.compute_txid(),index as u32,Some(false)).unwrap().unwrap();
			assert_eq!(actual.value,output.value); assert_eq!(actual.script_pub_key.script().unwrap(),output.script_pubkey);
		}
		let mut known_payouts = std::collections::BTreeSet::new();
		for height in 0..=chain.get_block_count().unwrap() {
			for tx in chain.get_block(&chain.get_block_hash(height).unwrap()).unwrap().txdata {
				for (index,output) in tx.output.iter().enumerate().filter(|(_,output)|output.script_pubkey==destination.spk) {
					known_payouts.insert((bitcoin::OutPoint::new(tx.compute_txid(),index as u32),output.value));
				}
			}
		}
		assert_eq!(known_payouts,std::collections::BTreeSet::from([(bitcoin::OutPoint::new(parent.compute_txid(),vout as u32),sat(99_000))]),
			"only one canonical output may pay the known registered destination");
		if own_pending {
			assert!(server::ln::resolve_uninjected_lightning(&cfg, &manifest).await.is_err(),
				"a refused absence incident must never replace the actual failure receipt after payout");
			let row = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) AS receipts, \
				COUNT(*) FILTER(WHERE created_index IS NOT NULL AND jsonrpc_error_code IS NULL \
					AND supervised_manifest_sha256 IS NULL) AS genuine \
				FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt.id]).await?)).await.unwrap();
			assert_eq!(row.get::<_, i64>("receipts"), 1); assert_eq!(row.get::<_, i64>("genuine"), 1);
		} else {
			let mut repeat = srv.get_custom_command(&[
				"resolve-uninjected-lightning", "--manifest", manifest_arg, "--acknowledge-trusted-operations",
			]).await.unwrap();
			repeat.kill_on_drop(true);
			let repeated = repeat.output().await.unwrap();
			assert!(repeated.status.success(), "post-payout CLI repeat failed: {}",String::from_utf8_lossy(&repeated.stderr));
			assert!(String::from_utf8_lossy(&repeated.stdout).contains("same incident already recorded"),
				"the actual CLI must return idempotently after payout, before the decided-source guard");
			let different = manifest.with_file_name("different-receipt-bytes.json");
			std::fs::write(&different,serde_json::to_vec_pretty(&original).unwrap()).unwrap();
			let error = server::ln::resolve_uninjected_lightning(&cfg,&different).await.unwrap_err();
			assert!(format!("{error:#}").contains("different terminal receipt"),"{error:#}");
			let row = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) AS receipts, \
				COUNT(*) FILTER(WHERE created_index IS NOT NULL OR jsonrpc_error_code IS NOT NULL) AS fabricated \
				FROM expiry_lightning_terminal_failure WHERE id=$1", &[&attempt.id]).await?)).await.unwrap();
			assert_eq!(row.get::<_, i64>("receipts"),1); assert_eq!(row.get::<_, i64>("fabricated"),0);
		}
		assert_eq!(db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_lightning_dispatch WHERE id=$1", &[&attempt.id]).await?.get::<_, i64>(0))).await.unwrap(),1);
		let total=db.read(async |tx|Ok(tx.query_one("SELECT COUNT(*) AS attempts, \
			COUNT(*) FILTER(WHERE d.id IS NOT NULL) AS dispatches, COUNT(*) FILTER(WHERE s.id IS NOT NULL) AS success \
			FROM lightning_payment_attempt a LEFT JOIN expiry_lightning_dispatch d ON d.id=a.id \
			LEFT JOIN expiry_lightning_external_success s ON s.id=a.id WHERE a.payment_hash=$1",&[&hash.to_string()]).await?)).await.unwrap();
		assert_eq!(total.get::<_,i64>("attempts"),1); assert_eq!(total.get::<_,i64>("dispatches"),1); assert_eq!(total.get::<_,i64>("success"),0);
		let mut final_node = lightning.internal.grpc_client().await;
		let raw = final_node.list_send_pays(cln_rpc::ListsendpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner();
		let aggregate = final_node.list_pays(cln_rpc::ListpaysRequest {
			payment_hash: Some(hash.to_vec()), ..Default::default()
		}).await.unwrap().into_inner();
		if foreign_complete {
			assert_eq!(raw.payments.len(), 1); assert_eq!(aggregate.pays.len(), 1);
			let payment = &raw.payments[0];
			assert_eq!(payment.label.as_deref(), Some(foreign_label));
			assert_eq!(payment.status(), cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Complete);
			assert_eq!(payment.amount_msat.as_ref().unwrap().msat, 100_000_000);
			assert_eq!(aggregate.pays[0].label.as_deref(), Some(foreign_label));
			assert_eq!(aggregate.pays[0].status(), cln_rpc::listpays_pays::ListpaysPaysStatus::Complete);
			let received = lightning.external.grpc_client().await.list_invoices(cln_rpc::ListinvoicesRequest {
				payment_hash: Some(hash.to_vec()), ..Default::default()
			}).await.unwrap().into_inner();
			assert_eq!(received.invoices.len(), 1);
			assert_eq!(received.invoices[0].status(), cln_rpc::listinvoices_invoices::ListinvoicesInvoicesStatus::Paid);
			assert_eq!(received.invoices[0].amount_received_msat.as_ref().unwrap().msat, 100_000_000);
		} else if own_pending {
			let original = accepted_parts.as_ref().unwrap();
			assert_eq!(raw.payments.len(), original.len()); assert_eq!(aggregate.pays.len(), 1);
			for pay in &raw.payments {
				assert!(pay.payment_hash == hash.to_vec() && pay.payment_preimage.is_none()
					&& pay.status() == cln_rpc::listsendpays_payments::ListsendpaysPaymentsStatus::Failed);
				assert!(original.iter().any(|before| before.created_index == pay.created_index
					&& before.groupid == pay.groupid && before.partid == pay.partid
					&& before.label == pay.label && before.amount_msat == pay.amount_msat),
					"only original injected parts may remain; no new dispatch or replay");
			}
			assert_eq!(raw.payments.iter().map(|pay| pay.amount_msat.as_ref().unwrap().msat).sum::<u64>(), 100_000_000);
			let pay = &aggregate.pays[0];
			let before = &accepted_pays.as_ref().unwrap()[0];
			assert!(pay.payment_hash == hash.to_vec() && pay.preimage.is_none()
				&& pay.status() == cln_rpc::listpays_pays::ListpaysPaysStatus::Failed);
			assert!(pay.created_index == before.created_index && pay.label == before.label);
			// CLN tallies aggregate amounts only for Pending/Complete parts.
			// Failed retains the original raw principal/identities checked above.
			assert!(pay.amount_msat.is_none());
			assert_eq!(pay.amount_sent_msat.as_ref().unwrap().msat, 0);
			let invoices = lightning.external.hold_client().await.list(hold::ListRequest {
				constraint: Some(hold::list_request::Constraint::PaymentHash(hash.to_vec())),
			}).await.unwrap().into_inner().invoices;
			assert_eq!(invoices.len(), 1); assert_eq!(invoices[0].state(), hold::InvoiceState::Cancelled);
			assert!(invoices[0].preimage.is_none());
			assert!(db.read(async |tx| tx.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
			let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
				hash: hash.to_vec(), wait: false, htlc_vtxo_ids: vec![source.to_bytes().to_vec()],
			}).await.unwrap().into_inner();
			assert!(matches!(status.payment_status, Some(protos::lightning_payment_status::PaymentStatus::Failed(_))));
			assert_eq!(status.htlc_vtxo_ids, vec![source.to_bytes().to_vec()]);
		} else {
			assert!(raw.payments.is_empty() && aggregate.pays.is_empty(),
				"restart/repeat must never replay the extinguished original request");
		}
	}).await.expect("supervised original dispatch case exceeded its unchanged 180-second budget");
}

/// Exercise the actual pre-dispatch database boundary with a natively
/// registered signed HTLC. The external xpay task is deliberately not run:
/// this proves admission fencing, not a complete payment-vs-payout RPC race.
#[tokio::test]
async fn expiry_dispatch_rechecks_registered_sources_and_closed_attempts() {
	let ctx = TestContext::new("server/expiry_dispatch_execution_boundary").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal)
		.funded(btc(10)).cfg(|cfg| cfg.vtxo_lifetime = BlockDelta::new(24))
		.watchmand_cfg(|cfg| cfg.watchman.sweep_interval = std::time::Duration::from_secs(3600))
		.create().await;
	let sender = ctx.bark("sender", &srv).funded(sat(500_000)).create().await;
	let ids = sender.board_and_confirm_and_register(&ctx, sat(100_000)).await;
	let wallet = sender.client().await;
	let input = wallet.get_full_vtxo(ids[0]).await.unwrap();
	let key = wallet.get_vtxo_key(&input).await.unwrap();
	let preimage_bytes = [12;32];
	let invoice = lightning.external.invoice_with_preimage(Some(sat(100_000)), "dispatch-boundary", "Registered source dispatch fence", preimage_bytes).await;
	let parsed: bark::lightning_invoice::Bolt11Invoice = invoice.parse().unwrap();
	let hash = PaymentHash::from(&parsed);
	let tip = BlockHeight::new(ctx.bitcoind().get_block_count().await as u32);
	let deadline = tip + srv.config().htlc_send_expiry_delta + BlockDelta::new(2);
	let builder = ArkoorPackageBuilder::new_claim_all_with_checkpoints(
		[input.clone()], VtxoPolicy::new_server_htlc_send(key.public_key(), hash, deadline),
	).unwrap().generate_user_nonces(&[key]).unwrap();
	let mut rpc = srv.get_public_rpc().await;
	let response = rpc.request_lightning_pay_htlc_cosign(protos::LightningPayHtlcCosignRequest {
		parts: protos::ArkoorPackageCosignRequest::from(builder.cosign_request()).parts,
	}).await.unwrap().into_inner();
	let response = ark::arkoor::package::ArkoorPackageCosignResponse::try_from(response).unwrap();
	let htlcs = builder.user_cosign(&[key], response).unwrap().build_signed_vtxos();
	wallet.register_vtxo_transactions_with_server(&htlcs).await.unwrap();
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let htlc = &htlcs[0];
	let invoice = Invoice::Bolt11(parsed);
	let node_id = db.read(async |t| Ok(t.query_one("
		SELECT id
		FROM lightning_node
		ORDER BY id
		LIMIT 1
	", &[]).await?.get::<_, i64>("id"))).await.unwrap();
	let attempt = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, sat(100_000), None,
			&[htlc.id()], None, tip, sat(0), None, Duration::from_secs(1), None).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else( || anyhow::anyhow!("attempt disappeared"))
	}).await.unwrap();
	assert!(db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap());
	let separate_dispatcher = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	assert!(!separate_dispatcher.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap(),
		"a separate connection cannot dispatch the same durable attempt twice");
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&attempt,
		LightningPaymentStatus::Failed, Some("closed before queued dispatch")).await).await.unwrap().is_some());
	let replacement = db.write(async |t| {
		t.store_lightning_payment_start(node_id, &invoice, sat(100_000), None,
			&[htlc.id()], None, tip, sat(0), None, Duration::from_secs(1), None).await?;
		t.get_open_lightning_payment_attempt_by_payment_hash(hash).await?.ok_or_else( || anyhow::anyhow!("replacement disappeared"))
	}).await.unwrap();
	assert_ne!(replacement.id, attempt.id);
	assert!(!db.write(async |t| t.admit_lightning_dispatch(&attempt).await).await.unwrap(),
		"an obsolete queued task cannot dispatch a replacement attempt");
	assert!(db.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap());
	let expired = ctx.generate_blocks(input.expiry_height().to_u32() + 1 - tip.to_u32()).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let error = db.write(async |t| t.admit_lightning_dispatch(&replacement).await).await.unwrap_err();
	let message = format!("{error:#}");
	assert!(message.contains("expired backing") && message.contains(&htlc.id().to_string()), "{message}");
	assert_eq!(db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().id,
		replacement.id, "refused dispatch does not create or replace an attempt");
}

/// A real external receiver settles after the coarse attempt row was made
/// stale. The actual preimage must still reach the monetary settlement WAL
/// and the public payment status must reflect it.
#[tokio::test]
async fn expiry_xpay_preserves_late_preimage_after_coarse_failure() {
	let ctx = TestContext::new("server/expiry_xpay_late_preimage").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(btc(10)).create().await;
	let wallet = ctx.bark_sdk("sender", &srv).boarded(btc(2)).create().await;
	lightning.sync().await;
	let preimage = Preimage::random();
	let hash = preimage.compute_payment_hash();
	let mut receiver = lightning.external.hold_client().await;
	let invoice = receiver.invoice(hold::InvoiceRequest {
		payment_hash: hash.to_vec(), amount_msat: 100_000_000,
		description: Some(hold::invoice_request::Description::Memo("late-preimage-after-stale-status".into())),
		min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
	}).await.unwrap().into_inner().bolt11;
	wallet.pay_lightning_invoice(invoice, None, false).await.unwrap();
	lightning.external.wait_for_hold_invoice_accepted(hash).await;
	let db = server::database::Db::connect(&srv.config().postgres).await.unwrap();
	let attempt = db.read(async |t| t.get_open_lightning_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap();
	let source_ids = db.read(async |tx| Ok(tx.query("SELECT vtxo_id FROM lightning_payment_attempt_htlc_vtxo \
		WHERE lightning_payment_attempt_id=$1 ORDER BY vtxo_id", &[&attempt.id]).await?
		.into_iter().map(|row| row.get::<_, &str>(0).parse::<ark::VtxoId>()).collect::<Result<Vec<_>, _>>()?))
		.await.unwrap();
	assert!(!source_ids.is_empty(), "the real native payment must bind its complete signed source set");
	let source_expiries = db.read(async |tx| Ok(tx.query("SELECT v.expiry,h.htlc_expiry \
		FROM lightning_payment_attempt_htlc_vtxo l JOIN vtxo v ON v.vtxo_id=l.vtxo_id \
		JOIN htlc_vtxo h ON h.id=v.id WHERE l.lightning_payment_attempt_id=$1", &[&attempt.id]).await?
		.into_iter().map(|row| (row.get::<_, i32>("expiry"), row.get::<_, i32>("htlc_expiry"))).collect::<Vec<_>>()))
		.await.unwrap();
	assert_eq!(source_expiries.len(), source_ids.len());
	// Inject the stale status at the actual database writer; neither the
	// receiver's HTLC nor the real xpay call is synthesized or cancelled.
	assert!(db.write(async |t| t.update_lightning_payment_attempt_status(&attempt,
		LightningPaymentStatus::Failed, Some("injected stale coarse reconciliation")).await).await.unwrap().is_some());
	assert!(db.read(async |t| t.get_htlc_settlement_by_payment_hash(hash).await).await.unwrap().is_none());
	receiver.settle(hold::SettleRequest { payment_preimage: preimage.to_vec() }).await.unwrap();
	async {
		loop {
			let proven = db.read(async |tx| Ok(tx.get_htlc_settlement_by_payment_hash(hash).await? == Some(preimage)
								&& tx.query_opt("SELECT id FROM expiry_lightning_external_success WHERE id=$1", &[&attempt.id]).await?.is_some()
				&& !tx.query_one("SELECT EXISTS(SELECT 1 FROM lightning_payment_attempt_htlc_vtxo l \
					JOIN vtxo v ON v.vtxo_id=l.vtxo_id WHERE l.lightning_payment_attempt_id=$1 \
					AND v.spend_state<>'ln-spent')", &[&attempt.id]).await?.get::<_, bool>(0)))
				.await.unwrap();
			if proven { break; }
			tokio::time::sleep(poll_interval()).await;
		}
	}.wait_millis(30_000).await;
	let receipt_tip = ctx.bitcoind().get_block_count().await;
	assert!(source_expiries.iter().all(|(backing, htlc)|
		receipt_tip < u64::try_from(*backing).unwrap() && receipt_tip < u64::try_from(*htlc).unwrap()),
		"authentic success receipt must arrive before every actual backing/HTLC expiry, not wait for fallback eligibility");
	let status = srv.get_public_rpc().await.check_lightning_payment(protos::CheckLightningPaymentRequest {
		hash: hash.to_vec(), wait: false,
		htlc_vtxo_ids: source_ids.iter().map(|source| source.to_bytes().to_vec()).collect(),
	}).await.unwrap().into_inner();
	let mut expected_sources = source_ids.iter().map(|source| source.to_bytes().to_vec()).collect::<Vec<_>>();
	expected_sources.sort_unstable();
	let mut echoed_sources = status.htlc_vtxo_ids.clone();
	echoed_sources.sort_unstable();
	assert_eq!(echoed_sources, expected_sources, "success must echo the complete actual signed payment scope");
	assert!(matches!(status.payment_status,
		Some(protos::lightning_payment_status::PaymentStatus::Success(ref success))
		if success.preimage == preimage.to_vec()), "{status:?}");
	let success_tip = ctx.bitcoind().get_block_count().await;
	assert!(source_expiries.iter().all(|(backing, htlc)|
		success_tip < u64::try_from(*backing).unwrap() && success_tip < u64::try_from(*htlc).unwrap()),
		"public Success must be usable before source expiry too");
	assert_eq!(db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await).await.unwrap().unwrap().status,
		LightningPaymentStatus::Failed, "the public result must use actual settlement rather than trusting this stale row");
}
