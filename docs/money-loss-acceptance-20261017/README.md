# Money-loss acceptance programme

The owner requires tests for all 200 catalogue scenarios. The full upstream regression suite and public Signet simulation remain separate required work. The catalogue includes 81 Lightning scenarios. Its classifications are 4 observed failures, 58 source-supported risks, 134 hypotheses and 4 historical regression risks. Observed failures do not establish permanent customer loss.

Use [ACCEPTANCE.csv](ACCEPTANCE.csv) as the scenario acceptance ledger. Preserve ML001 through ML200. Each row starts as `UNASSESSED`. This state means that the full scenario oracle has not yet been reconciled with retained evidence. It does not mean that the project has no tests. Existing receipts retain their original results and limits.

The [research snapshot](research/README.md), [complete catalogue](research/MONEY_LOSS_SCENARIOS.md) and [priorities](research/PRIORITIES.md) are unchanged source material. [INTAKE.json](INTAKE.json) records their hashes and the research revision. References in the original research can point to workspace files outside this repository. Preserve these references as provenance. New evidence must use durable receipt paths and exact candidate pins.

The first reconciled [progress report](PROGRESS.json) records two complete bounded passes, 145 partial mappings and 53 scenarios without executed coverage. The two passes are ML165 and ML166 on captured source `70417933`, run `63a761b8`. The coordinator reviewed their full oracles against the actual multilevel fixture, canonical package order, 327-owner payout CSV and complete DAG conservation. This result does not qualify other shapes, restart, reorg or sustained load. The received independent evidence map is retained separately. `INTAKE.json` preserves the original intake state; `PROGRESS.json` reports the current ledger state.

## Work ownership

- The backend owner handles settlement, attribution, dispatch, payout decisions, splitting and chain state.
- The client owner handles protection, history, balances, external BIP84 integration and restoration.
- The simulation owner handles public Signet lifecycle campaigns and stock Noah backend compatibility. No mobile wallet build is required.
- The independent testing coordinator maps all 200 scenarios to complete or partial existing coverage, authors missing defensive tests and records actual executions.
- The coordinator integrates reviewed changes, allocates build resources, resolves cross-component gaps and publishes results.

The ledger names an implementation lane and a test owner for each scenario. These are work allocations. They do not assert that the scenario is reachable or that a defect exists.

## First work

1. Establish internal-Lightning closure with both clients absent: ML029 through ML031. Normal online completion is insufficient. Separate newly prevented states from historical unfinished obligations.
2. Establish source-specific dispatch terminal evidence: ML006 through ML009 and ML015 through ML018. Preserve the stock CLN failure separately from the typed-adapter result.
3. Establish final-recipient continuity through interrupted claim materialization and recovery. Include ML036 through ML040, ML067 and ML180.
4. Establish client protection on unsupported server `Spent` reports: ML181 through ML184. A history or balance display test does not establish retained emergency-exit protection.
5. Close actual strict Noah invoice compatibility, then test authentic offline receive and payout. Native Lightning receive does not establish stock Noah compatibility.
6. Complete physical split, funding, concurrency, reorg and authentic backup/restore coverage. Include sustained funded attribution and oldest unpaid entitlement measurements.

Continue the allocated upstream regressions while these tests are prepared. Do not replace existing test scopes with catalogue row counts.

## Acceptance evidence

Before a row becomes `PASS`, record the exact test names, source manifest, executable/plugin/config manifest, run ID and retained receipt. Review the full scenario oracle. Partial coverage remains `PARTIAL`. A source fix without an executed test remains `IMPLEMENTED_UNRUN`. Use `FAIL`, `BLOCKED` or `NOT_REPRODUCIBLE` with an explicit reason when applicable. A documented information limit or unreachable precondition is not a test pass.

For monetary scenarios, establish the independent initial obligations and expected payees. Verify actual amounts, scripts, source-specific Lightning outcomes and canonical chain outpoints. Check all relevant branches for duplicate payment. Account for customer fees, operator subsidy and miner fees separately. Where the scenario requires recovery, demonstrate discovery from the specified backup or seed and an actual spend. Where it requires eventual progress, state and test its bound.

Use regtest for controlled races, reorgs, fee pressure and restart tests. Use public Signet for simulation and real lifecycle observations. Public Signet block timing is outside our control. A mempool transaction, a database `Paid` label or a test process exit alone is insufficient monetary evidence. Do not change financial state through SQL to create a pass.

Preserve earlier failures and label later results with their changed conditions. The new owner-scan benchmark passed on captured source `70417933`; it supplements the earlier timeout. It does not erase it or establish funded production capacity. The corrected 327-payee split proves its actual shape. It does not establish arbitrary-shape recovery.

## Completion boundary

Report all 200 row states, with explicit unresolved items. Report upstream regressions and Signet campaigns separately. Every scenario must receive a test or a documented, reviewed prerequisite or reachability limit. Do not claim 200 successful scenarios until 200 full scenario oracles have passed. Production readiness requires a separate assessment of the remaining defects, recovery guarantees, resource budgets and operational evidence.
