# Simplification review: release/signature-link-rc

Review baseline: Bark **0.8.0**, `959a489ff4934b181783b41c7d3ee68d85b95cbf`.
Reviewed release: `d89a33fa9f06b5527a3e6eb846abe75e750bf5e2`.
Repository: https://github.com/BullishNode/abandon-ship-bark/tree/release/signature-link-rc

This is a draft plan handoff with an isolated deletion experiment. The release branch was not edited; the PR contains review artifacts and an experimental patch. The original checkout's pre-existing change in `bark-rest/src/api/v1/wallet.rs` was excluded and preserved. The release tree is identical to merged main `8b356f8ae966e206204301b169f1d1169520d554`; that fact does not make tests of different binaries interchangeable.

## Assessment

The net fork is 259 changed paths, 1,012 diff hunks, +48,349/-1,981 lines across 176 reachable commits since Bark 0.8.0. Of the additions, 36,232 are under `testing/`; 10,309 are product code; 1,283 are documentation/changelogs; 525 are build/test tooling and lockfiles. These are line counts, not measures of necessity.

There is removable residue, but the current tree has already removed the owner scanner, account xpub attribution, payout-receipt RPC, history tokens, and several settlement wrappers. Deleting their present replacements as though they were still the abandoned design would undo required behavior. Most retained complexity comes from payout ownership, interrupted Lightning operations, and cold/reorg recovery—contracts absent from Bark 0.8.0.

Use the fresh-fork patch described below as the recommended starting point. It removes dead representations and repeated work while preserving financial checks. Keep broader changes separate, so a small cleanup does not quietly become a protocol, persistence, or recovery rewrite.

## Handoff and evidence

This is a draft review handoff. The release-relative [cleanup patch](signature-link-simplification/cleanup.patch) is an experimental implementation of the proposed fresh-fork design, not a release qualification claim. Apply only to the pinned release; it already includes the initial mechanical cleanup. Do not apply a second cleanup patch on top.

- [Validation status](signature-link-simplification/VALIDATION.md): completed commands, invalidated attempts, native failures and remaining acceptance gates.
- [Per-file ledger](signature-link-simplification/path-review.tsv): all 259 original changed paths, with separate comparison and fresh-fork dispositions.
- [Additional proposed paths](signature-link-simplification/additional-paths.tsv): standard migration files and shared test helper.
- [Commit ledger](signature-link-simplification/commits.tsv): all 176 commits for historical navigation. This is not a claim that every intermediate revision compiles.
- [Exact test deletions/additions](signature-link-simplification/test-changes.json): 604 → 599 test functions in the changed integration files. Seven obsolete scenarios are removed; a real fresh-reopen test is added and an upstream recovery test restored. One constructor-specific SQLite unit test is also removed.
- [Manifest](signature-link-simplification/manifest.json): source, patch, environment and upstream-reconstruction identities.

The complete net review surface is reproducible with `git diff bark-0.8.0 d89a33fa9f06b5527a3e6eb846abe75e750bf5e2`; the history with `git log bark-0.8.0..d89a33fa9f06b5527a3e6eb846abe75e750bf5e2`. Raw compiler/native logs and scratch runners remain outside the product tree. The patch was also reconstructed from a clean 0.8.0 checkout: 1,034 tracked files and executable modes matched, with no mismatch.

Product mechanisms were compared with upstream implementations and callers. Tests were assessed by scenario and oracle; moved assertion helper bodies were byte-identical before extraction. All changed paths are accounted for. Passing tests do not prove all schedules, outages, external consumers or reorganizations.

## Fresh-fork target

The user clarified that this can be rebuilt as a fresh fork from upstream, with fresh paired wallet/server stores. The recommended design therefore has no transition from abandoned fork formats. It must still reopen and restore stores created by this new fork.

A second isolated prototype, the attached `cleanup.patch`, includes the initial cleanup below and replaces the fork's parallel schema bootstrap with upstream's existing migration runners. Its current diff is **46 paths, +388/-939 lines, net -551** relative to the release. The attached patch already contains the first cleanup; do not apply that cleanup twice. Validation for the two variants is reported separately.

