-- Apply after expiry-settlement.sql, before starting this fork.
-- These tables are required for coin creation even when payouts are disabled.
CREATE TABLE IF NOT EXISTS fallback_record (
	mailbox_pk BYTEA PRIMARY KEY,
	spk BYTEA NOT NULL,
	seq BIGINT NOT NULL,
	sig BYTEA NOT NULL
);
CREATE TABLE IF NOT EXISTS key_link (
	user_pubkey BYTEA PRIMARY KEY,
	mailbox_pk BYTEA NOT NULL,
	sig BYTEA NOT NULL
);
ALTER TABLE expiry_settlement ADD COLUMN IF NOT EXISTS spk BYTEA;

-- The funding outpoint admits exactly one entitlement. Retain the row after
-- registration or payout: it serializes both paths and lost-COMMIT recovery.
-- The unsigned user coin is deliberately absent from vtxo until registration
-- or settlement, so generic transaction registration cannot activate it.
CREATE TABLE IF NOT EXISTS pending_board (
	id TEXT PRIMARY KEY,
	vtxo_id TEXT NOT NULL UNIQUE,
	vtxo BYTEA NOT NULL,
	expiry INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pending_board_expiry ON pending_board (expiry, vtxo_id);

-- The xpay monitor fails a payment attempt that never reached the node only
-- after its invoice expired: the node refuses an expired invoice, so a
-- delayed request can no longer start. NULL for older attempts.
ALTER TABLE lightning_payment_attempt ADD COLUMN IF NOT EXISTS invoice_expires_at TIMESTAMPTZ;

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
