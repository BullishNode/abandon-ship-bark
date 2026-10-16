-- Exact original urgent packages enter the existing nursery only after their
-- independent same-ledger u1/u2 and SQL input intent. Relay stays owner-gated.
ALTER TYPE nursery_tx_kind ADD VALUE 'native-urgent';
