-- Exact attempt/source provenance on the existing durable signed outbox.
-- NULL preserves original non-urgent nursery history without inventing metadata.
ALTER TABLE nursery_tx ADD COLUMN urgent_fee_attempt JSONB;
-- Publication permission, never payment or liability completion. Only set after
-- exact independent signed-history binding has been fsynced.
ALTER TABLE nursery_tx ADD COLUMN urgent_fee_bound BOOLEAN NOT NULL DEFAULT FALSE;
