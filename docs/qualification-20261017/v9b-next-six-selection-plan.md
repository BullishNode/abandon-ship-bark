# Proposed v9b six-case selection (source only)

No runtime allocation or execution is granted. All six remain NOT RUN. The proposed serial order is:

1. `ark-testing::server` — `qualification_offboard_restart::registered_offboard_restarts_before_nursery_handoff_and_pays_absent_owner_once`
2. `ark-testing::server` — `qualification_failed_swap::expiry_failed_swap_during_restart_pays_its_absent_owner_once`
3. `ark-testing::server` — `qualification_conflict_reorg::expiry_actual_partial_conflict_reorg_restores_one_canonical_payment`
4. `ark-testing::bark-sdk` — `lightning::pay_hold_expired_inputs_are_refused_then_refresh_succeeds`
5. `ark-testing::bark-sdk` — `exit::watchman_sweeps_funding_tx_during_exit`
6. `ark-testing::bark-sdk` — `exit::watchman_sweeps_vtxo_chain_during_exit`

The archived source matches its frozen manifest for all five relevant files and all six target bodies. The corrected expired-Lightning target is d97faa40 (old eb82a122). Failed-swap uses the corrected full file 237dbe45 and helper ebae0d73; its wrapper stays 0fe44adf. The other four target bodies and files match the reviewed original intake. Full hashes and relevant helper-body hashes are in the JSON. The later mixed/round correction 0cf is excluded: v9b retains c52 recovery source.

Admission still requires final immutable v9b ELF/metadata/source provenance, actual exact singleton unignored listings, all 17 pins (14 core plus the three CLN tools), fresh resources/private contexts and explicit root allocation with parent launch. Default 180 seconds, serial 1, retry 0 and first-failure stop remain required. Only failed-send restart and expired-Lightning require CLN. No control files or pins were generated, and no metadata, listing, preflight, Cargo, runtime or actor action was performed.
