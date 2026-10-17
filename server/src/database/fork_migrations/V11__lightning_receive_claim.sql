-- A validated receive destination survives collection and a missing claim caller.
-- The output row need not exist until the later materialization transaction.
CREATE TABLE expiry_lightning_receive_claim (
	source_vtxo_id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	subscription_id BIGINT NOT NULL REFERENCES lightning_htlc_subscription(id),
	payment_hash TEXT NOT NULL,
	output_vtxo_id TEXT NOT NULL UNIQUE,
	output_vtxo BYTEA NOT NULL,
	materialized BOOLEAN NOT NULL DEFAULT FALSE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
