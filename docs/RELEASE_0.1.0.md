# Release 0.1.0 — Foundation

**Release ID:** `rel-1fa0a40f`
**Branch:** `release/0.1.0`
**Date:** 2026-04-17

## What was implemented

This release lays the foundation for Ryve's multi-agent orchestration layer. Eight epics shipped, delivering the primitives that Atlas, Heads, Hands, and Mergers need to coordinate safely across worktrees, branches, and releases.

### R5 — Event outbox + versioned schema + replay projector
- `event_outbox` table with migration `014_event_outbox.sql`
- Transactional state+event writer: every Assignment mutation atomically appends to the outbox
- Outbox relay with exponential backoff retry (`data/src/sparks/relay.rs`)
- Replay projector with byte-equal deterministic replay test (`data/src/sparks/projector.rs`)
- Consolidation of assignment phase tracking (`015_consolidate_assignments_phase_tracking.sql`)

### R4 — Bundled tmux sessions for Hand/Head durability
- Vendored tmux 3.5a auto-builds via `build.rs` so `bundled_tmux_path()` resolves on fresh clone
- End-to-end lifecycle test: spawn -> reconcile -> attach (`tests/tmux_lifecycle.rs`)
- Updated `docs/VENDORED_TMUX.md` and `scripts/build-vendored-tmux.sh`

### R6 — Branching model + merge isolation enforcement
- Actor-scoped branch naming: `hand/<short>` replaced by `<actor>/<short>` derived from `Assignment.actor_id`
- `source_branch` + `target_branch` columns on Assignment with spawn-time validator
- Pre-merge validator (`data/src/pre_merge_validator.rs`) rejects:
  - User branch -> main (only epic branch is legal target)
  - Cross-actor branch mutations
- Release-branch awareness: epic branches scaffold off the release branch when the epic belongs to an open Release
- Integration tests for all four legality cases

### R2 — Native exploration Hands (read-only investigator archetype)
- `HandKind::Investigator` with read-only tool policy
- `compose_investigator_prompt` in `agent_prompts.rs` — language-agnostic, comment-based findings
- CLI: `ryve hand spawn <spark> --role investigator`
- End-to-end integration test (`tests/investigator_hand.rs`)
- Documentation across `HAND_CAPABILITIES.md`, `HEAD_ARCHETYPES.md`, `WORKSHOP.md`

### R3 — Hand archetypes registry
The largest epic (10 children). Delivered:
- **Core registry** (`src/hand_archetypes.rs`): `Archetype` struct, `ArchetypeId`, `CapabilityGate`, `ToolPolicy`, `compose` function pointer seam
- **Spawn path + CLI**: `archetype_id` column on `agent_sessions` (migration `016`), `--archetype` flag
- **Tool policy enforcement**: `CallerArchetype` enum + `enforce_action` mechanical gate at spawn time
- **Five concrete archetypes:**
  - **Bug Hunter** — Triager+Surgeon hybrid; write-capable, language-agnostic, acceptance = failing-then-passing test + smallest diff
  - **Architect** — read-only design reviewer; outputs structured comments (recommendations / tradeoffs / risks), never diffs
  - **Performance Engineer** — write-capable; profile -> measure -> fix -> verify cycle, records numbers as spark comments
  - **Explorer/Cartographer** — adopted into registry from prior investigator foundation
  - **Release Manager** — Atlas-only comms enforced mechanically; can run `ryve release *`, read-only workgraph, comment only to Atlas
- **Releases E2E test + `docs/RELEASES.md`**: canonical release documentation + CLI integration test
- **Language-agnostic integration test** (`tests/archetype_language_agnostic.rs`): multi-lang fixture (Rust/Python/TypeScript/Go) verifying no archetype prompt leaks language-specific assumptions

### R1 — Head primitive: `ryve head spawn`, archetypes, and Crew orchestration
- `ryve head spawn <epic_id> --archetype <build|research|review>` CLI
- Head lifecycle: decompose epic -> spawn Crew of Hands -> poll progress -> spawn Merger -> post PR URL
- Stall-bug detection (`ryve sweep stalls`): identifies orphaned merge/PR handoffs where all children are closed but no active coordinator remains
- Integration tests for the sweep CLI

### R7 — Reviewer Hand role + deterministic cross-vendor selection
- Reviewer assignment: `Approved` / `Rejected` transition authority enforced at the Assignment state machine level
- Reviewer != author constraint at selection and transition time
- Reviewer spawn path with policy relaxation for cross-vendor review scenarios
- Availability events for reviewer lifecycle

### R8 — Atlas: recurring watch + scheduled task primitive
- `watches` table (migration `017_watches.sql`) with `watch_repo.rs`
- `ryve watch create|list|show|cancel|replace` CLI
- Durable, restart-safe watch runner (`src/watch_runner.rs`, `data/src/sparks/watch_runner.rs`)
- Atlas watch hook template (`src/head_templates/atlas_watch.md`)
- Duplicate suppression for concurrent watch firings
- E2E tests: `tests/atlas_watch_e2e.rs`, `tests/watch_cli.rs`, `tests/watch_duplicate_suppression.rs`, `tests/watch_runner.rs`

## Intent behind the implementation

Ryve 0.1.0 exists to answer one question: **can a Director agent (Atlas) reliably coordinate a tree of coding agents (Heads -> Hands -> Merger) to ship real code across a multi-epic release, without human intervention at every step?**

The eight epics were chosen as the minimal set of primitives to make that loop durable:

