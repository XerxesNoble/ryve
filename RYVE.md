# RYVE.md

Skill file for any coding agent working inside a Ryve-managed repository. Read this first.

Ryve is a task manager and orchestration layer that lives in `.ryve/` and exposes a single CLI: `ryve`. All work — claiming tasks, recording decisions, opening dependencies, spawning sub-agents — goes through this CLI. There is no other source of truth.

## Prerequisites

- Run every command from the repository root (the directory containing `.ryve/`).
- The CLI binary is `ryve`. In unbuilt checkouts it is also available as `./target/debug/ryve`.
- Never touch `.ryve/sparks.db` with `sqlite3` or any other tool — that bypasses event logging and schema validation. Use the CLI.
- Work only in your current working tree. Do not `cd` into sibling worktrees or the parent of the repo.

## Concepts

| Term | Meaning |
|---|---|
| **Spark** | A unit of work. Types: `bug`, `feature`, `task`, `epic`, `chore`, `spike`, `milestone`. Every spark has a status, priority (P0–P4), risk, and structured intent. |
| **Intent** | The structured definition of "done" on a spark: `problem_statement`, `invariants`, `non_goals`, `acceptance_criteria`. Read it before writing code. |
| **Bond** | A directed edge between sparks. Types: `blocks`, `conditional_blocks`, `waits_for`, `parent_child`, `related`, `duplicates`, `supersedes`. `blocks` and `conditional_blocks` are hard dependencies. |
| **Hand** | A worker agent owning one spark in its own git worktree. |
| **Head** | An orchestrator that owns an epic, decomposes it into children, and supervises a Crew of Hands. Never edits code itself. |
| **Atlas** | The top-level Director that talks to the user and delegates to Heads. Singleton per workshop. Never edits code itself. |
| **Crew** | A bundle of Hands working on the children of one epic under one Head. |
| **Merger** | A Hand whose only job is to integrate the Crew's member branches into one PR. |
| **Contract** | A verification requirement attached to a spark. Required contracts must pass before the spark can close. |
| **Constraint** | A project-wide architectural rule. Violations are blocking regardless of spark. |
| **Ember** | A short-lived signal between Hands / the UI. Tiers: `glow`, `flash`, `flare`, `blaze`, `ash`. |
| **Alloy** | A named bundle of sparks meant to be executed together. Types: `scatter` (parallel), `chain` (sequential), `watch` (monitor). |
| **Release** | A versioned group of epics (v0.1.0 etc.) with its own branch and close-out flow. |

## Agent hierarchy

```
User → Atlas (Director) → Head (Crew orchestrator) → Hands (workers) → Merger (integrator)
```

Each layer is a separate coding-agent subprocess. Roles are distinguished by system prompt and `session_label`, not by binary.

## CLI reference

### Query state
```sh
ryve status                             # workshop summary
ryve hot                                # sparks with no unmet blocking bonds (ready to claim)
ryve spark list                         # active sparks
ryve spark list --all                   # include closed
ryve spark show <spark-id>              # details + intent
ryve bond list <spark-id>               # dependency bonds
ryve constraint list                    # architectural constraints
ryve contract list <spark-id>           # verification contracts
ryve contract failing                   # all failing required contracts
ryve event list <spark-id>              # audit trail for a spark
ryve ember list                         # live signals
ryve crew list                          # active crews
ryve crew show <crew-id>                # crew + its members
ryve head list                          # active Head sessions
ryve hand list                          # active Hand assignments
ryve release list                       # releases
ryve release show <release-id>          # release + member epics
```

Add `--json` to any command for machine-readable output: `ryve --json spark show <id>`.

