# Checkpoint19 independent publication audit

PASS: no blocker found for PR23 fixed head `e8d0beafb5b42d6d6a54a8d74686660d1dfe8d9e`, whose immediate parent is fixed report18 `5dde8947196fb840ec616871d662e658f1c038d9`. GitHub head/branch/body and all 81 changed paths independently match the publication receipt and exact docs allowlist. The 80 payload files plus manifest match every full SHA256 and committed Git byte, frozen external copy and owned docs copy. Manifest SHA256: `94d4cb3e8d4ae36c3d163a1814fe77bff3ecf2e9f37a28f892730419684f3505`.

The fixed cutoff is 248 executions: 233 PASS, 15 FAIL; 231 unique pairs, 221 nextest invocations, 236 native plus 12 component executions, and 459 mixed coverage rows. The exact 333,206-byte /456-row prior prefix and every old FAIL row remain unchanged. Only FT-BARKD-044–046 are appended. Historical aggregate executions are not one-to-one with mixed CSV rows.

Independent static checks passed: 46 JSON, six Python AST, four shell syntax, six TOML and one CSV; Git whitespace check; strong credential patterns; full 2,048-word English BIP39 checksums; WIF checksums. All 16 protected owned Rust files match their frozen baselines and remain unstaged. No raw wallet/DB/seed/config or native logs were read.

Batch47 qualifies only its actual old-v10 Exited/discovery/address-nonreuse oracles. It does not prove recovered payment, full financial conservation, Core 6 recovery, full ML, production readiness or newer product fixes. P07 source/artifact proofs remain pre-runtime evidence at this cutoff; its separately reported later PASS is excluded. No frozen payload, source, Git/index, controls or rolling reports changed during this audit.

Detailed checks and exact provenance: [audit JSON](checkpoint19-publication-independent-audit.json), SHA256 `61befa64e6f5d8a6b93c04cfde6bf58eb86e29a9f20f9f7791172048d0fda25b`.
