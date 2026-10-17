# Noah, forwarding, and retained-evidence review

Read-only research on 2026-10-17. Scenario labels NE01–NE18 below are local candidates for the parent catalogue, not additional ML IDs. No software, tests, services, wallets, campaigns, or deployments were run or changed. This document is a research artifact only.

## Inspected scope and provenance

- `/home/francis/noah-fresh-backend-20261016`: `AGENTS.md`; clean detached HEAD `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0`; relevant backend source `barkd_client.rs`, `routes/public_api_v0.rs`, `routes/gated_api_v0.rs`, `cache/invoice_store.rs`, `db/user_repo.rs`, `types.rs`; selected tests `tests/public_api_v0.rs`, `tests/gated_invoice_tests.rs`. Mobile/Android excluded.
- `/home/francis/bark-simulator-signet-rework`: clean HEAD `d8cdb3b66c44df62cd946c57d18c3ffa8c544fab`, branch `work/signet-simulation`; targeted `docs/evidence/noah-actual-invoice-rejection-20261017.json`, S03 blocker/repair/depth-six receipts, load50 receipt, `docs/noah-fresh-signet.md`, `docs/80-case-coverage.md`.
- `/home/francis/abandon-ship-fresh-integration`: HEAD `751bd5a5d8ec9bbf72f0a44c1e7d4ac371b6269d`, branch `fresh/xpub-sweep`, observed dirty files `server/src/database/expiry_lightning.rs`, `server/src/ln/mod.rs`, `server/src/ln/node_manager.rs`, `testing/tests/bark_sdk/lightning_receive_binding.rs` plus untracked worktrees. Read relevant portions of `bark/src/actions/lightning/receive.rs`, `bark/src/lightning/receive.rs`, `server/src/database/expiry_attribution.rs`, `testing/tests/server/qualification_receive_fallback.rs`. These reads establish source facts, not the build identity of deployed executables.
- Integration directory: final plan; testing-track evidence-review, findings/checkpoints, batch09 original dispatch reconciliation, batch11/batch14 summary logs; historical retirement `STATE.json` and `RECOVERY-HANDOFF.txt`; `git worktree list --porcelain` for the shared Bark repository. The integration directory itself is not a Git repository.
- No large archives extracted, no executable bytes independently rehashed, no remote/live process queried, no fresh chain query. Earlier review documents and manifests are evidence with stated scope, not authority. Several root-owned retained test-data directories were unavailable during an inventory search; their contents were not reviewed. No claim of exhaustive historical source/log review is made.

Historical inventory discovery: former A includes worktrees `abandon-ship-bark-two-step-release-integration-20261015` HEAD `9a9334d6a18bfcd7a371a10a6fe85ce985dacf97`, branch `production/track-a-two-step-per-key`, and `abandon-ship-bark-two-step-comparison` HEAD `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b`. Former B root `abandon-ship-bark` HEAD `2c29c425debeeb28e7975c53777db08a9eea1aba`, branch `variant-b`; selected port source in accepted plan is `15e4fd708`, a different pin. Historical sweep worktrees are retained beneath `sweep-to-payout-evidence-2026-10-13` and include `sweep-ownership-src` HEAD `62ced22494d8dec53d8a81beb8664149f16f4af1`, `sweep-mixed-evidence-src` HEAD `b8b443ae4e1c81894443520767b442dea1e97376`, and many detached snapshots. Merely listing them does not constitute review of their implementations.

Retirement STATE says all 12 old A/B/C target/simulator/qualification/Noah VMs and three owned volumes were deleted, keys/stopped databases retained, 4,000 sat remained reserved across four Lightning nodes, older inactive B fallback seed balance unknown, and full restore drills were not performed. Its `fresh_product_implementation: NOT STARTED` is a historical checkpoint, contradicted by later fresh activity and not current status. Original A 54,970-sat reserved backing was owner-authorized reclaimed to treasury; this is explicitly not a customer-payout success.

## Concrete retained results