### Create / mutate sparks
```sh
ryve spark create <title>                                      # task under no parent (rare)
ryve spark create --type epic <title>                          # top-level epic
ryve spark create --parent <epic-id> <title>                   # child task under an epic
ryve spark create --type bug --priority 1 \
    --scope 'src/auth/' \
    --problem '<why>' \
    --invariant '<must hold>' --invariant '<...>' \
    --non-goal '<out of scope>' \
    --acceptance '<verifiable>' --acceptance '<...>' \
    '<title>'
ryve spark create --help                                       # full flag list

ryve spark edit <id> --title <t> --priority <0-4> --risk <level> --scope <boundary>
ryve spark status <id> <new_status>                            # open | in_progress | blocked | ...
ryve spark close <id> [reason]                                 # close a spark
```

`--scope` is the concrete file/directory boundary the spark touches. Always set it — it drives the merge-clean bond-discipline check (see below).

### Bonds
```sh
ryve bond list <spark-id>
ryve bond create <from> <to> <type>                            # types listed in Concepts table
ryve bond delete <bond-id>
```

### Comments, stamps, contracts
```sh
ryve comment add <spark-id> <body>
ryve comment list <spark-id>

ryve stamp add <spark-id> <label>
ryve stamp remove <spark-id> <label>
ryve stamp list <spark-id>

ryve contract add <spark-id> <kind> <description>
ryve contract check <contract-id> pass|fail
```

### Assignments (claim / release)
```sh
ryve assign list <spark-id>                                    # current owner + heartbeat
ryve assign claim <session-id> <spark-id>
ryve assign release <session-id> <spark-id>
```

### Spawning agents
```sh
ryve hand spawn <spark-id> [--agent <name>] [--role owner|head|merger] [--crew <id>]
ryve head spawn <epic-id> --archetype <build|research|review> [--agent <n>] [--crew <id>]
ryve head orchestrate <parent-spark> --children <csv> \
    [--merge-spark <id>] [--crew-name <n>] [--agent <n>] \
    [--stall-seconds <N>] [--poll-seconds <M>] [--max-cycles <N>]
ryve head archetype list
ryve head render <archetype> --epic <id>                       # dry-run prompt

ryve crew create <name> [--purpose <t>] [--parent <spark-id>] [--head-session <id>]
ryve crew add-member <crew-id> <session-id> [--role hand|merger]
ryve crew remove-member <crew-id> <session-id>
ryve crew status <crew-id> active|merging|completed|abandoned
```

### Embers (live signals)
```sh
ryve ember send <type> <content>        # type: glow | flash | flare | blaze | ash
ryve ember sweep                        # clean up expired
```

### Chat-of-record (post + channel tail)
```sh
ryve post --channel <name> <body>                                      # write one chat-of-record line
ryve channel tail --channel <name> [--since <ts>] [--limit <N>] [--author <actor_id>]
```

The full contract — mandatory posting boundaries, channel naming, tool-gated on-close enforcement — lives in the **Chat-of-record** section below. Epic `ryve-12f09190`.

### Commits
```sh
ryve commit link <spark-id> <hash>      # link a commit to a spark
ryve commit list <spark-id>
ryve commit scan                        # scan git log for [sp-xxxx] references
```

### Releases
```sh
ryve release create <major|minor|patch>
ryve release add-epic <release-id> <epic-id>
ryve release remove-epic <release-id> <epic-id>
ryve release status <release-id> <new_status>
ryve release close <release-id>         # verify, tag, build, record
```

### Backups & worktrees
```sh
ryve backup create                      # snapshot sparks.db
ryve backup list
ryve backup prune [--keep=N]
ryve restore <snapshot>

ryve worktree prune [--yes]             # remove stale Hand worktrees
ryve wt prune                           # alias
```

## House rules

