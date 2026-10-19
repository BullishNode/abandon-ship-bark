-- Fork schema for fresh databases; upstream migrations keep their own history.

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

CREATE INDEX expiry_vtxo_anchor_point_ix ON vtxo (anchor_point, vtxo_id);

CREATE TABLE expiry_entitlement (
	id BIGSERIAL PRIMARY KEY,
	vtxo_id TEXT NOT NULL UNIQUE REFERENCES vtxo(vtxo_id),
	gross_sat BIGINT NOT NULL CHECK (gross_sat > 0),
	irreversible_facts JSONB NOT NULL DEFAULT '{}'::jsonb,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (vtxo_id, id)
);

CREATE TABLE expiry_decided_coin (
	vtxo_id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	entitlement_id BIGINT NOT NULL UNIQUE,
	FOREIGN KEY (vtxo_id, entitlement_id) REFERENCES expiry_entitlement(vtxo_id, id)
);

CREATE TABLE expiry_cancelled_participation (
	-- Upstream may delete a superseded pending participation. Keep its cancellation tombstone.
	participation_id BIGINT PRIMARY KEY,
	unlock_hash TEXT UNIQUE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

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
	service_ticket BIGSERIAL NOT NULL,
	PRIMARY KEY (user_pubkey, mailbox_pk)
);
CREATE INDEX expiry_scan_prefix_ix ON expiry_scan (scanned_up_to, mailbox_pk);

-- Chain-derived decisions are invalid until both processes acknowledge their
-- successful sync cycles and the rebuild verifies the current database tips.
CREATE TABLE expiry_chain_view (
	id SMALLINT PRIMARY KEY CHECK (id = 1),
	generation BIGINT NOT NULL DEFAULT 0 CHECK (generation >= 0),
	invalid BOOLEAN NOT NULL DEFAULT TRUE,
	invalid_from BIGINT NOT NULL DEFAULT 0,
	rebuilt_captaind_height BIGINT,
	rebuilt_captaind_hash TEXT,
	rebuilt_watchmand_height BIGINT,
	rebuilt_watchmand_hash TEXT,
	rebuilt_ledger_count BIGINT,
	rebuilt_ledger_revision BIGINT
);
INSERT INTO expiry_chain_view (id) VALUES (1);

CREATE TABLE expiry_sync_ack (
	process TEXT PRIMARY KEY CHECK (process IN ('captaind', 'watchmand')),
	generation BIGINT NOT NULL,
	height BIGINT NOT NULL,
	hash TEXT NOT NULL
);

-- Financial decisions are immutable; active links and chain results are rebuilt.
CREATE TABLE expiry_allocation (
	id BIGSERIAL PRIMARY KEY,
	expiry_height INTEGER NOT NULL CHECK (expiry_height >= 0),
	generation BIGINT NOT NULL CHECK (generation >= 0),
	status TEXT NOT NULL CHECK (status IN ('pending', 'confirmed', 'conflicted')),
	sweep_txid TEXT UNIQUE,
	sweep_tx BYTEA,
	broadcast_at TIMESTAMPTZ,
	confirmed_height BIGINT,
	confirmed_hash TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CHECK ((sweep_txid IS NULL) = (sweep_tx IS NULL)),
	CHECK ((confirmed_height IS NULL) = (confirmed_hash IS NULL))
);

CREATE TABLE expiry_allocation_input (
	allocation_id BIGINT NOT NULL REFERENCES expiry_allocation(id),
	vtxo_id TEXT NOT NULL REFERENCES vtxo(vtxo_id),
	PRIMARY KEY (allocation_id, vtxo_id)
);

