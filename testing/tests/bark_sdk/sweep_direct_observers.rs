use bitcoin::hex::DisplayHex;
// Shared test-only direct journal observer. Never authorizes or writes an allocation.
const DIRECT_TERMINAL_COUNT: &str = "SELECT count(DISTINCT a.id),count(DISTINCT c.txid)
	FROM expiry_allocation a JOIN expiry_carrier_allocation m ON m.allocation_id=a.id
	JOIN expiry_carrier c ON c.txid=m.txid WHERE a.id=ANY($1)
	AND a.disposition IN ('pay','forfeit') AND c.confirmed_height IS NOT NULL";

fn require_operating_build() {
	let path = std::env::var("SCALE_OPERATING_BUILD_RECEIPT").expect("root must first qualify and release the operating build; no scale dispatch during preparation");
	let receipt: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
	assert_eq!(receipt["architecture_profile"], "direct_per_key");
	assert_eq!(receipt["operating_build_qualified"], true, "operating build is not qualified");
	assert_eq!(receipt["candidate_commit"].as_str(), Some(std::env::var("SCALE_CANDIDATE_COMMIT").unwrap().as_str()), "release receipt differs from frozen candidate");
	assert_eq!(receipt["automatic_direct_route_qualified"], true, "a builder/freeze-only proof is not automatic-route qualification");
	let sources=receipt["qualified_sources"].as_array().expect("exact source qualifications required");
	for source in ["registered","pending_board","unregistered","failed_send","collected_receive","hark_unclaimed"] {
		assert!(sources.iter().any(|s|s.as_str()==Some(source)),"operating build does not qualify every preserved source");
	}
	assert_eq!(receipt["fee_limits_qualified"],true,"actual fee ceilings/subsidy limits are required before campaigns");
}

