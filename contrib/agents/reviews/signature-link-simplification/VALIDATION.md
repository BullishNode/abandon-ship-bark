# Validation evidence

Source release: d89a33fa9f06b5527a3e6eb846abe75e750bf5e2.
Upstream comparison: Bark 0.8.0, 959a489ff4934b181783b41c7d3ee68d85b95cbf.

All commands use the captured project Nix environment inside Docker image `sha256:8ff8b71e01b76a308ce8428e7769565ff8574c4b5c8f72eb847719bc04370f79`, offline Cargo, network-disabled containers with loopback, and a private build target. Native integration runs use an init/reaper, UID 1000, isolated test data, Core 31.0, the qualified six-confirmation CLN runtime and qualified hold/grpc plugin bytes. No production services are contacted. Existing release qualification containers are not interrupted.

## Accepted baseline results

- `just checks`: exit 0. Includes repository prechecks, ark-lib arithmetic clippy, WASM tests compilation and all workspace tests/examples compilation. See `baseline-checks.log`.
- `just unit`: exit 0. **834 passed, 1 skipped**. See `baseline-unit.log`.
- No baseline native integration run was launched by this audit. Another existing release run uses the identical merged source tree; its ongoing results are supplementary and are not counted as candidate results.

## Invalidated attempts and corrections

- `candidate-checks-invalid-cache.*`, `candidate-unit-invalid-cache.*`: **not valid candidate evidence**. Both checkouts were originally mounted at the same source path against a copied build cache. Candidate source timestamps predated later baseline build outputs, allowing Cargo to reuse baseline libraries. The two temporary compatibility probes exposed the mistake: the linked snapshot Debug output still contained the removed cursor. Every result from this attempt was excluded.
- Corrective action: candidate source now uses the distinct `/fresh-cache/audit-candidate` path; tracked candidate inputs were touched after baseline outputs. Compiler logs show the distinct path. The compatibility probe explicitly asserts that the candidate's serialized snapshot has no cursor before constructing an old-format record.
- `candidate-checks-first-compile.*`: failed with references to the removed test-only inventory report in two additional test files. Updated their constructors and replaced report-derived totals with direct sums of terminal and residual amounts. Actual payout, chain and financial conservation checks remain. The complete check is rerun after the correction.
- Initial test-helper extraction produced unused imports. Removed those imports; compare final warnings to baseline. No automatic clippy fix was used.

- `candidate-int-noexec.*`: zero native tests executed. `just` could not execute its temporary shebang script on a noexec container tmpfs. Corrected only the isolated runner mount to allow execution on `/tmp`, then reran the final source checks/unit/native pipeline.

- Core version preflight exposed the container user's actual home at `/fresh-cache/home`. The initial runtime mount covered `/root` instead. Stopped the audit-only container before native tests and remounted its private home at the correct path; the full native suite will start only after a successful default-datadir Core version probe. No product change was made for this runner correction.

- `candidate-int-shm-exhausted.log`: interrupted after the default 64 MiB Docker `/dev/shm` filled. PostgreSQL initdb then failed before application startup. The runner now matches the existing qualification runner's 4 GiB shared-memory setting; rerun uses a new test directory. Product binary/plugin hashes are checked before reuse, and the existing `ASSUME_BUILT` harness option avoids rebuilding those verified binaries. Affected cases are not passes.

## Comparison prototype results

The 35-path first prototype retains release initialization and snapshot format except the unused cursor. It is a comparison variant; do not apply its patch together with the recommended fresh-fork patch.

- Final `just checks`: exit 0 (`candidate-final-checks.log`).
- Complete `just unit`: exit 0, **836 passed, 1 skipped**. Includes the 834 retained baseline tests and two temporary compatibility probes. The probes were removed before final checks and are absent from the patch.
- The probes reopen old non-null-cursor JSON through SQLite and the JSON adaptor, preserve every live set and transaction body, and write/reopen SQLite again. This establishes serialization compatibility, not transaction financial validity.
- Native binaries were rebuilt from the isolated candidate path. Product/plugin SHA256 identities and actual nextest executable environment were verified before the native rerun.
- `just int`: **running, not green**. Normal selection has 768 tests across seven binaries; four ignored tests and three dependency-specific binaries are excluded. Current results are below; absence of an exit file is not a pass.

