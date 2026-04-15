-- Rebuild the hand_assignments backward-compatible view to include the
-- assignment_phase columns added in migration 014. The previous view
-- (created in 013) only projected the original columns, causing a schema
-- mismatch when transition.rs tried to SELECT/UPDATE phase columns through it.

DROP VIEW IF EXISTS hand_assignments;

CREATE VIEW hand_assignments AS
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
    handoff_reason,
    assignment_phase,
    phase_changed_at,
    phase_changed_by,
    phase_actor_role,
    phase_event_id
FROM assignments;
