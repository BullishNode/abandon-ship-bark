# Backend test inventory, 2026-10-17

Inspection pin: `e44ddf95741478508908c17adc61752832d75c6a` in `/home/francis/abandon-ship-fresh-integration`, using `git show` only. Canonical dirty client work was not touched. No builds, tests, services, campaigns, funding or cleanup were started. The disk hold remains active.

Read the fresh prompt and Signet handoff, both repository agent instruction files, running/debugging and writing-test skills, final plan revision 5 including accepted corrections, client correction references, simulation network constraints, current coordinator status and pinned backend evidence documents. Host `which captaind`/`IN_NIX_SHELL` did not identify a Nix shell. The parent independently verified the existing container Nix environment; source/mount and shared build allocation remain pending.

`backend-inventory.csv` contains 59 existing named tests and 13 proposed gap scenarios. All results are **NOT RUN by this testing track**. Existing tests are source inventory, not fresh PASS evidence. This is a backend slice, not an audited count of legacy cases; original simulator IDs are deliberately left to the catalogue mapping owner. Some multi-order proposed rows require more than one concrete test. No scenario total should be inferred from unit assertions, test invocation counts or payment counts.

## Existing coverage worth reusing immediately

- `expiry_payout_native_board_seed_restore_and_confirmed_spend`: actual automatic ordinary payout, independent Core parent/child conservation, six confirmations, seed-only restore and confirmed spend. This is configured native recovery, not the distinct external BIP84 app test.
- `expiry_decision_wins_over_prepared_native_offboard` and `expiry_registered_native_offboard_wins_over_waiting_decision`: real signed offboard and actual payout decision in both lock orders. Losing registration rolls back its financial insert. The first retains the prepared checkpoint/action holder after refusal; completion of the client finality behavior is separately owned.
- `expiry_actual_partial_exit_conflict_regroups_after_restart`: two genuine ready roots first form one merged decision with one fee. An actual holder exit then conflicts one input; the intermediate exit is not paid; terminal claim and the unaffected root's six-confirmation payout reconcile after restart. The initial merged parent is intentionally never published because the watchman is unfunded.
- `expiry_actual_payout_reorg_preserves_receipt_and_fence` and `expiry_actual_deep_payout_reorg_preserves_receipt_and_fence`: real Core forks at depths 6 and 101, actual daemon restart, immutable parent and unique active/confirmed allocation. These do not cover split, conflict-after-regroup or chain-only HTLC ownership reorgs.
- `expiry_payout_fee_replacement_keeps_parent_and_counts_only_confirmed_child`: real watchman child replacement, independently measured fee, immutable parent, canonical confirmed child accounting. This is configured regtest fee rise, not public congestion.
- `expiry_actual_failed_external_send_pays_offline_owner`, `expiry_actual_no_part_external_send_pays_offline_owner`, and the two `expiry_unused_same_hash_grant_*` tests provide genuine source-specific external Lightning evidence. They do not establish autonomous internal/cross-tree closure or ambiguous dispatch crash recovery.
- Attribution tests cover late lower keys, per-key cursor isolation, stale prepared work, wrong mailboxes, registry changes and actual daemon worker operation. Exhaustive terminal no-owner coverage uses a documented prefix fixture. The ignored authenticated RPC scale case has explicit 120-second budgets; the older direct-DB ignored resolution benchmark has no deadline and should not be first work.

## Historical-forfeit/current-cut oracle limitation

`expiry_frozen_recipient_is_paid_after_sender_change_refresh` reproduces genuine transfer, accepted change-input forfeits, current economic-cut selection and actual recipient conservation. It explicitly accepts at most two active payout rows/parents and checks:

`total_actual_customer_outputs + 1000 * actual_parent_count == original_recipient_gross + received_gross`.

Consequently, a run paying the two same-expiry roots in two outputs is still a PASS for that test. The result must not be credited as proof of strict one-output grouping, and must not retroactively replace the original Signet S03 one-output oracle. `expiry_actual_partial_exit_conflict_regroups_after_restart` does assert one combined output and one fee in its initial decision, but deliberately prevents publication. A dedicated healthy merged-publication case remains useful.

