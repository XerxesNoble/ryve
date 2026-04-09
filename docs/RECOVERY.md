# Workgraph Backup & Recovery

This document explains how Ryve backs up the workgraph and how to recover
from a corrupted, deleted, or otherwise unusable `.ryve/sparks.db`.

Spark of record: **ryve-7c8573c4** (workgraph backup/restore). The
original incident that motivated this doc: on **2026-04-08** the main
workshop's `sparks.db` was corrupted and the only artifacts left on
`origin/main` were the WAL/SHM sidecars. `sqlite3 .recover` salvaged
only orphan B-tree fragments and about 100 sparks of historical state
were lost. Periodic backups eliminate that class of incident.

## How backups work

Every open workshop gets a periodic SQLite snapshot written to
`.ryve/backups/` next to its `sparks.db`.

- **Format** — each snapshot is a complete, self-contained SQLite
  database file produced via `VACUUM INTO`. Because `VACUUM INTO` is
  run through the live connection pool, WAL frames are applied and the
  snapshot is a consistent point-in-time copy, safe under concurrent
  writes.
- **Cadence** — the Ryve UI fires a `BackupTick` every
  `data::backup::DEFAULT_BACKUP_INTERVAL_SECS` (10 minutes by default).
  Every open workshop snapshots on each tick.
- **Graceful shutdown** — closing a workshop tab (confirmed or not)
  takes one final snapshot before the workshop's SQLite pool is
  dropped. So the worst-case loss on clean shutdown is zero.
- **Retention** — after each successful snapshot the backups directory
  is pruned to keep the newest
  `data::backup::DEFAULT_BACKUP_RETENTION` files (48 by default, so
  ~8 hours of ten-minute rolling history plus daily tails).
- **Naming** — files are named
  `sparks-YYYYMMDDTHHMMSSZ.db`, which sorts lexicographically in
  chronological order. Anything in `.ryve/backups/` that does **not**
  match this prefix is ignored by listing and retention, so you can
  drop manually-archived copies in the directory without losing them.

## Manual CLI commands

```sh
ryve backup create            # take a snapshot now (+ prune to retention)
ryve backup list              # list snapshots (name, size, ISO-8601 time)
ryve backup prune --keep=20   # trim to the newest 20

ryve restore <snapshot>       # restore sparks.db from a snapshot
```

`<snapshot>` is either a bare filename resolved against
`.ryve/backups/` (e.g. `sparks-20260408T130500Z.db`) or an absolute
path to any SQLite file — the latter is useful for restoring from an
external backup (rsync, Time Machine, `git show`).

## Recovery procedure

Follow this when Ryve refuses to open a workshop because
`.ryve/sparks.db` is missing or malformed (symptom: `file is not a
database`, `database disk image is malformed`, or `no such table`).

### 1. Shut down the Ryve UI for this workshop

SQLite cannot be safely overwritten while another process has it open.
Close the workshop tab (or quit the whole app). If a Hand is running,
stop it first.

### 2. Confirm the live database is actually damaged

From the workshop root:

```sh
sqlite3 .ryve/sparks.db "PRAGMA integrity_check;"
```

If it prints `ok`, the DB is fine — your problem is elsewhere (stale
pool, wrong workshop, wrong branch).

### 3. Inspect available snapshots

```sh
ryve backup list
```

Output looks like:

```text
NAME                                           SIZE  TAKEN
------------------------------------------------------------------------
sparks-20260408T130500Z.db                   319488  2026-04-08T13:05:00+00:00
sparks-20260408T125500Z.db                   319488  2026-04-08T12:55:00+00:00
```

Pick the most recent snapshot whose timestamp is **before** the
corruption occurred.

### 4. Restore

```sh
ryve restore sparks-20260408T130500Z.db
```

This does three things:

1. Moves the current `.ryve/sparks.db` to
   `.ryve/sparks.db.pre-restore-<stamp>.bak`, plus the matching
   `-wal` and `-shm` sidecars.
2. Copies the chosen snapshot into place as the new
   `.ryve/sparks.db`.
3. Reports all three paths so you can audit the change.

**Important:** the pre-restore backup is not deleted. If you restored
the wrong snapshot, you can put the original back by renaming
`sparks.db.pre-restore-<stamp>.bak` back over `sparks.db` (and
removing the pristine sidecars). Once you've confirmed the restored
state is what you wanted, delete the `.bak` files to reclaim disk.

### 5. Reopen the workshop

Reopen the workshop in the Ryve UI. `ryve status` should now print
the expected spark counts from the snapshot point in time:

```sh
ryve status
```

### 6. File a follow-up spark if work was lost

If the snapshot is from before some in-flight work you'd already done,
capture a new spark for the lost changes so the next Hand can redo
them. Include the snapshot timestamp so reviewers understand the
recovery window.

## Recovering without any snapshots

If `.ryve/backups/` is empty (pre-backup era, or the backups dir was
never created), fall back to:

1. **Time Machine / host backup** — restore the entire `.ryve/` directory
   from the most recent host backup.
2. **Git history** — if `.ryve/sparks.db` is tracked in git (discouraged,
   but some workshops do it), `git checkout <ref> -- .ryve/sparks.db`
   retrieves the last committed copy.
3. **SQLite `.recover`** — last resort. Produces a dump of whatever
   B-tree pages are still intact, which may not preserve relational
   integrity:

   ```sh
   sqlite3 .ryve/sparks.db.broken ".recover" | sqlite3 .ryve/sparks.db.new
   ```

   Expect orphaned rows, missing foreign keys, and lost bonds. Treat the
   result as "salvage" and create replacement sparks manually where
   needed.

None of these are as good as an in-tree snapshot, which is why the
periodic backup exists.

## Where the code lives

- `data/src/backup.rs` — snapshot / restore primitives, retention,
  filename helpers. Tested by `data/tests/backup_ops.rs`.
- `src/main.rs` — `Message::BackupTick` + `Message::BackupFinished`,
  the periodic subscription, and the final-snapshot hook in
  `App::do_close_workshop`.
- `src/cli.rs` — `ryve backup …` and `ryve restore …` subcommands.
  `restore` deliberately does **not** open a SQLite pool against the
  live database before replacing it.
- `data/src/ryve_dir.rs` — `RyveDir::backups_dir()` and the
  directory-creation hook in `ensure_exists`.
