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

## Remaining acceptance work

- Actual payout-decision races against refresh completion, offboard registration and Lightning grants, with both winners and transaction rollback checks.
- Full settlement coverage, including cross-tree Lightning provenance and actual payment attempts.
- Mixed-tree progress that preserves ordinary customer recovery. A pure public-key leaf has no server expiry clause. A signed progress transaction alone does not prove automatic fallback remains possible.
- Actual oversized-tree split, conflict, regroup, fee replacement, fee-wallet depletion and replenishment, restart and reorg cases.
- Authenticated payout history and coherent pagination with the paired client implementation.
- The external BIP84 app adapter's full fallback payout, seed restore and confirmed spend.
- Abandonment before board registration. Upstream board co-sign does not persist a pending-board record.
- Attribution and operational scale measurements, bounded response sizes, old payout retention and successful public Signet campaigns.

The native happy path does not remove these requirements.
