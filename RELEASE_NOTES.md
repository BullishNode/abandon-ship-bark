# Fallback payout candidate

Use fresh server and client stores for this fork schema. Complete stores from
the same schema can reopen without resetting registrations or unpaid obligations.
Earlier experimental fork database formats are not converted. Preserve earlier
stores and backups.

Bitcoin-address and output-script history queries now read the complete wallet
history. They preserve exact matching and movement order. Work and memory use
grow with stored history. A decode or projection error in another movement can
fail an address query. Other payment methods keep indexed queries.

Use the release-pinned complete Core Lightning runtime, `v26.06.6-abandon6`, with
its matching subdaemons and plugins. The ordinary upstream runtime does not
provide the required on-chain HTLC failure boundary. Internal-Lightning refunds
require at least six confirmations. The protocol assumes that no reorg removes
six or more blocks.

Read the [operator manual](docs/operator.md) before installation or recovery.
Financial qualification is incomplete. This candidate is for testing and is not
approved for bitcoin mainnet.

## Current limits

- Ambiguous external Lightning outcomes can remain pending. Supervised recovery
  of a request that never reached Lightning requires intact original history,
  complete producer shutdown and retained incident records. Missing history or
  an unproven shutdown can prevent this procedure.
- Unknown owners remain held and can delay other payouts in a shared tree.
  Restore must retain all accepted registrations and financial commitments.
- Payouts require confirmed usable watchman fee inputs. The operator miner
  subsidy has no hard cap.
- Financial recovery, reorg handling and production capacity still require
  qualification on the final programs. A successful build is insufficient.
