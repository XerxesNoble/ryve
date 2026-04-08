# Ryve Architecture

> See also [`AGENT_HIERARCHY.md`](AGENT_HIERARCHY.md) for the Atlas (Director) → Heads → Hands agent model and delegation rules.

Ryve is a desktop IDE for managing development work through LLM-powered coding agents. It combines a tabbed terminal interface (the **bench**), a file explorer with git awareness, an embedded issue tracker (**Workgraph**), and support for multiple coding agent CLIs. Built in Rust with Iced 0.14 for the UI, SQLite for persistence, and alacritty-terminal for embedded terminals.

```
+------------------------------------------------------------------+
|  Workshop Tab Bar              [bg picker] [+ New Workshop]       |
+------------------------------------------------------------------+
|  File Explorer  |  Bench (tabbed terminals)      |  Workgraph     |
|  ============   |  ============================  |  ============  |
|  > src/         |  [Terminal] [Claude] [Aider] +  |  #SP-001 P0   |
|    main.rs      |                                |  #SP-002 P1   |
|    workshop.rs  |  $ claude --chat               |  #SP-003 P2   |
|  > data/        |  > Working on feature...       |               |
|                 |                                |               |
|  -----------    |                                |               |
|  Agents         |                                |               |
|  ============   |                                |               |
|  Claude Code    |                                |               |
|  Aider          |                                |               |
+------------------------------------------------------------------+
```

---

## Workspace Structure

Ryve is a Cargo workspace with four crates and one vendored dependency:

```
ryve/
+-- Cargo.toml           # workspace root (edition 2024, AGPL-3.0-or-later)
+-- src/                  # Application crate (binary)
+-- data/                 # Data layer: DB, config, git, sparks, unsplash, github
+-- llm/                  # LLM client (genai multi-provider)
|   +-- proto/            # Protocol types: Thread, Message, Agent
+-- ipc/                  # Single-instance enforcement (Unix socket)
+-- vendor/
|   +-- iced_term/        # Vendored terminal emulator widget (patched)
+-- docs/
```

### Key Dependencies

| Dependency | Version | Purpose |
|---|---|---|
| iced | 0.14 | UI framework (with tokio, image, canvas, lazy, advanced) |
| alacritty-terminal | (via iced_term) | Terminal emulation backend |
| sqlx | 0.8 | SQLite with compile-time checked migrations |
| genai | 0.5 | Multi-provider LLM client |
| tokio | 1 | Async runtime (multi-threaded) |
| petgraph | | Cycle detection in spark dependency graphs |
| rfd | | Native file dialogs |
| octocrab | | GitHub API client |

### Build Profiles

- **dev**: Default (unoptimized + debuginfo)
- **release**: LTO enabled, symbols stripped
- **release-package**: `opt-level = z` for minimum binary size

### Clippy Policy

Denied: `clone_on_ref_ptr`, `dbg_macro`, `todo`

---

## Application Layer (`src/`)

### Entry Point (`main.rs`)

The `App` struct is the top-level Elm Architecture state container:

```rust
struct App {
    available_agents: Vec<CodingAgent>,  // Detected on PATH at boot
    workshops: Vec<Workshop>,            // All open workshops
    active_workshop: Option<usize>,      // Index of focused workshop
    next_terminal_id: u64,               // Global unique terminal counter
}
```

Iced lifecycle methods:

- `boot()` -- Detects available coding agents, returns initial state
- `update(Message)` -- Central message dispatcher
- `view()` -- Renders workshop bar + active workshop (or welcome screen)
- `subscription()` -- Batches terminal event subscriptions + 3-second sparks poll timer
- `theme()` -- Returns dark or light theme based on system appearance

### Message Routing

The top-level `Message` enum routes to handlers by category:

