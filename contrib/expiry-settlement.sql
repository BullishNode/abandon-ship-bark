-- Apply once to captaind's database before using the optional settlement RPC.
-- Kept outside refinery so this maintained patch does not take an upstream
-- migration number or prevent stock watchmand from opening the same database.
CREATE TABLE IF NOT EXISTS expiry_settlement (
	id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
