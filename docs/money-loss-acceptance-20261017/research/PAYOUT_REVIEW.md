# Independent payout, attribution and client review

This supplemental review supplies candidate root causes for the main catalogue; PR identifiers below are local research identifiers, not additional ML scenarios. No builds, tests, services, financial operations or source changes were performed. Existing test sources were read; their existence does not imply execution of current HEAD.

## Revision and scope

Inspected `/home/francis/abandon-ship-fresh-integration`, HEAD `751bd5a5d8ec9bbf72f0a44c1e7d4ac371b6269d`. At inspection, tracked changes existed in server/src/database/expiry_lightning.rs, server/src/ln/mod.rs, server/src/ln/node_manager.rs and testing/tests/bark_sdk/lightning_receive_binding.rs. Seven nested worktrees were untracked entries. The files examined for this supplement were not those dirty files. This is current workspace source, not proof of deployed source.

Read AGENTS.md, contrib/agents/agents.md and contrib/agents/skills/review.md. The user's explicit read-only research scope supersedes the review skill's build check and default output location. Read the final server plan and client plan dated 2026-10-16. Read source in expiry_attribution, expiry_owner_scan, fallback registration, expiry_inventory, expiry_geometry, expiry_readiness, expiry_decision, expiry_payout, watchman/payout, watchman/recovery, watchman/settlement, bitcoin-ext/src/bdk.rs, bark fallback/adoption, protection, act_by, sync, reconcile, payouts, import/recovery, relevant lib/vtxo/daemon code, and the attribution, owner-scan, split, payout-flow, reorg, external-BIP84 and adoption test sources.

Read docs/fallback-registration-scale.md and targeted fields of docs/evidence/owner-scheduler-indexed-20261017.json. The latter pins build manifest `4af4b54efb5079b87379a7322bc7d58c5bcb483e720a5d248c1de7a5fe9a4d80`, captaind ELF `edebcb6953e835301b910b88192c1908e6f0829f527583d0f5e6d935c6a8d5aa`, watchmand ELF `a3deb3f9b4dd71fbe297f826650c01b3c189b1eb700a6f24ad9cd05b072f0fe3`, native-server ELF `3309fcca545dcf4c011b49c6076c7b7209806a1ea981af77e0b0e1b5d41c91c2`, source components 9384441b3, 792c8ea7d and 3d7a3fd4a. Those stored hashes were read, not independently recomputed over archived executables here. No source archive was extracted. No production-state inspection occurred. A broad initial file listing encountered permission-denied private bitcoind/Postgres directories; this supplement does not claim their contents were reviewed.

## Strong release blockers

**K3a is not satisfied by the ordinary-Spent adoption path.** `bark/src/fallback/adoption.rs:83-85` only waits when an entitlement exists but lacks verified payout. If authenticated post-status history is empty, it proceeds to mark the signed coin Spent at :105, clears quarantine and awaiting-settlement at :110-112, and removes it from `act_by.rs:155`'s Spendable/Locked watch. `validate_vtxo` at `bark/src/lib.rs:1603-1608` checks the original signed coin against funding, not confirmed fate. No `adopted_payout_vtxos` marker is recorded when paid=false, so protection.rs:137's repair path cannot restore this retirement. The history barrier provides freshness/consistency, not proof that an omitted entitlement does not exist or that an ordinary spend confirmed. This is a source-supported risk, not an observed hacked-server runtime failure.

**Oversized inventory cannot reach its own splitting mechanism.** `expiry_readiness.rs:28-30` caps an anchor at 250,000 rows/256 MiB; :155 requires full-anchor snapshot before current economic cut or automatic split. `expiry_inventory.rs:253` selects all anchor history; :301-302 refuses excess. `request_expiry_split` at expiry_payout.rs:185-186 repeats the same full-anchor limits. Successful splitting does not remove historical anchor rows. Source therefore has no progressing path for a sufficiently large otherwise valid anchor, and a stale historical record can affect current roots before current-cut isolation.

