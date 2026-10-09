# Native expiry payouts

Approach D adds an off-by-default captaind task. The earlier coin-key payout evidence is in
`expiry-payout-regtest/README.md`. Qualification of registered BIP84 destinations
is in progress; the earlier results do not qualify this protocol. Mainnet deployment
remains gated on the shipping image's complete regtest and signet/client/restore results.

## Settlement

Each tick pages expired user entitlements and verifies a confirmed sweep of an output
on that coin's own exit path into the rounds wallet. Missing funding transactions,
live paths and missing predecessor history wait. Unclaimed refresh outputs require
all their original inputs swept. Padding leaves without a participation are skipped.
Coins with a proven key link use the wallet record's destination. Coins without a
record or link use their own taproot key (legacy fallback). A blocklisted destination
or a destination owned by the rounds wallet waits.

Board cosigning also records an unsigned entitlement in `pending_board`, keyed
uniquely by its funding outpoint. The server checks the proposed funding output's
amount and script, and adds the funding anchor to watchmand's frontier before
returning the cosign. The unsigned user coin stays outside the ordinary coin
table, so generic transaction registration cannot bypass board confirmation.
A proposal never broadcast has no swept backing path and receives no payout.
Normal registration and payout lock the same retained pending row. Settlement
inserts the user coin already spent with its receipt and nursery transaction;
an uncertain COMMIT waits on that pending row before checking its outcome.
The fork requires the funding transaction in the cosign request, or an already
known chain transaction when `require_board_funding_tx` is false.

Unregistered arkoor and claim outputs belong to their actual input owner. The
last transaction's single input must match a stored parent spent into that
transaction; checkpoint parents retain the original user's key. Each output's
own value follows that key's fallback record. The selected source and owner are
checked again at commit. Registration locks the same coin rows: if it wins,
payout must reselect the recipient; if payout wins, registration is refused.
Outputs already present in the HTLC ledger are excluded from this unregistered
source.

An unclaimed Lightning receive with a recorded preimage and no prior HTLC
resolution pays the coin's own key through its fallback record. It requires the
same confirmed sweep, grace and destination checks. The task takes the payment
guard before coin locks, as cooperative claims do, and retains it through an
uncertain commit. The spent state, fulfilled HTLC resolution and payout commit
together. A receive whose external payment was canceled without disclosing the
preimage receives no payout. Failed-send refunds and returning Lightning-client
reconciliation remain unqualified.

A returning arkoor sender checks expired outputs before delivery. A per-output
settlement refusal and the server's stored signed chain distinguish a recipient
payment from an input-owner refund, including a lost registration reply. Refunded
amounts are removed from the reported recipients; already-paid change is not
restored as spendable Ark value. Unknown outcomes retain the action and its locks.
Change stored before an interrupted finalization keeps ordinary coin-status
accounting so its later settlement is not debited twice.

The task groups all currently payable coins by destination across page boundaries.
Waiting coins do not fill the batch. Failed batches split between groups, never
within one group. One new payment per tick. Sweeps that confirm in different ticks
can produce separate payments to the same wallet.

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
with deterministic satoshi rounding. Each destination gets one output per payment.
The minimum applies to that output after its fee deduction; there is no percentage
cap. Confirmed funding, dust and maximum transaction weight are checked.
There is no Ark service fee on expiry payouts.

The coin lock arbitrates against refresh/offboard. The wallet lock protects signing
through durable commit. Core's mempool preflight also holds that lock, so slow Core
calls can delay ordinary wallet funding. Database coin states, receipt associations,
change-key metadata and the nursery transaction commit atomically. An uncertain
COMMIT retains the selected inputs until its outcome is known. The nursery stores
one raw transaction; receipt rows reference it. Startup waits for the previous
process's nursery writes to finish, then loads the wallet and reapplies pending
spends before workers, even with new payouts disabled.

## Configuration and deployment

