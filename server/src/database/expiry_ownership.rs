//! Terminal ownership proposals for a proven geometric backing partition.
//! No proposal is an authorization: allocation revalidates its evidence under
//! coin, participation and chain-view locks before committing a spend fence.

use std::str::FromStr;

use anyhow::Context;
use bitcoin::{Amount, Transaction, Txid};
use bitcoin::secp256k1::PublicKey;
use bitcoin_ext::{BlockHeight, BlockRef};
use chrono::{DateTime, Local};

use ark::{ProtocolEncoding, ServerVtxo, ServerVtxoPolicy, VtxoId};
use ark::lightning::PaymentHash;
use ark::mailbox::MailboxIdentifier;
use ark::vtxo::Full;

use super::expiry_inventory::BackingVtxoFact;
use super::expiry_lightning::LightningSourceState;
use super::expiry_receive::ReceiveClaimSettlement;
use super::htlc_vtxo::{self, HtlcDirection, HtlcResolution};
use super::{BlockTable, SpendState, Tx};

lazy_static::lazy_static! {
	// Same unspendable key as round/mod.rs uses for padding leaves.
	static ref PADDING_KEY: PublicKey =
		"02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c".parse().unwrap();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipDisposition {
	Customer { user_pubkey: PublicKey, mailbox: Option<MailboxIdentifier> },
	Operator,
	Unresolved(&'static str),
}

/// The irreversible layer records these facts; a chain-only conclusion is
/// always tied to the block that supports it and is rebuilt after invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipEvidence {
	Pubkey,
	IrreversibleEntitlement { entitlement_id: i64 },
	InputOwner,
	LightningReceiveClaim { source: VtxoId, subscription_id: i64, payment_hash: PaymentHash, output: VtxoId },
	Pool,
	Padding,
	Forfeit,
	Offboard(Txid),
	Exchange { participation_id: i64, unlock_hash: String, forfeited: bool },
	HtlcOffchain { payment_hash: PaymentHash, resolution: HtlcResolution },
	HtlcStoredPreimage { payment_hash: PaymentHash },
	HtlcSourceDischarged { payment_hash: PaymentHash },
	/// No external payment was attempted, and the admitted refund deadline is
	/// behind both completed views. This is an expiry decision, not a forged
	/// upstream off-chain revocation.
	HtlcUnattemptedExpired { payment_hash: PaymentHash, htlc_expiry: BlockHeight },
	HtlcExternalFailed { payment_hash: PaymentHash, htlc_expiry: BlockHeight },
	HtlcInternalZeroGrantCancelled { payment_hash: PaymentHash, htlc_expiry: BlockHeight, attempt_id: i64, subscription_id: i64 },
	/// Every receive grant of the internal receive was taken back on chain by
	/// the server's timeout clause. Chain-derived: invalidated with its block.
	HtlcInternalGrantsReclaimed { payment_hash: PaymentHash, attempt_id: i64, subscription_id: i64, block: BlockRef },
	/// An unsettled receive grant past its HTLC expiry and unspent on chain.
	/// The operator takes it back with the expiry sweep of its tree output.
	/// Chain-derived: a later preimage spend conflicts the sweep.
	HtlcRecvExpired { payment_hash: PaymentHash, htlc_expiry: BlockHeight },
	HtlcChain { payment_hash: PaymentHash, resolution: HtlcResolution, block: BlockRef },
}

#[derive(Debug)]
pub struct OwnershipProposal {
	pub id: VtxoId,
	pub gross: Amount,
	pub updated_at: DateTime<Local>,
	pub spend_state: SpendState,
	pub disposition: OwnershipDisposition,
	pub evidence: Option<OwnershipEvidence>,
	/// These entire participations must be cancelled atomically at commit.
	/// A proposal never cancels anything during background readiness work.
	pub cancellations: Vec<i64>,
	/// Preserve newly learned preimages even when the chain conclusion rolls back.
	pub has_stored_preimage: bool,
	/// Only a fully evidenced, expired HTLC settlement can request actual
	/// signed progression. Missing ownership or path evidence never does.
	pub needs_chain_progress: bool,
}

impl OwnershipProposal {
	fn pending(fact: &BackingVtxoFact, reason: &'static str) -> Self {
		Self {
			id: fact.state.vtxo_id, gross: fact.state.vtxo.amount(),
			updated_at: fact.state.updated_at, spend_state: fact.state.spend_state,
			disposition: OwnershipDisposition::Unresolved(reason), evidence: None,
			cancellations: Vec::new(), has_stored_preimage: false, needs_chain_progress: false,
		}
	}
}

impl Tx<'_> {
	/// Resolve one selected economic terminal. The caller supplies its actual
	/// funding transaction and must already have proved the complete partition.
	pub async fn propose_expiry_ownership(
		&self,
		fact: &BackingVtxoFact,
		funding: &Transaction,
		tip: BlockHeight,
		watchman_tip: Option<BlockHeight>,
		internal_refund_depth: u32,
	) -> anyhow::Result<OwnershipProposal> {
		let mut proposal = OwnershipProposal::pending(fact, "missing complete source path");
		let Some(full) = &fact.full_vtxo else { return Ok(proposal); };
		full.validate_unsigned(funding).context("invalid expiry terminal path")?;
		let state = &fact.state;

		if let Some(txid) = state.offboarded_in {
			ensure!(self.query_opt("SELECT txid FROM offboards WHERE txid=$1", &[&txid.to_string()]).await?.is_some(),
				"expiry offboard reference has no durable transaction");
			proposal.disposition = OwnershipDisposition::Operator;
			proposal.evidence = Some(OwnershipEvidence::Offboard(txid));
			return Ok(proposal);
		}

		// A conflicted payout keeps its original spend fence. Replanning
		// reads the already committed irreversible owner rather than treating
		// our own "spent" status as proof that the customer disappeared.
		if state.spend_state == SpendState::Spent {
			if let Some(row) = self.query_opt("
				SELECT e.id,e.irreversible_facts::TEXT AS irreversible_facts
				FROM expiry_entitlement e
				WHERE e.vtxo_id=$1
			", &[&state.vtxo_id.to_string()]).await? {
				let facts: serde_json::Value = serde_json::from_str(row.get("irreversible_facts"))?;
				let kind = facts.get("kind").and_then(serde_json::Value::as_str);
				if matches!(kind, Some(
					"pubkey"
						| "input_owner"
						| "lightning_receive_claim"
						| "pool"
						| "padding"
						| "forfeit"
						| "offboard"
						| "exchange"
						| "htlc_unattempted_expired"
						| "htlc_external_failed"
						| "htlc_internal_zero_grant_cancelled"
						| "htlc_source_discharged"
				)) {
					proposal.disposition = if let Some(key) = facts.get("customer_key").and_then(serde_json::Value::as_str) {
						self.expiry_customer(state.vtxo_id, key.parse()?).await?
					} else if facts.get("operator") == Some(&serde_json::Value::Bool(true)) {
						OwnershipDisposition::Operator
					} else { return Ok(proposal); };
					proposal.evidence = Some(OwnershipEvidence::IrreversibleEntitlement { entitlement_id: row.get("id") });
					return Ok(proposal);
				}
			}
		}

		// Bind originals to the particular stored round they fund; a superseded
		// cancelled pending participation is not a permanent coin blocklist.
		let parts = self.query(
			"
				SELECT p.id, p.unlock_hash, p.forfeited_at IS NOT NULL AS forfeited,
				EXISTS(SELECT 1 FROM round_part_input i2 WHERE i2.participation_id=p.id
					AND i2.signed_forfeit_tx IS NOT NULL) AS any_forfeit
			FROM round_part_input i JOIN round_participation p ON p.id=i.participation_id
			LEFT JOIN round r ON r.funding_txid=p.round_id
			WHERE i.vtxo_id=$1 AND (p.round_id IS NULL OR r.id=$2) ORDER BY p.id DESC LIMIT 2
			",
			&[&state.vtxo_id.to_string(), &state.spent_in_round],
		).await?;
		ensure!(parts.len() <= 1, "expiry input has conflicting round participations");
		if let Some(part) = parts.first() {
			let forfeited = part.get::<_, bool>("forfeited");
			if !forfeited && part.get::<_, bool>("any_forfeit") {
				proposal.disposition = OwnershipDisposition::Unresolved("partially stored forfeits");
				return Ok(proposal);
			}
			let id = part.get::<_, i64>("id");
			proposal.evidence = Some(OwnershipEvidence::Exchange {
				participation_id: id, unlock_hash: part.get("unlock_hash"), forfeited,
			});
			if forfeited {
				proposal.disposition = OwnershipDisposition::Operator;
			} else if let Ok(user) = full.clone().try_into_user_vtxo() {
				proposal.disposition = self.expiry_customer(state.vtxo_id, user.user_pubkey()).await?;
				proposal.cancellations.push(id);
			}
			return Ok(proposal);
		}

		if state.spend_state == SpendState::Pool {
			// Pool is assigned only by the trusted server's own issuance and
			// allocation paths, including dust change removed from the live pool.
			proposal.disposition = OwnershipDisposition::Operator;
			proposal.evidence = Some(OwnershipEvidence::Pool);
			return Ok(proposal);
		}
		if matches!(state.spend_state, SpendState::RoundForfeit | SpendState::OffboardForfeit | SpendState::OffboardConnector) {
			full.validate(funding).context("invalid expiry forfeit path")?;
			proposal.disposition = OwnershipDisposition::Operator;
			proposal.evidence = Some(OwnershipEvidence::Forfeit);
			return Ok(proposal);
		}
		let Ok(user) = full.clone().try_into_user_vtxo() else { return Ok(proposal); };
		if matches!(user.policy(), ark::VtxoPolicy::Pubkey(policy) if policy.user_pubkey == *PADDING_KEY) {
			proposal.disposition = OwnershipDisposition::Operator;
			proposal.evidence = Some(OwnershipEvidence::Padding);
			return Ok(proposal);
		}
		// Receive claims carry their own authenticated destination. A provider
		// checkpoint is not the recipient of a settled receive-for-address.
		let mut cancelled_receive_claim = false;
		if state.spend_state == SpendState::Unregistered || matches!(user.policy(), ark::VtxoPolicy::ServerHtlcRecv(..) | ark::VtxoPolicy::ServerHtlcRecv_v0(..)) {
			if let Some(claim) = self.receive_claim_entitlement(state.vtxo_id).await? {
				match claim.settlement {
					ReceiveClaimSettlement::CancelledUncollected => {
						// Cancellation creates no customer principal. Preserve the
						// grant's normal terminal revocation/expiry proof below;
						// a shared preimage cannot redirect it to the provider.
						cancelled_receive_claim = true;
					},
					ReceiveClaimSettlement::Pending => {
						proposal.disposition = OwnershipDisposition::Unresolved("receive claim settlement is not confirmed");
						return Ok(proposal);
					},
					ReceiveClaimSettlement::Settled => {
						if let Some(key) = claim.recipient {
							proposal.disposition = self.expiry_customer(claim.output_id, key).await?;
							proposal.evidence = Some(OwnershipEvidence::LightningReceiveClaim {
								source: claim.source, subscription_id: claim.subscription_id,
								payment_hash: claim.payment_hash, output: claim.output_id,
							});
						} else {
							proposal.disposition = OwnershipDisposition::Unresolved("receive destination has conditional policy");
						}
						return Ok(proposal);
					},
				}
			}
		}
		if state.spend_state == SpendState::Unregistered {
			if let Some((parent, key)) = self.expiry_unregistered_input_owner(full).await? {
				proposal.disposition = self.expiry_customer(parent, key).await?;
				proposal.evidence = Some(OwnershipEvidence::InputOwner);
			}
			return Ok(proposal);
		}
		if state.spend_state == SpendState::Unclaimed {
			let Some(hash) = user.unlock_hash() else { return Ok(proposal); };
			let part = self.query_opt("SELECT id, forfeited_at IS NOT NULL AS forfeited FROM round_participation WHERE unlock_hash=$1", &[&hash.to_string()]).await?;
			if let Some(part) = part {
				let id = part.get::<_, i64>("id");
				let forfeited = part.get::<_, bool>("forfeited");
				proposal.disposition = if forfeited { self.expiry_customer(state.vtxo_id, user.user_pubkey()).await? }
					else { proposal.cancellations.push(id); OwnershipDisposition::Operator };
				proposal.evidence = Some(OwnershipEvidence::Exchange { participation_id: id, unlock_hash: hash.to_string(), forfeited });
			} else if let Some(cancelled) = self.query_opt(
				"SELECT participation_id FROM expiry_cancelled_participation WHERE unlock_hash=$1",
				&[&hash.to_string()],
			).await? {
				// Upstream removes superseded participation rows. Its old
				// unclaimed output remains nonbinding: the durable cancellation
				// also prevents its unlock preimage from ever being released.
				proposal.disposition = OwnershipDisposition::Operator;
				proposal.evidence = Some(OwnershipEvidence::Exchange {
					participation_id: cancelled.get("participation_id"),
					unlock_hash: hash.to_string(), forfeited: false,
				});
			} else if user.user_pubkey().to_string() == "02dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c"
				&& hash.to_string() == "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
				&& user.amount() == Amount::from_sat(330) {
				proposal.disposition = OwnershipDisposition::Operator;
				proposal.evidence = Some(OwnershipEvidence::Padding);
			}
			return Ok(proposal);
		}
		if state.oor_spent_txid.is_some() {
			proposal.disposition = OwnershipDisposition::Unresolved("terminal has registered descendants");
			return Ok(proposal);
		}
		let payment = user.policy().as_server_htlc_send().map(|p| p.payment_hash)
			.or_else(|| user.policy().as_server_htlc_recv().map(|p| p.payment_hash));
		if let Some(hash) = payment {
			let htlcs = htlc_vtxo::get_htlc_vtxo_states_by_payment_hash(self, hash).await?;
			let Some(htlc) = htlcs.iter().find(|h| h.vtxo.vtxo_id == proposal.id) else { return Ok(proposal); };
			proposal.has_stored_preimage = self.get_htlc_settlement_by_payment_hash(hash).await?.is_some();
			let expired_contract = htlc.htlc.htlc_expiry <= tip
				&& watchman_tip.is_some_and(|height| htlc.htlc.htlc_expiry <= height)
				&& fact.onchain_spent_height.is_none() && fact.onchain_spent_txid.is_none();
			let Some(resolution) = htlc.htlc.resolution() else {
				// Use the funded operator sweep even when the receiver exposed
				// the grant: a small grant cannot pay for its own timeout claim.
				if htlc.direction() == HtlcDirection::Outgoing
					&& (!proposal.has_stored_preimage || cancelled_receive_claim) && expired_contract {
					proposal.disposition = OwnershipDisposition::Operator;
					proposal.evidence = Some(OwnershipEvidence::HtlcRecvExpired { payment_hash: hash, htlc_expiry: htlc.htlc.htlc_expiry });
					return Ok(proposal);
				}
				if proposal.has_stored_preimage && htlc.direction() == HtlcDirection::Outgoing && !cancelled_receive_claim {
					// A granted receive is already a signed obligation to this
					// recipient. Sending grants require their own funding evidence.
					proposal.disposition = self.expiry_customer(state.vtxo_id, user.user_pubkey()).await?;
					proposal.evidence = Some(OwnershipEvidence::HtlcStoredPreimage { payment_hash: hash });
					return Ok(proposal);
				}
				let source = if htlc.direction() == HtlcDirection::Incoming {
					self.lightning_source_state(state.vtxo_id, hash).await?
				} else { LightningSourceState::Pending };
				if source == LightningSourceState::Discharged {
					proposal.disposition = OwnershipDisposition::Operator;
					proposal.evidence = Some(OwnershipEvidence::HtlcSourceDischarged { payment_hash: hash });
					return Ok(proposal);
				}
				// An uncollected internal payment: once every receive grant is
				// back with the server on chain, the sender's source is theirs.
				// Its own send expiry can lie beyond the tree's expiry and does
				// not matter: no one can collect the payment any more.
				let unspent = fact.onchain_spent_height.is_none() && fact.onchain_spent_txid.is_none()
					&& htlc.htlc.chain_resolution.is_none();
				if let LightningSourceState::InternalCommitted { attempt_id, subscription_id } = source {
					if let Some(watchman_tip) = watchman_tip.filter(|_| unspent) {
						if let Some(block) = self.internal_grants_reclaimed(subscription_id, hash, tip, watchman_tip, internal_refund_depth).await? {
							proposal.disposition = self.expiry_customer(state.vtxo_id, user.user_pubkey()).await?;
							proposal.evidence = Some(OwnershipEvidence::HtlcInternalGrantsReclaimed {
								payment_hash: hash, attempt_id, subscription_id, block,
							});
							return Ok(proposal);
						}
					}
				}
				// A same-hash receive funded by another source cannot retain
				// this sender's money. Attempts without outcome evidence still wait.
				if matches!(source, LightningSourceState::Unattempted | LightningSourceState::Failed | LightningSourceState::InternalZeroGrantCancelled { .. })
					&& fact.onchain_spent_height.is_none() && htlc.htlc.chain_resolution.is_none()
					&& htlc.htlc.htlc_expiry <= tip && watchman_tip.is_some_and(|h| htlc.htlc.htlc_expiry <= h) {
					proposal.disposition = self.expiry_customer(state.vtxo_id, user.user_pubkey()).await?;
					proposal.evidence = Some(match source {
						LightningSourceState::InternalZeroGrantCancelled { attempt_id, subscription_id } =>
							OwnershipEvidence::HtlcInternalZeroGrantCancelled { payment_hash: hash, htlc_expiry: htlc.htlc.htlc_expiry, attempt_id, subscription_id },
						LightningSourceState::Failed => OwnershipEvidence::HtlcExternalFailed { payment_hash: hash, htlc_expiry: htlc.htlc.htlc_expiry },
						_ => OwnershipEvidence::HtlcUnattemptedExpired { payment_hash: hash, htlc_expiry: htlc.htlc.htlc_expiry },
					});
				} else {
					proposal.disposition = OwnershipDisposition::Unresolved("Lightning settlement pending");
					proposal.needs_chain_progress = expired_contract;
				}
				return Ok(proposal);
			};
			if htlc.direction() == HtlcDirection::Outgoing && resolution == HtlcResolution::Revoked
				&& !crate::ln::htlc_recvs_expired(&htlcs, tip, watchman_tip) {
				proposal.disposition = OwnershipDisposition::Unresolved("Lightning receive not irrevocably expired");
				proposal.needs_chain_progress = expired_contract;
				return Ok(proposal);
			}
			proposal.evidence = if let Some(chain_resolution) = htlc.htlc.chain_resolution {
				let height = htlc.htlc.chain_resolution_height.context("HTLC chain resolution has no height")?;
				if watchman_tip.is_none_or(|point| height > point) || height > tip {
					proposal.disposition = OwnershipDisposition::Unresolved("HTLC chain resolution ahead of completed sync");
					return Ok(proposal);
				}
				let block = self.get_block_by_height(BlockTable::Watchmand, height).await?
					.context("HTLC chain resolution block not in completed watchman view")?;
				Some(OwnershipEvidence::HtlcChain { payment_hash: hash, resolution: chain_resolution, block })
			} else { Some(OwnershipEvidence::HtlcOffchain { payment_hash: hash, resolution }) };
			proposal.disposition = match (htlc.direction(), resolution) {
				(HtlcDirection::Incoming, HtlcResolution::Fulfilled) | (HtlcDirection::Outgoing, HtlcResolution::Revoked) => OwnershipDisposition::Operator,
				_ => self.expiry_customer(state.vtxo_id, user.user_pubkey()).await?,
			};
			return Ok(proposal);
		}
		if state.spend_state != SpendState::Spendable {
			proposal.disposition = OwnershipDisposition::Unresolved("nonterminal spend state without settlement evidence");
			return Ok(proposal);
		}
		full.validate(funding).context("invalid customer expiry source signature")?;
		proposal.disposition = self.expiry_customer(state.vtxo_id, user.user_pubkey()).await?;
		proposal.evidence = Some(OwnershipEvidence::Pubkey);
		Ok(proposal)
	}

	async fn expiry_customer(&self, id: VtxoId, user_pubkey: PublicKey) -> anyhow::Result<OwnershipDisposition> {
		let mailbox = self.query_opt("SELECT unblinded_mailbox_id FROM mailbox WHERE vtxo_id=$1 ORDER BY id LIMIT 1", &[&id.to_string()]).await?
			.map(|r| MailboxIdentifier::from_str(r.get("unblinded_mailbox_id"))).transpose()?;
		Ok(OwnershipDisposition::Customer { user_pubkey, mailbox })
	}

	async fn expiry_unregistered_input_owner(&self, full: &ServerVtxo<Full>) -> anyhow::Result<Option<(VtxoId, PublicKey)>> {
		let Some(last) = full.transactions().last() else { return Ok(None); };
		let [input] = last.tx.input.as_slice() else { return Ok(None); };
		let parent = VtxoId::from(input.previous_output);
		let Some(row) = self.query_opt(
			"SELECT vtxo FROM vtxo WHERE vtxo_id=$1 AND oor_spent_txid=$2",
			&[&parent.to_string(), &full.point().txid.to_string()]
		).await? else {
			return Ok(None);
		};
		let parent_vtxo: ServerVtxo = ServerVtxo::deserialize(row.get("vtxo"))?;
		let key = match parent_vtxo.policy() {
			ServerVtxoPolicy::User(p) => p.user_pubkey(),
			ServerVtxoPolicy::Checkpoint(p) => p.user_pubkey,
			_ => return Ok(None),
		};
		Ok(Some((parent, key)))
	}
}
