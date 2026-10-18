# Regtest qualification checkpoint 22

Three repeated SDK regressions passed on 2026-10-18 using the immutable captured-C runtime. They add three executions and zero unique binary/test pairs. After append-only curation, the testing track has 253 executions: 238 PASS and 15 preserved FAIL, across 233 unique binary/test pairs. The 464 coverage rows also contain source inventory and other records; they are not an execution count.

| Exact SDK selector | Native seconds | Result |
| --- | ---: | --- |
| `lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds` | 35.152 | PASS |
| `exit::watchman_sweeps_funding_tx_during_exit` | 44.183 | PASS |
| `exit::watchman_sweeps_vtxo_chain_during_exit` | 55.186 | PASS |

Each invocation selected one unignored test, used a 180-second timeout and zero retries, and filtered 660 other tests. Filtered tests receive no execution credit. The three tests ran serially; their combined native time was 134.521 seconds and controller time was 137.725 seconds. Parent execution session 79687 exited with code 0. The recorded process groups were 173577, 174944 and 175188.

The first test proves refusal of a new Lightning payment from expired inputs followed by an actual refresh and successful hold payment. The watchman tests assert actual signed payout parents, confirmed customer outputs, ledger ownership and absence of duplicate economic outputs within their driven scenarios. They use manual clients and explicit watchman triggers. They do not qualify default daemon behavior, reorg recovery, a complete fee-child/sponsor conservation graph or global custody.

The captured source manifest is `cdedd62efcf1e4f131f8378017cae5facd729d36995419a4ff2e36d0456bf635`; runtime metadata is `fca28b4dceef4d667ff0bae1351a76c152fb3edcdd3ca275d69551220991276a`. These results do not qualify a later root candidate or silently transfer to a newer Git head. Independent postflight verified all 17 pinned runtime files, 19 parent admission hashes, 12 archived source files and 13 function/helper regions. Recorded process groups and scoped processes were absent after termination.

The public control export is under `batch48-control/`. Its `native-initial-receipt.safe.json` preserves the initial receipt's pending audit status at creation; the separate `batch48-backend-postflight.json` and export audit resolve that status additively. `EXPORT.json` describes each exact copy or command-free projection and records original hashes. Raw native logs, test data, wallets, databases, keys and configuration contents are excluded. No independent financial receipt was invented for these tests.

The same checkpoint carries the H30 grouped-payout fixture, `qualification_tiny_payout_grouping::three_sub_fee_leaves_pay_one_registered_owner_once`, committed as `57f5d32943cc0b1a30b86164e520aa68aa8577c3`. Current source SHA-256 is `db2e2356b563bb56140f9de3c133df7bdf738f97f8dc70001256057f7194662c`. It requires three genuine 500-sat leaves to share one 500-sat customer output and one 1,000-sat output charge, while retaining strict SDK receipt and history checks for fee-only contributors. Its selected 300,000-sat root/sponsor cut excludes ordinary board change and the upfront board miner fee explicitly.

H30 is source reviewed, unregistered, uncompiled and NOT RUN. A source-supported server receipt classification concern remains distinct from observed native failures. The product-owner's isolated classifier proposal is not applied by this testing branch. Historical v1 source and its review are preserved; the v2 packet explicitly corrects missing macro imports. Source publication adds no PASS, FAIL, unique-scenario or full legacy-case credit.

H32 implementation is proceeding separately under source-only ownership. It must first prove actual dust forfeiture and accepted history in the original wallet, then use an independently funded donor for a later receipt to that same account and payout script. Its source, compilation and execution are outside this checkpoint's completed runtime evidence.

Earlier failures and frozen checkpoint payloads remain preserved. Production readiness, complete legacy money-loss coverage, sustained scale and remaining Lightning recovery gates are not established by this checkpoint.
