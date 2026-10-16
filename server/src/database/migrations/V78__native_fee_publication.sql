-- Fee evidence does not create customer obligations or automatically publish.
-- The independent native history is authoritative for every retained charge.
CREATE TABLE public.native_fee_publication (
	wtxid TEXT PRIMARY KEY CHECK (wtxid ~ '^[0-9a-f]{64}$'),
	txid TEXT NOT NULL CHECK (txid ~ '^[0-9a-f]{64}$'),
	purpose TEXT NOT NULL CHECK (purpose IN ('round', 'pool')),
	record_hash TEXT NOT NULL CHECK (record_hash ~ '^[0-9a-f]{64}$'),
	policy_hash TEXT NOT NULL CHECK (policy_hash ~ '^[0-9a-f]{64}$'),
	opening_hash TEXT NOT NULL CHECK (opening_hash ~ '^[0-9a-f]{64}$'),
	signed_tx BYTEA NOT NULL CHECK (octet_length(signed_tx) > 0 AND octet_length(signed_tx) <= 400000),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	published_at TIMESTAMPTZ
);
-- An uncertain or unused permit retains the WHOLE original input. No routine
-- confirmation, failed publication or restart deletes/renews this intent.
CREATE TABLE public.native_fee_input (
	outpoint TEXT PRIMARY KEY CHECK (outpoint ~ '^[0-9a-f]{64}:[0-9]+$'),
	wtxid TEXT NOT NULL REFERENCES public.native_fee_publication(wtxid)
);
CREATE INDEX native_fee_input_candidate ON public.native_fee_input (wtxid, outpoint);
