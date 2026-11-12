-- Delivery retries survive process restarts and wait without polling the DB.
ALTER TABLE mail_requests ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mail_requests ADD COLUMN available INTEGER NOT NULL DEFAULT 0;
CREATE INDEX mail_requests_available ON mail_requests(available);

-- Unkept 1-to-1 calls are temporary. Existing direct links get a day's grace.
ALTER TABLE rooms ADD COLUMN expires INTEGER;
UPDATE rooms SET expires=unixepoch()+86400 WHERE kind='direct' AND closed=0;
CREATE INDEX rooms_expiry ON rooms(expires) WHERE expires IS NOT NULL;
