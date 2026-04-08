// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

    /// Custom font family used inside terminal/coding-agent panes.
    /// When unset, uses the platform default monospace font.
    #[serde(default)]
    pub terminal_font_family: Option<String>,

    /// Terminal font size in points. Defaults to 14.0 when unset so that
    /// pre-existing configs keep their previous look. Spark sp-ux0014.
    #[serde(default)]
    pub terminal_font_size: Option<f32>,

    /// Default coding agent to launch with Cmd+H (e.g. "claude", "codex").
    /// Must match a known agent command name. When unset, Cmd+H does nothing.
    #[serde(default)]
    pub default_agent: Option<String>,

    /// Per-agent settings keyed by command name (e.g. "claude", "codex").
    #[serde(default)]
    pub agent_settings: HashMap<String, AgentConfig>,

    /// Recently opened workshop directories, most-recent first.
    /// Persisted across launches so the welcome screen can offer
    /// one-click reopen. Capped at `MAX_RECENT_WORKSHOPS` entries.
    #[serde(default)]
    pub recent_workshops: Vec<PathBuf>,
}

/// Maximum number of recent workshop entries to retain in the global config.
pub const MAX_RECENT_WORKSHOPS: usize = 10;

/// Default terminal font size (in points) used when no override is set.
pub const DEFAULT_TERMINAL_FONT_SIZE: f32 = 14.0;
/// Hard floor for the terminal font size — anything below this is unreadable
/// and starts producing degenerate cell measurements in iced_term.
pub const MIN_TERMINAL_FONT_SIZE: f32 = 6.0;
/// Hard ceiling for the terminal font size — keeps Cmd+scroll from running
/// the cell grid off the screen.
pub const MAX_TERMINAL_FONT_SIZE: f32 = 48.0;

impl Config {
    /// Effective terminal font size, applying the default + clamp.
    pub fn effective_terminal_font_size(&self) -> f32 {
        self.terminal_font_size
            .unwrap_or(DEFAULT_TERMINAL_FONT_SIZE)
            .clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE)
    }
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

    /// Record `path` as the most-recently opened workshop directory.
    /// Existing entries pointing at the same path are removed first
    /// so the list stays deduplicated, then the list is truncated to
    /// `MAX_RECENT_WORKSHOPS`. Caller is responsible for `save()`.
    pub fn add_recent_workshop(&mut self, path: PathBuf) {
        self.recent_workshops.retain(|p| p != &path);
        self.recent_workshops.insert(0, path);
        if self.recent_workshops.len() > MAX_RECENT_WORKSHOPS {
            self.recent_workshops.truncate(MAX_RECENT_WORKSHOPS);
        }
    }

    /// Drop a recent workshop entry by path. No-op if not present.
    pub fn remove_recent_workshop(&mut self, path: &Path) {
        self.recent_workshops.retain(|p| p.as_path() != path);
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

#[cfg(test)]
mod terminal_font_tests {
    use super::*;

    #[test]
    fn defaults_to_14() {
        let cfg = Config::default();
        assert_eq!(
            cfg.effective_terminal_font_size(),
            DEFAULT_TERMINAL_FONT_SIZE
        );
    }

    #[test]
    fn clamps_below_minimum() {
        let cfg = Config {
            terminal_font_size: Some(1.0),
            ..Config::default()
        };
        assert_eq!(cfg.effective_terminal_font_size(), MIN_TERMINAL_FONT_SIZE);
    }

    #[test]
    fn clamps_above_maximum() {
        let cfg = Config {
            terminal_font_size: Some(999.0),
            ..Config::default()
        };
        assert_eq!(cfg.effective_terminal_font_size(), MAX_TERMINAL_FONT_SIZE);
    }

    #[test]
    fn passes_through_in_range() {
        let cfg = Config {
            terminal_font_size: Some(18.0),
            ..Config::default()
        };
        assert_eq!(cfg.effective_terminal_font_size(), 18.0);
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
            terminal_font_family: None,
            terminal_font_size: None,
            default_agent: None,
            agent_settings: HashMap::new(),
            recent_workshops: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_recent_workshop_dedupes_and_caps() {
        let mut cfg = Config::default();
        for i in 0..(MAX_RECENT_WORKSHOPS + 5) {
            cfg.add_recent_workshop(PathBuf::from(format!("/tmp/ws{i}")));
        }
        assert_eq!(cfg.recent_workshops.len(), MAX_RECENT_WORKSHOPS);
        // Most recent first.
        assert_eq!(
            cfg.recent_workshops[0],
            PathBuf::from(format!("/tmp/ws{}", MAX_RECENT_WORKSHOPS + 4))
        );

        // Re-adding an existing path moves it to the front rather than
        // duplicating.
        let target = PathBuf::from("/tmp/ws7");
        cfg.add_recent_workshop(target.clone());
        assert_eq!(cfg.recent_workshops[0], target);
        let occurrences = cfg
            .recent_workshops
            .iter()
            .filter(|p| **p == target)
            .count();
        assert_eq!(occurrences, 1);
    }

    #[test]
    fn remove_recent_workshop_drops_entry() {
        let mut cfg = Config::default();
        cfg.add_recent_workshop(PathBuf::from("/a"));
        cfg.add_recent_workshop(PathBuf::from("/b"));
        cfg.remove_recent_workshop(Path::new("/a"));
        assert_eq!(cfg.recent_workshops, vec![PathBuf::from("/b")]);
    }
}
