// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

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

    pub fn workshop_md_path(&self) -> PathBuf {
        self.root.join("WORKSHOP.md")
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
        tokio::fs::create_dir_all(self.checklists_dir()).await?;
        Ok(())
    }
}

// ── Workshop Config ────────────────────────────────────

/// Per-workshop configuration stored in `.ryve/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkshopConfig {
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
}

impl Default for WorkshopConfig {
    fn default() -> Self {
        Self {
            name: None,
            github: GitHubConfig::default(),
            layout: LayoutConfig::default(),
            default_assignee: None,
            default_owner: None,
            explorer: ExplorerConfig::default(),
            background: BackgroundConfig::default(),
            agents: AgentsConfig::default(),
        }
    }
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
        if path.extension().is_some_and(|ext| ext == "toml") {
            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                if let Ok(def) = toml::from_str::<AgentDef>(&content) {
                    defs.push(def);
                }
            }
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
pub async fn init_ryve_dir(ryve_dir: &RyveDir) -> Result<(), std::io::Error> {
    ryve_dir.ensure_exists().await?;

    // Write default config if missing
    if !ryve_dir.config_path().exists() {
        save_config(ryve_dir, &WorkshopConfig::default()).await?;
    }

    // Write default AGENTS.md if missing
    if !ryve_dir.agents_md_path().exists() {
        tokio::fs::write(
            ryve_dir.agents_md_path(),
            "# Agent Instructions\n\nAdd project-specific instructions for coding agents here.\n",
        )
        .await?;
    }

    // Write default DONE.md checklist if missing
    if !ryve_dir.done_md_path().exists() {
        tokio::fs::write(
            ryve_dir.done_md_path(),
            DEFAULT_DONE_MD,
        )
        .await?;
    }

    Ok(())
}

const DEFAULT_DONE_MD: &str = r#"# DONE Checklist

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
