# Ryve Workshop

You are working inside a **Ryve Workshop**. Ryve manages tasks (called *sparks*) in an embedded workgraph stored at `.ryve/sparks.db`.

## Active Sparks

| ID | P | Risk | Type | Status | Scope | Title |
|----|---|------|------|--------|-------|-------|
| `sp-ux0001` | P0 | elevated | epic | open | src/screen/sparks.rs, src/main.rs | Spark detail view (SelectSpark TODO) |
| `sp-ux0002` | P0 | critical | feature | open | src/main.rs, src/screen/bench.rs | Hand-Spark assignment at spawn time |
| `sp-ux0003` | P0 | normal | feature | open | src/main.rs | Keyboard shortcuts system |
| `sp-ux0004` | P0 | elevated | feature | open | src/screen/file_viewer.rs | File viewer text selection and copy |
| `sp-ux0005` | P0 | normal | feature | open | src/screen/sparks.rs | Spark filtering and search in workgraph panel |
| `sp-ux0035` | P0 | critical | epic | open | new: src/head.rs, data/src/sparks/, llm/ | Head: orchestrator agent that manages a Crew of Hands |
| `ryve-35463d5b` | P0 | critical | bug | open | src/cli.rs | ryve-cli missing: intent fields, subcommand --help, ember send doc |
| `sp-f5e4` | P1 | elevated | spike | open | data/src/sparks/, llm/ | Save conversation history and plans to workgraph |
| `sp-ux0006` | P1 | elevated | feature | open | src/screen/sparks.rs, data/src/agent_context.rs | Bond/dependency visualization in UI and WORKSHOP.md |
| `sp-ux0007` | P1 | normal | feature | open | src/screen/sparks.rs, src/main.rs | Contract management UI |
| `sp-ux0008` | P1 | normal | feature | open | src/main.rs, src/screen/ | Ember notification system UI |
| `sp-ux0009` | P1 | normal | feature | open | src/main.rs, src/style.rs | Drag-to-resize panels |
| `sp-ux0010` | P1 | normal | bug | open | src/screen/bench.rs | Tab bar overflow scrolling |
| `sp-ux0011` | P1 | normal | bug | open | src/main.rs | Error toast notification system |
| `sp-ux0012` | P1 | normal | feature | open | src/screen/file_viewer.rs | Find-in-file (Cmd+F) for file viewer |
| `sp-ux0013` | P1 | elevated | feature | open | src/screen/, src/main.rs | Multi-Hand coordination dashboard |
| `sp-ux0014` | P1 | normal | feature | open | src/workshop.rs, vendor/iced_term/ | Terminal font/size preferences |
| `sp-ux0015` | P1 | trivial | feature | open | src/screen/status_bar.rs | Status bar enrichment |
| `sp-ux0016` | P1 | trivial | feature | open | src/main.rs | Welcome screen with recent workshops and onboarding |
| `sp-ux0017` | P1 | normal | bug | open | data/src/agent_context.rs | WORKSHOP.md: add bond info and spark intent |
| `sp-ux0034` | P1 | normal | feature | open | src/screen/agents.rs, src/main.rs | Hand status colors: red for unassigned, green for idle |
| `sp-ux0018` | P2 | normal | feature | open | src/screen/sparks.rs | Blocked/Deferred status settable from UI |
| `sp-ux0019` | P2 | normal | bug | open | src/workshop.rs | Terminal background respects appearance mode |
| `sp-ux0020` | P2 | trivial | feature | open | src/screen/file_viewer.rs | File viewer breadcrumb path |
| `sp-ux0021` | P2 | trivial | feature | open | src/main.rs | Close workshop confirmation dialog |
| `sp-ux0022` | P2 | trivial | bug | open | src/screen/bench.rs | Click-outside-to-dismiss dropdown menus |
| `sp-ux0023` | P2 | trivial | feature | open | src/screen/background_picker.rs | Background picker: dim opacity slider + preview |
| `sp-ux0024` | P2 | trivial | chore | open | src/screen/file_explorer.rs | File explorer hover highlight |
| `sp-ux0025` | P2 | normal | feature | open | src/main.rs | Responsive layout: panel collapse at small window sizes |
| `sp-ux0026` | P2 | normal | feature | open | src/screen/file_explorer.rs | Git status accessibility: add letters/icons alongside colors |
| `sp-ux0027` | P2 | trivial | chore | open | src/workshop.rs | Reduce bottom-pin newline hack from 200 to 20 |
| `sp-49fd1738` | P2 | normal | task | open |  | Auto-detect agent processes in plain terminals and register as Hands with system prompt injection |
| `sp-ux0028` | P3 | elevated | feature | open | src/main.rs, src/screen/ | Command palette (Cmd+Shift+P) |
| `sp-ux0029` | P3 | normal | feature | open | src/screen/file_viewer.rs | File viewer minimap |
| `sp-ux0030` | P3 | normal | feature | open | vendor/iced_term/, src/screen/bench.rs | Terminal search (Cmd+F in terminal) |
| `sp-ux0031` | P3 | elevated | epic | open | src/screen/, data/src/git.rs | Source control panel (staging, commit, history) |
| `sp-ux0032` | P3 | normal | feature | open | src/ | Smooth animations and transitions |
| `sp-ux0033` | P3 | trivial | chore | open | src/main.rs | Unsplash attribution overlay in workspace |

## Rules

1. **Always reference spark IDs** in commit messages: `fix(auth): validate token expiry [sp-a1b2]`
2. **Work in priority order** — P0 is critical, P4 is negligible.
3. **Respect architectural constraints** listed above — violations are blocking.
4. **Check required contracts** before considering a spark done.
5. **Do not work on a spark that is already claimed** by another Hand.
6. If you discover a new bug or task, create a spark for it (see commands below).

## Workgraph Commands

Use `ryve-cli` to interact with the workgraph. Run from the workshop root.

### List active sparks

```sh
ryve-cli spark list
ryve-cli spark list --all   # include closed
```

### Create a new spark

```sh
ryve-cli spark create Fix the authentication timeout bug
```

### Update spark status

```sh
ryve-cli spark status sp-a1b2 in_progress
```

### Close a spark

```sh
ryve-cli spark close sp-a1b2 completed successfully
```

### Show spark details

```sh
ryve-cli spark show sp-a1b2
```

### Check contracts for a spark

```sh
ryve-cli contract list sp-a1b2
```

### List architectural constraints

```sh
ryve-cli constraint list
```

Ryve auto-refreshes every 3 seconds. Any changes you make will be picked up by the UI and by other Hands automatically.
