# Additional native task regressions — 2026-10-08

These are individual regtest results. They do not qualify capacity, a full final
image suite, native device support or signet operation.

| Scenario | Result | Duration | Scope |
| --- | --- | --- | --- |
| `mixed-users.py` | PASS | 297.958 s | Six wallets, three transfers, delegated refresh, offboard, emergency exit and expiry payouts to the remaining owners |
| `disabled.py` | PASS | 23.990 s | Board, delegated refresh and offboard with new payouts disabled and the optional settlement tables renamed |
| `unclaimed-client.py` | PASS | 135.108 s | Fresh Bark daemon restores a seed, discovers a never-claimed round output's payout through the client API and spends it |

The mixed-user and disabled cases used image
`abandon-ship/captaind:expiry-d-c594b92abe78`, ID
`sha256:3a2e15a668699c9cef5bc04cc70d322de46eaef6bc52deb84c4fddffb7120fc9`.
The restored-client result predates that image and used
`abandon-ship/captaind:expiry-d-b2c19a215f3c`.

Private evidence directories under `expiry-task-evidence/` in the integration
workspace are `native-mixed-corrected-1`,
`native-restart-commit-after-release` and `native-unclaimed-client`.
Each retains the report, script hashes, source snapshot and scenario logs.
The disabled result belongs to a subset with three passes and one failure;
the subset as a whole failed. The original mixed-user failure is retained.

The corrected transfer assertion identifies selected inputs from the actual
before/after coin IDs. It checks their spent state and retains unselected coins.
Payout assertions aggregate entitlements that share one address in one payment.
They then compare gross value to the actual output plus its mining fee.

The restored daemon receives the seed and birthday. The test does not inject the
replacement coin ID into its discovery request. Its confirmed spending transaction
consumes the independently identified payout outpoint. This API result does not
substitute for the required browser and native device journeys.

At publication, the three scenario scripts match their saved passing hashes.
The separate shared-anchor optimization in `common.py` is still under capacity
qualification and is not part of this commit. Repository prechecks, arithmetic
clippy, WASM checks and workspace checks passed before publication.
