-- Bind one independently retained original opening certificate; no allowance credit.
ALTER TABLE fee_allowance_policy ADD COLUMN opening_digest TEXT
 CHECK (opening_digest IS NULL OR opening_digest ~ '^[0-9a-f]{64}$');
