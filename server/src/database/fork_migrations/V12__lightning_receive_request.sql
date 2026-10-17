-- An admitted claim can be retried without the original provider. The request
-- includes its public signing nonces, attestation and payment preimage, never
-- the user's signing secret. Existing V11 intents are not reconstructed.
CREATE TABLE expiry_lightning_receive_replay (
	subscription_id BIGINT PRIMARY KEY REFERENCES lightning_htlc_subscription(id),
	payment_hash TEXT NOT NULL UNIQUE,
	encoding_version INTEGER NOT NULL CHECK (encoding_version > 0),
	protocol_version BIGINT NOT NULL CHECK (protocol_version > 0),
	request BYTEA NOT NULL CHECK (octet_length(request) <= 4194304),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	last_attempt_at TIMESTAMPTZ,
	uncollected_at TIMESTAMPTZ
);

CREATE INDEX expiry_receive_replay_pending_order
	ON expiry_lightning_receive_replay (last_attempt_at NULLS FIRST, subscription_id);
CREATE INDEX expiry_receive_unmaterialized_subscription
	ON expiry_lightning_receive_claim (subscription_id) WHERE NOT materialized;
