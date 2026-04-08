<p align="center">
  <img src="./assets/logo.svg" alt="Ryve logo" width="140" />
</p>

<h1 align="center">Ryve</h1>

<p align="center">
  <strong>A native desktop IDE for multi-hand development.</strong>
</p>

<p align="center">
  Ryve coordinates terminals, coding tools, and structured work so parallel development can move fast without pinching, blocking, or kicking back.
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-stable-black?logo=rust" />
  <img alt="License" src="https://img.shields.io/badge/license-AGPL--3.0--or--later-blue" />
  <img alt="Platform" src="https://img.shields.io/badge/platform-desktop-6f42c1" />
</p>

---

## What is Ryve?

**Ryve** is a desktop IDE for managing development work through autonomous coding processes, embedded terminals, and a structured workgraph.

It is named after the **riving knife** on a table saw: the safety device that keeps the cut open and prevents dangerous kickback. In the same way, Ryve keeps parallel development work moving safely by reducing collisions, ambiguity, and coordination failure.

Ryve is built for people who want:

- a native desktop environment
- multiple coding tools running side by side
- a terminal-first workflow
- structured work tracking inside the editor
- better coordination between active workers and the code they touch

---

## Core concepts

| Concept | Meaning |
|---|---|
| **Workshop** | A project workspace opened in Ryve |
| **Bench** | The tabbed work surface for terminals, agents, and file viewers |
| **Hand** | A coding-agent worker process executing one Spark in its own worktree |
| **Head** | An orchestrator coding agent that decomposes work and spawns a Crew of Hands |
| **Crew** | A group of Hands working in parallel under a single Head, merged by a designated Merger Hand |
| **Atlas** | Ryve's primary Director agent — the default user-facing entry point that delegates to Heads (in progress) |
| **Spark** | A unit of work in the workgraph, carrying structured intent |
| **Bond** | A typed dependency or relationship between Sparks |
| **Ember** | A short-lived inter-Hand signal with TTL |
| **Engraving** | Persistent workshop knowledge, including architectural constraints |
| **Alloy** | A planning bundle of related Sparks (scatter / chain / watch) |
| **Contract** | A machine-checkable verification criterion attached to a Spark |

---

## Interface

Ryve combines a file explorer, a tabbed terminal bench, and an embedded workgraph.

```text
┌──────────────────────────────────────────────────────────────────┐
│ Workshop Tabs                                   [+ New Workshop] │
├──────────────────────────────────────────────────────────────────┤
│ File Explorer     │ Bench (tabbed terminals)   │ Workgraph       │
│                   │                             │                 │
│ > src/            │ [Terminal] [Claude] [+]    │ SP-001  P0      │
│   main.rs         │                             │ SP-002  P1      │
│   workshop.rs     │ $ claude --chat            │ SP-003  P2      │
│ > data/           │ > working on feature...    │                 │
│                   │                             │                 │
│ Active Hands      │                             │                 │
│ Claude Code       │                             │                 │
│ Aider             │                             │                 │
└──────────────────────────────────────────────────────────────────┘
```

<p align="center">
  <img src="./assets/example.png" alt="Ryve desktop screenshot" width="800" />
</p>

| Area | Purpose |
|------|---------|
| File Explorer | Project tree with git-aware status display |
| Bench | Tabbed terminal and tool sessions |
| Active Hands | Running worker sessions inside the workshop |
| Workgraph | Sparks, Bonds, and coordination state |

---

## Features

### Native desktop UI

Ryve is built with Iced for a fast, cross-platform Rust desktop experience. Supports both dark and light mode based on system appearance.

### Embedded terminals

Each workshop can run multiple terminal-backed sessions using alacritty-terminal through a patched iced_term integration.

### Multi-tool workflow

Ryve detects supported coding tools on your PATH and can launch them directly into Bench tabs.

Supported tools (must support system prompt injection):

- **Claude Code** (`--system-prompt`)
- **Codex** (`--instructions`)
- **Aider** (`--read`)
- **OpenCode** (`--prompt`)

Only agents that accept system prompt injection are supported. Ryve requires control over each Hand's instructions to enforce workgraph coordination rules.

### Heads, Hands, and Crews

