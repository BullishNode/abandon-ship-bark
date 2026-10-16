-- Exact immutable epoch and conservative per-signed-candidate fee reservations.
CREATE TABLE fee_allowance_policy (
 id text PRIMARY KEY CHECK(id = 'active'),
 policy_bytes bytea NOT NULL,
 history_identity text NOT NULL
);
CREATE TABLE fee_allowance_candidate (
 id text PRIMARY KEY,
 epoch_id text NOT NULL,
 candidate_bytes bytea NOT NULL,
 urgent boolean NOT NULL,
 fee_sat bigint NOT NULL CHECK(fee_sat >= 0 AND fee_sat <= 2100000000000000)
);
-- No confirmations, retries, abandoned-state or clock-based credits.
-- Exact signed candidate identity includes witness. No UPDATE/DELETE API.
