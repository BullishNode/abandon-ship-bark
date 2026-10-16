-- A physical bound over reclaimed backing, not a second customer settlement ledger.
-- Source outpoints are unique because their backing can be reclaimed only once.
CREATE TABLE recovery_reserve (
	id TEXT PRIMARY KEY CHECK (id = 'rounds'),
	sweep_spk BYTEA NOT NULL,
	checkpoint_height BIGINT NOT NULL CHECK (checkpoint_height >= 0),
	checkpoint_hash TEXT NOT NULL
);

CREATE TABLE recovery_receipt (
	id TEXT PRIMARY KEY,
	transaction_bytes BYTEA NOT NULL,
	block_height BIGINT NOT NULL CHECK (block_height >= 0),
	block_hash TEXT NOT NULL,
	proof_state TEXT NOT NULL CHECK (proof_state IN ('unknown_gross', 'physical_bound')),
	proof_gap TEXT,
	CHECK ((proof_state = 'unknown_gross') = (proof_gap IS NOT NULL))
);

CREATE TABLE recovery_source_hold (
	id TEXT PRIMARY KEY,
	receipt_id TEXT NOT NULL REFERENCES recovery_receipt(id),
	gross_sat BIGINT NOT NULL CHECK (gross_sat >= 0),
	held_sat BIGINT NOT NULL CHECK (held_sat >= 0 AND held_sat <= gross_sat)
);
