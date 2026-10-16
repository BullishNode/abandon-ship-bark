#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn stage5_unclaimed_hark_source() {
	require_operating_build();
	let ctx = TestContext::new("bark_sdk/stage5_unclaimed_hark_source").await;
	let out = Out::new();
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc())
		.no_vtxo_pool().funded(sat(100_000_000)).recovery_fee_headroom(sat(20_000)).watchmand_cfg(|c|c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(5))).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(128);
			c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(5));
			c.round_interval = Duration::from_secs(3600);
		}).create().await;
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let env = Env { bitcoind: ctx.bitcoind_arc(), datadir: ctx.datadir.clone(),
		cfg: ctx.bark_default_cfg(&srv.ark_url(), Some(ctx.bitcoind())), db: db.clone(), out: out.clone(),ln_client:None,hold_client:None };
	provision_fee_reserve(&ctx,&env,&srv).await;
	enable_scale_native_epoch(&ctx,&env,&srv,Vec::new()).await;
	let opening=treasury_edge(&env,&srv,"opening").await;
	let (mut owner, wallet) = create_owner(env.clone(), 0, Profile::Registered, srv.ark_url()).await;
	owner.cohort="hark";
	let board = env.board(&wallet, 300_000).await.unwrap();
	ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let input = wait_spendable(&wallet, 1, Duration::from_secs(120)).await.unwrap().remove(0);
	let full_input = wallet.get_full_vtxo(input.id()).await.unwrap();
	let scheduled = ctx.bitcoind().get_block_count().await as u32 + 10;
	let round = wallet.refresh_vtxos_scheduled(vec![input.id()], bitcoin_ext::BlockHeight::new(scheduled))
		.await.unwrap().expect("scheduled participation");
	assert!(round.state().unlock_hash().is_some());
	let accepted_input=round.state().participation().total_in().unwrap().to_sat();
	let accepted_output=round.state().participation().total_out().unwrap().to_sat();
	// Disappear before the scheduled height and before any claim/forfeit RPC.
	drop(round);
	drop(wallet);
	out.event("owner-absent", json!({ "scheduled_height": scheduled }));
	let tip = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(scheduled - tip).await;
	let mut finished = srv.subscribe_log::<server_log::RoundFinished>();
	srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(120)).await.expect("scheduled round completes without the owner");
	ctx.generate_blocks(6).await;
	let candidates = db.read(async |t| Ok(t.query(
		"SELECT vtxo FROM vtxo WHERE spend_state='unclaimed' AND policy_type='pubkey'", &[],
	).await?)).await.unwrap();
	let mailbox = owner.mailbox_pk.parse::<PublicKey>().unwrap().serialize().to_vec();
	for row in candidates {
		let coin = Vtxo::deserialize(&row.get::<_, Vec<u8>>(0)).unwrap();
		let key = coin.user_pubkey().serialize().to_vec();
		let linked = db.read(async |t| Ok(t.query_opt(
			"SELECT mailbox_pk FROM key_link WHERE user_pubkey=$1", &[&key],
		).await?)).await.unwrap();
		if linked.is_some_and(|r| r.get::<_, Vec<u8>>(0) == mailbox) {
			owner.coins.push(Coin { id: coin.id().to_string(), amount: coin.amount().to_sat(),
				source: "hark_unclaimed", user_pubkey: coin.user_pubkey().to_string() });
			owner.anchors.push(coin);
		}
	}
	assert!(!owner.coins.is_empty(), "owner has genuinely unclaimed hArk outputs");
	assert_eq!(owner.coins.iter().map(|c|c.amount).sum::<u64>(),accepted_output);
	out.line("hark-economic-groups.jsonl",&json!({"owner":owner.index,"original_id":full_input.id(),"replacement_ids":owner.coins.iter().map(|c|&c.id).collect::<Vec<_>>(),"accepted_input_sat":accepted_input,"accepted_output_sat":accepted_output,"customer_consented_protocol_delta_sat":accepted_input.checked_sub(accepted_output).unwrap(),"protocol_delta_is_not_miner_cost":true}));
	let expiry = owner.anchors.iter().chain([&full_input]).map(|v| v.expiry_height().to_u32()).max().unwrap();
	let tip = ctx.bitcoind().get_block_count().await as u32;
	ctx.generate_blocks(expiry.saturating_sub(tip) + 6).await;
	train_fee_estimator_recorded(&ctx,&env).await;
	srv.stop().await.unwrap();
	{
		let watchman = srv.watchmand().config().data_dir.join(WATCHMAND_CONFIG_FILE);
		let mut cfg = srv.config_mut();
		cfg.expiry_payout.enabled = true;
		cfg.expiry_payout.interval = Duration::from_secs(1);
		cfg.expiry_payout.grace_blocks = 0;
		cfg.expiry_payout.sweep_min_confs=1;
		cfg.expiry_payout.min_payout_sat=1000;
		cfg.expiry_payout.watchman_config = Some(watchman);
	}
	srv.start().await.unwrap();
	let enabled = utc();
	let ids = owner.coins.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
	let deadline = Instant::now() + Duration::from_secs(180);
	loop {
		let settled = db.read(async |t| Ok(t.query_one(
			"SELECT count(*) FROM expiry_settlement WHERE id=ANY($1)", &[&ids],
		).await?)).await.unwrap().get::<_, i64>(0);
		if settled as usize == ids.len() { break; }
		assert!(Instant::now() < deadline, "unclaimed hArk payout did not settle");
		ctx.generate_blocks(1).await;
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	ctx.generate_blocks(2).await;
	out.line("owners.jsonl", &owner.public());
	let owners = vec![owner];
	let reconciliation = reconcile(&env, &owners, enabled).await;
	assert!(reconciliation["failures"].as_array().unwrap().is_empty(), "{reconciliation}");
	out.json("reconciliation.json", &reconciliation);
	let paid_input = db.read(async |t| Ok(t.query_one(
		"SELECT count(*) FROM expiry_settlement WHERE id=$1", &[&full_input.id().to_string()],
	).await?)).await.unwrap().get::<_, i64>(0);
	assert_eq!(paid_input, 0, "predecessor cannot be paid in addition to replacement");
	let old_state = db.read(async |t| Ok(t.query_one(
		"SELECT spend_state::TEXT, spent_in_round FROM vtxo WHERE vtxo_id=$1", &[&full_input.id().to_string()],
	).await?)).await.unwrap();
	assert_eq!(old_state.get::<_, String>(0), "spent");
	assert!(old_state.get::<_, Option<i64>>(1).is_some());
	let remaining = db.read(async |t| Ok(t.query(
		"SELECT vtxo FROM vtxo WHERE policy_type='pubkey' AND spend_state IN ('spendable','unclaimed','unregistered')", &[],
	).await?)).await.unwrap();
	let mut padding = Vec::new();
	for r in remaining {
		let v: Vtxo = Vtxo::deserialize(&r.get::<_, Vec<u8>>(0)).unwrap();
		let key = v.user_pubkey().serialize().to_vec();
		let linked = db.read(async |t| Ok(t.query_opt("SELECT mailbox_pk FROM key_link WHERE user_pubkey=$1", &[&key]).await?))
			.await.unwrap();
		assert!(linked.is_none(), "no actual user entitlement remains unsettled");
		let hash = v.unlock_hash().expect("remaining output is a locked padding leaf");
		let participation = db.read(async |t| t.get_round_participation_by_unlock_hash(hash).await).await.unwrap();
		assert!(participation.is_none(), "padding has no user's participation");
		padding.push(json!({ "id": v.id(), "amount_sat": v.amount().to_sat(), "mailbox_link": false,
			"participation": false, "classification": "server padding, no user entitlement" }));
	}
	out.json("hark-state-proof.json", &json!({ "predecessor": full_input.id(),
		"predecessor_state": "spent", "predecessor_settlements": paid_input, "remaining_padding": padding }));
	let closing=treasury_edge(&env,&srv,"pre-recovery").await;conserve_treasury(&env,&opening,&closing,0);
	srv.stop().await.unwrap();
	let recovery = recover(&ctx, &env, &owners, &Params::from_env()).await;
	assert!(recovery["failures"].as_array().unwrap().is_empty(), "{recovery}");
	assert_eq!(recovery["spends"], 1);
	out.json("recovery.json", &recovery);
	srv.watchmand().stop().await.unwrap();
	let final_edge=treasury_edge_after_recovery(&env,&srv,"after-seed-recovery").await;
	conserve_treasury(&env,&closing,&final_edge,1);
	let final_accounting=finalize_treasury_accounting(&env,&opening,&final_edge);
	ctx.bitcoind().stop().await.unwrap(); // Preserve the final confirmed recovery block.
	out.json("hark-result.json", &json!({ "result": "UNQUALIFIED", "independent_audit_required":true,"original_input_paid": false,
		"entitlements": ids.len(), "owner_silent_until_payout": true, "confirmed_seed_spends": 1, "whole_boundary_final_accounting":final_accounting }));
}


