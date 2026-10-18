# Seventeenth qualification checkpoint — 2026-10-17 testing track

**242 executions: 227 PASS, 15 FAIL; 227 unique binary/test pairs; 215 nextest invocations; 230 native plus 12 component executions; 453 mixed coverage rows.** This report supersedes the sixteenth 236/447 cutoff. PR20 head `b8513872c8e6a5efee43ee2cb9dbaebe1acf10ed` and logical checkpoints 15/16 remain fixed.

| New row | Exact test within its binary | PASS seconds |
| --- | --- | ---: |
| FT-SERVER-014 | `qualification_conflict_reorg::expiry_actual_partial_conflict_reorg_restores_one_canonical_payment` | 34.690 |
| FT-SERVER-015 | `qualification_failed_swap::expiry_failed_swap_during_restart_pays_its_absent_owner_once` | 53.453 |
| FT-SERVER-016 | `qualification_offboard_restart::registered_offboard_restarts_before_nursery_handoff_and_pays_absent_owner_once` | 52.846 |
| FT-SDK-055 | `lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds` | 39.847 |
| FT-SDK-056 | `exit::watchman_sweeps_funding_tx_during_exit` | 49.248 |
| FT-SDK-057 | `exit::watchman_sweeps_vtxo_chain_during_exit` | 57.051 |

Batch44 parent session 49687 ended with exit 0: six exact unignored cases, all PASS, 287.135 native seconds. The repository `just int-binaries` recipe used default 180-second limits, serial 1 and retries 0; no resource errors or extra executions. Formal client postflight verifies all 17 pins, 16 frozen controls, five archived files/six bodies and relevant helpers unchanged. All six owned groups/SIDs, scoped actors and CLN/plugin candidates are absent.

The tested dirty candidate has HEAD label `4fa8997b`, source manifest `d0fb8640a67d96cc1ffba795fdeae328538513eb3ef86476aab04b30a1d358b3`, bundle `fcba315b96f3a7f7690c9e8e33ec16bcdbda0949c36fddbc304c89e533cdbf95` and runtime metadata `fbc7525ff3180f5dcc73f6d165ec2e63d4c6a8647339e8b0b80d14ddd96e68db`. Root's earlier v9 check failure, mechanical API correction and v9b native PASS remain distinct. The SDK expired-input body is d97; the failed-swap wrapper is unchanged but its actual full file/helper is 237d, rather than old 55bb. Source-only intake snapshots are not relabeled.

The conflict case performs authentic completed-claim/partial-conflict rollback, mempool-wiping restart, rebuild and reconsider while preserving irreversible obligations. C9 allows either legitimate canonical paying disposition; it does not force the old parent to win. Its independent retained-Core audit observes the merged 199,000-sat payout and actual same-block fee child at seven confirmations. The finite graph reconciles **700,000 = 199,000 customer payout + 298,474 ordinary customer change + 198,187 other unspent leaves/unused sponsor + 4,339 board/package miner fees**. Dynamic payee identity and signature validity remain native/source assertions; no global custody or exhaustive unconfirmed exclusion follows. Root J01 has a separate audit and contributes no execution here.

The failed-swap case uses real Accepted→receiver Cancel during owned outage, source-labelled all-Failed/no-preimage proof, absent sender and restart, pre-deadline no-fallback, then rightful 99,000-sat payout/Core 6. The offboard case crosses the genuine durable-registration/nursery-handoff boundary with a scheduler-only PG lock, stops/reaps the original caller, and requires startup publication of exact authorized bytes/Core 6 without duplicate payment. These are bounded native assertions; neither has an additional independent whole-cut/custody audit here.

The SDK Lightning case proves typed refusal of actual expired inputs before dispatch, principal retention, genuine confirmed refresh and successful payment of a distinct hold invoice with actual CLN completion/preimage. The watchman pair proves shallow depth remains nonterminal and eventual registered rightful customer payout/Core 6, with history/current-principal and duplicate-payment checks. The partial-chain fixture explicitly uses change factor 1: 299,000 + 99,000 customer payouts and two 1,000-sat charges. Default factor 2, full fee-child/subsidy custody and default-daemon behavior remain unqualified. These five cases have native assertion evidence, not additional independent offline financial audits.

The immutable initial receipt is `batch44-six-money-v9b-receipt.json` (528a52d3…), formal postflight is `batch44-client-postflight.json` (96e04e64…), and the conflict financial audit is `batch44-case01-conflict-retained-core-reconciliation.json` (cedd6a2b…). `batch44-coverage-append.json` binds exact source/body/helper hashes, runs, row IDs, prefix proof and scope. Initial receipts and the conditional plan remain unchanged.

All 15 earlier FAIL outcomes remain preserved. Batch42 remains 9 PASS/1 FAIL/2 NOT STARTED; batch43 remains a separate quarantine PASS. Mixed/held-round patch 0cf is absent from v9b. The later b657 balance/ownership review and two-target recovery v2 are source-only, uncompiled and NOT RUN; no actual all-balance-bucket or future-protection result is inferred. The unsafe old server-word literal remains held. A root-owned fairness failure is a separate lane pending coin-selection audit, with no execution or fund-loss conclusion added here.

The 200-case ML map stays 198 NOT RUN/two bounded PASS, with 145 PARTIAL/53 NONE/two FULL scopes. No full ML, production or release gate is promoted. Only six LF rows are appended; the full prior 447-row prefix is byte-identical. STATUS, ML maps, public docs, source, controls, Cargo, metadata, runtime and Git are outside this curation.
