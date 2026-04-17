-- IRC messages — durable log of every IRC event delivered to a channel.
--
-- Provides the replay, audit, and UI-channel-view backing store for the
-- adversarial-review IRC facade (epic `ryve-5dcdf56e`). Every message the
-- relay accepts (inbound user privmsg, system notice, topic change) lands
-- here; consumers replay by `(epic_id, created_at)` and search by full-text
-- over `raw_text`.
--
-- ## Spec translation note
--
-- The originating spark specifies `BIGSERIAL` and `GIN on to_tsvector(simple,
-- raw_text)` (PostgreSQL). This workshop runs SQLite, so we map faithfully:
--   - `BIGSERIAL PK` → `INTEGER PRIMARY KEY AUTOINCREMENT` (64-bit rowid)
--   - `GIN on to_tsvector(simple, raw_text)` → FTS5 virtual table using
--     the `unicode61` tokenizer, which is SQLite's language-agnostic
--     equivalent of Postgres' `simple` dictionary (case-folded,
--     diacritic-stripped, no stemming).
--
-- ## Invariants
--
-- - `structured_event_id` is nullable: not every IRC message originates
--   from a lifecycle event (inbound user commands, operator notices).
-- - IRC messages are never edited or deleted (spark non-goal). The FTS
--   index therefore only needs insert triggers; no update/delete triggers
--   are emitted.

CREATE TABLE IF NOT EXISTS irc_messages (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    epic_id             TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    channel             TEXT NOT NULL,
    irc_message_id      TEXT NOT NULL,
    sender_actor_id     TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL,
    command             TEXT NOT NULL CHECK (command IN ('PRIVMSG', 'NOTICE', 'TOPIC')),
    raw_text            TEXT NOT NULL,
    structured_event_id TEXT REFERENCES event_outbox(event_id) ON DELETE SET NULL,
    created_at          TEXT NOT NULL
);

-- Pagination / replay index: list_by_epic walks this path.
CREATE INDEX IF NOT EXISTS idx_irc_messages_epic_created
    ON irc_messages(epic_id, created_at);

-- Full-text search index. `content_rowid` ties the virtual table to the
-- base row's INTEGER PK so MATCH joins stay rowid-based. `unicode61` is
-- the language-agnostic tokenizer — see translation note above.
CREATE VIRTUAL TABLE IF NOT EXISTS irc_messages_fts USING fts5(
    raw_text,
    content = 'irc_messages',
    content_rowid = 'id',
    tokenize = 'unicode61'
);

-- Keep the FTS index in sync on insert. No update/delete triggers: IRC
-- messages are immutable by design (see invariants above).
CREATE TRIGGER IF NOT EXISTS irc_messages_ai
AFTER INSERT ON irc_messages
BEGIN
    INSERT INTO irc_messages_fts (rowid, raw_text) VALUES (new.id, new.raw_text);
END;