```
Message
+-- Workshop lifecycle    SelectWorkshop, CloseWorkshop, NewWorkshopDialog, WorkshopDirPicked
+-- Initialization        WorkshopReady, SparksLoaded, FilesScanned
+-- Screen delegation     FileExplorer(_), Agents(_), Bench(_), Sparks(_), Background(_)
+-- Background system     BackgroundLoaded, UnsplashThumbnailLoaded, UnsplashDownloaded, etc.
+-- Workgraph sync        AgentContextSynced, SparksPoll
```

Each screen sub-message is forwarded to the active workshop's corresponding state. Terminal events (`Bench(TerminalEvent(BackendCall(id, cmd)))`) are special-cased: the app searches *all* workshops by terminal ID rather than assuming the active workshop.

### Async Pattern

All I/O uses Iced's `Task::perform()`:

```rust
Task::perform(async_fn(), |result| Message::SomeResult(result))
```

Multiple independent tasks run in parallel via `Task::batch()`. Results arrive as messages in `update()`.

---

## Workshop System (`src/workshop.rs`)

A Workshop is a self-contained workspace bound to a project directory. Each has its own `.ryve/` directory, database, config, terminals, and UI state.

```rust
struct Workshop {
    id: Uuid,
    directory: PathBuf,
    ryve_dir: RyveDir,
    config: WorkshopConfig,
    bench: BenchState,
    terminals: HashMap<u64, iced_term::Terminal>,
    agent_sessions: Vec<AgentSession>,
    file_explorer: FileExplorerState,
    sparks_db: Option<SqlitePool>,
    sparks: Vec<Spark>,
    custom_agents: Vec<AgentDef>,
    agent_context: Option<String>,
    background_handle: Option<image::Handle>,
    background_picker: PickerState,
}
```

### Workshop Lifecycle

1. User picks a directory via native file dialog
2. `Workshop::new(path)` creates in-memory state
3. `init_workshop(path)` runs async:
   - Creates `.ryve/` directory structure
   - Opens/migrates SQLite database
   - Loads `config.toml`, agent definitions, and `AGENTS.md` context
4. On `WorkshopReady`, three parallel tasks launch:
   - Load sparks from DB
   - Scan file tree + git statuses
   - Load background image (if configured)
5. Workshop is ready for interaction

### Terminal Spawning

`spawn_terminal(title, agent, next_terminal_id, session_id)`:

1. Creates a `Tab` in the bench (Terminal or CodingAgent kind)
2. **If spawning an agent**: creates a git worktree at `.ryve/worktrees/<session-id>/` via `create_hand_worktree()`, sets `working_directory` to the worktree path. Plain terminals use the workshop root.
3. **If agent supports system prompt injection**: appends the agent's system prompt flag (e.g. `--system-prompt` for Claude Code) with `.ryve/WORKSHOP.md` as the argument. For agents that take inline text (e.g. OpenCode's `--prompt`), reads the file content instead.
4. Wraps the command with the **bottom-pin technique**
5. Creates an `iced_term::Terminal` and stores it by ID

**Worktree isolation** -- Every Hand runs in its own git worktree (`hand/<session-id-prefix>` branch) to prevent merge conflicts between concurrent agents.

**System prompt injection** -- The WORKSHOP.md file containing active sparks, constraints, contracts, and workflow rules is injected directly into the agent's system instructions. This is not optional — only agents that support system prompt flags are included in `KNOWN_AGENTS`.

**Bottom-pin technique** -- Prevents the shell prompt from appearing at the top of an empty terminal by prepending 200 newlines before `exec`-ing the actual command:

```bash
i=0; while [ "$i" -lt 200 ]; do printf '\n'; i=$((i+1)); done; exec [command]
```

### Terminal Event Flow

```
PTY output
  -> alacritty EventLoop
    -> EventProxy (tokio mpsc sender)
      -> backend_event_rx (in Terminal)
        -> terminal_subscription_stream (Iced subscription)
          -> Event::BackendCall(id, Command::ProcessAlacrittyEvent(event))
            -> App::update(Message::Bench(TerminalEvent(...)))
              -> Terminal::handle(Command::ProxyToBackend(...))
                -> Backend::handle(cmd)
                  -> Action (Shutdown | ChangeTitle | Ignore)
```

