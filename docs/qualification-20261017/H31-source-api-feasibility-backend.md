# H31 public API feasibility — source only

Five isolated family cases, each with three distinct original accounts and application-selected scripts, are feasible using existing public APIs. Root approved source-only authoring after the checkpoint25 export review. No fixture has been authored or executed;180 seconds per case remains unmeasured.

Use public `Wallet::open` with real SQLite persistence, actual seed/config, explicit `fallback_spk`, no onchain component and manual scheduling. Registration genuinely signs and verifies the app script binding. The SDK builder has no override; do not invent a builder/CLI API. Use actual random app keys/standard addresses/redeem or witness scripts, asserting the actual script dust at runtime. This does not prove app destination spend custody.

|Family|Source-derived dust|Gross -1 / equal / +1|
|---|---:|---|
|P2PKH|546|1545 / 1546 / 1547|
|P2SH|540|1539 / 1540 / 1541|
|P2WPKH|294|1293 / 1294 / 1295|
|P2WSH|330|1329 / 1330 / 1331|
|P2TR|330|1329 / 1330 / 1331|

Below boundary forfeits the entire gross with fee0/nooutput; equality pays dust and plus1 pays dust+1, each charged1000. Every boundary must have a distinct script: different keys sharing a payout script would regroup fees and change the result. All proposed amounts exceed Ark330 admission dust.

Each case uses one genuine100k root, three normal SDK sends, and two separately confirmed100k sponsors. Discover all default-factor2 sender change pieces and signed paths. Sender is one separate script group charged1000: allcustomerpaid=96001−dust, policyfees3000, forfeiture999+dust, total100k. Physical miner fees are separate. The300k finite graph includes actual child change/unused sponsors; ordinary board change/upfront miner fee stay outside this labelled cut.

Require canonical6 actual parent/same-blockchild, complete prevout/DAG/accounting and whole-chain payee scan before cut filtering, global mempool negatives, nullable dust allocation, and exact per-owner fullhistory/all10balances/emptyintents/idempotence/reopen. Recipients must own genuine local signed Spendable bodies before absence; fresh seed-only reopening would miss local debits. Emit a phase-labelled typed public capsule before SDK history assertions to retain partial financial evidence on failure.

Companion JSON pins exact source files/API refs and specifies all oracles/traps. Five three-boundary cases are preferable to one15-boundary matrix; no runtime feasibility was measured. H30/H32 are adjacent coverage only. No source/Git/global report, build/runtime, custody, daemon, production or FULLML credit is claimed.
