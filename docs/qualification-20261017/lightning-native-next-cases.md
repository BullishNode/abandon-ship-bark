# Next independent native Lightning case

Source-only preparation after coordinator reprioritization to Lightning. Non-Lightning drafts remain frozen. Read the existing native `expiry_lightning_terminal`, `expiry_dispatch`, `expiry_hash_provenance`, `expiry_admission`, `expiry_operation_races`, actual xpay/node-manager/source-ownership code and schema/protobuf definitions. Worktree HEAD during inspection was `5635b5a95b636894b1e1aae9117eac3d057329a9`; the underlying prompt baseline and historical executable pins remain separately recorded by root. No new build/source candidate or archived binary is qualified by this document.

Implemented only `testing/tests/server/qualification_lightning.rs`; root owns module registration. No Cargo, just, metadata, build, runtime, shared-target write, service action or Signet operation was performed. The existing confirmed container Nix environment is recorded by root. Product fixes remain backend-owner work.

## Existing evidence and the distinct gap

- `expiry_actual_failed_external_send_pays_offline_owner` and `expiry_actual_no_part_external_send_pays_offline_owner` obtain real CLN terminal failure/rejection and actual refund. They do not stop the product while an accepted external dispatch completes remotely.
- `expiry_unused_same_hash_grant_retains_its_own_liability` and its cooperative-refund variant distinguish real paid source from unused same-hash source. They do not lose the xpay task/response through real process downtime.
- `expiry_dispatch_rechecks_registered_sources_and_closed_attempts` manually starts/closes attempt rows to cover admission fences. `expiry_xpay_preserves_late_preimage_after_coarse_failure` injects a coarse failed row before actual settlement. These are useful separate boundary tests; neither satisfies the requested all-native ambiguous-dispatch restart without financial-state injection.
- Admission-duration/typed-refusal and prepared-offboard/uninitiated-grant races test other boundaries. No new direct financial-row writer is reused to manufacture this case's ambiguity or terminal result.

## Implemented case: authentic accepted dispatch settles during downtime

Test name: `expiry_external_success_during_restart_is_bound_to_its_actual_source` (proposed requirement ID `FT-LN-RESTART01`, W5/W10, D30 source-specific ambiguity and H35/D29 no duplicate refund subrequirements; no full original-ID credit).

1. Reuse TestContext's real two-peer CLN/direct-channel setup and one captaind/watchmand/Core/Postgres fixture. One native wallet boards two actual 100,000-sat roots at the same expiry, creates two signed/server-registered HTLC grants sharing one fresh hash, and captures the authentic payout script.
2. Dispatch only the first grant to a real external hold invoice for **exactly 100,000 sat**. The code calculates the actual existing fixture Lightning fee and requires zero, verifies the stored attempt's user fee is zero, and later requires actual CLN amount and amount-sent to both be 100,000,000 msat. The fixture does not override fee policy or hide a 90,000-sat invoice/100,000-sat grant remainder. Both grants are exactly 100,000 sat; only one funds the real payment.
3. Wait for actual receiver Accepted and independently verify the pending CLN group carries the exact persisted unique dispatch label and no preimage. Capture the authentic attempt ID/source links. Close/fence the sender; no subsequent wallet open, sync, revocation or money retry occurs.
4. Stop only this fixture's watchman/captaind. The owned CLN peers continue. Settle the held invoice through its real receiver API while both product processes are down. Independently require the original CLN group to be Complete, with the right hash/preimage/amount/label/created index. Product source-success receipt and settlement WAL must still be absent, proving the remote outcome was not consumed before restart.
5. Restart those same owned product daemons. Their existing periodic reconciliation must discover the original remote group and atomically persist the source-specific external-success receipt plus actual preimage. Match its created index to the independent CLN receipt, require public payment Success, and require exactly one original attempt/dispatch, one paid-source link, zero unused-sibling links and no terminal-failure receipt. The implementation waits up to 45 seconds because native reconciliation conservatively waits retry duration plus the existing 15-second xpay buffer.
6. Supply two actual confirmed watchman fee sponsors, advance the declared native HTLC deadline, and observe the unused same-hash source's automatic refund without opening the absent sender. Require exactly 99,000 sat to the authenticated script, real parent byte equality, input/output backing conservation, six actual Core confirmations and one active/canonical ledger refund with exact 1,000-sat fee. No entitlement under the **entire paid backing anchor** may have a customer allocation/refund, including historical predecessor/other descendant leaves. Independently enumerate all recorded canonical parents touching both original anchors and require their complete set of registered-script customer outputs to be exactly the one observed 99,000-sat outpoint. Requery independent CLN and require the same single completed group/created index, no second attempt or dispatch.

Economic oracle: actual external principal 100,000 + **all canonical registered-script customer outputs** totaling 99,000 + one actual customer-output fee 1,000 = both native grants' 200,000 sat. This full-anchor check incorporates client peer review and prevents a hidden predecessor refund from escaping a grant-leaf-only query. Operator fee sponsors are separate. Actual CLN direct-channel amount-sent equality excludes hidden routing cost/change in that equation. Full channel/onchain operator capital accounting is outside this bounded case.

Declared profile/resources: one wallet/two real grant roots; two existing CLN peers/direct channel; lifetime 24, exit delta 3, HTLC-send delta 40, native expiry margin 6, invoice final CLTV18. No fees are changed. Reconciliation interval is one second for the fixture; semantic safety delays remain unchanged. At most 96 blocks after setup and a 180-second outer timeout, with bounded 30/45-second observation stages. No mainnet, public-Signet campaign, external database or other track's actor is used.

This is lost **internal xpay response/task** evidence after the initiation RPC has acknowledged admission. It does not claim a lost user-facing InitiateLightningPayment acknowledgement, proxy-level transport fault injection, payment re-dispatch safety for every method, invoice timeout/refund variants, adversarial duplicate workers or all Lightning races. No elapsed height alone proves the paid source terminal: actual source-labelled CLN completion and correct preimage are required. The unused sibling's refund additionally requires its verified zero-attempt provenance and the genuine admitted deadline; the other source's preimage is not discharge evidence for it.

Proposed future command, only after root review, canonical integration, mandatory checks, immutable source/binary/metadata receipt and shared-target lease:

```sh
just int 'qualification_lightning::expiry_external_success_during_restart_is_bound_to_its_actual_source'
```

Compilation/API/runtime result: **NOT RUN**. Runtime must verify direct-channel zero routing fee, actual pending label visibility, product shutdown leaving the CLN hold accepted, and reconciliation recovering the receipt within the declared budget. Preserve every failed attempt and reconcile the original hash; never retry its money action because a stage times out.

## Subsequent narrow candidates, not implemented

1. Same accepted-dispatch downtime boundary, but receiver genuinely cancels while product is down. Require the exact source-labelled terminal CLN failure to be recovered after restart before eventual absent-owner refund; a missing/empty CLN result remains unresolved. This extends the existing native failure case through actual lost-response recovery without injecting statuses.
2. Restart with the real receiver still holding the accepted HTLC across tree expiry. Require no customer refund/operator discharge while actual CLN outcome is Pending, independently ready other roots progress, then settle/cancel and reconcile the original source. Keep this separate from never-initiated grant readiness and coordinate with backend's product unblock lane before implementing.

Stock Noah/offline-recipient claim and delivery are the client agent's separate scope. Internal cross-root subscription/committed-receive discharge is the backend product owner's lane. No competing implementation changes were made.