1. **Actual Noah callback rejection, no paid receive.** `noah-actual-invoice-rejection-20261017.json` identifies clean Noah source `8e68ba5...`, ELF `87b2c181e5ba6e57bdb9ac715d2cb56602f2edc6f621c42ef7c6fd397489d187`, and an actual HTTP 200 callback for 5,000 sat. Independent verifier rejects `LNURL metadata hash missing; plain description is incompatible`. Payment was not attempted, watchman not started, funding zero. This demonstrates a compatibility/liveness failure, not theft, fallback failure, or successful offline payment.
2. **Original S03 failure and separate repair.** Blocker receipt shows 28,670 sat paid, a separate 10,000-sat customer entitlement blocked by historical forfeit geometry, with valid owner derivation. Repair receipt records another 9,000-sat payout and actual seed-only spend of both payouts; depth-six receipt later confirms that spend six-deep. The repair used dirty build HEAD `fb85d9a...`, captured manifest `0ff7819...`, published-equivalent geometry `f809bc...`; it was not a clean f809 build. It did not establish original one-output grouping: 37,670 sat/two actual output fees instead of the proposed 38,670 sat/one fee. Whether an additional fee violates policy requires simultaneous independent readiness, which these receipts do not establish.
3. **Stock CLN no-part gate failed.** Original batch09 run `930221cb...` kept its timeout. Read-only reconciliation confirms stock plugin SHA `421c19...`, actual RPC 205 wrapped in untyped gRPC Unknown, zero parts in original immutable SQLite. Required typed adapter SHA `6a752d...` was not installed. Zero stored parts alone does not prove dispatch quiescence. Separate adapter-positive evidence does not erase this stock capability gap.
4. **Registration scale remains bounded failure.** Retained indexed receipt `7562d5ea...` resolves absent-mailbox 10K in 103.265 s but wrong-mailbox remains unresolved at 120.139 s. `records_scanned=9902` counts repeated account-range operations, not 9,902 distinct accounts. Source `expiry_attribution.rs:234-237` increments per result; 100/1K receipts even exceed population. No funded VTXOs/payout or full 2^31 search was demonstrated.
5. **Native offline-forwarding coverage has material credit.** Batch14 run `61d55416...` PASS 36.184 s, `qualification_registered_forwarded_receive_pays_absent_recipient_after_provider_stops`, uses one 10,000-sat invoice/one final recipient output, blocks mailbox delivery, stops provider, and tests native fallback plus seed recovery. Source asserts all customer outputs and provider exclusion (`qualification_receive_fallback.rs:209-247`), real BIP84 recovery spend (`250-305`), and actual Core unspent/confirmed state (`160-185`). It is not Noah HTTP/LNURL coverage, multi-input forwarding coverage, or interruption before registration.

## Candidate scenarios

### NE01 — LNURL invoice cannot commit advertised metadata

A. Concrete title: Stock Noah returns a plain-description invoice to a strict LNURL payer.

B. Invariant: An advertised offline Lightning receive must have a valid payable invoice and preserve the declared recipient binding.

C. Applicable: Current Noah source and actually observed controlled Signet backend artifact.

D. Preconditions: Active registered recipient with Ark address; forwarding enabled; recipient absent; callback requested for 5,000 sat without Ark negotiation.

E. Sequence: Metadata response publishes exact bytes; amount callback asks barkd for a plain memo; invoice returns HTTP 200; strict payer/verifier rejects missing description hash. No payment has occurred.

F. Correct behavior: Invoice commits exactly to advertised metadata, correct amount/network/recipient, and remains payable.

G. Potential outcome: Recipient receives nothing; payer retains funds. This is an observed admission/liveness failure, not an established loss of an already-created customer entitlement. If operators bypass validation to claim success, wrong-recipient binding becomes a separate risk.

H. Evidence: Noah `8e68ba5`, `public_api_v0.rs:385-392,446-449`; `barkd_client.rs:163-217`; simulator `d8cdb3b`, actual-invoice rejection receipt and exact ELF pins above.

I. Coverage: `test_lnurlp_invoice_request_uses_barkd_without_waking_device` at `tests/public_api_v0.rs:412-457` accepts fake `lnbc-forwarded`, asserts plain description, and proves routing only. Actual retained verifier establishes rejection, not a financial receive.

J. Oracle: Independently parse callback invoice and compare description hash against SHA256 of exact metadata bytes; only then follow original payment/claim/output/fallback lineage.

K. Priority/status: P0 compatibility release blocker; **Observed failure**, limited to invoice acceptance.

### NE02 — Device invoice submission is not bound to the authenticated recipient

A. Title: A misrouted request identifier accepts a different account's invoice.

B. Invariant: Payer's invoice must belong to the user whose Lightning address they selected.

C. Applicable: Current Noah.

D. Preconditions: Device fallback path is used; a request identifier is delivered to the wrong existing authenticated account through an assumed coordination/misdelivery fault.

E. Sequence: Request is associated only with a random identifier in the notification; submit handler discards auth identity and stores caller-supplied invoice under that identifier; public waiting request returns that raw invoice.

F. Correct behavior: Immutable request owner and invoice recipient binding checked independently of possession of an opaque identifier.

