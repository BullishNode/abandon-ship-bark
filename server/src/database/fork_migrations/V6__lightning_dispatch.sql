-- One-shot execution claims, not payment or refund outcomes. A committed
-- claim can outlive a crashed dispatcher and never implies remote failure.
-- Remote settlement must still be established independently.
CREATE TABLE expiry_lightning_dispatch (
	id BIGINT PRIMARY KEY REFERENCES lightning_payment_attempt(id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