The server DDL moves unchanged in substance to the next ordinary migration, `V73__signature_link_fallback.sql`. `Db::run_migrations` and the connection setup return to the exact upstream approach. This removes the fork marker query, extra initialization transaction, migration advisory-lock key, dedicated MigrationConnection and its cancellation/drop task. `nix/package-server.nix` returns entirely to upstream: its existing migrations-directory inclusion picks up V73.

Wallet tables move to an ordinary `Migration0043` under the existing MigrationContext. `SqliteClient::open` no longer runs a separate CREATE TABLE batch on every open. The new migration's version is recorded by the existing mechanism; no fork-specific version table, converter or backfill is introduced.

Delete the tests whose sole contract is applying the separate overlay to a plain upstream database or managing its custom session lock. Replace them with fresh creation followed by concurrent reopen, checking that schema history and expiry generation do not reset. Keep real wallet/server restart, registration immutability, backup restore, seed recovery and payout tests. The extra SQLite test of the removed constructor-owned transaction is also removed; the existing migration runner owns that transaction now, and the wallet record/reopen test remains.

Use upstream's normal one-time creation command before starting both daemons. Simultaneously applying migrations to an uninitialized database is not introduced as another deployment mode. Do not delete or rewrite inherited upstream migrations, their repair commands, MessagePack compatibility code or tests solely because they contain the word “legacy”: they are outside the fork diff, and deleting them would create more divergence. Existing ordinary HTLC encodings are also not old-fork migration tooling.

The fresh prototype also replaces the always-key-zero `terminal_observations` map with an `own_claims` list. Current records retain all their recovery material; obsolete/missing-field snapshots fail ordinary deserialization. It introduces no legacy converter or fallback decoder. The fork-only awaiting-settlement capability probe also goes; existing upstream version checks and direct JSON parsing remain.

A separate comparison prototype preserves the release's initializer. The fresh prototype has its own source path, target directory, compiler logs, unit run and native acceptance subset. Its results cannot be inferred from tests of the first prototype.

## First cleanup: concrete deletion prototype

Current size: **35 paths, +319/-685 lines, net -366** relative to the release. These comparison counts include the new shared test module; the attached fresh patch is the 46-path variant above. The net diff against 0.8.0 touches 257 paths after this cleanup. No SQL schema, protocol encoding, persistent field shape other than the unused JSON cursor, fee formula, confirmation threshold, or financial admission boundary is changed.

