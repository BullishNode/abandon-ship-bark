# Fallback implementation evidence

This branch contains the fresh xpub attribution and sweep-to-payout implementation.
The upstream base is `bfbe6b7c3088a09dd4e81c4ac24cdc763a627a8c`.
This checkpoint extends the checked registration, attribution and wallet adapter milestones.
It does not qualify a production deployment.

## First native payout

The native test `expiry_payout_native_board_seed_restore_and_confirmed_spend`
passes against actual PostgreSQL and bitcoin Core 31 processes in Second's test framework.
Run ID: `27d5e036-045e-497a-a376-9692d569d50a`. Runtime: 32.313 seconds.

The test performs these operations without allocation, spend-fence or sync-ack fixtures:

1. Register and confirm an actual board. Issue 250 wallet addresses before the board.
2. Fund the distinct watchman wallet with confirmed bitcoin.
3. Leave the customer wallet offline through expiry. Use one derived key per owner-scan chunk.
4. Require the background worker to decide and publish a payout. Require no split request while attribution is pending.
5. Check the actual parent and fee child in Core. Check version 3, the parent size limit, zero parent fee, the exact customer amount and the actual child fee.
6. Require six confirmations of the actual payout output.
7. Restore a new wallet database from the seed phrase. Require it to find the payout and spend it in a transaction that Core confirms.
8. Require the stored payout parent bytes to remain unchanged.

This is the generic configured-destination payout test. It is not the separate external BIP84 wallet adapter acceptance test.
The Signet campaign uses separate evidence and receives no qualification credit from this regtest result.

The first run failed. Run `4099d0b2-73fb-42da-b143-eb05a268eaf5` timed out after 59.794 seconds.
The readiness worker attempted `SELECT FOR SHARE` in a read-only database transaction.
PostgreSQL refused the operation, and no payout was published.
The corrected worker uses a short write transaction for token validation and the shared lock.
The successful retry keeps the same test scope and timeout.

Local evidence is retained in `test/fresh-runtime-native-payout-20261016.log`,
`test/fresh-runtime-native-payout-retry-20261016.log` and `test/evidence/ordinary-payout-first-failure-4099/`.
Source, product executable and native test executable hashes are retained in `test/evidence/ordinary-payout-pass-27d5-*.sha256`.
The published source removes one redundant blank line at the end of `server/src/expiry_readiness.rs`.
The exact passing file is also retained as `test/evidence/ordinary-payout-pass-27d5-expiry_readiness.rs`; its hash matches the original manifest.
This whitespace correction does not change executable code. The final workspace check covers the published text.
These local artifacts can contain operational details and are not committed.

## Other checks

The runtime workspace checks include native tests and examples, WebAssembly and repository prechecks.
The focused unit gate passes 24 tests: run `e48fbe5a-49d3-486f-b497-f93e528c4096`.
The tests include backing conservation, source geometry, terminal claim recognition,
script-specific allocation, operator remainder and fee subsidy accounting, attribution boundaries and typed refusal metadata.

The additional native gate passes three tests: run `b45cad52-44a3-4cf3-829b-09e4d68198e0`, 25.053 seconds.
It checks stale rebuild rejection and restart fencing, actual typed RPC refusals with full and bare database queries,
and an abandoned registered Lightning grant whose sender never initiates payment.
The latter test proves the isolated no-attempt deadline ownership rule and expired first-attempt refusal.
It does not prove a complete Lightning fallback payout or cross-tree settlement.
The product executable hashes match the first successful ordinary payout run.

Earlier W2 and W3 evidence remains in their commits. Their manually inserted spend-fence fixtures establish the shared database writer boundary.
They do not prove every actual payout-decision race against native refresh completion, offboard registration or Lightning grants.

## Authenticated history and external BIP84 recovery

The real server endpoint accepts either the existing server-bound xpub recovery authorization
or the existing per-VTXO status attestation. Account history uses permanent economic-owner associations.
Each page uses one repeatable-read database snapshot. A ledger token detects a changed view across pages.
A metadata-only size check limits transaction-proof bytes before the server loads them.
The server can return a smaller page with the correct cursor. An oversized single history entry fails explicitly.
A long-history pagination or transaction-deduplication design remains an acceptance requirement.

The actual PostgreSQL ledger test passes: run `1091fd95-278a-4b82-b14c-b82a1a43d4a6`.
A transaction with a lower allocated revision commits after another transaction with a higher revision.
The visible commit count changes although the maximum revision does not. A no-op update does not change it.
The same native run checks that cached undecided value remains owned but unavailable for new spends
when Core is actually offline. Both tests pass. The external app test is excluded from this default-feature run.

