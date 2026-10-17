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


## Corrected real-size multilevel native split: actual PASS on source70417933

The corrected fixture passed in164.064s, run63a761b8-7f82-4f73-9bd5-3433e6912403, preserving the600s budget and real size/fanout/depth limits. It completed327registeredpayees/154nativecosign+registrationpairs/327mailboxposts and635signedpaths. Both actual327/324recipient oversized backing instructions progressed automatically. All327exact registeredmailbox/key/script owners received one4,000sat actual Coreconfirmed output (minimum7confirmations), with5,000gross/1,000outputfee/no forfeiture. Full canonical graph checked7parent/sponsorpackages/14transactions and conserved2,835,000externalinput =2,746,179remainingoutputs +88,821actualminerfees. These inputs count only consumed declared backing/sponsors, not all reserved wallet funding.

Original batch12FAIL16.359s remains unchanged. This passing rerun is the same test name on separately frozen dirty814+owner-scan source70417933, not a new unique scenario or a retroactive pass. All source/HEAD/diff and nine product/test ELFs were verified across capture and release. RequiredchecksPASS; fullunits800PASS1explicitignored, with the exact ignored isolate_dust_fuzz later run separately PASS15.454s. That supplemental unit adds no financialcase credit.

Five-binary immutable inventory contains643names/638default-selected/5ignored. Inventory and unit results supply no full integration-suite qualification. Actual controlledregtest/harness cadence and concurrent extra metadata compilation bound the duration claim; production scale, default adapter packaging, reorg/restart/restore, Noah/LNURL and Signet remain separate. No compiler or owned native runtime remains, and the coordinator owns the next PG/race/unchanged120sscale gate. See SIXTH-CHECKPOINT.md and exact receipts.


## Upstream offline-address fixture: preserved failure on source7041

The serial ordinary bark batch stopped at case27: `create::bark_address_works_offline` failed in10.652 seconds, run `46a2b233-8a96-4b12-8ca2-df654fc0cd26`. The first26 cases passed;48 never started. This was an assertion failure, with ample shared memory and no retry. All original logs, result and wallet state remain preserved.

This is one wallet, not a seed restore. Its first fresh address differs from its explicit index0 address after the server stops. The explicit offline address command itself succeeded; the later fresh-offline assertion was never reached. The accepted client plan K2/WC1 reserves key0, and frozen source7041 persists that identity before generating the first fresh key1. The upstream test assumes fresh generation starts at0. A minimal adapted fixture will compare the same explicit0 address online/offline and the same actual fresh1 address online/offline, preserving the genuinely new offline-address assertion. Source/peer review and a separately allocated compile/run are required; the original FAIL is not converted to PASS.

Read-only SQLite inspection found zero VTXOs, pending boards, Lightning sends/receives, movements, wallet action checkpoints, BDK transactions and transaction outputs. Two native key records exist, consistent with reserved0 plus generated1. There were no SQLite sidecars; the DB SHA256 was unchanged. The fixture performs only wallet creation and address operations. No customer money operation was retried or newly invoked during investigation. This evidence establishes a fixture index-assumption mismatch; it does not demonstrate offline RPC failure or a financial loss.

Completed main-track totals now118executions:115PASS/3FAIL,114unique(binary,name),91nextest invocations. Of these109 are daemon-backed native cases (including10 infrastructure cases), and9 are components. The current batch's26PASS comprise25 daemon-backed cases and one library component; its failed case is daemon-backed. These are regression counts, not full ML acceptance. The two original failures remain preserved, and the200-scenario full-oracle ledger remains198NOTRUN/2 boundedPASS.


## First-time forced offline creation: product compatibility conflict

`create::bark_create_force_flag` failed in15.259seconds on source7041, run `c5640527-12f8-45df-8dff-47798cc91637`. Batch22 stopped at its first case;46 additional cases never started, and originalcase61 remains separately deferred. This test creates its first wallet after stopping Ark; it does not recreate an existing wallet. No retry occurred.