// C has no attribution account/xpub or per-output origin field. Before the
// genuine protocol request, independently verify every output's retained V1
// authorization against its immutable V4 record and the durable server rows.
async fn per_key_arkoor_request(
	env:&Env, wallet: &bark::Wallet, recipient: &ark::Address, request: protos::ArkoorPackageCosignRequest,
) -> protos::ArkoorPackageCosignRequest {
	let record = wallet.fallback_destination().await.unwrap();
	record.verify(Network::Regtest, wallet.mailbox_keypair().public_key()).unwrap();
	for part in &request.parts {
		for output in part.outputs.iter().chain(part.isolated_outputs.iter()) {
			let key = ark::VtxoPolicy::deserialize(&output.policy).unwrap().user_pubkey();
			let row=env.db.read(async |t|Ok(t.query_one("SELECT l.mailbox_pk,l.sig,l.recovery_index,r.spk,r.mailbox_sig FROM key_link l JOIN fallback_record r ON r.mailbox_pk=l.mailbox_pk WHERE l.user_pubkey=$1",&[&key.serialize().as_slice()]).await?)).await.unwrap();
			let mailbox=PublicKey::from_slice(&row.get::<_,Vec<u8>>(0)).unwrap();
			let durable_record=ark::fallback::FallbackRecord{spk:ScriptBuf::from_bytes(row.get(3)),mailbox_signature:bitcoin::secp256k1::schnorr::Signature::from_slice(&row.get::<_,Vec<u8>>(4)).unwrap()};
			let durable_link=ark::fallback::KeyLink{user_pubkey:key,recovery_index:row.get::<_,Option<i32>>(2).map(|i|u32::try_from(i).unwrap()),signature:bitcoin::secp256k1::schnorr::Signature::from_slice(&row.get::<_,Vec<u8>>(1)).unwrap()};
			durable_record.verify(Network::Regtest,mailbox).unwrap();durable_link.verify(Network::Regtest,mailbox,&durable_record).unwrap();
			if key != recipient.policy().user_pubkey() {
				let link = wallet.fallback_key_link(key).await.unwrap();
				assert_eq!(link.user_pubkey,key); assert_eq!(durable_link,link); assert_eq!(durable_record,record);
				link.verify(Network::Regtest,wallet.mailbox_keypair().public_key(),&record).unwrap();
			}
		}
	}
	// The receiving key's pre-use canonical link is exported by create_owner;
	// the native server authenticates its actual durable row. No empty query RPC.
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

// An absent hArk owner never supplied the final leaf signature/forfeit, so
// the retained terminal must remain locked. Authenticate its complete native
// body against the independently retained signed tree, and FULLY validate its
// signed immediate ancestor; never insert a preimage or invent a leaf witness.
fn verify_unclaimed_hark_path(
    full: &Vtxo, tree: &ark::tree::signed::CachedSignedVtxoTree,
    parent: &ark::ServerVtxo<ark::vtxo::Full>, funding: &bitcoin::Transaction,
) -> anyhow::Result<()> {
    anyhow::ensure!(tree.spec.spec.vtxos.len()==2,"fresh hArk tree must contain replacement and padding only");
    anyhow::ensure!(full.chain_anchor()==tree.spec.utxo,"hArk tree funding differs");
    anyhow::ensure!(full.unlock_hash().is_some(),"hArk terminal must remain hash locked");
    let matching=tree.output_vtxos().filter(|v|v.id()==full.id()).collect::<Vec<_>>();
    anyhow::ensure!(matching.len()==1 && matching[0].serialize()==full.serialize(),"hArk terminal body differs from native signed tree");
    full.validate_unsigned(funding)?;
    let steps=full.transactions().collect::<Vec<_>>();
    anyhow::ensure!(!steps.is_empty(),"hArk path has no final transition");
    match full.validate(funding) {
        Err(ark::vtxo::VtxoValidationError::GenesisTransition{error:"missing preimage",genesis_idx,genesis_len,transition_kind})
            if genesis_len==steps.len() && genesis_idx+1==genesis_len &&
                transition_kind=="hash-locked-cosigned-v1"=>{},
        other=>anyhow::bail!("expected only the unissued final hArk witness, got {other:?}"),
    }
    let leaf=&steps.last().unwrap().tx;
    anyhow::ensure!(leaf.input.len()==1,"hArk leaf must have exactly one signed parent");
    let parent_id=ark::VtxoId::from(leaf.input[0].previous_output);
    anyhow::ensure!(parent.id()==parent_id && parent.chain_anchor()==full.chain_anchor() && parent.server_pubkey()==full.server_pubkey(),"hArk parent identity differs");
    let matching_parent=tree.internal_vtxos().filter(|(v,_)|v.id()==parent_id).collect::<Vec<_>>();
    anyhow::ensure!(matching_parent.len()==1 && matching_parent[0].0.serialize()==parent.serialize(),"hArk parent body differs from native signed tree");
    parent.validate(funding)?;
    Ok(())
}

async fn reconcile(env: &Env, owners: &[Owner], enabled_utc: f64) -> Value {
	let core = env.core();
	let mut failures = Vec::<String>::new();
	let rows = env.db.read(async |t| {
		let size=t.query_one("SELECT count(*),coalesce(sum(octet_length(row_to_json(q)::text)),0)::BIGINT FROM (SELECT a.*,m.txid,m.vout,c.backing_ids FROM expiry_allocation a LEFT JOIN expiry_carrier_allocation m ON m.allocation_id=a.id LEFT JOIN expiry_carrier c ON c.txid=m.txid) q",&[]).await?;
		assert!(size.get::<_,i64>(0)<=100_000 && size.get::<_,i64>(1)<=128*1024*1024,"complete allocation snapshot exceeds declared rows/bytes");
		Ok(t.query("SELECT a.id,a.spk,a.disposition,a.gross_sat,a.fee_sat,a.payout_sat,a.forfeited_sat,
	a.retirement_ids,a.source,a.source_evidence::TEXT,extract(epoch FROM a.frozen_at)::FLOAT8,m.txid,m.vout,
	c.backing_ids,c.miner_fee_sat,c.funding_subsidy_sat,c.confirmed_height
	FROM expiry_allocation a LEFT JOIN expiry_carrier_allocation m ON m.allocation_id=a.id
	LEFT JOIN expiry_carrier c ON c.txid=m.txid ORDER BY a.id,m.txid", &[]).await?) }).await.unwrap();
	let expected = owners.iter().flat_map(|o| o.coins.iter().map(move |c| (c.id.clone(), (o,c))))
		.collect::<HashMap<_,_>>();
	assert_eq!(expected.len(),owners.iter().map(|o|o.coins.len()).sum::<usize>(),"duplicate intended economic entitlement");
	let mut terminal = BTreeSet::new();
	let mut winners = HashMap::<String,String>::new();
	let mut by_tx = BTreeMap::<String,Vec<Value>>::new();
	for r in rows {
		let id: String = r.get(0);
		let disposition: String = r.get(2);
		if disposition == "operator" { continue; }
		let Some((owner, coin)) = expected.get(&id) else { failures.push(format!("unexpected customer allocation {id}")); continue; };
		let spk: Vec<u8> = r.get::<_,Option<Vec<u8>>>(1).unwrap_or_default();
		let gross = u64::try_from(r.get::<_,i64>(3)).unwrap();
		let fee = u64::try_from(r.get::<_,i64>(4)).unwrap();
		let net = u64::try_from(r.get::<_,i64>(5)).unwrap();
		let forfeited = u64::try_from(r.get::<_,i64>(6)).unwrap();
		if spk != owner.spk.as_bytes() || gross != coin.amount || gross != fee.checked_add(net).and_then(|n|n.checked_add(forfeited)).unwrap() {
			failures.push(format!("allocation {id} differs from independent owner/principal"));
		}
		let Some(txid) = r.get::<_,Option<String>>(11) else { continue; };
		let info = core.get_raw_transaction_info(&txid.parse().unwrap(),None);
		let confirmed = info.as_ref().ok().and_then(|i|i.confirmations).unwrap_or(0)>0;
		if !confirmed { continue; }
		if r.get::<_,Option<i32>>(16).is_none() { failures.push(format!("confirmed carrier {txid} lacks canonical journal observation")); }
		if winners.insert(id.clone(),txid.clone()).is_some() { failures.push(format!("entitlement {id} has multiple confirmed carriers")); }
		let retirement: Vec<String> = r.get(7); let backing: Vec<String> = r.get(13);
		let tx = core.get_raw_transaction(&txid.parse().unwrap(),None).unwrap();
		let inputs = tx.input.iter().map(|i|i.previous_output.to_string()).collect::<BTreeSet<_>>();
		if retirement.is_empty() || retirement.iter().any(|i|!backing.contains(i)) || backing.iter().any(|i|!inputs.contains(i)) {
			failures.push(format!("{id}: incomplete actual physical retirement"));
		}
		// Actual ancestor funding identity must be on this entitled full path.
		let full = owner.anchors.iter().find(|v|v.id().to_string()==id).expect("intended full source proof");
		let key=full.user_pubkey();let mailbox=owner.mailbox_pk.parse::<PublicKey>().unwrap();
		let proof=env.db.read(async |t|Ok(t.query_one("SELECT l.mailbox_pk,l.sig,l.recovery_index,r.spk,r.mailbox_sig FROM key_link l JOIN fallback_record r ON r.mailbox_pk=l.mailbox_pk WHERE l.user_pubkey=$1",&[&key.serialize().as_slice()]).await?)).await.unwrap();
		assert_eq!(proof.get::<_,Vec<u8>>(0),mailbox.serialize());
		let record=ark::fallback::FallbackRecord{spk:ScriptBuf::from_bytes(proof.get(3)),mailbox_signature:bitcoin::secp256k1::schnorr::Signature::from_slice(&proof.get::<_,Vec<u8>>(4)).unwrap()};
		let link=ark::fallback::KeyLink{user_pubkey:key,recovery_index:proof.get::<_,Option<i32>>(2).map(|i|u32::try_from(i).unwrap()),signature:bitcoin::secp256k1::schnorr::Signature::from_slice(&proof.get::<_,Vec<u8>>(1)).unwrap()};
		assert_eq!(record.spk,owner.spk);record.verify(Network::Regtest,mailbox).unwrap();link.verify(Network::Regtest,mailbox,&record).unwrap();
		let funding = core.get_raw_transaction(&full.chain_anchor().txid,None).unwrap();
        if coin.source=="hark_unclaimed" {
            let round=env.db.read(async |t|t.get_round(full.chain_anchor().txid.into()).await).await.unwrap().expect("retained native hArk round");
            assert_eq!(round.funding_tx.compute_txid(),funding.compute_txid());
            let tree=round.into_cached_tree().unwrap();
            let parent_id=ark::VtxoId::from(full.transactions().last().unwrap().tx.input[0].previous_output);
            let parent=tree.internal_vtxos().find(|(v,_)|v.id()==parent_id).expect("retained signed immediate parent").0;
            verify_unclaimed_hark_path(full,&tree,&parent,&funding).unwrap();
            // Exercise actual-path refusal controls, not a generic malformed RPC.
            let mut wrong_tree=tree.spec.clone();wrong_tree.utxo.vout^=1;
            assert!(verify_unclaimed_hark_path(full,&wrong_tree.into_cached_tree(),&parent,&funding).is_err());
            let mut wrong_body=tree.spec.clone();wrong_body.spec.vtxos[0].vtxo.amount+=sat(1);
            assert!(verify_unclaimed_hark_path(full,&wrong_body.into_cached_tree(),&parent,&funding).is_err());
            assert!(verify_unclaimed_hark_path(full,&tree,&ark::ServerVtxo::from(full.clone()),&funding).is_err());
            let mut wrong_funding=funding.clone();wrong_funding.output[0].value+=sat(1);
            assert!(verify_unclaimed_hark_path(full,&tree,&parent,&wrong_funding).is_err());
            env.out.event("hark-retained-signed-path-authenticated",json!({"id":full.id(),"signed_parent":parent.id(),"funding_txid":funding.compute_txid(),"unclaimed_final_witness_retained":true,"wrong_tree_body_parent_funding_refused":true,"preimage_inserted":false}));
        } else { full.validate(&funding).unwrap(); }
		let path = std::iter::once(full.chain_anchor().to_string()).chain(full.transactions().map(|v|
			OutPoint::new(v.tx.compute_txid(),u32::try_from(v.output_idx).unwrap()).to_string())).collect::<BTreeSet<_>>();
		if !retirement.iter().any(|i|path.contains(i)) { failures.push(format!("{id}: carrier never consumes an actual entitled funding path")); }
		let vout = r.get::<_,Option<i32>>(12);
		if disposition == "pay" {
			let output = vout.and_then(|i|tx.output.get(usize::try_from(i).unwrap()));
			if output.is_none_or(|o|o.script_pubkey.as_bytes()!=spk ) { failures.push(format!("{id}: exact recipient role/amount mismatch")); }
		} else if disposition == "forfeit" {
			if vout.is_some() || net!=0 || fee!=0 || forfeited!=gross || gross.checked_sub(1000).is_some_and(|n|n>=owner.spk.minimal_non_dust().to_sat()) {
				failures.push(format!("{id}: unauthorized terminal forfeiture"));
			}
		} else { failures.push(format!("{id}: unknown customer disposition")); }
		terminal.insert(id.clone());
		let row = json!({"id":id,"owner":owner.index,"spk":owner.spk.to_hex_string(),"disposition":disposition,
			"gross_sat":gross,"fee_sat":fee,"net_sat":net,"forfeited_sat":forfeited,"txid":txid,"vout":vout,
			"retirement_ids":retirement,"backing_ids":backing,"source":r.get::<_,String>(8),"source_evidence":serde_json::from_str::<Value>(&r.get::<_,String>(9)).unwrap(),
			"frozen_utc":r.get::<_,f64>(10),"eligible_utc":Value::Null,"miner_fee_sat":r.get::<_,i64>(14),"funding_subsidy_sat":r.get::<_,i64>(15)});
		env.out.line("direct-allocations.jsonl",&row); by_tx.entry(txid).or_default().push(row);
	}
	for id in expected.keys().filter(|i|!terminal.contains(*i)) { failures.push(format!("unresolved entitlement {id}")); }
	let mut paid_out = HashMap::<usize,Vec<(OutPoint,u64)>>::new();
	let (mut sum_net,mut sum_flat,mut sum_forfeit,mut sum_miner,mut sum_subsidy)=(0u64,0u64,0u64,0u64,0u64);
	let mut txs = Vec::new();
	for (txid, rows) in by_tx {
		let tx = core.get_raw_transaction(&txid.parse().unwrap(),None).unwrap();
		let total_in=tx.input.iter().map(|i|core.get_raw_transaction(&i.previous_output.txid,None).unwrap().output[i.previous_output.vout as usize].value.to_sat()).sum::<u64>();
		let miner=total_in.checked_sub(tx.output.iter().map(|o|o.value.to_sat()).sum::<u64>()).unwrap();
		if rows.iter().any(|r|r["miner_fee_sat"].as_u64()!=Some(miner)) { failures.push(format!("{txid}: actual miner fee differs from journal")); }
		let mut roles=BTreeMap::<u32,Vec<&Value>>::new();
		for r in &rows { if let Some(vout)=r["vout"].as_u64() { roles.entry(u32::try_from(vout).unwrap()).or_default().push(r); } }
		let mut flat=0;
		for (vout, group) in &roles {
			let charge=group.iter().map(|r|r["fee_sat"].as_u64().unwrap()).sum::<u64>();
			let payout=group.iter().map(|r|r["net_sat"].as_u64().unwrap()).sum::<u64>();
			if charge!=1000 || payout!=tx.output[*vout as usize].value.to_sat() { failures.push(format!("{txid}:{vout}: grouped output/1000sat policy mismatch")); }
			let owner=usize::try_from(group[0]["owner"].as_u64().unwrap()).unwrap();
			if group.iter().any(|r|r["owner"].as_u64()!=Some(owner as u64)) { failures.push(format!("{txid}:{vout}: unrelated owners share a recipient role")); }
			paid_out.entry(owner).or_default().push((OutPoint::new(txid.parse().unwrap(),*vout),payout));
			sum_net+=payout; flat+=charge;
		}
		let forfeit=rows.iter().map(|r|r["forfeited_sat"].as_u64().unwrap()).sum::<u64>();
		let subsidy=rows[0]["funding_subsidy_sat"].as_u64().unwrap();
		sum_flat+=flat;sum_forfeit+=forfeit;sum_miner+=miner;sum_subsidy+=subsidy;
		let t=json!({"txid":txid,"tx_fee_sat":miner,"recipient_fee_sat":flat,"economic_miner_beyond_flat_sat":miner.saturating_sub(flat),
			"funding_subsidy_sat":subsidy,"recipient_vouts":roles.keys().collect::<Vec<_>>(),"inputs":tx.input.iter().map(|i|i.previous_output.to_string()).collect::<Vec<_>>()});
		env.out.line("direct-transactions.jsonl",&t); txs.push(t);
	}
	let owner_rows=owners.iter().map(|o| { let outputs=paid_out.get(&o.index).cloned().unwrap_or_default(); json!({"index":o.index,
		"funded_sat":o.coins.iter().map(|c|c.amount).sum::<u64>(),"net_sat":outputs.iter().map(|(_,n)|n).sum::<u64>(),"outputs":outputs.iter().map(|(p,n)|json!([p.to_string(),n])).collect::<Vec<_>>()}) }).collect::<Vec<_>>();
	let gross=owners.iter().flat_map(|o|o.coins.iter()).map(|c|c.amount).sum::<u64>();
	if gross!=sum_net+sum_flat+sum_forfeit { failures.push("whole principal disposition does not conserve".into()); }
	json!({"failures":failures,"expected_coins":expected.len(),"settled_coins":terminal.len(),"payout_txs":txs,"gross_sat":gross,
		"net_sat":sum_net,"recipient_fee_sat":sum_flat,"forfeited_sat":sum_forfeit,"tx_fee_sat":sum_miner,"funding_subsidy_sat":sum_subsidy,
		"owners":owner_rows,"enabled_utc":enabled_utc,"eligibility_to_confirmation_complete":false,"treasury_complete":false})
}

async fn record_ln_boundary(env:&Env,lightning:&Option<ark_testing::context::LightningPaymentSetup>) {
	if let Some(ln)=lightning { env.out.json("ln-boundary.json",&json!({"included_node_id":ln.internal.id().await.to_lower_hex_string(),"excluded_payer_payee_id":ln.external.id().await.to_lower_hex_string(),"receipts_require_paid_external_intent":true})); }
}

async fn treasury_edge(env:&Env,srv:&Captaind,label:&str)->TreasuryEdge {
	tokio::time::timeout(Duration::from_secs(600),treasury_edge_inner(env,srv,label)).await.expect("bounded complete treasury edge failed")
}

// Whole physical treasury evidence. These are assets, never Ark claim sums.
// Chain/LN frames remain separately timed; a moving edge is refused, not called atomic.
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

async fn treasury_edge_inner(env: &Env, srv: &Captaind, label: &str) -> TreasuryEdge {
	let deadline=Instant::now()+Duration::from_secs(600);
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
	for (account,response) in [
		("rounds",srv.get_wallet_rpc().await.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().rounds),
		("watchman",server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap()))
			.await.unwrap().wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman),
	] {
		let inventory=response.expect("actual owning daemon must provide complete inventory");
		assert!(inventory.complete,"owning wallet refused whole inventory"); observed.insert(account.into());
		let kind=if account=="rounds" {server::wallet::WalletKind::Rounds}else{server::wallet::WalletKind::Watchman};
		let cs=env.db.read(async |t| {
			let size=t.query_one("SELECT count(*),coalesce(sum(octet_length(content)),0)::BIGINT FROM wallet_changeset WHERE kind=$1::TEXT::wallet_kind",&[&kind.name()]).await?;
			assert!(size.get::<_,i64>(0)<=100_000 && size.get::<_,i64>(1)<=128*1024*1024,"complete wallet descriptor source budget");t.read_aggregate_changeset(kind).await }).await.unwrap().expect("actual persisted descriptors");
		let public_desc=persisted_public_descriptors(cs.descriptor.map(|d|d.to_string()),
			cs.change_descriptor.map(|d|d.to_string()),&inventory.public_descriptor);
		descriptors.push(json!({"account":account,"actual_public_descriptors":public_desc,"public_descriptor":inventory.public_descriptor,
			"tip":inventory.tip_hash,"reserved_outpoints":inventory.reserved_outpoints}));
		for u in inventory.utxos { assert!(Instant::now()<deadline,"bounded complete treasury observation");if let Some(a)=canonical_asset(&core,u.outpoint.parse().unwrap(),account) { assert_eq!(a["value_sat"],u.value_sat); add(a); } }
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
			let full:ark::ServerVtxo<ark::vtxo::Full>=ark::ServerVtxo::deserialize(&bytes).expect("actual full server source proof");physical_points.insert(full.chain_anchor());
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
		"external_received_msat":received_msat,"external_paid_msat":paid_msat,"router_msat":router_msat,"asset_total_msat":total_msat,"ark_liabilities_as_assets":false});
	env.out.line("whole-treasury-edges.jsonl",&frame);
	assert!(complete,"whole physical/LN edge is moving or incomplete; keep failure, restore prerequisites without weakening deadline");
	TreasuryEdge{balance_msat:total_msat,frame,height,received_msat,paid_msat,router_msat}
}

fn conserve_treasury(env: &Env, before: &TreasuryEdge, after: &TreasuryEdge, cycle: usize) -> Value {
	let core=env.core(); let mut miner_sat=0u64; let mut txids=Vec::new();
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
			let row=json!({"cycle":cycle,"txid":tx.compute_txid(),"height":height,"fee_sat":fee,
				"inputs":tx.input.iter().map(|i|i.previous_output.to_string()).collect::<Vec<_>>(),
				"outputs":tx.output.iter().enumerate().map(|(n,o)|json!({"n":n,"sat":o.value.to_sat(),"script":o.script_pubkey.to_hex_string()})).collect::<Vec<_>>(),
				"boundary_membership_audit_required":true});
			env.out.line("whole-treasury-transactions.jsonl",&row);txids.push(tx.compute_txid());
		}
	}
	let received=after.received_msat.checked_sub(before.received_msat).unwrap();
	let paid=after.paid_msat.checked_sub(before.paid_msat).unwrap();let router=after.router_msat.checked_sub(before.router_msat).unwrap();
	let opening=before.balance_msat;let closing=after.balance_msat;
	assert_eq!(opening.checked_add(received).unwrap(),closing.checked_add(paid).and_then(|n|n.checked_add(miner_sat*1000)).and_then(|n|n.checked_add(router)).unwrap(),"whole treasury cycle conservation to msat; never reuse rounds-only sweep equation");
	let row=json!({"cycle":cycle,"opening_msat":opening,"external_receipts_msat":received,"declared_injections_msat":0,"closing_msat":closing,
		"external_payments_msat":paid,"miner_costs_msat":miner_sat*1000,"router_costs_msat":router,"difference_msat":0,"txids":txids,
		"independently_audited":false,"boundary_membership_complete":false,"non_atomic_frames":true});
	env.out.line("capital-cycles.jsonl",&row);let _=&before.frame;row
}

