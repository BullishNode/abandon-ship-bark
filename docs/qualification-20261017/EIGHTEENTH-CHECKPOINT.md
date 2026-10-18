# Eighteenth qualification checkpoint — 2026-10-17 testing track

**245 executions: 230 PASS, 15 FAIL; 228 unique binary/test pairs; 218 nextest invocations; 233 native plus 12 component executions; 456 mixed coverage rows.** This supersedes the seventeenth 242/453 cutoff. Fixed PR21/report17 payloads and every original outcome remain unchanged.

| New row | Exact barkd test | PASS seconds | New unique pair |
| --- | --- | ---: | ---: |
| FT-BARKD-041 | `recovery::recovered_wallet_finds_round_vtxo` | 34.026 | 1 |
| FT-BARKD-042 | `recovery::recovered_wallet_finds_mixed_origin_vtxos` | 37.976 | 0 |
| FT-BARKD-043 | `recovery::recovered_wallet_quarantines_invalid_spent_vtxos` | 22.385 | 0 |

Batch45 parent session 17820 ended with exit 0. Three exact unignored cases passed in 94.387 native seconds, with default 180-second limits, serial 1 and retries 0. Formal backend postflight verifies genuine native contexts after version gating, all 17 pins and 13 frozen paths unchanged, exact source/body/helper bindings, and absent groups/SIDs 143297, 143753 and 144233 plus scoped argv/cwd/FD actors. Available shared memory was 822,743,040 bytes. Other root lanes are excluded; no global CLN absence is claimed or required.

Actual dirty v10 source is `3e0437f1d1c89e554a38d7756f2d89238732cc368882f7b3f382b799487c0f33`, archive `03686b6d3e567ee42ebaa6bfc1f8278cafb3e185e9f92c839e52d7af493aba3d`, bundle `db58ea456c1c5e545496514274e62e9d45436ec0503ae3a5016c454ba3b8352c`, runtime metadata `5014cd46046da81a36e19a4f82e4900fc3ae1c90c0c688a72a62fcf495dd918a`. Product labels use captured HEAD 783c0c5c; later Git commits do not relabel these artifacts. Recovery file97118 contains round body0ece3fbc, mixed3ee4d770 and unchanged quarantine0f174/shared helperb81d. Queued documentation8fb was not captured or applied.

The two v2 cases now actually reach and pass their public all-ten-balance checks: the exact recovered current cut appears in spendable plus nine zero monetary buckets before a genuine full recipient payment, and all ten monetary fields are zero afterward. Both checkpoints require an empty persisted exit-status list. Current-cut IDs, amounts and anchors match the original; recipient sum/anchor partition conserves that cut; discharged current inputs are Spent. Only the exact known cross-anchor historical board remains Locked(None,None), with unchanged full public information and signed body. There is no arbitrary Locked exemption or forced per-VTXO awaiting-settlement flag false.

The unchanged quarantine case separately rejects invalid signed live successors, retains historical quarantine and checks zero monetary balances. Its genuine valid same-anchor ordinary-Arkoor control validates source/successor bodies against the actual Core anchor, recovers exact current change, recognizes its ancestor Spent and conserves the actual board amount as recipient plus change. This is a repeated name/body on a new candidate, not a new unique test.

These are bounded native public-state/recipient assertions. Original daemons remain alive and the Ark endpoint is live; no absent-original, Ark-offline or physical backup is established. Point-in-time empty exit statuses do not prove future act-by or automatic/default-daemon protection. There is no added independent Core/SQLite money reconstruction, cryptographic audit, Core 6, global wallet custody or full ML result.

The immutable initial receipt `batch45-recovery-three-v10-receipt.json` SHA256 `5d0bdfdef75d97130de60e06c82b8933cc32fe8eac05a46f29e5027131692253` retains its pending-at-creation postflight label. Completed `batch45-client-postflight.json` SHA256 `97f378ee2371aa37bfa33bf239c20d9a7d59aec74eb1c7d9f1d54a8a80d84c8e` resolves integrity/actor absence additively through `batch45-coverage-append.json`. Source proof c7af preserves its earlier source-only state; parent metadata proof e5a0e100 and actual allocation0b6453 provide separate later bindings. The initial bare caller-shell/probe exit127 events preceded native execution and remain distinct from native FAIL/retry counts.

Both earlier mixed-origin failures (FT-BARKD-022 and 039) remain FAIL. The held original round source remains historically NOT RUN; this adapted v2 result does not qualify that old body. Earlier quarantine FT-BARKD-040 remains a separate PASS. Batch42 remains 9 PASS/1 FAIL/2 NOT STARTED at its historical cutoff, and the unsafe old server-word literal stays held. All 15 FAIL outcomes and prior source-only reviews remain immutable; later v10 PASS is new evidence rather than a rewrite.

The 200-case ML map remains 198 NOT RUN/two bounded PASS, with 145 PARTIAL/53 NONE/two FULL scopes. No ML, production or release gate is promoted. Three LF rows are appended, preserving every byte of the prior 453-row prefix. STATUS, ML maps, public docs, source, controls, metadata, Cargo, runtime and Git are outside this curation.
