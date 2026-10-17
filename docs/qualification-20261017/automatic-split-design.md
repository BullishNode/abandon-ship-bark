# Automatic oversized split: bounded native acceptance case

Source inspected in isolated `/home/francis/abandon-ship-fresh-integration/testing-track-worktree`, based on published `e44ddf95741478508908c17adc61752832d75c6a` plus testing-only checkpoint `bc3d`. No product edits, compiler, metadata, build or runtime work is authorized for this design phase.

The case is source-feasible with **327 payees**, avoiding a 1,296-plus-payee full balanced tree. Its physical/economic geometry is deliberately asymmetric:

```
real confirmed board, 327 * 5,000 sat user principal
  checkpoint / signed fanout
    324-payee branch -> checkpoint / signed fanout -> four 81-payee branches
    one payee       -> signed claim-all checkpoint -> final user leaf
    one payee       -> signed claim-all checkpoint -> final user leaf
    one payee       -> signed claim-all checkpoint -> final user leaf
```

The 324 branch is recursively divided into at most four balanced positive integer groups until groups contain at most four recipients. Every intermediate branch is a genuine newly cosigned, user-cosigned and registered Ark-to-Ark operation. Deep final user leaves are never physically exposed: each ready 81-payee branch can pay directly from its retained checkpoint backing.

The three shallow recipients need their additional **genuine claim-all checkpoint operation**. Directly splitting into final ordinary user leaves would remove the server's expiry path, and `split_recovery_paths` correctly refuses it. No fabricated ephemeral keys or test-only split instruction can bypass that requirement.

## Unchanged limits and declared harness differences

- Production payout cap stays **10,000 vB**, in both `ReadyOutput::group_fits` and `Watchman::build_expiry_payout`. A P2WPKH `TxOut` serializes to 31 bytes. **324 payout outputs alone occupy 10,044 bytes**, before transaction/header/input/witness/operator outputs; both the 327-recipient root and 324-recipient subtree necessarily exceed the real cap. The test serializes actual expected `TxOut`s to prove this lower bound, rather than assuming a rough 318-payee limit.
- Server fanout stays **4**, production exit-depth cap is **100**, production lifetime **4,320 blocks**, and exit delta **144**. Set these explicitly because the repository harness normally uses depth 50, lifetime 432 and delta 12. The longest logical fanout depth is six, approximately thirteen genesis steps including the board; actual `exit_depth()` is asserted below 100 on every genuine request.
- Owner scanning keeps production **10,000 children/chunk and two workers**. Correct native mailbox posting identifies candidates but never bypasses key derivation or registration verification. Source review corrected the initial coarse derivation estimate: the production scheduler starts each new per-key/account range with **32 children**, growing later spans up to the unchanged configured maximum. All 327 true keys are at child 1, so their first successful record passes derive 10,464 children in total; fair-scheduler and repeated-prefix work can add to that. About 107,256 per-key/account cursor pairs can initially exist for 327 pending keys and 328 registrations. No actual timing, total scan-work or peak-storage measurement is claimed before execution.
- **Harness cadence remains explicit:** repository watchman reactions/sweeps run every one second, versus production 10-minute/2-hour timer defaults. The two-block progression grace and actual chain maturity are preserved. This qualifies native size, geometry and money behavior; it does not measure production wall-clock emergency response under those timers. This difference was declared to the root before any run.

## Exact native APIs and bounded dataset

Use one real PostgreSQL/Core31/captaind/watchmand test context, one manually scheduled native board wallet, no CLN, no recipient wallet processes, serial RPC work and no financial SQL mutations.

1. Create 327 distinct cryptographically random account Xprivs, coin keys at normal child index 1, mailbox keypairs and independently random standard P2WPKH payout scripts. Geometry is reproducible by public owner index; private keys/seeds are never written to the receipt, and no deterministic private-key replay recipe is needed. Sign `ark::fallback::FallbackRecord::new(Network::Regtest, script, &account, &mailbox)` and register each using real `SetFallback(SetFallbackRequest { mailbox_pk, record: record.serialize() })`. Require byte-identical authenticated replies. This is native configured-script registration, not external BIP84 app acceptance or seed recovery evidence.
2. Use **one source actor** funded with 2,000,000 sat and board exactly **1,635,000 sat**; confirm/register with the existing helper. The native source adds one fallback record beyond the 327 payee records: a real read-only count must observe **328 registry records**. Fund the distinct watchman using **16 independently confirmed 200,000-sat outputs**: 3,200,000 sat of isolated regtest funding, with recorded exact outpoints. Total reserved funding is **5,200,000 regtest sat**. Real funding anchors/residuals are measured, not hardcoded as user principal. The transaction-DAG conservation oracle counts the actual consumed backing and sponsors, not unused source-wallet change or unused sponsors.
3. Each operation uses `ArkoorPackageBuilder::new_with_checkpoints`, `generate_user_nonces`, real `request_arkoor_cosign`, `ArkoorPackageCosignResponse::try_from`, `user_cosign`, `signed_virtual_transactions`, `build_signed_internal_vtxos`, `build_signed_vtxos` and native `register_vtxo_transactions_with_server`. Root topology [324,1,1,1], balanced recursion over 324, plus three shallow claim-all operations gives **157 cosign/register pairs**, checked by the fixture counter. There are 153 operations in the balanced 324 subtree, one initial root operation and three shallow extensions. Every request has one real input and at most four outputs; no account/policy limit is reduced.
4. Deliver each final signed ordinary leaf through real `PostArkoorMessage`, with `MailboxIdentifier::to_blinded(server_mailbox_pubkey, &actual_leaf_key)`. There are 327 single-leaf posts. These are native deliverable payment objects, not fabricated pending-owner scan keys. No recipient is opened after abandonment.
5. Total nominal workload: 327 signed payee registrations, **157 native cosign + output-registration pairs (314 RPCs)**, one initial genuine board-path registration, 327 mailbox posts, one board and sixteen fee-funding outputs. Native wallet setup/opening also authenticates the source's own registration; these explicit workload counters do not pretend to count every RPC made by wallet setup. The receipt names completed operation **pairs**, successful payee registrations, actual registry count 328 and source actor count 1 separately. No concurrency increase is implicit. In-memory public path material is only a few MiB, but real PostgreSQL cursor/index/WAL and Core footprint is unmeasured; request a bounded 1-GiB scratch allowance rather than calling the source estimate a measured disk result.

