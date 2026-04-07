# Plan: Head — orchestrator coding agent that manages a Crew of Hands [sp-ux0035]

## Context

Today the user manually clicks **+ → New Coding Agent**, picks a spark,
and a single Hand (a coding agent like `claude` or `codex`) starts work.
There is no autonomous layer that takes a high-level goal ("build the
auth system"), splits it into sparks, and farms them out to several
parallel Hands.

The **Head** is the answer. After clarification:

- The Head is **not** an in-process LLM call from inside Ryve. It is a
  **launched coding agent** — exactly the same kind of subprocess as a
  Hand (claude / codex / aider / opencode) — but launched with a
  different *system prompt* and a different *role*. From Ryve's
  perspective spawning a Head is mechanically identical to spawning a
  Hand.
- The Head manages a **Crew**: a group of Hands that work in parallel
  on related sparks, each in its own git worktree.
- One member of every Crew is the **Merger**: a Hand whose job is to
  collect the Crew's worktrees, merge them into a single integration
  branch, push, and open one PR for review (and, on approval, merge to
  main).
- The user must be able to override the Head at any point through the
  workgraph (close sparks, retire Hands) — invariant.

Existing scaffolding we will reuse:

- `crews` and `crew_members` tables already exist as schema-only in
  `data/migrations/004_workgraph_enhancements.sql:79`. No repo / types
  yet — that is part of this spark.
- `data/src/sparks/types.rs:688` defines `AssignmentRole::{Owner,
  Assistant, Observer}`. We add `Merger`.
- `src/screen/bench.rs:38` has the bench dropdown with `NewTerminal`
  and `NewCodingAgent(CodingAgent)` items — we extend it.
- `src/main.rs:1965` (`compose_hand_prompt`) builds the per-Hand
  initial prompt. We add a parallel `compose_head_prompt` and
  `compose_merger_prompt`.
- `src/workshop.rs:451` (`create_hand_worktree`) creates a worktree per
  session. Reused unchanged.
- `src/main.rs:1235` (`spawn_pending_agent`) is the master spawn flow
  (worktree + session row + assignment + initial prompt). We refactor
  the spark-picker step so the user picks the coding agent **at the
  same time** as the spark.
- `data/src/sparks/assignment_repo.rs` — `assign`, `expire_stale_claims`,
  `is_spark_claimed`, `handoff`. Used by the Head via `ryve` CLI.

## User-visible flow

### Bench dropdown gains three explicit options

The bench `+` dropdown currently lists detected coding agents and
"New Terminal". After this spark it shows:

```
+ ┐
  ├ New Head        ← spawns a Head agent (Crew orchestrator)
  ├ New Hand        ← spawns a Hand agent on a chosen spark
  └ New Terminal    ← unchanged
```

### "New Head" flow

1. User clicks **New Head**.
2. A simple modal asks: which coding agent to use? (claude / codex /
   aider / opencode — populated from `available_agents`). Optional:
   one-line "What do you want the Crew to do?" textbox; if blank, the
   user just types it directly into the agent terminal once it boots.
3. Ryve creates an `agent_sessions` row labelled `head`, creates a
   worktree, opens a bench tab, and launches the chosen coding agent
   in **full-auto** mode with the **Head system prompt** injected via
   the agent's system-prompt flag (already supported in
   `coding_agents.rs:system_prompt_flag`).
4. The Head, now running in a bench tab like any other agent, sees its
   instructions and starts using the `ryve` CLI to do its job.

### "New Hand" flow (refactor of today's coding-agent spawn)

1. User clicks **New Hand**.
2. The spark picker opens. **New:** alongside the spark list there is a
   coding-agent selector (radio / dropdown of detected agents).
3. User selects spark + agent → existing `spawn_pending_agent` runs
   exactly as today, but the agent comes from the picker rather than
   from the dropdown click.

### Visible Crew status

Existing Hands panel (`src/screen/agents.rs`) will get a small
"Crew: <name>" badge under any session whose `agent_sessions.id` is in
`crew_members`. Computed at view time from cached crew data; no new UI
screens for this spark.

