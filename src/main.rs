// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

mod agent_prompts;
mod cli;
mod coding_agents;
mod hand_spawn;
mod icons;
mod screen;
mod style;
mod widget;
mod workshop;

use std::path::PathBuf;

use std::collections::HashSet;

use data::sparks::types::{Bond, Contract, PersistedAgentSession, Spark};
use iced::widget::{Space, button, column, container, row, stack, text};
use iced::{Color, Element, Length, Point, Size, Subscription, Task, Theme};
use iced::{event, keyboard, mouse, window};
use sysinfo::{Pid, ProcessesToUpdate, System};
use uuid::Uuid;

use coding_agents::CodingAgent;
use screen::agents::AgentSession;
use screen::file_explorer;
use screen::file_viewer;
use screen::toast::{self, Toast, ToastKind};
use style::Appearance;
use widget::splitter::{self, SplitterDrag, SplitterKind};
use workshop::Workshop;

fn process_is_alive(child_pid: i64) -> bool {
    let Ok(pid) = u32::try_from(child_pid) else {
        return false;
    };

    let mut system = System::new_all();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.process(Pid::from_u32(pid)).is_some()
}

fn main() -> iced::Result {
    // Dispatch: if the first non-flag arg is a known CLI subcommand,
    // run in CLI mode (tokio runtime). Otherwise launch the UI app.
    let args: Vec<String> = std::env::args().collect();
    let first_non_flag = args
        .iter()
        .skip(1)
        .find(|a| a.as_str() != "--json")
        .map(|s| s.as_str());

    if let Some(cmd) = first_non_flag {
        if cli::CLI_COMMANDS.contains(&cmd) {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(cli::run(args));
            return Ok(());
        }
    }

    // Load global config for font preferences
    let config = data::config::Config::load();
    let default_font = match config.font_family {
        Some(name) => iced::Font {
            family: iced::font::Family::Name(Box::leak(name.into_boxed_str())),
            ..iced::Font::DEFAULT
        },
        None => iced::Font {
            family: iced::font::Family::SansSerif,
            ..iced::Font::DEFAULT
        },
    };

    // Window settings — transparent title bar on macOS
    let mut window = iced::window::Settings {
        size: iced::Size::new(1400.0, 900.0),
        transparent: true,
        ..Default::default()
    };

    #[cfg(target_os = "macos")]
    {
        window.platform_specific.title_hidden = true;
        window.platform_specific.titlebar_transparent = true;
        window.platform_specific.fullsize_content_view = true;
    }

    iced::application(App::boot, App::update, App::view)
        .title("Ryve")
        .subscription(App::subscription)
        .theme(App::theme)
        .default_font(default_font)
        .window(window)
        .run()
}

struct App {
    /// System appearance (dark/light mode)
    appearance: Appearance,
    /// Global configuration (~/.config/ryve/config.toml)
    global_config: data::config::Config,
    /// Available coding agents detected on PATH
    available_agents: Vec<CodingAgent>,
    /// All open workshops
    workshops: Vec<Workshop>,
    /// Index of the active workshop in `workshops`
    active_workshop: Option<usize>,
    /// Global terminal ID counter (unique across all workshops)
    next_terminal_id: u64,
    /// Guard: true while a SparksPoll load is in flight
    poll_in_flight: bool,
    /// Whether the Shift key is currently held (for shift-click line selection).
    shift_held: bool,
    /// Active drag-to-resize state, if any.
    splitter_drag: Option<SplitterDrag>,
    /// Last known window size — used to convert vertical splitter
    /// drag deltas into a sidebar split ratio.
    window_size: Size,
    /// Active toast notifications (global across all workshops).
    toasts: Vec<Toast>,
    /// Monotonic counter for toast ids.
    next_toast_id: u64,
}

#[derive(Clone)]
enum Message {
    /// Workshop-level tab bar
    SelectWorkshop(usize),
    CloseWorkshop(usize),
    NewWorkshopDialog,
    WorkshopDirPicked(Option<PathBuf>),
    /// `workshop::init_workshop` failed (bad db, unreadable config, etc.).
    WorkshopInitFailed {
        id: Uuid,
        error: String,
    },

    /// Workshop .ryve/ initialized
    WorkshopReady {
        id: Uuid,
        pool: sqlx::SqlitePool,
        config: data::ryve_dir::WorkshopConfig,
        custom_agents: Vec<data::ryve_dir::AgentDef>,
        agent_context: Option<String>,
    },
    /// Workgraph sparks loaded from DB
    SparksLoaded(Uuid, Vec<Spark>),
    /// Failing/pending required contract count loaded from DB
    FailingContractsLoaded(Uuid, usize),
    /// Contracts for the currently selected spark loaded from DB.
    ContractsLoaded(Uuid, String, Vec<Contract>),
    /// Bonds (dependency edges) for the currently selected spark loaded
    /// from DB. Includes both incoming and outgoing edges so the detail
    /// view can render Blocks / Blocked-by lists.
    BondsLoaded(Uuid, String, Vec<Bond>),
    /// Set of spark IDs in the workshop that have at least one open
    /// blocking bond pointing at them. Computed on every sparks reload so
    /// the panel can show a "blocked" indicator next to each row.
    BlockedSparkIdsLoaded(Uuid, HashSet<String>),
    /// A contract check command finished — store the resolved status,
    /// then trigger a contracts reload for the spark.
    ContractCheckFinished {
        ws_id: Uuid,
        spark_id: String,
    },
    /// Agent sessions loaded from DB
    AgentSessionsLoaded(Uuid, Vec<PersistedAgentSession>),
    /// Agent session saved to DB
    AgentSessionSaved,
    /// Persisted open-tabs snapshot loaded from DB. Each entry is replayed
    /// against the bench to restore the user's prior tab list.
    OpenTabsLoaded(Uuid, Vec<data::sparks::open_tab_repo::PersistedTab>),
    /// Open-tabs snapshot persisted to DB.
    OpenTabsSaved,
    /// File tree scanned for a workshop
    FilesScanned(Uuid, file_explorer::Message),

    /// Forwarded to the active workshop
    FileExplorer(screen::file_explorer::Message),
    FileViewer(screen::file_viewer::Message),
    Agents(screen::agents::Message),
    Bench(screen::bench::Message),
    Sparks(screen::sparks::Message),
    SparkDetail(screen::spark_detail::Message),
    SparkPicker(screen::spark_picker::Message),
    HeadPicker(screen::head_picker::Message),
    Background(screen::background_picker::Message),
    StatusBar(screen::status_bar::Message),

    /// Background image loaded from disk
    BackgroundLoaded(Uuid, Option<Vec<u8>>),
    /// Unsplash photo downloaded to disk
    UnsplashDownloaded {
        filename: String,
        photographer: String,
        photographer_url: String,
    },
    /// Result of an Unsplash search request (success or error).
    UnsplashSearchResult(Result<data::unsplash::SearchResult, String>),
    /// Background photo download failed.
    UnsplashDownloadFailed(String),
    /// Local file copied to backgrounds dir
    LocalFileCopied(String),
    /// Background config saved
    BackgroundConfigSaved,
    /// Agent context files synced (WORKSHOP.md etc.)
    AgentContextSynced,
    /// Periodic sparks poll tick
    SparksPoll,
    /// Spawn a new Hand with the default agent (Cmd+H)
    NewDefaultHand,
    HandAssignmentSaved,
    /// Shift key state changed (for shift-click line selection).
    ShiftStateChanged(bool),
    /// Send initial spark prompt to a Hand's terminal after agent boots.
    SendSparkPrompt {
        tab_id: u64,
        prompt: String,
    },
    /// Submit the previously-pasted prompt by sending Enter.
    SubmitSparkPrompt {
        tab_id: u64,
    },

    /// User pressed a layout splitter handle.
    SplitterPressed(SplitterKind),
    /// Cursor moved while a splitter drag is active.
    SplitterMoved(Point),
    /// Mouse button released while a splitter drag is active.
    SplitterReleased,
    /// Layout config persisted to disk after a drag.
    LayoutSaved,
    /// Window was resized.
    WindowResized(Size),

