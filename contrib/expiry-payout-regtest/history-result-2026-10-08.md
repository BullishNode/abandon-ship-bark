# Candidate lookup with spent history

The current candidate query scans settled history without an eligible-coin index.
A session-local PostgreSQL benchmark copied the real fixture and added one million
synthetic spent records. These are planner fixtures, not valid new entitlements or
payment/user throughput evidence.

First executed measurement: 1,145.181ms, failing the declared 1,000ms bound
(`20261008T131347Z-history`). It filtered 1,001,446 rows and read 143,015 local
blocks to return 29 candidates. An earlier setup attempt ran no SQL because Docker
stdin was not forwarded; its failure is retained.

A second run (`20261008T134718Z-history`) measured the unchanged source query before
and after the proposed partial index in the same temporary table:

| Measurement | Before | After |
| --- | --- | --- |
| Total rows | 1,001,509 | 1,001,509 |
| Candidate rows returned | 29 | 29 |
| Execution time | 714.633ms | 0.202ms |
| Local blocks read | 143,018 | 30 |
| VTXO access | sequential scan | `expiry_payout_candidates` index scan |

The index covers `(expiry,vtxo_id)` only for unconfirmed pubkey coins whose state
is spendable or unclaimed. The query and payout policy are unchanged. It is part
of the optional installation DDL, not a numbered upstream migration. History
outside that predicate no longer fills the candidate scan.

The complete test passed in 345.702 seconds, mostly constructing the synthetic
history. Cache state and fixture setup affect timings; the execution plans prove
the changed access path. This is a single-host planner result, not a latency SLA
or a mainnet capacity claim. Tables disappeared when the session disconnected;
no live coin state or original volume was replaced. Raw plans and source snapshots
are retained under `expiry-task-evidence/native-history-index-1/` and its runtime
folder, outside the checkout.

The index was then installed on the private fixture. PostgreSQL reports it valid
and ready. The shipping c594 image's funded payout check passed in 336.999 seconds,
including exact recipient fees, stable settlement identity and confirmation
(`native-index-live-1/report.json`). Server units: 162 passed, zero failed;
repository style/clippy/WASM/workspace checks passed.

The benchmark excludes this index from its baseline even after installation and
checks identical candidate IDs and order, plus actual index use. That revised
harness passed on 10,000 additional spent rows (11,544 total) in 10.199 seconds;
indexed query 0.413ms (`native-history-repeatable-1/report.json`). This smaller
repeat checks harness behavior; it does not replace the million-row measurement.
