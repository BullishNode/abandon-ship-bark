# Isolated native task fixture

Run from the bark checkout with explicit `PROTOC` and installed Core/Docker/Postgres tooling. Build `cargo build --locked -p bark-server --bin captaind --bin watchmand`, then:

```sh
docker compose -f contrib/expiry-payout-regtest/compose.yaml up -d
python3 contrib/expiry-payout-regtest/bootstrap.py
python3 contrib/expiry-payout-regtest/happy.py
```

The scripts currently use the local qualification paths and ports recorded in `common.py`. They create only the `abandon-captaind-task` project and generated test wallets; never remove existing volumes. Core53443, Postgres50432, captaind48535/48536, watchmand48538. Daemons use the binaries built in this checkout. Bootstrap funds rounds/watchman once and primes real fee estimates. User boards explicitly receive fresh test funds. This is not a capital-conservation campaign.

Evidence goes to `EXPIRY_EVIDENCE` (default local expiry-task-evidence/runtime), with `EXPIRY_CASE` selecting the case directory. Keep seeds/private runtime state out of Git. `happy.py` checks no payment before expiry, autonomous payment after sweep, the exact tr(key) output and mining fee, stable identity across repeated ticks and nursery confirmation. It does not exercise a new Ark round, crash, restore, partial tree or browser; those need separate cases.

Initial pass:20261008T004212-493a1415. Added explicit destination assertion was also run separately on that actual transaction,20261008T004445-6f36be7e. WAL archive permission was corrected during initial bootstrap and made explicit in the script. Verify real WAL recovery separately; an archive count is not a restore drill.

`EXPIRY_CASE=fee-estimate python3 contrib/expiry-payout-regtest/fee-estimate.py` restarts only this fixture's captaind behind a local RPC fault proxy. Core still has a real estimate; the proxy withholds it from captaind. An expired, swept coin remains unpaid and spendable across ticks. Restoring direct Core access must pay it and publish an exact fee receipt. The original captaind config is restored in `finally`. Initial runtime PASS20261008T004757-9026073d. This test changes only the private D fixture, never signet.


Recovery and path scenarios (local regtest, debug binary `05930d221bd5af4b753646e4e7d9967517432bed804176131172af7eb9c30266`):

| Scenario | Observed result | Evidence run |
| --- | --- | --- |
| `crash.py` | PASS: SIGKILL after atomic coin/receipt/nursery commit, before wallet graph persistence and broadcast; disabled task startup broadcasts identical bytes | `20261008T005738-27d7b257` |
| `precommit-crash.py` | PASS: SIGKILL before atomic commit rolls back; entitlement remains spendable and later pays once | `20261008T010054-490bea2a` |
| `pitr.py` | PASS: physical base backup before payout plus subsequently archived WAL; recovered cluster retains spent state, raw transaction and nursery; same payment confirms | `20261008T010131-13fdaa1e` |
| `partial.py` | PASS: twelve users in one round, an exited leaf, and separate live sibling outputs; only an exact swept path pays; delayed branch pays after its own sweep | `20261008T010220-d45ba860` |

Run each with a distinct `EXPIRY_CASE`. SQL triggers/advisory locks hold real commit boundaries; they do not fabricate a payout. The partial test temporarily delays one watchman frontier entry, checks actual Core outpoints, then restores that entry. The PITR clone has a new retained volume/port50433; original volumes remain intact. WAL replay must include the payment commit: this is not support for arbitrary stale snapshots.

Failed attempts remain recorded. `20261008T005430-fc0f76db` failed before the crash boundary because the fixture omitted Docker interactive stdin for its SQL lock. `20261008T005957-97fa6872` failed at setup because a foreground command teardown had reaped a daemon. The corrected fixture uses detached daemon sessions. Fee priming now checks all three estimator targets, matching captaind's real-rate availability gate. These four passes are individual cases, not a complete release-image suite.


The fixture now takes an exclusive OS lock for each scenario and waits for the previous daemon process to exit fully before a restart. Two earlier overlapping cases (`20261008T010715-d6b1f009`, `20261008T010758-399699b7`) were interrupted and invalidated. Restarting one daemon from the consistent durable checkpoints recovered the fixture without database edits; all19existing payment receipts remained correct. The exclusive lock prevents repeating that setup error.

`startup.py` checks rejection of a foreign sweep address, missing watchman configuration and nonloopback admin bind, then checks disabled startup and restores enabled operation. PASS `20261008T014645-cd986e70`. The startup-check build passed163serverunit tests. An earlier driver stopped before the scenario because the admin port was still in use; that attempt is not a scenario pass. Bootstrap starts disabled until the actual rounds-wallet sweep address and watchmand configuration exist.

Additional individual flow evidence:

| Scenario | Observed result | Evidence run |
| --- | --- | --- |
| `eligibility.py` | PASS: a live coin is excluded; an expired owner's successful offboard excludes its entitlement from later payouts | `20261008T010506-8d7a1004` |
| `fees.py` | PASS: an older coin exceeding the actual fee cap stays spendable, a later affordable coin pays, and restoring the cap releases the waiting coin | `20261008T010626-e4538152` |
| `shared.py` | PASS: three real Ark transfers sharing a key get one output with exact aggregate principal and fee | `20261008T011658-a4b93e37` |
| `race.py` | PASS: hold the native receipt transaction, contest its input from offboard and a restored wallet's delegated refresh, then verify both lose and exactly one payout commits | `20261008T015114-dd9ac15a` |

