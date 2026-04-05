# Ryve

A new-era IDE where you manage threads with LLMs, not just code.

Ryve reimagines the development environment as an agent orchestration workspace. Instead of tabs of source files, you work with agent conversations attached to git worktrees — each agent has full context of its working tree, and you see the results in real time.

## Architecture

Ryve is built with [Iced](https://iced.rs) for a fast, cross-platform native GUI, [iced_term](https://github.com/Harzu/iced_term) for embedded terminals, and [genai](https://github.com/jeremychone/rust-genai) for multi-provider LLM support.

### Layout

```
┌──────────┬─────────────────────────────────┐
│  Files   │  Bench (tabbed)          [+ ▾]  │
│          ├─────────────────────────────────┤
│          │                                 │
│──────────│  Terminal / Claude Code / Codex  │
│  Agents  │                                 │
│          │                                 │
└──────────┴─────────────────────────────────┘
```

| Panel | Purpose |
|-------|---------|
| **Files** (top-left) | File explorer with native git & worktree support |
| **Agents** (bottom-left) | Active coding agent sessions, linked to worktrees |
| **Bench** (right) | Tabbed workspace — terminals and coding agents with embedded PTY |

The "+" button auto-detects coding agents on your PATH (Claude Code, Codex, Aider, Goose, etc.) and always offers a plain terminal.

### Workspace Crates

| Crate | Purpose |
|-------|---------|
| `data` | Persistence (SQLite), config, git/worktree operations |
| `ipc` | Inter-process communication for multi-instance coordination |
| `llm` | Multi-provider LLM integration via genai |
| `llm/proto` | Message, thread, and agent protocol types |

## Requirements

- Rust stable (1.94+)

## Getting Started

```bash
# Clone
git clone https://github.com/loomantix/ryve.git
cd ryve

# Build & run
cargo run
```

## License

AGPL-3.0-or-later — see [LICENSE](LICENSE) for details.

Copyright 2026 Loomantix
