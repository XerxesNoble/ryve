# Workgraph Write Discipline

Every Ryve workshop stores its workgraph in a single SQLite database at
`.ryve/sparks.db`. That database is written to by **many independent
processes simultaneously**:

- the GUI application,
- every `ryve` CLI invocation (one per command),
- every Hand subprocess (Claude Code, Codex, etc.) running the `ryve`
  CLI from a git worktree,
- background jobs like ember sweeps and backup snapshots,
- and, with the Head + Crew system, an arbitrary fan-out of Hands
  spawned concurrently by a single Head.

Without an explicit discipline, this fan-out corrupts the database —
exactly what the 2026-04-08 incident showed (spark `sp-b862594d`, the
first perf-audit attempt, and its parent `sp-fbf2a519`). This document
is the policy that every writer — direct or indirect — must obey.

## Policy at a glance

Ryve uses a **documented + enforced concurrency contract**, not a
single-writer mediator. The invariant is:

> N concurrent Hands making normal `ryve` CLI calls cannot corrupt
> `sparks.db`, lose writes, or wedge the workgraph.

This is achieved by five layered mechanisms, each enforced in a single
place that every writer is forced to go through:

| # | Mechanism | Enforced in | What it protects against |
|---|---|---|---|
| 1 | `journal_mode=WAL` | `data::db::connect_options` | Reader/writer blocking; torn writes |
| 2 | `busy_timeout=5000ms` | `data::db::connect_options` | `SQLITE_BUSY` under bursty fan-out |
| 3 | `synchronous=NORMAL` | `data::db::connect_options` | Unsafe defaults; performance floor |
| 4 | `foreign_keys=ON` | `data::db::connect_options` | Orphan rows, referential drift |
| 5 | `with_busy_retry` (defensive) | `data::db::with_busy_retry` | Edge cases where (2) expires |

Because every writer in the system opens its pool through
`data::db::open_sparks_db`, all five mechanisms are applied uniformly.
There is no path that opens the file with `rusqlite`, `sqlite3`, or a
hand-rolled `SqliteConnectOptions` — `grep` the repo and you will find
exactly one place.

## What each mechanism does

### 1. WAL mode

`PRAGMA journal_mode=WAL` switches SQLite from the default rollback
journal to a write-ahead log. In WAL mode:

- readers never block writers, and writers never block readers;
- only **one writer at a time** may hold the write lock, but that lock
  is file-level, so it serializes writers from *different processes*
  as well as from the same process;
- commits are atomic at the WAL-frame level.

The sidecars this introduces (`sparks.db-wal`, `sparks.db-shm`) are
never committed to git — see `docs/WORKGRAPH.md` for that invariant.

### 2. Busy timeout (5000ms)

When a writer encounters a locked database — another process is in the
middle of a `BEGIN IMMEDIATE ... COMMIT` — SQLite would normally return
`SQLITE_BUSY` immediately. `PRAGMA busy_timeout=5000` tells it to retry
internally for up to 5 seconds instead. Under the fan-out patterns Ryve
actually sees (Head spawns ~8–16 Hands, each makes a handful of CLI
calls per minute), 5 s is orders of magnitude more than the worst-case
queue depth.

The timeout is chosen to be *long enough* to ride out a full burst of
writers without surfacing transient errors, and *short enough* that a
truly stuck writer (a process crash mid-transaction is impossible
thanks to WAL, but a flock contention bug would be) surfaces as a
visible error instead of a hang.

### 3. `synchronous=NORMAL`

Safe in WAL mode and roughly 2–3× faster than the default `FULL`.
Still fsync's on checkpoint, so crash safety is preserved; the only
window of loss is the in-WAL frames of the last ~1 ms if the OS itself
crashes, which Ryve tolerates.

### 4. `foreign_keys=ON`

SQLite ships with foreign keys *off* by default. Enforcing them on
every connection guarantees that a bug which tries to write a dangling
bond or orphan comment fails loudly rather than silently corrupting
the referential shape of the graph.

### 5. `with_busy_retry` (second line of defense)

