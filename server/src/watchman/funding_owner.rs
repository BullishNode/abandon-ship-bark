//! SAME Watchman/FLOCK owner for early native funding. RPC authentication does
//! not substitute for complete actual Core/own-descriptor and urgent-cash proof.
use std::collections::BTreeSet;
use std::sync::{Arc,atomic::{AtomicBool,Ordering}};
use std::time::Duration;
use anyhow::Context;
use bitcoin::{FeeRate,Network,OutPoint,Transaction,PublicKey};
use bitcoin::secp256k1::Keypair;
use bitcoin::hashes::{sha256,Hash};
use bitcoin::consensus::deserialize;
use bitcoind_async_client::{Client,traits::Reader};
use bitcoin_ext::rpc::BitcoinAsyncRpcExt;
use crate::database::Db;
use crate::utils::InstrumentedLock;
use crate::wallet::{PersistedWallet,WalletKind};
use super::ordinary_fee::OrdinaryFeeBudget;
use super::ordinary_funding::{FundingRequest,FundingReservation};

pub(crate) struct FundingOwner {
	db:Db,core:Client,budget:Arc<OrdinaryFeeBudget>,wallet:InstrumentedLock<PersistedWallet>,
	key:crate::secret::Secret<Keypair>,server:PublicKey,genesis:bitcoin::BlockHash,descriptor:sha256::Hash,max_rate:FeeRate,
	runtime:crate::system::RuntimeManager,requests:Arc<tokio::sync::Semaphore>,
}
impl FundingOwner {
	pub(crate) async fn new(db:Db,core:Client,budget:Arc<OrdinaryFeeBudget>,wallet:InstrumentedLock<PersistedWallet>,
		key:Keypair,network:Network,master:&bitcoin::bip32::Xpriv,max_rate:FeeRate,runtime:crate::system::RuntimeManager,
	)->anyhow::Result<Self> {
		let server=key.public_key();
		let rounds_key=master.derive_priv(&crate::SECP,&[WalletKind::Rounds.child_number()])?;
		let public=bitcoin::bip32::Xpub::from_priv(&crate::SECP,&rounds_key);
		let view=bdk_wallet::Wallet::create_single(format!("tr({public}/0/*)")).network(network).create_wallet_no_persist()?;
		let descriptor=sha256::Hash::hash(view.public_descriptor(bdk_wallet::KeychainKind::External).to_string().as_bytes());
		ensure!(max_rate>FeeRate::ZERO,"native funding owner has no actual rate ceiling");
		let history=budget.funding_states()?;
		db.write(async |t|t.require_native_funding_history(&history).await).await?;
		Ok(Self {db,core,budget,wallet,key:crate::secret::Secret::new(key),server,genesis:bitcoin::constants::genesis_block(network).block_hash(),descriptor,max_rate,runtime,requests:Arc::new(tokio::sync::Semaphore::new(4))})
	}
	pub(crate) async fn execute(self:Arc<Self>,raw:Vec<u8>,bind:bool)->anyhow::Result<Vec<u8>> {
		ensure!(!self.runtime.shutdown_requested() && raw.len()<=2*1024*1024,"native funding owner unavailable or request too large");
		let permit=self.requests.clone().try_acquire_owned().context("native funding owner request bound")?;
		let worker=self.runtime.spawn("NativeFundingOwner");
		// A disconnected caller cannot cancel fsync/SQL COMMIT or release ownership.
		tokio::spawn(async move {let _permit=permit;let _worker=worker;let reservation=self.execute_original(&raw,bind).await?;super::ordinary_funding::sign_reply(reservation,self.key.leak_ref(),self.genesis)}).await.context("native funding owner interrupted; original intent retained")?
	}
	async fn execute_original(&self,raw:&[u8],bind:bool)->anyhow::Result<FundingReservation> {
		let request=FundingRequest::authenticate(raw,bind,self.server,self.genesis,self.budget.identity(),self.budget.opening_digest()?)?;
		let wallet=self.wallet.lock().await;
		ensure!(wallet.kind()==WalletKind::Watchman,"native fee owner wallet differs");
		let old=self.budget.funding_states()?;
		let retained=old.iter().find(|s|s.reservation.candidate.key()==request.candidate.key());
		if let Some(state)=retained {ensure!(state.reservation.candidate==request.candidate,"native funding retry changed body");}
		// Fresh source observations run before any global financial SQL lock.
		if retained.is_none() {
			tokio::time::timeout(Duration::from_secs(15),self.check_sources(&wallet,&request.candidate)).await.context("native funding source deadline before reservation")??;
		}
		let debit_attempted=AtomicBool::new(false);
		let result=self.db.write(async |t| {
			t.batch_execute("SET LOCAL statement_timeout='8s'; SET LOCAL lock_timeout='5s';").await?;
			t.require_native_funding_history(&old).await?;
			let reservation=if let Some(state)=retained {state.reservation.clone()} else {
				debit_attempted.store(true,Ordering::SeqCst);
				self.budget.reserve_funding(request.candidate.clone(),self.max_rate)?
			};
			t.stage_native_funding(&reservation).await?;
			if let Some(raw)=&request.signed {
				let signed:Transaction=deserialize(raw)?;
				debit_attempted.store(true,Ordering::SeqCst);
				self.budget.bind_funding(&reservation,&signed)?;
				t.bind_native_funding(&reservation,&signed).await?;
			}
			Ok(reservation)
		}).await;
		if result.is_err() && debit_attempted.load(Ordering::SeqCst) {self.budget.mark_native_uncertain()?;}
		drop(wallet);result.context("native funding outcome uncertain; retain original input intents")
	}
	async fn check_sources(&self,wallet:&PersistedWallet,candidate:&super::ordinary_funding::FundingCandidate)->anyhow::Result<()> {
		candidate.require_rate(self.max_rate)?;
		let tx=candidate.transaction()?;let tip=crate::bitcoind::tip(&self.core).await?;
		let inventory=wallet.inventory()?;
		ensure!(inventory.wallet=="watchman" && inventory.complete && inventory.unavailable_parent_txids.is_empty()
			&& inventory.utxos.len()<=2000 && inventory.tip_height==tip.height.to_u32() && inventory.tip_hash==tip.hash.to_string()
			&& self.core.get_block_hash(0).await?==self.genesis,"native funding urgent source is not actual complete owning tip");
		let mut urgent=0u64;let mut seen=BTreeSet::new();let mut protected=BTreeSet::<OutPoint>::new();
		for output in &inventory.utxos {
			ensure!(seen.insert(output.outpoint.clone()),"duplicate urgent owning source");
			if !output.confirmed_fee_eligible {continue;}
			ensure!(output.trusted && output.mature && !output.reserved && output.confirmations>=inventory.min_trusted_confirmations
				&& !inventory.reserved_outpoints.contains(&output.outpoint),"urgent input is not actually eligible");
			let point:OutPoint=output.outpoint.parse()?;
			let actual=self.core.try_get_tx_out(point,true).await?.context("urgent owning input spent or missing")?;
			ensure!(actual.bestblock==tip.hash && actual.value.to_sat()==output.value_sat && actual.script_pub_key.script()?.as_bytes()==output.script_pubkey
				&& actual.confirmations>0 && actual.confirmations>=inventory.min_trusted_confirmations
				&& (!actual.coinbase || actual.confirmations>=bitcoin::constants::COINBASE_MATURITY),"urgent owning input differs from actual Core");
			urgent=urgent.checked_add(output.value_sat).context("urgent source sum overflow")?;protected.insert(point);
		}
		ensure!(urgent>=self.budget.urgent_reserve()?.to_sat() && tx.input.iter().all(|i|!protected.contains(&i.previous_output)
			&& !inventory.reserved_outpoints.contains(&i.previous_output.to_string())),"native funding lacks independent whole-input urgent liquidity");
		let state=self.db.read(async |t|t.read_aggregate_changeset(WalletKind::Rounds).await).await?.context("actual rounds source missing")?;
		ensure!(state.descriptor.as_ref().map(|d|sha256::Hash::hash(d.to_string().as_bytes()))==Some(self.descriptor),"native funding actual seed descriptor differs");
		let network=state.network.context("rounds source network missing")?;ensure!(bitcoin::constants::genesis_block(network).block_hash()==self.genesis,"rounds source chain differs");
		let view=bdk_wallet::Wallet::load().check_network(network).load_wallet_no_persist(state)?.context("actual rounds graph unavailable")?;
		for (input,expected) in tx.input.iter().zip(&candidate.prevouts) {
			ensure!(view.is_mine(expected.script_pubkey.clone()),"native funding spends a foreign descriptor input");
			let actual=self.core.try_get_tx_out(input.previous_output,true).await?.context("native funding input already spent or missing")?;
			ensure!(actual.bestblock==tip.hash && actual.value==expected.value && actual.script_pub_key.script()?==expected.script_pubkey,"native funding complete prevout differs from Core");
		}
		ensure!(crate::bitcoind::tip(&self.core).await?==tip,"native funding source crossed actual Core tip");Ok(())
	}
}