| Change | Why it can go | Upstream/reused approach | Verification required |
|---|---|---|---|
| Restore `watchman/frontier.rs` exactly to 0.8.0 | Caller already computes the txid; passing the transaction only recomputed it | Original `Frontier::spend` signature and call | Watchman and actual payout/split flows |
| Restore `watchman/config.rs` | Only an added blank line | Exact upstream file | Diff identity |
| Remove `Db::read_snapshot` and its per-call isolation plumbing | Sole remaining caller performs one SQL statement; the old paged-RPC rationale no longer applies | Upstream `Db::read`; retain the fork's explicit Read Committed setting for existing transactions, preserving statement-level snapshot consistency | Source-specific Lightning outcome/status cases; no write locks removed |
| Remove optional `attempt_id` filter from `lightning_payment_outcome` | Every caller passed `None`; the conditional SQL branch was unreachable | Keep existing full HTLC-set/source query | Native status, same-hash and partial-set regressions |
| Remove duplicate allocation lookup from `lock_unallocated_watchman_inputs` | `guard_obligation_changes` already locks the same coin rows and checks both entitlements and allocation inputs | Existing shared guard and typed refusal | Coin-fence, operation and forfeit races |
| Remove repeated mailbox check from `get_mailbox_entries` | It delegates to `get_mailbox_messages`, which performs the same check before any read, including empty batches | Existing guarded reader | Mailbox writer/reader and registration tests |
| Remove `SnapshotGenesis`, `BackingVtxoFact.genesis/frontier_at`, exported `serialized_bytes` | Production never reads those representations. Full path availability and local row/byte budget checks remain | Keep upstream VTXO decoding and existing bounded inventory | Bare/malformed path, metadata mismatch, budget, split and scale cases |
| Make `check_partition` return `Result<()>` | Production discards the sorted report; only tests inspect its redundant totals | Preserve all conservation, uniqueness, coverage, residual and budget checks in place | Existing negative geometry tests plus actual full payout accounting |
| Remove `FallbackSnapshot.ordinary_spend_cursor` | No reader or writer exists; both SQLite and adaptor store JSON, whose unknown fields can be ignored | Existing serde defaults; no new migration | Reopen old JSON with a non-null cursor, preserve every live set, write and reopen again |
| Make `automatic_exit_ownership_known` return `bool` | It cannot fail; the report's ownership-error branch was dead | Direct predicate | Protection isolation and ordinary-spend/automatic-exit race cases |
| Restore upstream `history()` expression | Unrelated cosmetic churn | Exact original expression | Existing movement/history tests |
| Drop `supports_exit_fee_margin` | 0.8.0 already has the flag; its nine callers can use the original version predicate | Upstream `is_bark_version!(> "0.7.1")` | Existing CLI exit tests; retain the separate awaiting-settlement capability check |
| Remove seven registration fixtures from unrelated Postgres tests | These tests only exercise node CRUD, block tables, sweeps, nursery, tweaks, integration config, or wallet changesets | Original independent fixtures | Entire Postgres test module; all VTXO/mailbox fixtures keep registration |
| Move identical test helpers into `expiry_test.rs` | Three scenario files repeat the same balance fields, rebuild wait, actual fee lookup, wallet-owned coins and Epoch type; two repeat the same 113-line financial cut | A small test module, keeping independent chain-derived assertions | All three callers, all five script dust boundaries; exact original body hashes |
| Remove two scanner-era tests | One only pays indices 500 then 17, without doing recovery; the other pays index 20001. Index scanning no longer exists, and the paid-to-signed-owner behavior remains covered | Keep registration, no-mailbox, HTLC, revoked-input and gift payment cases | Retained attribution suite and real seed-only recovery tests |
| Remove `history_payment_method_queries_preserve_order_and_variant_identity` | It was added for the deleted custom projection; both methods now use upstream persistence directly | Retain upstream `test_get_movements_by_payment_method` plus real payout/reorg/history tests | Persister suite and fallback adoption suite |
| Restore upstream `recover_vtxos_by_xpub_disabled` test | The optional upstream xpub-recovery RPC still implements the documented zero-key disable setting. Its deleted test had no feature-based reason to go | Exact original test body | Execute the restored native RPC test |
| Delete obsolete changelog `ark-lib/32`; trim old history projection and rebroadcast claims | They describe removed xpub registration, history projection and automatic claim rebroadcast | Keep current signature-link changelogs | Compare every remaining claim to current code |
| Correct recovery/operator/protobuf/config comments and one Nix blank line | Comments promise removed history tokens/account-history RPC or attach a depth explanation to round interval | Describe actual rebuilt state, signed-spend evidence and on-chain verification | Documentation/source comparison and whitespace checks |

