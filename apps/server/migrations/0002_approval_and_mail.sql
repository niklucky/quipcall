-- Existing pending desktop challenges cannot be approved without a device code.
ALTER TABLE desktop_logins ADD COLUMN code_hash TEXT;
ALTER TABLE desktop_logins ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;

-- Queue all valid email requests identically, before checking account existence.
-- Only hashes of invitation credentials are persisted here.
CREATE TABLE mail_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL,
    requester TEXT NOT NULL,
    invite TEXT,
    created INTEGER NOT NULL
);
