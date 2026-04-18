-- Heartbeat + repair-cycle + liveness columns for the `assignments` table.
--
-- Parent epic ryve-cf05fd85: liveness monitoring that turns a silent Hand
-- into an observable state transition. This migration lays the storage
-- foundation; the watchdog, event emitter, and override-recovery flows
-- arrive in sibling sparks (ryve-fe4e03d3, ryve-85034c27, ryve-60e1d586,
-- ryve-d649bb6f).
--
-- Notes
-- -----
-- `last_heartbeat_at` was introduced for the legacy `hand_assignments`
-- table and preserved in the consolidated `assignments` table by
-- migration 015, so it is NOT re-added here. It already exists as a
-- nullable TEXT column (RFC3339 timestamp, same convention as every
-- other timestamp in this schema). [sp-8c87070d]
--
-- `repair_cycle_count` counts how many times a rejected assignment has
-- re-entered the repair loop. The watchdog reads it to escalate
-- Assignments past the configured repair_cycle_limit into Stuck.
--
-- `liveness` is the derived health state persisted so the watchdog can
-- atomically transition Healthy -> AtRisk -> Stuck without re-computing
-- from events on every tick. Values mirror the AssignmentLiveness enum
-- in data/src/sparks/types.rs.

ALTER TABLE assignments
    ADD COLUMN repair_cycle_count INTEGER NOT NULL DEFAULT 0;

ALTER TABLE assignments
    ADD COLUMN liveness TEXT NOT NULL DEFAULT 'healthy';

CREATE INDEX IF NOT EXISTS idx_assignments_liveness ON assignments(liveness);