G. Outcome: Payer may settle a wrong user's invoice, leaving intended recipient unpaid; recovery depends on that payee returning funds. Actual cross-account payout is unobserved.

H. Evidence: Noah `8e68ba5`, `gated_api_v0.rs:225-244` (`_auth_payload` unused), `public_api_v0.rs:509-527,542-578`; `InvoiceStore::store:18-22`.

I. Coverage: `test_submit_invoice_requires_auth`/`requires_existing_user` check authentication/existence only; `test_submit_invoice_stores_in_redis` stores fabricated data without request-owner proof. No inspected cross-account binding test.

J. Oracle: Two controlled identities and independently verified invoice payee; only intended identity's invoice may be returned; wrong submission must leave canonical request unchanged and no wrong payment.

K. P0; **Source-supported risk**; misdelivery/request-ID availability is an explicit assumption, not observed access compromise.

### NE03 — Device fallback does not validate the requested amount

A. Title: An incorrect device invoice amount is forwarded unchanged.

B. Invariant: Settled amount must equal payer-authorized LNURL amount and intended entitlement.

C. Applicable: Current Noah device fallback; distinct from forwarding-provider path which does check amount.

D. Preconditions: Correct authenticated recipient and request ID; device supplies a valid BOLT11 with an incorrect amount because of stale request state or multiple-device coordination.

E. Sequence: Submit stores raw invoice; polling returns it without parsing or comparing original `amount`; payer may pay incorrect amount or reject, leaving expected receive absent.

F. Correct: Bind exact msat amount to durable request and validate before response.

G. Outcome: Overpayment or underpayment relative to intended transfer, or delayed receive. No observed loss; a conforming independent payer may reject first.

H. Evidence: Noah `8e68ba5`, `public_api_v0.rs:408-421,542-578`; `gated_api_v0.rs:225-244`; `SubmitInvoicePayload`, `types.rs:239-243`, contains only invoice and transaction ID.

I. Coverage: `test_submit_invoice_overwrites_existing:189-242` replaces fabricated `lnbc1000...` with `lnbc2000...` and asserts success, proving byte replacement, not amount safety. Barkd amount test does not cover device path.

J. Oracle: Independently compare returned invoice amount and all actual payments against original callback msat amount; inspect no extra invoices/settlements.

K. P1; **Source-supported risk**; actual payment depends on payer validation.

### NE04 — Competing device replies replace the invoice for one request

A. Title: Second-device completion overwrites an already selected receive attempt.

B. Invariant: One request has one stable payment identity and recoverable owner-specific operation.

C. Applicable: Current Noah.

D. Preconditions: Two authorized devices process the same notification, each creates a separate valid invoice/receive action for the same recipient.

E. Sequence: Both submissions use identical request ID; Redis SETEX unconditionally replaces first invoice; polling GET and separate DEL can interleave with replacement; caller loses the stable identity of the selected original attempt while a different receive checkpoint remains live.

F. Correct: A single committed request-to-payment identity, idempotent retries, durable reconciliation of any extra created attempt.

G. Outcome: Payment status/history may follow the wrong attempt and later retry can create an additional payment; absent original device can make attribution/reconciliation difficult. Double payment is conditional on retries being permitted, not proven by overwrite alone.

H. Evidence: Noah `8e68ba5`, `InvoiceStore::store:18-22`, `get:25-29`, `remove:32-36`; public polling `542-554`; test explicitly permits overwrites.

I. Coverage: `test_submit_invoice_overwrites_existing` proves sequential last-write-wins; it has no concurrent GET/DEL, stable original hash, settlement, or duplicate-payment oracle.

J. Oracle: Independently enumerate invoices/actions and payer settlements associated with one request; retain one original hash and at most one actual debit; every created conditional receive must resolve.

K. P1; **Hypothesis requiring validation**; duplicate payment requires unsafe retry beyond the demonstrated overwrite.

### NE05 — Stale or corrupt Ark address is accepted as a recipient binding

A. Title: Valid Noah authentication updates forwarding destination without proving Ark key ownership.

B. Invariant: Money addressed to a hosted Lightning identity reaches that identity's accepted rightful Ark key.

C. Applicable: Current Noah/shared Bark address validation boundary.

D. Preconditions: An authenticated account re-registers using a stale, corrupted, or another wallet's Ark address due to restore/configuration error; no malicious credential compromise is assumed.

E. Sequence: Registration stores `ark_address` string; uniqueness rejects duplication only; invoice request forwards directly to this stored address; valid claim may become owned by an unintended key.

