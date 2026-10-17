# ML144 / ML160: focused native observation design

**SOURCE-ONLY DESIGN; NOT RUN.** No source, fixture, receipt, research intake, Cargo, runtime or canonical changes were made. Preserve tested fixture SHA256 `1d708bd6e4fa9bc80ff5b95d1acb52fc0269278dbb2c5b186107c6e023785366`, original batch12 failure, and successful batch16 run `63a761b8-7f82-4f73-9bd5-3433e6912403` unchanged. A future augmented candidate/run needs separate source/product/metadata pins; its results cannot be backfilled into batch16.

Source inspection HEAD was `544f28b50bd9db9d4bc11b324763d27da33901db`. Relevant server bytes still match research anchors: payout.rs `6c62a55d…`, settlement.rs `d09a4c2a…`, watchman/mod.rs `ebfd9323…`, expiry_decision.rs `ffd9db09…`, expiry_details.rs `292206c5…`, expiry_payout.rs `67b97408…`. This read-time source is not the batch16 tested candidate; batch16 remains dirty814+owner patch/source `704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06`.

## Exact gaps

| Scenario | Already verified in batch16 | Required additional observation |
|---|---|---|
| ML144 |327 rightful5,000-sat leaves each receive4,000 once; final per-coin fee1,000; full canonical DAG conservation. | All structural split stages, including inactive/historical ledger attempts and authenticated history, charge no separate customer fee or duplicate principal debit. A final correct output does not alone establish that history. |
| ML160 | All seven canonical parent/sponsor packages are independently enumerated; actual fees total88,821 sat and all inputs/remaining UTXOs conserve value. | Product accounting covers progress/split costs as well as final payout children; canonical costs are counted once rather than summing replacements. A real above-threshold proposed subsidy emits a correctly attributed warning, without preventing payment or changing customer amounts. |

These are extra oracles on the same327-owner/154-operation/635-signed-path/two-oversized-level experiment. Do not introduce another economic dataset, synthetic terminal/fee SQL, a benchmark,327 new running wallet daemons, or another nominal geometry scenario merely to increase row counts.

## Independent model, captured before expiry

Keep the existing327 owner indices, immutable source IDs, registered mailbox/account/script and5,000-sat gross; retain every signed native transition and map each physical backing output to its disjoint descendant owner set. Keep original source funding and the16 declared200k confirmed sponsor outpoints. No product row is the independent ownership/value model.

For each actual customer output, its owner set must sum to gross; the authorized customer deduction is exactly1,000 per real output, not per progress transaction or checkpoint. All original owners remain absent after preparation. The whole graph check must continue searching parent **and sponsor** outputs for extra owner payments; exact allocation-parent association and complete input/UTXO conservation remain mandatory.

For each canonical transaction, independently recompute `fee = sum(actual prevouts) - sum(actual outputs)`. Deduplicate by canonical txid/outpoint, never by broadcast-log count or latest database ID. Structural parents remain zero-fee version3 transactions; their P2A children pay miner costs. A saved/replaced/unconfirmed child has zero canonical expense until independently confirmed.

## ML144 observation points

Use the original physical milestones: before expiry; board exit confirmed; root fanout confirmed (three shallow owners may now legitimately pay); heavy checkpoint exit confirmed; heavy fanout confirmed; final payments Core>=6; one further completed dual-daemon/ledger rebuild. Classify work from the actual signed graph and canonical spends, not elapsed height alone. At each milestone collect a coherent read-only ledger snapshot for the **whole original anchor**, without filtering `active`:

- `expiry_entitlement`: permanent ID/source/gross/facts; `expiry_decided_coin`: permanent source fence.
- `expiry_allocation` and `expiry_allocation_input`: status/generation/input sources/immutable raw parent.
- Every `expiry_allocation_coin`, active and inactive: entitlement, disposition, mailbox/key/script, gross/fee/payout/forfeit and current paying link.
- Every `expiry_allocation_payout`: actual planned output association. Include allocations reached through input roots as well as coin entitlements; an input-only intermediate allocation must not disappear from the observation.
- Split requests and actual structural transactions are recorded separately from financial decisions.

