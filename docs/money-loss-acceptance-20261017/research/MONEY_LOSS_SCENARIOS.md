# Money-loss scenario catalogue

This catalogue contains **200 scenarios, ML001–ML200; 81 concern Lightning**, including its Noah, attribution, settlement and recovery boundaries. These are concrete event chains for later controlled defensive testing, not 200 asserted bugs. The research did not execute the scenarios. Read [README.md](README.md) for pinned scope, evidence semantics, financial contract, exclusions and the mandatory independent oracle shared by every entry. [PRIORITIES.md](PRIORITIES.md) orders the next testing phase.

Each entry uses A–K. “Current” means the inspected source, finally pinned at `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`. The initial review was at `751bd5a5d8ec9bbf72f0a44c1e7d4ac371b6269d` plus its hashed internal-receive overlay; that overlay was subsequently committed, and the two additional changed ownership-scan source files were reviewed at closeout. This is **not a claim about a live deployed artifact**. E01–E55 resolve below each scenario to exact repository/commit/file/symbol/line and read-time SHA256 in [source-evidence.json](source-evidence.json). A current guard is positive evidence; a scenario that asks whether a bypass is reachable remains a hypothesis. Missing tests do not establish a defect.

For hypotheses, the failure-producing transition in E is conditional: its reachability, not just its consequence, must be validated. Normal atomic transactions are never assumed to tear; incomplete rows explicitly require legacy, import or nonaligned-backup context. Multiple same-hash subscriptions require schema/version/import reachability proof; no such state is assumed possible merely because a query accepts a hash.

Baseline oracle for **every J**: freeze the original independently known customer, signed source/contract, amount and intended script before fault injection; inspect actual canonical inputs, outpoints, outputs and effective terminal Lightning evidence afterward; enumerate the entire affected lineage and other recipients, not only the expected output; count duplicate value and canonical miner fees once; retain unpaid/unknown value and measure eventual progress. The amount/script in J is the proposed concrete fixture, not a result measured by this research. RPC success, “Spent,” allocation rows and mempool admission are insufficient. A hypothetical prepayment rejection is not a funded entitlement loss.

<a id="ml001"></a>

## ML001 — Unused same-hash send grant becomes operator property

**A. Title:** Unused same-hash send grant becomes operator property.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** historical; current prevention requires regression.

**D. Context and financial state:** Two signed 100,000-sat send grants share a hash; only one funds the external invoice.

**E. Event chain and suspected mechanism:** Historical resolver treats any stored hash preimage as payment of both sources; expiry confiscates the unused liability.

**F. Expected correct behavior:** Discharge only the attempted source; eventually pay the unused source 99,000 sat.

**G. Potential incorrect outcome:** Unused sender loses 100,000 sat gross to operator; after a confirmed sweep the original grant cannot recover it.

**H. Evidence and inference:** E39 send_ownership; E06 current source protection. Prior wrong-source failure is reported in docs/fallback-lightning-evidence.md; raw first failure was not recovered here. Earlier behavior/evidence is kept separate from current protection; a historical result does not qualify the inspected release.

E39: [server/src/database/expiry_ownership.rs:394](/home/francis/sweep-to-payout-src/server/src/database/expiry_ownership.rs:394), `send_ownership`, repository `/home/francis/sweep-to-payout-src`, HEAD `994aede0ca7b2631e65ec3857f48b71888e0316f`; exact read bytes SHA256 `b45b467f5a4484ce997f796cbd7683415e74f0281d903c314a2c45822d5e9c77`. Code fact: Historical B treats any same-hash preimage/succeeded attempt as paid send; receive ownership also uses hash-level evidence.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

Test reference: [expiry_unused_same_hash_grant_retains_its_own_liability](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_hash_provenance.rs:148), file SHA256 `f51d93c02fd187dc4be727653a70da5e9d62965afaeecea7e7ceda3018f5c6b6` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_unused_same_hash_grant_retains_its_own_liability creates two genuine grants, proves one attempted and confirms unused99k; it covers one attempt/source, not multipart retry graphs. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Bind every attempt to exact input IDs; observe external invoice90k once, unused99k once, zero customer payout from paid source across both roots. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Historical regression risk**. Root-cause family: `LN-SOURCE`.

<a id="ml002"></a>

## ML002 — Refund cancels an unrelated receive sharing the hash

**A. Title:** Refund cancels an unrelated receive sharing the hash.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** historical; current regression boundary.

**D. Context and financial state:** A refundable sender and an independently externally funded receive share a hash.

**E. Event chain and suspected mechanism:** Legacy store_expiry_payment cancels receive subscription by hash while refunding the sender; receive principal is then treated as canceled.

**F. Expected correct behavior:** Cancel only obligations funded by that exact sender source.

**G. Potential incorrect outcome:** Receiver may lose its claim to operator or external payer; durable cancellation can suppress eventual payout.

**H. Evidence and inference:** E41 store_expiry_payment; E15 lightning_source_subscriptions narrows current behavior. Earlier behavior/evidence is kept separate from current protection; a historical result does not qualify the inspected release.

E41: [server/src/database/expiry_settlement.rs:190](/home/francis/abandon-ship-bark-two-step-comparison/server/src/database/expiry_settlement.rs:190), `store_expiry_payment`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `377bee85dc542ec86957a00b7ca2e7fea58e6182f564e3de1a14f1bd97db841e`. Code fact: Historical A persists signed payout in nursery and settlement rows; refund path cancels subscriptions by hash.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

Test reference: [expiry_unused_same_hash_grant_can_be_refunded_after_other_source_paid](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_hash_provenance.rs:153), file SHA256 `f51d93c02fd187dc4be727653a70da5e9d62965afaeecea7e7ceda3018f5c6b6` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_unused_same_hash_grant_can_be_refunded_after_other_source_paid proves refund of an unused send; it does not construct a separate externally funded receive subscription. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Show unrelated receive10k settles/claims and pays recipient9k, alongside sender99k; cancellation must touch only actual source-linked IDs. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Historical regression risk**. Root-cause family: `LN-SOURCE`.

<a id="ml003"></a>

## ML003 — Coarse Failed status permits refund after real external success

**A. Title:** Coarse Failed status permits refund after real external success.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** historical regression risk; current guard present.

**D. Context and financial state:** A source actually paid90k; local attempt status is stale Failed and the preimage exists.

**E. Event chain and suspected mechanism:** Status writer or backup retains Failed; refund decision reads that projection without source success receipt.

**F. Expected correct behavior:** Actual source-bound success must veto any customer refund.

**G. Potential incorrect outcome:** Sender can receive both external90k and Ark100k/refund99k; operator bears duplicate loss.

**H. Evidence and inference:** E12 source receipts; E15 source-state revocation guard; E14 optimistic status writes. Historical context/positive guard does not establish an earlier actual failing execution; classify the proposed reachability conservatively as a hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

E14: [server/src/ln/payment_handler.rs:76](/home/francis/abandon-ship-fresh-integration/server/src/ln/payment_handler.rs:76), `process_payment_attempt`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ac6da2446dece70ce724cca06dfc3e78316e69615e8cfe94a04f2c49378f69fa`. Code fact: Preimage storage precedes optimistic attempt update; only updated linked coins become ln-spent; notifications follow DB commit.

Test reference: [reject_revocation_when_settled_but_status_regressed](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:304), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** reject_revocation_when_settled_but_status_regressed is named in corrected run d3025d92; proves one deliberately regressed attempt refusal, not a restore losing receipt tables. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe authoritative CLN complete group plus exact source label; assert zero confirmed sender refund anywhere in its lineage. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-TERMINAL`.

<a id="ml004"></a>

## ML004 — One failed Lightning part is mistaken for whole-payment failure

**A. Title:** One failed Lightning part is mistaken for whole-payment failure.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Multipart external payment has one failed part while another remains pending or complete.

**E. Event chain and suspected mechanism:** A reducer sees terminal failure for one part/group and admits source refund before all payment obligations are terminal.

**F. Expected correct behavior:** Require whole source dispatch terminality and no successful or pending part.

**G. Potential incorrect outcome:** Successful payee plus sender refund duplicates principal; unresolved route parts may still settle later.

**H. Evidence and inference:** E12 terminal_failure and source-labelled listpays reduction; E06 source state. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

Test reference: [expiry_actual_failed_external_send_pays_offline_owner](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_lightning_terminal.rs:114), file SHA256 `7511a38dd772c57f753c26731df86e39824d3163642f5be30e7db282e4e612d9` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_actual_failed_external_send_pays_offline_owner uses one accepted-then-canceled invoice; no retained multipart mixed-result oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** For a100k source enumerate node parts/group labels independently; after any90k success require refund0; all-failed path eventually yields99k once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-TERMINAL`.

<a id="ml005"></a>

## ML005 — Latest shared-hash payment misreports an unused source as paid

**A. Title:** Latest shared-hash payment misreports an unused source as paid.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** An older same-hash attempt actually completes remotely after a newer source was admitted, or an authentic imported/backup history introduces that ordering. Normal admission refuses an already-settled hash; that guard must not be assumed bypassed.

**E. Event chain and suspected mechanism:** Reconciliation selects a coarse latest-created-index node record, while SDK success uses a hash preimage. Validate whether the late old completion can mark the newer unused source spent without its own funding evidence.

**F. Expected correct behavior:** Coarse/client success must identify which source actually discharged; unused source remains recoverable.

**G. Potential incorrect outcome:** Unused sender loses visible entitlement or stops protection although its fallback liability remains; recovery may require absent source metadata.

**H. Evidence and inference:** E12 latest-created-index selection; E18 settle_lightning_send_payment; E06 source-specific server resolver. Reachability of the late-completion ordering remains unproved. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E18: [bark/src/actions/lightning/pay.rs:152](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/pay.rs:152), `check_lightning_send_payment_status`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `55daadb631de69c322d79b767ff2808f537c55f94fe8b39858fff632169478a6`. Code fact: Expired Pending/transport error collapses to local Failed; revocation and exit still need authentic protection. Refund registration failure is warned, then local outputs stored.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

**I. Current coverage:** Same-hash tests assert server liability/refund; they do not reopen two SDK send actions and independently audit each movement. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Both SDK histories must agree with input-specific CLN spend links; unused100k never becomes an unexplained final debit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-STATUS`.

<a id="ml006"></a>

## ML006 — Dispatch reservation survives a crash before the node call

**A. Title:** Dispatch reservation survives a crash before the node call.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** 100k source has a committed expiry_lightning_dispatch row; no Xpay reached CLN.

**E. Event chain and suspected mechanism:** Process stops after durable dispatch insert and before call_xpay; restart refuses replay, finds empty history and has no typed rejection.

**F. Expected correct behavior:** Retain entitlement and establish a bounded safe terminal recovery without needing the sender.

**G. Potential incorrect outcome:** Sender99k remains blocked indefinitely; safe nonreplay prevents duplication but does not satisfy liveness.

**H. Evidence and inference:** E11 admit_lightning_dispatch; E12 ClnXpayClient::pay; fork V6 dispatch fence. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

Test reference: [expiry_external_success_during_restart_is_bound_to_its_actual_source](/home/francis/abandon-ship-fresh-integration/testing/tests/server/qualification_lightning.rs:57), file SHA256 `ca5ea7806da75e6a8229af77baed34b499706a70aa855ec9a75f8241d056d91f` at inventory time. Listing/reference is not an execution claim.

Test reference: [external_success_during_restart](/home/francis/abandon-ship-fresh-integration/testing/tests/server/qualification_lightning.rs:62), file SHA256 `ca5ea7806da75e6a8229af77baed34b499706a70aa855ec9a75f8241d056d91f` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_external_success_during_restart_is_bound_to_its_actual_source covers an actual already dispatched success, not this no-call durable boundary. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Prove dispatch count1, CLN executions0, source remains accounted for and eventually99k reaches rightful script under an explicit justified terminal procedure. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-DISPATCH`.

<a id="ml007"></a>

## ML007 — Lost no-part error leaves an executed dispatch permanently ambiguous

**A. Title:** Lost no-part error leaves an executed dispatch permanently ambiguous.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Xpay rejects before creating parts, but transport loses that exact rejection reply.

**E. Event chain and suspected mechanism:** Dispatch is durable; empty node history drives coarse Failed but cannot create a trusted terminal receipt.

**F. Expected correct behavior:** Recover the original dispatch safely and eventually discharge or refund its source.

**G. Potential incorrect outcome:** Sender fallback can remain unresolved forever despite no external payment; original client return does not create missing terminal evidence.

**H. Evidence and inference:** E11 dispatch fence; E12 no_attempt_rejection and history checks. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

**I. Current coverage:** Qualified adapter no-part positive requires an actual typed205 reply; neither positive nor stock negative proves recovery when the original reply disappears. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently establish zero remote executions/parts under the admitted node identity; require one99k canonical payout or an explicitly bounded recovery state. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-DISPATCH`.

<a id="ml008"></a>

## ML008 — Stock CLN transport cannot prove a genuine zero-attempt failure

**A. Title:** Stock CLN transport cannot prove a genuine zero-attempt failure.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** shared dependency; stock runtime observed.

**D. Context and financial state:** Actual no-channel case uses stock cln-grpc421c19f3 and returns Unknown carrying205 in text.

**E. Event chain and suspected mechanism:** Backend requires typed metadata; retained run930221cb times out waiting30s for terminal proof despite zero retained CLN parts.

**F. Expected correct behavior:** Deploy qualified structured-error transport and retain source-specific rejection evidence.

**G. Potential incorrect outcome:** Real no-attempt refund liveness is unavailable on this pairing; no actual payout loss was demonstrated by the timeout.

**H. Evidence and inference:** E12; fresh-testing-track-20261017/batch09-native-lightning-terminal-sources-summary.log; cache/backend-no-part-original-diagnosis-20261017/diagnosis.json. The retained observation establishes only the scope explicitly described; permanent monetary loss is not inferred.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

Test reference: [expiry_actual_no_part_external_send_pays_offline_owner](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_lightning_terminal.rs:119), file SHA256 `7511a38dd772c57f753c26731df86e39824d3163642f5be30e7db282e4e612d9` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_actual_no_part_external_send_pays_offline_owner FAIL44.737s; separate adapter c0a5f364 PASS28.777s with same money fixture. Changed transport, not disappearance of failure. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Exact selected plugin hash plus authentic rejection205/zero parts and99k Core>=6 payout; health/RPC availability is insufficient. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Observed failure**. Root-cause family: `LN-TRANSPORT`.

<a id="ml009"></a>

## ML009 — Node version upgrade disables terminal-failure recognition

**A. Title:** Node version upgrade disables terminal-failure recognition.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Node reports a version other than exact v26.06.6 while real outgoing payment fails.

**E. Event chain and suspected mechanism:** Terminal observation reaches explicit version ensure; receipt is never persisted and source stays Pending.

**F. Expected correct behavior:** An admitted runtime must support qualified terminal semantics or preserve bounded documented recovery.

**G. Potential incorrect outcome:** Failed send99k delayed indefinitely after an otherwise healthy upgrade; operator retains unresolved liability.

**H. Evidence and inference:** E12 sync_payment_attempt_status_locked exact version gate. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

**I. Current coverage:** Adapter positive pins26.06.6; no mixed-version node upgrade/receipt compatibility execution inspected. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** On declared upgraded version prove original dispatch failure and eventual99k; otherwise expose unsupported refund capability before accepting new sends. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-TRANSPORT`.

<a id="ml010"></a>

## ML010 — Restored dispatch labels no longer match node history

**A. Title:** Restored dispatch labels no longer match node history.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Product DB and CLN history come from different backup instants; genuine payment completed.

**E. Event chain and suspected mechanism:** Restored/migrated dispatch row receives a new label or loses original mapping; Complete history cannot satisfy source receipt query.

**F. Expected correct behavior:** Preserve original dispatch identity through restore and reconcile all completed sources.

**G. Potential incorrect outcome:** Paid source remains ambiguous; indefinite block or an unsafe manual refund can respectively strand or duplicate funds.

**H. Evidence and inference:** E11/E12; V7 label default and E45 durable ledger independence. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E45: [server/src/database/fork_migrations/V5__expiry_allocations.sql:91](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V5__expiry_allocations.sql:91), `expiry_ledger_commit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `3712331243e47c147d3387b5887f02eeccb0d7a59e2b273ba9d4d55574ecea42`. Code fact: Permanent one-active allocation index; append-only visible-transaction revision/count; triggers across payout ledger.

**I. Current coverage:** Restart success test preserves DB/node histories; it is not a nonaligned backup restore. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare backup dispatch label with canonical node group and exact linked input IDs; require paid-source refund0 and unused-source eventual payout. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-BACKUP`.

<a id="ml011"></a>

## ML011 — Lightning node replacement hides the only authoritative attempt history

**A. Title:** Lightning node replacement hides the only authoritative attempt history.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** A dispatched payment belongs to retired nodeID; replacement node is healthy but lacks history.

**E. Event chain and suspected mechanism:** Manager reconnects replacement; old node-specific receipt polling cannot establish terminal outcome.

**F. Expected correct behavior:** Retain original-node evidence/access or an explicit safe recovery path for each outstanding source.

**G. Potential incorrect outcome:** User entitlement can be frozen permanently; treating replacement empty history as failure risks duplicate refund.

**H. Evidence and inference:** E12 node_id polling; E13 node_by_id and node-specific invoice settlement. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

**I. Current coverage:** Normal downtime success test restarts product while original CLN survives; no node-loss restore oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Inventory all source labels and original node identities; no pending liability disappears and any refunded100k source has authentic original terminal evidence. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-BACKUP`.

<a id="ml012"></a>

## ML012 — Stream checkpoint advances beyond an unpersisted payment receipt

**A. Title:** Stream checkpoint advances beyond an unpersisted payment receipt.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** CLN emits completion while product processes stream indices and receipt/status writes separately.

**E. Event chain and suspected mechanism:** Interruption or error leaves resumed stream after event, but success receipt/preimage not durable; final status rows may be outside ordinary open polling.

**F. Expected correct behavior:** Periodic original-source reconciliation must repair every lost event independently of cursor.

**G. Potential incorrect outcome:** Paid source may stay ambiguous indefinitely or client may retry payment; operator/user loss depends on retry behavior.

**H. Evidence and inference:** E12 ClnXpayProcess::process_payment_attempts; E14; database/ln payment-index methods. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E14: [server/src/ln/payment_handler.rs:76](/home/francis/abandon-ship-fresh-integration/server/src/ln/payment_handler.rs:76), `process_payment_attempt`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ac6da2446dece70ce724cca06dfc3e78316e69615e8cfe94a04f2c49378f69fa`. Code fact: Preimage storage precedes optimistic attempt update; only updated linked coins become ln-spent; notifications follow DB commit.

Test reference: [expiry_external_success_during_restart_is_bound_to_its_actual_source](/home/francis/abandon-ship-fresh-integration/testing/tests/server/qualification_lightning.rs:57), file SHA256 `ca5ea7806da75e6a8229af77baed34b499706a70aa855ec9a75f8241d056d91f` at inventory time. Listing/reference is not an execution claim.

Test reference: [external_success_during_restart](/home/francis/abandon-ship-fresh-integration/testing/tests/server/qualification_lightning.rs:62), file SHA256 `ca5ea7806da75e6a8229af77baed34b499706a70aa855ec9a75f8241d056d91f` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_external_success_during_restart_is_bound_to_its_actual_source checks downtime success; no inspected oracle freezes index ahead of receipt durability. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After restart compare every canonical CLN Complete label to durable source evidence and verify zero new dispatch, zero paid-source refund. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-STREAM`.

<a id="ml013"></a>

## ML013 — Returned success is lost before durable preimage storage

**A. Title:** Returned success is lost before durable preimage storage.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** CLN has irreversibly paid90k; product receives success but settlement storage fails.

**E. Event chain and suspected mechanism:** Xpay success precedes settler commit; process exits; status/reply may have indicated completion without retained preimage.

**F. Expected correct behavior:** Reconcile exact CLN history, persist valid preimage and source receipt, never refund paid source.

**G. Potential incorrect outcome:** Without original node history/preimage, operator may be unable to collect its source or prove user discharge; unsafe refund doubles payment.

**H. Evidence and inference:** E12 ClnXpayClient::pay settlement error handling; E14 preimage-before-status ordering. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

E14: [server/src/ln/payment_handler.rs:76](/home/francis/abandon-ship-fresh-integration/server/src/ln/payment_handler.rs:76), `process_payment_attempt`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ac6da2446dece70ce724cca06dfc3e78316e69615e8cfe94a04f2c49378f69fa`. Code fact: Preimage storage precedes optimistic attempt update; only updated linked coins become ln-spent; notifications follow DB commit.

**I. Current coverage:** Downtime success test covers retained remote success; not loss of remote history after failed WAL persistence. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Authoritative90k paid once, valid durable secret presence (without logging it), source operator disposition and refund0 after restart. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-WAL`.

<a id="ml014"></a>

## ML014 — Confirmed timeout refund remains final after another source reveals the shared preimage

**A. Title:** Confirmed timeout refund remains final after another source reveals the shared preimage.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current guard / regression boundary.

**D. Context and financial state:** Source A already has a canonical confirmed timeout refund of its 100,000 sat; independent source B later pays the same invoice/hash and reveals the preimage. No reorg occurs.

**E. Event chain and suspected mechanism:** After B completes, restart/rebuild ownership for A. A hash-level success shortcut could overwrite A's effective Revoked outcome, or create an additional fallback refund despite its actual timeout payment.

**F. Expected correct behavior:** Preserve A's canonical terminal refund and discharge B alone from its actual funding; new knowledge cannot create a second principal.

**G. Potential incorrect outcome:** A could receive an extra fallback payment, or A's confirmed refund could be falsely reassigned to operator accounting. B must not receive a refund after paying.

**H. Evidence and inference:** E06 source-specific state; E16 effective HTLC resolution; E39 historical global-preimage shortcut motivates regression scrutiny. Current chain-first resolution is a positive protection; no runtime failure established. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E16: [server/src/ln/mod.rs:101](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:101), `htlc_recvs_expired`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: All outgoing grants must be strictly past deadline in both views; exited grants need chain Revoked.

E39: [server/src/database/expiry_ownership.rs:394](/home/francis/sweep-to-payout-src/server/src/database/expiry_ownership.rs:394), `send_ownership`, repository `/home/francis/sweep-to-payout-src`, HEAD `994aede0ca7b2631e65ec3857f48b71888e0316f`; exact read bytes SHA256 `b45b467f5a4484ce997f796cbd7683415e74f0281d903c314a2c45822d5e9c77`. Code fact: Historical B treats any same-hash preimage/succeeded attempt as paid send; receive ownership also uses hash-level evidence.

**I. Current coverage:** Same-hash unused-source tests do not establish a confirmed timeout first, followed by another source's later actual payment and a full rebuild. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Enumerate A's confirmed timeout outpoint and every fallback output: A receives only its existing timeout principal, no extra 99,000 sat; B has exactly one real Lightning debit and zero refund. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-TERMINAL-PRECEDENCE`.

<a id="ml015"></a>

## ML015 — Final ambiguous attempts are polled only after backing expiry

**A. Title:** Final ambiguous attempts are polled only after backing expiry.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** A coarse-final source lacks a terminal receipt; backing remains unexpired for a long period.

**E. Event chain and suspected mechanism:** unproven_expiry_lightning_dispatches filters on v.expiry<=captaind tip; regular open-attempt polling skips final rows.

**F. Expected correct behavior:** Keep user recovery visible and resolve ambiguity within declared operational bounds.

**G. Potential incorrect outcome:** Money is unavailable until backing expiry or longer; stale success/failure display can induce duplicate user payment elsewhere.

**H. Evidence and inference:** E11 unproven_expiry_lightning_dispatches; E12 process_payment_attempts. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

**I. Current coverage:** Downtime case deliberately ages the backing; it does not establish pre-expiry recovery latency for coarse-final ambiguity. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Measure source terminal-evidence latency before expiry against stated SLA, while checking no replay and no false refundable status. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P2 — Source-supported risk**. Root-cause family: `LN-RECONCILE`.

<a id="ml016"></a>

## ML016 — One stalled hash reconciliation starves later hashes on the same node

**A. Title:** One stalled hash reconciliation starves later hashes on the same node.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Several pending source attempts use one otherwise-responsive CLN node. The listpays observation for the first hash stalls while unrelated queries would succeed.

**E. Event chain and suspected mechanism:** That node's ClnXpayProcess awaits per-attempt observations serially; later hashes on the same process receive no receipt polling until the first returns.

**F. Expected correct behavior:** Bound one observation and preserve terminal-evidence progress for independent hashes on that node.

**G. Potential incorrect outcome:** Later failed senders may remain unpaid after recovery is otherwise available. Other nodes have separate processes and are not shown blocked.

**H. Evidence and inference:** E12 ClnXpayProcess::process_payment_attempts serial awaits. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

**I. Current coverage:** Historical fallback_unresponsive_node_does_not_stall_tick exists in TrackA/B, but fresh current receipt polling is a distinct implementation; no qualified current stall oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Delay only one hash observation; later failed 100,000-sat sources on the same node still obtain authenticated terminal proof and 99,000-sat fallback within an explicit bound. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-QUEUE`.

<a id="ml017"></a>

## ML017 — Payment-local guard remains held by a hung Xpay call

**A. Title:** Payment-local guard remains held by a hung Xpay call.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** A100k send holds payment hash guard across the external network call.

**E. Event chain and suspected mechanism:** Call stalls; same-hash cancellation/status synchronization uses guard or try_lock and cannot repair the source.

**F. Expected correct behavior:** Keep ambiguous source protected while providing bounded original-call reconciliation.

**G. Potential incorrect outcome:** User cannot revoke or obtain fallback; guard clears on restart but durable dispatch ambiguity remains.

**H. Evidence and inference:** E12 pay holds _payment through call_xpay; sync uses try_lock. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E12: [server/src/ln/cln/xpay.rs:165](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/xpay.rs:165), `sync_payment_attempt_status_locked`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `7ea3fcee5ab94b9e74759171ef55fbe2c12b9a814de83d8513377901779a3ceb`. Code fact: Success/failure receipts require dispatch label; typed 205/207 plus history and exact CLN version gate no-part failure; latest shared-hash payment still drives coarse state.

**I. Current coverage:** Existing accepted cancellation finishes a real node call; no inspected persistent transport-stall plus eventual original-source recovery. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Track guard wait versus node terminal evidence; require no second dispatch and eventual99k if failure is established. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-QUEUE`.

<a id="ml018"></a>

## ML018 — Replacement attempt starts while the previous remote payment is still alive

**A. Title:** Replacement attempt starts while the previous remote payment is still alive.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Two financial server processes share a DB but have separate PaymentGuards. The first attempt was coarsely marked Failed while its already-dispatched remote operation is still unresolved.

**E. Event chain and suspected mechanism:** The partial unique open-attempt index no longer covers the terminal first row. A second process admits a replacement for the same hash before the old remote operation actually terminates.

**F. Expected correct behavior:** Source-specific terminal proof and dispatch binding must fence overlapping live remote activity, beyond uniqueness of locally open rows.

**G. Potential incorrect outcome:** Both source sets can remain economically committed; mistaken refund of either can duplicate payment or leave an unpaid valid source frozen.

**H. Evidence and inference:** E11 attempt insertion and dispatch markers; E13 process-local guards. server/schema.sql lightning_payment_attempt_open_payment_hash_uix (partial unique index) is an existing protection against two simultaneously open local rows, not proof that remote activity ended. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

**I. Current coverage:** Same-hash single-process tests and the unique index rule out simple double-open insertion; they do not demonstrate replacement after a false terminal local status while a remote call survives. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Count actual node dispatches and source links across both processes; one invoice amount only and every unused source retains refund entitlement. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CONCURRENCY`.

<a id="ml019"></a>

## ML019 — Expired warehoused grant begins its first external payment

**A. Title:** Expired warehoused grant begins its first external payment.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Grant signed before tree expiry is queued but not yet externally dispatched.

**E. Event chain and suspected mechanism:** Queue delays until backing expired; worker uses initiation-time tip instead of locked current source check.

**F. Expected correct behavior:** Refuse a new first dispatch from expired backing, while preserving safe admitted recovery.

**G. Potential incorrect outcome:** Operator can pay Lightning after fallback was decided or backing lost; competing obligations can double spend economic value.

**H. Evidence and inference:** E10 initial admission; E11 admit_lightning_dispatch rechecks locked inputs. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E10: [server/src/ln/mod.rs:207](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:207), `request_lightning_pay_htlc_cosign`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: New grant admission and duration bound; idempotent old spends handled separately.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

Test reference: [expiry_dispatch_rechecks_registered_sources_and_closed_attempts](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_dispatch.rs:19), file SHA256 `2078937b987b5434aea1550e4a9c6d9ad37b87e55d00ca66153fb62d3ecf21b0` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_dispatch_rechecks_registered_sources_and_closed_attempts is component evidence; no inspected live blocking-queue expiry boundary with actual remote nonexecution. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** One100k source yields either authentic external payment or99k fallback; if decision first, node execution count0. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-ADMISSION`.

<a id="ml020"></a>

## ML020 — Same-hash open attempt is used to bypass admission for different inputs

**A. Title:** Same-hash open attempt is used to bypass admission for different inputs.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** One open attempt exists; caller presents a different same-hash grant with expired backing.

**E. Event chain and suspected mechanism:** initiate_lightning_payment derives admitted_attempt from hash, skips early new-input check, then later source binding must reject mismatch.

**F. Expected correct behavior:** Retry exemption must apply to exact original source set and immutable attempt.

**G. Potential incorrect outcome:** Expired alternate source could start new obligation or become falsely attached to prior success; operator or sender loses principal.

**H. Evidence and inference:** E10 initiate_lightning_payment hash-level admitted_attempt; E11 store/admit current locked guards. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E10: [server/src/ln/mod.rs:207](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:207), `request_lightning_pay_htlc_cosign`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: New grant admission and duration bound; idempotent old spends handled separately.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

**I. Current coverage:** Dispatch component checks closed attempts; no inspected actual same-hash changed-input expired retry. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Verify unchanged attempt input set, node dispatch1 maximum and alternate source99k recovery; no new expired financial promise. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-ADMISSION`.

<a id="ml021"></a>

## ML021 — Lock wait lets an admitted HTLC deadline pass before grant commitment

**A. Title:** Lock wait lets an admitted HTLC deadline pass before grant commitment.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** A request has deadline D greater than the initial tip; it waits on a source lock until tip >= D, while backing expiry E is still in the future.

**E. Event chain and suspected mechanism:** Pre-lock duration validation passed. The locked path rechecks backing expiry but may not recheck that requested HTLC D remains usable, committing an immediately timeoutable obligation.

**F. Expected correct behavior:** At commitment either preserve a valid admitted HTLC deadline or reject before creating a new economic obligation; advancing tip shortens remaining duration.

**G. Potential incorrect outcome:** A new payment/grant may race an already-available timeout, risking failed completion or disputed refund. The suspected issue is a passed deadline, not an increased maximum duration.

**H. Evidence and inference:** E10 validate_htlc_duration; E11 locked current input expiry check; E09 receive grant. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E10: [server/src/ln/mod.rs:207](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:207), `request_lightning_pay_htlc_cosign`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: New grant admission and duration bound; idempotent old spends handled separately.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

Test reference: [expiry_refuses_unbounded_requested_lightning_duration](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_admission.rs:12), file SHA256 `6f5ea04b93dac7dc274d64b387bc4cc606bbc10d660b594e07bc9a5c126990cd` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_refuses_unbounded_requested_lightning_duration tests explicit oversized request, not lock-wait tip movement or config change. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Record initial/commit tips and D < E. No new dispatch or customer charge after an unusable deadline; any committed obligation has exactly one independently verified resolution. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-ADMISSION`.

<a id="ml022"></a>

## ML022 — Recovery retry is rejected as a new expired Lightning grant

**A. Title:** Recovery retry is rejected as a new expired Lightning grant.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Already signed original grant/revocation/receive claim is legitimately recoverable after backing expiry.

**E. Event chain and suspected mechanism:** New-grant admission check is applied to identical old binding replay; user cannot obtain lost signatures or materialize paid claim.

**F. Expected correct behavior:** Refuse only new obligations; identical safe recovery remains possible under decision fences.

**G. Potential incorrect outcome:** Customer100k is locked without usable signed successor; permanent absence removes the only replay actor.

**H. Evidence and inference:** E10 retry checks; E38 new_lightning_send logic; E15 cooperative recovery. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E10: [server/src/ln/mod.rs:207](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:207), `request_lightning_pay_htlc_cosign`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: New grant admission and duration bound; idempotent old spends handled separately.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

Test reference: [expiry_uninitiated_grant_can_revoke_after_backing_expiry](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_operation_races.rs:226), file SHA256 `ea363782dd0f7d9b92a90f5a3b807b17deb53bb626c09f46b0634af64a1dbdac` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_uninitiated_grant_can_revoke_after_backing_expiry proves one source revocation; not expired receive reply-loss or mixed-input idempotent grant. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Original signed transaction/output IDs unchanged on replay; no new LN dispatch; confirmed99k sender or rightful recipient fallback once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-ADMISSION`.

<a id="ml023"></a>

## ML023 — Generic Ark or mailbox flow bypasses HTLC-specific ownership rules

**A. Title:** Generic Ark or mailbox flow bypasses HTLC-specific ownership rules.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** A conditional send/receive coin is presented to ordinary transfer/import/round/offboard code.

**E. Event chain and suspected mechanism:** Generic writer treats HTLC as ordinary Pubkey, releasing value without authentic payment terminality.

**F. Expected correct behavior:** Only proper claim/revoke paths may alter conditional obligation; actual refund/success evidence retained.

**G. Potential incorrect outcome:** Recipient may receive LN plus generic Ark refund, or original owner may lose conditional funds to another user.

**H. Evidence and inference:** E38 cosign_oor policy exclusions; E22 shared write fence; E15 revocation. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

Test reference: [refuse_generic_spends_of_htlc_send_vtxo_while_payment_in_flight](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:802), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

Test reference: [refuse_htlc_send_vtxo_posted_to_arkoor_mailbox](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:928), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** refuse_generic_spends_of_htlc_send_vtxo_while_payment_in_flight and refuse_htlc_send_vtxo_posted_to_arkoor_mailbox are source-named guards; full fallback-at-expiry race not established by those names. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** All ordinary endpoints reject inappropriate conditional spend; whole lineage value100k has one valid terminal beneficiary. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-ADMISSION`.

<a id="ml024"></a>

## ML024 — Payout decision races a receive claim after collection starts

**A. Title:** Payout decision races a receive claim after collection starts.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Receive grant has bound destination/preimage while fallback prepares the same source.

**E. Event chain and suspected mechanism:** Phase1 saw older state; claim collects invoice then attempts tree update while decision commits its prior proposal.

**F. Expected correct behavior:** One locked commitment wins; collected entitlement cannot be erased or simultaneously paid on two paths.

**G. Potential incorrect outcome:** Customer may receive no signed claim after payer paid, or both claim and fallback; operator could receive recipient principal.

**H. Evidence and inference:** E07 source updated_at; E08 collection ordering; E23 locked resolver revalidation. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

Test reference: [expiry_coin_fence_serializes_actual_tree_writes](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_races.rs:15), file SHA256 `c101bb783b275514d06785623567ff950c7552df221f4bf7f09e8ea5db3930cd` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** Registered receive fallback stops after full claim; expiry_coin_fence_serializes_actual_tree_writes is a lower-level fence fixture, not this genuine collect-versus-decision operation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe actual hold Paid, original source ID, binding output and canonical payouts across race; gross=net+fee once, no extra claim/fallback. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-RACE`.