The public SDK `OpenWalletArgs::create_without_server` still promises creation while Ark is unreachable, and CLI passes its force flag through. Initial wallet creation honors this, but `Wallet::open` then unconditionally opens fallback registration with a requested script, and first-time `open_fallback` requires a live Ark connection. The actual CLI error is ConnectionRefused. Five relevant files remain byte-identical in canonical9ea05f and the tested7041 capture, so the observed conflict is still source-supported on that newer source; no new native result is claimed there.

K2 automatic registration does not explicitly retire forced offline creation. Independent reviews reject treating this as an automatically obsolete test or weakening its success oracle. The coordinator assigned a narrow isolated product fix: preserve explicit offline creation as an unregistered state, persist the local choice, defer real registration only during genuine unavailability, and keep key issuance/money operations fenced. Broadly ignoring authentication, pinning or database errors would be invalid. Original fixture remains mandatory and unchanged for a later separately admitted run.

The new CLI directory is removed on failure. There is no retained wallet database to count; no durable half-created-wallet claim follows from an intermediate Created-wallet log. The raw BDK sync reported all balance categories zero, and the source contains no funding/board/payment operation. Read-only reconciliation created no wallet/node process and no retry. Full raw failure and source reviews are preserved privately; curated receipt publication excludes raw config and secret-bearing logs.

## Subsequent upstream fee, dust and recovery fixtures

The original failures through batch33 remain FAIL. Read-only reconciliation separates their actual native money outcomes from fixed historical fee assumptions:

- Ordinary offboards to the actual 22-byte BIP84 script charge 854 sat under the recorded quote; several upstream fixtures expect 938 sat for a 34-byte script. The 84-sat difference affects their change, debit or fee assertions. Actual physical miner fees are separate from this customer charge.
- Board-all funding produces 99,392 sat before the configured Ark charge, rather than the fixed 99,443 sat assumed by two recovery fixtures. Original and recovered wallets refer to the same coins. Their final fixed-amount sends fail by 51 sat; adding the two wallet views would double-count principal.
- The dust fixture requests 329 sat using an old P2TR threshold. Its actual P2WPKH destination has a 294-sat threshold, so accepting that request does not establish acceptance of dust for that script.
- `onchain::onchain_drain` fails its fixed 999,443-sat recipient expectation. Actual signed Core transaction `46cf13cf3f58ed1d6bd869606cbd022f3cde6860954d014333c8c7b7a32d88aa` spends exactly 1,000,000 sat into one 999,452-sat recipient output and a 548-sat miner fee. The native sender-zero assertion passed. Retained canonical block ancestry proves one confirmation; no six-confirmation claim follows. The independent receipt is `onchain-drain-retained-reconciliation.json`.

The drain adaptation bounds the fee using the actual chain quote and descriptor satisfaction weight, then checks the complete input cut, exact canonical destination output, sender zero and recipient conservation. It does not accept an arbitrary loss by defining the fee as whatever remains. Its source and the other fee/dust/recovery adaptations remain uncompiled and NOT RUN on their proposed candidate. Earlier original failures and assertions that were never reached retain their recorded status.

These fixture findings do not close the separate production money gates. The coordinator's read-only `fresh-execution-20261016/PRODUCTION_GATES.json` records unresolved autonomous signed-grant refunds, conflict/replan recovery, final-candidate qualification, attribution/scale and operational evidence. The reviewed ordinary physical-backup fixture has no runtime PASS and does not cover Lightning backup. The older frozen conflict draft needs stronger canonical fee-child conservation before admission. An earlier source review incorrectly claimed its synchronized mining necessarily hangs on a stopped server; both current and frozen7041 helpers explicitly skip stopped services. Direct continuing-Core mining makes that interval explicit but is not a proven deadlock fix. The additive erratum is `stopped-captaind-wait-review-correction.json`; original packets remain preserved. No production-readiness claim follows from the regression totals.

## Ordinary physical backup and seed-only recovery: actual PASS on new v5 candidate

