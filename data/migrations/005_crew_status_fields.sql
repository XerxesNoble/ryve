-- Crew status fields and Head/Merger linkage [sp-ux0035]
--
-- The crews/crew_members tables were introduced as schema-only in migration
-- 004. The Head orchestrator workflow needs three more pieces of state:
--
--   • status            — lifecycle state (active|merging|completed|abandoned)
--   • head_session_id   — which agent session is the Head of this crew
--   • parent_spark_id   — the user-facing epic that the crew was spun up for
--
-- These are added as nullable / defaulted columns so existing rows are
-- preserved. SQLite has no `ADD COLUMN IF NOT EXISTS`, but ALTER fails
-- loudly when re-applied — sqlx::migrate runs each migration once, so this
-- is safe.

ALTER TABLE crews ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
ALTER TABLE crews ADD COLUMN head_session_id TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL;
ALTER TABLE crews ADD COLUMN parent_spark_id TEXT REFERENCES sparks(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_crews_workshop ON crews(workshop_id);
CREATE INDEX IF NOT EXISTS idx_crews_status ON crews(status);
CREATE INDEX IF NOT EXISTS idx_crew_members_crew ON crew_members(crew_id);
CREATE INDEX IF NOT EXISTS idx_crew_members_session ON crew_members(session_id);