**Bumpable funding is narrower than wallet balance.** Fee replacement uses a new `build_tx`, excludes unconfirmed outputs and cannot reuse a funding outpoint already consumed by the old child. The existing replacement test explicitly supplies two confirmed sponsor outputs (`expiry_reorg.rs:179-182`). A single large sponsor becomes unconfirmed change after the first child, leaving no eligible second sponsor for fee increase. This is a source-supported liveness limitation even when value held by the sponsor wallet exceeds the needed fee.

## Candidate mechanisms

All references below are relative to the repository and HEAD above unless a historical evidence pin is explicitly given. Existing tests are described by assertions read, not by names alone.

### PR01 — Empty post-status history retires an unpaid signed coin

- Invariant/applicability: current; a server assertion cannot discharge a customer claim or remove its exit watch.
- Preconditions: valid signed Pubkey coin, authenticated registration, no locally cached entitlement, malicious or stale-backup server returns Spent and a complete empty account history.
- Mechanism: adoption validates original funding, interprets absent entitlement as ordinary historic spend, sets Spent and removes quarantine. Act-by enumerates only Spendable/Locked.
- Correct: retain pending protection until independently confirmed payout, round/offboard or other permitted final fate.
- Outcome: customer may lose unilateral recovery window; operator could sweep expiry while client has stopped protecting. Loss becomes permanent when expiry path wins and no entitlement is retained.
- Evidence: adoption.rs:38-115, particularly :83-85,:105-112; lib.rs:1603-1608; act_by.rs:155; protection.rs:137.
- Coverage: `fallback_adoption_history_failure_quarantines_then_reconciles_an_ordinary_spend` actually sends Arkoor at test :158, imports after dropping the coin, and asserts Spent/:170 plus empty schedule/:171. It proves compatibility with an actual ordinary send; it never substitutes false Spent plus omission for an unpaid coin. The post-status-barrier test proves freshness after a genuine decision, not truthfulness of empty history.
- Oracle: original signed principal remains watched; no confirmed rightful-script payout/confirmed final fate implies no final Spent adoption, through act-by.
- Priority/status: P0; Source-supported risk. Root: unauthenticated negative financial inference.

### PR02 — Full-anchor inventory budget prevents all splitting and payouts

- Invariant/applicability: current; valid oversized trees must make eventual progress.
- Preconditions: one anchor has over 250,000 retained rows or 256 MiB serialized VTXO history, even if each current payable output fits a sweep.
- Mechanism: full snapshot refuses before economic cut and request split; split independently requires identical bounded snapshot; history remains under same anchor after splitting.
- Correct: complete ownership/conservation with bounded progressing preparation rather than permanent refusal.
- Outcome: all rightful users under anchor remain unpaid indefinitely without external reconstruction or code changes.
- Evidence: expiry_readiness.rs:28-30,:153-155; expiry_inventory.rs:245-302; expiry_payout.rs:180-186.
- Coverage: inventory unit assertions prove budget refusal; `expiry_requested_split_preserves_mixed_siblings_and_eventual_refund` uses small actual tree and explicitly calls split at :75-83. Neither proves over-budget inventory eventually progresses.
- Oracle: monotonically decreasing independently measured remaining payable backing and eventual confirmed customer outputs without raising limits or deleting history.
- Priority/status: P0; Source-supported risk. Root: hard fail-closed resource bound without continuation.

### PR03 — Invalid historical metadata poisons a valid current branch

- Invariant/applicability: current; unrelated historical descendants cannot strand the current economic owner.
- Preconditions: an unused historical row beneath an anchor has an encoded/column mismatch or wrong anchor metadata, while current selected path is valid.
- Mechanism: backing_snapshot decodes and cross-checks every historical row before economic_cut can discard obsolete branches; source_transitions similarly checks every full VTXO's funding anchor.
- Correct: preserve evidence and isolate invalid unused historical facts from verified current terminals; never sweep an incomplete partition.
- Outcome: legitimate current users get no payout despite complete valid current path; repair may require retained original source evidence.
- Evidence: expiry_inventory.rs:306-324; expiry_geometry.rs:85-87; readiness.rs:155-163.
- Coverage: `expiry_current_cut_ignores_invalid_unused_historical_descendant` creates invalid unused descendant but invokes economic_cut on a prebuilt in-memory snapshot; it does not traverse the production snapshot decoder/metadata checks.
- Oracle: actual DB inventory containing invalid unused row still permits a separately proven current terminal payout; no historical recipient also receives value.
- Priority/status: P1; Source-supported risk. Root: validation applied before economic branch isolation; related to PR02 but independent trigger/protection.

