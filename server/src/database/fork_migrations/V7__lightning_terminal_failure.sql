ALTER TABLE expiry_lightning_dispatch
	ADD COLUMN label TEXT NOT NULL DEFAULT gen_random_uuid()::TEXT UNIQUE,
	ADD COLUMN checked_at TIMESTAMPTZ;

-- This records a particular CLN dispatch, not a coarse local Failed status.
-- Its randomly unique label must occur in CLN's actual sendpay history, and
-- every same-hash group must be Failed with no live xpay object or preimage.
CREATE TABLE expiry_lightning_terminal_failure (
	id BIGINT PRIMARY KEY REFERENCES expiry_lightning_dispatch(id),
	created_index BIGINT NOT NULL CHECK (created_index > 0),
	updated_index BIGINT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