`data::db::with_busy_retry` wraps a write closure in a bounded retry
loop that catches `SQLITE_BUSY` / `SQLITE_LOCKED` errors that escape
the 5 s busy timeout. It uses exponential backoff (25 ms → 400 ms, 5
attempts) and propagates any non-busy error unchanged. This is purely
defensive: under current load levels the busy timeout alone is enough,
but if a future fan-out pattern starts to graze the 5 s ceiling this
wrapper absorbs it without a policy change.

## What writers must (and must not) do

### MUST

- **Open the database only through `data::db::open_sparks_db`.** No
  direct `SqliteConnectOptions::new().filename(...)` anywhere in the
  tree. The function is the single choke point where pragmas are
  applied.
- **Reference the spark id in commits** — not a concurrency concern,
  but it's part of the same write-discipline contract.
- **Use the `ryve` CLI** for all workgraph mutations, from Hands and
  scripts alike. The CLI goes through the same `open_sparks_db` path,
  so it inherits all five mechanisms automatically.

### MUST NOT

- **Never touch `sparks.db` with `sqlite3`, `rusqlite`, or an
  unguarded `SqliteConnectOptions`.** Doing so bypasses every pragma
  above and reintroduces the corruption risk.
- **Never stage or commit `sparks.db*` files** (see
  `docs/WORKGRAPH.md` — `.gitignore`, pre-commit hook, and
  `tests/no_tracked_sparks_db.rs` all enforce this).
- **Never share a `SqlitePool` across process boundaries.** Each
  process opens its own pool; multi-process serialization is provided
  by SQLite's file lock, not by the pool.
- **Never hold an open write transaction across an `await` on a slow
  external resource** (network, LLM, subprocess). The in-process
  writer count is small but the transaction *duration* is what
  matters.

## Why not a single-writer mediator?

Alternative policy considered and rejected: the Head process opens the
only `sparks.db` connection pool and exposes an IPC API; Hands send
mutation requests to the Head over a Unix socket instead of opening
the database themselves.

Rejected because:

1. **Heads are not the root of the process tree.** A user can run
   `ryve spark list` from a terminal with no Head alive. A
   mediator-only policy would either require a daemon or make the
   CLI unusable without a GUI, both of which are non-starters.
2. **Multiple Heads per workshop is an explicit design goal** (see
   `docs/AGENT_HIERARCHY.md`). A single-writer mediator would require
   electing a leader among Heads, which is vastly more machinery than
   SQLite's file lock already provides for free.
3. **SQLite's file lock is the most battle-tested single-writer
   mediator in existence.** Reimplementing it in Rust over a Unix
   socket has negative ROI.

The WAL + busy_timeout approach gets the same correctness guarantee
(one writer at a time, across all processes) with no new moving parts.

## How this is tested

Two stress tests in the tree exercise the policy end-to-end:

1. **In-process fan-out** — `data/tests/concurrency_stress.rs` spawns
   50 concurrent tokio tasks against a single pool, a separate 60-task
   multi-pool storm, and a busy-retry propagation check. Verifies WAL
   pragma is applied, all writes durable, `PRAGMA integrity_check = ok`
   after the storm, and DB is reopenable.
2. **Multi-process fan-out** — `tests/concurrent_cli_writers.rs`
   spawns ≥8 real `ryve spark create` subprocesses against a single
   temp workshop. This is the true test of the policy: it exercises
   the exact code path a Crew of Hands uses (separate OS processes,
   each opening its own pool, racing for the file lock). Verifies all
   N subprocess exit codes are 0, all N rows are durable, and
   `PRAGMA integrity_check` returns `ok` after the storm.

Adding a new writer to the system? Add a line to one of those tests.
Policy violations must fail in CI, not in production.

## Related

- `data/src/db.rs` — the single choke point where the policy is
  implemented.
- `docs/WORKGRAPH.md` — broader workgraph design; sidecar tracking
  invariant.
- `docs/RECOVERY.md` — what to do if a corruption does slip through.
- Spark `sp-b862594d` (2026-04-08) — the incident this policy exists
  to prevent a recurrence of.
- Spark `sp-fbf2a519` (parent of this work) — perf-audit fan-out
  corruption that motivated Crew-wide write discipline.
