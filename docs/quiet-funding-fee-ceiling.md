# Quiet round and pool funding fees

When `fee_estimator.max_fee_rate` is set, round funding and pool issuance apply it to the fee and weight of the signed funding transaction. The estimate alone is insufficient: a child can subsidize unconfirmed ancestors, and omitted dust change is also a fee.

The wallet first signs a private PSBT copy without committing it, reserving an input or recording a signed transaction. If BDK's conservative weight estimate is the only excess, it rebuilds with the same selected inputs and configured recipients at the actual signed requirement. Round construction retains its root at vout zero and its pinned input. The rebuilt transaction must still cover its chunk requirement and fit the signed child ceiling. Dust absorption that violates the ceiling is refused. No extra input is selected to hide the excess.

Acceptance precedes the funding input locks, exposure of the round funding transaction, pool tree cosigning, transaction persistence and nursery submission. The accepted funding PSBT is checked again when it is finally signed. A refusal defers quiet work; it does not authorize an uncapped funding transaction. A later confirmation of a cheap parent can remove the ancestor deficit and allow the same work to proceed.

Configured recovery capital and confirmed fee headroom remain protected during input selection, after fee correction and before final signing. An affordable transaction fee alone does not authorize spending protected capital.

The ceiling uses the existing optional configuration. With no configured ceiling, the existing unrestricted signing behavior remains. This change does not impose a customer payout fee policy, create an economic reserve, select an urgent fee budget, change fallback destinations or alter the two-step payout route. Ephemeral pool key preparation and attempt diagnostics still exist; a refused attempt is not a successfully issued pool tree.

Qualification must execute both actual Core regressions: `nursery::quiet_round_defers_ancestor_shortfall_then_recovers` and `nursery::quiet_pool_defers_ancestor_shortfall_then_recovers`. The refusal must preserve the actual wallet locks and UTXOs, and the recovered transaction's raw Core fee must fit the signed ceiling. Unit tests cover conservative BDK fees, a root at vout zero, pinned inputs, unchanged recipients, omitted dust and pure signing. Run these checks against the exact server binaries and configuration being qualified.