async fn provision_fee_reserve(ctx: &TestContext, env: &Env, srv: &Captaind) {
	let total=env_u64("STAGE4_FEE_RESERVE_SAT",2_000_000);
	let fragments=std::env::var("STAGE4_FEE_RESERVE_FRAGMENTS").expect("COMMON must declare actual confirmed fee UTXO shape").parse::<u64>().unwrap();
	if total==0 {
		assert_eq!(fragments,0);
		let mut rpc=server_rpc::admin::WalletAdminServiceClient::connect(format!("http://{}",srv.watchmand().config().admin_address.unwrap())).await.unwrap();
		let inventory=rpc.wallet_inventory(protos::Empty{}).await.unwrap().into_inner().watchman.expect("actual existing fee wallet");assert!(inventory.complete);
		let eligible=inventory.utxos.iter().filter(|u|u.confirmed_fee_eligible).map(|u|u.value_sat).sum::<u64>();
		assert_eq!(eligible,0,"insufficient fixture requires actual zero eligible reserve, never an assumed zero");
		env.out.json("fee-reserve-ready.json",&json!({"eligible_sat":eligible,"declared_sat":0,"fragments":0,"producer":"actual owning wallet complete inventory","qualified":false}));return;
	}
	assert!(fragments>=2 && fragments<=128 && total>=fragments*1000,"ordinary funding needs separately confirmed urgent reserve; one coin is not isolation");
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
		if eligible>=total { assert!(inventory.utxos.iter().filter(|u|u.confirmed_fee_eligible).count()>=2,"whole-input urgent reserve needs distinct eligible outputs"); env.out.json("fee-reserve-ready.json",&json!({"eligible_sat":eligible,"declared_sat":total,"fragments":fragments,"utc":utc(),"qualified":false}));break; }
		assert!(Instant::now()<deadline,"declared actual confirmed fee reserve must become eligible");tokio::time::sleep(Duration::from_millis(200)).await;
	}
}