### PR04 — Exhaustive high-index attribution is operationally unrecoverable

- Invariant/applicability: current; issuance below 2^31 must not strand an offline recipient because ownership lookup is impractically slow.
- Preconditions: rightful registered account has a valid key far beyond ordinary scan prefixes and no useful mailbox; many registered accounts.
- Mechanism: derive contiguous prefixes per key/account until match; full keyspace may require billions of derivations per candidate record. No operator appropriation occurs, but ready output remains Hold.
- Correct: actual complete recovery within an explicitly accepted operational bound; policy ambiguity must remain visible if no bound was accepted.
- Outcome: payout delayed beyond useful lifetime, potentially practically permanent when necessary operational infrastructure/evidence is later lost.
- Evidence: expiry_attribution.rs:52-63,:127-173; readiness.rs:49-56. Final plan C2 permits all nonhardened children. Indexed report pins above retains low-index wrong-mailbox timeout after120.139s at10k registrations.
- Coverage: last-normal-child unit begins at OWNER_KEYSPACE_END-2 and derives two keys; no full search latency proof. Scale test low indices18/19 is not high-index recovery.
- Oracle: time-to-verified-owner and eventual actual payout for accepted issued high-index key under pinned source/hardware; never classify timeout no-owner.
- Priority/status: P1; Source-supported risk. Root: exhaustive search cost, distinct from incorrect cursor coverage.

### PR05 — Continuous low-prefix arrivals starve an older deep search

- Invariant/applicability: current; every existing entitlement must eventually resolve under sustained legitimate activity.
- Preconditions: unresolved older high-index key; sustained newly queued keys/registrations create lower-prefix cursor rows faster than workers consume them.
- Mechanism: fair scheduler orders by scanned_up_to then mailbox; preferred low mailbox prefixes additionally outrank older higher prefix. Continuous zero coverage can keep deep account unselected.
- Correct: bounded fair progress for existing searches while admitting new work.
- Outcome: older offline user's fallback stays unavailable indefinitely although system continues resolving new users.
- Evidence: expiry_attribution.rs:133-154 and :161-162; expiry_owner_scan.rs:20-31.
- Coverage: duplicate-cursor test queues finite three accounts and six keys, calls three passes and asserts matches; late-arrival test adds one key. Neither supplies ongoing arrival stream and proves age fairness.
- Oracle: every older search advances contiguous coverage and pays despite bounded ongoing new-key arrivals.
- Priority/status: P1; Hypothesis requiring validation. Assumption: arrivals can outpace low-prefix service long enough to dominate scheduling. Root: prefix priority without age guarantee.

### PR06 — Registration/pending-key cross product exhausts storage before attribution

- Invariant/applicability: current; financial liveness survives many genuine pending keys and permanent registrations.
- Preconditions: many registered accounts and many unresolved keys, particularly missing/wrong mailbox keys.
- Mechanism: queue_expiry_owner inserts one scan pair for every registered account; registration inserts scan rows for every pending key. Match cleanup occurs only once a key resolves. Shared/exclusive registry advisory lock serializes this work against registrations/decisions.
- Correct: maintain bounded useful progress and account lifetime registration without starving decisions or consuming all available storage.
- Outcome: disk/DB exhaustion can stop new registrations and payouts; unresolved users remain unpaid, backup recovery may lack latest obligations.
- Evidence: expiry_attribution.rs:70-90,:198; fallback.rs:22-24,:37,:50; migrations/V3:16-20.
- Coverage: authenticated scale is primarily sequential registrations then isolated owner cases; does not exercise large simultaneous pending-key × permanent-registry state. Initial environment has known disk exhaustion history, but no claim this mechanism caused it.
- Oracle: measured persisted scan-row cardinality, storage growth, registry lock waits and eventual payouts during simultaneous real pending-key load; protect original obligation records across exhaustion.
- Priority/status: P1; Source-supported risk. Root: multiplicative persistent search state.

### PR07 — One ownership-revalidation error rolls back other ready trees