1. **Reference spark IDs in every commit message.** Format: `fix(auth): validate token [sp-a1b2]`. The commit scanner uses this to link work.
2. **Work in priority order.** P0 critical, P4 negligible. Prefer `ryve hot` to find unblocked work at the highest priority.
3. **Check bonds before claiming.** Do not start a spark that is the target of an unmet `blocks` or `conditional_blocks`.
4. **Do not claim a spark another agent owns.** Check `ryve assign list <id>` and the last heartbeat before claiming.
5. **Respect architectural constraints.** Run `ryve constraint list` — violations are blocking.
6. **Required contracts must pass before a spark closes.** Run `ryve contract list <id>` and resolve failing ones.
7. **Hands never work in the main tree.** Each Hand has its own worktree under `.ryve/worktrees/<short>/`. Never run code changes, commits, or destructive git commands from the workshop root.
8. **Never modify applied sqlx migrations.** The migration files in `data/migrations/` are checksummed; comment-only edits break the tracker. Add a new migration file instead.
9. **New work belongs to an epic.** A task spark without a parent epic is an orphan and may be rejected by the validator.
10. **If you discover something, file a spark.** Don't expand scope — `ryve spark create` with the right parent and get back to your current work.

## Merge-clean bond discipline (for Heads)

When a Head decomposes an epic into child sparks, it MUST serialise any siblings whose file scopes overlap, so the Merger can integrate them cleanly.

Rule:
1. Every child spark is created with `--scope` set to the concrete files/directories it touches.
2. For every pair of siblings whose scopes touch the same file (including shared `mod.rs`, `Cargo.toml`, a migrations directory, etc.), add a `blocks` bond: `ryve bond create <earlier> <later> blocks`.
3. Only file-disjoint siblings may run in parallel.

The Merger integrates child branches sequentially onto the epic branch and cannot mechanically resolve same-file conflicts. An unresolvable Merger conflict is a bond-discipline failure by the Head, not a merge-time problem.

## Merger isolation

The Merger NEVER changes the branch checked out in the workshop root. All merge work happens in a dedicated worktree created with `git worktree add .ryve/worktrees/merge-<crew-id> -b crew/<crew-id> origin/main`. Forbidden in the workshop root: `git checkout`, `git switch`, `git pull`, `git reset --hard`. Fetches are fine — they don't move HEAD.

## Chat-of-record

Every agent in a Ryve workshop writes its intent, plans, design picks, blocks, and hand-offs to an IRC channel that is durably recorded in `irc_messages`. This is the record layer that makes sudden-death recovery, cross-agent context, and Merger read-back possible without manual archaeology. Epic `ryve-12f09190`.

**Channels.**
- Hands and Heads working on an epic's children post to that epic's channel, `#epic-<epic_id>-<slug>`. Derive the channel from the parent epic id (the canonical form is produced by `ipc::channel_manager::channel_name`; the IRC 50-octet limit truncates long names).
- Atlas posts to the workshop-wide `#atlas` channel (boot, routing decisions, shutdown).

**Mandatory posting boundaries.** Every agent posts at each of these transitions:

| Boundary    | When                                                             | Example post                                                          |
|-------------|------------------------------------------------------------------|-----------------------------------------------------------------------|
| claim       | right after claiming / entering in_progress                      | `claim: starting on <task>; plan is <outline> [sp-xxxx]`              |
| design-pick | when selecting an approach, library, boundary, or scope split   | `design: chose <A> over <B> because <reason> [sp-xxxx]`               |
| block       | when stuck on an obstacle, ambiguity, or bond-discipline clash   | `block: tests red on <file>; escalating as <question> [sp-xxxx]`      |
| commit      | after each non-trivial commit                                    | `commit: wired <feature> into <module> [sp-xxxx]`                     |
| handoff     | before `ryve spark close` / `ryve assign release`                | `handoff: shipped X/Y; next agent should do Z [sp-xxxx]`              |

Atlas replaces `claim`/`commit` with `boot`/`route` since Atlas never claims sparks or commits code itself.

**On-close enforcement is tool-gated.** `ryve spark close` and `ryve assign close` refuse to close an assignment with zero chat-of-record posts since its claim timestamp. This is not prompt-only rhetoric — the tool bounces the close if the record is empty. Fix it by posting, not by arguing with the prompt.