## Fixed deadline and execution gate

Implement as an explicitly ignored heavy integration case, with an **unchangeable 600-second enclosing timeout** including setup and execution. Before the first run the coordinator must agree to 327 registrations/1,635,000 user sat/3,200,000 sponsor sat of isolated regtest data, a serial runtime slot and source/binary pins. No public Signet campaign follows from this case. Do not extend the deadline after observing a failure.

Use the existing `fallback-scale` nextest profile so its external process timeout does not kill the declared 600-second case at the default 240 seconds; the case's own timeout remains the actual budget. Proposed exact recipe after allocation:

`NEXTEST_PROFILE=fallback-scale NEXTEST_TEST_THREADS=1 KEEP_ALL_TEST_DATA=1 TEST_DIRECTORY=<allocated-private-directory> just int 'expiry_automatic_real_limit_multilevel_split_pays_every_owner --run-ignored all'`

No new test profile, lower payout limit or direct split call is required. The root registers the new module; the delegated file writer owns only `qualification_oversize.rs`.

## Oracle and retained evidence

Wait for actual automatic publication. Mine only regtest blocks, respecting the configured two-block grace, and follow known signed transactions through board exit, root checkpoint, root fanout, 324-child checkpoint/fanout and four ready 81-child checkpoints. The enclosing 600-second timeout bounds all polling and mining; a timeout remains FAIL.

- Require durable automatically created split requests at the original physical anchor and at **both** oversized checkpoints (327 and 324 recipients), with the exact corresponding signed progress txids. The test never writes those requests.
- At every published level, independently require the exact signed parent in Core, version 3, parent <=10,000 vB, zero parent miner fee from actual previous outputs, and its actual canonically confirmed P2A sponsor <=1,000 vB. Locate the actual sponsor by spending the exact anchor in the canonical block, without assuming its change-output shape. Each next physical level must confirm in a strictly later block than its actual parent. This is real Core acceptance, not only local weight arithmetic.
- Every one of the 327 distinct registered payout scripts receives **exactly one 4,000-sat output**, with at least six canonical confirmations. Total customer payment is **1,308,000 sat**. SQL reads reconcile each actual terminal coin's gross 5,000, fee 1,000, forfeited zero, one active confirmed allocation and actual paying transaction/output. Intermediate nodes must not acquire extra customer entitlements. The grouping count is observed, not asserted as a made-up policy; per-output fees follow actual outputs.
- Reconcile the complete published transaction DAG using real Core previous outputs: external inputs are precisely the original physical backing plus declared watchman sponsors; no outpoint is consumed twice; all remaining DAG outputs exist with correct values/scripts; external value equals actual remaining outputs plus canonical miner fees. Account each fee child once. Do not double-count intermediate split backing as a new economic payment.
- Retain a small incrementally written sanitized receipt with configuration/deadline, exact case/operation counts, signed public path txids, automatic split requests, canonical per-level blocks/sponsors/weights/fees, all 327 payouts, and conservation totals. Private seeds/keys and raw wallet databases remain private test artifacts. Preserve partial receipt and logs on the first failure.

This case does not qualify split reorg/restart, user exit during splitting, an unregistered branch, maximum capacity, sustained throughput or public Signet operation. Those remain separate scenarios after the first real-size automatic case.

## Source review handoff

The implementation is confined to the unregistered new `qualification_oversize.rs`; the root owns module registration and eventual integration. Source review verified the 157-operation-pair recursion, required shallow checkpoint extensions, public RPC signatures against existing native cases, real mailbox blinding, actual canonical anchor-spender discovery, six-confirmation payout and sponsor accounting, and the declared-funding boundary in the full DAG oracle. Random private account/payout material remains in memory, while receipt reproducibility uses public owner indices and transaction paths. No product behavior or limit was changed.

No Cargo, metadata, compiler, check, test, service or campaign ran for this case during delegated preparation. Exact API/type compilation, observed cursor/disk footprint and the fixed 600-second runtime remain unqualified. The runtime still needs the coordinator's resource allocation and the shared build lease; a source-complete fixture is not execution evidence.