Terminal actions are handled by `Workshop::handle_terminal_action()`:
- **Shutdown**: Remove terminal, clean up agent session, close tab
- **ChangeTitle**: Update tab and agent session name
- **Ignore**: No-op

---

## Screen Components (`src/screen/`)

### Bench (`bench.rs`)

Tabbed workspace area holding terminals and coding agent sessions.

```rust
struct BenchState {
    tabs: Vec<Tab>,          // id, title, kind (Terminal | CodingAgent)
    active_tab: Option<u64>, // Currently displayed tab
    dropdown_open: bool,     // "+" menu visibility
}
```

The tab bar renders tab buttons with close controls and a "+" dropdown menu listing "New Terminal" plus each detected coding agent.

### File Explorer (`file_explorer.rs`)

Tree view of the workshop directory with git integration.

```rust
struct FileExplorerState {
    tree: Vec<FileEntry>,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    git_statuses: HashMap<PathBuf, FileStatus>,
    branch: Option<String>,
}
```

**Directory scanning** (`scan_directory()`):
- Recursive traversal (max depth 12)
- Filters configurable ignore patterns (defaults: `.git`, `node_modules`, `target`, `.ryve`, `__pycache__`, `.DS_Store`, `*.pyc`)
- Directories sorted first, then alphabetical
- Git statuses and branch loaded in parallel

**Visual features**:
- File type icons (branded emojis per extension)
- Git status coloring: Modified=yellow, Added=green, Deleted=red, Untracked=gray, Conflicted=magenta
- Directories inherit the most significant child status
- Spark link button (planned)

### Agents Panel (`agents.rs`)

Lists active `AgentSession` objects (id, name, tab_id). Clicking an agent switches the bench to its terminal tab.

### Workgraph Panel (`sparks.rs`)

Live management interface for sparks with full CRUD:
- Status indicators: open (○), in_progress (◔), blocked (■), deferred (◌), closed (●)
- Clickable status indicators cycle through: open → in_progress → closed
- Priority badges (P0-P4)
- "+" button opens inline create form (title input, creates Task at P2)
- Refresh button to reload from DB
- **Auto-refresh**: 3-second polling subscription detects external DB changes (e.g. from agents)
- Every mutation triggers `SparksLoaded` → WORKSHOP.md regeneration

### Background Picker (`background_picker.rs`)

Modal overlay (rendered via `stack![]` layering) for setting workshop backgrounds:

- **Unsplash search**: Query API, display thumbnail grid (3 columns), download full image
- **Local file upload**: Native file picker, copy to `.ryve/backgrounds/`
- **Remove background**: Clear config and image handle
- Photographer attribution stored in config

---

## Coding Agents (`src/coding_agents.rs`)

Auto-detection of CLI coding agents on the system PATH. Only agents that support system prompt injection are included — Ryve requires control over Hand instructions to enforce workgraph coordination.

| Agent | Command | System Prompt Flag | Resume |
|---|---|---|---|
| Claude Code | `claude` | `--system-prompt <file>` | `--resume` |
| Codex | `codex` | `--instructions <file>` | `--resume` |
| Aider | `aider` | `--read <file>` | — |
| OpenCode | `opencode` | `--prompt <text>` | — |

Detection uses `which` to check command availability. Custom agents can also be defined per-workshop in `.ryve/agents/*.toml` files.

`system_prompt_flag()` returns `(flag, is_file_path)` — if `is_file_path` is false (OpenCode), the WORKSHOP.md content is read and passed inline rather than as a path.

---

## Data Layer (`data/`)

### Ryve Directory (`ryve_dir.rs`)

Each workshop's `.ryve/` directory:

