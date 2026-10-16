-- Only actual matches are cached. Exhaustive coverage belongs to each pending
-- key and registered account, not to an account-wide lifetime cursor.
CREATE TABLE expiry_pending_key (
	user_pubkey TEXT PRIMARY KEY,
	mailbox_pk TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE expiry_owner_key (
	user_pubkey TEXT PRIMARY KEY,
	mailbox_pk TEXT NOT NULL REFERENCES fallback_record(mailbox_pk),
	child_index BIGINT NOT NULL CHECK (child_index >= 0 AND child_index < 2147483648),
	UNIQUE (mailbox_pk, child_index)
);

CREATE TABLE expiry_scan (
	user_pubkey TEXT NOT NULL REFERENCES expiry_pending_key(user_pubkey) ON DELETE CASCADE,
	mailbox_pk TEXT NOT NULL REFERENCES fallback_record(mailbox_pk),
	scanned_up_to BIGINT NOT NULL DEFAULT 0 CHECK (scanned_up_to >= 0 AND scanned_up_to <= 2147483648),
	PRIMARY KEY (user_pubkey, mailbox_pk)
);
CREATE INDEX expiry_scan_prefix_ix ON expiry_scan (scanned_up_to, mailbox_pk);
