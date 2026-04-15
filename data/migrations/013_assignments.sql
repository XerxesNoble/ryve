-- Canonical Assignment entity.
--
-- Introduces the phase-based `assignments` table required by the Assignment
-- epic while preserving the pre-existing `hand_assignments` table for the
-- workshop's session-claim runtime. Existing `hand_assignments` rows are
-- copied into `assignments` so old workshops retain their in-flight work.

CREATE TABLE IF NOT EXISTS assignments (
    assignment_id        TEXT PRIMARY KEY NOT NULL,
    spark_id             TEXT NOT NULL REFERENCES sparks(id) ON DELETE RESTRICT,
    actor_id             TEXT NOT NULL,
    assignment_phase     TEXT NOT NULL,
    source_branch        TEXT,
    target_branch        TEXT,
    event_version        INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_assignments_spark ON assignments(spark_id);
CREATE INDEX IF NOT EXISTS idx_assignments_actor ON assignments(actor_id);
CREATE INDEX IF NOT EXISTS idx_assignments_phase ON assignments(assignment_phase);

INSERT INTO assignments (
    assignment_id,
    spark_id,
    actor_id,
    assignment_phase,
    source_branch,
    target_branch,
    event_version,
    created_at,
    updated_at
)
SELECT
    'asgn-migrated-' || id,
    spark_id,
    session_id,
    CASE status
        WHEN 'completed' THEN 'merged'
        ELSE 'assigned'
    END,
    'hand/' || substr(session_id, 1, 8),
    'main',
    0,
    assigned_at,
    COALESCE(completed_at, last_heartbeat_at, assigned_at)
FROM hand_assignments;
