-- Watches: durable workgraph rows that represent a recurring observation of
-- a target spark. Atlas (and other orchestrators) use watches to fire a
-- recurring intent — "poll this spark every N seconds", "remind me when
-- this moves to merged", etc. — without keeping state in memory.
--
-- Spark ryve-7f5a9e5b [sp-ee3f5c74]: this is the foundation every downstream
-- watch-related sibling depends on (scheduler, firing, UI, wiring).
--
-- Invariants enforced here:
--
-- * `cadence` is a text blob owned by the repo layer. It is encoded as
--   `interval-secs:<N>` or `cron:<expr>` by `WatchCadence::to_storage`.
-- * `stop_condition` is nullable text; `NULL` means "never stop".
-- * `status` is one of (`active`, `completed`, `cancelled`). Cancellation is
--   soft — rows are never deleted so the audit trail is preserved.
-- * A UNIQUE partial index on (target_spark_id, intent_label) prevents two
--   non-cancelled watches from racing on the same target+intent. Cancelled
--   rows are excluded so a replace (cancel + create) can succeed.
CREATE TABLE IF NOT EXISTS watches (
    id                TEXT PRIMARY KEY,
    target_spark_id   TEXT NOT NULL,
    cadence           TEXT NOT NULL,
    stop_condition    TEXT,
    intent_label      TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'active'
                      CHECK (status IN ('active','completed','cancelled')),
    last_fired_at     TEXT,
    next_fire_at      TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    created_by        TEXT
);

-- Prevent duplicate active/completed watches for the same (target, intent).
-- Cancelled rows are excluded so replace-style flows (cancel-then-create in
-- one tx) do not trip the constraint.
CREATE UNIQUE INDEX IF NOT EXISTS idx_watches_target_intent_live
    ON watches(target_spark_id, intent_label)
    WHERE status != 'cancelled';

-- Scheduler lookup path: find watches whose next_fire_at has elapsed.
CREATE INDEX IF NOT EXISTS idx_watches_status_next_fire
    ON watches(status, next_fire_at);

-- Target lookup path: list all watches on a given spark.
CREATE INDEX IF NOT EXISTS idx_watches_target
    ON watches(target_spark_id);