Run `874f3d92-bea4-4599-955e-be88b2b4c2fd` passed once in 28.352 seconds with the unchanged 180-second bound and zero retries. This supersedes the earlier NOT RUN status for the ordinary cold-backup fixture only. Source manifest `87e82448bbad37bc7435de89f92c20192713725c7bca61f53ab66c148218400f` captures published HEAD `e443d415ee7b0d512203bd56f85e22cdec69d35f` plus declared reviewed changes. Runtime manifest `740da075ad58542352fea1b2ce6c93e3827cf02381d9b88f2c99c76f8e4c25b6`, native server ELF `26d701e21e623889f9cba0e0af76d6084e032f9aa8b96dfa9a098e8a471f7387`, and exact fixture body `ced8c436e9106e68bcd916a640aee6ef1007fb7584d2c09f0114b71902009374` identify this new candidate separately from the upstream regression bundle. The cached embedded version label reports `9ea05f`; the coordinator confirmed that label is not the source identity.

The native fixture stopped the original authorities, required PostgreSQL's clean shutdown control state, copied and restored 1,605 physical cluster files plus service identities, and retained 111 actual watchman wallet changesets. Fresh service paths resumed against the same continuing Core. The original customer stayed absent while the restored services paid the registered 99,000-sat destination. A genuinely fresh seed-only BIP84 wallet recovered that payout and the original 99,237-sat ordinary change, then spent both inputs into one chosen destination.

Independent offline retained-Core reconstruction confirms the actual parent and same-block fee child at height 139 with 12 confirmations, and the recovery spend at height 145 with six confirmations at tip 150. The full declared physical cut is `300,000 = 197,349 customer recovery + 98,719 retained operator outputs + 763 board miner fee + 2,281 package miner fee + 888 recovery miner fee`. The 1,000-sat customer payout fee remains distinct from the physical miner fees. Actor-absence checks passed and inspected retained hashes were unchanged.

See `batch38-cold-backup-v5-receipt.json`, `batch38-cold-backup-public-receipt.json` and the separate `cold-backup-retained-core-reconciliation-client.json`. Independent offline checks cover actual chain bytes, money and confirmation depth; physical-copy, clean-control, identity and BDK custody assertions retain their native test provenance. No private backup, seed, PostgreSQL dataset or full native log was read or published by that independent reconciliation. The case does not establish Lightning backup, unclean shutdown recovery, cross-version migration, Core backup, arbitrary disaster recovery, global solvency or production readiness. Root gate P07 remains incomplete because material Lightning state boundaries remain unqualified.

## Four v6 controls passed; qualification remains bounded

Batch39 ran four allocated cases once on source manifest `779189df58ccf6e1225e5e89ea7f74ba62134974b8c232c78be596049ddea79a`, bundle `e66ae1dd887e7b46a33c2620544562cab1f91b8a8b6736ecec3a3e44836bbf82`. All passed with zero retries and no resource errors. Independent postflight verified four exact singleton bodies, all 16 unchanged runtime pins and no remaining owned batch processes. The actual version labels identify captured HEAD `828863b21a21a10d3d26ae5f29479be3ee87a233`; the declared dirty archive, rather than the label alone, identifies the tested source.

The evidence supports one confirmed 99,000-sat zero-grant refund, normal internal/external receive binding and idempotence, and callerless materialization of the original 50,000-sat receive request after a real interrupted write. The zero-grant case does not check the full fee-child/operator cut; normal external binding does not assert an exact 50,000-sat net balance; callerless restart does not reproduce the complete historical ML031 durable-preimage condition or demonstrate fallback/seed recovery. These are partial requirement proofs. No whole money-loss row or release gate is promoted. See `batch39-owner-replan-v6-receipt.json`, `batch39-client-postflight.json` and `batch39-native-oracle-review.json`.

This track's completed totals are 220 executions, 206 PASS and 14 preserved FAIL, 216 unique binary/test pairs, 193 nextest invocations, 208 native cases and 12 components. COVERAGE has 431 mixed inventory, requirement and execution rows. These counts exclude other tracks and the recovered historical Noah callback attempt; they are not a full-suite or production-capacity result.

## Stock Noah: recovered strict-campaign rejection before payment

