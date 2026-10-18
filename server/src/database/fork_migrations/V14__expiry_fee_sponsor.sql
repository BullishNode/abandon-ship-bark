-- Paired with strict observed-child recognition and explicit own-child writers.
-- Known operator scope is this watchman wallet plus the committed parent's
-- non-customer outputs, not every wallet an operator might own elsewhere.
ALTER TABLE expiry_fee_child
	ADD COLUMN funding_kind TEXT NOT NULL DEFAULT 'watchman',
	ADD COLUMN operator_net_contribution_sat BIGINT,
	ADD COLUMN observed_prevouts BYTEA;

-- Existing children were constructed only from this wallet and their parent
-- anchor, with change returned to the same wallet. Their contribution is fee.
UPDATE expiry_fee_child SET operator_net_contribution_sat=fee_sat;
ALTER TABLE expiry_fee_child ALTER COLUMN operator_net_contribution_sat SET NOT NULL;
ALTER TABLE expiry_fee_child ADD CONSTRAINT expiry_fee_child_funding_scope CHECK (
	(funding_kind='watchman' AND observed_prevouts IS NULL
		AND operator_net_contribution_sat=fee_sat)
	OR (funding_kind='observed_fee_only' AND observed_prevouts IS NOT NULL
		AND operator_net_contribution_sat>=0
		AND operator_net_contribution_sat<=operator_remainder_sat)
);
