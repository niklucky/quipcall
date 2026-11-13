-- Orphan-guest cleanup checks each foreign-key reference. Keep these lookups
-- indexed so a large live guest population cannot turn housekeeping quadratic.
CREATE INDEX sessions_principal ON sessions(principal);
CREATE INDEX desktop_logins_principal ON desktop_logins(principal);
CREATE INDEX rooms_owner ON rooms(owner);
CREATE INDEX rooms_guest ON rooms(guest);
CREATE INDEX room_invites_used_by ON room_invites(used_by);
CREATE INDEX identities_principal ON identities(principal);
CREATE INDEX account_invites_created_by ON account_invites(created_by);