F. Correct: Accepted recipient identity/address policy must be explicit and verified; ambiguous changes cannot silently redirect existing entitlements.

G. Outcome: Another wallet can receive money intended for hosted identity, or an inaccessible key strands it permanently. Intentional authorized forwarding is not a bug; scenario assumes unintended binding and demands a policy clarification.

H. Evidence: Noah `8e68ba5`, `RegisterPayload:187-194`; `public_api_v0.rs:611-624,667-672,437-449`; `user_repo.rs:266-279` updates raw string with duplicate constraint only.

I. Coverage: Inspected forwarding endpoint test supplies a generated valid address; it proves request forwarding, not authenticated Ark key ownership or restoration correctness.

J. Oracle: Independently compare signed claim's actual policy key to documented intended wallet seed/address, registered xpub and final fallback script; foreign address must be rejected or explicitly confirmed under an accepted policy.

K. P0 ownership-policy ambiguity; **Source-supported risk** for absent binding, financial error contingent on unintended address.

### NE06 — Hosted username changes between metadata and callback

A. Title: A reused Lightning username binds an in-flight callback to a different account.

B. Invariant: Metadata selection and callback cannot change rightful recipient during a payment.

C. Applicable: Current Noah; username reassignment semantics need confirmation.

D. Preconditions: Payer caches metadata; original account changes hosted Lightning identity and old name becomes available to another valid account before callback.

E. Sequence: Callback URL encodes username only; callback resolves current `find_by_lightning_address`; no request token pins earlier account; new account supplies invoice/address under identical text metadata.

F. Correct: Define accepted alias-reuse policy and bind in-flight metadata to immutable recipient under it.

G. Outcome: Payer pays newly assigned owner while intending original user; valid invoice amount/network alone cannot identify mistake. Permanent recovery may depend on new recipient.

H. Evidence: Noah `8e68ba5`, `public_api_v0.rs:356-370,385-395`; `gated_api_v0.rs:update_lightning_identity:330-367`; repo update API reassigns current hosted identity. No inspected tombstone policy establishes whether reuse is prohibited.

I. Coverage: Public tests cover active/inactive users and server negotiation; no inspected metadata/callback identity-change test.

J. Oracle: Pin immutable identity at initial metadata selection and inspect exact invoice/claim/payout recipient after controlled username transition; require policy-specific rejection or original binding.

K. P1; **Hypothesis requiring validation**, especially whether old names are reusable and expected semantics.

### NE07 — Same-amount, same-network forwarding invoice has the wrong receive identity

A. Title: Misassociated barkd response passes all Noah invoice checks.

B. Invariant: Invoice must correspond to the forwarding receive action for the requested recipient.

C. Applicable: Current Noah plus shared forwarding daemon.

D. Preconditions: Faulty backend version/proxy/state returns a different valid invoice with same network/amount; trusted backend is not presumed malicious.

E. Sequence: Barkd request contains recipient address; response contains only invoice; Noah checks parse/network/amount but no recipient-action proof or returned operation identity; accepts misassociated invoice.

F. Correct: Follow the returned hash to immutable receive action/destination, not infer recipient from equal amount.

G. Outcome: Another recipient/provider receives funds, intended recipient remains unpaid; permanent if wrong private key owns final outputs.

H. Evidence: Noah `8e68ba5`, `InvoiceResponse:122-125`; `BarkdClient::create_invoice_for_address:169-217`. This is an end-to-end boundary risk, not evidence barkd actually returns a wrong response.

I. Coverage: `creates_an_authenticated_invoice_for_the_supplied_address` asserts outbound HTTP request and expected invoice string; wrong-amount/network tests prove only those two checks. No inspected signed destination lineage proof.

J. Oracle: Inspect original invoice hash's actual provider checkpoint, signed claim policy, server-registered outputs, mailbox identity and fallback script; amount-only comparison is insufficient.

K. P0; **Hypothesis requiring validation**, backend misassociation is explicit assumption.

### NE08 — Invalid or expired device invoice outlives an accepted request

A. Title: Device fallback reports success for a nonpayable invoice.

B. Invariant: A request acknowledged as invoice-ready must remain payable or be reconciled as unpaid without losing customer intent.

C. Applicable: Current Noah device path.

D. Preconditions: Device returns stale/expired or wrong-network invoice after delayed push processing.

E. Sequence: Submit accepts any invoice string; polling removes stored bytes before response; payer cannot pay, while backend success/cleanup removes request's only invoice evidence. A later operator may confuse successful submission with receipt.

