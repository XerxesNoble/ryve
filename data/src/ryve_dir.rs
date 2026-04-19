// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `.ryve/` directory — per-workshop configuration, state, and context.
//!
//! Every workshop has a `.ryve/` directory at its root containing:
//!
//! ```text
//! .ryve/
//! ├── config.toml       # Workshop configuration
//! ├── sparks.db         # SQLite database (sparks, bonds, embers, engravings)
//! ├── agents/           # Custom agent definitions
//! │   └── *.toml
//! └── context/          # Files that agents read for project context
//!     └── AGENTS.md     # Default agent instructions
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Paths within a `.ryve/` directory.
#[derive(Debug, Clone)]
pub struct RyveDir {
    root: PathBuf,
}

impl RyveDir {
    pub fn new(workshop_dir: &Path) -> Self {
        Self {
            root: workshop_dir.join(".ryve"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn sparks_db_path(&self) -> PathBuf {
        self.root.join("sparks.db")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    pub fn context_dir(&self) -> PathBuf {
        self.root.join("context")
    }

    pub fn agents_md_path(&self) -> PathBuf {
        self.context_dir().join("AGENTS.md")
    }

    pub fn backgrounds_dir(&self) -> PathBuf {
        self.root.join("backgrounds")
    }

    /// Directory holding timestamped SQLite snapshots of `sparks.db`.
    /// See [`crate::backup`].
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn workshop_md_path(&self) -> PathBuf {
        self.root.join("WORKSHOP.md")
    }

    /// The workshop root directory (parent of `.ryve/`).
    pub fn workshop_dir(&self) -> &Path {
        self.root
            .parent()
            .expect(".ryve/ must have a parent directory")
    }

    /// Path to the universal `RYVE.md` skill file at the workshop root.
    pub fn ryve_md_path(&self) -> PathBuf {
        self.workshop_dir().join("RYVE.md")
    }

    /// Per-workshop UI state (collapsed epic groups, etc.) stored in
    /// `.ryve/ui_state.json`. Kept separate from `config.toml` so frequent
    /// UI-driven writes don't churn the canonical config file.
    /// Spark ryve-926870a9.
    pub fn ui_state_path(&self) -> PathBuf {
        self.root.join("ui_state.json")
    }

    pub fn checklists_dir(&self) -> PathBuf {
        self.root.join("checklists")
    }

    pub fn done_md_path(&self) -> PathBuf {
        self.checklists_dir().join("DONE.md")
    }

    /// Create the `.ryve/` directory structure if it doesn't exist.
    pub async fn ensure_exists(&self) -> Result<(), std::io::Error> {
        tokio::fs::create_dir_all(&self.root).await?;
        tokio::fs::create_dir_all(self.agents_dir()).await?;
        tokio::fs::create_dir_all(self.context_dir()).await?;
        tokio::fs::create_dir_all(self.backgrounds_dir()).await?;
        tokio::fs::create_dir_all(self.backups_dir()).await?;
        tokio::fs::create_dir_all(self.checklists_dir()).await?;
        Ok(())
    }
}

// ── Workshop Config ────────────────────────────────────

/// Per-workshop configuration stored in `.ryve/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkshopConfig {
    /// Workshop schema version. Compared against
    /// [`crate::migrations::CURRENT_SCHEMA_VERSION`] on workshop open;
    /// any pending migrations are run and this field is bumped.
    ///
    /// Defaults to `0` for workshops created before migrations existed.
    #[serde(default)]
    pub workshop_schema_version: u32,

    /// Display name for the workshop (defaults to directory name).
    #[serde(default)]
    pub name: Option<String>,

    /// GitHub sync settings.
    #[serde(default)]
    pub github: GitHubConfig,

    /// Layout preferences.
    #[serde(default)]
    pub layout: LayoutConfig,

    /// Default assignee for new sparks.
    #[serde(default)]
    pub default_assignee: Option<String>,

    /// Default owner for new sparks.
    #[serde(default)]
    pub default_owner: Option<String>,

    /// File explorer settings.
    #[serde(default)]
    pub explorer: ExplorerConfig,

    /// Background image settings.
    #[serde(default)]
    pub background: BackgroundConfig,

    /// Agent context injection settings.
    #[serde(default)]
    pub agents: AgentsConfig,

    /// Preferred coding agent for Atlas (the Director). When set, Atlas
    /// spawns with this agent instead of probing PATH. When unset,
    /// resolution follows Claude Code → Codex → OpenCode order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_agent: Option<String>,

    /// IRC coordination settings. Under the bundled-IRC model (spark
    /// ryve-300f661c [sp-31659bbb]) IRC defaults on: the workshop runs
    /// a local daemon on `127.0.0.1:<irc_bundled_port>` and
    /// [`WorkshopConfig::effective_irc_server_address`] resolves to that
    /// address unless `irc_server` explicitly overrides it (e.g.
    /// `irc_server = "irc.libera.chat"` for a mesh setup).
    ///
    /// `irc_enabled` is the master opt-out switch; set it to `false` to
    /// keep the whole subsystem dormant.
    #[serde(default = "default_true")]
    pub irc_enabled: bool,
    /// Workshop-local IRC daemon port, allocated and recorded by
    /// `ryve init` (spark ryve-4d5881c2). Read by
    /// [`WorkshopConfig::effective_irc_server_address`] to form the
    /// default `127.0.0.1:<port>` address when no explicit `irc_server`
    /// override is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irc_bundled_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irc_server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irc_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irc_tls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irc_nick: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irc_password: Option<String>,

    /// Seconds between `HeartbeatReceived` emissions from an active Hand
    /// assignment. Per-workshop override for the liveness loop; default
    /// [`DEFAULT_HEARTBEAT_INTERVAL_SECS`]. Part of parent epic
    /// ryve-cf05fd85 (liveness + stuck detection).
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,

    /// Seconds past the last heartbeat after which the watchdog transitions
    /// an assignment to `Stuck`. Per-workshop override; default
    /// [`DEFAULT_STUCK_THRESHOLD_SECS`]. Part of parent epic
    /// ryve-cf05fd85 (liveness + stuck detection).
    #[serde(default = "default_stuck_threshold_secs")]
    pub stuck_threshold_secs: u64,
}

impl WorkshopConfig {
    /// Whether the IRC subsystem should start for this workshop. Defaults
    /// to `true` — Ryve ships a bundled daemon and IRC is the backbone of
    /// agent coordination. Set the `irc_enabled` field to `false` to keep
    /// the client, relay, and inbound listener dormant.
    pub fn irc_enabled(&self) -> bool {
        self.irc_enabled
    }

    /// Effective IRC server address the runtime should dial. Resolution
    /// order:
    ///
    /// 1. Explicit `irc_server` override combined with
    ///    [`effective_irc_port`] — e.g. `"irc.libera.chat:6697"` for a
    ///    mesh/cross-workshop setup.
    /// 2. Bundled workshop-local daemon at
    ///    `127.0.0.1:<irc_bundled_port>` (the port allocated by
    ///    `ryve init`).
    ///
    /// Returns `None` only when neither source yields an address — i.e.
    /// the workshop has not yet been initialised and no override is set.
    /// Callers (IPC lifecycle, supervisor) treat that as "skip IRC for
    /// this boot".
    pub fn effective_irc_server_address(&self) -> Option<String> {
        if let Some(host) = self
            .irc_server
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(format!("{host}:{}", self.effective_irc_port()));
        }
        self.irc_bundled_port
            .map(|port| format!("127.0.0.1:{port}"))
    }

    /// Effective IRC port: falls back to 6697 when TLS is on and 6667
    /// otherwise. Call sites must not hand-roll this default so every
    /// subsystem agrees on which port to dial.
    pub fn effective_irc_port(&self) -> u16 {
        self.irc_port.unwrap_or(if self.irc_tls.unwrap_or(false) {
            6697
        } else {
            6667
        })
    }

    /// Effective IRC nick: falls back to `"ryve"` when unset.
    pub fn effective_irc_nick(&self) -> String {
        self.irc_nick
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "ryve".to_string())
    }
}

impl Default for WorkshopConfig {
    fn default() -> Self {
        Self {
            workshop_schema_version: 0,
            name: None,
            github: GitHubConfig::default(),
            layout: LayoutConfig::default(),
            default_assignee: None,
            default_owner: None,
            explorer: ExplorerConfig::default(),
            background: BackgroundConfig::default(),
            agents: AgentsConfig::default(),
            atlas_agent: None,
            irc_enabled: true,
            irc_bundled_port: None,
            irc_server: None,
            irc_port: None,
            irc_tls: None,
            irc_nick: None,
            irc_password: None,
            heartbeat_interval_secs: DEFAULT_HEARTBEAT_INTERVAL_SECS,
            stuck_threshold_secs: DEFAULT_STUCK_THRESHOLD_SECS,
        }
    }
}

/// Default heartbeat emission cadence for a Hand.
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;
/// Default threshold after which the watchdog flags an assignment as
/// `Stuck`. Chosen as 10x the heartbeat interval so a single missed beat
/// does not escalate, but sustained silence does.
pub const DEFAULT_STUCK_THRESHOLD_SECS: u64 = 300;

fn default_heartbeat_interval_secs() -> u64 {
    DEFAULT_HEARTBEAT_INTERVAL_SECS
}

fn default_true() -> bool {
    true
}

fn default_stuck_threshold_secs() -> u64 {
    DEFAULT_STUCK_THRESHOLD_SECS
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// GitHub personal access token (or env var name like `$GITHUB_TOKEN`).
    #[serde(default)]
    pub token: Option<String>,

    /// Repository in "owner/repo" format.
    #[serde(default)]
    pub repo: Option<String>,

    /// Auto-sync sparks to GitHub issues on every change.
    #[serde(default)]
    pub auto_sync: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutConfig {
    /// Sidebar width in pixels.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,

    /// Workgraph panel width in pixels.
    #[serde(default = "default_sparks_width")]
    pub sparks_width: f32,

    /// Sidebar split ratio (files vs agents, 0.0 - 1.0).
    #[serde(default = "default_sidebar_split")]
    pub sidebar_split: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            sidebar_width: default_sidebar_width(),
            sparks_width: default_sparks_width(),
            sidebar_split: default_sidebar_split(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorerConfig {
    /// File and directory names to hide in the file explorer.
    #[serde(default = "default_ignore_patterns")]
    pub ignore: Vec<String>,
}

impl Default for ExplorerConfig {
    fn default() -> Self {
        Self {
            ignore: default_ignore_patterns(),
        }
    }
}

fn default_ignore_patterns() -> Vec<String> {
    Vec::new()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundConfig {
    /// Filename of the background image (stored in `.ryve/backgrounds/`).
    #[serde(default)]
    pub image: Option<String>,

    /// Dim opacity over the background so content stays readable (0.0–1.0).
    #[serde(default = "default_dim_opacity")]
    pub dim_opacity: f32,

    /// Unsplash attribution: photographer name.
    #[serde(default)]
    pub unsplash_photographer: Option<String>,

    /// Unsplash attribution: photographer profile URL.
    #[serde(default)]
    pub unsplash_photographer_url: Option<String>,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self {
            image: None,
            dim_opacity: default_dim_opacity(),
            unsplash_photographer: None,
            unsplash_photographer_url: None,
        }
    }
}

fn default_dim_opacity() -> f32 {
    0.7
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsConfig {
    /// Override which agent boot files get the Ryve pointer injection.
    /// Defaults to `["CLAUDE.md", ".cursorrules", ".github/copilot-instructions.md"]`.
    #[serde(default)]
    pub target_files: Option<Vec<String>>,

    /// Disable automatic context injection entirely.
    #[serde(default)]
    pub disable_sync: bool,
}

fn default_sidebar_width() -> f32 {
    250.0
}
fn default_sparks_width() -> f32 {
    280.0
}
fn default_sidebar_split() -> f32 {
    0.65
}

// ── Agent Definition ───────────────────────────────────

/// A custom agent definition stored in `.ryve/agents/*.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDef {
    /// Display name.
    pub name: String,

    /// CLI command to run (e.g. "claude", "aider", or a custom script).
    pub command: String,

    /// Arguments passed to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,

    /// System prompt or instructions file path (relative to workshop root).
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Model to use (if applicable).
    #[serde(default)]
    pub model: Option<String>,
}

// ── I/O Operations ─────────────────────────────────────

/// Load the workshop config from `.ryve/config.toml`.
/// Returns default config if the file doesn't exist.
pub async fn load_config(ryve_dir: &RyveDir) -> WorkshopConfig {
    let path = ryve_dir.config_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => toml::from_str(&content).unwrap_or_default(),
        Err(_) => WorkshopConfig::default(),
    }
}

/// Save the workshop config to `.ryve/config.toml`.
pub async fn save_config(
    ryve_dir: &RyveDir,
    config: &WorkshopConfig,
) -> Result<(), std::io::Error> {
    let content =
        toml::to_string_pretty(config).map_err(|e| std::io::Error::other(e.to_string()))?;
    tokio::fs::write(ryve_dir.config_path(), content).await
}

// ── UI State (spark ryve-926870a9) ─────────────────────

/// Per-workshop UI state persisted across restarts. Contains collapsed
/// epic IDs and sparks filter/sort state. Each field carries
/// `#[serde(default)]` so new additions deserialise cleanly from older
/// files.
///
/// Stored as JSON in `.ryve/ui_state.json` — deliberately separate from
/// `config.toml` so high-frequency, UI-driven writes don't rewrite the
/// canonical (user-editable) workshop config on every chevron click.
///
/// The `version` field enables future migrations. Current version: `1`.
/// Spark ryve-926870a9 (initial), ryve-27e33825 (filter persistence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiState {
    /// Format version for forward-compatible migration. Defaults to 1.
    #[serde(default = "ui_state_version_default")]
    pub version: u32,
    /// IDs of epics the user has collapsed in the sparks panel. Default-
    /// expanded: only collapsed IDs are persisted, so a freshly-created
    /// epic appears open.
    #[serde(default)]
    pub collapsed_epics: std::collections::HashSet<String>,
    /// Persisted sparks-panel filter + sort state. Scoped per workshop —
    /// each workshop stores its own filter in its `.ryve/ui_state.json`.
    /// Spark ryve-27e33825.
    #[serde(default)]
    pub sparks_filter: SparksFilterState,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            version: 1,
            collapsed_epics: std::collections::HashSet::new(),
            sparks_filter: SparksFilterState::default(),
        }
    }
}