The race test uses two copies of the generated test wallet: a failed offboard reserves the input locally, so attempting refresh from that same copy could silently select zero inputs. Initial run `20261008T014653-a95b50d6` exposed this test defect. Run `20261008T014953-91fa206a` reached the server but failed an overly narrow error-text assertion; the server returned the exact coin as unusable. The passing case requires the rejected coin ID, checks no round participation was created, and independently verifies the final on-chain payment and fee. These are regtest results, not signet or a complete suite.

Recovery and funding cases on the startup-checked native binary:

| Scenario | Observed result | Evidence run |
| --- | --- | --- |
| `commit-reply.py` | PASS: the real PostgreSQL COMMIT response is dropped after commit; the task resolves the durable payment and broadcasts it once | `20261008T015346-f52b94bd` |
| `receipt-outage.py` | PASS: an unwritable receipt directory does not stop payment; fixing the path recreates the exact receipt, as does deleting the file afterward | `20261008T015439-145d62a6` |
| `participating.py` | PASS: owner refresh after expiry wins before payout; the unfinished round input stays unpaid and its replacement later settles | `20261008T015740-d11b254f` |
| `unclaimed.py` | PASS with one original input: legitimate unclaimed output pays, a plain Core wallet finds it from the seed over indexes0..200 and spends it | `20261008T015934-936badf4` |
| `predecessor.py` | PASS with one original input: its confirmed exit excludes the unclaimed replacement from payout | `20261008T020018-e3968e15` |
| `fragmented.py` | PASS: funding shortage leaves claims intact; the smaller eligible payout uses10inputs, and returning the temporarily withdrawn capital releases the larger payment | `20261008T020105-a8a75839` |

Build the recovery helper with `cargo build --locked -p bark-server --example coin_key_descriptor` before seed recovery cases. It reads the generated wallet mnemonic on stdin; private descriptors and seeds stay in local test state, outside committed evidence. `INPUT_COUNT=2` expands the unclaimed/predecessor setup but is not implied by a one-input pass.

Failed setup attempts remain recorded: commit-reply `20261008T015234-56408767` could not bind a fixed proxy port; it now obtains a free port from the OS. Participating `20261008T015518-ef012ac9` tried to schedule a refresh beyond expiry, which captaind correctly rejected. The corrected case performs an immediate expired-owner refresh instead. Fragmented funding temporarily moves this fixture's rounds funds to its own faucet and returns the same principal; it is not the no-top-up liquidity test.


`missing-anchor.py` kills captaind after it commits an unsigned round, while the signed funding update is held. After expiry, Core has never seen that funding transaction. The old task aborted every tick on that candidate and left a later ordinary swept coin unpaid: FAIL at the intended assertion, `20261008T011338-09cc58e0`. With the narrow not-found-as-waiting fix, the same persisted case passes `20261008T011650-5ce34723`; the unfunded replacement stays unpaid and the later valid coin pays. Use `REUSE_MISSING_ANCHOR=1` only to rerun an existing fixture, otherwise the scenario creates a new real interrupted round. Build plus163serverunits PASS `20261008T011527-10d0a02f`; fixed debug captaind SHA256 `fe5b51cedc50238c327b0032a3fe845e1c33d59f3d6a89893b4e72b3a5269eb3`. Transport errors still fail the tick and no missing transaction becomes affirmative sweep proof.

`liquidity.py` completed10real board/refresh/expiry cycles, with two users each and no rounds-wallet top-ups: PASS `20261008T020244-848141cd`. Every cycle reconciled actual rounds-wallet change against board inflow, gross recipient entitlements and mined round/sweep fees. Total board inflow1200000sat, recipient gross1190400sat, operator mining fees15858sat; rounds balance100084602→100078344sat. User wallets were funded before the accounting boundary; this measures recycling and operating fees, not a zero-capital service.

`sparse.py` produced actual native payouts at seed indexes0,257,65537,999999, then used only the mnemonic with Core descriptors to discover and spend them: PASS `20261008T021436-42395b96`. A0..200scan found only the first output. Core31's0..999999scan found all four in116.073seconds; sampled process RSS rose from280300KiB to a peak1185404KiB. This is one local measurement, not an unbounded scan guarantee. Recovery spend `927721029f1ac69d1caff92f5a72eda0f67bf42b5b2dd0a5546f86d9c8099d96` confirmed. The fixture inserts sparse, genuinely seed-derived key-history entries before boarding; it does not fabricate coin entitlements.

`mixed-cancel.py` reproduced a two-input unfinished delegated refresh where one original exited and the other remained stranded: BEFORE FAIL at the intended assertion, `20261008T021744-8b259ed1`. After cancellation support, the same persisted exchange PASS `20261008T022041-d306560e`: only the swept original paid, its149670sat principal became149207sat plus463sat fee at its own key, the replacement retired, repeated ticks kept one payment, and a seed-only Core wallet found and spent it. Payout `547889d00cc8ad706fb9dd4caa9a33a41a786ddeafbe02784d747904f5a54bd4`. Build plus163serverunit tests PASS `20261008T021912-58b41f1a`. Multiple replacements, competing forfeits and cancellation-specific crash/replay still need separate evidence.

`short-cycle.py` PASS `20261008T022319-fc1a1869`: four-block lifetime, exit delta1, board depth1, grace0, sweep depth1; actual board→refresh→sweep→payout took heights8400→8409. These are rehearsal values, not mainnet settings. `stock-watchman.py` PASS `20261008T022337-37a710e3`: unmodified upstream watchmand shares the optional schema and produces a real sweep followed by native payout. Both scenarios restore the original fixture configuration.