F. Correct: Independently validate invoice network, expiry and metadata, and retain accurate unpaid state.

G. Outcome: No funded entitlement has necessarily arisen; request liveness and monitoring fail, and replacement attempts can obscure original payment status. No observed funded loss.

H. Evidence: Noah `8e68ba5`, `gated_api_v0.rs:225-244`, `public_api_v0.rs:542-578`, `InvoiceStore` 60-second TTL. Contrast barkd path's amount/network check; expiry is not checked there either.

I. Coverage: Gated tests deliberately use fabricated invoice strings, proving storage, not payability. No inspected device expiry/metadata/network tests.

J. Oracle: Independent invoice verification and original hash settlement history; reject invalid data before caller-visible success; no invented paid state.

K. P1; **Source-supported risk** for invalid acceptance, financial consequences conditional.

### NE09 — Forwarding provider disappears after disclosure but before signed recipient registration

A. Title: Lightning settles while the only durable receive action is provider-side.

B. Invariant: Actual successful payment must create a recoverable rightful recipient entitlement without the provider returning.

C. Applicable: Current shared Bark receive path; Noah depends on it.

D. Preconditions: External forwarded receive; provider has destination intent; preimage persisted before transmission; server receives claim RPC and obtains preimage; provider stops before successful output registration.

E. Sequence: Provider stores `PreimageRevealed`; remote payment can settle; signed outputs are constructed/registered only after reply; provider never reopens, so replay cannot be assumed. Server may retain conditional/provider HTLC identity instead of final recipient leaf.

F. Correct: Autonomous source-specific settlement/recovery must preserve intended recipient under accepted ownership rules; stored intent cannot be called materialized signed delivery.

G. Outcome: Recipient unpaid; provider/operator may be incorrectly refunded or funds block indefinitely. Permanent if no authoritative recoverable destination/claim survives.

H. Evidence: Fresh `751bd5a5` receive action `712-731` separates preimage RPC from output registration; `670-675` binds destination only when building claim. Source demonstrates boundary; no runtime failure established.

I. Coverage: Batch14 source requires actual registered `Delivery.vtxos` before provider stop; batch11 covers provider reopen. Neither establishes permanent-stop before registration.

J. Oracle: Original external settlement plus complete source-specific liability, no sender refund, rightful signed policy/output and independently confirmed fallback without provider/recipient returning; conserve entire pool lineage.

K. P0; **Source-supported risk**; no claim current resolver necessarily mishandles it.

### NE10 — Registered recipient output exists but mailbox delivery never does

A. Title: Attribution confuses the forwarding provider's checkpoint with ownership.

B. Invariant: Registered signed recipient outputs belong to recipient despite provider holding bytes.

C. Applicable: Current source with partial positive native coverage; historical regression target.

D. Preconditions: Actual signed claim registered; recipient offline; all mailbox POSTs fail; provider stops permanently.

E. Sequence: Server output registration succeeds before separate `Delivering` checkpoint/delivery; no mailbox association exists; expiry ownership must derive recipient from xpub rather than provider receive subscription.

F. Correct: Recipient's registered script paid once; provider has zero corresponding entitlement.

G. Outcome: Incorrect provider refund/operator payout or indefinitely unattributed recipient money if fallback depends on mailbox delivery.

H. Evidence: Fresh `751bd5a5` receive action `731,748-760,792-831`; native fixture `qualification_receive_fallback.rs:96-136,209-247`.

I. Coverage: Batch14 PASS has real one-output claim, outage, provider stop, full tree customer-output exclusion and seed-only spend. It proves this exact 10K/one-pool-input case on its runtime manifest, not multiple sources, different xpub search positions, or Noah HTTP integration.

J. Oracle: Enumerate all active allocations and actual outputs rooted in every funding input; match recipient key/script and gross-minus-per-output fee; zero provider payout; actual seed spend.

K. P0 regression gate; **Hypothesis requiring validation** for untested multi-source variants; baseline has strong positive evidence.

### NE11 — Disclosure retry budget expires without an autonomous owner-resolution producer

A. Title: Parked post-preimage action waits forever after retry exhaustion.

B. Invariant: Already committed Lightning entitlement must eventually recover without original wallet returning.

C. Applicable: Current shared client action behavior and server fallback dependency.

D. Preconditions: Preimage revealed; transient errors repeatedly prevent claim or delivery; finite configured retry budget exhausted.

E. Sequence: `claim_attempts` increments durably; action becomes parked with no wake time; recipient is absent; only independent server resolution can discharge liability. A source/policy mismatch can leave neither active retry nor identifiable final output.