The one-statement `SELECT` uses a coherent snapshot in Read Committed, as specified by [PostgreSQL 17 transaction isolation](https://www.postgresql.org/docs/17/transaction-iso.html#XACT-READ-COMMITTED). The prototype keeps the existing explicit Read Committed setting for all other transactions, so it does not make their behavior depend on a role/session default.

The test consolidation deliberately leaves each scenario's payout selection and confirmation loop independent where they differ. It does not introduce a generic scenario framework, configurable test DSL, or production-derived expected accounting.

## Mechanism-by-mechanism upstream comparison

| Mechanism | What is reused from Bark 0.8.0 | Why the remaining extension is necessary |
|---|---|---|
| Signed coin-key → mailbox link; immutable mailbox → payout script | Mailbox blinding/unblinding, Schnorr/secp256k1, tagged hashing primitives, address delivery extensions, ProtocolEncoding | An unsigned mailbox route is delivery information, not a coin owner's payout authorization. The new signature and registration bind different authorities. Keep their domain/network separation and malformed-input vectors. |
| Registration during open, reconnect, normal/recovery mailbox reconciliation | Existing tonic channel and server-key pinning | Offline creation and lost acknowledgements need a pending local binding. Upstream `OnceCell` cannot remove a cached authenticated connection on failed/cancelled refresh. Current code already reuses warm channels. Reverting to OnceCell changes this failure contract. |
| Admission validation plus free tree-writer validation | Existing RPC parsers, `VtxoTreeUpdate`, SQL transaction boundaries | Early rejection prevents invoice creation, flux retention or other side effects. The database writer protects non-RPC callers and commit-time changes. Neither boundary replaces the other. Private padding and provider-owned pool outputs need explicit internal exemptions. |
| Expiry fences and cancelled-participation tombstones | Existing coin rows, row locks and participation state | Captain and watchman are separate processes. Process-local flux locks and upstream `Spent` cannot distinguish a committed fallback decision from financial payment or stop a stale forfeit replay. Keep the shared durable checks and stable refusal codes. |
| Complete bounded inventory and economic/geometric cuts | Full VTXO decoding, signed path validation, transaction identities and funding output metadata | Validating one signed path does not prove it covers all live sibling rights or preserves residual operator capital. Keep row/byte budgets, complete cut conservation and checks immediately before the private proof boundary. |
| Allocation, entitlements and canonical payout mapping | Existing transaction/database APIs | Upstream watchman drains recoverable provider capital. It has no customer payee mapping, fee ceiling, once-only entitlement accounting or source-scoped Lightning refund. Replacing this ledger with a spent flag loses obligations. |
| Canonical chain view across two daemons | Existing block listeners/index and Core RPC | Maximum indexed height does not mean callbacks completed successfully or that two daemons agree on the same block hash. Generation, completed-sync acknowledgements and commit/broadcast row locks protect restart/reorg gaps. Keep them. |
| Cumulative payout fee and dust grouping | Bitcoin amount arithmetic and script dust rules | Fees apply cumulatively per script and expiry height: `max(1000 sat, ceil(gross/100))`. A per-leaf charge or dust decision can change the customer's payout. Tests must retain exact fees, ownership and conservation. |
| Payout parent and CPFP child | Upstream signer, P2A helper, BDK selection and `build_fee_bump` | The new parent pays customers and stays immutable; fee children spend operator funding. Confirmed-only funding, exclusions and orphan recovery are real extensions. Retain actual signed size and package-acceptance checks. |
| Oversized splitting and subsequent recovery | Existing signed path/progression transactions and watchman `process_progress_tx` | Keep the additional preservation proof so splitting one root cannot destroy siblings or charge another customer fee. The 327-leaf geometry and 4096-owner inventory limits are different tests. |
| Lightning terminal/status decisions | Existing subscription, payment-attempt, HTLC tables, node manager and native CLN RPC | Hash-wide preimages/status cannot discharge an unrelated or queued source using the same hash. Keep complete own-source sets, attempt labels, exact fee metadata and wake-then-query mailbox behavior. These can be proposed upstream separately; they cannot be reverted safely in this fork. |
| Forwarded receive binding before collection | Existing receive state and arkoor builder | A provider can stop after collection and before cosigning delivery. The retained binding names the final recipient independently of the provider grant key. Upstream lacks that durable association. |
| Ordinary spend proof RPC | Existing signed successor transactions and VTXO verifier | Existing status/get-by-id responses do not establish the signed immediate successor of a retained source. Keep proof verification; an admission fence or unavailable response is not payment. |
| Quarantine, awaiting-settlement balance and payout adoption | Existing persisters, VTXO states, action/round checkpoints, BDK wallet discovery | A restored wallet must retain signed material without inventing spendable value. Upstream accepts server `Spent` as final; that is insufficient when the flag can represent an unpaid fallback decision. Keep source mapping, holder checks and single-principal balance rules. |
| Local deadline protection and binding-refresh recovery | Existing refresh selection, exit manager and StoredRoundState | The protection loop must run with the server unavailable. The signed path's remaining confirmation runway is not the ordinary refresh threshold. The before-wire forfeit checkpoint and real shared lock preserve recovery after an accepted request loses its reply. |
| Completed exit/own claim rearm across a reorg | Existing exit transaction manager, movement and persistence models | Cached depth is not canonical identity. Previously completed debits must survive rearming, and one failed checkpoint must not publish a new in-memory intent. These are correctness fixes, not duplicate exit engines. |
| Fresh-fork schema initialization | Upstream refinery migrations and wallet MigrationContext | Use one normal initial migration in each chain. Remove the separate overlay and its custom lock/cancellation mechanism. Verify creation before concurrent daemon startup, idempotent reopen, unchanged physical schema and real backup/payout recovery. |
| External app BIP84 test wallet | Public OnchainWalletTrait, BDK and persister APIs | It proves the SDK does not assume its native BIP86 descriptor or seed derivation. Replacing it with the native wallet would remove the independence that the test is asserting. |
| Native crash and mining test support | Existing daemon controller, nextest, Core RPC and test context | SIGKILL/SIGQUIT with reaping tests a different persistence boundary from graceful stop. Retried block generation is non-idempotent; the small single-attempt batches prevent accidental extra blocks. Keep this support. |
| Patched CLN and slow-mode readiness | Shared Nix CLN/grpc derivations and upstream native CLI | Six-confirmation native collection is part of the supported 1–5-block reorg assumption. The normal three-confirmation daemon is not equivalent. Slow mode preserves source-specific native outcomes. Keep config, runtime patch and readiness checks. |
| Watchman wallet admin status | Existing WalletAdminService/schema | Reusing this service exposes actual owned funding/change for operations and financial tests. No parallel admin API is needed. |

## Broader simplifications: separate, gated follow-ups

These are candidates, not a promise that the current prototype validates them.

1. **Consolidate operational documentation.** Keep one canonical operator runbook; make `contrib/release/README.md` a concise build/install entry point, `ROLLBACK.md` an actual rollback procedure, and release notes a short contract/limits delta. Remove repeated installation, restore and qualification explanations. Preserve unique limits: forwarded grants can require the provider key, small operator reclaims can postpone sender refunds, finite key scans cannot prove arbitrary-gap recovery, and fresh-fork initialization does not migrate old fork schemas. Verify every documented command/flag against the pinned binaries. Do not replace the prose with a new documentation generator.
2. **Retire `just int-binaries` if the existing archive flow covers its users.** Upstream already has `build-integration-tests-ci` and `test-integration-prebuilt`. Build a native archive, remap it into an isolated runtime, apply the same tor/migration/upgrade exclusions, verify exact product/plugin identities, run representative native financial and crash tests, then change external runners before deleting the seven-line recipe. Current external release harnesses are consumers; an in-repository caller search alone is insufficient.
3. **Consider removing `expiry_chain_view.invalid_from`.** It is written but never read by current product code. An older external financial SQL exporter still selects it; that exporter also references already-removed fields. First identify the current export/checker contract. For an existing initialized database, deleting DDL alone leaves a column; making it a real schema migration would be a larger change. Keep it out of this cleanup. Do not automatically delete other write-only provenance fields such as allocation/split generations.
4. **Reuse atomic `retire_vtxo_lock` in offboard payout reconciliation.** `actions/offboard.rs::reconcile_fallback_payout` currently releases its holder then writes Spent. The existing atomic method already implements holder-checked Locked → Spent. A focused change can remove the temporary Spendable transition. Test exact own-holder retirement, foreign-holder refusal, already-Spent replay, lost acknowledgement, snapshot/write failure, concurrent selection and payout reorg on SQLite and adaptor backends. This is a concurrency behavior improvement, so keep it separate from mechanical deletion.
5. **Fresh-only record simplification is included in the second prototype.** The first prototype preserves the released map shape. With the user's fresh paired-store boundary, the second uses a required own-claim list and rejects obsolete records. It must pass actual same-fork reopen and own-claim/path reorg tests; it does not promise conversion of abandoned fork backups.
6. **Avoid removing public SDK methods solely because only tests call them in this repository.** `exit_protection_schedule/report`, `forfeits_may_be_exposed` and `verify_fallback_output` may be external app surfaces. Review consumers first. The last helper checks a destination and confirmation depth; it is not, by itself, proof of allocation/source ownership. Preserve full payout/source assertions.
7. **Keep source-sensitive test scenarios; continue extracting only identical fixtures/oracles.** Similar-looking internal-send, reserved-grant, batched-terminal, partial-terminal, before/after refund, shallow/deep reorg and seed/cold-backup tests have different live liabilities. Do not merge them into one happy path. Old VTXO encodings remain accepted upstream; a fresh fork database does not make encoding-compatibility tests obsolete.

## Required contract repairs discovered during the audit

These are pre-existing mismatches, not regressions introduced by the prototype.

**Generated REST contract is stale.** The checked-in OpenAPI diff only changes an invoice description. It lacks `/wallet/fallback`, FallbackRegistrationInfo, FallbackRefusalError, Balance.awaiting_settlement_sat, the wallet-VTXO flag, and the new round recovery variant present in Rust. The generated REST client only changes the same description. The Rust OpenAPI annotations also omit HTTP 409, although the shared error mapper returns 409 with a stable refusal code. Add the relevant response annotations before generation. Regenerate with the existing upstream recipes after deciding the retained API; inspect the resulting schemas and client methods, compile the client, and exercise registration/refusal/balance/round responses through native barkd. The correct generated diff may grow. Dropping a required API to shrink a generated file would be a contract change.

**Server schema snapshot is stale.** `server/schema.sql` is byte-identical to 0.8.0 and omits all 13 tables in `server/src/database/fork_schema.sql`. Existing repository guidance identifies schema.sql as the full DDL, so reviewers following that instruction miss the fork's state. Use the existing `just dump-server-sql-schema` flow against a freshly initialized database; confirm every fork table, constraint and index appears. Verify fresh creation, concurrent same-fork reopen and actual backup/restore. The removed custom initialization lock is not part of the fresh-fork contract. Do not invent another schema source or generator.

## Test and acceptance strategy

Validation status and commands are recorded in the linked VALIDATION.md; raw logs remain in the local audit archive. Distinguish baseline checks, candidate checks, candidate native tests and previously existing release evidence. A passing type check is not a financial integration test.

For the first patch, accept only after:

1. The exact baseline and candidate pass `just checks` and the complete `just unit` suite. Review warnings relative to baseline.
2. Old non-null-cursor snapshot JSON reopens through SQLite and adaptor storage, retains every live ownership set, and survives write/reopen. The audit probe is temporary evidence, not new product tooling.
3. Every retained attribution case, every shared-helper caller, all five script dust boundaries, Postgres/mailbox coverage and native source-status cases pass.
4. Fence/forfeit/offboard races, actual payout/split/conflict/reorg, fallback adoption and ordinary-spend/automatic-protection cases pass against candidate binaries. Keep actual chain payments, exact fees, owned change and once-only debit assertions.
5. The ordinary integration selection is run with the normal upstream just/nextest harness. Tor and old-release migration/upgrade suites require their separate prepared dependencies; their exclusion must be explicit.
6. Run the ignored real-size split and 4096-owner inventory cases after the partition representation change. Use the existing fallback-scale profile, not a new timeout mechanism. A timed-out or interrupted run is not a pass.
7. Save the final patch hash and exact command/results; confirm source release and original dirty checkout are untouched. Test only the final candidate, and rerun affected checks after fixes.

For later schema/API/tooling/doc batches, require their specific gates above before adding them to the accepted cleanup. Do not squash unvalidated ideas into the first patch. Tests reduce risk; no audit can certify that nothing will ever break.

## Execution order

- Batch A: remove stale representations/queries and restore upstream expressions/signature; preserve all safety checks.
- Batch B: remove the three obsolete tests and seven irrelevant fixtures; share the exact duplicate helpers.
- Batch C: correct stale claims/comments; do the broader documentation consolidation only after preserving unique operational limits and checking commands.
- Fresh-fork schema batch: replace the separate bootstrap with the standard V73 and wallet 43 migrations; validate it independently before including it in the recommended patch.
- Batch D: regenerate the existing schema/OpenAPI/client artifacts and validate the external contracts.
- Batch E, only with separate evidence: archive-based runner reuse, unused chain-column retirement, and offboard atomic-retirement reuse.

A and B should be independently reviewable commits when implemented, even though the attached experiment exercises their combined effect. No history rewrite, rebase onto a newer Bark version, deployment, or protocol redesign is required for this plan.

The repository review note names `just check commits`, but the actual recipe is `check-commits`. That script fetches and force-checks out historical commits against `master`; it was not used to mutate this review checkout or presented as evidence that 176 intermediate revisions compile. The explicit checks here validate the pinned end states.