## First bounded runtime batch after explicit hold release and allocation

Run serially in the allocated Nix container, using one private `TEST_DIRECTORY`, repository `just int` recipe and exact-source or explicitly approved immutable artifacts. Do not invoke direct `cargo test` or take a private lock as permission to write the shared target.

1. `just int expiry_frozen_recipient_is_paid_after_sender_change_refresh`
2. `just int expiry_decision_wins_over_prepared_native_offboard`
3. `just int expiry_registered_native_offboard_wins_over_waiting_decision`

If the artifact allocation cannot satisfy the exact published pin, record each product/test source separately and call it interoperability evidence. Record executable/metadata hashes, exact command, run IDs, timings, Core state, and preserved first failures. This batch reuses already implemented high-value tests before new framework work. Add the ready/unready isolation case next; do not wait for full catalogue completion.

## Minimal next implementation: independent ready/unready roots

Proposed name: `expiry_ready_root_does_not_wait_for_unresolved_same_expiry_root` in a new server integration test file, registered in the existing server test module. No product patch is needed to express the oracle.

1. Reuse the actual distinct-root construction in `expiry_actual_partial_exit_conflict_regroups_after_restart`: one manually scheduled actor boards twice before mining, then confirms/registers both. Assert distinct anchors and equal expiry. Disable automatic client protection through the established test configuration so the actor stays offline on the intended schedule.
2. Leave one coin ordinary. Convert the other through genuine `request_lightning_pay_htlc_cosign`, user cosign and `register_vtxo_transactions_with_server`, following `expiry_uninitiated_grant_can_revoke_after_backing_expiry` or the mixed-split fixture. Use a fresh hash and a never-initiated HTLC deadline later than tree expiry plus the six confirmation blocks. Assert its source has zero attempts through a narrowly scoped read-only query. There is no need for a CLN process.
3. Fund the distinct watchman wallet with confirmed regtest funds before expiry. Drop the client handle and do not reopen/sync that actor during the observation interval. Mine past equal tree expiry, keeping the HTLC deadline ahead of both daemon tips.
4. Require the actual background worker to publish the ordinary root's parent within a bounded 30-second wait. Check Core sees the exact stored parent and fee child; the parent spends only the ordinary root and pays its authenticated destination `gross - 1000`. Check the fee child against actual previous output values.
5. Before mining and again after six confirmations, require no entitlement/fence/active allocation for the unresolved HTLC, no spend of its backing root in Core, and no `expiry_split_request` for that root. Before its admitted deadline, ownership code sets `needs_chain_progress = false`; do not enforce this no-progression condition after the deadline.
6. Require the ordinary output to have six canonical confirmations while the HTLC remains pending and the actual tip is still before its deadline. Read both wallet-independent chain state and server durable entitlement state. The decisive failure is absence/delay of the ordinary real payout, premature HTLC/operator payout, or lost backing conservation.

This directly exercises W6 readiness isolation between independently ready tree outputs with equal expiry. The existing mixed-tree split case covers different geometry: an ordinary and unresolved HTLC sibling under one root, with an explicitly requested first split.

## Subsequent release evidence gaps

Priority gaps in the CSV are genuine refresh completion/forfeit and Lightning send/receive grant races in both orders; strict healthy same-expiry merged publication; unconfirmed-only watchman funding/warning/replenishment; automatic oversized/multilevel split and split rollback; conflict rollback after regroup; chain-only HTLC ownership rollback with retained preimages; real delayed-frontier listener versus payout/broadcast; ambiguous dispatch restart; autonomous native cross-root internal Lightning discharge. Existing unit fixtures or ordinary reorg tests do not close these requirements.

No product defect is asserted solely from a missing test. The known 10,000-record wrong-mailbox deadline failure remains a FAIL in existing evidence, not a production-scale PASS. No runtime or production-readiness conclusion follows from this source inventory.
