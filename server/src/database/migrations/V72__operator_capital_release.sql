-- Operator relief is a disposition of a proved original source, not a payout.
-- Existing customer releases keep their original identity and constructors.
ALTER TABLE recovery_capital_release
	ADD COLUMN kind TEXT NOT NULL DEFAULT 'payout',
	ADD COLUMN operator_proof BYTEA,
	ALTER COLUMN payout_txid DROP NOT NULL,
	ADD CONSTRAINT recovery_capital_release_kind_ck CHECK (
		(kind = 'payout' AND payout_txid IS NOT NULL AND operator_proof IS NULL)
		OR (kind = 'unused-pool' AND payout_txid IS NULL AND operator_proof IS NOT NULL)
	);
