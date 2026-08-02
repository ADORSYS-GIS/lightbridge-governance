-- WARNING: this migration contains blocking operations.
-- A required column was added without a default. The migration
-- will fail on a non-empty table unless an `up.pre.sql` backfills
-- the affected columns before this statement runs.

ALTER TABLE integrations ADD COLUMN credential_prefix TEXT NOT NULL;

ALTER TABLE integrations ADD COLUMN last_used_at TIMESTAMPTZ;

ALTER TABLE integrations ADD COLUMN revoked_at TIMESTAMPTZ;

