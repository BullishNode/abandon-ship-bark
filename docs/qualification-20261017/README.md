# Fresh qualification checkpoint, 2026-10-17

All 17 selected case executions passed across 16 unique test names and eight nextest invocations. The current-cut case ran once from its original archive, then again with stronger returning-wallet history assertions. Mandatory `just checks` passed. Production readiness remains unproven.

This branch adds two independent native oracles. A ready root must pay while another root with the same expiry retains an unresolved HTLC. The current-cut regression now verifies authenticated two-root payout history, exact debit across all subsystems that consume either original coin, and stable movement IDs through repeated sync and reopen. The actor remains absent until all original payout outputs have six independent Core confirmations.

| Batch | Result | Scope |
|---|---|---|
| Current cut after change refresh | PASS 1/1, 33.743 s | Actual recipient conservation, six confirmations; permits one or two payouts |
| Decision before prepared offboard | PASS 1/1, 22.446 s | Refusal retains checkpoint/holder; actual fallback discharge |
| Registered offboard before decision | PASS 1/1, 22.579 s | Actual offboard pays; no customer fallback duplication |
| Upstream registration | PASS 2/2, 46.373 s | Duplicate IDs and child of completed exit |
| Upstream offboard/mailbox | PASS 5/5, 62.605 s | Signed retry, limits, session cleanup, two mailbox component checks |
| Corrected reorg/fee/conflict/split | PASS 5/5, 142.702 s | Depth 6/101, real RBF, partial-conflict restart, requested mixed-sibling split |
| New independent-root readiness | PASS 1/1, 26.184 s | Exact 99,000-sat payout and canonical fee child; pending backing untouched before HTLC deadline |
| Stronger two-root return history | PASS 1/1, 44.926 s | Original payout oracle plus exact 39,670-sat debit once, actual per-output fees and stable IDs |

The two mailbox cases use component fixtures. The other 15 executions use native operations. Test executions, unique test names, original catalogue scenarios and payment counts are separate quantities. No full workspace unit run or maximum throughput measurement was performed.

## Exact source and product scope

The branch starts from published `e44ddf95741478508908c17adc61752832d75c6a`. The test code commit is `bc3d02835e0aa539d5a5dd8f7c1385ed85227ce5`; all 1,021 source file paths and bytes match the captured build snapshot. It contains no product changes. The new test ELF was compiled from that source plus these test changes, source-manifest SHA256 `3f52d4f99a55097698c698b012d035f0037328d8baf392f4d13e18047fbad0ae`, and archived with ELF SHA256 `81a1414d68d894b5d20a7a96a32dd56df26058b768a915f99f17ee19ae88a42f`. Build duration was 3m31s. Metadata package IDs match the isolated worktree.

The first six batches reuse corrected archived products and test code built from dirty source labeled `fb85d9a9c3b31b3eab1eb2113d2caec89c7f10dd`, source-manifest SHA256 `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69`. Both new tests use those same external corrected servers. CLI products have separate older source provenance in every receipt. The SDK code is linked into its test ELF. These pairings are not a clean all-products candidate qualification.

All integration execution uses repository `just int-binaries`, the cwd required by metadata package IDs, uid 1000, the existing Nix environment, shared Cargo cache, and private test/snapshot/nextest state. The coordinator released disk hold before execution and assigned the shared-target write lease before the metadata build and mandatory checks. No product executable was rebuilt. The lease was returned after checks. Exact commands, hashes, times and oracles are in per-run receipts and runner scripts. A zero-test selection is not a pass.

## Coverage and release limits

The inventory preserves all 80 original IDs: 37 unchanged, 43 adapted, none discarded. Additional requirement-gap and existing-test rows overlap. The CSV row count is not a count of unique scenarios or passes. Historical evidence stays separate. No whole original case is credited merely from a partial test or a different candidate.

The original S03 one-output grouping failure remains failed. The current-cut tests permit one or two actual outputs, with 1,000 sat per actual output. They do not establish the original one-output 38,670-sat oracle. The 10K wrong-mailbox run remains a failed 120-second bounded run. Its 9,902 counter measures account-range scan work, not proven distinct records. Existing public-Signet load observations establish neither maximum throughput nor sustained production capacity.

Remaining gates include automatic oversized/multilevel splitting, split/conflict/chain-only ownership rollback, native cross-root discharge and ambiguous Lightning dispatch recovery, broader attribution/scale deadlines, migrations and backup/restore, operational liquidity/resource pressure and sustained load against declared production objectives. Newer client finality work belongs to its separate owner and source pin. Stock-backend offline LNURL still needs authentic strict invoice compatibility and payment/discharge evidence. No mobile work is required.

No Signet campaign, funding or service mutation occurred in this track. Per-run receipts and summary logs contain no seeds, private keys, credentials or raw wallet databases. Successful private raw datasets were hashed and removed after process ownership checks; receipts, scripts, logs and private hash manifests remain. The independent source review found two oracle gaps, both fixed and re-reviewed before compilation. Inventory notes describe their earlier read-only checkpoint and do not supersede later coordinator allocations.
