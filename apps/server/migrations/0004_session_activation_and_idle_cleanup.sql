-- An approved desktop credential is provisional until used after Keychain save.
ALTER TABLE sessions ADD COLUMN pending INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN replaces TEXT;

-- Expired Quick calls close only after observed inactivity. Failed cleanups
-- are rescheduled so one room cannot monopolize a bounded batch.
ALTER TABLE rooms ADD COLUMN empty_since INTEGER;
ALTER TABLE rooms ADD COLUMN cleanup_after INTEGER DEFAULT 0;
