# Twenty-fifth checkpoint

**Both allocated tests passed on their first runs.** H32, which tests a later receipt after a dust forfeiture, passed in 43.613 seconds. H11, which transfers the same principal through three owners, passed in 27.819 seconds. Each run selected exactly one test, filtered out 304 others, used the default 180-second limit, and allowed no retries.

The updated testing record contains **257 executions: 242 PASS and 15 preserved FAIL**, across 237 unique binary/test pairs. There are 230 nextest invocations, 245 native executions, 12 component executions and 468 mixed coverage rows. Rows FT-SERVER-020 and FT-SERVER-021 were appended without changing any of the previous 466 rows or their failures. No whole catalogue or money-loss scenario was promoted automatically.

## Money outcomes

The independent audit reconstructed these selected transaction sets from retained public Bitcoin blocks:

| Transaction set | Boundary inputs (sat) | Unspent terminal outputs (sat) | Actual miner fees (sat) |
| --- | ---: | ---: | ---: |
| H32, first epoch | 500,000 | 497,719 | 2,281 |
| H32, final epoch | 600,000 | 595,438 | 4,562 |
| H11, final state | 300,000 | 297,719 | 2,281 |

In H32, the original recipient first accepted the completed forfeiture of a genuine 1,200-sat receipt: zero payout and zero output fee. Only afterward did the test create a fresh, independently funded 100,000-sat receipt for the same account and destination. That receipt paid 99,000 sat with a 1,000-sat output fee. The old receipt and history remained unchanged through repeated synchronization and reopening. Across both original customer roots, 200,000 sat equals 196,800 paid, 2,000 in policy charges and 1,200 forfeited.

The first H32 package had seven confirmations at its checkpoint and 59 at the retained final tip; the later package had eight. An unused 100,000-sat sponsor output from the first checkpoint was spent in the second epoch. The two frontiers are successive states and must not be added together.

In H11, two genuine, full-value SDK sends moved 100,000 sat from A to B to C. After all original clients stopped, only C received a 99,000-sat fallback output with a 1,000-sat output fee. The complete retained chain contains no fallback output to A or B. The actual parent and fee child share a block and have eight confirmations. A and B retain their legitimate send history; C records one fallback debit. Repeated synchronization and reopening passed the original wallet assertions.

Policy charges are separate from actual miner fees. Ordinary funding change and board miner fees lie outside the selected root-and-sponsor transaction sets.

## Provenance and checks

The source manifest `7d7c5b45…` binds the captured V19 test source, including H32 file hash `67aa2a84…` and H11 file hash `acaaabcd…`. Runtime metadata is `293c92d3…`; the native server binary is `36a09461…`. The four product binaries are verified, source-equivalent V18 artifacts with their original `57f59` label. They are not relabelled as the captured `4e3` HEAD or the later coordinator commit.

The independent postflight, `161a5912…`, verified all 17 sealed control files and 16 runtime pins. Both actual test process groups and scoped services were absent. The allocation was consumed and released. Session 61525 is the execution handle, not a process ID.

The financial audit, `ca8f1bd9…`, checked public block structure, headers, Merkle roots, canonical input uniqueness, actual values and complete selected frontiers. Parent recomputation, `503892b4…`, independently checked the decoded transaction arithmetic and rehashed nine original public inputs; it was not a second raw-block decode. Full hashes and run IDs are in the linked receipts.

Wallet identity, signed ownership proofs, database classification, balances, history and reopening retain their native-test provenance. Public operator/watch script labels do not independently prove private-key custody. No independent mempool reconstruction, global custody, default-daemon, seed recovery, reorg, throughput, full money-loss catalogue or production-readiness claim follows.

The initial receipt and original control export retain their pending-at-creation wording. Later audit receipts resolve that history without rewriting it. The earlier V1 summary-parser issue was found before execution; it caused no native failure or retry. All earlier failures remain preserved.

This checkpoint publishes reports only. The separate H31 source draft and the coordinator's V20 work are excluded and receive no execution credit. Original frozen documents are preserved alongside this editorial revision.

Evidence: [native receipt](batch51-H32-H11-v19-receipt.json), [formal postflight](v19-H32-H11-postflight-backend.md), [financial audit](batch51-financial-audit-legacy.md), [parent recomputation](batch51-financial-parent-recompute.json), [coverage append](batch51-coverage-append.json).
