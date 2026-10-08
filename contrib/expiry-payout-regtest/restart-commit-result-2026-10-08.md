# Restart with a database COMMIT still running

The old image `expiry-d-b2c19a215f3c` failed the single-input regression in
161.678 seconds (`20261008T125630Z-commit-crash`). A deferred PostgreSQL COMMIT
survived SIGKILL. The replacement process opened its listeners before that
transaction completed, and a normal offboard consumed the payout's funding input.
The later durable payout could no longer broadcast.

Before: payout `355f9b8fdc60d58b757b0f57c2613ec3d1b09bd5b557b13fdd77bfc2990fd1b8`
and offboard `5d3cf8b6c0cf52082aaab6e19ef777a6707816fe9a401db520c107bda7aa3bdb`
conflict on `70bdcb7dee1fb085aa912adac5b46b34d4e40db2f47dbaa499280a49c36ae391:0`.
These are private regtest transactions.

Startup now takes and releases a SHARE lock on the nursery table before loading
the rounds wallet. Existing nursery writers must finish first, including a COMMIT
from a dead process. Wallet metadata and pending payments are then loaded from
committed history. This also applies when new expiry payouts are disabled.

After: release image `expiry-d-c594b92abe78`, image ID
`sha256:3a2e15a668699c9cef5bc04cc70d322de46eaef6bc52deb84c4fddffb7120fc9`:

| Check | Result |
| --- | --- |
| Same delayed-COMMIT regression | PASS, 171.224s, `20261008T132551Z-commit-crash` |
| Startup checks | PASS, 9.120s |
| Disabled task without optional tables | PASS, 23.990s |
| Server units | 162 PASS / 0 FAIL |
| Repository style, clippy, WASM and workspace checks | PASS |

The regression observed the real blocked nursery lock and closed listeners,
then confirmed payout `80dcf65a9beb292d1136254f21806d7f1842c62d5211e31f1aa69bee1bdcf557`.
A subsequent normal offboard confirmed as
`51227bc4cfd71924831c13cb840e6eb6236ea6638931bca8b23e22fc50cc9821` without
reusing the original funding input. Temporarily withdrawn fixture capital was
returned; failed history and original volumes remain intact.

The preceding debug attempt reached payout confirmation but failed its final
test assertion by treating a decoded offboard response as raw stdout. Its failure
is retained. An earlier disjoint-input probe is partial evidence only.

The four-case release run finished **3 PASS / 1 FAIL**: the later mixed-user
scenario incorrectly required every wallet coin to be consumed by one send.
Its unused change input and assertion need a corrected rerun. This is not a clean
full-suite report. A new complete image suite, capacity and signet qualification
remain required. Raw reports, immutable script snapshots and transaction evidence
are under `expiry-task-evidence/native-restart-commit-*` in the project handoff
folder; private wallet material is not published.
