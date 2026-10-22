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
- An unresolved owner lookup holds its output without a split. A group whose net
  value is below dust also remains held while another coin under the same source
  anchor at that height is undecided. The dust decision completes once those
  coins are decided.
- Padding leaves belong to the operator when their policy key is upstream's
  unspendable key, `0x02 || sha256("unspendable1")`. There is no provenance table or
  change to upstream round creation. This also assigns coins deliberately locked
  to that key to the operator, an accepted exception to the rule that the operator
  keeps only value it paid for. No one can spend with that key.
- Before a claim can publish, the fork rechecks the source inside the write
  transaction. The source row must be unchanged, confirmed and unspent. Pubkey
  and HTLC-send sources must have no successor. A refused claim is not broadcast.
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

- Earlier programs could forfeit a 1,000-sat customer coin before the same
  owner's other coins were ready. The regression requires 19 rights to pay
  18,000 sats with one 1,000-sat fee and no forfeiture. The cumulative fee and
  sub-dust hold require qualification on the final programs. Earlier results do
  not qualify this release.
- Earlier programs could stop payout fee replacement when an unrecognized
  foreign P2A fee child appeared. A targeted regression and its independent
  review retain their original program identities. They do not qualify the
  final programs or bundle.
- The owner accepts the 4,096-owner capacity failure as an operating limit. An
  earlier test took 1,543.594 seconds, including teardown, against its unchanged
  1,500-second bound. Later candidates also failed that bound. The fork does not
  add a geometry cache to meet it. This size is not a qualified production
  envelope. Historical payments and timing do not prove the final release's
  capacity or complete financial conservation.
- Ambiguous external Lightning outcomes can remain pending. Supervised
  recovery of a request that never reached Lightning requires intact original
  history, complete producer and queue shutdown, and retained incident records.
  A timeout, empty history or shared payment hash does not prove failure.
- Automated fallback requires the recipient's authenticated registration.
  Supplied-address forwarding does not verify that registration. Unknown
  owners and registered owners with incomplete attribution remain held and
  can delay shared-tree payouts. Never assign their bitcoin to the operator.
  Restore must retain all accepted registrations and financial commitments.
- The fallback payout fee remains fixed in code at max(1,000 sats, 1%), counted
  cumulatively per payout script and expiry height. Ordinary board, refresh,
  offboard and Lightning fees are configured in TOML. This candidate does not
  make every fee configurable.
- Payouts require confirmed usable watchman fee inputs. The operator miner
  subsidy has no hard cap. Monitor fee reserves and verify payout recovery
  after replenishment.
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
