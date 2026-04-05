-- Persistent agent sessions: track conversations across restarts
CREATE TABLE IF NOT EXISTS agent_sessions (
    id           TEXT PRIMARY KEY,
    workshop_id  TEXT NOT NULL,
    agent_name   TEXT NOT NULL,
    agent_command TEXT NOT NULL,
    agent_args   TEXT NOT NULL DEFAULT '[]',
    session_label TEXT,
    status       TEXT NOT NULL DEFAULT 'active',
    started_at   TEXT NOT NULL,
    ended_at     TEXT,
    resume_id    TEXT
);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_workshop ON agent_sessions(workshop_id);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_status ON agent_sessions(status);
