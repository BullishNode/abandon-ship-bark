# Signature-linked fallback payout candidate

This candidate pairs `captaind` and `watchmand` with the matching Bark SDK and
reference clients for signet qualification. Complete financial and operational
qualification before production use. Source review and a successful build do
not establish qualification. Do not deploy this release on bitcoin mainnet.

Use fresh server and client stores. This release requires mailbox-only v4
registrations, signed recipient addresses and protocol 6 output proofs. There
is no conversion or compatibility mode for earlier fork stores or addresses,
including the earlier mailbox-only v4 stores. Fallback tables use ordinary
server migration 73 and wallet migration 43. Complete stores created by this
release can reopen without a reset of registrations or unpaid obligations.
Preserve earlier stores and backups.

Use the release-pinned complete Core Lightning runtime,
`v26.06.6-abandon6`, with its matching subdaemons and qualified plugin bytes.
That runtime carries one patch: `onchaind` reports a timed-out on-chain HTLC
after 6 confirmations instead of 3, so Core Lightning marks a payment failed
only outside the supported reorg window. The protocol assumes that no reorg
removes 6 or more blocks. Qualification of this boundary and protection of customers
beyond it remain incomplete.

Read `docs/operator.md` and `contrib/release/README.md` in the source, or
`OPERATOR.md`, `INSTALL.md` and `ROLLBACK.md` in the bundle, before installation
or recovery.

## Installation and recovery

- Require the release bundle to include its matching server/watchman TOML
  templates. Verify the full source commit, all four program checksums and
  runtime closures from the release itself.
- Import the complete portable runtime bundle. The hold plugin needs the GCC,
  OpenSSL, SQLite and PostgreSQL client library closures. Load the bundled
  environment only in the CLN service. Use the bundled Nix Core 31
  `bitcoin-cli` for its subprocesses.
- Use PostgreSQL 17 in a separate cluster when another major version is
  installed. Select its socket, port and database explicitly. On Ubuntu, use
  `/usr/lib/postgresql/17/bin/pg_dump` and
  `/usr/lib/postgresql/17/bin/pg_restore`.
- For public Signet with 2 board confirmations and a 12-block client exit
  margin, use `vtxo_lifetime` of at least 16 blocks. Preserve the normal
  4,320-block mainnet default; this is not permission to deploy on mainnet.
- Reserve fixed service ports from ephemeral port allocation. Configure the
  supervisor to restart unexpected clean exits as well as failures.
- A same-release rollback rehearsal preserved registrations, every balance
  field, confirmed and unconfirmed UTXO sets, source spend fences, seeds and
  the CLN identity. Identity comparisons stayed on the original host. The
  wallet status RPC advances its receiving address; that address alone is
  not a balance mismatch. A same-release rehearsal does not qualify an
  upgrade or disaster recovery on a fresh host.

## Changes in this source

- A mailbox key signs its immutable fallback script and network. Each customer
  output carries a coin-key signature over the server mailbox key and blinded
  recipient mailbox. The server verifies the actual output, registration and
  link before financial side effects and again in the tree writer. It rechecks
  the stored evidence before payout. No account xpub, child index or owner
  scanner participates in fallback attribution.
- The matching reference clients emit signed `0x02` addresses and retain external
  output links across restarts. Offline address generation requires an earlier
  online open that caches both server public keys and authenticates both mailbox
  registrations. Delivery-less addresses and addresses with only an unsigned
  `0x01` mailbox entry are refused before coin reservations. Normal and recovery
  mailboxes must both register the canonical payout script before live mailbox
  use. Unregistered mailbox reads, subscriptions and notices are refused; round
  notifications follow commit.
- A wallet app must persist its chosen immutable fallback script or the recovery
  parameters needed to reproduce it. There is no `GetFallback` RPC. A seed phrase
  alone does not identify a separately chosen destination.
- A valid link can direct a gift to another registered mailbox. It authorizes
  that destination rather than claiming ownership of the mailbox. Explicit
  recipient links remain authoritative even for a locally owned coin key.
- Forwarded invoice requests from the reference SDK include the recipient's
  policy, signed link and amount after current receive fees. The server checks
  them before invoice creation or a Created-invoice retry.
- Expiry payouts charge each owner payout script once per expiry height. The
  cumulative fee is the greater of 1,000 sats and 1% of the total gross paid,
  rounded up. Each new output pays that cumulative fee minus the fees already
  charged by non-conflicted allocations for the same script and height.
- Unresolved ownership holds its output and splits it away from ready
  siblings. A group whose net value is below dust also remains held while
  another coin under the same source anchor at that height is undecided. The
  dust decision completes once those
  coins are decided.
- Round construction retains the positions of its server-generated padding
  outputs. It passes their VTXO IDs to persistence so only those rows bypass the
  customer-link fence. At expiry, padding leaves belong to the operator when
  their policy key is upstream's unspendable key, `0x02 || sha256("unspendable1")`.
  There is no separate provenance table. This also assigns coins deliberately
  locked to that key to the operator, an accepted exception to the rule that the
  operator keeps only value it paid for. No one can spend with that key.
- The watchman uses Core's observed fee to replace an unrecognized P2A fee child.
  It preserves the exact payout parent. An unrecognized child does not establish
  operator funding or customer settlement.