<a id="ml025"></a>

## ML025 — Pool selection changes to expired backing before receive signatures

**A. Title:** Pool selection changes to expired backing before receive signatures.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Pool has fresh and near-expiry outputs; selection/reservation races chain advance or another allocation.

**E. Event chain and suspected mechanism:** Preflight validates candidate A but signing uses locked B or a now-expired source; receive creates a new obligation against unusable backing.

**F. Expected correct behavior:** Check actual selected locked pool inputs at signature release, not advertised pool readiness.

**G. Potential incorrect outcome:** Inbound payer can pay while receiver grant cannot safely exit or fall back; operator may owe unbacked value.

**H. Evidence and inference:** E09 prepare_lightning_claim; E22 guard_new_lightning_inputs; server/src/vtxopool.rs callback boundary. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

**I. Current coverage:** Normal receive fixture confirms pool first and has ample lifetime; no retained actual selection/expiry contention race. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Tie signed grant ancestry to exact actual pool input and locked tip; if E<=tip, no grant/signature and inbound payer refunded. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-POOL`.

<a id="ml026"></a>

## ML026 — Two processes prepare two receive grant sets for one invoice

**A. Title:** Two processes prepare two receive grant sets for one invoice.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Two server processes share one Accepted subscription with separate in-memory hash guards.

**E. Event chain and suspected mechanism:** Each allocates a different pool set before subscription transition; database callback must reject second atomically.

**F. Expected correct behavior:** Exactly one complete signed grant set may leave the server for one funded invoice.

**G. Potential incorrect outcome:** Receiver can claim twice; operator loses a second invoice principal, or discarded pool claims stay stranded.

**H. Evidence and inference:** E09 prepare_lightning_claim comment and process-local lock; E11 database subscription updater. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

Test reference: [server_concurrent_prepare_lightning_claim](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:1452), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** server_concurrent_prepare_lightning_claim exercises tasks in one server; it cannot establish cross-process hash-lock exclusion. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Enumerate all signed grants and pool spends for invoice10k across processes; total customer proceeds equals10k minus one receive fee, not20k. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CONCURRENCY`.

<a id="ml027"></a>

## ML027 — Signed receive grants commit but their only reply is lost

**A. Title:** Signed receive grants commit but their only reply is lost.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Pool cosigning and signed VTXO construction succeeded; the pool/subscription transaction commits HtlcsReady. Provider has not yet received the signed grant response.

**E. Event chain and suspected mechanism:** The reply is lost and provider never retries. Signed grants remain durably allocated, but no recipient claim is registered. Validate identical retrieval and autonomous cancellation/timeout after permanent absence.

**F. Expected correct behavior:** Return exactly the same signed grants on retry; absent a retry, retire conditional obligations safely and return pool value without losing any actually collected entitlement.

**G. Potential incorrect outcome:** Pool principal may stay reserved indefinitely or be counted incorrectly as a final received customer claim. A lost reply alone does not prove the invoice paid.

**H. Evidence and inference:** E09 prepare_lightning_claim; server/src/vtxopool.rs cosign_both/build_signed precede DB write. This is a lost signed-response boundary; E38 ordinary commit-before-sign behavior must not be imported into this pool path. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

Test reference: [server_returned_htlc_recv_vtxos_identical](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:1377), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** server_returned_htlc_recv_vtxos_identical covers retrieval identity; no retained permanent-provider-stop immediately after pool/subscription commit with later independent timeout/conservation audit. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Inspect actual signed grants, their outpoints and invoice terminal status. Retries return identical grants; without retry, conditional pool value eventually recovers and any actual paid 10,000-sat right remains accounted. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-RECEIVE-GRANT`.

<a id="ml028"></a>

## ML028 — Receive fee changes between invoice acceptance and grant issuance

**A. Title:** Receive fee changes between invoice acceptance and grant issuance.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Invoice amount is 10,000 sat and a prior client/provider quote is demonstrably binding under the accepted receive-fee contract; configured fee changes before grant issuance. Whether that earlier binding quote exists is unresolved.

**E. Event chain and suspected mechanism:** prepare_lightning_claim calculates the live configured fee. Compare the signed net against the independently retained binding quote; a mismatch matters only if the policy promised an earlier net.

**F. Expected correct behavior:** Apply the accepted fee policy consistently and expose exact net. This research does not invent an immutable quote requirement absent an owner ruling.

**G. Potential incorrect outcome:** If an earlier net was contractually accepted, recipient may be underpaid or its funded receive may become unclaimable; otherwise this is permitted live fee selection.

**H. Evidence and inference:** E09 computes current configured fee; E17 validates outputs; E26 separate fallback fee. Missing evidence: an accepted immutable earlier quote or policy requiring one. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** Registered receive test explicitly asserts configured fee0; no retained nonzero receive-fee configuration-change boundary. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently quote invoice10k, actual receive fee f and signed proceeds10k-f; eventual payout10k-f-1000 per actual output, no extra deductions. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-FEES`.

<a id="ml029"></a>

## ML029 — Canceled internal payment with zero receiver grants never refunds offline sender

**A. Title:** Canceled internal payment with zero receiver grants never refunds offline sender.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** 100k sender funds Accepted internal subscription; receiver never prepares grants; both clients absent.

**E. Event chain and suspected mechanism:** Native setup timeout cancels subscription and may fail attempt; durable internal funding still classifies source InternalCommitted forever.

**F. Expected correct behavior:** After source-specific proof of no receiver grant or external payment, autonomously refund99k.

**G. Potential incorrect outcome:** Sender100k remains locked indefinitely; progressing to user-only timeout leaf may remove server-only recovery and require original sender.

**H. Evidence and inference:** E06 lightning_source_state; E05 refund accepts only Unattempted/Failed; E13 promotion; LIGHTNING_REVIEW L01 and hold.rs timeout path. E48. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

E48: [server/src/ln/cln/hold.rs:225](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/hold.rs:225), `cancel_reason`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5625df4688feaa0efb27555153d919e35376b1162709497af88096d428bb6d0b`. Code fact: Hold lifecycle has Accepted setup timeout and HtlcsReady expiry cancellation; internal funding markers live in separate fork tables.

**I. Current coverage:** Normal internal binding test actively claims; external failed-send tests do not exercise internal cancellation/zero-grant source closure. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Keep both actors stopped; enumerate grants0 and node payments0; require99k confirmed to sender and no operator confiscation. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-INTERNAL-CLOSURE`.

<a id="ml030"></a>

## ML030 — Expired unclaimed internal receive grants never release funded sender

**A. Title:** Expired unclaimed internal receive grants never release funded sender.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Internal sender100k funds signed receiver grants; receiver never reveals preimage; every grant safely expires.

**E. Event chain and suspected mechanism:** htlc_recvs_expired/native cancellation becomes true, but funding record still returns InternalCommitted and no autonomous sender refund is selected.

**F. Expected correct behavior:** Prove all signed receiver grants irrevocably unclaimable, then pay rightful sender once.

**G. Potential incorrect outcome:** Sender waits forever despite terminated receive; unsafe bypass could instead pay sender while recipient can still claim.

**H. Evidence and inference:** E06 InternalCommitted branch; E16 all-grant expiry; E15 returning-client revocation path. E48. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E16: [server/src/ln/mod.rs:101](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:101), `htlc_recvs_expired`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: All outgoing grants must be strictly past deadline in both views; exited grants need chain Revoked.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

E48: [server/src/ln/cln/hold.rs:225](/home/francis/abandon-ship-fresh-integration/server/src/ln/cln/hold.rs:225), `cancel_reason`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5625df4688feaa0efb27555153d919e35376b1162709497af88096d428bb6d0b`. Code fact: Hold lifecycle has Accepted setup timeout and HtlcsReady expiry cancellation; internal funding markers live in separate fork tables.

Test reference: [intra_ark_revoke_refused_until_htlc_recvs_expire](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:703), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** intra_ark_revoke_refused_until_htlc_recvs_expire checks cooperative sender recovery; no retained both-clients-absent automatic closure. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Audit every outgoing grant and completed watchman view; once no recipient can claim,99k sender payout confirms without any wallet call. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-INTERNAL-CLOSURE`.

<a id="ml031"></a>

## ML031 — Internal attempt becomes final before recipient claim materializes

**A. Title:** Internal attempt becomes final before recipient claim materializes.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current plus uncommitted internal overlay.

**D. Context and financial state:** Recipient binding and preimage are durable; internal attempt reaches Succeeded; no final output exists yet.

**E. Event chain and suspected mechanism:** Provider stops before tree update; c52056a4c replay defers final attempt when own_claim_complete=false; maintenance requires already fulfilled grants.

**F. Expected correct behavior:** Autonomously preserve and pay the bound recipient or complete justified resolution without requiring caller return.

**G. Potential incorrect outcome:** Sender source and receiver tree can remain unresolved forever; canceled native hold supplies no repair evidence.

**H. Evidence and inference:** E08 separated commits; E13 DeferredInternalReceive; E06 reconcile_completed_internal_receives; E07 Pending binding. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

**I. Current coverage:** Latest f464ce5c normal internal PASS exercises completed online materialization/idempotency; reply-loss case reopens provider before abandonment. The inspected overlay was subsequently committed as c52056a4c; the source bytes are unchanged. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Stop all clients before materialization; retain exact10k binding; eventual recipient9k canonical payout, sender refund0 after funded entitlement commits. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-INTERNAL-CLAIM`.

<a id="ml032"></a>

## ML032 — Canceled native hold erases a successful internal-payment entitlement

**A. Title:** Canceled native hold erases a successful internal-payment entitlement.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Intra-Ark payment intentionally cancels CLN hold while sender-side Ark funding remains binding.

**E. Event chain and suspected mechanism:** Reconciler treats native Canceled as definitive unpaid receive and releases sender or confiscates recipient grant.

**F. Expected correct behavior:** Distinguish native hold state from immutable internal funding/claim state.

**G. Potential incorrect outcome:** Sender can be refunded after recipient gets paid, or recipient loses already-funded entitlement.

**H. Evidence and inference:** E13 set_created_subscription_to_accepted cancels native hold; E07 receive_claim_settlement preserves internal funding. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

**I. Current coverage:** receive_claim_cancelled_uncollected_intent_preserves_original_grant_recovery unit covers predicates; no full offline cancellation/settlement race or rollback. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Native holdCanceled may coexist with genuine Ark-paid10k; whole sender/receiver lineage must still show exactly one correct economic transfer. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-INTERNAL-IDENTITY`.

<a id="ml033"></a>

## ML033 — Completed internal receive remains stale after lost Settled write

**A. Title:** Completed internal receive remains stale after lost Settled write.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** committed historical behavior; dirty draft prevention.

**D. Context and financial state:** Signed recipient output and fulfilled grants exist; attempt is final but subscription remains HtlcsReady or Canceled.

**E. Event chain and suspected mechanism:** Committed implementation looks only for an open attempt and tries canceled native hold; no new WAL event forces correction.

**F. Expected correct behavior:** Use durable original funding and materialization to repair stale receive state without a new caller.

**G. Potential incorrect outcome:** Recipient fallback remains Pending indefinitely despite signed entitlement; operator retains money owed.

**H. Evidence and inference:** E13 funded_internal_receive dirty correction; E06 maintenance overlay; LIGHTNING_REVIEW L13. Earlier behavior/evidence is kept separate from current protection; a historical result does not qualify the inspected release.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

**I. Current coverage:** Latest normal2/2 proves online internal path; no retained actual interruption between signed materialization and subscription status update/restart. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** With real signed recipient10k and source-specific funding retained, stale subscription repairs and recipient9k pays once while sender refund remains0. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Historical regression risk**. Root-cause family: `LN-INTERNAL-STATUS`.

<a id="ml034"></a>

## ML034 — Early shared preimage leaves an Accepted receive permanently ungrantable

**A. Title:** Early shared preimage leaves an Accepted receive permanently ungrantable.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** External receive Accepted but no grant; same hash preimage is learned from another source.

**E. Event chain and suspected mechanism:** prepare_lightning_claim refuses settled hash; hold settler skips non-HtlcsReady subscription; legitimate inbound must actually time out rather than be reported received.

**F. Expected correct behavior:** Do not collect without recipient entitlement; ensure cancellation/refund and no false customer credit.

**G. Potential incorrect outcome:** Payer funds remain held until timeout; incorrect status can hide refund or mint customer payout without collection.

**H. Evidence and inference:** E09 settled-hash refusal; E13 try_settle_hold_invoice Accepted skip. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

Test reference: [settled_hash_cannot_be_granted_htlc_recv_vtxos](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:1834), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** settled_hash_cannot_be_granted_htlc_recv_vtxos is admission evidence; no full absent backend/hold timeout/whole-lineage reconciliation inspected. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently follow original inbound10k: either real recipient entitlement funded once or payer10k returned; grant0 if refused, no operator windfall. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-INTERNAL-IDENTITY`.

<a id="ml035"></a>

## ML035 — Canceled uncollected claim intent is treated as collected receive

**A. Title:** Canceled uncollected claim intent is treated as collected receive.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Valid unsigned destination binding exists but external incoming HTLCs failed back before collection.

**E. Event chain and suspected mechanism:** Shared preimage remains known; resolver incorrectly promotes intent or provider grant to customer payment.

**F. Expected correct behavior:** Intent alone creates no paid external principal; use actual subscription collection and preserve safe cancellation.

**G. Potential incorrect outcome:** Operator pays recipient despite payer refund; or provider receives an incorrect refund from an unfunded grant.

**H. Evidence and inference:** E07 receive_claim_settlement; E05 cancelled_receive_claim suppresses preimage shortcut. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

**I. Current coverage:** receive_claim_cancelled_uncollected_intent_preserves_original_grant_recovery is predicate unit, not actual hold-cancel/preimage/materialization interleaving. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Original payer refund10k, recipient/provider payout0, pool backing entirely conserved; no new entitlement merely from stored destination. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-INTENT`.

<a id="ml036"></a>

## ML036 — Collected external receive stops before any final signed claim exists

**A. Title:** Collected external receive stops before any final signed claim exists.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** External hold Paid; validated final-recipient binding exists; tree update/signing never completes.

**E. Event chain and suspected mechanism:** Provider disappears between collection and materialization; only unsigned destination intent and original grant back the debt.

**F. Expected correct behavior:** Pay bound registered recipient from its actual conserving source, without original provider returning.

**G. Potential incorrect outcome:** A defect in reconstruction or attribution could leave the bound recipient unpaid or pay the provider. Current code explicitly supports Settled external receive ownership before materialization, so passive absence alone is not a demonstrated defect.

**H. Evidence and inference:** E07 external Paid + durable final binding resolves Settled even when materialized=false; E05 pays bound recipient; E08 collection precedes cosign. These are positive source protections; missing native permanent-stop evidence supports validation, not a proven missing branch. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

**I. Current coverage:** Batch14 proves POSTregistered Delivering; own reply-loss case reopens client before disappearance. No retained permanent-stop at collection-before-materialization. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** With actual holdPaid10k and materialized=false, recipient9k must confirm; provider0, duplicate0, whole pool root conserves. Stored intent must never be called signed receipt. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-INTENT`.

<a id="ml037"></a>

## ML037 — Hold settles remotely but subscription settlement reply is lost

**A. Title:** Hold settles remotely but subscription settlement reply is lost.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Preimage and binding durable; original hold node accepted settle but product status update did not commit.

**E. Event chain and suspected mechanism:** Transport returns unknown; caller stops; server must query exact original hold and durably record Paid without recollecting or canceling it.

**F. Expected correct behavior:** Autonomous original-subscription reconciliation, followed by rightful fallback.

**G. Potential incorrect outcome:** Receiver9k remains held as Pending forever; classifying unknown as Canceled can redirect owed principal.

**H. Evidence and inference:** E13 settle_invoice is_hold_invoice_paid fallback; E08 separated status/tree commit. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

**I. Current coverage:** Historical fallback_collected_lightning_receive_pays_after_status_recovers exists; fresh registered-flow pass does not inject ambiguous original hold response. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe original node Paid and payer debit, then recipient9k once without provider replay; status alone is not oracle. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-HOLD-RECONCILE`.

<a id="ml038"></a>

## ML038 — One uncollectible external receive blocks every later preimage

**A. Title:** One uncollectible external receive blocks every later preimage.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Hold-settler stream itemA belongs to permanently unavailable node; healthyB has imminent inbound expiry.

**E. Event chain and suspected mechanism:** run_hold_settler retriesA forever before readingB; B deadline passes while its stored preimage waits.

**F. Expected correct behavior:** Independent invoices must settle or safely resolve without waiting on unrelated node failure.

**G. Potential incorrect outcome:** B recipient loses timely collection/fallback or operator pays an uncollected grant; queued liabilities can grow without bound.

**H. Evidence and inference:** E13 run_hold_settler serial retry and original node requirement. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

**I. Current coverage:** Normal single-invoice tests and internal deferred no-op handling do not prove unavailable externalA/healthyB isolation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** KeepA unavailable, proveB original inbound is collected before actual deadline and rightful proceeds paid;A remains accounted, not silently skipped. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-HOLD-QUEUE`.

<a id="ml039"></a>

## ML039 — Legacy forwarded receives have no final-recipient binding after migration

**A. Title:** Legacy forwarded receives have no final-recipient binding after migration.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** historical data migrating to current.

**D. Context and financial state:** Pre-V11 successful forwarded claim left unregistered final output and provider-key checkpoint; provider absent at upgrade.

**E. Event chain and suspected mechanism:** V11 creates empty binding table; generic unregistered input-owner rule still selects provider when no historical binding exists.

**F. Expected correct behavior:** Preserve evidence of actual original recipient and migrate/recover entitlement without guessing from shared hash.

**G. Potential incorrect outcome:** Provider may receive recipient money, or migrated liability blocks forever if original recipient proof is gone.

**H. Evidence and inference:** E07 V11 new-only storage; E05 unregistered fallback; E39 historical forwarding semantics. Historical context/positive guard does not establish an earlier actual failing execution; classify the proposed reachability conservatively as a hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E39: [server/src/database/expiry_ownership.rs:394](/home/francis/sweep-to-payout-src/server/src/database/expiry_ownership.rs:394), `send_ownership`, repository `/home/francis/sweep-to-payout-src`, HEAD `994aede0ca7b2631e65ec3857f48b71888e0316f`; exact read bytes SHA256 `b45b467f5a4484ce997f796cbd7683415e74f0281d903c314a2c45822d5e9c77`. Code fact: Historical B treats any same-hash preimage/succeeded attempt as paid send; receive ownership also uses hash-level evidence.

**I. Current coverage:** New receive-binding and batch14 cases start with V11-aware server; no authentic pre-V11 forwarding backup migration was qualified. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Use old genuine invoice/claim artifact, independently known recipient script; recipient net paid once, provider0 over entire lineage. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `LN-LEGACY`.

<a id="ml040"></a>

## ML040 — Provider checkpoint key overrides final forwarded recipient

**A. Title:** Provider checkpoint key overrides final forwarded recipient.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** historical generic rule; current guarded boundary.

**D. Context and financial state:** Receive materializes an Unregistered recipient output through provider-owned checkpoint before final signed registration.

**E. Event chain and suspected mechanism:** Generic immediate-input-owner rule attributes leaf to provider unless exact receive binding is selected first.

**F. Expected correct behavior:** Bind receive proceeds to actual validated final recipient, independently of checkpoint custody.

**G. Potential incorrect outcome:** Provider receives recipient9k, rightful absent customer loses principal after backing sweep.

**H. Evidence and inference:** E05 receive binding before Unregistered branch; E07 final output equality; E38 materialization. Historical context/positive guard does not establish an earlier actual failing execution; classify the proposed reachability conservatively as a hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** receive_claim_uses_final_recipient_instead_of_provider_checkpoint proves builder binding; batch14 stops after registered Spendable, not permanent absence while Unregistered. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Original recipient10k leaf, provider checkpoint distinct; actual recipient9k canonical output and provider0 before/after original grant deadline. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-OWNER`.

<a id="ml041"></a>

## ML041 — Identical receive retry changes incidental signatures and loses its binding

**A. Title:** Identical receive retry changes incidental signatures and loses its binding.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Destination/source/amount are unchanged; provider retries after lost reply with fresh signing nonces.

**E. Event chain and suspected mechanism:** Bytewise or output-identity comparison rejects economically identical claim; stored binding vetoes every later attempt.

**F. Expected correct behavior:** Accept exact economic retry without redirecting beneficiary; never create another entitlement.

**G. Potential incorrect outcome:** Recipient10k remains paid-in but unmaterialized; absent provider cannot repair incompatible retry identity.

**H. Evidence and inference:** E07 same_claim_output ignores signatures and checks immutable geometry; E08 claim replay. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

**I. Current coverage:** receive_claim_retry_ignores_signature_upload_but_not_destination is unit evidence; not repeated real provider restarts with fresh nonces before registration. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** One binding/output ID and one recipient9k; prove retry cannot produce a second principal or permanently reject identical intended output. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-IDEMPOTENCY`.

<a id="ml042"></a>

## ML042 — Receive retry redirects a destination already committed before collection

**A. Title:** Receive retry redirects a destination already committed before collection.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** First validated claim binds recipientA; reply lost; retry requestsB for same granted sources.

**E. Event chain and suspected mechanism:** Mutable receive destination or insufficient binding comparison overwritesA after external collection.

**F. Expected correct behavior:** Immutable original validated claim wins; changed beneficiary refused before new signatures.

**G. Potential incorrect outcome:** B/provider receives money owed toA; original binding evidence may be overwritten and recovery impossible.

**H. Evidence and inference:** E07 prepare_receive_claims conflict comparison and same_claim_output; E08 binding before settle. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

**I. Current coverage:** Binding comparison unit covers unequal outputs; batch14 does not attempt redirected retry across a real collection interruption. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Retain original binding bytes/key, actual payer10k debit; onlyA9k canonical payout, B0 and exactly one source consumption. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-IDEMPOTENCY`.

<a id="ml043"></a>

## ML043 — Aggregate internal completion waits forever on a differently resolved grant

**A. Title:** Aggregate internal completion waits forever on a differently resolved grant.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** One invoice uses multiple grants; some fulfill offchain, another resolves validly onchain or lacks materialization after interruption.

**E. Event chain and suspected mechanism:** claim_complete requires every outgoing grant offchain Fulfilled; independently paid leaves can remain blocked by aggregate subscription Pending.

**F. Expected correct behavior:** Recognize every authentic per-source terminal fate and preserve total original recipient entitlement.

**G. Potential incorrect outcome:** A correctly funded portion remains indefinitely unpaid or entire subscription stalls despite known terminal evidence.

**H. Evidence and inference:** E06 INTERNAL_RECEIVE_FUNDING and all-offchain predicate; E07 subscription-level settlement gate; E16 chain semantics. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E16: [server/src/ln/mod.rs:101](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:101), `htlc_recvs_expired`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: All outgoing grants must be strictly past deadline in both views; exited grants need chain Revoked.

**I. Current coverage:** Normal internal test covers full online materialization; single-package atomicity prevents partial writes, so assumption is mixed chain/offchain or legacy partial state, not arbitrary transaction tearing. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Enumerate each grant amount/outpoint and resolution; recipient total equals original net with correct per-output charges, no refunded source also pays recipient. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-INTERNAL-AGGREGATE`.

<a id="ml044"></a>

## ML044 — Offchain internal discharge ignores a contradictory canonical chain resolution

**A. Title:** Offchain internal discharge ignores a contradictory canonical chain resolution.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Receive grants show offchain Fulfilled while canonical chain has a different terminal resolution after timing dispute/reorg.

**E. Event chain and suspected mechanism:** internal_claimed predicate reads only offchain fields; source discharged even though upstream Htlc::resolution gives chain priority.

**F. Expected correct behavior:** Resolve effective chain/offchain obligations consistently and preserve binding signed successor rights.

**G. Potential incorrect outcome:** Sender may be denied refund with unpaid recipient, or both a chain refund and recipient claim persist across different roots.

**H. Evidence and inference:** E06 offchain-only completeness; E05 HtlcChain path; database/htlc_vtxo.rs::resolution. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

**I. Current coverage:** Normal internal and ordinary payout reorg tests do not construct cross-tree contradictory effective HTLC resolutions. Assumption: such jointly stored state is reachable. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Match canonical witnesses and every signed successor to exactly one rightful principal; no reliance on offchain flag alone. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-INTERNAL-CHAIN`.

<a id="ml045"></a>

## ML045 — Obsolete receive binding overrides a later legitimate recipient transfer

**A. Title:** Obsolete receive binding overrides a later legitimate recipient transfer.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Recipient received and registered10k, then transfers to another registered user before expiry.

**E. Event chain and suspected mechanism:** Resolver follows old receive binding by ancestor/output ID rather than current economic terminal.

**F. Expected correct behavior:** Current legitimate registered descendant owns value; old binding must not freeze historical recipient forever.

**G. Potential incorrect outcome:** Original receiver receives fallback after paying it onward; new owner loses principal or operator pays twice.

**H. Evidence and inference:** E05 binding queried only for Unregistered/receive policies; E25 economic_cut current transitions. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

**I. Current coverage:** Batch14 recipient remains absent and never transfers; current-cut transfer regression uses ordinary coins, not receive-bound descendants. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual final transferee9k payout, original receiver/provider0, source input counted once across transfer chain. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLAIM-DESCENDANT`.

<a id="ml046"></a>

## ML046 — Receive pool change is counted as customer swap principal

**A. Title:** Receive pool change is counted as customer swap principal.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** 10k receive draws100k pool source and leaves90k operator change plus anchors.

**E. Event chain and suspected mechanism:** Attribution groups all descendants of grant subscription/anchor as customer money rather than exact final amount.

**F. Expected correct behavior:** Partition grant principal, operator pool change, receive fee and anchors independently.

**G. Potential incorrect outcome:** Recipient/provider overpaid by operator change; other pool customers lose reserved backing.

**H. Evidence and inference:** E24 partition; E25 economic_cut; E05 Pool disposition; E09 fee-net grant. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

**I. Current coverage:** Batch14 audits its whole root and operator residual with one pool input; no multi-input change-reuse/concurrent-receive conservation execution inspected. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently sum every100k source output; customer10k minus actual fees only, remaining operator/pool value explicit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-POOL-PARTITION`.

<a id="ml047"></a>

## ML047 — Ancestor HTLC refund is paid after the signed recipient leaf already paid

**A. Title:** Ancestor HTLC refund is paid after the signed recipient leaf already paid.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Successful receive produced registered recipient10k; original HTLC deadline is later than backing expiry.

**E. Event chain and suspected mechanism:** Leaf fallback pays9k; later deadline evaluation independently treats ancestor as refundable provider/customer liability.

**F. Expected correct behavior:** Ancestor and descendant are one conserved entitlement path; never duplicate customer principal.

**G. Potential incorrect outcome:** Provider or prior owner gets an extra refund after rightful recipient paid; operator or sibling backing funds duplicate.

**H. Evidence and inference:** E24 disjoint cut; E05 terminal has registered descendants; E06 source evidence. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

**I. Current coverage:** Batch14 explicitly rechecks whole root after original HTLC deadline and excludes provider refund; it proves one10k/one-input lineage, not cross-anchor refresh successors. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Across all grant, checkpoint, replacement and final leaf outpoints: recipient9k once, ancestor customer refund0 and whole-root balance conserved. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-LINEAGE`.

<a id="ml048"></a>

## ML048 — Fragmented receive net is dust-forfeited before same-script aggregation

**A. Title:** Fragmented receive net is dust-forfeited before same-script aggregation.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** One actually collected inbound amount, after its authorized receive fee, produces two ready 1,200-sat recipient entitlements with identical payout script and expiry; both fit one actual payout output.

**E. Event chain and suspected mechanism:** If settlement applies the 1,000-sat fee and dust retirement to each grant fragment before grouping, both 1,200-sat fragments forfeit. Correct aggregation would pay 1,400 sat with one fee.

**F. Expected correct behavior:** Partition actual received net once, aggregate as accepted grouping policy requires, and apply the payout fee/dust to actual outputs. Independent groups legitimately committed at different readiness times require separate policy analysis.

**G. Potential incorrect outcome:** Recipient loses 2,400 sat instead of receiving 1,400 sat; history can misleadingly call a policy forfeiture payment. Reachability of premature per-grant retirement must be validated.

**H. Evidence and inference:** E09 receive-fee subtraction and grant set; E26 actual script aggregation/fee is a current protection. This exercises inbound net partition before allocator input, rather than a different dust threshold. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** Registered receive fixture uses 10,000 sat and fee zero; allocator units cover grouped amounts, not nonzero receive fee plus several grants through full inbound ownership. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Freeze invoice gross and receive fee; sum all grant entitlements to 2,400 sat, require one rightful 1,400-sat output and 1,000-sat payout fee when simultaneously ready and grouped by the accepted policy; no hidden fragment forfeiture. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-RECEIVE-FRAGMENT-FEES`.

<a id="ml049"></a>

## ML049 — Mailbox accepted forwarded outputs but lost acknowledgment triggers duplicate accounting

**A. Title:** Mailbox accepted forwarded outputs but lost acknowledgment triggers duplicate accounting.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Signed recipient outputs already posted; provider sees transport failure and retains Delivering.

**E. Event chain and suspected mechanism:** Restart reposts identical bytes; mailbox ingestion or movement creation treats replay as another receive principal.

**F. Expected correct behavior:** Transport retries may repeat; economic outputs/history/payout must deduplicate by exact signed IDs.

**G. Potential incorrect outcome:** Recipient balance or entitlements counted twice; later spending/fallback may overpay or permanently lock reconciliations.

**H. Evidence and inference:** E17 deliver_lightning_receive separate post/local finish; E38 registered IDs. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

Test reference: [qualification_native_forwarding_delivery_survives_provider_reopen](/home/francis/abandon-ship-fresh-integration/testing/tests/server/qualification_noah.rs:49), file SHA256 `4ec45e896ed5e126e1e439542f72a13f9eb45188d78b5794f5a9e81d6d9128be` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** qualification_native_forwarding_delivery_survives_provider_reopen blocks POST before upstream; it does not drop ACK after successful upstream acceptance. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** One accepted signed outputID10k, duplicate mailbox entries if any economically collapse; one9k canonical payout and one receive credit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-DELIVERY`.

<a id="ml050"></a>

## ML050 — One delivery-channel acknowledgment ends provider recovery before recipient can discover outputs

**A. Title:** One delivery-channel acknowledgment ends provider recovery before recipient can discover outputs.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Recipient address has multiple delivery methods; one succeeds without durable availability on recipient restoration path.

**E. Event chain and suspected mechanism:** AnySucceeded ends forwarding and removes active checkpoint; absent recipient never imports signed outputs; remaining mailbox unavailable.

**F. Expected correct behavior:** Attribution and seed recovery must survive transport completion without actual recipient availability.

**G. Potential incorrect outcome:** Unseen VTXO can expire with no recovery producer; permanent loss if all signed material/attribution evidence vanish.

**H. Evidence and inference:** E17 deliver_vtxos_to_address returns on AnySucceeded; E03 mailbox is candidate, not proof. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

**I. Current coverage:** Batch14 fails all mailbox posts and batch11 resumes one; no mixed-channel success/failed restoration oracle. Assumption: configured address has meaningful multiple channels. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Original signed recipient10k and registered xpub independently produce9k payout and seed spend, regardless of ACK path. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-DELIVERY`.

<a id="ml051"></a>

## ML051 — Client discloses preimage before durable irreversible-action checkpoint

**A. Title:** Client discloses preimage before durable irreversible-action checkpoint.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** historical client; current source corrected.

**D. Context and financial state:** Claim RPC can settle inbound before response; old client state remains HtlcsReady until call returns.

**E. Event chain and suspected mechanism:** Crash after wire disclosure restores pre-disclosure state; client abandons/deletes claim when deadline passes.

**F. Expected correct behavior:** Persist PreimageRevealed before any wire disclosure and retain claim material.

**G. Potential incorrect outcome:** Recipient loses already paid-in money; operator/provider may sweep as abandoned while client destroyed recovery state.

**H. Evidence and inference:** E17 current pre-wire checkpoint; client-lightning-before-wire-handoff-20261017.md records historical gap. Earlier behavior/evidence is kept separate from current protection; a historical result does not qualify the inspected release.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

Test reference: [receive_claim_reply_loss_is_durable_before_wire_and_recovers_after_reopen](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/lightning.rs:523), file SHA256 `ed1ef1a1ce76e9875b465539131b42f3cd9606840f8b20776881771ddfa9f081` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** receive_claim_reply_loss_is_durable_before_wire_and_recovers_after_reopen exercises held successful response and actual client reopen; not never-returning pre-materialization server recovery. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Inspect persisted checkpoint before first disclosure, then restart after genuine remote settlement; no abandon branch and exactly one recipient discharge. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Historical regression risk**. Root-cause family: `LN-CLIENT-COMMIT`.

<a id="ml052"></a>

## ML052 — One-use receive proof is consumed before pool allocation fails

**A. Title:** One-use receive proof is consumed before pool allocation fails.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** An inbound invoice is Accepted and recipient has one valid single-use receive token. No receive grants exist yet.

**E. Event chain and suspected mechanism:** verify_ln_receive_anti_dos commits TokenStatus::Used; subsequent fee/deadline validation or pool allocation fails. An identical retry cannot reuse the proof, and the original provider stops.

**F. Expected correct behavior:** Failure must not leave an accepted paid/conditional receive unrecoverable behind consumed admission state; safely cancel/refund incoming conditional value or preserve a supported recovery path.

**G. Potential incorrect outcome:** Inbound HTLC liquidity can remain locked until cancellation; an actually collected entitlement must not disappear. Permanent customer loss is not established merely by consuming a token.

**H. Evidence and inference:** E09 calls verify_ln_receive_anti_dos before pool send; E08 same source file: verify_ln_receive_anti_dos lines 884-904 commits Used independently. No atomic token/pool transaction spans these steps. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

**I. Current coverage:** Normal receive and token validation do not fail pool allocation after token consumption and then retry with the same authentic proof. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe token, subscription, actual incoming HTLCs and grants independently. After failure, retry or autonomous cancellation reaches one rightful terminal outcome; no funded right or pool principal is lost. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-ANTIDOS-COMMIT`.

<a id="ml053"></a>

## ML053 — Separate receive input retirement and output persistence leave a zero-balance wallet

**A. Title:** Separate receive input retirement and output persistence leave a zero-balance wallet.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Server registration succeeded; local wallet marks original HTLCs spent before storing signed recipient outputs.

**E. Event chain and suspected mechanism:** Local store fails or process stops between writes; original action/checkpoint must replay without another financial payment.

**F. Expected correct behavior:** Recover same signed outputs and history deterministically; source retirement never deletes value.

**G. Potential incorrect outcome:** Wallet reports zero despite10k owned coin; seed recovery may miss it if best-effort recovery mailbox also failed.

**H. Evidence and inference:** E17 local receive writes and best-effort post_recovery_vtxo_ids. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

**I. Current coverage:** Reply-loss test interrupts RPC and reopens; no inspected storage failure exactly between mark_vtxos_as_spent and store_vtxos. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Reopen original and independently restored seed; total recoverable principal10k, one movement and eventual9k fallback; no second invoice. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CLIENT-LOCAL`.

