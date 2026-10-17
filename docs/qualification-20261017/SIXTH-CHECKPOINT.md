# Corrected native multilevel split qualification, 2026-10-17

The corrected real-size fixture **passed in 164.064 seconds**, run `63a761b8-7f82-4f73-9bd5-3433e6912403`, inside its unchanged 600-second whole-case deadline. Exactly one ignored integration case was selected, retries were disabled, and a new private state/store was used. The original batch12 failure remains unchanged.

All **327 rightful owners received exactly one 4,000-sat payout** from 5,000 sat gross and one 1,000-sat output fee. Each output's actual Core script/value, registered mailbox and ownership key, durable paying transaction and canonical block were checked. The minimum observed customer confirmation count was seven; the oracle required at least six. The owner-output uniqueness search covered the full canonical graph, including sponsor children.

Preparation completed 327 payee registrations, 154 genuine native cosign/output-registration pairs and 327 native mailbox posts. The source wallet added its own registration, giving 328 registry rows. The fixture authenticated **635 distinct signed path transactions**, including the board exit. After preparation the original wallet/client handles were dropped; no online recipient supplied recovery or splitting assistance.

Both required actual oversized physical levels progressed automatically: the original board backing represented 327 recipients and the heavy checkpoint represented 324. The 324 P2WPKH outputs alone serialize to 10,044 bytes before transaction overhead. Both exact signed split instructions were observed. The test permits additional legitimate native substeps rather than requiring an exact global request count; this run observed two matching instructions.

Actual chain evidence contained **seven parent/sponsor packages and 14 canonical transactions**. Every parent was version3, zero-fee and within 10,000 vB; the largest observed parent was 7,828 vB. Each actual same-block P2A sponsor was version3, 153 vB, within the 1,000-vB bound, and paid a positive independently computed miner fee. Actual previous outputs, physical-level ordering, canonical blocks and six-confirmation graph history were checked.

The confirmed DAG conserved **2,835,000 sat external inputs = 2,746,179 sat remaining outputs + 88,821 sat actual miner fees**. Customer outputs totalled **1,308,000 sat**; customer payout-output fees totalled 327,000 sat. Remaining outputs include customer payouts and operator/watchman change; declared funding not consumed by this graph is outside this equation. The DAG rejected duplicate spends, undeclared external inputs, missing/spent remaining outputs and any script/value mismatch. Output fees and miner fees are distinct quantities; no unsupported subsidy amount is inferred.

## Candidate and build provenance

This run used the exact frozen working source at HEAD label `814d7b0deb6a5f2a6758d34ca4313589f948f4e3` plus the coordinator's reviewed three-file owner-scan batch/fence patch. It is a **dirty source capture**, not clean-commit qualification. The original patch label was `08d985e8`; the actual captured binary Git diff SHA256 is `73821aa9ad1a1315d59d1a22de8e20d2a6b7e6d62bafc92b7f1c09988cb4d13b`.

All 1,156 tracked source files, HEAD and the dirty diff were verified unchanged before/after every build phase and at final lease release. Nested untracked worktrees were excluded. Corrected fixture SHA256 is `1d708bd6e4fa9bc80ff5b95d1acb52fc0269278dbb2c5b186107c6e023785366`.

| Artifact | SHA256 |
| --- | --- |
| Exact source manifest | `704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06` |
| Paired candidate manifest, including exact skipped-unit audit | `85149de281996da9a6c856e520de8b20247c9dd4b6912a0ba988543ccf6661a6` |
| Immutable SDK/server metadata used by the split run | `cf273b091bee345a3d66027abe091ac5951ea04f8c6bb06053e78e4167d12a90` |
| captaind | `175dc56e2177cbda495e2cb5c56bae2061d0590dc2481ddb40a944e68e5dd0fb` |
| watchmand | `4d1d52778032aed837bfc4e28ddcf637458837635a62ad3f6436be3adc4311d5` |
| bark | `d971db9547a3bb6c8ebd0413404ed886d3d1157f233c259ab905945ad0a96444` |
| barkd | `5194c687beb3e17a637dcb2754402daca5d6a1a90174a8c12d7091706500e114` |
| Native server test binary | `cd9c13c01726ce785398d254ab05d8da3d28773ddba502150a0e4a712ff41a08` |
| Native SDK test binary | `aaf60430b2517f0b4151ad3f869ec81339ac4a65cd8687884d13e505e57eb2d9` |