fn ui_state_version_default() -> u32 {
    1
}

/// Serializable sparks filter state for persistence in `UiState`.
///
/// Mirrors the fields of the UI-side `SparksFilter` struct but lives in
/// the `data` crate so it stays decoupled from the rendering layer. The
/// `#[serde(default)]` on every field means older files that lack filter
/// state deserialise cleanly into the default (show all non-closed).
/// Spark ryve-27e33825.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SparksFilterState {
    #[serde(default)]
    pub status: std::collections::HashSet<String>,
    #[serde(default)]
    pub spark_type: std::collections::HashSet<String>,
    #[serde(default)]
    pub priority: std::collections::HashSet<i32>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub sort_mode: String,
    #[serde(default)]
    pub show_closed: bool,
}

/// Load per-workshop UI state from `.ryve/ui_state.json`. Returns the
/// default (empty) state on any missing-file or parse error — UI state
/// is cosmetic and must never block workshop open. A corrupted file
/// logs a warning and falls back to the default. Spark ryve-27e33825.
pub async fn load_ui_state(ryve_dir: &RyveDir) -> UiState {
    let path = ryve_dir.ui_state_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(state) => state,
            Err(e) => {
                log::warn!("corrupted .ryve/ui_state.json, falling back to defaults: {e}");
                UiState::default()
            }
        },
        Err(_) => UiState::default(),
    }
}