<a id="ml054"></a>

## ML054 — Refund registration failure is ignored before locally declaring spendable refund

**A. Title:** Refund registration failure is ignored before locally declaring spendable refund.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Server cosigned sender refund but signed chain upload fails; client receives valid outputs.

**E. Event chain and suspected mechanism:** revoke_lightning_send_htlcs warns on registration failure, stores local spendable refund and marks original HTLC spent.

**F. Expected correct behavior:** Retain/retry signed registration and preserve fallback ownership/discovery if client never returns.

**G. Potential incorrect outcome:** User sees100k refund that server cannot spend; fallback may classify unregistered descendant differently or recovery cannot find it.

**H. Evidence and inference:** E18 revoke_lightning_send_htlcs warning then local state writes; E05 Unregistered input-owner. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E18: [bark/src/actions/lightning/pay.rs:152](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/pay.rs:152), `check_lightning_send_payment_status`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `55daadb631de69c322d79b767ff2808f537c55f94fe8b39858fff632169478a6`. Code fact: Expired Pending/transport error collapses to local Failed; revocation and exit still need authentic protection. Refund registration failure is warned, then local outputs stored.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

**I. Current coverage:** Cooperative same-hash refund test requires registration succeed; actual failed-send offline test bypasses cooperative refund. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** With upload outage, exact signed refundIDs remain durable; no misleading usable balance, rightful99k eventual payout and seed-only recovery of same principal. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-CLIENT-REFUND`.

<a id="ml055"></a>

## ML055 — Local expiry collapses unknown send status into Failed despite delayed success

**A. Title:** Local expiry collapses unknown send status into Failed despite delayed success.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** HTLC deadline passed locally; node may have completed while server status RPC fails or remains Pending.

**E. Event chain and suspected mechanism:** SDK maps unknown to Failed and begins revocation/exit; UI/application may treat final failure as permission for another invoice/payment.

**F. Expected correct behavior:** Unknown remains economically unresolved until authentic terminal evidence; any recovery cannot duplicate actual success.

**G. Potential incorrect outcome:** Customer pays same purchase twice, or operator refunds actual success if downstream trusts local status.

**H. Evidence and inference:** E18 check_lightning_send_payment_status expired fallback; E15 guarded revocation. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E18: [bark/src/actions/lightning/pay.rs:152](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/pay.rs:152), `check_lightning_send_payment_status`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `55daadb631de69c322d79b767ff2808f537c55f94fe8b39858fff632169478a6`. Code fact: Expired Pending/transport error collapses to local Failed; revocation and exit still need authentic protection. Refund registration failure is warned, then local outputs stored.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

**I. Current coverage:** Paid-source revoke-refusal tests protect server; no application-level retry/unknown-history audit with delayed successful first payment. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe original external90k once and every subsequent debit; SDK history must distinguish recovery intent from proven failure and source refund remains0. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-CLIENT-STATUS`.

<a id="ml056"></a>

## ML056 — Pubkey-only act-by protection omits stuck conditional receive/send coins

**A. Title:** Pubkey-only act-by protection omits stuck conditional receive/send coins.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Conditional HTLC coin is locally locked/pending; fallback server unavailable near backing expiry.

**E. Event chain and suspected mechanism:** exit_protection_schedule explicitly skips non-Pubkey; only Lightning action escalation can preserve conditional claim, which may be parked.

**F. Expected correct behavior:** Every valid conditional entitlement needs an active recoverable deadline path or independently live server fallback.

**G. Potential incorrect outcome:** Customer cannot start required exit in time; lost claim window can become permanent if server recovery also stalls.

**H. Evidence and inference:** E34 Pubkey filter; E17 receive parked state; E18 exit_lightning_send_htlcs. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E18: [bark/src/actions/lightning/pay.rs:152](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/pay.rs:152), `check_lightning_send_payment_status`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `55daadb631de69c322d79b767ff2808f537c55f94fe8b39858fff632169478a6`. Code fact: Expired Pending/transport error collapses to local Failed; revocation and exit still need authentic protection. Refund registration failure is warned, then local outputs stored.

**I. Current coverage:** Default-daemon protection test covers ordinary Pubkey coin; batch14 turns receive into Pubkey before absence. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Track each actual HTLC D and backingE under server outage; at least one valid recovery path remains funded/signable until principal discharged. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-CLIENT-PROTECTION`.

<a id="ml057"></a>

## ML057 — HTLC outlives backing and progress destroys the server refund path

**A. Title:** HTLC outlives backing and progress destroys the server refund path.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** D>E is valid policy; unresolved send source has no known preimage, sender permanently absent.

**E. Event chain and suspected mechanism:** Expiry readiness requests signed progression; tree reaches a user-timeout-only leaf where watchman cannot sign customer refund.

**F. Expected correct behavior:** Preserve a recoverable server-funded fallback obligation while awaiting terminality; progression must not strand rightful source.

**G. Potential incorrect outcome:** Sender99k can become permanently stranded at expired user-only contract despite operator retaining a bookkeeping obligation.

**H. Evidence and inference:** E04 NeedsHtlcProgress; E28 split_recovery_paths; watchman/policy.rs ServerHtlcSend Wait without preimage; LIGHTNING_REVIEW counter-path analysis. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** Requested mixed-sibling split test uses never-attempted source and checkpoints; no canceled internal funded-source progress to terminal user-only leaf with both users absent. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently follow physical frontier through D>E; retain signable exact-value recovery and eventual99k without sender signature. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-BACKING-DEADLINE`.

<a id="ml058"></a>

## ML058 — Restored watchman cursor never revisits a missing HTLC resolution

**A. Title:** Restored watchman cursor never revisits a missing HTLC resolution.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Watchman/node have resumed and are available, but a nonaligned restored cursor says past height H while the durable resolution row from H is absent.

**E. Event chain and suspected mechanism:** If completion is published from the high cursor without replaying the missing lower resolution, htlc_recvs_expired can remain false forever, or a view can incorrectly claim complete. Simply stopping the watchman is expected temporary unavailability, not this scenario.

**F. Expected correct behavior:** Rebuild missing canonical facts from durable invalidation/replay boundaries before acknowledging completion; preserve independent recovery progress.

**G. Potential incorrect outcome:** A genuinely terminated receive can veto the rightful sender refund indefinitely after service restoration, or an incorrect completed view can release money prematurely.

**H. Evidence and inference:** E16 requires min(tip,watchman_tip) and effective HTLC states; E30 completion barriers. Backup/cursor inconsistency reachability is an explicit hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E16: [server/src/ln/mod.rs:101](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:101), `htlc_recvs_expired`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: All outgoing grants must be strictly past deadline in both views; exited grants need chain Revoked.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

Test reference: [intra_ark_revoke_refused_while_watchman_is_behind](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:741), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** intra_ark_revoke_refused_while_watchman_is_behind proves refusal safety; readiness-isolation proves independent root payment, not automatic catch-up after lost watchman state. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Restore service with authentic omitted resolution/cursor mismatch; independently confirm terminal spend and replay. Sender remains owed and eventually receives 99,000 sat; no false completion while facts are absent. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-VIEW`.

<a id="ml059"></a>

## ML059 — Unrelated same-hash grants block another receive retirement predicate

**A. Title:** Unrelated same-hash grants block another receive retirement predicate.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** historical/imported state; current normal subscription uniqueness is a guard.

**D. Context and financial state:** An authentic legacy/imported dataset permits multiple subscriptions or outgoing grant sets sharing one hash but distinct sources/deadlines. Current unique payment_hash subscription constraint prevents assuming this from normal creation.

**E. Event chain and suspected mechanism:** get_htlc_vtxo_states_by_payment_hash feeds all grants to htlc_recvs_expired; one unrelated long-lived grant vetoes source refund.

**F. Expected correct behavior:** Use complete source-specific dependency closure; do not borrow or veto entitlements via unrelated hash rows.

**G. Potential incorrect outcome:** Sender funds remain frozen after its own recipient obligations terminated; malicious-looking mailbox/hash data must never divert value.

**H. Evidence and inference:** E16 hash-wide grant predicate; E15 subscription/source matching; server/schema.sql subscription unique payment_hash and fork V64 constrain current reachability. Imported version/schema must be pinned before this scenario is admitted. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E16: [server/src/ln/mod.rs:101](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:101), `htlc_recvs_expired`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: All outgoing grants must be strictly past deadline in both views; exited grants need chain Revoked.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

**I. Current coverage:** Same-hash tests use two sends, not unrelated outgoing receive grants with differing deadlines. Schema/import reachability must be validated. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Audit subscription membership and all effective resolutions; unrelated grant remains intact while rightful terminated source99k pays. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-HASH-AGGREGATE`.

<a id="ml060"></a>

## ML060 — Invoice wall-clock expiry is mistaken for incoming HTLC termination

**A. Title:** Invoice wall-clock expiry is mistaken for incoming HTLC termination.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Invoice expired after payer already locked inbound HTLCs; live contracts remain collectible before block deadline.

**E. Event chain and suspected mechanism:** Timeout/cancellation uses invoice timestamp instead of actual accepted HTLC state or stored internal funding.

**F. Expected correct behavior:** Honor already admitted binding HTLCs; only authentic terminal evidence resolves money.

**G. Potential incorrect outcome:** Incorrect payer refund plus recipient fallback duplicates funds, or valid recipient principal is canceled after successful payment.

**H. Evidence and inference:** E08 incoming-expiry claim validation; E13 hold/internal distinction; E16 grant deadline. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E08: [server/src/ln/mod.rs:928](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:928), `claim_lightning_receive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Validate grant set and deadline; bind destination, store preimage, settle native hold, then materialize/cosign. These are separate transactions.

E13: [server/src/ln/node_manager.rs:28](/home/francis/abandon-ship-fresh-integration/server/src/ln/node_manager.rs:28), `settle_invoice`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06aafa0b1641e7a8e294896876d7a3c622bce4386aad87e1fbaa8a8853d3698d`. Code fact: Native settlement and subscription update are separate; internal funding must survive coarse attempt completion. run_hold_settler serially retries nondeferred errors.

E16: [server/src/ln/mod.rs:101](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:101), `htlc_recvs_expired`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: All outgoing grants must be strictly past deadline in both views; exited grants need chain Revoked.

Test reference: [intra_ark_invoice_expiry_after_grant_does_not_allow_revoke](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:362), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** intra_ark_invoice_expiry_after_grant_does_not_allow_revoke proves one internal path; historical prepared receive after invoice expiry is not fresh multi-node liveness proof. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Keep invoiceExpired while holdAccepted; independently demonstrate one collectible10k and eventual rightful9k, no timeout-only sender refund. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-TIME`.

<a id="ml061"></a>

## ML061 — Offline Noah recipient gets an invoice that strict LNURL payers cannot pay

**A. Title:** Offline Noah recipient gets an invoice that strict LNURL payers cannot pay.

**B. Financial invariant:** The advertised offline receive has a valid admission and completion path; prepayment unavailability is reported separately from loss of funded value.

**C. Applicable implementation:** current Noah/shared API; observed controlled Signet artifact.

**D. Context and financial state:** Hosted recipient is offline; caller requests5000sat through actual Noah callback without Ark negotiation.

**E. Event chain and suspected mechanism:** Metadata GET commits exact JSON string; amount callback requests plain memo invoice; HTTP200 returns; independent strict verifier rejects missing metadata hash; no payment starts.

**F. Expected correct behavior:** Return an invoice bound to exact metadata, amount and network before claiming offline-receive support.

**G. Potential incorrect outcome:** Recipient receives nothing, payer keeps funds; observed compatibility/liveness failure, not observed loss of funded entitlement.

**H. Evidence and inference:** E19/E20; simulator docs/evidence/noah-actual-invoice-rejection-20261017.json pins Noah87b2c181 and server32ea166. The retained observation establishes only the scope explicitly described; permanent monetary loss is not inferred.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E20: [server/src/barkd_client.rs:25](/home/francis/noah-fresh-backend-20261016/server/src/barkd_client.rs:25), `create_invoice_for_address`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `65d91a1acea67f8eb7022d1a2955876a60fb7e3e89760754a66438a1363d9215`. Code fact: Provider validates network and exact amount; no metadata hash, invoice age or recipient proof is checked here; redirects disabled.

**I. Current coverage:** test_lnurlp_invoice_request_uses_barkd_without_waking_device uses fake lnbc-forwarded; actual retained callback verifier FAIL is stronger and has payment_attempted=false. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Exact original metadata hash must match original invoice description hash; only afterward can original-hash settlement, recipient claim and fallback earn credit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Observed failure**. Root-cause family: `LN-NOAH-METADATA`.

<a id="ml062"></a>

## ML062 — Misdelivered Noah request ID accepts another authenticated user invoice

**A. Title:** Misdelivered Noah request ID accepts another authenticated user invoice.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Device fallback used; request intended forAlice reachesBob due to notification/coordination fault; Bob is an existing authenticated account.

**E. Event chain and suspected mechanism:** Bob submits his valid invoice under supplied requestID; submit handler ignores auth identity; public waiter returns Bob invoice; payer may settle it.

**F. Expected correct behavior:** Bind request to intended recipient and reject unrelated account submission.

**G. Potential incorrect outcome:** Bob receives money owed toAlice; permanent loss if Bob cannot or does not return it. Request-ID misdelivery is an assumption, not observed access compromise.

**H. Evidence and inference:** E21 _auth_payload unused; E19 polling raw invoice; Noah InvoiceStore SETEX. E50. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E21: [server/src/routes/gated_api_v0.rs:225](/home/francis/noah-fresh-backend-20261016/server/src/routes/gated_api_v0.rs:225), `submit_invoice`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `15d08dc9c4046516fb0db0afef426debf3c807dbd78dfb0386dec9de25c1f1ff`. Code fact: Authenticated submit stores arbitrary invoice by caller-provided transaction ID; auth identity is unused by handler.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E50: [server/src/cache/invoice_store.rs:18](/home/francis/noah-fresh-backend-20261016/server/src/cache/invoice_store.rs:18), `store`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `2fd8570a21c55f9168714b7458596876ea59b3bf9af9ffb7e20289ab25c66a46`. Code fact: Redis SETEX60 seconds accepts caller request ID/invoice; get and remove are separate unversioned operations.

**I. Current coverage:** Auth/existing-user tests prove admission only; test_submit_invoice_stores_in_redis stores fake data without request-owner validation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Two controlled users, original amount10k: onlyAlice invoice may be returned; independent node receipt must creditAlice and Bob0. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-NOAH-REQUEST`.

<a id="ml063"></a>

## ML063 — A second device submits the amount from the wrong receive request

**A. Title:** A second device submits the amount from the wrong receive request.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Alice has concurrent10k and20k LNURL requests; device completion attaches20k invoice to10k requestID.

**E. Event chain and suspected mechanism:** Submit stores bytes without amount binding; waiting10k callback returns20k invoice; a permissive payer pays it.

**F. Expected correct behavior:** Validate exact original millisatoshi amount before returning invoice; never reinterpret request amount.

**G. Potential incorrect outcome:** Payer overpays10k or strict payer rejects and recipient misses receive; no actual incorrect payment observed.

**H. Evidence and inference:** E19 raw polling; E21 SubmitInvoicePayload; E20 amount check applies only provider path. E50. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E21: [server/src/routes/gated_api_v0.rs:225](/home/francis/noah-fresh-backend-20261016/server/src/routes/gated_api_v0.rs:225), `submit_invoice`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `15d08dc9c4046516fb0db0afef426debf3c807dbd78dfb0386dec9de25c1f1ff`. Code fact: Authenticated submit stores arbitrary invoice by caller-provided transaction ID; auth identity is unused by handler.

E20: [server/src/barkd_client.rs:25](/home/francis/noah-fresh-backend-20261016/server/src/barkd_client.rs:25), `create_invoice_for_address`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `65d91a1acea67f8eb7022d1a2955876a60fb7e3e89760754a66438a1363d9215`. Code fact: Provider validates network and exact amount; no metadata hash, invoice age or recipient proof is checked here; redirects disabled.

E50: [server/src/cache/invoice_store.rs:18](/home/francis/noah-fresh-backend-20261016/server/src/cache/invoice_store.rs:18), `store`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `2fd8570a21c55f9168714b7458596876ea59b3bf9af9ffb7e20289ab25c66a46`. Code fact: Redis SETEX60 seconds accepts caller request ID/invoice; get and remove are separate unversioned operations.

**I. Current coverage:** test_submit_invoice_overwrites_existing accepts fabricated1000-to2000 replacement; proves storage replacement, not amount correctness. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare original callback amount10000000msat with independently decoded invoice and actual debit; a mismatch cannot become payment/entitlement. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-NOAH-REQUEST`.

<a id="ml064"></a>

## ML064 — Two Noah devices overwrite one request while invoice retrieval is in progress

**A. Title:** Two Noah devices overwrite one request while invoice retrieval is in progress.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Two authorized Alice devices create invoices for one request. The payer actually receives invoice A, pays A, then an unsafe retry is authorized against distinct invoice B; both real debits are explicit assumptions, not consequences of response loss.

**E. Event chain and suspected mechanism:** Second SETEX replaces A between waiter GET and DEL. The waiter returns A but deletes B's cache entry; request retry creates/selects another payment identity instead of reconciling A's real paid state.

**F. Expected correct behavior:** One stable request-to-payment identity; all extra conditional receive actions reconciled without replaying purchase.

**G. Potential incorrect outcome:** Actual payer retry can double-pay the intended purchase; without the second real debit, this is lost request correlation/conditional receive availability, not demonstrated duplicate money.

**H. Evidence and inference:** E21 handler; E19 GET then remove; cache/invoice_store.rs unversioned SETEX/GET/DEL. E50. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E21: [server/src/routes/gated_api_v0.rs:225](/home/francis/noah-fresh-backend-20261016/server/src/routes/gated_api_v0.rs:225), `submit_invoice`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `15d08dc9c4046516fb0db0afef426debf3c807dbd78dfb0386dec9de25c1f1ff`. Code fact: Authenticated submit stores arbitrary invoice by caller-provided transaction ID; auth identity is unused by handler.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E50: [server/src/cache/invoice_store.rs:18](/home/francis/noah-fresh-backend-20261016/server/src/cache/invoice_store.rs:18), `store`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `2fd8570a21c55f9168714b7458596876ea59b3bf9af9ffb7e20289ab25c66a46`. Code fact: Redis SETEX60 seconds accepts caller request ID/invoice; get and remove are separate unversioned operations.

**I. Current coverage:** Overwrite unit is sequential and checks last bytes only; no concurrent two-device hash/payment oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Record both actual created hashes; request chooses one immutable hash, payer debit<=10k once, other hold/grant safely cancels. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-REQUEST`.

<a id="ml065"></a>

## ML065 — Stored Noah forwarding address belongs to a stale external wallet

**A. Title:** Stored Noah forwarding address belongs to a stale external wallet.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** A device restore or account-address update supplies a stale Ark address without the user intentionally authorizing forwarding to that key. The accepted identity/update policy and reachability of that unintended update are unresolved.

**E. Event chain and suspected mechanism:** Noah stores supplied address string; later offline LNURL invoice forwards to that key; signed claim/fallback follows wrong registration.

**F. Expected correct behavior:** Preserve the explicitly authorized recipient address across restore/update; intentional forwarding to a different wallet remains allowed. Resolve identity policy before declaring a stored address incorrect.

**G. Potential incorrect outcome:** Bob may receiveAlice10k or value strands under a keyAlice cannot recover. Intentional authorized forwarding is not an error.

**H. Evidence and inference:** E19 uses stored forwarding address; E51 stores supplied address. Source proves storage/use, not that intentional external forwarding violates the contract; unintended restore/update is the hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E51: [server/src/db/user_repo.rs:238](/home/francis/noah-fresh-backend-20261016/server/src/db/user_repo.rs:238), `update_lightning_identity`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `3e92f349b17faf49dbe58bfe4ca4c17de92986ab6d7f33d5830fcd751de71288`. Code fact: Hosted identity/address persistence and uniqueness are distinct from signed Ark key ownership.

**I. Current coverage:** Provider-route unit proves exact supplied string passed; no Ark key-ownership/seed-restoration binding proof. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare actual claim policy with independently declaredAlice address/seed and fallback script; reject or explicitly resolve unintended mismatch. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-IDENTITY`.

<a id="ml066"></a>

## ML066 — Hosted username is reassigned between metadata and payment callback

**A. Title:** Hosted username is reassigned between metadata and payment callback.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Payer caches metadata forAlice username; Alice renames; same username is assigned toBob before callback.

**E. Event chain and suspected mechanism:** Callback re-queries current username and obtainsBob address; metadata identifier text is unchanged; payer may payBob while believingAlice.

**F. Expected correct behavior:** Define and preserve in-flight identity binding across hosted-name changes; do not invent a policy that treats all later owners as original recipient.

**G. Potential incorrect outcome:** Bob receives10k intended forAlice; recovery may require voluntary return. Metadata hash alone does not bind previous account identity.

**H. Evidence and inference:** E19 find_by_lightning_address on each request; E21 update_lightning_identity; Noah user repo. E51. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E21: [server/src/routes/gated_api_v0.rs:225](/home/francis/noah-fresh-backend-20261016/server/src/routes/gated_api_v0.rs:225), `submit_invoice`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `15d08dc9c4046516fb0db0afef426debf3c807dbd78dfb0386dec9de25c1f1ff`. Code fact: Authenticated submit stores arbitrary invoice by caller-provided transaction ID; auth identity is unused by handler.

E51: [server/src/db/user_repo.rs:238](/home/francis/noah-fresh-backend-20261016/server/src/db/user_repo.rs:238), `update_lightning_identity`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `3e92f349b17faf49dbe58bfe4ca4c17de92986ab6d7f33d5830fcd751de71288`. Code fact: Hosted identity/address persistence and uniqueness are distinct from signed Ark key ownership.

**I. Current coverage:** Endpoint tests use unchanged user; no inspected rename/reuse between exact first and second responses. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Freeze original user identity and metadata response; after reassignment callback must preserve original obligation or reject before payer debit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-IDENTITY`.

<a id="ml067"></a>

## ML067 — Forwarded receive escalation drains recipient-bound principal to the provider

**A. Title:** Forwarded receive escalation drains recipient-bound principal to the provider.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** A receive-for-address targets offline Alice. Provider has HTLC grants and has disclosed the preimage, but recipient claim registration fails or its reply is lost. Provider uses the supported stuck-receive recovery API.

**E. Event chain and suspected mechanism:** attempt_lightning_receive_exit accepts PreimageRevealed without a claim_destination guard, exits provider-key grants, records a settled receive without destination and removes the action. Generic exit draining accepts a caller-selected provider address.

**F. Expected correct behavior:** Recovery preserves Alice's beneficiary and the forwarding obligation; collecting source grants into the provider wallet cannot itself discharge Alice.

**G. Potential incorrect outcome:** Provider may receive principal owed to Alice while her destination disappears from the terminal client record. Runtime redirection and source-ownership interaction remain unproven; server-side misclassification is separately ML180.

**H. Evidence and inference:** E53 attempt_lightning_receive_exit / exit_lightning_receive_htlcs; E54 drain_exits; E55 SettledLightningReceive omits destination. Code supports destination-blind escalation; actual funded mispayment is not observed. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E53: [bark/src/lightning/receive.rs:216](/home/francis/abandon-ship-fresh-integration/bark/src/lightning/receive.rs:216), `attempt_lightning_receive_exit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `f713f8aa16d07924de8fe358910718af3c745400be272209b54601f73c3c2fb6`. Code fact: Escalates PreimageRevealed grants without a forwarding-destination branch, permanently records settled receive and removes action.

E54: [bark/src/exit/mod.rs:47](/home/francis/abandon-ship-fresh-integration/bark/src/exit/mod.rs:47), `drain_exits`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b69b2aae81a418cec280073a1f28d2e853afeea86039e4e507baaf9c6c1ed33f`. Code fact: Claims signable exit inputs to a caller-supplied address; this generic API does not receive forwarding identity.

E55: [bark/src/persist/models.rs:222](/home/francis/abandon-ship-fresh-integration/bark/src/persist/models.rs:222), `SettledLightningReceive`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71fff6d8ba7d97902b586dfe12fe283810c6c922d81f710202812fd92cfb14ff`. Code fact: Permanent settled receive contains hash, preimage, invoice, amount and time, with no forwarding destination.

Test reference: [server_settles_invoice_from_on_chain_htlc_preimage](/home/francis/abandon-ship-fresh-integration/testing/tests/server/lightning.rs:161), file SHA256 `6408253c78252d098441b7f120b394fad1d0135135034adacf7bd6031738f716` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** bark_should_exit_a_htlc_recv_that_server_refuse_to_cosign and server_settles_invoice_from_on_chain_htlc_preimage exercise source exit/settlement for local receives only, not a distinct offline forwarded recipient. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Retain frozen Alice destination, real incoming payment and source grants. Inspect every actual exit and fallback outpoint; Alice receives the correct amount once, provider receives none of her principal, and destination survives terminal records. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-RECOVERY-BENEFICIARY`.

<a id="ml068"></a>

## ML068 — Paid cached invoice is discarded as expired when backend action is recreated

**A. Title:** Paid cached invoice is discarded as expired when backend action is recreated.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** A real payer already paid invoice H while it was valid. Noah/provider action restoration later encounters the cached invoice after its wall-clock expiry; backing/grants and recipient entitlement still exist.

**E. Event chain and suspected mechanism:** A recovery path classifies expired invoice H as unusable and creates a new invoice/action instead of reconciling H's terminal payment and signed/intent ownership. Whether such replacement is reachable must be verified; no normal provider expiry check is assumed to discharge a paid invoice.

**F. Expected correct behavior:** Invoice expiry only controls new admission; actual paid H remains a durable right through action recovery and fallback.

**G. Potential incorrect outcome:** Offline recipient's funded right can become orphaned behind an abandoned original action; a fresh unpaid invoice is no substitute for its money.

**H. Evidence and inference:** E20 provider validates network/amount; E17 durable receive recovery; E07 final-recipient intent. No observed paid-cache abandonment path; restoration/replacement is an explicit assumption. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E20: [server/src/barkd_client.rs:25](/home/francis/noah-fresh-backend-20261016/server/src/barkd_client.rs:25), `create_invoice_for_address`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `65d91a1acea67f8eb7022d1a2955876a60fb7e3e89760754a66438a1363d9215`. Code fact: Provider validates network and exact amount; no metadata hash, invoice age or recipient proof is checked here; redirects disabled.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

**I. Current coverage:** Actual Noah callback rejection is prepayment only. No retained paid invoice restored after wall-clock expiry with its original source/outpoint and autonomous fallback oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently prove H paid before expiry; after wall-clock expiry and backend restoration, its original 10,000-sat right still yields rightful 9,000-sat fallback once, without another payer debit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P2 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-INVOICE`.

<a id="ml069"></a>

## ML069 — Valid invoice response is associated with another forwarding destination

**A. Title:** Valid invoice response is associated with another forwarding destination.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** A response/cache/endpoint mix-up returns a correctly signed same-network 10,000-sat invoice for a different forwarding action or recipient. Merely using another Lightning node is insufficient: another node may correctly forward to Alice.

**E. Event chain and suspected mechanism:** Provider validates amount/network but not an application binding between returned invoice/action and frozen Alice forwarding destination. Payer pays the substituted action.

**F. Expected correct behavior:** Bind the operation to the intended forwarding beneficiary under the accepted identity contract; do not invent a requirement that every invoice use one node.

**G. Potential incorrect outcome:** Bob's actual action may receive the payment while Alice has no corresponding entitlement. Response misassociation must be demonstrated; correct cross-node forwarding is valid.

**H. Evidence and inference:** E20 check_readiness and create_invoice_for_address; E19 recipient forwarding boundary. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E20: [server/src/barkd_client.rs:25](/home/francis/noah-fresh-backend-20261016/server/src/barkd_client.rs:25), `create_invoice_for_address`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `65d91a1acea67f8eb7022d1a2955876a60fb7e3e89760754a66438a1363d9215`. Code fact: Provider validates network and exact amount; no metadata hash, invoice age or recipient proof is checked here; redirects disabled.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

**I. Current coverage:** Unit provider checks network and amount; batch14 uses exact known native server, not misconfigured Noah provider. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare actual paid hash and signed final-recipient policy to frozen Alice intent and source anchors, regardless of invoice node. Only Alice's correct claim/fallback discharges her payment. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-INVOICE`.

<a id="ml070"></a>

## ML070 — Healthy Noah backend accepts invoices while pool and watchman are unusable

**A. Title:** Healthy Noah backend accepts invoices while pool and watchman are unusable.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Backend HTTP and ArkInfo healthy; pool has no eligible confirmed source; watchman has no usable confirmed fee inputs; recipient offline.

**E. Event chain and suspected mechanism:** Invoice returned and payer locks inbound; provider cannot obtain grants before HTLC deadline; cancellation/recovery must preserve payer and recipient state.

**F. Expected correct behavior:** Operation admission must reflect actual receive funding/claim capability, and interrupted failure must return funds correctly.

**G. Potential incorrect outcome:** Inbound can remain locked then fail with no fallback, or false received state hides absence; no entitlement should be invented from invoice creation.

**H. Evidence and inference:** E20 readiness network-only; E09 pool grant; E37 confirmed fee funding; actual Noah rejected probe had zero funding and watchman off. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E20: [server/src/barkd_client.rs:25](/home/francis/noah-fresh-backend-20261016/server/src/barkd_client.rs:25), `create_invoice_for_address`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `65d91a1acea67f8eb7022d1a2955876a60fb7e3e89760754a66438a1363d9215`. Code fact: Provider validates network and exact amount; no metadata hash, invoice age or recipient proof is checked here; redirects disabled.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

**I. Current coverage:** Noah HTTP200 and strict rejection are prepayment evidence; native batch14 starts with confirmed pool and two sponsors. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Real inbound10k either safely refunded10k or becomes signed recipient proceeds with eventual9k; no paid flag from health response. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `LN-NOAH-CAPACITY`.

<a id="ml071"></a>

## ML071 — Offline backend cannot supply required anti-DoS proof after inbound acceptance

**A. Title:** Offline backend cannot supply required anti-DoS proof after inbound acceptance.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Production server requires receive proof; provider wallet lacks qualifying proof/token; payer already locked inbound10k.

**E. Event chain and suspected mechanism:** prepare_lightning_claim rejects proof after invoice acceptance; provider parks retries; neither absent recipient nor backend can complete grant.

**F. Expected correct behavior:** Qualify proof-required receive operation before advertising capability, and cancel/refund authentic failed inbound safely.

**G. Potential incorrect outcome:** Customer never gets swap/fallback; payer can remain held to deadline; if provider marks received, valid complaint/entitlement disappears.

**H. Evidence and inference:** E09 verify_ln_receive_anti_dos before pool grant; E17 compute_lightning_receive_anti_dos. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E09: [server/src/ln/mod.rs:763](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:763), `prepare_lightning_claim`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Process-local hash guard; allocate actual pool grant set and subscription update together; refuse previously settled hash.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

**I. Current coverage:** Registered forwarding fixture explicitly sets ln_receive_anti_dos_required=false; it provides no token/proof-required coverage. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual required-proof profile, original inbound hash and deadline: valid proof leads real recipient9k, absent proof returns payer10k without false receipt. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-ADMISSION`.

<a id="ml072"></a>

## ML072 — LNURL metadata changes serialization between first response and invoice commitment

**A. Title:** LNURL metadata changes serialization between first response and invoice commitment.

**B. Financial invariant:** The advertised offline receive has a valid admission and completion path; prepayment unavailability is reported separately from loss of funded value.

**C. Applicable implementation:** draft future integration; current feature absent.

**D. Context and financial state:** Future compatible provider hashes metadata; two requests use equivalent text but different JSON byte serialization or identifier normalization.

**E. Event chain and suspected mechanism:** Invoice binds recomputed bytes rather than exact originally advertised UTF8 string; strict payer rejects; a relaxed checker may bind another identity.

**F. Expected correct behavior:** Use exact original metadata bytes consistently; preserve immutable failed originals.

**G. Potential incorrect outcome:** Receive unavailable or identity assurance lost; no automatic funded loss shown. This is a future compatibility regression scenario after current missing-hash blocker is fixed.

**H. Evidence and inference:** E19 metadata.to_string; E20 description-only current interface; simulator docs/noah-metadata-hash-support.md. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E19: [server/src/routes/public_api_v0.rs:350](/home/francis/noah-fresh-backend-20261016/server/src/routes/public_api_v0.rs:350), `lnurlp_request`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `a4aec467fff068c603045093a0eabce16accd4fb4f91dbc93fc99f831b865209`. Code fact: Metadata is serialized JSON string; provider receives a plain memo. Each callback re-reads user; timeout errors fall through to push path.

E20: [server/src/barkd_client.rs:25](/home/francis/noah-fresh-backend-20261016/server/src/barkd_client.rs:25), `create_invoice_for_address`, repository `/home/francis/noah-fresh-backend-20261016`, HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; exact read bytes SHA256 `65d91a1acea67f8eb7022d1a2955876a60fb7e3e89760754a66438a1363d9215`. Code fact: Provider validates network and exact amount; no metadata hash, invoice age or recipient proof is checked here; redirects disabled.

**I. Current coverage:** Actual callback fails missing hash; no current positive exact-byte metadata-binding test exists. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent SHA256 of unchanged advertised string equals invoice hash; changed whitespace/order/identifier must not be silently accepted. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P2 — Hypothesis requiring validation**. Root-cause family: `LN-NOAH-METADATA`.

<a id="ml073"></a>

## ML073 — Legacy failed send has no source-provenance markers after upgrade

**A. Title:** Legacy failed send has no source-provenance markers after upgrade.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** historical data plus current migration.

**D. Context and financial state:** Authentic pre-V9 DB contains failed/ambiguous100k send; original wallet never returns after fresh migration.

**E. Event chain and suspected mechanism:** New empty queue/funding/success tables cannot prove old execution; no dispatch row means new expired-dispatch polling excludes it.

**F. Expected correct behavior:** Preserve and reconcile historical entitlement using actual old node/source evidence, without fabricating markers.

**G. Potential incorrect outcome:** Sender99k can remain Pending forever; unsafe blanket legacy-failed refund can duplicate an old success.

**H. Evidence and inference:** E06 queue-required never_dispatched; E11 polling joins dispatch; V9 migration intentionally leaves legacy unknown. E49. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E49: [server/src/database/fork_migrations/V9__lightning_source_discharge.sql:3](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V9__lightning_source_discharge.sql:3), `expiry_lightning_queued_attempt`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b85b63c0020886d9abea607ab40a16a18acc439057c6add8918622378351a11c`. Code fact: Source-provenance marker tables start empty; legacy attempts are not silently asserted to be never dispatched.

**I. Current coverage:** Migration startup tests prove schema history/locks, not authentic legacy financial outcome recovery. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Restore actual pre-V9 source and node history; every100k liability is independently discharged once, failed source99k pays without client. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `LN-LEGACY`.

<a id="ml074"></a>

## ML074 — Chain-only HTLC fulfillment is reorged out after ownership proposal

