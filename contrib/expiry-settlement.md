# Experimental expiry-settlement RPC

`bark_server.ExpirySettlementAdminService/Exchange` is registered on captaind's
existing admin listener. It has the same deployment and access assumptions as
the other admin methods. Install `contrib/expiry-settlement.sql` in captaind's
database before calling it. This optional table does not change refinery's
migration history or require a modified watchmand.

The request selects one operation:

- `page`: up to 256 expired pubkey VTXOs, ordered by `(expiry, vtxo_id)`, with
  grace and minimum-amount filters. `claimed = true` instead returns durable
  handoff IDs/expiries, ignoring grace/amount filters and omitting VTXO bytes.
  Retry `claim` to fetch an unknown receipt's VTXO. Use the last returned expiry and ID
  as the next cursor. Smaller page limits reduce encoded response size.
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
signed transaction journal. This patch does not implement a sidecar adapter or
an automatic recovery barrier before captaind starts normal work after restore.
It is not a complete replacement for the existing sidecar.

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
