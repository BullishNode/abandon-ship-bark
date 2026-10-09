# Native binding recovery of an expiry payout

`native-ffi-client-1` passed in 181.934 seconds against native server image
`expiry-d-c594b92abe78` and fixture client `refresh-boundary-b226bda962f2`.
The generated Python/UniFFI wallet restored from a seed, found an actual unclaimed
round replacement's payout, checked its amount and fee, preserved discovery with
an unknown fee during a receipt outage, and spent the exact output with confirmation.

The matching FFI pins, build hashes, transaction IDs, unit/build checks and limits
are recorded in the [published native result](https://github.com/BullishNode/abandon-ship-bark-ffi/blob/0117b42ac2077e1aa75abb2ef262e625fa05ddd7/tests/results/2026-10-08-native-expiry-payout.txt).
This Linux generated-language journey does not establish mobile-device, browser,
signet or production readiness.

Run `native-client.py` with `EXPIRY_NATIVE_BINDINGS` pointing to matching generated
`bark.py` and `libbark_ffi.so`, an unused `EXPIRY_CASE`, and explicit
`EXPIRY_BARK_IMAGE` / `EXPIRY_NATIVE_IMAGE` values. The script records the binding
hashes. Keep runtime wallets, seeds and generated private data outside Git.
