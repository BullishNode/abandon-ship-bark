-- Run with psql autocommit before starting this fork. Not a refinery migration.
-- Stock watchmand keeps the same numbered schema; captaind must understand this kind.
ALTER TYPE nursery_tx_kind ADD VALUE IF NOT EXISTS 'expiry-payout';
CREATE TABLE IF NOT EXISTS expiry_settlement (
	id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	txid TEXT NOT NULL REFERENCES nursery_tx(txid),
	fee_sat BIGINT NOT NULL CHECK (fee_sat >= 0)
);
CREATE INDEX IF NOT EXISTS expiry_settlement_txid ON expiry_settlement (txid);
CREATE INDEX IF NOT EXISTS expiry_payout_candidates ON vtxo (expiry, vtxo_id)
	WHERE policy_type='pubkey' AND spend_state IN ('spendable','unclaimed')
	AND confirmed_height IS NULL;
-- Keep the exact exchange attribution when its live participation is removed.
CREATE TABLE IF NOT EXISTS expiry_cancelled_participation (
	id TEXT PRIMARY KEY,
	round_id BIGINT NOT NULL REFERENCES round(id),
	input_ids TEXT[] NOT NULL,
	output_ids TEXT[] NOT NULL,
	exited_ids TEXT[] NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
