# Corrected native oversized-tree fixture: source-only handoff

The isolated fixture now matches the real checkpoint-per-destination graph on canonical `751bd5a5d8ec9bbf72f0a44c1e7d4ac371b6269d`. This is preparation for the next allocated real split gate, after the internal-settlement compiler allocation. It is **uncompiled and NOT RUN**. The original batch12 fixture failure remains unchanged.

Owned file: `testing/tests/server/qualification_oversize.rs`. Final SHA256: `1d708bd6e4fa9bc80ff5b95d1acb52fc0269278dbb2c5b186107c6e023785366`. The private handoff patch `automatic-oversize-actual-graph-v3.patch` hashes to `55971b55d9d53aac57423cb25d5195177b537a8aa582065985143f3e7e2638a9`. A read-only `git apply --check` passes against the inspected canonical file SHA256 `d61438dc6050c754db04969983d5bd5e204fb33f22ebe7e0a980671bca9c9f1e`. No canonical file was changed by this track.

## What changed and why

The first four-destination cosign returned four checkpoints, correctly, but the original fixture assumed one. The helper now checks four distinct destination checkpoints and each signed branch exit, with exact point, policy, script, amount, expiry, exit delta and server key. This replaces the incorrect graph model and adds concrete graph assertions.

The 327-recipient workload remains asymmetric `[324,1,1,1]`. The three shallow destinations use their final owners directly. The native construction therefore requires **154 cosign/output-registration pairs**, 480 destination exits and 154 checkpoint-fanout transactions; adding the original signed board exit gives **635 distinct signed path transactions**. The declared 963 explicit RPC calls exclude incidental setup/helper calls.

The 324 distinct P2WPKH payout outputs alone serialize to 10,044 bytes. Both the original 327-recipient physical backing and the heavy 324-recipient physical checkpoint must exceed the unchanged 10,000-vB payout limit. The test requires both exact automatic split instructions, authenticates every observed same-anchor instruction against a known signed input/path, and permits additional legitimate native substeps. It does not require a nonexistent shared 327-recipient checkpoint or an exact global request count.

After the real two-block grace, ordinary user-key outputs progress through the existing watchtower. The observer requires actual Core confirmation of the board exit, root checkpoint fanout, heavy checkpoint exit and heavy checkpoint fanout. It includes every additional canonical transaction among the 635 known paths and each actual same-block anchor sponsor in the final DAG.

Review additionally strengthened each customer row's exact mailbox and ownership-key association alongside its script and confirmed output. The exactly-one-owner-output check searches the complete canonical graph, including sponsor children; its associated-parent assertion still binds that sole output to the durable payout parent. The completed-pair counter is now set to one and persisted immediately after the successful root operation, so a later partial failure preserves that completed work accurately.

## Assertions and limits retained

- 327 unique authenticated registrations and genuine native mailbox paths; recipients remain offline.
- Each owner receives exactly one actual 4,000-sat output, with at least six Core confirmations, from 5,000 sat gross and one 1,000-sat output fee; no forfeiture or intermediate customer allocation.
- Stored ownership, script, paying transaction and confirmed block agree with the independently known owner and canonical Core history.
- Exact signed parent bytes; version3 zero-fee parents at most 10,000 vB; actual version3 sponsor children at most 1,000 vB with positive fees computed from previous outputs.
- All graph transactions have six confirmations; no input is spent twice; external inputs are restricted to the real board anchor and declared watchman funding outputs; every remaining output exists with exact script/value; external inputs equal remaining outputs plus independently computed miner fees.
- Fanout4, exit-depth100, 327 owners, 5.2M regtest sat declared funding and the fixed whole-case **600-second** deadline are unchanged. Harness cadence remains explicitly distinguished from production timers.

## Required next gate

Coordinator integration must precede an explicitly allocated shared-target writer. Capture exact current source after integration; compare the fixture hash; compile server metadata with `ark-testing/fallback-test-bip84`; run mandatory checks under that lease; archive immutable binary/metadata/source pins. Use matched current product binaries and record any separately pinned dependency. This review does not authorize Cargo, metadata generation, a runtime or another product build.

Then allocate one isolated serial regtest invocation using the repository `just int-binaries` recipe, the unique test name `expiry_automatic_real_limit_multilevel_split_pays_every_owner`, `--run-ignored only`, `--test-threads 1`, `--retries 0` and a private fallback-scale profile whose outer timeout accommodates the unchanged inner600s. Use a new private directory and nextest store. Preserve failed attempts and reconcile any unknown money outcome before another invocation. Do not restart batch12 actors or reuse its state.

Source review cannot establish actual native acceptance, automatic scheduling, sufficient runtime budget or production capacity. The original run `4a930d92-ac3d-4ec7-b344-38d7b23d2d9a` remains **FAIL16.359s**, before initial path registration/expiry. It registered 327 payees; the first native cosign returned before the incorrect fixture assertion, with zero completed cosign/registration pairs. No product automatic-split verdict follows from that failure.

Three source reviews completed and re-reviewed the final fixture: `oversize-review-current-backend.md`, `oversize-review-current-money.md` and `oversize-review-current-shape.md`. No outstanding substantive source blocker was found in those scopes. The packet JSON records each exact review hash. Both attribution/graph-wide payout gaps and the receipt-counter issue are addressed. This is source inspection, not a compile or runtime PASS.