- Invariant/applicability: current; independent ready roots must pay despite another root becoming unready.
- Preconditions: several ready trees same expiry grouped; one member's ownership revalidation returns an error (not merely changed disposition).
- Mechanism: commit_expiry_group's per-coin propose_expiry_ownership(...).await? or payee lookup aborts entire transaction, rather than adding one dropped root; caller logs group error and rolls back all.
- Correct: preserve atomic decision for accepted group while independently dropping the errored tree, or otherwise guarantee other trees eventually regroup.
- Outcome: persistent error member repeatedly prevents other customers' payout if deterministic regrouping repeats.
- Evidence: expiry_decision.rs:183-194; expiry_readiness.rs:131-143,:244-247.
- Coverage: readiness isolation requirements and geometry tests establish rejected preparations; they do not establish isolation for an error arising only after group preparation. No exact runtime case located here.
- Oracle: confirmed payouts for unaffected ready roots with repeated revalidation error in one same-expiry member and no silent confiscation for errored member.
- Priority/status: P1; Hypothesis requiring validation. Root: transaction-wide error propagation after grouping.

### PR08 — Single sponsor coin cannot support a needed fee replacement

- Invariant/applicability: current/shared dependency; enough sponsor value must be usable for eventual fee bump.
- Preconditions: watchman has one large confirmed UTXO; initial fee child spends it and remains unconfirmed; target rate increases.
- Mechanism: new child build selects only confirmed unspent funding; old child's change is unconfirmed and old input already spent. has_trusted_balance may refuse before build, or builder cannot fund replacement.
- Correct: preserve rightful parent/payees and restore confirmation progress using accepted funding policy; explicitly distinguish locked liquidity from actual exhaustion.
- Outcome: every customer in parent may wait indefinitely despite substantial sponsor change; expires only if other confirmation or new confirmed top-up arrives.
- Evidence: watchman/payout.rs:93-115; bitcoin-ext/src/bdk.rs:414-421.
- Coverage: expiry_payout_fee_replacement_keeps_parent_and_counts_only_confirmed_child intentionally creates TWO100k sponsor outputs at expiry_reorg.rs:179-182; proves replacement with fresh sponsor, not single-sponsor bumpability.
- Oracle: with exactly one adequate initial sponsor outpoint, actual unchanged payout parent eventually confirms after fee rise; actual confirmed fee and rightful output values checked independently.
- Priority/status: P1; Source-supported risk. Root: replacement funding liquidity.

### PR09 — Crash between fee-child DB storage and wallet persistence splits reservations

- Invariant/applicability: current; watchman funding cannot be promised inconsistently across durable stores.
- Preconditions: fee child signed and inserted in Postgres, process dies before wallet.commit_tx/persist; another allocation may be processed after restart.
- Mechanism: authoritative expiry child exists at payout.rs:131 while wallet reservation starts at :136-137. Recovery order must ensure stored input is reserved before selecting it elsewhere.
- Correct: replay durable child reservation before funding another child, or rebuild equivalent consistent state.
- Outcome: mutually conflicting fee children delay a customer group, or old sponsor commitment is forgotten; no customer double payment established by source alone.
- Evidence: watchman/payout.rs:131-137; pending_expiry_payouts orders allocations by expiry_height/id at expiry_payout.rs:98.
- Coverage: replacement test includes restart after published child; it does not kill precisely after DB insert before wallet persistence or restore databases at mismatched positions.
- Oracle: actual funding outpoints never counted usable by two durable active packages; eventual confirmed payouts for all committed groups after boundary recovery.
- Priority/status: P1; Hypothesis requiring validation. Root: cross-store commit boundary; assumes restart wallet does not reconstruct reservation first.

### PR10 — One malformed pending allocation stops all payout loading

