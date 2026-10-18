# Current v8 testing-track ownership and readiness

Completed evidence remains **225 executions: 211 PASS and 14 preserved FAIL**, 219 unique binary/test pairs, 198 nextest invocations, 213 native and 12 component executions, and 436 coverage rows. No own native runtime is active. The nine latest PASS results are the four batch39 controls, three batch40 adapted CLI cases and two batch41 ignored registration cohorts. Their exact names, run IDs, timings and distinct v6/v7 source pins are in the JSON; none is relabeled as v8 evidence.

PR18 is a verified draft on `work/fresh-test-report-checkpoint14-20261017`, head `b969dd862ad3d08d7c30ffa289e708f04578d331`, with 47 new report paths and the fixed 225/436 cutoff. PR17 head `e36fff479b8c9b29452677316becfbe014172f3d` is preserved. Batch41 client postflight and both independent retained-Core audits are complete. The finite 1M-sat cuts do not establish custody of all other leaves, full ML qualification or production readiness.

The root reports that the v8 freeze/build has started and remains the sole Cargo/shared-target writer. No immutable v8 capture, completed compilation evidence or native allocation has been provided to this handoff. Candidate source/archive/ELF/metadata pins therefore remain null. Four integration binaries are expected, including the actual `ark-testing::barkd` test ELF; a barkd product executable alone is insufficient. Historical source reference `f1a609d9b44d0a7b7513d7d162e6b5e71f5a61f0` is not an immutable v8 candidate pin.

The 12 ordinary adaptations are now integrated into canonical source. Read-only hashes of all five target files still match the reviewed results bound by aggregate manifest `c589354c1fdd931f5e313a531fd3c434531105156d5ffb991166c2554049bcbb`, private patch `3aeb8840d040a67cee0bc423a3ab80953df3b795229d864e8be2fd6bee1ea165`, and peer ACK `842169d08989eec03f2134c1967af79ff05417c8f4292694cf6351c18a37a326`. Selection plan `bef0af66394f3839eabad46f422ee39f2ad94c9cd03ddfd5857bc058bcfc83c8` lists seven CLI and five barkd targets. All adapted targets remain NOT RUN, and compile qualification awaits the immutable capture. Nine original FAIL results, two held originals and the held unsafe server-word contract remain preserved.

| Expected binary | Exact ordinary target | Source author/owner |
| --- | --- | --- |
| `ark-testing::bark` | `onchain::list_utxos` | client_test_gaps |
| `ark-testing::bark` | `onchain::onchain_drain` | client_test_gaps |
| `ark-testing::bark` | `movement::board_board` | client_test_gaps |
| `ark-testing::bark` | `movement::movement_offboard` | client_test_gaps |
| `ark-testing::bark` | `movement::movement_send_onchain` | client_test_gaps |
| `ark-testing::bark` | `offboard::bark_send_onchain` | client_test_gaps |
| `ark-testing::bark` | `offboard::bark_rejects_offboarding_dust_amount` | client_test_gaps |
| `ark-testing::barkd` | `boards::board_all_barkd` | client_test_gaps |
| `ark-testing::barkd` | `recovery::recovered_wallet_finds_boarded_vtxo` | client_test_gaps |
| `ark-testing::barkd` | `recovery::recovered_wallet_finds_mixed_origin_vtxos` | client_test_gaps |
| `ark-testing::barkd` | `recovery::recovered_wallet_finds_round_vtxo` | legacy_inventory; client_test_gaps peer |
| `ark-testing::barkd` | `recovery::recovered_wallet_quarantines_invalid_spent_vtxos` | backend_test_gaps; legacy/client peers |

These are prospective selections, not an allocation. The quarantine target replaces the unsafe held server-word oracle; it must not select that old literal. Version gates must actually pass and native assertions must be reached before credit. The ordinary recovery adapters retain their original-alive/distinct-DB scope; they are not seed-only recovery or original-absent tests.

Six additional money fixtures are authored and reviewed but have no matching immutable compiled capture or native run. The `cd43538fb754deded5abc5dc73ebde341b3f527d5a60016d0ff333693b811ec0` intake records the source state at its observed HEAD: the three server files were absent, the expired-Lightning body was absent, and the two existing watchman names had different bodies. Those source observations do not predict the unfinished v8 capture.

| Exact source-only target | Source owner | Reviewed source SHA256 |
| --- | --- | --- |
| `qualification_conflict_reorg::expiry_actual_partial_conflict_reorg_restores_one_canonical_payment` | backend_test_gaps | `15ec3495faf2baba22675d1a3960d85655d03066c48de5bed21e883271c6daf1` |
| `qualification_failed_swap::expiry_failed_swap_during_restart_pays_its_absent_owner_once` | legacy_inventory originalfixture; backend_test_gaps v2/v3 hardening and frozen55bb; client_test_gaps/parent source peers | `55bb4efbab450247052dec3aac00578a774c7fbe7ad02181625e9f5153521b1e` |
| `qualification_offboard_restart::registered_offboard_restarts_before_nursery_handoff_and_pays_absent_owner_once` | backend_test_gaps; legacy peer | `6512e1ed42f4591afcc2f7b930da9eba7c24f6daaf5cc28cadb5d4fc1d4c5bf9` |
| `lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds` | client_test_gaps | `3fef86397a648c4b11ef92f2f0c252b870e9ec766cc1ac14ff96075347964c88` |
| `exit::watchman_sweeps_funding_tx_during_exit` | legacy_inventory author; client_test_gaps peer | `bda6c497af71c94ea799fda1dc99d720bd6a034776428fa2d4fed5488386b9b1` |
| `exit::watchman_sweeps_vtxo_chain_during_exit` | legacy_inventory author; client_test_gaps peer | `bda6c497af71c94ea799fda1dc99d720bd6a034776428fa2d4fed5488386b9b1` |

The JSON binds each of these six targets to its exact function hash, packet manifest, patch reference, source peer, bounded oracle and scope limits. Root integration must use full add-file content for absent server files and target-only overlays for existing SDK files. The failed-send v3 delta requires its preserved v2 baseline; it cannot be applied directly to the original fixture. The watchman partial-chain fixture explicitly sets change factor1 and does not qualify the product default factor2. The expired-Lightning fixture uses manual sync and watch900 cadence to isolate admission; it does not qualify default-daemon protection.

Older restore and owned ef70 conflict drafts, the ML190 expected-red protection fixture, and obsolete operator-payout/full-principal-refresh expectations remain frozen and excluded. Internal refund/shared-source replan, pre-obligation admission, default-client product work and fairness fixtures remain root-owned; this handoff delegates none of them. Only the existing parent, backend, client and inventory roles are identified. No additional four-agent team is announced or assumed.

Before any native run, the root must supply immutable source/archive/test-ELF/metadata and actual tool pins, exact body compatibility, explicit case allocation, singleton/version/resource gates and fresh private contexts. Default proposed controls are repository `just int-binaries`, serial1, retry0, default180 and first-failure stop. CLN cases additionally require the approved native CLN/adapter/hold pins and Docker unset. Root capture or compilation success alone grants no runtime authority.

This handoff supersedes status prose in older general ownership/source-queue snapshots while leaving those snapshots and all packets untouched. The 200-case ML map is unchanged: 198 NOT RUN, two bounded PASS; evidence scopes 145 PARTIAL, 53 NONE, two FULL. Only this new external JSON and Markdown were written; no STATUS, COVERAGE, FINDINGS, public documentation, source, runtime, Cargo, metadata or Git change occurred.
