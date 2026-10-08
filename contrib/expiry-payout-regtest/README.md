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