Ryve supports two roles for coding-agent sessions:

- **New Hand** picks a single Spark and launches a coding agent against it.
- **New Head** launches an orchestrator agent that decomposes a goal into Sparks, opens a Crew, and spawns child Hands via the `ryve hand spawn` CLI. A designated **Merger** Hand later integrates the Crew's worktrees into a single PR.

Both flows go through the same `hand_spawn` helper, so UI-spawned and CLI-spawned Hands are mechanically identical and persist across Ryve restarts.

### Worktree isolation

Every Hand spawns in its own git worktree (`.ryve/worktrees/<session>/`) on a `hand/<session>` branch, preventing merge conflicts between concurrent agents working on the same project. Stale worktrees can be cleaned up with `ryve worktree prune`.

### Workgraph-driven coordination

The workgraph is the nervous system of Ryve. Every Hand reads `.ryve/WORKSHOP.md` (injected via system prompt) which contains active sparks, architectural constraints, failing contracts, and coordination rules. The workgraph database is the source of truth; WORKSHOP.md is a generated projection.

The workgraph includes:

- **Sparks** — work items with structured intent (problem, invariants, non-goals, acceptance criteria, risk, scope)
- **Bonds** — typed dependency graph with cycle detection
- **Contracts** — machine-checkable verification criteria (required/advisory)
- **Embers** — ephemeral inter-Hand signals with TTL (`glow` → `blaze` → `ash`)
- **Engravings** — persistent knowledge and architectural constraints
- **Alloys** — planning bundles (scatter/chain/watch)
- **Crews** — Head-led groups of Hands with status, parent spark, and Merger linkage
- **Hand Assignments** — liveness-aware claims with heartbeat, handoff, and lease expiry
- **Agent Sessions** — process-tracked Hand sessions with PID, log path, and parent-Hand linkage
- **Commit Links** — git commits linked to sparks via `[sp-xxxx]` references
- **Open Tabs** — per-workshop bench snapshot, restored on workshop reopen

### `ryve` CLI

The `ryve` binary doubles as a CLI for the workgraph. Hands use it to read state and report progress; Heads use it to spawn child Hands. Major surfaces: `spark`, `bond`, `comment`, `stamp`, `contract`, `constraint`, `ember`, `event`, `assign`, `commit`, `crew`, `hand`, `hot`, `init`, `status`, `worktree prune`. Pass `--json` for machine-readable output.

### Real-time synchronisation

The sparks panel auto-refreshes every 3 seconds, detecting changes made by agents directly in the database. WORKSHOP.md is regenerated on every spark mutation so all Hands see current state.

### Workshop-local state

Each workshop gets its own `.ryve/` directory for config, data, worktrees, and context.

---

## Architecture

Ryve is a Rust workspace made up of focused crates:

| Crate | Purpose |
|-------|---------|
| `src/` | Main desktop application + `ryve` CLI (single binary) |
| `data/` | SQLite persistence, config, git, workgraph repos, agent context, integrations |
| `llm/` | Multi-provider LLM integration |
| `llm/proto/` | Shared protocol types (Thread, Message, Agent) |
| `ipc/` | Single-instance and local coordination support |
| `vendor/iced_term/` | Vendored terminal widget integration |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full layered breakdown.

Built with:

