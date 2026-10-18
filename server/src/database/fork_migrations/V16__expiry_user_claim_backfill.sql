-- Nonfinancial service position only. No expiry ledger/history revision trigger.
-- An epoch's finite row-ID watermark prevents new arrivals extending it forever.
CREATE TABLE expiry_user_claim_backfill_service (
	id SMALLINT PRIMARY KEY CHECK (id=1),
	after_id BIGINT NOT NULL DEFAULT 0 CHECK (after_id>=0),
	through_id BIGINT NOT NULL DEFAULT 0 CHECK (through_id>=after_id)
);
INSERT INTO expiry_user_claim_backfill_service(id) VALUES(1);