- Invariant/applicability: current; unrelated customer groups survive corrupt/unreadable historical pending state.
- Preconditions: one pending allocation has invalid raw child, invalid script/value field or unreadable VTXO blob; other pending groups are sound.
- Mechanism: pending_expiry_payouts materializes/deserializes every pending group into one Vec and returns Err on first malformed item; per-group error isolation starts only after full loader succeeds.
- Correct: preserve malformed evidence and keep sound independent packages progressing without declaring malformed group paid.
- Outcome: all pending users delayed by one allocation until repair/restoration; prolonged loss of infrastructure could make recovery permanent.
- Evidence: expiry_payout.rs:97-119, notably :103,:108-115; watchman/payout.rs:75-80.
- Coverage: happy-path flow validates one correctly encoded parent; no malformed-item isolation assertion inspected.
- Oracle: at least unaffected groups confirm with full amounts/scripts while malformed group's entitlement remains durable and unpaid.
- Priority/status: P1; Source-supported risk. Root: global batch decoding before isolation.

### PR11 — Lifetime reorg rebuild scans all historical transactions on each changed view

- Invariant/applicability: current; recovery cost cannot indefinitely overtake chain progress or new liabilities.
- Preconditions: long-lived server with many allocations and replacement children; block progress changes view while serial rebuild RPC traversal is slow.
- Mechanism: rebuild selects every allocation/fee child, queries every tx status sequentially, then verifies original view; changed view/revision can force another full pass. No100-block retirement filter appears here.
- Correct: preserve deep reorg guarantees and prove bounded eventual publication at retained-history scale.
- Outcome: perpetual reconstruction prevents fresh payout decisions/broadcast, stranding all offline users while obligations remain fenced.
- Evidence: watchman/recovery.rs:42-65,:73-79; expiry_chain.rs:94-106. Final plan C9 says stop following allocation after100 blocks; current rebuild still includes all.
- Coverage: actual deep payout reorg test covers a small history; it does not establish lifetime cost,100-block retirement or progress when per-pass work exceeds new-view interval.
- Oracle: actual payouts continue during realistic accumulated allocation/feechild history and advancing blocks; count/revision watermark stabilizes enough to make progress without dropping debts.
- Priority/status: P1; Source-supported risk. Root: unbounded historical reconstruction.

### PR12 — Unavailable old transaction aborts unrelated reconstruction

- Invariant/applicability: current/shared chain dependency; one historical lookup must not permanently freeze unrelated money.
- Preconditions: retained old fee child/claim transaction cannot be queried by current Core setup (pruning/index change or transaction-specific RPC error); newer liabilities have complete evidence.
- Mechanism: serial tx_status(...).await? or get_raw_transaction(...).await? fails whole rebuild before finishing valid view.
- Correct: require sufficient chain evidence for affected facts while independently advancing unrelated fully proven liabilities, or declare explicit required archival dependency before release.
- Outcome: all users fenced until old chain evidence restored; archival loss may prevent eventual payout even for independent trees.
- Evidence: recovery.rs:60-71; old feechild collection :47; full view barrier at :111.
- Coverage: reorg tests use actual full-index regtest node with all generated transaction data; do not remove historical RPC availability.
- Oracle: independently proven unrelated entitlement reaches correct confirmed output while unavailable historical fact remains unresolved; otherwise document and verify permanent indexing requirements.
- Priority/status: P1; Hypothesis requiring validation. Assumption: tx_status errors for specific retained historical transaction under supported configuration. Root: global external-evidence dependency.

### PR13 — Alternating daemon restarts continually invalidate the shared generation

- Invariant/applicability: current; ordinary rolling recovery must eventually authorize payout work.
- Preconditions: captaind/watchmand restart or crash independently while other process is syncing; overlapping startup invalidations.
- Mechanism: each listener start invalidates from zero and deletes its own ack; both current-generation acks and rebuilt exact views needed. Repeated counterpart starts can invalidate the other's just-published ack indefinitely.
- Correct: converge after bounded healthy restart sequence without forgetting permanent decisions; monitoring must expose convergence failure.
- Outcome: every customer waits even with funded watchman and healthy chain; permanent if operational restart loop persists undetected.
- Evidence: expiry_chain.rs:29-34,:64-78,:87-89.
- Coverage: shorter-fork test stops watchman once, invalidates then restarts and proves both tips converge; no repeated alternating start sequence under active payments.
- Oracle: after final restart and healthy dependencies, both ack generations converge and real outstanding payout confirms; no previously accepted entitlements disappear.
- Priority/status: P2; Hypothesis requiring validation. Root: dual-process synchronization liveness.