Existing public `Db::read_snapshot`, ordinary bounded `tx.query`, the demonstrated `expiry_rebuild_token`/`expiry_view_is_rebuilt` checks, and actual Core RPC helpers suffice for these reads. Cast any custom SQL enum to TEXT; the monetary/disposition fields above are BIGINT/TEXT. Do not hold a database snapshot over network requests.

Assertions:

1. A purely structural split stage with no actual customer output cannot charge a customer fee or create another economic debit. If the implementation creates a split placeholder, inspect it rather than demanding a particular row count. Current code may create no placeholder; absence is acceptable, and must be verified across the whole anchor.
2. Superseded intermediate source/checkpoint IDs cannot acquire independent customer rights. Original terminal economic IDs remain stable, with one eventual actual covering payment each.
3. Every final paying output accounts for5,000=4,000+1,000 per owner here, zero forfeiture. No earlier structural fee is hidden in inactive history. Do **not** confuse a legitimate decided/unconfirmed payout's reserved fee with a second fee for structural progression.
4. Complete authenticated account receipts agree with the ledger and actual parent/output. Each account has one final authorized output fee; split-only stages never project another fee or successful payment. Repeating collection preserves entitlement and movement identity and does not add deductions.

Account authentication can reuse `XpubRecoveryAuthorization::new(account_xpriv, server_pubkey, expiry)` (`lib/src/recovery.rs:38`) and the existing `GetFallbackPayouts` account query (`server-rpc/protos/bark_server.proto:790`; `server/src/rpcserver/ark.rs:325`). Follow its view-token/cursor contract; a changing collection must restart or remain incomplete, not become a positive assertion. Server collection already returns historical attempts, not just active ones (`expiry_details.rs:99–143`).

**Required helper change for a future candidate:** the current fixture's `owner()` discards its account Xpriv after making the record. A read-only observer cannot manufacture account authorization from the public xpub or child key. Retain each account Xpriv privately in the future in-memory owner model solely for read-only history authorization; never print or serialize it. The original tested artifact cannot acquire this capability retroactively.

For a real client `history()` assertion, retain one genuine SDK-generated owner/seed in the same327-owner cohort and keep its daemon stopped; record its baseline before absence, then reopen a separate observer database only after canonical payment. The current random owner account/external destination is not automatically an Ark wallet seed. Existing integration code cannot call private `receipt`/`FallbackSnapshot::project` as though they were public APIs. Without that authentic SDK-owner helper, claim server authenticated-history coverage only and keep the client projection portion explicitly open. This is one passive observer, not327 application wallets; every owner still receives a full server-receipt and independent chain audit.

## ML160 accounting and warning

Read current product behavior accurately:

- Final payout child construction computes actual input/output fee and `payout_fee_accounting` (`watchman/payout.rs:108–131`). Its proposed warning includes allocation_id, proposed_subsidy_sat, miner_fee_sat, operator_remainder_sat, net_operator_result_sat and warning_sat. It occurs **before** child storage/wallet persistence/broadcast, so it is not a canonical expense receipt.
- `payout_fee_accounting` uses all value after the committed customer prefix as operator remainder, including a nonzero P2A; subsidy is `max(fee - remainder,0)` (`settlement.rs:46–59`). Count that remainder only once in any wider family audit.
- Final child versions are stored in `expiry_fee_child`, including fee_sat/operator_remainder_sat (`expiry_payout.rs:154–172`); canonical confirmation chooses actual expense.
- Structural progress uses `process_progress_tx`: real CPFP then `ProgressBroadcast { vtxo_id,txid,cpfp_txid }` (`watchman/mod.rs:791–815`; `server-log/src/msgs/watchman.rs:142`). That path currently has no expiry_fee_child insertion, stable split-family cost association or analogous subsidy warning. Therefore full product-level split accounting cannot be asserted by inventing such a row/event in the test.

Before any physical progression, install `watchmand.subscribe_log::<server_log::ProgressBroadcast>()` and a generic `add_slog_handler` for the WARN record. `ParsedRecord.extra` carries the ordinary tracing warning fields; `subscribe_log<T>` alone cannot capture this untyped warning as a nonexistent structured message. The daemon's existing `stdout.log` is the final bounded capture of all complete records (`testing/src/daemon/mod.rs:162–171,304`). Subscribe early, and reconcile the complete file with the channel so initialization/flush timing cannot create a false missing-event result. Preserve only public txids/amounts/IDs in the report.

