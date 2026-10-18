# P07 v11c mechanical compile correction: independent source ACK

**ACK: exact narrow source correction; no additional blocker found.** Protected owned source remains `abd0e399a1676ad425d7780fdddf61bd7f25fa9982cb6e3ba294cebc6f622695`; canonical corrected source is `6051ab33231cbe9cb88dad49fd8fa2baa852aade2bc519476c37ae24b17a870c`. Parent receipt3ea2 is independently bound in the companion JSON.

Independent whole-file replacement proves only two lines change: at534 the local maturity block hash is named block_hash, and535 uses BlockRef { height, hash: block_hash }. The earlier invoice PaymentHash variable at299 therefore remains the value passed to get_htlc_settlement_by_payment_hash at628 and payment-hash SQL at637. The scoped loop-local block hashes already end within their own blocks. This fixes the root-reported E0308 shadowing/type mismatch without altering amounts, policies, state assertions, deadlines or backup/drain oracles.

Independently verified all thirteen helper hashes, test entry and ExistingCluster implementation against both sources/parent bindings. Main function hash b475c2b7 and brace-inclusive body29508f4a agree. The protected owned source and old packets remain unchanged. The prior v2 peer review was source-only and supplied no compiler guarantee; this root-discovered correction is separate later evidence.

Root's earlier checks failure remains a compilation event, not native FAIL or financial-loss evidence. This review supplies no Rust parser/type-check/build/native PASS, module/admission/allocation or ML/count credit. Exact v11c capture/artifact pins and runtime proof must be verified separately. Only two new external peer reports were written; no source, control, index, HEAD, rolling report, Cargo or runtime mutation occurred.
