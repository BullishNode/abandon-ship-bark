use bitcoin::hex::DisplayHex;
const DIRECT_TERMINAL_COUNT:&str="SELECT count(*),count(DISTINCT txid) FROM expiry_settlement WHERE id=ANY($1)";
fn require_operating_build(){
	let path=std::env::var("SCALE_OPERATING_BUILD_RECEIPT").expect("frozen A benchmark authority required");
	let r:Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
	assert_eq!(r["architecture_profile"],"two_step_per_key");
	assert_eq!(r["SDK_commit"],"e2291996ac2713227fd25ead508abe800373f012");
	assert_eq!(r["common_commit"],"25916ec4ba87a1f72bf247eb522106128b556c46");
	assert_eq!(r["route"],"two_step_reserved_sweep_then_payout");
}
fn assert_fresh_key(key:String){
	let path=std::env::var("SCALE_PRIOR_COHORT_KEYS").expect("actual earlier public-key exclusion source required");
	let bytes=std::fs::read(path).unwrap();assert!(bytes.len()<=16*1024*1024);
	let old:Vec<String>=serde_json::from_slice(&bytes).unwrap();assert!(old.len()<=100000);
	assert!(!old.contains(&key),"new actual wallet/entitlement key reused an earlier cohort key");
}
async fn account_arkoor_request(_wallet:&bark::Wallet,_recipient:&ark::Address,request:protos::ArkoorPackageCosignRequest)->protos::ArkoorPackageCosignRequest{
	// A's actual RequestArkoorCosign schema has no account-child origins.
	// Server require_fallback verifies linked output keys; no fabricated origin fields.
	request
}
async fn read_owning_wallets(srv: &Captaind) -> Value {
	let rounds = srv.get_wallet_rpc().await.wallet_inventory(protos::Empty {}).await.unwrap().into_inner();
	let mut rpc = server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap())).await.unwrap();
	let watchman = rpc.wallet_inventory(protos::Empty {}).await.unwrap().into_inner();
	let r = rounds.rounds.expect("captaind owns rounds inventory");
	let w = watchman.watchman.expect("watchmand owns fee inventory");
	assert!(r.complete && w.complete, "incomplete owning-wallet inventory");
	json!({ "rounds": { "total_sat":r.utxos.iter().map(|u| u.value_sat).sum::<u64>(), "public_descriptor":r.public_descriptor,
		"tip":r.tip_height,"reserved_outpoints":r.reserved_outpoints },
		"watchman": { "total_sat":w.utxos.iter().map(|u| u.value_sat).sum::<u64>(),"public_descriptor":w.public_descriptor,
		"tip":w.tip_height,"reserved_outpoints":w.reserved_outpoints },"atomic":false,"treasury_complete":false })
}

