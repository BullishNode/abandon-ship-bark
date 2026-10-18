# Recovery round-retirement v2: independent source review

**Scoped source ACK; no concrete API or oracle blocker found. UNCOMPILED and NOT RUN.**

Reviewed owned recovery source `97118c62993592f80fc167a1346168389c0386dae88a762cc99205ee417567b4`, delta `e674bcafc14011cac555606d19d33b80a9c030d408dbbb98b587ae663f284142`, aggregate `546269c665c58c75a297b49b524c187a1842f2e534b13fa2881af9ba7ff4622c` and manifest `7ecb78075275a4600bec876e5c097e3660907062af683c1d3fb26f48af177395`.

Independent in-memory reversal of all four delta hunks removes exactly 46 added lines, with no deleted v1 statement, and reconstructs whole-file v1 SHA `23f4a74b916c5dcc429b442ec099808ad2757df74d4162fbed7ec4d575b736c5`. Masking only the two exact target functions leaves byte-identical surrounding source against both v1 and original c52. Imports, shared strict partition helper, quarantine and genuine valid same-anchor Arkoor control remain unchanged. Frozen v1 patch/notes/manifest hashes still match. No patch was applied to source or canonical files.

Each target now checks all ten public monetary buckets: the actual recovered current cut appears only in spendable before the existing real payment; all ten are zero afterward. The existing recipient exact sum/per-anchor, recovered current input Spent, full known historical ancestor identity/body/Locked(None,None), and exact final singleton checks remain. The new exit-status assertions require no persisted exit entry at both observations. They neither force a historical per-VTXO awaiting-settlement flag false nor treat the historical body as positive principal.

API inspection: `testing/src/daemon/barkd.rs:433` returns the public off-chain Balance without forcing synchronization; all ten referenced fields are bitcoin Amount (`bark-json/src/cli/mod.rs:251`). `barkd.rs:625` returns all exit statuses. REST `bark-rest/src/api/v1/exits.rs:167` forwards None detail flags as false to list_all; `bark/src/exit/mod.rs:496` retrieves every persisted exit entry. The flags suppress history/transaction details rather than removing live or finished entries. No new API or internal-call dependency was introduced.

The checks are two point-in-time prospective oracles. They supply no actual balance/exit response, future act-by/protection execution, default-daemon, absent-original, offline restore, Core 6, full custody or FULL ML result. Original FAIL/held dispositions and the strict unsafe-body control remain unchanged. The original round-test documentation outside the allowed body scope still describes the old retirement expectation; it is historical prose, not an executed skip/Spent oracle in this revised body.

No Rust parser/type-check/compile, Cargo, native test, controller, preflight, listing, DB, RPC, actor or Git mutation was performed. The author's unavailable rustfmt exit127 supplies no parsing credit. Reviewed API file hashes are recorded below as the actual local source inspected, not a claim of new candidate capture or compilation.

- `testing/src/daemon/barkd.rs`: `40c0539e04a79f0cd7bf1438217a9712e8934cf4f1d946cc9c42aff78cfa12b3`
- `bark-json/src/cli/mod.rs`: `49936f9af4d5ebd00e64f603e2c580749e84a39dd0dc4cb66c22ad1ecb82827a`
- `bark-rest/src/api/v1/exits.rs`: `e925334b853094bdefe159a7c9788ab464b8eeb3003f22a0dc18f8e1e91e8168`
- `bark/src/exit/mod.rs`: `ed351105d2fc368b18d7a9f6f737bfe6214417c246c313d20403ca9b3c89917e`
