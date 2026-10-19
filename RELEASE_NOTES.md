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
The sender's VTXO remains pending in this case. An empty node payment history does
not authorize a refund. Automatic recovery and an operator resolution command
are not implemented. This requires a defined resolution policy, implementation
and tests before production approval.

### Owners without a registration

VTXOs whose owners never registered are held. A tree output containing such a
VTXO can also delay payouts for its other owners. This candidate has no automatic
disposition for those VTXOs. Ownership information is required to release them.

### Watchman fee supply

Payouts require confirmed, usable watchman UTXOs. Payouts wait when those UTXOs are
unavailable. The operator subsidy has no hard cap. An operator must monitor the
wallet balance, fee demand and pending payouts.

### Qualification still required

The final source revision and its release binaries are not yet qualified. The
remaining work includes:

- Resolve or disclose every outstanding independent diff review finding.
- Complete checks and unit tests on the final revision.
- Complete the server, wallet SDK, CLI, daemon and core integration suites using
  the release binaries and the declared patched Core Lightning plugin.
- Confirm funded exposed-grant closure, internal refunds, restart recovery and
  claim rebroadcast after reorgs within the supported bound.
- Confirm both an attached BIP84 application wallet and the released CLI and
  daemon wallet configuration.
- Build and identify all four Nix release binaries. Verify the embedded web
  client has the required payout settings, balances and history.
- Complete the fresh private Signet lifecycle: ordinary expiry and payout,
  uncollected internal Lightning receive with both clients absent, grant closure,
  sender refund, history, balances and recovery.

Earlier test results do not qualify binaries built from changed source. This
section must be updated with the final candidate's results before delivery.