async fn reconcile(env: &Env, owners: &[Owner], enabled_utc: f64) -> Value {
	let core = env.core();
	let mut failures = Vec::<String>::new();
	let rows = env.db.read(async |t| Ok(t.query(
		"SELECT s.id, s.txid, s.fee_sat, s.spk, extract(epoch FROM s.created_at)::FLOAT8, v.amount
		 FROM expiry_settlement s JOIN vtxo v ON v.vtxo_id = s.id", &[],
	).await?)).await.unwrap();
	struct Row { txid: String, fee: u64, spk: Vec<u8>, created: f64, amount: u64 }
	let settled = rows.into_iter().map(|r| (r.get::<_, String>(0), Row {
		txid: r.get(1), fee: r.get::<_, i64>(2) as u64, spk: r.get::<_, Option<Vec<u8>>>(3).unwrap_or_default(),
		created: r.get(4), amount: r.get::<_, i64>(5) as u64,
	})).collect::<HashMap<_, _>>();
	// 1. Each expected coin settled exactly once, to its owner, for its amount.
	let mut expected = HashMap::<String, (usize, u64)>::new();
	for (k, o) in owners.iter().enumerate() {
		for c in &o.coins {
			assert_fresh_key(c.user_pubkey.clone());
			if expected.insert(c.id.clone(), (k, c.amount)).is_some() { failures.push(format!("coin {} listed twice", c.id)); }
			match settled.get(&c.id) {
				None => failures.push(format!("owner {} coin {} ({}) not settled", o.index, c.id, c.source)),
				Some(r) => {
					if r.spk != o.spk.as_bytes() { failures.push(format!("owner {} coin {} paid to another script", o.index, c.id)); }
					if r.amount != c.amount { failures.push(format!("coin {} server amount {} != funded {}", c.id, r.amount, c.amount)); }
				},
			}
		}
	}
	let others = settled.keys().filter(|id| !expected.contains_key(*id)).cloned().collect::<Vec<_>>();
	if !others.is_empty() { failures.push(format!("{} settled coins outside the cohort: {:?}", others.len(), &others[..others.len().min(10)])); }
	// 2. Per payout transaction: one fee for all its rows, one output per
	// destination, deductions adding up to that fee, operator pays the rest.
	let mut by_tx = BTreeMap::<String, Vec<&Row>>::new();
	for r in settled.values() { by_tx.entry(r.txid.clone()).or_default().push(r); }
	let nursery = env.db.read(async |t| Ok(t.query(
		"SELECT txid FROM nursery_tx WHERE kind::TEXT='expiry-payout'", &[],
	).await?)).await.unwrap().into_iter().map(|r| r.get::<_, String>(0)).collect::<BTreeSet<_>>();
	if nursery != by_tx.keys().cloned().collect::<BTreeSet<_>>() {
		failures.push(format!("nursery payouts {} != settlement txids {}", nursery.len(), by_tx.len()));
	}
	let mut paid_out = HashMap::<Vec<u8>, Vec<(OutPoint, u64)>>::new();
	let mut txs = Vec::new();
	let (mut sum_gross, mut sum_net, mut sum_recipient_fee, mut sum_tx_fee) = (0u64, 0u64, 0u64, 0u64);
	for (txid_s, rows) in &by_tx {
		let txid: Txid = txid_s.parse().unwrap();
		let info = core.get_raw_transaction_info(&txid, None).unwrap();
		let tx: Transaction = core.get_raw_transaction(&txid, None).unwrap();
		let fees = rows.iter().map(|r| r.fee).collect::<BTreeSet<_>>();
		if fees.len() != 1 { failures.push(format!("{txid}: rows carry different fee_sat {fees:?}")); }
		let fee = *fees.iter().next().unwrap();
		let mut gross = BTreeMap::<Vec<u8>, u64>::new();
		for r in rows { *gross.entry(r.spk.clone()).or_default() += r.amount; }
		let mut deducted = 0u64;
		for (spk, g) in &gross {
			let outs = tx.output.iter().enumerate().filter(|(_, o)| o.script_pubkey.as_bytes() == spk.as_slice()).collect::<Vec<_>>();
			if outs.len() != 1 { failures.push(format!("{txid}: {} outputs for one destination", outs.len())); continue; }
			let (vout, o) = outs[0];
			match g.checked_sub(o.value.to_sat()) {
				Some(d) => deducted += d,
				None => failures.push(format!("{txid}: output exceeds its gross")),
			}
			paid_out.entry(spk.clone()).or_default().push((OutPoint::new(txid, vout as u32), o.value.to_sat()));
			sum_net += o.value.to_sat();
		}
		if deducted != fee { failures.push(format!("{txid}: deductions {deducted} != fee_sat {fee}")); }
		let total_in = tx.input.iter().map(|i| core.get_raw_transaction(&i.previous_output.txid, None).unwrap()
			.output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		let tx_fee = total_in - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
		let change = tx.output.iter().filter(|o| !gross.contains_key(o.script_pubkey.as_bytes())).collect::<Vec<_>>();
		let operator = tx_fee.checked_sub(fee);
		let (weight, change_weight) = (tx.weight().to_wu(), change.first().map(|c| c.weight().to_wu()).unwrap_or(0));
		let operator_ok = change.len() == 1 && operator.is_some_and(|op| op > 0
			&& op * weight > (tx_fee - 1) * change_weight && op * weight < tx_fee * change_weight + weight);
		if !operator_ok { failures.push(format!("{txid}: operator part {operator:?} of {tx_fee} for change {change_weight}/{weight} WU, {} change outputs", change.len())); }
		if info.confirmations.unwrap_or(0) == 0 { failures.push(format!("{txid}: unconfirmed")); }
		sum_gross += gross.values().sum::<u64>();
		sum_recipient_fee += fee;
		sum_tx_fee += tx_fee;
		let created = rows.iter().map(|r| r.created).fold(f64::NAN, f64::min);
		txs.push(json!({ "txid": txid_s, "coins": rows.len(), "destinations": gross.len(), "gross_sat": gross.values().sum::<u64>(),
			"recipient_fee_sat": fee, "tx_fee_sat": tx_fee, "operator_fee_sat": operator, "weight": weight,
			"vsize": tx.vsize(), "committed_utc": created, "blocktime": info.blocktime,
			"confirmations": info.confirmations, "inputs": tx.input.len(), "outputs": tx.output.len() }));
	}
	// 3. Per owner: funded principal = settled gross; net = principal - fee shares.
	let mut owner_rows = Vec::new();
	for o in owners {
		let funded = o.coins.iter().map(|c| c.amount).sum::<u64>();
		let gross = o.coins.iter().filter_map(|c| settled.get(&c.id)).map(|r| r.amount).sum::<u64>();
		let net = paid_out.get(o.spk.as_bytes()).map(|v| v.iter().map(|(_, a)| a).sum::<u64>()).unwrap_or(0);
		if funded != gross { failures.push(format!("owner {}: funded {funded} != settled {gross}", o.index)); }
		if net == 0 || net > gross { failures.push(format!("owner {}: net {net} for gross {gross}", o.index)); }
		owner_rows.push(json!({ "index": o.index, "funded_sat": funded, "net_sat": net,
			"outputs": paid_out.get(o.spk.as_bytes()).map(|v| v.iter().map(|(op, a)| json!([op.to_string(), a])).collect::<Vec<_>>()) }));
	}
	let ages = settled.values().map(|r| r.created - enabled_utc).collect::<Vec<_>>();
	let tip = core.get_block_count().unwrap() as i32;
	let unswept_candidates = env.db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM vtxo WHERE policy_type='pubkey' AND spend_state::TEXT IN ('spendable','unclaimed','unregistered')
		 AND expiry < $1", &[&tip],
	).await?)).await.expect("actual complete expiry counter query").get::<_,i64>(0);
	json!({
		"failures": failures,
		"settled_coins": settled.len(), "expected_coins": expected.len(),
		"payout_txs": txs, "gross_sat": sum_gross, "net_sat": sum_net,
		"recipient_fee_sat": sum_recipient_fee, "tx_fee_sat": sum_tx_fee,
		"operator_fee_sat": sum_tx_fee - sum_recipient_fee,
		"commit_age_after_enable_s": { "p50": quantile(&ages, 0.5), "p95": quantile(&ages, 0.95),
			"p99": quantile(&ages, 0.99), "max": quantile(&ages, 1.0) },
		"expired_unsettled_pubkey_coins_server_wide_raw_including_padding": unswept_candidates,
		"owners": owner_rows,
	})
}

fn fee_quote_frame(env:&Env,label:&str){
	let core=env.core();
	let quotes=[1u16,3,6].into_iter().map(|target|{
		let response:Value=core.call("estimatesmartfee",&[target.into(),"economical".into()]).unwrap();
		json!({"target":target,"actual_core_response":response})
	}).collect::<Vec<_>>();
	env.out.line("fee-oracle-frames.jsonl",&json!({"utc":utc(),"label":label,"height":core.get_block_count().unwrap(),"quotes":quotes,"configured_normal_ceiling_sat_vb":5,"A_pool_target":"SLOW6","same_inputs_not_assumed_equal_selected_targets":true}));
}

async fn record_ln_boundary(env:&Env,lightning:&Option<ark_testing::context::LightningPaymentSetup>) {
	if let Some(ln)=lightning { env.out.json("ln-boundary.json",&json!({"included_node_id":ln.internal.id().await.to_lower_hex_string(),"excluded_payer_payee_id":ln.external.id().await.to_lower_hex_string(),"receipts_require_paid_external_intent":true})); }
}

async fn treasury_edge(env:&Env,srv:&Captaind,label:&str)->TreasuryEdge {
	tokio::time::timeout(Duration::from_secs(600),treasury_edge_inner(env,srv,label,true)).await.expect("bounded complete treasury edge failed")
}

async fn treasury_edge_after_recovery(env:&Env,srv:&Captaind,label:&str)->TreasuryEdge {
	// captaind remains stopped: observe persisted public BDK state and Core only.
	tokio::time::timeout(Duration::from_secs(600),treasury_edge_inner(env,srv,label,false)).await.expect("bounded post-recovery treasury edge failed")
}

// Whole physical treasury evidence. These are assets, never Ark claim sums.
// Chain/LN frames remain separately timed; a moving edge is refused, not called atomic.
#[derive(Clone)]
struct TreasuryEdge { balance_msat: u64, frame: Value, height: u64, received_msat: u64, paid_msat: u64, router_msat: u64 }

