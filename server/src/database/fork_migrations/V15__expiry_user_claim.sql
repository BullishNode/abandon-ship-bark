-- Public, already-authorized user timeout transactions outlive chain rollback.
-- One body is retained per txid, even when it covers several original sources.
CREATE TABLE expiry_user_claim_tx (
	txid TEXT PRIMARY KEY,
	raw_tx BYTEA NOT NULL,
	confirmed_height BIGINT,
	confirmed_hash TEXT,
	last_replay_attempt_at TIMESTAMPTZ,
	CHECK ((confirmed_height IS NULL) = (confirmed_hash IS NULL)),
	CHECK (confirmed_height IS NULL OR confirmed_height >= 0)
);
CREATE TABLE expiry_user_claim_source (
	source_vtxo_id TEXT NOT NULL REFERENCES vtxo(vtxo_id),
	claim_txid TEXT NOT NULL REFERENCES expiry_user_claim_tx(txid),
	PRIMARY KEY (source_vtxo_id, claim_txid)
);
CREATE INDEX expiry_user_claim_pending_ix ON expiry_user_claim_tx
	(last_replay_attempt_at NULLS FIRST, txid) WHERE confirmed_height IS NULL;
CREATE TRIGGER expiry_user_claim_body_ledger AFTER INSERT OR DELETE OR UPDATE OF
	raw_tx, confirmed_height, confirmed_hash ON expiry_user_claim_tx
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
CREATE TRIGGER expiry_user_claim_source_ledger AFTER INSERT OR DELETE ON expiry_user_claim_source
	FOR EACH ROW EXECUTE FUNCTION expiry_record_ledger_commit();
