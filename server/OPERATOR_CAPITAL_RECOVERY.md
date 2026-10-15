# Reclaimed original pool capital

A confirmed sweep into the rounds wallet reserves each source's gross backing.
This release can retire a source positively proved to contain only untouched
original operator pool leaves, then release that source's gross hold. Customer
settlement, recipient selection and urgent claims use their existing paths.

The proof requires the exact original native pool funding transaction, its Core
confirmation, an exhaustive connected signed tree with no unclassified positive
terminal output, and every original leaf in its unchanged native `pool` state.
The confirmed receipt must spend that one source and pay only the configured
recovery script, with the existing network confirmation policy. Missing pool
membership, earlier allocation, a customer branch or an incomplete tree leaves
the full gross reserved. This first implementation supports up to 256 leaves and
1,024 signed path transactions per source, with a four-MiB source proof budget.
It does not classify arbitrary mixed trees or sources split across receipts.

Before any hold can fall, the server writes a typed operator certificate and
its index into `operator-recovery-witness`, a sibling of the customer payout
witness. Both are durable before the native retirement, hold revision and typed
`recovery_capital_release` row commit together. The certificate contains public
transaction bodies and signed-byte commitments, never unlock witnesses or
private keys. No customer payout or settlement row is invented for operator
relief. The customer payout constructor and ledger identities are retained.

Pool sends lock the same native parents before publishing any signed children.
They require exact eligible input membership before signing and recheck the
same bytes under SQL row locks before commit. A failed send restores only inputs
independently still eligible in the database, while those native rows remain
locked. Retirement in another process can still leave an old cache entry in a
process that did not perform retirement. A cache entry proves no spendability:
the authoritative lookup and parent-locked publication checks reject retired
leaves. Database reload excludes native retired rows.

On startup, before wallets or workers start, every indexed certificate must
survive independently, match the exact source, receipt and chain evidence, and
agree with native retirement and the release row. A certificate durable before
a crash but lacking its SQL commit replays only the identical still-eligible
intent. Ambiguous commit waits for the same locks and resolves the exact state;
missing, mixed or consumed evidence refuses recovery. Restoring both the
independent witness and database to an older point remains outside this guard.

New server creation initializes the sibling witness. An existing server without
it is refused when expiry payouts are enabled: deployment requires a separately
reviewed, independently reconciled first-enable bootstrap, preserving all old
customer witness history. Do not create an empty directory to bypass that gate.
The journal is retained with the server identity and must be backed up outside
PostgreSQL. Its startup limits are 100,000 records and 64 MiB of certificates;
reaching a limit requires an explicit retention design, never deletion or reset.

Gross operator relief is not customer payout, new funding, recycling, or fee
income. Reconcile closing chain cash against opening capital and actual funding
and sweep fees. Source classification and final financial qualification still
require executed native controls and full checks; the existence of this path is
not a production readiness verdict.
