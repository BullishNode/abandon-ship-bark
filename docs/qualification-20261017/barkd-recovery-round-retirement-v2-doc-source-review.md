# Queued recovery documentation delta

Source-only ACK; no concrete blocker. Exact one-hunk a7f3d4e patch applies in memory to97118 and produces8fb7578. It changes only two removed/three added `///` lines above the round test, correcting the old claim that recovery skips the historical board. Every non-doccomment byte, all three selected bodies, and the strict shared helper remain identical.

The delta is queued after the reported v10 capture. It is not a compile or runtime result, and was not applied to either source tree. No Cargo, listing, preflight, runtime or actor action occurred.
