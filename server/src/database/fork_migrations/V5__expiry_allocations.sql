-- Financial decisions are immutable; active links and chain results are rebuilt.
CREATE TABLE expiry_allocation (
	id BIGSERIAL PRIMARY KEY,
	expiry_height INTEGER NOT NULL CHECK (expiry_height >= 0),
	generation BIGINT NOT NULL CHECK (generation >= 0),
	status TEXT NOT NULL CHECK (status IN ('pending', 'split', 'confirmed', 'conflicted')),
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
	paying_txid TEXT,
	paying_height BIGINT,
	paying_hash TEXT,
	PRIMARY KEY (allocation_id, entitlement_id),
	CHECK ((disposition = 'customer' AND owner_mailbox IS NOT NULL AND user_pubkey IS NOT NULL
		AND spk IS NOT NULL AND gross_sat = fee_sat + payout_sat + forfeited_sat)
		OR (disposition = 'operator' AND fee_sat = 0 AND payout_sat = 0 AND forfeited_sat = 0)),
	CHECK ((paying_txid IS NULL) = (paying_height IS NULL)),
	CHECK ((paying_height IS NULL) = (paying_hash IS NULL))
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
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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

ALTER TABLE expiry_chain_view
	ADD COLUMN rebuilt_ledger_count BIGINT,
	ADD COLUMN rebuilt_ledger_revision BIGINT;