```
.ryve/
+-- config.toml          # WorkshopConfig
+-- sparks.db            # SQLite database
+-- WORKSHOP.md          # Auto-generated context for Hands (source of truth projection)
+-- agents/              # Custom agent definitions (*.toml)
+-- context/
|   +-- AGENTS.md        # Additional instructions read by agents
+-- backgrounds/         # Workshop background images
+-- worktrees/           # Git worktrees for active Hand sessions
```

**`WorkshopConfig`** (TOML):

```toml
name = "My Project"

[github]
token = "..."
repo = "owner/repo"
auto_sync = false

[layout]
sidebar_width = 250.0
sparks_width = 280.0
sidebar_split = 0.65

[explorer]
ignore = [".git", "node_modules", "target", ...]

[background]
image = "photo.jpg"
dim_opacity = 0.7
unsplash_photographer = "Jane Doe"
unsplash_photographer_url = "https://unsplash.com/@jane"

[agents]
target_files = ["CLAUDE.md", "OPENCODE.md", ".cursorrules", ".github/copilot-instructions.md"]
disable_sync = false
```

**`AgentDef`** (per `.ryve/agents/*.toml`):

```toml
name = "My Agent"
command = "python"
args = ["agent.py"]
env = { API_KEY = "..." }
system_prompt = "You are a helpful assistant."
model = "claude-sonnet-4-20250514"
```

### Database (`db.rs`)

SQLite with WAL journaling, opened via sqlx. Max 5 connections per pool.

**Schema** (from `migrations/001-004`):

| Table | Purpose |
|---|---|
| `sparks` | Core work items (status, priority, type, assignee, parent, GitHub link, metadata, risk_level, scope_boundary) |
| `bonds` | Dependency edges between sparks (with bond type) |
| `stamps` | Labels/tags on sparks |
| `comments` | Discussion threads on sparks |
| `events` | Audit trail of all changes (with actor_type, change_nature, session_id for provenance) |
| `embers` | Ephemeral inter-Hand signals (TTL-based context passing) |
| `engravings` | Persistent shared knowledge + architectural constraints (`constraint:` prefix) |
| `alloys` | Coordination templates (groups of sparks with execution order) |
| `alloy_members` | Members of an alloy with bond types and positions |
| `spark_file_links` | Spark-to-code-region associations (file, line range) |
| `agent_sessions` | Hand session tracking with resume capability |
| `contracts` | Verification criteria on sparks (required/advisory, kind, check_command, status) |
| `commit_links` | Git commit-to-spark linkage (parsed from `[sp-xxxx]` in messages) |
| `hand_assignments` | Liveness-aware Hand-to-Spark claims (heartbeat, lease, handoff) |
| `crews` | Optional Hand groupings (schema-only, future use) |
| `crew_members` | Crew membership (schema-only, future use) |

### Workgraph System (`sparks/`)

**Types** (`types.rs`):

- **SparkStatus**: `open`, `in_progress`, `blocked`, `deferred`, `closed`
- **SparkPriority**: P0 (critical) through P4 (negligible)
- **SparkType**: `bug`, `feature`, `task`, `epic`, `chore`, `spike`, `milestone`
- **RiskLevel**: `trivial`, `normal`, `elevated`, `critical`
- **SparkIntent**: Structured intent embedded in metadata JSON (`problem_statement`, `invariants`, `non_goals`, `acceptance_criteria`)
- **BondType**: `blocks`, `parent_child`, `related`, `conditional_blocks`, `waits_for`, `duplicates`, `supersedes`
- **ContractKind**: `test_pass`, `no_api_break`, `custom_command`, `grep_absent`, `grep_present`
- **ContractEnforcement**: `advisory`, `required`
- **ActorType**: `human`, `hand`, `system`, `unknown` (provenance)
- **ChangeNature**: `code`, `refactor`, `format`, `generated`, `review`, `config`, `documentation`, `test`
- **AssignmentStatus**: `active`, `completed`, `handed_off`, `abandoned`, `expired`
- **AssignmentRole**: `owner`, `assistant`, `observer`
- **ArchConstraint**: Stored as typed engravings with `constraint:` key prefix