F. Correct: Parked client action cannot erase server-side liability; autonomous chain/settlement reconciliation must produce rightful payout or durable progress.

G. Outcome: Funds delayed indefinitely; permanent if pending source data/preimage/destination is lost or expired ancestry becomes unrecoverable.

H. Evidence: Fresh `751bd5a5`, receive action `199-224,247-249`; comments `263-268` require caller fallback exit after disclosure. Source supports client liveness stop; server eventual failure is unproven.

I. Coverage: Batch11 provider reopen exercises resumption, not never-returning wallet with exhausted budget before registration. Batch14 stops only after recipient registration.

J. Oracle: Permanently absent provider after exhausted budget; actual original settlement; eventual independently confirmed rightful payout and lineage conservation, not merely persistent parked row.

K. P0; **Source-supported risk**, qualified by independent server resolver behavior.

### NE12 — One delivery channel acknowledgment conceals failure of the recipient's recoverable mailbox

A. Title: Any-success delivery ends forwarding while the restoration channel is absent.

B. Invariant: Delivery completion must not destroy eventual discovery/recovery of recipient-owned value.

C. Applicable: Shared Bark delivery dependency.

D. Preconditions: Destination supports multiple delivery mechanisms; one returns success while authoritative/recovery mailbox is unavailable; recipient stops before importing outputs.

E. Sequence: `post_arkoor_to_mailboxes` returns `AnySucceeded`; provider finishes movement and settled record; action disappears; recipient cannot obtain signed bytes from its available channel. Fallback search is last recovery producer.

F. Correct: Accepted delivery semantics and fallback guarantee must preserve discovery/ownership independently of a transport acknowledgment.

G. Outcome: Unseen VTXOs can expire without known recipient or seed restoration discovery; permanent if all delivery bytes and verified ownership vanish.

H. Evidence: Fresh `751bd5a5`, `deliver_vtxos_to_address:821-831`, `deliver_lightning_receive:804-818`. Assumption: address has multiple meaningful delivery paths and a successful acknowledgment need not imply durable recipient availability; underlying mailbox helper was not fully inspected.

I. Coverage: Batch14 forces all mailbox requests to fail, not mixed delivery outcomes. No inspected mixed-success recovery test.

J. Oracle: Inspect actual signed outputs/policy and all channel outcomes; recipient absent; independent seed-only discovery and actual spend after expiry; no reliance on transport return code.

K. P1; **Hypothesis requiring validation**; do not treat `AnySucceeded` alone as bug.

### NE13 — Historical accepted-forfeit geometry blocks an independently owned leaf

A. Title: Valid recipient entitlement is stalled by obsolete point/exit geometry elsewhere under the root.

B. Invariant: Complete conserving cut and independent readiness must eventually pay rightful customer value.

C. Applicable: Observed historical deployed snapshot; correction source present/current prevention needs verification.

D. Preconditions: Root includes accepted refresh forfeits and an attributable recipient leaf; historical forfeit path point does not match computed exit outpoint.

E. Sequence: Native unsigned leaf paths validate; historical forfeit path fails; root stays unready; another root pays, this root remains unpaid despite confirmed available backing.

F. Correct: Select valid economic cut and recover customer leaf while preserving accepted-forfeit/operator partition.

G. Outcome: Original observed 10,000-sat entitlement stalled. Later repair paid 9,000 sat after one output fee; not permanent in retained case. Broader malformed ancestry may strand indefinitely.

H. Evidence: Simulator `d8cdb3b`, S03 blocker receipt, readiness error `server/src/expiry_readiness.rs:128`; original product `7055d1fce...`; correction publication `f809bc...` equivalent captured dirty-fb85 source; S03 repair/depth-six receipts.

I. Coverage: Original failed live case remains failure; separate native correction permits one or two payouts and confirms both; repaired liability actually recovered. This does not establish original one-output grouping or all incomplete-forfeit shapes.

J. Oracle: Every original customer leaf and operator partition conserved on chain, no duplicate descendant allocation, actual seed spending; preserve failing original bytes as regression input.

K. P0 regression; **Observed failure** for retained original, with later scoped repair explicitly credited.

### NE14 — Stock transport cannot prove genuine zero-attempt send terminal

A. Title: Correctly refused ambiguous refund becomes an unbounded legitimate payout hold.

B. Invariant: A genuinely failed unattempted send must eventually refund, while actual successful sends cannot refund.

C. Applicable: Observed stock CLN runtime packaging; separate typed adapter current configuration uncertain.

