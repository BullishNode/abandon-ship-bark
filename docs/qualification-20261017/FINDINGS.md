# Fresh testing qualification findings

Base: `e44ddf95741478508908c17adc61752832d75c6a`.

Current checkpoint: 17/17 case executions passed across 16 unique names and eight nextest invocations. One current-cut case was repeated with stronger history assertions. The two mailbox cases are component coverage; the other 15 executions use native operations. Mandatory `just checks` passed. Per-product/runtime pins differ and are explicit in the receipts. Production readiness remains open.

No new product verdict exists at startup. The explicit disk hold prevents builds, native test runs, large copies, services, funding and campaigns until coordinator release. The three workers inspect source and prepare a small inventory.

## Execution gates at startup (superseded by later allocation)

- Coordinator acknowledged release of disk hold: pending.
- Shared target build ownership and bounded runtime allocation: pending.
- Isolated worktree container visibility: absent; allocation requested.
- Host Nix shell: absent. Container cached flake environment verified with `IN_NIX_SHELL=impure`, Rust 1.90.0 and Core 31. No code edits occur before environment confirmation.

## Preserved known failures and limits

- The 10K wrong-mailbox search failed its 120-second deadline after 9,902 scan operations. These are not proven distinct records.
- S03 original Signet grouping failed on historical-forfeit geometry. A native correction exists. A separate repair cannot convert the original failed grouping into a pass.
- Corrected immutable runtime has dirty-source provenance and individually pinned products. Its manifest is preserved at `/home/francis/.cache/abandon-fresh/runtime-s03-current-cut-correction-20261017/manifest.json`.
- Canonical client finality files belong to another owner. This track does not alter, copy, stage, stash or reset them.
- Existing 12-wallet/60-transfer and 50-wallet/200-transfer runs are workload observations. They do not prove maximum throughput or equivalent distinct scenario coverage.
- Cross-root discharge, sustained load, legacy adaptation, stock-backend offline LNURL and production qualification remain open.

Each new run must retain its exact command, times, source and binary provenance, configuration, oracle and failure evidence. No new test pass is claimed.

## First allocated native runs

The coordinator released disk hold at 14:41 UTC and allocated one archived test at a time. The client/backend retain all shared-target writes. The isolated branch uses the coordinator-corrected nested path `/home/francis/abandon-ship-fresh-integration/testing-track-worktree` so it is visible in the existing container.

- PASS: current economic cut after sender change refresh, run `1949f9ef-8ab4-4d27-adcb-1fa7624f80fa`, 33.743 seconds. This permits one or two actual payout parents. It does not prove the original one-output S03 oracle.
- PASS: decision beats native prepared offboard, run `98579530-8ac9-46eb-a016-2abdf0c3d271`, 22.446 seconds. Genuine refusal retains the prepared checkpoint and action lock; the fallback pays and the offboard does not. Full client finality is separate.
- Registered-offboard-first reverse ordering: running.

These runs use corrected immutable native/server products built from recorded dirty `fb85d9a9c3b31b3eab1eb2113d2caec89c7f10dd` source, source manifest SHA256 `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69`, plus separately pinned older CLI products. They are not clean `e44ddf957` qualification. Per-run receipts record exact binary and metadata hashes.

## Reporting defect: search work is not distinct population

The prior 10K wrong-mailbox search result is a failed 120-second bounded run. Its 9,902 counter increments account-range work across passes and does not establish 9,902 distinct records searched. The 100/1K receipts report 116/1,016 work units, confirming that a work count can exceed the registry population. See `evidence-review.md` for exact source/receipt references. No scale pass follows from the timeout.

## Preflight corrections

- `--test server` conflicts with nextest `--binaries-metadata`; the list preflight exited 2 before running any product test. Runtime commands use one unique substring and metadata containing one binary.
- One metadata-list preflight ran as container root without the established shared `CARGO_HOME`, causing a 524MB registry download into new `/root/.cargo/registry`. Subsequent commands run as uid 1000 with `CARGO_HOME=/fresh-cache/cargo` and offline Cargo. Only that verified new root registry was removed; source, evidence and shared target/cache were preserved. No compile occurred.

## New native results and current release gates

- PASS readiness isolation, run `b4597c9f-0b79-40c5-9b2b-4865174a9791`, 26.184 seconds: independently ready same-expiry root pays 99,000 sat with six Core confirmations, while authentic pending HTLC backing remains unresolved and untouched before its deadline. The actual confirmed fee child, block hash, bytes and independently computed fee agree with the durable ledger.
- PASS stronger current-cut/two-root return history, run `e5d2a93c-c9e5-4d02-955f-76c419f53781`, 44.926 seconds: original absent-actor payout/confirmation oracle precedes reopen, authenticated per-output history and fees conserve the two coins, and total debit is exactly 39,670 sat once across all subsystems that consume either original coin. Movement IDs remain stable through repeated sync and reopen.
- PASS required `just checks`, approximately 159 seconds wall time, including style, clippy, wasm and workspace test/example type checks. This is not a unit-suite execution.

The test changes contain no product patch. Source review found an initial cross-subsystem debit blind spot and missing explicit canonical child confirmation; both were fixed and re-reviewed before the passing build/run. There were no new product-test failures in this checkpoint. The preserved original S03 and wrong-mailbox failures remain open under their exact oracles.

The bounded passing cases do not qualify automatic oversized/multilevel splitting, split/conflict/chain-only ownership rollback, native cross-root discharge or ambiguous Lightning dispatch, complete client finality on the newer published pin, stock Noah offline LNURL, sustained production scale, migrations/backup/restore or all original scenarios. Read CHECKPOINT.md for complete counts and pin limits.
