# Ark fallback-payout money-loss scenario research

This directory contains **200 distinct event scenarios, ML001–ML200, including 81 concerning Lightning**. They are proposed suites of events and financial states for later defensive testing. They are not 200 asserted defects. Each entry has the requested A–K fields, including preconditions, transitions, correct behavior, possible loss, source references, coverage limits and an independent future oracle.

Read [MONEY_LOSS_SCENARIOS.md](MONEY_LOSS_SCENARIOS.md) for the scenarios, [MONEY_LOSS_SCENARIOS.csv](MONEY_LOSS_SCENARIOS.csv) for sorting, and [PRIORITIES.md](PRIORITIES.md) for the recommended testing order and shared causes.

Final classifications are **4 observed failures, 58 source-supported risks, 134 hypotheses requiring validation and 4 historical regression risks**. “Observed” includes an actual receive-admission failure and a synthetic attribution deadline failure; it does not mean four demonstrated permanent customer losses. Priority is a testing/release-gate priority, not a measured probability or a claim that a hypothesis is reachable.

## Review boundary

The review began from the three requested locations, read the three specified plans, followed their references into relevant source, historical tracks, manifests, runner scripts, retained financial records and selected operational reports, and used three subagents for Lightning, payout/attribution, and Noah/evidence review. Their initial candidate notes are retained as `LIGHTNING_REVIEW.md`, `PAYOUT_REVIEW.md` and `NOAH_EVIDENCE_REVIEW.md`. **The full catalogue supersedes those preliminary notes**, including their counts, earlier scope statements and hypotheses corrected during review.

No product fixes, tests, builds, binaries, financial RPCs, deployments, service restarts or campaign modifications were performed by this research. Existing binaries were hashed without execution. Document generation and structural checks read/write this research directory only. No large evidence archive was copied or extracted, and no prior evidence was deleted. A single targeted small JSON member was streamed from the retained scale archive. Private configuration, credentials, seed words and preimage values are excluded.

This was a bounded review, not an audit of every source file, worktree, session or log. Source facts are separated from conditional reachability, inferred consequences and actual retained observations. A missing test does not establish a defect; existing guards are described even when they may prevent the scenario.

## Pinned implementation inventory

The initial primary working tree was `751bd5a5d8ec9bbf72f0a44c1e7d4ac371b6269d` on `fresh/xpub-sweep`, with four tracked modified files: `expiry_lightning.rs`, `ln/mod.rs`, `ln/node_manager.rs`, and `lightning_receive_binding.rs`. Their read-time hashes and the initial diff hash are preserved in the inventories. Other workspace activity subsequently committed the internal receive overlay as `c52056a4c`, integrated the corrected oversized fixture, and published the owner-scan fence/batched-commit change.

The primary source at closeout is **`8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc`**. Its tracked tree was clean; nested worktrees appeared as untracked directories. The final source-anchor hashes were checked against these bytes. Only two anchored source files changed relative to the initial inspected bytes: `server/src/database/fallback.rs` and `expiry_attribution.rs`; their diff was reviewed and related coverage descriptions updated. “Current” in the catalogue refers to this inspected source, **not the source currently running on a remote server**. An active workspace can change again after this snapshot.

