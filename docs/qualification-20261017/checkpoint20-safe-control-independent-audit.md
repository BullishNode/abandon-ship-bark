# Checkpoint20 P07 safe control export audit

PASS: no blocker found. All 36 source/output entries and the exact 37-file inventory match their full hashes, lengths and equality flags. Frozen EXPORT SHA256 is `4d6d159333d9f001916964ff38a4da5ecfcd7b83e3e0eafbc85e89fee55070d7`; independently recomputed sorted path+NUL+hash+LF digest is `bc376d35fb5e142a33e25970f1dceb9e270664841dafac35c29fde03f1027f99`. Every source input and the export remain unchanged.

All 16 active frozen controls match formal postflight. The active source6051 is distinct from the nine byte-identical historical closed-v15/abd0 files, including preserved pending markers; those originals also remain unchanged. All 17 authoritative pins are retained; the 14-row projection removes only the three explicit CLN/plugin rows.

PREPARATION, case/progress/summary and initial-native receipt projections preserve selected original values and omit command arrays. The native summary matches sanitized formal18c PASS/summary/run metadata without opening raw logs. Initial native9648 retains its temporal pending fields. The public receipt projection has exactly 37 fields, all values/types matching actual public SHA91ff and formal18c; no invented 38th field. The private-manifest reference is a hash only, and its target was not opened.

Independent checks passed: 22 JSON (21 payload files plus EXPORT.json), six Python AST, four shell syntax and two TOML parses; strong credential patterns, full 2,048-word English BIP39 checksums and WIF checksums found no secrets. Independent Core63ddb and stability6f49 receipts remain unchanged. No raw wallet, database, journal, backup, config, seed, native or Core log was read.

This audit covers this subtree only; the parent must validate the final full publication payload. It adds no execution or financial qualification. The result remains one configured clean HtlcsReady boundary and selected605k cut, excluding continuing CLN12BTC, crash/PITR, full P07/ML and global custody. No source, controls, runtime, Git or rolling reports changed.

[Detailed audit JSON](checkpoint20-safe-control-independent-audit.json), SHA256 `8857c91cb1018f36f1cd85adc707a6280208573eaa3e73fd7749fd7094b7989b`.
