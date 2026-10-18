# H30 v2 — explicit macro imports only

The parent source review found that child source v1 called two unqualified ark_testing macros without importing them. `testing/Cargo.toml` identifies `tests/server/mod.rs` as the test crate root; its ordinary `use` does not import macro names into this child. No global `macro_use` override exists.

v2 adds `require_bark_version` and `require_bitcoind_chain_source` to the one existing ark_testing import. Reversing that single line recreates v1 SHA106bd exactly. No fixture bodies, financial assertions, timeouts, block budget or retry policy changed. All122 previous test files and v1 source/packet/peer hashes remain unchanged. v1 peer acknowledgement is historical and superseded for admission by this finding.

Source only: no compile or native failure was observed; no parse/typecheck/runtime was run. v2 requires independent delta review and a future exact candidate build/allocation.
