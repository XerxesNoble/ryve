-- Open tabs per workshop. Restored when the workshop opens so users
-- don't have to manually re-open shells, file viewers, etc.
--
-- We persist a snapshot of the bench's tab list. The whole snapshot is
-- rewritten on every change (delete-then-insert) so the table is always
-- a faithful mirror of what the user sees — no diff/merge logic needed.
--
-- `tab_kind` is one of: 'terminal' | 'file_viewer'. Coding-agent tabs
-- are NOT persisted here; they're already tracked in `agent_sessions`
-- and restored via the existing Resume button flow.
--
-- `payload` is kind-specific:
--   - terminal:    NULL
--   - file_viewer: absolute file path
CREATE TABLE IF NOT EXISTS open_tabs (
    workshop_id  TEXT    NOT NULL,
    position     INTEGER NOT NULL,
    tab_kind     TEXT    NOT NULL,
    title        TEXT    NOT NULL,
    payload      TEXT,
    PRIMARY KEY (workshop_id, position)
);

CREATE INDEX IF NOT EXISTS idx_open_tabs_workshop
    ON open_tabs (workshop_id);
