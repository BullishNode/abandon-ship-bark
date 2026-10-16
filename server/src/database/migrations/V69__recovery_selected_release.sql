-- Existing activated rows intentionally have no invented fee policy. Native
-- startup refuses NULL until a declared maintenance transition supplies one.
ALTER TABLE recovery_reserve ADD COLUMN fee_headroom_sat BIGINT
	CHECK (fee_headroom_sat > 0 AND fee_headroom_sat <= 2100000000000000);
ALTER TABLE recovery_source_hold ADD COLUMN revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0);

-- Mirrors one existing expiry_settlement's capital effect. That optional fork
-- table is installed separately, so membership is checked in the same native
-- settlement transaction instead of a migration-time foreign key.
CREATE TABLE recovery_capital_release (
	id TEXT PRIMARY KEY,
	payout_txid TEXT NOT NULL,
	source_id TEXT NOT NULL REFERENCES recovery_source_hold(id),
	receipt_id TEXT NOT NULL REFERENCES recovery_receipt(id),
	gross_sat BIGINT NOT NULL CHECK (gross_sat > 0),
	source_revision BIGINT NOT NULL CHECK (source_revision >= 0)
);