// Independent treasury accounting against Core prevouts, with the persisted
// upstream BDK wallet used only to identify which scripts belong to the server.
// Public transaction data is retained; no private descriptor is exported.
// Reuses the genuine funding flows of the stage-4 harness. Each cycle shares
// the same server, database, Core wallet and rounds treasury; owner seeds/keys
// and funder wallets are fresh. No operator funding occurs here.
async fn soak_fund_cohort(
	ctx: &TestContext, srv: &Captaind, env: &Env, p: &Params,
	lightning: &Option<ark_testing::context::LightningPaymentSetup>, cycle: usize,
) -> Vec<Owner> {
	let out = env.out.clone();
	let db = env.db.clone();
	let funders = p.owners().div_ceil(p.round_chunk.max(1));
	let funding_deadline = Instant::now() + Duration::from_secs(p.funding_deadline_s);
	// Funders board first. Each later refreshes its board in one delegated
	// round into its own coins, one per owner of its chunk, for the arkoor
	// sends. A delegated participant must cosign every output leaf, so a
	// round cannot pay other wallets' keys.
	let mut funder_wallets = Vec::new();
	for j in 0..funders {
		let members = (j * p.round_chunk..((j + 1) * p.round_chunk).min(p.owners())).collect::<Vec<_>>();
		let mnemonic = bip39::Mnemonic::generate(12).unwrap();
		let wallet = env.open_wallet(&format!("funder-{cycle:02}-{j:03}"), &mnemonic, &srv.ark_url(), true).await;
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
				let (owner, wallet) = create_owner(env.clone(), cycle * 1000 + i, profile, server).await;
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
	assert!(owners.iter().enumerate().all(|(i, o)| o.index == cycle * 1000 + i));
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
	for owner in &owners { out.line("owners.jsonl", &owner.public()); }
	owners
}

// One chain, Core customer wallet, captaind database and operator treasury for
// the entire soak. Coins are genuinely created by the same flows as stage 4.
#[test]
fn stage5_soak() {
	// Nested unoptimized fixture futures need a larger stack. This applies
	// only to the harness threads, never to the production child processes.
	std::thread::Builder::new().name("soak-fixture".into()).stack_size(16 * 1024 * 1024)
		.spawn(|| tokio::runtime::Builder::new_multi_thread().worker_threads(8)
			.thread_stack_size(16 * 1024 * 1024).enable_all().build().unwrap()
			.block_on(Box::pin(soak_inner()))).unwrap().join().unwrap();
}

async fn soak_inner() {
	let capital_cycles = env_u64("STAGE5_CAPITAL_CYCLES", 10) as usize;
	let mixed_cycles = env_u64("STAGE5_MIXED_CYCLES", 10) as usize;
	let seed = env_u64("STAGE5_SEED", 20261011);
	require_operating_build();
	let ctx = TestContext::new("bark_sdk/stage5_soak").await;
	let out = Out::new();
	let lightning = Some(ctx.new_lightning_setup("lightningd").await);
	let srv = ctx.captaind("server").bitcoind(ctx.bitcoind_arc()).funded(sat(100_000_000)).no_vtxo_pool()
		.lightningd(&lightning.as_ref().unwrap().internal).recovery_fee_headroom(sat(20_000)).watchmand_cfg(|c|c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(5))).cfg(|c| {
			c.vtxo_lifetime = BlockDelta::new(432);
			c.vtxopool.vtxo_lifetime = BlockDelta::new(432);
			c.vtxopool.vtxo_pre_expiry = BlockDelta::new(24);
			c.vtxopool.vtxo_targets = vec![
				server::vtxopool::VtxoTarget { amount: sat(100_000), count: 8 },
				server::vtxopool::VtxoTarget { amount: sat(10_000), count: 4 },
			];
			c.min_board_amount = sat(330);
			c.fee_estimator.max_fee_rate=Some(FeeRate::from_sat_per_vb_u32(5));
			c.round_interval = Duration::from_secs(2);
		}).create().await;

	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let env = Env { bitcoind: ctx.bitcoind_arc(), datadir: ctx.datadir.clone(),
		cfg: ctx.bark_default_cfg(&srv.ark_url(), Some(ctx.bitcoind())), db, out: out.clone(),
		ln_client:Some(lightning.as_ref().unwrap().internal.grpc_client().await),
		hold_client:Some(lightning.as_ref().unwrap().internal.hold_client().await) };
	record_ln_boundary(&env,&lightning).await;
	provision_fee_reserve(&ctx,&env,&srv).await;
	enable_scale_native_epoch(&ctx,&env,&srv,vec![
		server::vtxopool::VtxoTarget {amount:sat(100_000),count:8},
		server::vtxopool::VtxoTarget {amount:sat(10_000),count:4},
	]).await;
	train_fee_estimator_recorded(&ctx,&env).await;
	srv.wait_for_sync_height(bitcoin_ext::BlockHeight::new(env.core().get_block_count().unwrap() as u32)).await;
	let core_open: Value = env.core().call("getbalances", &[]).unwrap();
	let mut previous = treasury_edge(&env, &srv, "opening").await;
	let opening=previous.clone();
	out.json("opening-float.json", &json!({ "seed": seed, "whole_assets_msat": previous.balance_msat,
		"core_bank_balance_subset": core_open["mine"], "operator_funding_sat": 100_000_000,
		"later_operator_injections_allowed": false, "capital_cycles": capital_cycles, "mixed_cycles": mixed_cycles }));
	let mut all_owners = Vec::new();
	let errors = Arc::new(AtomicUsize::new(0));
	let mut enabled_utc = utc();
	for cycle in 0..capital_cycles + mixed_cycles {
		let mixed = cycle >= capital_cycles;
		soak_restart(&srv, false).await;
		let mut p = Params::from_env();
		(p.registered, p.pending, p.unregistered, p.ln_send, p.ln_receive) =
			if mixed { (2, 1, 1, 1, 1) } else { (3, 0, 0, 0, 0) };
		p.workload_seed=seed^(cycle as u64).wrapping_mul(0x9e3779b97f4a7c15);
		p.batch = p.owners(); p.round_chunk = p.owners(); p.concurrency = 4;
		p.round_interval_s = 2; p.vtxo_lifetime = 432; p.active_users = if mixed { 2 } else { 0 };
		p.baseline_cycles = 1; p.loaded_min_cycles = 2;
		p.refresh_deadline_s = 120; p.funding_deadline_s = 900; p.loaded_deadline_s = 600;
		p.recovery_deadline_s = 900;
		// Seeded amount variation changes the transaction and capital workload.
		let variation = seed.wrapping_mul(6364136223846793005).wrapping_add(cycle as u64 * 104729) % 5000;
		p.board_sat += variation; p.pending_sat += variation; p.unregistered_sat += variation;
		out.line("cycle-workloads.jsonl", &json!({ "cycle": cycle, "kind": if mixed { "mixed" } else { "capital" },
			"seed": seed, "params": p }));
		// Confirmed Core customer outputs; these never enter the rounds wallet
		// except as backing swept by watchmand. Each batch is confirmed first.
		let mut outputs = serde_json::Map::new();
		for _ in 0..2 * p.owners() + 20 {
			outputs.insert(env.core().get_new_address(None, None).unwrap().assume_checked().to_string(), json!(0.0016));
		}
		for _ in 0..p.active_users {
			outputs.insert(env.core().get_new_address(None, None).unwrap().assume_checked().to_string(), json!(0.003));
		}
		outputs.insert(env.core().get_new_address(None, None).unwrap().assume_checked().to_string(),
			json!(Amount::from_sat(p.round_chunk as u64 * p.arkoor_sat + 1_000_000).to_btc()));
		let split_txid: Txid = env.core().call("sendmany", &["".into(), Value::Object(outputs)]).unwrap();
		out.line("chain-fee-intents.jsonl",&json!({"txid":split_txid,"category":"bank_allocation"}));
		ctx.generate_blocks(1).await;
		let cohort = Box::pin(soak_fund_cohort(&ctx, &srv, &env, &p, &lightning, cycle)).await;
		all_owners.extend(cohort);
		if mixed { all_owners.push(soak_hark_owner(&ctx,&srv,&env,cycle*1000+900,cycle).await); }
		let mut active = Vec::new();
		let mut active_owners = Vec::new();
		for u in 0..p.active_users {
			let (mut owner, wallet) = create_owner(env.clone(), cycle * 1000 + 100 + u, Profile::Registered, srv.ark_url()).await;
			let board = env.board(&wallet, 200_000).await.unwrap();
			ctx.await_transaction(board.funding_tx.compute_txid()).await;
			owner.cohort="active"; active_owners.push(owner); active.push(wallet);
		}
		ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
		for wallet in &active { wait_spendable(wallet, 1, Duration::from_secs(120)).await.unwrap(); }
		drop(active);
		soak_restart(&srv, true).await;
		if cycle == 0 { enabled_utc = utc(); }
		let mut active = Vec::new();
		for o in &active_owners {
			let mnemonic: bip39::Mnemonic = std::fs::read_to_string(env.datadir.join(&o.name).join("mnemonic"))
				.unwrap().trim().parse().unwrap();
			active.push(env.open_wallet(&o.name, &mnemonic, &srv.ark_url(), false).await);
		}
		let mining = Arc::new(AtomicBool::new(true));
		// These are estimator-training self-payments, not customer receive
		// addresses. Reuse four Core-owned scripts within a cycle rather than
		// deriving thousands of unused wallet keys and fsyncing each derivation.
		// Keep the four real transactions, amount and 3 sat/vB fee unchanged.
		// A fresh fixture-only training wallet keeps coin selection bounded.
		// The previous shared wallet accumulated four self-payment UTXOs per
		// block over every cycle; by cycle 16 RPC work alone took >1.5 s/block.
		// Keep the same four transactions, amounts, fee rate and drain deadline.
		let training_name = format!("soak-training-{cycle:02}");
		env.bitcoind.create_wallet(&training_name).await;
		let training_url = format!("{}/wallet/{training_name}", env.bitcoind.rpc_url());
		let training_wallet = bitcoin_ext::rpc::Client::new(&training_url, env.bitcoind.auth()).unwrap();
		let training_funding_address = training_wallet.get_new_address(None, None).unwrap().assume_checked();
		let training_funding = env.core().send_to_address(&training_funding_address, sat(5_000_000),
			None, None, None, None, None, None).unwrap();
		ctx.await_transaction(training_funding).await;
		out.line("chain-fee-intents.jsonl",&json!({"txid":training_funding,"category":"training_allocation"}));
		ctx.generate_blocks(1).await;
		out.line("training-wallets.jsonl", &json!({ "cycle":cycle,"wallet":training_name,
			"fixture_funding_txid":training_funding.to_string(),"fixture_funding_sat":5_000_000,
			"operator_injection_sat":0,"source":"existing Core central fixture wallet",
			"four_self_payment_amount_sat_each":100_000,"fee_sat_per_vb":3 }));
		let training_addresses = (0..4).map(|_| training_wallet.get_new_address(None, None)
			.unwrap().assume_checked()).collect::<Vec<_>>();
		// Four unchanged self-payments, transported as one Core JSON-RPC
		// batch. Their individual replies must all succeed before mining.
		// This avoids four serialized TCP round trips in every loaded block.
		let training_rpc = bitcoin_ext::rpc::Client::new(&training_url, env.bitcoind.auth()).unwrap();
		let training_params = training_addresses.iter().map(|destination| {
			bitcoin_ext::rpc::jsonrpc::arg(&vec![
				destination.to_string().into(), 0.001.into(), "".into(), "".into(),
				false.into(), true.into(), Value::Null, "unset".into(), Value::Null, 3.into(),
			])
		}).collect::<Vec<_>>();
		let miner = tokio::spawn({ let mining = mining.clone(); let core = env.core(); let out = out.clone(); async move {
			// Schedule the declared half-second cadence from tick starts. Sleeping
			// after RPC work adds its latency to every block and can leave coins
			// unexpired at the unchanged drain deadline. Skip missed ticks rather
			// than producing a catch-up burst.
			let mut cadence = tokio::time::interval(Duration::from_millis(500));
			cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
			while mining.load(Ordering::Relaxed) {
				cadence.tick().await;
				let started = Instant::now();
				let rpc = training_rpc.get_jsonrpc_client();
				let requests = training_params.iter().map(|params| rpc.build_request("sendtoaddress", Some(params))).collect::<Vec<_>>();
				let responses = rpc.send_batch(&requests).unwrap();
				assert_eq!(responses.len(), 4);
				for response in responses {
					let txid: Txid = response.expect("missing estimator-training reply").result().unwrap();
					out.line("chain-fee-intents.jsonl",&json!({"txid":txid,"category":"estimator_training","scheduled_sat_vb":3}));
				}
				let address = "mzU8XRVhUdXtdxmSA3Vw8XeU2FDV4iBDRW".parse::<Address<bitcoin::address::NetworkUnchecked>>().unwrap().assume_checked();
				core.generate_to_address(1, &address).unwrap();
				out.line("mining.jsonl", &json!({ "utc": utc(), "cycle": cycle,
					"height": core.get_block_count().unwrap(), "rpc_work_seconds": started.elapsed().as_secs_f64(),
					"configured_interval_seconds": 0.5, "training_transactions": 4 }));
			}
		}});
		let done = Arc::new(AtomicBool::new(false));
		let deadline = Instant::now() + Duration::from_secs(p.loaded_deadline_s);
		let paid = async {
			soak_wait_paid(&env, &all_owners, deadline).await;
			done.store(true, Ordering::Relaxed);
		};
		let refresh = run_refreshes(&env, &active, "soak-loaded", p.loaded_min_cycles, done.clone(), &errors, &p);
		tokio::join!(paid, refresh);
		mining.store(false, Ordering::Relaxed); miner.await.unwrap();
		let public_desc:Value=training_wallet.call("listdescriptors",&[false.into()]).unwrap();
		assert!(!public_desc.to_string().contains("prv"),"training descriptor must be public");
		let training_assets=training_wallet.list_unspent(Some(0),None,None,None,None).unwrap().iter()
			.map(|u|json!({"outpoint":OutPoint::new(u.txid,u.vout),"value_sat":u.amount.to_sat()})).collect::<Vec<_>>();
		out.line("training-wallet-assets.jsonl",&json!({"account":training_name,"utc":utc(),"public_descriptors":public_desc,"utxos":training_assets,"complete":true}));
		training_wallet.call::<serde_json::Value>("unloadwallet", &[]).unwrap();
		assert_eq!(errors.load(Ordering::Relaxed), 0, "an active user saw a refresh error");
		// Active clients now disappear too. Their last coins become explicit
		// obligations and must drain before the cycle closes.
		let mut active_expiry = env.core().get_block_count().unwrap() as u32;
		for (mut owner, wallet) in active_owners.into_iter().zip(active) {
			for coin in wallet.spendable_vtxos().await.unwrap() {
				let full = wallet.get_full_vtxo(coin.id()).await.unwrap();
				active_expiry = active_expiry.max(full.expiry_height().to_u32());
				owner.coins.push(Coin { id: full.id().to_string(), amount: full.amount().to_sat(),
					source: "active_then_absent", user_pubkey: full.user_pubkey().to_string() });
				owner.anchors.push(full);
			}
			assert!(!owner.coins.is_empty()); drop(wallet);
			out.line("owners.jsonl", &owner.public()); all_owners.push(owner);
		}
		ctx.generate_blocks(active_expiry.saturating_sub(env.core().get_block_count().unwrap() as u32) + 3).await;
		let deadline = Instant::now() + Duration::from_secs(180);
		loop {
			let check = tokio::time::timeout(Duration::from_secs(2), soak_wait_paid(&env, &all_owners, deadline)).await;
			if check.is_ok() { break; }
			assert!(Instant::now() < deadline, "final active-user obligations stuck");
			ctx.generate_blocks(1).await;
		}
		ctx.generate_blocks(2).await;
		srv.wait_for_sync_height(bitcoin_ext::BlockHeight::new(env.core().get_block_count().unwrap() as u32)).await;
		let reconciliation = reconcile(&env, &all_owners, enabled_utc).await;
		assert!(reconciliation["failures"].as_array().unwrap().is_empty(), "{reconciliation}");
		out.json(&format!("cycle-{cycle:02}-reconciliation.json"), &reconciliation);
		let after = treasury_edge(&env, &srv, &format!("cycle-{cycle:02}")).await;
		conserve_treasury(&env, &previous, &after, cycle); previous = after;
	}
	let reconciliation = reconcile(&env, &all_owners, enabled_utc).await;
	out.json("reconciliation.json", &reconciliation);
	srv.stop().await.unwrap();
	let recovery = recover(&ctx, &env, &all_owners, &Params::from_env()).await;
	assert!(recovery["failures"].as_array().unwrap().is_empty(), "{recovery}");
	assert_eq!(recovery["spends"].as_u64().unwrap(), all_owners.len() as u64);
	out.json("recovery.json", &recovery);
	srv.watchmand().stop().await.unwrap();
	let final_edge=treasury_edge_after_recovery(&env,&srv,"after-seed-recovery").await;
	conserve_treasury(&env,&previous,&final_edge,capital_cycles+mixed_cycles);
	let final_accounting=finalize_treasury_accounting(&env,&opening,&final_edge);
	ctx.bitcoind().stop().await.unwrap();
	out.json("soak-result.json", &json!({ "result": "UNQUALIFIED", "reason":"independent whole-boundary membership/accounting audit still required", "capital_cycles": capital_cycles,
		"mixed_cycles": mixed_cycles, "owners": all_owners.len(), "refresh_errors": errors.load(Ordering::Relaxed),
		"operator_injections_sat": 0, "closing_whole_assets_msat": final_edge.balance_msat, "whole_boundary_final_accounting":final_accounting,
		"confirmed_seed_spends": all_owners.len(), "policy_holds_not_inferred_from_absence":true,"minimum_holding_policy":true,"whole_treasury_raw_evidence":true }));
}



