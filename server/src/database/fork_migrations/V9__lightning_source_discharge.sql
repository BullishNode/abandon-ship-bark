-- These rows describe execution of an actual attempt, never merely knowledge
-- of an invoice preimage. Legacy attempts without a queue marker stay unknown.
CREATE TABLE expiry_lightning_queued_attempt (
	id BIGINT PRIMARY KEY REFERENCES lightning_payment_attempt(id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE expiry_lightning_external_success (
	id BIGINT PRIMARY KEY REFERENCES expiry_lightning_dispatch(id),
	created_index BIGINT NOT NULL CHECK (created_index >= 0),
	updated_index BIGINT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- A failed attempt may name a subscription somebody else already funded.
-- Only the transaction which actually promotes Created -> Accepted writes this.
CREATE TABLE expiry_lightning_internal_funding (
	id BIGINT PRIMARY KEY REFERENCES lightning_payment_attempt(id),
	subscription_id BIGINT NOT NULL UNIQUE REFERENCES lightning_htlc_subscription(id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