**A. Title:** Chain-only HTLC fulfillment is reorged out after ownership proposal.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Grant/source was classified from chain Fulfilled without any irreversible stored preimage or binding successor.

**E. Event chain and suspected mechanism:** Reorg removes resolution block; old proposal/allocation remains customer/operator terminal unless invalidation rebuild re-resolves.

**F. Expected correct behavior:** Keep obligation/fence but invalidate chain-only owner and wait for canonical effective terminal evidence.

**G. Potential incorrect outcome:** Wrong owner receives funds, or user original right vanishes under stale irreversible classification.

**H. Evidence and inference:** E05 HtlcChain and stored_preimage; E30 generation; E29 chain rebuild. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Ordinary6/101-block payout reorg tests retain Pubkey ownership; they do not prove chain-only HTLC ownership reversal. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual fork removes supporting block; stable gross100k persists, no wrong-owner payout; later canonical recipient/refund exactly once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CHAIN-REORG`.

<a id="ml075"></a>

## ML075 — Known Lightning preimage is forgotten when its revealing block is reorged

**A. Title:** Known Lightning preimage is forgotten when its revealing block is reorged.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Actual onchain spend disclosed preimage; watchman persisted it; revealing block later disconnects.

**E. Event chain and suspected mechanism:** Chain rollback deletes settlement knowledge with chain resolution; sender refund becomes eligible although secret remains usable elsewhere.

**F. Expected correct behavior:** Cryptographic knowledge remains durable independently of chain-derived confirmations.

**G. Potential incorrect outcome:** Sender refunded and receiver later claims with already-disclosed secret; operator pays twice or loses source backing.

**H. Evidence and inference:** E05 has_stored_preimage; E14 settler durable storage; E29 stable-vs-derived rebuild. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E14: [server/src/ln/payment_handler.rs:76](/home/francis/abandon-ship-fresh-integration/server/src/ln/payment_handler.rs:76), `process_payment_attempt`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ac6da2446dece70ce724cca06dfc3e78316e69615e8cfe94a04f2c49378f69fa`. Code fact: Preimage storage precedes optimistic attempt update; only updated linked coins become ln-spent; notifications follow DB commit.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Ordinary reorg fixtures do not reveal real HTLC preimage and fork it out; current plans W5 explicitly require this. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Without exposing secret in report, verify durable knowledge retained across reorg; paid/claimable source refund0 until safe terminal resolution. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CHAIN-KNOWLEDGE`.

<a id="ml076"></a>

## ML076 — Accepted refresh forfeit retires a Lightning-created coin before replacement is usable

**A. Title:** Accepted refresh forfeit retires a Lightning-created coin before replacement is usable.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** User receives10k via Lightning, starts refresh into another tree, forfeits accepted, reply lost; wallet then permanently absent.

**E. Event chain and suspected mechanism:** Old receive source becomes operator-owned; replacement unclaimed/locked; fallback must locate binding replacement rather than forget both sides.

**F. Expected correct behavior:** One valid customer entitlement survives accepted forfeits across trees, without original wallet returning.

**G. Potential incorrect outcome:** Operator sweeps old source while new output remains unrecognized/unattributed; permanent if unlock/owner evidence is lost.

**H. Evidence and inference:** E05 Exchange forfeited ownership; E23 cancellation/decision; E17 receive-origin keys; E25 current economic cut. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

**I. Current coverage:** Native forfeit races use ordinary inputs; client binding recovery reopens/actively recovers. No retained offline Lightning-origin cross-tree accepted-forfeit lifecycle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Trace10k actual swap principal through both roots; rightful replacement9k or justified policy outputs, no duplicate old refund. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-REFRESH`.

<a id="ml077"></a>

## ML077 — Incomplete refresh cancellation loses the original failed-swap refund source

**A. Title:** Incomplete refresh cancellation loses the original failed-swap refund source.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Failed send is entitled to100k; refund successor joins an unfinished refresh spanning another root; no forfeits accepted; clients stop.

**E. Event chain and suspected mechanism:** Expiry cancellation marks speculative outputs operator; original refundable inputs must be restored and attributed despite Lightning lineage.

**F. Expected correct behavior:** Atomically retain original sender entitlement and permanently fence canceled exchange outputs.

**G. Potential incorrect outcome:** Both speculative output and original source may become operator or unready; sender loses99k eventual recovery.

**H. Evidence and inference:** E22 cancel_expiry_participations; E05 source-specific terminal/Exchange handling. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

Test reference: [expiry_cancels_unfinished_exchange_without_freezing_other_tree_inputs](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_races.rs:75), file SHA256 `c101bb783b275514d06785623567ff950c7552df221f4bf7f09e8ea5db3930cd` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_cancels_unfinished_exchange_without_freezing_other_tree_inputs is component/ordinary exchange coverage; failed-send test has no refresh. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Enumerate all participation inputs/outputs and original refund source; sender99k once, canceled outputs customer0; other-tree valid owner preserved. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-REFRESH`.

<a id="ml078"></a>

## ML078 — Successful Lightning debit outruns confirmed fee sponsorship before source timeout

**A. Title:** Successful Lightning debit outruns confirmed fee sponsorship before source timeout.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** A real external 90,000-sat payment succeeds from a 100,000-sat source and its preimage is durable. Source HTLC exit confirms near timeout, but watchman has no confirmed usable fee sponsor.

**E. Event chain and suspected mechanism:** Unconfirmed balance/fee exhaustion prevents the preimage collection spend from confirming before the sender's canonical timeout claim. Later reconciliation must account for the irreversible Lightning debit and already-returned source value separately.

**F. Expected correct behavior:** Meet contested collection deadlines where supported; if source collection loses, expose the operator deficit and never grant an additional fallback refund or erase the real recipient payment.

**G. Potential incorrect outcome:** Operator can lose the paid Lightning principal when sender recovers its source; incorrect reconciliation may additionally pay a 99,000-sat fallback, amplifying loss. No live timing failure is established.

**H. Evidence and inference:** E06 source-specific effective resolution; E27 watchman fee sponsorship; E37 confirmed coin selection; watchman policy HTLC collection deadlines. This is contested source collection after irreversible Lightning payment, distinct from delayed ordinary offboard. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E06: [server/src/database/expiry_lightning.rs:82](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_lightning.rs:82), `lightning_source_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `5eadc1562006545d130ee113b49ead0b75c6bf86aa09ee62652a575fe85745e5`. Code fact: Actual source-to-attempt links, external receipts and internal funding determine discharge; global preimage alone cannot discharge sends. Now-committed c52056a4c repair handles completed internal receives.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

**I. Current coverage:** Failed-send fallback and funded fee-replacement tests do not combine actual Lightning success, near-deadline source exit, unavailable confirmed sponsor and a canonical timeout winner. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe actual 90,000-sat Lightning debit and canonical source timeout outpoint; enumerate all later fallback outputs (zero for already-refunded source), record operator deficit explicitly and recipient payment once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-CONTESTED-COLLECTION`.

<a id="ml079"></a>

## ML079 — Unmaterialized final-output ID loses attribution hint before late xpub registration

**A. Title:** Unmaterialized final-output ID loses attribution hint before late xpub registration.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** External receive is Paid with immutable final-recipient binding, materialized=false. Its prospective output_id has no vtxo row. Recipient registers its xpub after the settled intent became pending.

**E. Event chain and suspected mechanism:** Ownership queues the bound user key using the prospective ID; a mailbox lookup expecting a real VTXO cannot recover the delivery hint. Validate that late registration creates and fairly scans the key/account pair independently of that absent row.

**F. Expected correct behavior:** Attribution of actual paid bound intent must use registered xpub verification even without a materialized output/mailbox row; late registration starts at zero.

**G. Potential incorrect outcome:** Alice's 9,000-sat fallback can remain unqueued/unidentified indefinitely even though the provider never returns. This is prospective-ID integration, not merely a wrong mailbox candidate.

**H. Evidence and inference:** E07 bound output_id exists before materialization; E05 receive-specific ownership; E03 queue_expiry_owner and registration initialization. Exact missing-row/late-registration reachability needs validation. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

**I. Current coverage:** Batch14 has a materialized signed recipient leaf and low derivation index; generic mailbox tests have real VTXO rows, not a paid prospective output ID. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Show no materialized VTXO row and real Paid source; register Alice afterward, observe zero-start pair creation and eventual correct 9,000-sat output, provider zero. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-SYNTHETIC-OUTPUT-ATTRIBUTION`.

<a id="ml080"></a>

## ML080 — Forwarded receive restored without backend action state loses original destination

**A. Title:** Forwarded receive restored without backend action state loses original destination.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Provider backend DB restored to before invoice/claim destination while server retained accepted/settled receive; recipient absent.

**E. Event chain and suspected mechanism:** Provider seed recovery reconstructs own grant keys but not external intended address; replay may self-claim or leave source unresolved.

**F. Expected correct behavior:** Original validated destination survives in server binding or durable original intent; seed recovery cannot choose a new beneficiary.

**G. Potential incorrect outcome:** Provider can receive recipient principal or value remains permanently unclaimable when destination metadata was lost.

**H. Evidence and inference:** E07 durable server binding only after claim preparation; E17 claim_destination in action; E31 provider versus recipient registration. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E07: [server/src/database/expiry_receive.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_receive.rs:65), `prepare_receive_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4431df885cd4756a64dce01875437151e0ff1d173f37c68920c257b45c8d195`. Code fact: Immutable validated final-output intent precedes collection; materialization requires matching actual output and Fulfilled grant.

E17: [bark/src/actions/lightning/receive.rs:144](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/receive.rs:144), `claim_lightning_receive_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `993c294f1f506b04474e86a554fad8772f61551f82f0c8970026a337b7c19ef0`. Code fact: PreimageRevealed checkpoint before wire; signed output registration precedes Delivering; local input/output/history/settled writes are separate.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

**I. Current coverage:** Seed-only batch14 restores recipient after already registered claim; no mismatched provider/server backup before binding. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Use genuine old provider backup and exact original invoice destination; recipient9k, provider0, no second invoice or rewritten claim intent. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-BACKUP-RECEIVE`.

<a id="ml081"></a>

## ML081 — Second device binds the same Ark seed to a different external payout wallet

**A. Title:** Second device binds the same Ark seed to a different external payout wallet.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** DeviceA registers Ark account to external BIP84walletA; restored deviceB supplies BIP84walletB address.

**E. Event chain and suspected mechanism:** First server binding wins; deviceB rejects mismatch; walletA keys later unavailable; all future Ark receipts still pay immutableA.

**F. Expected correct behavior:** Preserve canonical destination and make its recovery prerequisites explicit before accepting further value.

**G. Potential incorrect outcome:** Rightful user can own Ark seed yet be unable to spend fallback; later seed-only Ark restore cannot recover externalA funds. No new address-change policy is assumed.

**H. Evidence and inference:** E01 same_binding; E02 first record; E31 authenticate_binding. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E01: [lib/src/fallback.rs:41](/home/francis/abandon-ship-fresh-integration/lib/src/fallback.rs:41), `FallbackRecord`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `1935bc0eab2b40adc0b5fc4779c7e49e90975ef32601751cb610938c430257a1`. Code fact: Signatures bind network, mailbox, script and account; account child zero proves account possession, not destination spendability.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

Test reference: [fallback_native_registration_survives_seed_only_second_device_and_reopen](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/fallback.rs:5), file SHA256 `1a383a3afa4d94e5c8112b40341c557e40706aa0b1636a781997d561b02771d9` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** fallback_native_registration_survives_seed_only_second_device_and_reopen uses matching destination; external two-seed test preserves original external seed. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Use same Ark seed with different controlled external wallet; canonical script never silently changes, original external key remains recoverable or operation blocks before funding. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `REG-IMMUTABLE`.

<a id="ml082"></a>

## ML082 — A standard payout script is registered without any spendable wallet behind it

**A. Title:** A standard payout script is registered without any spendable wallet behind it.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** App accidentally supplies standard P2WSH/P2SH output whose spending script/key backup is missing; record signatures valid.

**E. Event chain and suspected mechanism:** Server accepts standard script; permanent xpub binding persists; expiry pays actual script and marks owed amount discharged.

**F. Expected correct behavior:** App must supply recoverable destination; protocol acceptance cannot be mistaken for proof user can spend it.

**G. Potential incorrect outcome:** Customer fallback funds permanently stranded at valid but unusable script; operator did pay specified script. This is app/integration responsibility, not proof protocol violated chosen address policy.

**H. Evidence and inference:** E01 validate_script and dual signatures prove account, not script possession; E31 address-agnostic integration. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E01: [lib/src/fallback.rs:41](/home/francis/abandon-ship-fresh-integration/lib/src/fallback.rs:41), `FallbackRecord`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `1935bc0eab2b40adc0b5fc4779c7e49e90975ef32601751cb610938c430257a1`. Code fact: Signatures bind network, mailbox, script and account; account child zero proves account possession, not destination spendability.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

**I. Current coverage:** fallback_accepts_application_standard_scripts_without_derivation_rules intentionally accepts synthetic scripts; it proves shape only, not spendability. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** For each accepted integration destination independently produce a real spending witness after seed-only restore; e.g.100k gross leads99k actually spendable output. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `REG-DESTINATION`.

<a id="ml083"></a>

## ML083 — An xpub header alias registers a second payout identity

**A. Title:** An xpub header alias registers a second payout identity.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Same actual account pubkey/chaincode is encoded with different metadata and mailbox/payee registration.

**E. Event chain and suspected mechanism:** Attempt serial and concurrent metadata/header aliases. A regression in derivation-identity uniqueness could accept competing payees; current database UNIQUE(pubkey, chaincode) and exclusive registry lock explicitly prevent that simple alias path.

**F. Expected correct behavior:** Deduplicate derivation identity independently of xpub header and reject ambiguous ownership.

**G. Potential incorrect outcome:** First or second payee can receive money owed to same seed; conflicting owner mapping can stall all affected keys.

**H. Evidence and inference:** E02 pubkey/chaincode identity and exclusive registry lock are positive guards; fork V1 unique index. A bypass is hypothetical, not an absent constraint. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

**I. Current coverage:** Registration immutable/idempotent tests cover source protections; no inspected funded coin payout after alias-registration contention. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Both serial and concurrent alias registration leave one canonical account/payee; one99k payment to original registered script. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `REG-XPUB-ALIAS`.

<a id="ml084"></a>

## ML084 — Mailbox authentication succeeds while account possession was never proven

**A. Title:** Mailbox authentication succeeds while account possession was never proven.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Mailbox signing key is valid but supplied xpub belongs to another user; destination is submitter-controlled.

**E. Event chain and suspected mechanism:** A mixed client/server version or restore importer checks only mailbox signature and stores record; later victim key derivation selects wrong script.

**F. Expected correct behavior:** Verify independent account-child0 and mailbox signatures over every field before any record can win.

**G. Potential incorrect outcome:** Victim coins redirected to another mailbox owner; once swept to wrong script recovery may be impossible.

**H. Evidence and inference:** E01 verify dual domains; E02 set_fallback verifies before lookup/insert. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E01: [lib/src/fallback.rs:41](/home/francis/abandon-ship-fresh-integration/lib/src/fallback.rs:41), `FallbackRecord`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `1935bc0eab2b40adc0b5fc4779c7e49e90975ef32601751cb610938c430257a1`. Code fact: Signatures bind network, mailbox, script and account; account child zero proves account possession, not destination spendability.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

**I. Current coverage:** fallback_record_v3_binds_every_field_and_role mutates every encoded byte and swaps roles; not a mixed-version importer or real funded payout. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Invalid account proof creates no canonical record/cursor; authentic victim record yields correct99k and no output to submitter. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `REG-DUAL-AUTH`.

<a id="ml085"></a>

## ML085 — Portable registration is replayed onto a different Signet with the same genesis

**A. Title:** Portable registration is replayed onto a different Signet with the same genesis.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Wallet/server profiles accidentally mix public and custom Signet sharing genesis and NetworkKind; same registered script/account.

**E. Event chain and suspected mechanism:** Fallback signatures bind genesis ChainHash, not challenge; registration verifies while actual UTXOs and recovery backend belong to different Signets.

**F. Expected correct behavior:** Operational network admission must distinguish actual challenge/server/funding identity.

**G. Potential incorrect outcome:** Wallet reports valid registration but scans wrong chain; actual paid99k remains undiscovered or inaccessible in configured recovery environment.

**H. Evidence and inference:** E01 message uses ChainHash; E43 network_identity; simulation plan §5 explicitly says shared genesis insufficient. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E01: [lib/src/fallback.rs:41](/home/francis/abandon-ship-fresh-integration/lib/src/fallback.rs:41), `FallbackRecord`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `1935bc0eab2b40adc0b5fc4779c7e49e90975ef32601751cb610938c430257a1`. Code fact: Signatures bind network, mailbox, script and account; account child zero proves account possession, not destination spendability.

E43: [simulator/signet_lightning.py:89](/home/francis/bark-simulator-signet-rework/simulator/signet_lightning.py:89), `stage`, repository `/home/francis/bark-simulator-signet-rework`, HEAD `d8cdb3b66c44df62cd946c57d18c3ffa8c544fab`; exact read bytes SHA256 `b5c9bfb025494a8efb221e58075dc36c4c2caca78326cf8a50f0f7836f2e9e85`. Code fact: Controller pins operation/runtime/profile; unknown post-effect outcomes held; source is supervision, not evidence that native money moved.

**I. Current coverage:** Wrong Regtest/Signet signature unit distinguishes different genesis, not two Signets; no challenge-mismatch financial recovery evidence. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare actual challenge, genesis, source server and canonical payout network; wrong profile rejected before new money action. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `REG-NETWORK-IDENTITY`.

<a id="ml086"></a>

## ML086 — Lost registration reply leaves key reservation and binding persistence out of step

**A. Title:** Lost registration reply leaves key reservation and binding persistence out of step.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Server commits immutable registration but first reply is lost. A retry receives the canonical reply; client stores child-zero identity, then crashes or persistence fails before local fallback-record storage.

**E. Event chain and suspected mechanism:** Separate store_vtxo_key(0) and store_fallback_record writes leave only the first local fact. Reopen must complete canonical registration before new financial keys/operations.

**F. Expected correct behavior:** No issuance/spend can precede authenticated durable registration; identical retry restores both local facts.

**G. Potential incorrect outcome:** Wallet can issue unsupported/unregistered coin keys or fail open forever; unpaid coins may never enter resumable owner search.

**H. Evidence and inference:** E31 register and persist_native_identity; E02 idempotent first binding. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

Test reference: [fallback_native_retries_a_lost_registration_reply](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/fallback.rs:56), file SHA256 `1a383a3afa4d94e5c8112b40341c557e40706aa0b1636a781997d561b02771d9` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** fallback_native_retries_a_lost_registration_reply covers remote reply loss. It does not fail local persistence after the successful retry and child-zero write but before fallback-record storage. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Original canonical record unchanged; no new financial key before fence; first funded key resolves to original xpub and99k payout. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `REG-LOCAL-COMMIT`.

<a id="ml087"></a>

## ML087 — Legacy hardened-index coin cannot be attributed from permanently registered xpub

**A. Title:** Legacy hardened-index coin cannot be attributed from permanently registered xpub.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** historical client data / current migration.

**D. Context and financial state:** Old client issued index>=2^31 before fresh wallet issuance cap; genuine100k remains live at migration.

**E. Event chain and suspected mechanism:** Server scans every normal child and never matches hardened key; NoRegisteredOwner remains Hold despite rightful seed.

**F. Expected correct behavior:** Preserve existing entitlement and define authentic legacy recovery rather than pretending normal exhaustive scan covers it.

**G. Potential incorrect outcome:** Customer principal remains indefinitely unattributed; if original client/seed derivation metadata is unavailable, recovery may be impossible.

**H. Evidence and inference:** E03 OWNER_KEYSPACE_END; E31 next_key_index; final plan key derivation contract/history. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

**I. Current coverage:** Last-normal-child unit starts at final two indices; no authentic old hardened financial coin migration. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Use old signed coin/key independently derived from known seed/index; preserve gross100k and eventual rightful99k, no operator fallback. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `REG-DERIVATION-CONTRACT`.

<a id="ml088"></a>

## ML088 — Ark-seed-only restore lacks the separate external BIP84 seed

**A. Title:** Ark-seed-only restore lacks the separate external BIP84 seed.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** App embeds independently seeded external OnchainWalletTrait wallet; only Ark seed backed up; fallback99k already confirmed.

**E. Event chain and suspected mechanism:** Fresh Ark restore authenticates record/history but cannot reconstruct external destination spending key.

**F. Expected correct behavior:** State recovery requirements honestly: independent external wallet backup is necessary for this supported integration.

**G. Potential incorrect outcome:** User sees correct paid history but cannot spend actual payout; permanent loss if external key/seed destroyed.

**H. Evidence and inference:** E31 app-supplied address; E33 chain verification does not prove key possession; client planK1. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

Test reference: [fallback_external_bip84_actual_payout_seed_restore_and_confirmed_spend](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/fallback_external.rs:108), file SHA256 `4436f65dd044766405eda580f3dddc313b93efb9334dcc9e2e92a1780f43598a` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** fallback_external_bip84_actual_payout_seed_restore_and_confirmed_spend preserves/restores external wallet seed; it is not proof one Ark seed recovers unrelated BIP84 seed. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Restore with exact documented backup set; confirm actual99k outpoint spend. Ark seed alone must not be claimed sufficient when seeds differ. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `REG-EXTERNAL-RESTORE`.

<a id="ml089"></a>

## ML089 — Release build restores the wrong native onchain descriptor family

**A. Title:** Release build restores the wrong native onchain descriptor family.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Test client uses fallback-test-bip84, release defaults BIP86; user restores same seed after payout.

**E. Event chain and suspected mechanism:** Recovery wallet scans default descriptor while permanent fallback points to BIP84 index0; tests used different feature/artifact.

**F. Expected correct behavior:** Preserve exact payout descriptor/network/address integration through release and restoration.

**G. Potential incorrect outcome:** Confirmed payout remains invisible/unspendable to default client; recoverable only if correct descriptor is recovered.

**H. Evidence and inference:** E31 no derivation in registration; client planK1; built manifest explicitly enables fallback-test-bip84. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

**I. Current coverage:** Native/external BIP84 payout passes pin feature; no equivalence to ordinary BIP86 release build is established. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Rehash actual release wallet features, derive original script independently, restore and spend exact99k without original DB. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `REG-BUILD-MODE`.

<a id="ml090"></a>

## ML090 — Cached registration survives a server restore that lost the permanent registry

**A. Title:** Cached registration survives a server restore that lost the permanent registry.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Client retains valid cached record and opens without requested destination; server restored backup lacks registration; wallet later disappears.

**E. Event chain and suspected mechanism:** open_fallback accepts cached record and does not re-register unless app supplies address; owner scanner cannot find record for live coin.

**F. Expected correct behavior:** Restore permanent registry consistently or reliably re-establish it before accepting more financial obligations.

**G. Potential incorrect outcome:** Customer100k becomes NoRegisteredOwner/Hold indefinitely; unavailable original wallet cannot register after expiry.

**H. Evidence and inference:** E31 cached requested=None path; E02 permanent registry; E04 no-owner Hold. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Second-device/lost reply tests preserve server DB; no nonaligned registry restore plus offline customer. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare restored server record set with prebackup live owners, then actual99k payout without returning client. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `REG-BACKUP`.

<a id="ml091"></a>

## ML091 — Wrong mailbox candidate is accepted without deriving the actual coin key

**A. Title:** Wrong mailbox candidate is accepted without deriving the actual coin key.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** ActualAlice100k coin is posted toBob mailbox; both registered.

**E. Event chain and suspected mechanism:** At expiry candidate lookup points toBob; shortcut treats mailbox association as ownership and allocatesBob script.

**F. Expected correct behavior:** Always derive and verify coin key against candidate xpub, then search true owner if disproved.

**G. Potential incorrect outcome:** Bob receives99k owed toAlice; correct key ownership no longer recovers swept backing.

**H. Evidence and inference:** E03 expiry_owner cached derivation verification; E05 mailbox candidate only. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

Test reference: [expiry_attribution_wrong_mailbox_is_not_a_payee_and_registration_invalidates_no_owner](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_attribution.rs:173), file SHA256 `11ac095afc2c3437f3a98979b584f903bcd6e2f2f1e75595cfbc9ec1dd20de2b` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_attribution_wrong_mailbox_is_not_a_payee_and_registration_invalidates_no_owner derives actual low key and rejects wrong candidate; no actual funded wrong-mailbox payout oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** SignedAlice key, Alice99k actual output, Bob0; no shared mailbox shortcut even if Bob record is newest. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-VERIFY`.

<a id="ml092"></a>

## ML092 — Malformed first mailbox record blocks a later correct ownership clue

**A. Title:** Malformed first mailbox record blocks a later correct ownership clue.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** legacy/imported malformed mailbox state; current writer reachability uncertain.

**D. Context and financial state:** Coin has early malformed/unparsable mailbox record and later correct authenticated delivery; customer xpub registered.

**E. Event chain and suspected mechanism:** expiry_customer chooses ORDER BY id LIMIT1 and parses it; parse error prevents queuing key/search before later candidate can be considered.

**F. Expected correct behavior:** Unusable delivery metadata cannot suppress independent xpub attribution and eventual payout.

**G. Potential incorrect outcome:** Alice99k remains unpaid indefinitely despite valid later record; no wrong-payee payment is necessary for failure.

**H. Evidence and inference:** E05 expiry_customer first mailbox parse; E03 queue_expiry_owner. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

**I. Current coverage:** Wrong-mailbox test uses well-formed mailbox; no retained malformed-first plus later-valid record case. Assumption: malformed legacy/import row is reachable. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Original signed100k remains accounted; search starts despite bad metadata and paysAlice99k, no record deletion required. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-MAILBOX-ORDER`.

<a id="ml093"></a>

## ML093 — Absent mailbox plus high-index owner exhausts practical recovery capacity

**A. Title:** Absent mailbox plus high-index owner exhausts practical recovery capacity.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Permanently registeredAlice key is far beyond normal window; no mailbox survives; many other records exist.

**E. Event chain and suspected mechanism:** Every pending key needs per-record normal-child coverage; bounded workers spend long periods on wrong accounts before true match.

**F. Expected correct behavior:** Remain unresolved safely and provide measured fair eventual attribution within accepted operational envelope.

**G. Potential incorrect outcome:** Alice principal delayed for potentially impractical time; permanent if operator stops search or backup loses cursors and restarts forever.

**H. Evidence and inference:** E03 complete normal keyspace and scheduler; final planC2 cost expressly R×2^31. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

**I. Current coverage:** Scale cases match indices17-19; last-index unit derives only final two children, not full end-to-end search latency. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent high-index key oracle, per-pair actual covered intervals and oldest liability age; eventualAlice99k rather than a timeout-to-operator classification. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `ATTR-EXHAUSTIVE`.

<a id="ml094"></a>

## ML094 — Later lower key inherits completed coverage from an earlier higher key

**A. Title:** Later lower key inherits completed coverage from an earlier higher key.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** earlier plan regression; current pair cursors.

**D. Context and financial state:** Scanner already passed index8 looking forAlice key80; a new100k coin at index8 becomes pending after restart.

**E. Event chain and suspected mechanism:** Record-wide cursor or mistaken join says range0..64 already searched for new key; actual index8 is never compared.

**F. Expected correct behavior:** Coverage belongs to exact pending key/record pair; new key begins0.

**G. Potential incorrect outcome:** New rightful owner never identified; coin100k waits indefinitely or is wrongly declared no owner.

**H. Evidence and inference:** E03 expiry_scan primary pair and queue zero coverage. Historical context/positive guard does not establish an earlier actual failing execution; classify the proposed reachability conservatively as a hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

Test reference: [expiry_attribution_batches_keys_and_recovers_late_lower_keys_after_restart](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_attribution.rs:29), file SHA256 `11ac095afc2c3437f3a98979b584f903bcd6e2f2f1e75595cfbc9ec1dd20de2b` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_attribution_batches_keys_and_recovers_late_lower_keys_after_restart proves indices17/25 then8; not concurrent worker/DB crash during new-pair initialization. Later 8fb729a2f adds an exclusive registry fence and atomic bounded-pass commit; retained owner-scan-atomic-pass-20261017.json reports six real PostgreSQL component passes (e868e1e6). These protect races, but do not turn the original 120-second wrong-mailbox failure into a current benchmark pass. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe new key cursor0 and actual comparisons at8; old cursor remains independent; final99k script matchesAlice. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-CURSOR-IDENTITY`.

<a id="ml095"></a>

## ML095 — Key arriving during derivation is advanced by a batch that never compared it

**A. Title:** Key arriving during derivation is advanced by a batch that never compared it.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Work snapshot for key80 derives0..32; new key8 inserted while worker runs; result commits after arrival.

**E. Event chain and suspected mechanism:** Broad cursor update advances all pending keys for account; new key misses its only matching range.

**F. Expected correct behavior:** Commit only keys captured in immutable batch, each contiguous prefix atomically.

**G. Potential incorrect outcome:** Late customer100k stays unidentified despite completed-looking search; permanent if wrong no-owner result finalized.

**H. Evidence and inference:** E03 OwnerScanBatch keys and commit_expiry_scan compare-and-advance. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

Test reference: [expiry_attribution_prepared_work_never_advances_a_new_pending_key](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_attribution.rs:140), file SHA256 `11ac095afc2c3437f3a98979b584f903bcd6e2f2f1e75595cfbc9ec1dd20de2b` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_attribution_prepared_work_never_advances_a_new_pending_key checks prepared snapshots, dropped batch/reconnect and stale result; not process kill during actual large-worker commit. Later 8fb729a2f adds an exclusive registry fence and atomic bounded-pass commit; retained owner-scan-atomic-pass-20261017.json reports six real PostgreSQL component passes (e868e1e6). These protect races, but do not turn the original 120-second wrong-mailbox failure into a current benchmark pass. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** New key8 remains cursor0 after old commit, subsequently matches and yields99k; no skipped intervals for any key. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-CURSOR-BATCH`.

<a id="ml096"></a>

## ML096 — New xpub registration is omitted from a running negative search

**A. Title:** New xpub registration is omitted from a running negative search.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Alice coin is pending beforeAlice registers; prior records all exhausted; registry insertion races owner-result commit.

**E. Event chain and suspected mechanism:** If old registry-version results omit creation of the new key/account pair, the no-owner result remains Hold and is never revisited for Alice's valid new registration. Current locks are intended to prevent this ordering.

**F. Expected correct behavior:** Serialize registration and dependent decision; late record scanned before any affected final allocation.

**G. Potential incorrect outcome:** Alice's valid entitlement remains unpaid after registration despite resumable scanning elsewhere. NoRegisteredOwner does not authorize operator allocation in current code.

**H. Evidence and inference:** E02 exclusive registry lock/add_expiry_scan_record; E03 revalidate_expiry_no_owner; E23 shared registry boundary. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

**I. Current coverage:** Wrong-mailbox test invalidates synthetic terminal coverage on registration; registry-fence test is component exclusion, not funded coin with native concurrent open. Later 8fb729a2f adds an exclusive registry fence and atomic bounded-pass commit; retained owner-scan-atomic-pass-20261017.json reports six real PostgreSQL component passes (e868e1e6). These protect races, but do not turn the original 120-second wrong-mailbox failure into a current benchmark pass. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Once Alice registration commits, observe a zero-start pair and eventual correct 99,000-sat payout; old negative coverage must not keep the coin in Hold forever. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-REGISTRY-RACE`.

<a id="ml097"></a>

## ML097 — Crash commits scan cursor but loses the match found in that range

**A. Title:** Crash commits scan cursor but loses the match found in that range.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Worker findsAlice key17 in chunk0..32; persistence is interrupted.

**E. Event chain and suspected mechanism:** Separate transactions would commit cursor32 before owner map; resume starts32 and can never rediscover17.

**F. Expected correct behavior:** Match and contiguous-prefix advancement share one atomic transaction; rollback repeats unfinished chunk.

**G. Potential incorrect outcome:** Alice100k owner disappears permanently from search, even though logs report match.

**H. Evidence and inference:** E03 commit_expiry_owner_scan write transaction contains cursor plus owner insert/delete. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

**I. Current coverage:** Prepared-work test drops before commit and checks stale results; no inspected database termination between actual statements in commit transaction. Later 8fb729a2f adds an exclusive registry fence and atomic bounded-pass commit; retained owner-scan-atomic-pass-20261017.json reports six real PostgreSQL component passes (e868e1e6). These protect races, but do not turn the original 120-second wrong-mailbox failure into a current benchmark pass. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After real crash recovery either cursor0/no match or verified match17/cursor cleanup; never cursor32 without match; eventual99k. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-CURSOR-ATOMIC`.

<a id="ml098"></a>

## ML098 — One derivation worker error discards every completed batch in its pass

**A. Title:** One derivation worker error discards every completed batch in its pass.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Multiple account batches run; one bad legacy xpub or worker panic errors while others found valid owners.

**E. Event chain and suspected mechanism:** try_collect returns error before result commits; healthy completed work is discarded; deterministic bad account selected again each pass.

**F. Expected correct behavior:** Isolate failing record while preserving safe verified work and progress for other registrations.

**G. Potential incorrect outcome:** Unrelated100k entitlements remain unpaid indefinitely; no value is misassigned, but queue never drains.

**H. Evidence and inference:** E03 scan_expiry_owners buffer_unordered/try_collect then commit loop. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

**I. Current coverage:** Duplicate-cursor test covers three valid accounts; no persistent one-worker failure with independent valid pending keys. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Preserve records, repeatedly fail one declared account, observe other match17 and actual99k payout without advancing failed coverage. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `ATTR-WORKER-FAILURE`.

<a id="ml099"></a>

## ML099 — Continuous low-index arrivals repeatedly preempt an older high-index entitlement

**A. Title:** Continuous low-index arrivals repeatedly preempt an older high-index entitlement.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** An old high-index Alice key has partial coverage; sustained low-cursor arrivals exceed the scheduler's available service rate. This workload/fairness condition has not been measured in a funded runtime.

**E. Event chain and suspected mechanism:** Scheduler orders smallest scanned_up_to and mailbox priority; old account range never selected often enough to reachAlice.

**F. Expected correct behavior:** Bound search concurrency with demonstrable fairness/age progress, not merely low-index throughput.

**G. Potential incorrect outcome:** Old valid100k entitlement waits forever under sustained load while dashboards show many fast matches.

