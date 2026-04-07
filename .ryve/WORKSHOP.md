# Ryve Workshop

You are a **Hand** working inside a **Ryve Workshop**. Ryve manages tasks (called *sparks*) in an embedded workgraph stored in `.ryve/sparks.db`.

**IMPORTANT: Work in your current directory.** Do not navigate to parent directories or other worktrees. All code changes, commits, and CLI commands must be run from within this working tree.

## Getting Started

Before doing any work, check the current workgraph state with `ryve spark list` to see active sparks, their priorities, and which are already claimed.

## Rules

1. **Always reference spark IDs** in commit messages: `fix(auth): validate token expiry [sp-a1b2]`
2. **Work in priority order** — P0 is critical, P4 is negligible.
3. **Respect architectural constraints** — run `ryve constraint list` to check. Violations are blocking.
4. **Check required contracts** before considering a spark done: `ryve contract list <spark-id>`.
5. **Do not work on a spark that is already claimed** by another Hand.
6. If you discover a new bug or task, create a spark for it (see commands below).

## Workgraph Commands

Use `ryve` to query and update the workgraph. **Always run from the workshop root.**

### Query state

```sh
ryve spark list              # active sparks
ryve spark list --all         # include closed
ryve spark show <spark-id>    # spark details
ryve constraint list           # architectural constraints
ryve contract list <spark-id>  # verification contracts
```

### Mutate state

```sh
ryve spark create <title>                    # create a new spark
ryve spark status <spark-id> in_progress      # claim / update status
ryve spark close <spark-id> <reason>           # close a spark
```

Ryve auto-refreshes every 3 seconds. Changes are picked up by the UI and other Hands automatically.

## Workflow

- **Claim a spark** before starting work to prevent duplicate effort.
- **Reference spark IDs** in commit messages (e.g. `fix(auth): validate token expiry [sp-a1b2]`).
- **Focus on priority order** — P0 sparks are critical, P4 are negligible.
- **Respect architectural constraints** — violations are blocking.
- **Check required contracts** before marking a spark as done.
- If you discover a new bug or task while working, mention it so it can be tracked as a new spark.
- Do not close or modify sparks directly — Ryve manages spark lifecycle.
