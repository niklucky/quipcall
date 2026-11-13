CREATE TABLE principals (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('user', 'guest')),
    name TEXT NOT NULL,
    email TEXT UNIQUE,
    admin INTEGER NOT NULL DEFAULT 0
);
-- Providers link to an internal principal, never to an email as the account ID.
CREATE TABLE identities (
    provider TEXT NOT NULL,
    subject TEXT NOT NULL,
    principal TEXT NOT NULL REFERENCES principals(id),
    PRIMARY KEY(provider, subject)
);
CREATE TABLE sessions (
    hash TEXT PRIMARY KEY,
    principal TEXT NOT NULL REFERENCES principals(id),
    expires INTEGER NOT NULL
);
CREATE TABLE account_invites (
    hash TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    expires INTEGER NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id)
);
CREATE TABLE magic_links (
    hash TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    invite TEXT,
    expires INTEGER NOT NULL
);
CREATE TABLE desktop_logins (
    id TEXT PRIMARY KEY,
    secret_hash TEXT NOT NULL,
    principal TEXT REFERENCES principals(id),
    expires INTEGER NOT NULL
);
CREATE TABLE rooms (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('direct', 'public', 'private')),
    owner TEXT NOT NULL REFERENCES principals(id),
    guest TEXT REFERENCES principals(id),
    closed INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE memberships (
    room TEXT NOT NULL REFERENCES rooms(id),
    principal TEXT NOT NULL REFERENCES principals(id),
    revoked INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(room, principal)
);
CREATE TABLE room_invites (
    hash TEXT PRIMARY KEY,
    room TEXT NOT NULL REFERENCES rooms(id),
    kind TEXT NOT NULL CHECK(kind IN ('member', 'guest')),
    email TEXT,
    expires INTEGER NOT NULL,
    used_by TEXT REFERENCES principals(id)
);
CREATE TABLE rate_limits (
    key TEXT PRIMARY KEY,
    window INTEGER NOT NULL,
    count INTEGER NOT NULL,
    expires INTEGER NOT NULL
);
CREATE INDEX sessions_expiry ON sessions(expires);
CREATE INDEX memberships_principal ON memberships(principal);
