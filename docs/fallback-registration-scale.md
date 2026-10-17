# Registration and attribution envelope

The ignored `expiry_registration_authenticated_scale` integration case uses
100, 1,000 and 10,000 distinct signed accounts. Every registration goes through
the real native server's public `SetFallback` RPC. No SQL insert substitutes for
registration. No user or server wallet is funded, and no payout is initiated.

After registration, only that isolated captaind is stopped gracefully. Its
automatic scanner cannot consume work behind the measurement. The harness
queues synthetic public coin keys through the actual attribution API, then
runs the production bounded scanner against the retained real Postgres.
These keys are not funded VTXOs and do not establish payout correctness.

Each registry measures:

- Signed-record generation and sequential RPC registration time, throughput,
  p50/p95/p99 latency, captaind RSS and process-lifetime peak RSS.
- Correct-mailbox prioritization, absent-mailbox fallback, and wrong-mailbox
  fallback. The true owner sorts last. Each case has a 120-second budget.
- Completed scan passes, derived children, registry records scanned, cursor
  storage, and 100 subsequent authenticated cached lookups if resolved.

The mailbox path still derives the key before accepting ownership. It is a
priority path, not proof that mailbox delivery alone establishes a payee.
This harness resolves low-index positive matches. It does not benchmark the
full `records * 2^31` no-owner search, concurrent operations, payout throughput,
or Postgres/Core memory. Scanner RSS includes fixture records and the native
test harness. Peak RSS is process lifetime, not a per-phase peak or delta.

In the confirmed Nix environment, with the shared compile slot allocated:

```sh
NEXTEST_PROFILE=fallback-scale KEEP_ALL_TEST_DATA=1 NEXTEST_TEST_THREADS=1 \
  just int 'expiry_registration_authenticated_scale --run-ignored all'
```

Use rebuilt exact-source native binaries. Any explicitly approved
`ASSUME_BUILT=1` interoperability run must record their source pins and hashes
separately from the harness pin. Preserve the complete command, compiler
profile, hardware/process contention and raw nextest log with the JSON.

The partial JSON is written after registration and every attribution case to
the retained test directory's `authenticated-registration-scale.json`. A
budget overrun is recorded as `resolved: false` and fails the final assertion;
it is not classified as no owner. Other sizes still run. No extrapolated
capacity or production qualification follows from a single run.

## First measurement, 17 October 2026

The actual archived native run `a8928dcf-a33a-400b-a37e-8306b6e5b4f0`
**failed** its positive-attribution budget after 250.494 seconds. Every signed
registration succeeded. The 10,000-record missing-mailbox search did not reach
the true owner, sorted last at child index 18, within 120 seconds. It scanned
3,844 records and derived 123,008 children before the timeout. The result is
unresolved, never an operator-owned or no-owner classification.

| Registered accounts | Registration records/s | Verified mailbox | No mailbox | Wrong mailbox |
|---|---:|---:|---:|---:|
| 100 | 110.58 | 0.024 s | 0.719 s | 4.741 s |
| 1,000 | 116.36 | 0.044 s | 9.812 s | 14.499 s |
| 10,000 | 119.77 | 0.202 s | Failed at 120.114 s | Not run |

These are sequential signed-RPC registrations and low-index positive matches
on a shared 14-logical-CPU host with 63,720 MiB RAM. Other isolated native
Lightning tests overlapped part of the measurement. Products were Nix debug
builds, not optimized production release builds. The verified-mailbox path
still queued 10,000 cursors; its 0.202-second timing includes that work.

The [complete public report](evidence/registration-scale-first-20261017.json)
records source and ELF hashes, latencies, storage, memory and explicit unmeasured
cases. Full private logs and database files remain in the test evidence archive.
No production capacity or maximum payment-rate claim follows from this run.

The separately executed real watchman fee-replacement regression passed in
18.816 seconds, run `48226daf-0b2d-470b-b354-ca309b8f7de9`, using the same
archived product and native-test binaries. It replaced the mempool fee child
without changing the parent or payees, independently summed actual input
values, and counted only the canonical confirmed child's miner fee. It tested
a configured regtest fee rise and restart, not public-network congestion.