/// Save per-workshop UI state to `.ryve/ui_state.json`. The parent
/// `.ryve/` directory is assumed to already exist — every caller has
/// already passed through `ensure_exists`/`migrate_workshop`.
pub async fn save_ui_state(ryve_dir: &RyveDir, state: &UiState) -> Result<(), std::io::Error> {
    let content =
        serde_json::to_string_pretty(state).map_err(|e| std::io::Error::other(e.to_string()))?;
    tokio::fs::write(ryve_dir.ui_state_path(), content).await
}

/// Load all custom agent definitions from `.ryve/agents/*.toml`.
pub async fn load_agent_defs(ryve_dir: &RyveDir) -> Vec<AgentDef> {
    let agents_dir = ryve_dir.agents_dir();
    let mut defs = Vec::new();

    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(entries) => entries,
        Err(_) => return defs,
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml")
            && let Ok(content) = tokio::fs::read_to_string(&path).await
            && let Ok(def) = toml::from_str::<AgentDef>(&content)
        {
            defs.push(def);
        }
    }

    defs
}

/// Load the context file for agents (`.ryve/context/AGENTS.md`).
/// Returns None if it doesn't exist.
pub async fn load_agents_context(ryve_dir: &RyveDir) -> Option<String> {
    tokio::fs::read_to_string(ryve_dir.agents_md_path())
        .await
        .ok()
}

