# Option B: refund absent internal senders after their grants are reclaimed

Status: **proposal for review**, 2026-10-18. Not merged, not qualified.
Code: branch `poc/stage1-b` of `BullishNode/abandon-ship-bark`, draft PR #31, head `e279e7bcf`, base `788515aed`
(the newest fork commit with the V23 money fixes).

This document states what option B changes, why, the safety argument, the evidence gathered, and what is
not yet proven. Reviewers: please try to break the safety argument in §4 and check the evidence in §5. The
specific questions are in §8.

---

## 1. Problem

**Internal Lightning payment.** Alice pays a Lightning invoice issued by Bob, and both are users of the same Ark
server. The server routes nothing over Lightning:
- Alice's coins become **HTLC-send coins**, locked to the payment hash. The server can take them with the
  preimage; Alice can take them back after their own timeout.
- The server gives Bob **grants**: pool coins locked to the same hash. Bob can take them with the preimage
  (he made the invoice, so he has it); the server can take them back after their timeout.

**The case in question.** Bob never collects and Alice never comes back. Alice's tree expires, so under
sweep-to-payout the server must pay her money to her registered fallback address: the **internal refund**.
Bob still holds the preimage. The refund must never leave Bob able to claim his grants too, or the
operator pays twice.

**Today's design (option A, joint refund).** One transaction spends Alice's backing and every one of Bob's
grants (through the server timeout clause) and pays Alice. Consequences:
- The parent's input set must be exactly "every live original of every connected attempt plus every grant".
  This needs component discovery across attempts, a grant-size budget at grant admission (so the joint
  transaction fits 10,000 vB), and a classification of every original Alice already exited herself (single,
  batch, after a prior reservation, after a reorg). Each classification is its own code path.
- The server keeps a journal of Alice's own exit transactions and re-broadcasts them after a reorg, because the
  joint transaction omits exited originals on the premise that the exit stays on chain.
- About 2,200 production lines. Both reproduced money defects of this family (V23 defect 1, and the
  cross-attempt hold in §5.1) live in this code.