### PR14 — One problematic cached receipt suppresses protection for all coins

- Invariant/applicability: current; a due unsigned financial claim must retain act-by protection even if unrelated history lookup fails.
- Preconditions: client holds due standard coin and an older cached payout candidate whose chain lookup errors or entitlement amount validation fails.
- Mechanism: protect_expiring_vtxos invokes verify_fallback_chain_evidence before enumerating schedules; its per-entitlement/chain queries propagate errors with ?, so due exit loop is never reached.
- Correct: unknown older receipt must remain pending while independent due signed coin receives protective action.
- Outcome: customer loses exit window if recurring specific receipt failure persists past act-by; operator expiry path may then prevail.
- Evidence: act_by.rs:57-67 then :112-140; protection.rs:57-60,:76,:82.
- Coverage: protection suite demonstrates server pagination cannot hold protection lock; it does not establish per-receipt chain-error isolation. No runtime failure observed here.
- Oracle: make one historical candidate unavailable while another due coin's funding/path remain independently available; due coin's real exit starts and confirms by rightful clause.
- Priority/status: P1; Source-supported risk. Root: history-wide prerequisite before per-coin safety.

### PR15 — Unknown seed-restored entitlement can reuse an unrelated confirmed payout

- Invariant/applicability: current; each confirmed payment must be tied to the actual signed entitlement lineage, not merely a correct script and available output value.
- Preconditions: seed-only wallet has authenticated registration but no local full VTXO for an account-history entitlement; server returns a plausible receipt referring to a real confirmed output at registered script.
- Mechanism: amount check only occurs if local coin exists (protection.rs:58-60), and source-path linkage only if full coin exists (:89-96). A seed-only account can accept receipt without independently established original source/value lineage.
- Correct: recovered history must obtain independent lineage evidence before asserting that specific entitlement is discharged; never count one output for more customer value than actual.
- Outcome: client may report paid for a missing entitlement while actual correct-script funds originated elsewhere; user may not identify omitted debt before evidence disappears.
- Evidence: protection.rs:57-60,:88-99; sync.rs:144-147; AccountPages checks identity/cursor consistency, not independent owner lineage.
- Coverage: external BIP84 actual payout test validates genuine backing+actual outpoint and real spend, but supplies honest server history. Output-group duplicate unit checks one local account, not separately fetched unknown histories.
- Oracle: independently establish source outpoints, original gross amounts, rightful script and aggregate payment across recovered accounts; unrelated correct-script deposit cannot discharge new debt.
- Priority/status: P1; Hypothesis requiring validation. Root: absent local source proof during seed-only history restoration.

### PR16 — Large mixed tree cannot split across a binding settlement boundary

- Invariant/applicability: current/historical regression; a valid oversized current cut containing round/offboard obligations must eventually progress without traversing obsolete forfeits.
- Preconditions: root needs splitting; one terminal is a binding round/offboard input with historical oor_spent_txid/forfeit geometry; remaining branches have recoverable paths.
- Mechanism: economic_cut explicitly stops at offboarded_in/spent_in_round, but split_recovery_paths does not mirror those terminal boundaries; it follows oor_spent_txid and requires a one-input signed transition. A historical connector-bearing forfeit can fail this split check or make recovery return None.
- Correct: preserve binding entitlement/replacement and original recovery capability while splitting only valid current economic paths.
- Outcome: legitimate owners in oversized mixed root stay unpaid; following wrong branch would instead risk forfeiting the wrong customer's economic value.
- Evidence: geometry.rs:41-44 versus expiry_payout.rs:48-75. docs/fallback-registration-scale.md explicitly preserves earlier historical-forfeit geometry failure, not cured by scheduler.
- Coverage: geometry test stops at settlement boundary in-memory; split test has Pubkey+HTLC, no binding round/offboard terminal. No current runtime reproduction established.
- Oracle: actual oversize mixed tree with accepted forfeit/offboard yields complete conservation across old backing, signed replacement and all rightful payouts; no duplicated payout to former owner.
- Priority/status: P1; Hypothesis requiring validation. Root: split/current-cut semantic mismatch.

### PR17 — Conservative split refusal leaves ready siblings behind unresolved ownership

