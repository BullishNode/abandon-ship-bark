No concrete blocker found in the frozen batch45 control export. Reviewed `checkpoint18-proposed-safe/batch45-control/EXPORT.json` SHA256 `468d0e109ca6d2a0fc4dc894ed982a3df42bcaad4d2241855418ffe9b0cfda29`. This ACK covers only that31-file subtree:30 declared exports plus its receipt. The parent full checkpoint18 manifest/global payload is not yet reviewed or claimed final.

Every declared source and destination hash, destination byte length and byte-equality flag matches the actual files. The actual file inventory exactly matches the allowlist; there are no symlinks or extra files. All13 exported frozen controls match parent admission `439f73b462d886017a0fd8a91b1c5379362543d8338b79dc138d238fb070748c` and postflight `97f378ee2371aa37bfa33bf239c20d9a7d59aec74eb1c7d9f1d54a8a80d84c8e`. Source inputs and export hashes remain unchanged after this review.

Progress, cohort summary and all three result projections retain exact original selected values and remove runtime command/argv fields. Each bounded native summary exactly projects the frozen postflight, including actual barkd binary, source97118/body hash, fresh singleton selection, default180/no retry, raw-log SHA only and these actual terminal results:

| Exact test | Run ID | Native seconds |
| --- | --- | ---: |
| recovery::recovered_wallet_finds_round_vtxo | fad96514-47ba-4b94-9348-1c597054a1a5 |34.026 |
| recovery::recovered_wallet_finds_mixed_origin_vtxos |02a13f8b-6088-48db-9248-9d06cd1a91e8 |37.976 |
| recovery::recovered_wallet_quarantines_invalid_spent_vtxos |a35c677b-696f-4674-843d-87bdd2c4839f |22.385 |

All3PASS/0FAIL sum94.387 native seconds. Only one new unique binary/test pair is claimed. Prior mixed FAIL and prior quarantine PASS are not overwritten. Root FAIR/sponsor/partial lanes are not included. Native assertion limits remain those of postflight; no independent financial/FULLML/production credit is added by exporting controls.

The primary root pin file contains17 entries. All14 ordinary artifacts were independently rehashed through host-equivalent paths; the actual CLN binary and two plugins were independently rehashed in the admitted container using only sha256sum, with exact matches. The derived `core-pins-14.sha256` is byte-for-byte the first14 lines of the authoritative17-pin file. Its derived status and the three unused-by-these-cases CLN entries are explicit; no CLN execution is credited.

The112-byte original caller exit127 log contains only the known missing-python3 diagnostic/path. The1214-byte corrected preflight log contains the impure environment marker and17 public pin OK lines. The separate environment/actor diagnostic receipts correctly distinguish pre-native caller/probe failures from native failures/retries. These are bounded safe diagnostics, not raw native or wallet logs.

No raw nextest/wallet log, private database, mnemonic, key, identity/config or runtime argv file is exported. Controller source naturally contains command construction; observed result/progress/summary projections contain no runtime argv. All31 text files passed bounded literal PEM/private-key/WIF/xprv/tprv/credential-pattern checks and a12-consecutive-word BIP39 English scan using the actual2048-word bip39-2.2.2 wordlist, with zero hits. These scans support this explicit allowlist review; they are not a universal secret detector. Parent's complete-payload publication scan remains required.

Independent syntax-only checks passed: three Python AST parses, two bash -n checks, three TOML parses and the exported JSON parses. No controller, preflight body, fresh listing, Cargo, native test, service, actor inspection or root financial audit was executed. Only this new external audit Markdown was written; export/private controls/original receipts/source/rolling reports remain unchanged.
