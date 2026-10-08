# Cancellation crash and late-forfeit checks

Native image expiry-d-b2c19a215f3c, image ID
sha256:6314a4abcf7e1bfeeee3b034808f859ac225242cae7a239c382e6b6fa57c220f:
**2 PASS / 0 FAIL**, runtime prefix20261008T043717Z.

- cancel-crash:110.221s. SIGKILL at the cancellation INSERT rolls back retirement
  and preserves the old exchange. SIGKILL at the later payment INSERT preserves
  the committed cancellation and the released original claim. Restart pays only
  that swept original once. Tx864f37e989b85f84babf3496e3f612b8923087b7f414b13e2dfa7f0c669fdfdd.
- cancel-forfeit:110.754s. Genuine signed forfeits pass verification and wait on
  the association UPDATE behind cancellation. After cancellation commits, the
  cached request fails with `inputs not fully matched`, returning no unlock
  preimage. Only the swept original is paid, tx
  d7dc47a6c5da409044bcc77613099627527408fe8d8fc02369306dbc003bd6dd.

Both cases also check duplicate replacement keys, a spent-replacement veto,
exact preserved original values, no payment to exited/replacement keys and
unchanged settlement identity on later ticks. Full report and trigger/crash
proofs: native-cancellation-extra/report.json and per-case private runtime files.
The helper signs real requests; generated test seeds are supplied on stdin.
Helper build PASS. The same production image passed162serverunits/0FAIL.
These focused checks do not establish full-suite or signet readiness.
