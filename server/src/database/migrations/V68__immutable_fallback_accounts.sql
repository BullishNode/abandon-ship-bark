-- Fresh installation of immutable expiry payout identities and admitted children.
CREATE TABLE fallback_record (
	mailbox_pk BYTEA PRIMARY KEY CHECK (octet_length(mailbox_pk)=33),
	spk BYTEA NOT NULL CHECK (octet_length(spk) BETWEEN 22 AND 42),
	account_xpub BYTEA NOT NULL CHECK (octet_length(account_xpub)=78),
	mailbox_sig BYTEA NOT NULL CHECK (octet_length(mailbox_sig)=64),
	account_sig BYTEA NOT NULL CHECK (octet_length(account_sig)=64)
);
CREATE TABLE key_link (
	user_pubkey BYTEA PRIMARY KEY CHECK (octet_length(user_pubkey)=33),
	mailbox_pk BYTEA NOT NULL REFERENCES fallback_record(mailbox_pk),
	child_index INTEGER NOT NULL CHECK (child_index >= 0),
	UNIQUE (mailbox_pk, child_index)
);
