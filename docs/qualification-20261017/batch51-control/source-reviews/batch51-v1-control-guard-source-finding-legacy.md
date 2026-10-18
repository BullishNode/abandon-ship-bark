# Batch51 V1 control finding

**Pre-native blocker:** controller lines 181–182 accept only `1 passed, N skipped`, rejecting the valid default-profile `1 passed (1 slow), N skipped` summary. An actual exit0/exact PASS could therefore become a false controller FAIL. This is a static parser finding, not an observed native failure.

V1 PREPARATION85e42 and all 14 static hashes remain unchanged; syntax checks passed. Pending marker remains present, parent admission absent, and no native progress/results/context exist. No ACK was issued.

Use a separate V2 directory for the bounded optional slow annotation and corresponding path/probe-output changes. Keep exact selection, exit0/PASS/filter-count checks, 180-second limit, zero retries and financial oracles unchanged. No control bodies, listings, probes or native tests were executed during this review.