/// Initialize a new `.ryve/` directory with default files.
///
/// Backwards-compatible wrapper around [`crate::migrations::migrate_workshop`].
/// New code should call `migrate_workshop` directly to receive the migration log.
pub async fn init_ryve_dir(ryve_dir: &RyveDir) -> Result<(), std::io::Error> {
    crate::migrations::migrate_workshop(ryve_dir)
        .await
        .map(|_| ())
}

pub(crate) const DEFAULT_AGENTS_MD: &str =
    "# Agent Instructions\n\nAdd project-specific instructions for coding agents here.\n";

/// Universal CLI reference and agent skill file, written to the workshop
/// root as `RYVE.md` during init and propagated to every Hand worktree
/// via agent-context sync. Source of truth: `data/defaults/RYVE.md`.
pub(crate) const DEFAULT_RYVE_MD: &str = include_str!("../defaults/RYVE.md");

pub(crate) const DEFAULT_DONE_MD: &str = r#"# DONE Checklist

A spark is only "done" when ALL of the following are true. Verify each item
before closing the spark with `ryve spark close <id>`.

## Code
- [ ] All acceptance criteria from the spark intent are satisfied
- [ ] Code compiles cleanly (no new warnings introduced)
- [ ] No `todo!()`, `unimplemented!()`, or stub functions left behind
- [ ] No debug prints, `dbg!`, or commented-out code

