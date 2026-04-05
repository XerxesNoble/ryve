# Ryve Workshop

You are working inside a **Ryve Workshop**. Ryve manages tasks (called *sparks*) in an embedded workgraph stored at `.ryve/sparks.db`.

There are no active sparks right now.

## Workflow

- **Claim a spark** before starting work to prevent duplicate effort.
- **Reference spark IDs** in commit messages (e.g. `fix(auth): validate token expiry [sp-a1b2]`).
- **Focus on priority order** — P0 sparks are critical, P4 are negligible.
- **Respect architectural constraints** — violations are blocking.
- **Check required contracts** before marking a spark as done.
- If you discover a new bug or task while working, mention it so it can be tracked as a new spark.
- Do not close or modify sparks directly — Ryve manages spark lifecycle.