    /// Toast notifications
    Toast(toast::Message),
    /// Push a new toast onto the stack from an async task.
    #[allow(dead_code)]
    ShowToast {
        title: String,
        body: String,
        kind: ToastKind,
    },
    /// A toast's lifetime elapsed — remove it if still present.
    ToastExpired(u64),
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelectWorkshop(i) => write!(f, "SelectWorkshop({i})"),
            Self::CloseWorkshop(i) => write!(f, "CloseWorkshop({i})"),
            Self::NewWorkshopDialog => write!(f, "NewWorkshopDialog"),
            Self::WorkshopDirPicked(p) => write!(f, "WorkshopDirPicked({p:?})"),
            Self::WorkshopInitFailed { id, error } => {
                write!(f, "WorkshopInitFailed({id}, {error})")
            }
            Self::WorkshopReady { id, .. } => write!(f, "WorkshopReady({id})"),
            Self::SparksLoaded(id, s) => write!(f, "SparksLoaded({id}, {} sparks)", s.len()),
            Self::FailingContractsLoaded(id, n) => {
                write!(f, "FailingContractsLoaded({id}, {n})")
            }
            Self::ContractsLoaded(id, sid, c) => {
                write!(f, "ContractsLoaded({id}, {sid}, {} contracts)", c.len())
            }
            Self::BondsLoaded(id, sid, b) => {
                write!(f, "BondsLoaded({id}, {sid}, {} bonds)", b.len())
            }
            Self::BlockedSparkIdsLoaded(id, ids) => {
                write!(f, "BlockedSparkIdsLoaded({id}, {} ids)", ids.len())
            }
            Self::ContractCheckFinished { ws_id, spark_id } => {
                write!(f, "ContractCheckFinished({ws_id}, {spark_id})")
            }
            Self::AgentSessionsLoaded(id, s) => {
                write!(f, "AgentSessionsLoaded({id}, {} sessions)", s.len())
            }
            Self::AgentSessionSaved => write!(f, "AgentSessionSaved"),
            Self::OpenTabsLoaded(id, t) => {
                write!(f, "OpenTabsLoaded({id}, {} tabs)", t.len())
            }
            Self::OpenTabsSaved => write!(f, "OpenTabsSaved"),
            Self::FilesScanned(id, _) => write!(f, "FilesScanned({id})"),
            Self::FileExplorer(m) => write!(f, "FileExplorer({m:?})"),
            Self::FileViewer(m) => write!(f, "FileViewer({m:?})"),
            Self::Agents(m) => write!(f, "Agents({m:?})"),
            Self::Bench(m) => write!(f, "Bench({m:?})"),
            Self::Sparks(m) => write!(f, "Sparks({m:?})"),
            Self::SparkDetail(m) => write!(f, "SparkDetail({m:?})"),
            Self::SparkPicker(m) => write!(f, "SparkPicker({m:?})"),
            Self::HeadPicker(m) => write!(f, "HeadPicker({m:?})"),
            Self::Background(m) => write!(f, "Background({m:?})"),
            Self::StatusBar(m) => write!(f, "StatusBar({m:?})"),
            Self::BackgroundLoaded(id, _) => write!(f, "BackgroundLoaded({id})"),
            Self::UnsplashDownloaded { filename, .. } => {
                write!(f, "UnsplashDownloaded({filename})")
            }
            Self::UnsplashSearchResult(r) => {
                write!(f, "UnsplashSearchResult(ok={})", r.is_ok())
            }
            Self::UnsplashDownloadFailed(e) => write!(f, "UnsplashDownloadFailed({e})"),
            Self::LocalFileCopied(name) => write!(f, "LocalFileCopied({name})"),
            Self::BackgroundConfigSaved => write!(f, "BackgroundConfigSaved"),
            Self::AgentContextSynced => write!(f, "AgentContextSynced"),
            Self::SparksPoll => write!(f, "SparksPoll"),
            Self::NewDefaultHand => write!(f, "NewDefaultHand"),
            Self::HandAssignmentSaved => write!(f, "HandAssignmentSaved"),
            Self::ShiftStateChanged(held) => write!(f, "ShiftStateChanged({held})"),
            Self::SendSparkPrompt { tab_id, .. } => write!(f, "SendSparkPrompt({tab_id})"),
            Self::SubmitSparkPrompt { tab_id } => write!(f, "SubmitSparkPrompt({tab_id})"),
            Self::SplitterPressed(k) => write!(f, "SplitterPressed({k:?})"),
            Self::SplitterMoved(p) => write!(f, "SplitterMoved({:.0},{:.0})", p.x, p.y),
            Self::SplitterReleased => write!(f, "SplitterReleased"),
            Self::LayoutSaved => write!(f, "LayoutSaved"),
            Self::WindowResized(s) => write!(f, "WindowResized({:.0}x{:.0})", s.width, s.height),
            Self::Toast(m) => write!(f, "Toast({m:?})"),
            Self::ShowToast { title, kind, .. } => write!(f, "ShowToast({title}, {kind:?})"),
            Self::ToastExpired(id) => write!(f, "ToastExpired({id})"),
        }
    }
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let global_config = data::config::Config::load();
        let available_agents = coding_agents::detect_available();
        let appearance = Appearance::detect();

        let mut app = Self {
            appearance,
            global_config,
            available_agents,
            workshops: Vec::new(),
            active_workshop: None,
            next_terminal_id: 1,
            poll_in_flight: false,
            shift_held: false,
            splitter_drag: None,
            window_size: Size::new(1400.0, 900.0),
            toasts: Vec::new(),
            next_toast_id: 1,
        };

        // Surface an upgrade toast for any detected CLI whose version is
        // outside Ryve's known-good range. Spark ryve-133ebb9b: catching
        // this at boot — instead of when a Hand spawn fails cryptically —
        // is the whole point of the version probe.
        let unsupported: Vec<(String, String)> = app
            .available_agents
            .iter()
            .filter_map(|a| match &a.compatibility {
                coding_agents::CompatStatus::Unsupported { reason, .. } => {
                    Some((a.display_name.clone(), reason.clone()))
                }
                _ => None,
            })
            .collect();
        let mut tasks: Vec<Task<Message>> = Vec::new();
        for (name, reason) in unsupported {
            tasks.push(app.push_toast(
                format!("Upgrade {name} CLI"),
                reason,
                ToastKind::Warning,
            ));
        }

        (app, Task::batch(tasks))
    }

    fn active_workshop(&self) -> Option<&Workshop> {
        self.active_workshop.and_then(|i| self.workshops.get(i))
    }

    /// Push a new toast onto the stack and return a `Task` that will
    /// emit `ToastExpired` after the toast's lifetime.
    /// Persist the open-tabs snapshot for `workshop_idx`. Returns a Task
    /// that writes the new snapshot to the database; returns `Task::none()`
    /// if the workshop has no DB pool yet (e.g., during init).
    ///
    /// This is invoked on every tab create/close so the database stays in
    /// sync with the bench. Coding-agent tabs are filtered out by
    /// `Workshop::snapshot_open_tabs`.
    fn persist_open_tabs(&self, workshop_idx: usize) -> Task<Message> {
        let Some(ws) = self.workshops.get(workshop_idx) else {
            return Task::none();
        };
        let Some(pool) = ws.sparks_db.clone() else {
            return Task::none();
        };
        let workshop_id = ws.workshop_id();
        let snapshot = ws.snapshot_open_tabs();
        Task::perform(
            async move {
                if let Err(e) = data::sparks::open_tab_repo::save_snapshot(
                    &pool,
                    &workshop_id,
                    &snapshot,
                )
                .await
                {
                    log::warn!("Failed to persist open tabs for {workshop_id}: {e}");
                }
            },
            |_| Message::OpenTabsSaved,
        )
    }

    fn push_toast(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        kind: ToastKind,
    ) -> Task<Message> {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        let title = title.into();
        let body = body.into();
        // Also log to console so failures remain greppable in release logs.
        match kind {
            ToastKind::Error => log::error!("toast: {title}: {body}"),
            ToastKind::Warning => log::warn!("toast: {title}: {body}"),
            ToastKind::Info => log::info!("toast: {title}: {body}"),
        }
        self.toasts.push(Toast {
            id,
            title,
            body,
            kind,
        });
        // Drop oldest when over the cap.
        while self.toasts.len() > toast::MAX_TOASTS {
            self.toasts.remove(0);
        }
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(toast::TOAST_LIFETIME_SECS))
                    .await;
                id
            },
            Message::ToastExpired,
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // -- Workshop tab bar --
            Message::SelectWorkshop(idx) => {
                if idx < self.workshops.len() {
                    self.active_workshop = Some(idx);
                }
                Task::none()
            }
            Message::CloseWorkshop(idx) => {
                if idx < self.workshops.len() {
                    self.workshops.remove(idx);
                    // Adjust active index
                    if self.workshops.is_empty() {
                        self.active_workshop = None;
                    } else if let Some(active) = self.active_workshop {
                        if active > idx {
                            self.active_workshop = Some(active - 1);
                        } else if active == idx {
                            self.active_workshop = if self.workshops.is_empty() {
                                None
                            } else {
                                Some(idx.min(self.workshops.len() - 1))
                            };
                        }
                    }
                }
                Task::none()
            }
            Message::NewWorkshopDialog => Task::perform(pick_workshop_directory(), |path| {
                Message::WorkshopDirPicked(path)
            }),
            Message::WorkshopDirPicked(Some(path)) => {
                let workshop = Workshop::new(path.clone());
                let ws_id = workshop.id;
                self.workshops.push(workshop);
                let idx = self.workshops.len() - 1;
                self.active_workshop = Some(idx);

                // Async: init .ryve/ dir, DB, config, agents, context
                Task::perform(workshop::init_workshop(path), move |result| match result {
                    Ok(init) => Message::WorkshopReady {
                        id: ws_id,
                        pool: init.pool,
                        config: init.config,
                        custom_agents: init.custom_agents,
                        agent_context: init.agent_context,
                    },
                    Err(e) => Message::WorkshopInitFailed {
                        id: ws_id,
                        error: e.to_string(),
                    },
                })
            }
            Message::WorkshopDirPicked(None) => Task::none(),
            Message::WorkshopInitFailed { id, error } => {
                // Remove the half-initialized workshop so we don't leave a
                // ghost tab pointing at a broken directory.
                if let Some(pos) = self.workshops.iter().position(|ws| ws.id == id) {
                    self.workshops.remove(pos);
                    if self.workshops.is_empty() {
                        self.active_workshop = None;
                    } else if let Some(active) = self.active_workshop {
                        if active >= pos && active > 0 {
                            self.active_workshop = Some(active - 1);
                        } else if self.workshops.is_empty() {
                            self.active_workshop = None;
                        }
                    }
                }
                self.push_toast(
                    "Workshop failed to open",
                    format!("Database or config init error: {error}"),
                    ToastKind::Error,
                )
            }

            Message::WorkshopReady {
                id,
                pool,
                config,
                custom_agents,
                agent_context,
            } => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    ws.sparks_db = Some(pool.clone());
                    ws.config = config;
                    ws.custom_agents = custom_agents;
                    ws.agent_context = agent_context;

                    // Load sparks + agent sessions + scan file tree in parallel
                    let ws_id = ws.workshop_id();
                    let dir = ws.directory.clone();
                    let pool2 = pool.clone();
                    let ws_id2 = ws_id.clone();
                    let sparks_task = Task::perform(load_sparks(pool, ws_id), move |sparks| {
                        Message::SparksLoaded(id, sparks)
                    });
                    let pool3 = ws.sparks_db.clone().unwrap();
                    let ws_id3 = ws.workshop_id();
                    let sessions_task =
                        Task::perform(load_agent_sessions(pool2, ws_id2), move |sessions| {
                            Message::AgentSessionsLoaded(id, sessions)
                        });
                    let open_tabs_task =
                        Task::perform(load_open_tabs(pool3, ws_id3), move |tabs| {
                            Message::OpenTabsLoaded(id, tabs)
                        });
                    let ignore = ws.config.explorer.ignore.clone();
                    let scan_task = Task::perform(
                        file_explorer::scan_directory(dir, ignore),
                        move |(tree, statuses, diff_stats, branch)| {
                            Message::FilesScanned(
                                id,
                                file_explorer::Message::TreeLoaded(
                                    tree, statuses, diff_stats, branch,
                                ),
                            )
                        },
                    );
                    // Optionally load background image
                    let bg_task = if let Some(ref filename) = ws.config.background.image {
                        let path = ws.ryve_dir.backgrounds_dir().join(filename);
                        Task::perform(
                            async move { tokio::fs::read(&path).await.ok() },
                            move |bytes| Message::BackgroundLoaded(id, bytes),
                        )
                    } else {
                        Task::none()
                    };

                    return Task::batch([
                        sparks_task,
                        sessions_task,
                        open_tabs_task,
                        scan_task,
                        bg_task,
                    ]);
                }
                Task::none()
            }
            Message::SparksLoaded(id, sparks) => {
                self.poll_in_flight = false;
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    ws.sparks = sparks;

                    // Refresh failing contract count + blocked-spark set
                    // alongside sparks so the status bar warning indicator
                    // and the per-row blocked indicator stay in sync.
                    let mut tasks: Vec<Task<Message>> = Vec::new();
                    if let Some(ref pool) = ws.sparks_db {
                        let pool_c = pool.clone();
                        let pool_b = pool.clone();
                        let ws_id_c = ws.workshop_id();
                        let ws_id_b = ws.workshop_id();
                        tasks.push(Task::perform(
                            load_failing_contract_count(pool_c, ws_id_c),
                            move |n| Message::FailingContractsLoaded(id, n),
                        ));
                        tasks.push(Task::perform(
                            load_blocked_spark_ids(pool_b, ws_id_b),
                            move |ids| Message::BlockedSparkIdsLoaded(id, ids),
                        ));
                    }

                    // Sync .ryve/WORKSHOP.md and pointers (including into worktrees)
                    if !ws.config.agents.disable_sync {
                        let dir = ws.directory.clone();
                        let ryve_dir = ws.ryve_dir.clone();
                        let config = ws.config.clone();
                        tasks.push(Task::perform(
                            async move {
                                let _ = data::agent_context::sync(&dir, &ryve_dir, &config).await;
                            },
                            |_| Message::AgentContextSynced,
                        ));
                    }

                    return Task::batch(tasks);
                }
                Task::none()
            }
            Message::FailingContractsLoaded(id, count) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    ws.failing_contracts = count;
                }
                Task::none()
            }
            Message::ContractsLoaded(id, spark_id, contracts) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    // Only apply if this spark is still selected — avoids
                    // racing a stale load against a newer selection.
                    if ws.selected_spark.as_deref() == Some(spark_id.as_str()) {
                        ws.selected_spark_contracts = contracts;
                    }
                }
                Task::none()
            }
            Message::BondsLoaded(id, spark_id, bonds) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    if ws.selected_spark.as_deref() == Some(spark_id.as_str()) {
                        ws.selected_spark_bonds = bonds;
                    }
                }
                Task::none()
            }
            Message::BlockedSparkIdsLoaded(id, ids) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    ws.blocked_spark_ids = ids;
                }
                Task::none()
            }
            Message::ContractCheckFinished { ws_id, spark_id } => {
                // Reload contracts for the spark and refresh the failing badge.
                let Some(ws) = self.workshops.iter().find(|ws| ws.id == ws_id) else {
                    return Task::none();
                };
                let Some(ref pool) = ws.sparks_db else {
                    return Task::none();
                };
                let pool = pool.clone();
                let workshop_id = ws.workshop_id();
                let id = ws.id;
                let pool2 = pool.clone();
                let workshop_id2 = workshop_id.clone();
                let load_task =
                    Task::perform(load_contracts(pool, spark_id.clone()), move |list| {
                        Message::ContractsLoaded(id, spark_id.clone(), list)
                    });
                let count_task =
                    Task::perform(load_failing_contract_count(pool2, workshop_id2), move |n| {
                        Message::FailingContractsLoaded(id, n)
                    });
                Task::batch([load_task, count_task])
            }

            Message::AgentSessionsLoaded(id, persisted) => {
                // Merge persisted sessions into the in-memory vec.
                //
                // This handler is fired both at workshop init and on every
                // SparksPoll tick (so CLI-spawned Hands — which write to the
                // `agent_sessions` table directly via `ryve hand spawn` —
                // appear in the Hands panel without requiring the workshop
                // to be reopened).
                //
                // Sessions already known in memory keep their `tab_id` so we
                // don't clobber a live UI tab. Persisted rows are then
                // reclassified as active/history/stale from DB end-state,
                // live UI terminal presence, and detached child PID liveness.
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    let available = &self.available_agents;
                    let known_ids: std::collections::HashSet<String> =
                        ws.agent_sessions.iter().map(|s| s.id.clone()).collect();

                    for p in persisted {
                        let existing_tab_id = ws
                            .agent_sessions
                            .iter()
                            .find(|s| s.id == p.id)
                            .and_then(|s| s.tab_id);
                        let display_state = screen::agents::classify_session(
                            p.ended_at.is_some(),
                            existing_tab_id.is_some(),
                            p.child_pid.is_some_and(process_is_alive),
                        );

                        if known_ids.contains(&p.id) {
                            // Already in memory — preserve tab_id, but refresh liveness.
                            if let Some(s) = ws.agent_sessions.iter_mut().find(|s| s.id == p.id) {
                                s.active =
                                    display_state == screen::agents::SessionDisplayState::Active;
                                s.stale =
                                    display_state == screen::agents::SessionDisplayState::Stale;
                            }
                            continue;
                        }
                        let agent = available
                            .iter()
                            .find(|a| a.command == p.agent_command)
                            .cloned()
                            .unwrap_or_else(|| CodingAgent {
                                display_name: p.agent_name.clone(),
                                command: p.agent_command.clone(),
                                args: serde_json::from_str(&p.agent_args).unwrap_or_default(),
                                resume: coding_agents::ResumeStrategy::None,
                                compatibility: coding_agents::CompatStatus::Unknown,
                            });
                        ws.agent_sessions.push(AgentSession {
                            id: p.id,
                            name: p.agent_name,
                            agent,
                            tab_id: existing_tab_id,
                            active: display_state == screen::agents::SessionDisplayState::Active,
                            stale: display_state == screen::agents::SessionDisplayState::Stale,
                            resume_id: p.resume_id,
                            started_at: p.started_at,
                        });
                    }
                }
                Task::none()
            }

            Message::AgentSessionSaved => Task::none(),

            Message::OpenTabsLoaded(id, persisted) => {
                // Replay the persisted snapshot against the bench. Each
                // entry becomes a fresh tab — terminals re-spawn an empty
                // shell, file viewers re-open their path. Coding-agent
                // tabs aren't persisted, so we never recreate one here.
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };

                let mut follow_up: Vec<Task<Message>> = Vec::new();
                for tab in persisted {
                    match tab.tab_kind.as_str() {
                        "terminal" => {
                            let next_id = &mut self.next_terminal_id;
                            self.workshops[idx].spawn_terminal(
                                tab.title,
                                None,
                                next_id,
                                None,
                                false,
                            );
                        }
                        "file_viewer" => {
                            let Some(payload) = tab.payload else { continue };
                            let path = std::path::PathBuf::from(payload);
                            // Skip files that no longer exist on disk so a
                            // restored snapshot from a stale workshop doesn't
                            // pop a wall of failure toasts.
                            if !path.exists() {
                                continue;
                            }
                            let ws = &mut self.workshops[idx];
                            let (tab_id, is_new) =
                                ws.open_file_tab(path.clone(), &mut self.next_terminal_id);
                            if is_new {
                                let repo_root = ws.directory.clone();
                                let pool = ws.sparks_db.clone();
                                let ws_id = ws.workshop_id();
                                follow_up.push(Task::perform(
                                    file_viewer::load_file(
                                        tab_id,
                                        path,
                                        repo_root,
                                        pool,
                                        ws_id,
                                        self.appearance == style::Appearance::Light,
                                    ),
                                    Message::FileViewer,
                                ));
                            }
                        }
                        other => {
                            log::warn!("Unknown persisted tab kind: {other}");
                        }
                    }
                }

                if follow_up.is_empty() {
                    Task::none()
                } else {
                    Task::batch(follow_up)
                }
            }
            Message::OpenTabsSaved => Task::none(),

            Message::FilesScanned(id, msg) => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    if let file_explorer::Message::TreeLoaded(tree, statuses, diff_stats, branch) =
                        msg
                    {
                        ws.file_explorer.tree = tree;
                        ws.file_explorer.git_statuses = statuses;
                        ws.file_explorer.diff_stats = diff_stats;
                        ws.file_explorer.branch = branch;
                        // Start collapsed — user expands directories on demand
                    }
                }
                Task::none()
            }

            // -- Forward to active workshop --
            Message::FileExplorer(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                match msg {
                    file_explorer::Message::SelectFile(ref path) => {
                        ws.file_explorer.selected = Some(path.clone());

                        // Open (or switch to) a file viewer tab
                        let file_path = path.clone();
                        let (tab_id, is_new) =
                            ws.open_file_tab(file_path.clone(), &mut self.next_terminal_id);

                        if is_new {
                            let repo_root = ws.directory.clone();
                            let pool = ws.sparks_db.clone();
                            let ws_id = ws.workshop_id();
                            let load = Task::perform(
                                file_viewer::load_file(
                                    tab_id,
                                    file_path,
                                    repo_root,
                                    pool,
                                    ws_id,
                                    self.appearance == style::Appearance::Light,
                                ),
                                Message::FileViewer,
                            );
                            let persist = self.persist_open_tabs(idx);
                            return Task::batch([load, persist]);
                        }
                    }
                    file_explorer::Message::ToggleDirectory(ref path) => {
                        if ws.file_explorer.expanded.contains(path) {
                            ws.file_explorer.expanded.remove(path);
                        } else {
                            ws.file_explorer.expanded.insert(path.clone());
                        }
                    }
                    file_explorer::Message::Refresh => {
                        let dir = ws.directory.clone();
                        let ignore = ws.config.explorer.ignore.clone();
                        let ws_id = ws.id;
                        return Task::perform(
                            file_explorer::scan_directory(dir, ignore),
                            move |(tree, statuses, diff_stats, branch)| {
                                Message::FilesScanned(
                                    ws_id,
                                    file_explorer::Message::TreeLoaded(
                                        tree, statuses, diff_stats, branch,
                                    ),
                                )
                            },
                        );
                    }
                    file_explorer::Message::TreeLoaded(..) => {
                        // Handled via FilesScanned
                    }
                    file_explorer::Message::LinkSpark(ref path) => {
                        // If we have sparks and a DB, link the file to the first open spark
                        // (In the future this should open a spark picker dialog)
                        if let Some(ref pool) = ws.sparks_db {
                            if let Some(spark) = ws.sparks.first() {
                                let pool = pool.clone();
                                let ws_id = ws.workshop_id();
                                let rel_path = path
                                    .strip_prefix(&ws.directory)
                                    .unwrap_or(path)
                                    .to_string_lossy()
                                    .to_string();
                                let spark_id = spark.id.clone();
                                return Task::perform(
                                    async move {
                                        let link = data::sparks::types::NewSparkFileLink {
                                            spark_id,
                                            file_path: rel_path,
                                            line_start: None,
                                            line_end: None,
                                            workshop_id: ws_id.clone(),
                                        };
                                        let _ = data::sparks::file_link_repo::create(&pool, &link)
                                            .await;
                                    },
                                    |_| Message::Sparks(screen::sparks::Message::Refresh),
                                );
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::FileViewer(msg) => {
                match msg {
                    file_viewer::Message::FileLoaded {
                        tab_id,
                        content,
                        lines,
                        line_changes,
                        spark_links,
                    } => {
                        // Find which workshop owns this tab
                        for ws in &mut self.workshops {
                            if let Some(viewer) = ws.file_viewers.get_mut(&tab_id) {
                                viewer.set_content(content, lines, line_changes, spark_links);
                                break;
                            }
                        }
                    }
                    file_viewer::Message::GoToSpark(_spark_id) => {
                        // TODO: navigate to spark detail / select spark in panel
                    }
                    file_viewer::Message::Scrolled {
                        offset_y,
                        viewport_height,
                    } => {
                        if let Some(idx) = self.active_workshop {
                            let ws = &mut self.workshops[idx];
                            if let Some(active_id) = ws.bench.active_tab {
                                if let Some(viewer) = ws.file_viewers.get_mut(&active_id) {
                                    viewer.scroll_offset = offset_y;
                                    viewer.viewport_height = viewport_height;
                                }
                            }
                        }
                    }
                    file_viewer::Message::ClickLine(idx) => {
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &mut self.workshops[ws_idx];
                            if let Some(active_id) = ws.bench.active_tab {
                                if let Some(viewer) = ws.file_viewers.get_mut(&active_id) {
                                    if self.shift_held {
                                        viewer.selection_end = Some(idx);
                                    } else {
                                        viewer.selection_anchor = Some(idx);
                                        viewer.selection_end = Some(idx);
                                    }
                                }
                            }
                        }
                    }
                    file_viewer::Message::CopySelection => {
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &self.workshops[ws_idx];
                            if let Some(active_id) = ws.bench.active_tab {
                                if let Some(viewer) = ws.file_viewers.get(&active_id) {
                                    if let Some(selected) = viewer.selected_text() {
                                        if let Ok(mut clip) = arboard::Clipboard::new() {
                                            let _ = clip.set_text(selected);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    file_viewer::Message::ClearSelection => {
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &mut self.workshops[ws_idx];
                            if let Some(active_id) = ws.bench.active_tab {
                                if let Some(viewer) = ws.file_viewers.get_mut(&active_id) {
                                    viewer.clear_selection();
                                }
                            }
                        }
                    }
                    file_viewer::Message::FileLoadFailed {
                        tab_id,
                        path,
                        error,
                    } => {
                        // Close the empty viewer tab since there's nothing to show,
                        // then toast the failure so it doesn't vanish.
                        let mut closed_in: Option<usize> = None;
                        for (idx, ws) in self.workshops.iter_mut().enumerate() {
                            if ws.file_viewers.remove(&tab_id).is_some() {
                                ws.bench.close_tab(tab_id);
                                closed_in = Some(idx);
                                break;
                            }
                        }
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.to_string_lossy().into_owned());
                        let toast = self.push_toast(
                            format!("Failed to open {name}"),
                            error,
                            ToastKind::Error,
                        );
                        if let Some(idx) = closed_in {
                            return Task::batch([toast, self.persist_open_tabs(idx)]);
                        }
                        return toast;
                    }
                }
                Task::none()
            }
            Message::Agents(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                match msg {
                    screen::agents::Message::SelectAgent(session_id) => {
                        // Decide what clicking the row should do based on
                        // session state. We compute an Outcome first so that
                        // the mutable borrow of `ws` ends before we call
                        // `self.push_toast` (which needs `&mut self`).
                        enum Outcome {
                            Focused,
                            Stale { name: String },
                            Past { name: String, started_at: String, can_resume: bool },
                            NotFound,
                        }

                        let outcome = match ws
                            .agent_sessions
                            .iter()
                            .find(|s| s.id == session_id)
                            .cloned()
                        {
                            None => Outcome::NotFound,
                            Some(session) if session.active => match session.tab_id {
                                Some(tab_id)
                                    if ws.bench.tabs.iter().any(|t| t.id == tab_id) =>
                                {
                                    ws.bench.active_tab = Some(tab_id);
                                    Outcome::Focused
                                }
                                _ => Outcome::Stale { name: session.name },
                            },
                            Some(session) => {
                                let can_resume = session.can_resume();
                                Outcome::Past {
                                    name: session.name,
                                    started_at: session.started_at,
                                    can_resume,
                                }
                            }
                        };

                        match outcome {
                            Outcome::Focused | Outcome::NotFound => {}
                            Outcome::Stale { name } => {
                                return self.push_toast(
                                    format!("{name} is no longer running"),
                                    "Its terminal tab has closed. Use the resume button to restart it.",
                                    ToastKind::Warning,
                                );
                            }
                            Outcome::Past { name, started_at, can_resume } => {
                                let when = screen::agents::format_relative_time(&started_at);
                                let body = if can_resume {
                                    format!("Past session started {when}. Click \u{25B6} to resume.")
                                } else {
                                    format!("Past session started {when}. Cannot be resumed.")
                                };
                                return self.push_toast(name, body, ToastKind::Info);
                            }
                        }
                    }
                    screen::agents::Message::ResumeAgent(session_id) => {
                        // Find the session and resume it
                        let session = ws
                            .agent_sessions
                            .iter()
                            .find(|s| s.id == session_id)
                            .cloned();
                        if let Some(session) = session {
                            if let Some((cmd, args)) =
                                session.agent.resume_args(session.resume_id.as_deref())
                            {
                                let resume_agent = CodingAgent {
                                    display_name: session.agent.display_name.clone(),
                                    command: cmd.clone(),
                                    args: args.clone(),
                                    resume: session.agent.resume.clone(),
                                    compatibility: session.agent.compatibility.clone(),
                                };
                                let next_id = &mut self.next_terminal_id;
                                let full_auto = self
                                    .global_config
                                    .agent_settings
                                    .get(&resume_agent.command)
                                    .map_or(false, |s| s.full_auto);
                                let tab_id = ws.spawn_terminal(
                                    session.name.clone(),
                                    Some(&resume_agent),
                                    next_id,
                                    Some(&session_id),
                                    full_auto,
                                );

                                // Update the existing session to active
                                if let Some(s) =
                                    ws.agent_sessions.iter_mut().find(|s| s.id == session_id)
                                {
                                    s.tab_id = Some(tab_id);
                                    s.active = true;
                                    s.stale = false;
                                }

                                // Mark as active in DB
                                if let Some(ref pool) = ws.sparks_db {
                                    let pool = pool.clone();
                                    let sid = session_id.clone();
                                    return Task::perform(
                                        async move {
                                            let _ = data::sparks::agent_session_repo::reactivate(
                                                &pool, &sid,
                                            )
                                            .await;
                                        },
                                        |_| Message::AgentSessionSaved,
                                    );
                                }
                            }
                        }
                    }
                    screen::agents::Message::DeleteSession(session_id) => {
                        ws.agent_sessions.retain(|s| s.id != session_id);
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let sid = session_id.clone();
                            return Task::perform(
                                async move {
                                    let _ =
                                        data::sparks::agent_session_repo::delete(&pool, &sid).await;
                                },
                                |_| Message::AgentSessionSaved,
                            );
                        }
                    }
                }
                Task::none()
            }
            Message::SparkDetail(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                match msg {
                    screen::spark_detail::Message::Back => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.selected_spark = None;
                            ws.selected_spark_contracts.clear();
                            ws.selected_spark_bonds.clear();
                            ws.contract_create_form.reset();
                        }
                    }
                    screen::spark_detail::Message::ShowCreateContract => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.contract_create_form.visible = true;
                        }
                    }
                    screen::spark_detail::Message::CancelCreateContract => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.contract_create_form.reset();
                        }
                    }
                    screen::spark_detail::Message::CycleContractKind => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.contract_create_form.kind = screen::spark_detail::next_contract_kind(
                                ws.contract_create_form.kind,
                            );
                        }
                    }
                    screen::spark_detail::Message::ToggleContractEnforcement => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.contract_create_form.enforcement =
                                screen::spark_detail::toggle_enforcement(
                                    ws.contract_create_form.enforcement,
                                );
                        }
                    }
                    screen::spark_detail::Message::ContractDescriptionChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.contract_create_form.description = val;
                        }
                    }
                    screen::spark_detail::Message::ContractCheckCommandChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.contract_create_form.check_command = val;
                        }
                    }
                    screen::spark_detail::Message::SubmitContract(spark_id) => {
                        let ws = &mut self.workshops[idx];
                        let form = ws.contract_create_form.clone();
                        if form.description.trim().is_empty() {
                            return Task::none();
                        }
                        let cmd = form.check_command.trim().to_string();
                        let check_command = if cmd.is_empty() { None } else { Some(cmd) };
                        ws.contract_create_form.reset();
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let ws_id = ws.id;
                            let workshop_id = ws.workshop_id();
                            let new_contract = data::sparks::types::NewContract {
                                spark_id: spark_id.clone(),
                                kind: form.kind,
                                description: form.description.trim().to_string(),
                                check_command,
                                pattern: None,
                                file_glob: None,
                                enforcement: form.enforcement,
                            };
                            let load_pool = pool.clone();
                            let count_pool = pool.clone();
                            let count_ws_id = workshop_id.clone();
                            let sid = spark_id.clone();
                            let create_task = Task::perform(
                                async move {
                                    let _ =
                                        data::sparks::contract_repo::create(&pool, new_contract)
                                            .await;
                                    data::sparks::contract_repo::list_for_spark(&load_pool, &sid)
                                        .await
                                        .unwrap_or_default()
                                },
                                move |list| Message::ContractsLoaded(ws_id, spark_id.clone(), list),
                            );
                            let count_task = Task::perform(
                                load_failing_contract_count(count_pool, count_ws_id),
                                move |n| Message::FailingContractsLoaded(ws_id, n),
                            );
                            return Task::batch([create_task, count_task]);
                        }
                    }
                    screen::spark_detail::Message::DeleteContract {
                        spark_id,
                        contract_id,
                    } => {
                        let ws = &self.workshops[idx];
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let ws_id = ws.id;
                            return Task::perform(
                                async move {
                                    let _ = data::sparks::contract_repo::delete(&pool, contract_id)
                                        .await;
                                },
                                move |_| Message::ContractCheckFinished {
                                    ws_id,
                                    spark_id: spark_id.clone(),
                                },
                            );
                        }
                    }
                    screen::spark_detail::Message::RunContract {
                        spark_id,
                        contract_id,
                    } => {
                        let ws = &self.workshops[idx];
                        let Some(contract) = ws
                            .selected_spark_contracts
                            .iter()
                            .find(|c| c.id == contract_id)
                            .cloned()
                        else {
                            return Task::none();
                        };
                        let Some(cmd) = contract
                            .check_command
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                        else {
                            return Task::none();
                        };
                        let Some(ref pool) = ws.sparks_db else {
                            return Task::none();
                        };
                        let pool = pool.clone();
                        let ws_id = ws.id;
                        let cwd = ws.directory.clone();
                        return Task::perform(
                            async move {
                                let status = run_contract_check(&cmd, &cwd).await;
                                let _ = data::sparks::contract_repo::update_status(
                                    &pool,
                                    contract_id,
                                    status,
                                    "ui",
                                )
                                .await;
                            },
                            move |_| Message::ContractCheckFinished {
                                ws_id,
                                spark_id: spark_id.clone(),
                            },
                        );
                    }
                    screen::spark_detail::Message::CycleStatus(spark_id, new_status) => {
                        if let Some(ws) = self.workshops.get(idx) {
                            if let Some(ref pool) = ws.sparks_db {
                                let pool = pool.clone();
                                let ws_id = ws.workshop_id();
                                let id = ws.id;
                                return Task::perform(
                                    async move {
                                        if new_status == "closed" {
                                            let _ = data::sparks::spark_repo::close(
                                                &pool,
                                                &spark_id,
                                                "completed",
                                                "user",
                                            )
                                            .await;
                                        } else {
                                            let status = data::sparks::types::SparkStatus::from_str(
                                                &new_status,
                                            );
                                            if let Some(s) = status {
                                                let upd = data::sparks::types::UpdateSpark {
                                                    status: Some(s),
                                                    ..Default::default()
                                                };
                                                let _ = data::sparks::spark_repo::update(
                                                    &pool, &spark_id, upd, "user",
                                                )
                                                .await;
                                            }
                                        }
                                        load_sparks(pool, ws_id).await
                                    },
                                    move |sparks| Message::SparksLoaded(id, sparks),
                                );
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::SparkPicker(msg) => self.handle_spark_picker_message(msg),
            Message::HeadPicker(msg) => self.handle_head_picker_message(msg),
            Message::HandAssignmentSaved => Task::none(),
            Message::SendSparkPrompt { tab_id, prompt } => {
                // Find the terminal across all workshops and send the prompt as input.
                // Wrap in bracketed paste so TUI agents see it as a single paste.
                // Enter is sent separately after a delay (some agents need time
                // to finish processing the paste before accepting the submit key).
                for ws in &mut self.workshops {
                    if let Some(term) = ws.terminals.get_mut(&tab_id) {
                        let mut bytes = Vec::with_capacity(prompt.len() + 16);
                        bytes.extend_from_slice(b"\x1b[200~");
                        bytes.extend_from_slice(prompt.as_bytes());
                        bytes.extend_from_slice(b"\x1b[201~");
                        term.handle(iced_term::Command::ProxyToBackend(
                            iced_term::BackendCommand::Write(bytes),
                        ));
                        break;
                    }
                }
                // Schedule the Enter key to submit the paste
                Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    },
                    move |_| Message::SubmitSparkPrompt { tab_id },
                )
            }
            Message::SubmitSparkPrompt { tab_id } => {
                for ws in &mut self.workshops {
                    if let Some(term) = ws.terminals.get_mut(&tab_id) {
                        term.handle(iced_term::Command::ProxyToBackend(
                            iced_term::BackendCommand::Write(vec![b'\r']),
                        ));
                        break;
                    }
                }
                Task::none()
            }
            Message::ShiftStateChanged(pressed) => {
                self.shift_held = pressed;
                Task::none()
            }
            Message::Bench(msg) => self.handle_bench_message(msg),
            Message::Sparks(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                match msg {
                    screen::sparks::Message::Refresh => {
                        if let Some(ws) = self.workshops.get(idx) {
                            if let Some(ref pool) = ws.sparks_db {
                                let pool = pool.clone();
                                let ws_id = ws.workshop_id();
                                let id = ws.id;
                                return Task::perform(load_sparks(pool, ws_id), move |sparks| {
                                    Message::SparksLoaded(id, sparks)
                                });
                            }
                        }
                    }
                    screen::sparks::Message::SelectSpark(spark_id) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.selected_spark = Some(spark_id.clone());
                            ws.selected_spark_contracts.clear();
                            ws.selected_spark_bonds.clear();
                            ws.contract_create_form.reset();
                            if let Some(ref pool) = ws.sparks_db {
                                let pool_c = pool.clone();
                                let pool_b = pool.clone();
                                let ws_id = ws.id;
                                let sid_c = spark_id.clone();
                                let sid_b = spark_id.clone();
                                let contracts_task = Task::perform(
                                    load_contracts(pool_c, sid_c.clone()),
                                    move |list| {
                                        Message::ContractsLoaded(ws_id, sid_c.clone(), list)
                                    },
                                );
                                let bonds_task = Task::perform(
                                    load_bonds(pool_b, sid_b.clone()),
                                    move |list| Message::BondsLoaded(ws_id, sid_b.clone(), list),
                                );
                                return Task::batch([contracts_task, bonds_task]);
                            }
                        }
                    }
                    screen::sparks::Message::ShowCreateForm => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.reset();
                            ws.spark_create_form.visible = true;
                        }
                    }
                    screen::sparks::Message::CreateFormTitleChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.title = val;
                            ws.spark_create_form.error = None;
                        }
                    }
                    screen::sparks::Message::CreateFormTypeChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            // Switching to epic clears any previously-picked
                            // parent so the validation rule stays consistent.
                            if val == "epic" {
                                ws.spark_create_form.parent_epic_id = None;
                            }
                            ws.spark_create_form.spark_type = val;
                            ws.spark_create_form.error = None;
                        }
                    }
                    screen::sparks::Message::CreateFormPriorityChanged(p) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.priority = p;
                        }
                    }
                    screen::sparks::Message::CreateFormProblemChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.problem = val;
                            ws.spark_create_form.error = None;
                        }
                    }
                    screen::sparks::Message::CreateFormAcceptanceChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.acceptance = val;
                            ws.spark_create_form.error = None;
                        }
                    }
                    screen::sparks::Message::CreateFormParentEpicChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.parent_epic_id = val;
                            ws.spark_create_form.error = None;
                        }
                    }
                    screen::sparks::Message::CancelCreate => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.visible = false;
                            ws.spark_create_form.reset();
                        }
                    }
                    screen::sparks::Message::SubmitNewSpark => {
                        let ws = &mut self.workshops[idx];
                        if let Err(e) = ws.spark_create_form.validate() {
                            ws.spark_create_form.error = Some(e);
                            return Task::none();
                        }

                        let title = ws.spark_create_form.title.trim().to_string();
                        let problem = ws.spark_create_form.problem.trim().to_string();
                        let acceptance = ws.spark_create_form.acceptance.trim().to_string();
                        let spark_type_str = ws.spark_create_form.spark_type.clone();
                        let priority = ws.spark_create_form.priority;
                        let parent_id = ws.spark_create_form.parent_epic_id.clone();

                        // Build the structured intent metadata block. The
                        // CLI's `spark create` writes the same shape so the
                        // two paths stay interchangeable.
                        let metadata = serde_json::json!({
                            "intent": {
                                "problem_statement": problem,
                                "invariants": Vec::<String>::new(),
                                "non_goals": Vec::<String>::new(),
                                "acceptance_criteria": vec![acceptance],
                            }
                        })
                        .to_string();

                        let spark_type = match spark_type_str.as_str() {
                            "bug" => data::sparks::types::SparkType::Bug,
                            "feature" => data::sparks::types::SparkType::Feature,
                            "epic" => data::sparks::types::SparkType::Epic,
                            "chore" => data::sparks::types::SparkType::Chore,
                            "spike" => data::sparks::types::SparkType::Spike,
                            "milestone" => data::sparks::types::SparkType::Milestone,
                            _ => data::sparks::types::SparkType::Task,
                        };

                        ws.spark_create_form.visible = false;
                        ws.spark_create_form.reset();

                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let ws_id = ws.workshop_id();
                            let id = ws.id;
                            return Task::perform(
                                async move {
                                    let new = data::sparks::types::NewSpark {
                                        title,
                                        description: String::new(),
                                        spark_type,
                                        priority,
                                        workshop_id: ws_id.clone(),
                                        assignee: None,
                                        owner: None,
                                        parent_id,
                                        due_at: None,
                                        estimated_minutes: None,
                                        metadata: Some(metadata),
                                        risk_level: None,
                                        scope_boundary: None,
                                    };
                                    let _ = data::sparks::spark_repo::create(&pool, new).await;
                                    load_sparks(pool, ws_id).await
                                },
                                move |sparks| Message::SparksLoaded(id, sparks),
                            );
                        }
                    }
                    screen::sparks::Message::CycleStatus(spark_id, new_status) => {
                        if let Some(ws) = self.workshops.get(idx) {
                            if let Some(ref pool) = ws.sparks_db {
                                let pool = pool.clone();
                                let ws_id = ws.workshop_id();
                                let id = ws.id;
                                return Task::perform(
                                    async move {
                                        if new_status == "closed" {
                                            let _ = data::sparks::spark_repo::close(
                                                &pool,
                                                &spark_id,
                                                "completed",
                                                "user",
                                            )
                                            .await;
                                        } else {
                                            let status = data::sparks::types::SparkStatus::from_str(
                                                &new_status,
                                            );
                                            if let Some(s) = status {
                                                let upd = data::sparks::types::UpdateSpark {
                                                    status: Some(s),
                                                    ..Default::default()
                                                };
                                                let _ = data::sparks::spark_repo::update(
                                                    &pool, &spark_id, upd, "user",
                                                )
                                                .await;
                                            }
                                        }
                                        load_sparks(pool, ws_id).await
                                    },
                                    move |sparks| Message::SparksLoaded(id, sparks),
                                );
                            }
                        }
                    }
                }
                Task::none()
            }

            // ── Background ───────────────────────────────
            Message::StatusBar(screen::status_bar::Message::OpenSettings) => {
                if let Some(idx) = self.active_workshop {
                    self.workshops[idx].background_picker.open = true;
                }
                Task::none()
            }
            Message::StatusBar(screen::status_bar::Message::RequestBranchSwitch) => {
                // TODO: open branch picker modal
                Task::none()
            }
            Message::Background(msg) => self.handle_background_message(msg),
            Message::BackgroundLoaded(id, Some(bytes)) => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    // Compute luminance to choose adaptive font color
                    if let Some(lum) = workshop::compute_image_luminance(&bytes) {
                        ws.bg_is_dark = Some(lum < 0.5);
                    }
                    ws.background_handle = Some(iced::widget::image::Handle::from_bytes(bytes));
                }
                Task::none()
            }
            Message::BackgroundLoaded(_, None) => Task::none(),
            Message::UnsplashDownloaded {
                filename,
                photographer,
                photographer_url,
            } => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                let ws_uuid = ws.id;
                ws.config.background.image = Some(filename.clone());
                ws.config.background.unsplash_photographer = Some(photographer);
                ws.config.background.unsplash_photographer_url = Some(photographer_url);
                ws.background_picker.open = false;
                ws.background_picker.loading = false;

                // Load the image + save config
                let bg_dir = ws.ryve_dir.backgrounds_dir();
                let path = bg_dir.join(&filename);
                let ryve_dir = ws.ryve_dir.clone();
                let config = ws.config.clone();
                Task::batch([
                    Task::perform(
                        async move { tokio::fs::read(&path).await.ok() },
                        move |bytes| Message::BackgroundLoaded(ws_uuid, bytes),
                    ),
                    Task::perform(
                        async move {
                            data::ryve_dir::save_config(&ryve_dir, &config).await.ok();
                        },
                        |_| Message::BackgroundConfigSaved,
                    ),
                ])
            }
            Message::UnsplashSearchResult(result) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                match result {
                    Ok(sr) => {
                        // Re-enter the existing SearchResults flow to populate thumbnails.
                        Task::done(Message::Background(
                            screen::background_picker::Message::SearchResults(sr.photos),
                        ))
                    }
                    Err(e) => {
                        ws.background_picker.loading = false;
                        ws.background_picker.results.clear();
                        ws.background_picker.thumbnails.clear();
                        self.push_toast("Unsplash search failed", e, ToastKind::Error)
                    }
                }
            }
            Message::UnsplashDownloadFailed(error) => {
                // Critical: clear the loading state that SelectPhoto set, so
                // the picker doesn't hang forever. This was the real bug.
                if let Some(idx) = self.active_workshop {
                    self.workshops[idx].background_picker.loading = false;
                }
                self.push_toast("Background download failed", error, ToastKind::Error)
            }
            Message::LocalFileCopied(filename) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                let ws_uuid = ws.id;
                ws.config.background.image = Some(filename.clone());
                ws.config.background.unsplash_photographer = None;
                ws.config.background.unsplash_photographer_url = None;
                ws.background_picker.open = false;

                let bg_dir = ws.ryve_dir.backgrounds_dir();
                let path = bg_dir.join(&filename);
                let ryve_dir = ws.ryve_dir.clone();
                let config = ws.config.clone();
                Task::batch([
                    Task::perform(
                        async move { tokio::fs::read(&path).await.ok() },
                        move |bytes| Message::BackgroundLoaded(ws_uuid, bytes),
                    ),
                    Task::perform(
                        async move {
                            data::ryve_dir::save_config(&ryve_dir, &config).await.ok();
                        },
                        |_| Message::BackgroundConfigSaved,
                    ),
                ])
            }
            Message::BackgroundConfigSaved => Task::none(),
            Message::AgentContextSynced => Task::none(),
            Message::SparksPoll => {
                // Opportunistically surface any worktree warnings that the
                // synchronous spawn paths accumulated since the last tick.
                let warnings: Vec<String> = self
                    .workshops
                    .iter_mut()
                    .filter_map(|ws| ws.take_worktree_warning())
                    .collect();
                let mut warning_tasks: Vec<Task<Message>> = warnings
                    .into_iter()
                    .map(|w| self.push_toast("Worktree fallback", w, ToastKind::Warning))
                    .collect();

                if self.poll_in_flight {
                    return Task::batch(warning_tasks);
                }

                let mut tasks: Vec<Task<Message>> = std::mem::take(&mut warning_tasks);

                // Auto-detect agent processes in plain terminals
                for ws in self.workshops.iter_mut() {
                    let detected = ws.detect_untracked_agents();
                    for (tab_id, agent) in detected {
                        let session_id = Uuid::new_v4().to_string();
                        let name = agent.display_name.clone();
                        log::info!("Auto-detected {name} in terminal tab {tab_id}");

                        // Update the tab kind from Terminal → CodingAgent
                        if let Some(tab) = ws.bench.tabs.iter_mut().find(|t| t.id == tab_id) {
                            tab.title = name.clone();
                            tab.kind = screen::bench::TabKind::CodingAgent(agent.clone());
                        }

                        ws.agent_sessions.push(AgentSession {
                            id: session_id.clone(),
                            name: name.clone(),
                            agent: agent.clone(),
                            tab_id: Some(tab_id),
                            active: true,
                            stale: false,
                            resume_id: None,
                            started_at: chrono::Utc::now().to_rfc3339(),
                        });

                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let ws_id = ws.workshop_id();
                            let new_session = data::sparks::types::NewAgentSession {
                                id: session_id,
                                workshop_id: ws_id,
                                agent_name: name,
                                agent_command: agent.command.clone(),
                                agent_args: agent.args.clone(),
                                session_label: Some("auto-detected".to_string()),
                                child_pid: None,
                                resume_id: None,
                            };
                            tasks.push(Task::perform(
                                async move {
                                    let _ = data::sparks::agent_session_repo::create(
                                        &pool,
                                        &new_session,
                                    )
                                    .await;
                                },
                                |_| Message::AgentSessionSaved,
                            ));
                        }
                    }
                }

                // Reload persisted agent sessions for every workshop with a
                // DB. This is what surfaces CLI-spawned Hands (`ryve hand
                // spawn`) in the GUI Hands panel — without this poll the
                // panel only ever sees what the UI itself launched.
                let session_tasks: Vec<_> = self
                    .workshops
                    .iter()
                    .filter(|ws| ws.sparks_db.is_some())
                    .map(|ws| {
                        let pool = ws.sparks_db.clone().unwrap();
                        let ws_id = ws.workshop_id();
                        let id = ws.id;
                        Task::perform(load_agent_sessions(pool, ws_id), move |sessions| {
                            Message::AgentSessionsLoaded(id, sessions)
                        })
                    })
                    .collect();
                tasks.extend(session_tasks);

                // Poll all workshops that have a sparks_db and at least one
                // agent session in memory (active or not — past CLI Hands
                // may have left sparks worth refreshing).
                let spark_tasks: Vec<_> = self
                    .workshops
                    .iter()
                    .filter(|ws| {
                        ws.sparks_db.is_some()
                            && (ws.agent_sessions.iter().any(|s| s.active)
                                || !ws.agent_sessions.is_empty())
                    })
                    .map(|ws| {
                        let pool = ws.sparks_db.clone().unwrap();
                        let ws_id = ws.workshop_id();
                        let id = ws.id;
                        Task::perform(load_sparks(pool, ws_id), move |sparks| {
                            Message::SparksLoaded(id, sparks)
                        })
                    })
                    .collect();
                tasks.extend(spark_tasks);

                if !tasks.is_empty() {
                    self.poll_in_flight = true;
                    return Task::batch(tasks);
                }
                Task::none()
            }
            Message::NewDefaultHand => {
                let Some(_idx) = self.active_workshop else {
                    return Task::none();
                };
                let Some(ref default_cmd) = self.global_config.default_agent else {
                    return Task::none();
                };
                let Some(agent) = self
                    .available_agents
                    .iter()
                    .find(|a| &a.command == default_cmd)
                    .cloned()
                else {
                    return Task::none();
                };
                // Delegate to the existing NewCodingAgent flow
                return self.handle_bench_message(screen::bench::Message::NewCodingAgent(agent));
            }

            // ── Layout splitters ─────────────────────────────
            Message::SplitterPressed(kind) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &self.workshops[idx];
                let start_value = match kind {
                    SplitterKind::SidebarRight => ws.config.layout.sidebar_width,
                    SplitterKind::SparksLeft => ws.config.layout.sparks_width,
                    SplitterKind::SidebarFilesHands => ws.config.layout.sidebar_split,
                };
                self.splitter_drag = Some(SplitterDrag::new(kind, start_value));
                Task::none()
            }
            Message::SplitterMoved(point) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let Some(drag) = self.splitter_drag.as_mut() else {
                    return Task::none();
                };
                let cursor = if drag.kind.is_horizontal_drag() {
                    point.x
                } else {
                    point.y
                };
                if drag.start_cursor.is_none() {
                    drag.start_cursor = Some(cursor);
                    return Task::none();
                }
                // Approximate sidebar height — only used for the
                // files↕hands ratio. Subtract title bar + status bar
                // + paddings so the ratio feels right under the cursor.
                let sidebar_height = (self.window_size.height - 80.0).max(1.0);
                let new_value = splitter::compute_new_value(drag, cursor, sidebar_height);
                let kind = drag.kind;
                let ws = &mut self.workshops[idx];
                match kind {
                    SplitterKind::SidebarRight => ws.config.layout.sidebar_width = new_value,
                    SplitterKind::SparksLeft => ws.config.layout.sparks_width = new_value,
                    SplitterKind::SidebarFilesHands => ws.config.layout.sidebar_split = new_value,
                }
                Task::none()
            }
            Message::SplitterReleased => {
                if self.splitter_drag.take().is_none() {
                    return Task::none();
                }
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &self.workshops[idx];
                let ryve_dir = ws.ryve_dir.clone();
                let config = ws.config.clone();
                Task::perform(
                    async move {
                        if let Err(e) = data::ryve_dir::save_config(&ryve_dir, &config).await {
                            log::warn!("Failed to save layout config: {e}");
                        }
                    },
                    |_| Message::LayoutSaved,
                )
            }
            Message::LayoutSaved => Task::none(),
            Message::WindowResized(size) => {
                self.window_size = size;
                Task::none()
            }

            // ── Toast notifications ──────────────────────
            Message::ShowToast { title, body, kind } => self.push_toast(title, body, kind),
            Message::Toast(toast::Message::Dismiss(id)) => {
                self.toasts.retain(|t| t.id != id);
                Task::none()
            }
            Message::ToastExpired(id) => {
                self.toasts.retain(|t| t.id != id);
                Task::none()
            }
        }
    }

    fn handle_bench_message(&mut self, msg: screen::bench::Message) -> Task<Message> {
        // Terminal events can come from any workshop, so we need to
        // find the right one by terminal ID for terminal events.
        if let screen::bench::Message::TerminalEvent(iced_term::Event::BackendCall(id, ref cmd)) =
            msg
        {
            // Find which workshop owns this terminal
            let ws_idx = self
                .workshops
                .iter()
                .position(|ws| ws.terminals.contains_key(&id));

            if let Some(idx) = ws_idx {
                let ws = &mut self.workshops[idx];
                let mut tab_closed = false;
                if let Some(term) = ws.terminals.get_mut(&id) {
                    let action = term.handle(iced_term::Command::ProxyToBackend(cmd.clone()));
                    let was_shutdown =
                        matches!(action, iced_term::actions::Action::Shutdown);
                    let ended_sessions = ws.handle_terminal_action(id, action);
                    if was_shutdown {
                        tab_closed = true;
                    }
                    if !ended_sessions.is_empty() {
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let mut tasks: Vec<Task<Message>> = ended_sessions
                                .into_iter()
                                .map(|sid| {
                                    let pool = pool.clone();
                                    Task::perform(
                                        async move {
                                            let _ =
                                                data::sparks::agent_session_repo::end_session(
                                                    &pool, &sid,
                                                )
                                                .await;
                                        },
                                        |_| Message::AgentSessionSaved,
                                    )
                                })
                                .collect();
                            if tab_closed {
                                tasks.push(self.persist_open_tabs(idx));
                            }
                            return Task::batch(tasks);
                        }
                    }
                }
                if tab_closed {
                    return self.persist_open_tabs(idx);
                }
            }
            return Task::none();
        }

        // All other bench messages go to the active workshop
        let Some(idx) = self.active_workshop else {
            return Task::none();
        };

        match msg {
            screen::bench::Message::SelectTab(id) => {
                let ws = &mut self.workshops[idx];
                let prev_tab = ws.bench.active_tab;
                ws.bench.active_tab = Some(id);

                // Evict the previously-focused file viewer to free memory
                if let Some(prev_id) = prev_tab {
                    if prev_id != id {
                        if let Some(prev_viewer) = ws.file_viewers.get_mut(&prev_id) {
                            prev_viewer.evict();
                        }
                    }
                }

                // Focus the terminal immediately so it accepts keyboard input
                if let Some(term) = ws.terminals.get(&id) {
                    return iced_term::TerminalView::focus(term.widget_id().clone());
                }

                // Reload an evicted file viewer when its tab becomes active
                if let Some(viewer) = ws.file_viewers.get(&id) {
                    if !viewer.is_loaded() {
                        let path = viewer.path.clone();
                        let repo_root = ws.directory.clone();
                        let pool = ws.sparks_db.clone();
                        let ws_id = ws.workshop_id();
                        return Task::perform(
                            file_viewer::load_file(
                                id,
                                path,
                                repo_root,
                                pool,
                                ws_id,
                                self.appearance == style::Appearance::Light,
                            ),
                            Message::FileViewer,
                        );
                    }
                }
            }
            screen::bench::Message::CloseTab(id) => {
                let ws = &mut self.workshops[idx];
                ws.terminals.remove(&id);
                ws.file_viewers.remove(&id);

                // Mark agent sessions as ended (keep in history) rather than removing
                let mut end_tasks: Vec<Task<Message>> = Vec::new();
                for session in ws.agent_sessions.iter_mut() {
                    if session.tab_id == Some(id) {
                        session.tab_id = None;
                        session.active = false;
                        session.stale = false;
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let sid = session.id.clone();
                            end_tasks.push(Task::perform(
                                async move {
                                    let _ =
                                        data::sparks::agent_session_repo::end_session(&pool, &sid)
                                            .await;
                                },
                                |_| Message::AgentSessionSaved,
                            ));
                        }
                    }
                }

                ws.bench.close_tab(id);
                let persist = self.persist_open_tabs(idx);
                end_tasks.push(persist);
                return Task::batch(end_tasks);
            }
            screen::bench::Message::ToggleDropdown => {
                self.workshops[idx].bench.dropdown_open = !self.workshops[idx].bench.dropdown_open;
            }
            screen::bench::Message::NewTerminal => {
                let next_id = &mut self.next_terminal_id;
                let tab_id = self.workshops[idx].spawn_terminal(
                    "Terminal".to_string(),
                    None,
                    next_id,
                    None,
                    false,
                );
                let persist = self.persist_open_tabs(idx);
                if let Some(term) = self.workshops[idx].terminals.get(&tab_id) {
                    let focus = iced_term::TerminalView::focus(term.widget_id().clone());
                    return Task::batch([focus, persist]);
                }
                return persist;
            }
            screen::bench::Message::NewCodingAgent(agent) => {
                // Legacy direct spawn — preserved for the auto-prompt-default
                // path used by NewDefaultHand. Goes straight to the spark
                // picker with the agent already chosen.
                let full_auto = self
                    .global_config
                    .agent_settings
                    .get(&agent.command)
                    .map_or(false, |s| s.full_auto);

                let ws = &mut self.workshops[idx];
                ws.bench.dropdown_open = false;
                ws.pending_agent_spawn = Some(workshop::PendingAgentSpawn {
                    agent: Some(agent),
                    is_custom: false,
                    custom_def: None,
                    full_auto,
                });
            }
            screen::bench::Message::NewHand => {
                // "+ → New Hand" — open the spark picker without an agent
                // pre-selected. The picker now lets the user choose both.
                let ws = &mut self.workshops[idx];
                ws.bench.dropdown_open = false;
                ws.pending_agent_spawn = Some(workshop::PendingAgentSpawn {
                    agent: None,
                    is_custom: false,
                    custom_def: None,
                    full_auto: false,
                });
            }
            screen::bench::Message::NewHead => {
                // "+ → New Head" — open the Head picker overlay.
                let ws = &mut self.workshops[idx];
                ws.bench.dropdown_open = false;
                ws.pending_head_spawn = Some(screen::head_picker::PickerState::default());
            }
            screen::bench::Message::NewCustomAgent(agent_idx) => {
                let ws = &mut self.workshops[idx];
                let def = match ws.custom_agents.get(agent_idx) {
                    Some(d) => d.clone(),
                    None => return Task::none(),
                };
                let agent = CodingAgent {
                    display_name: def.name.clone(),
                    command: def.command.clone(),
                    args: def.args.clone(),
                    resume: coding_agents::ResumeStrategy::None,
                    compatibility: coding_agents::CompatStatus::Unknown,
                };

                // Show spark picker before spawning the custom agent
                ws.bench.dropdown_open = false;
                ws.pending_agent_spawn = Some(workshop::PendingAgentSpawn {
                    agent: Some(agent),
                    is_custom: true,
                    custom_def: Some(def),
                    full_auto: false,
                });
            }
            // TerminalEvent handled above
            screen::bench::Message::TerminalEvent(_) => {}
        }
        Task::none()
    }

    fn handle_spark_picker_message(&mut self, msg: screen::spark_picker::Message) -> Task<Message> {
        let Some(idx) = self.active_workshop else {
            return Task::none();
        };

        match msg {
            screen::spark_picker::Message::SelectAgent(command) => {
                let ws = &mut self.workshops[idx];
                if let Some(pending) = ws.pending_agent_spawn.as_mut() {
                    if let Some(agent) = self
                        .available_agents
                        .iter()
                        .find(|a| a.command == command)
                        .cloned()
                    {
                        let full_auto = self
                            .global_config
                            .agent_settings
                            .get(&agent.command)
                            .map_or(false, |s| s.full_auto);
                        pending.agent = Some(agent);
                        pending.full_auto = full_auto;
                    }
                }
                Task::none()
            }
            screen::spark_picker::Message::SelectSpark(spark_id) => {
                // Refuse to spawn if no agent has been chosen yet — the
                // picker view greys out spark rows in that case but a
                // synthetic message could still arrive.
                let has_agent = self.workshops[idx]
                    .pending_agent_spawn
                    .as_ref()
                    .and_then(|p| p.agent.as_ref())
                    .is_some();
                if !has_agent {
                    return Task::none();
                }
                self.spawn_pending_agent(idx, spark_id)
            }
            screen::spark_picker::Message::Cancel => {
                self.workshops[idx].pending_agent_spawn = None;
                Task::none()
            }
        }
    }

    fn handle_head_picker_message(&mut self, msg: screen::head_picker::Message) -> Task<Message> {
        let Some(idx) = self.active_workshop else {
            return Task::none();
        };
        match msg {
            screen::head_picker::Message::GoalChanged(goal) => {
                if let Some(state) = self.workshops[idx].pending_head_spawn.as_mut() {
                    state.goal = goal;
                }
                Task::none()
            }
            screen::head_picker::Message::SelectAgent(command) => {
                let goal = self.workshops[idx]
                    .pending_head_spawn
                    .as_ref()
                    .map(|s| s.goal.clone())
                    .unwrap_or_default();
                self.workshops[idx].pending_head_spawn = None;
                let agent = match self
                    .available_agents
                    .iter()
                    .find(|a| a.command == command)
                    .cloned()
                {
                    Some(a) => a,
                    None => return Task::none(),
                };
                self.spawn_head(idx, agent, goal)
            }
            screen::head_picker::Message::Cancel => {
                self.workshops[idx].pending_head_spawn = None;
                Task::none()
            }
        }
    }

    /// Proceed with spawning the pending agent and assigning a spark.
    fn spawn_pending_agent(&mut self, workshop_idx: usize, spark_id: String) -> Task<Message> {
        let ws = &mut self.workshops[workshop_idx];
        let pending = match ws.pending_agent_spawn.take() {
            Some(p) => p,
            None => return Task::none(),
        };
        // Spark picker now waits for both spark and agent — bail if for any
        // reason the agent slot is still empty.
        let pending_agent = match pending.agent {
            Some(a) => a,
            None => return Task::none(),
        };

        let session_id = Uuid::new_v4().to_string();
        let title = pending_agent.display_name.clone();
        let agent_command = pending_agent.command.clone();
        let agent_args = pending_agent.args.clone();

        let tab_id = if pending.is_custom {
            if let Some(ref def) = pending.custom_def {
                ws.spawn_custom_agent(def, &mut self.next_terminal_id, &session_id)
            } else {
                return Task::none();
            }
        } else {
            ws.spawn_terminal(
                title.clone(),
                Some(&pending_agent),
                &mut self.next_terminal_id,
                Some(&session_id),
                pending.full_auto,
            )
        };

        ws.agent_sessions.push(AgentSession {
            id: session_id.clone(),
            name: title.clone(),
            agent: pending_agent,
            tab_id: Some(tab_id),
            active: true,
            stale: false,
            resume_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
        });

        // Persist session to DB + optional spark assignment
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(ref pool) = ws.sparks_db {
            let pool = pool.clone();
            let ws_id = ws.workshop_id();
            let sid_for_assign = session_id.clone();
            let new_session = data::sparks::types::NewAgentSession {
                id: session_id,
                workshop_id: ws_id,
                agent_name: title,
                agent_command,
                agent_args,
                session_label: None,
                child_pid: None,
                resume_id: None,
            };
            tasks.push(Task::perform(
                async move {
                    let _ = data::sparks::agent_session_repo::create(&pool, &new_session).await;
                },
                |_| Message::AgentSessionSaved,
            ));

            // Create hand-spark assignment (spark is required)
            let pool2 = ws.sparks_db.clone().unwrap();

            // Compose the initial prompt: house rules + spark details + DONE checklist
            let prompt = agent_prompts::compose_hand_prompt(&ws.sparks, &spark_id);

            let spark_id_clone = spark_id.clone();
            tasks.push(Task::perform(
                async move {
                    let assignment = data::sparks::types::NewHandAssignment {
                        session_id: sid_for_assign,
                        spark_id: spark_id_clone,
                        role: data::sparks::types::AssignmentRole::Owner,
                    };
                    let _ = data::sparks::assignment_repo::assign(&pool2, assignment).await;
                },
                |_| Message::HandAssignmentSaved,
            ));

            // Send the initial prompt to the agent after a delay (let it boot)
            let prompt_tab_id = tab_id;
            tasks.push(Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                },
                move |_| Message::SendSparkPrompt {
                    tab_id: prompt_tab_id,
                    prompt,
                },
            ));
        }
        if let Some(term) = ws.terminals.get(&tab_id) {
            tasks.push(iced_term::TerminalView::focus(term.widget_id().clone()));
        }
        Task::batch(tasks)
    }

    /// Spawn a Head — a coding agent launched with the Head system prompt
    /// instead of a Hand prompt. The Head has no spark assignment of its
    /// own; its job is to *create* sparks via the `ryve` CLI.
    fn spawn_head(
        &mut self,
        workshop_idx: usize,
        agent: CodingAgent,
        user_goal: String,
    ) -> Task<Message> {
        let ws = &mut self.workshops[workshop_idx];

        let session_id = Uuid::new_v4().to_string();
        let title = format!("Head ({})", agent.display_name);
        let agent_command = agent.command.clone();
        let agent_args = agent.args.clone();
        let full_auto = self
            .global_config
            .agent_settings
            .get(&agent.command)
            .is_some_and(|s| s.full_auto);

        let tab_id = ws.spawn_terminal(
            title.clone(),
            Some(&agent),
            &mut self.next_terminal_id,
            Some(&session_id),
            full_auto,
        );

        ws.agent_sessions.push(AgentSession {
            id: session_id.clone(),
            name: title.clone(),
            agent: agent.clone(),
            tab_id: Some(tab_id),
            active: true,
            stale: false,
            resume_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
        });

        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(ref pool) = ws.sparks_db {
            let pool = pool.clone();
            let ws_id = ws.workshop_id();
            let new_session = data::sparks::types::NewAgentSession {
                id: session_id.clone(),
                workshop_id: ws_id,
                agent_name: title,
                agent_command,
                agent_args,
                session_label: Some("head".to_string()),
                child_pid: None,
                resume_id: None,
            };
            tasks.push(Task::perform(
                async move {
                    let _ = data::sparks::agent_session_repo::create(&pool, &new_session).await;
                },
                |_| Message::AgentSessionSaved,
            ));
        }

        // Inject the Head system prompt the same way the Hand flow injects
        // its prompt: a delayed type-into-terminal so the agent has had time
        // to boot. Coding agents like claude/codex pick up `--system-prompt`
        // via flag too, but the existing infra here uses the typed-prompt
        // path so we stay consistent and avoid having to fork the spawn API.
        let prompt = agent_prompts::compose_head_prompt(&user_goal);
        let prompt_tab_id = tab_id;
        tasks.push(Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            },
            move |_| Message::SendSparkPrompt {
                tab_id: prompt_tab_id,
                prompt,
            },
        ));

        if let Some(term) = ws.terminals.get(&tab_id) {
            tasks.push(iced_term::TerminalView::focus(term.widget_id().clone()));
        }
        Task::batch(tasks)
    }

    fn handle_background_message(
        &mut self,
        msg: screen::background_picker::Message,
    ) -> Task<Message> {
        let Some(idx) = self.active_workshop else {
            return Task::none();
        };
        let ws = &mut self.workshops[idx];

        match msg {
            screen::background_picker::Message::Close => {
                ws.background_picker.open = false;
                Task::none()
            }
            screen::background_picker::Message::PickLocalFile => {
                let bg_dir = ws.ryve_dir.backgrounds_dir();
                Task::perform(
                    async move {
                        let file = rfd::AsyncFileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
                            .pick_file()
                            .await?;
                        let bytes = file.read().await;
                        let name = file.file_name();
                        let dest = bg_dir.join(&name);
                        tokio::fs::write(&dest, &bytes).await.ok()?;
                        Some(name)
                    },
                    |name| match name {
                        Some(name) => Message::LocalFileCopied(name),
                        None => Message::BackgroundConfigSaved, // no-op
                    },
                )
            }
            screen::background_picker::Message::QueryChanged(q) => {
                ws.background_picker.query = q;
                Task::none()
            }
            screen::background_picker::Message::Search => {
                let query = ws.background_picker.query.clone();
                if query.is_empty() {
                    return Task::none();
                }
                ws.background_picker.loading = true;
                ws.background_picker.results.clear();
                ws.background_picker.thumbnails.clear();

                let api_key = std::env::var("UNSPLASH_ACCESS_KEY").unwrap_or_default();
                Task::perform(
                    async move {
                        data::unsplash::search(&api_key, &query, 1)
                            .await
                            .map_err(|e| e.to_string())
                    },
                    |result| Message::UnsplashSearchResult(result),
                )
            }
            screen::background_picker::Message::SearchResults(photos) => {
                ws.background_picker.loading = false;
                ws.background_picker.results = photos.clone();

                // Kick off thumbnail downloads
                let tasks: Vec<_> = photos
                    .into_iter()
                    .map(|photo| {
                        let id = photo.id.clone();
                        let url = photo.thumb_url.clone();
                        Task::perform(
                            async move { data::unsplash::fetch_thumbnail_bytes(&url).await },
                            move |result| match result {
                                Ok(bytes) => Message::Background(
                                    screen::background_picker::Message::ThumbnailLoaded(
                                        id.clone(),
                                        bytes,
                                    ),
                                ),
                                Err(_) => Message::BackgroundConfigSaved, // no-op
                            },
                        )
                    })
                    .collect();

                Task::batch(tasks)
            }
            screen::background_picker::Message::ThumbnailLoaded(id, bytes) => {
                ws.background_picker
                    .thumbnails
                    .insert(id, iced::widget::image::Handle::from_bytes(bytes));
                Task::none()
            }
            screen::background_picker::Message::SelectPhoto(photo) => {
                ws.background_picker.loading = true;
                let api_key = std::env::var("UNSPLASH_ACCESS_KEY").unwrap_or_default();
                let bg_dir = ws.ryve_dir.backgrounds_dir();
                let photographer = photo.photographer.clone();
                let photographer_url = photo.photographer_url.clone();

                Task::perform(
                    async move {
                        data::unsplash::download(&api_key, &photo, &bg_dir)
                            .await
                            .map_err(|e| e.to_string())
                    },
                    move |result| match result {
                        Ok(filename) => Message::UnsplashDownloaded {
                            filename,
                            photographer: photographer.clone(),
                            photographer_url: photographer_url.clone(),
                        },
                        Err(e) => Message::UnsplashDownloadFailed(e),
                    },
                )
            }
            screen::background_picker::Message::RemoveBackground => {
                ws.config.background.image = None;
                ws.config.background.unsplash_photographer = None;
                ws.config.background.unsplash_photographer_url = None;
                ws.background_handle = None;
                ws.bg_is_dark = None;
                ws.background_picker.open = false;

                let ryve_dir = ws.ryve_dir.clone();
                let config = ws.config.clone();
                Task::perform(
                    async move {
                        data::ryve_dir::save_config(&ryve_dir, &config).await.ok();
                    },
                    |_| Message::BackgroundConfigSaved,
                )
            }

            // ── Agent Settings ───────────────────────────────
            screen::background_picker::Message::SetDefaultAgent(cmd) => {
                self.global_config.default_agent = cmd;
                let config = self.global_config.clone();
                Task::perform(
                    async move {
                        config.save().ok();
                    },
                    |_| Message::BackgroundConfigSaved,
                )
            }
            screen::background_picker::Message::ToggleFullAuto(cmd) => {
                let entry = self
                    .global_config
                    .agent_settings
                    .entry(cmd)
                    .or_insert(data::config::AgentConfig { full_auto: false });
                entry.full_auto = !entry.full_auto;
                let config = self.global_config.clone();
                Task::perform(
                    async move {
                        config.save().ok();
                    },
                    |_| Message::BackgroundConfigSaved,
                )
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let term_subs: Vec<_> = self
            .workshops
            .iter()
            .flat_map(|ws| ws.terminals.values())
            .map(|term| {
                term.subscription()
                    .map(|e| Message::Bench(screen::bench::Message::TerminalEvent(e)))
            })
            .collect();

        let poll =
            iced::time::every(std::time::Duration::from_secs(3)).map(|_| Message::SparksPoll);

        let hotkeys = keyboard::listen().map(|event| {
            match &event {
                keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(c),
                    modifiers,
                    ..
                } => {
                    if modifiers.command() && c.as_str() == "h" {
                        return Message::NewDefaultHand;
                    }
                    if modifiers.command() && c.as_str() == "c" {
                        return Message::FileViewer(file_viewer::Message::CopySelection);
                    }
                }
                keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                } => {
                    return Message::FileViewer(file_viewer::Message::ClearSelection);
                }
                keyboard::Event::ModifiersChanged(modifiers) => {
                    return Message::ShiftStateChanged(modifiers.shift());
                }
                _ => {}
            }
            // Swallow unmatched keyboard events — SparksPoll is a harmless no-op
            Message::SparksPoll
        });

        // Track window resizes so the splitter can convert vertical
        // drag deltas into a sensible sidebar split ratio.
        let resizes = window::resize_events().map(|(_, size)| Message::WindowResized(size));

        let mut subs: Vec<Subscription<Message>> = term_subs
            .into_iter()
            .chain(std::iter::once(poll))
            .chain(std::iter::once(hotkeys))
            .chain(std::iter::once(resizes))
            .collect();

        // Only listen to global mouse events while a splitter drag is
        // in progress — otherwise we'd waste cycles on every cursor
        // move when nothing cares about them.
        if self.splitter_drag.is_some() {
            subs.push(event::listen_with(splitter_event_filter));
        }

        Subscription::batch(subs)
    }

    fn view(&self) -> Element<'_, Message> {
        let workshop_bar = self.view_workshop_bar();

        let ws = self.active_workshop();

        let content = if let Some(ws) = ws {
            self.view_workshop(ws)
        } else {
            self.view_welcome()
        };

        let main_content: Element<'_, Message> = column![workshop_bar, content]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        let toast_pal = ws
            .map(|ws| match ws.bg_is_dark {
                Some(true) => style::Palette::dark(),
                Some(false) => style::Palette::light(),
                None => self.appearance.palette(),
            })
            .unwrap_or_else(|| self.appearance.palette());
        let toast_overlay = toast::view(&self.toasts, &toast_pal).map(|e| e.map(Message::Toast));

        // Layer background image behind everything (including tab bar)
        if let Some(ws) = ws {
            if ws.background_handle.is_some() || ws.background_picker.open {
                let mut layers: Vec<Element<'_, Message>> = Vec::new();

                if let Some(ref handle) = ws.background_handle {
                    layers.push(
                        iced::widget::image(handle.clone())
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .content_fit(iced::ContentFit::Cover)
                            .into(),
                    );

                    let opacity = ws.config.background.dim_opacity;
                    layers.push(
                        container(Space::new())
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .style(move |_theme: &Theme| container::Style {
                                background: Some(iced::Background::Color(Color {
                                    r: 0.0,
                                    g: 0.0,
                                    b: 0.0,
                                    a: opacity,
                                })),
                                ..Default::default()
                            })
                            .into(),
                    );
                }

                layers.push(main_content);

                // Settings modal overlay
                if ws.background_picker.open {
                    let has_bg = ws.config.background.image.is_some();
                    let pal = self.appearance.palette();
                    let agents: Vec<screen::background_picker::AgentInfo> = self
                        .available_agents
                        .iter()
                        .map(|a| screen::background_picker::AgentInfo {
                            command: a.command.clone(),
                            display_name: a.display_name.clone(),
                            full_auto: self
                                .global_config
                                .agent_settings
                                .get(&a.command)
                                .map_or(false, |s| s.full_auto),
                            is_default: self
                                .global_config
                                .default_agent
                                .as_ref()
                                .map_or(false, |d| d == &a.command),
                        })
                        .collect();
                    layers.push(
                        screen::background_picker::view(
                            &ws.background_picker,
                            &pal,
                            has_bg,
                            agents,
                        )
                        .map(Message::Background),
                    );
                }

                let stacked: Element<'_, Message> = stack(layers)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into();
                return overlay_with_toasts(stacked, toast_overlay);
            }
        }

        overlay_with_toasts(main_content, toast_overlay)
    }

    /// Top-level tab bar for workshops — liquid glass pill tabs.
    fn view_workshop_bar(&self) -> Element<'_, Message> {
        let pal = self.appearance.palette();
        let has_bg = self
            .active_workshop()
            .map_or(false, |ws| ws.background_handle.is_some());
        let mut tab_row = row![].spacing(6).align_y(iced::Alignment::Center);

        for (idx, ws) in self.workshops.iter().enumerate() {
            let is_active = self.active_workshop == Some(idx);
            let text_color = if is_active {
                pal.text_primary
            } else {
                pal.text_secondary
            };

            let tab_content = row![
                button(text(ws.name()).size(12).color(text_color))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::SelectWorkshop(idx)),
                button(text("\u{00D7}").size(14).color(pal.text_tertiary))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::CloseWorkshop(idx)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);

            let pill = container(tab_content)
                .padding([5, 12])
                .style(move |_theme: &Theme| style::tab_pill(&pal, is_active));

            tab_row = tab_row.push(pill);
        }

        let new_btn = button(text("+ New Workshop").size(12).color(pal.text_secondary))
            .style(button::text)
            .padding([5, 12])
            .on_press(Message::NewWorkshopDialog);

        let mut bar = row![].align_y(iced::Alignment::Center).spacing(6);
        if style::TRAFFIC_LIGHT_WIDTH > 0.0 {
            bar = bar.push(Space::new().width(style::TRAFFIC_LIGHT_WIDTH));
        }
        bar = bar.push(tab_row);
        bar = bar.push(Space::new().width(Length::Fill));
        bar = bar.push(new_btn);

        container(bar.padding([0, 12]))
            .width(Length::Fill)
            .padding([style::TITLE_BAR_TOP_PAD, 0.0])
            .center_y(38)
            .style(move |_theme: &Theme| style::tab_bar(&pal, has_bg))
            .into()
    }

    /// Welcome screen when no workshops are open.
    fn view_welcome(&self) -> Element<'_, Message> {
        let pal = self.appearance.palette();
        container(
            column![
                text("Ryve").size(40).color(pal.text_primary),
                text("Open a workshop to get started")
                    .size(16)
                    .color(pal.text_secondary),
                button(text("Open Workshop...").size(14))
                    .style(button::primary)
                    .padding([8, 20])
                    .on_press(Message::NewWorkshopDialog),
            ]
            .spacing(16)
            .align_x(iced::Alignment::Center),
        )
        .center(Length::Fill)
        .into()
    }

    /// Full workshop view (sidebar + bench), with optional background image.
    fn view_workshop<'a>(&'a self, ws: &'a Workshop) -> Element<'a, Message> {
        let has_bg = ws.background_handle.is_some();
        // Adaptive palette: if background image is present, choose palette based
        // on image luminance. Otherwise fall back to system appearance.
        let pal = match ws.bg_is_dark {
            Some(true) => style::Palette::dark(),
            Some(false) => style::Palette::light(),
            None => self.appearance.palette(),
        };

        // -- Left sidebar: files (top) + agents (bottom) --
        let files_view =
            file_explorer::view(&ws.file_explorer, &ws.directory, &pal).map(Message::FileExplorer);

        let files_panel = container(files_view)
            .width(Length::Fill)
            .height(Length::FillPortion((ws.sidebar_split() * 100.0) as u16))
            .style(move |_theme: &Theme| style::glass_panel(&pal, has_bg));

        let agents_panel = container(self.view_agents(ws, has_bg, &pal))
            .width(Length::Fill)
            .height(Length::FillPortion(
                ((1.0 - ws.sidebar_split()) * 100.0) as u16,
            ))
            .style(move |_theme: &Theme| style::glass_panel(&pal, has_bg));

        let sidebar_files_hands_splitter = widget::splitter::horizontal(
            Message::SplitterPressed(SplitterKind::SidebarFilesHands),
            &pal,
        );

        let sidebar = column![files_panel, sidebar_files_hands_splitter, agents_panel]
            .spacing(0)
            .width(ws.sidebar_width())
            .height(Length::Fill);

        // -- Center: bench (tabbed area) --
        let bench = self.view_bench(ws, has_bg, &pal);

        // -- Right: sparks panel (or detail view) --
        let sparks_panel = if let Some(ref selected_id) = ws.selected_spark {
            if let Some(spark) = ws.sparks.iter().find(|s| s.id == *selected_id) {
                screen::spark_detail::view(
                    spark,
                    &ws.selected_spark_contracts,
                    &ws.selected_spark_bonds,
                    &ws.sparks,
                    &ws.contract_create_form,
                    &pal,
                    has_bg,
                )
                .map(Message::SparkDetail)
            } else {
                screen::sparks::view(
                    &ws.sparks,
                    &ws.blocked_spark_ids,
                    &pal,
                    has_bg,
                    &ws.spark_create_form,
                )
                .map(Message::Sparks)
            }
        } else {
            screen::sparks::view(
                &ws.sparks,
                &ws.blocked_spark_ids,
                &pal,
                has_bg,
                &ws.spark_create_form,
            )
            .map(Message::Sparks)
        };

        let sparks_col = container(sparks_panel)
            .width(ws.sparks_width())
            .height(Length::Fill);

        let sidebar_bench_splitter =
            widget::splitter::vertical(Message::SplitterPressed(SplitterKind::SidebarRight), &pal);
        let bench_sparks_splitter =
            widget::splitter::vertical(Message::SplitterPressed(SplitterKind::SparksLeft), &pal);

        // -- Bottom: status bar --
        let spark_summary = {
            let mut s = screen::status_bar::SparkSummary::default();
            for spark in &ws.sparks {
                match spark.status.as_str() {
                    "open" => s.open += 1,
                    "in_progress" => s.in_progress += 1,
                    "blocked" => s.blocked += 1,
                    "deferred" => s.deferred += 1,
                    "closed" => s.closed += 1,
                    _ => {}
                }
            }
            s
        };
        let git_stats = {
            let mut gs = screen::status_bar::GitStats::default();
            for stat in ws.file_explorer.diff_stats.values() {
                gs.additions += stat.additions;
                gs.deletions += stat.deletions;
            }
            gs.changed_files = ws.file_explorer.git_statuses.len();
            gs
        };
        let active_hands = ws.agent_sessions.iter().filter(|a| a.active).count();
        let total_hands = ws.agent_sessions.iter().filter(|a| !a.stale).count();

        // Build file viewer info if the active bench tab is a file viewer.
        let file_info = ws.bench.active_tab.and_then(|tab_id| {
            let viewer = ws.file_viewers.get(&tab_id)?;
            let (line, column) = viewer.cursor_position();
            Some(screen::status_bar::FileViewerInfo {
                line,
                column,
                total_lines: viewer.total_lines(),
                language: screen::file_viewer::language_label(&viewer.path),
            })
        });

        let status_bar = screen::status_bar::view(
            ws.file_explorer.branch.as_deref(),
            &ws.directory,
            &spark_summary,
            &git_stats,
            active_hands,
            total_hands,
            ws.failing_contracts,
            file_info,
            &pal,
            has_bg,
        )
        .map(Message::StatusBar);

        let main_row = container(
            row![
                sidebar,
                sidebar_bench_splitter,
                bench,
                bench_sparks_splitter,
                sparks_col
            ]
            .spacing(0)
            .height(Length::Fill),
        )
        .padding(style::PANEL_GAP)
        .width(Length::Fill)
        .height(Length::Fill);

        let workshop_content: Element<'a, Message> =
            column![main_row, status_bar,].height(Length::Fill).into();

        // Layer background image behind content
        let mut layers: Vec<Element<'a, Message>> = Vec::new();

        if let Some(ref handle) = ws.background_handle {
            layers.push(
                iced::widget::image(handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(iced::ContentFit::Cover)
                    .into(),
            );

            // Dim overlay so UI stays readable
            let opacity = ws.config.background.dim_opacity;
            layers.push(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_theme: &Theme| container::Style {
                        background: Some(iced::Background::Color(Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: opacity,
                        })),
                        ..Default::default()
                    })
                    .into(),
            );
        }

        layers.push(workshop_content);

        // Background picker modal overlay
        if ws.background_picker.open {
            let has_bg = ws.config.background.image.is_some();
            let agents: Vec<_> = self
                .available_agents
                .iter()
                .map(|a| screen::background_picker::AgentInfo {
                    command: a.command.clone(),
                    display_name: a.display_name.clone(),
                    full_auto: self
                        .global_config
                        .agent_settings
                        .get(&a.command)
                        .map_or(false, |s| s.full_auto),
                    is_default: self
                        .global_config
                        .default_agent
                        .as_ref()
                        .map_or(false, |d| d == &a.command),
                })
                .collect();
            layers.push(
                screen::background_picker::view(&ws.background_picker, &pal, has_bg, agents)
                    .map(Message::Background),
            );
        }

        // Spark picker modal overlay (shown before spawning a Hand)
        if let Some(ref pending) = ws.pending_agent_spawn {
            let selected = pending.agent.as_ref().map(|a| a.command.as_str());
            layers.push(
                screen::spark_picker::view(&ws.sparks, &self.available_agents, selected, &pal)
                    .map(Message::SparkPicker),
            );
        }

        // Head picker modal overlay (shown before spawning a Head)
        if let Some(ref state) = ws.pending_head_spawn {
            layers.push(
                screen::head_picker::view(state, &self.available_agents, &pal)
                    .map(Message::HeadPicker),
            );
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn view_agents<'a>(
        &'a self,
        ws: &'a Workshop,
        has_bg: bool,
        pal: &style::Palette,
    ) -> Element<'a, Message> {
        screen::agents::view(&ws.agent_sessions, *pal, has_bg).map(Message::Agents)
    }

    fn view_bench<'a>(
        &'a self,
        ws: &'a Workshop,
        has_bg: bool,
        pal: &style::Palette,
    ) -> Element<'a, Message> {
        let tab_bar = ws.bench.view_tab_bar(pal).map(Message::Bench);

        let content: Element<'a, Message> = if let Some(active_id) = ws.bench.active_tab {
            if let Some(term) = ws.terminals.get(&active_id) {
                iced_term::TerminalView::show_with_transparent_bg(term, has_bg)
                    .map(|e| Message::Bench(screen::bench::Message::TerminalEvent(e)))
                    .into()
            } else if let Some(viewer) = ws.file_viewers.get(&active_id) {
                file_viewer::view(viewer, pal, has_bg).map(Message::FileViewer)
            } else {
                container(text("Loading...").size(14))
                    .center(Length::Fill)
                    .into()
            }
        } else {
            container(
                column![
                    text("Ryve").size(32).color(pal.text_primary),
                    text("Press + to open a terminal or coding agent")
                        .size(14)
                        .color(pal.text_secondary),
                ]
                .spacing(8)
                .align_x(iced::Alignment::Center),
            )
            .center(Length::Fill)
            .into()
        };

        let body = column![tab_bar, content]
            .width(Length::Fill)
            .height(Length::Fill);

        // Overlay the dropdown menu on top of the content area
        if let Some(dropdown) =
            ws.bench
                .view_dropdown(&self.available_agents, &ws.custom_agents, pal)
        {
            stack![
                body,
                // Position the dropdown just below the tab bar
                column![
                    Space::new().height(30), // approximate tab bar height
                    dropdown.map(Message::Bench),
                ]
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            body.into()
        }
    }

    fn theme(&self) -> Theme {
        self.appearance.theme()
    }
}

