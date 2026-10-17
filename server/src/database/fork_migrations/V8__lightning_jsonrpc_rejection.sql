-- Receipt of an explicit upstream no-attempt error, distinct from a transport
-- failure. Only source-reviewed CLN26.06.6 errors are admitted here.
CREATE TABLE expiry_lightning_rpc_rejection (
	id BIGINT PRIMARY KEY REFERENCES expiry_lightning_dispatch(id),
	code INTEGER NOT NULL CHECK (code IN (205,207)),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE expiry_lightning_terminal_failure
	ALTER COLUMN created_index DROP NOT NULL,
	ADD COLUMN jsonrpc_error_code INTEGER,
	ADD CONSTRAINT expiry_lightning_failure_provenance CHECK (
		(created_index IS NOT NULL AND jsonrpc_error_code IS NULL)
		OR (created_index IS NULL AND jsonrpc_error_code IS NOT NULL AND jsonrpc_error_code IN (205,207))
	);