- [Iced](https://iced.rs) — native Rust GUI
- [alacritty-terminal](https://github.com/alacritty/alacritty) — terminal backend
- [sqlx](https://github.com/launchbadge/sqlx) — SQLite access and migrations
- [genai](https://github.com/nickel-org/genai) — multi-provider LLM support
- [tokio](https://tokio.rs) — async runtime

---

## Project layout

```
ryve/
├── Cargo.toml
├── src/                  # desktop app + CLI
│   ├── main.rs           # App state, message routing, Iced lifecycle
│   ├── cli.rs            # ryve CLI dispatch (spark/bond/crew/hand/...)
│   ├── workshop.rs       # per-workshop state and lifecycle
│   ├── hand_spawn.rs     # shared Hand/Head spawn helper (UI + CLI)
│   ├── agent_prompts.rs  # compose_hand_prompt / compose_head_prompt / compose_merger_prompt
│   ├── coding_agents.rs  # PATH detection + system-prompt flag table
│   ├── worktree_cleanup.rs
│   ├── screen/           # bench, sparks, file_explorer, head_picker, spark_detail, log_tail, ...
│   └── widget/           # badge, splitter
├── data/                 # persistence, git, sparks repos, agent_context
│   ├── migrations/       # 001-009 sqlx migrations
│   └── src/sparks/       # spark/bond/crew/contract/assignment/... repos
├── llm/                  # LLM client + protocol types
├── ipc/                  # local process coordination
├── vendor/
│   └── iced_term/        # patched embedded terminal widget
├── assets/
└── docs/                 # ARCHITECTURE.md, HEAD_PLAN.md, WORKGRAPH.md
```

---

## Requirements

- Rust stable
- A desktop OS supported by Iced
- Git installed and available in shell

Recommended:

- Latest stable Rust toolchain
- One or more supported coding CLIs for Hand sessions

---

## Getting started

```sh
git clone https://github.com/loomantix/ryve.git
cd ryve
cargo run
```

### Build

```sh
cargo build
```

### Run checks

```sh
cargo check
cargo test
cargo clippy --all-targets --all-features
```

---

## Status

Ryve is in active development. The core desktop shell, workgraph, multi-Hand coordination, and `ryve` CLI are all working today. Expect rapid iteration.

### Shipped

- Native Iced desktop shell with dark/light theme tracking
- Tabbed Bench with embedded alacritty terminals, file viewers, and a persistent open-tab snapshot
- File explorer with git status, configurable ignore patterns, and an Unsplash/local-image background picker
- Workgraph with sparks, bonds (cycle-checked), contracts, embers, engravings, alloys, crews, contracts, hand assignments, and a full audit/event log
- Workshop-scoped SQLite database with sqlx migrations 001–009
- `ryve` CLI sharing the same binary as the UI; full coverage of workgraph mutations
- Hand spawning with per-session git worktrees, system-prompt injection of `.ryve/WORKSHOP.md`, and PID/log-path tracking
- Head/Hand/Merger orchestration: `ryve hand spawn` lets a Head launch detached children that are auto-discovered by the running UI
- 3-second sparks polling so external CLI mutations show up live in the UI

### In flight (near-term objectives)

- **Execution Workflow Foundation** — append-only `lifecycle_events` table, `assignment_phase` axis, pure transition validator, projector, in-process broadcast bus, and `ryve lifecycle` CLI subcommand
- **Adversarial Review Runtime + IRC Facade** — reviewer Hand role, auto-spawn review/repair loop with cycle caps, and a localhost IRC adapter over the lifecycle stream
- **Atlas as Primary Director Agent** — formalize a single user-facing Director that delegates through Heads to Hands; align UI copy and delegation traces
- **Hand lifecycle ownership** — runtime reaps Hands on spark close (SIGTERM/SIGKILL), watchdog for stale processes, and a Hands panel that joins (spark_status, assignment.active, process_alive) for truthful state
- **Build & test health** — restore `cargo test -p data` after `NewSpark` field additions
- **Terminal improvements** — font preferences, theme-aware background, scrollback fix, in-terminal search
- **Workshop shell polish** — onboarding/welcome, close confirmation, responsive collapse, attribution chip
- **Auto-clean stale Hand worktrees** — `ryve worktree prune` (shipped), session-end auto-prune, boot-time sweeper
- **Pro polish** — command palette (Cmd+Shift+P), source control panel, smooth animations

---

## Design goals

Ryve is being built around a few simple principles:

- **Native first** — not a web app wrapped in a shell
- **Terminal centered** — terminals are first-class, not bolted on
- **Structured coordination** — work should be visible and traceable
- **Tool agnostic** — Hands can be powered by different engines
- **Local ownership** — workshop state lives with the project

---

## Contributing

Ryve is open source and still early. The best way to contribute right now is to:

- Open issues
- Suggest UX improvements
- Test workflows on real projects
- Contribute focused PRs once the architecture stabilizes

A fuller contributor guide can be added as the project matures.

---

## License

Licensed under AGPL-3.0-or-later. See [LICENSE](LICENSE).

Copyright 2026 Xerxes Noble