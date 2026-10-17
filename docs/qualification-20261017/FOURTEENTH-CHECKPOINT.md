# Fourteenth qualification checkpoint — 2026-10-17

The testing track has completed **225 test executions: 211 PASS and 14 preserved FAIL**, covering 219 unique `(binary, test name)` pairs in 198 nextest invocations. These comprise 213 native executions and 12 component executions. `COVERAGE.csv` has 436 mixed inventory, requirement and execution rows; it is not a count of financial scenarios. No own runtime is active, and the two-case allocation is consumed.

This checkpoint adds two explicitly allocated ignored server cases. Each ran once, serially, through the repository `just int-binaries` recipe with its exact literal, `--run-ignored only`, zero retries and the existing `fallback-scale` profile. The authorized outer timeout was **1,740 seconds**; the source's **120-second lookup poll per leg** was unchanged. Both actual logs confirm the profile and exactly one test run and passed, with 566 other tests skipped. The prior 434 coverage rows, all original failures and the fixed PR17 snapshot remain unchanged.

| Coverage ID | Exact server test name | Native seconds | Result |
| --- | --- | ---: | --- |
| FT-SERVER-012 | `expiry_owner_load::expiry_owner_live_funded_100_registrations` | 47.490 | PASS |
| FT-SERVER-013 | `expiry_owner_load::expiry_owner_live_funded_1000_registrations` | 80.781 | PASS |

The 100 and 1,000 values count initial authenticated registrations. **Each cohort funds two accounts and three board roots.** Live native daemons discover and publish two rightful owner payouts under bounded registration pressure, while an independent absent control receives its payout. The original owner wallet is held idle with manual sync during the first leg, makes a genuine late board between legs, and is dropped for the second leg. No manual scanner or queue calls drive discovery. These conditions do not establish 100/1,000 funded-user throughput, general fairness, sustained arrivals, high-index completeness or production capacity.

| Registrations | Owner child index | Post-sync lookup poll seconds | Phase elapsed after confirmations, seconds |
| ---: | ---: | ---: | ---: |
| 100 | 80 | 6.289 | 9.584 |
| 100 | 1 | 3.628 | 6.809 |
| 1,000 | 80 | 24.683 | 27.984 |
| 1,000 | 1 | 7.674 | 11.059 |

The lookup clock begins after expiry mining and daemon synchronization. The proof's phase elapsed value is sampled after confirmation work; it is a different measurement. The authorized 1,740-second outer profile does not change the 120-second lookup assertion. The workload bounds include scan chunks of 32, two workers, at most 16 additional records, and at most 2,048 pressure requests per leg. Exact public numeric proofs and progress samples are included.

Independent client postflight verifies both ignored singleton selections, all 13 actual runtime pins, the ten original static controls and twelve parent admission hashes unchanged. Both owned process groups and all scoped actors are absent. No native retry, resource error or new compilation occurred. Postflight reports 860,446,720 bytes of available shared memory.

The independent retained-Core audit reconstructs canonical ancestry, block-header hashes and transaction Merkle roots from preserved blocks and the final tip at height 177. For **each** cohort, ten graph transactions and nine terminal unspent leaves reconcile as:

`1,000,000 = 495,983 seed-drain output + 99,000 control payout + 395,394 other observed unspent leaves + 9,623 actual miner fees`.

Actual funding is owner 500,000 plus control 200,000 plus three 100,000-sat sponsors. Miner fees are 2,289 for three boards, 6,843 for three actual same-block fee children, and 491 for the seed drain. Each 100,000-sat board liability pays its rightful 99,000-sat output and 1,000-sat customer charge. Both owner payouts are spent once by the confirmed drain; the control payout remains unspent. All ten transactions have at least six confirmations. Two original sponsors are consumed and one remains unused; a later fee child legitimately spends confirmed earlier child change. The audit follows this internal edge without double counting it.

The initial offline decoder assumed each child consumed an original 100,000-sat sponsor and nominal total funding was 900,000 sats. Actual prevouts corrected those assumptions. The source-admission report's nominal funding wording is retained as historical evidence and explicitly superseded by the final Core audit. These were offline parser assertions, not native-test failures; no native test was retried.

Block data proves exact canonical scripts, amounts, prevouts, fees, unique spends, confirmations and this finite physical cut. It does not prove private-key control or operator custody of all other leaves. Recipient identity, seed control, registry authentication, mailbox behavior and background-task scheduling remain native source/assertion/public-proof evidence. Fresh-seed recovery uses a live Ark endpoint and its drain includes ordinary funding change; it does not establish Ark-offline restore. No mempool file was read and no new Core consensus validation was performed.

The initial receipt `batch41-live-funded-v7-receipt.json` remains immutable with both audits marked pending at creation. `batch41-client-postflight-supplement.json` and `batch41-financial-supplement.json` resolve those states additively through exact hashes. Coverage's historical pending-audit text is likewise preserved. Neither supplement adds execution credit.

The source manifest is `d05d3d5c7a24584fc9893f6601d9c0b243784f9bb16602feca699b4e532c3db0`; bundle manifest `04b05a11e01b3b0af46e812cf3cb94d053d2a3bd79d4c7b25ac234948fa3eb33`; runtime metadata `748a2f9fc21d008ad3a7bdc166363c04b524461b3339d084c2b9481a37a08b1d`; server test ELF `c0078f00a577b2caa55c2ac09ce26289d7ada00c5872eaf02698da9dd94974bd`. The exact archived source body is `a7361097f02126425342e575b95f9c681dc853fd2496197c11cd8fd98dce34a9`. Product version labels identify captured head `759f12c1f0c7586c597557f422374415f330b9f8`; the immutable manifests, archive and file hashes identify the tested candidate, rather than a current branch label.

The 200-case money-loss map is unchanged: 198 NOT RUN and two bounded PASS, with 145 PARTIAL, 53 NONE and two FULL evidence scopes. These cohorts cause no full ML or readiness-gate promotion. The full relevant integration suite, production qualification, general backup coverage, complete operator custody and subsidy accounting remain open.

This packet is prepared externally for a new report branch, `work/fresh-test-report-checkpoint14-20261017`, preserving PR17 head `e36fff479b8c9b29452677316becfbe014172f3d` and its fixed 223-execution/434-row snapshot. Latest verified merged publication remains PR15, merge `a2961be8376fe236b9a6788e831884e05d425f0e`. The parent owns branch creation and publication. Only safe public proofs, control scripts, numeric summaries, immutable hashes and reports are proposed; no Rust, patches, full native logs, wallet or service databases, backups, identities, credentials or seeds are included.
