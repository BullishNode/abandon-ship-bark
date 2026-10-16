-- Fork migrations deliberately have a separate history from upstream migrations.
CREATE TABLE fallback_record (
	id BIGSERIAL PRIMARY KEY,
	mailbox_pk TEXT NOT NULL UNIQUE,
	spk TEXT NOT NULL,
	account_xpub TEXT NOT NULL UNIQUE,
	-- BIP32 metadata does not affect child keys. Deduplicate the account's
	-- derivation identity too, so aliases cannot assign different payees.
	account_pubkey TEXT NOT NULL,
	account_chain_code TEXT NOT NULL,
	UNIQUE (account_pubkey, account_chain_code),
	mailbox_sig TEXT NOT NULL,
	account_sig TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
