-- GitHub mirror foundation: schema for the spark → PR mirror and event
-- dedup. Ryve will translate canonical GitHub events (`CanonicalGitHubEvent`
-- in `data/src/github/types.rs`) into assignment-phase transitions; this
-- migration lays the storage that every downstream translator / applier /
-- poller / webhook relies on.
--
-- Invariants established here:
--
-- * `assignments.github_artifact_branch` / `github_artifact_pr_number` are
--   the authoritative link from an `assignments` row to the external
--   artifact (head branch + PR number) it mirrors on GitHub. Both columns
--   are nullable because an assignment starts life without a pushed branch
--   or PR — the applier fills them in once the artifact exists.
-- * `github_events_seen` is a dedup log keyed by `github_event_id`, the
--   provider-supplied identifier every webhook/poll response carries
--   (delivery UUID for webhooks, `updated_at` + id for polled reviews,
--   etc.). Presence of a row means the event has already been applied;
--   downstream appliers MUST consult this table before mutating state so
--   retries from GitHub are idempotent.

-- 1. Assignment mirror columns -------------------------------------------
ALTER TABLE assignments ADD COLUMN github_artifact_branch TEXT;
ALTER TABLE assignments ADD COLUMN github_artifact_pr_number INTEGER;

-- Lookup path: webhook/poll handlers receive a PR number and must locate
-- the owning assignment in O(log n).
CREATE INDEX IF NOT EXISTS idx_assignments_github_pr
    ON assignments(github_artifact_pr_number)
    WHERE github_artifact_pr_number IS NOT NULL;

-- 2. GitHub event dedup log ----------------------------------------------
CREATE TABLE IF NOT EXISTS github_events_seen (
    github_event_id  TEXT PRIMARY KEY NOT NULL,
    event_type       TEXT NOT NULL,
    ingested_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_github_events_seen_type
    ON github_events_seen(event_type);