/// Translate global runtime events into splitter messages while a
/// drag is in progress. `listen_with` requires a `fn` (no closures),
/// so we always emit messages and let the `update` function decide
/// what to do based on `splitter_drag` state.
fn splitter_event_filter(
    event: iced::Event,
    _status: event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(Message::SplitterMoved(position))
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::SplitterReleased)
        }
        _ => None,
    }
}

/// Stack toast notifications on top of an existing view, if any are active.
fn overlay_with_toasts<'a>(
    base: Element<'a, Message>,
    toasts: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    match toasts {
        Some(toast_layer) => stack![base, toast_layer]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => base,
    }
}

/// Open a native directory picker dialog.
async fn pick_workshop_directory() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Select Workshop Directory")
        .pick_folder()
        .await
        .map(|handle| handle.path().to_path_buf())
}

/// Load persisted agent sessions for a workshop from the database.
async fn load_agent_sessions(
    pool: sqlx::SqlitePool,
    workshop_id: String,
) -> Vec<PersistedAgentSession> {
    data::sparks::agent_session_repo::list_for_workshop(&pool, &workshop_id)
        .await
        .unwrap_or_default()
}

/// Load the persisted open-tabs snapshot for a workshop. Errors are
/// swallowed since failing to restore tabs is non-fatal — the user just
/// gets an empty bench.
async fn load_open_tabs(
    pool: sqlx::SqlitePool,
    workshop_id: String,
) -> Vec<data::sparks::open_tab_repo::PersistedTab> {
    data::sparks::open_tab_repo::list_for_workshop(&pool, &workshop_id)
        .await
        .unwrap_or_default()
}

