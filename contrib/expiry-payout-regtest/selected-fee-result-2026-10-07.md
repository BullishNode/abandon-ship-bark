# Selected-target real fee estimate

On the private abandon-captaind-task regtest fixture, hide only Core target1 while
target6 remains real. The same scenario must pay a swept eligible coin at target6.

| Check | Result |
| --- | --- |
| Old image expiry-d-e2f14b7d56b3 | Expected FAIL: eligible payout times out; observation20261008T043058-2039fc20 |
| New image expiry-d-b2c19a215f3c | PASS; observation20261008T043257-821d0486 |
| All estimates missing, then restored | PASS; observation20261008T043523-04377b58 |
| Server unit suite | 162 PASS / 0 FAIL |
| Repository workspace/clippy/WASM prechecks | PASS |

After-case payment15f172831f8bf1c6382be3bacf88405e81f6974d79cb4699103023116995f584:
its decoded owner output plus charged fee equals the original coin. The outage
case leaves the coin spendable until a real estimate returns. Proxy responses use
Core's valid result schema including blocks; no synthetic rate funds either case.

New binary SHA256:b2c19a215f3c26c631ddaa50d53b9bb131cc778083b2c92a2a8ca850102b12f5.
Image ID:sha256:6314a4abcf7e1bfeeee3b034808f859ac225242cae7a239c382e6b6fa57c220f.
The image contains the production diff in this commit atop46caac9f452a373b6cda8030990e1d125efd3b29.
These focused checks do not replace a complete shipping-image suite or signet.
