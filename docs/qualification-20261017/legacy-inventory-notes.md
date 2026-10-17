# Legacy catalogue crosswalk, 2026-10-17

Disk hold remains active. This is a source inventory; no build, test, wallet open, service change, remote campaign, funding or cleanup was performed. No source or other agent's work was changed. The only owned outputs are this note and `legacy-inventory.csv`.

The CSV preserves all 80 original IDs and their exact behavior, expected outcome and driver fields. It maps them to W0-W10 and K/WC client requirements using revision 5 and the prompt's accepted corrections. Counts:

| Classification | Original IDs | Additional IDs |
|---|---:|---:|
| unchanged | 37 | 0 |
| adapted | 43 | 0 |
| obsolete | 0 | 0 |
| missing original-catalogue requirement | 0 | 23 |
| total | 80 | 23 |

`adapted` means the existing coverage overlay changes the expectation, including wording about standard supplied scripts, lineage, actual decision boundaries and finality. It is broader than the existing report's 12 `needs_adaptation` evidence-category rows, which identify principally removed hint/per-key APIs. Both fields are retained separately. Removed hints, blanket margins and mandatory per-key publication are obsolete subrequirements; none of the 80 whole economic behaviors is discarded. The 23 `FT-*` IDs name explicit requirements absent from the original behavior statements, such as migrations, per-key cursor crashes, daemon automation and reorg invalidation. They are gap candidates, not 23 newly discovered defects or 23 absent implementation tests. Related legacy IDs are described in their limitations; execution owners should reuse existing tests and avoid counting the same execution twice.

Every candidate result is **NOT RUN**, with an empty `tested_revision`. The proposed target is the prompt's published `e44ddf95741478508908c17adc61752832d75c6a`; no worktree or binary was created or repinned. Historical evidence is retained in distinct columns and cannot qualify that target. Automated names remain UNASSIGNED until the test-inventory owner binds actual native test names; no executable command was fabricated.

The existing simulator report credits only H03/H07 on public Signet and H36 on a separate fresh native-regtest snapshot. The other 77 original IDs are not fully demonstrated. These are reports from the inspected evidence catalogue, not rerun or independently reverified financial claims. The original report's category counts (including partial evidence) are: public Signet 9, native regtest 24, unit only 10, source only 15, not run 9, failed 1, needs adaptation 12. They are not scenario PASS totals.

Known evidence limits:

- D39's original failed run remains failed. A separately pinned rerun passed absent-mailbox 10,000 registrations in 103.265 seconds, while wrong-mailbox failed its 120-second deadline at 120.139 seconds after 9,902 scan operations. The legacy report calls these records; the current prompt explicitly warns that distinct records were not established. Registration searches are not funded payment or payout throughput.
- H06 retains the failed S03 same-expiry campaign. Historical-forfeit geometry blocked a 10,000-sat entitlement after an original 28,670-sat payout. A later separate 9,000-sat repair payout cannot prove the original one-output 38,670-sat grouping oracle. The corrected native regression is separate evidence and the repair is coordinator-owned.
- The older simulator K3 evidence describes a direct-method protection run and does not itself establish default-daemon automation. A newer separate receipt, `docs/evidence/client-default-daemon-20261017.json` in the product repository, records autonomous early refresh and offline signed-path broadcast PASS (`86eaa53a`, 18.373 seconds, SDK manifest `8ad4ac83...`, historical 7055 servers). The pinned e44ddf source also contains `fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable`. This newer evidence proves initiation/path broadcast, not a final claim spend or latest-server qualification. It was not executed by this testing track. FT-CLIENT01 remains a named original-catalogue gap with a reusable test, not a claim that daemon source/evidence is absent. Uncommitted offboard-finality work has another owner and lacks a complete native money-case pass in the prompt.
- Controller units, source inspection, RPC acknowledgements, synthetic inserted fences and payment counts cannot establish a full original economic scenario. No old A/B/C results are inherited.
- The 12-wallet/60-transfer and 50-wallet/200-transfer public-Signet workloads are bounded workloads, not 200 edge cases or maximum capacity.
- Stock Noah HTTPS/backend health is preparation evidence; strict invoice metadata-hash compatibility, native Lightning admission and offline recipient ownership remain unresolved. No mobile work is needed.

After coordinator release and build ownership agreement, the highest-value first bounded native-regtest batch should reuse the current-economic-cut regression and actual refresh/offboard decision races, then run default-daemon or prepared-offboard finality against immutable pins. The initial independent oracles should prove actual canonical outputs, retained signed recovery material and exactly one economic discharge. Do not wait for all 103 rows or build a new framework. Parent/test-inventory owner chooses exact runnable names after inspecting the repository's just recipes. Controlled fee pressure and reorgs belong on regtest; simulation campaigns belong on separately allocated public Signet. Candidate pin, dirty-source manifest, actual binary hashes, command, start/end and per-run receipt are still required before any candidate PASS.

Read-only sources inspected:

- Prompt and Signet handoff, plus the final server revision-5 plan, client plan, simulation revision-1 plan and coordinator `fresh-execution-20261016/STATUS.json`. That STATUS was dated 13:16 UTC and had older source/Signet observations than the prompt; its pins were not treated as current.
- Simulator `simulator/scenarios.json`, `docs/80-case-coverage.{json,md}`, `docs/public-signet.md`, `docs/signet-load.md`, `docs/signet-phase-upgrade.md`, `docs/signet-s03-repair.md`, `docs/signet-results-20261017.md`, `docs/noah-fresh-signet.md`. No applicable AGENTS.md was found at the inspected ancestor locations or anywhere in this simulator worktree, including hidden files.
- Simulator published HEAD was `c83168fe7e126a154232599236a88b88d8fc7d01` during inspection. Files were read as present; other agents' uncommitted changes were left untouched. This report does not imply every inspected file is committed at that HEAD.

Input SHA256 values:

| Input | SHA256 |
|---|---|
| `simulator/scenarios.json` | `fd01b08c72abb9cf61c5f93aa2c43362fa7e8b58788a7fbc54d0e1ab85de86b4` |
| `docs/80-case-coverage.json` | `bd1f9d914f0aab736b284c80e1bd5a290f05af507b446c099daefb09beb425b0` |
| final server plan | `d32c2e67e8ae7b54da6b7edf8cfe34e5ce1d7f935ecbd1f396dc87968db0caa9` |
| client plan | `f7e4d640dc3f3f1c8a0e24dad92d5acd1ed6b696e01551784727077a17ea7a54` |