D. Preconditions: Genuine no-channel/no-part send; CLN RPC 205; stock gRPC plugin omits typed terminal metadata; sender disappears.

E. Sequence: Node returns untyped Unknown; server correctly retains ambiguity; original bounded terminal-proof wait expires; no authoritative quiescence producer arrives to release source refund.

F. Correct: Qualified source-specific authenticated terminal proof, autonomous payout progress, no text-parsing or coarse-hash shortcut.

G. Outcome: Legitimate refund delayed indefinitely under stock capability mismatch. Original test establishes timeout, not permanent loss or adapter defect.

H. Evidence: Batch09 run `930221cb...`, `FINDINGS.md:57-66`, `batch09-original-dispatch-reconciliation.json`; stock/qualified plugin SHA pins above. Original immutable SQLite proves zero stored parts at observation, not absence of possible in-flight dispatch.

I. Coverage: Three companion cases passed; original no-part FAIL retained. Separate qualified-adapter PASS establishes exact pairing only; no default stock-package success or runtime startup capability enforcement established.

J. Oracle: Exact executable/plugin/config pins plus independent source-specific no-dispatch/quiescence, no actual payee credit, original sender's full gross-minus-one-fee fallback on chain without restart/replay.

K. P0 packaging/recovery gate; **Observed failure**, timeout scope only.

### NE15 — Repeated wrong-mailbox work starves a valid registered owner

A. Title: Nonmatching mailbox evidence consumes bounded attribution capacity before rightful xpub is reached.

B. Invariant: Missing/misleading mailbox cannot make a valid owner wait forever or become operator money.

C. Applicable: Retained current-family indexed scanner; exact latest production capacity unqualified.

D. Preconditions: 10K permanent registrations, wrong mailbox hint, low-index actual owner among synthetic keys; production concurrency/arrival assumptions not measured.

E. Sequence: Scanner performs repeated account-range operations; wrong-mailbox case stays unresolved at fixed 120s; monitoring misreads 9,902 as distinct accounts; queue can accumulate faster than coverage reaches rightful registration.

F. Correct: Fair durable coverage and eventual actual match; no timeout-to-operator transition; honest progress measures.

G. Outcome: Customer entitlement delayed indefinitely if workload outruns service; permanent if operations incorrectly finalize unknown ownership. No funded loss observed in synthetic case.

H. Evidence: Indexed run `7562d5ea...`; source captured as `792c8ea7d`/`3d7a3fd4a`; `expiry_attribution.rs:229-237`, test harness repeated counter; testing-track `evidence-review.md` details exact manifests. Latest read HEAD751 source still increments per result.

I. Coverage: Synthetic signed registrations and positive low-index matches, not funded VTXOs, full range/no-owner proof, distinct-account coverage, concurrent traffic or production capacity. Bounded timeout remains FAIL.

J. Oracle: Track per-key/per-registration covered intervals and age of actual unpaid funded entitlement; prove eventual match/payout with concurrent arrivals; independent chain/script verification and no operator diversion.

K. P0 liveness qualification; **Observed failure** of bounded discovery requirement, not actual money loss.

### NE16 — Historical retirement sweep is mistaken for discharged customer liabilities

A. Title: Treasury consolidation obscures reserved customer value after environment destruction.

B. Invariant: Destruction/migration must preserve every entitlement and recovery path; treasury receipt is not customer payment.

C. Applicable: Historical A/B/C operational evidence; current migration regression target.

D. Preconditions: Retired environment has reserved backing or Lightning funds; owner authorizes test treasury recovery; later migration/report reuses recovery totals as settlement evidence.

E. Sequence: Available backing swept to shared treasury; target VMs/volumes deleted; backups retained but full restore drill absent; customer-specific claims not executed or marked paid. Conflating treasury credit with payment discards unresolved obligations.

F. Correct: Maintain liabilities independently of treasury custody and prove old customer recovery before claiming discharge.

G. Outcome: Rightful user's entitlement disappears from migrated accounting, with operator holding backing; permanent if mapping/seeds/contract data unavailable. The owner-authorized test retirement is not itself evidence of unauthorized taking.

H. Evidence: Retirement `STATE.json`, old A 54,970-sat reclaim statement; remaining 4,000 sat/four nodes and inactive B seed balance unknown; full restore drills not performed.

I. Coverage: Structural/hash checks and treasury consolidation demonstrate custody recovery, not individual customer payout or complete contract empty-state. Original A campaign not a fallback success.

J. Oracle: Reconstruct per-owner liability before/after migration independent of treasury balance; every unresolved source preserved, actual recoverable key/contract and eventual rightful spend; no unexplained operator enrichment.

