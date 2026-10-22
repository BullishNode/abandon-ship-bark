-- This column records only the successful creation path. Do not backfill old rounds.
ALTER TABLE round ADD COLUMN padding_issuance JSONB
	CHECK (padding_issuance IS NULL OR
		(jsonb_typeof(padding_issuance)='object' AND octet_length(padding_issuance::TEXT)<=4194304));

CREATE FUNCTION round_padding_issuance_immutable() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
	IF NEW.padding_issuance IS DISTINCT FROM OLD.padding_issuance THEN
		RAISE EXCEPTION 'round padding issuance cannot be updated';
	END IF;
	RETURN NEW;
END;
$$;
CREATE TRIGGER round_padding_issuance_immutable BEFORE UPDATE OF padding_issuance ON round
	FOR EACH ROW EXECUTE FUNCTION round_padding_issuance_immutable();
