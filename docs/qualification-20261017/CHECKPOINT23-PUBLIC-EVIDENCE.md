# Regtest qualification checkpoint 23

Two new native fixtures passed on captured V17: grouped tiny payouts in 23.972 seconds and restored offboard-reply recovery in 44.444 seconds. Each used the default 180-second limit, zero retries and one selected server test. The other 300 tests were filtered in each invocation and receive no execution credit. Parent execution session 89868 exited with code 0; combined native time was 68.416 seconds and controller time was 70.621 seconds.

Coverage now records 255 executions: 240 PASS and 15 preserved failed executions, across 235 unique binary/test pairs. There are 243 native and 12 component executions, 228 nextest invocations and 466 mixed coverage rows. The previous 464 CSV rows and all historical failures remain byte-identical. These counts exclude coordinator tests and do not count whole legacy money-loss cases automatically.

The final consistency check found a stale PASS counter in the initial curated STATUS: its top-level total and coverage were correct, but `execution_outcomes.PASS` still read 238. The original snapshot and curation receipt are preserved. `batch50-STATUS-outcome-correction-parent.json` records the correction to 240; the final STATUS satisfies 240 + 15 = 255. No native outcome or failed oracle was changed.

The runtime is bound to source manifest `5d50a0f4dfd7a2fd61257363e5b29b013b6cdf48fdbb048f527e9f93f3fd178b`. Its products carry inherited Git label `d2fae7db778096ff940e3861ca575d0b7bcddb56`; the captured source contains additional changes and is not represented as a clean build of that commit. Independent postflight verified all 14 runtime pins, 16 frozen parent-admission paths, exact source/module/classifier bindings and absence of the recorded groups and scoped processes. These results do not qualify later V18 changes. The coordinator's separate reserved-partial timeout remains preserved and outside this track's execution totals.

## Grouped tiny payouts

Three genuine 500-sat leaves belonging to the same registered customer produced one 500-sat payout with one 1,000-sat output charge. The per-leaf fee shares are 500, 500 and 0 sats; their payout shares are 0, 0 and 500 sats against the same actual output. The strict SDK receipt and history checks accepted every contributor, including those with zero individual payout share.

The independent public-block audit found the exact 500-sat recipient and 97,500-sat sender outputs and their same-block fee child, with seven confirmations. The selected 300,000-sat root/sponsor cut reconciles as 297,502 sats of terminal outputs plus 2,498 sats of actual miner fees. Customer principal separately reconciles as 100,000 = 98,000 paid + 2,000 output charges. Ordinary board change and the upfront board miner fee remain outside that selected cut. Genuine whole-group dust, forged receipts, the complete script boundary matrix and global custody remain separate requirements.

## Restored offboard reply

The native fixture reached one economic offboard despite two finish calls: the original signed reply was lost after commitment, and an original client checkpoint restored into a disjoint directory adopted that same discharge after a real expired-session response. The positive watchman control also completed. This is a clean client checkpoint on the same native server/Core, not a seed-only restore, server crash backup, PITR or universal recovery proof.

The independent audit verified the original signed transaction and its serialized-byte hash with 36 confirmations. It spends one operator-funded 500,000-sat input:

`500,000 = 99,146 customer payout + 330 connector + 399,226 operator change + 1,298 actual miner fee`.

The distinct customer liability equation is:

`200,000 = 99,237 ordinary change + 99,146 offboard net + 854 committed user fee + 763 board miner fee`.

The 854-sat user charge must not be replaced by the 1,298-sat physical miner fee, and the operator input is not a customer deposit. The genuine 20,000-sat positive control paid 19,000 sats with a 1,000-sat output charge; its actual same-block child fee was 2,281 sats, with 12 confirmations. The two 100,000-sat prefunding outputs were found.

The original source-ID-to-anchor association, original fallback-script negative, private checkpoint closure/identity, RPC-call counts and SDK history remain native-assertion evidence where the public capsule omits their bindings. The audit used public receipts and retained block/XOR bytes, with stable hashes before and after. It did not inspect private wallets or databases, restart nodes, use RPC, decode an independent mempool, establish custody, or independently read Core's best-state database.

## Public artifacts and next source fixture

`batch50-control/` contains the bounded control export; `financial/` contains the two exact public receipts, reconciliation, scoped parser, pure decoder helper and provenance. Original initial receipts and exports retain their pending-at-creation fields. Separate formal, financial, export and curation receipts resolve them additively. Raw logs, blocks, test data, wallets, databases, configurations, seeds and keys are excluded from publication.

The new H32 source is `qualification_dust_later_receipt::forfeited_small_receipt_does_not_revive_when_same_customer_receives_again`, committed as `f36f377be24aefa78e36ad84d120356fd395379b`, source SHA-256 `67aa2a84cb7f3aa5192fea5b12c8c4738796097f869f40ed16489e586a45f524`. It requires genuine first-epoch dust forfeiture and accepted history in the original wallet before an independently funded later receipt to that same account and script. Whole-chain payout checks, real fee-child graphs and both original-wallet history epochs remain mandatory. V1 findings and the corrected V2 packet are preserved.

H32 is source reviewed, unregistered, uncompiled and NOT RUN. It is outside the captured V17/V18 evidence and adds no execution or whole-case credit. The separate H11 forwarding fixture is being authored and is excluded from this checkpoint's source payload. Production readiness, complete legacy coverage, sustained scale and remaining Lightning recovery gates remain open.
