-- Required obligation and payment-evidence tables on a fresh database.
CREATE TABLE expiry_settlement (
	id TEXT PRIMARY KEY REFERENCES vtxo(vtxo_id),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	txid TEXT NOT NULL REFERENCES nursery_tx(txid),
	fee_sat BIGINT NOT NULL CHECK (fee_sat >= 0),
	spk BYTEA NOT NULL
);
CREATE INDEX expiry_settlement_txid ON expiry_settlement (txid);
CREATE INDEX expiry_payout_candidates ON vtxo (expiry, vtxo_id)
	WHERE policy_type='pubkey' AND spend_state IN ('spendable','unclaimed')
	AND confirmed_height IS NULL;
-- Keep the exact exchange attribution when its live participation is removed.
CREATE TABLE expiry_cancelled_participation (
	id TEXT PRIMARY KEY,
	round_id BIGINT NOT NULL REFERENCES round(id),
	input_ids TEXT[] NOT NULL,
	output_ids TEXT[] NOT NULL,
	exited_ids TEXT[] NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The funding outpoint admits exactly one entitlement. Retain the row after
-- registration or payout: it serializes both paths and lost-COMMIT recovery.
-- The unsigned user coin is deliberately absent from vtxo until registration
-- or settlement, so generic transaction registration cannot activate it.
CREATE TABLE pending_board (
	id TEXT PRIMARY KEY,
	vtxo_id TEXT NOT NULL UNIQUE,
	vtxo BYTEA NOT NULL,
	expiry INTEGER NOT NULL
);
CREATE INDEX pending_board_expiry ON pending_board (expiry, vtxo_id);

-- The xpay monitor fails a payment attempt that never reached the node only
-- after its invoice expired: the node refuses an expired invoice, so a
-- delayed request can no longer start. The deadline is persisted before dispatch.
ALTER TABLE lightning_payment_attempt ADD COLUMN invoice_expires_at TIMESTAMPTZ;

-- Padding has no customer key holder. Keep its value in frontier totals,
-- but classify only its exact pubkey policy as operator-owned. Do not filter
-- all unclaimed outputs or all dust amounts: those can belong to customers.
CREATE OR REPLACE VIEW v_frontier_ownership_totals AS
WITH RECURSIVE descendants(vtxo_id, vtxo_txid, oor_spent_txid, spend_state, amount, policy) AS (
    SELECT f.vtxo_id, f.vtxo_txid, f.oor_spent_txid, f.spend_state, f.amount, v.policy
    FROM v_frontier_vtxos f JOIN vtxo v ON v.vtxo_id=f.vtxo_id
    WHERE NOT f.conflicted
    UNION
    SELECT c.vtxo_id, c.vtxo_txid, c.oor_spent_txid, c.spend_state, c.amount, c.policy
    FROM descendants p
    CROSS JOIN LATERAL (
        SELECT vtxo.vtxo_id, vtxo.vtxo_txid, vtxo.oor_spent_txid,
               vtxo.spend_state, vtxo.amount, vtxo.onchain_spent_txid, vtxo.policy
        FROM vtxo
        WHERE vtxo.vtxo_txid = p.oor_spent_txid
        OFFSET 0
    ) AS c
    WHERE p.oor_spent_txid IS NOT NULL
      AND c.onchain_spent_txid IS NULL
      AND c.spend_state <> 'offboard-connector'
      AND NOT EXISTS (
          SELECT 1 FROM v_conflicted_txids ct WHERE ct.txid = c.vtxo_txid
      )
)
SELECT CASE spend_state::text
         WHEN 'spendable'           THEN 'theirs'
         WHEN 'unregistered'        THEN 'theirs'
         WHEN 'unclaimed'           THEN CASE WHEN policy=decode('0002dfa52f6690299d2d6a08323083e290597b56fee125063e5f4e2957731639c42c', 'hex')
                                          THEN 'ours' ELSE 'theirs' END
         WHEN 'htlc-recv-unclaimed' THEN 'pending'
         WHEN 'spent'               THEN 'ours'
         WHEN 'pool'                THEN 'ours'
         WHEN 'round-forfeit'       THEN 'ours'
         WHEN 'offboard-forfeit'    THEN 'ours'
         WHEN 'ln-spent'            THEN 'ours'
         ELSE 'unknown'
       END AS ownership,
       COUNT(*)::bigint AS n,
       COALESCE(SUM(amount), 0)::bigint AS volume
FROM descendants
WHERE oor_spent_txid IS NULL
   OR EXISTS (SELECT 1 FROM v_conflicted_txids ct WHERE ct.txid = oor_spent_txid)
GROUP BY 1;