**H. Evidence and inference:** E03 prepare_expiry_scans preferred/fair ORDER BY cursor; E04 worker integration. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Finite three-account fairness test and low-index10k scale do not measure continuous arrivals versus aged key. Later 8fb729a2f adds an exclusive registry fence and atomic bounded-pass commit; retained owner-scan-atomic-pass-20261017.json reports six real PostgreSQL component passes (e868e1e6). These protect races, but do not turn the original 120-second wrong-mailbox failure into a current benchmark pass. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Under declared arrival rate trackAlice covered-prefix advance and funded entitlement age; eventually99k while newer workload continues. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-SCHEDULING`.

<a id="ml100"></a>

## ML100 — Permanent registration growth multiplies pending cursor storage until recovery stops

**A. Title:** Permanent registration growth multiplies pending cursor storage until recovery stops.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Many unmatched keys and permanent registrations accumulate concurrently with normal payments.

**E. Event chain and suspected mechanism:** Every new pair creates expiry_scan rows; K×R growth consumes database/disk/lock capacity; scanner and financial writes stall.

**F. Expected correct behavior:** Preserve liabilities and provide measured sustainable queue/storage drain under admitted population.

**G. Potential incorrect outcome:** Thousands of rightful payouts delayed indefinitely; disk exhaustion during persistence can extend outage or make backups incomplete.

**H. Evidence and inference:** E03 queue_expiry_owner/add_expiry_scan_record cross product; E02 records permanent; prior disk-space incident context. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E02: [server/src/database/fallback.rs:38](/home/francis/abandon-ship-fresh-integration/server/src/database/fallback.rs:38), `set_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `71b8dfae45ddad92f9c69a32f9b0d474f4c928f202c9b1aed647b83993b23fe9`. Code fact: First mailbox record is immutable; exclusive registry advisory lock serializes insertion; account pubkey plus chain code deduplicated.

**I. Current coverage:** 10k registration fixture has a few synthetic keys and matched-key cleanup; it does not qualify large simultaneous unmatched-key backlog. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Record actual pending-pair count, disk growth and oldest funded source; conservation preserved across low-disk stop and resumed payout drain. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `ATTR-STORAGE`.

<a id="ml101"></a>

## ML101 — Ten-thousand-account wrong-mailbox search misses its bounded deadline

**A. Title:** Ten-thousand-account wrong-mailbox search misses its bounded deadline.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** historical indexed scanner failure; later current 8fb729a2f changes commit cost/fencing, timing not established here.

**D. Context and financial state:** Retained indexed run has10000 authentic registrations, key19 and wrong mailbox; actual owner sorts last.

**E. Event chain and suspected mechanism:** Scanner runs120.139seconds, performs9902 account-range operations and remains unresolved; absent-mailbox companion matched in103.265seconds.

**F. Expected correct behavior:** Meet declared test bound or report failure and still preserve eventual rightful attribution; never count visits as distinct owners.

**G. Potential incorrect outcome:** Observed bounded discovery failure; no funded loss shown. Under similar sustained workload a real customer fallback can be delayed indefinitely.

**H. Evidence and inference:** E03; root-private-archives/owner-scheduler-26ee-7562-scale-data-20261017.tar.gz member authenticated-registration-scale.json, member hash cfaaf6244a0ccbb966791858f68e0a43e9e085a26b1713f95c41681a99b79fc1. The retained observation establishes only the scope explicitly described; permanent monetary loss is not inferred.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

Test reference: [expiry_registration_authenticated_scale](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_registration_scale.rs:61), file SHA256 `3cc296282ecf020be15888122b3b6691fbf6590a8c0541f4a00277dff20d064b` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_registration_authenticated_scale FAIL wrong-mailbox120s; synthetic low-index keys, not funded payout throughput or complete-keyspace proof. 116/1016 earlier visits exceed record counts. Later 8fb729a2f adds an exclusive registry fence and atomic bounded-pass commit; retained owner-scan-atomic-pass-20261017.json reports six real PostgreSQL component passes (e868e1e6). These protect races, but do not turn the original 120-second wrong-mailbox failure into a current benchmark pass. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Track distinct pair intervals and real fundedAlice100k, not operation counter; require actual owner match and99k within stated tested bound. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Observed failure**. Root-cause family: `ATTR-SCALE`.

<a id="ml102"></a>

## ML102 — True unregistered owner remains unpaid after every registered xpub is exhausted

**A. Title:** True unregistered owner remains unpaid after every registered xpub is exhausted.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current resolver applied to authentic legacy/external unregistered coins; modern SDK admission guards registration.

**D. Context and financial state:** A valid legacy or externally issued 100,000-sat customer coin has a key outside every registered account. Its owner never registered and never returns. Modern SDK issuance guards mean this is not assumed reachable through ordinary current SDK creation.

**E. Event chain and suspected mechanism:** Search actually completes all normal spaces; result NoRegisteredOwner; prepare_expiry_coin returns Hold permanently.

**F. Expected correct behavior:** Never convert unknown entitlement to operator money; specify a defensible eventual owner-identification/recovery path or acknowledge unmet guarantee.

**G. Potential incorrect outcome:** Principal remains permanently stranded absent external owner evidence. No amount of completed scanning can identify a nonexistent registration.

**H. Evidence and inference:** E03 expiry_owner complete coverage; E04 NoRegisteredOwner Hold; final plan no-wait-forever versus permanent registration scope. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** No-owner test synthetically advances prefixes near end; it proves classification, not actual identification or payout of truly unregistered key. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Maintain explicit100k unpaid liability indefinitely without operator appropriation; acceptance needs identified rightful claimant and actual spend, not merely safe Hold. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `ATTR-NO-REGISTERED-OWNER`.

<a id="ml103"></a>

## ML103 — Cached owner row becomes invalid and repeatedly blocks its known entitlement

**A. Title:** Cached owner row becomes invalid and repeatedly blocks its known entitlement.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** MatchedAlice key17 cached; restored owner row references wrong child or unavailable record while signed source remains valid.

**E. Event chain and suspected mechanism:** expiry_owner rederives and rejects mismatch; ready worker retries same failure without rebuilding that cache/pair.

**F. Expected correct behavior:** Fail safely and recover owner evidence from authentic immutable registration; valid user cannot wait forever because cache is damaged.

**G. Potential incorrect outcome:** Alice99k remains unpaid; broad anchor processing may also stall siblings if error propagates.

**H. Evidence and inference:** E03 expiry_owner cache authentication; E04 prepare_expiry_coin error isolation. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Attribution tests use valid cache and genuine registration; no inconsistent backup of sparse owner map versus permanent registry. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Detect mismatch without payingBob, rebuild authentic key17 proof, thenAlice99k; preserve original corrupt evidence for audit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `ATTR-CACHE`.

<a id="ml104"></a>

## ML104 — Server restarts without the background owner-scan worker progressing

**A. Title:** Server restarts without the background owner-scan worker progressing.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Expiry readiness queues validAlice key; captaind stays healthy but owner worker is disabled, failed or never restarted.

**E. Event chain and suspected mechanism:** No derivation occurs; readiness repeatedly Hold; server/watchman health masks absence of ownership progress.

**F. Expected correct behavior:** Worker lifecycle/monitoring must expose failure and resume durable queued work without user.

**G. Potential incorrect outcome:** Every newly attributed payout waits forever while RPC and chain sync appear healthy.

**H. Evidence and inference:** E03 queue and worker separation; server/src/expiry_owner_scan.rs::start_expiry_owner_scan; E04 critical worker loop. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

Test reference: [expiry_owner_scan_runs_in_captaind_and_stops_gracefully](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_owner_scan.rs:11), file SHA256 `92a1629d334fb099678aa4138804c76544276afec2e13a6e77179fa0bc005bca` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_owner_scan_runs_in_captaind_and_stops_gracefully covers normal start/stop; no worker-only fault with healthy services and recovery backlog. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Observe actual child comparisons/cursor advancement and fundedAlice99k; liveness alert tied to stalled oldest entitlement rather than process uptime. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ATTR-WORKER-LIFECYCLE`.

<a id="ml105"></a>

## ML105 — Historical forfeit paths block a valid transferred entitlement under the same anchor

**A. Title:** Historical forfeit paths block a valid transferred entitlement under the same anchor.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** historical deployed7055; current correction present.

**D. Context and financial state:** Original S03 recipient owns29670sat board plus10000sat transferred coin under another root; sender refresh accepts forfeits.

**E. Event chain and suspected mechanism:** Deployed7055 preparer validates historical descendants first; two Hark forfeit paths fail point validation; transferred root never reaches decision while first root pays28670.

**F. Expected correct behavior:** Select conserving current economic cut and settle unaffected recipient principal; preserve genuine forfeit obligations.

**G. Potential incorrect outcome:** Observed10000sat entitlement stayed unpaid at height326761; later separate repair paid9000sat and seed recovery spent it. Original one-output grouping never proved.

**H. Evidence and inference:** E25 current correction; simulator docs/signet-s03-blocker-20261017.md; cache/s03-geometry-diagnostic-20261017/result.log actual path validations; original product7055d1fce. The retained observation establishes only the scope explicitly described; permanent monetary loss is not inferred.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

Test reference: [expiry_frozen_recipient_is_paid_after_sender_change_refresh](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_transfer_refresh.rs:20), file SHA256 `a6b52ab184288358963fc9e5183c232cbc0c602f3782aec458af4eec9adb5712` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_frozen_recipient_is_paid_after_sender_change_refresh correction permits one/two outputs; later repair does not overwrite original failure or establish single38670 output. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Replay retained original geometry later: all39670 customer gross conserved as real payouts plus1000 per actual output; original recipient script only, no duplicate descendants. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Observed failure**. Root-cause family: `INV-HISTORICAL-GEOMETRY`.

<a id="ml106"></a>

## ML106 — An unrelated malformed historical source row prevents current-root inventory

**A. Title:** An unrelated malformed historical source row prevents current-root inventory.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Anchor contains valid payableAlice leaf and unused old row whose encoded metadata no longer matches columns.

**E. Event chain and suspected mechanism:** backing_snapshot decodes/compares every anchor row before economic_cut; one mismatch aborts preparation for all current roots.

**F. Expected correct behavior:** Never pay from invalid evidence, but isolate unrelated obsolete metadata and retain progress for independently proven current backing.

**G. Potential incorrect outcome:** Alice99k blocked forever even though selected path itself is valid; operator gets no justified release.

**H. Evidence and inference:** E24 backing_snapshot all-row metadata equality; E25 only later selects current cut. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

**I. Current coverage:** Current-cut unit ignores invalid unused full path, not an encoding/column decode error before cut selection. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Preserve malformed row, independently proveAlice exact100k path and99k payout; no unproved sibling value swept. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `INV-ANCHOR-DECODE`.

<a id="ml107"></a>

## ML107 — Whole-anchor inventory exceeds its limits before any split can reduce work

**A. Title:** Whole-anchor inventory exceeds its limits before any split can reduce work.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Valid large tree/history exceeds250000 rows or256MiB across anchor; even a current frontier child is small.

**E. Event chain and suspected mechanism:** backing_snapshot refuses whole anchor; prepare never reaches size split; request_expiry_split independently reads same bounded whole inventory and refuses.

**F. Expected correct behavior:** Bound resources while providing an incremental conserving path to payout; refusal alone cannot satisfy eventual recovery.

**G. Potential incorrect outcome:** All customer value under anchor can remain permanently unprocessed; splitting current root does not shrink retained historical rows.

**H. Evidence and inference:** E04 INVENTORY_LIMITS/prepare_expiry_anchor; E24 snapshot anchor-wide cap; E28 request_expiry_split same budget; PAYOUT_REVIEWP02. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** Batch16 run 63a761b8 passed the corrected 327-owner/635-path two-level automatic split (all 327 paid 4,000 sat, >=7 confirmations). Its inventory is far below the 250,000-row/256-MiB whole-anchor cap and does not exercise cap-before-split deadlock. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** At genuine oversized retained anchor, every entitlement remains accounted and frontier/payout progresses without deleting evidence; actual totals and max memory measured. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `INV-BOUNDED-DEADLOCK`.

<a id="ml108"></a>

## ML108 — A valid geometric ancestor cut hides economically transferred descendants

**A. Title:** A valid geometric ancestor cut hides economically transferred descendants.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Root100k can itself form a conserving geometric cut, but90k legitimately transferred toBob below it.

**E. Event chain and suspected mechanism:** Caller treats geometry proof alone as terminal ownership and allocates original owner100k.

**F. Expected correct behavior:** Select actual recorded current transitions before assigning owner; geometric containment is not economic entitlement.

**G. Potential incorrect outcome:** Original owner receives99k whileBob loses90k claim; same ancestry can conceal wrong payee despite conservation.

**H. Evidence and inference:** E24 check_partition documents ancestor cut limitation; E25 economic_cut transitions. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

**I. Current coverage:** Board geometry unit proves conserving leaf; current-cut tests use ordinary transfer/change; no arbitrary caller-supplied stale ancestor authorization should succeed. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Bob actual89k plus any original owner remainder under policy, no original full99k; independent signed transfer oracle beats resolver verdict. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `INV-ECONOMIC-CUT`.

<a id="ml109"></a>

## ML109 — Anchor residuals are omitted or counted again as customer principal

**A. Title:** Anchor residuals are omitted or counted again as customer principal.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Board/tree transition contains330sat or zero-value P2A residual alongside customer leaves.

**E. Event chain and suspected mechanism:** Partition ignores anchor or assigns it both to operator and leaf; repeated levels inflate/erase value before sweep.

**F. Expected correct behavior:** Every actual outpoint belongs to exactly one leaf/residual/miner accounting term.

**G. Potential incorrect outcome:** Customer/operator shortfall or duplicate principal; may strand allocation when required sum cannot be reconstructed.

**H. Evidence and inference:** E24 explicit BackingPartitionResidual; E25 is_p2a_fee_anchor; E26 allocation. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** expiry_readiness_follows_registered_board_and_conserves_actual_backing checks one330 residual; not multilevel mixed nonzero/zero anchors. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently decode entire transaction DAG; sum external inputs=terminalUTXOs+actual miner fees, residual outpoints each once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `INV-RESIDUAL`.

<a id="ml110"></a>

## ML110 — New descendant appears after readiness snapshot without invalidating its parent version

**A. Title:** New descendant appears after readiness snapshot without invalidating its parent version.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Phase1 holds valid snapshot ofAlice100k; concurrent registered transfer insertsBob90k descendant.

**E. Event chain and suspected mechanism:** Writer leaves selected parent updated_at/spend state unchanged or modifies a path outside tracked versions; phase2 accepts old owner cut.

**F. Expected correct behavior:** Every obligation-changing write must invalidate/revalidate all selected economic evidence under common locks.

**G. Potential incorrect outcome:** Alice incorrectly paid for value already owedBob, or duplicate successor/fallback claims exist.

**H. Evidence and inference:** E23 versions includes selected regular rows; E22 write guards; E38 tree update. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Coin-fence tests serialize existing rows; no inspected native descendant-insertion phantom across every tree-update variant. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Force real transfer between preparation/commit later; exactly one current owner/financial operation wins; canonical payouts never overcount100k. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `INV-SNAPSHOT-PHANTOM`.

<a id="ml111"></a>

## ML111 — Partially exited anchor is treated as one entirely paid or entirely unspent tree

**A. Title:** Partially exited anchor is treated as one entirely paid or entirely unspent tree.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Alice andBob share tree; Alice exits one branch; Bob leaf remains backed by confirmed sibling output.

**E. Event chain and suspected mechanism:** Anchor-level spent flag suppressesBob inventory or reuses consumed ancestor as fallback input.

**F. Expected correct behavior:** Follow actual descendant outpoints and preserve unpaid siblings independently.

**G. Potential incorrect outcome:** Bob permanently loses payout, or server records payment based only onAlice backing spend; duplicate allocation may remain unbroadcastable.

**H. Evidence and inference:** E24 path_root and snapshot; E29 conflict rebuild; E25 actual root containment. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

Test reference: [expiry_actual_partial_exit_conflict_regroups_after_restart](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_conflict.rs:15), file SHA256 `7b47f99a2987bfda74942e0c5c716ff2f2acc0430aab9739a78bb66cb8d1e847` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_actual_partial_exit_conflict_regroups_after_restart covers distinct two-root merge, not shared-anchor partial exit with multiple frontier children. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** ActualAlice terminal claim andBob99k (for100kBob leaf) each once; enumerate every spent/intermediate/unspent branch and conservation. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `INV-PARTIAL-EXIT`.

<a id="ml112"></a>

## ML112 — Multi-input forfeit is mistaken for an ordinary one-input tree transition

**A. Title:** Multi-input forfeit is mistaken for an ordinary one-input tree transition.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Accepted round/offboard forfeit consumes original plus connector; historical signed tx remains under anchor.

**E. Event chain and suspected mechanism:** Graph follows oor_spent_txid past settlement boundary and demands one input, rejecting legitimate current obligations or attributing connector value to customer.

**F. Expected correct behavior:** Treat binding settlement as ownership boundary; validate its specific multi-input conservation separately.

**G. Potential incorrect outcome:** Unrelated customer payout stalls or connector value is overpaid; old/replacement rights may be double-counted.

**H. Evidence and inference:** E25 stops on spent_in_round/offboarded_in; E24 complete partition; E05 Forfeit. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

**I. Current coverage:** expiry_readiness_stops_at_offboard_and_round_settlement_boundaries tests metadata stop; S03 forfeit correction addresses another shape, not full connector-rich native family. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent original/connector/replacement DAG accounts each sat once; correct registered customer receives only genuine entitlement. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `INV-FORFEIT-GEOMETRY`.

<a id="ml113"></a>

## ML113 — Bare intermediate rows replace the only complete signed recovery path

**A. Title:** Bare intermediate rows replace the only complete signed recovery path.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** DB restore retains bare root/leaf rows but loses full genesis or signed virtual transaction; customer100k valid in original state.

**E. Event chain and suspected mechanism:** Snapshot returns MissingPath; resolver safely refuses; no surviving autonomous producer can reconstruct missing signatures.

**F. Expected correct behavior:** Back up all required signed paths or recover authentic copies before declaring restore complete.

**G. Potential incorrect outcome:** Funds permanently stranded if original client absent and signing material unavailable; database amount alone cannot recover them.

**H. Evidence and inference:** E24 decode_genesis/MissingPath; E28 signed split requirements; E38 commit-before-sign. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Inventory refusal units prove safety on missing paths; no authentic partial-backup restore-to-spend test. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Restore backup then independently validate original source and spend actual99k; unavailable proof remains explicit unpaid100k, never operator value. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `INV-PATH-AVAILABILITY`.

<a id="ml114"></a>

## ML114 — Pruned or unindexed Core cannot return the old funding transaction

**A. Title:** Pruned or unindexed Core cannot return the old funding transaction.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Expired customer path needs old anchor funding tx; Core UTXO exists but full old transaction unavailable locally.

**E. Event chain and suspected mechanism:** prepare_expiry_anchor calls get_raw_transaction without retained raw funding fallback; each pass errors before ownership.

**F. Expected correct behavior:** Retain/retrieve authentic required funding data and verify hash, without sacrificing independent chain checks.

**G. Potential incorrect outcome:** Oldest customer payouts stall indefinitely on otherwise synced Core; permanent if no raw transaction source remains.

**H. Evidence and inference:** E04 prepare_expiry_anchor funding retrieval; E24 unsigned path validation. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

**I. Current coverage:** Native fixtures retain full regtest transaction history; no pruned-node/index-loss recovery evidence. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Exact anchor tx bytes/hash recovered and100k source proven, then99k actual script payout; missing data must trigger unpaid-liveness alert. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `INV-CHAIN-DATA`.

<a id="ml115"></a>

## ML115 — Shared ancestor encodings make a valid tree exceed the path-work budget

**A. Title:** Shared ancestor encodings make a valid tree exceed the path-work budget.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Many leaves share depth100 path; distinct raw rows fit limits but check_partition counts each repeated full path.

**E. Event chain and suspected mechanism:** rows adds each terminal exit_depth and bytes adds each encoding; proof refuses before size split can subdivide economics.

**F. Expected correct behavior:** Bound proof work with a terminating conservative decomposition; repeated valid history cannot make recovery impossible.

**G. Potential incorrect outcome:** Large valid customer cohort remains unready although physical frontier could be processed in smaller pieces.

**H. Evidence and inference:** E24 check_partition work counting; E04 hard constants; E28 split preflight. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** 327-payee fixture targets10000-vB parent size, not high shared-depth proof-work cap; final-child unit cannot establish capacity. New retained batch16 run 63a761b8 proves a bounded 327-owner, 635-path, two-level native split with all rightful outputs; it does not exercise this scenario's extra fault/precondition. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Generate independently valid shared-path workload in future; measure actual budget and prove all owners receive correct net via conserving progress. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `INV-PROOF-WORK`.

<a id="ml116"></a>

## ML116 — Ancestor and descendant frontier candidates overlap after interrupted rollback

**A. Title:** Ancestor and descendant frontier candidates overlap after interrupted rollback.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Listener restart briefly leaves both physical root and its confirmed child marked frontier for same customer.

**E. Event chain and suspected mechanism:** Readiness groups both; partition overlap is refused, but repeated selection can block otherwise valid frontier until state repairs.

**F. Expected correct behavior:** No overlapping allocations; rebuild canonical frontier and continue on correct unique covering outpoints.

**G. Potential incorrect outcome:** Double payment if overlap slips through, or indefinite nonpayment if safe rejection never repairs stale frontier.

**H. Evidence and inference:** E24 path_root rejects overlapping roots; E30 completed view gate; E04 queries frontier_at. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Ordinary reorg test reopens original parent, not deliberately overlapping partial listener state with independently payable siblings. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** One canonical frontier cut after restart; source100k counted once and99k paid once; stale row never becomes separate entitlement. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `INV-FRONTIER-OVERLAP`.

<a id="ml117"></a>

## ML117 — Canceled refresh participation row is deleted before its outputs expire

**A. Title:** Canceled refresh participation row is deleted before its outputs expire.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Unfinished exchange canceled by fallback; upstream later removes superseded participation while old unclaimed output remains.

**E. Event chain and suspected mechanism:** Ownership lookup loses unlock_hash association; output may look like valid customer Pubkey or hold forever; late client might seek unlock.

**F. Expected correct behavior:** Persist cancellation independent of upstream row lifetime and never release canceled preimage.

**G. Potential incorrect outcome:** Speculative output can pay in addition to original refund, or original entitlement blocks on missing history.

**H. Evidence and inference:** E05 expiry_cancelled_participation fallback; E22 guard_participation. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

Test reference: [expiry_cancelled_unclaimed_output_keeps_owner_after_participation_removed](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_ownership.rs:79), file SHA256 `1a96d28659f00f607ea5d58ba0bb8e4eea20df11137ccdcc154c163a2af925a8` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_cancelled_unclaimed_output_keeps_owner_after_participation_removed checks durable cancellation; no full migration/retention cleanup plus late native unlock request. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Canceled output customer0 forever; original100k yields rightful99k once after row deletion/restart, unlock never released. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ROUND-CANCEL-HISTORY`.

<a id="ml118"></a>

## ML118 — A canceled old refresh permanently blocks reuse of an undecided original input

**A. Title:** A canceled old refresh permanently blocks reuse of an undecided original input.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Participation across two roots canceled when only one input committed for fallback; other input remains legitimate customer property.

**E. Event chain and suspected mechanism:** Guard treats any canceled-history membership as permanent blocklist on coin; later valid refresh/offboard fails repeatedly.

**F. Expected correct behavior:** Fence canceled exchange itself, not unrelated undecided input or its new binding operation.

**G. Potential incorrect outcome:** User loses availability of second100k without any payout; original fallback may succeed while remaining entitlement waits forever.

**H. Evidence and inference:** E22 guard_participation and cancel_expiry_participations restore criteria; E05 participation query tied to current round. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

Test reference: [expiry_cancels_unfinished_exchange_without_freezing_other_tree_inputs](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_races.rs:75), file SHA256 `c101bb783b275514d06785623567ff950c7552df221f4bf7f09e8ea5db3930cd` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_cancels_unfinished_exchange_without_freezing_other_tree_inputs proves component scope; actual successive canceled then accepted native round not fully established here. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** First root99k payout; second root can complete authentic new recovery or receive99k later; no residual canceled participation veto. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ROUND-CANCEL-SCOPE`.

<a id="ml119"></a>

## ML119 — Legacy partial forfeits leave an exchange with no terminal owner and no repair actor

**A. Title:** Legacy partial forfeits leave an exchange with no terminal owner and no repair actor.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** historical/backup state with current resolver.

**D. Context and financial state:** Backup/import has some signed forfeits stored but forfeited_at unset; original wallet absent; inputs/outputs span trees.

**E. Event chain and suspected mechanism:** Resolver sees any_forfeit and returns Unresolved; cancel_expiry_participations refuses partial rows; no automatic completion found.

**F. Expected correct behavior:** Preserve all binding rights and recover from authentic partial evidence without canceling accepted forfeit value.

**G. Potential incorrect outcome:** Inputs and replacement outputs can remain permanently blocked; impossible recovery if missing signatures never existed and clients gone.

**H. Evidence and inference:** E05 partially stored forfeits; E22 cancellation refuses any signed_forfeit_tx; trusted DB partial-backup assumption explicit. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

**I. Current coverage:** Native normal forfeit write is atomic; race tests do not establish repair of preexisting legacy/backup-inconsistent partial exchange. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Use genuine historical partial artifact; independently identify binding side per signed commitment; all gross preserved and eventual correct output, not a fabricated forfeit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `ROUND-PARTIAL-FORFEIT`.

<a id="ml120"></a>

## ML120 — Forfeited participation is trusted despite missing one of its financial inputs

**A. Title:** Forfeited participation is trusted despite missing one of its financial inputs.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Legacy participation says forfeited_at set, but one original/input or signed-forfeit row was omitted from restore.

**E. Event chain and suspected mechanism:** Resolver assigns all surviving originals operator based on marker; replacement principal cannot be proved complete from missing input.

**F. Expected correct behavior:** Verify whole binding exchange and preserve every individual entitlement across incomplete inventory.

**G. Potential incorrect outcome:** Customer original can be confiscated while no fully backed replacement is available; operator or another recipient receives missing principal.

**H. Evidence and inference:** E05 Exchange marker and any_forfeit logic; E24 complete backing proof; historicalE39 round group checks stricter full-input evidence. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

**I. Current coverage:** Current normal atomic forfeit tests do not cover a restored marker with missing membership/signatures. Assumption is authentic incomplete backup/migration, not arbitrary live DB tearing. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare exact signed participation input set/value and replacement set against prebackup oracle; no operator disposition without complete discharge evidence. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ROUND-FORFEIT-CLOSURE`.

<a id="ml121"></a>

## ML121 — Original user claim and later same-owner replacement payout both discharge one refresh

**A. Title:** Original user claim and later same-owner replacement payout both discharge one refresh.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Self-refresh accepts forfeits; original signed user path survives; replacement resides under different anchor; wallet loses reply and stops.

**E. Event chain and suspected mechanism:** Original user claim confirms under a surviving path; fallback later pays replacement because its entitlement ID/root is independent.

**F. Expected correct behavior:** A binding self-refresh principal must be economically discharged once across old claim and replacement, with actual enforceability of forfeit checked.

**G. Potential incorrect outcome:** Customer could receive original claim plus replacement fallback; operator loses duplicate principal across nonconflicting roots. Reachability depends on authentic forfeit/claim ordering.

**H. Evidence and inference:** E05 Forfeit/Exchange; E29 terminal claim tied to entitlement vtxo_id; client-binding-chain-recovery-handoff explicitly leaves server old-claim/replacement discharge open. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Different-recipient replacement test proves a transferred right survives; client chain-recovery pass claims replacement, not old-user-claim versus later fallback. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent two-root lineage ledger: one original100k principal, actual terminal receipts+fees exactly100k, no same-account extra99k after already discharged claim. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `ROUND-CROSS-ROOT-DISCHARGE`.

<a id="ml122"></a>

## ML122 — Suppressing a replacement after old-root recovery erases a legitimate transferred owner

**A. Title:** Suppressing a replacement after old-root recovery erases a legitimate transferred owner.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Alice refreshes then legitimately transfers replacement100k toBob; original backing later recovers under binding forfeit.

**E. Event chain and suspected mechanism:** A broad same-lineage duplicate detector interprets any original-root spend as discharge ofBob replacement and cancels his payout.

**F. Expected correct behavior:** Distinguish actual same-owner duplicate discharge from enforceable independent transferred replacement rights.

**G. Potential incorrect outcome:** Bob loses99k; operator owns original recovered backing while validBob entitlement disappears.

**H. Evidence and inference:** E05 ownership by current terminal; E25 binding boundary; testing/tests/server/expiry_replacement_transfer.rs. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

Test reference: [expiry_transferred_refresh_replacement_survives_old_backing_settlement](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_replacement_transfer.rs:18), file SHA256 `8e8f5b19033dc14ae11cf17479a2b93b96282cd1b8d23678012a08e59b1a7d8c` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_transferred_refresh_replacement_survives_old_backing_settlement actual different-recipient99k pass is meaningful; no current same-owner/transfer mixture with several replacements. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Bob original signed replacement100k yields99k once; original recovery cannot falsely eraseBob, andAlice cannot claimBob amount. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ROUND-TRANSFERRED-REPLACEMENT`.

<a id="ml123"></a>

## ML123 — Cross-tree cancellation restores an input already committed to another operation

**A. Title:** Cross-tree cancellation restores an input already committed to another operation.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Unfinished participation usesA andB;B also has a newer registered offboard/transfer when cancellation gathers its input list.

**E. Event chain and suspected mechanism:** Stale cancellation restoresB Spendable or clears spent_in_round after newer financial commitment without reread under all locks.

**F. Expected correct behavior:** Cancel whole old participation atomically while preservingB newer binding obligation and locks.

**G. Potential incorrect outcome:** B can be spent twice economically, or later valid entitlement loses its spend/owner metadata.

**H. Evidence and inference:** E22 cancel_expiry_participations lock order and guarded restore WHERE; E23 cancellation inside decision. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

**I. Current coverage:** Component cancellation fixture checks undecided cross-tree input; no actual three-operation old-refresh/new-offboard/payout contention. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** ObserveB exact newer signed operation; it remains authoritative, A99k fallback alone, aggregate customer value never exceedsA+B. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ROUND-CANCEL-RESTORE`.

<a id="ml124"></a>

## ML124 — Accepted refresh depends on replacement funding that disappears in a reorg

**A. Title:** Accepted refresh depends on replacement funding that disappears in a reorg.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Forfeits/unlock commitment irreversible; replacement funding transaction confirmed then reorged/evicted; old inputs fenced.

**E. Event chain and suspected mechanism:** Resolver permanently gives old input operator ownership; rebuilt replacement frontier absent; wallet/provider no longer active to rebroadcast.

**F. Expected correct behavior:** Preserve replacement financial obligation and actual signed funding recovery without reopening original for duplicate use.

**G. Potential incorrect outcome:** Customer loses entitlement at the gap between irrevocable forfeit and chain-dependent replacement backing; permanent if funding cannot be reestablished.

**H. Evidence and inference:** E05 Exchange/Forfeit stable layer; E30 reorg generation; E29 rebuilt frontier; E38 signed material durability. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Ordinary payout reorg preserves same parent funding; client self-refresh test reorgs final claim, not replacement issuance/funding loss. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After authentic fork, original100k obligation stays visible, exact replacement funding resumes or safe discharge occurs, final rightful99k once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `ROUND-FUNDING-REORG`.

<a id="ml125"></a>

## ML125 — Lost offboard finish reply leads a restored device to create a second payment

**A. Title:** Lost offboard finish reply leads a restored device to create a second payment.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Actual offboard100k registered; reply lost; local session expires; user reopens restored wallet before fallback decision.

**E. Event chain and suspected mechanism:** Client reprepares instead of adopting original signed tx; second offboard/fallback path competes without stable original intent link.

**F. Expected correct behavior:** Identify original durable offboard before any new payment; source locks/fence permit one discharge only.

**G. Potential incorrect outcome:** Two distinct onchain payments may debit operator if backed by different replacement paths, or rightful user blocked between sessions.

**H. Evidence and inference:** E22 guard and E05 offboard binding; testing/tests/bark_sdk/offboard.rs replay/adopt tests; offboards.rs durable registration. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

Test reference: [finish_offboard_replays_for_identical_retry](/home/francis/abandon-ship-fresh-integration/testing/tests/server/offboard.rs:150), file SHA256 `b9b78d75f518bd4b73f439835a4441dba1d2cffaa8b6dd1136325f2f8db02bd0` at inventory time. Listing/reference is not an execution claim.

Test reference: [offboard_recovers_lost_finish_by_adopting_chain_tx](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/offboard.rs:415), file SHA256 `69fbf20f6ea458b0afd1e863a20d529ea75632226dea2330e7dc22e88540810e` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** offboard_recovers_lost_finish_by_adopting_chain_tx and finish_offboard_replays_for_identical_retry cover ordinary behavior; not offboard lost reply plus restored DB/payout boundary. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Exact original source, destination and both possible signed txids; canonical outputs discharge100k only once and no new source silently substituted. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `OFFBOARD-IDEMPOTENCY`.

<a id="ml126"></a>

## ML126 — Registered offboard never enters a surviving broadcaster after a crash

**A. Title:** Registered offboard never enters a surviving broadcaster after a crash.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** register_offboard commits signed transaction; process stops before commit_offboard/nursery broadcast; customer stays offline.

**E. Event chain and suspected mechanism:** Ownership gives backing to operator on durable offboard; restored process fails to enqueue this transaction and expiry suppresses customer fallback.

**F. Expected correct behavior:** Autonomously rebroadcast exact registered transaction until confirmed or safely resolved; retain unpaid obligation meanwhile.

**G. Potential incorrect outcome:** User gets nothing while original source is no longer spendable and operator may sweep backing; permanent if signed offboard bytes lost.

**H. Evidence and inference:** E05 offboard lookup; final planC4 and described offboards.rs register/commit separation; E22 admission fence. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

**I. Current coverage:** Reverse native offboard race proves committed offboard wins and pays; no retained crash exactly between database register and broadcaster handoff. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Stored signed offboard pays exact rightful script/value; compare canonical tx to stored bytes; no fallback duplicate and no false paid status. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `OFFBOARD-BROADCAST-DURABILITY`.

<a id="ml127"></a>

## ML127 — One registered offboard input is lost from a multi-tree restore

**A. Title:** One registered offboard input is lost from a multi-tree restore.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Multi-input offboard spansA andB; partial restore retains transaction/flag onA but losesB source row or connector.

**E. Event chain and suspected mechanism:** A classified operator; broadcaster cannot reconstruct or spend complete input set;B may separately regain Spendable.

**F. Expected correct behavior:** All offboard inputs/transaction/financial fence restore atomically or remain explicitly unresolved together.

**G. Potential incorrect outcome:** No customer offboard payment, possible double use ofB, andA rightful value permanently hidden under operator classification.

**H. Evidence and inference:** E05 Offboard; E24 complete source inventory; E22 guard actual input set. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

**I. Current coverage:** Native offboard race uses genuine complete input data; no mismatched source/transaction backup family. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Reconstruct exact signed transaction inputs and prevouts from independent original artifact; conserve combined200k and show one actual customer discharge. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `OFFBOARD-INPUT-CLOSURE`.

<a id="ml128"></a>

## ML128 — Fee requote after an interrupted offboard signs a changed customer amount

**A. Title:** Fee requote after an interrupted offboard signs a changed customer amount.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** User approved offboard output amount under fee quote; finish reply lost; session expires and fees change.

**E. Event chain and suspected mechanism:** Recovery reprepares with new quote/output while old signed financial intent may still be viable; displayed refund/amount follows different intent.

**F. Expected correct behavior:** Require explicit consistent original authorization and reconcile earlier signed tx before committing changed economics.

**G. Potential incorrect outcome:** User underpaid or operator double-pays old/new outputs; permanently ambiguous history can suppress correct recovery.

**H. Evidence and inference:** Offboards.rs durable registration boundary described in plan; E22; SDK offboard state checkpoints. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