- Deployment controls belong to the operator. Bind admin RPC to loopback, block
  public access with the firewall, and restrict any reverse proxy to the public
  API. Admin RPC has no authentication. The daemon does not enforce these
  isolation or proxy-trust rules.

These changes describe source behavior. Runtime and financial qualification on
matching final GNU programs and the final release bundle remain required.

## Known limits and remaining qualification

- Native HTLC-send claims require the coin's own upstream attempt to have
  succeeded, with contract, confirmation, spend and revocation checks retained.
  CLN must run `xpay-slow-mode=true`. Upstream marks an open attempt Failed when
  `listpays` is empty after the local in-flight guard is released. A received
  native command can still have no first part at that instant; this inherited
  transport/restart race remains an accepted limit.
- Receive timeout claims refuse settled or fulfilled grants and conservatively
  hold a disclosed preimage while collection remains unresolved. A timeout
  already broadcast before a later collection cannot be retracted by this
  guard. An exposed forwarded grant still requires its original provider's key
  to spend; no new recipient signing protocol is introduced.
  Recipient checks at invoice creation do not commit a destination to the
  payment hash. Later record loss or corruption can still park a receive at
  its final claim guard.
- Exposed uncollected receive grants use upstream's native timeout claim. It
  pays the actual miner fee from reclaimed operator funds rather than using a
  sponsored payout child. A grant that cannot cover its claim fee plus a
  non-dust drain output cannot be reclaimed alone; co-batching with larger
  eligible grants can make the claim economical. Such a grant can keep an
  internal sender refund pending while its required closure remains unconfirmed.
- Wallets discover registered fallback outputs through their ordinary chain
  scan. The payout receipts API, history tokens, commit ledger and supervised
  Lightning manifest recovery have been removed. Logical restore retains the
  financial state and rebuilds canonical observations, with no ledger reset.
- The server does not retain or replay customer emergency-exit claims. Core
  can evict a claim when a reorg makes its timelock nonfinal. The holder must
  retain the signed claim and resubmit it when its timelocks are valid again.
  Automatic server replay for an absent holder is no longer provided.
- Expiry fee children are rebuilt from current wallet and Core state. The payout
  parent stays immutable; no fee-child or operator subsidy ledger is retained.
- Earlier programs could forfeit a 1,000-sat customer coin before the same
  owner's other coins were ready. The regression requires 19 rights to pay
  18,000 sats with one 1,000-sat fee and no forfeiture. The cumulative fee and
  sub-dust hold require qualification on the final programs. Earlier results do
  not qualify this release.
- Earlier programs could stop payout fee replacement when an unrecognized
  foreign P2A fee child appeared. A targeted regression and its independent
  review retain their original program identities. They do not qualify the
  final programs or bundle.
- The 4,096-owner test has a 3,000-second limit, with every payment assertion
  unchanged. It must pass on the final release programs, with its measured time
  reported. Earlier failures against the 1,500-second limit remain historical.
  Passing this case establishes correct payouts and progress at that size; it
  does not establish a production payout-speed guarantee or load envelope.
- Open external Lightning outcomes follow their own upstream attempt status.
  A timeout alone does not resolve them. Empty native history uses upstream's
  Failed rule with the transport/restart limit described above.
- A missing registration or link, an invalid link or a decoded registration
  that fails authentication leaves customer bitcoin held and separates it from
  ready siblings. Never assign it to the operator. `captaind` derives the server
  mailbox key from the original server seed and authenticates links before it
  commits payout allocations. `watchmand` consumes committed allocations without
  that mailbox key. Every link depends on the exact original key.
  Restore must retain that seed, all accepted registrations,
  signed links and financial commitments. Re-sent evidence does not recreate
  lost commitments.
- Malformed script hex or signature text in a stored registration can pause an
  entire backing root, including ready siblings, until the authentic record is
  restored. Public registration RPCs reject this encoding. Database corruption
  or administrative edits can cause this hold; it never authorizes a sweep.
- Normal wallet key derivation and optional upstream xpub recovery remain.
  This RPC is independent of fallback attribution and can still receive an
  xpub. Seed-only discovery does not guarantee recovery across arbitrary gaps
  or spending of a separately chosen fallback destination.
- The fallback payout fee remains fixed in code at max(1,000 sats, 1%), counted
  cumulatively per payout script and expiry height. Ordinary board, refresh,
  offboard and Lightning fees are configured in TOML. This candidate does not
  make every fee configurable.
- The operator always sponsors payout mining fees, with no cap or budget
  condition (E-4, accepted by the owner). Payouts require confirmed usable
  watchman fee inputs. Monitor fee reserves and verify payout recovery after
  replenishment.
- Ordinary watchman claims sign and broadcast before the surrounding
  database transaction commits. A commit error or lost reply does not prove
  that publication failed. Durable authorization before publication,
  exact-byte replay after a crash and complete historical claim
  reconciliation remain unqualified. Preserve original records and signed
  transactions; source locks do not provide these guarantees.
- Final artifact checks, the absent-wallet payout and seed-restored spend on
  the clean installation, disaster recovery, upgrade, reorg tests, the reviewed
  financial checker, randomized load and the final-candidate soak remain
  required. Earlier results apply to their original source and program bytes.
  A successful build or a bounded payment scenario does not qualify production.
