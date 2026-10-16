-- Original fee activation is distinct from individual publication intents.
-- The independent owner-private certificate/history must also agree on load.
CREATE TABLE public.native_fee_opening (
	id SMALLINT PRIMARY KEY CHECK (id = 1),
	opening_hash TEXT NOT NULL CHECK (opening_hash ~ '^[0-9a-f]{64}$'),
	policy_bytes BYTEA NOT NULL CHECK (octet_length(policy_bytes) <= 4096),
	opening_bytes BYTEA NOT NULL CHECK (octet_length(opening_bytes) <= 4194304),
	history_identity TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
