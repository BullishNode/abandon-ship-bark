-- Original urgent attempts share the existing fee journal and allowance.
-- These rows retain unsigned inputs BEFORE signing; they grant no authority.
-- The independent u1/u2 witnesses must match every retained row on activation.
CREATE TABLE public.native_urgent_attempt (
	attempt_index BIGINT PRIMARY KEY CHECK (attempt_index > 0),
	candidate_hash TEXT NOT NULL UNIQUE CHECK (candidate_hash ~ '^[0-9a-f]{64}$'),
	publication_txid TEXT NOT NULL UNIQUE CHECK (publication_txid ~ '^[0-9a-f]{64}$'),
	purpose TEXT NOT NULL CHECK (purpose IN ('claim', 'progression')),
	policy_hash TEXT NOT NULL CHECK (policy_hash ~ '^[0-9a-f]{64}$'),
	opening_hash TEXT NOT NULL CHECK (opening_hash ~ '^[0-9a-f]{64}$'),
	reservation_entry BYTEA NOT NULL CHECK (octet_length(reservation_entry) > 0 AND octet_length(reservation_entry) <= 4096),
	candidate BYTEA NOT NULL CHECK (octet_length(candidate) > 0 AND octet_length(candidate) <= 4194304),
	signed_entry BYTEA CHECK (octet_length(signed_entry) > 0 AND octet_length(signed_entry) <= 4096),
	signed_record BYTEA CHECK (octet_length(signed_record) > 0 AND octet_length(signed_record) <= 4194304),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CHECK ((signed_entry IS NULL) = (signed_record IS NULL))
);
-- Whole EXTERNAL inputs stay retained, including unused or uncertain attempts.
-- Cross-class exclusion is checked under the existing NativeFeePublication lock.
CREATE TABLE public.native_urgent_input (
	outpoint TEXT PRIMARY KEY CHECK (outpoint ~ '^[0-9a-f]{64}:[0-9]+$'),
	attempt_index BIGINT NOT NULL REFERENCES public.native_urgent_attempt(attempt_index)
);
CREATE INDEX native_urgent_input_attempt ON public.native_urgent_input (attempt_index, outpoint);
