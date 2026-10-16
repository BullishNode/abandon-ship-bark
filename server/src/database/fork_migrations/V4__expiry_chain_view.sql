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
	rebuilt_watchmand_hash TEXT
);
INSERT INTO expiry_chain_view (id) VALUES (1);

CREATE TABLE expiry_sync_ack (
	process TEXT PRIMARY KEY CHECK (process IN ('captaind', 'watchmand')),
	generation BIGINT NOT NULL,
	height BIGINT NOT NULL,
	hash TEXT NOT NULL
);