**Repositories** (async CRUD via sqlx):

- `spark_repo` -- Create, get, update, close, delete, list with `SparkFilter` (includes risk_level filter)
- `bond_repo` -- Create (with cycle detection), delete, list for spark, list blockers
- `comment_repo`, `event_repo`, `stamp_repo` -- Standard CRUD (events include provenance fields)
- `ember_repo` -- Create, get, expire by TTL, list active
- `engraving_repo` -- Upsert, get by key, list for workshop
- `alloy_repo` -- Create, add/remove members, list
- `contract_repo` -- Create, list for spark, update status, list failing (workshop-wide)
- `commit_link_repo` -- Create, list for spark, list for commit
- `assignment_repo` -- Assign, complete, handoff, abandon, heartbeat, expire stale claims
- `constraint_helpers` -- Thin wrapper over engravings for `constraint:` prefix convention

**Graph** (`graph.rs`):

- `would_create_cycle()` -- Builds a petgraph `DiGraphMap` from all `blocks`-type bonds and checks if a proposed edge creates a cycle
- `hot_sparks()` -- Finds ready-to-work sparks by excluding closed blockers, future-deferred sparks, and deferred children; sorts by priority then creation date
- `topological_order()` -- Topological sort for chain alloy execution order

### Git Integration (`git.rs`)

Wraps the `git` CLI (no library dependency):

- `current_branch()` -- `git rev-parse --abbrev-ref HEAD`
- `file_statuses()` -- `git status --porcelain=v1 -uall`, parses into `HashMap<PathBuf, FileStatus>`
- `is_repo()` -- Checks `.git` existence
- `list_worktrees()` -- `git worktree list --porcelain`
- `create_worktree(branch, target)` -- `git worktree add -b <branch> <target>`
- `remove_worktree(target)` -- `git worktree remove --force <target>`
- `parse_spark_refs(message)` -- Extracts `[sp-xxxx]` spark references from commit messages
- `scan_commits_for_sparks(repo, since)` -- Scans `git log` for commits referencing sparks

File statuses: Modified, Added, Deleted, Renamed, Copied, Untracked, Ignored, Conflicted.

### Agent Context (`agent_context.rs`)

Generates `.ryve/WORKSHOP.md` and injects pointers into agent boot files (`CLAUDE.md`, `OPENCODE.md`, `.cursorrules`, `.github/copilot-instructions.md`).

**WORKSHOP.md** is the generated projection of the workgraph, containing:
- Active sparks table (with risk level and scope)
- Architectural constraints (from `constraint:` engravings)
- Failing verification contracts
- Active Hand assignments
- Workflow rules (claim before work, reference spark IDs, check contracts)

Regenerated on every `SparksLoaded` event (including the 3-second poll). The pointer injected into boot files uses `<!-- RYVE:START -->` / `<!-- RYVE:END -->` markers to safely replace content without clobbering user-written instructions.

### Unsplash Integration (`unsplash.rs`)

- `search(api_key, query, page)` -- Searches Unsplash API for landscape photos (12 per page)
- `download(api_key, photo, dest_dir)` -- Triggers download tracking endpoint, saves to `.ryve/backgrounds/{id}.jpg`
- `fetch_thumbnail_bytes(url)` -- Downloads thumbnail for picker preview

Requires `UNSPLASH_ACCESS_KEY` environment variable.

### GitHub Sync (`github/`)

Skeleton in place for bidirectional sync between Workgraph and GitHub Issues. Not yet implemented.

---

## LLM Layer (`llm/`)

### Protocol Types (`llm/proto/`)

```rust
struct Thread { id, agent_id, title, messages, created_at, updated_at }
struct Message { role: Role, content: String }  // Role: User | Assistant | System
struct Agent { name, provider, model, system_prompt, worktree_path }
```

