# Native expiry payouts

Approach D adds an off-by-default captaind task. Qualification evidence is in
`expiry-payout-regtest/README.md`. Mainnet deployment remains gated on the shipping
image's complete regtest and signet/client/restore results.

## Settlement

Each tick pages expired pubkey coins and verifies a confirmed sweep of an output
on that coin's own exit path into the rounds wallet. Missing funding transactions,
live paths and missing predecessor history wait. Unclaimed refresh outputs require
all their original inputs swept. Padding leaves without a participation are skipped.
Waiting coins do not fill the batch; failed batches split. One new payment per tick.

A failed delegated exchange can be cancelled once every replacement is swept and
each original either has a confirmed exit or its own swept path. Under the coin
locks and one database transaction, cancellation retires all unclaimed replacements,
releases only swept originals, preserves original values/keys and records the exact
ID grouping. Completed forfeits or changed replacement states veto cancellation.
Live originals still wait. Cancellation is checked once per participation per tick
and waits for a real fee estimate, like payouts.

Each tick requests a real Core estimate for its configured target. No estimate means
wait; an unavailable different target does not block this one. The shared estimator
is unchanged. The signed payout's entire mining fee is deducted proportionally from users,
with deterministic satoshi rounding and a per-coin percentage cap. Shared keys get
one output. Confirmed funding, dust and maximum transaction weight are checked.
There is no Ark service fee on expiry payouts.

The coin lock arbitrates against refresh/offboard. The wallet lock protects signing
through durable commit. Core's mempool preflight also holds that lock, so slow Core
calls can delay ordinary wallet funding. Database coin states, receipt associations,
change-key metadata and the nursery transaction commit atomically. An uncertain
COMMIT retains the selected inputs until its outcome is known. The nursery stores
one raw transaction; receipt rows reference it. Startup reapplies pending spends
before workers, even with new payouts disabled.

## Configuration and deployment

Apply `contrib/expiry-settlement.sql` with psql autocommit before startup. The optional
schema is outside numbered migrations, so stock watchmand remains compatible.
It does not migrate approach B's different schema. For an earlier D draft containing
`expiry_settlement.raw_tx`, stop captaind, verify every copy equals its nursery row,
then drop the duplicate column and add the nursery foreign key before upgrading.

`[expiry_payout]` defaults:

| Setting | Default |
| --- | --- |
| enabled | false |
| interval | 60s |
| grace_blocks | 1008 |
| sweep_min_confs | 100 |
| min_payout_sat | 10000 |
| max_fee_pct | 20 |
| max_batch | 100 |
| conf_target_blocks | 6; accepts1,3,6 |

Mainnet enforces grace>=144 and sweep depth>=100. Short regtest/signet timings are
rehearsal settings. When enabled, `watchman_config` must be the same file mounted
into watchmand. Startup verifies its network and rounds-wallet sweep address, and
requires disabled or loopback admin RPC. This validates the file, not a different
running process. No settlement RPC, sidecar or separate payout wallet is used.

Set `receipt_dir` to a directory served at `/expiry-payouts/` on the Ark HTTP origin.
Files expose only txid, output index, net amount and fee. Missing files regenerate
once per payment, not once per coin. Publication failure does not stop settlement.
The client shows an unavailable fee when a receipt cannot be fetched. The exporter
runs only while the task is enabled.

## Operations and recovery

Use complete PostgreSQL base backups plus continuous WAL archiving, including the
payment commit and wallet metadata. Monitor archive failures and exercise physical
replay on a separate volume. An old snapshot with missing WAL is not a supported
restore. Preserve nursery history: receipts depend on it, enforced by a foreign key.
Keep seed backups offline before a mainnet deployment.

Monitor `expiry payout tick summary` for success, duration, candidates/waiting/paid,
real-estimate warnings, nursery warnings, receipt-export errors and rounds-wallet
funding. A growing eligible backlog requires checking its exact-path sweep depth,
fee cap and confirmed funding. The task keeps entitlements while those gates wait.

For a committed transaction missing from the mempool, check the nursery and Core's
rejection, restore funding/service conditions, and restart the same fork if needed;
startup retries the identical transaction. A manual CPFP requires an actual owned
change output. Exactly funded payouts may have none: wait for acceptance/fees to
improve; do not construct another payment for the same coins. Receipt deletion is
repaired from nursery bytes and the original coin records.

To stop new payouts, set enabled=false and restart this fork. Pending nursery
payments still recover. Do not roll back to stock captaind against this history:
it does not know the new nursery kind. Keep the fork until the cutover procedure
and pending-payment reconciliation are qualified.
