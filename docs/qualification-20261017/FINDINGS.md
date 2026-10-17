# Fresh testing qualification findings

Base: `e44ddf95741478508908c17adc61752832d75c6a`.

First checkpoint: 17/17 case executions passed across 16 unique names and eight nextest invocations. Latest aggregate:23PASS/2FAIL across25executions,24unique names and13nextest invocations. One current-cut case was repeated with stronger history assertions. The two mailbox cases are component coverage; the other 15 executions use native operations. Mandatory `just checks` passed. Per-product/runtime pins differ and are explicit in the receipts. Production readiness remains open.

No new product verdict exists at startup. The explicit disk hold prevents builds, native test runs, large copies, services, funding and campaigns until coordinator release. The three workers inspect source and prepare a small inventory.

## Execution gates at startup (superseded by later allocation)

- Coordinator acknowledged release of disk hold: pending.
- Shared target build ownership and bounded runtime allocation: pending.
- Isolated worktree container visibility: absent; allocation requested.
- Host Nix shell: absent. Container cached flake environment verified with `IN_NIX_SHELL=impure`, Rust 1.90.0 and Core 31. No code edits occur before environment confirmation.

## Preserved known failures and limits

- The 10K wrong-mailbox search failed its 120-second deadline after 9,902 scan operations. These are not proven distinct records.
- S03 original Signet grouping failed on historical-forfeit geometry. A native correction exists. A separate repair cannot convert the original failed grouping into a pass.
- Corrected immutable runtime has dirty-source provenance and individually pinned products. Its manifest is preserved at `/home/francis/.cache/abandon-fresh/runtime-s03-current-cut-correction-20261017/manifest.json`.
- Canonical client finality files belong to another owner. This track does not alter, copy, stage, stash or reset them.
- Existing 12-wallet/60-transfer and 50-wallet/200-transfer runs are workload observations. They do not prove maximum throughput or equivalent distinct scenario coverage.
- Cross-root discharge, sustained load, legacy adaptation, stock-backend offline LNURL and production qualification remain open.

Each new run must retain its exact command, times, source and binary provenance, configuration, oracle and failure evidence. No new test pass is claimed.

## First allocated native runs

The coordinator released disk hold at 14:41 UTC and allocated one archived test at a time. The client/backend retain all shared-target writes. The isolated branch uses the coordinator-corrected nested path `/home/francis/abandon-ship-fresh-integration/testing-track-worktree` so it is visible in the existing container.

- PASS: current economic cut after sender change refresh, run `1949f9ef-8ab4-4d27-adcb-1fa7624f80fa`, 33.743 seconds. This permits one or two actual payout parents. It does not prove the original one-output S03 oracle.
- PASS: decision beats native prepared offboard, run `98579530-8ac9-46eb-a016-2abdf0c3d271`, 22.446 seconds. Genuine refusal retains the prepared checkpoint and action lock; the fallback pays and the offboard does not. Full client finality is separate.
- Registered-offboard-first reverse ordering: running.

These runs use corrected immutable native/server products built from recorded dirty `fb85d9a9c3b31b3eab1eb2113d2caec89c7f10dd` source, source manifest SHA256 `0ff7819dc89062aef8c6ce43b81d99c851fd813ef0ac75f0d462ef457deb6b69`, plus separately pinned older CLI products. They are not clean `e44ddf957` qualification. Per-run receipts record exact binary and metadata hashes.

## Reporting defect: search work is not distinct population

The prior 10K wrong-mailbox search result is a failed 120-second bounded run. Its 9,902 counter increments account-range work across passes and does not establish 9,902 distinct records searched. The 100/1K receipts report 116/1,016 work units, confirming that a work count can exceed the registry population. See `evidence-review.md` for exact source/receipt references. No scale pass follows from the timeout.

## Preflight corrections

- `--test server` conflicts with nextest `--binaries-metadata`; the list preflight exited 2 before running any product test. Runtime commands use one unique substring and metadata containing one binary.
- One metadata-list preflight ran as container root without the established shared `CARGO_HOME`, causing a 524MB registry download into new `/root/.cargo/registry`. Subsequent commands run as uid 1000 with `CARGO_HOME=/fresh-cache/cargo` and offline Cargo. Only that verified new root registry was removed; source, evidence and shared target/cache were preserved. No compile occurred.

## New native results and current release gates