### Client (`llm/src/client.rs`)

`RyveClient` wraps `genai::Client` for multi-provider LLM chat:

```rust
impl RyveClient {
    pub fn new() -> Self;
    pub async fn chat(&self, agent: &Agent, messages: Vec<Message>) -> Result<String>;
}
```

Prepends the agent's system prompt, sends all messages, returns the assistant's text response. Supports any provider that genai supports (OpenAI, Anthropic, etc.).

---

## IPC (`ipc/`)

Minimal crate providing `socket_path()` for Unix socket-based single-instance enforcement. Returns a platform-appropriate path for `ryve.sock`. Intended for future multi-window coordination.

---

## Vendored Terminal (`vendor/iced_term/`)

Patched fork of iced_term 0.8.0 providing an alacritty-based terminal widget for Iced.

### Key Components

**`Terminal`** -- Top-level struct owning backend, theme, font, bindings, and canvas cache. Exposes:
- `subscription()` -- Returns an Iced `Subscription` that streams PTY events
- `handle(Command)` -- Processes commands and returns an `Action`

**`Backend`** -- Manages the PTY process via alacritty-terminal:
- Creates PTY with `tty::new()`
- Spawns `EventLoop` for async PTY I/O
- Handles: Write, Scroll, Resize, Selection, Mouse reporting, Link detection
- URL regex matching for clickable hyperlinks

**`TerminalView`** -- Canvas-based widget rendering the terminal grid:
- Cell-by-cell rendering with colors, cursor, selections
- Mouse and keyboard event capture
- Transparent background support for workshop backgrounds

### Subscription Model

Each terminal has an independent subscription that:
1. Locks the `backend_event_rx` (Arc<Mutex<Receiver>>)
2. Awaits the next alacritty event
3. Wraps it as `Event::BackendCall(id, Command::ProcessAlacrittyEvent(event))`
4. Sends it through the Iced stream channel

All terminal subscriptions are batched in `App::subscription()`.

### Resize Guard

The backend's `resize()` method only sends PTY resize notifications when the computed terminal dimensions (lines/cols) actually change. This prevents an infinite event loop where resize -> PTY event -> handle -> sync_font -> resize would cycle indefinitely.

---

## Design Patterns

| Pattern | Where | Why |
|---|---|---|
| **Elm Architecture** | `App` update/view | Iced's core pattern; unidirectional data flow |
| **Workshop Scoping** | All state per-workshop | Switching workshops = changing an index, not rebuilding state |
| **Task-based Async** | All I/O operations | Non-blocking via `Task::perform()`, results as messages |
| **Subscription Streams** | Terminal events + sparks poll | Continuous PTY event polling + 3-second DB refresh via Iced subscriptions |
| **Stack Layering** | Background + content + modals | `stack![]` for background images, dim overlays, modal dialogs |
| **Cycle Detection** | Bond creation | petgraph DiGraphMap prevents circular blocking dependencies |
| **Bottom-pin** | Terminal spawning | Newline padding keeps shell prompt at viewport bottom |
| **CLI Wrapping** | Git integration | Shell out to `git` rather than linking libgit2 |
| **Channel Bridge** | PTY to Iced | tokio mpsc channels bridge alacritty's sync EventListener to Iced's async subscriptions |
| **Worktree Isolation** | Hand spawning | Each Hand gets its own git worktree to prevent merge conflicts |
| **System Prompt Injection** | Hand spawning | WORKSHOP.md injected via agent-specific CLI flags (`--system-prompt`, etc.) |
| **Marker Injection** | Agent boot files | `<!-- RYVE:START/END -->` markers safely inject pointers into CLAUDE.md etc. |
| **Constraint Engravings** | Architectural rules | `constraint:` key prefix on engravings for typed architectural constraints |
| **Poll-based Refresh** | Sparks panel | 3-second timer detects external DB mutations by agents |
