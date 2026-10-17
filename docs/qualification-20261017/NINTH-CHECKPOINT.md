# Ninth testing checkpoint — 2026-10-17

The fixed cutoff through batch32 contains **184 executions: 171 PASS and 13 preserved FAIL; 180 unique binary/test names; 157 nextest invocations; 173 native cases and 11 components.** COVERAGE has 395 mixed inventory, requirement and execution rows. These numbers are not counts of complete money-loss scenarios or a clean candidate full-suite result.

The newer 16-case cohort stopped with 3 PASS / 1 FAIL and 12 unstarted after this cutoff. Those four results, including the original `onchain::onchain_drain` failure, are excluded from every total in this checkpoint. They require separate receipts and reconciliation. No unstarted case receives PASS or FAIL credit.

| Increment | Executions | PASS | FAIL | Native | Components |
|---|---:|---:|---:|---:|---:|
| Eighth checkpoint through batch24 |142|136|6|131|11|
| Batch25 barkd first2 |2|1|1|2|0|
| Batch26 CLI first2 |2|1|1|2|0|
| Batch27 barkd first19 |19|18|1|19|0|
| Batch28 barkd first1 |1|0|1|1|0|
| Batch29 barkd remaining14 |14|14|0|14|0|
| Batch30 CLI first1 |1|0|1|1|0|
| Batch31 CLI first2 |2|1|1|2|0|
| Batch32 CLI first1 |1|0|1|1|0|
| **Fixed completed cutoff** |**184**|**171**|**13**|**173**|**11**|

These 42 added executions are distinct names and singleton invocations. All use immutable source manifest `704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06`: captured814d7b0d plus the reviewed owner patch. The five-binary metadata is `c22e785532b29bfcda024483c669e2ade5b8e96d0038dac9309df4bed51e8ea4`. Exact original function bodies match upstream `bfbe6b7c3088a09dd4e81c4ac24cdc763a627a8c`; executable products remain fork builds. Bark's test ELF is `f3e33f52d458f5dc5f0dcd98f99ca363b266d4e35f979a418d47e8830dcd46f7`; barkd's is `1fd1f89bc795f770890b10342325f7b41214bfe9cda3d4cc792bf0bdb3239e12`. CLI runtime metadata3143345c and barkd metadata ca5f652c preserve their single-binary parent subsets and helper relocation. The product Bark is d971db95…, version 0.7.1-dev. True version conditions and actual native body directories exclude macro early-return credit.

The original 38 ordinary barkd queue now has 36 executed: 33 PASS / 3 FAIL. Two originals remain unrun: the fixed funding-fee round recovery case and the invalid-signed-body/server-status case. Root requires the latter to quarantine invalid bodies; Spent requires authentic signed discharge. The ordinary CLI 77 scope has 59 executed: 51 PASS / 8 FAIL at this cutoff. The reserved-index address case and the same fixed-fee `onchain::list_utxos` case remain unrun and deferred for truthful source adaptations. The newer 16-case continuation is outside this checkpoint. Core 16 and the ordinary SDK 45 remain PASS under their own earlier receipts; CLN/heavy/watchman additions are not implied by those passes.

Seven new original failures remain visible:

- Batch25 `boards::board_all_barkd`: FAIL 6.071 s, run a807d8a4…. Net 98,298 differs from fixed 98,348. Separate retained accounting shows100,000 = 608 actual miner fee + 1,094 Ark fee + 98,298 credited board. The1094 is base 100 plus the configured percentage of actual gross 99,392. The target was mempool accepted and unconfirmed; later confirmation/drain assertions did not run.
- Batch26 `movement::movement_offboard`: FAIL 16.378 s, run 67572fd5…. Actual customer charge 854 differs from fixed 938. The actual 22-byte BIP84 script requires 122 charged vbytes, compared with 134 for BIP86, under the unchanged 7 sat/vB quote. Original later assertions remain unexecuted. Separate accounting retains the other two 100k VTXOs and distinguishes customer charge from physical miner fee.
- Batch27 `recovery::recovered_wallet_finds_boarded_vtxo`: FAIL 18.294 s, run a17a9bb2…. Actual recovered principal 99,392 cannot satisfy fixed 99,443. Earlier same-ID recovery assertions succeeded; the final send failed. Retained original/recovered records match. This does not establish lost bitcoin. HTTP 500 follows the existing generic error mapping; no undeclared HTTP 400 policy is inferred.
- Batch28 `recovery::recovered_wallet_finds_mixed_origin_vtxos`: FAIL 36.531 s, run 551dd1bb…. Actual recovered total 649,392 cannot satisfy fixed 649,443. Earlier real three-coin identity checks remain distinct from the failed final send. Reconciliation retains exact current records and excludes historical spent inputs from additive balance.
- Batch30 `movement::movement_send_onchain`: FAIL 17.477 s, run 5642a425…. Actual effective debit −50,854 differs from fixed −50,938. Separate retained accounting shows100,000 = 49,146 current change + 50,000 actual destination + 854 charge. The historical 50,854 spent intermediate is not extra principal. The actual target has two retained canonical confirmations; later original assertions and Core 6 are not credited.
- Batch31 `offboard::bark_rejects_offboarding_dust_amount`: FAIL 15.095 s, run 3d1aa01c…. A329-sat request returned a real offboard transaction. That value exceeds the actual BIP84 dust threshold 294; the old test used P2TR 330 − 1. Reconciliation shows800,000 = 798,817 live change + 329 actual output + 854 charge. Pending 1,183 is the same lineage, not additional bitcoin. The target is unconfirmed and the movement is pending; no completed lifecycle is claimed.
- Batch32 `offboard::bark_send_onchain`: FAIL 18.621 s, run 7a6f0ea0…. Actual change 499,146 differs from fixed 499,062. Reconciliation shows800,000 = 499,146 current change + 300,000 actual destination + 854 charge. Historical 300,854 spent intermediate is not additive. Physical miner fee 1,298 is separate. The target has two retained canonical confirmations; later movement/recipient assertions did not run.

Native pending checkpoints stay frozen. Later financial supplements have separate hashes and do not turn an original FAIL into PASS. Every failed run and its raw private evidence remains preserved. Continuation queues overlap: their unstarted counts must not be summed as new executions or unique financial scenarios.

The paired address, public-quote fee, Core-derived board/recovery, actual-script dust and two SDK watchman adaptations are **source only, uncompiled and NOT RUN**. Their new assertions preserve exact money checks; no source adaptation receives execution credit. The ordinary physical PostgreSQL/service-identity backup plus seed-only BIP84 recovery draft has peer review at source ced8c436. It has no new ELF or runtime receipt. It is not universal backup, a Lightning backup, a migration/upgrade test or a production claim. Older restore/conflict and ML designs also remain outside executed credit. This publication includes reports and evidence only; Rust files, patch files and private databases/backups/keys are excluded.

The 200-row ML map remains **198 NOT RUN and 2 bounded FULL/PASS; 145 PARTIAL and 53 NONE**. ML165/166 retain only their actual 327-owner source 7041 / run 63a761b8 geometry and canonical conservation scope. No ordinary regression, source review, backup design or seed-recovery title automatically qualifies a complete ML scenario.

The unit baseline remains 800 PASS / 1 skipped, with its separately executed ignored unit under a distinct receipt. Default integration inventory remains 643 listed / 638 default selected / 5 ignored. Full relevant integration, migrations/upgrades, Tor/network/feature gates, deferred CLN/heavy/scale cases, adapted fixtures and production readiness remain open. Native recipes retain serial execution, zero retries and a default 60-second slow period with terminate-after 3 (180 s). Durations here are observed test times, not isolated throughput measurements.

Historical manifests keep their then-current COVERAGE hashes. COVERAGE and STATUS are accumulators; a new accumulated file is not expected to match an older checkpoint hash. The new publication manifest lists exact historical accumulator exceptions and verifies every other included immutable entry. Earlier receipts and checkpoints remain unchanged.