- PASS readiness isolation, run `b4597c9f-0b79-40c5-9b2b-4865174a9791`, 26.184 seconds: independently ready same-expiry root pays 99,000 sat with six Core confirmations, while authentic pending HTLC backing remains unresolved and untouched before its deadline. The actual confirmed fee child, block hash, bytes and independently computed fee agree with the durable ledger.
- PASS stronger current-cut/two-root return history, run `e5d2a93c-c9e5-4d02-955f-76c419f53781`, 44.926 seconds: original absent-actor payout/confirmation oracle precedes reopen, authenticated per-output history and fees conserve the two coins, and total debit is exactly 39,670 sat once across all subsystems that consume either original coin. Movement IDs remain stable through repeated sync and reopen.
- PASS required `just checks`, approximately 159 seconds wall time, including style, clippy, wasm and workspace test/example type checks. This is not a unit-suite execution.

The test changes contain no product patch. Source review found an initial cross-subsystem debit blind spot and missing explicit canonical child confirmation; both were fixed and re-reviewed before the passing build/run. There were no new product-test failures in this checkpoint. The preserved original S03 and wrong-mailbox failures remain open under their exact oracles.

The bounded passing cases do not qualify automatic oversized/multilevel splitting, split/conflict/chain-only ownership rollback, native cross-root discharge or ambiguous Lightning dispatch, complete client finality on the newer published pin, stock Noah offline LNURL, sustained production scale, migrations/backup/restore or all original scenarios. Read CHECKPOINT.md for complete counts and pin limits.


## Lightning batch09: original no-part failure preserved

Run `930221cb-38b3-46f9-b88d-9c826febe4d8` finished in155.849seconds:3PASS,1FAIL. Real accepted-then-cancelled external payment and two same-hash source-liability/refund cases passed. The real no-channel/no-part case failed its unchanged30second terminal-proof wait (`Elapsed(())`),44.737seconds wholecase. Its raw state and original logs remain private and preserved. No retry or payment replay occurred.

Pre-run hashes show stock CLN26.06.6 gRPC plugin `421c19f3b0a2cf1c19b2d2f61737be7264c9ecca673e735f5fcb114198e362ea`. Genuine CLN returned RPC205, Unknown source node; stock gRPC returned Unknown text without `abandon-cln-jsonrpc-error-code`. Backend deliberately requires the typed metadata and retains ambiguity otherwise. The dispatch-terminal handoff requires adapter `6a752d8fb498b5757e8e6efc68e64f04fe3732f61010fe2d55f2c9df9d9637d1` with the exact hold plugin. This run did not satisfy that environment contract. It remains FAIL, and supplies no qualified no-part PASS or established product defect. Original-dispatch reconciliation is read-only with the backend lead. Any later qualified pairing needs a separately pinned run; it cannot replace this failed receipt.

Three next-source paths are prioritized: authentic source-bound external settlement during product downtime, registered recipient forwarding interrupted before mailbox delivery, and genuinely automatic327-payee multilevel splitting. These drafts are uncompiled/unrun. Stock Noah LNURL metadata-hash compatibility remains a separate unqualified gate. Restore/conflict drafts are preserved and frozen after the coordinator's Lightning priority.


The backend lead independently opened the original CLN SQLite with `mode=ro&immutable=1`, verified no WAL sidecar, and found zero matching and zero total payment parts. SHA256 was unchanged after observation. This corroborates the no-part reply but does not replace typed dispatch-quiescence proof. No node/payment/wallet was restarted or original financial row changed. Full sanitized reconciliation is in `batch09-original-dispatch-reconciliation.json`. The coordinator assigned a separately isolated exact-adapter positive gate to the backend lead; this track will not duplicate it. Stock transport packaging/capability qualification remains open even if that separate pairing passes.


The separate backend-owned adapter run `c0a5f364-a775-4ed0-9fe7-22286ec2eeb9` passed in 28.777 seconds. Independent source-manifest comparison verifies the original Lightning fixture bytes are identical, while the whole native test bundle differs. Actual typed 205 receipts and a 99,000-sat Core-confirmed payout support this exact adapter pairing. The confirmed ledger projection was not complete at shutdown. This result is excluded from our counts, preserves the original stock failure, and leaves default Nix packaging open. See `external-backend-adapter-positive-receipt.json`. All four source prechecks for the three new tests passed; compilation and runtime remain pending.


## Canonical second native batch and primary money invariant

Canonical source99a0553ac plus owned test patch/public-API correction compiled in51.41seconds; mandatory justchecks passed with unchanged captured1078sourcefiles. ImmutabletestELF50b881/source-manifestfd82 retain exactfullhashes in next-canonical-runtime-manifest.json. Firstcompilefailed on2private Xpriv.ckd_priv calls; same-singlechild publicderive_priv correction preserved originalfailedsource/log and changed no moneyoracle.

