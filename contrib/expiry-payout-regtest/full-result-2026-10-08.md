# Native shipping-image result

`results/2026-10-08-native-full.json`: **27 PASS / 0 FAIL**, 4708.423 seconds,
started 2026-10-08 04:51:13 UTC. Image `expiry-d-b2c19a215f3c`, ID
`sha256:6314a4abcf7e1bfeeee3b034808f859ac225242cae7a239c382e6b6fa57c220f`.
The report pins the source commit, image, helper binaries and scenario hashes.
Exact script snapshots and individual logs are retained in the private evidence
directory `native-selected-fee-full2/`.

The ten liquidity cycles each reconcile actual capital without rounds-wallet
top-ups: starting 100,352,222 sat, ending 100,347,708 sat, board inflow 1,200,000 sat,
gross user payouts 1,190,400 sat, ordinary round/sweep mining fees 14,114 sat.
Payout mining fees are deducted from recipients. Those cycles use delegated refresh.

Separate restored-client check: **1 PASS / 0 FAIL**, 135.108 seconds, client image
`abandon-ship/bark:release-2c29c425d`. A fresh barkd restored the seed, discovered
the payout of a never-claimed delegated output and spent it through its regular
API. Payout `2f9cd7ba16fd6eb20c32a9441625edba038546c09e04d0bd1f9ccba2de6bcea5`;
spend `da24b422b84cfd34a69b94bbcad3e40e64a628a2f46ff780fa735e9c115bf143`.
Its report is `results/2026-10-08-unclaimed-client.json`.

The 2,000-owner workload failed in setup before its first large round: a fixed
board-fee allowance left 19,989,890 sat for 20,000,000 sat of requested outputs.
It is not capacity evidence. Earlier failed suite attempts remain recorded in the
Session 2 handoff. Large-load qualification, additional crash/mixed-flow checks,
and real native signet/browser/restore evidence remain open. These passing results
do not establish production readiness.
