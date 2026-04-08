-- Track which Hand spawned each Hand. NULL means the session was started
-- directly by the user (no orchestrator). When a Head spawns a child via
-- `ryve hand spawn`, the CLI reads `RYVE_HAND_SESSION_ID` from its env
-- (set by the parent process at spawn time) and persists it here.
--
-- The Hands panel uses this column to render Head → solo-hand attribution
-- when the child does not belong to any of the Head's crews.
ALTER TABLE agent_sessions ADD COLUMN parent_session_id TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_sessions_parent
    ON agent_sessions (parent_session_id);