- Invariant/applicability: current; independently owed ordinary sibling eventually pays even when another terminal cannot be attributed.
- Preconditions: current economic root contains ready ordinary customer and another legitimate but unregistered/unresolved owner, including a Lightning-derived sibling; ordinary whole-root payout cannot be committed.
- Mechanism: prepare_expiry_anchor sets Hold for unknown owner and suppresses split if any hold (:211-217), even where signed recovery-preserving subdivision may exist. Forever-unregistered sibling prevents ready customer's whole-root payment.
- Correct: retain unknown customer's value and identify/execute a recovery-preserving subdivision when possible, or state the actual shared-backing limitation. Do not silently redirect unknown money.
- Outcome: known offline owner remains unpaid indefinitely because someone else's owner registration never appears. This is liveness failure, not proof unknown money goes to operator.
- Evidence: readiness.rs:47-56,:195-217; split_recovery_paths recovery proof :25-83.
- Coverage: mixed split test has known same wallet ordinary sibling and zero-attempt HTLC, and explicitly authorizes split; it does not have a permanently missing registration that sets Hold. Readiness isolation across different trees does not prove isolation within one backing output.
- Oracle: known sibling actual net output confirms while all unresolved sibling principal remains conserved under valid recoverable scripts and no operator appropriation.
- Priority/status: P1; Source-supported risk. Root: safety-preserving whole-root Hold without partial progress.

### PR18 — Partial payout below-dust history remains awaiting forever

- Invariant/applicability: current; accepted below-dust policy must reach an explained final state without pretending payment or suppressing other valid recovery.
- Preconditions: coin is legitimately below dust after per-output fee policy and server commits/broadcasts confirmed allocation to operator remainder.
- Mechanism: verify_fallback_chain_evidence considers only CustomerPayout (:68); BelowDust never appears in verified_payouts; adoption waits for entitlement&&!paid and awaiting_settlement persists. History projection may explain dust, but state finality remains pending.
- Correct: distinguish authorized fee/dust disposition from actual customer payment using independently checked confirmed conservation; no claim of a customer payout that did not occur.
- Outcome: money is already authorized operator money under P1, so do not label this additional theft. Financial status can stay perpetually unpaid/pending, trigger futile protective exits and conceal a genuinely missing payout among perpetual expected pending records.
- Evidence: protection.rs:54,:68,:117-125; adoption.rs:83-85; payouts.rs:129-131,:164-166. Owner P1 explicitly allows below-dust forfeiture.
- Coverage: payout history unit explains dust but cannot establish native independent terminal disposition and absence of repeated recovery attempts after real confirmation.
- Oracle: actual confirmed disposition, exact gross=authorized forfeit, no customer payout claim, no residual indefinite awaited payment or extra payout.
- Priority/status: P2; Source-supported risk. Root: client proof model only understands positive customer payment.

## Coverage that should be credited precisely

The external BIP84 test at testing/tests/bark_sdk/fallback_external.rs:108-226 is materially stronger than the earlier registration-only fixture. It creates distinct Ark/app seeds, funds and boards100k, stops the wallet before expiry, observes actual automatic zero-fee version3 fallback output, requires six confirmations, rebuilds both databases from the two original seeds, discovers account history without supplying source IDs, spends the exact fallback outpoint through the app trait object with a two-item P2WPKH witness, broadcasts and confirms recovery, and verifies original payout outpoint consumed. This source is real seed-restoration/spending coverage for one ordinary boarded coin, not evidence for all Lightning, multiple payouts, changed account paths, missing app seed or concurrent devices. The file's introductory comment claiming actual fallback still needs testing is stale relative to this second function.

The mixed split test explicitly invokes the first split and proves49k ordinary payout before HTLC deadline, no premature HTLC decision, later49k refund, distinct backing inputs and six confirmations for each. It does not prove fully automatic oversized fanout or all split levels.

The retained indexed scale measurement is a failure at the wrong-mailbox budget; the earlier missing-mailbox failure remains in the documentation. Neither constitutes actual loss of customer principal. Source preserves unresolved attribution and refuses operator sweep. The observed failure is latency against a defined120-second experimental budget under low-index debug-build conditions. Current HEAD may differ; the old result must remain separately pinned.
