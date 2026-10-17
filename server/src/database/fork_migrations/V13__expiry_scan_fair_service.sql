-- Scheduling metadata only: preserve every existing contiguous coverage prefix.
-- The volatile default assigns existing rows finite tickets before later arrivals.
-- Successful cursor CAS updates alone move work to the back of this queue.
CREATE SEQUENCE expiry_scan_service_ticket_seq;
ALTER TABLE expiry_scan ADD COLUMN service_ticket BIGINT NOT NULL
	DEFAULT nextval('expiry_scan_service_ticket_seq');
ALTER SEQUENCE expiry_scan_service_ticket_seq OWNED BY expiry_scan.service_ticket;

CREATE INDEX expiry_scan_service_ticket_ix
	ON expiry_scan (service_ticket, user_pubkey, mailbox_pk)
	WHERE scanned_up_to < 2147483648;
CREATE INDEX expiry_scan_account_service_ticket_ix
	ON expiry_scan (mailbox_pk, scanned_up_to, service_ticket, user_pubkey)
	WHERE scanned_up_to < 2147483648;