For every progress event, verify exact known signed parent, real Core admission/canonical block and actual P2A spender; match the fee to its real prevouts. Record final allocation child attempts similarly from the ledger and Core. Produce a union of all canonical split/progress/final child txids; each expense appears exactly once. Retain rejected/replaced attempts as observations with zero canonical expense. All warning fields must match an actually constructed unique proposal in this bounded run; a warning alone cannot establish payment.

**The old successful run cannot provide a nonvacuous warning assertion.** Its four structural fees are1,934+2,837+1,934+2,837=9,542 sat. The three final groups have3/81/243 owners; their actual fees are3,779/19,641/55,859 versus customer-fee operator remainders3,000/81,000/243,000. The positive final subsidy is only779 for the shallow group, below the unchanged default10,000 warning (`watchman/config.rs:35–41`). Total88,821 alone is not subsidy: subtract real operator remainder in the correct nonduplicated accounting domain. No warning was required merely because aggregate miner fees exceeded10,000.

A bounded warning trigger may reuse the same dataset and sponsor pool: after a genuine final payout parent/first child reaches the mempool, before confirming it, use the **existing native fee-replacement mechanism** from `expiry_reorg.rs:169–240` to change the owned watchman's miner-fee target and restart it, preserving the exact parent. For the shallow3-owner parent, a declared100sat/vB target should make real positive subsidy exceed10,000; this is only a planning estimate, and the test must compute the actual result and fail the setup if it did not cross. Keep the customer fee and warning threshold unchanged; never lower the threshold or replace the fee/subsidy arithmetic with assumed constants. There are already multiple genuinely confirmed sponsor outputs for native RBF. Require actual replacement admission, exact parent identity, a matching above-threshold proposed warning, actual canonical replacement fee, zero expense for the superseded child, and the same327 owner outcomes. This phase is a declared controlled operator-fee regime, not production fee-market qualification or a relaxation of Core/TRUC limits. It needs a separate coordinator allocation/source review; it is not part of the retained164.064s result.

That trigger proves the **existing final-payout warning path only**. It does not close ML160's missing cumulative progress/split accounting. Full ML160 requires a product-observable stable cost family spanning an oversized backing, its progress steps and eventual allocations, with each real fee/remainder attributed once. Shared ancestor costs cannot be charged in full to each child allocation, nor can the test silently choose a new cost-sharing policy. The C7/P2 owner rule promises coverage of sweep, every split level and every replacement; root must resolve the stable accounting association and product instrumentation before this assertion can pass. A test-only fabricated event/table would hide the gap. Until then record independent complete DAG cost PASS separately and product accounting/warning completeness NOT RUN or a reproducible missing-evidence failure, never FULL.

## Implementation and run boundary

Do not duplicate the635-path construction or replace the tested fixture. Its helpers and owners are private and offer no observer callbacks; authenticated-account/client history needs the retained material noted above. A future separately assigned reviewed augmentation must expose a small shared observer hook/fixture-result model while preserving every original graph/money assertion and the archived1d708 source. This design supplies requirements, not permission to refactor the frozen file now.

New read-only helpers should be limited to whole-anchor ledger snapshot, complete account-page collection, public warning/progress collection, and canonical fee-family reconciliation. Reuse `native_node`, existing Core transaction/block helpers, original package checks and ordinary logger/DB APIs when an authorized versioned helper becomes available. Do not introduce a new daemon framework, a new financial fixture loader, a second 327-owner population or policy overrides to force a PASS.

Retain the original preparation workload, production size/fanout/depth constraints, funding budget and explicit600-second inner experiment budget. Additional observers/RBF consume that same budget unless a separately reviewed experiment declares otherwise; no measured runtime estimate is assigned here. Use repo `just int-binaries`, one exact selected ignored case,1 thread/0 retries, private state/store and an explicitly allocated scale-profile runner whose termination covers the existing600-second inner budget. Stop and preserve the first genuine setup/oracle failure; no repeat, source correction or deadline extension can silently rewrite it.

Expected output is one new bounded experiment receipt with separate fields for ML144 ledger/history, ML160 independent canonical cost, final warning trigger, and cumulative product split-cost completeness. All327 payee outpoints and the full conservation proof remain mandatory. Neither catalogue row changes from PARTIAL/NOT RUN merely because this design exists.
