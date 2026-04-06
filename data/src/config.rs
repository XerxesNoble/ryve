// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the SQLite database file
    pub database_path: PathBuf,
    /// Root directory for projects
    pub workspace_dir: PathBuf,
    /// Custom font family name (e.g., "Menlo", "JetBrains Mono").
    /// When unset, uses the system default sans-serif font.
    #[serde(default)]
    pub font_family: Option<String>,

    /// Default coding agent to launch with Cmd+H (e.g. "claude", "codex").
    /// Must match a known agent command name. When unset, Cmd+H does nothing.
    #[serde(default)]
    pub default_agent: Option<String>,

    /// Per-agent settings keyed by command name (e.g. "claude", "codex").
    #[serde(default)]
    pub agent_settings: HashMap<String, AgentConfig>,
}

/// Per-agent configuration stored in the global config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Launch this agent in full-auto mode (no confirmation prompts).
    #[serde(default)]
    pub full_auto: bool,
}

impl Config {
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .expect("no config directory found")
            .join("ryve")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    /// Load the global config from disk, or create a default one.
    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => {
                let config = Self::default();
                config.save().ok();
                config
            }
        }
    }

    /// Save the global config to disk.
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, content)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_path: Self::config_dir().join("ryve.db"),
            workspace_dir: dirs::home_dir()
                .expect("no home directory found")
                .join("dev"),
            font_family: None,
            default_agent: None,
            agent_settings: HashMap::new(),
        }
    }
}