CREATE TABLE expiry_allocation_coin (
	allocation_id BIGINT NOT NULL REFERENCES expiry_allocation(id),
	entitlement_id BIGINT NOT NULL REFERENCES expiry_entitlement(id),
	owner_mailbox TEXT REFERENCES fallback_record(mailbox_pk),
	user_pubkey TEXT,
	spk TEXT,
	disposition TEXT NOT NULL CHECK (disposition IN ('customer', 'operator')),
	gross_sat BIGINT NOT NULL CHECK (gross_sat > 0),
	fee_sat BIGINT NOT NULL CHECK (fee_sat >= 0),
	payout_sat BIGINT NOT NULL CHECK (payout_sat >= 0),
	forfeited_sat BIGINT NOT NULL CHECK (forfeited_sat >= 0),
	active BOOLEAN NOT NULL DEFAULT TRUE,
	evidence JSONB NOT NULL,
	PRIMARY KEY (allocation_id, entitlement_id),
	CHECK ((disposition = 'customer' AND owner_mailbox IS NOT NULL AND user_pubkey IS NOT NULL
		AND spk IS NOT NULL AND gross_sat = fee_sat + payout_sat + forfeited_sat)
		OR (disposition = 'operator' AND fee_sat = 0 AND payout_sat = 0 AND forfeited_sat = 0))
);
CREATE UNIQUE INDEX expiry_one_active_allocation_per_coin ON expiry_allocation_coin(entitlement_id) WHERE active;

CREATE TABLE expiry_allocation_payout (
	allocation_id BIGINT NOT NULL REFERENCES expiry_allocation(id),
	output_index INTEGER NOT NULL CHECK (output_index >= 0),
	spk TEXT NOT NULL,
	amount_sat BIGINT NOT NULL CHECK (amount_sat > 0),
	PRIMARY KEY (allocation_id, output_index),
	UNIQUE (allocation_id, spk)
);

-- The full signed parent is stored once. Fee children may be replaced and each
-- exact version remains available for confirmation and actual subsidy accounting.
CREATE TABLE expiry_fee_child (
	id BIGSERIAL PRIMARY KEY,
	allocation_id BIGINT NOT NULL REFERENCES expiry_allocation(id),
	txid TEXT NOT NULL UNIQUE,
	raw_tx BYTEA NOT NULL,
	fee_sat BIGINT NOT NULL CHECK (fee_sat >= 0),
	-- All parent value retained by the operator, including drain and P2A.
	operator_remainder_sat BIGINT NOT NULL CHECK (operator_remainder_sat >= 0),
	confirmed_height BIGINT,
	confirmed_hash TEXT,
	funding_kind TEXT NOT NULL DEFAULT 'watchman',
	operator_net_contribution_sat BIGINT NOT NULL,
	observed_prevouts BYTEA,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT expiry_fee_child_funding_scope CHECK (
		(funding_kind='watchman' AND observed_prevouts IS NULL
			AND operator_net_contribution_sat=fee_sat)
		OR (funding_kind='observed_fee_only' AND observed_prevouts IS NOT NULL
			AND operator_net_contribution_sat>=0
			AND operator_net_contribution_sat<=operator_remainder_sat)
	),
	CHECK ((confirmed_height IS NULL) = (confirmed_hash IS NULL))
);

