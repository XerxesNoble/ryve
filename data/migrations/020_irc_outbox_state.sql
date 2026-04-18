-- IRC outbox state — per-event delivery bookkeeping for the IRC relay.
--
-- The generic `event_outbox.delivered_at` column marks a row as delivered
-- to every registered subscriber (see `sparks::relay`). The IRC relay
-- (see `ipc::outbox_relay`) runs alongside that and keeps its own
-- per-event status because it needs to record bounded retries and a
-- terminal 'failed' state that the generic relay has no column for.
--
-- Rows here are created the first time the IRC relay touches an event
-- and transition pending → sent (delivered or intentionally filtered)
-- or pending → failed (max_attempts exceeded). Missing rows are
-- interpreted as 'pending, never attempted'.

CREATE TABLE IF NOT EXISTS irc_outbox_state (
    event_id     TEXT PRIMARY KEY NOT NULL
                 REFERENCES event_outbox(event_id) ON DELETE CASCADE,
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'sent', 'failed')),
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    sent_at      TEXT,
    updated_at   TEXT NOT NULL
);

-- The relay polls for "needs-work" rows: either no state row yet
-- (implicitly pending) or an explicit failed row within the retry budget.
CREATE INDEX IF NOT EXISTS idx_irc_outbox_state_status
    ON irc_outbox_state(status);
