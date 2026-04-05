# Workgraph — Ryve's Embedded Issue Tracker

Workgraph is Ryve's built-in issue tracker with a dependency graph, inspired by [beads](https://github.com/gastownhall/beads). Each workshop gets its own SQLite database at `.ryve/sparks.db`.

## Naming

| Concept | Ryve Name | Description |
|---------|-----------|-------------|
| Issue/task | **Spark** | Unit of work (`sp-xxxx`) |
| Coordination template | **Alloy** | Scatter/Watch/Chain patterns |
| Ephemeral signal | **Ember** | Glow/Flash/Flare/Blaze/Ash |
| Persistent knowledge | **Engraving** | Key-value shared memory |
| Compression | **Tempering** | Semantic compaction (future) |
| Ready work | **Hot** | Unblocked, non-deferred sparks |
| Dependency | **Bond** | Blocks/ParentChild/Related/etc. |
| Label | **Stamp** | Tags on sparks |

## Architecture

All Workgraph code lives in the `data` crate:

```
data/
├── migrations/001_create_sparks_tables.sql
├── src/
│   ├── db.rs                    # Database connection & migration
│   ├── sparks/
│   │   ├── mod.rs               # Module exports
│   │   ├── types.rs             # All domain types & enums
│   │   ├── error.rs             # SparksError
│   │   ├── id.rs                # Hash-based ID generation
│   │   ├── spark_repo.rs        # Spark CRUD + filtering
│   │   ├── bond_repo.rs         # Dependency CRUD + cycle guard
│   │   ├── stamp_repo.rs        # Label CRUD
│   │   ├── comment_repo.rs      # Comment CRUD
│   │   ├── event_repo.rs        # Audit trail (append-only)
│   │   ├── ember_repo.rs        # Ephemeral signals + TTL sweep
│   │   ├── engraving_repo.rs    # Persistent knowledge (upsert)
│   │   ├── alloy_repo.rs        # Coordination templates
│   │   └── graph.rs             # Cycle detection, hot query, topo sort
│   └── github/
│       ├── mod.rs
│       └── sync.rs              # GitHub Issues bidirectional sync
└── tests/
    ├── fixtures/seed_sparks.sql
    ├── spark_crud.rs
    ├── bond_crud.rs
    ├── cycle_detection.rs
    ├── hot_query.rs
    ├── alloy_ops.rs
    ├── ember_ops.rs
    └── engraving_ops.rs
```

## Database Schema (9 tables)

- **sparks** — Core work items with status, priority, type, assignee, GitHub link
- **bonds** — Dependencies (blocks, parent_child, related, conditional_blocks, waits_for, duplicates, supersedes)
- **stamps** — Labels on sparks
- **comments** — Discussion threads
- **events** — Audit trail (append-only, records field changes)
- **embers** — Ephemeral inter-agent signals with TTL
- **engravings** — Persistent shared knowledge (key-value per workshop)
- **alloys** — Coordination templates (scatter/watch/chain)
- **alloy_members** — Ordered spark membership in alloys

## Hot Query Algorithm

A spark is "hot" (ready to work) when:
1. Status is `open` or `in_progress`
2. Not deferred (`defer_until` is null or in the past)
3. No open blocking bonds (all blockers are `closed`)
4. Not a child of a deferred parent

Results sorted by priority (P0 first), then creation time.

## GitHub Issues Sync

Workgraph syncs bidirectionally with GitHub Issues via `octocrab`:

| Spark field | GitHub Issue field |
|-------------|-------------------|
| title | title |
| description | body |
| status | state (open/closed) |
| stamps | labels |
| priority | label (`P0`..`P4`) |
| assignee | assignee |
| closed_reason | closing comment |

### Sync operations
- `push_spark` — Create or update a GitHub issue from a spark
- `pull_issue` — Import a GitHub issue as a spark
- `push_all` / `pull_all` — Batch sync all sparks/issues
- `close_issue` — Close GitHub issue when spark closes
- `sync_comments` — Pull new GitHub comments into spark comments

## Alloy Patterns

| Type | Description | Bond Type |
|------|-------------|-----------|
| **Scatter** | Parallel independent work | Parallel |
| **Watch** | Cyclic monitoring pattern | Parallel |
| **Chain** | Sequential pipeline | Sequential |

## Ember Types

| Type | Purpose | Typical TTL |
|------|---------|-------------|
| **Glow** | Heartbeat — "I'm still working" | 5 min |
| **Flash** | Quick signal — "API changed" | 1 hour |
| **Flare** | Warning — "I hit a problem" | 4 hours |
| **Blaze** | Urgent — needs immediate attention | 8 hours |
| **Ash** | Cleanup report — "I removed X" | 30 min |

## Test Coverage

36 tests across 7 test files covering:
- Spark CRUD, filtering, close-with-reason
- Bond CRUD, cascade deletion, blocker listing
- Cycle detection (linear, cyclic, self-ref, non-blocking bypass)
- Hot query (blocked exclusion, deferred exclusion, priority ordering, complex 5-node graph)
- Alloy creation (scatter, chain), member ordering, cascade deletion
- Ember creation, TTL filtering, type filtering, expired sweep
- Engraving upsert, workshop isolation, deletion
