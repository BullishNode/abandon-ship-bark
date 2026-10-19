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

The most recently checked candidate passed static checks and 852 unit tests,
with one test skipped. Earlier Nix builds succeeded. The latest corrections
still require completion and qualification of the final captaind, watchmand,
bark and barkd Nix artifacts. The complete server, wallet SDK, CLI, daemon and
core integration suites remain pending for those artifacts, using the required
patched Core Lightning plugin.

An earlier build completed ordinary expiry payouts, funded receiver-grant
closures six confirmations deep, a server and watchman restart across the
fifth-to-sixth confirmation, and the sender's refund on a fresh private Signet.
Seed-restored wallets spent the resulting outputs. These observations do not
qualify changed binaries; the lifecycle, history, balances and recovery must be
verified again on the final artifacts. An intermediate native daemon passed
HTTP checks for the embedded web assets, authentication, payout registration,
zero balances and empty history. Rendered payout history and final-product web
verification remain pending.

The following findings still require verification or completion:

- Earlier exit scenarios timed out. Fee funding selection has been corrected to
  exclude unconfirmed wallet outputs, but package relay and the affected exit
  scenarios still need to pass on the final binaries.
- The Lightning revocation test setup is being corrected for receiver-grant
  closure and the six-confirmation refund depth. Its rerun remains pending.
- Outgoing Lightning history retained a pending send entry after its fallback
  refund was confirmed. A correction now verifies the complete set of confirmed
  payout proofs before ending that send. Its integration regression and private
  Signet verification on the final artifacts remain pending.
- Seed recovery retains historical conditional, round and offboard inputs when
  the server cannot provide a supported discharge proof. These inputs must not
  inflate balances or prevent spending recovered change. Updated recovery tests
  still need to pass their full balance and spending checks.
- Two automatic-refresh fixtures reached the protective-exit boundary instead
  of completing a refresh. Their setup must exercise the refresh window with
  protection enabled, and the refresh and rejected-input assertions must pass.
- A background ownership-scan test observed a different scheduling checkpoint
  than expected. The scan ordering and late-arriving-key guarantees remain under
  review; the final regression has not passed.
- Native seed-recovery backup tests must use the released BIP86 wallet, while
  application-wallet tests retain BIP84 coverage. The corrected native backup
  tests have not yet passed on the final artifacts.

Final qualification must also retain coverage of an attached BIP84 application
wallet and the released CLI and daemon configuration, exposed-grant closure,
internal refunds, restart recovery and user-claim rebroadcast after reorgs within
the supported bound. Outstanding independent review findings must be resolved or
disclosed before the final assessment.
