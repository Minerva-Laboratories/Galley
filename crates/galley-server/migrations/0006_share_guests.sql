-- The share link a guest joined through, so revoking the link removes them. NULL for
-- invited members, signed-in users who joined by link, and guests from before this migration.
ALTER TABLE members ADD COLUMN via_share TEXT;
CREATE INDEX members_via_share ON members(via_share) WHERE via_share IS NOT NULL;
