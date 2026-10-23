# Server fallback payout candidate

This candidate releases `captaind` and `watchmand` for Signet qualification.
Bark and `barkd` are test helpers. This is not a client release. Financial and
operational qualification remain incomplete. This candidate is not production
ready and is not approved for bitcoin mainnet.

Use fresh server stores for this fork schema. Complete stores from the same
schema can reopen without a reset of registrations or unpaid obligations.
Earlier experimental fork database formats are not converted. Preserve earlier
stores and backups.

Use the release-pinned complete Core Lightning runtime,
`v26.06.6-abandon6`, with its matching subdaemons and qualified plugin bytes.
The ordinary upstream runtime does not provide the required on-chain HTLC
failure boundary. Internal Lightning sender refunds require all necessary
grant closures to reach the configured depth. That depth defaults to 6
confirmations and cannot be lower. The protocol assumes that no reorg removes
6 or more blocks. Qualification of this boundary and protection of customers
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

- Expiry payouts charge each owner payout script once per expiry height. The
  cumulative fee is the greater of 1,000 sats and 1% of the total gross paid,
  rounded up. Each new output pays that cumulative fee minus the fees already
  charged by non-conflicted allocations for the same script and height.
- An unresolved owner lookup holds its output and splits it away from ready
  siblings. A group whose net value is below dust also remains held while another coin under the same source
  anchor at that height is undecided. The dust decision completes once those
  coins are decided.
- Padding leaves belong to the operator when their policy key is upstream's
  unspendable key, `0x02 || sha256("unspendable1")`. There is no provenance table or
  change to upstream round creation. This also assigns coins deliberately locked
  to that key to the operator, an accepted exception to the rule that the operator
  keeps only value it paid for. No one can spend with that key.
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
- Automated fallback requires the recipient's authenticated registration.
  Supplied-address forwarding does not verify that registration. Unknown
  owners and registered owners with incomplete attribution remain held and
  split away from ready siblings. Owner lookup uses upstream wallet recovery
  after the cached-match and 32-key mailbox checks. Recovery stops at an unused
  chunk of 10,000 keys. A registered coin beyond that gap remains held. Every
  cache miss starts from index 0; a completed no-match search is repeated after
  a new registration or one day. Never assign held bitcoin to the operator.
  Restore must retain all accepted registrations and financial commitments.
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