| Location under `/home/francis/` | Inspected HEAD | Classification and scope |
| --- | --- | --- |
| `abandon-ship-fresh-integration` | `8fb729a2ff5c8c83d497dcbc6b2b3abd1a0bd9dc` | Current selected source; targeted server/watchman/SDK/persistence/transaction/migration/test review. Initial revision and overlay preserved separately. |
| `abandon-ship-fresh-attribution` | `cf32afccb8582e915c39cc20b6d830f1d5aecc5a` | Uncommitted attribution/client development tree; 19 tracked dirty entries initially; not the current release. Nested client/protection/finality/scale worktrees discovered. |
| `abandon-ship-fresh-settlement` | `bfbe6b7c3088a09dd4e81c4ac24cdc763a627a8c` | Draft over upstream; 33 tracked dirty entries initially; not an independently qualified artifact. |
| `abandon-ship-client` | `36d1a07c3503690857fab499483f13432886ebc8` | Historical web/client tree; clean at initial observation. |
| `abandon-ship-fresh-web` | `e1a498be563d0c1121fba3b1f0e08700855c7758` | Fresh web source; clean initially. Retained embedded bundle identifies separate `10ecfb93d520c70bb55f2472c9c5d750a7a37941` build input. Whole UI not audited. |
| `noah-fresh-backend-20261016` | `8e68ba5c3f032d9fd55e2f2f1f2af1441395eff0` | Clean detached backend source: LNURL, provider client, auth/request/cache and address persistence boundaries. No mobile/Android work. |
| `bark-simulator-signet-rework` | `d8cdb3b66c44df62cd946c57d18c3ffa8c544fab` initially; `39751de3cde93e27edfba99ad40aca837613b45b` at closeout | Clean simulator/controller source; targeted invoice verification, campaign/repair receipts and runner behavior. The anchored implementation file stayed unchanged; the later two receipt-publication commits were inventoried, not fully reviewed. |
| `bark-simulator-fresh-integration` | `c458496a6b5ced8210f0399c6fab126e48dc04a5` | Clean integration simulator tree; inspected provenance and selected relevant differences. |
| `abandon-ship-bark-two-step-comparison` | `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b` | Historical Track A two-step implementation and selected settlement/payout tests. Separate historical release ref `9a9334d6a18bfcd7a371a10a6fe85ce985dacf97` discovered; names are not deployment proof. |
| `abandon-ship-bark-two-step-integrated` | `3d3b28428cd33eb5e703342a8441c1a8d5f4a71b` | Historical A draft, 28 dirty tracked entries initially; not interchangeable with clean comparison tree. |
| `sweep-to-payout-src` | `994aede0ca7b2631e65ec3857f48b71888e0316f` | Historical Track B xpub direct-payout implementation; selected ownership/settlement/test review. Historical deployment ledger also names `4d041f7f2f13ba0a5a95e540c70d6aba60f9bb50`; distinct revision. |
| `sweep-to-payout-per-key-src-9cb02072` | `af52be109466e47de96b0fed0e42c7688420a06c` | Historical Track C per-key ownership implementation; selected registration/settlement/test and migration-contract review. |

[git-inventory.json](git-inventory.json) preserves initial HEADs, branches, tracked changes, diff hashes and discovered worktrees. A worktree listed there was **discovered, not necessarily reviewed**. [scope-closeout.json](scope-closeout.json) preserves ending Git observations and the two changed source anchors. The older `/home/francis/abandon-ship-bark` variant-B tree and many evidence-local frozen worktrees were discovered through Git inventories and reports; selected historical references were followed, not every checkout audited.

[source-evidence.json](source-evidence.json) contains **55 exact source anchors**, including repository, full revision, file, symbol, line, SHA256 and a bounded code fact. These cover the relevant attribution, ownership, Lightning, locking, geometry, splitting, payout, funding, reorg, history, SDK, Noah and historical paths. Individual catalogue entries expand the anchors. [test-source-inventory.json](test-source-inventory.json) indexes names/locations/hashes in **35 relevant test-source files**; only specifically discussed test bodies were substantively reviewed. A test name, listing or component assertion is not a completed financial scenario.

## Contract reconstructed from plans and owner rulings

The accepted selection is fresh upstream-based permanent xpub attribution plus sweep-to-payout. The required plans are:

- [Final backend plan](../abandon-ship-final-plan-2026-10-16.md), including its owner rulings and locked acceptance requirements.
- [Client plan](../abandon-ship-client-plan-2026-10-16.md).
- [Simulation rework plan](../simulation-rework-plan-2026-10-16.txt).

The contract requires permanent authenticated registration without payer-carried ownership hints; mailbox candidates verified by the registered xpub; resumable coverage for each pending key/account pair; no finite scan prefix as proof of no owner; and late keys/registrations starting their own coverage. Unknown customer ownership must not become operator money. **Current `NoRegisteredOwner` remains Hold**; catalogue hypotheses do not treat that as permission to allocate operator value.

Ordinary upstream refresh/offboard behavior must remain available until the payout decision commits. Conflicting financial writers and payout decisions need shared database exclusion and fresh locked revalidation, beyond process-local guards. Lightning admission is narrow: refuse genuinely new expired-source grants/first dispatches, preserve binding recovery/idempotency, and allow a bounded HTLC deadline to exceed backing expiry where the accepted design does so.

Each actual payout output costs 1,000 sat, followed by its script-specific dust rule. Independently ready same-expiry trees should group opportunistically when they fit without waiting for unrelated/unready trees. Miner fees come from usable watchman funding. Oversized admitted trees require authentic safe progress/splitting; a signed next transaction alone does not prove every resulting branch can recover. Canonical chain facts must rebuild after reorg without losing stable obligations, ownership evidence, fences or disclosed knowledge.

