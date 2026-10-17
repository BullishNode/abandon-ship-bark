# Supplemental regression receipts for the frozen200 map

This is a supporting receipt cross-reference, not a new200-case map,200 new tests or ML acceptance results. Input is the completed source7041 SDK45 plus CLI2 named tests. Active batch21’s75 names are excluded. Exactly **2 existing matched-test tokens link to2 completed SDK names and2 ML rows**; the remaining45 names receive no invented match. No CLI2 name matched the frozen intake. No title, keyword or thematic inference was used: each link requires exact token equality in `matched_test_names`; ML125/128 full research A–K also explicitly references `testing/tests/bark_sdk/offboard.rs` at the corresponding function.

Frozen PR11 intake byte hashes before/after this write:

- ML-COVERAGE.csv: `305746845f6ee9526d4e862def7154b9e70803c38af37ab01f2b68f00b854310`
- ML-COVERAGE-SUMMARY.json: `16ade43ac1e8d112d2d7b096fb2cf1a3c332f90bc61e0bdfa16bbe23a8b9fb6b`

Neither file was edited. ML125 andML128 remain execution_status NOT RUN and evidence_scope PARTIAL. No FULL status or research source is changed.

Completed-receipt inputs (not all47 logs re-audited here; the2 linked logs/results were independently rechecked):

- batch18-sdk-upstream-normal-01-receipt.json: `83966063ecfcd732bcb6705c5da66a3ab8a42d3234f007ec3cc8d1325a46477c`
- batch19-sdk-upstream-normal-final-receipt.json: `380585c9cf2769db2bf2b8e3417452eefce0c22188b3d10626cb324ca8355820`
- batch20-upstream-bark-first-two-receipt.json: `1908c66929a3834f8b661e1ff2f93f3f2d2040bca6707fba93da3876f80614a3`

The linked2 names were genuine PASS executions in the original SDK41 prefix before its resource-preflight stop at unstarted case29. That stop and the separately admitted13-case resume remain in the batch19 final receipt; neither link is a retry or a changed candidate. Linked runs use the immutable source manifest70417933 and SDK ELFaaf60430, default180-second threshold, threads1/retries0. Both functions have no version gate. Their complete source file and exact function hashes were independently compared against source7041 archive and pinned upstreambfbe bytes; each raw result/log hash, exact run ID, one-selected-test PASS duration and summary matched.

ML125: FT-SDK-010, `offboard::offboard_recovers_lost_finish_by_adopting_chain_tx`, run88fab39d-64cd-4cf8-b3ee-36cae0cd83bf,21.086s. Actual800k offboard with successfully processed finish response lost; after session expiry the original DB adopts the existing broadcast transaction and one Core block completes it. The asserted destination receipt is nonzero, not exact net/Core6. There is no new restored DB or fallback/reorg/dual-candidate economic oracle. This upgrades only the supporting fact that the already-named ordinary adoption test actually ran on source7041; it does not qualify full ML125.

ML128: FT-SDK-007, `offboard::offboard_cancels_when_fees_change_after_lost_session`, run2801699f-44b3-4aa7-bfda-7823ba90a141,11.204s. The proxy drops an unfinished finish REQUEST, so the old checkpoint contains an unsigned session transaction. Actual native restart changes fee conditions; cancellation releases800k, and a subsequent valid offboard confirms1block with799756sat received. This does not establish the ML scenario’s genuinely signed old transaction surviving lost finishREPLY or fallback/reorg concurrency; exact authorization/old-new canonical-output closure remains missing.

The earlier batch05 server identical-finish replay evidence is retained in the frozen map under its earlier dirty source/product conditions. It is not replaced, relabelled source7041 or silently merged with these SDK receipts. No ordinary unit/core pass or source-only ML126/190 fixture is credited. CSV per-link missing-full-oracle text is explicit; no native execution, source/canonical/STATUS/COVERAGE/Git mutation was performed to prepare this supplement.
