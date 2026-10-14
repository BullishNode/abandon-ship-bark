-- Fresh direct-settlement journal. A frozen entitlement is not a payment.
-- Keep allocation independent of carrier versions: either an original or a
-- replacement signed carrier can win, without changing its authorized amounts.
CREATE TABLE expiry_allocation (
	id TEXT PRIMARY KEY,
	freeze_set TEXT NOT NULL,
	owner_pubkey BYTEA REFERENCES key_link(user_pubkey),
	spk BYTEA,
	disposition TEXT NOT NULL CHECK (disposition IN ('pay', 'forfeit', 'operator')),
	gross_sat BIGINT NOT NULL CHECK (gross_sat > 0 AND gross_sat <= 2100000000000000),
	fee_sat BIGINT NOT NULL CHECK (fee_sat BETWEEN 0 AND 1000),
	payout_sat BIGINT NOT NULL CHECK (payout_sat >= 0),
	forfeited_sat BIGINT NOT NULL CHECK (forfeited_sat >= 0),
	source TEXT NOT NULL,
	source_evidence JSONB NOT NULL CHECK (jsonb_typeof(source_evidence) = 'object'),
	retirement_ids TEXT[] NOT NULL CHECK (cardinality(retirement_ids) > 0),
	frozen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CHECK (
		(disposition = 'pay' AND owner_pubkey IS NOT NULL AND spk IS NOT NULL
			AND forfeited_sat = 0 AND gross_sat = fee_sat + payout_sat)
		OR (disposition = 'forfeit' AND owner_pubkey IS NOT NULL AND spk IS NOT NULL
			AND fee_sat = 0 AND payout_sat = 0 AND forfeited_sat = gross_sat)
		OR (disposition = 'operator' AND owner_pubkey IS NULL AND spk IS NULL
			AND fee_sat = 0 AND payout_sat = 0 AND forfeited_sat = 0)
	)
);
CREATE INDEX expiry_allocation_set ON expiry_allocation (freeze_set, id);

-- Signed bytes are durable before any network submission. No insertion here
-- automatically broadcasts; watchmand must retain ownership/retirement guards
-- and its independent recovery witness before submitting the actual carrier.
CREATE TABLE expiry_carrier (
	txid TEXT PRIMARY KEY,
	signed_tx BYTEA NOT NULL CHECK (octet_length(signed_tx) > 0),
	backing_ids TEXT[] NOT NULL CHECK (cardinality(backing_ids) > 0),
	fee_inputs JSONB NOT NULL CHECK (jsonb_typeof(fee_inputs) = 'array'),
	miner_fee_sat BIGINT NOT NULL CHECK (miner_fee_sat >= 0),
	funding_subsidy_sat BIGINT NOT NULL CHECK (funding_subsidy_sat >= 0),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	confirmed_height INTEGER CHECK (confirmed_height >= 0),
	confirmed_block TEXT,
	CHECK ((confirmed_height IS NULL) = (confirmed_block IS NULL))
);

-- Explicit vout attribution survives shared scripts and multiple carrier
-- versions. A forfeit/operator recovery has no customer vout. The writer
-- checks the complete group, exact outputs and closure in the same transaction.
CREATE TABLE expiry_carrier_allocation (
	txid TEXT NOT NULL REFERENCES expiry_carrier(txid),
	allocation_id TEXT NOT NULL REFERENCES expiry_allocation(id),
	vout INTEGER CHECK (vout >= 0),
	PRIMARY KEY (txid, allocation_id)
);
CREATE INDEX expiry_carrier_allocation_owner ON expiry_carrier_allocation (allocation_id, txid);
