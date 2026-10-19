# Post-RC maintenance cleanup (PR33)

This maintenance series reduces duplicated fork storage and makes Bitcoin
payment-method history use the complete wallet-history projection. Exact-source
builds, artifacts and qualification results are recorded in
[PR33](https://github.com/BullishNode/abandon-ship-bark/pull/33).

## Installation and reopen scope

Use fresh server and client stores for this fork schema. The inherited migration
chains remain unchanged. Fork tables initialize atomically, and normal reopen of
a complete store created by the same schema retains identity, registrations,
pending acknowledgments, snapshots and obligations. Earlier experimental fork
formats are not converted or repaired by this cleanup. Initialization success
does not establish completed payment or recovery.

## History and public API

`Wallet::history_by_payment_method` filters the complete history projection for
Bitcoin addresses and output scripts. Matching preserves the exact payment-method
variant. Query results keep ascending movement-ID order; complete history keeps
newest-first time/ID order. Non-Bitcoin methods retain their indexed path.
`Wallet::exit_protection_schedule` remains public.

Bitcoin payment-method queries read and project the complete stored history and
fallback snapshot before filtering. Their work and memory use can grow with
history size, and an unrelated movement decode or projection error can fail a
query. Small fixture timings do not establish production-scale performance.

## Confirmation and qualification boundaries

Payout confirmation uses the authoritative allocation record rather than copied
confirmation fields. Native user-claim evidence remains separate from fallback
payment evidence. Ownership, authorization, principal, grant closure, source
fences and actual canonical-payment checks retain their existing roles.

Entitlement presence is a durable decision fence. It does not prove customer
payment, grant collection or discharge of principal.

Results belong to their exact source and artifacts. A retained actionable claim
is an unpaid entitlement; an actual confirmed spend establishes a different
recovery outcome. Native seed-derived fallback recovery, an application-owned
external payout wallet, and a caller-authorized claim destination have separate
key and spend requirements.

For predecessor `3d6c4ca1a6cfea869542b2e4cea6203966af4ede`, retained per-case runner
records reconcile 706 eligible cases as 496 passes, four failures, one process
abort and 205 unknown outcomes. These are historical observations, not results
for changed programs. The original narrative below records earlier snapshots
and remains preserved. Current exact-source evidence is maintained in PR33.

On `a8c8076157cdc860e99181c60c46f80ce0443dcb`, the targeted 24-case batch
recorded 23 passes and one failure. The internal-Lightning physical-backup case
stopped at its original 40-second grant-closure wait, before refund and seed-spend
assertions. These are observations of that exact A8 revision, not qualification
of the later FR6 changes. PR33 records qualification for each subsequent source.

## Current limits retained by this cleanup

An ambiguous external Lightning payment remains held without automatic outcome
resolution or an operator resolution command. An empty node payment history does
not authorize a refund.

Owners without registrations remain held. Their unresolved ownership can also
delay other payouts that share a tree output.

Payouts require confirmed, usable watchman fee-wallet outputs. They wait when
those outputs are unavailable; the operator subsidy has no hard cap.

The internal-Lightning refund design retains its assumption that reorgs remove
no more than three blocks. Six confirmations do not eliminate the deeper-fork
double-payment risk. Tests of other recovery forks do not remove this assumption.
Production capacity remains unqualified.

# Historical release-candidate narrative

# Abandon-ship release candidate

This candidate adds emergency payouts to registered on-chain addresses. It is
not approved for bitcoin mainnet. Qualification is incomplete.

## Behaviour

- A wallet registers its xpub and the payout address selected by the application.
  Payments do not carry attribution hints. The server retains the registration.
- The server attributes expired VTXOs before it decides a payout. It checks the
  recorded mailbox first and uses xpub derivation when that check cannot establish
  ownership. An unknown owner does not make a VTXO the operator's property.
- Each payout output pays a 1,000 sat service fee. After grouping and the service
  fee, a payout below its script's dust threshold goes to the operator.
  Ready outputs with the same expiry height can share a payout to one address.
- The watchman pays miner fees from its wallet. The payout transaction does not
  change when the watchman replaces its fee child.
- A failed internal Lightning payment waits for the expired, unsettled receiver
  grants to close before the sender receives an ordinary fallback payout. The
  operator's funded sweep closes these grants, including exposed grants.
- A recipient who claims internal Lightning grants on chain can complete the
  payment only after every issued grant has verified fulfillment. A shared
  preimage or a partially claimed set does not establish collection.
- The refund depth defaults to six blocks. Values of three or fewer are refused.
- The server retains the bytes of user exit claims it observes confirmed. After
  a reorg, it can rebroadcast those claims when they are valid and their inputs
  remain unspent.

## Known issues and remaining work

### Reorg bound

The design assumes that reorgs do not remove more than three blocks. A deeper
reorg can allow both sides of an internal Lightning payment to receive bitcoin.
Six confirmations do not remove this risk. Deployment requires explicit
acceptance of this bound and monitoring for violations.

### Ambiguous external Lightning payments

A crash or connection failure can leave a payment without proof of its outcome.
The sender's VTXO remains pending indefinitely in this case. An empty node payment
history does not authorize a refund. Automatic recovery and an operator
resolution command are not implemented. This requires a defined resolution
policy, implementation and tests before production approval.

### Owners without a registration

VTXOs whose owners never registered are held. A tree output containing such a
VTXO can also delay payouts for its other owners. This candidate has no automatic
disposition for those VTXOs. Ownership information is required to release them.

### Watchman fee supply

Payouts require confirmed, usable watchman UTXOs. Payouts wait when those UTXOs are
unavailable. The operator subsidy has no hard cap. An operator must monitor the
wallet balance, fee demand and pending payouts.

### Qualification status

The preceding candidate passed static checks and 852 unit tests, with one
skipped. Its four Nix release programs built and were exercised on a fresh
private Signet. Ordinary payouts, closure of an unsettled internal Lightning
receive including a small grant, a restart between the fifth and sixth grant
confirmations, the sender's refund, and seed-restored spending were observed.
The released native wallet and embedded UI also showed the confirmed payout,
fee, permanent address and correct balance. These are observations of that
preceding revision, not qualification of changed programs.

Across its 706 selected integration cases and separate qualification after a
shared-memory setup failure, 690 distinct cases passed. Eleven watchman cases
failed at old claim-broadcast waits, three same-hash payment cases timed out,
one internal-Lightning backup case timed out, and one wallet test process
aborted during setup. Original failures and the separate qualification results
are retained. This is not a clean full-suite pass.

This revision separates external dispatch locking from short payment decisions
so a pending remote payment does not block a same-hash receive. Dispatch remains
excluded until its result is reconciled; durable source and refund checks are
unchanged. The three original integration scenarios and their payment assertions
are unchanged. The correction still requires compilation and runtime checks.

The backup test now recognizes a verified, funded closure of a signed ancestor,
instead of requiring every virtual leaf to appear on chain. Its refund,
receiver, seed-spend and value-conservation assertions remain unchanged. The
wallet setup fixture now allocates its large asynchronous future on the heap.
Both test corrections still need to compile and complete on the release
programs. They do not establish a passing result by source inspection.

The following work remains before production approval:

- Build and qualify all four programs from this exact revision. Run static,
  unit and integration checks, the affected payment and recovery scenarios,
  and the private-Signet lifecycle. Earlier artifacts cannot qualify them.
- Complete the eleven watchman scenarios with the applicable payout receipts
  and original amount, fee and recovery assertions. Their former event waits
  stopped before those checks; the observations do not establish lost funds
  or correct final operator credit.
- Verify same-hash receive progress without weakening dispatch authorization,
  and complete the interrupted backup refund and seed-spend checks.
- Complete receive-side return reporting. A preceding application-wallet driver
  reported an error after a closed-window receive was canceled, while a separate
  snapshot showed zero balances. A narrowly checked driver correction has been
  built, but its fresh lifecycle has not completed. A new board attempt stopped
  before a proposal because the private node had no fee estimate; no estimate
  was fabricated or fee guard bypassed.
- Finish the capacity assessment. Default-ignored scale tests remain available
  on demand and are outside the full-suite counts. Production capacity has not
  been qualified.
- Check intermediate commits independently if the review series needs to be
  bisected. The preceding final revision was built; every earlier commit was
  not separately built.

The private-Signet profile is deliberately short. It does not establish public
network fee behaviour or production capacity. Final release qualification and
remaining work are also recorded with the downloadable release artifacts.