// The existing real scheduled/unclaimed journey, inside the same soak treasury.
// Original and replacement are one economic group; no synthetic sixth-source label.
// A source contract: expiry_payout.rs475-535 admits replacement only after every
// predecessor path is swept. cancel_failed_expiry_refresh559-604 requires an
// actual exited original; this fixture never starts an exit. Unexpected original
// settlement is rejected by the complete A reconciliation, not silently adopted.
async fn soak_hark_owner(ctx: &TestContext, srv: &Captaind, env: &Env, index: usize, cycle: usize) -> Owner {
	let (mut owner,wallet)=create_owner(env.clone(),index,Profile::Registered,srv.ark_url()).await;
	owner.cohort="hark";
	let board=env.board(&wallet,300_000).await.unwrap();ctx.await_transaction(board.funding_tx.compute_txid()).await;
	ctx.generate_blocks(BOARD_CONFIRMATIONS).await;
	let input=wait_spendable(&wallet,1,Duration::from_secs(120)).await.unwrap().remove(0);
	let original=wallet.get_full_vtxo(input.id()).await.unwrap();
	let scheduled=ctx.bitcoind().get_block_count().await as u32+10;
	let round=wallet.refresh_vtxos_scheduled(vec![input.id()],bitcoin_ext::BlockHeight::new(scheduled)).await.unwrap().expect("actual scheduled hArk participation");
	assert!(round.state().unlock_hash().is_some());
	let accepted_input=round.state().participation().total_in().unwrap().to_sat();
	let accepted_output=round.state().participation().total_out().unwrap().to_sat();
	assert_eq!(accepted_input,original.amount().to_sat());drop(round);drop(wallet);
	env.out.event("hark-owner-absent",json!({"owner":index,"cycle":cycle,"scheduled_height":scheduled,"before_forfeits":true}));
	let tip=ctx.bitcoind().get_block_count().await as u32;ctx.generate_blocks(scheduled.saturating_sub(tip)).await;
	let mut finished=srv.subscribe_log::<server_log::RoundFinished>();srv.trigger_round().await;
	finished.recv().wait(Duration::from_secs(120)).await.expect("real scheduled round completes while owner absent");ctx.generate_blocks(6).await;
	// policy is protocol-encoded; use the same actual full-VTXO key extraction as
	// the existing fixture rather than assuming SQL policy bytes equal a pubkey.
	let mut matched=Vec::new();
	let candidates=env.db.read(async |t|Ok(t.query("SELECT vtxo FROM vtxo WHERE spend_state='unclaimed' AND policy_type='pubkey'",&[]).await?)).await.unwrap();
	for row in candidates {
		let coin=Vtxo::deserialize(&row.get::<_,Vec<u8>>(0)).unwrap();let key=coin.user_pubkey().serialize().to_vec();
		let binding=env.db.read(async |t|Ok(t.query_opt("SELECT mailbox_pk FROM key_link WHERE user_pubkey=$1",&[&key]).await?)).await.unwrap();
		if binding.is_some_and(|r|r.get::<_,Vec<u8>>(0)==owner.mailbox_pk.parse::<PublicKey>().unwrap().serialize().to_vec()) {
			matched.push(coin.id().to_string());owner.coins.push(Coin{id:coin.id().to_string(),amount:coin.amount().to_sat(),source:"hark_unclaimed",user_pubkey:coin.user_pubkey().to_string()});owner.anchors.push(coin);
		}
	}
	assert!(!matched.is_empty(),"actual scheduled hArk must have genuine unclaimed user leaves");
	let replacement=owner.coins.iter().map(|c|c.amount).sum::<u64>();assert_eq!(replacement,accepted_output,"replacement preserves actual accepted participation output budget");
	env.out.line("hark-economic-groups.jsonl",&json!({"cycle":cycle,"owner":index,"economic_id":original.id(),"original_id":original.id(),
		"replacement_ids":matched,"original_sat":original.amount().to_sat(),"replacement_sat":replacement,"accepted_input_sat":accepted_input,"accepted_output_sat":accepted_output,"customer_consented_protocol_delta_sat":accepted_input.checked_sub(accepted_output).unwrap(),"protocol_delta_is_not_miner_cost":true,
		"original_and_replacement_not_separate_rights":true,"user_absent_before_forfeits":true,"qualified":false}));
	owner.anchors.push(original);env.out.line("owners.jsonl",&owner.public());owner
}
