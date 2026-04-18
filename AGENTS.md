# Ryve Workshop

You are a **Hand** working inside a **Ryve Workshop**. Ryve manages tasks (called *sparks*) in an embedded workgraph stored in `.ryve/sparks.db`.

**IMPORTANT: Work in your current directory.** Do not navigate to parent directories or other worktrees. All code changes, commits, and CLI commands must be run from within this working tree.

## Getting Started

Before doing any work, check the current workgraph state with `ryve spark list` to see active sparks, their priorities, and which are already claimed. Prefer `ryve hot` for a ready-to-work view that filters out sparks blocked by bonds.

## Rules

1. **Always reference spark IDs** in commit messages: `fix(auth): validate token expiry [sp-a1b2]`
2. **Work in priority order** — P0 is critical, P4 is negligible.
3. **Respect architectural constraints** — run `ryve constraint list` to check. Violations are blocking.
4. **Check required contracts** before considering a spark done: `ryve contract list <spark-id>`.
5. **Check bonds before claiming a spark** — run `ryve bond list <spark-id>`. If the spark is the target of a `blocks` or `conditional_blocks` bond whose source is not yet completed, do NOT start it. Pick a different spark or use `ryve hot` to see only unblocked work.
6. **Do not work on a spark that is already claimed** by another Hand.
7. If you discover a new bug or task, create a spark for it (see commands below).

## Spark Intent

Every spark can carry a structured **intent** that spells out what "done" actually means. Always read it with `ryve spark show <id>` before writing code.

- **problem_statement** — the concrete problem the spark is solving (the *why*).
- **invariants** — properties that MUST hold throughout and after your change. Violating an invariant means the spark is not done, even if the feature works.
- **non_goals** — things explicitly out of scope. Do not expand the spark to cover them; file a new spark instead.
- **acceptance_criteria** — the checklist that must pass before the spark can be closed. Each criterion should be verifiable.

When creating sparks, pass intent via flags on `ryve spark create`: `--problem`, `--invariant` (repeatable), `--non-goal` (repeatable), `--acceptance` (repeatable). Example:

```sh
ryve spark create --type bug --priority 1 \
  --problem 'tokens survive logout' \
  --invariant 'session table is empty after logout' \
  --non-goal 'refresh token rotation' \
  --acceptance 'integration test: logout then /me returns 401' \
  'auth: purge session on logout'
```

## Bonds (Dependencies)

Bonds are directed edges between sparks. They tell Hands which work is actually ready and which must wait. **Check bonds before starting a spark.**

Bond types:

- `blocks` — source must complete before target can start. **Blocking.**
- `conditional_blocks` — blocks only under a runtime condition. **Blocking until resolved.**
- `waits_for` — soft ordering hint; target should wait but isn't hard-blocked.
- `parent_child` — target is a subtask of source (used for epics).
- `related` — informational cross-link; no ordering.
- `duplicates` — target duplicates source; one should be closed.
- `supersedes` — target replaces source.

```sh
ryve bond list <spark-id>                 # all bonds touching this spark
ryve bond create <from> <to> blocks        # add a blocking dependency
ryve bond delete <bond-id>                 # remove a bond
ryve hot                                   # sparks with no unmet blocking bonds
```

## Workgraph Commands

Use `ryve` to query and update the workgraph. **Always run from the workshop root.**

### Query state

```sh
ryve spark list                       # active sparks
ryve spark list --all                 # include closed
ryve hot                              # sparks unblocked by bonds (ready to work)
ryve spark show <spark-id>            # spark details + intent
ryve bond list <spark-id>             # dependency bonds
ryve constraint list                  # architectural constraints
ryve contract list <spark-id>         # verification contracts
ryve ember list                       # live signals from other Hands / the UI
```

### Mutate state

```sh
ryve spark create <title>                           # create a task spark
ryve spark create --type bug --priority 1 \
  --problem '...' --invariant '...' \
  --non-goal '...' --acceptance '...' <title>       # create with structured intent
ryve spark edit <spark-id> --title <t> \
  --priority <0-4> --risk <level> --scope <path>    # edit fields in place
ryve spark status <spark-id> in_progress            # claim / update status
ryve spark close <spark-id> <reason>                # close a spark

ryve bond create <from> <to> <type>                 # add dependency (blocks, related, ...)
ryve bond delete <bond-id>                          # remove a bond

ryve comment add <spark-id> <body>                  # leave a note on a spark
ryve stamp add <spark-id> <label>                   # tag a spark
ryve contract add <spark-id> <kind> <description>   # add a verification contract
ryve contract check <contract-id> pass|fail         # record a contract result

ryve ember send <type> <content>                    # broadcast an ember signal
ryve ember sweep                                    # clean up expired embers
```

Ember types, in order of urgency: `glow` (ambient), `flash` (quick heads-up), `flare` (needs attention soon), `blaze` (urgent — interrupt-worthy), `ash` (archival / post-mortem).

Ryve auto-refreshes every 3 seconds. Changes are picked up by the UI and other Hands automatically.

## Alloys (Spark Groupings)

An **alloy** is a named bundle of sparks that should be executed together. Alloys let a planner stage a group of related work up front so Hands can pick them up in the right shape. Alloys are a planning aid — individual spark lifecycle (status, bonds, contracts) still applies to each member.

Alloy types:

- `scatter` — fan-out: members are independent and can be worked in parallel by multiple Hands.
- `chain` — sequential: members must be completed in order. Each member typically has a `blocks` bond on the next.
- `watch` — observation group: members share a watch/monitor relationship (e.g. a spark plus the checks that gate it).

Alloys are currently managed from the Ryve UI and internal APIs — there is no top-level `ryve alloy` CLI subcommand yet. When you encounter an alloy membership on a spark, treat it as planning context: respect the implied ordering (for chains) or parallelism (for scatters) when choosing what to work on.

## Workflow

- **Claim a spark** before starting work to prevent duplicate effort.
- **Read the spark intent** (`ryve spark show <id>`) before coding — it defines problem, invariants, non-goals, and acceptance criteria.
- **Inspect bonds** (`ryve bond list <id>` or `ryve hot`) — Do not work on a blocked spark that is still waiting on an incomplete upstream.
- **Reference spark IDs** in commit messages (e.g. `fix(auth): validate token expiry [sp-a1b2]`).
- **Focus on priority order** — P0 sparks are critical, P4 are negligible.
- **Respect architectural constraints** — violations are blocking.
- **Check required contracts** before marking a spark as done.
- If you discover a new bug or task while working, mention it so it can be tracked as a new spark.
- Do not close or modify sparks directly — Ryve manages spark lifecycle.

<!-- RYVE:START -->
## Ryve Workshop — MANDATORY

This project is managed by **Ryve**. You MUST read `RYVE.md` before doing ANY work — it is the universal CLI reference, house rules, and coordination protocol for all coding agents.

**Work in your current directory.** Do not navigate to parent directories or other worktrees. Run `ryve spark list` to see active tasks and find work to claim.
<!-- RYVE:END -->
