// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! A Workshop is a self-contained workspace bound to a directory.
//! Each workshop has its own `.ryve/` directory containing config,
//! sparks database, agent definitions, and context files.

use std::collections::{HashMap, HashSet};
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


/// State for a pending agent spawn waiting for spark selection.
pub struct PendingAgentSpawn {
    pub agent: CodingAgent,
    pub is_custom: bool,
    pub custom_def: Option<AgentDef>,
    pub full_auto: bool,
}

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
    /// Cached count of failing or pending required contracts (loaded from DB).
    pub failing_contracts: usize,
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
    /// Currently selected spark ID (for detail view).
    pub selected_spark: Option<String>,
    /// Whether the background image is dark (for adaptive font color).
    /// `None` means no background or not yet computed.
    pub bg_is_dark: Option<bool>,
    /// Pending agent spawn -- shows spark picker before creating terminal.
    pub pending_agent_spawn: Option<PendingAgentSpawn>,
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
            failing_contracts: 0,
            custom_agents: Vec::new(),
            agent_context: None,
            background_handle: None,
            background_picker: PickerState::new(),
            spark_create_form: Default::default(),
            selected_spark: None,
            bg_is_dark: None,
            pending_agent_spawn: None,
        }
    }

    /// Stable workshop identifier for database queries.
    ///
    /// Derived from the directory name so it matches the CLI (`ryve`)
    /// and persists across app restarts. The `id` field (UUID) is only
    /// used for internal UI message routing.
    pub fn workshop_id(&self) -> String {
        self.directory
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
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
        self.file_viewers.insert(tab_id, FileViewerState::new(path));

        (tab_id, true)
    }

    /// Spawn a terminal tab, optionally running a coding agent command.
    /// When `full_auto` is true, the agent's auto-accept flags are appended.
    pub fn spawn_terminal(
        &mut self,
        title: String,
        agent: Option<&CodingAgent>,
        next_terminal_id: &mut u64,
        session_id: Option<&str>,
        full_auto: bool,
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

        // Inject Ryve env vars for Hand sessions so `ryve` CLI works
        // from inside worktrees without any cwd gymnastics.
        if agent.is_some() {
            for (k, v) in hand_env_vars(&self.directory) {
                settings.backend.env.insert(k, v);
            }
        }

        if let Some(agent) = agent {
            let mut args = agent.args.clone();

            // Inject full-auto flags when enabled
            if full_auto {
                args.extend(agent.full_auto_flags());
            }

            // Inject system prompt flag for agents that support it
            if let Some((flag, is_file)) = agent.system_prompt_flag() {
                let prompt_path = self.ryve_dir.workshop_md_path();
                if prompt_path.exists() {
                    args.push(flag.to_string());
                    if is_file {
                        args.push(prompt_path.to_string_lossy().into_owned());
                    } else {
                        // Inline text — read the file content
                        let content = std::fs::read_to_string(&prompt_path).unwrap_or_default();
                        args.push(content);
                    }
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

        // Inject Ryve env vars first, then custom agent env overrides
        for (k, v) in hand_env_vars(&self.directory) {
            settings.backend.env.insert(k, v);
        }
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
                if let Some(session) = self
                    .agent_sessions
                    .iter_mut()
                    .find(|s| s.tab_id == Some(id))
                {
                    session.name = title;
                }
            }
            iced_term::actions::Action::Ignore => {}
        }
    }

    /// Scan terminals for agent processes that aren't yet tracked as sessions.
    /// Returns `(tab_id, agent)` pairs for newly detected agents.
    pub fn detect_untracked_agents(&self) -> Vec<(u64, CodingAgent)> {
        // Collect tab IDs that already have an agent session
        let tracked_tabs: HashSet<u64> = self
            .agent_sessions
            .iter()
            .filter_map(|s| s.tab_id)
            .collect();

        let mut found = Vec::new();

        for (&tab_id, term) in &self.terminals {
            if tracked_tabs.contains(&tab_id) {
                continue;
            }

            let shell_pid = term.child_pid();
            if let Some(agent) = detect_agent_in_process_tree(shell_pid) {
                found.push((tab_id, agent));
            }
        }

        found
    }
}

/// Walk the process tree rooted at `shell_pid` looking for a known coding agent.
fn detect_agent_in_process_tree(shell_pid: u32) -> Option<CodingAgent> {
    use crate::coding_agents::ResumeStrategy;
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    // Known agent binary names → CodingAgent constructors
    let known: &[(&str, &str, ResumeStrategy)] = &[
        ("claude", "Claude Code", ResumeStrategy::ResumeFlag),
        ("codex", "Codex", ResumeStrategy::ResumeFlag),
        ("aider", "Aider", ResumeStrategy::None),
        ("opencode", "OpenCode", ResumeStrategy::None),
    ];

    let root = Pid::from_u32(shell_pid);

    // BFS through children of the shell process
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        for (child_pid, proc_info) in sys.processes() {
            if proc_info.parent() == Some(pid) {
                let name = proc_info.name().to_string_lossy();
                for &(cmd, display, ref resume) in known {
                    if name == cmd {
                        return Some(CodingAgent {
                            display_name: display.to_string(),
                            command: cmd.to_string(),
                            args: Vec::new(),
                            resume: resume.clone(),
                        });
                    }
                }
                queue.push(*child_pid);
            }
        }
    }

    None
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

