# Twenty-second testing checkpoint

The current cutoff is **253 executions: 238 PASS and 15 preserved failed executions; 233 unique binary/test pairs; 226 nextest invocations; 241 native plus 12 component executions; 464 mixed coverage rows**. Three repeated SDK names add no unique pair. The prior 461-row byte prefix and 14 literal CSV FAIL rows remain unchanged; 15 is the aggregate failed-execution count.

Batch48 ran once on immutable capturedC, parent session 79687 terminal 0: expired-input Lightning refusal/refresh/payment 35.152 seconds, funding watch sweep 44.183 and partial-chain watch sweep 55.186; total 134.521 native seconds. Each actual unignored SDK singleton ran once with 660 other tests filtered, default 180 seconds, serial 1 and retries0.

Formal `batch48-backend-postflight.json` verifies 17 actual pins, 19 frozen controls, 12 source files/13 function-helper regions, three known groups/SIDs and scoped actors absent. Other root lanes and global continuous service absence are excluded. Source manifest cdedd, dirty captured HEAD 0496, runtime fca28 and SDK c3bb bind the results to capturedC rather than latest root fixes.

The first case proves genuine expired-input refusal without dispatch, followed by real refresh and a distinct native CLN hold payment; it is not unilateral fallback/refund. The two manual/live watch cases retain the shallow Processing/await-membership/no-history check and partial-chain race, with rightful 399,000 or 299,000/99,000 customer outputs, 1,000 per actual output, canonical Core 6 parents, economic uniqueness and history idempotence. Explicit factor 1 change does not qualify default 2. There is no independent whole CPFP/sponsor-fee/custody cut or all-ten-bucket shallow proof.

New rows FT-SDK-059 through 061 append only current attempts; earlier FT-SDK-055 through 057 on d0fb and all failures remain. Initial receipt effab and frozen safe export 13c retain pending-at-creation labels; formal 906f and independent safe-control audit e0fd are additive completion evidence. The frozen conditional plan's “15 FAIL rows” wording is corrected here to 14 literal CSV rows / 15 aggregate failed executions.

Separate H30 source-only positive tiny-group case 18 is committed as 57f5 from fixed PR25 head 39ca; source db2e remains unregistered, uncompiled and NOT RUN. All 150 protected prior Rust files are unchanged. The one-line product proposal remains separate; no native failure or repair success is inferred. The client owns new H32 source 19 authoring only; it is uncompiled/unrun and excluded from this terminal result and pending source publication.

No own runtime is active; allocation 48 is consumed. Fullsuite, fullML, custody, defaultdaemon/current-root qualification and production/release claims remain open. No ML map or source was changed by this curation. Fixed PR25/report21 at 250/461 and earlier snapshots remain immutable. Exact hashes and preservation checks are recorded in `batch48-coverage-append.json`.
