-- The scheduler first selects a bounded set of accounts, then compares all
-- pending keys in each account's next interval without scanning other accounts.
CREATE INDEX expiry_scan_record_prefix_ix ON expiry_scan (mailbox_pk, scanned_up_to);