/// Compute average luminance from image bytes (0.0 = black, 1.0 = white).
/// Samples a grid of pixels for speed rather than scanning every pixel.
pub fn compute_image_luminance(bytes: &[u8]) -> Option<f32> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    if w == 0 || h == 0 {
        return None;
    }

    // Sample ~100 pixels in a grid
    let step_x = (w / 10).max(1);
    let step_y = (h / 10).max(1);
    let mut total = 0.0_f64;
    let mut count = 0u32;

    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let p = rgb.get_pixel(x, y);
            // Relative luminance (ITU-R BT.709)
            let lum = 0.2126 * (p[0] as f64 / 255.0)
                + 0.7152 * (p[1] as f64 / 255.0)
                + 0.0722 * (p[2] as f64 / 255.0);
            total += lum;
            count += 1;
            x += step_x;
        }
        y += step_y;
    }

    Some((total / count as f64) as f32)
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
        .args(["worktree", "add", "-b", &branch, &wt_dir.to_string_lossy()])
        .current_dir(workshop_dir)
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    Ok(wt_dir)
}

/// Env vars to inject into every Hand's terminal so the `ryve` CLI works
/// from inside the worktree without requiring the user to cd or know
/// absolute paths.
///
/// - `RYVE_WORKSHOP_ROOT` — absolute path to the workshop directory.
///   The `ryve` binary reads this to locate `.ryve/sparks.db`.
/// - `PATH` — prepended with the directory containing the currently
///   running Ryve executable so `ryve <cmd>` resolves.
fn hand_env_vars(workshop_dir: &Path) -> Vec<(String, String)> {
    let mut vars = Vec::new();

    vars.push((
        "RYVE_WORKSHOP_ROOT".to_string(),
        workshop_dir.to_string_lossy().into_owned(),
    ));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let exe_dir_str = exe_dir.to_string_lossy().into_owned();
            let existing_path = std::env::var("PATH").unwrap_or_default();
            let new_path = if existing_path.is_empty() {
                exe_dir_str
            } else {
                format!("{exe_dir_str}:{existing_path}")
            };
            vars.push(("PATH".to_string(), new_path));
        }
    }

    vars
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

    // Run any pending workshop schema migrations. Returns the (now-current)
    // config plus a log of what ran so the caller can surface it to the user.
    let (config, migration_log) = data::migrations::migrate_workshop(&ryve_dir)
        .await
        .map_err(data::sparks::SparksError::Io)?;

    if migration_log.is_empty() {
        log::debug!("{}", migration_log.summary());
    } else {
        // Acceptance criterion: migration log printed to stdout (or UI toast).
        // Stdout is the simplest durable surface today; the log is also
        // returned in WorkshopInit so a UI toast can pick it up.
        println!("{}", migration_log.summary());
        log::info!("{}", migration_log.summary());
    }

    // Open/migrate database (sqlx handles its own schema migrations).
    let pool = data::db::open_sparks_db(&directory).await?;

    // Load agents in parallel — config already loaded by the migration step.
    let custom_agents = data::ryve_dir::load_agent_defs(&ryve_dir).await;
    let agent_context = data::ryve_dir::load_agents_context(&ryve_dir).await;

    // Generate WORKSHOP.md and inject pointers into agent boot files
    // (also propagates into any existing worktrees).
    if !config.agents.disable_sync {
        let _ = data::agent_context::sync(&directory, &ryve_dir, &config).await;
    }

    Ok(WorkshopInit {
        pool,
        config,
        custom_agents,
        agent_context,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workshop_id_derives_from_directory_name() {
        let ws = Workshop::new(PathBuf::from("/home/user/projects/my-project"));
        assert_eq!(ws.workshop_id(), "my-project");
    }

    #[test]
    fn workshop_id_matches_cli_derivation() {
        // The CLI derives workshop_id via: cwd.file_name().to_string_lossy()
        // This test ensures the UI method produces the same result.
        let dir = PathBuf::from("/tmp/ryve");
        let cli_id = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ws = Workshop::new(dir);
        assert_eq!(ws.workshop_id(), cli_id);
    }

    #[test]
    fn workshop_id_is_stable_across_instances() {
        let dir = PathBuf::from("/home/user/dev/ryve");
        let ws1 = Workshop::new(dir.clone());
        let ws2 = Workshop::new(dir);
        // UUIDs differ, but workshop_id is the same
        assert_ne!(ws1.id, ws2.id);
        assert_eq!(ws1.workshop_id(), ws2.workshop_id());
    }
}