### Writing: `ryve post`

```sh
ryve post --channel <name> <body>                            # returns the message id
ryve post --channel <name> --author <actor_id> <body>         # override the default author
ryve post --channel <name> --author "" <body>                 # anonymous (human CLI use)
```

The `--author` default is `$RYVE_HAND_SESSION_ID` when the caller is a spawned Hand, so posts are automatically attributable. The DB write is the durability contract — IRC wire delivery via the outbox relay is best-effort.

Examples:

```sh
ryve post --channel '#epic-ryve-12f09190-chat-of-record-mandatory-agent' \
    'claim: starting ryve-73fd1c5f — wiring CHAT_OF_RECORD into archetype prompts [sp-73fd1c5f]'

ryve post --channel '#atlas' \
    'route: user goal "stabilise release 0.3.0" → build head on ryve-d20d42cb'
```

### Reading: `ryve channel tail`

```sh
ryve channel tail --channel <name>                            # default limit=50, from channel start
ryve channel tail --channel <name> --since <rfc3339-ts>       # incremental read
ryve channel tail --channel <name> --limit <N>                # up to 1000
ryve channel tail --channel <name> --author <actor_id>        # filter by author
```

Add `--json` for machine-readable output (e.g. `ryve --json channel tail --channel <name>`).

Examples:

```sh
ryve channel tail --channel '#epic-ryve-12f09190-chat-of-record-mandatory-agent' --limit 20

ryve channel tail --channel '#atlas' \
    --since 2026-04-19T00:00:00Z --author atlas-<prior_session_id>
```

Post before acting, tail before handing off. Every agent reads the channel on claim to inherit prior context and posts on handoff so the next one can resume.

## Common workflows

### Claim and work a spark (Hand)
```sh
ryve hot                                        # find an unblocked spark
ryve spark show <id>                            # read intent
ryve assign claim $RYVE_SESSION_ID <id>         # claim it
ryve spark status <id> in_progress
# … do the work inside your worktree …
git commit -m 'feat: add <thing> [sp-<id>]'
ryve contract list <id>                         # resolve any required contracts
ryve spark close <id> completed
```

### Decompose an epic (Head)
```sh
ryve spark show <epic-id>                                   # read intent
# For each discrete piece of work:
ryve spark create --type task --priority 2 \
    --scope '<files>' --acceptance '<criterion>' \
    --parent <epic-id> '<title>'
# Apply bond discipline: for each pair of scope-overlapping siblings,
ryve bond create <earlier> <later> blocks
# Then orchestrate:
ryve crew create '<name>' --parent <epic-id> --head-session $RYVE_SESSION_ID
ryve head orchestrate <epic-id> --children <child1>,<child2>,...
```

### Respond to a user goal (Atlas)
```sh
ryve spark list                                             # check existing work
ryve spark create --type epic --priority 1 \
    --problem '<goal>' --acceptance '<measurable>' '<title>'
ryve head spawn <epic-id> --archetype <build|research|review> --agent <name>
# Poll with a recurring check-in; re-spawn the Head if its session ends
# while the epic is still open and at least one child is unblocked.
```

## Further reading

Deeper docs live under `docs/`:
- `docs/AGENT_HIERARCHY.md` — Atlas → Head → Hand architecture.
- `docs/HEAD_ARCHETYPES.md` — Build / Research / Review archetypes + cross-archetype invariants.
- `docs/HAND_CAPABILITIES.md` — the twelve Hand classes (Surgeon, Builder, Refactorer, …).
- `docs/DELEGATION_CONTRACTS.md` — `DirectorBrief` / `HeadAssignment` / `HandReturn` / `HeadSynthesis` schemas.
- `docs/ATLAS.md` — Director routing rules.

Source of truth for system prompts: `src/agent_prompts.rs`.
