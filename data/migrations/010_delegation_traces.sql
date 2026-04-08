-- Delegation traces capture how a user request flows from the originating
-- Director (Atlas, by default) down through Heads and Hands and back up as
-- a final synthesis. Each row is one delegation hop. Hops can chain via
-- `parent_trace_id` to reconstruct the full call tree of a single request.
--
-- Spark ryve-1e3848b6 (Implement delegation trace model with Atlas as origin).
CREATE TABLE IF NOT EXISTS delegation_traces (
    id                     TEXT PRIMARY KEY,
    workshop_id            TEXT NOT NULL,
    spark_id               TEXT REFERENCES sparks(id) ON DELETE SET NULL,
    parent_trace_id        TEXT REFERENCES delegation_traces(id) ON DELETE CASCADE,
    -- The user-visible request that initiated this whole delegation tree.
    -- Copied onto every hop in the chain so any single row is interpretable.
    originating_request    TEXT NOT NULL,
    -- The Director identity that owns the originating request. Defaults to
    -- 'atlas' so traces are recognisable as Atlas-rooted even when the
    -- delegating actor on a given hop is a downstream Head.
    origin_actor           TEXT NOT NULL DEFAULT 'atlas',
    -- The actor performing this delegation hop (e.g. 'atlas', a Head session
    -- id, etc.).
    delegating_actor       TEXT NOT NULL,
    delegating_actor_kind  TEXT NOT NULL,
    -- The actor receiving the delegation on this hop.
    delegated_target       TEXT NOT NULL,
    delegated_target_kind  TEXT NOT NULL,
    status                 TEXT NOT NULL DEFAULT 'pending',
    -- Raw result returned by the delegated target (tool output, Hand summary,
    -- Head report, etc.). Filled in when the hop completes.
    execution_result       TEXT,
    -- The Director's final synthesis back to the originating user. Only set
    -- on the root hop of a chain — leaf hops leave this NULL.
    final_synthesis        TEXT,
    created_at             TEXT NOT NULL,
    updated_at             TEXT NOT NULL,
    completed_at           TEXT
);

CREATE INDEX IF NOT EXISTS idx_delegation_traces_workshop
    ON delegation_traces (workshop_id);
CREATE INDEX IF NOT EXISTS idx_delegation_traces_spark
    ON delegation_traces (spark_id);
CREATE INDEX IF NOT EXISTS idx_delegation_traces_parent
    ON delegation_traces (parent_trace_id);
CREATE INDEX IF NOT EXISTS idx_delegation_traces_origin
    ON delegation_traces (origin_actor);
CREATE INDEX IF NOT EXISTS idx_delegation_traces_delegating
    ON delegation_traces (delegating_actor);
CREATE INDEX IF NOT EXISTS idx_delegation_traces_target
    ON delegation_traces (delegated_target);
CREATE INDEX IF NOT EXISTS idx_delegation_traces_status
    ON delegation_traces (status);
