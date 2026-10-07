# Experimental expiry-settlement RPC

`bark_server.ExpirySettlementAdminService/Exchange` is registered on captaind's
existing admin listener. It has the same deployment and access assumptions as
the other admin methods. Install `contrib/expiry-settlement.sql` in captaind's
database before calling it. This optional table does not change refinery's
migration history or require a modified watchmand.

The request selects one operation:

- `page`: up to 256 expired pubkey VTXOs, ordered by `(expiry, vtxo_id)`, with
  grace and minimum-amount filters. `claimed = true` instead returns durable
  handoff IDs ordered by ID, ignoring grace/amount/expiry filters and omitting
  VTXO bytes. Claimed pages set expiry to zero and use only the last ID as cursor;
  candidate pages use both expiry and ID. Retry `claim` to fetch an unknown
  receipt's VTXO. Restart claimed scans each tick: a concurrent receipt can commit
  behind the current cursor. Smaller page limits reduce encoded response size.
- `claim`: up to 256 IDs. Each independent result is `CLAIMED`, `BUSY` or
  `INELIGIBLE`. A committed receipt and the transition to `spent` occur in the
  same transaction. Retrying an existing receipt returns `CLAIMED` and the
  original VTXO. An ordinary Ark-spent coin does not qualify.
- `spenders`: hints for up to 256 exact outpoints, in request order. Missing
  outpoints have no txid. These hints are not chain validation.

Claims hold captaind's VTXO flux lock through the commit and reject unfinished
round participations. Delegated registration holds the same lock through its
validation/store operation. The receipt table is permanent; it records transfer
of an obligation to the payout service, not a payment confirmation.

The caller remains responsible for validated exit paths, confirmed sweeps,
mainnet grace/confirmation policy, fees, signing, its durable payout ledger and
signed transaction journal. After a restore, configure the optional top-level `settlement_replay_ids` path.
Captaind reads whitespace-separated VTXO IDs and replays them in one database
transaction immediately after opening the database, before workers or listeners.
Missing VTXOs, other recorded spends, confirmed exits or unfinished participations
abort startup and roll back the whole import. Replaying an existing receipt is
idempotent. The import deliberately does not require a current chain tip.

Stop captaind, watchmand and sidecar writers before restoring. With the current
sidecar journal and its state database, run its `--export-settlement-ids PATH`
command, then start captaind with that path. Keep writers stopped until the
export is complete. Restore missing captaind history or resolve an unfinished
round from authoritative records before retrying; payout IDs cannot reconstruct
lost Ark transfers. An omitted path disables this barrier, and a stale export
cannot reveal omitted settlements. This is an operator restore procedure, not
automatic backup freshness detection.

The paired experiment adapter keeps only its own quarantine/payout tables in a
separate database. It reconciles permanent claim receipts before creating new
transactions; signed journal transactions can rebroadcast without the admin RPC.
All adapter replicas must share that state database and journal. Independent
payout ledgers must not consume the same permanent captaind receipts.

A small generated-client example is available:

```sh
cargo run -p bark-server --example expiry-settlement -- http://127.0.0.1:3536 page
```

The example accepts `page`, `claimed`, `claim ID...` and `spenders ID...`; it
prints only one page. Its grace is 144 blocks. Database tests are ignored by
default and require `EXPIRY_TEST_POSTGRES_PORT` pointing to an isolated instance.
Each test creates a fresh database and applies the normal migrations plus the
optional table DDL. They do not launch captaind or Bitcoin Core.

```sh
EXPIRY_TEST_POSTGRES_PORT=5432 cargo test -p bark-server --lib expiry_settlement -- --include-ignored --test-threads=1
```
