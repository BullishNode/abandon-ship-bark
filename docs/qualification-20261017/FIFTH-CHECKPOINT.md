# Final paired runtime qualification, 2026-10-17

Both coordinator-allocated native money cases passed on the final v2 paired runtime. The track now records **27 executions: 25 PASS and two FAIL**, across **24 unique test names and 15 nextest invocations**. Twenty-five executions are native and two are mailbox components. Repeated names and other tracks' unit/native results are excluded from unique-scenario credit.

| Native case | Result | Run ID | Evidence |
| --- | --- | --- | --- |
| Registered receive, absent recipient, permanently stopped provider, actual fallback and seed recovery | PASS, 36.184s | `61d55416-31d6-4704-8f17-dbc8036178bd` | `batch14-v2-registered-receive-fallback-receipt.json` |
| Actual external send Accepted then Cancelled, offline original sender, rightful fallback | PASS, 52.392s | `e968e1cf-20d6-43a9-aad8-24f592bee113` | `batch15-v2-failed-send-refund-receipt.json` |

The first case received a genuine 10,000-sat native invoice with the fixture's explicit zero receive fee. Fully signed recipient outputs were registered at durable Delivering. The recipient was absent before invoicing, and the provider stopped permanently after registration. The recipient's authenticated BIP84 fallback received **9,000 sat with at least six Core confirmations**, plus one actual 1,000-sat payout-output fee. Actual parent previous outputs and outputs conserved value. All customer allocations and canonical customer outputs across the backing root belonged to the recipient, with no provider or ancestor refund, checked again after the retained HTLC deadline and completed daemon/ledger rebuild.

Only after those assertions, a new database using only the recipient seed and genesis birthday authenticated the exact receipt and discovered the exact payout UTXO. The actual BIP84 recovery transaction spent only that 9,000-sat output, paid the chosen destination and a positive independently computed miner fee, and confirmed at least six times. Destination value plus recovery miner fee plus payout fee equalled the original 10,000 sat. No original database, raw transaction, address or VTXO was imported.

The second case used a genuine 90,000-sat external invoice and signed registered 100,000-sat source. The external receiver accepted then cancelled; the original source obtained its real CLN terminal-failure proof without a settlement preimage. After dropping the SDK client, no further client call was made. The rightful registered fallback script received **99,000 sat with at least six Core confirmations**. The actual parent conserved its previous-output total, dispatch count remained one, and no cooperative user revocation was manufactured. The SDK used `run_daemon: false`, and wallet CLI commands completed before abandonment; no persistent wallet actor supplied the refund.

This existing failed-send test does not establish sender seed recovery, completed confirmed-ledger projection, all-lineage absence, cancellation during product downtime or reorg handling. The stronger downtime-cancellation draft remains reviewed source only. The recipient test covers normal POST-registration native forwarding; stock Noah/LNURL, required-proof admission, pre-registration reply loss and the public Signet receive-fee profile remain separate gates.

## Exact candidate

Both runs rehashed all six immutable executables, runtime/source/metadata manifests and three actual CLN runtime pins before dispatch. No shared-target writer or new compilation was used by this track for these runs.

| Artifact | SHA256 |
| --- | --- |
| Exact dirty source manifest, HEAD label `6950be5a6855625d76d04e33c86a6b030cd9406e` | `c60c5cd4c9d8cc0fbeaa8f8f6a71ba4102a46075cd2d9515fb505d9ba2dc882b` |
| Runtime manifest | `3c4df44d92a60683fcc6923e6e36a1eb1696e2f3755a0b3256086c9fce5bfee9` |
| Immutable nextest metadata | `d307ebbd308b9e7087edf240f12ed94ea1fb911bbe023e33c0c249c0e47adbfb` |
| captaind | `592df297c9b58b22261b7487c57a4076bba72b735232707d98275f84981642a8` |
| watchmand | `edf59b0b352f8aa0a5ef76e8cc306b3c591836b73181baf48b3ce46da0764bbf` |
| bark | `7a240c143bb103b67c360a75316034211d6212947e0b65571ceaebe19fc06ba4` |
| barkd | `570ecf6cbbdc441b1262e68ee84035718ac9f72a5f881038b9ca51812a342218` |
| native server tests | `9913a696a7754cc68f4792e09c531330cf252de380ef031aff758c0367a8104c` |
| native SDK tests | `e14ca0432313af170fc9e063e7afe6c37789970cad9897ed081c14489fa0b547` |

All four products and paired test binaries came from that exact source capture. The current CLI pair was used throughout; older archived CLI binaries were not substituted. The unchanged recipient test source hashes to `56b1553c67864161ea3266d9f51e38d50f3939d2de8b32f697352e68447e911a`; the unchanged existing failed-send fixture hashes to `7511a38dd772c57f753c26731df86e39824d3163642f5be30e7db282e4e612d9`. The recipient source is committed separately as `0870bbbb915f133b98b00b5c3d7dc2a2a2ac0bc5`.

The manifest records backend-owned required checks, 798 passing unit tests/one skip and product builds. These are supporting provenance, excluded from this track's execution counts. The changed Nix CLN derivation was **not built**. Actual runs used the separately pinned ad-hoc gRPC adapter `6a752d8fb498b5757e8e6efc68e64f04fe3732f61010fe2d55f2c9df9d9637d1`, with the exact hold plugin and CLN executable listed in each receipt. Default packaging remains unqualified by this evidence.

Existing final-v2 failed-send logs corroborate the included child's **2,281-sat fee**: Core logged the zero-fee parent and a block containing the pair with 2,281 sat aggregate fees. This is bounded log corroboration, not independent reconstruction of all child previous outputs. Exact operator remainder and subsidy remain unavailable; no property follows from absent warnings. See `final-v2-failedsend-fee-log-evidence.json`. No actor/database/RPC was reopened, and no additional test is counted.

## Preserved failures and remaining gates

The original stock-plugin no-part timeout in batch09 remains FAIL. Its unchanged bounded terminal-proof oracle failed because the stock transport omitted required typed error metadata. A separately owned adapter-positive run does not erase it. Batch12's first automatic-split test remains FAIL: the fixture assumed one checkpoint where the native builder produced four. It stopped before initial path registration and expiry, so it supplies no automatic-payout product verdict. The isolated corrected graph is uncompiled/unrun and excluded from this source checkpoint.

All 80 original catalogue IDs are preserved; the mixed inventory rows are not a unique executed-scenario count. Wrong-mailbox 10,000-record search still failed its 120-second bound after 9,902 work operations, which are not necessarily distinct records. Complete legacy adaptation, sustained production load, public Signet repair and authentic stock Noah offline LNURL, broader reorg/conflict/backup recovery, required-proof receive and default adapter packaging remain open. No production-readiness claim is made.

Both approved native runs and all owned Cargo work have ended. Failed evidence and the latest money-path raw evidence remain preserved privately. Further runtime work requires another coordinator allocation. This checkpoint publishes only the direct recipient test and curated sanitized reports; it does not merge, promote or deploy a product candidate.
