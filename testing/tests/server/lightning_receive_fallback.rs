//! A registered receive belongs to its recipient even when the forwarding
//! provider stops before mailbox delivery. This uses the native receive path;
//! stock Noah's LNURL callback and pre-registration interruption are separate.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ark::lightning::PaymentHash;
use ark_testing::{sat, TestContext};
use ark_testing::daemon::captaind::{proxy::MailboxRpcProxy, MailboxClient};
use bark::FallbackDisposition;
use bark::actions::lightning::receive::Progress;
use bark::movement::MovementStatus;
use bitcoin::{Address, Amount, Network, OutPoint, Transaction};
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::consensus::deserialize;
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::Secp256k1;
use bitcoin_ext::BlockDelta;
use bitcoin_ext::rpc::{BitcoinRpcExt, RpcApi};
use server::database::{Db, SpendState};
use server::vtxopool::VtxoTarget;
use server_rpc::protos;

#[derive(Clone, Default)]
struct OfflineRecipient(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl MailboxRpcProxy for OfflineRecipient {
	async fn post_arkoor_message(&self, _upstream: &mut MailboxClient, _request: protos::mailbox_server::PostArkoorMessageRequest)
		-> Result<protos::core::Empty, tonic::Status>
	{
		self.0.fetch_add(1, Ordering::SeqCst);
		// Never call upstream: even one successful post would cross the
		// interruption boundary this case is intended to qualify.
		Err(tonic::Status::unavailable("owned recipient mailbox remains offline"))
	}
}

#[tokio::test]
async fn registered_forwarded_receive_pays_absent_recipient_after_provider_stops() {
	tokio::time::timeout(Duration::from_secs(240), registered_receive_fallback()).await
		.expect("registered receive fallback and seed-only recovery must finish within 240 seconds");
}

async fn registered_receive_fallback() {
	let ctx = TestContext::new("server/registered_receive_fallback").await;
	let lightning = ctx.new_lightning_setup("lightningd").await;
	let srv = ctx.captaind("server").lightningd(&lightning.internal).funded(sat(500_000))
		.cfg(|config| {
			config.vtxo_lifetime = BlockDelta::new(32);
			config.vtxopool.vtxo_lifetime = BlockDelta::new(32);
			config.vtxopool.vtxo_targets = vec![VtxoTarget { count: 2, amount: sat(100_000) }];
			config.ln_receive_anti_dos_required = false;
		}).watchmand().create().await;
	srv.wait_for_vtxopool(&ctx).await;
	// Pool readiness only observes the issuance in the mempool. Confirm that
	// backing and the separate watchman fee funds before making the invoice.
	let fee_address = srv.watchmand().wait_wallet_address().await;
	let mut fee_funding = Vec::new();
	for _ in 0..2 { fee_funding.push(ctx.bitcoind().fund_addr(fee_address.clone(), sat(100_000)).await); }
	let funded = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(funded).await;
	srv.watchmand().wait_for_sync_height(funded).await;
	for txid in fee_funding {
		let transaction = ctx.bitcoind().await_transaction(txid).await;
		let vout = transaction.output.iter().position(|output|
			output.script_pubkey == fee_address.script_pubkey() && output.value == sat(100_000)).unwrap();
		let output = ctx.bitcoind().sync_client().get_tx_out(&txid, vout as u32, Some(true)).unwrap().unwrap();
		assert!(output.confirmations >= 6);
		assert_eq!(output.value, sat(100_000));
	}
	let outage = OfflineRecipient::default();
	let proxy = srv.start_proxy_with_mailbox((), outage.clone()).await;
	let provider = ctx.bark_sdk("provider", &proxy).cfg(|config| config.daemon_manual_sync = true).create().await;
	provider.stop_daemon_wait().await.unwrap();
	let provider_destination = provider.fallback_destination().await.unwrap();
	let recipient = ctx.bark_sdk("recipient", &srv).cfg(|config| config.daemon_manual_sync = true).create().await;
	recipient.stop_daemon_wait().await.unwrap();
	let address = recipient.new_address().await.unwrap();
	let destination = recipient.fallback_destination().await.unwrap();
	let recipient_mailbox = recipient.mailbox_keypair().public_key().to_string();
	assert_ne!(destination.account_xpub, provider_destination.account_xpub);
	assert_ne!(destination.spk, provider_destination.spk);
	let seed = bip39::Mnemonic::from_str(tokio::fs::read_to_string(ctx.datadir.join("recipient/mnemonic"))
		.await.unwrap().trim()).unwrap();
	let secp = Secp256k1::new();
	let master = Xpriv::new_master(Network::Regtest, &seed.to_seed("")).unwrap();
	let path = DerivationPath::from_str("m/86'/1'/0'/0/0").unwrap();
	let fallback_key = master.derive_priv(&secp, &path).unwrap().to_keypair(&secp);
	let expected_fallback_script = Address::p2tr(
		&secp, fallback_key.x_only_public_key().0, None, Network::Regtest,
	).script_pubkey();
	assert_eq!(destination.spk, expected_fallback_script, "the native fallback must use the seed-derived BIP86 destination");
	assert!(destination.spk.is_p2tr());
	assert_eq!(recipient.balance().await.unwrap().spendable, sat(0));
	drop(recipient);
	// The original recipient is never reopened, synced, delivered to or
	// refreshed. A fresh seed-only wallet is allowed only after Core confirms.
	let amount = sat(10_000);
	let receive_fee = srv.ark_info().await.fees.lightning_receive.calculate(amount).unwrap();
	assert_eq!(receive_fee, sat(0), "this bounded fixture declares the actual native zero receive fee");
	let invoice = provider.bolt11_invoice_for_address(amount, address.clone(),
		Some("registered receive rightful fallback".into()), None).await.unwrap();
	let hash = PaymentHash::from(&invoice);
	let (payment, delivery) = tokio::join!(
		lightning.external.try_pay_bolt11(invoice.to_string()),
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let _ = provider.try_claim_lightning_receive(hash, false).await;
				if let Some(checkpoint) = provider.lightning_receive_checkpoint(hash).await.unwrap()
					&& let Progress::Delivering(delivery) = checkpoint.progress { break delivery; }
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}),
	);
	payment.expect("the original external Lightning payment must actually settle");
	let delivery = delivery.expect("receive must durably reach registered Delivering before provider stops");
	assert!(outage.0.load(Ordering::SeqCst) > 0);
	assert_eq!(delivery.vtxos.len(), 1, "one pool input must produce one 10k recipient output in this fixture");
	let source = delivery.vtxos[0].clone();
	assert_eq!(source.amount(), amount - receive_fee);
	assert_eq!(source.policy(), address.policy());
	provider.validate_vtxo(&source).await.unwrap();
	assert!(provider.pubkey_keypair(&source.policy().user_pubkey()).await.unwrap().is_none());
	let db = Db::connect(&srv.config().postgres).await.unwrap();
	let registered = db.read(async |tx| tx.get_user_vtxo_by_id(source.id()).await).await.unwrap();
	assert_eq!(registered.vtxo.id(), source.id());
	assert_eq!(registered.vtxo.policy(), address.policy());
	assert_eq!(registered.vtxo.amount(), source.amount());
	assert_eq!(registered.spend_state, SpendState::Spendable);
	let anchor = source.chain_anchor();
	assert!(ctx.bitcoind().sync_client().get_tx_out(&anchor.txid, anchor.vout, Some(true))
		.unwrap().unwrap().confirmations >= 6, "the receive's actual pool backing must be confirmed");
	let ancestor_deadline = srv.htlc_vtxos(hash).await.iter().map(|grant| grant.htlc.htlc_expiry)
		.max().expect("a genuine receive must retain its actual HTLC grant");
	let height = ctx.bitcoind().get_block_count().await as u32;
	assert!(source.expiry_height().to_u32() > height + provider.config().vtxo_exit_margin.to_u32());
	assert_eq!(provider.balance().await.unwrap().spendable, sat(0));
	assert_eq!(provider.claimable_lightning_receive_balance().await.unwrap(), sat(0));
	drop(provider);
	// The provider is fenced forever at Delivering. No later client action
	// can supply mailbox delivery, registration, cancellation or a refund.
	let stopped_posts = outage.0.load(Ordering::SeqCst);
	let root_ids = vec![source.chain_anchor().to_string()];
	let source_id = source.id().to_string();
	let expired = ctx.generate_blocks(source.expiry_height().to_u32() + 1 - height).await;
	srv.wait_for_sync_height(expired).await;
	srv.watchmand().wait_for_sync_height(expired).await;
	let parent = tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let rows = db.read(async |tx| Ok(tx.query("SELECT a.sweep_tx FROM expiry_allocation a \
				JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
				WHERE e.vtxo_id=$1 AND c.active AND c.disposition='customer' AND a.broadcast_at IS NOT NULL",
				&[&source_id],
			).await?)).await.unwrap();
			assert!(rows.len() <= 1, "one registered recipient coin cannot have duplicate active payouts");
			if let Some(row) = rows.first() { break deserialize::<Transaction>(row.get("sweep_tx")).unwrap(); }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("native expiry path must pay the absent recipient without its forwarding provider");
	assert_eq!(ctx.bitcoind().await_transaction(parent.compute_txid()).await, parent);
	let outputs = parent.output.iter().enumerate().filter(|(_, output)| output.script_pubkey == destination.spk)
		.collect::<Vec<_>>();
	assert_eq!(outputs.len(), 1);
	let (vout, output) = outputs[0];
	let expected = source.amount() - sat(1_000);
	assert_eq!(output.value, expected);
	let payout = OutPoint::new(parent.compute_txid(), vout as u32);
	let mut input_value = Amount::ZERO;
	for input in &parent.input {
		let previous = ctx.bitcoind().await_transaction(input.previous_output.txid).await;
		input_value += previous.output[input.previous_output.vout as usize].value;
	}
	assert_eq!(input_value, parent.output.iter().map(|output| output.value).sum::<Amount>(),
		"the actual payout parent conserves operator remainder and customer value without a hidden miner debit");
	let confirmed = ctx.generate_blocks(6).await;
	srv.wait_for_sync_height(confirmed).await;
	srv.watchmand().wait_for_sync_height(confirmed).await;
	let actual = ctx.bitcoind().sync_client().get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().unwrap();
	assert!(actual.confirmations >= 6);
	assert_eq!(actual.value, expected);
	assert_eq!(actual.script_pub_key.script().unwrap(), destination.spk);
	tokio::time::timeout(Duration::from_secs(30), async {
		loop {
			let paid = db.read(async |tx| Ok(tx.query_one("SELECT COUNT(*) FROM expiry_allocation \
				WHERE sweep_txid=$1 AND status='confirmed'", &[&payout.txid.to_string()],
			).await?.get::<_, i64>(0))).await.unwrap();
			if paid == 1 { break; }
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	}).await.expect("Core-confirmed recipient payout must reconcile in the native ledger");
	// Examine the WHOLE root, including predecessor HTLCs and pool change.
	// An extra refund to the provider cannot hide behind the correct leaf.
	for observation in 0..2 {
		if observation == 1 {
			// The original HTLC can outlive the backing tree. Also observe
			// beyond that REAL deadline so a late ancestor/provider refund
			// cannot coexist with the rightful registered-leaf payment.
			let height = ctx.bitcoind().get_block_count().await as u32;
			let tip = ctx.generate_blocks((ancestor_deadline.to_u32() + 1).max(height + 1) - height).await;
			srv.wait_for_sync_height(tip).await;
			srv.watchmand().wait_for_sync_height(tip).await;
		}
		let point = server::bitcoind::tip(&ctx.bitcoind().async_client()).await.unwrap();
		tokio::time::timeout(Duration::from_secs(30), async {
			loop {
				let rebuilt = db.read(async |tx| {
					let Some(view) = tx.expiry_rebuild_token().await? else { return Ok(false); };
					Ok(view.captaind == point && view.watchmand == point && tx.expiry_view_is_rebuilt(view).await?)
				}).await.unwrap();
				if rebuilt { break; }
				tokio::time::sleep(Duration::from_millis(50)).await;
			}
		}).await.expect("both real daemon views and their financial ledger must finish rebuilding this Core tip");
		let rows = db.read(async |tx| Ok(tx.query("SELECT e.vtxo_id,c.owner_mailbox,c.user_pubkey,c.spk, \
			c.gross_sat,c.fee_sat,c.payout_sat,c.forfeited_sat,c.active,a.sweep_txid,a.status FROM expiry_allocation_coin c \
			JOIN expiry_allocation a ON a.id=c.allocation_id \
			JOIN expiry_entitlement e ON e.id=c.entitlement_id JOIN vtxo v ON v.vtxo_id=e.vtxo_id \
			WHERE v.anchor_point=ANY($1) AND c.disposition='customer'", &[&root_ids],
		).await?)).await.unwrap();
		assert_eq!(rows.len(), 1, "only the registered recipient leaf may receive customer money under this root");
		let row = &rows[0];
		assert_eq!(row.get::<_, String>("vtxo_id"), source_id);
		assert_eq!(row.get::<_, String>("owner_mailbox"), recipient_mailbox);
		assert_eq!(row.get::<_, String>("user_pubkey"), source.policy().user_pubkey().to_string());
		assert_eq!(row.get::<_, String>("spk"), destination.spk.as_bytes().to_lower_hex_string());
		assert_eq!(row.get::<_, i64>("gross_sat"), source.amount().to_sat() as i64);
		assert_eq!(row.get::<_, i64>("fee_sat"), 1_000);
		assert_eq!(row.get::<_, i64>("payout_sat"), expected.to_sat() as i64);
		assert_eq!(row.get::<_, i64>("forfeited_sat"), 0);
		assert!(row.get::<_, bool>("active"));
		assert_eq!(row.get::<_, String>("status"), "confirmed");
		assert_eq!(row.get::<_, Option<String>>("sweep_txid"), Some(payout.txid.to_string()));
		let allocations = db.read(async |tx| Ok(tx.query("SELECT DISTINCT a.sweep_tx FROM expiry_allocation a \
			JOIN expiry_allocation_coin c ON c.allocation_id=a.id JOIN expiry_entitlement e ON e.id=c.entitlement_id \
			JOIN vtxo v ON v.vtxo_id=e.vtxo_id WHERE v.anchor_point=ANY($1) AND a.sweep_tx IS NOT NULL",
			&[&root_ids],
		).await?)).await.unwrap();
		let mut customer_outputs = BTreeMap::new();
		for allocation in allocations {
			let recorded = deserialize::<Transaction>(allocation.get("sweep_tx")).unwrap();
			let txid = recorded.compute_txid();
			let Some(info) = ctx.bitcoind().sync_client().custom_get_raw_transaction_info(txid, None).unwrap() else { continue; };
			if info.confirmations.unwrap_or(0) == 0 { continue; }
			assert!(info.confirmations.unwrap() >= 6);
			assert_eq!(ctx.bitcoind().await_transaction(txid).await, recorded);
			for (index, output) in recorded.output.iter().enumerate() {
				assert_ne!(output.script_pubkey, provider_destination.spk, "a registered recipient receive is never a provider refund");
				if output.script_pubkey == destination.spk {
					assert!(customer_outputs.insert(OutPoint::new(txid, index as u32), output.value).is_none());
				}
			}
		}
		assert_eq!(customer_outputs, BTreeMap::from([(payout, expected)]));
		assert_eq!(customer_outputs.values().copied().sum::<Amount>() + sat(customer_outputs.len() as u64 * 1_000), source.amount());
	}
	assert_eq!(outage.0.load(Ordering::SeqCst), stopped_posts, "no stopped provider may drive another delivery attempt");
	// First allowed recovery: a NEW database gets only the original recipient
	// seed and genesis birthday, never its unseen VTXO or payout transaction.
	let restored = ctx.bark("recipient-seed-only", &srv).mnemonic(seed).create().await;
	let client = restored.client().await;
	assert_eq!(client.fallback_destination().await.unwrap(), destination);
	client.sync_fallback_payouts().await.unwrap();
	let receipts = client.fallback_payouts().await.unwrap();
	assert_eq!(receipts.len(), 1);
	assert_eq!(receipts[0].vtxo_id, source.id());
	assert_eq!(receipts[0].gross, source.amount());
	let attempts = receipts[0].attempts.iter().filter(|attempt| attempt.active).collect::<Vec<_>>();
	assert_eq!(attempts.len(), 1);
	assert_eq!(attempts[0].disposition, FallbackDisposition::CustomerPayout);
	assert_eq!(attempts[0].parent.as_ref().unwrap(), &parent);
	assert_eq!(attempts[0].output_index, Some(payout.vout));
	assert_eq!(attempts[0].script.as_ref(), Some(&destination.spk));
	assert_eq!(attempts[0].payout, expected);
	assert_eq!(attempts[0].fee, sat(1_000));
	let history = client.history().await.unwrap();
	let movements = history.iter().filter(|movement| movement.subsystem.name == "abandon.fallback_payout").collect::<Vec<_>>();
	assert_eq!(movements.len(), 1);
	assert_eq!(movements[0].status, MovementStatus::Successful);
	assert_eq!(movements[0].metadata["fallback"]["chain_verified"], true);
	assert_eq!(movements[0].effective_balance.to_sat(), 0, "seed-only recovery never held the undelivered Ark leaf");
	assert_eq!(movements[0].sent_to[0].amount, expected);
	assert_eq!(movements[0].offchain_fee, sat(1_000));
	assert!(!history.iter().any(|movement| movement.effective_balance.to_sat() < 0 && movement.input_vtxos.contains(&source.id())));
	drop(client);
	let utxos = restored.onchain_utxos().await;
	assert_eq!(utxos.len(), 1, "the recipient has no unrelated funding or ordinary change");
	assert_eq!(utxos[0].outpoint, payout);
	assert_eq!(utxos[0].amount, expected);
	let recovery_address = ctx.bitcoind().get_new_address();
	restored.onchain_drain(&recovery_address).await;
	let mut recoveries = Vec::new();
	for txid in ctx.bitcoind().sync_client().get_raw_mempool().unwrap() {
		let transaction = ctx.bitcoind().await_transaction(txid).await;
		if transaction.input.iter().any(|input| input.previous_output == payout) { recoveries.push(transaction); }
	}
	assert_eq!(recoveries.len(), 1);
	let recovery = &recoveries[0];
	assert_eq!(recovery.input.iter().map(|input| input.previous_output).collect::<BTreeSet<_>>(), BTreeSet::from([payout]));
	assert_eq!(recovery.input.len(), 1);
	assert_eq!(recovery.input[0].witness.len(), 1, "the native BIP86 recipient must use a key-path witness");
	bitcoin::taproot::Signature::from_slice(recovery.input[0].witness.iter().next().unwrap()).unwrap();
	let previous = ctx.bitcoind().await_transaction(recovery.input[0].previous_output.txid).await;
	let previous_output = &previous.output[recovery.input[0].previous_output.vout as usize];
	assert!(previous_output.script_pubkey.is_p2tr());
	assert_eq!(previous_output.script_pubkey, expected_fallback_script);
	assert_eq!(previous_output.value, expected);
	assert_eq!(recovery.output.len(), 1);
	assert_eq!(recovery.output[0].script_pubkey, recovery_address.script_pubkey());
	let recovery_fee = expected.checked_sub(recovery.output[0].value).unwrap();
	assert!(recovery_fee > Amount::ZERO);
	ctx.generate_blocks(6).await;
	let recovered = ctx.bitcoind().sync_client().get_tx_out(&recovery.compute_txid(), 0, Some(true)).unwrap().unwrap();
	assert!(recovered.confirmations >= 6);
	assert_eq!(recovered.script_pub_key.script().unwrap(), recovery_address.script_pubkey());
	assert_eq!(recovered.value, recovery.output[0].value);
	assert_eq!(ctx.bitcoind().get_received_by_address(&recovery_address), recovered.value);
	assert_eq!(recovered.value + recovery_fee + sat(1_000), source.amount());
	assert!(ctx.bitcoind().sync_client().get_tx_out(&payout.txid, payout.vout, Some(true)).unwrap().is_none());
	assert_eq!(ctx.bitcoind().await_transaction(recovery.compute_txid()).await, *recovery);
}
