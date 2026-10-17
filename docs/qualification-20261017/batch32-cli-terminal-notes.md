# CLI batch32: terminal first-case failure

Original source7041 `offboard::bark_send_onchain`, FT-CLI-059, run7a6f0ea0-1685-4abd-bfbe-664ed76582ad, **FAIL18.621s**, nextest summary18.632s. Parent87245 terminated with100. No retry;0PASS/1FAIL/17unstarted.

The original real800k source produced change_total499146 against fixedexpected499062 at offboard.rs143. Its preceding expected change-count assertion passed. The old fee literal938 is at137; later movement/destination/recipient balance assertions145–159 did not run. Native receipt financial reconciliation remains PENDING; a separate client retained-money supplement may resolve the actual fee/chain accounting without altering this original FAIL or crediting unexecuted assertions.

Only FT-CLI-059 appended COVERAGE394→395 with all394 rows preserved byte-for-byte. NewSHA0d816261693558f7dcaffd52fea114380b2732ff04da14efddd492302e63b709; append event2d032fcf…. Parent-reported checkpoint184 executions=171PASS+13FAIL,180 unique names,157 invocations,173 native+11 component. No full suite/release/ML claim.

Fresh17 prep is exactly stopped18expected_names[1:], first `offboard::bark_send_onchain_too_much`, with no scan-based exclusions. All runtime remains root-owned. No native/Cargo/metadata execution, source product change, STATUS/publicdocs or Git mutation by this agent.
