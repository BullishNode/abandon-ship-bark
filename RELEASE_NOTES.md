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