## Workgraph additions

### Schema (no migration needed — tables exist)

`crews` and `crew_members` are already created by migration 004. We
only add a Rust repo and types.

If we need any new column we add migration `005_crew_role_status.sql`
adding:

```sql
ALTER TABLE crews ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
ALTER TABLE crews ADD COLUMN head_session_id TEXT
    REFERENCES agent_sessions(id) ON DELETE SET NULL;
ALTER TABLE crews ADD COLUMN parent_spark_id TEXT
    REFERENCES sparks(id) ON DELETE SET NULL;
```

(`crew_members.role` already exists as TEXT, so the Merger flag fits
without schema changes.)

### Types (`data/src/sparks/types.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Crew {
    pub id: String,
    pub workshop_id: String,
    pub name: String,
    pub purpose: Option<String>,
    pub status: String,           // active | merging | completed | abandoned
    pub head_session_id: Option<String>,
    pub parent_spark_id: Option<String>,
    pub created_at: String,
}

pub struct NewCrew {
    pub name: String,
    pub purpose: Option<String>,
    pub workshop_id: String,
    pub head_session_id: Option<String>,
    pub parent_spark_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CrewMember {
    pub id: i64,
    pub crew_id: String,
    pub session_id: String,
    pub role: Option<String>,     // "hand" | "merger"
    pub joined_at: String,
}
```

Add `Merger` variant:

```rust
pub enum AssignmentRole { Owner, Assistant, Observer, Merger }
// as_str: "merger"
```

### Repo (`data/src/sparks/crew_repo.rs`, new)

Functions:

- `create(pool, NewCrew) -> Crew`
- `get(pool, &id) -> Crew`
- `list_for_workshop(pool, ws_id) -> Vec<Crew>`
- `add_member(pool, &crew_id, &session_id, role: Option<&str>) -> CrewMember`
- `remove_member(pool, &crew_id, &session_id) -> ()`
- `members(pool, &crew_id) -> Vec<CrewMember>`
- `set_status(pool, &id, &status) -> ()`
- `attach_sparks(pool, &crew_id, &[spark_id]) -> ()` — bonds the crew's
  parent spark to the children via existing `bond_repo` (`ParentChild`).

Register in `data/src/sparks/mod.rs`. Re-exports from `lib.rs`.

## CLI surface (`src/cli.rs`)

The Head is a coding agent — its hands are the `ryve` CLI. New
subcommands:

```
ryve crew create <name> [--purpose <text>] [--parent <spark_id>] [--head-session <id>]
ryve crew list                              # active crews in this workshop
ryve crew show <crew_id>                    # crew + members + sparks
ryve crew add-member <crew_id> <session_id> [--role hand|merger]
ryve crew remove-member <crew_id> <session_id>
ryve crew status <crew_id> active|merging|completed|abandoned

ryve hand spawn <spark_id> [--agent claude|codex|aider|opencode]
                            [--role owner|merger] [--crew <crew_id>]
ryve hand list                              # delegates to existing assignment_repo::list_active
```

`ryve hand spawn` is the most important new command — it lets the Head
programmatically launch a sub-Hand without going through the UI:

1. Resolve the coding agent from `coding_agents::known_agents()`.
2. Generate a session id (`uuid v4`).
3. Call the existing `create_hand_worktree(workshop_dir, ryve_dir,
   session_id)` (extracted from `workshop.rs` into a public helper in
   `data/src/git.rs` or `src/hand_spawn.rs`).
4. `agent_session_repo::create` with the new session.
5. `assignment_repo::assign` with the requested role.
6. If `--crew` is set, `crew_repo::add_member`.
7. Compose the initial prompt with `compose_hand_prompt` (or
   `compose_merger_prompt` for `--role merger`).
8. `tokio::process::Command::new(agent.command)` with the
   system-prompt flag, full-auto flag, env vars (`RYVE_WORKSHOP_ROOT`,
   `PATH`), `current_dir(worktree_path)`, redirect stdout/stderr to
   `.ryve/logs/hand-<session_id>.log`, **detach** via
   `process::Stdio::null()` and `setsid`/`CREATE_NEW_PROCESS_GROUP`.
9. Print the session id (and `--json` mode the full record).

The detached subprocess survives Ryve restarts and shows up in the
Hands panel automatically because the next workgraph poll picks up the
new `agent_sessions` row (`load_agent_sessions` already exists at
`src/main.rs:2050`).

Register the new commands in `CLI_COMMANDS` and the dispatch `match`.

## Head and Merger system prompts

Both prompts live in a new module `src/agent_prompts.rs` (or extend
`src/main.rs` neighbours). The existing `compose_hand_prompt` moves
there too so all prompt-composition is in one place.

### `compose_head_prompt(workshop_root, user_goal: Option<&str>) -> String`

Tells the Head:

> You are the **Head** of a Crew in a Ryve workshop at
> `{workshop_root}`. The user has asked you to: "{user_goal}".
>
> Your job:
> 1. Use `ryve spark list --json` to read the workgraph.
> 2. Decompose the goal into 2–8 task sparks. Create them with
>    `ryve spark create --type task --priority N --acceptance "..." "<title>"`.
> 3. Create a Crew: `ryve crew create "<name>" --purpose "<goal>"
>    --parent <epic_spark_id> --head-session $RYVE_SESSION_ID`.
> 4. For each child spark, spawn a Hand:
>    `ryve hand spawn <spark_id> --agent <agent> --crew <crew_id>`.
> 5. Poll progress with `ryve crew show <crew_id>` every minute.
>    Use `ryve assignment list <spark_id>` to check heartbeats.
>    If a Hand's heartbeat is older than 2 minutes and its spark is
>    not closed, retire it (`ryve assign release ...`) and respawn.
> 6. When all child sparks are closed, create a merge spark and
>    `ryve hand spawn <merge_spark_id> --role merger --crew <crew_id>`.
> 7. When the merger reports a PR URL, post it as a comment on the
>    parent epic and exit.
>
> Hard rules:
> - Never edit `.ryve/sparks.db` directly. Always go through `ryve`.
> - Never make architectural decisions on the user's behalf — when in
>   doubt, post a question on the parent spark with `ryve comment add`
>   and wait one poll cycle.
> - Never run destructive git/shell commands yourself. Hands and the
>   Merger do that.
> - The user can pause you at any time by closing your bench tab or
>   by closing sparks out from under you. Respect both.

### `compose_merger_prompt(crew_id, member_spark_ids) -> String`

Tells the Merger Hand:

> You are the **Merger** for crew `{crew_id}`. The other members of
> your crew worked in worktrees under `.ryve/worktrees/<short>/` on
> branches named `hand/<short>`. Your job:
>
> 1. Wait until every spark in `ryve crew show {crew_id}` is closed
>    with status `completed`. Poll every 30 s.
> 2. From the workshop root, create an integration branch:
>    `git checkout -b crew/{crew_id} main`.
> 3. For each member branch in order, `git merge --no-ff hand/<short>`.
>    Resolve conflicts; if you cannot, post a comment on the merge
>    spark and `ryve spark status <id> blocked`, then exit.
> 4. Push: `git push -u origin crew/{crew_id}`.
> 5. Open a single PR with `gh pr create` listing every member spark
>    in the body.
> 6. Post the PR URL as a comment on the merge spark, mark it
>    completed, and exit.
> 7. Do **not** merge to main automatically — that requires human
>    review approval.

## Files

### New

- `data/src/sparks/crew_repo.rs` — repo described above.
- `data/migrations/005_crew_status_fields.sql` — adds `status`,
  `head_session_id`, `parent_spark_id` to `crews`. Idempotent
  `ALTER TABLE` with `IF NOT EXISTS` semantics replicated via the
  pattern in `004_workgraph_enhancements.sql`.
- `src/agent_prompts.rs` — `compose_hand_prompt` (moved),
  `compose_head_prompt`, `compose_merger_prompt`.
- `src/cli/mod.rs`, `src/cli/crew.rs`, `src/cli/hand.rs` — split the
  growing `cli.rs` into a module folder. Existing handlers move into
  `cli/spark.rs`, `cli/bond.rs`, etc., as part of the same change so
  the file stays scannable. (Optional refactor — can keep flat if
  reviewer prefers; defaulting to flat for minimum churn: just add
  `handle_crew` and `handle_hand_spawn` in `cli.rs`.) **Decision:
  flat — single `cli.rs` keeps the diff focused.**
- `tests/cli_head.rs` (root crate integration test) — invokes the
  built `ryve` binary in a temp workshop and asserts:
  1. `ryve crew create` creates a row visible to `ryve crew list`.
  2. `ryve hand spawn <spark> --agent echo` (using a stub agent
     pointing at `/bin/echo` registered via env var) creates a
     session row, an active assignment, and a worktree.
  3. `ryve crew add-member` + `crew show` shows the membership.
  4. `Merger` role round-trips through `assignment_repo::assign`.

### Modified

- `data/src/sparks/types.rs` — add `Crew`, `NewCrew`, `CrewMember`,
  `AssignmentRole::Merger`. Update `as_str` and `from_str`-style
  helpers.
- `data/src/sparks/mod.rs` — `pub mod crew_repo;`
- `data/src/lib.rs` — no change (re-exports flow through `sparks::`).
- `data/src/migrations.rs` — bump `CURRENT_SCHEMA_VERSION`, add
  migration entry. (Note: that file handles workshop *config*
  migrations; sqlx db migrations are picked up automatically from
  `data/migrations/`. Confirm against `data/src/db.rs` before bumping.)
- `src/coding_agents.rs` — no change (already exposes
  `system_prompt_flag` and `full_auto_flags`).
- `src/screen/bench.rs` — extend `Message` enum with `NewHead` and
  `NewHand` variants; redesign the dropdown to surface "New Head",
  "New Hand", "New Terminal" as the top items, with the per-agent
  list relegated to a submenu (or removed in favor of the picker).
- `src/screen/spark_picker.rs` — add a coding-agent selector to the
  picker view; emit `SelectSpark { spark_id, agent }`.
- `src/main.rs` —
  - move `compose_hand_prompt` to `agent_prompts.rs`,
  - add `Message::NewHead`/`Message::NewHand` handlers,
  - implement `spawn_head` (mirrors `spawn_pending_agent` but no spark
    assignment, injects Head system prompt, registers session as
    `session_label = Some("head")`, and creates a placeholder Crew row
    via `crew_repo::create`),
  - update `spawn_pending_agent` to read the agent from the picker
    payload instead of the prior dropdown click.
- `src/cli.rs` —
  - add `"crew" | "crews" | "hand" | "hands"` to `CLI_COMMANDS`,
  - dispatch to `handle_crew` and `handle_hand`,
  - extend `print_usage` with the new commands,
  - implement `handle_crew` (create/list/show/add-member/remove-member/status)
    using `crew_repo`,
  - implement `handle_hand` with the `spawn` and `list` subcommands.
- `src/workshop.rs` — make `create_hand_worktree` and `hand_env_vars`
  `pub(crate)` (or move them to a new `src/hand_spawn.rs`) so the CLI
  handler can call them. The CLI handler cannot call into iced
  state, so the worktree+session+launch logic must be split into a
  pure async function that takes (`workshop_dir`, `ryve_dir`, `pool`,
  `agent`, `spark_id`, `role`, `crew_id`) and returns the new
  `session_id`. Both the UI's `spawn_pending_agent` and the CLI's
  `handle_hand_spawn` will call this shared helper.

## Acceptance-criterion mapping

| Criterion | How it is satisfied |
|-----------|--------------------|
| Head can decompose a user prompt into sparks | Head is a coding agent with the `compose_head_prompt` system prompt that instructs it to call `ryve spark create`. The agent's LLM does the decomposition; Ryve gives it the tools. |
| Head can spawn Hands and assign sparks to them | `ryve hand spawn <spark_id> --agent <a> --crew <c>` is the new CLI verb the Head invokes. It creates a worktree, persists a session, calls `assignment_repo::assign`, and launches a detached coding-agent subprocess. |
| Head monitors Hand progress via workgraph polling | Head loops on `ryve crew show <crew_id>` and `ryve assignment list <spark_id>` (existing repos already expose heartbeat and status). |
| Head reassigns work when a Hand fails or goes stale | Head sees stale heartbeats via `assignment list`, calls `ryve assign release …` then `ryve hand spawn …` again. Backed by existing `assignment_repo::abandon` + `assign`. |
| User can pause/resume/override Head at any time | The Head is a normal bench tab — the user can close it, send it text, or close any spark/crew out from under it. The Head's prompt requires it to honor those changes on the next poll. |

## Invariant compliance

- **Workgraph as the only coordination mechanism** — every Head action
  goes through the `ryve` CLI, which goes through the existing repos,
  which fire `event_repo::record`. No direct sqlx, no shared memory.
- **No bypassing the workgraph** — `crew_repo` is the only writer to
  `crews`/`crew_members`; `crew_repo::add_member` is the only path to
  membership; `crew_repo::attach_sparks` uses existing `bond_repo`.
- **User override** — closing a tab kills the Head process; closing a
  spark causes the Head's next `ryve spark list` to omit it; the Head
  is required by its prompt to recompute on every poll.
- **Headless** — clarified: not required. Head launches into a normal
  bench tab. The original "no terminal tab required" line is
  superseded by the user's clarification.
- **No destructive auto** — only the Merger Hand runs destructive git
  commands, and only against an integration branch + PR. Merge to
  main always requires human PR review.

## Tests

1. `data/tests/crew_repo.rs` — round-trip create/list/get/add_member/
   remove_member/set_status against an in-memory pool.
2. `data/tests/assignment_role_merger.rs` — assert
   `AssignmentRole::Merger` persists through `assignment_repo::assign`
   and `list_for_session`.
3. `tests/cli_crew.rs` — exec the built `ryve` binary in a temp
   workshop, verify `crew create`/`crew list`/`crew show`/
   `crew add-member` end to end with `--json` parsing.
4. `tests/cli_hand_spawn.rs` — same, but uses an `--agent` whose
   command is `/bin/echo` (stubbed via env var) so we don't depend on
   `claude` being installed. Asserts a worktree was created, the
   `agent_sessions` row exists, the `hand_assignments` row exists, and
   the echo log file was written.
5. `tests/agent_prompts.rs` — snapshot tests on
   `compose_head_prompt` / `compose_merger_prompt` to lock the
   instructions and prevent regressions.
6. UI: manual smoke test (no automation) — open Ryve, click
   **+ → New Head**, watch a Crew get created, see Hands appear in the
   Hands panel after the Head calls `ryve hand spawn`.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test -p data crew_repo
cargo test -p data assignment_role_merger
cargo test cli_crew cli_hand_spawn agent_prompts
cargo build --release        # ensure UI compiles
```

End-to-end (manual, in this worktree):

```sh
# 1. Build & install
cargo build && export PATH="$PWD/target/debug:$PATH"

# 2. Verify CLI surface
ryve crew create "demo" --purpose "smoke test"
ryve crew list
ryve hand spawn <some_spark> --agent echo --crew <crew_id>
ryve crew show <crew_id>      # member appears
ryve assignment list <spark>  # owner == new session id

# 3. Open Ryve UI, click + → New Head with claude (or codex),
#    paste a small goal, watch the Head create sparks + spawn Hands.
```

Then verify against `.ryve/checklists/DONE.md` and close:

```sh
ryve spark close sp-ux0035 completed
```

## Out of scope (deferred to follow-up sparks)

- Crew dashboard screen (rolled-up status across all crews).
- Real-time Head→user notifications (we rely on the bench tab today).
- Merger conflict-resolution UI.
- IPC channel between CLI-spawned Hands and the running Ryve UI for
  instant tab insertion (UI picks them up via the polling loop within
  ~3 s, which is acceptable for v1).
- LLM-side validation of Head's decomposition quality.
