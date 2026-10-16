-- Input ownership and exact candidate provenance; all fee debits stay in the
-- existing independently retained Watchman ledger. NULL signed means unresolved.
CREATE TABLE native_funding_fee_intent (
 attempt_key TEXT PRIMARY KEY,
 reservation JSONB NOT NULL,
 signed_tx BYTEA
);
CREATE TABLE native_funding_fee_input (
 outpoint TEXT NOT NULL,
 attempt_key TEXT NOT NULL REFERENCES native_funding_fee_intent(attempt_key),
 PRIMARY KEY(outpoint, attempt_key)
);