Clients must distinguish reported Spent from independently justified discharge, provide coherent balance/history, preserve binding refresh/cancellation recovery, discover actual payouts after restoration and demonstrate a real recovery spend. A transaction in the mempool or a row called Paid is insufficient.

Unresolved boundaries are kept explicit:

- The trusted database/server settlement model and K3a's client protection against misleading server Spent reports are different threat boundaries; server trust cannot silently waive K3a.
- Truly unregistered legacy or hardened-index owners cannot be identified from nonexistent or mathematically insufficient registered xpub material. Current SDK registration/normal-index admission reduces new exposure but does not settle old liabilities.
- P3's exact opportunistic grouping opportunity across a 64-root pass boundary needs a ruling. P1 expressly permits multiple outputs and fees; separate readiness or prior committed groups do not automatically violate policy.
- C4 assigns a delivered but server-Unregistered ordinary transfer to its input owner. That accepted rule and recipient expectations must be resolved explicitly where relevant.
- Ark-seed-only restore cannot derive an unrelated external BIP84 wallet's private keys. The separate external seed is a prerequisite. Native default BIP86 and qualified BIP84 feature builds must also be distinguished.
- Current receive fees are computed at preparation; an immutable earlier net quote is not invented without a binding policy/quote.
- Authorized external forwarding is permitted. A different node or wallet address alone is not proof of wrong ownership; unauthorized destination/action reassociation is the scenario.
- A fresh database or retired treasury sweep does not prove funded historical customer rights were discharged. No production throughput or recovery SLA follows from synthetic registrations, scheduled operations or harness timers.

## Retained observations and what they establish

[document-evidence-index.json](document-evidence-index.json) pins selected plans, handoffs, receipts and bounded logs by bytes/hash. It is an index of the cited subset, not every workspace document.

| Observation | What was actually shown | What it does not show |
| --- | --- | --- |
| ML008, batch09 `930221cb` | Stock CLN no-part case failed its 30-second terminal-proof wait; full case 44.737 s. Companion cases passed. A separate adapter pairing passed in `c0a5f364`. | Zero SQLite parts alone do not prove dispatch quiescence; adapter success does not qualify stock packaging. No permanent customer loss demonstrated. |
| ML061, actual Noah callback | Actual HTTP 200 / 5,000-sat invoice lacked LNURL metadata-hash binding; strict verifier rejected it. `payment_attempted=false`. | No paid entitlement, fallback loss or positive real Noah receive was demonstrated. |
| ML101, indexed attribution run | Wrong-mailbox 10,000-account case failed at 120.139 s with 9,902 work units; absent-mailbox companion passed at 103.265 s. | Work units are not distinct owners; synthetic keys are not funded throughput. Later batched/fenced commits change source, so the original timing is not a current benchmark result. |
| ML105, original S03 | Historical forfeit geometry blocked the transferred 10,000-sat entitlement; another root paid 28,670 sat. Native diagnosis and separate corrected runs exist; later repair paid 9,000 sat and recovery spent it. | Repair does not turn original grouping into a pass. Two payouts total 37,670 sat/two fees; simultaneous grouping readiness/one-fee requirement was not independently established. |
| Batch14 `61d55416` | Genuine external 10,000-sat receive, zero receive fee, **materialized/registered** recipient claim, provider permanently stopped, recipient absent/no mailbox; correct 9,000 sat >=6 confirmations, full relevant lineage exclusion through HTLC deadline, new seed-only DB discovery and real spend. | Not a permanent stop before materialization, nonzero-fee receive, stock Noah or public-Signet Lightning qualification. |
| Batch15 `e968e1cf` | Genuine Accepted-then-Cancelled external send; offline sender's 99,000 sat confirmed >=6; source terminal proof, one dispatch and parent conservation. | No sender seed recovery, full-lineage/postconfirmed-ledger/reorg or cancellation-during-downtime proof. |
| Normal internal `f464ce5c` | Retained newer raw log has two normal binding cases PASS, including completed internal materialization/idempotency. | Not zero-grant cancellation, pre-materialization permanent absence or both-clients-absent refund closure. Older status metadata saying unrun was stale. |
| Batch16 `63a761b8` | Newer retained corrected fixture PASS164.064 s: 327 owners, 635 signed paths, two real oversized levels, each 5,000 -> 4,000 sat, min seven confirmations, full canonical DAG conservation. | Not arbitrary historical shape, inventory-cap, restart/reorg, sustained load or production capacity proof. Original batch12 expected-one/actual-four checkpoint failure was a fixture failure and remains preserved. |

