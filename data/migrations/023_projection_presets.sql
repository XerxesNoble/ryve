-- Projection presets — saved combinations of channel projection filters.
--
-- The channel projection module (`ipc::channel_projection`) builds filtered
-- views over `irc_messages` by ANDing any combination of five structured
-- axes (epic_id, spark_id, assignment_id, pr_number, actor_id) plus an
-- optional FTS5 query. Presets persist a user's favourite filter
-- combinations per channel so the UI can restore them across restarts and
-- derive an unread count from the last message the user saw while the
-- preset was active.
--
-- ## Schema
--
-- Identity: `(workshop_id, channel, name)` is unique. The same preset name
-- can exist on different channels of the same workshop and on channels of
-- different workshops — presets are scoped to a single channel by design.
--
-- `filters_json` carries the serialised `ChannelProjectionQuery` (minus
-- the per-session `current_actor_id` and the paging cursor). Keeping it
-- as a single JSON column trades per-axis columns for forward
-- compatibility: new axes added to the query API don't require a
-- migration, and the projection module is the single place that knows the
-- schema.
--
-- `last_seen_message_id` points at the highest `irc_messages.id` the user
-- had observed while this preset was active. Unread count is derivable as
-- `max(irc_messages.id matching preset filters) - last_seen_message_id`
-- without touching this table beyond a read.
--
-- ## Invariants
--
-- - Presets reference a channel by name, not by FK — channels live only
--   on the IRC server and are derivable from `epic_id` via
--   `channel_manager::channel_name`. A preset keyed on a stale channel
--   string degrades to "no matches" rather than a FK failure.
-- - `last_seen_message_id` starts at `0` (nothing seen yet). It is
--   monotonic — callers must only bump it upwards.

CREATE TABLE IF NOT EXISTS projection_presets (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    workshop_id          TEXT NOT NULL,
    channel              TEXT NOT NULL,
    name                 TEXT NOT NULL,
    filters_json         TEXT NOT NULL,
    last_seen_message_id INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    UNIQUE (workshop_id, channel, name)
);

-- Lookup path used by the UI: "all presets for this channel, in name order".
CREATE INDEX IF NOT EXISTS idx_projection_presets_workshop_channel
    ON projection_presets(workshop_id, channel);
