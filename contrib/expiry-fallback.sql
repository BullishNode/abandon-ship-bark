-- Apply after expiry-settlement.sql, before starting this fork.
-- These tables are required for coin creation even when payouts are disabled.
CREATE TABLE IF NOT EXISTS fallback_record (
	mailbox_pk BYTEA PRIMARY KEY,
	spk BYTEA NOT NULL,
	seq BIGINT NOT NULL,
	sig BYTEA NOT NULL
);
CREATE TABLE IF NOT EXISTS key_link (
	user_pubkey BYTEA PRIMARY KEY,
	mailbox_pk BYTEA NOT NULL,
	sig BYTEA NOT NULL
);
ALTER TABLE expiry_settlement ADD COLUMN IF NOT EXISTS spk BYTEA;

-- The funding outpoint admits exactly one entitlement. Retain the row after
-- registration or payout: it serializes both paths and lost-COMMIT recovery.
-- The unsigned user coin is deliberately absent from vtxo until registration
-- or settlement, so generic transaction registration cannot activate it.
CREATE TABLE IF NOT EXISTS pending_board (
	id TEXT PRIMARY KEY,
	vtxo_id TEXT NOT NULL UNIQUE,
	vtxo BYTEA NOT NULL,
	expiry INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pending_board_expiry ON pending_board (expiry, vtxo_id);

-- The xpay monitor fails a payment attempt that never reached the node only
-- after its invoice expired: the node refuses an expired invoice, so a
-- delayed request can no longer start. NULL for older attempts.
ALTER TABLE lightning_payment_attempt ADD COLUMN IF NOT EXISTS invoice_expires_at TIMESTAMPTZ;