Test reference: [offboard_cancels_when_fees_change_after_lost_session](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/offboard.rs:471), file SHA256 `69fbf20f6ea458b0afd1e863a20d529ea75632226dea2330e7dc22e88540810e` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** offboard_cancels_when_fees_change_after_lost_session tests ordinary cancellation; no combined fallback decision/old signed tx still confirmable on different branch. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare exact approved script/amount, old/new signatures and canonical outputs; only authorized one payment counted, all deductions explicit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `OFFBOARD-FEE-CONTRACT`.

<a id="ml129"></a>

## ML129 — Signed Ark transfer is delivered while server still records it Unregistered

**A. Title:** Signed Ark transfer is delivered while server still records it Unregistered.

**B. Financial invariant:** The accepted ownership contract determines the beneficiary before value is irreversibly allocated; unresolved policy cannot be replaced with an invented rule.

**C. Applicable implementation:** current.

**D. Context and financial state:** Alice signed transfer100k toBob; signed bytes reachBob but server upload fails; both wallets then disappear.

**E. Event chain and suspected mechanism:** Current C4 attributes Unregistered leaf to input ownerAlice; mailbox recipientBob and signed transfer suggest different entitlement outside registered contract.

**F. Expected correct behavior:** Apply accepted C4 consistently and explicitly resolve whether signed delivery before registration transfers entitlement; do not invent a new policy.

**G. Potential incorrect outcome:** Alice may receive99k afterBob believed he received100k; with no accepted policy on this boundary, catalogue cannot call it resolved.

**H. Evidence and inference:** E05 Unregistered input-owner rule; E38 commit-before-sign/registration; final planC4 explicitly says unregistered output belongs input owner. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Ordinary mailbox/registration tests do not prove financial ownership at delivered-but-unregistered boundary; strong registered receive rule is a special case. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Freeze actual signed delivery, registration failure and independently declared contract; expected payee must be ruled before later test, whole lineage100k conserved. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `TRANSFER-REGISTRATION-CONTRACT`.

<a id="ml130"></a>

## ML130 — Funded board is abandoned before the server records its customer claim

**A. Title:** Funded board is abandoned before the server records its customer claim.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** User funds board100k and obtains usable signed board material, then stops before server registration; backing confirms/expires.

**E. Event chain and suspected mechanism:** Server tree inventory or permanent attribution never sees owner/claim; ordinary watchman path may reclaim funding without recognizing customer.

**F. Expected correct behavior:** Account for authenticated funded board entitlement even when original user never returns; state exact supported commitment boundary.

**G. Potential incorrect outcome:** Customer100k can become operator money or remain stranded; fresh design explicitly lacks full unregistered pre-board abandonment evidence.

**H. Evidence and inference:** E24 stored-row inventory; E04 confirmed frontier query; docs/fallback-lightning-evidence.md remaining pre-board gate; historical TrackB pending_board_return reference. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

Test reference: [expiry_payout_native_board_seed_restore_and_confirmed_spend](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_payout_flow.rs:14), file SHA256 `2fbe0d5b50f67f5d2afbd6d9faa7e8bb380104fdb9301162fd541ad03721bbb1` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_payout_native_board_seed_restore_and_confirmed_spend boards and registers first; historical pending-board tests are not fresh implementation qualification. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent original funding/signed board/owner registration oracle; user99k actual payout or authentic documented nonbinding refund, never unaccounted operator sweep. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `BOARD-PRE-REGISTRATION`.

<a id="ml131"></a>

## ML131 — Decision locks a customer leaf while competing writer locks its backing ancestor

**A. Title:** Decision locks a customer leaf while competing writer locks its backing ancestor.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Prepared fallback covers leafBob under rootR; concurrent financial operation changes ancestorR or a checkpoint.

**E. Event chain and suspected mechanism:** Each transaction locks different vtxo rows; neither sees another decided coin; one source path can acquire conflicting obligations.

**F. Expected correct behavior:** Shared lock domain must cover every obligation-changing ancestor/descendant, not just matching leaf IDs.

**G. Potential incorrect outcome:** Wrong owner or double customer entitlement despite each table having unique per-coin fence.

**H. Evidence and inference:** E23 versions all selected visited rows; E22 caller-provided IDs; E38 input update; E24 path containment. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

Test reference: [expiry_coin_fence_serializes_actual_tree_writes](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_races.rs:15), file SHA256 `c101bb783b275514d06785623567ff950c7552df221f4bf7f09e8ea5db3930cd` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_coin_fence_serializes_actual_tree_writes proves same existing row locking; not every ancestor/leaf alternate-input API. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Native competing ancestor/leaf operation yields one valid covering path for100k, one rightful99k discharge and no duplicate external successor. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-LOCK-DOMAIN`.

<a id="ml132"></a>

## ML132 — Signed transaction registration reopens a source already fenced for payout

**A. Title:** Signed transaction registration reopens a source already fenced for payout.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Payout committed for100k; stale wallet uploads a signed successor obtained before decision but not registered.

**E. Event chain and suspected mechanism:** Registration helper changes spend_state or inserts descendants without rereading expiry_decided_coin under shared row locks.

**F. Expected correct behavior:** Every registration/import tree write respects permanent decision and canceled participation fences.

**G. Potential incorrect outcome:** Fallback plus newly spendable successor can pay twice; user sees revived balance that cannot safely be honored.

**H. Evidence and inference:** E22 required guard; E23 decided+Spent atomic; E38 registration-dependent outputs. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Coin-fence component tests cover actual tree writes but not all mixed protocol registration versions or legacy import helpers; no blanket coverage claim. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After decision, authenticated stale signed upload cannot create new payable principal; parent99k only, preserved receipt/history unchanged. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-WRITER-COVERAGE`.

<a id="ml133"></a>

## ML133 — Missing input row means FOR UPDATE guards no row before insertion

**A. Title:** Missing input row means FOR UPDATE guards no row before insertion.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Tree writer references an ID absent in its transaction snapshot; later insert/upsert materializes that path while physical ancestor already allocated.

**E. Event chain and suspected mechanism:** guard_obligation_changes locks only existing rows and does not require returned count=expected; fresh row can avoid a prior leaf fence.

**F. Expected correct behavior:** Validate complete actual sources and their fenced ancestry before inserting any financial successor.

**G. Potential incorrect outcome:** Unfenced descendant can become a second claim; or missing source erases entitlement from revalidation.

**H. Evidence and inference:** E22 guard query and empty/missing-row behavior; E24 completeness; E38 execute_vtxo_tree_update. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Existing-row fence race has all source rows present; no inspected native missing-row/import phantom. Assumption: supported writer can introduce absent source identity. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Absent-source request must fail atomically or prove parent path unreserved; exact100k lineage receives no duplicate99k. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-PHANTOM`.

<a id="ml134"></a>

## ML134 — Financial state changes while a writer waits for the decision lock

**A. Title:** Financial state changes while a writer waits for the decision lock.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Writer prevalidates sourceSpendable; waits on lock; another transaction registers offboard or forfeit/payout.

**E. Event chain and suspected mechanism:** After lock acquired writer uses cached VtxoState instead of fresh state/fence; stale validation authorizes incompatible commitment.

**F. Expected correct behavior:** Reread actual obligation under lock immediately before binding write/signatures.

**G. Potential incorrect outcome:** Second signed obligation can escape after first committed; wrong user/operator payment or source becomes unrecoverable.

**H. Evidence and inference:** E22 guard rereads fence; E23 version revalidation; E38 initial reads precede write transaction. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Native offboard and forfeit races cover both winner orders; they do not cover every registration, revocation, pool or round-finalization writer. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** For each real endpoint later tests identify first binding commit and prove second has no new valid financial signature/payment, one100k entitlement only. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-STALE-RECHECK`.

<a id="ml135"></a>

## ML135 — Changed output is dropped but its fee or gross remains in surviving group

**A. Title:** Changed output is dropped but its fee or gross remains in surviving group.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Ready rootsA/B planned together;B changes owner/spend state whileA remains valid.

**E. Event chain and suspected mechanism:** Phase2 dropsB but reuses prior allocator/output plan;A pays a wrong amount orB omitted value included in operator drain.

**F. Expected correct behavior:** Recompute allocation only from surviving revalidated outputs with current script grouping/dust policy.

**G. Potential incorrect outcome:** A underpaid, B double-paid, or operator retains customer remainder; discrepancy may be hidden by one correct output.

**H. Evidence and inference:** E23 accepted list then allocate; E26 fee/dust grouping. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** Current source explicitly recomputes; no retained genuine changed-native-output plus script/dust-threshold race across phase1/phase2. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent survivorA100k pays99k;B exact newer obligation remains separate; input/output conservation verified from actual prevouts. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-REVALIDATION-ALLOCATION`.

<a id="ml136"></a>

## ML136 — A stale Pool state makes a registered customer output operator-owned

**A. Title:** A stale Pool state makes a registered customer output operator-owned.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current resolver; legacy/migration-state assumption.

**D. Context and financial state:** A genuine100k customer Pubkey output survived pool issuance/transfer, but a mixed-version upsert or restore leaves its spend_state=Pool; encoded key and registered xpub still identifyAlice.

**E. Event chain and suspected mechanism:** The current cut reaches this output; propose_expiry_ownership handles SpendState::Pool before authenticating customer attribution; phase2 preserves operator disposition and sweeps its value to drain.

**F. Expected correct behavior:** A pool marker must represent actual trusted operator ownership; a stale state cannot erase an accepted signed customer transfer.

**G. Potential incorrect outcome:** Operator receivesAlice100k backing; Alice99k fallback missing after irreversible sweep. Reachability of this stale Pool row through a supported migration/upsert must be proven, not fabricated as a live bug.

**H. Evidence and inference:** E05 Pool early-return; E24 encoded metadata checks do not themselves authenticate SpendState::Pool; E38 customer tree updates. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E38: [server/src/arkoor.rs:95](/home/francis/abandon-ship-fresh-integration/server/src/arkoor.rs:95), `cosign_oor_with_builder`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `32056594c0db2a387b761bf140240f15c647d716200e35a265aa458350cf7fbc`. Code fact: Unsigned outputs, checkpoint links, Fulfilled receive and binding materialization commit before server signatures return.

**I. Current coverage:** Normal pool/registered receive tests use consistent states; current-cut geometry units do not construct a genuine old-version pool-to-customer state migration. No retained stale-Pool financial outcome. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Begin with authentic signed customer source and independently knownAlice key; after supported migration/update the source must yieldAlice99k or stay explicitly unpaid, never drain as pool value. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-STATE-OWNERSHIP`.

<a id="ml137"></a>

## ML137 — Physical root is progressed while a grouped customer payout already reserves it

**A. Title:** Physical root is progressed while a grouped customer payout already reserves it.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Allocation covers several leaves but stores rootR as actual input; upstream watchman progressesR independently.

**E. Event chain and suspected mechanism:** Only leaf decided fences are checked; root reservation omitted; progress spendsR before immutable payout can confirm.

**F. Expected correct behavior:** Serialize all physical input spends and progression with committed group reservations.

**G. Potential incorrect outcome:** All group recipients stranded behind unbroadcastable parent or paid status without actual money; regroup must preserve every entitlement.

**H. Evidence and inference:** E28 lock_unallocated_watchman_inputs; E44 upstream claim/progress paths; E23 allocation_input. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

**I. Current coverage:** Requested split test explicitly checks normal watchman reservation refusal after first real payout; no concurrent two-watchman publication race. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** R can be consumed by exactly one correct path; every customer gross remains mapped to actual descendants/payout and no paid flag without confirmation. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-PHYSICAL-RESERVATION`.

<a id="ml138"></a>

## ML138 — Deadlock retries always sacrifice the oldest cross-tree entitlement

**A. Title:** Deadlock retries always sacrifice the oldest cross-tree entitlement.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Large multi-input cancellation/decision and normal forfeit writer repeatedly acquire intersecting coin/participation/settlement locks.

**E. Event chain and suspected mechanism:** Lock-order cycle causes oldest allocation to abort; immediate competing traffic wins every retry; cursor/report records only successful newer work.

**F. Expected correct behavior:** Consistent ordering and fair retries must eventually settle old rightful liability while preserving safety.

**G. Potential incorrect outcome:** Old customer100k waits forever despite sustained green newer transactions; no duplicate payment is needed.

**H. Evidence and inference:** E22 cancellation gathers cross-tree inputs then participation rows; E23 coin/settlement locks; E11/E15 alternate writer locks. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E11: [server/src/database/ln/mod.rs:242](/home/francis/abandon-ship-fresh-integration/server/src/database/ln/mod.rs:242), `admit_lightning_dispatch`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4080bb5ea63fc873652a40f0c423dcf8fff9905aa1bc9fa018b48dce0d0611c1`. Code fact: Dispatch row commits before network call; one row per attempt forbids replay, including after restart.

E15: [server/src/ln/mod.rs:461](/home/francis/abandon-ship-fresh-integration/server/src/ln/mod.rs:461), `revoke_lightning_pay_htlcs`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e54d3010c94db288b4477da980268f043cfa09c18567868e3046fd7f8ee1bb2e`. Code fact: Sorted source guards and settlement barrier precede source-state checks; only source-funded internal subscriptions may be canceled.

**I. Current coverage:** Two-winner native races are finite; they do not measure repeated deadlock/serialization victims under continuous traffic. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Retain original entitlementID and age, capture actual DB waits/retries, prove99k eventual payment at admitted workload without dropping liability. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DECISION-LOCK-LIVENESS`.

<a id="ml139"></a>

## ML139 — Historical allocation reservation blocks all later legitimate progression

**A. Title:** Historical allocation reservation blocks all later legitimate progression.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Old allocation of R became conflicted, its historical input rows remain, and that physical input is currently unspent and needs a new ordinary signed progress step. A conflict alone does not establish this state; untouched roots may regroup directly.

**E. Event chain and suspected mechanism:** lock_unallocated_watchman_inputs rejects any expiry_allocation_input row without status filter; historical reservation can veto current recovery forever.

**F. Expected correct behavior:** Permanent obligation fences coexist with current physical-path progression; superseded reservations must not suppress valid recovery.

**G. Potential incorrect outcome:** Customer principal remains stranded after conflict even if new source path is recoverable.

**H. Evidence and inference:** E28 lock_unallocated_watchman_inputs query; E29 conflicted allocations retained. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Partial-conflict test regroups an untouched root directly; it does not require upstream progress through a previously reserved conflicted physical input. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After authentic conflict/reorg, enumerate active path and verify required legitimate signed progress is allowed while duplicate old payout cannot confirm. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `DECISION-RESERVATION-LIFETIME`.

<a id="ml140"></a>

## ML140 — Same-expiry ready roots cross the 64-row pass boundary and pay avoidable extra fees

**A. Title:** Same-expiry ready roots cross the 64-row pass boundary and pay avoidable extra fees.

**B. Financial invariant:** Only authorized fee, dust, timing and grouping policy can reduce or retire customer value; ambiguous policy remains an open requirement.

**C. Applicable implementation:** current.

**D. Context and financial state:** More than 64 independently ready roots share expiry. Alice has 1,200 sat on adjacent pages. An independent serialized-size calculation must establish that the simultaneously ready roots could actually fit after grouping.

**E. Event chain and suspected mechanism:** Each pass commits its own group before seeing later already-ready roots; two subdust fragments forfeit instead of one1400sat output.

**F. Expected correct behavior:** Resolve P3's opportunistic ready-set scope. P1 permits separate outputs/fees; pagination is not itself proof that all fitting roots must form one group.

**G. Potential incorrect outcome:** Alice can lose2400sat rather than receive1400; possible requirement mismatch depends on owner meaning of ready-set boundary, not arithmetic.

**H. Evidence and inference:** E04 ROOTS_PER_PASS64 and ready grouping per pass; E26 per-output policy; final planP1/P3. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** Separate-output allocator unit proves allowed fee/dust effects; no native >64 simultaneously-ready grouping oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Freeze readiness before pass and exact serialized size; accepted P3 ruling determines whether one1400 output is required, then observe canonical result. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `GROUP-SCHEDULING`.

<a id="ml141"></a>

## ML141 — One revalidation error rolls back other independently ready trees

**A. Title:** One revalidation error rolls back other independently ready trees.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Ready rootsA/B grouped same expiry;B becomes malformed only after preparation;A remains valid.

**E. Event chain and suspected mechanism:** propose_expiry_ownership/payee lookup returns Err inside phase2; ? aborts whole group rather than droppingB; deterministic retries keep them together.

**F. Expected correct behavior:** ErroredB retains unpaid liability while independently readyA gets its conserving payout.

**G. Potential incorrect outcome:** A99k can wait indefinitely behind unrelated persistentB error, despite normal readiness-isolation pass.

**H. Evidence and inference:** E23 revalidation await?; E04 commit_ready_group logs group error; PAYOUT_REVIEWPR07. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

Test reference: [expiry_ready_root_does_not_wait_for_unresolved_same_expiry_root](/home/francis/abandon-ship-fresh-integration/testing/tests/server/qualification_readiness.rs:54), file SHA256 `7aa2e8fee5e13b2edca47bf1ed2079f6334d1c0a3c5b5979ec7ad508cfd97c69` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_ready_root_does_not_wait_for_unresolved_same_expiry_root keepsB unresolved in phase1; no error introduced only during short locked revalidation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Repeat genuineB error at commit;A100k pays99k within bound, B preserved/unpaid, no partial illegal group commit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `GROUP-ERROR-ISOLATION`.

<a id="ml142"></a>

## ML142 — Malformed pending payout prevents every other group from reaching broadcaster isolation

**A. Title:** Malformed pending payout prevents every other group from reaching broadcaster isolation.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Many valid pending groups plus one damaged saved transaction/script after migration or partial backup.

**E. Event chain and suspected mechanism:** pending_expiry_payouts decodes whole vector before per-group loop; one decode error aborts all groups each tick.

**F. Expected correct behavior:** Isolate unreadable group without authorizing it; valid groups continue and damaged liability stays explicit.

**G. Potential incorrect outcome:** All recipients unpaid indefinitely; healthy daemon logs recurring generic error.

**H. Evidence and inference:** E28 pending_expiry_payouts full decode; E27 per-group catch only after load. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** Normal payout tests have valid groups; no malformed saved group alongside valid canonical99k payout. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Preserve damaged evidence, verify validA99k confirms and damagedB is not swept/omitted from unpaid report. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `PAYOUT-LOADER-ISOLATION`.

<a id="ml143"></a>

## ML143 — Different-expiry value is merged into one premature payout group

**A. Title:** Different-expiry value is merged into one premature payout group.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Alice owns100k expiringE and100k expiringE+100; first mature, second still ordinary spendable.

**E. Event chain and suspected mechanism:** Grouping uses destination alone; second principal fenced early or first waits for later maturity.

**F. Expected correct behavior:** Only independently mature equal-expiry roots group; earlier pays without waiting.

**G. Potential incorrect outcome:** Second entitlement disappears before allowed decision or earlier99k never arrives.

**H. Evidence and inference:** E23 same-expiry/maturity ensure; E04 grouping by height. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Allocator unit has no chain expiry; grouping/conflict tests deliberately use equal expiry. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** AtE only first100k consumed/99k paid; second remains usable until its own decision; no cross-height fee mixing. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `GROUP-EXPIRY-BOUNDARY`.

<a id="ml144"></a>

## ML144 — Split placeholder and final child payout both charge one principal a fee

**A. Title:** Split placeholder and final child payout both charge one principal a fee.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** One ready output exceeds size and splits through two levels before customer payout.

**E. Event chain and suspected mechanism:** Intermediate allocation/history deducts1000 despite no customer output; final child deducts another1000.

**F. Expected correct behavior:** Only actual paying outputs incur1000; intermediate splits use operator miner funding.

**G. Potential incorrect outcome:** Customer underpaid; artificial intermediate deductions can make valid final payout dust.

**H. Evidence and inference:** E26 output fee; E28 split request without payout; E45 split status; planC8. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E45: [server/src/database/fork_migrations/V5__expiry_allocations.sql:91](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V5__expiry_allocations.sql:91), `expiry_ledger_commit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `3712331243e47c147d3387b5887f02eeccb0d7a59e2b273ba9d4d55574ecea42`. Code fact: Permanent one-active allocation index; append-only visible-transaction revision/count; triggers across payout ledger.

**I. Current coverage:** Small requested split proves two real49k outputs; no runtime for multilevel placeholders and one final output per recipient. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Each5000 leaf yields4000 actual output; intermediate customer fee0; operator miner costs separately conserved. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FEE-SPLIT-STAGES`.

<a id="ml145"></a>

## ML145 — Merged output charges once per leaf instead of once per output

**A. Title:** Merged output charges once per leaf instead of once per output.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Twenty100sat leaves share script and are simultaneously ready in one fitting group.

**E. Event chain and suspected mechanism:** Persisted financial projection repeats1000 deduction per leaf or subtracts aggregate fee twice.

**F. Expected correct behavior:** Combined2000 makes one1000 output with fee1000 and stable leaf shares.

**G. Potential incorrect outcome:** Payable customer value becomes dust/zero or unexplained operator remainder.

**H. Evidence and inference:** E26 allocate; E23 stored per-coin values. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

**I. Current coverage:** direct_fee_many_small_leaves_pay_one_non_dust_output proves pure allocator, not DB/RPC/wallet-to-chain accounting. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual parent has one1000 rightful output; leaf fee sum1000/gross2000; audit all other group outputs. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FEE-AGGREGATION`.

<a id="ml146"></a>

## ML146 — Dust policy uses a fixed address assumption instead of actual script

**A. Title:** Dust policy uses a fixed address assumption instead of actual script.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Actual net is at script-specific dust boundary; app uses any supported standard script.

**E. Event chain and suspected mechanism:** Allocator/builder/history uses generic330 or wallet P2WPKH threshold; valid output forfeited or nonstandard output committed.

**F. Expected correct behavior:** Apply exact script dust policy and explain relay-dependent waits.

**G. Potential incorrect outcome:** Customer loses valid payout to operator or committed group never relays.

**H. Evidence and inference:** E01 standard scripts; E26 minimal_non_dust; E27 is_standard. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E01: [lib/src/fallback.rs:41](/home/francis/abandon-ship-fresh-integration/lib/src/fallback.rs:41), `FallbackRecord`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `1935bc0eab2b40adc0b5fc4779c7e49e90975ef32601751cb610938c430257a1`. Code fact: Signatures bind network, mailbox, script and account; account child zero proves account possession, not destination spendability.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** direct_fee_script_dust_equality_is_paid covers all five in memory; no actual Core admission for each script/current relay configuration. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** At every true threshold observe exact payout or correct explicit forfeiture using real prevouts/script, never substitute address. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FEE-SCRIPT-DUST`.

<a id="ml147"></a>

## ML147 — Historical minimum payout suppresses a fresh script-dust-valid entitlement

**A. Title:** Historical minimum payout suppresses a fresh script-dust-valid entitlement.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** historical config/draft migration.

**D. Context and financial state:** Old TrackA minimum1000 imported into fresh profile; gross1294sat P2WPKH should net294.

**E. Event chain and suspected mechanism:** Legacy scheduler filter defers/forfeits a payout permitted by accepted freshP1.

**F. Expected correct behavior:** Fresh per-output fee/script dust governs; old route minimum cannot silently override.

**G. Potential incorrect outcome:** Customer never gets valid294sat and operator may retain unsupported forfeiture.

**H. Evidence and inference:** E42 historical filter/config; E26 fresh allocator; planP1. Historical context/positive guard does not establish an earlier actual failing execution; classify the proposed reachability conservatively as a hypothesis. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E42: [server/src/expiry_payout.rs:402](/home/francis/abandon-ship-bark-two-step-comparison/server/src/expiry_payout.rs:402), `expiry_payout_tick`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `5a2a5c62e1691f77df2bdc0565b05b94e434e5ff7aabf70d700138f9a2d0a566`. Code fact: Historical two-step route requires real fee estimate and swept-path proof; default sweep minimum100 confirmations; different fee/minimum policy.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** Allocator equality unit is current pure math; no mixed TrackA operational configuration migration. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual294 output and1000fee from gross1294; no legacy threshold silently affects canonical result. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FEE-LEGACY-POLICY`.

<a id="ml148"></a>

## ML148 — Subdust operator remainder is burned or counted twice through fee anchor

**A. Title:** Subdust operator remainder is burned or counted twice through fee anchor.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Correct customer prefix leaves200sat operator remainder below drain dust.

**E. Event chain and suspected mechanism:** NonzeroP2A passes200 into fee child; accounting assumes zero anchor or counts value twice as subsidy.

**F. Expected correct behavior:** Parent zero-fee conservation and child prevout conservation count anchor once.

**G. Potential incorrect outcome:** Operator treasury loss/misreported cost can hide deficit or prompt an incorrect refund/top-up.

**H. Evidence and inference:** E27 remainder branch; E26 payout_fee_accounting; E37 anchor spend. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

**I. Current coverage:** expiry_fee_accounting_includes_the_drain_and_nonzero_anchor is unit math; no actual nonzero-anchor canonical child with replacement oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Customer net unchanged; real child inputs/outputs show remainder200 once and subsidy=max(minerfee-200,0). Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FEE-OPERATOR-REMAINDER`.

<a id="ml149"></a>

## ML149 — Validated parent prefix hides an unintended extra residual payee

**A. Title:** Validated parent prefix hides an unintended extra residual payee.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Customer99k prefix matches stored plan; other operator/sibling residual remains.

**E. Event chain and suspected mechanism:** Internal construction/import bug adds tail output to unintended user; store_expiry_parent checks prefix and total but not all tail roles.

**F. Expected correct behavior:** Every residual goes only to accepted drain/P2A; no undeclared customer output.

**G. Potential incorrect outcome:** Wrong user receives residual; correct99k alone conceals additional wrong payment.

**H. Evidence and inference:** E28 store_expiry_parent prefix/sum; E27 trusted builder drain/anchor. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** Whole-root receive test excludes provider but no malformed conserving tail storage case. Assumption is internal bug, not untrusted DB writer. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Enumerate actual outputs with exact role/script/value, including all tail outputs and sponsor change. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `PAYOUT-CONSTRUCTION-TAIL`.

<a id="ml150"></a>

## ML150 — Restart rebuilds stored parent with changed locktime or witness

**A. Title:** Restart rebuilds stored parent with changed locktime or witness.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Signed parentP durable; unconfirmed; restart after chain tip advances.

**E. Event chain and suspected mechanism:** Rebuilder generatesP2 rather than loadingP; fee child/history bindP, broadcaster usesP2 or CAS rejects repeatedly.

**F. Expected correct behavior:** Rebroadcast exact stored parent; only child changes.

**G. Potential incorrect outcome:** False transaction receipt or permanent pending group; same-input exclusion prevents canonical double spend but not liveness loss.

**H. Evidence and inference:** E28 parentCAS; E27 parent loading. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** Actual6/101-block reorg and fee tests assert parent immutability; no signer/config-change restart case. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Stored serializedP/txid stable; actual99k confirms and every sponsor child spendsP anchor. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `PAYOUT-IMMUTABILITY`.

<a id="ml151"></a>

## ML151 — Core rejects package while server records broadcast

**A. Title:** Core rejects package while server records broadcast.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Parent/child submitted; Core returns transportOk with package/per-transaction rejection fields.

**E. Event chain and suspected mechanism:** submit_package returns raw SubmitPackageResult; caller ignores fields and sets broadcast_at.

**F. Expected correct behavior:** Broadcast reflects actual admission; rejection cannot earn financial completion.

**G. Potential incorrect outcome:** History shows Broadcast with no mempool/canonical payment; persistent bad child strands group.

**H. Evidence and inference:** E27 result ignored; server/src/bitcoind.rs:202 raw call; E35 broadcast stage. E47. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E35: [server/src/database/expiry_details.rs:99](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_details.rs:99), `fallback_payout_details_by_account`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `292206c5c2745aff5e4fc7abacada2b7cb435c305674f2886958c91de86e9c5c`. Code fact: Account-owned history uses immutable registration, stable IDs and global view token; 2MiB page budget and 20,001-row guard.

E47: [server/src/bitcoind.rs:202](/home/francis/abandon-ship-fresh-integration/server/src/bitcoind.rs:202), `submit_package`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `189ae26c24b2f37f165bc2908ee93cb559683c9e87e536dbed21acd8f0cb7623`. Code fact: Returns raw Core SubmitPackageResult; transport success is distinct from package/transaction admission.

**I. Current coverage:** Successful package cases do not test structured rejection with transportOk and payout-history assertions. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent parent/child admission plus exact outputs; rejected package remains unpaid/actionable, eventual correct99k confirms. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `PAYOUT-RPC-RESULT`.

<a id="ml152"></a>

## ML152 — Saved fee child survives failed watchman-wallet persistence

**A. Title:** Saved fee child survives failed watchman-wallet persistence.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Child C using confirmed sponsor U is saved in payout DB, but wallet commit/persist fails before C is broadcast. After restart a newly ready, earlier-expiry group D sorts before C; chain sync cannot recover an unbroadcast reservation.

**E. Event chain and suspected mechanism:** Wallet still offers U. Processing D first reserves/spends U, then saved C is retried using the same sponsor. Source ordering and reservation recovery must be exercised; overlap is not established by persistence ordering alone.

**F. Expected correct behavior:** Reconcile durable sponsor reservations between DB and wallet; no independent live child families silently shareU.

**G. Potential incorrect outcome:** One group remains unpaid despite positive funds; principal still owed after wallet state loss.

**H. Evidence and inference:** E27 DB child before wallet.persist; E28 saved child; E37 selection. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

**I. Current coverage:** Normal restart tests preserve wallet; no persister failure at this exact two-store boundary. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** EnumerateU attempts after restart; canonical sponsor spend counted once, all rightful99k obligations eventually pay. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `FUNDING-DUAL-DURABILITY`.

<a id="ml153"></a>

## ML153 — Unadmitted high-fee saved child permanently suppresses sponsor reconstruction

**A. Title:** Unadmitted high-fee saved child permanently suppresses sponsor reconstruction.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Latest childC claims fee>=target but sponsor spent elsewhere orC violates relay; alternate confirmed funds available.

**E. Event chain and suspected mechanism:** Broadcaster selects savedC by recorded fee and resubmits invalid bytes each tick instead of rebuilding usable child.

**F. Expected correct behavior:** Keep parent fixed and recover actual usable fee sponsorship based on chain/admission evidence.

**G. Potential incorrect outcome:** Customer group pending forever even with usable alternate watchman funds.

**H. Evidence and inference:** E27 fee_child selection/reuse; E28 latestID query. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** Fee-rise test confirms valid replacement; no sufficient-fee invalid latest child with healthy alternate funding. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Check actual sponsor prevouts/package rejection; later valid child confirms exact parent99k without changing customer amounts. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `FUNDING-CHILD-SELECTION`.

<a id="ml154"></a>

## ML154 — Unconfirmed latest fee child replaces canonical miner cost in accounting

**A. Title:** Unconfirmed latest fee child replaces canonical miner cost in accounting.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** ChildC1 confirmed; higherIDC2 saved but not confirmed or reorged out.

**E. Event chain and suspected mechanism:** Report chooses latestID or sums attempts rather than canonical spend.

**F. Expected correct behavior:** Count actual canonical fee once; retain unsuccessful variants as history.

**G. Potential incorrect outcome:** False expense/profit hides treasury deficit and can misallocate funds/refunds.

**H. Evidence and inference:** E28 pending latest child; E29 independent canonical confirmations; E26 accounting. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** Fee replacement test counts one confirmed replacement; no late unbroadcast higherID after original confirmation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compute actualC1 fee from prevouts/outputs once; C2 canonical expense0; reconcile whole cohort. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FUNDING-CANONICAL-COST`.

<a id="ml155"></a>

## ML155 — Stalled package RPC holds the chain-validity barrier during a reorg

**A. Title:** Stalled package RPC holds the chain-validity barrier during a reorg.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Payout write holds FOR SHARE view lock while Core submitpackage stalls; invalidation arrives.

**E. Event chain and suspected mechanism:** Invalidation waits for exclusive lock; all safe decisions/rebuilds wait behind network operation.

**F. Expected correct behavior:** Bound wait/recover barrier while retaining durable obligations and invalid-view exclusion.

**G. Potential incorrect outcome:** Independent payouts and deadline-sensitive work can stop behind one stalled package.

**H. Evidence and inference:** E27 DB closure spansRPC; E30 view locks. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

**I. Current coverage:** Ordinary reorg tests do not hang Core packageRPC during invalidation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Owned transport stall later ends/times out; generation rebuild resumes, no invalid-view decision andA99k eventually pays. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `PAYOUT-LOCK-LIVENESS`.

<a id="ml156"></a>

## ML156 — Same-chain mempool eviction leaves a broadcast payout absent forever

**A. Title:** Same-chain mempool eviction leaves a broadcast payout absent forever.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Same-chain eviction/node restart removes unconfirmed parent and child; saved child remains valid and sponsor remains usable. broadcast_at is retained.

**E. Event chain and suspected mechanism:** Current code retries all pending groups regardless of broadcast_at. Validate whether restart scheduling and actual relay restore the evicted package; any failure requires a demonstrated missed wake/relay condition rather than assuming Broadcast is terminal.

**F. Expected correct behavior:** Identical parent returns with usable fee child until canonical receipt.

**G. Potential incorrect outcome:** Customer99k waits indefinitely behind stale Broadcast.

**H. Evidence and inference:** E27 all-pending broadcaster; E28 exact stored bytes. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** Reorg test permits package retention; source all-pending retry is a positive guard. No forced same-chain loss of both parent/child with still-valid sponsor and independent reappearance oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Prove actual absence then reappearance/confirmation of exactP99k; no rewritten parent or second principal. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `PAYOUT-REBROADCAST`.

<a id="ml157"></a>

## ML157 — Positive watchman balance has no confirmed usable sponsorship

**A. Title:** Positive watchman balance has no confirmed usable sponsorship.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Committed payout; watchman100k total all unconfirmed top-up outputs.

**E. Event chain and suspected mechanism:** Confirmed-only selection blocks child; monitor checks total balance and overlooks unpaid entitlement.

**F. Expected correct behavior:** Alert on usable confirmed funds and resume when authentic funding confirms.

**G. Potential incorrect outcome:** Customer99k delayed indefinitely despite positive display; operator still owes100k gross.

**H. Evidence and inference:** E27 min_cpfp branch; E37 exclude_unconfirmed; planP2. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

**I. Current coverage:** Native fixtures confirm sponsors first; no retained exact unconfirmed-only warning/top-up lifecycle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Before funding confirmation payout0/unpaid100k; afterward99k canonical at same script/fee, specific unusable-funds alert. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `FUNDING-USABLE-CONFIRMED`.

<a id="ml158"></a>

## ML158 — Sole confirmed sponsor cannot fund replacement after its first child spends it

**A. Title:** Sole confirmed sponsor cannot fund replacement after its first child spends it.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** One large confirmedU funds first childC; C unconfirmed; target fee rises.

**E. Event chain and suspected mechanism:** New builder selects only unspent confirmed funds;U spent, C change unconfirmed, so bump fails despite ample change.

**F. Expected correct behavior:** Preserve parent and accepted operator funding recovery; distinguish unavailable change from real exhaustion.

**G. Potential incorrect outcome:** Every customer waits for new confirmed top-up or old child confirmation.

**H. Evidence and inference:** E27 replacement new-builder; E37 confirmed-only selection; reorg test supplies two sponsors. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

Test reference: [expiry_payout_fee_replacement_keeps_parent_and_counts_only_confirmed_child](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_reorg.rs:169), file SHA256 `40785c812b8c78d5a0480cd3dbe80438b0d6915eb0d07e2ed5963c56e3f02dc7` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_payout_fee_replacement_keeps_parent_and_counts_only_confirmed_child explicitly has second confirmed UTXO, missing sole-sponsor condition. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Measure actual replaceability with oneU; if top-up needed alert names condition; exact99k parent eventually confirms unchanged. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `FUNDING-RBF-LIQUIDITY`.

<a id="ml159"></a>

## ML159 — Fragmented sponsor coins create a nonstandard oversized fee child

**A. Title:** Fragmented sponsor coins create a nonstandard oversized fee child.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Enough confirmed sats spread over many tinyUTXOs; payout parent near10000vB.

**E. Event chain and suspected mechanism:** BDK selects many inputs for guaranteed change; version3 child exceeds1000vB or cannot be funded economically; Core refuses.

**F. Expected correct behavior:** Require usable funding shape and recover after suitable confirmed funding without deducting customer value.

**G. Potential incorrect outcome:** All recipients remain pending while total balance says funded; false Broadcast may compound ML151.

**H. Evidence and inference:** E37 selection/weight iteration; E27 no child-size check here; pinned Core31 rules in plan. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** Native fee tests use large100k sponsors; no fragmented-confirmed relay/recovery case. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual child<=1000vB, prevout-based miner fee; after adequate confirmed input exact99k pays and customer never covers miner shortfall. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `FUNDING-FRAGMENTATION`.

<a id="ml160"></a>

## ML160 — Split progression fees bypass promised per-allocation subsidy accounting

**A. Title:** Split progression fees bypass promised per-allocation subsidy accounting.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Large tree needs several actual progress packages/replacements before final payout.

**E. Event chain and suspected mechanism:** Final child stored/warned under allocation; split path uses ordinary process_progress_tx without cumulative allocation cost evidence.

**F. Expected correct behavior:** Record canonical split and payout fees/subsidy and required warning, without introducing a cap.

**G. Potential incorrect outcome:** Operator silently exhausts sponsors; users remain unpaid while report understates recovery cost.

**H. Evidence and inference:** E44 split->progress path; E27 final-payout warning; E45 fee-child schema; planP2. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E45: [server/src/database/fork_migrations/V5__expiry_allocations.sql:91](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V5__expiry_allocations.sql:91), `expiry_ledger_commit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `3712331243e47c147d3387b5887f02eeccb0d7a59e2b273ba9d4d55574ecea42`. Code fact: Permanent one-active allocation index; append-only visible-transaction revision/count; triggers across payout ledger.

