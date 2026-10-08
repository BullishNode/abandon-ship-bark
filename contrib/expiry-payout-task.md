# Native expiry payout task: implementation status

Branch `expiry-payout-task` is an unfinished implementation of approach D.
There is no background payout task yet. It currently retains approach B's
settlement endpoint while the native path is being built; it is not a release
of the no-sidecar design.

The fee estimator now exposes `real_rates()`. It returns no rate at startup
or after a fallback update, including when fallback happens to have identical
numeric values. The returned actual Core estimates bypass the existing rate
cap; existing fast/regular/slow callers retain their previous behavior.
Availability and rates are read together under the existing lock.

Validation, 2026-10-07: server unit suite 161 PASS / 0 FAIL, 12 database tests
ignored by that invocation. Repository prechecks, ark-lib arithmetic clippy,
WASM test compilation and complete workspace test/example compilation PASS.
Compilation is not execution of the integration/database tests. Existing
warnings remain. Exact commands/output are recorded in the local four-way
comparison observation log, runs 20261007T234848-4d6af54b and
20261007T235558-e073fca4.

Still required: the native task, atomic payment integration, receipts/client,
full regtest and signet qualification, backup/restore and operations runbooks,
final four-way comparison and production readiness review.
