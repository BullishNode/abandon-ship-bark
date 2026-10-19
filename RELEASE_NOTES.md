# Maintenance cleanup

Use fresh server and client stores for this fork schema. Complete stores created
by the same schema can reopen without resetting registrations or pending
obligations. Earlier experimental fork database formats are not supported or
converted. Preserve earlier stores and backups.

Bitcoin-address and output-script history queries now read the complete wallet
history. They preserve exact matching and movement-ID order. Work and memory
use grow with the stored history. A decode or projection error in another
movement can fail an address query. Other payment methods keep indexed queries.

Financial qualification is incomplete. This candidate is for testing and is not
approved for bitcoin mainnet.

## Current limits

- An ambiguous external Lightning outcome can leave a payment pending
  indefinitely. Automatic resolution and an operator resolution command are
  absent.
- Unknown owners remain held and can delay other payouts in a shared tree.
- Payouts require confirmed usable watchman fee inputs. The operator miner
  subsidy has no hard cap.
- Internal-Lightning refunds assume reorgs remove at most three blocks. Six
  confirmations do not eliminate the deeper-fork double-payment risk.
- Interrupted internal-Lightning backup recovery and several watchman payout
  checks remain unqualified. Production capacity is not established.
