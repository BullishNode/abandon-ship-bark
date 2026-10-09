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