- PASS restart56.310seconds, ec0030d3:100kexternalsettlement duringcap/watchdowntime recoveredtheoriginalsource-labelleddispatch; no customerrefundanywherepaidanchor; unusedsamehashsource99kCore>=6confirmed. That refund is unusedgrantliability, not receivedswap proceeds or failed-dispatchedswap.
- PASS nativeforwarding/providerreopen28.617seconds,e380c4ca:actualregisteredrecipientoutputs/durableDelivering/exactrecipientmailbox/signedbytes/zero-net forwarding. Supportingonly: no expiry/rightfulrecipientfallback credit.
- FAIL automaticoversize16.359seconds,4a930d92:fixturewronglyassumed1checkpoint for4destinations; actualnativebuilderhas4checkpointoutputs and5signedtransactions.327recordsregistered; assertionbeforeinitialpathregistration andexpiry. Allfailedrawstate preserved. Automaticpayoutoracle not reached; no productverdict. Actualnativegraph source repair must keep10kvB/fanout4/depth100/600seconds and genuine327payees.

The user's primary invariant is the rightful user's real fallback for swap-received or refundable failed-swap value. The next primary normal acceptance case stops provider at POSTregistration Delivering, keeps recipientabsent, proves actual registeredrecipient payout atactualbackingexpiry withCore>=6confirmations, then freshseed-only discovery/confirmedrecoveryspend. A complementary accepted-swap cancellation duringproductdowntime tests actual failedsender99k refund. Backend pre-registration risk remains source-level/unconfirmed; its incorrect-payout reproduction is stopped and this track does not duplicate it.


## Primary normal registered receive fallback: actual PASS

Run ffa4d760-e441-4ab5-991e-251bdb9e42b9 passed in34.497seconds. A real10,000sat external invoice (actualnativeconfigured receivefee0) produced a fullysigned registered10,000sat recipientcoin atdurableDelivering. Originalrecipient was absentbeforeinvoice; provider stoppedforeverafterregistration andneverdelivered/reopened. The recipient's standardBIP84script received9,000sat with>=6Coreconfirmations andone1,000satpayoutfee. Realparentprevouts/outputsconservedthefullparent, includingoperatorresidual. Allcustomerallocations/canonicaloutputsacrossthewholebackingroot belongedonlytotherecipient, withnoprovider/ancestorrefund, recheckedpasttheoriginalHTLCdeadlineafterbothdaemonviews/financialledgerrebuilt.

ONLYafterthoseabsence/payoutoracles, a NEWdatabase/genesisbirthday usingonlyrecipientseed authenticatedtheexactreceipt anddiscoveredtheexact9,000satUTXO. ActualBIP84drainspentonlythatoutpoint, producedonechosen-scriptoutputandpositive independentlycomputedminerfee, andconfirmed>=6inCore with exactvalueconservation. This isdirectrightful-userfallbackproofforNORMALPOSTregisteredreceivedswapvalue. It givesnocredittoPREregistrationlostreply, failedsendrefund, stockNoah/LNURLorSignetfeeprofile. Noassertion/source/feepolicywasweakened andnoretryoccurred.

The testsourceSHA56b1553 is frozenin source-manifestf2961801 oncanonical3606fa78eplusownedtestpatch. NewimmutableELF2a749d53 andmetadatade04cd55 bindthisresult; fullhashes/commandsarein batch13receipt anddirect-receive-runtime-manifest.json. MandatorycanonicaljustchecksPASS; compilerleasewasreturnedbeforeexecution.


## Final paired v2: both rightful-user fallback cases pass

The unchanged recipient fallback case passed again in 36.184 seconds (`61d55416-31d6-4704-8f17-dbc8036178bd`). The existing genuine Accepted-then-Cancelled external-send case passed in 52.392 seconds (`e968e1cf-20d6-43a9-aad8-24f592bee113`): its rightful registered script received 99,000 sat with at least six Core confirmations, with parent conservation and one dispatch. This failed-send case does not prove sender seed recovery, a completed confirmed-ledger projection, full-lineage absence, a reorg or cancellation during product downtime.

Both runs independently verified all six archived ELFs, source/metadata/runtime manifests and actual CLN plugin hashes before execution. All four products and paired SDK/server binaries came from exact dirty source manifest `c60c5cd4c9d8cc0fbeaa8f8f6a71ba4102a46075cd2d9515fb505d9ba2dc882b`, labelled `6950be5a`. The changed Nix CLN derivation was not built; the exact separately pinned ad-hoc adapter was used. Backend units and other tracks' native runs are excluded from our counts.

The 15 invocations now account for 27 executions: 25 PASS, two FAIL, 24 unique names, 25 native executions and two mailbox components. Both original failed receipts remain unchanged. The 80 original scenarios are not all qualified. See FIFTH-CHECKPOINT.md and batch14/batch15 receipts for full pins, commands and limits. No additional Cargo or native runtime remains owned by this track. Production readiness is not established.
