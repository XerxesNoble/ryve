// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! A Workshop is a self-contained workspace bound to a directory.
//! Each workshop has its own `.ryve/` directory containing config,
//! sparks database, agent definitions, and context files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use data::ryve_dir::{AgentDef, RyveDir, WorkshopConfig};
use data::sparks::types::Spark;
use iced::Theme;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::coding_agents::CodingAgent;
use crate::screen::agents::AgentSession;
use crate::screen::background_picker::PickerState;
use crate::screen::bench::{BenchState, TabKind};
use crate::screen::file_explorer::FileExplorerState;
use crate::screen::file_viewer::FileViewerState;

const BOTTOM_PIN_NEWLINES: usize = 200;

pub struct Workshop {
    pub id: Uuid,
    pub directory: PathBuf,
    pub ryve_dir: RyveDir,
    pub config: WorkshopConfig,
    pub bench: BenchState,
    pub terminals: HashMap<u64, iced_term::Terminal>,
    pub agent_sessions: Vec<AgentSession>,
    /// Open file viewer states, keyed by tab ID.
    pub file_viewers: HashMap<u64, FileViewerState>,
    /// File explorer state for this workshop.
    pub file_explorer: FileExplorerState,
    /// Workgraph database for this workshop.
    pub sparks_db: Option<SqlitePool>,
    /// Cached sparks for display (loaded from DB).
    pub sparks: Vec<Spark>,
    /// Custom agent definitions from `.ryve/agents/`.
    pub custom_agents: Vec<AgentDef>,
    /// Agent context from `.ryve/context/AGENTS.md`.
    pub agent_context: Option<String>,
    /// Loaded background image handle.
    pub background_handle: Option<iced::widget::image::Handle>,
    /// Background picker modal state.
    pub background_picker: PickerState,
    /// Inline spark create form state.
    pub spark_create_form: crate::screen::sparks::CreateForm,
}

impl Workshop {
    pub fn new(directory: PathBuf) -> Self {
        let ryve_dir = RyveDir::new(&directory);
        Self {
            id: Uuid::new_v4(),
            directory,
            ryve_dir,
            config: WorkshopConfig::default(),
            bench: BenchState::new(),
            terminals: HashMap::new(),
            agent_sessions: Vec::new(),
            file_viewers: HashMap::new(),
            file_explorer: FileExplorerState::new(),
            sparks_db: None,
            sparks: Vec::new(),
            custom_agents: Vec::new(),
            agent_context: None,
            background_handle: None,
            background_picker: PickerState::new(),
            spark_create_form: Default::default(),
        }
    }

    /// Display name — from config, or last path component.
    pub fn name(&self) -> &str {
        self.config.name.as_deref().unwrap_or_else(|| {
            self.directory
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workshop")
        })
    }

    /// Sidebar split ratio from config.
    pub fn sidebar_split(&self) -> f32 {
        self.config.layout.sidebar_split
    }

    /// Sidebar width from config.
    pub fn sidebar_width(&self) -> f32 {
        self.config.layout.sidebar_width
    }

    /// Workgraph panel width from config.
    pub fn sparks_width(&self) -> f32 {
        self.config.layout.sparks_width
    }

    /// Open a file viewer tab, or switch to it if already open.
    /// Returns the tab ID and whether it was newly created (true) or reused (false).
    pub fn open_file_tab(&mut self, path: PathBuf, next_terminal_id: &mut u64) -> (u64, bool) {
        // Check if this file is already open in an existing tab
        for (tab_id, viewer) in &self.file_viewers {
            if viewer.path == path {
                self.bench.active_tab = Some(*tab_id);
                return (*tab_id, false);
            }
        }

        // Create new tab
        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;

        let title = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        self.bench
            .create_tab(tab_id, title, TabKind::FileViewer(path.clone()));
        self.file_viewers
            .insert(tab_id, FileViewerState::new(path));

        (tab_id, true)
    }

    /// Spawn a terminal tab, optionally running a coding agent command.
    pub fn spawn_terminal(
        &mut self,
        title: String,
        agent: Option<&CodingAgent>,
        next_terminal_id: &mut u64,
        session_id: Option<&str>,
    ) -> u64 {
        let kind = match agent {
            Some(a) => TabKind::CodingAgent(a.clone()),
            None => TabKind::Terminal,
        };

        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        self.bench.create_tab(tab_id, title, kind);

        // Create a worktree for agent sessions (not plain terminals)
        let working_dir = if let (Some(agent), Some(sid)) = (agent, session_id) {
            match create_hand_worktree(&self.directory, &self.ryve_dir, sid) {
                Ok(wt_path) => wt_path,
                Err(e) => {
                    log::warn!("Failed to create worktree for hand {sid}: {e}");
                    self.directory.clone()
                }
            }
        } else {
            self.directory.clone()
        };

        let mut settings = iced_term::settings::Settings::default();
        settings.font.size = 14.0;
        settings.theme.color_pallete.background = app_background_color();
        settings.backend.working_directory = Some(working_dir);

        if let Some(agent) = agent {
            let mut args = agent.args.clone();

            // Inject system prompt flag for agents that support it
            if let Some(flag) = agent.system_prompt_flag() {
                let prompt_path = self.ryve_dir.workshop_md_path();
                if prompt_path.exists() {
                    args.push(flag.to_string());
                    args.push(prompt_path.to_string_lossy().into_owned());
                }
            }

            (settings.backend.program, settings.backend.args) =
                wrap_command_with_bottom_pin(&agent.command, &args);
        } else {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            (settings.backend.program, settings.backend.args) =
                wrap_command_with_bottom_pin(&shell, &[]);
        }

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
        }