1. **Event outbox** (R5) ensures state transitions are never lost — every mutation is atomically recorded and replayable.
2. **Tmux bundling** (R4) makes agent subprocesses survive terminal disconnects and power outages.
3. **Branching model** (R6) prevents agents from stepping on each other's branches or merging directly to main.
4. **Investigator archetype** (R2) gives Heads a read-only tool for codebase audits without accidental writes.
5. **Hand archetypes** (R3) makes the agent type system first-class — each archetype has a mechanical tool policy, not just a prompt suggestion.
6. **Head primitive** (R1) formalizes the Head -> Crew -> Merger lifecycle with stall detection.
7. **Reviewer Hand** (R7) adds the review gate that cross-vendor collaboration requires.
8. **Recurring watch** (R8) lets Atlas schedule unattended coordination loops — the primitive this very release was driven by.

## Known tech debt

1. **Merger does not target release branches.** Throughout this release, Atlas manually merged crew branches into `release/0.1.0` because the Merger always targets `main`. The Release Manager Hand archetype (shipped in R3) is the intended fix, but it was not exercised end-to-end for release close. Tracked as `ryve-476ef264` (Merge Hand contract).

2. **Head "ask the user" stall pattern.** Build Heads sometimes exit after asking A/B/C options instead of deciding unilaterally. This burned multiple cycles on R5 and R2. The archetype prompt needs hardening — logged as `cm-edab7e20` on `ryve-fbf2a519`.

3. **Orphan crew/assignment rows.** Ended Head sessions leave "active" crew rows and "active" assignment rows behind. Tracked as `ryve-312b98ad` (stall-bug detection shipped in R1 partially addresses this with `ryve sweep stalls`).

4. **Rogue child `.ryve/` workshops in worktrees.** Three worktrees had auto-initialized child workshops (0 sparks, harmless but confusing). Root cause: a code path auto-inits `.ryve/` when CWD lacks one instead of walking up. Filed as `ryve-8efa7a76`.

5. **No automatic base-branch stacking for serial child sparks.** Each Hand gets a fresh worktree from the release tip, not from its predecessor's branch. This caused the R3 integration nightmare (10 branches independently modifying the same files). R6's branching model partially addresses this for epic-level scaffolding, but intra-epic child stacking is not yet implemented.

6. **API stream idle timeouts.** The Bug Hunter archetype took 5 attempts due to ~40-min API timeouts. Hands that accumulate large uncommitted changesets are vulnerable. Mitigation: commit-early directives in spark comments. Structural fix: smaller task decomposition or agent-level checkpoint commits.

7. **Migration numbering collisions.** R5 shipped two `014_*.sql` files from parallel Hand branches. Resolved by renumbering one to `015`. Prevention: the branching model (R6) + base-branch stacking should eliminate this class of collision.

## How to manually test this release

Prerequisites: Rust toolchain, SQLite, git. Clone the repo and checkout `release/0.1.0`.

```bash
git checkout release/0.1.0
cargo build --release
```

### 1. Event outbox (R5)

```bash
cargo test --test outbox_relay
cargo test --test projector_replay
cargo test --test transition_phase
```

**Expected:** All tests pass. The outbox relay test exercises exponential backoff. The projector replay test verifies byte-equal deterministic replay.

### 2. Bundled tmux (R4)

```bash
cargo test --test tmux_lifecycle
```

**Expected:** If `vendor/tmux/bin/tmux` is built (run `scripts/build-vendored-tmux.sh` first), the test spawns a tmux session, verifies reconciliation, and cleans up. If the vendored binary is absent, the test is skipped with a clear message.

### 3. Branching model (R6)

```bash
cargo test --test pre_merge_validator
cargo test --test agent_session_archetype
```

**Expected:** The pre-merge validator rejects user-branch -> main merges and cross-actor mutations. The archetype session test verifies `archetype_id` is stored on agent sessions.

### 4. Investigator archetype (R2)

```bash
cargo test --test investigator_hand
```

**Expected:** Spawns a mock investigator Hand, asserts: `session_label == "investigator"`, prompt contains "READ-ONLY" and "ryve comment add", tool policy is ReadOnly.

### 5. Hand archetypes (R3)

```bash
cargo test --test archetype_language_agnostic
cargo test --test bug_hunter_hand
cargo test --test architect_hand
cargo test --test performance_engineer_hand
cargo test --test release_manager_hand
cargo test --test release_e2e
```

**Expected:** All pass. The language-agnostic test verifies no archetype prompt contains language-specific tool names, file extensions, or framework references. Each archetype test verifies its prompt skeleton, tool policy, and spawn behavior.

### 6. Head primitive + stall detection (R1)

```bash
cargo test --test sweep_stalls_cli
```

**Expected:** The sweep test creates an orphaned merge state (all children closed, no active coordinator) and verifies `ryve sweep stalls` detects and reports it.

### 7. Reviewer Hand (R7)

```bash
cargo test --test transition_phase -- reviewer
```

**Expected:** Reviewer-specific transition tests pass: reviewer != author enforcement, Approved/Rejected authority gating.

### 8. Recurring watch (R8)

```bash
cargo test --test watch_cli
cargo test --test watch_runner
cargo test --test watch_duplicate_suppression
cargo test --test atlas_watch_e2e
```

**Expected:** Watch CLI creates/lists/cancels watches. Runner test exercises the durable scheduler. Duplicate suppression prevents concurrent firings. Atlas watch E2E verifies the full hook -> restart -> resume cycle.

### Full suite

```bash
cargo test
```

**Expected:** All tests pass (warnings about unused code are acceptable — some archetype variants are not yet exercised by the main binary).