// The owning server creates a single-keychain BDK wallet. A missing internal
// descriptor is its actual layout, not missing treasury evidence. The external
// descriptor remains mandatory and independently matches the owning inventory.
fn persisted_public_descriptors(external:Option<String>, internal:Option<String>, owning:&str)->Vec<String> {
	let external=external.expect("actual persisted external descriptor required");
	assert_eq!(external,owning,"persisted source must match the independently owning daemon");
	let mut descriptors=vec![external];
	if let Some(internal)=internal { descriptors.push(internal); }
	assert!(descriptors.iter().all(|d|!d.contains("prv")),"no private descriptors in evidence");
	descriptors
}

#[test]
fn treasury_single_keychain_requires_actual_persisted_owning_descriptor() {
	let key=bitcoin::secp256k1::Keypair::from_secret_key(&bitcoin::secp256k1::Secp256k1::new(),
		&bitcoin::secp256k1::SecretKey::from_slice(&[1;32]).unwrap());
	let mut wallet=bdk_wallet::Wallet::create_single(format!("tr({})",key.x_only_public_key().0))
		.network(Network::Regtest).create_wallet_no_persist().unwrap();
	let actual=wallet.public_descriptor(bitcoin_ext::bdk::KEYCHAIN).to_string();
	let cs=wallet.take_staged().unwrap();
	assert!(cs.change_descriptor.is_none(),"genuine single-keychain creation has no fabricated internal descriptor");
	let external=cs.descriptor.unwrap().to_string();
	assert_eq!(persisted_public_descriptors(Some(external.clone()),None,&actual),vec![actual.clone()]);
	assert!(std::panic::catch_unwind(||persisted_public_descriptors(None,None,&actual)).is_err());
	assert!(std::panic::catch_unwind(||persisted_public_descriptors(Some(external),None,"wrong owning descriptor")).is_err());
}
