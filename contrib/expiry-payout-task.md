# Native expiry payouts

Approach D adds an off-by-default captaind task. The regtest harness and the earlier
coin-key payout evidence live outside this repository. Qualification of registered BIP84 destinations
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
inserts the user coin already spent with its settlement row and nursery transaction;
an uncertain COMMIT waits on that pending row before checking its outcome.
The fork requires the funding transaction in the cosign request, or an already
known chain transaction when `require_board_funding_tx` is false.

An arkoor or claim output belongs to its recipient if and only if the server
held its full signed chain, from a registration or a mailbox post, before its
settlement committed. A mailbox post registers its outputs that are unregistered
or spent, through the same registration call, before it stores them. It is
refused once any of them settled: every settled coin is spent. A chain that
fails registration otherwise, for example an unsigned one, is still delivered,
as upstream does, and stays with the input owner.
Other outputs, never registered or posted, belong to their actual input owner:
the last transaction's single input must match a stored parent spent into that
transaction; checkpoint parents retain the original user's key. Each output's
own value follows that key's fallback record. The selected source and owner are
checked again at commit. Registration and posts lock the same coin rows: if one
wins, payout must reselect the recipient; if payout wins, it is refused.
Outputs already present in the HTLC ledger are excluded from this unregistered
source. Change outputs are never posted and stay with the input owner. An
output delivered outside this server's mailbox without registration, to a
recipient silent through expiry and grace, is refunded to its input owner.

A Lightning receive forwarding to another wallet registers its claim outputs
before delivery, so a settled claim output was paid to the destination's own
record; the client treats its refused delivery as complete.

An unclaimed Lightning receive with a recorded preimage, a settled subscription
and no prior HTLC resolution pays the coin's own key through its fallback
record. The preimage is recorded before the hold invoice settles, so it does
not prove the payer paid; the settled subscription does, for an external
payment and for an intra-Ark one. A receive whose collection failed or is not
yet recorded waits and is checked again every tick. It requires the same
confirmed sweep, grace and destination checks. The task takes the payment
guard before coin locks, as cooperative claims do, and holds it through the
commit. The spent state, fulfilled HTLC resolution and payout commit
together. A receive whose external payment was canceled without disclosing the
preimage receives no payout.

Once the server grants a receive's HTLC-recv coins or knows its preimage, it
collects the held incoming HTLCs and never cancels them: neither an expired
invoice nor a forwarding timeout fails them back. A grant first checks that the
hold plugin still holds the incoming HTLCs. The hold settler retries
collection until it succeeds, and stops only when the hold plugin reports the
invoice canceled, since its HTLCs then went back to the payer. Such a receive
can never be collected; its subscription stays unsettled and its coins stay
held, unpaid.

A Lightning send's HTLC coins return to the sender's own key when the payment
cannot have succeeded. The task and the sender's refund request share one
decision under the payment guard: no recorded preimage, no successful attempt,
every attempt concluded, and every node that sent an attempt reports the
payment failed or unknown. An attempt concludes as failed only on evidence:
xpay refused it before sending any HTLC (invalid parameters, an expired
invoice, or no route before the first try), or, after its retry time plus a
buffer, the node reports nothing pending or complete. That retry time counts
from when the node is known to have received the request: lightningd answered
the xpay call, or the node listed the payment. A request the node never
answered, after a transport error or a restart, can still arrive late and
start, so it fails only after its invoice expired, since the node refuses to
start paying an expired invoice. A transport error on the xpay call leaves the
attempt open. Applying `contrib/expiry-fallback.sql` adds the column that
holds the invoice expiry; attempts from before it stay open. An offline or still-paying node leaves the
coins waiting; a completed payment whose preimage was never recorded is never
refunded. A node query gives up after ten seconds, and a node that failed to
answer is not asked again in the same tick: its sends wait, while other
wallets are paid in that tick. The commit takes the settlement write lock, rechecks the preimage,
marks the HTLCs revoked and cancels an intra-Ark receive for the same hash. A
settled intra-Ark receive holds the coins.

An intra-Ark receive whose claim was prepared holds the sender's coins while
its recipient can still claim, even after the invoice expired. The server
cannot collect without the preimage, so once the recipient can no longer
claim, the sender is refunded, by its own request or by the task, and its
open attempt fails. The recipient can no longer claim when the tip is past
the HTLC expiry of every granted HTLC-recv coin, none of them was claimed or
resolved, and a confirmed sweep spent each one's backing path into the rounds
wallet at `sweep_min_confs` depth. A granted coin the recipient exited counts
once watchmand's spend of its output through the server's timeout clause has
`sweep_min_confs` confirmations; a spend with the preimage records the
preimage, and the refund is refused. The decision runs under the
payment guard, which keeps the cooperative claim out; the commit rechecks the
recorded state and cancels the receive, so a later claim is refused. The
granted coins are never paid out. An external payer of a prepared receive
that is never claimed gets its payment back from the hold plugin, which fails
the incoming HTLCs back before they expire. Returning Lightning-client
reconciliation remains unqualified.

A returning arkoor sender checks expired outputs before delivery. A per-output
settlement refusal and the server's stored signed chain distinguish a recipient
payment from an input-owner refund, including a lost registration reply. Refunded
amounts are removed from the reported recipients; already-paid change is not
restored as spendable Ark value. Unknown outcomes retain the action and its locks.
Change stored before an interrupted finalization keeps ordinary coin-status
accounting so its later settlement is not debited twice.

The task groups all currently payable coins by destination across page boundaries.
Waiting coins do not fill the batch. Failed batches split between groups, never
within one group. A claim holds at most 10,000 coins; a group with more coins
is paid alone in its own payment. One new payment per tick. Sweeps that confirm in different ticks
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

