# Captured C default-daemon protection proposal

SOURCE ONLY / NOT RUN / unallocated. Exact binary `ark-testing::bark-sdk`; exact name `fallback_act_by::fallback_protection_default_daemon_refreshes_then_exits_when_ark_is_unavailable`.

This existing native SDK case autonomously starts early refresh, adopts a replacement after six mined blocks, avoids exit at the original deadline, and broadcasts an actual replacement signed-path transaction after Ark stops. It never directly calls protection or exit progression. CLN and watchmand are not used.

Complete C file `d3e79a29af1cd1c03767ac24e89960e91745a96d8e4480fbfd3f5b09fe31da70`; whole function `f70ded222b148d44d06eadd1e4b541865820046a9a28c37e1af610b3ba0f88cb`; brace body `1e38f5ae82e6333b35b982a53439228f0f33fd36e3a8d824c66649cec7b6062d`. C capture cdedd/archiveacc3, actual SDK ELF and full helper/function pins are in JSON. Saved P07 listing8071 shows this name unignored but filter-mismatched, without execution.

Explicit settings: 40-block lifetime, exit delta3, refresh threshold4, exit margin2 and one-second automatic SDK daemon interval. Actual signed-path budget derives deadlines. Four20-second event waits make standard180 plausible; setup/mining/stop and current C runtime remain unmeasured. No timeout extension or product change.

Final oracle is mempool presence of at least one signed-path transaction plus exit-manager registration. It does not require terminal claim, final Core6, exact payout/fee conservation, seed recovery or confirmed drain. Six mined refresh blocks must not be reported as final claim confirmation. Native barkd lifecycle, default production timing, reorg/journal replay and error isolation remain separate.

CQUAL-007 remains source inventory/NOT RUN; default inventory is INVENTORY_ONLY. Existing ML056/189/190/191 links remain PARTIAL with full scenarios NOT RUN. Scoped public receipt/allocation and source-ownership searches found no exact-name executed/owned entry; unknown coordinator-led work outside those inputs must be checked by root.

Future explicit root allocation, parent fresh gates and one fresh private context required. Standard180/retries0/serial1/firstFAIL/global2; allocation/readiness/admission null. No current count/PASS/FULLML credit.

JSON SHA256: `f081d54bdacb172195a7ae2a7e0935989ea991f5c47e697df77c26d198b8bcf2`. No frozen48/global/source/runtime changes.