/// Load all contracts for a single spark from the database. Errors are
/// swallowed (treated as empty) since this is a non-critical display value.
async fn load_contracts(pool: sqlx::SqlitePool, spark_id: String) -> Vec<Contract> {
    data::sparks::contract_repo::list_for_spark(&pool, &spark_id)
        .await
        .unwrap_or_default()
}

/// Load all bonds touching a single spark (incoming + outgoing). Errors
/// are swallowed since this is a non-critical display value.
async fn load_bonds(pool: sqlx::SqlitePool, spark_id: String) -> Vec<Bond> {
    data::sparks::bond_repo::list_for_spark(&pool, &spark_id)
        .await
        .unwrap_or_default()
}

/// Load the set of spark IDs that have at least one open blocking bond
/// pointing at them, scoped to the given workshop. Errors are swallowed.
async fn load_blocked_spark_ids(
    pool: sqlx::SqlitePool,
    workshop_id: String,
) -> HashSet<String> {
    data::sparks::bond_repo::list_blocked_spark_ids(&pool, &workshop_id)
        .await
        .unwrap_or_default()
}

/// Execute a contract check command via the user's shell from the workshop
/// directory and translate the exit status into a `ContractStatus`.
///
/// - `pass` if the command exits 0
/// - `fail` if the command exits non-zero or fails to spawn
async fn run_contract_check(
    command: &str,
    cwd: &std::path::Path,
) -> data::sparks::types::ContractStatus {
    use data::sparks::types::ContractStatus;
    let result = tokio::process::Command::new("/bin/sh")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    match result {
        Ok(status) if status.success() => ContractStatus::Pass,
        _ => ContractStatus::Fail,
    }
}

/// Count the failing or pending required contracts for a workshop. Used by
/// the status bar warning indicator. Errors are swallowed (treated as zero)
/// since this is a non-critical display value.
async fn load_failing_contract_count(pool: sqlx::SqlitePool, workshop_id: String) -> usize {
    data::sparks::contract_repo::list_failing(&pool, &workshop_id)
        .await
        .map(|v| v.len())
        .unwrap_or(0)
}

/// Load all sparks for a workshop from the database.
async fn load_sparks(pool: sqlx::SqlitePool, workshop_id: String) -> Vec<Spark> {
    data::sparks::spark_repo::list(
        &pool,
        data::sparks::types::SparkFilter {
            workshop_id: Some(workshop_id),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_default()
}