The trade-off was visible in the 15 October reuse plan (`upstream-rebase-and-reuse-plan-2026-10-15.md:121`,
item U1: upstream's refund rule is "circular under direct payout") but was not evaluated explicitly.

---

## 2. Option B

**Rule.** An internal sender's original coin belongs to the sender once **every** receive grant of that
internal receive has been taken back on chain through the server's timeout clause, at least N blocks deep,
the preimage is unknown, and the coin is unspent on chain. It is then paid like any other expired coin:
ordinary readiness, ordinary per-output allocation (P1 fee, P3 same-height merge), ordinary payout.

**Who reclaims the grants.** Second's own watchman, unchanged. Its policy already claims an expired
`ServerHtlcRecv` leaf through the server timeout clause (`upstream/server/src/watchman/policy.rs:209-228`),
and the frontier records the spend as `chain_resolution = revoked` (`server/src/watchman/frontier.rs:105-115`,
classifier `:188-231`). The fork used to **block** this reclaim while the sender was unresolved; B removes that
block.

**Code** (all at `e279e7bcf`):
- Rule: `server/src/database/expiry_ownership.rs:277-293` (inside the "no resolution yet" branch).
- Grant check: `server/src/database/expiry_lightning.rs:234-261` (`internal_grants_reclaimed`): returns the
  block of the latest reclaim only if every grant of the subscription is an outgoing HTLC with this payment
  hash, has no off-chain resolution, is `revoked` on chain, and is at least N deep in both completed views; and
  no settlement (preimage) exists for the hash.
- Source state: `LightningSourceState::InternalCommitted { attempt_id, subscription_id }`
  (`expiry_lightning.rs:52, 176-230`). Several internal attempts on one source resolve to `Pending`, so the rule
  never fires for them.
- Evidence `htlc_internal_grants_reclaimed { attempt_id, subscription_id, height, hash }`
  (`expiry_decision.rs:145`). It is **conditional** like `htlc_chain` (`:262`): no irreversible customer fact is
  stored, and the rebuild marks a pending allocation conflicted when the evidence block leaves the canonical
  chain (`server/src/watchman/recovery.rs:143`).
- Commit-time locks: the evidence adds the attempt's original funding set and its subscription to the locks
  taken before the final re-check (`expiry_decision.rs:187`), the same path the zero-grant case already uses.
- Depth N: PoC knob `ABANDON_INTERNAL_REFUND_DEPTH`, default 6 (`expiry_ownership.rs:26`). Production would
  make it a config field.

**Deleted:** `server/src/database/expiry_internal_refund.rs`, `server/src/expiry_internal_refund.rs`,
`server/src/database/expiry_user_claim.rs`, `server/src/watchman/user_claim.rs`,
`server/src/vtxopool/grant_budget.rs` (and the commit that wired it into `vtxopool.rs`/`ln/mod.rs`,
reverse-applied), migrations V15/V16, the joint branches in decision/readiness/recovery/watchman, and the
watchman fence in `expiry_payout.rs`.

---

## 3. Assumptions

1. **Reorgs deeper than 3 blocks do not happen** (owner, 2026-10-18; to be recorded as a ruling). This is stricter
   than plan C10, which explicitly does not treat 100 blocks as final.
2. **N ≥ 4**; recommended **N = 6**.
3. The server host and its database writer are trusted (plan ruling §1.6), as today.
4. The watchman runs and has a credible fee estimate (plan P2), as today. Grant reclaims pay their own fee from
   the grant value; refund payouts use the funded watchman fee child, as for any payout.

---

## 4. Safety argument

**4.1 No double payment between sender and receiver.**
- The refund is decided only after every grant has a confirmed timeout spend at depth ≥ N.
- Bob can be paid only by spending a grant through his preimage clause. A grant already spent by the reclaim
  cannot be spent again unless a reorg removes the reclaim.
- A reorg that removes a reclaim with ≥ N confirmations has depth ≥ N ≥ 4, excluded by assumption 1.
- Reorgs of depth ≤ 3 that remove a reclaim **before** the refund is decided: the grant's `chain_resolution`
  is cleared on reorg (`frontier.rs:340`), the rule no longer holds, nothing is committed.
- Reorgs that remove a reclaim **after** commit but before the refund confirms (possible only if N ≤ 3): the
  evidence block is no longer canonical, so the pending allocation is marked conflicted and re-planned
  (`recovery.rs:143`). This guards decisions, not a public refund already broadcast; that is exactly the case
  excluded by N ≥ 4.
- If Bob claims a grant with the preimage, the frontier extracts the preimage and records the settlement
  (`frontier.rs:100-103`). From then on `internal_grants_reclaimed` returns none, and the sender's coin follows
  upstream's normal path (the server can take it with the preimage).

**4.2 Each sender coin is paid exactly once.**
- Every allocation covering a coin spends an outpoint on that coin's own signed path, so any two payments of
  one coin conflict in Bitcoin (plan C9, "Why nothing is paid twice").
- If Alice exited a coin herself, the coin is spent on chain and is not a payout candidate.
- If a reorg removes Alice's own exit, the coin is unspent again. Her exit transaction remains valid and public
  (Core returns it to the mempool on a shallow reorg). Any server payout and her exit spend the same coin, so
  at most one confirms. The server no longer needs to keep or replay her transaction.

**4.3 Liveness.** The refund needs: the watchman to reclaim each grant (upstream behaviour), the reclaims to
reach depth N, then the ordinary payout path. No coin is held by another coin's or another payment's state.

**4.4 Authorization.** The only new authority is the rule in §2. It never moves a coin to the operator, never
pays Bob, and requires chain facts that only the server's own timeout spend can create.

---

## 5. Evidence

Environment: isolated container `abandon-poc-recovery` (image `abandon-fresh-runtime-env:20261017`, Bitcoin Core
31.0, CLN 26.06.6 with the fork's patched cln-grpc plugin), bridge network, own cache `~/.cache/abandon-poc`,
tests run as uid 1000. Logs and kept test data under `~/.cache/abandon-poc/runs/<run>/`. Runner:
`~/.cache/abandon-poc/run-tests.sh <run> '<nextest filter>' ABANDON_INTERNAL_REFUND_DEPTH=<N>`.

**5.1 Baseline on the current design** (`poc/stage0-baseline.md`, run `stage0b`).
New test `expiry_internal_cross_attempt_claim_pays_remaining_sources` (`testing/tests/server/expiry_internal_cross_attempt_claim.rs`):
one user claim spends one original of each of two internal attempts. On `788515aed` the remaining originals were
**never refunded** (test failed at its own oracle after 330 s with blocks mined; server logged
`joint incoming source already has another commitment` for both attempts on every pass).

**5.2 Size** (brace-matched split of `server/src`, script `poc/split-count.py`):

| | production | embedded tests |
|---|---|---|
| base `788515aed` | 35,995 | 5,048 |
| option B | 33,788 | 4,329 |
| change | **−2,207** | **−719** |

`git diff --stat 788515aed -- server`: 19 files, +92 −3,045.

**5.3 Tests under option B** (`poc/stage1-B.md`):

| Scenario | Test | N | Run | Result |
|---|---|---|---|---|
| cross-attempt user claim | `expiry_internal_cross_attempt_claim_pays_remaining_sources` | 1 | s1b | PASS |
| no grants issued | `expiry_internal_zero_grant_timeout_pays_absent_sender` | 1 | s1b | PASS |
| grants, nobody acts | `expiry_internal_signed_grants_joint_timeout_pays_absent_sender` | 1 | s1b | PASS |
| two attempts share one coin | `expiry_internal_shared_incoming_root_refunds_both_absent_attempts` | 1 | s1b | PASS |
| partial user exit | `expiry_internal_one_attempt_partial_terminal_keeps_other_source_owed` | 1 | s1b | PASS |
| two originals exited in one claim | `expiry_internal_two_originals_timeout_together_keeps_third_source_owed` | 1 | s1c | PASS |
| **defect 1** (prior reservation + two-input claim) | `expiry_internal_reserved_batch_pays_remaining_source` | 1 | s1e | PASS |
| reorg of the user's claim | `expiry_internal_batched_terminal_replays_exact_authorization_after_reorg` | 1 | s1d | PASS (198,000 claim + 99,000 payout; each paid once) |
| defect 1 + reorg | `expiry_internal_reserved_batch_replays_after_reorg` | 1 | s1f | PASS (each paid once) |
| reorg removes reclaim **before** refund decided | `expiry_internal_reclaim_reorg_before_refund_pays_once` | 8 | s1g | PASS (receiver paid, sender not) |
| reorg removes reclaim **after** refund confirmed (5-block reorg) | `expiry_internal_reclaim_reorg_after_refund_pays_once` | 1 | s1h | **FAIL: double payment** (outside assumption 1; demonstrates why N ≥ 4) |

Upstream's watchman reclaimed the grants in every grant case without fork help (watchman log
`htlc vtxo resolved on chain … revoked`).

**5.4 How the existing tests were re-targeted.** Commits `55ea95b65`, `86a0481ba`, `40e474b5f`, `02b14aa0b`,
`2eb6a1b42`, `65959577e`. Test names and money assertions were kept: 99,000 to the sender's fallback script,
zero-fee refund parent, no output to the receiver, each source paid once, conservation of principal. Assertions on
today's joint transaction shape (grants as inputs, 380,000 in / 281,000 operator) and on the deleted journal were
replaced by the same facts under B (grants reclaimed by their own timeout transactions; refund spends only the
sender's backing). The shared helper `assert_retained_claim` now checks the chain fact (the coin is spent by the
user's claim) instead of the journal. **Reviewers should diff these commits and check no money assertion was
weakened.**

---

## 6. Costs and behaviour changes

| | Effect | Who |
|---|---|---|
| Refunds are ordinary per-output payouts | Coins of one sender with different tree expiry heights are paid in separate outputs, each charged 1,000 sat (P1/P3). Defect-1 fixture: 297,000 instead of 299,000. More small coins can fall under dust. | user pays |
| Refund latency | +N blocks after the reclaims (≈1 h at N = 6). | user waits |
| User's own exit after a reorg | Not replayed by the server. Core re-mines it after a shallow reorg; if the node's mempool is also lost, it waits for the user's wallet or anyone to rebroadcast. Funds stay the user's. | user (rare) |
| On-chain fees | Separate reclaim transaction (pays its own fee from the grant) and possibly several refund payouts, each with a fee child. Somewhat more total fees. | operator |
| Grant admission | The fork's grant-size budget is removed; admission is upstream's again. Receives currently refused for a fragmented pool should succeed (not yet tested). | both |
| Guarantee wording | "No double payment" now relies on assumption 1 and N ≥ 4. | owner ruling |
| Fork size and upstream distance | −2,207 production lines; the fork no longer blocks upstream's reclaim or changes grant admission; two fork migrations fewer. | maintainers |

---

## 7. Not yet done

1. Test inside the assumption: N = 4, refund confirmed, 3-block reorg at the tip; expect no double payment.
2. Remaining internal-refund tests not yet re-targeted or run: `expiry_internal_terminal_return` (4),
   `expiry_internal_partial_terminal` reorg and all-exited variants, `expiry_internal_replan`,
   `expiry_internal_existing_large_pool_cover_refunds_absent_owner`,
   `expiry_internal_unsafe_fragmented_pool_leaves_zero_grants_and_refunds_owner`.
3. Full server integration suite and client suites (`bark_sdk`, `barkd`) unchanged.
4. Alert for "a reclaimed grant later claimed with the preimage after the sender was refunded" (defence in depth).
5. Depth N as a real config field; the env knob is PoC-only.
6. Upgrade path for a database that already holds joint-refund allocations or V15/V16 tables (fresh-database
   ruling makes this optional).

Option C (the refund also spends an output of each reclaim, removing the depth assumption) is not built. Under
assumption 1 it adds no protection; its cost is an upstream change to the watchman claim builder plus a signing
path and a dependency registry.

---

## 8. Questions for reviewers

1. Can the rule in `expiry_ownership.rs:277-293` fire while any grant of the receive is still spendable by the
   receiver? Consider: grants never exited (still inside the pool tree), grants added after the first reclaim,
   a second subscription with the same hash, grants spent by a key path.
2. Can a sender coin be refunded while the server could still settle the subscription cooperatively (Bob comes
   back online before the reclaim)? Check the lock order at commit (`expiry_decision.rs:187`) against
   `lock_internal_receive_claim` (`expiry_lightning.rs`) and `server/src/arkoor.rs:140-151`.
3. Is the per-coin argument in §4.2 sound for every coin shape that reaches readiness (exposed HTLC-send leaf,
   checkpoint prefix, tree output), and does any path still assume the deleted journal?
4. Does removing the grant budget reintroduce a risk the budget protected against other than joint-transaction
   size?
5. Did the test re-targeting (§5.4) weaken any financial assertion?
6. Is N ≥ 4 the right bound under "reorgs deeper than 3 blocks never happen", given how confirmations are counted
   (`expiry_lightning.rs:234-261`: `height + N − 1 ≤ min(captaind tip, watchman tip)`)?
7. Anything in the client (bark) that depended on the joint refund's single-output shape or on server replay?

## 9. Reproduce

```bash
git clone https://github.com/BullishNode/abandon-ship-bark && git checkout poc/stage1-b
# inside a container from abandon-fresh-runtime-env:20261017 with the cln-grpc plugin in LIGHTNINGD_PLUGIN_DIR:
cargo build --workspace --bins
cargo nextest run -p ark-testing --config-file <nextest with 600s slow-timeout> \
  -E 'binary(server) and test(expiry_internal_)'   # set ABANDON_INTERNAL_REFUND_DEPTH as in §5.3
```
