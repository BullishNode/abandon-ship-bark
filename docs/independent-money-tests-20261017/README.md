# Independent money-safety source intake

These two fixtures add independent assertions for gaps outside the ordinary regression queue. Both received source review and crossreview. They are **not compiled or executed**. The coordinator owns integration, candidate capture and resource allocation. Production readiness remains unestablished.

| Scenario | Proposed test | Bounded assertion |
| --- | --- | --- |
| ML049 | `qualification_forwarded_ack::forwarded_receive_ack_loss_replays_identical_outputs_and_credits_recipient_once` | A real mailbox accepts a forwarded receive, its reply is lost, then an explicit retry sends identical signed outputs. Recipient ingestion before and after retry must preserve one credit and the exact output IDs. The original Lightning payment completes once. |
| ML186/ML187, partial alternate recovery | `fallback_external::fallback_external_bip84_seed_restore_spends_actual_payout_while_history_unavailable` | A real honest fallback confirms while the original recipient is absent. Two original independent seeds and fresh stores must discover/spend the actual BIP84 output while only history RPC transport is unavailable. Actual canonical blocks and the complete observed script interval guard receipt and duplication. |

Base is `4fa8997ba21158f20326c19f68efba5f42af1f3c`. Only the two test files and this intake document are authored in this branch. The prior two external-wallet test bodies remain byte-identical. The new server file is intentionally not registered in this branch. The coordinator must add `mod qualification_forwarded_ack;` to `testing/tests/server/mod.rs` in the accepted candidate, then capture and compile that exact source. A green build that omits the module cannot qualify this test.

Before execution, record a concrete coordinator allocation, source/archive/file/body/executable/tool hashes, exact expected binary, and a fresh singleton listing with one unignored test. Use the existing `just int-binaries` artifact protocol, serial one, retries zero, default 180-second timeout, first failure stops. ML049 requires the approved native CLN/grpc/hold pairing and `BARK_DOUBLE_DRIVE_ACTIONS` completely absent from the environment. It must not use the reentrant recipe. No tests were run to prepare this branch. CI is skipped for this draft source packet so the coordinator retains build admission.

ML049 does not qualify fallback, seed recovery or Noah compatibility. The recovery test does not reproduce a 2-MiB history or actual revision churn, and does not independently prove custody of fee-child output keys. Neither is a full ML acceptance claim. Every original failed execution remains unchanged.

External append-only handoff, source packets, crossreviews and the independent retained v7 finite-DAG audit are under `/home/francis/bark-integration-2026-10-01/independent-money-tests-20261017/`. The separate historical audit reconciles 1,850,000 input sats = 1,803,531 terminal sats + 46,469 miner-fee sats over 41 canonical descendant transactions. It retains native recipient/LN-state provenance and adds no new execution count. Raw/private artifacts are excluded from this branch.

## Runner correction

The ML049 fixture requires `unset BARK_DOUBLE_DRIVE_ACTIONS` **after** the cached Nix environment is sourced. `action_drive_factor` checks variable presence, so `false`, `0` and an empty string all enable double driving. Every earlier unset/false allowance in the external immutable packets is superseded by `double-drive-environment-correction-001.json`. Preserve those packets as historical review evidence. No source test, native run or result changed.
