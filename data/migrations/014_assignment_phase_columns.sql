-- Add phase-tracking columns to assignments table.
-- These columns track the workflow phase of an assignment
-- (governed by the transition validator).

ALTER TABLE assignments ADD COLUMN assignment_phase TEXT;
ALTER TABLE assignments ADD COLUMN phase_changed_at TEXT;
ALTER TABLE assignments ADD COLUMN phase_changed_by TEXT;
ALTER TABLE assignments ADD COLUMN phase_actor_role TEXT;
ALTER TABLE assignments ADD COLUMN phase_event_id INTEGER;
