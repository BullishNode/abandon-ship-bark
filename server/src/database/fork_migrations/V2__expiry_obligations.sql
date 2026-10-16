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
