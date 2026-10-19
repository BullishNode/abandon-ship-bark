# Structured xpay errors

This patch targets Second's already pinned Core Lightning `v26.06.6` source.
The original `cln-grpc/src/server.rs` SHA256 is
`9eca049f3530b28aa2d8f2a49bdb2ccf3a318e60976c411b21fa83e00ec815ad`.
The `cln-grpc` Nix package builds the patched plugin separately. The server package
exposes it as `requiredClnGrpc`; configure the Lightning node to load that plugin.

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

The patch includes adapter unit tests for metadata encoding, transport failure and
an in-progress error. Backend tests cover receiver cancellation, no-route failures
and settlement after a stale local status write.
Stock plugins remain supported through the conservative observed-group path.

Economic discharge is source-specific. A preimage learned from one paid invoice does not discharge
another signed source sharing its hash. The backend retains global preimage knowledge, but requires
that source's own execution receipt before assigning its value to the operator.

## Ambiguous-dispatch recovery

A socket failure or generic error can leave a remote command alive without a recorded sendpay part.
Neither an empty `listpays` result nor a local Failed row proves quiescence.
The sender's coin remains pending until there is evidence that the payment has
settled or cannot complete. Automated recovery and an operator resolution command
are not implemented. An empty payment history or a plugin restart is insufficient
to release the coin.
