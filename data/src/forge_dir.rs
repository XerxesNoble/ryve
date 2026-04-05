// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! The `.forge/` directory — per-workshop configuration, state, and context.
//!
//! Every workshop has a `.forge/` directory at its root containing:
//!
//! ```text
//! .forge/
//! ├── config.toml       # Workshop configuration
//! ├── sparks.db         # SQLite database (sparks, bonds, embers, engravings)
//! ├── agents/           # Custom agent definitions
//! │   └── *.toml
//! └── context/          # Files that agents read for project context
//!     └── AGENTS.md     # Default agent instructions
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Paths within a `.forge/` directory.
#[derive(Debug, Clone)]
pub struct ForgeDir {
    root: PathBuf,
}

impl ForgeDir {
    pub fn new(workshop_dir: &Path) -> Self {
        Self {
            root: workshop_dir.join(".forge"),
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

    /// Create the `.forge/` directory structure if it doesn't exist.
    pub async fn ensure_exists(&self) -> Result<(), std::io::Error> {
        tokio::fs::create_dir_all(&self.root).await?;
        tokio::fs::create_dir_all(self.agents_dir()).await?;
        tokio::fs::create_dir_all(self.context_dir()).await?;
        Ok(())
    }
}

// ── Workshop Config ────────────────────────────────────

/// Per-workshop configuration stored in `.forge/config.toml`.
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
}

impl Default for WorkshopConfig {
    fn default() -> Self {
        Self {
            name: None,
            github: GitHubConfig::default(),
            layout: LayoutConfig::default(),
            default_assignee: None,
            default_owner: None,
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

    /// Sparks panel width in pixels.
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

/// A custom agent definition stored in `.forge/agents/*.toml`.
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

/// Load the workshop config from `.forge/config.toml`.
/// Returns default config if the file doesn't exist.
pub async fn load_config(forge_dir: &ForgeDir) -> WorkshopConfig {
    let path = forge_dir.config_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => toml::from_str(&content).unwrap_or_default(),
        Err(_) => WorkshopConfig::default(),
    }
}

/// Save the workshop config to `.forge/config.toml`.
pub async fn save_config(
    forge_dir: &ForgeDir,
    config: &WorkshopConfig,
) -> Result<(), std::io::Error> {
    let content =
        toml::to_string_pretty(config).map_err(|e| std::io::Error::other(e.to_string()))?;
    tokio::fs::write(forge_dir.config_path(), content).await
}

/// Load all custom agent definitions from `.forge/agents/*.toml`.
pub async fn load_agent_defs(forge_dir: &ForgeDir) -> Vec<AgentDef> {
    let agents_dir = forge_dir.agents_dir();
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

/// Load the context file for agents (`.forge/context/AGENTS.md`).
/// Returns None if it doesn't exist.
pub async fn load_agents_context(forge_dir: &ForgeDir) -> Option<String> {
    tokio::fs::read_to_string(forge_dir.agents_md_path())
        .await
        .ok()
}

/// Initialize a new `.forge/` directory with default files.
pub async fn init_forge_dir(forge_dir: &ForgeDir) -> Result<(), std::io::Error> {
    forge_dir.ensure_exists().await?;

    // Write default config if missing
    if !forge_dir.config_path().exists() {
        save_config(forge_dir, &WorkshopConfig::default()).await?;
    }

    // Write default AGENTS.md if missing
    if !forge_dir.agents_md_path().exists() {
        tokio::fs::write(
            forge_dir.agents_md_path(),
            "# Agent Instructions\n\nAdd project-specific instructions for coding agents here.\n",
        )
        .await?;
    }

    Ok(())
}