fn evidence_rows(env: &Env, name: &str) -> Vec<Value> {
	let path=env.out.dir.join(name);
	if !path.exists() { return Vec::new(); }
	let text=std::fs::read_to_string(path).unwrap();
	assert!(text.len()<=128*1024*1024,"whole evidence byte budget");
	let rows=text.lines().map(|l|serde_json::from_str(l).unwrap()).collect::<Vec<_>>();
	assert!(rows.len()<=100_000,"whole evidence row budget"); rows
}

fn canonical_asset(core: &bitcoin_ext::rpc::BitcoinRpcClient, point: OutPoint, account: &str) -> Option<Value> {
	let unspent=core.get_tx_out(&point.txid,point.vout,Some(true)).unwrap()?;
	Some(json!({"outpoint":point.to_string(),"account":account,"value_sat":unspent.value.to_sat(),
		"actual_unspent":true,"actual_confirmed":unspent.confirmations>0,"script":unspent.script_pub_key.hex.to_lower_hex_string()}))
}

async fn accounting_sources(env:&Env) -> Value {
	// Database facts are source evidence, never a substituted chain confirmation.
	let roles=env.db.read(async|t|Ok(t.query("(SELECT txid,kind::TEXT FROM nursery_tx UNION ALL SELECT txid,'sweep' FROM sweep UNION ALL SELECT funding_txid,'round' FROM round UNION ALL SELECT DISTINCT txid,'expiry-payout' FROM expiry_settlement) LIMIT 100001",&[]).await?)).await.unwrap();
	assert!(roles.len()<=100000,"bounded complete native carrier roles");
	let roles=roles.into_iter().map(|r|json!({"txid":r.get::<_,String>(0),"kind":r.get::<_,String>(1)})).collect::<Vec<_>>();
	let pending=env.db.read(async|t|Ok(t.query("WITH bounded AS MATERIALIZED (
		SELECT v.vtxo_id,v.amount,v.policy_type,v.spend_state,v.vtxo FROM vtxo v
		WHERE v.spend_state::TEXT IN ('spendable','unclaimed','unregistered') AND v.onchain_spent_txid IS NULL
		AND NOT EXISTS(SELECT 1 FROM expiry_settlement s WHERE s.id=v.vtxo_id) LIMIT 100001),
		size AS(SELECT count(*) AS n,coalesce(sum(octet_length(vtxo)),0)::BIGINT AS bytes FROM bounded)
		SELECT size.n,size.bytes,b.vtxo_id,b.amount,b.policy_type::TEXT,b.spend_state::TEXT,
		CASE WHEN size.n<=100000 AND size.bytes<=134217728 THEN b.vtxo ELSE NULL END
		FROM size LEFT JOIN bounded b ON true",&[]).await?)).await.unwrap();
	assert!(pending.len()<=100000,"bounded current liability candidates");
	let links=env.db.read(async|t|Ok(t.query("SELECT user_pubkey FROM key_link LIMIT 100001",&[]).await?)).await.unwrap();
	assert!(links.len()<=100000);
	let linked=links.into_iter().map(|r|r.get::<_,Vec<u8>>(0)).collect::<BTreeSet<_>>();
	let mut candidates=Vec::new();let mut ordinary=0u64;let mut conditional=0u64;let mut unlinked_ordinary=0u64;
	for r in pending {
		assert!(r.get::<_,i64>(0)<=100000 && r.get::<_,i64>(1)<=134217728,"current liability source bounds");
		let Some(bytes)=r.get::<_,Option<Vec<u8>>>(6) else {continue;};assert!(bytes.len()<=4*1024*1024);
		let coin:Vtxo<ark::vtxo::Full,ark::ServerVtxoPolicy>=Vtxo::deserialize(&bytes).unwrap();
		let key=coin.policy().user_pubkey();let has_link=key.is_some_and(|k|linked.contains(&k.serialize().to_vec()));
		let policy=r.get::<_,String>(4);let amount=u64::try_from(r.get::<_,i64>(3)).unwrap();
		if policy=="pubkey" {if has_link {ordinary=ordinary.checked_add(amount).unwrap();}else{unlinked_ordinary=unlinked_ordinary.checked_add(amount).unwrap();}}
		if policy!="pubkey" {conditional=conditional.checked_add(amount).unwrap();}
		candidates.push(json!({"id":r.get::<_,String>(2),"amount_sat":amount,"policy":policy,"state":r.get::<_,String>(5),"actual_per_key_link_present":has_link,"conditional_arbitration_not_inferred":true}));
	}
	let settlements=env.db.read(async|t|Ok(t.query("SELECT s.id,s.txid,s.fee_sat,s.spk,v.amount FROM expiry_settlement s JOIN vtxo v ON v.vtxo_id=s.id LIMIT 100001",&[]).await?)).await.unwrap();
	assert!(settlements.len()<=100000);
	let mut selected=Vec::new();let mut committed_pending=0u64;let mut fees=BTreeMap::<String,u64>::new();
	for r in settlements {
		let txid=r.get::<_,String>(1);let amount=u64::try_from(r.get::<_,i64>(4)).unwrap();
		let charge=u64::try_from(r.get::<_,i64>(2)).unwrap();
		if let Some(old)=fees.insert(txid.clone(),charge) {assert_eq!(old,charge,"one recipient fee per actual carrier");}
		let confirmations=match env.core().get_raw_transaction_info(&txid.parse().unwrap(),None) {
			Ok(info)=>info.confirmations.unwrap_or(0),
			Err(bitcoin_ext::rpc::Error::JsonRpc(bitcoin_ext::rpc::jsonrpc::Error::Rpc(e))) if e.code==-5=>0,
			Err(e)=>panic!("actual signed payout lookup failed:{e}"),
		};
		if confirmations==0 {committed_pending=committed_pending.checked_add(amount).unwrap();}
		selected.push(json!({"id":r.get::<_,String>(0),"txid":txid,"gross_sat":amount,"recipient_script":r.get::<_,Option<Vec<u8>>>(3).map(|b|b.to_lower_hex_string()),"recipient_fee_per_carrier_sat":charge,"actual_confirmations":confirmations}));
	}
	let hold=env.db.read(async|t|Ok(t.query_one("SELECT (SELECT coalesce(sum(held_sat),0)::TEXT FROM recovery_source_hold),EXISTS(SELECT 1 FROM recovery_receipt WHERE proof_state='unknown_gross')",&[]).await?)).await.unwrap();
	json!({"carrier_roles":roles,"current_candidates":candidates,"known_linked_ordinary_liability_sat":ordinary,"unlinked_ordinary_candidate_sat":unlinked_ordinary,"conditional_candidate_sat":conditional,
		"committed_pending_payout_gross_sat":committed_pending,"settlement_rows":selected,"recipient_fee_by_carrier":fees,
		"recovery_held_sat":hold.get::<_,String>(0).parse::<u64>().unwrap(),"recovery_unknown_gross":hold.get::<_,bool>(1),
		"global_economic_group_attribution_complete":false,"no_forfeit_inferred_from_missing_settlement":true})
}

async fn treasury_edge_inner(env: &Env, srv: &Captaind, label: &str, owning_daemons_live: bool) -> TreasuryEdge {
	let deadline=Instant::now()+Duration::from_secs(600);
	fee_quote_frame(env,label);
	let accounting=accounting_sources(env).await;
	let fees=&srv.config().fees;
	assert_eq!(fees.board.min_fee,Amount::ZERO);assert_eq!(fees.board.base_fee,Amount::ZERO);
	assert_eq!(fees.board.ppm,ark::fees::PpmFeeRate::ZERO);
	assert_eq!(fees.refresh.base_fee,Amount::ZERO);assert!(fees.refresh.ppm_expiry_table.iter().all(|e|e.ppm==ark::fees::PpmFeeRate::ZERO));
	assert_eq!(fees.lightning_receive.base_fee,Amount::ZERO);assert_eq!(fees.lightning_receive.ppm,ark::fees::PpmFeeRate::ZERO);
	assert_eq!(fees.lightning_send.min_fee,Amount::ZERO);assert_eq!(fees.lightning_send.base_fee,Amount::ZERO);assert!(fees.lightning_send.ppm_expiry_table.iter().all(|e|e.ppm==ark::fees::PpmFeeRate::ZERO));
	let core=env.core(); let start=core.get_best_block_hash().unwrap(); let height=core.get_block_count().unwrap();
	let mut assets=BTreeMap::<String,Value>::new();
	let mut observed=["native_actors".to_owned(),"training_wallets".to_owned()].into_iter().collect::<BTreeSet<String>>(); let mut descriptors=Vec::<Value>::new();
	let mut add=|asset:Value| {
		let point=asset["outpoint"].as_str().unwrap().to_owned();
		if let Some(existing)=assets.get(&point) { assert_eq!(existing["value_sat"],asset["value_sat"],"same physical point disagrees"); }
		else { assets.insert(point,asset); }
	};
	let bank_desc:Value=core.call("listdescriptors",&[false.into()]).unwrap();
	assert!(!bank_desc.to_string().contains("prv"),"only public Core descriptors may leave the wallet");
	descriptors.push(json!({"account":"bank","source":"actual Core listdescriptors(private=false)","data":bank_desc}));
	observed.insert("bank".into());
	let locked:Vec<OutPoint>=core.call("listlockunspent",&[]).unwrap();
	for point in locked { if let Some(a)=canonical_asset(&core,point,"bank") { add(a); } }
	for u in core.list_unspent(Some(0),None,None,None,None).unwrap() {
		if let Some(a)=canonical_asset(&core,OutPoint::new(u.txid,u.vout),"bank") { add(a); }
	}
	for account in ["rounds", "watchman"] {
		let response = if owning_daemons_live {
			let response = if account == "rounds" {
				srv.get_wallet_rpc().await.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().rounds
			} else {
				server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap()))
					.await.unwrap().wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman
			};
			let inventory = response.expect("actual owning wallet source");
			assert!(inventory.complete, "owning wallet refused whole inventory");
			Some(inventory)
		} else { None };
		observed.insert(account.into());
		let kind=if account=="rounds" {server::wallet::WalletKind::Rounds}else{server::wallet::WalletKind::Watchman};
		let cs=env.db.read(async |t| {
			let size=t.query_one("SELECT count(*),coalesce(sum(octet_length(content)),0)::BIGINT FROM wallet_changeset WHERE kind=$1::TEXT::wallet_kind",&[&kind.name()]).await?;
			assert!(size.get::<_,i64>(0)<=100_000 && size.get::<_,i64>(1)<=128*1024*1024,"complete wallet descriptor source budget");t.read_aggregate_changeset(kind).await }).await.unwrap().expect("actual persisted descriptors");
		let mut public_desc=vec![cs.descriptor.as_ref().expect("actual external descriptor").to_string()];
		// The real server creates a single-keychain wallet; internal is optional.
		if let Some(change)=cs.change_descriptor.as_ref() { public_desc.push(change.to_string()); }
		assert!(public_desc.iter().all(|d|!d.contains("prv")),"no private descriptors in evidence");
		let public_wallet=bdk_wallet::Wallet::load().check_network(Network::Regtest)
			.load_wallet_no_persist(cs).unwrap().expect("nonempty actual public changeset");
		descriptors.push(json!({"account":account,"actual_public_descriptors":public_desc,
			"source":if owning_daemons_live {"owning RPC plus persisted public BDK and raw Core"} else {"persisted public BDK plus raw Core; no wallet process opened"},
			"owning_RPC":response.as_ref().map(|i|json!({"tip":i.tip_hash,"reserved_outpoints":i.reserved_outpoints}))}));
		// Include recorded graph outputs even when a pending spend hides an input.
		for (point,output) in public_wallet.tx_graph().all_txouts() {
			if public_wallet.is_mine(output.script_pubkey.clone()) {
				if let Some(a)=canonical_asset(&core,point,account) { add(a); }
			}
		}
		for keychain in [KeychainKind::External,KeychainKind::Internal] {
			let Some(last)=public_wallet.derivation_index(keychain) else { continue; };
			assert!(last<=100_000,"bounded actual revealed wallet range");
			let descriptor=public_wallet.public_descriptor(keychain).to_string();
			let scan:Value=core.call("scantxoutset",&["start".into(),json!([{"desc":descriptor,"range":[0,last]}])]).unwrap();
			assert_eq!(scan["success"],true);
			for u in scan["unspents"].as_array().unwrap() {
				let point=OutPoint::new(u["txid"].as_str().unwrap().parse().unwrap(),u32::try_from(u["vout"].as_u64().unwrap()).unwrap());
				add(canonical_asset(&core,point,account).expect("actual public descriptor unspent"));
			}
		}
	}
	// Decode actual bounded full source paths, including every transaction output.
	// Root/leaf-only lookup misses a progressed sibling or P2A residual.
	let source_rows=env.db.read(async |t| Ok(t.query("WITH inventory AS MATERIALIZED (
	SELECT vtxo FROM vtxo UNION ALL SELECT p.vtxo FROM pending_board p
	WHERE NOT EXISTS(SELECT 1 FROM vtxo v WHERE v.vtxo_id=p.vtxo_id)
	UNION ALL SELECT p.vtxo FROM vtxo_pool p WHERE NOT EXISTS(SELECT 1 FROM vtxo v WHERE v.vtxo_id=p.vtxo_id)
	), bounded AS MATERIALIZED (SELECT vtxo FROM inventory LIMIT 100001),
	size AS (SELECT count(*) AS rows,coalesce(sum(octet_length(vtxo)),0)::BIGINT AS bytes FROM bounded)
	SELECT size.rows,size.bytes,CASE WHEN size.rows<=100000 AND size.bytes<=134217728 THEN b.vtxo ELSE NULL END AS vtxo
	FROM size LEFT JOIN bounded b ON true",&[]).await?)).await.unwrap();
	let mut physical_points=BTreeSet::new();
	for row in source_rows {
		assert!(row.get::<_,i64>(0)<=100_000 && row.get::<_,i64>(1)<=128*1024*1024,"complete full-source row/byte budget");
		if let Some(bytes)=row.get::<_,Option<Vec<u8>>>(2) {
			let full:Vtxo<ark::vtxo::Full,ark::ServerVtxoPolicy>=Vtxo::deserialize(&bytes).expect("actual full source proof");physical_points.insert(full.chain_anchor());
			for node in full.transactions() {
				for index in 0..node.tx.output.len() {physical_points.insert(OutPoint::new(node.tx.compute_txid(),u32::try_from(index).unwrap()));}
			}
		}
	}
	let funding_txids=physical_points.iter().map(|p|p.txid).collect::<BTreeSet<_>>();
	// Known actual chain transactions may be partial tree progress. Include all
	// canonical unspent outputs, including funding/P2A residuals, once.
	for txid in funding_txids {
		assert!(Instant::now()<deadline,"bounded complete physical funding observation");
		match core.get_raw_transaction(&txid,None) {
			Ok(tx)=>for index in 0..tx.output.len() {physical_points.insert(OutPoint::new(txid,u32::try_from(index).unwrap()));},
			Err(bitcoin_ext::rpc::Error::JsonRpc(bitcoin_ext::rpc::jsonrpc::Error::Rpc(e))) if e.code==-5=>{},
			Err(e)=>panic!("cannot read actual funding/progress transaction: {e}"),
		}
		assert!(physical_points.len()<=100_000,"complete derived physical point budget");
	}
	assert!(physical_points.len()<=100_000,"complete derived physical point budget");
	observed.insert("protocol_backing".into());
	for point in &physical_points {
		assert!(Instant::now()<deadline,"bounded complete treasury observation");
		if let Some(mut a)=canonical_asset(&core,*point,"protocol_backing") {
			a["physical_source_path"]=json!(true);
			a["p2a_nonexclusive_residual"]=json!(a["script"]=="51024e73");
			a["residual_is_not_operator_surplus"]=json!(true);add(a);
		}
	}
	let native=evidence_rows(env,"native-public-descriptors.jsonl");
	let mut scans=Vec::new();
	for d in &native {
		for desc in d["descriptors"].as_array().unwrap() { scans.push(json!({"desc":desc,"range":[0,19]})); }
	}
	descriptors.extend(native);
	if !scans.is_empty() {
		let scan:Value=core.call("scantxoutset",&["start".into(),json!(scans)]).unwrap(); assert_eq!(scan["success"],true);
		for u in scan["unspents"].as_array().unwrap() {
			let p=OutPoint::new(u["txid"].as_str().unwrap().parse().unwrap(),u32::try_from(u["vout"].as_u64().unwrap()).unwrap());
			add(canonical_asset(&core,p,"native_actors").expect("scanned native asset is actually unspent"));
		}
	}
	let training=evidence_rows(env,"training-wallet-assets.jsonl");
	for frame in training {
		descriptors.push(json!({"account":frame["account"],"data":frame["public_descriptors"],"source":"actual training wallet before unload"}));
		for a in frame["utxos"].as_array().unwrap() { if let Some(a)=canonical_asset(&core,a["outpoint"].as_str().unwrap().parse().unwrap(),"training_wallets") { add(a); } }
	}
	drop(add);
	let mut channels=Vec::new(); let (mut received_msat,mut paid_msat,mut router_msat)=(0u64,0u64,0u64);
	let mut quiet=true;
	if let Some(mut node)=env.ln_client.clone() {
		observed.insert("internal_ln".into());
		let funds=node.list_funds(cln_rpc::ListfundsRequest{spent:Some(false)}).await.unwrap().into_inner();
		let opening_funds=funds.clone();
		let peers=node.list_peer_channels(cln_rpc::ListpeerchannelsRequest{id:None,short_channel_id:None,channel_id:None}).await.unwrap().into_inner();
		assert!(funds.outputs.len()+funds.channels.len()<=2000 && peers.channels.len()<=2000,"whole node inventory row budget");
		for output in funds.outputs {
			let point=OutPoint::new(output.txid.to_lower_hex_string().parse::<Txid>().unwrap(),output.output);
			if let Some(a)=canonical_asset(&core,point,"internal_ln") { assets.entry(point.to_string()).or_insert(a); }
			else { panic!("node output lacks actual canonical Core evidence; never invent zero"); }
		}
		for c in peers.channels {
			quiet &= c.htlcs.is_empty() && c.inflight.is_empty();
			let short=c.short_channel_id.as_ref().expect("channel needs actual confirmed short ID").split('x').map(|s|s.parse::<usize>().unwrap()).collect::<Vec<_>>();
			assert_eq!(short.len(),3);
			let block=core.get_block(&core.get_block_hash(short[0] as u64).unwrap()).unwrap();
			let funding=OutPoint::new(block.txdata[short[1]].compute_txid(),u32::try_from(short[2]).unwrap());
			let matching=funds.channels.iter().find(|f|f.channel_id==c.channel_id.clone().unwrap()).expect("same actual local channel in both reads");
			let local=matching.our_amount_msat.as_ref().expect("owned local channel amount").msat;
			assert!(!assets.contains_key(&funding.to_string()),"channel funding cannot also be a physical accessible chain asset");
			channels.push(json!({"account":"internal_ln","channel_id":c.channel_id.unwrap().to_lower_hex_string(),"funding_outpoint":funding,
				"local_owned_msat":local,"to_us_msat":c.to_us_msat.map(|a|a.msat),"htlcs":c.htlcs.len(),"inflight":c.inflight.len(),"state":c.state}));
		}
		let mut hold=env.hold_client.clone().expect("actual hold owner required; Core listinvoices is not the hold source");
		let invoices=hold.list(hold::ListRequest{constraint:Some(hold::list_request::Constraint::Pagination(
			hold::list_request::Pagination{index_start:0,limit:2001}))}).await.unwrap().into_inner();
		assert!(invoices.invoices.len()<=2000,"complete hold receipt budget");
		for i in invoices.invoices {
			if i.state==hold::InvoiceState::Accepted as i32 {quiet=false;}
			if i.state==hold::InvoiceState::Paid as i32 {
				let amount=i.htlcs.iter().filter(|h|h.state==hold::InvoiceState::Paid as i32).map(|h|h.msat).sum::<u64>();
				assert!(amount>0 && i.settled_at.is_some(),"conclusive collected hold needs actual paid HTLC amounts");
				let hash=i.payment_hash.to_lower_hex_string();
				let external=evidence_rows(env,"external-receipts.jsonl");
				let intended=external.iter().filter(|r|r["payment_hash"]==hash).collect::<Vec<_>>();
				assert_eq!(intended.len(),1,"collected receive needs exact outside-payer boundary provenance; same-node preimage/grant is not an external receipt");
				assert_eq!(intended[0]["amount_msat"],amount);
				received_msat=received_msat.checked_add(amount).unwrap();
				env.out.line("node-receipts.jsonl",&json!({"label":label,"payment_hash":i.payment_hash.to_lower_hex_string(),"status":"collected","amount_msat":amount,"paid_at":i.settled_at,"producer":"actual owning hold node Paid invoice + Paid HTLCs"}));
			}
		}
		let pays=node.list_pays(cln_rpc::ListpaysRequest{limit:Some(2001),..Default::default()}).await.unwrap().into_inner();
		assert!(pays.pays.len()<=2000,"complete outgoing result budget");
		let mut hashes=BTreeSet::new();
		for p in pays.pays { assert!(hashes.insert(p.payment_hash.clone()),"aggregate payment hash appears twice"); if p.status==0 { quiet=false; }
			if p.status==1 {
				let parts=node.list_send_pays(cln_rpc::ListsendpaysRequest{payment_hash:Some(p.payment_hash.clone()),limit:Some(2001),..Default::default()}).await.unwrap().into_inner();
				assert!(!parts.payments.is_empty() && parts.payments.len()<=2000 && parts.payments.iter().all(|p|p.status==1),"failed aggregate requires complete terminal failed parts; unresolved cost cannot become zero");
				env.out.line("node-failed-payments.jsonl",&json!({"label":label,"payment_hash":p.payment_hash.to_lower_hex_string(),"actual_aggregate_status":"failed","actual_part_count":parts.payments.len(),"all_parts_terminal_failed":true,"router_cost_inference_not_used":true}));
			}
			if p.status==2 {
				let boundary:Value=serde_json::from_slice(&std::fs::read(env.out.dir.join("ln-boundary.json")).unwrap()).unwrap();
				assert_eq!(p.destination.as_ref().unwrap().to_lower_hex_string(),boundary["excluded_payer_payee_id"].as_str().unwrap(),"actual paid outgoing boundary must be known; same-node circulation is internal");
				let amount=p.amount_msat.expect("actual complete outgoing principal").msat; let sent=p.amount_sent_msat.expect("actual aggregate sent amount").msat;
				paid_msat=paid_msat.checked_add(amount).unwrap();router_msat=router_msat.checked_add(sent.checked_sub(amount).unwrap()).unwrap();
				env.out.line("node-payments.jsonl",&json!({"label":label,"payment_hash":p.payment_hash.to_lower_hex_string(),"number_of_parts":p.number_of_parts,"producer":"actual aggregate ListPays once per payment hash","amount_msat":amount,"sent_msat":sent,"status":"paid"}));
			}
		}
		let closing_funds=node.list_funds(cln_rpc::ListfundsRequest{spent:Some(false)}).await.unwrap().into_inner();
		assert_eq!(opening_funds,closing_funds,"owning node funds moved during non-atomic read: refuse this edge");
	}
	assert!(Instant::now()<deadline,"complete treasury observation exceeded predeclared600s budget");
	let end=core.get_best_block_hash().unwrap();
	let complete=start==end && quiet && assets.values().all(|a|a["actual_confirmed"]==true);
	let chain_total=assets.values().map(|a|a["value_sat"].as_u64().unwrap()).sum::<u64>();
	let local_total=channels.iter().map(|c|c["local_owned_msat"].as_u64().unwrap()).sum::<u64>();
	let total_msat=chain_total.checked_mul(1000).and_then(|n|n.checked_add(local_total)).unwrap();
	assert!(serde_json::to_vec(&assets).unwrap().len()+serde_json::to_vec(&channels).unwrap().len()<=128*1024*1024,"whole frame byte budget");
	let frame=json!({"label":label,"utc":utc(),"complete":complete,"quiescent":quiet,"atomic":false,"chain_start_hash":start,"chain_end_hash":end,
		"height":height,"physical_source_points":physical_points,"observed_accounts":observed,"chain_assets":assets.values().collect::<Vec<_>>(),"channels":channels,"public_descriptors":descriptors,
		"external_received_msat":received_msat,"external_paid_msat":paid_msat,"router_msat":router_msat,"asset_total_msat":total_msat,"ark_liabilities_as_assets":false,
		"accounting_sources":accounting,"actual_service_fee_schedule":fees,"customer_protocol_service_fees_sat":0,"global_epoch_fee_budget_proof":Value::Null,"urgent_signed_reserve_proof":Value::Null});
	env.out.line("whole-treasury-edges.jsonl",&frame);
	assert!(complete,"whole physical/LN edge is moving or incomplete; keep failure, restore prerequisites without weakening deadline");
	TreasuryEdge{balance_msat:total_msat,frame,height,received_msat,paid_msat,router_msat}
}

fn conserve_treasury(env: &Env, before: &TreasuryEdge, after: &TreasuryEdge, cycle: usize) -> Value {
	let core=env.core(); let mut miner_sat=0u64; let mut txids=Vec::new();
	let mut ancestry=before.frame["chain_assets"].as_array().unwrap().iter().map(|a|a["outpoint"].as_str().unwrap().to_owned()).collect::<BTreeSet<_>>();
	let mut roles=BTreeMap::<String,BTreeSet<String>>::new();
	for edge in [&before.frame,&after.frame] {for r in edge["accounting_sources"]["carrier_roles"].as_array().unwrap() {roles.entry(r["txid"].as_str().unwrap().into()).or_default().insert(r["kind"].as_str().unwrap().into());}}
	let mut intents=BTreeMap::<String,String>::new();let mut planned_signed=BTreeMap::<String,u64>::new();
	for r in evidence_rows(env,"chain-fee-intents.jsonl") {
		let id=r["txid"].as_str().unwrap().to_owned();let kind=r["category"].as_str().unwrap().to_owned();
		if let Some(fee)=r["planned_signed_fee_sat"].as_u64() {if let Some(old)=planned_signed.insert(id.clone(),fee){assert_eq!(old,fee);}}
		if let Some(old)=intents.insert(id,kind.clone()){assert_eq!(old,kind);}
	}
	let mut by_role=BTreeMap::<String,u64>::new();let mut unknown=Vec::new();let mut recipient_fees=0u64;

	for height in before.height+1..=after.height {
		let block=core.get_block(&core.get_block_hash(height).unwrap()).unwrap();
		for tx in block.txdata { if tx.is_coinbase() {
			let excluded="mzU8XRVhUdXtdxmSA3Vw8XeU2FDV4iBDRW".parse::<Address<bitcoin::address::NetworkUnchecked>>().unwrap().assume_checked().script_pubkey();
			assert!(tx.output.iter().filter(|o|o.value>Amount::ZERO).all(|o|o.script_pubkey==excluded),"post-opening reward/returned fees must go only to explicitly excluded miner");
			env.out.line("excluded-mining.jsonl",&json!({"height":height,"txid":tx.compute_txid(),"reward_including_fees_sat":tx.output.iter().map(|o|o.value.to_sat()).sum::<u64>(),"miner_address":"mzU8XRVhUdXtdxmSA3Vw8XeU2FDV4iBDRW","inside_treasury":false}));
			continue; }
			let inputs=tx.input.iter().map(|i|core.get_raw_transaction(&i.previous_output.txid,None).unwrap().output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
			let outputs=tx.output.iter().map(|o|o.value.to_sat()).sum::<u64>();
			let fee=inputs.checked_sub(outputs).unwrap(); miner_sat=miner_sat.checked_add(fee).unwrap();
			let id=tx.compute_txid().to_string();
			if let Some(planned)=planned_signed.get(&id){assert_eq!(*planned,fee,"original signed recovery fee equals actual Core prevout expense");}
			for input in &tx.input {assert!(ancestry.remove(&input.previous_output.to_string()),"unaccounted outside input/injection after opening:{}",input.previous_output);}
			for (n,_) in tx.output.iter().enumerate() {assert!(ancestry.insert(OutPoint::new(tx.compute_txid(),u32::try_from(n).unwrap()).to_string()));}
			let native=roles.get(&id);
			let role=if native.is_some_and(|k|k.contains("sweep")){"operator_sweep".to_owned()}
			else if native.is_some_and(|k|k.contains("expiry-payout")){"operator_payout".to_owned()}
			else if native.is_some_and(|k|k.contains("round")){"operator_round".to_owned()}
			else if native.is_some_and(|k|k.contains("vtxopool")){"operator_pool".to_owned()}
			else if let Some(role)=intents.get(&id){role.clone()}
			else if tx.input.iter().any(|i|{
				let p=core.get_raw_transaction(&i.previous_output.txid,None).unwrap();
				p.output[i.previous_output.vout as usize].script_pubkey.to_hex_string()=="51024e73"&&roles.contains_key(&i.previous_output.txid.to_string())
			}){"operator_bump".to_owned()}
			else if native.is_some_and(|k|k.contains("internal")){"operator_internal".to_owned()}
			else {unknown.push(id.clone());"unclassified".to_owned()};
			let subtotal=by_role.entry(role.clone()).or_default();*subtotal=subtotal.checked_add(fee).unwrap();
			if role=="operator_payout" {
				let charged=after.frame["accounting_sources"]["recipient_fee_by_carrier"][&id].as_u64().expect("actual payout charge source");
				assert!(charged<=fee,"recipient charges cannot exceed actual complete payout mining fee");recipient_fees=recipient_fees.checked_add(charged).unwrap();
			}

			let row=json!({"cycle":cycle,"txid":tx.compute_txid(),"height":height,"fee_sat":fee,
				"inputs":tx.input.iter().map(|i|i.previous_output.to_string()).collect::<Vec<_>>(),
				"outputs":tx.output.iter().enumerate().map(|(n,o)|json!({"n":n,"sat":o.value.to_sat(),"script":o.script_pubkey.to_hex_string()})).collect::<Vec<_>>(),
				"boundary_membership_audit_required":true,"fee_category":role,"native_kinds":native});
			env.out.line("whole-treasury-transactions.jsonl",&row);txids.push(tx.compute_txid());
		}
	}
	assert_eq!(by_role.values().sum::<u64>(),miner_sat,"every actual confirmed miner fee accounted exactly once");
	let received=after.received_msat.checked_sub(before.received_msat).unwrap();
	let paid=after.paid_msat.checked_sub(before.paid_msat).unwrap();let router=after.router_msat.checked_sub(before.router_msat).unwrap();
	let opening=before.balance_msat;let closing=after.balance_msat;
	assert_eq!(opening.checked_add(received).unwrap(),closing.checked_add(paid).and_then(|n|n.checked_add(miner_sat*1000)).and_then(|n|n.checked_add(router)).unwrap(),"whole treasury cycle conservation to msat; never reuse rounds-only sweep equation");
	let row=json!({"cycle":cycle,"opening_msat":opening,"external_receipts_msat":received,"declared_injections_msat":0,"closing_msat":closing,
		"external_payments_msat":paid,"miner_costs_msat":miner_sat*1000,"router_costs_msat":router,"difference_msat":0,"txids":txids,
		"fee_by_role_sat":by_role,"unclassified_fee_txids":unknown,"fee_classification_complete":unknown.is_empty(),
		"recipient_fees_internal_sat":recipient_fees,"recipient_fees_are_not_added_to_whole_assets":true,
		"from_label":before.frame["label"],"to_label":after.frame["label"],
		"zero_chain_injections_proved_by_opening_ancestry":true,
		"independently_audited":false,"boundary_membership_complete":false,"non_atomic_frames":true});
	env.out.line("capital-cycles.jsonl",&row);let _=&before.frame;row
}


fn finalize_treasury_accounting(env:&Env,opening:&TreasuryEdge,closing:&TreasuryEdge)->Value {
	// Use disjoint, actually observed spans. Never add a pre-recovery projection
	// to a post-recovery cut or count a carrier once per recipient.
	let spans=evidence_rows(env,"capital-cycles.jsonl");
	assert!(!spans.is_empty());
	let mut unique=BTreeSet::new();let mut cursor=opening.balance_msat;
	let mut label=opening.frame["label"].as_str().unwrap().to_owned();
	let mut miner=0u64;let mut recipient=0u64;let mut roles=BTreeMap::<String,u64>::new();
	for (index,span) in spans.iter().enumerate() {
		assert_eq!(span["cycle"].as_u64().unwrap(),index as u64,"ordered disjoint accounting spans");
		assert_eq!(span["opening_msat"].as_u64().unwrap(),cursor);
		assert_eq!(span["from_label"].as_str().unwrap(),label);
		assert_eq!(span["zero_chain_injections_proved_by_opening_ancestry"],true);
		assert_eq!(span["difference_msat"],0);
		for id in span["txids"].as_array().unwrap() {
			assert!(unique.insert(id.as_str().unwrap().to_owned()),"confirmed carrier counted twice");
		}
		miner=miner.checked_add(span["miner_costs_msat"].as_u64().unwrap()).unwrap();
		recipient=recipient.checked_add(span["recipient_fees_internal_sat"].as_u64().unwrap()).unwrap();
		for (role,fee) in span["fee_by_role_sat"].as_object().unwrap() {
			let n=roles.entry(role.clone()).or_default();*n=n.checked_add(fee.as_u64().unwrap()).unwrap();
		}
		cursor=span["closing_msat"].as_u64().unwrap();label=span["to_label"].as_str().unwrap().to_owned();
	}
	assert_eq!(cursor,closing.balance_msat);assert_eq!(label,closing.frame["label"].as_str().unwrap());
	let transactions=evidence_rows(env,"whole-treasury-transactions.jsonl");
	let mut raw_ids=BTreeSet::new();let mut raw_fees=0u64;
	for tx in &transactions {
		assert!(raw_ids.insert(tx["txid"].as_str().unwrap().to_owned()),"actual raw carrier repeated");
		raw_fees=raw_fees.checked_add(tx["fee_sat"].as_u64().unwrap()).unwrap();
	}
	assert_eq!(raw_ids,unique);assert_eq!(raw_fees.checked_mul(1000).unwrap(),miner);
	assert_eq!(roles.values().try_fold(0u64,|n,f|n.checked_add(*f)).unwrap(),raw_fees);
	let receipts=closing.received_msat.checked_sub(opening.received_msat).unwrap();
	let payments=closing.paid_msat.checked_sub(opening.paid_msat).unwrap();
	let routing=closing.router_msat.checked_sub(opening.router_msat).unwrap();
	assert_eq!(opening.balance_msat.checked_add(receipts).unwrap(),closing.balance_msat.checked_add(payments)
		.and_then(|n|n.checked_add(miner)).and_then(|n|n.checked_add(routing)).unwrap(),"actual opening through final confirmed seed recovery");
	let payout_miner=roles.get("operator_payout").copied().unwrap_or(0);
	let net_payout=payout_miner.checked_sub(recipient).expect("actual gross payout miner expense minus actual recipient contribution");
	let report=json!({"result":"UNQUALIFIED","opening_actual_msat":opening.balance_msat,"closing_after_confirmed_seed_recovery_msat":closing.balance_msat,
		"actual_external_ln_receipts_msat":receipts,"actual_external_ln_payments_msat":payments,"actual_router_cost_msat":routing,
		"post_opening_chain_injections_sat":0,"chain_injection_proof":"all actual confirmed inputs descend from the observed opening physical outpoints",
		"internal_bank_allocations_are_not_injections":true,"actual_gross_miner_cost_sat":raw_fees,"actual_fee_by_role_sat":roles,
		"recipient_fee_income_internal_sat":recipient,"payout_gross_miner_expense_sat":payout_miner,"payout_net_operator_expense_sat":net_payout,
		"recipient_charge_counted_as_boundary_income":false,"confirmed_carriers":unique.len(),"disjoint_spans":spans.len(),"difference_msat":0,
		"fee_classification_complete":!roles.contains_key("unclassified"),
		"unclassified_fee_txids":transactions.iter().filter(|t|t["fee_category"]=="unclassified").map(|t|t["txid"].clone()).collect::<Vec<_>>(),
		"closing_liability_source":closing.frame["accounting_sources"],"global_economic_group_attribution_complete":false,
		"global_epoch_fee_budget_proof":Value::Null,"urgent_signed_reserve_proof":Value::Null,
		"open_gates":["independent exhaustive boundary membership and economic-group attribution","global epoch fee budget enforcement","urgent signed reserve enforcement"],
		"final_cut":"after every required mnemonic-only recovery spend confirms; captaind and watchmand stopped"});
	env.out.json("whole-boundary-final-accounting.json",&report);report
}

async fn provision_fee_reserve(ctx: &TestContext, env: &Env, srv: &Captaind) {
	let total=env_u64("STAGE4_FEE_RESERVE_SAT",2_000_000);
	let fragments=env_u64("STAGE4_FEE_RESERVE_FRAGMENTS",1);
	if total==0 {
		assert_eq!(fragments,0);
		let mut rpc=server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap())).await.unwrap();
		let inventory=rpc.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman.expect("actual existing fee wallet");assert!(inventory.complete);
		let eligible=inventory.utxos.iter().filter(|u|u.confirmed_fee_eligible).map(|u|u.value_sat).sum::<u64>();
		assert_eq!(eligible,0,"insufficient fixture requires actual zero eligible reserve, never an assumed zero");
		env.out.json("fee-reserve-ready.json",&json!({"eligible_sat":eligible,"declared_sat":0,"fragments":0,"producer":"actual owning wallet complete inventory","qualified":false}));return;
	}
	assert!(fragments>0 && fragments<=128 && total>=fragments*1000,"declared fee reserve shape");
	let address=tokio::time::timeout(Duration::from_secs(120),srv.watchmand().wait_wallet_address()).await.expect("existing watchman receive script must be ready");
	let mut funded=0u64;
	for index in 0..fragments {
		let amount=total/fragments+u64::from(index<total%fragments);
		let txid=env.core().send_to_address(&address,sat(amount),None,None,None,None,None,None).unwrap();
		ctx.await_transaction(txid).await;ctx.generate_blocks(1).await;funded+=amount;
		env.out.line("fee-reserve-funding.jsonl",&json!({"utc":utc(),"txid":txid,"amount_sat":amount,"fragment":index,
			"source":"included fixture bank","destination":"existing included watchman fee wallet","external_injection":false}));
	}
	assert_eq!(funded,total);ctx.generate_blocks(6).await;
	let deadline=Instant::now()+Duration::from_secs(120);
	loop {
		let mut rpc=server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap())).await.unwrap();
		let inventory=rpc.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman.expect("actual owning wallet inventory");
		assert!(inventory.complete);
		let eligible=inventory.utxos.iter().filter(|u|u.confirmed_fee_eligible).map(|u|u.value_sat).sum::<u64>();
		if eligible>=total { env.out.json("fee-reserve-ready.json",&json!({"eligible_sat":eligible,"declared_sat":total,"fragments":fragments,"utc":utc(),"qualified":false}));break; }
		assert!(Instant::now()<deadline,"declared actual confirmed fee reserve must become eligible");tokio::time::sleep(Duration::from_millis(200)).await;
	}
}
