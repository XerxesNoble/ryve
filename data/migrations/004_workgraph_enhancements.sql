-- Workgraph enhancements: intent, contracts, provenance, commit linkage,
-- hand assignments, and crew support.

-- ── Phase 1: Structured intent on sparks ──────────────

ALTER TABLE sparks ADD COLUMN risk_level TEXT DEFAULT 'normal';
ALTER TABLE sparks ADD COLUMN scope_boundary TEXT;

CREATE INDEX IF NOT EXISTS idx_sparks_risk ON sparks(risk_level);

-- ── Phase 2: Verification contracts ───────────────────

CREATE TABLE IF NOT EXISTS contracts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    spark_id        TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    description     TEXT NOT NULL,
    check_command   TEXT,
    pattern         TEXT,
    file_glob       TEXT,
    enforcement     TEXT NOT NULL DEFAULT 'required',
    status          TEXT NOT NULL DEFAULT 'pending',
    last_checked_at TEXT,
    last_checked_by TEXT,
    created_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_contracts_spark ON contracts(spark_id);
CREATE INDEX IF NOT EXISTS idx_contracts_status ON contracts(status);

-- ── Phase 3: Provenance on events ─────────────────────

ALTER TABLE events ADD COLUMN actor_type TEXT DEFAULT 'unknown';
ALTER TABLE events ADD COLUMN change_nature TEXT;
ALTER TABLE events ADD COLUMN session_id TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL;

-- ── Phase 4: Commit-spark linkage ─────────────────────

CREATE TABLE IF NOT EXISTS commit_links (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    spark_id        TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    commit_hash     TEXT NOT NULL,
    commit_message  TEXT,
    author          TEXT,
    committed_at    TEXT,
    workshop_id     TEXT NOT NULL,
    linked_by       TEXT NOT NULL DEFAULT 'scan',
    created_at      TEXT NOT NULL,
    UNIQUE(spark_id, commit_hash)
);

CREATE INDEX IF NOT EXISTS idx_commit_links_spark ON commit_links(spark_id);
CREATE INDEX IF NOT EXISTS idx_commit_links_hash ON commit_links(commit_hash);
CREATE INDEX IF NOT EXISTS idx_commit_links_workshop ON commit_links(workshop_id);

-- ── Phase 6: Hand-spark assignments ───────────────────

CREATE TABLE IF NOT EXISTS hand_assignments (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    spark_id            TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    status              TEXT NOT NULL DEFAULT 'active',
    role                TEXT NOT NULL DEFAULT 'owner',
    assigned_at         TEXT NOT NULL,
    last_heartbeat_at   TEXT,
    lease_expires_at    TEXT,
    completed_at        TEXT,
    handoff_to          TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL,
    handoff_reason      TEXT,
    UNIQUE(session_id, spark_id)
);

CREATE INDEX IF NOT EXISTS idx_hand_assignments_session ON hand_assignments(session_id);
CREATE INDEX IF NOT EXISTS idx_hand_assignments_spark ON hand_assignments(spark_id);
CREATE INDEX IF NOT EXISTS idx_hand_assignments_status ON hand_assignments(status);

-- ── Crew support (schema-only for now) ────────────────

CREATE TABLE IF NOT EXISTS crews (
    id              TEXT PRIMARY KEY,
    workshop_id     TEXT NOT NULL,
    name            TEXT NOT NULL,
    purpose         TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS crew_members (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    crew_id         TEXT NOT NULL REFERENCES crews(id) ON DELETE CASCADE,
    session_id      TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
    role            TEXT,
    joined_at       TEXT NOT NULL,
    UNIQUE(crew_id, session_id)
);
