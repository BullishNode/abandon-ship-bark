# Interactive refresh at a round boundary

The 2,000-owner capacity run exposed a shared Bark client defect before native
payouts were enabled. Of 64 baseline interactive refreshes, 57 succeeded, six
arrived after the round closed, and one used a stale attestation challenge.
The failed run is retained as `native-scale-2000-resume-1` (954.864 seconds).
It did not reach payout contention or seed recovery. Capital was sufficient.

Bark now consumes the first event after subscription and waits for a later
round's first attempt. The initial event can be replayed near the deadline.
The change applies to explicit and maintenance refresh. It adds eight lines in
`bark/src/lib.rs`, with no dependencies or signing/uncertain-response retries.
It can add one round of waiting. Requests must still reach the server within
the configured submission window.

The production change is published as
[`08f184417`](https://github.com/BullishNode/abandon-ship-bark/commit/08f1844173334922f5111e98f4019195052720ab)
on `qualification/refresh-round-boundary`. Publication of this qualification
branch does not complete the pending final-image suite.

The regression proxy buffers a real attempt until the next round begins. It
then sends the old and current events to the client. It forwards payment
requests to the real server unchanged. The assertion requires one accepted
submission, different VTXO IDs, spent original inputs, a confirmed funding
transaction and no expiry payment for those original inputs.

| Client and refresh run | Result |
| --- | --- |
| Old release `2c29c425d`, `native-refresh-boundary-before-1` | FAIL at the intended refresh assertion, 39.459 seconds; old-challenge attestation rejected |
| Corrected release, `native-refresh-boundary-after-1` | PASS, 39.301 seconds; exactly one accepted submission, funding confirmed |
| Old release, maintenance, `native-maintenance-boundary-before-1` | FAIL at the intended refresh assertion, 23.282 seconds; stale attestation rejected |
| Corrected release, maintenance, `native-maintenance-boundary-after-1` | PASS, 31.321 seconds; exactly one accepted submission, funding confirmed |

The maintenance mode sets the wallet's refresh threshold high enough to select
its real board VTXO. It requires a real funding transaction and replacement VTXO,
so a no-op maintenance result cannot pass. Capacity and the full final-image suite
remain separate gates; these regressions are not a production-readiness verdict.

The corrected client subsequently passed 64 baseline and 942 concurrent refreshes
in `native-scale-2000-fresh-round-1`, with zero failures. Loaded p50/p95/p99 were
12.646/18.618/22.984 seconds, against a predeclared p95 limit of 32.094 seconds.
That overall run FAILED after 2593.032 seconds during Core descriptor import for
seed recovery. Its completed refresh measurements do not turn it into a scale
PASS. The import timeout and recovery continuation are retained separately.
The separate continuation later confirmed twelve transactions spending all 6,000
outputs once. It does not change the original run's failed status or substitute
for a clean complete scale run.

The corrected release was built from `2c29c425debeeb28e7975c53777db08a9eea1aba`
plus production patch SHA-256
`14c2db5996c2290a00455ff9b9c37a04e6329a21bc5e35ab78ae0aed3d5e1b5c`.
The binary reports that base commit. Build profile: release with the repository's
default LTO and system SQLite.

- Image: `abandon-ship/bark:refresh-boundary-b226bda962f2`.
- Image ID: `sha256:5a7aacb04799a6d33d425da6e336ccb9959d85149efc0243462bed1da85ea0af`.
- Bark SHA-256: `b226bda962f2361c25f0c44b1c9501858e7f8e4d232f169282c41d955c5d86a8`.
- Barkd SHA-256: `ede4583ff912aee0c149b0529c6cc9b26a4b36afdd030bc64b4e9a27eae8f474`.
- Native server image in all four regressions: `abandon-ship/captaind:expiry-d-c594b92abe78`.

The initial parallel wallet unit run had 235 passes and one failure. The fcntl
and flock lock tests used the same key in their shared process-wide memory lock
manager. The failing test passed alone, and all 236 passed serially. Distinct
test-only fcntl keys restored a normal parallel 236/236 pass. Production lock code
did not change. The client and native fixture worktrees passed repository style,
arithmetic clippy, WASM and workspace checks.

Private evidence is under `bark-integration-2026-10-01/expiry-task-evidence/`:
the named run directories, `refresh-boundary-client.patch`, source/image
manifests, unit logs and `refresh-boundary-unit-collision.json`. No seed or private
wallet database is included in this report.