## Tests
- [ ] New behavior has at least one test (unit or integration)
- [ ] All existing tests still pass
- [ ] Edge cases identified in the spark are covered

## Workgraph hygiene
- [ ] Commit messages reference the spark id: `[sp-xxxx]`
- [ ] Any new bugs/tasks discovered were created as new sparks
- [ ] All required contracts on the spark pass (`ryve contract list <id>`)
- [ ] Architectural constraints respected (`ryve constraint list`)

## Done
- [ ] Spark closed: `ryve spark close <id> completed`
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // Spark ryve-926870a9: per-workshop UI state persistence.

    #[tokio::test]
    async fn ui_state_load_missing_file_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();
        let state = load_ui_state(&ryve_dir).await;
        assert!(state.collapsed_epics.is_empty());
    }

    #[tokio::test]
    async fn ui_state_roundtrip_preserves_collapsed_epics() {
        let tmp = tempfile::tempdir().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let mut state = UiState::default();
        state.collapsed_epics.insert("ep-1".to_string());
        state.collapsed_epics.insert("ep-2".to_string());
        save_ui_state(&ryve_dir, &state).await.unwrap();

        let reloaded = load_ui_state(&ryve_dir).await;
        assert_eq!(reloaded, state);
    }

    #[tokio::test]
    async fn ui_state_load_tolerates_garbage_json() {
        // Cosmetic state must never block workshop open — a corrupted
        // file should silently fall back to the default.
        let tmp = tempfile::tempdir().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();
        tokio::fs::write(ryve_dir.ui_state_path(), "not json")
            .await
            .unwrap();
        let state = load_ui_state(&ryve_dir).await;
        assert!(state.collapsed_epics.is_empty());
    }

    // Spark ryve-27e33825: sparks filter persistence.

    #[tokio::test]
    async fn ui_state_roundtrip_preserves_sparks_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let state = UiState {
            sparks_filter: SparksFilterState {
                status: ["open".to_string()].into_iter().collect(),
                priority: [0, 1].into_iter().collect(),
                show_closed: true,
                sort_mode: "recently_updated".to_string(),
                search: "auth".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        save_ui_state(&ryve_dir, &state).await.unwrap();

        let reloaded = load_ui_state(&ryve_dir).await;
        assert_eq!(reloaded.sparks_filter, state.sparks_filter);
        assert_eq!(reloaded.version, 1);
    }

    #[tokio::test]
    async fn ui_state_without_filter_field_deserialises_to_default() {
        // Older ui_state.json files won't have sparks_filter — they must
        // still load cleanly with the default filter (forward compat).
        let tmp = tempfile::tempdir().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();
        tokio::fs::write(ryve_dir.ui_state_path(), r#"{"collapsed_epics":["ep-1"]}"#)
            .await
            .unwrap();

        let state = load_ui_state(&ryve_dir).await;
        assert!(state.collapsed_epics.contains("ep-1"));
        assert_eq!(state.sparks_filter, SparksFilterState::default());
    }

    // Spark ryve-5a0e1d97: IRC lifecycle integration.
    // Spark ryve-300f661c [sp-31659bbb]: default-on IRC + bundled-port resolution.

    #[test]
    fn irc_enabled_by_default() {
        // A fresh WorkshopConfig represents a workshop that has passed
        // through `ryve init`; IRC is the coordination backbone and must
        // default on. The non-IRC fields stay None/unset — the bundled
        // port is written by `ryve init` (spark ryve-4d5881c2).
        let cfg = WorkshopConfig::default();
        assert!(cfg.irc_enabled());
        assert!(cfg.irc_server.is_none());
        assert!(cfg.irc_port.is_none());
        assert!(cfg.irc_tls.is_none());
        assert!(cfg.irc_nick.is_none());
        assert!(cfg.irc_password.is_none());
        assert!(cfg.irc_bundled_port.is_none());
    }

    #[test]
    fn irc_enabled_honours_explicit_opt_out() {
        // Users can still disable the whole subsystem by flipping the
        // master switch in `.ryve/config.toml`.
        let cfg = WorkshopConfig {
            irc_enabled: false,
            ..Default::default()
        };
        assert!(!cfg.irc_enabled());
    }

    #[test]
    fn effective_irc_server_address_uses_bundled_port_on_fresh_config() {
        // Freshly-inited workshop: `ryve init` has recorded a bundled
        // port but no explicit override. The helper resolves to the
        // workshop-local daemon.
        let cfg = WorkshopConfig {
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_irc_server_address().as_deref(),
            Some("127.0.0.1:6971"),
        );
    }

    #[test]
    fn effective_irc_server_address_honours_explicit_override() {
        // Mesh / cross-workshop setup: an explicit `irc_server` wins
        // over the bundled port, and the helper composes host:port via
        // `effective_irc_port`.
        let cfg = WorkshopConfig {
            irc_server: Some("irc.libera.chat".into()),
            irc_port: Some(6697),
            irc_tls: Some(true),
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_irc_server_address().as_deref(),
            Some("irc.libera.chat:6697"),
        );
    }

    #[test]
    fn effective_irc_server_address_override_defaults_port() {
        // Override with no explicit port falls back to 6667 for plain
        // and 6697 for TLS — the same logic as `effective_irc_port`.
        let plain = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert_eq!(
            plain.effective_irc_server_address().as_deref(),
            Some("irc.example.com:6667"),
        );

        let tls = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            irc_tls: Some(true),
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert_eq!(
            tls.effective_irc_server_address().as_deref(),
            Some("irc.example.com:6697"),
        );
    }

    #[test]
    fn effective_irc_server_address_none_without_override_or_bundled() {
        // Pre-init state (no override, no bundled port allocated yet):
        // the lifecycle treats `None` as "skip IRC this boot".
        let cfg = WorkshopConfig::default();
        assert!(cfg.effective_irc_server_address().is_none());
    }

    #[test]
    fn effective_irc_server_address_blank_override_falls_back_to_bundled() {
        // A whitespace-only `irc_server` is treated as no override so a
        // stray blank line in config.toml doesn't brick IRC for the
        // workshop.
        let cfg = WorkshopConfig {
            irc_server: Some("   ".into()),
            irc_bundled_port: Some(6971),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_irc_server_address().as_deref(),
            Some("127.0.0.1:6971"),
        );
    }

    #[test]
    fn irc_port_defaults_depend_on_tls() {
        let cfg_plain = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            ..Default::default()
        };
        assert_eq!(cfg_plain.effective_irc_port(), 6667);

        let cfg_tls = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            irc_tls: Some(true),
            ..Default::default()
        };
        assert_eq!(cfg_tls.effective_irc_port(), 6697);

        let cfg_custom = WorkshopConfig {
            irc_server: Some("irc.example.com".into()),
            irc_port: Some(9999),
            ..Default::default()
        };
        assert_eq!(cfg_custom.effective_irc_port(), 9999);
    }

    #[test]
    fn irc_nick_falls_back_to_ryve() {
        let cfg = WorkshopConfig::default();
        assert_eq!(cfg.effective_irc_nick(), "ryve");

        let cfg = WorkshopConfig {
            irc_nick: Some("  ".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_irc_nick(), "ryve");

        let cfg = WorkshopConfig {
            irc_nick: Some("bot".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_irc_nick(), "bot");
    }

    #[test]
    fn irc_config_round_trips_through_toml() {
        let cfg = WorkshopConfig {
            irc_enabled: true,
            irc_bundled_port: Some(6971),
            irc_server: Some("irc.example.com".into()),
            irc_port: Some(6697),
            irc_tls: Some(true),
            irc_nick: Some("ryvebot".into()),
            irc_password: Some("secret".into()),
            ..Default::default()
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        let restored: WorkshopConfig = toml::from_str(&serialized).expect("deserialize");
        assert!(restored.irc_enabled());
        assert_eq!(restored.irc_bundled_port, Some(6971));
        assert_eq!(restored.irc_server.as_deref(), Some("irc.example.com"));
        assert_eq!(restored.irc_port, Some(6697));
        assert_eq!(restored.irc_tls, Some(true));
        assert_eq!(restored.irc_nick.as_deref(), Some("ryvebot"));
        assert_eq!(restored.irc_password.as_deref(), Some("secret"));
    }

    #[test]
    fn irc_config_missing_fields_default_to_enabled() {
        // Legacy config files written before ryve-300f661c won't have
        // `irc_enabled` or `irc_bundled_port`. They must load cleanly
        // and inherit the new default-on behaviour — the supervisor
        // (spark ryve-242252b0) re-runs `ryve init` semantics if the
        // bundled port is missing.
        let legacy = r#"
            workshop_schema_version = 1
        "#;
        let cfg: WorkshopConfig = toml::from_str(legacy).expect("legacy parse");
        assert!(cfg.irc_enabled());
        assert!(cfg.irc_bundled_port.is_none());
        assert!(cfg.effective_irc_server_address().is_none());
    }

    #[tokio::test]
    async fn ui_state_version_defaults_to_one() {
        let tmp = tempfile::tempdir().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();
        tokio::fs::write(ryve_dir.ui_state_path(), r#"{}"#)
            .await
            .unwrap();

        let state = load_ui_state(&ryve_dir).await;
        assert_eq!(state.version, 1);
    }
}
