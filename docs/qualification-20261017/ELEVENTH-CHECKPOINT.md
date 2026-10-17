# Eleventh testing checkpoint — 2026-10-17

The completed cutoff through batch38 is **216 executions: 202 PASS and 14 preserved FAIL; 212 unique binary/test names; 189 nextest invocations; 204 native cases and 12 components.** COVERAGE has 427 mixed inventory, requirement and execution rows. This adds one native cold-backup PASS to the previous checkpoint. It does not rerun a failed case or qualify a full integration suite, complete ML catalogue or production release.

| Increment | Executions | PASS | FAIL | Native | Components |
|---|---:|---:|---:|---:|---:|
| PR14 tenth checkpoint through batch37 | 215 | 201 | 14 | 203 | 12 |
| Batch38 ordinary cold backup | 1 | 1 | 0 | 1 | 0 |
| **Completed cutoff** | **216** | **202** | **14** | **204** | **12** |

`qualification_cold_backup::qualification_orderly_physical_backup_seed_only_recovery` passed in **28.352 seconds**, run `874f3d92-bea4-4599-955e-be88b2b4c2fd`. The parent verified session 55622 ended with exit 0. One exact repository `just int-binaries` invocation ran with one thread, zero retries and the existing 180-second default termination. The source-authored public receipt exists after all financial assertions; subsequent native service teardown also completed. The curator read that public receipt and safe result summaries, extracted only strict PASS/summary fields from native bytes and recorded their hash. Full native logs, private clusters, backups, wallet databases, identities and seeds were not ingested or published.

This case uses a **new candidate**, separate from the frozen source7041 ordinary regression runs. Source manifest is `87e82448bbad37bc7435de89f92c20192713725c7bca61f53ab66c148218400f`; bundle manifest is `740da075ad58542352fea1b2ce6c93e3827cf02381d9b88f2c99c76f8e4c25b6`. The exact test body is `ced8c436e9106e68bcd916a640aee6ef1007fb7584d2c09f0114b71902009374`, native server ELF `26d701e21e623889f9cba0e0af76d6084e032f9aa8b96dfa9a098e8a471f7387`, and runtime metadata `aa3f342eb4d4a191946550c3e1899afa35be2d96bca6248c6110b14f2e824f63`. Root's captured dirty source is based on `e443d415ee7b0d512203bd56f85e22cdec69d35f`; its source/diff, build and product hashes are authoritative. The actual cached Bark label is `0.7.1-dev (9ea05f9680be2757e875ee81f4546fd24e5b6354)`, explicitly accepted by root as a cached label, not a clean release or matching source-head claim. Actual CLI and native test features include BIP84. No old regression result is silently transferred to this new candidate.

The native fixture starts with 200,000 sat of customer funding and a separate confirmed 100,000-sat watchman sponsor. It creates a real 100,000-sat board, makes the original customer absent, stops the owned services, independently requires PostgreSQL's clean shutdown state, and physically copies the whole cluster plus service identity/configuration into disjoint restored directories. It checks copied hashes before startup, retained public identity/registry and 111 watch-wallet changesets, without resetting the continuing Core or reinitializing/reusing the live original cluster. The private copy manifest remains private; only its hash and the 1,605-file count are public. These are reached native assertions, not an independent audit of every private backup byte by the reviewers.

After restoration, native processing pays the registered customer 99,000 sat with a 1,000-sat fallback charge. It uses an actual same-block fee child and the pre-backup sponsor, without a fresh top-up. A genuinely fresh seed-only BIP84 client scans from genesis, authenticates the receipt, finds the exact payout plus ordinary unboarded change, and spends the complete 198,237-sat input map into a real 197,349-sat destination after an 888-sat drain miner fee. Actual signatures, canonical Core transactions and at least six confirmations for the package/drain are asserted. Immutable stored parent bytes and one allocation coin remain unchanged.

The independent client report then reconstructs the retained canonical Core graph with all owned actors absent and all evidence hashes unchanged. At retained tip 150, the board at height 106 has 45 confirmations; the payout parent and actual same-block anchor child at height 139 have 12; the drain at height 145 has six. It resolves actual prevout amounts/scripts and fees, unique graph spends, and three terminal outputs that remain unspent. The pre-backup height-114 block remains on that same chain.

| Actual accounting | Satoshis |
|---|---:|
| Recovered customer destination | 197,349 |
| Operator retained: parent remainder plus child change | 98,719 |
| Board miner fee | 763 |
| Actual package miner fee | 2,281 |
| Recovery drain miner fee | 888 |
| **Complete initial customer plus sponsor cut** | **300,000** |

Customer accounting is **200,000 = 197,349 + 763 + 888 + 1,000**. Operator accounting is **100,000 + 1,000 = 98,719 + 2,281**. The original and restored copies are the same assets and are counted once. Historical spent backing, the payout and its recovery spend are successive lineage states, not additive balances.

Independent offline Core establishes canonical transaction evidence, amounts, fees, confirmations and terminal unspentness. Configured sweep ownership, restored BDK child-change control, clean PostgreSQL shutdown/copy and seed-only control remain native source/assertion/public-receipt facts; the offline report does not infer those from scripts alone. The native receipt's initial pending-financial-audit state is preserved. A separate supplement links the completed client reconciliation by hash; no historical receipt was rewritten.

This is one **ordinary, orderly, same-candidate physical operator backup and seed-only customer recovery on continuing Core**. It does not establish Lightning backup, Core backup, universal restore, crash/PITR recovery, cross-version upgrade, global wallet solvency, autonomous production behavior or full-suite readiness. The separate candidate gate mapping identifies the bounded ordinary clause of P07 as fully demonstrated; full P07 and P11 remain PARTIAL/OPEN. ML195 gains only candidate positive-branch PARTIAL support because omitted or misaligned signing keys and material Lightning faults were not exercised. No intake or ML map was changed. The 200-row ML map remains 198 NOT RUN and two bounded FULL/PASS, with 145 PARTIAL and 53 NONE. No ML row is automatically promoted by this case. The fee/oracle, expired-Lightning, watchman and conflict source packets retain their own uncompiled/NOT RUN dispositions unless a later exact candidate receipt explicitly changes them.

PR14 was independently verified merged as `3d17b09ef646c3bf74037f796bc002da43685076` at 21:32:22 UTC, preserving its 215-execution/426-row cutoff. This report is a new accumulator snapshot. The publication manifest verifies the safe payload and explicitly records historical STATUS/COVERAGE/FINDINGS hash exceptions. No Rust, patches, raw native logs or private backup data are included. No active native case or additional runtime allocation is claimed at this cutoff.
