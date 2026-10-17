# Source-specific Lightning fallback evidence

A stored preimage proves cryptographic knowledge, not that every signed grant
using its hash funded a payment. This implementation records actual queued
attempts, one-shot external dispatches, terminal node receipts and internal
funding commitments. Operator ownership requires discharge of that particular
source. A coarse local Failed status never proves an external attempt terminal.

An external success receipt requires the source dispatch's own unique label in
actual CLN history, a Complete group and its valid preimage. Internal funding is
recorded atomically with the actual Created-to-Accepted promotion. Its economic
discharge requires the associated nonempty receive grant set to be claimed
off-chain. Unrelated same-hash receives are not cancelled by a sender refund.
Unknown legacy attempts and ambiguous remote executions remain unresolved.

## Captured candidate

The corrected 1,007-file source manifest SHA256 is
`f41c7d958e3f4acfddf9cf629bbb84f0f4e79b1f6e42a22b27936129b461ce1c`.
It records base `8d2ed38b032ab054af4991e9ea3cd0b62da5231c` plus the explicit
backend changes. Product build used the external BIP84 and embedded-web test
features, with web distribution `10ecfb9`. Mandatory `just checks` passed.
The preceding aggregate source passed 788 unit tests with one upstream skip;
the subsequent code delta restored existing refusal wording and added a
second confirmed test fee sponsor. Documentation added after these gates does
not change the tested product source.

Corrected executable SHA256 values:

| Executable | SHA256 |
|---|---|
| captaind | `19953757e5f9760fcb8b58dcb24ab9560cf2ce49ed20e6664c0a5a685f001d03` |
| watchmand | `e5f30f76cc60a10165a7fbb5ac4e60fbd0893039d91158f8bff0c9956344c7af` |
| bark | `0bf0f1f6e50c0cbe1656f1f78a67a46d429b9bf1250e89dc2b91787f0d5967b3` |
| barkd | `ece7c3064332fb863e1838f1031c81b9f3246984c479b343caa39a26c8c86830` |
| native server tests | `d606e47e6fb167adfcd4ce7d8f44af2c0496e10bf044cc54384c52e3b74a7012` |

Source, executable copies, original/immutable nextest metadata and complete
logs are retained in `runtime-backend-source-discharge-corrected-20261017`
under the coordinator's private artifact cache. No raw wallet or node secrets
are published here.

## Actual native results

Run `d3025d92-06ad-44b3-8649-4a0848d3452e`: **5/5 passed, 99.128 seconds**.

- Two genuine signed grants share a hash; only the first pays the external
  invoice. The unused grant remains its own liability and receives an actual
  99,000-sat payout with at least six Core confirmations. The paid source never
  receives a second customer payout.
- Actual paid-source revocation is refused, including after its coarse attempt
  status is deliberately regressed. Existing public refusal wording remains.
- A granted intra-Ark receive stays committed after invoice expiry.
- Revocation remains refused while the watchman is behind and becomes eligible
  only after its genuine receive obligations are no longer claimable.

On the preceding captured candidate, run
`d4286ba5-946c-4877-8401-3194cb3cd67c` passed six additional cases: actual failed
external payment and reviewed no-part external payment both produced confirmed
offline refunds; late actual settlement persisted its preimage despite a stale
Failed row; a cancelled superseded refresh retained its owner; both ordinary
and already-cancelled intra-Ark receives blocked premature revocation.
Two other cases in that run failed solely on changed refusal wording; the
corrected run above retained and passed those assertions.

Run `ed93c482-d392-470e-871a-b6efb6065533` passed the genuine unused same-hash
cooperative refund, actual queued-dispatch admission checks and three migration
startup/cancellation/history cases. Its offline same-hash case timed out
because the first tree's fee child consumed its sole confirmed watchman
sponsor. The corrected fixture supplies two separately confirmed sponsors;
all ownership, payout and confirmation assertions remain unchanged.

The original `800aa433-8fea-4f71-9583-0c924d17d9c7` wrong-source ownership
failure, blocked migration cancellation and contextual-error fixture failure
remain preserved. A later unexpected development-container exit interrupted
the session after the product build had completed; no native pass is inferred
from that interruption.

## Remaining release blockers

This is targeted regtest evidence, not production qualification. Autonomous
internal/cross-tree Lightning closure, ambiguous dispatch recovery, legacy
attempt provenance, full product-operation decision races and unregistered
pre-board abandonment still need evidence or implementation. The node
quiescence runbook is not an automatic recovery mechanism. Exact patched CLN
transport is required for reviewed zero-part receipts; no live node was changed
by these tests. Registration-scale results and public-Signet lifecycle evidence
are reported separately and must not be inferred from this test set.
