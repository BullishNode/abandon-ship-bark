# Default integration inventory scope

Source-only inventory at canonical HEAD `814d7b0deb6a5f2a6758d34ca4313589f948f4e3`, plus the reviewed three-file owner-scan batch/fence patch. Exact captured source manifest: `704179331e4f672ac09f5dad6dc564d42fa36707dc32bad5b6889c831848cf06` (`owner-scan-split-v1-manifest.json`). Nested worktrees and untracked drafts are outside that capture. No Cargo, metadata/list execution, native run, canonical mutation or Git write was performed for this inventory.

## Exact ordinary recipe

`just int` aliases `test-integration`, whose default empty TEST argument leaves this command unchanged (`justfile:159–163`):

```text
cargo nextest run --no-fail-fast --profile {{NEXTEST_PROFILE}} --package ark-testing \
  -E 'not binary(tor) and not binary(server-migrations) and not binary(upgrade)' {{TEST}}
```

`NEXTEST_PROFILE` defaults to `default` (`justfile:13`). Prerequisites are `ensure-build-bins` and `docker-pull`: the former builds product binaries unless ASSUME_BUILT is set; the latter conditionally ensures the configured Lightning image exists (`justfile:98–105,141–142`). CAPTAIND_EXEC, WATCHMAND_EXEC, BARK_EXEC and BARKD_EXEC may override the ordinary debug paths (`justfile:7–10`). Therefore a runtime result needs its actual products, adapter/image, features, environment and source pins; a compiled test inventory alone supplies no execution credit.

The immutable-binary equivalent is `just int-binaries BINARIES_METADATA`, which uses the same exclusion filter with `--binaries-metadata` instead of selecting/building the package (`justfile:165–170`). It can only run binaries present in the supplied metadata. An SDK/server-only metadata file cannot represent the complete ordinary integration inventory.

## Five ordinary integration targets

These are the declared named ordinary targets in `testing/Cargo.toml:11–29`. The coordinator's immutable five-binary nextest listing is now available and independently checked: **643 listed,638 default-selected,5 explicitly ignored**. Counts and exact per-case names come from that actual listing, not source regexes, macros or file counts. `default-integration-inventory.csv` contains643 unique binary/name rows with the real ignored flag and default selection; every row is classified INVENTORY_ONLY.

| Binary | Listed | Ignored | Default selected | Source entry |
|---|---:|---:|---:|---|
| bark | 202 | 1 | 201 | testing/tests/bark/mod.rs |
| barkd | 78 | 1 | 77 | testing/tests/barkd/mod.rs |
| core | 16 | 0 | 16 | testing/tests/core/mod.rs |
| bark-sdk | 77 | 0 | 77 | testing/tests/bark_sdk/mod.rs |
| server | 270 | 3 | 267 | testing/tests/server/mod.rs |
| Total | 643 | 5 | 638 | INVENTORY_ONLY |

The five ignored names are `bark::recover::recover_mnemonic`; `barkd::fallback_browser::fallback_barkd_web_actual_payout_and_seed_restore`; and server's `expiry_attribution::expiry_attribution_resolution_scale`, `expiry_registration_scale::expiry_registration_authenticated_scale`, `qualification_oversize::expiry_automatic_real_limit_multilevel_split_pays_every_owner`. Every ignored listing entry has filter mismatch reason `ignored`; all638 others have filter status `matches`. SELECTED means eligible under the captured nextest selection, not executed or PASS.

Immutable inputs reside under `/home/francis/.cache/abandon-fresh/runtime-owner-scan-split-v1-20261017/`. Recomputed hashes agree with the supplied inventory manifest: five-binary metadata `c22e785532b29bfcda024483c669e2ade5b8e96d0038dac9309df4bed51e8ea4`; test listing `16adbb314708184af6aefbbd5248656fb6ac419f2b5b71ae34e5532936f368e8`; inventory manifest `814533119b3eb24295fafb685ad02071c1b87aa2cb5b4351db8b97fd8658dbde`; paired candidate manifest `85149de281996da9a6c856e520de8b20247c9dd4b6912a0ba988543ccf6661a6`. Suite keys and executable paths agree exactly between listing and metadata; all five host immutable ELF hashes match the applicable candidate/extra-build manifest. Sanitized CSV SHA256 is `9dd77db32ecd9adc433a1b740fcb81f83bd3f7a02567c03fd07956058b1d7845`; `default-integration-inventory-summary.json` records full per-binary hashes and reconciliation.

`bark-sdk` is linked against the current wallet crate; changing BARK_EXEC does not make it a released-client compatibility test (`justfile:217–222`). The ordinary recipe also does not enable the separate BARK_DOUBLE_DRIVE_ACTIONS reentrancy variant (`justfile:207–215`), the Esplora/mempool chain-source variants (`justfile:271–290`), or every feature/storage/backend combination.

## Gates and separate suites

Ordinary nextest runs skip explicit ignored cases. Changing to `fallback-scale` changes timeout policy; it does **not** by itself opt into ignored tests. Examples requiring explicit selection/ignored execution are attribution scale (`testing/tests/server/expiry_attribution.rs:218`), signed registration scale (`expiry_registration_scale.rs:60`), and the327-owner oversize case (`qualification_oversize.rs:189–192`). The default profile has60-second slow periods and terminate-after3 (240-second runner bound); fallback-scale uses terminate-after29 (1,800-second runner bound), while the oversize test retains its own tighter600-second timeout (`.config/nextest.toml:1–9`). These cases cannot silently count as passing default integration coverage. Runtime version guards and feature gates must also be reported honestly; presence in a listing does not prove the full intended behavior executed.

| Excluded binary | Separate entry | Required distinction |
|---|---|---|
| server-migrations | just int-server-migrations | Actual old captaind via OLD_CAPTAIND_EXEC; in-flight server upgrade (`justfile:172–178`; testing/tests/server-migrations/mod.rs:1–12) |
| upgrade | just int-upgrade | OLD_BARK_EXEC is required; actual old-client action checkpoint reopened by candidate (`justfile:180–199`; testing/tests/upgrade/mod.rs:1–7) |
| tor | just int-tor | Owned Tor hidden-service/proxy integration (`justfile:224–228`; testing/tests/tor.rs:12–40) |

Migration/fork-history component cases inside the server binary remain part of ordinary selection if not individually gated, but they do not substitute for the separately excluded actual-release upgrade suites.

## Execution accounting

The frozen-source **unit** receipt `owner-scan-split-v1-units-summary.json` records run `825f6b81-f598-456b-ab42-bb844161a470`: **800 PASS,1 skipped**,12.356s, source manifest70417933. `just unit` explicitly excludes ark-testing (`justfile:144–147`). Its one skip is the pre-existing ignored `ark-lib::arkoor::test::isolate_dust_fuzz`, documented separately in `owner-scan-split-v1-units-skip-audit.json`. None of these800 results is integration execution credit.

The complete default integration suite at this frozen source is **NOT EXECUTED by this inventory**; the metadata build and actual list establish availability and selection only. Earlier or separately executed named integration receipts retain only their own exact test/product/source scope, including any explicitly run ignored oversize case; none changes the643-row INVENTORY_ONLY classification. The seven historical unchanged-upstream regression rows labelled provenance `bfbe6b7c3` in COVERAGE.csv are not a full upstream-suite run; authored fallback/qualification overlays are not unchanged upstream tests. A future result should distinguish default selection, ignored/filtered/gated cases, separately excluded suites, and the exact authored overlay without claiming all upstream tests or every configuration passed. The800/1 baseline unit receipt above remains unchanged; any later separately authorized ignored-unit execution belongs in a separate receipt.