A wallet group whose net would fall below `min_payout_sat` is a retained balance,
not a loss. Its coins stay unsettled, in the candidate scan and in the wallet's
expired balance; nothing is written for them. When later expired coins of the
same wallet, or a lower fee, lift the group's net to the minimum, the next tick
pays every retained and new coin of the group in one output of one payment. A
retained coin is never paid twice: it settles in the same commit as the others.

The coin lock arbitrates against refresh/offboard. The wallet lock protects signing
through durable commit. Core's mempool preflight also holds that lock, so slow Core
calls can delay ordinary wallet funding. Database coin states, settlement rows,
change-key metadata and the nursery transaction commit atomically. After an
uncertain COMMIT the task waits for the original row locks and reads the outcome;
if the database does not answer, captaind exits and its restart waits for that
COMMIT, so run captaind under a restart policy. The nursery stores
one raw transaction; settlement rows reference it. Startup waits for the previous
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
A coin without a fallback link, from before creation refusal, is still paid to
its own key's key-path address, `tr(user_pubkey)`. bark no longer finds or sweeps
those; the coin key descriptor tool kept with the regtest tooling spends them.
The optional schema is outside numbered migrations, so stock watchmand remains compatible.
It does not migrate approach B's different schema. For an earlier D draft containing
`expiry_settlement.raw_tx`, stop captaind, verify every copy equals its nursery row,
then drop the duplicate column and add the nursery foreign key before upgrading.

A wallet record's signature covers the chain's genesis hash, so a record
signed for another network cannot be replayed here. A record replayed from
another server on the same network pays an address the wallet signed for this
network and scans. Records stored before this binding keep paying until their
wallet signs a new one, which it does at its next sync. Clients and server
must be upgraded together: an older client's records are refused.

## Operator rules

These rules are documented, not enforced in code.

- Run one captaind per database. Its payment guards and coin locks live in
  memory, as upstream's do. Stop the old binary before starting a new one, and
  never run `captaind drain` or `undo-round` next to a running captaind.
- `abandon` an expiry payout only once it can never confirm: a conflicting
  spend of one of its inputs has `sweep_min_confs` confirmations. Its coins
  stay settled to it, so paying them again is a manual operation.
- Run captaind under a restart policy. It exits when a payout's COMMIT
  outcome is unknown and the database does not answer.
- watchmand's `sweep_address` must be a rounds-wallet address. Startup checks
  the configured file, not a different running process.
- `[expiry_payout]` refuses unknown keys. Remove `max_batch` and `receipt_dir`
  from an older configuration before starting this build.

`[expiry_payout]` defaults:

| Setting | Default |
| --- | --- |
| enabled | false |
| interval | 60s |
| grace_blocks | 1008 |
| sweep_min_confs | 100 |
| min_payout_sat | 10000 |
| conf_target_blocks | 6; accepts1,3,6 |

The payout pays from the rounds wallet with one output per wallet group, so a
group's coin count does not change the transaction size; the 400,000 WU weight
check still applies. Every payout carries operator change: a rounds wallet whose
balance equals a payout's gross defers it until the operator adds funds.

Mainnet enforces grace>=144 and sweep depth>=100. Short regtest/signet timings are
rehearsal settings. When enabled, `watchman_config` must be the same file mounted
into watchmand. Startup verifies its network and rounds-wallet sweep address, and
requires disabled or loopback admin RPC. This validates the file, not a different
running process. No settlement RPC, sidecar or separate payout wallet is used.

## Operations and recovery

Use complete PostgreSQL base backups plus continuous WAL archiving, including the
pending boards, payment commit and wallet metadata. Monitor archive failures and exercise physical
replay on a separate volume. An old snapshot with missing WAL is not a supported
restore. Preserve nursery history: settlement rows depend on it, enforced by a foreign key.
Keep seed backups offline before a mainnet deployment.

Monitor `expiry payout tick summary` for success, duration, candidates/waiting/paid,
real-estimate warnings, nursery warnings and rounds-wallet
funding. A growing eligible backlog requires checking its exact-path sweep depth,
group net minimum and confirmed funding. The task keeps entitlements while those gates wait.

| Waiting condition | Release evidence and next action |
| --- | --- |
| Missing real estimate | Core returns the configured target's estimate; check estimation data and wait |
| Group below the net minimum | Later eligible coins at the same destination or a lower fee can make it payable. If neither occurs, the group remains unpaid under this policy |
| Blocklisted or rounds-wallet destination | The group waits until the destination is permitted; no alternate destination is substituted |
| Missing funding transaction | The persisted round's funding transaction reaches Core; inspect round/nursery recovery |
| Unswept path or insufficient depth | A confirmed sweep spends this coin's own path into the rounds wallet; inspect watchmand and the exact input, then wait |
| Unfinished delegated exchange | Originals are swept, or the documented cancellation conditions hold; inspect participation/forfeit state and the cancellation audit |
| Net minimum, dust, weight or confirmed funding | A valid affordable transaction can be built; wait for fees/confirmations or restore rounds-wallet funding; the batch splitter continues to other eligible coins |
| Unknown COMMIT outcome | Captaind exited; restore database service and restart it, startup waits for the original COMMIT and reapplies a committed payout |

For a committed transaction missing from the mempool, check the nursery and Core's
rejection and restore funding/service conditions; the nursery rebroadcasts the
identical transaction after the next block. A manual CPFP spends the payout's operator
change output; do not construct another payment for the same coins. Each settled coin's
row keeps its payout txid, fee and paid script; a later wallet-record change does
not alter it.

To stop new payouts, set enabled=false and restart this fork. Pending nursery
payments still recover. Do not roll back to stock captaind against this history:
it does not know the new nursery kind. Keep the fork until the cutover procedure
and pending-payment reconciliation are qualified.
