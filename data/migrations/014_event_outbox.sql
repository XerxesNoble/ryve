-- Event outbox — durable append-only log of assignment state changes.
--
-- Every Assignment state transition writes the new state AND appends a row
-- here inside a single transaction. A background relay task (see
-- `sparks::relay`) drains undelivered rows to registered subscribers
-- (IRC, GitHub mirror, state projector) and stamps `delivered_at` on success.
-- Failed deliveries are retried with exponential backoff; they are never
-- removed from the table, so no event is ever lost.

CREATE TABLE IF NOT EXISTS event_outbox (
    event_id         TEXT PRIMARY KEY NOT NULL,
    schema_version   INTEGER NOT NULL,
    timestamp        TEXT NOT NULL,
    assignment_id    TEXT NOT NULL,
    actor_id         TEXT NOT NULL,
    event_type       TEXT NOT NULL,
    payload          TEXT NOT NULL,
    delivered_at     TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_event_outbox_event_id ON event_outbox(event_id);
CREATE INDEX IF NOT EXISTS idx_event_outbox_undelivered
    ON event_outbox(timestamp) WHERE delivered_at IS NULL;