Read-only review recovered the original immutable simulator receipt for one stock-Noah8e68 callback at 2026-10-17 16:11:48 UTC. HTTP returned 200 for a 5,000-sat request, but the admitted strict verifier rejected the missing metadata description hash before any payment. Public receipt copy SHA256 is `dcc4410247d48589b0ba1bd6f7f13c0725d649f663fccaad25b5373110544ab6`. Stock Noah advertises the complete metadata JSON but passes a human memo through a memo-only provider API; the reviewed behavior is inherited from upstream. This establishes a strict-campaign compatibility failure, not a failed funded payment or loss-of-principal result.

Current LUD-06 no longer mandates metadata-h verification, and BOLT 11 permits a description field instead. Missing h alone therefore does not establish a generally invalid invoice. The accepted campaign separately requires strict binding, and the coordinator retained it; the original FAIL and verifier are unchanged. A compatible strict integration would need the exact metadata digest carried from the caller through to the issuer's distinct Hash variant. Adding provider capability alone cannot change stock Noah's memo-only caller. No fork or integration change was made. Full provenance, primary protocol links, remaining funded lifecycle gates and all three reviews are in `NOAH-READONLY-AUDIT.md`.

## Source queue correction: three existing adaptations already captured

The earlier source-ownership snapshot's pending-compile labels are historical. Exact target bodies for `create::bark_address_works_offline`, `onchain::bark_address_changes` and `fees::send_onchain_fee_deducted` are already present in published828863 and the captured v6 native CLI source. The attachment `source-intake-three-existing-v6-match.json` binds their bytes and existing executable metadata. They have no new native execution result in this checkpoint. Twelve other ordinary target overlays remain source-only, as do the separately identified pending money-path packets. The coordinator has not allocated another runtime batch; original failures and held cases remain preserved.

## Three existing v6 adaptations: later allocated native PASS

After the fixed PR16 cutoff, the coordinator allocated exactly those three captured CLI adaptations once. Batch40 completed successfully with native times 9.169, 6.738 and 18.798 seconds, respectively, under the unchanged 180-second bound, one thread and zero retries. Parent session85000 terminated with exit0. Source779/bundlee66/runtime metadata8a60 are the same immutable v6 pairing; fresh source/archive, exact-selection, 16-pin and resource gates passed. Independent postflight found all captured pins and frozen controls unchanged and no remaining owned actors.

These are later results on deliberately adapted bodies; the original source7041 failures and held original address fixture remain preserved. The two address tests check Ark receive-key identity reservation at index0, explicit index retrieval and fresh indices1/2, including offline behavior. Despite its historical `onchain::bark_address_changes` name, that fixture does not exercise Bitcoin address rotation. The fee test validates the configured public quote for the actual payout script, its customer fee, an 800,000-sat scoped input/change equation, and the fresh recipient CLI's 300,000-sat total and estimate agreement. It does not resolve canonical Core prevouts/miner fees, require six confirmations or prove the full initial funding/operator/fee-child cut. CLI total balance may include unconfirmed value.

See `batch40-native-oracle-review.json`, `batch40-client-postflight.json` and the batch40 terminal receipt. These three executions add one unique binary/test pair; the other two names appeared in earlier failed executions. The completed main-track cutoff becomes 223 executions, 209 PASS and 14 preserved FAIL, 217 unique pairs, 196 nextest invocations, 211 native cases, 12 components and 434 mixed coverage rows. No full money-loss or production gate is promoted.

The remaining twelve ordinary adaptations are now packaged as one source-only five-file patch with a strict published828863 base. It excludes the three already-integrated targets. Independent in-memory application reproduces the frozen reviewed result bytes without changing unrelated test functions; no new test code or canonical edit was made by packaging. Seven targets use the CLI test binary and five use the separate `ark-testing::barkd` test binary, which is absent from the current v6 metadata. Deliberate coordinator integration, compilation and a matching new capture are required before any of those twelve gains execution credit. The manifest is `ordinary-adaptations-twelve-source-only-manifest.json`; the patch remains outside public report payloads.

## Live funded attribution under 100 and 1,000 registrations: bounded v7 PASS

Batch41 ran the two explicitly allocated ignored owner-attribution envelopes once. The 100-registration case passed in 47.490 seconds and the 1,000-registration case in 80.781 seconds, with parent session1420 ending in exit0. They used the existing `fallback-scale` profile's 1,740-second outer limit, exact `--run-ignored only` selection, one thread and zero retries. The source's separate 120-second timeout around each post-expiry, post-daemon-sync publication poll was unchanged. These are not default-180-second executions, despite both observed durations being shorter than 180 seconds.