The feature-specific external app test is
`fallback_external_bip84_actual_payout_seed_restore_and_confirmed_spend`.
Run it with `just int "--features fallback-test-bip84 --test bark-sdk fallback_external_bip84_actual_payout_seed_restore_and_confirmed_spend"`.
Its first complete attempt reaches a real payout, six confirmations, fresh app and Ark databases,
and authenticated account history. It then fails because a historical recovered Spent row causes an extra Ark debit.
Failed run: `cf45a5b9-1838-4d77-8a30-8b1f9b503434`, 23.251 seconds.
No recovery-spend credit is assigned to that failed run.

The corrected client creates a debit only for previously held spendable, exited or action-locked value.
It retains an earlier recorded debit after adoption. Mere presence of a recovery-only Spent row does not create another debit.
The unchanged acceptance assertion requires zero extra debit in the fresh restored database.

The complete retry passes: `8f297156-7884-47da-8a19-640730082b72`, 24.186 seconds.
The external app and Ark wallet use distinct original seeds. Both databases are new on restore.
Core verifies a 99,000-sat payout with at least six confirmations, and then confirms the app's recovery spend.
The spend uses an actual two-item BIP84 witness. Authenticated history returns the real parent and output association.
The local sanitized proof is `test/evidence/external-bip84-pass-8f297-proof.json`.
Its SHA256 is `c8171ae2f3e577e284639464a2acd1806807ce923c38897ab36a2d805815d728`.
The exact source and native test executable hashes are retained beside it.
The test reuses unchanged server executables and compiles the actual BIP84 SDK feature.
It does not qualify BIP84 CLI, barkd, browser, Lightning or Signet operation.

## Requested mixed-tree split

The native test `expiry_requested_split_preserves_mixed_siblings_and_eventual_refund`
passes in run `9d9ee14a-71c4-41da-a18e-32c6189dc6a6`, 42.928 seconds.
The test creates an actual signed mixed tree, explicitly requests its first split,
then requires normal watchman progression and automatic payouts.
The ordinary sibling receives 49,000 sats while the HTLC remains unresolved.
The never-initiated HTLC later receives its own 49,000-sat refund without a client revocation.
Both actual payouts have at least six confirmations and spend distinct physical backing.
The actual database guard refuses normal watchman progression of backing reserved by a payout.

The split authorization proves every non-anchor child retains an unconditional server expiry path
through conserving signed transitions. Publication revalidates those paths under sorted source locks.
A board exit ending in a user-only leaf cannot authorize destructive progression.
The first native run, `5c716a33-566f-4765-8759-2d4ea3e0260c`, failed because its fixture
advanced one block while the configured watchman grace period required two.
The retry advances the actual configured grace; the original failure is retained.

Full workspace units pass 780 tests with one upstream skip in run
`533a0b9f-68eb-46d3-a284-fdec7c51a310`, 13.193 seconds.
That unit run preceded the final normal-watchman guard hook.
The final source passes fresh mandatory workspace checks and the native mixed-tree test above.
The exact 41 source files, four product executables and native test executable are captured locally
in `runtime-mixed-split-9d9ee/manifest.json` under the shared build cache.
No code formatting or normalization is applied between that capture and publication.

This test explicitly requests the first split. It does not prove automatic first-split discovery,
a completed external payment's terminal refund, cross-tree Lightning settlement or oversized-tree capacity.
The external BIP84 restore and confirmed-spend result remains its separately captured run above.

## Remaining acceptance work

- Actual payout-decision races against refresh completion, offboard registration and Lightning grants, with both winners and transaction rollback checks.
- Full settlement coverage, including cross-tree Lightning provenance and actual payment attempts.
- Automatic first-split discovery, wider mixed-tree geometries and actual oversized-tree capacity, beyond the passed explicitly requested split case.
- Actual oversized-tree split, conflict, regroup, fee replacement, fee-wallet depletion and replenishment, restart and reorg cases.
- Authenticated history boundary tests under actual reorg and regroup between pages, and response-size scale measurements.
- BIP84 CLI, barkd and browser acceptance, distinct from the passed external app callback test.
- Abandonment before board registration. Upstream board co-sign does not persist a pending-board record.
- Attribution and operational scale measurements, bounded response sizes, old payout retention and successful public Signet campaigns.

The native happy path does not remove these requirements.
