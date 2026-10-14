-- The first financial worker selects a route before it can consume backing.
-- No in-place route conversion is supported by this fresh-state release.
CREATE TABLE expiry_route (
	id SMALLINT PRIMARY KEY CHECK (id = 1),
	direct BOOLEAN NOT NULL
);
