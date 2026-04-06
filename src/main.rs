// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

mod coding_agents;
mod icons;
mod screen;
mod style;
mod widget;
mod workshop;

use std::path::PathBuf;

use data::sparks::types::{PersistedAgentSession, Spark};
use iced::widget::{Space, button, column, container, row, stack, text};
use iced::keyboard;
use iced::{Color, Element, Length, Subscription, Task, Theme};
use uuid::Uuid;

use coding_agents::CodingAgent;
use screen::agents::AgentSession;
use screen::file_explorer;
use screen::file_viewer;
use style::Appearance;
use workshop::Workshop;

fn main() -> iced::Result {
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
}

#[derive(Clone)]
enum Message {
    /// Workshop-level tab bar
    SelectWorkshop(usize),
    CloseWorkshop(usize),
    NewWorkshopDialog,
    WorkshopDirPicked(Option<PathBuf>),

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
    /// Agent sessions loaded from DB
    AgentSessionsLoaded(Uuid, Vec<PersistedAgentSession>),
    /// Agent session saved to DB
    AgentSessionSaved,
    /// File tree scanned for a workshop
    FilesScanned(Uuid, file_explorer::Message),

    /// Forwarded to the active workshop
    FileExplorer(screen::file_explorer::Message),
    FileViewer(screen::file_viewer::Message),
    Agents(screen::agents::Message),
    Bench(screen::bench::Message),
    Sparks(screen::sparks::Message),
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
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelectWorkshop(i) => write!(f, "SelectWorkshop({i})"),
            Self::CloseWorkshop(i) => write!(f, "CloseWorkshop({i})"),
            Self::NewWorkshopDialog => write!(f, "NewWorkshopDialog"),
            Self::WorkshopDirPicked(p) => write!(f, "WorkshopDirPicked({p:?})"),
            Self::WorkshopReady { id, .. } => write!(f, "WorkshopReady({id})"),
            Self::SparksLoaded(id, s) => write!(f, "SparksLoaded({id}, {} sparks)", s.len()),
            Self::AgentSessionsLoaded(id, s) => write!(f, "AgentSessionsLoaded({id}, {} sessions)", s.len()),
            Self::AgentSessionSaved => write!(f, "AgentSessionSaved"),
            Self::FilesScanned(id, _) => write!(f, "FilesScanned({id})"),
            Self::FileExplorer(m) => write!(f, "FileExplorer({m:?})"),
            Self::FileViewer(m) => write!(f, "FileViewer({m:?})"),
            Self::Agents(m) => write!(f, "Agents({m:?})"),
            Self::Bench(m) => write!(f, "Bench({m:?})"),
            Self::Sparks(m) => write!(f, "Sparks({m:?})"),
            Self::Background(m) => write!(f, "Background({m:?})"),
            Self::StatusBar(m) => write!(f, "StatusBar({m:?})"),
            Self::BackgroundLoaded(id, _) => write!(f, "BackgroundLoaded({id})"),
            Self::UnsplashDownloaded { filename, .. } => {
                write!(f, "UnsplashDownloaded({filename})")
            }
            Self::LocalFileCopied(name) => write!(f, "LocalFileCopied({name})"),
            Self::BackgroundConfigSaved => write!(f, "BackgroundConfigSaved"),
            Self::AgentContextSynced => write!(f, "AgentContextSynced"),
            Self::SparksPoll => write!(f, "SparksPoll"),
            Self::NewDefaultHand => write!(f, "NewDefaultHand"),
        }
    }
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let global_config = data::config::Config::load();
        let available_agents = coding_agents::detect_available();
        let appearance = Appearance::detect();

        (
            Self {
                appearance,
                global_config,
                available_agents,
                workshops: Vec::new(),
                active_workshop: None,
                next_terminal_id: 1,
                poll_in_flight: false,
            },
            Task::none(),
        )
    }

    fn active_workshop(&self) -> Option<&Workshop> {
        self.active_workshop.and_then(|i| self.workshops.get(i))
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
                    Err(e) => {
                        log::error!("Failed to init workshop: {e}");
                        Message::WorkshopDirPicked(None)
                    }
                })
            }
            Message::WorkshopDirPicked(None) => Task::none(),

            Message::WorkshopReady {
                id,
                pool,
                config,
                custom_agents,
                agent_context,
            } => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else { return Task::none(); };
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
                    let sessions_task = Task::perform(
                        load_agent_sessions(pool2, ws_id2),
                        move |sessions| Message::AgentSessionsLoaded(id, sessions),
                    );
                    let ignore = ws.config.explorer.ignore.clone();
                    let scan_task = Task::perform(
                        file_explorer::scan_directory(dir, ignore),
                        move |(tree, statuses, diff_stats, branch)| {
                            Message::FilesScanned(
                                id,
                                file_explorer::Message::TreeLoaded(tree, statuses, diff_stats, branch),
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

                    return Task::batch([sparks_task, sessions_task, scan_task, bg_task]);
                }
                Task::none()
            }
            Message::SparksLoaded(id, sparks) => {
                self.poll_in_flight = false;
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else { return Task::none(); };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    ws.sparks = sparks;

                    // Sync .ryve/WORKSHOP.md and pointers (including into worktrees)
                    if !ws.config.agents.disable_sync {
                        let dir = ws.directory.clone();
                        let ryve_dir = ws.ryve_dir.clone();
                        let config = ws.config.clone();
                        return Task::perform(
                            async move {
                                let _ = data::agent_context::sync(
                                    &dir, &ryve_dir, &config,
                                )
                                .await;
                            },
                            |_| Message::AgentContextSynced,
                        );
                    }
                }
                Task::none()
            }

            Message::AgentSessionsLoaded(id, persisted) => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else { return Task::none(); };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    let available = &self.available_agents;
                    ws.agent_sessions = persisted
                        .into_iter()
                        .map(|p| {
                            let agent = available
                                .iter()
                                .find(|a| a.command == p.agent_command)
                                .cloned()
                                .unwrap_or_else(|| CodingAgent {
                                    display_name: p.agent_name.clone(),
                                    command: p.agent_command.clone(),
                                    args: serde_json::from_str(&p.agent_args)
                                        .unwrap_or_default(),
                                    resume: coding_agents::ResumeStrategy::None,
                                });
                            AgentSession {
                                id: p.id,
                                name: p.agent_name,
                                agent,
                                tab_id: None,
                                active: false,
                                resume_id: p.resume_id,
                                started_at: p.started_at,
                            }
                        })
                        .collect();
                }
                Task::none()
            }

            Message::AgentSessionSaved => Task::none(),

            Message::FilesScanned(id, msg) => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else { return Task::none(); };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    if let file_explorer::Message::TreeLoaded(tree, statuses, diff_stats, branch) = msg {
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
                            return Task::perform(
                                file_viewer::load_file(tab_id, file_path, repo_root, pool, ws_id, self.appearance == style::Appearance::Light),
                                Message::FileViewer,
                            );
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
                                    file_explorer::Message::TreeLoaded(tree, statuses, diff_stats, branch),
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
                                        let _ = data::sparks::file_link_repo::create(&pool, &link).await;
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
                    file_viewer::Message::Scrolled { offset_y, viewport_height } => {
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
                }
                Task::none()
            }
            Message::Agents(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                match msg {
                    screen::agents::Message::SelectAgent(id) => {
                        let id_str = id.to_string();
                        if let Some(session) = ws.agent_sessions.iter().find(|s| s.id == id_str) {
                            if let Some(tab_id) = session.tab_id {
                                ws.bench.active_tab = Some(tab_id);
                            }
                        }
                    }
                    screen::agents::Message::ResumeAgent(session_id) => {
                        // Find the session and resume it
                        let session = ws.agent_sessions.iter().find(|s| s.id == session_id).cloned();
                        if let Some(session) = session {
                            if let Some((cmd, args)) = session.agent.resume_args(session.resume_id.as_deref()) {
                                let resume_agent = CodingAgent {
                                    display_name: session.agent.display_name.clone(),
                                    command: cmd.clone(),
                                    args: args.clone(),
                                    resume: session.agent.resume.clone(),
                                };
                                let next_id = &mut self.next_terminal_id;
                                let full_auto = self.global_config.agent_settings
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
                                if let Some(s) = ws.agent_sessions.iter_mut().find(|s| s.id == session_id) {
                                    s.tab_id = Some(tab_id);
                                    s.active = true;
                                }

                                // Mark as active in DB
                                if let Some(ref pool) = ws.sparks_db {
                                    let pool = pool.clone();
                                    let sid = session_id.clone();
                                    return Task::perform(
                                        async move {
                                            let _ = data::sparks::agent_session_repo::reactivate(&pool, &sid).await;
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
                                    let _ = data::sparks::agent_session_repo::delete(&pool, &sid).await;
                                },
                                |_| Message::AgentSessionSaved,
                            );
                        }
                    }
                }
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
                                return Task::perform(
                                    load_sparks(pool, ws_id),
                                    move |sparks| Message::SparksLoaded(id, sparks),
                                );
                            }
                        }
                    }
                    screen::sparks::Message::SelectSpark(_id) => {
                        // TODO: open spark detail view
                    }
                    screen::sparks::Message::ShowCreateForm => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.visible = true;
                            ws.spark_create_form.title.clear();
                        }
                    }
                    screen::sparks::Message::CreateFormTitleChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.title = val;
                        }
                    }
                    screen::sparks::Message::CancelCreate => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_create_form.visible = false;
                            ws.spark_create_form.title.clear();
                        }
                    }
                    screen::sparks::Message::SubmitNewSpark => {
                        let ws = &mut self.workshops[idx];
                        let title = ws.spark_create_form.title.trim().to_string();
                        if title.is_empty() {
                            return Task::none();
                        }
                        ws.spark_create_form.visible = false;
                        ws.spark_create_form.title.clear();

                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let ws_id = ws.workshop_id();
                            let id = ws.id;
                            return Task::perform(
                                async move {
                                    let new = data::sparks::types::NewSpark {
                                        title,
                                        description: String::new(),
                                        spark_type: data::sparks::types::SparkType::Task,
                                        priority: 2,
                                        workshop_id: ws_id.clone(),
                                        assignee: None,
                                        owner: None,
                                        parent_id: None,
                                        due_at: None,
                                        estimated_minutes: None,
                                        metadata: None,
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
                                                &pool, &spark_id, "completed", "user",
                                            )
                                            .await;
                                        } else {
                                            let status = data::sparks::types::SparkStatus::from_str(&new_status);
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
            Message::Background(msg) => self.handle_background_message(msg),
            Message::BackgroundLoaded(id, Some(bytes)) => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else { return Task::none(); };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    // Compute luminance to choose adaptive font color
                    if let Some(lum) = workshop::compute_image_luminance(&bytes) {
                        ws.bg_is_dark = Some(lum < 0.5);
                    }
                    ws.background_handle =
                        Some(iced::widget::image::Handle::from_bytes(bytes));
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
                        async move { data::ryve_dir::save_config(&ryve_dir, &config).await.ok(); },
                        |_| Message::BackgroundConfigSaved,
                    ),
                ])
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
                        async move { data::ryve_dir::save_config(&ryve_dir, &config).await.ok(); },
                        |_| Message::BackgroundConfigSaved,
                    ),
                ])
            }
            Message::BackgroundConfigSaved => Task::none(),
            Message::AgentContextSynced => Task::none(),
            Message::SparksPoll => {
                if self.poll_in_flight {
                    return Task::none();
                }

                let mut tasks: Vec<Task<Message>> = Vec::new();

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
                                resume_id: None,
                            };
                            tasks.push(Task::perform(
                                async move {
                                    let _ = data::sparks::agent_session_repo::create(&pool, &new_session).await;
                                },
                                |_| Message::AgentSessionSaved,
                            ));
                        }
                    }
                }

                // Poll all workshops that have a sparks_db and at least one active agent session
                let spark_tasks: Vec<_> = self
                    .workshops
                    .iter()
                    .filter(|ws| {
                        ws.sparks_db.is_some()
                            && ws.agent_sessions.iter().any(|s| s.active)
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
                let Some(agent) = self.available_agents.iter().find(|a| &a.command == default_cmd).cloned() else {
                    return Task::none();
                };
                // Delegate to the existing NewCodingAgent flow
                return self.handle_bench_message(screen::bench::Message::NewCodingAgent(agent));
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
                if let Some(term) = ws.terminals.get_mut(&id) {
                    let action = term.handle(iced_term::Command::ProxyToBackend(cmd.clone()));
                    ws.handle_terminal_action(id, action);
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
                            file_viewer::load_file(id, path, repo_root, pool, ws_id, self.appearance == style::Appearance::Light),
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
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let sid = session.id.clone();
                            end_tasks.push(Task::perform(
                                async move {
                                    let _ = data::sparks::agent_session_repo::end_session(&pool, &sid).await;
                                },
                                |_| Message::AgentSessionSaved,
                            ));
                        }
                    }
                }

                ws.bench.close_tab(id);
                if !end_tasks.is_empty() {
                    return Task::batch(end_tasks);
                }
            }
            screen::bench::Message::ToggleDropdown => {
                self.workshops[idx].bench.dropdown_open = !self.workshops[idx].bench.dropdown_open;
            }
            screen::bench::Message::NewTerminal => {
                let next_id = &mut self.next_terminal_id;
                let tab_id =
                    self.workshops[idx].spawn_terminal("Terminal".to_string(), None, next_id, None, false);
                if let Some(term) = self.workshops[idx].terminals.get(&tab_id) {
                    return iced_term::TerminalView::focus(term.widget_id().clone());
                }
            }
            screen::bench::Message::NewCodingAgent(agent) => {
                let title = agent.display_name.clone();
                let session_id = Uuid::new_v4().to_string();
                let full_auto = self.global_config.agent_settings
                    .get(&agent.command)
                    .map_or(false, |s| s.full_auto);
                let next_id = &mut self.next_terminal_id;
                let tab_id =
                    self.workshops[idx].spawn_terminal(title.clone(), Some(&agent), next_id, Some(&session_id), full_auto);
                self.workshops[idx].agent_sessions.push(AgentSession {
                    id: session_id.clone(),
                    name: title.clone(),
                    agent: agent.clone(),
                    tab_id: Some(tab_id),
                    active: true,
                    resume_id: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                });

                // Persist to DB
                let mut tasks: Vec<Task<Message>> = Vec::new();
                if let Some(ref pool) = self.workshops[idx].sparks_db {
                    let pool = pool.clone();
                    let ws_id = self.workshops[idx].workshop_id();
                    let new_session = data::sparks::types::NewAgentSession {
                        id: session_id,
                        workshop_id: ws_id,
                        agent_name: title,
                        agent_command: agent.command.clone(),
                        agent_args: agent.args.clone(),
                        session_label: None,
                        resume_id: None,
                    };
                    tasks.push(Task::perform(
                        async move {
                            let _ = data::sparks::agent_session_repo::create(&pool, &new_session).await;
                        },
                        |_| Message::AgentSessionSaved,
                    ));
                }
                if let Some(term) = self.workshops[idx].terminals.get(&tab_id) {
                    tasks.push(iced_term::TerminalView::focus(term.widget_id().clone()));
                }
                return Task::batch(tasks);
            }
            screen::bench::Message::NewCustomAgent(agent_idx) => {
                let ws = &mut self.workshops[idx];
                let def = match ws.custom_agents.get(agent_idx) {
                    Some(d) => d.clone(),
                    None => return Task::none(),
                };
                let session_id = Uuid::new_v4().to_string();
                let next_id = &mut self.next_terminal_id;
                let tab_id = ws.spawn_custom_agent(&def, next_id, &session_id);
                ws.agent_sessions.push(AgentSession {
                    id: session_id.clone(),
                    name: def.name.clone(),
                    agent: CodingAgent {
                        display_name: def.name.clone(),
                        command: def.command.clone(),
                        args: def.args.clone(),
                        resume: coding_agents::ResumeStrategy::None,
                    },
                    tab_id: Some(tab_id),
                    active: true,
                    resume_id: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                });

                let mut tasks: Vec<Task<Message>> = Vec::new();
                if let Some(ref pool) = ws.sparks_db {
                    let pool = pool.clone();
                    let ws_id = ws.workshop_id();
                    let new_session = data::sparks::types::NewAgentSession {
                        id: session_id,
                        workshop_id: ws_id,
                        agent_name: def.name,
                        agent_command: def.command,
                        agent_args: def.args,
                        session_label: None,
                        resume_id: None,
                    };
                    tasks.push(Task::perform(
                        async move {
                            let _ = data::sparks::agent_session_repo::create(&pool, &new_session).await;
                        },
                        |_| Message::AgentSessionSaved,
                    ));
                }
                if let Some(term) = ws.terminals.get(&tab_id) {
                    tasks.push(iced_term::TerminalView::focus(term.widget_id().clone()));
                }
                return Task::batch(tasks);
            }
            // TerminalEvent handled above
            screen::bench::Message::TerminalEvent(_) => {}
        }
        Task::none()
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
                    async move { data::unsplash::search(&api_key, &query, 1).await },
                    |result| match result {
                        Ok(sr) => Message::Background(screen::background_picker::Message::SearchResults(sr.photos)),
                        Err(e) => {
                            log::error!("Unsplash search failed: {e}");
                            Message::Background(screen::background_picker::Message::SearchResults(Vec::new()))
                        }
                    },
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
                                    screen::background_picker::Message::ThumbnailLoaded(id.clone(), bytes),
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
                    async move { data::unsplash::download(&api_key, &photo, &bg_dir).await },
                    move |result| match result {
                        Ok(filename) => Message::UnsplashDownloaded {
                            filename,
                            photographer: photographer.clone(),
                            photographer_url: photographer_url.clone(),
                        },
                        Err(e) => {
                            log::error!("Unsplash download failed: {e}");
                            Message::BackgroundConfigSaved // no-op
                        }
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
                    async move { data::ryve_dir::save_config(&ryve_dir, &config).await.ok(); },
                    |_| Message::BackgroundConfigSaved,
                )
            }

            // ── Agent Settings ───────────────────────────────
            screen::background_picker::Message::SetDefaultAgent(cmd) => {
                self.global_config.default_agent = cmd;
                let config = self.global_config.clone();
                Task::perform(
                    async move { config.save().ok(); },
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
                    async move { config.save().ok(); },
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

        let poll = iced::time::every(std::time::Duration::from_secs(3))
            .map(|_| Message::SparksPoll);

        let hotkeys = keyboard::listen().map(|event| {
            if let keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                modifiers,
                ..
            } = &event
            {
                if modifiers.command() && c.as_str() == "h" {
                    return Message::NewDefaultHand;
                }
            }
            // Swallow unmatched keyboard events — SparksPoll is a harmless no-op
            Message::SparksPoll
        });

        Subscription::batch(
            term_subs
                .into_iter()
                .chain(std::iter::once(poll))
                .chain(std::iter::once(hotkeys)),
        )
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

                // Background picker modal overlay
                if ws.background_picker.open {
                    let has_bg = ws.config.background.image.is_some();
                    let pal = self.appearance.palette();
                    layers.push(
                        screen::background_picker::view(&ws.background_picker, &pal, has_bg)
                            .map(Message::Background),
                    );
                }

                return stack(layers)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into();
            }
        }

        main_content
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

        let sidebar = column![files_panel, agents_panel]
            .width(ws.sidebar_width())
            .height(Length::Fill);

        // -- Center: bench (tabbed area) --
        let bench = self.view_bench(ws, has_bg, &pal);

        // -- Right: sparks panel --
        let sparks_panel = screen::sparks::view(&ws.sparks, &pal, has_bg, &ws.spark_create_form).map(Message::Sparks);

        let sparks_col = container(sparks_panel)
            .width(ws.sparks_width())
            .height(Length::Fill);

        // -- Bottom: status bar --
        let status_bar = screen::status_bar::view(
            ws.file_explorer.branch.as_deref(),
            &ws.directory,
            ws.sparks.len(),
            &pal,
            has_bg,
        )
        .map(Message::StatusBar);

        column![
            row![sidebar, bench, sparks_col].height(Length::Fill),
            status_bar,
        ]
        .height(Length::Fill)
        .into();

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

        // Settings modal overlay
        if ws.background_picker.open {
            let has_bg = ws.config.background.image.is_some();
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
                screen::background_picker::view(&ws.background_picker, &pal, has_bg, agents)
                    .map(Message::Background),
            );
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn view_agents<'a>(&'a self, ws: &'a Workshop, has_bg: bool, pal: &style::Palette) -> Element<'a, Message> {
        screen::agents::view(&ws.agent_sessions, *pal, has_bg).map(Message::Agents)
    }

    fn view_bench<'a>(&'a self, ws: &'a Workshop, has_bg: bool, pal: &style::Palette) -> Element<'a, Message> {
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
                    text("Press + to open a terminal or coding agent").size(14).color(pal.text_secondary),
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
        if let Some(dropdown) = ws.bench.view_dropdown(&self.available_agents, &ws.custom_agents, pal) {
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