301 passes recorded at this handoff snapshot. Failures/timeouts:

- `ark-testing::bark lightning::intra_ark_revoke_then_claim_does_not_drain_server`: fail, 104.378s.
- `ark-testing::bark-sdk lightning::pay_hold_refused`: timeout, 180.174s.
- `ark-testing::bark-sdk lightning::receive_claim_reply_loss_reopen_abandon_fallback_and_seed_recovery`: timeout, 180.131s.
- `ark-testing::bark-sdk round::maintenance_refresh_delegated_drops_server_rejected_vtxo`: fail, 170.754s.
- `ark-testing::bark-sdk round::manual_maintenance_refresh_drops_server_rejected_vtxo`: fail, 128.819s.

The CLI claim test failed its “timeout is not a business refusal” guard. Two SDK Lightning tests hit the default 180-second outer limit; the hold-refusal trace had reached its absent-sender expiry/sync phase. Two maintenance tests observed an empty failed round after their submission window. The same source tree has earlier serial passes in a separate release run, but that does not establish these candidate failures are harmless. Rerun candidate cases serially with the same binaries and assertions; investigate any repeat. No assertion was removed and no timeout was silently widened.

The native run uses four test threads. A 44-second existing-example build overlapped part of this run; independent build/runtime load can affect timing. Report that context instead of declaring all failures environmental.

Ignored real-size split and 4096-owner tests remain a separate gate: `NEXTEST_PROFILE=fallback-scale just int 'expiry_oversize:: --run-ignored=only'`. They have not completed in this audit.

## Recommended fresh-fork prototype results

- `just checks` and final post-probe `just checks`: exit 0, including repository prechecks, WASM and workspace tests/examples compilation.
- Complete `just unit`: exit 0, **835 passed, 1 skipped**. This is 833 retained tests plus two temporary fresh-format probes. One unit test specific to the removed constructor-owned SQLite initialization was deleted. The obsolete history-projection test is an integration test; it is not subtracted from this unit count. Inherited migration/persistence tests remain.
- Fresh-format probes reject old/missing snapshot shapes in SQLite and adaptor storage, preserve raw stored bytes after a failed read, and reopen a valid record containing all live sets and a transaction body. They were removed before final checks and are absent from the patch.
- Product native build: exit 0. A separate source path and target directory prevent reuse of the comparison binaries. Native acceptance subset is compiling/running and has **no completed result yet**.
- The subset covers fresh create/reopen, Postgres, attribution, native payout, races, refresh completion, seed/backup restore, external-wallet integration, payout adoption, own-claim/path reorg, affected balance/version gates and offboard restart. Its result must be recorded independently; comparison-variant passes are not fresh-variant passes.
- A net patch was applied to a clean Bark 0.8.0 worktree. **1,034 tracked files and executable modes match the fresh prototype, with zero mismatches.** `upstream-reconstruction.json` records the net-patch and tree-manifest hashes. No old fork history or data conversion was used.
- The final release-relative fresh patch applies cleanly to the pinned release. SHA256: `8bca2ea7b2135968d8e742fe8a99d969fc9bdb107bf29fa48f90e03e719b07e3`.

## Existing generator checks

Built and ran the repository's `dump-server-postgres-schema` and `dump_api_docs` examples: exit 0 for both. No new generator was introduced and no checked-in generated artifact was overwritten.

- Generated SQL contains all 13 fork tables omitted from checked-in server/schema.sql, with no inherited table missing.
- Generated OpenAPI contains the missing `/api/v1/wallet/fallback`, FallbackRefusalError, FallbackRegistrationInfo and awaiting-settlement field. Relevant 409 response annotations still require repair before regeneration/client qualification.
- Physical SQL equivalence between the separate initializer and V73 remains pending the fresh-schema dump.

## Limits and release-agent gates

This is a tested deletion experiment with unfinished native qualification, not a claim that nothing can break. Keep the PR draft until native failures are resolved or separately understood with candidate evidence, the fresh acceptance subset and scale cases pass, and the schema/API/client mismatches are repaired and verified.

Broader documentation consolidation, archive-runner replacement, SQL column removal, SDK surface deletion and offboard atomic-retirement reuse remain separate proposals. Their gates are in the plan. The fresh paired-store boundary deliberately excludes conversion of abandoned fork formats while requiring same-fork restart/backup/seed/reorg recovery.