The batch16 positive receipt arrived during research and was incorporated into affected scenario coverage; earlier “corrected source unrun” descriptions were removed. Owner-scan publication `8fb729a2f` records six PostgreSQL component passes (`e868e1e6`); that protects particular races and does not replace the retained wrong-mailbox deadline failure with an unobserved new timing result.

[artifact-verification.json](artifact-verification.json) independently rehashes **11 local retained ELF files across three captured runtimes**, all matching their manifests. Receive-binding runtime source manifest is `c60c5cd4c9d8cc0fbeaa8f8f6a71ba4102a46075cd2d9515fb505d9ba2dc882b`; internal-receive overlay runtime is `f70bdd0dc7f46576b8b688a8b45e61779afb2831afeec44f2dcd6c8775099f5b`; corrected S03 runtime is `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69`. File-level comparisons distinguish old and newer source. The later batch16 artifact hashes are **receipt-reported**, source manifest `704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06`; they were not added to the independently rehashed eleven.

**Built, deployed and current are separate.** Retained short-profile Signet deployment receipts identify dirty-fb85 corrected S03 products; long-profile ordinary and Lightning campaign artifacts have other pins. No SSH/live process/hash or current-chain query was performed. Thus no statement here independently certifies which artifact is running now. A branch label, current clean tree or coordinator readiness claim cannot supply that proof.

## Scope limits and unavailable evidence

The three mandatory plans, selected client/backend handoffs, testing-track findings/receipts, simulator S03/Noah/repair records, original batch09 diagnosis, selected native logs and retirement state were inspected. The owner-scale archive member `scale-data/server/expiry_registration_authenticated_scale/authenticated-registration-scale.json` was streamed only; its SHA256 is `cfaaf6244a0ccbb966791858f68e0a43e9e085a26b1713f95c41681a99b79fc1`. Other large source/test/database archives were not extracted or exhaustively read.

[journal-inventory.json](journal-inventory.json) records five coordinator/event/public-message/retirement files by path, size, hash and line count. Most session journals were **metadata-indexed only**; selected financial rulings/reports were followed into source/receipts. No claim of an exhaustive conversation audit is made. The full historical first failure behind some reported same-hash ownership regressions was not located as an original raw run; those are historical regression risks, not observed failures in this catalogue.

Some old private Core trees were inaccessible, including `sweep-to-payout-evidence-2026-10-13/simulation/live48-core-audit-private/bitcoind/regtest` and `live48-core-reindex-private/bitcoind/regtest`; a private `__pycache__` directory was also inaccessible and irrelevant. No permission changes or recovery attempts were made. No live remote databases, Lightning-node histories, full backup restore, production inventory or complete source archive audit was performed. Public invoices, summaries and hashes cannot replace those missing financial observations.

The most important next-phase gaps are concrete interrupted internal-send recovery with both clients absent; permanent-stop forwarding before signed materialization; forwarded stuck-receive escalation retaining the beneficiary; stock/typed CLN terminal-proof packaging; funded attribution under sustained arrivals; real inventory-cap/heterogeneous split liveness; source-specific reorg/backup replay; and a client false-Spent/empty-history protection oracle. These gaps are expressed as scenarios, not requests to run new experiments during this assignment.

## Scenario and oracle semantics

“Source-supported risk” means a concrete path supports the conditional mechanism, not that a new run reproduced monetary loss. “Hypothesis requiring validation” keeps prerequisite reachability explicit, including imported/legacy states, faulty receipts, concurrency and capacity assumptions. “Historical regression risk” retains earlier behavior while acknowledging current protection. “Observed failure” is limited to the retained observation above.

Every future oracle starts with independently known customer ownership, signed source, amount and intended script; follows the **whole affected economic lineage**, not just one expected recipient; verifies actual canonical outpoints, effective Lightning outcomes and authorization; counts principal/fees/miner costs once; checks for extra recipients/duplicate discharge; and demands eventual progress. No row status, RPC success, bare preimage or mempool entry establishes payment. Already-confirmed BIP84 outputs may remain recoverable by direct chain scan even if authenticated history fails; both paths must be assessed separately.

`catalogue_data.json` and the small Python files are research document assembly inputs/helpers only. They are not product changes or new test implementations. The CSV contains additional root-cause/evidence columns to support later test planning.

Closeout counts use explicit subject matter: ML001–ML080 are the 80 dedicated Lightning scenarios, and ML180 is a further Lightning-specific provider/beneficiary recovery scenario, giving **81 total**. Cross-cutting ordinary wallet cases are not added merely because their inputs could have originated from Lightning.