**I. Current coverage:** Small split checks customer amounts; multilevel actual-limit fixture runtime not retained; receive extraction cannot reconstruct full operator subsidy. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently enumerate all canonical progress/payout children, count each fee once, warning at accepted threshold, unchanged rightful payees. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `FUNDING-SPLIT-ACCOUNTING`.

<a id="ml161"></a>

## ML161 — Oversized backing has no proved safe split and remains unpaid

**A. Title:** Oversized backing has no proved safe split and remains unpaid.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Root exceeds payout size; next signed board/transfer step exposes user-only outputs whose users never return.

**E. Event chain and suspected mechanism:** split_recovery_paths correctly rejects a signed next step exposing abandoned user-only leaves. If no other bounded safe split/progress exists, the oversized root remains pending indefinitely.

**F. Expected correct behavior:** Preserve the current refusal of unsafe progress and demonstrate a terminating safe recovery path for the admitted tree shape; safe indefinite blocking does not satisfy eventual payout.

**G. Potential incorrect outcome:** Customers remain unpaid at the retained root. Current code is not shown advancing into unsignable leaves; permanent loss requires proving no alternate recoverable path or future claimant exists.

**H. Evidence and inference:** E28 split_recovery_paths explicitly rejects abandoned user-only board leaf; planC8 original signed-step termination claim. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** expiry_split_never_exposes_an_abandoned_user_only_board_leaf is unit refusal; no real oversized heterogeneous tree proving termination instead of indefinite refusal. New retained batch16 run 63a761b8 proves a bounded 327-owner, 635-path, two-level native split with all rightful outputs; it does not exercise this scenario's extra fault/precondition. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** For the real oversized heterogeneous tree, observe every progressed child retains authentic recovery, and every rightful 5,000-sat owner eventually receives 4,000 sat. An indefinite safe refusal fails the liveness oracle. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `SPLIT-RECOVERY-PATH`.

<a id="ml162"></a>

## ML162 — Unknown-owner sibling prevents every known owner in the same physical backing from progressing

**A. Title:** Unknown-owner sibling prevents every known owner in the same physical backing from progressing.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** One root contains knownAlice100k and permanently unregisteredBob leaf; Alice ready butBob returns Hold.

**E. Event chain and suspected mechanism:** prepare_expiry_anchor sets hold=true and suppresses request_expiry_split even if signed disjoint progress could exposeAlice branch.

**F. Expected correct behavior:** No operator appropriation; preserveBob liability while allowing safely separable knownAlice eventual payment.

**G. Potential incorrect outcome:** Alice99k waits forever solely becauseBob never registers; cross-tree isolation does not solve same-root coupling.

**H. Evidence and inference:** E04 hold/needs_htlc_progress condition; E28 signed path safety. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

**I. Current coverage:** Ready/unready native test uses distinct roots; requested mixed split injects split and has identifiable owners. Neither proves unknown-owner sibling progress. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** KeepBob permanently unknown; retain all its value and prove signed-safe progression leadsAlice99k withoutBob/client activity. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `SPLIT-READINESS-COUPLING`.

<a id="ml163"></a>

## ML163 — A saved split instruction is replayed after a newer financial commitment

**A. Title:** A saved split instruction is replayed after a newer financial commitment.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Split request forR persisted; before watchman runs, native refresh/offboard/transfer changes terminal rights.

**E. Event chain and suspected mechanism:** Broadcaster trusts saved progress_txid and old ownership, publishes path that invalidates new obligation or loses recovery key.

**F. Expected correct behavior:** Revalidate exact current signed paths, source commitments and reservations under shared locks through publication.

**G. Potential incorrect outcome:** Customer replacement/offboard entitlement disappears or same backing gets inconsistent commitments.

**H. Evidence and inference:** E28 request_expiry_split current path reproof; E44 process_sweeps rechecks before send. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

**I. Current coverage:** Small split case checks initial request; no actual new-binding-operation between saved request and split publication. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Freeze real newer commitment and prove saved instruction either safely revalidated or refused, with complete100k rightful recovery. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-STALE-INSTRUCTION`.

<a id="ml164"></a>

## ML164 — Ephemeral pool recovery key is pruned after split preflight but before signing

**A. Title:** Ephemeral pool recovery key is pruned after split preflight but before signing.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Split proof relies on server-owned ephemeral key for terminal pool/change output.

**E. Event chain and suspected mechanism:** Key lifecycle deletes or rotates it after proof snapshot; split confirms; later payout signing cannot recover that branch.

**F. Expected correct behavior:** Hold required key records or durable sufficient signing material across the entire obligation lifetime.

**G. Potential incorrect outcome:** Operator/customer funds permanently stranded at output deemed recoverable; backup source row alone insufficient.

**H. Evidence and inference:** E28 owned key lookup/locked_keys checks; E04 fetch_ephemeral_tweak; E44 signer path. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

**I. Current coverage:** Split unit uses fixed owned set; no real pool key-retirement concurrent with multilevel split/signing/backup. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Record public key identities only; after actual prune attempt and restart every progressed output still has valid witness and customer amount is paid. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-KEY-LIFETIME`.

<a id="ml165"></a>

## ML165 — Multilevel split broadcasts a grandchild before its parent confirmation

**A. Title:** Multilevel split broadcasts a grandchild before its parent confirmation.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Ready root requires at least two levels under Core31 version3 ancestry limits.

**E. Event chain and suspected mechanism:** Worker publishes next level using mempool parent as frontier; third unconfirmed transaction violates1-parent1-child/TRUC policy; repeated package never relays.

**F. Expected correct behavior:** Advance each level only after actual canonical parent confirmation and re-evaluate frontier.

**G. Potential incorrect outcome:** Large cohort never reaches final payout though smaller trees pass; funds delayed indefinitely under premature retry loop.

**H. Evidence and inference:** E28 signed next step; E44 progress packages; E04 frontier confirmed query; pinned Core31 rules in plan§7. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Batch16 run 63a761b8 passed two actual oversized levels with canonical parent-before-next-level ordering and every owner output. This positive bounded regtest case does not cover eviction, reorg or restart between levels; premature progression remains a guarded hypothesis. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent Core mempool+block observations at every level, parent-before-next-level confirmation, all5000->4000 payouts and global DAG conservation. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-TRUC-PACING`.

<a id="ml166"></a>

## ML166 — Real fanout has one checkpoint per destination but planner accounts for one shared checkpoint

**A. Title:** Real fanout has one checkpoint per destination but planner accounts for one shared checkpoint.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Native four-destination transfer yields four checkpoints and branch exits;327 owners eventually share original anchor.

**E. Event chain and suspected mechanism:** Split/inventory assumes a single checkpoint, misses siblings or selects wrong output; size/ownership proof never reaches correct branch.

**F. Expected correct behavior:** Follow actual signed native graph with all checkpoint outpoints, not a simplified fixture graph.

**G. Potential incorrect outcome:** Some owners lose payouts or whole tree stalls; extra correct recipient output does not establish sibling accounting.

**H. Evidence and inference:** E24 actual transactions; E25 one-input transitions; corrected OVERSIZE-CURRENT-SOURCE-HANDOFF.md details635 signed paths. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

E25: [server/src/database/expiry_geometry.rs:22](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_geometry.rs:22), `economic_cut`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `06e99d7ed905de8fd745868e9c3ddacefaf915099945c4654af22544e4629d0e`. Code fact: Current round/offboard settlement stops traversal; all selected paths checked. Historical descendants are candidates rather than eagerly validated current claims.

**I. Current coverage:** Original batch12 FAIL16.359s was expected-one/actual-four fixture geometry before expiry. Corrected batch16 run 63a761b8 now PASS164.064s: 635 signed paths, 327 owners each 4,000 sat and full canonical DAG conservation. This guards the named shape; heterogeneous historic shapes remain unqualified. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual builder graph exhaustively partitions every nonanchor output;327 distinct owners each4000, no omitted/extra payout anywhere. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-GRAPH-SHAPE`.

<a id="ml167"></a>

## ML167 — Splitting duplicates one entitlement when it is reassociated with child allocations

**A. Title:** Splitting duplicates one entitlement when it is reassociated with child allocations.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** Parent obligation exists; confirmed split exposes two child outputs; same coin lineage appears in both preparation batches after restart.

**E. Event chain and suspected mechanism:** Child allocations create new entitlementIDs/gross rather than preserve disjoint original identity, or both associations remain active.

**F. Expected correct behavior:** Exactly one permanent obligation and current active allocation per coin; move, never copy value.

**G. Potential incorrect outcome:** Customer can receive twice across nonidentical child inputs or another owner loses backing; DB per-ID uniqueness alone misses duplicate economic identities.

**H. Evidence and inference:** E23 stable entitlement lookup; E45 active index; E28 split requests; E29 regroup. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E45: [server/src/database/fork_migrations/V5__expiry_allocations.sql:91](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V5__expiry_allocations.sql:91), `expiry_ledger_commit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `3712331243e47c147d3387b5887f02eeccb0d7a59e2b273ba9d4d55574ecea42`. Code fact: Permanent one-active allocation index; append-only visible-transaction revision/count; triggers across payout ledger.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Small split demonstrates49k/49k distinct sibling sources; no multilevel interrupted parent-to-child identity conservation case. New retained batch16 run 63a761b8 proves a bounded 327-owner, 635-path, two-level native split with all rightful outputs; it does not exercise this scenario's extra fault/precondition. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent original coinIDs/gross mapped to exactly one covering child path each; canonical outputs+deductions equal original total and no duplicate actual outpoints. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-ENTITLEMENT-IDENTITY`.

<a id="ml168"></a>

## ML168 — Holder exits one branch while oversized split is between levels

**A. Title:** Holder exits one branch while oversized split is between levels.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Signed first split confirmed; holder starts actual exit of one child before next split/payout; other recipients remain offline.

**E. Event chain and suspected mechanism:** Scheduler reuses old complete path or treats ancestor spend as all-paid; sibling allocations disappear or parent repeatedly conflicts.

**F. Expected correct behavior:** Follow actual conflicting branch to terminal claim and continue unaffected child obligations with disjoint inputs.

**G. Potential incorrect outcome:** Sibling users permanently unpaid, exiting user paid twice, or intermediate exit falsely discharges others.

**H. Evidence and inference:** E28 path spent checks; E29 terminal claim/conflict; E24 partition; planW8. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

**I. Current coverage:** Two-root partial-conflict test is not a multilevel same-anchor split conflict; requested mixed split has no holder exit. New retained batch16 run 63a761b8 proves a bounded 327-owner, 635-path, two-level native split with all rightful outputs; it does not exercise this scenario's extra fault/precondition. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Canonical full DAG: exited owner actual claim once, every unaffected5000 owner4000, total external inputs=UTXOs+fees. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-EXIT-CONFLICT`.

<a id="ml169"></a>

## ML169 — Legacy signed progress transaction violates current package version policy

**A. Title:** Legacy signed progress transaction violates current package version policy.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** uncertain; conditional on an authentic supported imported non-v3 signed progress path.

**D. Context and financial state:** An authentic supported imported signed progress transaction is version 2 or has incompatible anchor policy. No such artifact was found in inspected historical A/B sources: both inspected builders already create version 3.

**E. Event chain and suspected mechanism:** Progress helper assumes compatible zero-fee parent; Core31 rejects mixed unconfirmed transaction versions; stored signature cannot change version.

**F. Expected correct behavior:** Provide compatible authentic recovery or reject incompatible adoption before new obligations; do not silently strand migrated paths.

**G. Potential incorrect outcome:** If the prerequisite exists and no compatible signed recovery remains, migrated principal could be blocked by package policy. This is not a demonstrated historical incompatibility.

**H. Evidence and inference:** E37 current version-3/zero-fee sponsorship assumptions; E44 progress submission. Missing prerequisite: actual admitted non-v3 signed progress artifact. E39-E42 do not establish historical version-2 progress; test funding transactions are not that evidence. Retained positive artifact: /home/francis/bark-integration-2026-10-01/fresh-testing-track-20261017/batch16-corrected-multilevel-split-receipt.json (dirty 814d7b0de capture, source manifest 704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06). The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

E39: [server/src/database/expiry_ownership.rs:394](/home/francis/sweep-to-payout-src/server/src/database/expiry_ownership.rs:394), `send_ownership`, repository `/home/francis/sweep-to-payout-src`, HEAD `994aede0ca7b2631e65ec3857f48b71888e0316f`; exact read bytes SHA256 `b45b467f5a4484ce997f796cbd7683415e74f0281d903c314a2c45822d5e9c77`. Code fact: Historical B treats any same-hash preimage/succeeded attempt as paid send; receive ownership also uses hash-level evidence.

E42: [server/src/expiry_payout.rs:402](/home/francis/abandon-ship-bark-two-step-comparison/server/src/expiry_payout.rs:402), `expiry_payout_tick`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `5a2a5c62e1691f77df2bdc0565b05b94e434e5ff7aabf70d700138f9a2d0a566`. Code fact: Historical two-step route requires real fee estimate and swept-path proof; default sweep minimum100 confirmations; different fee/minimum policy.

**I. Current coverage:** Fresh327 graph and current board fixtures use current native builders; no authentic old-policy signed path migrated through actual Core admission. New retained batch16 run 63a761b8 proves a bounded 327-owner, 635-path, two-level native split with all rightful outputs; it does not exercise this scenario's extra fault/precondition. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Preserve exact old signed bytes; independent relay/confirmation proves viable recovery and rightful net, no synthetic replacement signature. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `SPLIT-PROTOCOL-VERSION`.

<a id="ml170"></a>

## ML170 — Fee estimator stays stale while correctly constructed payouts never confirm

**A. Title:** Fee estimator stays stale while correctly constructed payouts never confirm.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Package target uses stale low regular fee; actual mempool minimum rises for sustained period; operator has funds.

**E. Event chain and suspected mechanism:** Saved child satisfies internal target, so no bump; identical package repeatedly rejected/evicted while monitor reports adequate recorded rate.

**F. Expected correct behavior:** Detect actual admission/confirmation failure and restore credible fee policy with required warnings, without customer deduction change.

**G. Potential incorrect outcome:** All ready users wait indefinitely even with usable sponsor liquidity; positive funding is not sufficient for liveness.

**H. Evidence and inference:** E27 fee_estimator.regular and recorded fee comparison; E44 split fee behavior. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

**I. Current coverage:** Fee replacement test deliberately changes estimator; no stale-feed versus actual Core pressure recovery evidence. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent actual mempool acceptance/fee rates and time-to-confirm; exact parent99k unchanged and canonical miner expense explicit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `FUNDING-ESTIMATOR-LIVENESS`.

<a id="ml171"></a>

## ML171 — Old watchman drains backing after fresh server committed customer allocations

**A. Title:** Old watchman drains backing after fresh server committed customer allocations.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Fresh captaind shares DB with older watchmand binary that lacks allocation_input/decided guards.

**E. Event chain and suspected mechanism:** Server commits entitlement/Spent; old watcher invokes legacy drain-only sweep on same expired root and confirms operator drain.

**F. Expected correct behavior:** Admit compatible paired financial writers and migration capabilities before they share live money state.

**G. Potential incorrect outcome:** Operator receives customer principal while new DB reports committed payout; original customer cannot recover consumed backing.

**H. Evidence and inference:** E44 fresh process_sweeps fork path; E22 durable guards; E39/E41/E42 historical routes; retained manifests show multiple binary families. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E39: [server/src/database/expiry_ownership.rs:394](/home/francis/sweep-to-payout-src/server/src/database/expiry_ownership.rs:394), `send_ownership`, repository `/home/francis/sweep-to-payout-src`, HEAD `994aede0ca7b2631e65ec3857f48b71888e0316f`; exact read bytes SHA256 `b45b467f5a4484ce997f796cbd7683415e74f0281d903c314a2c45822d5e9c77`. Code fact: Historical B treats any same-hash preimage/succeeded attempt as paid send; receive ownership also uses hash-level evidence.

E41: [server/src/database/expiry_settlement.rs:190](/home/francis/abandon-ship-bark-two-step-comparison/server/src/database/expiry_settlement.rs:190), `store_expiry_payment`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `377bee85dc542ec86957a00b7ca2e7fea58e6182f564e3de1a14f1bd97db841e`. Code fact: Historical A persists signed payout in nursery and settlement rows; refund path cancels subscriptions by hash.

E42: [server/src/expiry_payout.rs:402](/home/francis/abandon-ship-bark-two-step-comparison/server/src/expiry_payout.rs:402), `expiry_payout_tick`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `5a2a5c62e1691f77df2bdc0565b05b94e434e5ff7aabf70d700138f9a2d0a566`. Code fact: Historical two-step route requires real fee estimate and swept-path proof; default sweep minimum100 confirmations; different fee/minimum policy.

**I. Current coverage:** Current native tests pin paired products; none qualifies arbitrary old watchman against fresh schema. No actual wrong mixed deployment observed. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Inspect actual loadedELFs then canonical root spend: customer99k only or explicit compatible migration; no operator-only winning sweep for owed customer. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `DEPLOY-MIXED-FINANCIAL-WRITERS`.

<a id="ml172"></a>

## ML172 — Reorg unconfirms payout input itself while permanent fence survives

**A. Title:** Reorg unconfirms payout input itself while permanent fence survives.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Confirmed payout99k later loses ancestor funding in deeper fork; obligation and original source Spend fence remain.

**E. Event chain and suspected mechanism:** Rebuild clears confirmation but saved parent references unavailable input; readiness may never find replacement frontier or rebroadcast funding.

**F. Expected correct behavior:** Preserve obligation/fence and rebuild actual backing dependency until rightful payout recoverable; do not reopen original for duplicate use.

**G. Potential incorrect outcome:** Customer unpaid permanently if funding cannot be reintroduced; false paid status if input loss ignored.

**H. Evidence and inference:** E29 rebuild from actual tx_status; E30 invalidation; E27 saved parent input dependency. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** 6/101 payout reorg tests invalidate payout block and retain funding; they do not remove backing issuance itself. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual fork removes anchor, then authentic signed funding recovery; stable entitlement gross100k, one canonical99k receipt after restoration. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `REORG-FUNDING-ANCESTOR`.

<a id="ml173"></a>

## ML173 — Split confirmation is reorged out after children have been allocated

**A. Title:** Split confirmation is reorged out after children have been allocated.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Parent split confirmed; child payouts committed; reorg removes split and descendants.

**E. Event chain and suspected mechanism:** Child active links remain pointed at vanished outputs; original parent split request not restored, or both old/new child allocations considered payable.

**F. Expected correct behavior:** Rebuild child frontier/active links from canonical chain while preserving stable gross and spending fences.

**G. Potential incorrect outcome:** Users permanently stuck on nonexistent outpoints or duplicated allocations after repeated split; prior confirmed status false.

**H. Evidence and inference:** E28 split request persistence; E29 rebuild; E30 generation; planC9. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

**I. Current coverage:** Ordinary payout reorg and small requested split are separate passes, not actual split-rollback after child allocation. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After fork every obligation has exactly one current covering path, parent re-splits, each5000 owner4000 canonical once. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `REORG-SPLIT`.

<a id="ml174"></a>

## ML174 — Exit conflict disappears after regrouped replacement payout was prepared

**A. Title:** Exit conflict disappears after regrouped replacement payout was prepared.

**B. Financial invariant:** One economic principal is discharged at most once across all Lightning, Ark, exit, offboard and fallback paths.

**C. Applicable implementation:** current.

**D. Context and financial state:** MergedA/B parent conflicted byA exit;B regrouped; then fork removesA exit and restores original merged parent viability.

**E. Event chain and suspected mechanism:** Rebuild keepsB replacement active plus old group or erasesA obligation as permanently exited.

**F. Expected correct behavior:** Choose canonical winner globally; preserve both liabilities until actual terminal receipt, with exclusive lineage inputs.

**G. Potential incorrect outcome:** B duplicate economic payment orA lost entitlement; inactive old links can conceal canonical winner.

**H. Evidence and inference:** E29 global winner/active reset SQL; E23 existing-entitlement reservation; planC9. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

**I. Current coverage:** Partial-conflict test never reorgs conflict away; payout reorg has one allocation/no regroup. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently check old/new parent ancestry and actual canonical inputs;A/B each one justified receipt and total no duplicate principal. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `REORG-CONFLICT-REGROUP`.

<a id="ml175"></a>

## ML175 — Frontier rollback is delayed while fork listener has already observed reorg

**A. Title:** Frontier rollback is delayed while fork listener has already observed reorg.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Reorg listeners run concurrently; fork invalidation executes promptly, upstream frontier rollback delayed.

**E. Event chain and suspected mechanism:** Evaluator/broadcaster infers validity from listener order or old height and uses stale onchain spends/HTLC resolution before rollback complete.

**F. Expected correct behavior:** Durable invalid marker pauses financial work until all relevant listener callbacks and rebuild complete.

**G. Potential incorrect outcome:** Wrong owner payout or missing entitlement from stale spent state; permanent if wrong branch payment confirms afterward.

**H. Evidence and inference:** E46 concurrent listener calls; E30 completeACKs; E29 rebuild after token. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E46: [server/src/sync/block_index.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/sync/block_index.rs:180), `on_sync_complete`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `0c8670bf923c002f8884dee6983b847c839341956378efe82d1c909f89cc042f`. Code fact: Concurrent listener work precedes completed tip and completion ACK callbacks; listener order alone is not a rollback barrier.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Generation component and ordinary stopped-watchman reorg prove staleACK refusal; not actual delayed frontier callback while worker attempts publication. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** No decision/broadcast in invalid interval; after real rollback actual100k owner resolves and99k once, generation/hash evidence retained. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `REORG-LISTENER-BARRIER`.

<a id="ml176"></a>

## ML176 — Failed sync callback leaves an old completion acknowledgment usable

**A. Title:** Failed sync callback leaves an old completion acknowledgment usable.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Watchman restart or fork triggers sync; one listener fails after some DB updates; oldACK row remains.

**E. Event chain and suspected mechanism:** Evaluator accepts height/hash or staleACK without successful generation-specific completion; incomplete frontier regarded rebuilt.

**F. Expected correct behavior:** Failed cycle cannot authorize decisions; successful retried full sync resumes without erasing liabilities.

**G. Potential incorrect outcome:** Customer money misrouted from partial rollback or indefinitely fenced if invalidation cannot recover.

**H. Evidence and inference:** E30 start deletes ownACK/invalidate; E46 completion callbacks after successful listener work. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E46: [server/src/sync/block_index.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/sync/block_index.rs:180), `on_sync_complete`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `0c8670bf923c002f8884dee6983b847c839341956378efe82d1c909f89cc042f`. Code fact: Concurrent listener work precedes completed tip and completion ACK callbacks; listener order alone is not a rollback barrier.

Test reference: [expiry_generation_rejects_stale_rebuild_and_survives_restart](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_state.rs:14), file SHA256 `7b501e9e028d4f50e6f5636903548b810abfe136854c3eb1347ec583c5384a69` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_generation_rejects_stale_rebuild_and_survives_restart is component state; no retained real callback failure midway through actual chain sync. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent fail/retry timeline: no financial publication before all callbacks succeed, then right99k; oldACK never clears new invalidation. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `REORG-SYNC-FAILURE`.

<a id="ml177"></a>

## ML177 — Older rebuild clears a newer invalidation after a second reorg

**A. Title:** Older rebuild clears a newer invalidation after a second reorg.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** RebuildA queries canonical state for generationg; second reorg invalidatesg+1 beforeA commits.

**E. Event chain and suspected mechanism:** A unconditionally marks view valid and publishes old owner/confirmation state for new branch.

**F. Expected correct behavior:** CAS exact generation and completed tips; old work cannot authorize any new monetary action.

**G. Potential incorrect outcome:** Wrong payout recipient or false paid status; stable obligation may be tied to nonexistent child.

**H. Evidence and inference:** E30 finish_expiry_rebuild token comparison; E29 rechecks token under row lock. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

**I. Current coverage:** Generation test rejects stale token; no real two-reorg overlap with long chain/RPC scan and actual payout attempts. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** At every publication record exact generation/tips;A result cannot clearg+1; eventual rightful99k on canonical fork only. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Hypothesis requiring validation**. Root-cause family: `REORG-GENERATION-CAS`.

<a id="ml178"></a>

## ML178 — Repeated reorg notification prevents completed sync generations from converging

**A. Title:** Repeated reorg notification prevents completed sync generations from converging.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Same fork event delivered repeatedly while both daemons acknowledge at different times.

**E. Event chain and suspected mechanism:** Each duplicate increments global generation; neither pair ofACKs ever matches current generation; rebuild never yields valid token.

**F. Expected correct behavior:** Duplicate delivery must be safe and converge; no obligations or fences duplicated, and liveness eventually resumes.

**G. Potential incorrect outcome:** Every customer payout stays pending forever despite stable chain; logs show sync activity but no valid financial view.

**H. Evidence and inference:** E30 invalidate increments generation each call and requires twoACKs; E46 event callbacks. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E46: [server/src/sync/block_index.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/sync/block_index.rs:180), `on_sync_complete`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `0c8670bf923c002f8884dee6983b847c839341956378efe82d1c909f89cc042f`. Code fact: Concurrent listener work precedes completed tip and completion ACK callbacks; listener order alone is not a rollback barrier.

**I. Current coverage:** PlanW9 requires duplicate event; state test checks generation safety, not repeated real event convergence across independently scheduled daemons. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Bound repeated notifications, then require valid common generation and99k payout without registry/obligation loss. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `REORG-DUPLICATE-LIVENESS`.

<a id="ml179"></a>

## ML179 — Chain reconstruction misses a concurrent fee-child commit but publishes matching ledger watermark

**A. Title:** Chain reconstruction misses a concurrent fee-child commit but publishes matching ledger watermark.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Rebuild scans allocation/child txids; a new fee child commits/confirms after scan but before rebuild transaction; allocationID set unchanged.

**E. Event chain and suspected mechanism:** Rebuild compares only allocationIDs, then records latest ledger count/revision including unseen child; view looks fully rebuilt though canonical fee/payment projection stale.

**F. Expected correct behavior:** Financial snapshot and watermark must cover same visible commits or trigger another rebuild.

**G. Potential incorrect outcome:** Incorrect confirmed-cost/status or prolonged unpaid alarm; can conceal actual paid child or spend reservations until next event.

**H. Evidence and inference:** E29 txid scan and current==ids check; E30 finish captures current ledger count/revision; E45 triggers. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

