# H31 source-only proposal

H31 adds five test candidates for P2PKH, P2SH, P2WPKH, P2WSH and P2TR fallback destinations. In each case, three original SDK accounts use distinct payout scripts. They receive amounts equal to the 1,000 sat output fee plus the actual script dust threshold, with one receipt one sat below that boundary, one exactly at it, and one one sat above it.

The proposed assertions require the whole below-boundary receipt to be forfeited with no fee or payout. The other two receipts must pay the dust threshold and one sat more, each with a 1,000 sat fee. Three normal SDK sends retain the default change split factor of two; the sender's four remaining coins share one output fee.

The source also asserts a closed 300,000 sat backing-and-sponsor cut, actual canonical parent and fee-child transactions with at least six confirmations, complete returning-wallet history and balance checks, repeated synchronization, and reopening the same databases. These are assertions awaiting execution. They are not observed financial or recovery results. The original board change and board miner fee sit outside that selected cut; public script attribution does not independently prove custody.

The sole new Rust file was committed as `951b2a16f10e818492c95520b648c2ecf1ef8eb8` from fixed PR29 head `7fe9f2a181a58628dc8258b5ef97cde9aaed396d`. Its SHA256 is `ad89e56eaf8cfda9d13941eca04ae966da9f4025ecbef752e343d8ec48e2b381`. The raw add-file patch is included; no separate Rust copy or private protection snapshot is published here.

Independent source reviews found no concrete blocker. The file remains unregistered, uncompiled and NOT RUN. Each case's 180-second deadline and 96-block limit are unmeasured. The coordinator alone controls future module integration, coherent source capture, compilation, resource allocation and native execution. This packet supplies none of those permissions.

Existing totals remain 257 executions: 242 PASS and 15 FAIL, with 237 distinct binary/name pairs and 468 mixed coverage rows. H31 adds no execution, coverage, ML acceptance or production credit. The included PR29 audit is context for an earlier fixed report, not evidence that H31 ran or that this new packet is published.

All copied reports retain their exact original bytes and dated wording. In particular, early peer-pending statements remain historical; later peer and intake receipts record their resolution. The sole manifest projection is explicitly labelled and omits private-copy and protection-snapshot fields. No wallet, database, configuration, backup, native log, seed, private key or runtime artifact is included.