-- Splitting is a chain action, not another payout decision or customer fee.
CREATE TABLE expiry_split_request (
	vtxo_id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	progress_txid TEXT NOT NULL,
	generation BIGINT NOT NULL CHECK (generation >= 0),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE expiry_terminal_claim (
	entitlement_id BIGINT PRIMARY KEY REFERENCES expiry_entitlement(id),
	txid TEXT NOT NULL,
	height BIGINT NOT NULL CHECK (height >= 0),
	hash TEXT NOT NULL
);

-- Append-only, one visible row per financial modifying transaction. Sequence
-- MAX alone misses a late commit allocated before a later committed revision;
-- the visible row count changes for every commit regardless of that ordering.
CREATE TABLE expiry_ledger_commit (
	transaction_id XID8 PRIMARY KEY,
	revision BIGSERIAL NOT NULL UNIQUE
);

CREATE FUNCTION expiry_record_ledger_commit() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
	IF TG_OP = 'UPDATE' AND OLD IS NOT DISTINCT FROM NEW THEN
		RETURN NULL;
	END IF;
	INSERT INTO expiry_ledger_commit(transaction_id) VALUES(pg_current_xact_id())
		ON CONFLICT(transaction_id) DO NOTHING;
	RETURN NULL;
END;
$$;

CREATE TRIGGER expiry_entitlement_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_entitlement
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_decided_coin_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_decided_coin
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_allocation_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_allocation
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_allocation_input_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_allocation_input
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_allocation_coin_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_allocation_coin
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_allocation_payout_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_allocation_payout
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_fee_child_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_fee_child
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_terminal_claim_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_terminal_claim
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_chain_view_revision AFTER INSERT OR UPDATE OR DELETE ON expiry_chain_view
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();

-- One-shot execution claims, not payment or refund outcomes. A committed
-- claim can outlive a crashed dispatcher and never implies remote failure.
-- Remote settlement must still be established independently.
CREATE TABLE expiry_lightning_dispatch (
	id BIGINT PRIMARY KEY REFERENCES lightning_payment_attempt(id),
	label TEXT NOT NULL DEFAULT gen_random_uuid()::TEXT UNIQUE,
	checked_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- This records a particular CLN dispatch, not a coarse local Failed status.
-- Its randomly unique label must occur in CLN's actual sendpay history, and
-- every same-hash group must be Failed with no live xpay object or preimage.
CREATE TABLE expiry_lightning_terminal_failure (
	id BIGINT PRIMARY KEY REFERENCES expiry_lightning_dispatch(id),
	created_index BIGINT CHECK (created_index > 0),
	updated_index BIGINT,
	jsonrpc_error_code INTEGER,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT expiry_lightning_failure_provenance CHECK (
		(created_index IS NOT NULL AND jsonrpc_error_code IS NULL)
		OR (created_index IS NULL AND jsonrpc_error_code IS NOT NULL AND jsonrpc_error_code IN (205,207))
	)
);

-- Receipt of an explicit upstream no-attempt error, distinct from a transport
-- failure. Only source-reviewed CLN26.06.6 errors are admitted here.
CREATE TABLE expiry_lightning_rpc_rejection (
	id BIGINT PRIMARY KEY REFERENCES expiry_lightning_dispatch(id),
	code INTEGER NOT NULL CHECK (code IN (205,207)),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- These rows describe execution of an actual attempt, never merely knowledge
-- of an invoice preimage. An attempt without a queue marker stays unresolved.
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

-- The scheduler selects bounded keys and accounts, then compares each selected
-- key in its account's next interval without scanning other accounts.
CREATE INDEX expiry_scan_record_prefix_ix ON expiry_scan (mailbox_pk, scanned_up_to);

-- A validated receive destination survives collection and a missing claim caller.
-- The output row need not exist until the later materialization transaction.
CREATE TABLE expiry_lightning_receive_claim (
	source_vtxo_id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	subscription_id BIGINT NOT NULL REFERENCES lightning_htlc_subscription(id),
	payment_hash TEXT NOT NULL,
	output_vtxo_id TEXT NOT NULL UNIQUE,
	output_vtxo BYTEA NOT NULL,
	materialized BOOLEAN NOT NULL DEFAULT FALSE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- An admitted claim can be retried without the original provider. The request
-- includes its public signing nonces, attestation and payment preimage, never
-- the user's signing secret.
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

CREATE INDEX expiry_scan_service_ticket_ix
	ON expiry_scan (service_ticket, user_pubkey, mailbox_pk)
	WHERE scanned_up_to < 2147483648;
CREATE INDEX expiry_scan_account_service_ticket_ix
	ON expiry_scan (mailbox_pk, scanned_up_to, service_ticket, user_pubkey)
	WHERE scanned_up_to < 2147483648;

-- Confirmed user claims remain available for rebroadcast after a shallow reorg.
CREATE TABLE user_exit_claim (
	id TEXT PRIMARY KEY,
	raw_tx BYTEA NOT NULL,
	confirmed_height INT4
);
