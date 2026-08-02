-- This migration contains destructive operations and cannot be
-- auto-reversed. Affected ops:
--   - DropColumn applications.environment
--   - DropColumn integrations.environment
--
-- Write a real reverse migration before running `down`, or accept
-- that this migration is forward-only.
DO $$ BEGIN RAISE EXCEPTION 'destructive migration; reversal must be hand-written'; END $$;
