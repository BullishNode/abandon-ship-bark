#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn stage4_scale() {
	let p = Params::from_env();
	let out = Out::new();
	out.json("params-effective.json", &serde_json::to_value(&p).unwrap());
	let started = Instant::now();
	require_operating_build();
	let ctx = TestContext::new("bark_sdk/stage4_scale").await;
	let core = ctx.bitcoind().sync_client();

	// ── Core capital, mined before anything is funded ──
	let funders = p.owners().div_ceil(p.round_chunk.max(1));
	let need = p.server_funding_sat + if p.lightning() { 13 * 100_000_000 } else { 0 }
		+ (2 * p.owners() + 50) as u64 * 160_000 + p.active_users as u64 * 300_000
		+ funders as u64 * (p.round_chunk as u64 * p.arkoor_sat + 1_000_000)
		+ 5 * 100_000_000;
	loop {
		let balance: Value = core.call("getbalances", &[]).unwrap();
		let trusted = Amount::from_btc(balance["mine"]["trusted"].as_f64().unwrap()).unwrap().to_sat();
		out.event("core-capital", json!({ "trusted_sat": trusted, "need_sat": need,
			"tip": core.get_block_count().unwrap() }));
		if trusted >= need { break; }
		ctx.bitcoind().generate_to_wallet(50).await;
		ctx.bitcoind().generate(100).await;
	}
	let lightning = if p.lightning() { Some(ctx.new_lightning_setup("lightningd").await) } else { None };

	let mut builder = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.funded(sat(p.server_funding_sat)).recovery_fee_headroom(sat(20_000)).watchmand_cfg(|c|c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(5)));
	if let Some(ln) = &lightning { builder = builder.lightningd(&ln.internal); }
	// Bootstrap the native epoch before the first operator pool publication.
	builder = builder.no_vtxo_pool();
	let (lifetime, round_interval) = (p.vtxo_lifetime, p.round_interval_s);
	let (pool_amount, pool_count) = (p.ln_receive_sat, p.ln_receive + 2);
	let srv = builder.cfg(move |c| {
		c.vtxo_lifetime = BlockDelta::new(lifetime as u16);
		c.vtxopool.vtxo_lifetime = BlockDelta::new(lifetime as u16);
		if pool_count > 2 {
			// Provision the declared receive burst before funding begins. The
			// framework's three large pool coins run out of allocation depth
			// during a burst without new blocks, even with ample wallet funds.
			// Exact-sized coins need no chained change; retain the depth guard.
			c.vtxopool.vtxo_targets = vec![server::vtxopool::VtxoTarget {
				amount: sat(pool_amount), count: pool_count,
			}];
		}
		c.min_board_amount = sat(330);
		c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(5));
		c.round_interval = Duration::from_secs(round_interval);
	}).create().await;
	if let Some(ln) = &lightning { ln.sync().await; }
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let env = Env {
		bitcoind: ctx.bitcoind_arc(), datadir: ctx.datadir.clone(),
		cfg: ctx.bark_default_cfg(&srv.ark_url(), Some(ctx.bitcoind())), db: db.clone(), out: out.clone(),
		ln_client: match &lightning { Some(ln)=>Some(ln.internal.grpc_client().await),None=>None },
		hold_client:match &lightning {Some(ln)=>Some(ln.internal.hold_client().await),None=>None},
	};
	record_ln_boundary(&env,&lightning).await;
	provision_fee_reserve(&ctx,&env,&srv).await;
	let pool=if p.ln_receive>0 {vec![server::vtxopool::VtxoTarget {amount:sat(pool_amount),count:pool_count}]} else {Vec::new()};
	enable_scale_native_epoch(&ctx,&env,&srv,pool).await;
	out.json("environment.json", &json!({
		"datadir": ctx.datadir, "server_config": format!("{:?}", srv.config().expiry_payout),
		"vtxo_lifetime": p.vtxo_lifetime, "round_interval_s": p.round_interval_s,
		"lightning": p.lightning(), "vtxo_pool": p.ln_receive > 0,
	}));
	checkpoint(&env, Some(&*srv), "setup", "end").await;
	let opening=treasury_edge(&env,&srv,"opening").await;

	// ── Core capital split into confirmed coins, so boards never chain ──
	let mut splits = Vec::new();
	for _ in 0..(2 * p.owners() + 50) { splits.push(160_000u64); }
	for _ in 0..p.active_users { splits.push(300_000u64); }
	for _ in 0..funders { splits.push(p.round_chunk as u64 * p.arkoor_sat + 1_000_000); }
	for chunk in splits.chunks(1000) {
		let mut outputs = serde_json::Map::new();
		for amount in chunk {
			outputs.insert(core.get_new_address(None, None).unwrap().assume_checked().to_string(),
				json!(Amount::from_sat(*amount).to_btc()));
		}
		let txid: Txid = core.call("sendmany", &["".into(), Value::Object(outputs)]).unwrap();
		out.line("chain-fee-intents.jsonl",&json!({"txid":txid,"category":"bank_allocation"}));
		// Each 1,000-output split is large. Confirm it before spending its
		// change in the next split: otherwise Core's cluster-size limit is
		// reached by the full profile before any owners are funded.
		ctx.generate_blocks(1).await;
		out.event("capital-split", json!({ "txid": txid,
			"outputs": chunk.len(), "tip": core.get_block_count().unwrap() }));
	}
	checkpoint(&env, Some(&*srv), "funding", "start").await;
	let funding_deadline = Instant::now() + Duration::from_secs(p.funding_deadline_s);

	// Funders board first. Each later refreshes its board in one delegated
	// round into its own coins, one per owner of its chunk, for the arkoor
	// sends. A delegated participant must cosign every output leaf, so a
	// round cannot pay other wallets' keys.
	let mut funder_wallets = Vec::new();
	for j in 0..funders {
		let members = (j * p.round_chunk..((j + 1) * p.round_chunk).min(p.owners())).collect::<Vec<_>>();
		let mnemonic = bip39::Mnemonic::generate(12).unwrap();
		let wallet = env.open_wallet(&format!("funder-{j:03}"), &mnemonic, &srv.ark_url(), true).await;
		env.board(&wallet, members.len() as u64 * p.arkoor_sat).await.expect("funder board");
		funder_wallets.push((j, wallet, members));
	}

	// ── owners, in batches ──
	let receive_proxy = if p.ln_receive > 0 {
		let reached = Arc::new(Notify::new());
		Some((srv.start_proxy_no_mailbox(DropClaimReply { reached: reached.clone() }).await, reached))
	} else { None };
	let mut owners: Vec<Owner> = Vec::new();
	let mut incoming_gift_sat = BTreeMap::<usize, u64>::new();
	for start in (0..p.owners()).step_by(p.batch) {
		assert!(Instant::now() < funding_deadline, "funding deadline");
		let end = (start + p.batch).min(p.owners());
		let t0 = Instant::now();
		// Create; board the coin each owner refreshes, and the profile's own
		// coin where it is registered.
		let mut created = futures::stream::iter(start..end).map(|i| {
			let env = env.clone();
			let profile = profile_of(&p, i);
			let server = match (&receive_proxy, profile) {
				(Some((proxy, _)), Profile::LnReceive) => proxy.address.clone(),
				_ => srv.ark_url(),
			};
			let p = p.clone();
			tokio::spawn(async move {
				let (owner, wallet) = create_owner(env.clone(), i, profile, server).await;
				let mut amounts = vec![p.round_sat];
				match profile {
					Profile::Registered => amounts.push(p.board_sat),
					Profile::Unregistered => amounts.push(p.unregistered_sat),
					Profile::LnSend => amounts.push(p.ln_send_board_sat),
					Profile::Pending | Profile::LnReceive => {},
				}
				for amount in &amounts {
					let board = env.board(&wallet, *amount).await
						.unwrap_or_else(|e| panic!("owner {i} board: {e:#}"));
					env.bitcoind.await_transaction(board.funding_tx.compute_txid()).await;
				}
				(owner, wallet, amounts.len())
			})
		}).buffer_unordered(p.concurrency).collect::<Vec<_>>().await
			.into_iter().map(|r| r.expect("owner task")).collect::<Vec<_>>();
		created.sort_by_key(|(o, _, _)| o.index);
		ctx.generate_blocks(BOARD_CONFIRMATIONS).await;

		// Registration, then each owner refreshes its round coin in a
		// delegated round: the owner signs its forfeit and its own leaf.
		let registered = futures::stream::iter(created.into_iter()).map(|(owner, wallet, boards)| {
			let round_sat = p.round_sat;
			tokio::spawn(async move {
				let coins = wait_spendable(&wallet, boards, Duration::from_secs(120)).await
					.unwrap_or_else(|e| panic!("owner {} registration: {e:#}", owner.index));
				let input = coins.iter().find(|c| c.vtxo.amount() == sat(round_sat)).unwrap().vtxo.id();
				wallet.refresh_vtxos_delegated(vec![input]).await
					.unwrap_or_else(|e| panic!("owner {} delegated refresh: {e:#}", owner.index))
					.expect("a refresh participation");
				(owner, wallet, input)
			})
		}).buffer_unordered(p.concurrency).collect::<Vec<_>>().await;
		let mut batch = registered.into_iter().map(|r| r.expect("registration task")).collect::<Vec<_>>();
		batch.sort_by_key(|(o, _, _)| o.index);
		let round_deadline = Instant::now() + Duration::from_secs(900);
		loop {
			assert!(Instant::now() < round_deadline, "owner refreshes did not complete");
			let open = futures::stream::iter(batch.iter()).map(|(_, wallet, input)| async move {
				wallet.sync().await;
				let pending = wallet.pending_round_states().await.unwrap().len();
				let coins = wallet.spendable_vtxos().await.unwrap();
				let refreshed = coins.iter().any(|c| c.vtxo.amount() == sat(p.round_sat) && c.vtxo.id() != *input);
				pending > 0 || !refreshed
			}).buffer_unordered(p.concurrency).filter(|open| futures::future::ready(*open)).count().await;
			if open == 0 { break; }
			tokio::time::sleep(Duration::from_secs(2)).await;
			ctx.generate_blocks(1).await;
		}
		for (owner, wallet, input) in batch.iter_mut() {
			let coin = wallet.spendable_vtxos().await.unwrap().into_iter()
				.find(|c| c.vtxo.amount() == sat(p.round_sat) && c.vtxo.id() != *input).unwrap();
			let full = wallet.get_full_vtxo(coin.vtxo.id()).await.unwrap();
			owner.coins.push(Coin { id: full.id().to_string(), amount: full.amount().to_sat(),
				source: "round_output", user_pubkey: full.user_pubkey().to_string() });
			owner.anchors.push(full);
		}
		let mut batch = batch.into_iter().map(|(o, w, _)| (o, w)).collect::<Vec<_>>();

		// Registered boards; unregistered sends: the full board coin to the
		// next owner's spare address, cosigned by the server, never
		// registered nor posted; pending boards: the owner leaves before its
		// funding confirms.
		let n = batch.len();
		let gifts = batch.iter().map(|(o, _)| o.gift_address.clone()).collect::<Vec<_>>();
		let mut delivered_gifts = Vec::<(usize, usize, Vec<Vtxo>)>::new();
		for k in 0..n {
			let (owner, wallet) = &mut batch[k];
			let own_coin = |amount: u64| {
				let wallet = wallet.clone();
				async move {
					let coin = wallet.spendable_vtxos().await.unwrap().into_iter()
						.find(|c| c.vtxo.amount() == sat(amount)).unwrap();
					wallet.get_full_vtxo(coin.vtxo.id()).await.unwrap()
				}
			};
			match owner.profile {
				Profile::Registered => {
					let full = own_coin(p.board_sat).await;
					owner.coins.push(Coin { id: full.id().to_string(), amount: full.amount().to_sat(),
						source: "registered_board", user_pubkey: full.user_pubkey().to_string() });
					owner.anchors.push(full);
				},
				Profile::Unregistered => {
					let recipient = &gifts[(k + 1) % n];
					let full = own_coin(p.unregistered_sat).await;
					let key = wallet.pubkey_keypair(&full.user_pubkey()).await.unwrap().unwrap().1;
					let amount = full.amount();
					let builder = ArkoorPackageBuilder::new_with_checkpoints(vec![full], vec![ArkoorDestination {
						total_amount: amount, policy: recipient.policy().clone(),
					}]).unwrap().generate_user_nonces(&[key]).unwrap();
					let response = srv.get_public_rpc().await.request_arkoor_cosign(
						account_arkoor_request(wallet, recipient, builder.cosign_request().into()).await,
					).await.expect("arkoor cosign").into_inner();
					let outputs = builder.user_cosign(&[key], ArkoorPackageCosignResponse::try_from(response).unwrap())
						.unwrap().build_signed_vtxos();
					assert_eq!(outputs.iter().map(|v| v.amount().to_sat()).sum::<u64>(), p.unregistered_sat);
					assert!(outputs.iter().all(|v| v.user_pubkey() == recipient.policy().user_pubkey()));
					delivered_gifts.push(((k + 1) % n, owner.index, outputs));
				},
				Profile::Pending => {
					let board = env.board(wallet, p.pending_sat).await
						.unwrap_or_else(|e| panic!("owner {} pending board: {e:#}", owner.index));
					env.bitcoind.await_transaction(board.funding_tx.compute_txid()).await;
					let coin = wallet.get_full_vtxo(board.vtxos[0]).await.unwrap();
					owner.coins.push(Coin { id: coin.id().to_string(), amount: coin.amount().to_sat(),
						source: "pending_board", user_pubkey: coin.user_pubkey().to_string() });
					owner.anchors.push(coin);
				},
				_ => {},
			}
		}

		// Attribute the gift to the actual next owner, independently of its source profile.
		for (recipient_index, sender_index, outputs) in delivered_gifts {
			let (recipient, _) = &mut batch[recipient_index];
			*incoming_gift_sat.entry(recipient.index).or_default() += p.unregistered_sat;
			out.line("gift-assignments.jsonl", &json!({"sender_owner": sender_index,
				"recipient_owner": recipient.index, "expected_amount_sat": p.unregistered_sat,
				"recipient_pubkey": recipient.gift_address.policy().user_pubkey().to_string(),
				"output_ids": outputs.iter().map(|v| v.id().to_string()).collect::<Vec<_>>() }));
			for v in outputs {
				recipient.coins.push(Coin { id: v.id().to_string(), amount: v.amount().to_sat(),
					source: "unregistered_arkoor", user_pubkey: v.user_pubkey().to_string() });
				recipient.anchors.push(v);
			}
		}

		// Failed Lightning sends: a real hold invoice on the external node,
		// canceled by its payee; the sender leaves before asking a refund.
		// The amount exceeds the round coin, so the send spends the board.
		for (owner, wallet) in batch.iter_mut().filter(|(o, _)| o.profile == Profile::LnSend) {
			let ln = lightning.as_ref().unwrap();
			let preimage = Preimage::random();
			let hash = preimage.compute_payment_hash();
			let mut payee = ln.external.hold_client().await;
			let invoice = payee.invoice(hold::InvoiceRequest {
				payment_hash: hash.as_ref().to_vec(), amount_msat: p.ln_send_sat * 1_000,
				description: Some(hold::invoice_request::Description::Memo(format!("stage4 {}", owner.name))),
				min_final_cltv_expiry: Some(18), expiry: Some(3600), routing_hints: vec![],
			}).await.unwrap().into_inner().bolt11;
			ln.sync().await;
			wallet.pay_lightning_invoice(invoice, None, false).await
				.unwrap_or_else(|e| panic!("owner {} pay: {e:#}", owner.index));
			ln.external.wait_for_hold_invoice_accepted(hash).await;
			payee.cancel(hold::CancelRequest { payment_hash: hash.as_ref().to_vec() }).await.unwrap();
			tokio::time::timeout(Duration::from_secs(120), async {
				loop {
					let attempt = db.read(async |t| t.get_latest_payment_attempt_by_payment_hash(hash).await)
						.await.unwrap().unwrap();
					if attempt.status == server::database::ln::LightningPaymentStatus::Failed { break; }
					tokio::time::sleep(Duration::from_millis(200)).await;
				}
			}).await.expect("the server must record the failed payment");
			let round_id = owner.coins.iter().find(|c| c.source == "round_output").unwrap().id.clone();
			let mut total = 0;
			let mut round_left = false;
			for w in wallet.all_vtxos().await.unwrap() {
				if w.state.kind() == bark::vtxo::VtxoStateKind::Spent { continue; }
				let full = wallet.get_full_vtxo(w.vtxo.id()).await.unwrap();
				total += full.amount().to_sat();
				if full.id().to_string() == round_id { round_left = true; continue; }
				let source = if full.policy().as_server_htlc_send().is_some() { "ln_send_htlc" } else { "ln_send_change" };
				owner.coins.push(Coin { id: full.id().to_string(), amount: full.amount().to_sat(),
					source, user_pubkey: full.user_pubkey().to_string() });
				owner.anchors.push(full);
			}
			if !round_left {
				// The send spent the round coin too; its value is in the HTLC and change.
				let at = owner.coins.iter().position(|c| c.id == round_id).unwrap();
				owner.coins.remove(at);
				owner.anchors.retain(|v| v.id().to_string() != round_id);
			}
			assert_eq!(total, p.ln_send_board_sat + p.round_sat,
				"owner {}: HTLC coins, change and round coin hold the boarded value", owner.index);
		}

		// Lightning receives whose committed claim reply is lost.
		for (owner, wallet) in batch.iter_mut().filter(|(o, _)| o.profile == Profile::LnReceive) {
			let ln = lightning.as_ref().unwrap();
			let (_, reached) = receive_proxy.as_ref().unwrap();
			tokio::time::timeout(Duration::from_secs(120), srv.wait_for_vtxopool(&ctx)).await
				.expect("vtxo pool ready");
			let before = unregistered_pubkey_outputs(&db).await;
			let invoice = wallet.bolt11_invoice(sat(p.ln_receive_sat), None, None).await
				.unwrap_or_else(|e| panic!("owner {} invoice: {e:#}", owner.index));
			let hash = PaymentHash::from(&invoice);
			let bolt11 = invoice.to_string();
			let payer = ln.external.try_pay_bolt11(bolt11);
			let (paid, ()) = tokio::join!(payer, async {
				tokio::time::timeout(Duration::from_secs(60), async {
					loop {
						let sub = db.read(async |t| t.get_htlc_subscription_by_payment_hash(hash).await)
							.await.unwrap();
						if sub.is_some_and(|s| s.status == server::database::ln::LightningHtlcSubscriptionStatus::Accepted) { break; }
						tokio::time::sleep(Duration::from_millis(100)).await;
					}
				}).await.expect("external Lightning funds the receive");
				tokio::time::timeout(Duration::from_secs(60), async {
					tokio::select! {
						r = wallet.try_claim_lightning_receive(hash, true) =>
							panic!("owner {}: claim finished despite the lost reply (success={})", owner.index,r.is_ok()),
						_ = reached.notified() => {},
					}
				}).await.expect("the claim reaches the server");
			});
			paid.unwrap_or_else(|e| panic!("owner {}: external payment: {e:#}", owner.index));
			env.out.line("external-receipts.jsonl", &json!({ "owner":owner.index,"payment_hash":hash.to_string(),"amount_msat":p.ln_receive_sat.checked_mul(1000).unwrap(),"source":"excluded external Lightning payer","producer":"actual successful external node pay; independently verify internal collected invoice" }));
			let after = unregistered_pubkey_outputs(&db).await;
			let new = after.into_iter().filter(|(id, _)| !before.contains_key(id)).collect::<Vec<_>>();
			let total = new.iter().map(|(_, a)| a).sum::<u64>();
			assert_eq!(total, p.ln_receive_sat, "owner {}: claim outputs {new:?}", owner.index);
			let ids = new.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
			for full in full_coins(&db, &ids).await {
				owner.coins.push(Coin { id: full.id().to_string(), amount: full.amount().to_sat(),
					source: "ln_receive_claim", user_pubkey: full.user_pubkey().to_string() });
				owner.anchors.push(full);
			}
		}

		// Every owner of the batch leaves.
		for (owner, wallet) in batch {
			drop(wallet);
			owners.push(owner);
		}
		out.event("owner-batch", json!({ "first": start, "last": end - 1, "seconds": t0.elapsed().as_secs_f64() }));
	}

	// ── funders: a delegated round into their own coins, then arkoor sends ──
	assert!(owners.iter().enumerate().all(|(i, o)| o.index == i));
	let t0 = Instant::now();
	for (j, wallet, members) in &funder_wallets {
		let input = wait_spendable(wallet, 1, Duration::from_secs(120)).await.unwrap().remove(0);
		let input = wallet.get_full_vtxo(input.id()).await.unwrap();
		let mut outputs = Vec::new();
		for _ in members {
			let (key, _) = wallet.derive_store_next_keypair().await.unwrap();
			outputs.push(VtxoRequest { amount: sat(p.arkoor_sat), policy: ark::VtxoPolicy::new_pubkey(key.public_key()) });
		}
		assert_eq!(outputs.iter().map(|o| o.amount).sum::<Amount>(), input.amount(), "funder {j}: outputs spend the board exactly");
		wallet.join_next_round_delegated(RoundParticipation {
			inputs: vec![input], outputs, unblinded_mailbox_id: None,
		}, None).await.unwrap_or_else(|e| panic!("funder {j} participation: {e:#}"));
	}
	// Rounds run on their interval; confirm them and let funders claim.
	let round_deadline = Instant::now() + Duration::from_secs(1800);
	for (j, wallet, members) in &funder_wallets {
		loop {
			assert!(Instant::now() < round_deadline, "funder {j}: delegated round did not complete");
			wallet.sync().await;
			let pending = wallet.pending_round_states().await.unwrap();
			let spendable = wallet.spendable_vtxos().await.unwrap().len();
			if pending.is_empty() && spendable == members.len() { break; }
			tokio::time::sleep(Duration::from_secs(2)).await;
			ctx.generate_blocks(1).await;
		}
	}
	out.event("funder-rounds", json!({ "funders": funder_wallets.len(), "seconds": t0.elapsed().as_secs_f64() }));
	let t1 = Instant::now();
	let sends = futures::future::join_all(funder_wallets.iter().map(|(j, wallet, members)| {
		let owners = &owners;
		let arkoor_sat = p.arkoor_sat;
		async move {
			for i in members {
				wallet.send_arkoor_payment(&owners[*i].arkoor_address, sat(arkoor_sat)).await
					.unwrap_or_else(|e| panic!("funder {j} send to owner {i}: {e:#}"));
			}
			let left = wallet.spendable_vtxos().await.unwrap();
			assert!(left.is_empty(), "funder {j} keeps no coin: {left:?}");
		}
	})).await;
	drop(sends);
	drop(funder_wallets);
	out.event("funder-arkoor", json!({ "seconds": t1.elapsed().as_secs_f64() }));

	// Find each owner's arkoor receive by its own key.
	let ids = db.read(async |t| Ok(t.query(
		"SELECT vtxo_id FROM vtxo WHERE policy_type='pubkey' AND amount = $1", &[&(p.arkoor_sat as i64)],
	).await?)).await.unwrap().into_iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>();
	let by_key = owners.iter().enumerate().map(|(k, o)| (o.arkoor_address.policy().user_pubkey(), k))
		.collect::<HashMap<PublicKey, _>>();
	for coin in full_coins(&db, &ids).await {
		if let Some(k) = by_key.get(&coin.user_pubkey()) {
			let owner = &mut owners[*k];
			owner.coins.push(Coin { id: coin.id().to_string(), amount: coin.amount().to_sat(), source: "arkoor_receive",
				user_pubkey: coin.user_pubkey().to_string() });
			owner.anchors.push(coin);
		}
	}
	for o in &owners {
		assert_eq!(o.coins.iter().filter(|c| c.source == "arkoor_receive").count(), 1, "owner {}: one arkoor receive", o.index);
		let funded = p.round_sat + p.arkoor_sat + match o.profile {
			Profile::Registered => p.board_sat, Profile::Pending => p.pending_sat,
			Profile::Unregistered => 0, Profile::LnSend => p.ln_send_board_sat,
			Profile::LnReceive => p.ln_receive_sat,
		} + incoming_gift_sat.get(&o.index).copied().unwrap_or(0);
		assert_eq!(o.coins.iter().map(|c| c.amount).sum::<u64>(), funded, "owner {}: entitlements hold what it was funded", o.index);
	}
	// Server-side state of every entitlement, by source.
	let all_ids = owners.iter().flat_map(|o| o.coins.iter().map(|c| c.id.clone())).collect::<Vec<_>>();
	let states = db.read(async |t| Ok(t.query(
		"SELECT vtxo_id, spend_state::TEXT FROM vtxo WHERE vtxo_id = ANY($1)", &[&all_ids],
	).await?)).await.unwrap().into_iter().map(|r| (r.get::<_, String>(0), r.get::<_, String>(1))).collect::<HashMap<_, _>>();
	let pending_rows = db.read(async |t| Ok(t.query("SELECT vtxo_id FROM pending_board", &[]).await?))
		.await.unwrap().into_iter().map(|r| r.get::<_, String>(0)).collect::<BTreeSet<_>>();
	let mut mix = BTreeMap::<String, BTreeMap<String, (usize, u64)>>::new();
	for o in &owners {
		for c in &o.coins {
			let state = states.get(&c.id).cloned().unwrap_or_else(||
				if pending_rows.contains(&c.id) { "pending_board_row".into() } else { "absent".into() });
			let e = mix.entry(c.source.to_owned()).or_default().entry(state).or_default();
			e.0 += 1;
			e.1 += c.amount;
		}
	}
	for o in &owners { out.line("owners.jsonl", &o.public()); }
	let entitlements = all_ids.len();
	let principal = owners.iter().flat_map(|o| o.coins.iter().map(|c| c.amount)).sum::<u64>();
	out.json("funding.json", &json!({
		"owners": owners.len(), "entitlements": entitlements, "principal_sat": principal,
		"mix": mix, "seconds": started.elapsed().as_secs_f64(),
	}));
	checkpoint(&env, Some(&*srv), "funding", "end").await;

	// The cohort expires later, inside the loaded window. Its backing anchors,
	// and the first cohort expiry the baseline must stay below.
	let coins = owners.iter().flat_map(|o| o.anchors.iter().cloned()).collect::<Vec<_>>();
	let last_expiry = coins.iter().map(|v| v.expiry_height().to_u32()).max().unwrap();
	let first_expiry = coins.iter().map(|v| v.expiry_height().to_u32()).min().unwrap();
	let anchors = coins.iter().map(|v| v.chain_anchor().to_string()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
	out.json("expiry.json", &json!({ "first_expiry": first_expiry, "last_expiry": last_expiry,
		"anchors": anchors.len(), "tip_after_funding": core.get_block_count().unwrap() }));
	train_fee_estimator_recorded(&ctx,&env).await;

	// ── active users ──
	let mut active = Vec::new();
	let mut active_mnemonics = Vec::new();
	for u in 0..p.active_users {
		let mnemonic = bip39::Mnemonic::generate(12).unwrap();
		let wallet = env.open_wallet(&format!("active-{u:02}"), &mnemonic, &srv.ark_url(), true).await;
		env.board(&wallet, p.active_board_sat).await.expect("active user board");
		active.push(wallet);
		active_mnemonics.push(mnemonic);
	}
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	for (u, wallet) in active.iter().enumerate() {
		wait_spendable(wallet, 1, Duration::from_secs(120)).await.unwrap_or_else(|e| panic!("active {u}: {e:#}"));
	}

	// ── baseline refreshes, payouts disabled ──
	// captaind restarts before the baseline too, unchanged, so both phases
	// begin with users reopening their wallets on a freshly started server.
	drop(active);
	srv.stop().await.unwrap();
	srv.start().await.unwrap();
	let mut active = Vec::new();
	for (u, mnemonic) in active_mnemonics.iter().enumerate() {
		active.push(env.open_wallet(&format!("active-{u:02}"), mnemonic, &srv.ark_url(), false).await);
	}
	let errors = Arc::new(AtomicUsize::new(0));
	let mining = Arc::new(AtomicBool::new(true));
	// One block every interval, each carrying four real 3 sat/vB payments
	// from Core, so its fee estimator keeps data through the long advance.
	let miner = tokio::spawn({
		let (bitcoind, mining, interval, out) = (ctx.bitcoind_arc(), mining.clone(), p.block_interval_s, out.clone());
		async move {
			while mining.load(Ordering::Relaxed) {
				tokio::time::sleep(Duration::from_secs(interval)).await;
				let core = bitcoind.sync_client();
				for _ in 0..4 {
					let address = core.get_new_address(None, None).unwrap().assume_checked();
					let txid: Txid = core.call("sendtoaddress", &[
						address.to_string().into(), 0.001.into(), "".into(), "".into(),
						false.into(), true.into(), Value::Null, "unset".into(), Value::Null, 3.into(),
					]).expect("every real estimator-training transaction must succeed");
					out.line("chain-fee-intents.jsonl",&json!({"txid":txid,"category":"estimator_training","scheduled_sat_vb":3}));
				}
				bitcoind.generate(1).await;
			}
		}
	});
	checkpoint(&env, Some(&*srv), "baseline", "start").await;
	let baseline_t0 = Instant::now();
	let done = Arc::new(AtomicBool::new(true));
	run_refreshes(&env, &active, "baseline", p.baseline_cycles, done.clone(), &errors, &p).await;
	let baseline_seconds = baseline_t0.elapsed().as_secs_f64();
	checkpoint(&env, Some(&*srv), "baseline", "end").await;
	let tip = core.get_block_count().unwrap() as u32;
	assert!(tip < first_expiry, "the baseline reached the cohort's first expiry ({tip} >= {first_expiry}); \
		it is no longer free of expiry load");

	// ── payouts enabled: one captaind replaces the other ──
	drop(active);
	srv.stop().await.unwrap();
	{
		let watchman = srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE);
		let mut c = srv.config_mut();
		c.expiry_payout.enabled = true;
		c.expiry_payout.interval = Duration::from_secs(p.payout_interval_s);
		c.expiry_payout.grace_blocks = 0;
		c.expiry_payout.sweep_min_confs=1;
		c.expiry_payout.min_payout_sat=1000;
		c.expiry_payout.watchman_config = Some(watchman);
	}
	srv.start().await.unwrap();
	let enabled_utc = utc();
	let mut active = Vec::new();
	for (u, mnemonic) in active_mnemonics.iter().enumerate() {
		active.push(env.open_wallet(&format!("active-{u:02}"), mnemonic, &srv.ark_url(), false).await);
	}

	// ── loaded: the cohort expires, is swept and paid while users refresh ──
	checkpoint(&env, Some(&*srv), "loaded", "start").await;
	let loaded_t0 = Instant::now();
	let done = Arc::new(AtomicBool::new(false));
	let telemetry = tokio::spawn({
		let (db, out, done, bitcoind, ids) = (db.clone(), out.clone(), done.clone(), ctx.bitcoind_arc(), all_ids.clone());
		let anchors = anchors.clone();
		let deadline = Instant::now() + Duration::from_secs(p.loaded_deadline_s);
		async move {
			let t0 = Instant::now();
			loop {
				let row = db.read(async |t| Ok(t.query_one(
					DIRECT_TERMINAL_COUNT, &[&ids],
				).await?)).await.unwrap();
				let (settled, txs) = (row.get::<_, i64>(0) as usize, row.get::<_, i64>(1));
				let txids = db.read(async |t| Ok(t.query("SELECT DISTINCT txid FROM expiry_settlement", &[]).await?))
					.await.unwrap().into_iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>();
				let core = bitcoind.sync_client();
				let unconfirmed = txids.iter().filter(|t| core.get_raw_transaction_info(&t.parse::<Txid>().unwrap(), None)
					.ok().and_then(|i| i.confirmations).unwrap_or(0) == 0).count();
				let swept = db.read(async |t| Ok(t.query_one(
					"SELECT count(*) FROM vtxo WHERE vtxo_id = ANY($1) AND onchain_spent_txid IS NOT NULL", &[&anchors],
				).await?)).await.unwrap().get::<_, i64>(0);
				out.line("payout-telemetry.jsonl", &json!({ "utc": utc(), "seconds": t0.elapsed().as_secs_f64(),
					"settled": settled, "expected": ids.len(), "payout_txs": txs, "unconfirmed_payouts": unconfirmed,
					"anchors_spent": swept, "anchors": anchors.len(),
					"tip": core.get_block_count().unwrap(), "first_expiry": first_expiry, "last_expiry": last_expiry }));
				if settled == ids.len() && unconfirmed == 0 {
					done.store(true, Ordering::Relaxed);
					return Some(t0.elapsed().as_secs_f64());
				}
				if Instant::now() > deadline { done.store(true, Ordering::Relaxed); return None; }
				tokio::time::sleep(Duration::from_secs(2)).await;
			}
		}
	});
	run_refreshes(&env, &active, "loaded", p.loaded_min_cycles, done.clone(), &errors, &p).await;
	let backlog_cleared_after = telemetry.await.unwrap();
	let loaded_seconds = loaded_t0.elapsed().as_secs_f64();
	mining.store(false, Ordering::Relaxed);
	miner.await.unwrap();
	checkpoint(&env, Some(&*srv), "loaded", "end").await;
	let backlog_cleared_after = backlog_cleared_after.expect("the backlog must clear before the loaded deadline");
	drop(active);

	// ── reconciliation from the database and the chain ──
	checkpoint(&env, Some(&*srv), "reconcile", "start").await;
	let reconciliation = reconcile(&env, &owners, enabled_utc).await;
	out.json("reconciliation.json", &reconciliation);
	checkpoint(&env, Some(&*srv), "reconcile", "end").await;

	let closing=treasury_edge(&env,&srv,"pre-recovery").await;
	conserve_treasury(&env,&opening,&closing,0);
	// ── seed-only discovery and spend, server stopped ──
	srv.stop().await.unwrap();
	checkpoint(&env, None, "recovery", "start").await;
	let recovery = recover(&ctx, &env, &owners, &p).await;
	out.json("recovery.json", &recovery);
	checkpoint(&env, None, "recovery", "end").await;
	assert!(recovery["failures"].as_array().unwrap().is_empty(), "{recovery}");
	assert_eq!(recovery["spends"].as_u64().unwrap(),owners.len() as u64,"every declared owner must actually spend from seed");
	srv.watchmand().stop().await.unwrap();
	let final_edge=treasury_edge_after_recovery(&env,&srv,"after-seed-recovery").await;
	conserve_treasury(&env,&closing,&final_edge,1);
	let final_accounting=finalize_treasury_accounting(&env,&opening,&final_edge);

	let failures = reconciliation["failures"].as_array().unwrap().len() + recovery["failures"].as_array().unwrap().len();
	out.json("result.json", &json!({
		"owners": owners.len(), "entitlements": entitlements, "principal_sat": principal,
		"refresh_errors": errors.load(Ordering::Relaxed),
		"baseline_seconds": baseline_seconds, "loaded_seconds": loaded_seconds,
		"backlog_cleared_after_s": backlog_cleared_after,
		"payouts_enabled_utc": enabled_utc,
		"reconciliation_failures": reconciliation["failures"], "recovery_failures": recovery["failures"],
		"elapsed_s": started.elapsed().as_secs_f64(), "whole_boundary_final_accounting":final_accounting,
	}));
	assert_eq!(failures, 0, "reconciliation or recovery failed; see reconciliation.json and recovery.json");
	assert_eq!(errors.load(Ordering::Relaxed), 0, "active users saw errors; see refreshes.jsonl");
	println!("stage4 source execution finished; independent audit required owners={} entitlements={entitlements}", owners.len());
}
