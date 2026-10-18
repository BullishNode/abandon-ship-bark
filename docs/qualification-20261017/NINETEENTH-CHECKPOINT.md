# Nineteenth qualification checkpoint — 2026-10-17 testing track

**248 executions: 233 PASS, 15 FAIL; 231 unique binary/test pairs; 221 nextest invocations; 236 native plus 12 component executions; 459 mixed coverage rows.** This supersedes the eighteenth 245/456 cutoff. Fixed report18/PR22 and every historical outcome remain unchanged.

| New row | Exact barkd test | PASS seconds |
| --- | --- | ---: |
| FT-BARKD-044 | `recovery::recovered_wallet_skips_exited_vtxo` | 31.134 |
| FT-BARKD-045 | `recovery::recovered_wallet_finds_vtxo_only_within_the_gap_limit` | 31.595 |
| FT-BARKD-046 | `recovery::recovered_wallet_finds_vtxo_beyond_the_gap_limit_via_xpub` | 29.929 |

Batch47 parent session 28414 ended with exit 0. Three exact unignored tests passed in 92.658 native seconds, with default 180-second limits, serial 1 and retries 0. Formal backend postflight verifies genuine native contexts, all 17 pins and 15 frozen paths unchanged, exact source/body/helper bindings, and absence of known groups/SIDs 145878, 146890 and 148754 plus scoped actors. Available shared memory was 817,356,800 bytes. Other root lanes are excluded; no global service-absence claim is made.

Actual dirty v10 source manifest is `3e0437f1d1c89e554a38d7756f2d89238732cc368882f7b3f382b799487c0f33`, archive `03686b6d3e567ee42ebaa6bfc1f8278cafb3e185e9f92c839e52d7af493aba3d`, runtime metadata `5014cd46046da81a36e19a4f82e4900fc3ae1c90c0c688a72a62fcf495dd918a`, and recovery file `97118c62993592f80fc167a1346168389c0386dae88a762cc99205ee417567b4`. Captured HEAD 783c0c5c is a label, not a later-source qualification. Full per-function/body, helper, ELF, bundle and root/parent admission bindings are retained in the source proof and formal postflight.

The first case proves recovered exact-ID Exited classification after genuine unilateral-exit progression; it does not assert final claimed payment or drain. With server xpub recovery disabled, the second proves an empty gap-50 complete list and exact-ID discovery at gap 100. The third proves exact-ID discovery via native test xpub scanning despite local gap 50, and that the next address differs from the recovered old address. It does not prove an exact next index. The discovery cases do not assert recovered Spendable state or a successful recovered payment. Original actors remain alive.

There is no independent retained-Core/wallet financial reconstruction, full-balance or fee conservation, Core 6 recovery, absent-original/cold-backup, current V11 fix, P07, full ML, release or production credit from these cases. All 15 prior failed executions and the root V11a/V11b compile failures remain historical; no ML mapping changes. Mixed CSV rows do not correspond one-to-one with aggregate historical executions.

Evidence: [initial native receipt](batch47-ordinary-recovery-three-v10-receipt.json), [additive formal postflight](batch47-backend-postflight.json), [append receipt](batch47-coverage-append.json), [source peer](v15-proposed-ordinary-barkd-regressions-peer-review.json), and [safe control export](checkpoint19-proposed-safe/batch47-control/EXPORT.json). The initial receipt's pending-at-creation label is preserved; formal resolution is additive.

Only three LF rows were appended, with the entire prior 456-row byte prefix and existing FAIL rows unchanged. Source proposals, conditional plans and all frozen receipts remain immutable. No runtime is active for batch47. Its formal completion satisfies the prior-batch condition for the separate root-allocated P07 v11c candidate; final new controls and fresh parent admission remain required before that single run. P07 has no native execution or result at this cutoff.