K. P1 historical migration gate; **Hypothesis requiring validation** for misuse of receipts, not observed entitlement deletion.

### NE17 — Financial monitor counts returned invoice or RPC success as receipt

A. Title: Readiness telemetry reports offline receive completion without customer funds.

B. Invariant: Payment reporting must reflect actual rightful receipt and eventual recoverability.

C. Applicable: Current Noah logging and simulator evidence interpretation boundary.

D. Preconditions: Provider returns a syntactically accepted invoice; monitoring keys on HTTP 200 or `Created forwarded Lightning invoice with barkd`.

E. Sequence: Invoice creation logs success before any payment; metadata may be invalid as in actual retained callback; no payer debit/claim/output exists, yet monitoring could close customer ticket or suppress missing-payout alarm.

F. Correct: Distinct invoice/settlement/signed-output/delivery/fallback/recovery evidence; only actual independent financial observations establish customer receipt.

G. Outcome: Unpaid customer goes unnoticed; if operational accounting closes entitlement based on this event, recovery obligation may disappear. No such production monitor implementation was inspected; its use is a stated assumption.

H. Evidence: Noah `8e68ba5`, public endpoint `452-469` logs creation and returns invoice; actual-invoice receipt payment_attempted=false despite HTTP200. Simulator load50 receipt explicitly limits scale/throughput claims.

I. Coverage: Backend handler unit test proves HTTP routing; actual callback proves rejection. Neither establishes payment. Native batch14's chain/spend oracle is the appropriate stronger standard.

J. Oracle: Independent original-hash settlement, correct signed policy/output and actual confirmed fallback outpoint plus spending ability; no duplicate payouts anywhere in lineage.

K. P1; **Hypothesis requiring validation**, dependent on monitor interpretation; code log is accurately invoice-only.

### NE18 — Historical-source labels hide a mixed executable environment

A. Title: A green result from a corrected dirty server is attributed to an uncorrected release binary.

B. Invariant: Financial release qualification must apply to the exact executed code and dependencies.

C. Applicable: Shared operational qualification/deployment process, not an established current binary defect.

D. Preconditions: Client7055, dirty-fb85 corrected servers, stock versus adapter CLN, and newer test harness coexist; reports use one commit shorthand.

E. Sequence: Green native repair or adapter-positive result is credited to named release; actually deployed unchanged server/plugin lacks geometry or terminal-proof behavior; customer receives missing fallback after same historical trigger.

F. Correct: Source capture, executable hashes, config/profile/plugin pins and tested oracle remain inseparable; current Git branch cannot identify deployment.

G. Outcome: Release retains known liveness defects while readiness falsely passes; customer recovery delayed or blocked. No deliberate repinning or actual wrong deployment was observed.

H. Evidence: S03 repair runtime pins dirty server build HEADfb85 with equivalent published geometry f809 and original CLI7055; batch09 stock/adapter hash difference; latest HEAD751 dirty state. Manifests distinguish these explicitly.

I. Coverage: Existing retained manifests provide useful exact pins; source-document commit alone cannot replace them. No fresh executable hashing/deployment verification performed in this review.

J. Oracle: Independently obtain actual deployed ELF/plugin/config hashes and source manifest; require exact qualification or explain file-by-file equivalence and untested changes; replay later tests only in authorized next phase.

K. P0 release-evidence gate; **Hypothesis requiring validation** for mistaken deployment/qualification attribution.

## Root-cause map and test-order suggestions

- NE02–NE04 and NE08 share absent durable request-to-owner/invoice binding; amount, competing operation identity, and account ownership are distinct obligations but should share one controlled request model.
- NE01/NE07 concern invoice contract binding; metadata acceptance is already observed broken, recipient-action misassociation remains hypothetical.
- NE09–NE12 share the distinction between conditional receive, materialized signed recipient claim, and mailbox availability. They require separate boundaries because the rightful policy and recovery producer differ.
- NE13 is a retained actual geometry/liveness regression with scoped later recovery. NE14 is retained actual runtime-capability failure. NE15 is a retained bounded scale failure with no funded-loss claim.
- NE16–NE18 concern evidence/operations losing sight of liabilities; do not count them as observed product bugs.

First qualify exact stock/typed CLN packaging and pre-registration permanent-provider-stop recovery, then strict Noah metadata acceptance and immutable recipient identity, then multi-input registered forwarding without mailbox delivery, then fair funded owner-search progress. Keep all original failures and exact historical artifacts; do not reinterpret later narrow green runs as erasure of earlier evidence.