The pairing is new: source manifest `d05d3d5c7a24584fc9893f6601d9c0b243784f9bb16602feca699b4e532c3db0`, bundle `04b05a11e01b3b0af46e812cf3cb94d053d2a3bd79d4c7b25ac234948fa3eb33`, runtime metadata `748a2f9fc21d008ad3a7bdc166363c04b524461b3339d084c2b9481a37a08b1d`. The captured dirty source is based on published759f12c1f; actual labels and immutable archive are recorded separately. Independent source checks bind the exact reviewed patch's add-file bytes to the captured body, including both ignored wrappers and unchanged profile. Client postflight verifies all 13 pins, exact singleton execution, unchanged frozen controls and absent owned actors.

| Initial authenticated registrations | Child80 publication poll | Later child1 publication poll | Pressure RPCs by leg | Observed final registry size |
|---:|---:|---:|---:|---:|
| 100 | 6.289 seconds | 3.628 seconds | 98 / 57 | 116 |
| 1,000 | 24.683 seconds | 7.674 seconds | 394 / 122 | 1,016 |

Each cohort has two funded customer accounts and three genuine board roots. Most registry records are authenticated but unfunded. Pressure cycles at most eight new records per leg, with repeated normal registrations; this is not sustained unlimited arrivals. The actual server scanner/readiness tasks run without a test-only scan/queue invocation while only the customer's recovery-mailbox post is unavailable. The independent control wallet is absent; the main wallet is held idle with manual synchronization during the first leg, returns for a genuine later child1 board, and is dropped before the second. Fresh seed/database recovery uses the live Ark endpoint. These details limit default-client-daemon, total-outage, scheduling-fairness and throughput claims.

Independent offline Core reconstruction checks the last recorded canonical branch at tip177, header ancestry, transaction Merkle roots, actual prevouts, confirmation depths and terminal unspentness. Each finite graph contains ten transactions and nine unspent leaves. Both owner payouts are spent exactly once by the six-confirmation seed-recovery transaction; the separate 99,000-sat control payout remains unspent. Each actual declared boundary reconciles:

`1,000,000 = 495,983 seed recovery + 99,000 control payout + 395,394 other observed unspent leaves + 9,623 graph miner fees`.

The boundary is 500,000 sat of owner funding, 200,000 sat of actual control funding and three 100,000-sat sponsor outputs. Graph miner fees are 2,289 for boards, 6,843 for actual canonical fee children and 491 for recovery. A later fee child legitimately spends confirmed change from an earlier child, leaving one original sponsor output unused; the graph counts internal edges once and retains that unused output. Initial auditor assumptions that every fee input was an original 100k sponsor and that control funding was 100k were rejected by actual retained transactions. The correction supersedes nominal source-admission accounting; neither exploratory parser assertion is a native FAIL or retry.

Raw blocks/xor, final-tip log, safe proofs and result hashes remain unchanged. The audit starts no Core process and uses no RPC, SQL, wallet, private seed or native full log. It establishes the bounded chain/value evidence; scripts alone do not prove custody of other leaves, global operator solvency or all recipient-identity assertions. Those remain native source/assertion facts or explicit gaps. The initial receipt's pending-audit fields remain immutable; client and financial supplements link the completed reports. See `batch41-live-funded-v7-receipt.json`, `batch41-client-postflight-supplement.json`, `batch41-financial-supplement.json` and `batch41-retained-core-reconciliation.json`.

The completed main-track cutoff becomes 225 executions, 211 PASS and 14 preserved FAIL, 219 unique pairs, 198 nextest invocations, 213 native cases, 12 components and 436 mixed coverage rows. The original 10,000-registration wrong-mailbox deadline failure remains unchanged and is a different workload. These two positive cohorts prove neither 100/1,000 funded-user throughput nor maximum scale, exhaustive negative/high-index search, sustained fairness, full ML coverage or production readiness. No original ML row or release gate is automatically promoted.
