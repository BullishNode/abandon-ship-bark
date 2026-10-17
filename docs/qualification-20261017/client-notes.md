# Client inventory, read-only hold checkpoint

Inspected published source: `e44ddf95741478508908c17adc61752832d75c6a` in `/home/francis/abandon-ship-fresh-integration`, using `git show`/`git grep`. The canonical working files contain another owner's uncommitted offboard-finality changes and were not edited, copied, staged, or used as candidate source. No builds, tests, Docker operations, funding, service changes, wallet opens, cleanup, or campaigns were performed. Only these two small inventory reports were written. Parent independently confirmed the cached container environment; this inventory itself did not need a compiler.

Read the testing prompt/handoff, root and contrib agent instructions, `writing-tests.md`, `running-and-debugging-tests.md`, final revision-5 server plan, client plan, and client financial/actions/adoption/acceptance handoffs. The client plan's old STOP/status text is superseded by the active launch; the disk HOLD remains active. The decision boundary supersedes the blanket margin. New Lightning admission remains targeted; unknown owners do not become operator; app-provided standard scripts remain valid; BIP84 external-wallet proof is required; the fee is 1,000 sat per actual output. No new policy was introduced.

`client-inventory.csv` has 48 scenario rows: 39 existing scenarios and 9 explicit gaps. These are testing-track IDs `CQUAL-001..048`, not replacements for the simulator's original IDs, and must not be counted as 48 additional independent legacy cases. `classification=unchanged` means an existing pinned test is directly reusable for the stated limited oracle; it does not assert any original 80-case coverage. Every result is NOT RUN, with tested_revision blank and the separately named inspected_revision populated. Historical passing evidence is scoped below and is not promoted to this candidate.

## Most useful first native batch after coordinator release

First run the single automatic-daemon case CQUAL-007, then CQUAL-012, with one native test at a time and a fresh owned `TEST_DIRECTORY`. This immediately checks automatic protection and the distinction between an admission decision and actual customer payment. Third, run CQUAL-006 with explicitly BIP84-featured metadata to cover actual payout, independent app-wallet seed restore, and a confirmed spend. No UI/browser infrastructure is needed for this first batch.

The immutable `just int-binaries` recipe exists at the inspected pin. It avoids the build prerequisite of `just int`, but it still has the `docker-pull` prerequisite, so it may only execute after release and capacity agreement. Exact proposed recipe arguments, once the coordinator identifies compatible immutable metadata/products and the source working-directory mapping, are:

```bash
TEST_DIRECTORY="$CLIENT_RUN_ROOT/default-daemon" KEEP_ALL_TEST_DATA=1 \
  just int-binaries "$CLIENT_BINARIES_METADATA" \
  'fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable --test-threads 1'

TEST_DIRECTORY="$CLIENT_RUN_ROOT/unfunded-finality" KEEP_ALL_TEST_DATA=1 \
  just int-binaries "$CLIENT_BINARIES_METADATA" \
  'fallback_unfunded_decision_keeps_exit_material_until_actual_chain_payment --test-threads 1'

TEST_DIRECTORY="$CLIENT_RUN_ROOT/external-bip84" KEEP_ALL_TEST_DATA=1 \
  just int-binaries "$CLIENT_BIP84_BINARIES_METADATA" \
  'fallback_external_bip84_actual_payout_seed_restore_and_confirmed_spend --test-threads 1'
```

These commands were prepared, not executed. Variables deliberately remain unset until resource/provenance allocation. Bind the existing `CAPTAIND_EXEC`, `WATCHMAND_EXEC`, `BARK_EXEC`, and `BARKD_EXEC` to separately verified immutable products; do not infer them from the metadata file. Record all hashes, source manifests, linked SDK source, configuration, recipe and metadata/package paths before launching. Use the established canonical invocation when metadata IDs require it. The SDK ELF links wallet code directly; changing `BARK_EXEC` does not change that SDK code. The third test is gated behind `ark-testing/fallback-test-bip84` in `testing/tests/bark_sdk/mod.rs:19`; an unfeatured build silently has no such module. Do not call a zero-test selection a PASS.

Do not add another nextest `-E` filter to this recipe. Its existing default exclusion can OR-combine with another filter and select a much larger batch, as preserved in the earlier client actions handoff. Each corrected proposed command uses only the ordinary unique substring filter. Allocate SDK-only binaries metadata; do not pass --test with --binaries-metadata. Confirm the selected count before allowing a bounded run. Running only a named native case still requires owned PG/Core endpoints and test process ownership. Keep failures and unknown money outcomes; do not automatically retry a financial action.

## What the default-daemon test actually establishes

`testing/tests/bark_sdk/fallback_act_by.rs:28` already contains `fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable`. It sets `daemon_manual_sync=false` and a one-second interval, never calls `protect_expiring_vtxos`, advances the real chain to refresh-by, waits for the daemon-created pending round, triggers a real server round, waits for actual funding in Core's mempool, confirms it, and waits for the replacement coin. At the original act-by height it verifies no spurious exit. After stopping Ark it advances to the replacement deadline and waits for an actual signed exit-path transaction in Core and an exit-manager record. This is an authentic default-daemon test, despite an older actions-handoff remaining-work paragraph saying it had not yet been added.