E45: [server/src/database/fork_migrations/V5__expiry_allocations.sql:91](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V5__expiry_allocations.sql:91), `expiry_ledger_commit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `3712331243e47c147d3387b5887f02eeccb0d7a59e2b273ba9d4d55574ecea42`. Code fact: Permanent one-active allocation index; append-only visible-transaction revision/count; triggers across payout ledger.

Test reference: [expiry_ledger_detects_late_lower_revision_and_ignores_noop_updates](/home/francis/abandon-ship-fresh-integration/testing/tests/server/expiry_state.rs:67), file SHA256 `7b501e9e028d4f50e6f5636903548b810abfe136854c3eb1347ec583c5384a69` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** expiry_ledger_detects_late_lower_revision_and_ignores_noop_updates proves watermark component semantics, not concurrent child insertion during real rebuild. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent canonical child receipt must be reflected before declaring its revision rebuilt; no expense/payment inferred from unseen rows. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P2 — Hypothesis requiring validation**. Root-cause family: `REORG-REBUILD-SNAPSHOT`.

<a id="ml180"></a>

## ML180 — Terminal claim witness proves a signer but not payment to the entitled beneficiary

**A. Title:** Terminal claim witness proves a signer but not payment to the entitled beneficiary.

**B. Financial invariant:** Only the rightful beneficiary receives the entitlement; operator custody, a shared hash or a predecessor key cannot establish ownership.

**C. Applicable implementation:** current.

**D. Context and financial state:** Instantiate a paid forwarded-receive entitlement on the original provider-key HTLC grant, where immutable final-recipient binding identifies Alice. Provider later confirms its own source-key claim; this is not Alice choosing to spend her own entitlement.

**E. Event chain and suspected mechanism:** recovery joins entitlement.vtxo_id to the provider source; terminal_user_claim recognizes that source's user-clause witness and writes Exited against the entitlement without a beneficiary/output check.

**F. Expected correct behavior:** A terminal spend discharges debt only when it establishes payment to entitled party or an explicitly authorized transfer.

**G. Potential incorrect outcome:** Provider may obtain claim while recipient9k entitlement is erased; correct source spend alone does not prove customer receipt.

**H. Evidence and inference:** E29 recovery.rs joins source at line 49, recognizes source user clause lines 20-32 and writes claim at 96-98; E05 bound-recipient attribution can retain proposal.id as original provider grant. Requires authentic source/beneficiary mismatch, not an arbitrary authorized user spend. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

**I. Current coverage:** terminal_user_clause_requires_the_exact_source_outpoint is synthetic recognition; partial-conflict holder is same rightful party. No bound-recipient/provider-source discharge oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independently know entitled script/key; actual claimed outputs must discharge that exact obligation, otherwise retain unpaid recipient gross and no false Exited. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `LN-RECOVERY-BENEFICIARY`.

<a id="ml181"></a>

## ML181 — Fresh empty history lets an unpaid server-Spent coin leave the protection watch

**A. Title:** Fresh empty history lets an unpaid server-Spent coin leave the protection watch.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Fresh import/recovery has valid signed100k coin; server reportsSpent but returns coherent empty authenticated fallback history; no actual final claim/payment.

**E. Event chain and suspected mechanism:** adopt_reported_spent sees no entitlement and continues; marksSpent, clears quarantine/awaiting, records no adopted-payout marker; act-by skips coin thereafter.

**F. Expected correct behavior:** K3a requires independent confirmed fate before ending protection; absence from server history is not proof of ordinary spend.

**G. Potential incorrect outcome:** Customer can lose unilateral exit window, then operator sweep consumes backing; lost entitlement may be permanent because no receipt or rearm marker exists.

**H. Evidence and inference:** E32 lines83-112; E34 Spendable/Locked filter; E33 adopted marker loop; PAYOUT_REVIEWPR01. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E32: [bark/src/fallback/adoption.rs:38](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/adoption.rs:38), `adopt_reported_spent`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `aac920f00ae039828d6495006567c48f77265fa653f8ad1c15c7865498306a04`. Code fact: After post-status complete history, absent entitlement allows ordinary Spent adoption without an explicit chain payment check in this method.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

Test reference: [fallback_adoption_history_failure_quarantines_then_reconciles_an_ordinary_spend](/home/francis/abandon-ship-fresh-integration/testing/tests/bark_sdk/fallback_adoption.rs:149), file SHA256 `5aafb400a4ed7c01c3dbb925b3ed1f52ab8fd276a07ecec803dafd9a175507dc` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** fallback_adoption_history_failure_quarantines_then_reconciles_an_ordinary_spend actually sends coin first and assertsSpent; no falseSpent+omitted-history unpaid case. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Keep source100k watched with actual signed exit material until correct99k payout or independently confirmed authorized fate; inventedSpent/empty history must not retire it. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `CLIENT-NEGATIVE-PROOF`.

<a id="ml182"></a>

## ML182 — Seed-only receipt accepts an unrelated confirmed deposit as its fallback payout

**A. Title:** Seed-only receipt accepts an unrelated confirmed deposit as its fallback payout.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Fresh restored wallet has account history but no original full VTXO body; malicious/misconfigured server maps100k entitlement to earlier99k payment to same script.

**E. Event chain and suspected mechanism:** verify_fallback_chain_evidence checks output/script/value but skips ancestry when full coin absent; history labels unrelated deposit as current discharge.

**F. Expected correct behavior:** Even seed-only proof must bind actual paying transaction to actual original signed obligation/lineage, not just amount/address.

**G. Potential incorrect outcome:** User thinks unpaid entitlement settled and stops pursuing it; original100k may expire before authentic source discovered.

**H. Evidence and inference:** E33 optional get_full_vtxo ancestry branch; E35 account-owned history; K3a server-hacked rule. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

E35: [server/src/database/expiry_details.rs:99](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_details.rs:99), `fallback_payout_details_by_account`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `292206c5c2745aff5e4fc7abacada2b7cb435c305674f2886958c91de86e9c5c`. Code fact: Account-owned history uses immutable registration, stable IDs and global view token; 2MiB page budget and 20,001-row guard.

**I. Current coverage:** Native seed recovery uses authentic full histories/actual payout; no wrong-source same-script deposit proof for unknown coin. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent original funding/path oracle distinguishes deposit from fallback; no paid status without that binding and original liability remains recoverable. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `CLIENT-UNKNOWN-SOURCE-PROOF`.

<a id="ml183"></a>

## ML183 — Reorged payout cannot rearm a coin held by another unfinished action

**A. Title:** Reorged payout cannot rearm a coin held by another unfinished action.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Client adopted confirmed payout; then another retained native action holds source/replacement; payout block reorged out while server unreachable.

**E. Event chain and suspected mechanism:** apply_fallback_chain_states skipsLocked/Exited; action cannot finish or release holder; act-by sees blocked and no recoverable claim is started.

**F. Expected correct behavior:** Preserve legitimate action locks while ensuring real remaining principal can be reprotected before expiry.

**G. Potential incorrect outcome:** No duplicate payment occurs, but rightful customer loses exit opportunity behind permanent holder.

**H. Evidence and inference:** E33 apply skips locked/exited; E34 operation holder checks; E32 quarantine. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E32: [bark/src/fallback/adoption.rs:38](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/adoption.rs:38), `adopt_reported_spent`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `aac920f00ae039828d6495006567c48f77265fa653f8ad1c15c7865498306a04`. Code fact: After post-status complete history, absent entitlement allows ordinary Spent adoption without an explicit chain payment check in this method.

**I. Current coverage:** Prepared-offboard finality and self-refresh recovery test one action/known holder; no combination of actual payout reorg plus separately stalled action owning the affected claim. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** After fork actual payout absent, exact signed100k material still watched; legitimate holder resolved safely and eventual99k or actual authorized exit, no lock stealing. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `CLIENT-REORG-LOCKS`.

<a id="ml184"></a>

## ML184 — Two registered accounts sharing a payout script each count the same finite output in full

**A. Title:** Two registered accounts sharing a payout script each count the same finite output in full.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Untrusted/faulty server receipts cover accounts A/B sharing one destination. Each has gross 100,000 sat and legitimate 500-sat share of the one output fee. A malicious parent pays only 99,500 sat to the common script and diverts the rest.

**E. Event chain and suspected mechanism:** Each separate authenticated history claims 99,500 sat of the same actual 99,500-sat output. Per-account local subtotal checks pass, although the combined 199,000-sat entitlement is not covered.

**F. Expected correct behavior:** Shared output finite value must cover aggregate entitlements globally; script equality does not prove both customers paid.

**G. Potential incorrect outcome:** Combined customer value is short 99,500 sat; operator diversion is 100,500 sat instead of the authorized 1,000-sat fee. Honest allocator behavior is not claimed to construct this parent.

**H. Evidence and inference:** E33 comment permits other registered accounts sharing script and checks local subtotal only; E26 server group allocator should produce199k. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

E26: [server/src/watchman/settlement.rs:65](/home/francis/abandon-ship-fresh-integration/server/src/watchman/settlement.rs:65), `allocate`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `d09a4c2a303ed9d3f853fdd351b9e6e81ca89929642e573df6fddeb57d2f3d36`. Code fact: Group by exact script, one 1000-sat fee per paying output; script dust; stable per-leaf fee/payout/forfeit partition.

**I. Current coverage:** regroup_ids_cannot_reuse_one_output_as_two_payments proves two claims within one snapshot; not separate authenticated accounts with same script. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** For 200,000-sat gross, require one actual 199,000-sat output and one authorized 1,000-sat fee. A 99,500-sat output cannot discharge two 99,500-sat account claims. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `CLIENT-CROSS-ACCOUNT-CONSERVATION`.

<a id="ml185"></a>

## ML185 — Fallback history debits a coin already debited by a protective exit movement

**A. Title:** Fallback history debits a coin already debited by a protective exit movement.

**B. Financial invariant:** Every satoshi has exactly one owner/disposition; customer gross equals actual net plus authorized deductions, with miner costs accounted separately.

**C. Applicable implementation:** current.

**D. Context and financial state:** Client starts protective exit of100k, then actual fallback wins while exit movement remainsPending/Failed.

**E. Event chain and suspected mechanism:** Both exit subsystem and fallback projection record negative100k; user balance/history now double-debits one principal although chain paid once.

**F. Expected correct behavior:** One principal debit across all subsystems; losing protection intent stays visible with no duplicate monetary deduction.

**G. Potential incorrect outcome:** Customer may believe funds lost or authorize extra spend/refund based on wrong balance; accounting hides actual recoverable amount.

**H. Evidence and inference:** E33 apply states; bark/src/fallback/payouts.rs::project reserved_by_exit; E36 local_debits. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

E36: [bark/src/fallback/sync.rs:148](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/sync.rs:148), `sync_fallback_payouts`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `dc24a84878eaf20b4158836c2503881797a792fae699a442f47f3731ee6bfe26`. Code fact: Complete consistent paginated history replaces visible snapshot; previous observations retained; concurrent snapshot base checked.

**I. Current coverage:** Strengthened batch08 current-cut history checks total39670 once across all consuming movements; no all-action-type/reorg mixed state matrix. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent canonical99k receipt and sum of every movement consuming original coin: debit100k once, fees explicit, stable movementIDs through reopen. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `CLIENT-HISTORY-CONSERVATION`.

<a id="ml186"></a>

## ML186 — One entitlement accumulates too much history to fit any recovery page

**A. Title:** One entitlement accumulates too much history to fit any recovery page.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Repeated conflicts/regrouping preserve many attempts/large raw parents for one permanent entitlement.

**E. Event chain and suspected mechanism:** Its estimated history exceeds2MiB; fallback_payout_details_by_account returnsTooLarge even with page size1; client cannot obtain a complete snapshot.

**F. Expected correct behavior:** Bound transport while preserving a reachable complete account recovery/history path.

**G. Potential incorrect outcome:** Authenticated entitlement/history/adoption cannot complete; protection can stall if the application requires that snapshot. An already confirmed BIP84 payout may still be independently discoverable/spendable by direct chain scan; RPC failure alone does not strand it.

**H. Evidence and inference:** E35 HISTORY_BYTE_BUDGET and page-empty error; E36 full snapshot requirement. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E35: [server/src/database/expiry_details.rs:99](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_details.rs:99), `fallback_payout_details_by_account`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `292206c5c2745aff5e4fc7abacada2b7cb435c305674f2886958c91de86e9c5c`. Code fact: Account-owned history uses immutable registration, stable IDs and global view token; 2MiB page budget and 20,001-row guard.

E36: [bark/src/fallback/sync.rs:148](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/sync.rs:148), `sync_fallback_payouts`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `dc24a84878eaf20b4158836c2503881797a792fae699a442f47f3731ee6bfe26`. Code fact: Complete consistent paginated history replaces visible snapshot; previous observations retained; concurrent snapshot base checked.

**I. Current coverage:** Pagination component tests cover changed token and cursor rules; no authentic single-entitlement many-attempt history above2MiB. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Above the authentic single-entitlement history limit, independently test both complete authenticated history/protection progress and chain-only BIP84 discovery/spend of exact paid outpoints; report these separate outcomes. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `CLIENT-HISTORY-BOUND`.

<a id="ml187"></a>

## ML187 — Global ledger churn invalidates every page of one absent user recovery

**A. Title:** Global ledger churn invalidates every page of one absent user recovery.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Restored large account needs many pages; unrelated active users continually commit payouts/fee-child updates.

**E. Event chain and suspected mechanism:** Global view token changes during every collection; retries never assemble complete snapshot, although requested account itself is stable.

**F. Expected correct behavior:** Maintain coherent history without starving cold recovery under admitted concurrent financial load.

**G. Potential incorrect outcome:** Authenticated adoption/history may starve under unrelated churn, delaying protection and reconciliation. Direct BIP84 recovery may still spend a confirmed output; history churn alone does not prove it cannot.

**H. Evidence and inference:** E35 global ledger_count/revision token; E36 AccountPages and collect retries. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E35: [server/src/database/expiry_details.rs:99](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_details.rs:99), `fallback_payout_details_by_account`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `292206c5c2745aff5e4fc7abacada2b7cb435c305674f2886958c91de86e9c5c`. Code fact: Account-owned history uses immutable registration, stable IDs and global view token; 2MiB page budget and 20,001-row guard.

E36: [bark/src/fallback/sync.rs:148](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/sync.rs:148), `sync_fallback_payouts`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `dc24a84878eaf20b4158836c2503881797a792fae699a442f47f3731ee6bfe26`. Code fact: Complete consistent paginated history replaces visible snapshot; previous observations retained; concurrent snapshot base checked.

**I. Current coverage:** Changed-view unit proves refusal safety; no sustained unrelated-write churn plus eventual full seed-restoration completion. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Measure complete stable-account snapshot latency under unrelated writes, and separately prove chain-only discovery/real spend of confirmed customer outpoints. No partial-page false paid adoption. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `CLIENT-HISTORY-LIVENESS`.

<a id="ml188"></a>

## ML188 — Restoration birthday starts after the actual fallback payout

**A. Title:** Restoration birthday starts after the actual fallback payout.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** User lost original DB; backup/restore chooses current date/height as birthday; payout99k confirmed earlier at original external index0.

**E. Event chain and suspected mechanism:** Fresh BDK scan omits earlier history; server history unavailable or incompatible; wallet reportszero despite actual ownedUTXO.

**F. Expected correct behavior:** Seed-only recovery must use supported original/genesis birthday or another authentic complete discovery mechanism.

**G. Potential incorrect outcome:** Money appears permanently lost until correct scan range recovered; not established mainnet loss.

**H. Evidence and inference:** E31 app-supplied wallet; simulation planS2 original birthday; native seed fixtures use genesis/original scan context. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

**I. Current coverage:** Batch14 fresh recovery explicitly uses genesis birthday; external BIP84 test preserves correct restore configuration, not wrong new default. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** With only documented backup, discover exact99k outpoint without hints then confirm spend; report missing birthday requirement honestly. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `CLIENT-DISCOVERY-BIRTHDAY`.

<a id="ml189"></a>

## ML189 — Manual-sync application never calls the SDK act-by protection loop

**A. Title:** Manual-sync application never calls the SDK act-by protection loop.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** App opens SDK run_daemon=false/manual sync; registered100k remains locally known; server becomes unavailable before expiry.

**E. Event chain and suspected mechanism:** App calls ordinary snapshot/sync but omits protect_expiring_vtxos; no early refresh/exit scheduled; user assumes SDK auto-protects.

**F. Expected correct behavior:** Integration must execute documented protection lifecycle or truthfully withhold its guarantee.

**G. Potential incorrect outcome:** Customer misses unilateral recovery window; depends entirely on potentially failing server fallback.

**H. Evidence and inference:** E34 protect_expiring_vtxos docs/manual-sync condition; E31 wallet open; client planK3a. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E31: [bark/src/fallback.rs:87](/home/francis/abandon-ship-fresh-integration/bark/src/fallback.rs:87), `open_fallback`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `57cabf9b3eea09024e59c901067f1f5bb6bc2cc93f4fe376fa295ba9c81d546d`. Code fact: App supplies destination; cached binding can open offline. Automatic re-registration with cached record requires explicit requested address.

**I. Current coverage:** Default-daemon test proves normal automatic path; manual native fixtures stop daemons intentionally and do not qualify arbitrary embedding app loops. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Actual supported app scheduler under server outage starts valid signed protection beforeact_by and ultimately receives principal; merely importing SDK earns no credit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `CLIENT-INTEGRATION-LIVENESS`.

<a id="ml190"></a>

## ML190 — One missing signed coin body aborts protection scheduling for every healthy coin

**A. Title:** One missing signed coin body aborts protection scheduling for every healthy coin.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Wallet contains valid dueAlice coinA and another legacy/quarantined coinB whose full genesis retrieval fails.

**E. Event chain and suspected mechanism:** exit_protection_schedule uses ? inside per-coin loop; error prevents returning schedules forA; repeated daemon tick fails atB again.

**F. Expected correct behavior:** FaultyB remains protected/unresolved while independently validA refreshes/exits before deadline.

**G. Potential incorrect outcome:** A principal can expire without any protective action even though its own signed material/funding is available.

**H. Evidence and inference:** E34 per-coin get_full_vtxo/find_signable_clause awaits; E32 validated quarantine. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E32: [bark/src/fallback/adoption.rs:38](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/adoption.rs:38), `adopt_reported_spent`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `aac920f00ae039828d6495006567c48f77265fa653f8ad1c15c7865498306a04`. Code fact: After post-status complete history, absent entitlement allows ordinary Spent adoption without an explicit chain payment check in this method.

**I. Current coverage:** Ordinary healthy/default-daemon tests use complete bodies; no one-bad-body plus valid overdue coin protection oracle. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** PreserveB evidence and repeatedly fail its hydration; A100k still initiates actual exit/refresh in time and receives rightful net. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `CLIENT-PROTECTION-ISOLATION`.

<a id="ml191"></a>

## ML191 — Large cached history chain checks monopolize the protection deadline budget

**A. Title:** Large cached history chain checks monopolize the protection deadline budget.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Wallet has many historical entitlements/payout observations and a near-act-by live coin; chain backend slow.

**E. Event chain and suspected mechanism:** protect_expiring_vtxos verifies cached payout evidence under history lock before building schedules; many sequential lookups have no shared bounded deadline here.

**F. Expected correct behavior:** Keep independent live-coin protection timely despite large/slow historical observation.

**G. Potential incorrect outcome:** Live principal loses its exit window while correctly rechecking old receipts; no server RPC stall is needed.

**H. Evidence and inference:** E34 protection first calls E33 verify_fallback_chain_evidence; E36 cached observation history. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E33: [bark/src/fallback/protection.rs:50](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/protection.rs:50), `verify_fallback_chain_evidence`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4cb7198e8bb98871b378a9a7ae9e32a9ed0849bb774e9b8a0818982ef2212253`. Code fact: Six confirmations, registered script and group value; local full body enables ancestry check; unknown body skips that ancestry check.

E36: [bark/src/fallback/sync.rs:148](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/sync.rs:148), `sync_fallback_payouts`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `dc24a84878eaf20b4158836c2503881797a792fae699a442f47f3731ee6bfe26`. Code fact: Complete consistent paginated history replaces visible snapshot; previous observations retained; concurrent snapshot base checked.

**I. Current coverage:** Default daemon case has small history; remote-pagination lock separation does not establish slow chain-evidence isolation at scale. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Measure actual act-by with many retained receipts and slow owned chain backend; live100k initiates signable protection before deadline without dropping history. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `CLIENT-PROTECTION-QUEUE`.

<a id="ml192"></a>

## ML192 — Positional checkpoint migration silently drops binding refresh recovery state

**A. Title:** Positional checkpoint migration silently drops binding refresh recovery state.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** legacy checkpoint compatibility / current appended-field guard.

**D. Context and financial state:** Old MessagePack round/action checkpoint restored into new client after forfeits and lost unlock reply.

**E. Event chain and suspected mechanism:** Current optional recovery state is appended and known legacy positional encoding passes. Validate an authentic older interrupted-checkpoint corpus for any unsupported enum/shape that could lose binding exchange recovery; no earlier decode bug is established.

**F. Expected correct behavior:** Decode legacy structures compatibly and retain original/replacement signed material and correct holder until actual claim.

**G. Potential incorrect outcome:** Valid replacement entitlement disappears; no original client/key-state return can repair lost signed replacement bytes.

**H. Evidence and inference:** Client-binding-chain-recovery-handoff describes appended optional state and real legacy compatibility unit; E34 binding path. E52. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

E52: [bark/src/round/protection.rs:91](/home/francis/abandon-ship-fresh-integration/bark/src/round/protection.rs:91), `binding_preimage`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `cf2ce06468b00e1db087b0c0e6123f17a6a6bc7523e4e847945a5b735fcdbbc4`. Code fact: Binding recovery reads authentic revealing chain spends, retains learned preimage and existing original/replacement holder.

**I. Current coverage:** Retained legacy positional unit passed run d9ef5019. Earlier fixture failures concerned height subtraction/Claimed oracle, not demonstrated MessagePack migration loss. Missing: corpus of authentic interrupted Track A/B/C checkpoints. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Restore original historical serialized checkpoint, prove exact source/replacement IDs/holder, then real confirmed rightful recovery without another debit. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `CLIENT-CHECKPOINT-COMPAT`.

<a id="ml193"></a>

## ML193 — Fresh fork migration recreates empty entitlement tables around already-Spent source rows

**A. Title:** Fresh fork migration recreates empty entitlement tables around already-Spent source rows.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Restore includes upstream vtxoSpent rows after decisions but fork tables from before them or absent; migration creates empty new fork schema.

**E. Event chain and suspected mechanism:** No expiry_decided_coin/entitlement exists; resolver sees nonterminalSpent without evidence; cached clients/history may falsely infer ordinary spend.

**F. Expected correct behavior:** Restore financial stable layer consistently or reconstruct authentic obligations before accepting new settlement.

**G. Potential incorrect outcome:** Valid customer100k disappears from obligation ledger; operator may hold backing and recovery stops permanently.

**H. Evidence and inference:** E45 stable tables/history separate from upstream; E05 Spent-with-entitlement recovery; E32 empty-history path. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E45: [server/src/database/fork_migrations/V5__expiry_allocations.sql:91](/home/francis/abandon-ship-fresh-integration/server/src/database/fork_migrations/V5__expiry_allocations.sql:91), `expiry_ledger_commit`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `3712331243e47c147d3387b5887f02eeccb0d7a59e2b273ba9d4d55574ecea42`. Code fact: Permanent one-active allocation index; append-only visible-transaction revision/count; triggers across payout ledger.

E05: [server/src/database/expiry_ownership.rs:87](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_ownership.rs:87), `propose_expiry_ownership`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `e1723bb6efc0db4f18e5438f6882fd5e5d98bc58b470baa1b48f65e51c49e68a`. Code fact: Ownership distinguishes binding refresh/offboard, pool, unregistered input owner, receive bindings and per-source Lightning state.

E32: [bark/src/fallback/adoption.rs:38](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/adoption.rs:38), `adopt_reported_spent`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `aac920f00ae039828d6495006567c48f77265fa653f8ad1c15c7865498306a04`. Code fact: After post-status complete history, absent entitlement allows ordinary Spent adoption without an explicit chain payment check in this method.

Test reference: [fallback_migrations_do_not_share_upstream_history](/home/francis/abandon-ship-fresh-integration/testing/tests/server/fallback.rs:138), file SHA256 `3b384ca7e8daf6c022678030263c9245c71dbec219466ca4a50920a43faf23e6` at inventory time. Listing/reference is not an execution claim.

**I. Current coverage:** fallback_migrations_do_not_share_upstream_history proves startup/history separation; no financial checkpoint-inconsistent restore-to-spend case. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Compare prebackup signed sources and all stable entitlements/fences after restore; every100k accounted, canonical99k or explicit outstanding debt. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P0 — Source-supported risk**. Root-cause family: `BACKUP-FORK-LEDGER`.

<a id="ml194"></a>

## ML194 — Old client interprets new fallback refusal as a terminal ordinary operation failure

**A. Title:** Old client interprets new fallback refusal as a terminal ordinary operation failure.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** current.

**D. Context and financial state:** Mixed client/server deployment; server returns DecidedForFallback on prepared financial action; old client has no structured mapping/historyRPC.

**E. Event chain and suspected mechanism:** Client aborts and deletes pending material or unlocks inputs while payout remains unconfirmed/unfunded; user seesCanceled rather than owed payout.

**F. Expected correct behavior:** Distinct refusal indicates committed obligation only; retain protective material until actual chain discharge.

**G. Potential incorrect outcome:** Customer loses exit/recovery window or attempts a second payment after first still payable.

**H. Evidence and inference:** E22 FallbackRefusal; E18 retry_unless_rejected boundary; E32/E34 finality; client planK5. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E22: [server/src/database/expiry_guard.rs:9](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_guard.rs:9), `guard_obligation_changes`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b9e2eaa67d93bed53195371239ce420b15baa0a98c8a2aecb7a33a55e3557747`. Code fact: Shared coin-row locks and permanent decided fence guard obligations; participation cancellation is durable and restores only undecided other inputs.

E18: [bark/src/actions/lightning/pay.rs:152](/home/francis/abandon-ship-fresh-integration/bark/src/actions/lightning/pay.rs:152), `check_lightning_send_payment_status`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `55daadb631de69c322d79b767ff2808f537c55f94fe8b39858fff632169478a6`. Code fact: Expired Pending/transport error collapses to local Failed; revocation and exit still need authentic protection. Refund registration failure is warned, then local outputs stored.

E32: [bark/src/fallback/adoption.rs:38](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/adoption.rs:38), `adopt_reported_spent`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `aac920f00ae039828d6495006567c48f77265fa653f8ad1c15c7865498306a04`. Code fact: After post-status complete history, absent entitlement allows ordinary Spent adoption without an explicit chain payment check in this method.

E34: [bark/src/fallback/act_by.rs:67](/home/francis/abandon-ship-fresh-integration/bark/src/fallback/act_by.rs:67), `exit_protection_schedule`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `4d2087abf492f2d54cef2638f2dea281a27299ac51eb4b5584a7615d03edc5b7`. Code fact: Pubkey Spendable/Locked protection only; derives path/CSV deadlines; operation locks retained; manual-sync applications must call protection.

**I. Current coverage:** Native typed-refusal tests use compatible current client; no actual old client error/retention behavior against new server decision. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Run compatible-pair matrix later: refusal never deletes valid signed claim, actual99k verified before final retirement and no second payment intent. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `COMPAT-ERROR-SEMANTICS`.

<a id="ml195"></a>

## ML195 — Backup omits watchman signing keys although ledger and raw transactions restore cleanly

**A. Title:** Backup omits watchman signing keys although ledger and raw transactions restore cleanly.

**B. Financial invariant:** The rightful owner retains a usable eventual recovery path; database ownership or a standard output script alone does not establish spendability.

**C. Applicable implementation:** current.

**D. Context and financial state:** Cold backup has productDB/entitlements and some transaction bytes but not watchman wallet or ephemeral signing-key database at matching point.

**E. Event chain and suspected mechanism:** Restore passes schema/hash checks; new fee/progress/payout signatures cannot be created for existing branches; original users absent.

**F. Expected correct behavior:** Exercise real restore of all signing dependencies and actual customer spend, not merely database readability.

**G. Potential incorrect outcome:** Customer/operator value permanently stranded if required keys destroyed; retained seed for another wallet is insufficient.

**H. Evidence and inference:** E37 fee wallet signing; E28 ephemeral recovery keys; E27 signing; actual retirement notes say full restore drills not done. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E37: [bitcoin-ext/src/bdk.rs:390](/home/francis/abandon-ship-fresh-integration/bitcoin-ext/src/bdk.rs:390), `make_signed_p2a_cpfp`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `12bce48a0b62e24c0fc27892d872544fb28f8e8ce75ec4db7841d224278a5f93`. Code fact: Confirmed-only BDK selection, guaranteed non-dust change, version3 child and iterative actual weight.

E28: [server/src/database/expiry_payout.rs:180](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_payout.rs:180), `request_expiry_split`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `67b9740800f62fea5ea1b5781d6071c51df534b42456314b898df078f8766342`. Code fact: Split requires signed real path, every sibling and server recovery path; user-only board leaf is refused; physical reservations protect allocations.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** Native restart retains all datadirs; manifest/archive structural verification does not prove postrestore ability to sign/confirm payout. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Fresh isolated restore using documented backups only; produce actual valid parent/sponsor and confirmed99k then seed recovery spend; never print key material. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `BACKUP-SIGNING-MATERIAL`.

<a id="ml196"></a>

## ML196 — Historical treasury consolidation is mistaken for settlement of retired customer rights

**A. Title:** Historical treasury consolidation is mistaken for settlement of retired customer rights.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** historical operational migration.

**D. Context and financial state:** Retired TrackA backing54970sat reclaimed to operator treasury; simulated customer claims were not executed; old environment destroyed.

**E. Event chain and suspected mechanism:** Migration/report treats treasury recovery as zero outstanding liabilities and omits per-owner rights before later recovery project.

**F. Expected correct behavior:** Preserve customer obligations independently of custody/VM retirement; owner-authorized treasury move is not customer receipt.

**G. Potential incorrect outcome:** Operator retains backing while historical rightful user mapping disappears; permanent if signing/claim history lost. No unauthorized treasury taking is alleged.

**H. Evidence and inference:** Plan§9 retirement state; coordinator-2026-10-15/signet-retirement-20261016/STATE.json; E41/E42 old route. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E41: [server/src/database/expiry_settlement.rs:190](/home/francis/abandon-ship-bark-two-step-comparison/server/src/database/expiry_settlement.rs:190), `store_expiry_payment`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `377bee85dc542ec86957a00b7ca2e7fea58e6182f564e3de1a14f1bd97db841e`. Code fact: Historical A persists signed payout in nursery and settlement rows; refund path cancels subscriptions by hash.

E42: [server/src/expiry_payout.rs:402](/home/francis/abandon-ship-bark-two-step-comparison/server/src/expiry_payout.rs:402), `expiry_payout_tick`, repository `/home/francis/abandon-ship-bark-two-step-comparison`, HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`; exact read bytes SHA256 `5a2a5c62e1691f77df2bdc0565b05b94e434e5ff7aabf70d700138f9a2d0a566`. Code fact: Historical two-step route requires real fee estimate and swept-path proof; default sweep minimum100 confirmations; different fee/minimum policy.

**I. Current coverage:** Retained consolidation confirms custody recovery; no full old-user payout or restore drill established by it. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Per-owner pre/post retirement ledger, actual rightful scripts/spendability; recovered treasury value never counts as customer-paid amount. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `OPS-RETIREMENT-LIABILITIES`.

<a id="ml197"></a>

## ML197 — Changed financial view repeatedly rebuilds lifetime history

**A. Title:** Changed financial view repeatedly rebuilds lifetime history.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Retained history grows faster than the available rebuild service budget while changed chain/financial views continue arriving. Production starvation under this conditional workload has not been measured.

**E. Event chain and suspected mechanism:** When process_sweeps sees a changed view, rebuild reads all allocations/children, observes them sequentially through Core and performs global updates. Unchanged views short-circuit; this is not every sync callback.

**F. Expected correct behavior:** Bound operational work while preserving deep-reorg recovery and current entitlement progress.

**G. Potential incorrect outcome:** Current users wait indefinitely as history outgrows block interval/resources; DB locks/storage increase even when ordinary traffic appears healthy.

**H. Evidence and inference:** E29 unbounded SELECTs/global rebuild; E30 rebuilt tip check; planC10 operational stop-following after100. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E29: [server/src/watchman/recovery.rs:36](/home/francis/abandon-ship-fresh-integration/server/src/watchman/recovery.rs:36), `rebuild_expiry_state`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ee4534800f0b6845a90dc6f557682c9fc9049c23a4b7693fdf10d12b8301b4c0`. Code fact: Global chain re-observation rebuilds derived state; exact terminal claim clause; all allocation/fee txids scanned; unchanged allocation ID set rechecked.

E30: [server/src/database/expiry_chain.rs:131](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_chain.rs:131), `lock_valid_expiry_view`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `a20f49ca1edcec080db441fe5152a273e509b0141c34bf3de91cd02b0ed76e0c`. Code fact: Durable invalidation generations, two daemon completion ACKs and canonical hashes; restart invalidates before work resumes.

**I. Current coverage:** 6/101-block tests have one entitlement;12/50-wallet scheduled load does not establish lifetime ledger/rebuild throughput. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Measure backlog drain and oldest new entitlement under declared retained history/hardware; actual99k pays while deep-reorg evidence remains intact. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `OPS-GLOBAL-REBUILD`.

<a id="ml198"></a>

## ML198 — Urgent watchman work repeatedly consumes every new confirmed fallback sponsor

**A. Title:** Urgent watchman work repeatedly consumes every new confirmed fallback sponsor.

**B. Financial invariant:** Every valid entitlement remains accounted for and eventually reaches its rightful beneficiary; indefinite safe blocking is insufficient.

**C. Applicable implementation:** current.

**D. Context and financial state:** Same watchman wallet funds deadline claims/progress and fallback; sustained urgent work arrives before each payout sweep tick.

**E. Event chain and suspected mechanism:** New confirmed top-ups consumed by urgent packages; fallback sees only unconfirmed change/minimum shortfall each time.

**F. Expected correct behavior:** Safety-critical work may take priority, but accepted funding/operations must still ensure eventual fallback progress without altering payees.

**G. Potential incorrect outcome:** Old customer99k starves forever despite ongoing unlimited top-ups and overall active service.

**H. Evidence and inference:** E44 shared watchman_wallet/process_claims/process_progress_txs; E27 payout funding; P2 always top up no cap. The cited path supports the stated conditional mechanism; no new runtime failure is asserted.

E44: [server/src/watchman/mod.rs:377](/home/francis/abandon-ship-fresh-integration/server/src/watchman/mod.rs:377), `process_claims`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ebfd93238b0bd27248b2eaccacccaed7b1d621b4da59d54480417904377c2c6a`. Code fact: Upstream claims chunking and individual retry coexist with fork grouped payout/split paths; fee wallet and progress share resources.

E27: [server/src/watchman/payout.rs:74](/home/francis/abandon-ship-fresh-integration/server/src/watchman/payout.rs:74), `process_expiry_payout`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6c62a55dcdd76aba427de36a14f37b546beb57dc8667cbd71958426d5e92883e`. Code fact: Immutable zero-fee v3 parent; exact confirmed inputs; watchman sponsor; fee-child-only replacement; view lock spans submit_package.

**I. Current coverage:** Individual funded payout and fee cases do not supply sustained competing urgent workload with finite usable sponsor shapes. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Track each sponsor prevout, urgency cost and oldest fallback age; reserve sufficient actual confirmed throughput so original99k eventually confirms. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Source-supported risk**. Root-cause family: `OPS-FUNDING-PRIORITY`.

<a id="ml199"></a>

## ML199 — No entitlement row exists yet, so an obligations-only monitor reports nothing unpaid

**A. Title:** No entitlement row exists yet, so an obligations-only monitor reports nothing unpaid.

**B. Financial invariant:** Financial status/history must not discharge an entitlement without independently observed actual receipt by the rightful beneficiary.

**C. Applicable implementation:** current.

**D. Context and financial state:** Root contains rightful100k but readiness fails during decode/attribution before commit_expiry_group creates any entitlement.

**E. Event chain and suspected mechanism:** Monitor sums only expiry_entitlement/pending allocations; reports zero unpaid while valid signed customer backing expires unprocessed.

**F. Expected correct behavior:** Compare independent full funded cohort/source inventory to decisions and actual chain payments; predecision holds are liabilities too.

**G. Potential incorrect outcome:** Missing fallback remains invisible until evidence lost or funds swept; a quiet dashboard can become false completion.

**H. Evidence and inference:** E04 Hold/error before E23 entitlement insert; E24 inventory; simulation plan independent closed-cohort accounting. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

E23: [server/src/database/expiry_decision.rs:156](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_decision.rs:156), `commit_expiry_group`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `ffd9db09f222c73a2f6944bb5b85a9915ee58dca60ace67e14e21628faa22629`. Code fact: Ready outputs revalidated under view, registry, coin and settlement locks; dropped outputs removed before allocate; stable entitlements and Spent written atomically.

E24: [server/src/database/expiry_inventory.rs:124](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_inventory.rs:124), `check_partition`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `c4944ee82addf3369875be81d8a55e286472e961f2d6c941342f28f28a54bb94`. Code fact: Complete bounded anchor inventory; disjoint conserving geometric partition; actual paths and residuals required. Geometry alone is not economic ownership.

**I. Current coverage:** Batch14 whole-root checks are strong for its one case; no inspected actual production monitor proves unallocated-root denominator. Monitor-query choice is an explicit hypothesis. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Inject no financial state: later test uses genuine unpaid root; total funded gross100k must remain in unpaid/unknown denominator until99k+fee observed. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `OPS-INVENTORY-MONITOR`.

<a id="ml200"></a>

## ML200 — TrackC per-key owner proofs disappear when importing into xpub-only fresh state

**A. Title:** TrackC per-key owner proofs disappear when importing into xpub-only fresh state.

**B. Financial invariant:** An accepted customer right survives settlement, cancellation, recovery and migration until a justified actual discharge.

**C. Applicable implementation:** historical data / uncertain migration support.

**D. Context and financial state:** Old TrackC customer has valid signed key_link/fallback record but no server-stored account xpub; original wallet never returns.

**E. Event chain and suspected mechanism:** Fresh migration drops/ignores per-key links and creates no equivalent registered xpub; normal exhaustive search cannot match absent record.

**F. Expected correct behavior:** Preserve genuine old ownership proof or isolate legacy recovery; fresh-database permission does not establish discharge of old funded rights.

**G. Potential incorrect outcome:** Customer100k permanently NoRegisteredOwner/Hold or operator wrongly treats unmigrated value as treasury.

**H. Evidence and inference:** E40 per-key proof tables/optional recovery_index; E03 registered-xpub search; E04 no-ownerHold; selected design explicitly excludes TrackC carryover. The event chain is a falsifiable hypothesis; the cited code may already prevent it. Its stated assumptions and protection must be exercised.

E40: [server/src/database/fallback.rs:39](/home/francis/sweep-to-payout-per-key-src-9cb02072/server/src/database/fallback.rs:39), `set_fallback`, repository `/home/francis/sweep-to-payout-per-key-src-9cb02072`, HEAD `af52be109466e47de96b0fed0e42c7688420a06c`; exact read bytes SHA256 `ca60b98bb1e1649089ad7365dd816b9a254d4c72770a4de56c9d899cc55a5825`. Code fact: Historical C relies on mailbox-signed per-key links and missing-link refusal, not fresh exhaustive xpub attribution.

E03: [server/src/database/expiry_attribution.rs:67](/home/francis/abandon-ship-fresh-integration/server/src/database/expiry_attribution.rs:67), `queue_expiry_owner`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `b5b35072a0f5e4bba16113a725126784e9b76799a5e4b81845edef6ef7d5acbc`. Code fact: Per-key/per-record coverage; bounded priority and fair scans; compare-and-advance cursors; NoRegisteredOwner requires complete registered coverage. Final 8fb729a2f publication adds registry writer fence and atomic bounded-pass commit; original wrong-mailbox timing failure remains historical evidence, not a benchmark of this change.

E04: [server/src/expiry_readiness.rs:40](/home/francis/abandon-ship-fresh-integration/server/src/expiry_readiness.rs:40), `prepare_expiry_coin`, repository `/home/francis/abandon-ship-fresh-integration`, HEAD `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`; exact read bytes SHA256 `6d2b4b7b46fb2300ffa2949fc8d0a6e6eae3d6cf7dcaebf01e3871728cc57a10`. Code fact: NoRegisteredOwner remains Hold; no operator sweep is authorized. Pass batches 64 roots with hard inventory budgets.

**I. Current coverage:** Fresh migration startup tests have no authentic fundedTrackC link corpus; old forwarding/per-key tests cannot qualify fresh migration. Coverage is limited to the named source/assertions and separately pinned retained runs; it is not a fresh execution of this scenario.

**J. Future independent test oracle:** Independent old signed key_link proves rightful script; after migration/recovery actual99k reaches it once, old liabilities never vanish merely because schema changed. Apply the full-lineage and eventual-progress baseline above.

**K. Priority / evidence status:** **P1 — Hypothesis requiring validation**. Root-cause family: `MIGRATION-OWNERSHIP-PROOFS`.