Apply `contrib/expiry-settlement.sql`, then `contrib/expiry-fallback.sql`, with psql
autocommit before startup. New client wallets use BIP84. Do not reinterpret a
funded BIP86 wallet database with the new descriptor; no BIP86 migration is included.
Wallet sync and automatic refresh reconcile expired coins with the trusted server
before selecting inputs. Spent coins leave the Ark balance; their BIP84 payouts
appear in the ordinary on-chain balance. An unavailable status retains the coin
and defers its automatic refresh. A status reply cannot overwrite a concurrent
local coin lock. Adopting a newly spent coin records its full Ark debit atomically
with the state change; retries do not duplicate the movement. This reconciliation
records the observation time, without inferring a payout transaction or fee from
the status alone. Expired pending boards also query that authenticated status.
A persisted settlement phase consumes only the board's own lock, records one
debit and completes its original board movement. If the funding anchor is already
spent and no board exit is known, an unavailable server leaves the entitlement
pending without starting a new exit. Existing board exits keep their recovery path.
Historical coin-key payouts still use the existing sweep. Its
movement now has zero Ark balance change (the coin was already debited), with
`payout_total_sat`, `swept_sat`, and `sweep_fee_sat` recording the on-chain transfer.
The optional schema is outside numbered migrations, so stock watchmand remains compatible.
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
| max_batch | 100 |
| conf_target_blocks | 6; accepts1,3,6 |

`max_batch` counts coins per claim. The default is 100; a positive larger value
is valid. The current implementation leaves a larger indivisible group waiting.
The policy for such groups remains an open qualification gate.

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
pending boards, payment commit and wallet metadata. Monitor archive failures and exercise physical
replay on a separate volume. An old snapshot with missing WAL is not a supported
restore. Preserve nursery history: receipts depend on it, enforced by a foreign key.
Keep seed backups offline before a mainnet deployment.

Monitor `expiry payout tick summary` for success, duration, candidates/waiting/paid,
real-estimate warnings, nursery warnings, receipt-export errors and rounds-wallet
funding. A growing eligible backlog requires checking its exact-path sweep depth,
group net minimum and confirmed funding. The task keeps entitlements while those gates wait.

| Waiting condition | Release evidence and next action |
| --- | --- |
| Missing real estimate | Core returns the configured target's estimate; check estimation data and wait |
| Group below the net minimum | Later eligible coins at the same destination or a lower fee can make it payable. If neither occurs, the group remains unpaid under this policy |
| Group exceeds max_batch | The whole group waits. The oversized-group policy is not yet qualified |
| Blocklisted or rounds-wallet destination | The group waits until the destination is permitted; no alternate destination is substituted |
| Missing funding transaction | The persisted round's funding transaction reaches Core; inspect round/nursery recovery |
| Unswept path or insufficient depth | A confirmed sweep spends this coin's own path into the rounds wallet; inspect watchmand and the exact input, then wait |
| Unfinished delegated exchange | Originals are swept, or the documented cancellation conditions hold; inspect participation/forfeit state and the cancellation audit |
| Net minimum, dust, weight or confirmed funding | A valid affordable transaction can be built; wait for fees/confirmations or restore rounds-wallet funding; the batch splitter continues to other eligible coins |
| Unknown COMMIT outcome | The database answers after the original row locks release; restore database service while the task retains wallet inputs |
| Receipt publication failure | The configured directory becomes writable and served; the next enabled tick regenerates missing files from nursery history |

For a committed transaction missing from the mempool, check the nursery and Core's
rejection, restore funding/service conditions, and restart the same fork if needed;
startup retries the identical transaction. A manual CPFP requires an actual owned
change output. Exactly funded payouts may have none: wait for acceptance/fees to
improve; do not construct another payment for the same coins. Receipt deletion is
repaired from nursery bytes, original coin values and the stored paid scripts. A
subsequent wallet-record change does not alter the historical receipt.

To stop new payouts, set enabled=false and restart this fork. Pending nursery
payments still recover. Do not roll back to stock captaind against this history:
it does not know the new nursery kind. Keep the fork until the cutover procedure
and pending-payment reconciliation are qualified.