Its limits matter: configuration uses 4/2 refresh/exit settings and a 40-block server lifetime; it proves SDK daemon scheduling and exit-path broadcast, not stock native barkd process lifecycle or finished exit claim and recovered funds. It has no watchmand, payout-vs-exit race, process restart at a checkpoint, offboard outcome, or post-forfeit permanent-server-outage recovery. Those require separate oracles; no current execution receipt for this pinned test was generated here.

## Priority gaps and exact distinctions

- Prepared-offboard decision refusal/finality is a gap at the pin. Existing `offboard_cancels_when_fees_change_after_lost_session` proves safe cancellation only for stale fee parameters after a lost session and a server spendability check. It does not prove a prepared offboard refused after a genuine fallback decision, lock handoff to protection, independent confirmed discharge, or reorg reopening. Another owner has uncommitted finality work; obtain that owner's published commit and preserve this pin as a separate tested revision.
- `fallback_protection_lost_forfeit_reply_keeps_signed_replacements_durably` proves checkpoint retention and successful later RPC recovery. It releases the held reply and syncs successfully. It does not prove automatic full conditional recovery after permanent server loss once forfeits may bind.
- `delegated_refresh_completes_with_expired_inputs` proves a previously started delegated exchange can finish after expiry without a payout decision. A fresh returning wallet starting a new expired undecided refresh still needs an explicit/native maintenance case. Transient in-flight/unknown/status-unavailable exclusion and later positive-status retry need native RPC-count oracles.
- History regroup/split/reorg, unknown-descendant debit, and output reuse have useful units, but a synthetic projected receipt is not chain evidence. Need native multi-input/two-root payout history, partial conflict/regroup, reorg rebuild, one movement per actual output, and unchanged total local debit. The balance outage fixture explicitly writes a synthetic snapshot; classify it as component projection coverage, never as an actual payout.
- Wrong script/value/fee have unit checks; native malicious receipt/signed-path mismatch remains unqualified. Payout confirmation and rollback have a genuine native regression. Full non-payout dust/exit/round/offboard discharge and reorg winners still require native finality proof; an intermediate tree transaction must not discharge a claim.
- Browser fixture is ignored by default and requires BIP84/embedded web plus host browser acknowledgements. It covers ordinary board-origin payout and seed restore. It does not complete WC6 Lightning/dust/return-before-decision.

## Historical evidence must retain its pairing

The client handoffs report actual earlier native passes on separately archived products. The financial/actions milestones used archived 7055 servers with SDK-linked newer wallet source. The external BIP84 acceptance run `8f297156-7884-47da-8a19-640730082b72` passed after a preserved restored-debit failure; it is an earlier pairing. Browser run `fe405d38-4905-49d2-8224-8fabc6f9424b` passed all three phases on 7055 servers with `bd92e4574` plus an explicit backend client overlay and webdist10ecfb9; an earlier settings-locator timeout remains unexplained. Neither receipt proves all products were built at e44ddf957.

The corrected S03 manifest was read at `/home/francis/.cache/abandon-fresh/runtime-s03-current-cut-correction-20261017/manifest.json`. It explicitly records `head=fb85d9a9c3b31b3eab1eb2113d2caec89c7f10dd`, `dirty_source=true`, source-manifest SHA256 `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69`, and a native server regression. It archives captaind/watchmand hashes and says CLI products were reused from `runtime-operation-client-20261017`. It does not establish the linked native client SDK test provenance. Preserve this manifest unchanged and do not relabel it as a clean e44ddf957 client suite.

## Stock Noah boundary

Read `bark-simulator-signet-rework/docs/noah-fresh-signet.md`. Native product `bark/src/lightning/mod.rs:131` has only `lnurlp_invoice_amount_verification`; its metadata-hash mismatch path warns. That is not the independently strict verifier required for S09. The stock backend callback must first prove exact metadata-hash, amount and network on the actual original invoice. A mismatch is a failed compatibility oracle before payment, not permission to relax verification.

After a separately allocated public-Signet campaign, keep recipient absent, provider running, one original payment hash, no Ark-negotiation bypass, and inspect only the active provider's real signed Delivery checkpoint to distinguish conditional pre-claim state, registered recipient-owned outputs, and delivered mailbox state. CLN peer health, a balancing payment, or prepared auth does not prove Bark/Noah integration. No Android/mobile/Noah-fork work is needed. S09 is not executed and no funds were dispatched by this inventory.

## Preserved command preflight correction

Parent executed a CLI preflight of the initially proposed `just int-binaries` arguments and found `--test bark-sdk` incompatible with `--binaries-metadata`; exit2, no tests ran. That rejected proposal was my command-design error, not a product failure. The initial argument format was `--test bark-sdk <unique-test-substring> --test-threads 1`. The commands above now omit `--test` and require an SDK-only immutable metadata allocation. Root owns the actual receipt and later execution; this document claims no native result from the failed preflight. Avoid adding a second `-E` because of recipe filter combination.
