-- Recreate `hand_assignments` as a backward-compatible view over the
-- consolidated `assignments` table.
--
-- Migration 015 consolidated `hand_assignments` into `assignments` and
-- dropped the original table. Its comment promised a backward-compatible
-- view but the DDL was never emitted, leaving the `assignments` epic test
-- suite (and any legacy read path) without the view. This migration is
-- additive and restores the view in the shape the original table exposed.
--
-- The view is session-scoped: rows with a NULL `session_id` in
-- `assignments` represent non-hand claims (e.g. actor-only ownership) and
-- are omitted so downstream `FromRow` decoders can keep `session_id` as
-- `NOT NULL`.

CREATE VIEW IF NOT EXISTS hand_assignments AS
SELECT
    id,
    session_id,
    spark_id,
    status,
    role,
    assigned_at,
    last_heartbeat_at,
    lease_expires_at,
    completed_at,
    handoff_to,
    handoff_reason
FROM assignments
WHERE session_id IS NOT NULL;
