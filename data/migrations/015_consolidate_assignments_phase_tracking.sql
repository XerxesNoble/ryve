-- Consolidate hand_assignments into the assignments table and add
-- phase-transition audit columns for the transactional state+event writer.
--
-- After this migration the assignments table is the single source of truth.
-- Migration 015 recreates hand_assignments as a backward-compatible view.

PRAGMA foreign_keys=OFF;

CREATE TABLE assignments_new (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    assignment_id        TEXT UNIQUE NOT NULL,
    spark_id             TEXT NOT NULL REFERENCES sparks(id) ON DELETE RESTRICT,
    actor_id             TEXT NOT NULL DEFAULT '',
    assignment_phase     TEXT,
    source_branch        TEXT,
    target_branch        TEXT,
    event_version        INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    session_id           TEXT,
    status               TEXT NOT NULL DEFAULT 'active',
    role                 TEXT NOT NULL DEFAULT 'owner',
    assigned_at          TEXT,
    last_heartbeat_at    TEXT,
    lease_expires_at     TEXT,
    completed_at         TEXT,
    handoff_to           TEXT,
    handoff_reason       TEXT,
    phase_changed_at     TEXT,
    phase_changed_by     TEXT,
    phase_actor_role     TEXT,
    phase_event_id       INTEGER
);

INSERT INTO assignments_new (
    assignment_id, spark_id, actor_id, assignment_phase,
    source_branch, target_branch, event_version,
    created_at, updated_at,
    session_id, status, assigned_at
)
SELECT
    assignment_id, spark_id, actor_id, assignment_phase,
    source_branch, target_branch, event_version,
    created_at, updated_at,
    actor_id, 'active', created_at
FROM assignments;

INSERT INTO assignments_new (
    assignment_id, spark_id, actor_id,
    session_id, status, role, assigned_at,
    last_heartbeat_at, lease_expires_at, completed_at,
    handoff_to, handoff_reason,
    assignment_phase, event_version,
    created_at, updated_at
)
SELECT
    'asgn-ha2-' || ha.id,
    ha.spark_id,
    ha.session_id,
    ha.session_id,
    ha.status,
    ha.role,
    ha.assigned_at,
    ha.last_heartbeat_at,
    ha.lease_expires_at,
    ha.completed_at,
    ha.handoff_to,
    ha.handoff_reason,
    CASE ha.status WHEN 'completed' THEN 'merged' ELSE 'assigned' END,
    0,
    ha.assigned_at,
    COALESCE(ha.completed_at, ha.last_heartbeat_at, ha.assigned_at)
FROM hand_assignments ha
WHERE NOT EXISTS (
    SELECT 1 FROM assignments_new an
    WHERE an.spark_id = ha.spark_id
      AND an.session_id = ha.session_id
);

DROP TABLE assignments;
DROP TABLE hand_assignments;
ALTER TABLE assignments_new RENAME TO assignments;

CREATE INDEX idx_assignments_spark ON assignments(spark_id);
CREATE INDEX idx_assignments_actor ON assignments(actor_id);
CREATE INDEX idx_assignments_phase ON assignments(assignment_phase);
CREATE INDEX idx_assignments_session ON assignments(session_id);
CREATE INDEX idx_assignments_active ON assignments(status, role);

PRAGMA foreign_keys=ON;
