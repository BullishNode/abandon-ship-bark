# Eighth testing checkpoint — 2026-10-17

Completed evidence through the terminal six-case batch24 prefix totals **142 executions:136 PASS and6 preserved FAIL;138 unique test names;115 nextest invocations;131 daemon-backed native cases and11 components.** These are executed test counts, not money-loss scenario counts or one clean candidate's full-suite result. Batch24 stopped after5PASS/1FAIL; its remaining23 cases are unstarted. No native run remains active at this cutoff.

| Evidence increment | Executions | PASS | FAIL | Native | Components |
|---|---:|---:|---:|---:|---:|
| Previous checkpoint through SDK prefix28 |76|74|2|68|8|
| Remaining13 previously unstarted SDK cases |13|13|0|13|0|
| First two ordinary CLI cases |2|2|0|2|0|
| Batch21 stopped at original sequence27 |27|26|1|26|1|
| Batch22 stopped at its first case |1|0|1|1|0|
| Batch23 stopped at its case17 |17|16|1|15|2|
| Through terminal batch23 |136|131|5|125|11|
| Batch24 stopped at its case6 |6|5|1|6|0|
| **Current completed totals** |**142**|**136**|**6**|**131**|**11**|

The ordinary upstream SDK whitelist is now **45/45 PASS**: initial4 plus all41 in batch19. Its original resource stop occurred before case29 started; the resumed new-runtime cohort executed only the13 previously unstarted cases. It was neither a retry nor a failed test. The original28-case prefix receipt remains unchanged. Core's16-case cohort also passed. Selected source functions match upstream `bfbe6b7c3088a09dd4e81c4ac24cdc763a627a8c`; the binaries remain fork products. Ordinary SDK coverage does not qualify the12 deferred heavy/CLN/watchman cases or all fork additions.

These regression cohorts used immutable source manifest `704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06`, captured814d7b0d plus recorded owner patch. The CLI test ELF is `f3e33f52d458f5dc5f0dcd98f99ca363b266d4e35f979a418d47e8830dcd46f7`; product bark is `d971db9547a3bb6c8ebd0413404ed886d3d1157f233c259ab905945ad0a96444`, reporting0.7.1-dev. New authored adaptations were absent from those immutable binaries. Exact per-case receipts and provenance, including SDK metadata `faaa21775c92a77890b3c61f5af28d04c3e59d751c12868348e4a0f5b0c912b4`, are retained separately.

Four original CLI failures remain visible:

- `create::bark_address_works_offline`: original sequence27, FAIL10.652s. The old same-wallet test compared its first fresh address with explicit index0. Accepted K2 reserves child0 for the fallback identity, so the first fresh address is1. The paired address adaptations retain exact same-index online/offline equality, successive fresh1/2 and reserved0 separation. They do not claim a native PASS.
- `create::bark_create_force_flag`: FAIL15.259s, run`c5640527-12f8-45df-8dff-47798cc91637`. A first-time, unfunded creation after server shutdown passes the public create_without_server flag, but first fallback binding still requires the live server. Accepted child0 reservation alone does not establish an intentional retirement of that public option. The product owner fix/contract resolution is pending; this failure is not hidden by changing the test assertion. This case had no customer-money flow.
- `fees::send_onchain_fee_deducted`: FAIL13.196s, run`352e12d4-ac04-40ce-b2e3-28401ceebf8b`. Actual3854 differs from old P2TR literal3938 by84: base3000+7sat/vB*(100+actual22-byte BIP84 script)=3854. Retained transaction/accounting evidence supports300000destination+3854fee+496146change=800000. The fee assertion failed before the later recipient/change/estimate assertions; those did not pass in this original run. Its adaptation uses the public fee quote and actual script length while preserving every later exact assertion. Independent parent Core-block evidence is separate from fixture completion.

- `movement::board_board`: batch24 case6, FAIL11.082s, run`85c49b07-b0e1-4eaf-9709-3eb55920515a`. The original metadata assertion expected772sat onchain_fee and observed763. Independent parent retained Core reconciliation shows the actual transaction at3confirmations:1000000input=100000board+899237change+763minerfee. This supports the observed fee, not a passed metadata assertion or six-confirmation lifecycle. No retry or source adaptation is credited; any correction remains separately reviewable. The first5cases passed and the remaining23 never started.

The two paired address tests, fee test and two SDK watchman tests are **ADAPTED source only / NOT RUN / UNCOMPILED**. Original failures and frozen original source remain preserved; the original onchain address case and both watchman cases remain unrun. Watchman adapters replace the obsolete operator-only sweep/full-principal refresh contract with actual registered-customer net payouts,1000fee per actual output, Core6, whole-root ownership/conservation and no remint. Peer review found a default change-split factor2 assumption in their first draft. A separate reviewed v2 explicitly pins the sender factor1 to produce the intended two physical coins and two customer outputs; source bda6c497af71c94ea799fda1dc99d720bd6a034776428fa2d4fed5488386b9b1. The default2 grouped-output case remains unqualified. No product-default or passed-adaptation claim follows from this source work. Other source-only ML designs and frozen unintegrated fixtures also receive no execution credit.

The unchanged200-scenario ML map is **198 NOT RUN;2 bounded FULL/PASS;145 PARTIAL;53 NONE**. ML165/166 credit remains limited to the actual327-owner split geometry/source7041/run63a761b8 and its exact canonical conservation oracle. Neither ordinary regression passes nor new test source automatically close a complete financial scenario. Canonical acceptance remains coordinator-owned.

Default integration inventory remains643 listed /638 default-selected /5 ignored. The800-pass/1-skipped unit baseline, separately executed ignored unit, required checks and any external owner benchmarks retain their distinct receipts. Full relevant integration, migrations/upgrades, Tor/network/feature gates, deferred Lightning/scale cases and new adapted fixtures remain open. No full-suite, production, release, payout-throughput or public Signet qualification is established. Native runs retain serial execution, zero retries and default60-second slow period/terminate-after3=180s; observed durations are not isolated performance benchmarks.

This report is an evidence checkpoint, not completion of the testing track. Batch24 contributes only its terminal5PASS/1FAIL prefix to the current142-execution totals; its23 unstarted cases contribute no execution or acceptance credit. Original failures and every prior checkpoint receipt remain preserved.