Required `just checks` passed in 58.909 seconds. The full unit phase passed **800 tests with one explicitly ignored test**, nextest execution12.356s/run `825f6b81-f598-456b-ab42-bb844161a470`; including compilation the phase took100.200s. All four products built in147.994s, with BIP84 and embedded web enabled for both CLI products. Paired SDK/server BIP84 metadata built in133.348s. These phases used the sole coordinator-allocated Cargo writer and existing cached Nix environment/shared target.

The skipped unit was `ark-lib arkoor::test::isolate_dust_fuzz`, explicitly ignored with reason `slow; run on demand with --run-ignored all`. The same ignore annotation was verified in the prior c60c source capture. After separate coordinator authorization, an exact-literal selection ran **only that unit** and passed in **15.454s**, run `09ff9f23-e14f-4860-b005-70fdceecfcc8`. Its 10,000 amount placements are assertions within one unit test, not financial scenarios or payments. The original 800-pass/one-ignored receipt remains intact; the supplemental receipt closes that unit-execution gap separately.

## Broader immutable inventory, not a suite result

After the split case started, a separately authorized37.688s metadata build captured bark, barkd and core test binaries from the same unchanged source. It ran concurrently with part of the split case, so the case duration is not an isolated throughput measurement. All five test binaries were then enumerated without executing another integration case.

| Binary | Listed names | Default-selected | Explicitly ignored |
| --- | ---: | ---: | ---: |
| bark | 202 | 201 | 1 |
| barkd | 78 | 77 | 1 |
| core | 16 | 16 | 0 |
| bark-sdk | 77 | 77 | 0 |
| server | 270 | 267 | 3 |
| Total | **643** | **638** | **5** |

Immutable five-binary metadata SHA256 is `c22e785532b29bfcda024483c669e2ade5b8e96d0038dac9309df4bed51e8ea4`; actual nextest listing SHA256 is `16adbb314708184af6aefbbd5248656fb6ac419f2b5b71ae34e5532936f368e8`. The sanitized CSV and summary retain every exact name and flag as `INVENTORY_ONLY`. Nextest selection does not guarantee runtime version/backend/feature gates execute behavior. Migrations, upgrades and Tor remain separate explicit gates. No full integration-suite pass follows from this inventory or the unit results.

## Preserved failures and limits

The original stock-plugin no-part timeout and first oversized fixture attempt remain failed executions. The corrected split pass is the same named test on a separately pinned candidate, so it adds no unique-name credit. The track totals **28 executions: 26 PASS, two FAIL, 24 unique names and16nextest invocations**;26 executions are native and two are mailbox components. Full-unit and supplemental-unit results remain separate from those counts. All80original catalogue IDs are preserved; complete legacy qualification is not established.

Fanout4, depth100, genuine327owners, 5.2M regtest sat reserved funding, real10,000-vB/1,000-vB limits and the600s whole-case deadline were retained. The harness uses explicitly recorded1s reaction/sweep intervals; production timer defaults remain separately stated. This test does not qualify production scale, sustained pressure, reorg/restart/backup recovery, Noah/LNURL or public Signet. No CLN daemon was needed by this split case, and no Nix derivation was built.

All compiler/native work allocated to this track is terminal. All nine product/test hashes were reverified, and process inspection found no remaining Cargo/rustc/nextest writer or owned runtime. The coordinator now owns scheduling of PG/race checks and the unchanged120s scale benchmark; this track will not overlap another compiler or native runtime with that timing gate. Both failed raw evidence and the latest successful money-path evidence remain preserved privately. See `owner-scan-split-v1-completion-handoff.json`, per-run receipts and the financial summary for exact paths and commands.
