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

- Expiry payout and forfeiture decisions require the complete same-source
  owner group, including siblings under separate physical outputs. Final checks
  revalidate the source and ownership. Incomplete groups remain held; an
  independently ready fragment must not incur another output fee.
- The watchman uses Core's observed fee to replace an unrecognized P2A fee
  child. It preserves the exact payout parent. An unrecognized child is not
  evidence of operator funding or customer settlement.
- Expiry preparation reuses immutable source paths and funding bytes within
  one anchor pass. Selected-path, size, ownership, version and canonical-chain
  checks remain required. This change does not establish a capacity envelope.
- The optional `fee_estimator.max_fee_rate` ceiling clamps the initial fallback
  rates before use and all later fetched or fallback rates. This ceiling does
  not cap cumulative operator subsidy or every replacement fee.
- Both daemons reject `min_trusted_confs = 0` during configuration validation.
  A third-party unconfirmed deposit must not count as a trusted fee input.
- Server and standalone watchman initialization await initial wallet storage
  before success. Storage errors propagate to the caller. Database creation,
  the seed file and each wallet commit remain separate steps. A failure can
  leave partial state; preserve it and investigate before any retry.

These changes describe source behavior. Runtime and financial qualification on
matching final GNU programs and the final release bundle remain required.

## Known limits and remaining qualification

- Earlier programs could forfeit a small customer leaf before the same
  owner's siblings were ready. A confirmed case paid 17,000 instead of the
  merged 18,000 sats. The first proposed fix still forfeited 1,000 sats. The
  complete owner-group checks are now implemented, but final-program
  qualification remains required. Preserve every customer right and hold
  incomplete groups. Do not relax financial checks.
- Earlier programs could stop payout fee replacement when an unrecognized
  foreign P2A fee child appeared. A targeted regression fails before the
  source fix and passes after it. Independent review supports that fix's
  scoped financial behavior. These results retain their original program
  identities. They do not qualify the final programs or bundle.
- An earlier 4,096-payee native test took 1,543.594 seconds, including teardown,
  and failed its unchanged 1,500-second limit. Later candidates also failed
  that limit. Separate historical chain verification supports 17 zero-fee
  parents, 17 fee children with 925,040 sats in miner fees, and 4,096 distinct
  scripts paid exactly 4,000 sats once each. It does not establish a complete
  match with the registered-owner set or full operator-capital conservation.
  The current geometry reuse still requires the unchanged capacity test on
  the final programs. There is no qualified production capacity envelope.
- Ambiguous external Lightning outcomes can remain pending. Supervised
  recovery of a request that never reached Lightning requires intact original
  history, complete producer and queue shutdown, and retained incident records.
  A timeout, empty history or shared payment hash does not prove failure.
- Automated fallback requires the recipient's authenticated registration.
  Supplied-address forwarding does not verify that registration. Unknown
  owners and registered owners with incomplete attribution remain held and
  can delay shared-tree payouts. Never assign their bitcoin to the operator.
  Restore must retain all accepted registrations and financial commitments.
- The fallback output fee remains fixed in code at 1,000 sats. Ordinary board,
  refresh, offboard and Lightning fees are configured in TOML. This candidate
  does not make every fee configurable.
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
