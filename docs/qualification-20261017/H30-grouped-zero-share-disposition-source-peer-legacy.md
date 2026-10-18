# H30 grouped zero-share disposition source peer

**Source ACK; no concrete blocker. Not compiled or run.** Patch `f556330ac43ee5bccc0658ef439407ed003858418500457209acf61dcba8fca8` changes exactly one classifier line in `server/src/database/expiry_details.rs`. Applying only that substitution in memory maps original292206 to declaredc11c; the canonical original remains unchanged.

The query at183–193 joins the actual payout row by allocation ID **and exact payout script**. Fork V5 DDL48–55 declares a nonnegative non-null integer output index, positive output amount and unique allocation/script. This is the group output association; it is not each leaf's net share or a statement that the output is already paid.

For three lawful500-sat leaves, the1000-sat fee consumes500 from each of the first two sorted leaves; their payout shares are0, while the third contributes the500-sat output. All have forfeit0 and the same real group output. The old classifier labels the first twoBelowDust, contradicting the SDK's unchanged no-output/fee0/full-forfeit dust contract. The proposed output-index presence check classifies all threeCustomerPayout without changing money arithmetic. A genuine whole-group dust allocation has no payout row, so its index remainsNone and classification staysBelowDust.

Server parent txid/index/script/value checks at227–242 are unchanged. SDK `fallback/payouts.rs:153–187` still requires exact principal conservation, registered script, output/value/parent and valid stage. `fallback/protection.rs:22–69,136–168` still checks aggregate fee/value, no forfeiture, positive real output, actual transaction/script, canonicalCore6 and signed source ancestry. Committed output association alone cannot discharge protection. Grouped history/debit code and existing wrong-script/value/excess-fee/output-reuse/dust-negative test source remain byte-identical. No forged-output acceptance result is claimed from this review.

Future validation must use the authentic H30 grouped payout plus returning SDK history, a true no-output dust case, and unchanged rejection checks. Root's frozenV16 does not include this patch or test; no old/current result is promoted. Schema.sql was inspected first; these fork-table constraints are defined by the explicit V5 fork DDL.

[Exact source hashes and review](H30-grouped-zero-share-disposition-source-peer-legacy.json), SHA256 `4552714e70e68bbaae0731dcfd243e3e19a72a7bf4d92c35777e78d7f32ed7ab`. No canonical/product/source/global/control/Git/build/native changes; only this new review pair was written.
