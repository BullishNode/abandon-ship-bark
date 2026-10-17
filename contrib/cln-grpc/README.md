# Structured xpay errors

This patch targets Second's already pinned Core Lightning `v26.06.6` source.
The original `cln-grpc/src/server.rs` SHA256 is
`9eca049f3530b28aa2d8f2a49bdb2ccf3a318e60976c411b21fa83e00ec815ad`.
The plugin is already built separately by `nix/dev-shell.nix`; no Lightning routing implementation is forked here.

The Xpay gRPC handler preserves an existing `RpcError.code: Some(i32)` as
`abandon-cln-jsonrpc-error-code` metadata. Socket, decoding and connection errors have no such receipt.
The gRPC status remains Unknown for compatibility. Clients must not infer payment failure from its text.
The metadata reports the actual JSON-RPC error; it does not by itself declare a payment terminal.

The backend accepts no-part receipts only for source-reviewed errors 205 (route not found before any attempt)
and 207 (invoice expired before routing), on the qualified node version.
CLN increments `total_num_attempts` before reservation and injection. A later routing failure maps to 209.
The backend also checks actual payment history for pending parts, completed payments and preimages.
An in-progress error 200, a generic 209, or a transport error cannot authorize a no-part refund.
For ordinary attempted failures, a random durable label must occur in the node's actual sendpay history,
and all same-hash groups must be Failed. The existing source locks and durable payout fence close future dispatch admission.

The patch includes three adapter unit cases covering metadata wire encoding, local transport failure and an in-progress error.
The backend's native tests cover real receiver cancellation, real no-route failure and late actual settlement after a stale status write.
The isolated adapter's three wire-status tests and build passed. Its executable SHA256 is
`6a752d8fb498b5757e8e6efc68e64f04fe3732f61010fe2d55f2c9df9d9637d1`.
Native run `d4286ba5-946c-4877-8401-3194cb3cd67c` exercised this plugin with actual no-route and failed held payments,
confirmed offline refunds, and late settlement after a stale local status. Those cases passed.
This does not qualify ambiguous transport failures or authorize changing a live node.
Stock plugins remain supported through the conservative observed-group path.

Economic discharge is source-specific. A preimage learned from one paid invoice does not discharge
another signed source sharing its hash. The backend retains global preimage knowledge, but requires
that source's own execution receipt before assigning its value to the operator.

## Ambiguous-dispatch recovery

A socket failure or generic error can leave a remote command alive without a recorded sendpay part.
Neither an empty `listpays` result nor a local Failed row proves quiescence.
An operational recovery must stop new admission to the affected backend, finish or cancel all local dispatch tasks,
then gracefully restart the actual Lightning node through its authorized supervisor.
Verify the old process and its plugins have ended before reconnecting to the same node identity.
After restart, inspect its persisted payment/HTLC history: pending parts still block a refund. Store every valid preimage, then separately establish which source funded the settled obligation.
A new node epoch plus closed local dispatchers eliminates the old queued command; a plugin-only restart is not a proven equivalent.
Persist the exact source/attempt/node and operational evidence before releasing funds under the source locks.
This runbook is a recovery requirement, not an implemented automatic restart policy or authorization to restart a live node.