        tab_id
    }

    /// Spawn a terminal for a custom agent definition.
    pub fn spawn_custom_agent(
        &mut self,
        def: &AgentDef,
        next_terminal_id: &mut u64,
        session_id: &str,
    ) -> u64 {
        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        self.bench.create_tab(
            tab_id,
            def.name.clone(),
            TabKind::CodingAgent(CodingAgent {
                display_name: def.name.clone(),
                command: def.command.clone(),
                args: def.args.clone(),
                resume: crate::coding_agents::ResumeStrategy::None,
            }),
        );

        // Create a worktree for this hand
        let working_dir = match create_hand_worktree(&self.directory, &self.ryve_dir, session_id) {
            Ok(wt_path) => wt_path,
            Err(e) => {
                log::warn!("Failed to create worktree for hand {session_id}: {e}");
                self.directory.clone()
            }
        };

        let mut settings = iced_term::settings::Settings::default();
        settings.font.size = 14.0;
        settings.theme.color_pallete.background = app_background_color();
        settings.backend.working_directory = Some(working_dir);
        (settings.backend.program, settings.backend.args) =
            wrap_command_with_bottom_pin(&def.command, &def.args);
        for (k, v) in &def.env {
            settings.backend.env.insert(k.clone(), v.clone());
        }

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
        }

        tab_id
    }

    /// Handle terminal shutdown/title-change for a given terminal id.
    pub fn handle_terminal_action(&mut self, id: u64, action: iced_term::actions::Action) {
        match action {
            iced_term::actions::Action::Shutdown => {
                self.terminals.remove(&id);
                // Mark agent sessions as ended, keep in history
                for session in self.agent_sessions.iter_mut() {
                    if session.tab_id == Some(id) {
                        session.tab_id = None;
                        session.active = false;
                    }
                }
                self.bench.close_tab(id);
            }
            iced_term::actions::Action::ChangeTitle(title) => {
                if let Some(tab) = self.bench.tabs.iter_mut().find(|t| t.id == id) {
                    tab.title = title.clone();
                }
                if let Some(session) = self.agent_sessions.iter_mut().find(|s| s.tab_id == Some(id)) {
                    session.name = title;
                }
            }
            iced_term::actions::Action::Ignore => {}
        }
    }
}

fn wrap_command_with_bottom_pin(program: &str, args: &[String]) -> (String, Vec<String>) {
    let mut command = format!(
        "i=0; while [ \"$i\" -lt {BOTTOM_PIN_NEWLINES} ]; do printf '\\n'; i=$((i+1)); done; exec {}",
        shell_quote(program)
    );

    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }

    ("/bin/sh".to_string(), vec!["-lc".to_string(), command])
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Create a git worktree for a Hand session (blocking).
/// Returns the worktree path on success.
fn create_hand_worktree(
    workshop_dir: &Path,
    ryve_dir: &RyveDir,
    session_id: &str,
) -> Result<PathBuf, String> {
    // Only create worktrees for git repos
    let git_dir = workshop_dir.join(".git");
    if !git_dir.exists() {
        return Err("not a git repository".to_string());
    }

    let short_id = &session_id[..8.min(session_id.len())];
    let branch = format!("hand/{short_id}");
    let wt_dir = ryve_dir.root().join("worktrees").join(short_id);

    // Skip if worktree already exists
    if wt_dir.exists() {
        return Ok(wt_dir);
    }

    // Create parent dir
    std::fs::create_dir_all(wt_dir.parent().unwrap_or(ryve_dir.root()))
        .map_err(|e| e.to_string())?;

    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch,
            &wt_dir.to_string_lossy(),
        ])
        .current_dir(workshop_dir)
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    Ok(wt_dir)
}

fn app_background_color() -> String {
    let color = Theme::Dark.palette().background;

    format!(
        "#{:02x}{:02x}{:02x}",
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    )
}

/// Result of async workshop initialization.
pub struct WorkshopInit {
    pub pool: SqlitePool,
    pub config: WorkshopConfig,
    pub custom_agents: Vec<AgentDef>,
    pub agent_context: Option<String>,
}

/// Initialize a workshop's `.ryve/` directory, DB, and load config.
/// This is the single async entry point called when a workshop opens.
pub async fn init_workshop(directory: PathBuf) -> Result<WorkshopInit, data::sparks::SparksError> {
    let ryve_dir = RyveDir::new(&directory);

    // Create directory structure + default files
    data::ryve_dir::init_ryve_dir(&ryve_dir)
        .await
        .map_err(data::sparks::SparksError::Io)?;

    // Open/migrate database
    let pool = data::db::open_sparks_db(&directory).await?;

    // Load config and agents in parallel
    let config = data::ryve_dir::load_config(&ryve_dir).await;
    let custom_agents = data::ryve_dir::load_agent_defs(&ryve_dir).await;
    let agent_context = data::ryve_dir::load_agents_context(&ryve_dir).await;

    // Inject pointers into agent boot files (WORKSHOP.md will be written
    // once sparks are loaded — see SparksLoaded handler in main).
    if !config.agents.disable_sync {
        let ctx = data::agent_context::WorkshopContext {
            sparks: Vec::new(),
            constraints: Vec::new(),
            failing_contracts: Vec::new(),
            active_assignments: Vec::new(),
        };
        let _ = data::agent_context::sync(&directory, &ryve_dir, &config, &ctx).await;
    }

    Ok(WorkshopInit {
        pool,
        config,
        custom_agents,
        agent_context,
    })
}
