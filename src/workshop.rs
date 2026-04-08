// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! A Workshop is a self-contained workspace bound to a directory.
//! Each workshop has its own `.ryve/` directory containing config,
//! sparks database, agent definitions, and context files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use data::ryve_dir::{AgentDef, RyveDir, WorkshopConfig};
use data::sparks::types::{Bond, Contract, Crew, CrewMember, Ember, HandAssignment, Spark};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::coding_agents::CodingAgent;
use crate::process_snapshot::ProcessSnapshot;
use crate::screen::agents::AgentSession;
use crate::screen::background_picker::PickerState;
use crate::screen::bench::{BenchState, TabKind};
use crate::screen::file_explorer::FileExplorerState;
use crate::screen::file_viewer::FileViewerState;
use crate::screen::log_tail::LogTailState;
use crate::style::{Appearance, Palette};

const BOTTOM_PIN_NEWLINES: usize = 20;

/// State for a pending agent spawn waiting for spark selection.
///
/// `agent` is `None` when the user opened the picker via "+ → New Hand"
/// (the agent is chosen *inside* the picker). It is `Some` when the user
/// picked a custom agent from the dropdown — the agent is already known
/// at the time the picker opens.
pub struct PendingAgentSpawn {
    pub agent: Option<CodingAgent>,
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
    /// Open spy views (read-only log tails for background Hands), keyed by
    /// tab ID. Spark ryve-8c14734a.
    pub log_tails: HashMap<u64, LogTailState>,
    /// File explorer state for this workshop.
    pub file_explorer: FileExplorerState,
    /// Workgraph database for this workshop.
    pub sparks_db: Option<SqlitePool>,
    /// Cached sparks for display (loaded from DB).
    pub sparks: Vec<Spark>,
    /// Cached count of failing or pending required contracts (loaded from DB).
    pub failing_contracts: usize,
    /// Cached failing/pending required contracts (loaded from DB) — used by
    /// the Home overview to render the failing list, not just a count.
    pub failing_contracts_list: Vec<Contract>,
    /// Active hand assignments across all sparks in this workshop. Loaded
    /// alongside sparks so the Home overview can join sparks ↔ Hands.
    pub hand_assignments: Vec<HandAssignment>,
    /// All crews owned by this workshop. Used by the Hands panel to render
    /// the Head → Crew → Hand hierarchy. Refreshed alongside sparks.
    pub crews: Vec<Crew>,
    /// Membership join for `crews`. Refreshed alongside sparks.
    pub crew_members: Vec<CrewMember>,
    /// UI state for the Hands panel (search query, history pagination,
    /// collapse flags). Held here so it survives panel re-renders without
    /// agents.rs needing to manage its own state.
    pub agents_panel: crate::screen::agents::AgentsPanelState,
    /// Active embers (Hand → Hand notifications) for this workshop. Refreshed
    /// on every sparks poll so the Home overview reflects current activity.
    pub embers: Vec<Ember>,
    /// Spark IDs seen as `blocked` at the last poll. Used to detect
    /// transitions into the blocked state so a Flash ember can be
    /// auto-created. Spark sp-ux0008.
    pub prev_blocked_spark_ids: HashSet<String>,
    /// Contract row IDs seen as failing at the last poll. Used to detect
    /// new contract failures so a Flare ember can be auto-created.
    /// Spark sp-ux0008.
    pub prev_failing_contract_ids: HashSet<i64>,
    /// Hand assignment row IDs seen as active at the last poll. Used to
    /// detect Hand-finish transitions so a Glow ember can be auto-created.
    /// Spark sp-ux0008.
    pub prev_active_assignment_ids: HashSet<i64>,
    /// Baseline-seen flags per ember source. Without these, the initial
    /// load of each source would emit a Flash/Flare/Glow ember for every
    /// pre-existing blocked spark, failing contract, and finished Hand.
    /// Only transitions observed *after* each baseline is captured should
    /// fire embers. Spark sp-ux0008.
    pub sparks_baseline_seen: bool,
    pub contracts_baseline_seen: bool,
    pub assignments_baseline_seen: bool,
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
    /// Inline status popover state for the workgraph panel.
    pub spark_status_menu: crate::screen::sparks::StatusMenu,
    /// Currently selected spark ID (for detail view).
    pub selected_spark: Option<String>,
    /// Cached contracts for the currently selected spark.
    pub selected_spark_contracts: Vec<Contract>,
    /// Cached bonds (dependency edges) for the currently selected spark.
    /// Includes bonds in both directions so the detail view can render
    /// "Blocks" and "Blocked by" lists.
    pub selected_spark_bonds: Vec<Bond>,
    /// Set of spark IDs that have at least one open blocking bond pointing
    /// at them. Recomputed alongside `sparks` so the panel can show a
    /// blocked indicator without re-querying per row.
    pub blocked_spark_ids: HashSet<String>,
    /// Inline contract-create form for the spark detail view.
    pub contract_create_form: crate::screen::spark_detail::ContractCreateForm,
    /// Whether the background image is dark (for adaptive font color).
    /// `None` means no background or not yet computed.
    pub bg_is_dark: Option<bool>,
    /// Pending agent spawn -- shows spark picker before creating terminal.
    pub pending_agent_spawn: Option<PendingAgentSpawn>,
    /// Pending Head spawn -- shows the Head picker overlay (agent + goal).
    pub pending_head_spawn: Option<crate::screen::head_picker::PickerState>,
    /// One-shot warning set when the last worktree creation fell back to
    /// the main workshop directory. The UI drains this to surface a toast.
    pub last_worktree_warning: Option<String>,
    /// True once the persisted open-tabs snapshot has been restored for
    /// this workshop. Guards the boot-time `load_open_tabs` chain so it
    /// only fires once per workshop session, not on every SparksPoll tick
    /// that happens to refresh `agent_sessions`.
    pub tabs_restored: bool,
    /// System appearance (dark/light) the workshop was last told about.
    /// Used to pick the terminal background color so light mode doesn't
    /// produce a jarring dark terminal pane. The App owns the source of
    /// truth and propagates it via [`Workshop::set_appearance`] before
    /// spawning terminals. Spark sp-ux0019.
    pub appearance: Appearance,
    /// Effective terminal font size, in points. Mirrors `Config::terminal_font_size`
    /// (with the default applied) so spawn_terminal can read it without
    /// holding a reference to the global config. Updated by main.rs whenever
    /// the user changes the size via Cmd+scroll or the Settings modal.
    /// Spark sp-ux0014.
    pub terminal_font_size: f32,
    /// Effective terminal font family. `None` falls back to `Font::MONOSPACE`.
    /// Spark sp-ux0014.
    pub terminal_font_family: Option<String>,
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
            log_tails: HashMap::new(),
            file_explorer: FileExplorerState::new(),
            sparks_db: None,
            sparks: Vec::new(),
            failing_contracts: 0,
            failing_contracts_list: Vec::new(),
            hand_assignments: Vec::new(),
            crews: Vec::new(),
            crew_members: Vec::new(),
            agents_panel: crate::screen::agents::AgentsPanelState::default(),
            embers: Vec::new(),
            prev_blocked_spark_ids: HashSet::new(),
            prev_failing_contract_ids: HashSet::new(),
            prev_active_assignment_ids: HashSet::new(),
            sparks_baseline_seen: false,
            contracts_baseline_seen: false,
            assignments_baseline_seen: false,
            custom_agents: Vec::new(),
            agent_context: None,
            background_handle: None,
            background_picker: PickerState::new(),
            spark_create_form: Default::default(),
            spark_status_menu: Default::default(),
            selected_spark: None,
            selected_spark_contracts: Vec::new(),
            selected_spark_bonds: Vec::new(),
            blocked_spark_ids: HashSet::new(),
            contract_create_form: Default::default(),
            bg_is_dark: None,
            pending_agent_spawn: None,
            pending_head_spawn: None,
            last_worktree_warning: None,
            tabs_restored: false,
            appearance: Appearance::Dark,
            terminal_font_size: data::config::DEFAULT_TERMINAL_FONT_SIZE,
            terminal_font_family: None,
        }
    }

    /// Build the iced_term font settings used when spawning a new terminal
    /// or coding-agent pane. Reads the workshop's currently-effective
    /// terminal font size and family. Spark sp-ux0014.
    pub fn terminal_font_settings(&self) -> iced_term::settings::FontSettings {
        let font_type = match &self.terminal_font_family {
            Some(name) => iced::Font {
                family: iced::font::Family::Name(Box::leak(name.clone().into_boxed_str())),
                ..iced::Font::MONOSPACE
            },
            None => iced::Font::MONOSPACE,
        };
        iced_term::settings::FontSettings {
            size: self.terminal_font_size,
            font_type,
            ..iced_term::settings::FontSettings::default()
        }
    }

    /// Update the appearance the workshop should use for newly spawned
    /// terminals. The App calls this whenever its detected system
    /// appearance changes (and on workshop creation).
    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance;
    }

    /// Effective palette for this workshop, honoring an adaptive
    /// background image override (`bg_is_dark`) and falling back to the
    /// system appearance. Mirrors the same selection used by
    /// `App::view_workshop` so the terminal background matches the
    /// surrounding UI.
    pub fn effective_palette(&self) -> Palette {
        match self.bg_is_dark {
            Some(true) => Palette::dark(),
            Some(false) => Palette::light(),
            None => self.appearance.palette(),
        }
    }

    /// Hex string for the terminal background, derived from the
    /// effective palette's window background.
    pub fn terminal_bg_hex(&self) -> String {
        let c = self.effective_palette().window_bg;
        format!(
            "#{:02x}{:02x}{:02x}",
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        )
    }

    /// Drain a pending worktree warning, if any. Returns the message so the
    /// caller can surface it as a toast.
    pub fn take_worktree_warning(&mut self) -> Option<String> {
        self.last_worktree_warning.take()
    }

    /// Take a snapshot of the bench's open tabs in a form suitable for
    /// persistence. The returned vec preserves left-to-right tab order via
    /// the `position` field.
    ///
    /// Tab kinds persisted:
    /// - `terminal`     — plain shell, restored as a fresh shell on boot
    /// - `file_viewer`  — payload is absolute file path
    /// - `coding_agent` — payload is `agent_sessions.id`. Only restored on
    ///   boot if the underlying session row still exists; ended sessions
    ///   are dropped (per product decision: "users should not be able to
    ///   reopen ended sessions").
    /// - `log_tail`     — payload is `agent_sessions.id` (spy view for a
    ///   background Hand). Restored only if the session is still active.
    ///
    /// `Home` is intentionally excluded — it's a singleton dashboard rebuilt
    /// from in-memory data on demand; persisting it would create a duplicate
    /// when the user reopens it manually.
    pub fn snapshot_open_tabs(&self) -> Vec<data::sparks::open_tab_repo::PersistedTab> {
        let workshop_id = self.workshop_id();
        // Look up the session id this tab belongs to, by walking
        // `agent_sessions` for a matching `tab_id`. Used by both
        // CodingAgent and LogTail tabs.
        let session_id_for_tab = |tab_id: u64| -> Option<String> {
            self.agent_sessions
                .iter()
                .find(|s| s.tab_id == Some(tab_id))
                .map(|s| s.id.clone())
        };

        self.bench
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(idx, tab)| {
                let (kind, payload) = match &tab.kind {
                    TabKind::Terminal => ("terminal", None),
                    TabKind::FileViewer(path) => {
                        ("file_viewer", Some(path.to_string_lossy().into_owned()))
                    }
                    TabKind::CodingAgent(_) => {
                        // We need the session id to be able to revive this
                        // tab on boot. If the tab somehow has no matching
                        // session row (shouldn't happen, but defensive),
                        // skip it.
                        let sid = session_id_for_tab(tab.id)?;
                        ("coding_agent", Some(sid))
                    }
                    TabKind::LogTail { session_id, .. } => ("log_tail", Some(session_id.clone())),
                    // Home is a singleton dashboard rebuilt from in-memory
                    // data on demand; persisting it would just create a
                    // duplicate when the user reopens it manually.
                    TabKind::Home => return None,
                };
                Some(data::sparks::open_tab_repo::PersistedTab {
                    workshop_id: workshop_id.clone(),
                    position: idx as i64,
                    tab_kind: kind.to_string(),
                    title: tab.title.clone(),
                    payload,
                })
            })
            .collect()
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

    /// Decide which side panels are visible at a given window width.
    /// Returns `(show_sidebar, show_sparks)`.
    ///
    /// Below ~880px the workgraph (right) panel collapses so the bench
    /// keeps a usable width. Below ~600px the file/agents sidebar (left)
    /// also collapses, leaving the bench to fill the window. The bench
    /// itself is never hidden — it's always the primary surface.
    /// sp-ux0025.
    pub fn responsive_panels(window_width: f32) -> (bool, bool) {
        let show_sparks = window_width >= 880.0;
        let show_sidebar = window_width >= 600.0;
        (show_sidebar, show_sparks)
    }

    /// Open the Home overview tab, or focus the existing one if it's
    /// already open. Singleton — repeated invocations are no-ops beyond
    /// activating the tab. Returns the tab id.
    pub fn open_home_tab(&mut self, next_terminal_id: &mut u64) -> u64 {
        if let Some(existing) = self
            .bench
            .tabs
            .iter()
            .find(|t| matches!(t.kind, TabKind::Home))
            .map(|t| t.id)
        {
            self.bench.active_tab = Some(existing);
            return existing;
        }

        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        self.bench
            .create_tab(tab_id, "Home".to_string(), TabKind::Home);
        tab_id
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

    /// Open a read-only spy view tailing a Hand's log file. Returns the
    /// tab id and whether the tab was newly created (`true`) or an existing
    /// spy tab for the same session was reused (`false`). The caller is
    /// responsible for kicking off the initial `log_tail::load_tail` task.
    /// Spark ryve-8c14734a.
    pub fn open_log_tab(
        &mut self,
        session_id: &str,
        log_path: PathBuf,
        next_terminal_id: &mut u64,
    ) -> (u64, bool) {
        // If a spy view for this session is already open, focus it.
        for tab in &self.bench.tabs {
            if let TabKind::LogTail {
                session_id: sid, ..
            } = &tab.kind
                && sid == session_id
            {
                self.bench.active_tab = Some(tab.id);
                return (tab.id, false);
            }
        }

        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;

        let title = format!(
            "spy: {}",
            log_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("hand")
        );

        self.bench.create_tab(
            tab_id,
            title,
            TabKind::LogTail {
                session_id: session_id.to_string(),
                log_path: log_path.clone(),
            },
        );
        self.log_tails.insert(tab_id, LogTailState::new(log_path));

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
        let working_dir = if let (Some(_), Some(sid)) = (agent, session_id) {
            match create_hand_worktree(&self.directory, &self.ryve_dir, sid) {
                Ok(wt_path) => wt_path,
                Err(e) => {
                    log::warn!("Failed to create worktree for hand {sid}: {e}");
                    self.last_worktree_warning = Some(format!(
                        "Failed to create worktree for hand {sid}: {e}. Falling back to workshop root."
                    ));
                    self.directory.clone()
                }
            }
        } else {
            self.directory.clone()
        };

        let mut settings = iced_term::settings::Settings {
            font: self.terminal_font_settings(),
            ..iced_term::settings::Settings::default()
        };
        settings.theme.color_pallete.background = self.terminal_bg_hex();
        settings.backend.working_directory = Some(working_dir);

        // Inject Ryve env vars for Hand sessions so `ryve` CLI works
        // from inside worktrees without any cwd gymnastics.
        if agent.is_some() {
            for (k, v) in hand_env_vars(&self.directory) {
                settings.backend.env.insert(k, v);
            }
            // Tell the running agent its own session id. The CLI handler
            // for `ryve hand spawn` reads this env var and uses it as the
            // new child's `parent_session_id`, so children appear under
            // their parent in the Hands panel hierarchy.
            if let Some(sid) = session_id {
                settings
                    .backend
                    .env
                    .insert("RYVE_HAND_SESSION_ID".to_string(), sid.to_string());
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
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            }),
        );

        // Create a worktree for this hand
        let working_dir = match create_hand_worktree(&self.directory, &self.ryve_dir, session_id) {
            Ok(wt_path) => wt_path,
            Err(e) => {
                log::warn!("Failed to create worktree for hand {session_id}: {e}");
                self.last_worktree_warning = Some(format!(
                    "Failed to create worktree for hand {session_id}: {e}. Falling back to workshop root."
                ));
                self.directory.clone()
            }
        };

        let mut settings = iced_term::settings::Settings {
            font: self.terminal_font_settings(),
            ..iced_term::settings::Settings::default()
        };
        settings.theme.color_pallete.background = self.terminal_bg_hex();
        settings.backend.working_directory = Some(working_dir);
        (settings.backend.program, settings.backend.args) =
            wrap_command_with_bottom_pin(&def.command, &def.args);

        // Inject Ryve env vars first, then custom agent env overrides.
        for (k, v) in hand_env_vars(&self.directory) {
            settings.backend.env.insert(k, v);
        }
        // Tell the running custom agent its own session id so any
        // `ryve hand spawn` it issues is correctly attributed to itself
        // as the parent.
        settings
            .backend
            .env
            .insert("RYVE_HAND_SESSION_ID".to_string(), session_id.to_string());
        for (k, v) in &def.env {
            settings.backend.env.insert(k.clone(), v.clone());
        }

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
        }

        tab_id
    }

    /// Handle terminal shutdown/title-change for a given terminal id.
    pub fn handle_terminal_action(
        &mut self,
        id: u64,
        action: iced_term::actions::Action,
    ) -> Vec<String> {
        match action {
            iced_term::actions::Action::Shutdown => {
                self.terminals.remove(&id);
                let ended_sessions = self.end_agent_sessions_for_tab(id);
                self.bench.close_tab(id);
                ended_sessions
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
                Vec::new()
            }
            iced_term::actions::Action::Ignore => Vec::new(),
        }
    }

    pub fn end_agent_sessions_for_tab(&mut self, id: u64) -> Vec<String> {
        let mut ended_sessions = Vec::new();
        for session in self.agent_sessions.iter_mut() {
            if session.tab_id == Some(id) {
                session.tab_id = None;
                session.active = false;
                session.stale = false;
                ended_sessions.push(session.id.clone());
            }
        }
        ended_sessions
    }

    /// Scan terminals for agent processes that aren't yet tracked as sessions.
    /// Returns `(tab_id, agent)` pairs for newly detected agents.
    ///
    /// Reads from a shared [`ProcessSnapshot`] captured once per
    /// `SparksPoll` tick (spark `ryve-a5b9e4a1`) — this used to take its
    /// own `System::new()` + `refresh_processes` per untracked terminal,
    /// on the UI thread.
    pub fn detect_untracked_agents(&self, snapshot: &ProcessSnapshot) -> Vec<(u64, CodingAgent)> {
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
            if let Some(agent) = snapshot.detect_agent_in_tree(shell_pid) {
                found.push((tab_id, agent));
            }
        }

        found
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
///
/// Visible to the rest of the crate so the `hand_spawn` CLI helper can call
/// it without re-implementing the worktree convention.
pub(crate) fn create_hand_worktree(
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

    // Drop AGENTS.md into the worktree so agents without a system-prompt
    // CLI flag (codex, opencode) still see WORKSHOP.md instructions.
    let workshop_md = ryve_dir.workshop_md_path();
    if workshop_md.exists() {
        let agents_md = wt_dir.join("AGENTS.md");
        if !agents_md.exists()
            && let Err(e) = std::fs::copy(&workshop_md, &agents_md)
        {
            log::warn!("Failed to write AGENTS.md to worktree: {e}");
        }
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
pub(crate) fn hand_env_vars(workshop_dir: &Path) -> Vec<(String, String)> {
    let mut vars = Vec::new();

    vars.push((
        "RYVE_WORKSHOP_ROOT".to_string(),
        workshop_dir.to_string_lossy().into_owned(),
    ));

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let exe_dir_str = exe_dir.to_string_lossy().into_owned();
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = if existing_path.is_empty() {
            exe_dir_str
        } else {
            format!("{exe_dir_str}:{existing_path}")
        };
        vars.push(("PATH".to_string(), new_path));
    }

    vars
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
    use crate::coding_agents::{CodingAgent, ResumeStrategy};
    use crate::screen::agents::AgentSession;

    #[test]
    fn terminal_font_settings_uses_workshop_size() {
        // Spark sp-ux0014: spawn_terminal must read the workshop's
        // configured font size instead of a hardcoded 14.0.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.terminal_font_size = 22.0;
        let font = ws.terminal_font_settings();
        assert_eq!(font.size, 22.0);
    }

    #[test]
    fn terminal_font_settings_defaults_to_14() {
        // Spark sp-ux0014: a workshop with no override still gets 14pt.
        let ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        assert_eq!(
            ws.terminal_font_size,
            data::config::DEFAULT_TERMINAL_FONT_SIZE
        );
        assert_eq!(ws.terminal_font_settings().size, 14.0);
    }

    #[test]
    fn terminal_font_settings_applies_family_override() {
        // Spark sp-ux0014: a configured family must propagate into the
        // FontSettings handed to iced_term.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.terminal_font_family = Some("JetBrains Mono".to_string());
        let font = ws.terminal_font_settings();
        match font.font_type.family {
            iced::font::Family::Name(name) => assert_eq!(name, "JetBrains Mono"),
            other => panic!("expected named family, got {other:?}"),
        }
    }

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
    fn terminal_bg_follows_appearance_and_adaptive_palette() {
        // Spark sp-ux0019: terminal background must reflect the actual
        // appearance, not a hardcoded dark theme.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));

        // Default (dark) appearance, no background image — picks the dark
        // window background.
        ws.set_appearance(Appearance::Dark);
        let dark_hex = ws.terminal_bg_hex();
        let dark_expected = {
            let c = Palette::dark().window_bg;
            format!(
                "#{:02x}{:02x}{:02x}",
                (c.r * 255.0).round() as u8,
                (c.g * 255.0).round() as u8,
                (c.b * 255.0).round() as u8,
            )
        };
        assert_eq!(dark_hex, dark_expected);

        // Light appearance, no background image — must NOT return the dark
        // hex (this was the bug: light mode produced a dark terminal).
        ws.set_appearance(Appearance::Light);
        let light_hex = ws.terminal_bg_hex();
        let light_expected = {
            let c = Palette::light().window_bg;
            format!(
                "#{:02x}{:02x}{:02x}",
                (c.r * 255.0).round() as u8,
                (c.g * 255.0).round() as u8,
                (c.b * 255.0).round() as u8,
            )
        };
        assert_eq!(light_hex, light_expected);
        assert_ne!(light_hex, dark_hex);

        // Adaptive override: a dark background image forces the dark
        // palette even when system appearance is Light.
        ws.bg_is_dark = Some(true);
        assert_eq!(ws.terminal_bg_hex(), dark_expected);

        // And vice versa — a light background image forces the light
        // palette even when system appearance is Dark.
        ws.set_appearance(Appearance::Dark);
        ws.bg_is_dark = Some(false);
        assert_eq!(ws.terminal_bg_hex(), light_expected);
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

    #[test]
    fn ending_tab_marks_agent_ended_not_stale() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.agent_sessions.push(AgentSession {
            id: "session-1".to_string(),
            name: "Codex".to_string(),
            agent: CodingAgent {
                display_name: "Codex".to_string(),
                command: "codex".to_string(),
                args: Vec::new(),
                resume: ResumeStrategy::None,
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            },
            tab_id: Some(7),
            active: true,
            stale: false,
            resume_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            log_path: None,
            last_output_at: None,
            parent_session_id: None,
        });

        let ended = ws.end_agent_sessions_for_tab(7);

        assert_eq!(ended, vec!["session-1".to_string()]);
        assert_eq!(ws.agent_sessions[0].tab_id, None);
        assert!(!ws.agent_sessions[0].active);
        assert!(!ws.agent_sessions[0].stale);
    }

    // sp-ux0025: responsive panel collapse at small window sizes.
    #[test]
    fn responsive_panels_wide_shows_everything() {
        // Comfortable desktop width — sidebar + bench + sparks all visible.
        let (sidebar, sparks) = Workshop::responsive_panels(1400.0);
        assert!(sidebar);
        assert!(sparks);
    }

    #[test]
    fn responsive_panels_medium_collapses_sparks() {
        // ~800px (the threshold called out in the spark): sparks panel
        // hides so the bench has room, sidebar still visible.
        let (sidebar, sparks) = Workshop::responsive_panels(800.0);
        assert!(sidebar);
        assert!(!sparks);
    }

    #[test]
    fn responsive_panels_narrow_collapses_both() {
        // Below 600px nothing but the bench fits comfortably.
        let (sidebar, sparks) = Workshop::responsive_panels(560.0);
        assert!(!sidebar);
        assert!(!sparks);
    }

    #[test]
    fn responsive_panels_thresholds_are_monotonic() {
        // Sanity: as the window grows, panels can only appear, never
        // disappear. Walk a range of widths and assert no flicker.
        let mut prev = (false, false);
        for w in (300..1600).step_by(20) {
            let cur = Workshop::responsive_panels(w as f32);
            assert!(
                cur.0 >= prev.0 && cur.1 >= prev.1,
                "panels regressed at width {w}: prev={prev:?} cur={cur:?}"
            );
            prev = cur;
        }
    }

    #[test]
    fn bottom_pin_newlines_is_modest() {
        // Spark sp-ux0027: 200 newlines polluted scrollback. Keep this small
        // (<= 30) so scroll-up history isn't drowned in blank lines.
        assert!(BOTTOM_PIN_NEWLINES <= 30);
        assert!(BOTTOM_PIN_NEWLINES >= 10);
    }

    #[test]
    fn wrap_command_with_bottom_pin_uses_constant() {
        let (shell, args) = wrap_command_with_bottom_pin("echo", &["hi".to_string()]);
        assert_eq!(shell, "/bin/sh");
        assert_eq!(args[0], "-lc");
        assert!(
            args[1].contains(&format!("-lt {BOTTOM_PIN_NEWLINES}")),
            "wrapped command should embed BOTTOM_PIN_NEWLINES loop bound: {}",
            args[1]
        );
    }
}
