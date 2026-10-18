# Stage 0 baseline (base 788515aed + scenario-10 test)

Container `abandon-poc-recovery` (image abandon-fresh-runtime-env:20261017, bridge network, 6 CPUs, own cache
~/.cache/abandon-poc), tests run as uid 1000 with the patched cln-grpc plugin (structured errors) copied
from the coordinator's cache.

| Claim | Test | Result |
|---|---|---|
| K2 | `expiry_internal_cross_attempt_claim_pays_remaining_sources` | **HOLD reproduced.** FAIL after 330 s at the test's own oracle: "HOLD: a cross-attempt user claim stranded the remaining originals' payouts". Server logged "Whole internal refund component remains pending" for both attempts on every pass after the claim (cause: `joint incoming source already has another commitment`; the claimed originals carry `chain_resolution=revoked` but the two-attempt claim matches neither accepted claim shape, so they are treated as live). Log: ~/.cache/abandon-poc/runs/stage0b/cross-attempt.log |

First run (stage0/) was killed by nextest's 180 s limit while already holding; rerun used a 600 s limit.
