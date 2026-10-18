# Stage 1: option B (reclaim, wait N confirmations, refund independently)

Base 788515aed. Branch poc/stage1-b. All runs in the isolated container, logs under ~/.cache/abandon-poc/runs/.

## Change
- Deleted: joint internal refund (component discovery, joint parent, reserved/batch claim classifiers),
  user-claim journal + replay + backfill (V15/V16), receive-grant admission budget.
- Added: one ownership rule. An internal sender's original resolves to the sender once every receive
  grant of its receive is revoked on chain (server timeout clause) at depth N, nothing settled, source unspent.
  Upstream's watchman performs the reclaim; the fork's block on it is removed.
- PoC knob: ABANDON_INTERNAL_REFUND_DEPTH (default 6). Production would make it config.

## Size (K4) — brace-matched split of server/src
| | production | embedded tests |
|---|---|---|
| base | 35,995 | 5,048 |
| stage 1 | 33,788 | 4,329 |
| delta | **-2,207** | **-719** |
Migrations: -31 lines (V15, V16).

## Results
| Claim / scenario | Test | Depth | Result |
|---|---|---|---|
| K2 / 10 cross-attempt claim | expiry_internal_cross_attempt_claim_pays_remaining_sources | 1 | PASS (base: HOLD) |
| K1 / 1 zero grants | expiry_internal_zero_grant_timeout_pays_absent_sender | 1 | PASS (unchanged) |
| K1 / 2 grants, nobody acts | expiry_internal_signed_grants_joint_timeout_pays_absent_sender | 1 | PASS |
| K1 / 3 shared root, two attempts | expiry_internal_shared_incoming_root_refunds_both_absent_attempts | 1 | PASS |
| K1 / 4 partial user exit | expiry_internal_one_attempt_partial_terminal_keeps_other_source_owed | 1 | PASS |
| K1 / 6 batch user exit | expiry_internal_two_originals_timeout_together_keeps_third_source_owed | 1 | PASS |
| K1 / 7 defect 1 | expiry_internal_reserved_batch_pays_remaining_source | 1 | PASS |
| K1 / 8 reorg of user claim | expiry_internal_batched_terminal_replays_exact_authorization_after_reorg | 1 | PASS (each source paid once; 198k+99k) |
| K1 / 8 defect 1 + reorg | expiry_internal_reserved_batch_replays_after_reorg | 1 | PASS (each source paid once) |
| K3 / 9 reorg before refund signed | expiry_internal_reclaim_reorg_before_refund_pays_once | 8 | PASS (receiver paid, sender not) |
| K3 / 9 reorg after refund confirmed | expiry_internal_reclaim_reorg_after_refund_pays_once | 1 | **FAIL: DOUBLE PAYMENT** (5-block reorg; refund re-mined without the reclaim; receiver's preimage claim confirmed) |
| K6 upstream reclaims grants | observed in all grant cases | | confirmed |

## Behaviour differences vs the joint refund (owner should know)
1. Refunds are ordinary per-output payouts: coins of one sender with different tree expiry heights are paid
   in separate outputs, each charged the 1,000 sat fee (P1/P3). Defect-1 fixture: 297,000 instead of 299,000.
2. After a reorg the server no longer replays the user's own claim. The user's wallet (or anyone) rebroadcasts
   it; Bitcoin allows only one spend of each source, so each source is paid once.
3. Grant reclaims pay their own miner fee from the grant value (upstream claim), instead of the operator's
   CPFP child on the joint parent. Operator money either way.
4. B's guarantee is "no double payment unless a reorg deeper than N removes a grant reclaim after the refund
   was signed" (measured above).

## Not yet re-targeted / run
expiry_internal_terminal_return (4), expiry_internal_partial_terminal reorg + all-terminal variants,
expiry_internal_replan, expiry_lightning_terminal large-cover/fragmented-pool, the full server suite, client suites.
