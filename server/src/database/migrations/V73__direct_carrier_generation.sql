-- A new-route journal has not been deployed. Historical row/heap order is not
-- a carrier chronology: refuse populated pre-generation state rather than
-- invent precedence for signed payments that might already be on chain.
DO $$
BEGIN
	IF EXISTS (SELECT 1 FROM expiry_carrier) THEN
		RAISE EXCEPTION 'direct carrier generation requires a fresh expiry_carrier journal';
	END IF;
END
$$;

-- The existing carrier advisory lock serializes the writer through COMMIT.
-- Failed/idempotent insert attempts may leave gaps; order is not gapless.
ALTER TABLE expiry_carrier ADD COLUMN generation BIGSERIAL UNIQUE CHECK (generation > 0);
