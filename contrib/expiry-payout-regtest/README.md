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
