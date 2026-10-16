-- Append-only confirmed winner provenance; no allowance, refund or history deletion.
CREATE TABLE native_funding_fee_resolution (
 winner_index BIGINT PRIMARY KEY CHECK(winner_index>0),
 proof JSONB NOT NULL
);
