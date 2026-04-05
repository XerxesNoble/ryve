-- Workgraph: sparks (core work items)
CREATE TABLE IF NOT EXISTS sparks (
    id                  TEXT PRIMARY KEY,
    title               TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    status              TEXT NOT NULL DEFAULT 'open',
    priority            INTEGER NOT NULL DEFAULT 2,
    spark_type          TEXT NOT NULL DEFAULT 'task',
    assignee            TEXT,
    owner               TEXT,
    parent_id           TEXT REFERENCES sparks(id) ON DELETE SET NULL,
    workshop_id         TEXT NOT NULL,
    estimated_minutes   INTEGER,
    github_issue_number INTEGER,
    github_repo         TEXT,
    metadata            TEXT DEFAULT '{}',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    closed_at           TEXT,
    closed_reason       TEXT,
    due_at              TEXT,
    defer_until         TEXT
);

CREATE INDEX IF NOT EXISTS idx_sparks_status ON sparks(status);
CREATE INDEX IF NOT EXISTS idx_sparks_workshop ON sparks(workshop_id);
CREATE INDEX IF NOT EXISTS idx_sparks_parent ON sparks(parent_id);
CREATE INDEX IF NOT EXISTS idx_sparks_github ON sparks(github_issue_number, github_repo);

-- Bonds (dependencies between sparks)
CREATE TABLE IF NOT EXISTS bonds (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id   TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    to_id     TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    bond_type TEXT NOT NULL,
    UNIQUE(from_id, to_id, bond_type)
);

CREATE INDEX IF NOT EXISTS idx_bonds_from ON bonds(from_id);
CREATE INDEX IF NOT EXISTS idx_bonds_to ON bonds(to_id);

-- Stamps (labels on sparks)
CREATE TABLE IF NOT EXISTS stamps (
    spark_id TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    PRIMARY KEY (spark_id, name)
);

CREATE INDEX IF NOT EXISTS idx_stamps_name ON stamps(name);

-- Comments
CREATE TABLE IF NOT EXISTS comments (
    id         TEXT PRIMARY KEY,
    spark_id   TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    author     TEXT NOT NULL,
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comments_spark ON comments(spark_id);

-- Events (audit trail)
CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    spark_id   TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    actor      TEXT NOT NULL,
    field_name TEXT NOT NULL,
    old_value  TEXT,
    new_value  TEXT,
    reason     TEXT,
    timestamp  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_spark ON events(spark_id);

-- Embers (ephemeral inter-agent signals)
CREATE TABLE IF NOT EXISTS embers (
    id           TEXT PRIMARY KEY,
    ember_type   TEXT NOT NULL,
    content      TEXT NOT NULL,
    source_agent TEXT,
    workshop_id  TEXT NOT NULL,
    ttl_seconds  INTEGER NOT NULL DEFAULT 3600,
    created_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_embers_workshop ON embers(workshop_id);
CREATE INDEX IF NOT EXISTS idx_embers_type ON embers(ember_type);

-- Engravings (persistent shared knowledge)
CREATE TABLE IF NOT EXISTS engravings (
    key         TEXT NOT NULL,
    workshop_id TEXT NOT NULL,
    value       TEXT NOT NULL,
    author      TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (key, workshop_id)
);

-- Alloys (coordination templates)
CREATE TABLE IF NOT EXISTS alloys (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    alloy_type      TEXT NOT NULL,
    parent_spark_id TEXT REFERENCES sparks(id) ON DELETE SET NULL,
    workshop_id     TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

-- Alloy members (sparks in an alloy with ordering)
CREATE TABLE IF NOT EXISTS alloy_members (
    alloy_id  TEXT NOT NULL REFERENCES alloys(id) ON DELETE CASCADE,
    spark_id  TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    bond_type TEXT NOT NULL DEFAULT 'parallel',
    position  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (alloy_id, spark_id)
);
