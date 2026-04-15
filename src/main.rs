// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

mod agent_prompts;
mod bundled_tmux;
mod cli;
mod coding_agents;
mod delegation;
mod font_intern;
mod hand_spawn;
mod head;
mod head_archetype;
mod head_archetypes;
mod icons;
mod process_snapshot;
mod release_artifact;
mod screen;
#[allow(dead_code)]
mod sparks_filter;
mod style;
mod tmux;
mod widget;
mod workshop;
mod worktree_cleanup;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use coding_agents::CodingAgent;
use data::sparks::types::{
    Bond, Contract, Ember, EmberType, HandAssignment, NewEmber, PersistedAgentSession, Spark,
};
use iced::widget::{Space, button, column, container, row, stack, text};
use iced::{
    Color, Element, Length, Point, Size, Subscription, Task, Theme, event, keyboard, mouse, window,
};
use process_snapshot::ProcessSnapshot;
use screen::agents::AgentSession;
use screen::toast::{self, Toast, ToastKind};
use screen::{file_explorer, file_viewer, log_tail};
use style::Appearance;
use uuid::Uuid;
use widget::splitter::{self, SplitterDrag, SplitterKind};
use workshop::{SparkPatch, Workshop};

/// Slot the std `UnixListener` returned by `ipc::acquire` waits in until
/// the iced subscription wakes up and takes ownership of it. We can't
/// hand it directly to iced from `main()` because the subscription
/// closure is invoked later, from inside the iced runtime; this static
/// is the bridge. Stored as a `Mutex<Option<_>>` so the subscription can
/// `take()` it exactly once on first run.
#[cfg(unix)]
static IPC_LISTENER_SLOT: std::sync::OnceLock<
    std::sync::Mutex<Option<std::os::unix::net::UnixListener>>,
> = std::sync::OnceLock::new();

#[cfg(unix)]
fn store_ipc_listener(listener: std::os::unix::net::UnixListener) {
    let _ = IPC_LISTENER_SLOT.set(std::sync::Mutex::new(Some(listener)));
}

#[cfg(unix)]
fn take_ipc_listener() -> Option<std::os::unix::net::UnixListener> {
    IPC_LISTENER_SLOT.get().and_then(|m| m.lock().ok()?.take())
}

/// Stream factory for the single-instance accept loop. Pulled out into
/// a free function so it can be passed to `Subscription::run` (which
/// requires a `fn` pointer, not a closure).
///
/// The stream:
/// 1. Takes the std listener out of [`IPC_LISTENER_SLOT`] exactly once.
///    If the slot is empty (no listener registered, e.g. because
///    `ipc::acquire` errored at startup), the stream emits nothing and
///    completes immediately — no harm done.
/// 2. Converts to a tokio listener and accepts forwarded invocations
///    in a loop, emitting `Message::IpcInvocation` for each.
/// 3. Logs and ignores per-connection errors so a single malformed
///    peer cannot kill the listener.
#[cfg(unix)]
fn ipc_subscription_stream() -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::SinkExt;

    Box::pin(iced::stream::channel::<Message>(
        16,
        async move |mut output| {
            let Some(std_listener) = take_ipc_listener() else {
                return;
            };
            let listener = match tokio::net::UnixListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("ryve: failed to register IPC listener with tokio: {e}");
                    return;
                }
            };

            loop {
                match listener.accept().await {
                    Ok((mut stream, _addr)) => match ipc::read_invocation(&mut stream).await {
                        Ok(invocation) => {
                            if output
                                .send(Message::IpcInvocation(invocation))
                                .await
                                .is_err()
                            {
                                // iced runtime has dropped the receiver —
                                // app is shutting down. Stop accepting.
                                return;
                            }
                        }
                        Err(e) => {
                            eprintln!("ryve: malformed IPC invocation: {e}");
                        }
                    },
                    Err(e) => {
                        eprintln!("ryve: IPC accept failed: {e}");
                        // Brief backoff before retrying so a wedged listener
                        // doesn't spin the CPU.
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        },
    ))
}

// Note: the old synchronous `process_is_alive` helper was removed in
// hand/4858031b ([sp-6b19b1d9]) and replaced by the per-tick
// `ProcessSnapshot` shared via `App::last_process_snapshot`.

fn main() -> iced::Result {
    // Dispatch: if the first non-flag arg is a known CLI subcommand,
    // run in CLI mode (tokio runtime). Otherwise launch the UI app.
    let args: Vec<String> = std::env::args().collect();
    let first_non_flag = args
        .iter()
        .skip(1)
        .find(|a| a.as_str() != "--json")
        .map(|s| s.as_str());

    if let Some(cmd) = first_non_flag
        && cli::CLI_COMMANDS.contains(&cmd)
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(cli::run(args));
        return Ok(());
    }

    // Single-instance enforcement: only one ryve UI may run per user.
    // If another instance is already up, forward our cwd+args to it
    // (so it can raise its window and open this directory) and exit.
    // IPC failures are non-fatal — log and start anyway, otherwise a
    // broken socket dir would lock the user out of ryve entirely.
    let invocation = ipc::ForwardedInvocation::from_env();
    match ipc::acquire(&invocation) {
        Ok(ipc::Acquired::Forwarded) => {
            return Ok(());
        }
        #[cfg(unix)]
        Ok(ipc::Acquired::First { listener }) => {
            store_ipc_listener(listener);
        }
        #[cfg(not(unix))]
        Ok(ipc::Acquired::First {}) => {}
        Err(e) => {
            eprintln!(
                "ryve: single-instance check failed ({e}); starting anyway. \
                 Multiple ryve windows may run simultaneously."
            );
        }
    }

    // Load global config for font preferences
    let config = data::config::Config::load();
    let default_font = match config.font_family {
        Some(name) => iced::Font {
            family: iced::font::Family::Name(font_intern::intern(&name)),
            ..iced::Font::DEFAULT
        },
        None => iced::Font {
            family: iced::font::Family::SansSerif,
            ..iced::Font::DEFAULT
        },
    };

    // Window settings — transparent title bar on macOS
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut window = iced::window::Settings {
        size: iced::Size::new(1400.0, 900.0),
        // Floor below which the layout cannot collapse any further. Picked
        // so that even a single pane plus the title bar remains usable.
        // sp-ux0025.
        min_size: Some(iced::Size::new(480.0, 400.0)),
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
    /// Latest process snapshot captured for the running poll. Refreshed at
    /// most once per [`Message::SparksPoll`] tick (off the UI thread via
    /// `tokio::task::spawn_blocking`) and shared by every liveness /
    /// auto-detect check that runs in the same tick. Spark `ryve-a5b9e4a1`.
    last_process_snapshot: Option<Arc<ProcessSnapshot>>,
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
    /// If set, a "close workshop" confirmation dialog is open for the
    /// workshop at this index. Spark sp-ux0021.
    pending_close_workshop: Option<usize>,
}

#[derive(Clone)]
enum Message {
    /// Workshop-level tab bar
    SelectWorkshop(usize),
    CloseWorkshop(usize),
    /// User clicked "Close anyway" in the confirmation dialog. Spark sp-ux0021.
    ConfirmCloseWorkshop(usize),
    /// User dismissed the close-workshop confirmation dialog. Spark sp-ux0021.
    CancelCloseWorkshop,
    NewWorkshopDialog,
    WorkshopDirPicked(Option<PathBuf>),
    /// Open a workshop at a known path (recent-list click, drag-drop, etc.).
    /// Equivalent to picking the directory in a file dialog. If the path
    /// no longer exists, the entry is dropped from the recent list and a
    /// toast is shown.
    OpenWorkshopPath(PathBuf),
    /// A second `ryve` invocation tried to start while this process was
    /// already running and the `ipc` layer forwarded its `(cwd, args)`
    /// over the single-instance socket. The handler raises this window
    /// and, if the cwd looks like a workshop, opens it as a tab.
    IpcInvocation(ipc::ForwardedInvocation),
    /// `workshop::init_workshop` failed (bad db, unreadable config, etc.).
    WorkshopInitFailed {
        id: Uuid,
        error: String,
    },

    /// Workshop .ryve/ initialized. Config + ui_state are boxed to keep
    /// `Message` below the `large_enum_variant` clippy threshold — this
    /// variant was already the fattest, and spark ryve-926870a9 pushed
    /// it over the edge by adding a `UiState` field.
    WorkshopReady {
        id: Uuid,
        pool: sqlx::SqlitePool,
        config: Box<data::ryve_dir::WorkshopConfig>,
        custom_agents: Vec<data::ryve_dir::AgentDef>,
        agent_context: Option<String>,
        agent_context_sync_cache: std::sync::Arc<std::sync::Mutex<data::agent_context::SyncCache>>,
        ui_state: Box<data::ryve_dir::UiState>,
    },
    /// Workgraph sparks loaded from DB
    SparksLoaded(Uuid, Vec<Spark>),
    /// A new spark was just created via the inline "+" form. Carries the
    /// new spark's id (if the write succeeded) and a fresh sparks list.
    /// The handler applies the same bookkeeping as `SparksLoaded` and then
    /// auto-selects the new spark so the detail panel opens for it.
    SparkCreated(Uuid, Option<String>, Vec<Spark>),
    /// Failing/pending required contract count loaded from DB
    FailingContractsLoaded(Uuid, usize),
    /// Failing/pending required contract list loaded from DB (for Home overview)
    FailingContractsListLoaded(Uuid, Vec<Contract>),
    /// Active hand assignments loaded from DB (for Home overview)
    HandAssignmentsLoaded(Uuid, Vec<HandAssignment>),
    CrewsLoaded(
        Uuid,
        Vec<data::sparks::types::Crew>,
        Vec<data::sparks::types::CrewMember>,
    ),
    /// Active embers loaded from DB (for Home overview)
    EmbersLoaded(Uuid, Vec<Ember>),
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
    /// Dead-session reconciliation completed. Carries the session IDs whose
    /// `agent_sessions` rows were ended and `hand_assignments` were
    /// abandoned. Spark `ryve-a677498c`.
    DeadSessionsReconciled(Vec<String>),
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
    LogTail(screen::log_tail::Message),
    Agents(screen::agents::Message),
    Bench(screen::bench::Message),
    Sparks(screen::sparks::Message),
    Home(screen::home::Message),
    SparkDetail(screen::spark_detail::Message),
    SparkPicker(screen::spark_picker::Message),
    HeadPicker(screen::head_picker::Message),
    Releases(screen::releases::Message),
    Background(screen::background_picker::Message),
    StatusBar(screen::status_bar::Message),

    /// Release data loaded from DB (all releases + member epic IDs).
    ReleasesLoaded(
        Uuid,
        Vec<data::sparks::types::Release>,
        Vec<(String, Vec<String>)>,
    ),

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
    /// A `SparksPoll` tick captured a fresh [`ProcessSnapshot`] off the UI
    /// thread. The handler caches it on `App` and then runs the rest of the
    /// poll body — auto-detect, persisted-session reload, log tails, sparks
    /// reload — all reading liveness from this single snapshot. Spark
    /// `ryve-a5b9e4a1`.
    ProcessSnapshotReady(Arc<ProcessSnapshot>),
    /// Inert no-op. Used by the global keyboard subscription for any key
    /// event that does not map to a real hotkey, so unmatched keystrokes
    /// can never accidentally re-trigger an expensive `SparksPoll`.
    /// Spark ryve-5b9c5d93 (perf regression harness).
    Noop,
    /// Periodic backup tick — take a `.backup` snapshot of each open
    /// workshop's sparks.db into `.ryve/backups/`. Also fires once on
    /// graceful workshop close. Spark ryve-7c8573c4.
    BackupTick,
    /// A backup snapshot finished. `Ok(path)` on success so the UI can
    /// log where it landed; `Err(msg)` for failures, which are logged.
    BackupFinished(Result<PathBuf, String>),
    /// Spawn a new Hand with the default agent (Cmd+H)
    NewDefaultHand,
    HandAssignmentSaved,
    /// Shift key state changed (for shift-click line selection).
    ShiftStateChanged(bool),
    /// Cmd+F pressed. The handler routes to the active tab — file
    /// viewer search or terminal search overlay (sp-ux0030).
    HotkeyCmdF,
    /// Escape pressed. Dispatched globally; handlers close any open
    /// search overlay or selection on the active tab.
    HotkeyEscape,
    /// Result of an async `create_hand_worktree` task. Spark ryve-885ed3eb:
    /// Hand terminal spawns are a two-step Task — stage 1 allocates the
    /// tab and stores pending params, stage 2 (this message) finalizes the
    /// `iced_term::Terminal` once the worktree is ready.
    HandWorktreeReady {
        workshop_id: Uuid,
        tab_id: u64,
        result: Result<PathBuf, String>,
    },
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

    /// User interacted with the ember notification bar (dismiss button).
    EmberBar(screen::ember_bar::Message),
    /// Async result from `ember_repo::delete`. The ember row (if any) is
    /// already gone from the DB by the time this lands; we drop it locally
    /// too so the UI reflects the dismiss immediately rather than waiting
    /// for the next 3-second poll. Spark sp-ux0008.
    EmberDismissed {
        workshop_id: Uuid,
        ember_id: String,
    },

    /// Open a URL in the user's default browser. Used by the Unsplash
    /// attribution chip (spark sp-ux0033) to credit the photographer.
    OpenUrl(String),

    /// Apply a field-level edit to a spark. The handler mutates
    /// `ws.sparks` optimistically via `Workshop::apply_spark_patch`, then
    /// dispatches an async `spark_repo::update` that reports back with
    /// [`Message::SparkUpdateApplied`] on success or
    /// [`Message::SparkUpdateFailed`] on error. This is the single write
    /// path every editable field in the detail view is wired through —
    /// spark ryve-90174007.
    //
    // Dead-code allowed: this variant is the shared write path for every
    // field-level edit in the detail-view epic (ryve-82e1102f). Those
    // sparks (ryve-f58d0492, ryve-4742d98b, ryve-99528556, ...) land in
    // sibling branches and will wire view widgets to emit this message.
    // Until they merge there are no in-tree callers, but removing the
    // variant would just force each of them to invent its own duplicate.
    #[allow(dead_code)]
    SparkUpdate {
        workshop_id: Uuid,
        id: String,
        patch: SparkPatch,
    },
    /// Async `spark_repo::update` for spark `id` succeeded. The optimistic
    /// values already applied to `ws.sparks` are now durable; the handler
    /// is a no-op placeholder today (no per-field in-flight map exists in
    /// this branch yet — spark ryve-1d8c2847 introduces `SparkEdit` in a
    /// sibling branch). The message still exists so every field-edit
    /// caller has a symmetric success signal to plug into once the
    /// in-flight map lands.
    SparkUpdateApplied {
        #[allow(dead_code)]
        workshop_id: Uuid,
        #[allow(dead_code)]
        id: String,
        /// The fields that were part of this write, so we only clear those
        /// from `in_flight` rather than wiping all pending requests.
        fields: Vec<screen::spark_detail::Field>,
    },
    /// Async `spark_repo::update` for spark `id` failed. `prior` is the
    /// inverse patch captured at dispatch time; re-applying it restores
    /// `ws.sparks` to the pre-edit state. The handler also pushes an
    /// error toast with the failure reason so the user sees why the write
    /// was rejected. Spark ryve-90174007.
    SparkUpdateFailed {
        workshop_id: Uuid,
        id: String,
        prior: SparkPatch,
        error: String,
        /// The fields that were part of this failed write, so we only
        /// move those from `in_flight` back to `drafts`.
        fields: Vec<screen::spark_detail::Field>,
    },
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelectWorkshop(i) => write!(f, "SelectWorkshop({i})"),
            Self::CloseWorkshop(i) => write!(f, "CloseWorkshop({i})"),
            Self::ConfirmCloseWorkshop(i) => write!(f, "ConfirmCloseWorkshop({i})"),
            Self::CancelCloseWorkshop => write!(f, "CancelCloseWorkshop"),
            Self::NewWorkshopDialog => write!(f, "NewWorkshopDialog"),
            Self::WorkshopDirPicked(p) => write!(f, "WorkshopDirPicked({p:?})"),
            Self::OpenWorkshopPath(p) => write!(f, "OpenWorkshopPath({p:?})"),
            Self::IpcInvocation(inv) => write!(f, "IpcInvocation(cwd={:?})", inv.cwd),
            Self::WorkshopInitFailed { id, error } => {
                write!(f, "WorkshopInitFailed({id}, {error})")
            }
            Self::WorkshopReady { id, .. } => write!(f, "WorkshopReady({id})"),
            Self::SparksLoaded(id, s) => write!(f, "SparksLoaded({id}, {} sparks)", s.len()),
            Self::SparkCreated(id, new_id, s) => {
                write!(f, "SparkCreated({id}, {new_id:?}, {} sparks)", s.len())
            }
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
            Self::DeadSessionsReconciled(ids) => {
                write!(f, "DeadSessionsReconciled({} sessions)", ids.len())
            }
            Self::OpenTabsLoaded(id, t) => {
                write!(f, "OpenTabsLoaded({id}, {} tabs)", t.len())
            }
            Self::OpenTabsSaved => write!(f, "OpenTabsSaved"),
            Self::FilesScanned(id, _) => write!(f, "FilesScanned({id})"),
            Self::FileExplorer(m) => write!(f, "FileExplorer({m:?})"),
            Self::FileViewer(m) => write!(f, "FileViewer({m:?})"),
            Self::LogTail(m) => write!(f, "LogTail({m:?})"),
            Self::Agents(m) => write!(f, "Agents({m:?})"),
            Self::Bench(m) => write!(f, "Bench({m:?})"),
            Self::Sparks(m) => write!(f, "Sparks({m:?})"),
            Self::Home(m) => write!(f, "Home({m:?})"),
            Self::FailingContractsListLoaded(id, c) => {
                write!(f, "FailingContractsListLoaded({id}, {} contracts)", c.len())
            }
            Self::HandAssignmentsLoaded(id, a) => {
                write!(f, "HandAssignmentsLoaded({id}, {} assignments)", a.len())
            }
            Self::CrewsLoaded(id, c, m) => write!(
                f,
                "CrewsLoaded({id}, {} crews, {} memberships)",
                c.len(),
                m.len()
            ),
            Self::EmbersLoaded(id, e) => write!(f, "EmbersLoaded({id}, {} embers)", e.len()),
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
            Self::ProcessSnapshotReady(_) => write!(f, "ProcessSnapshotReady"),
            Self::Noop => write!(f, "Noop"),
            Self::BackupTick => write!(f, "BackupTick"),
            Self::BackupFinished(r) => match r {
                Ok(p) => write!(f, "BackupFinished(ok={})", p.display()),
                Err(e) => write!(f, "BackupFinished(err={e})"),
            },
            Self::NewDefaultHand => write!(f, "NewDefaultHand"),
            Self::HandAssignmentSaved => write!(f, "HandAssignmentSaved"),
            Self::ShiftStateChanged(held) => write!(f, "ShiftStateChanged({held})"),
            Self::HotkeyCmdF => write!(f, "HotkeyCmdF"),
            Self::HotkeyEscape => write!(f, "HotkeyEscape"),
            Self::HandWorktreeReady {
                workshop_id,
                tab_id,
                result,
            } => write!(
                f,
                "HandWorktreeReady({workshop_id}, {tab_id}, ok={})",
                result.is_ok()
            ),
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
            Self::EmberBar(m) => write!(f, "EmberBar({m:?})"),
            Self::EmberDismissed { ember_id, .. } => write!(f, "EmberDismissed({ember_id})"),
            Self::OpenUrl(url) => write!(f, "OpenUrl({url})"),
            Self::SparkUpdate { id, .. } => write!(f, "SparkUpdate({id})"),
            Self::SparkUpdateApplied { id, .. } => write!(f, "SparkUpdateApplied({id})"),
            Self::SparkUpdateFailed { id, error, .. } => {
                write!(f, "SparkUpdateFailed({id}, {error})")
            }
            Self::Releases(m) => write!(f, "Releases({m:?})"),
            Self::ReleasesLoaded(id, _, _) => write!(f, "ReleasesLoaded({id})"),
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
            last_process_snapshot: None,
            shift_held: false,
            splitter_drag: None,
            window_size: Size::new(1400.0, 900.0),
            toasts: Vec::new(),
            next_toast_id: 1,
            pending_close_workshop: None,
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
            tasks.push(app.push_toast(format!("Upgrade {name} CLI"), reason, ToastKind::Warning));
        }

        // Spark ryve-f65077f4: if no compatible coding agent is available,
        // notify the user that Atlas cannot start. The app remains fully
        // functional for all other features — only the Atlas entry point is
        // disabled.
        let has_compatible_agent = app
            .available_agents
            .iter()
            .any(|a| !a.compatibility.is_unsupported());
        if !has_compatible_agent {
            tasks.push(app.push_toast(
                "Atlas unavailable",
                "No compatible coding agent found. Install Claude Code, \
                 Codex, or OpenCode to enable Atlas.",
                ToastKind::Info,
            ));
        }

        (app, Task::batch(tasks))
    }

    fn active_workshop(&self) -> Option<&Workshop> {
        self.active_workshop.and_then(|i| self.workshops.get(i))
    }

    /// Build a Task that takes a final `.backup` snapshot of every open
    /// workshop's sparks.db. Called by [`Self::subscription`] on the
    /// periodic timer and by [`Self::do_close_workshop`] (via
    /// [`Self::snapshot_workshop`]) on graceful shutdown so the user
    /// never loses more than one polling interval of work.
    /// Spark ryve-7c8573c4.
    fn snapshot_all_workshops(&self) -> Task<Message> {
        let tasks: Vec<Task<Message>> = self
            .workshops
            .iter()
            .filter_map(|ws| ws.sparks_db.clone().map(|p| (p, ws.directory.clone())))
            .map(|(pool, dir)| Self::snapshot_task(pool, dir))
            .collect();
        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
        }
    }

    /// Construct a single snapshot Task: take a `.backup` of `pool`
    /// into `dir/.ryve/backups/` and prune to the default retention.
    fn snapshot_task(pool: sqlx::SqlitePool, dir: PathBuf) -> Task<Message> {
        Task::perform(
            async move {
                let ryve_dir = data::ryve_dir::RyveDir::new(&dir);
                data::backup::snapshot_and_retain(
                    &pool,
                    &ryve_dir,
                    data::backup::DEFAULT_BACKUP_RETENTION,
                )
                .await
                .map_err(|e| e.to_string())
            },
            Message::BackupFinished,
        )
    }

    /// Tear down the workshop at `idx` and fix up `active_workshop` so the
    /// tab bar still points at a valid index. Spark sp-ux0021: extracted so
    /// the no-confirm fast path and the confirmed-close path stay in sync.
    ///
    /// Returns a Task that captures a final backup snapshot before the
    /// workshop's pool is dropped. Callers MUST spawn the returned task
    /// (don't discard it) — spark ryve-7c8573c4 requires a snapshot on
    /// graceful close so a crashing post-close write never leaves the
    /// workgraph without a recent backup.
    #[must_use = "the returned Task writes the graceful-shutdown snapshot; spawn it"]
    fn do_close_workshop(&mut self, idx: usize) -> Task<Message> {
        if idx >= self.workshops.len() {
            return Task::none();
        }
        // Snapshot BEFORE removing the workshop so the pool is still
        // live. We clone the pool into the task; dropping the Workshop
        // immediately after is fine because the pool is refcounted.
        let snapshot = self
            .workshops
            .get(idx)
            .and_then(|ws| ws.sparks_db.clone().map(|p| (p, ws.directory.clone())))
            .map(|(pool, dir)| Self::snapshot_task(pool, dir))
            .unwrap_or(Task::none());
        // Kill all Ryve-managed tmux sessions for this workshop so no
        // agent processes survive after the workshop is closed.
        // Spark ryve-c1517114.
        let kill_agents = self
            .workshops
            .get(idx)
            .map(|ws| {
                let ws_dir = ws.directory.clone();
                Task::perform(
                    async move {
                        crate::tmux::kill_all_sessions(&ws_dir).await;
                    },
                    |_| Message::AgentSessionSaved,
                )
            })
            .unwrap_or(Task::none());
        self.workshops.remove(idx);
        if self.workshops.is_empty() {
            self.active_workshop = None;
        } else if let Some(active) = self.active_workshop {
            if active > idx {
                self.active_workshop = Some(active - 1);
            } else if active == idx {
                self.active_workshop = Some(idx.min(self.workshops.len() - 1));
            }
        }
        Task::batch([snapshot, kill_agents])
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
                if let Err(e) =
                    data::sparks::open_tab_repo::save_snapshot(&pool, &workshop_id, &snapshot).await
                {
                    log::warn!("Failed to persist open tabs for {workshop_id}: {e}");
                }
            },
            |_| Message::OpenTabsSaved,
        )
    }

    /// Persist the current draft acceptance-criteria vec for `spark_id`
    /// through `spark_repo::update`, merging the new list into the
    /// existing metadata JSON. Returns a task that reloads sparks so the
    /// in-memory `ws.sparks` mirror the new DB state — which in turn
    /// triggers the reseed guard in `SparksLoaded`, preserving the
    /// invariant that the editor vec matches persisted metadata after a
    /// save. Spark ryve-9b98f949.
    fn persist_acceptance_criteria(&mut self, ws_idx: usize, spark_id: &str) -> Task<Message> {
        let Some(ws) = self.workshops.get(ws_idx) else {
            return Task::none();
        };
        let Some(pool) = ws.sparks_db.clone() else {
            return Task::none();
        };
        let Some(spark) = ws.sparks.iter().find(|s| s.id == spark_id) else {
            return Task::none();
        };
        let new_metadata = screen::spark_detail::merge_acceptance_criteria_into_metadata(
            &spark.metadata,
            &ws.acceptance_criteria_edit.items,
        );
        let spark_id = spark_id.to_string();
        let ws_id = ws.workshop_id();
        let id = ws.id;
        Task::perform(
            async move {
                let upd = data::sparks::types::UpdateSpark {
                    metadata: Some(new_metadata),
                    ..Default::default()
                };
                if let Err(e) =
                    data::sparks::spark_repo::update(&pool, &spark_id, upd, "user").await
                {
                    return Err(e.to_string());
                }
                Ok(load_sparks(pool, ws_id).await)
            },
            move |res: Result<Vec<_>, String>| match res {
                Ok(sparks) => Message::SparksLoaded(id, sparks),
                Err(error) => Message::ShowToast {
                    title: "Could not save acceptance criteria".into(),
                    body: error,
                    kind: ToastKind::Error,
                },
            },
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
                // Spark sp-ux0021: if any Hands are still active, prompt the
                // user before tearing down the workshop instead of killing
                // their terminals/agents instantly.
                if let Some(ws) = self.workshops.get(idx) {
                    let active_hands = ws.agent_sessions.iter().filter(|s| s.active).count();
                    if active_hands > 0 {
                        self.pending_close_workshop = Some(idx);
                        return Task::none();
                    }
                }
                self.do_close_workshop(idx)
            }
            Message::ConfirmCloseWorkshop(idx) => {
                self.pending_close_workshop = None;
                self.do_close_workshop(idx)
            }
            Message::CancelCloseWorkshop => {
                self.pending_close_workshop = None;
                Task::none()
            }
            Message::NewWorkshopDialog => Task::perform(pick_workshop_directory(), |path| {
                Message::WorkshopDirPicked(path)
            }),
            Message::WorkshopDirPicked(Some(path)) => self.update(Message::OpenWorkshopPath(path)),

            Message::IpcInvocation(inv) => {
                // A second `ryve` invocation forwarded its working
                // directory to us via the single-instance socket. Raise
                // the window so the user knows we noticed, and if the
                // forwarded cwd looks like a workshop (existing
                // directory) hand it to OpenWorkshopPath — that handler
                // already deduplicates against open tabs and prunes
                // stale entries.
                let focus = window::oldest().and_then(window::gain_focus);
                if inv.cwd.is_dir() {
                    let open = self.update(Message::OpenWorkshopPath(inv.cwd));
                    return Task::batch([focus, open]);
                }
                focus
            }
            Message::WorkshopDirPicked(None) => Task::none(),
            Message::OpenWorkshopPath(path) => {
                // If the user clicks a stale recent entry, surface a toast
                // and prune the dead path so the welcome list doesn't keep
                // dangling references.
                if !path.is_dir() {
                    self.global_config.remove_recent_workshop(&path);
                    let cfg = self.global_config.clone();
                    std::thread::spawn(move || {
                        let _ = cfg.save();
                    });
                    return self.push_toast(
                        "Workshop not found",
                        format!("Path no longer exists: {}", path.display()),
                        ToastKind::Error,
                    );
                }

                // If this workshop is already open, just focus it instead
                // of duplicating the tab.
                if let Some(existing) = self.workshops.iter().position(|ws| ws.directory == path) {
                    self.active_workshop = Some(existing);
                    return Task::none();
                }

                // Record as most-recently opened.
                self.global_config.add_recent_workshop(path.clone());
                let cfg = self.global_config.clone();
                std::thread::spawn(move || {
                    let _ = cfg.save();
                });

                let mut workshop = Workshop::new(path.clone());
                workshop.set_appearance(self.appearance);
                // Inherit the user's currently-effective terminal font
                // settings so the very first terminal in the new workshop
                // already respects the global preference. Spark sp-ux0014.
                workshop.terminal_font_size = self.global_config.effective_terminal_font_size();
                workshop.terminal_font_family = self.global_config.terminal_font_family.clone();
                let ws_id = workshop.id;
                self.workshops.push(workshop);
                let idx = self.workshops.len() - 1;
                self.active_workshop = Some(idx);

                // Async: init .ryve/ dir, DB, config, agents, context
                Task::perform(workshop::init_workshop(path), move |result| match result {
                    Ok(init) => Message::WorkshopReady {
                        id: ws_id,
                        pool: init.pool,
                        config: Box::new(init.config),
                        custom_agents: init.custom_agents,
                        agent_context: init.agent_context,
                        agent_context_sync_cache: init.agent_context_sync_cache,
                        ui_state: Box::new(init.ui_state),
                    },
                    Err(e) => Message::WorkshopInitFailed {
                        id: ws_id,
                        error: e.to_string(),
                    },
                })
            }
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
                agent_context_sync_cache,
                ui_state,
            } => {
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };
                if let Some(ws) = self.workshops.get_mut(idx) {
                    ws.sparks_db = Some(pool.clone());
                    ws.config = *config;
                    ws.custom_agents = custom_agents;
                    ws.agent_context = agent_context;
                    // Apply persisted UI state before the first render so
                    // the sparks panel honours the user's last collapse
                    // choices. Stale IDs are pruned once sparks finish
                    // loading (see SparksLoaded below). Spark ryve-926870a9.
                    ws.collapsed_epics = ui_state.collapsed_epics.clone();
                    // Rehydrate the persisted sparks-panel filter so the
                    // user returns to the same view. Spark ryve-27e33825.
                    ws.sparks_filter = crate::screen::sparks::SparksFilter::from_persisted(
                        &ui_state.sparks_filter,
                    );
                    ws.sort_mode = crate::screen::sparks::SortMode::from_persist_key(
                        &ui_state.sparks_filter.sort_mode,
                    );
                    // Hand off the warm hash cache from init_workshop so the
                    // first SparksLoaded sync tick is a no-op on disk.
                    // Spark ryve-86b0b326.
                    ws.agent_context_sync_cache = agent_context_sync_cache;

                    // Reconcile tmux sessions before loading agent sessions
                    // so any stale DB rows are marked ended before the UI
                    // ever sees them. Spark [sp-0285181c].
                    let ws_id = ws.workshop_id();
                    let dir = ws.directory.clone();
                    let pool2 = pool.clone();
                    let ws_id2 = ws_id.clone();
                    let pool_rec = pool.clone();
                    let ws_id_rec = ws_id.clone();
                    let dir_rec = dir.clone();
                    let reconcile_then_sessions = Task::perform(
                        async move {
                            tmux::reconcile_sessions(&dir_rec, &pool_rec, &ws_id_rec).await;
                            load_agent_sessions(pool2, ws_id2).await
                        },
                        move |sessions| Message::AgentSessionsLoaded(id, sessions),
                    );

                    // Load sparks + (reconcile → agent sessions) + scan file tree in parallel
                    let sparks_task = Task::perform(load_sparks(pool, ws_id), move |sparks| {
                        Message::SparksLoaded(id, sparks)
                    });
                    // NOTE: open_tabs is NOT loaded here. It is dispatched
                    // from the `AgentSessionsLoaded` handler so that any
                    // persisted `coding_agent` / `log_tail` tab can be
                    // resolved against the freshly-populated `agent_sessions`
                    // vec when it is restored.
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

                    return Task::batch([sparks_task, reconcile_then_sessions, scan_task, bg_task]);
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
                    // Detect sparks that transitioned into the `blocked`
                    // status since the last poll and fire a Flash ember
                    // for each one. Spark sp-ux0008.
                    let mut ember_tasks: Vec<Task<Message>> = Vec::new();
                    let current_blocked: HashSet<String> = sparks
                        .iter()
                        .filter(|s| s.status == "blocked")
                        .map(|s| s.id.clone())
                        .collect();
                    if ws.sparks_baseline_seen
                        && let Some(ref pool) = ws.sparks_db
                    {
                        let ws_id_str = ws.workshop_id();
                        for sp in sparks.iter().filter(|s| s.status == "blocked") {
                            if !ws.prev_blocked_spark_ids.contains(&sp.id) {
                                let pool = pool.clone();
                                let ws_id_str = ws_id_str.clone();
                                let content = format!("Spark {} blocked: {}", sp.id, sp.title);
                                ember_tasks.push(Task::perform(
                                    create_ember_fire_and_forget(
                                        pool,
                                        ws_id_str,
                                        EmberType::Flash,
                                        content,
                                        Some("workgraph".to_string()),
                                    ),
                                    |_| Message::AgentContextSynced,
                                ));
                            }
                        }
                    }
                    ws.prev_blocked_spark_ids = current_blocked;
                    ws.sparks_baseline_seen = true;
                    // Replace (not append) so Refresh never duplicates
                    // entries. Invariant from spark ryve-7805b38b.
                    ws.sparks = sparks;
                    // Re-sort according to the active sort mode.
                    // Spark ryve-6f24ef2a.
                    ws.sort_sparks();
                    ws.recompute_filtered_sparks();
                    // Clear the Refresh-button indicator now that the
                    // refetch has landed. Both the explicit Refresh and
                    // the 3s poll route through this handler; clearing
                    // when the flag was already false is a no-op.
                    ws.sparks_refreshing = false;

                    // Silently drop any collapsed-epic IDs whose epic no
                    // longer exists (deleted between runs, or already gone
                    // when the stored set was first loaded). If anything
                    // was pruned, write the cleaned snapshot back so the
                    // on-disk file doesn't keep growing with dead IDs.
                    // Spark ryve-926870a9.
                    let live_epic_ids = Workshop::live_epic_ids(&ws.sparks);
                    if ws.prune_collapsed_epics(&live_epic_ids) {
                        let ryve_dir = ws.ryve_dir.clone();
                        let snapshot = ws.ui_state_snapshot();
                        tokio::spawn(async move {
                            if let Err(e) =
                                data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                            {
                                log::warn!("failed to save .ryve/ui_state.json: {e}");
                            }
                        });
                    }

                    // Only reseed the intent-list drafts if the selected
                    // spark *changed*; otherwise a mid-type keystroke
                    // would get clobbered on every 3-second poll. Spark
                    // ryve-212c63aa.
                    ws.reseed_intent_drafts_if_selection_changed();

                    // Reseed the acceptance-criteria editor from the freshly
                    // loaded spark so the invariant "in-memory vec == persisted
                    // metadata.intent.acceptance_criteria after a save" holds
                    // automatically across every load path (spark ryve-9b98f949).
                    if let Some(ref selected_id) = ws.selected_spark
                        && ws.acceptance_criteria_edit.is_for(selected_id)
                        && let Some(spark) = ws.sparks.iter().find(|s| s.id == *selected_id)
                    {
                        let persisted = spark.intent().acceptance_criteria;
                        let draft_trimmed: Vec<String> = ws
                            .acceptance_criteria_edit
                            .items
                            .iter()
                            .filter(|s| !s.trim().is_empty())
                            .cloned()
                            .collect();
                        if draft_trimmed == persisted {
                            ws.acceptance_criteria_edit =
                                screen::spark_detail::AcceptanceCriteriaEdit::load(spark);
                        }
                    }

                    // Refresh failing contract count + blocked-spark set +
                    // Home dashboard sources (failing contract list, active
                    // hand assignments, active embers) alongside sparks so
                    // the status bar, per-row blocked indicator, and Home
                    // dashboard all stay in sync with the workgraph panel —
                    // there is no separate Home poll.
                    let mut tasks: Vec<Task<Message>> = ember_tasks;
                    if let Some(ref pool) = ws.sparks_db {
                        let ws_id = ws.workshop_id();
                        tasks.push(Task::perform(
                            load_failing_contract_count(pool.clone(), ws_id.clone()),
                            move |n| Message::FailingContractsLoaded(id, n),
                        ));
                        tasks.push(Task::perform(
                            load_blocked_spark_ids(pool.clone(), ws_id.clone()),
                            move |ids| Message::BlockedSparkIdsLoaded(id, ids),
                        ));
                        tasks.push(Task::perform(
                            load_failing_contract_list(pool.clone(), ws_id.clone()),
                            move |list| Message::FailingContractsListLoaded(id, list),
                        ));
                        tasks.push(Task::perform(
                            load_hand_assignments(pool.clone(), ws_id.clone()),
                            move |list| Message::HandAssignmentsLoaded(id, list),
                        ));
                        tasks.push(Task::perform(
                            load_crews(pool.clone(), ws_id.clone()),
                            move |(crews, members)| Message::CrewsLoaded(id, crews, members),
                        ));
                        tasks.push(Task::perform(
                            load_embers(pool.clone(), ws_id),
                            move |list| Message::EmbersLoaded(id, list),
                        ));
                    }

                    // Sync .ryve/WORKSHOP.md and pointers (including into worktrees).
                    // The shared `agent_context_sync_cache` lets repeated calls
                    // skip writes when nothing on disk has changed — without it,
                    // ~25 file writes fired every 3s on a 5-worktree workshop.
                    // Spark ryve-86b0b326.
                    if !ws.config.agents.disable_sync {
                        let dir = ws.directory.clone();
                        let ryve_dir = ws.ryve_dir.clone();
                        let config = ws.config.clone();
                        let cache = ws.agent_context_sync_cache.clone();
                        tasks.push(Task::perform(
                            async move {
                                let _ = data::agent_context::sync(&dir, &ryve_dir, &config, &cache)
                                    .await;
                            },
                            |_| Message::AgentContextSynced,
                        ));
                    }

                    return Task::batch(tasks);
                }
                Task::none()
            }
            Message::SparkCreated(id, new_id, sparks) => {
                // First, drive the normal post-load bookkeeping (blocked
                // set, embers, baseline flags, etc.) by forwarding to the
                // standard SparksLoaded handler. Then, if the create
                // succeeded, select the new spark so the detail panel
                // focuses it immediately — this is the "new spark is
                // selected" half of the acceptance criterion. Epic
                // grouping and collapse state are available, so the
                // selection handler can expand the parent group for free.
                let load_task = self.update(Message::SparksLoaded(id, sparks));
                let select_task = if let Some(new_id) = new_id {
                    self.update(Message::Sparks(screen::sparks::Message::SelectSpark(
                        new_id,
                    )))
                } else {
                    Task::none()
                };
                Task::batch([load_task, select_task])
            }
            Message::FailingContractsLoaded(id, count) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    ws.failing_contracts = count;
                }
                Task::none()
            }
            Message::FailingContractsListLoaded(id, list) => {
                let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) else {
                    return Task::none();
                };
                // Fire a Flare ember for any contract that is newly in the
                // failing set since the last poll. Spark sp-ux0008.
                let mut ember_tasks: Vec<Task<Message>> = Vec::new();
                let current_ids: HashSet<i64> = list.iter().map(|c| c.id).collect();
                if ws.contracts_baseline_seen
                    && let Some(ref pool) = ws.sparks_db
                {
                    let ws_id_str = ws.workshop_id();
                    for c in &list {
                        if !ws.prev_failing_contract_ids.contains(&c.id) {
                            let pool = pool.clone();
                            let ws_id_str = ws_id_str.clone();
                            let content =
                                format!("Contract failed on {}: {}", c.spark_id, c.description);
                            ember_tasks.push(Task::perform(
                                create_ember_fire_and_forget(
                                    pool,
                                    ws_id_str,
                                    EmberType::Flare,
                                    content,
                                    Some("contracts".to_string()),
                                ),
                                |_| Message::AgentContextSynced,
                            ));
                        }
                    }
                }
                ws.prev_failing_contract_ids = current_ids;
                ws.contracts_baseline_seen = true;
                ws.failing_contracts_list = list;
                Task::batch(ember_tasks)
            }
            Message::HandAssignmentsLoaded(id, list) => {
                let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) else {
                    return Task::none();
                };
                // Fire a Glow ember for any assignment that was active at
                // the previous poll but is no longer active — i.e. the
                // Hand finished its spark. Spark sp-ux0008.
                let mut ember_tasks: Vec<Task<Message>> = Vec::new();
                let current_active_ids: HashSet<i64> = list.iter().map(|a| a.id).collect();
                if ws.assignments_baseline_seen
                    && let Some(ref pool) = ws.sparks_db
                {
                    let ws_id_str = ws.workshop_id();
                    // Anything in `prev_active_assignment_ids` that is no
                    // longer in `current_active_ids` transitioned out of
                    // the active set — that's a Hand finish.
                    for prev_id in &ws.prev_active_assignment_ids {
                        if !current_active_ids.contains(prev_id) {
                            let pool = pool.clone();
                            let ws_id_str = ws_id_str.clone();
                            let content = format!("Hand finished (assignment #{prev_id})");
                            ember_tasks.push(Task::perform(
                                create_ember_fire_and_forget(
                                    pool,
                                    ws_id_str,
                                    EmberType::Glow,
                                    content,
                                    Some("hands".to_string()),
                                ),
                                |_| Message::AgentContextSynced,
                            ));
                        }
                    }
                }
                ws.prev_active_assignment_ids = current_active_ids;
                ws.assignments_baseline_seen = true;
                ws.hand_assignments = list;
                Task::batch(ember_tasks)
            }
            Message::EmbersLoaded(id, list) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    ws.embers = list;
                }
                Task::none()
            }
            Message::Home(home_msg) => self.handle_home_message(home_msg),
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
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id)
                    && ws.selected_spark.as_deref() == Some(spark_id.as_str())
                {
                    ws.selected_spark_bonds = bonds;
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
                let mut chain_tasks: Vec<Task<Message>> = Vec::new();
                // Read liveness from the cached snapshot the current poll
                // captured. At workshop init time (the very first
                // `AgentSessionsLoaded`) there is no snapshot yet — every
                // PID falls back to "not alive", and the next 3-second
                // poll re-classifies them. Spark `ryve-a5b9e4a1`.
                let snapshot = self.last_process_snapshot.clone();

                // Spark `ryve-a677498c`: collect sessions whose process has
                // died but whose DB row is still active (ended_at IS NULL).
                // These need reconciliation: end the session + abandon
                // assignments so the Activity panel reflects reality.
                let mut dead_session_ids: Vec<String> = Vec::new();

                if let Some(ws) = self.workshops.get_mut(idx) {
                    let available = &self.available_agents;
                    let known_ids: std::collections::HashSet<String> =
                        ws.agent_sessions.iter().map(|s| s.id.clone()).collect();

                    // Cache the set of live tmux session names once per
                    // poll so we avoid shelling out per-session.
                    // Spark: copilot review comment #10.
                    let live_tmux_names: std::collections::HashSet<String> = {
                        let live = tmux::list_sessions_sync(&ws.directory);
                        live.into_iter().map(|s| s.name).collect()
                    };

                    // Use the tmux wrapper to diff tracked sessions against
                    // the process snapshot, collecting session IDs to feed
                    // into the per-session classification below.
                    let tracked: Vec<(String, Option<i64>)> = persisted
                        .iter()
                        .filter(|p| p.ended_at.is_none())
                        .map(|p| (p.id.clone(), p.child_pid))
                        .collect();
                    let dead_set: std::collections::HashSet<String> =
                        if let Some(ref snap) = snapshot {
                            tmux::dead_sessions(&tracked, snap).into_iter().collect()
                        } else {
                            std::collections::HashSet::new()
                        };

                    for p in persisted {
                        let existing_tab_id = ws
                            .agent_sessions
                            .iter()
                            .find(|s| s.id == p.id)
                            .and_then(|s| s.tab_id);
                        let child_alive = match (snapshot.as_ref(), p.child_pid) {
                            (Some(snap), Some(pid)) => snap.is_alive(pid),
                            _ => false,
                        };
                        // Tmux-managed sessions (hand/head/merger) don't have
                        // a child_pid — their liveness is determined by tmux
                        // reconciliation which runs before this load. If the
                        // DB row is still active after reconciliation, the
                        // tmux session exists. Spark [sp-0285181c].
                        let tmux_alive = p.ended_at.is_none()
                            && p.child_pid.is_none()
                            && matches!(
                                p.session_label.as_deref(),
                                Some("hand") | Some("head") | Some("merger")
                            );
                        let display_state = screen::agents::classify_session(
                            p.ended_at.is_some(),
                            existing_tab_id.is_some(),
                            child_alive || tmux_alive,
                        );

                        // Spark `ryve-a677498c`: if the DB row is still
                        // active but the process is gone (and no live
                        // terminal tab), queue it for reconciliation.
                        if p.ended_at.is_none()
                            && dead_set.contains(&p.id)
                            && existing_tab_id.is_none()
                        {
                            dead_session_ids.push(p.id.clone());
                        }

                        if known_ids.contains(&p.id) {
                            // Already in memory — preserve tab_id, but refresh liveness.
                            if let Some(s) = ws.agent_sessions.iter_mut().find(|s| s.id == p.id) {
                                s.active =
                                    display_state == screen::agents::SessionDisplayState::Active;
                                s.stale =
                                    display_state == screen::agents::SessionDisplayState::Stale;
                                // Refresh tmux liveness for the Attach button.
                                // Spark ryve-8ba40d83.
                                let label = s.session_label.as_deref().unwrap_or("hand");
                                let tmux_name = tmux::session_name_for(label, &s.id);
                                s.tmux_session_live =
                                    s.active && live_tmux_names.contains(&tmux_name);
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
                        let is_active =
                            display_state == screen::agents::SessionDisplayState::Active;
                        // Check tmux liveness for the Attach button.
                        // Spark ryve-8ba40d83.
                        let tmux_live = if is_active {
                            let label = p.session_label.as_deref().unwrap_or("hand");
                            let tmux_name = tmux::session_name_for(label, &p.id);
                            live_tmux_names.contains(&tmux_name)
                        } else {
                            false
                        };
                        ws.agent_sessions.push(AgentSession {
                            id: p.id,
                            name: p.agent_name,
                            agent,
                            tab_id: existing_tab_id,
                            active: is_active,
                            stale: display_state == screen::agents::SessionDisplayState::Stale,
                            resume_id: p.resume_id,
                            started_at: p.started_at,
                            log_path: p.log_path.map(PathBuf::from),
                            last_output_at: None,
                            parent_session_id: p.parent_session_id,
                            session_label: p.session_label.clone(),
                            tmux_session_live: tmux_live,
                        });
                    }

                    // Refresh cached agent session names for the filter bar
                    // (spark ryve-baca34b0).
                    ws.agent_session_names =
                        ws.agent_sessions.iter().map(|s| s.name.clone()).collect();

                    // First time we see agent_sessions for this workshop:
                    // chain into load_open_tabs so the persisted snapshot
                    // can resolve `coding_agent` / `log_tail` tabs against
                    // the just-populated session vec.
                    if !ws.tabs_restored
                        && let Some(ref pool) = ws.sparks_db
                    {
                        let pool = pool.clone();
                        let ws_id = ws.workshop_id();
                        let id_copy = id;
                        chain_tasks.push(Task::perform(load_open_tabs(pool, ws_id), move |tabs| {
                            Message::OpenTabsLoaded(id_copy, tabs)
                        }));
                    }

                    // Spark `ryve-a677498c`: fire off async reconciliation
                    // for sessions whose process disappeared. This ends
                    // their agent_sessions row and abandons their active
                    // hand_assignments so the next poll classifies them as
                    // History instead of Stale.
                    if !dead_session_ids.is_empty()
                        && let Some(ref pool) = ws.sparks_db
                    {
                        let pool = pool.clone();
                        let ids = dead_session_ids;
                        let ws_dir = ws.directory.clone();
                        chain_tasks.push(Task::perform(
                            reconcile_dead_sessions(pool, ws_dir, ids),
                            Message::DeadSessionsReconciled,
                        ));
                    }
                }
                if chain_tasks.is_empty() {
                    Task::none()
                } else {
                    Task::batch(chain_tasks)
                }
            }

            Message::CrewsLoaded(id, crews, members) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    ws.crews = crews;
                    ws.crew_members = members;
                }
                Task::none()
            }

            Message::AgentSessionSaved => Task::none(),

            Message::DeadSessionsReconciled(session_ids) => {
                // The DB rows are already updated — the next poll cycle's
                // `AgentSessionsLoaded` will reclassify these sessions as
                // History (ended_at is now set). Log for observability.
                if !session_ids.is_empty() {
                    log::info!(
                        "Reconciled {} dead session(s): {:?}",
                        session_ids.len(),
                        session_ids
                    );
                }
                Task::none()
            }

            Message::OpenTabsLoaded(id, persisted) => {
                // Replay the persisted snapshot against the bench. Tabs are
                // restored in the original left-to-right order. Tabs whose
                // backing state has gone (file deleted, agent session ended)
                // are silently dropped — restoring them would either pop
                // failure toasts or surface tabs the user can't usefully
                // interact with.
                let ws_idx = self.workshops.iter().position(|ws| ws.id == id);
                let Some(idx) = ws_idx else {
                    return Task::none();
                };

                let mut follow_up: Vec<Task<Message>> = Vec::new();
                for tab in persisted {
                    match tab.tab_kind.as_str() {
                        "terminal" => {
                            let next_id = &mut self.next_terminal_id;
                            self.workshops[idx].spawn_plain_terminal(tab.title, next_id);
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
                        "coding_agent" => {
                            // Per product decision: ended sessions are NOT
                            // restorable. Only revive the tab if the session
                            // is still active in the loaded session vec.
                            let Some(session_id) = tab.payload else {
                                continue;
                            };
                            let ws = &mut self.workshops[idx];
                            let session = ws
                                .agent_sessions
                                .iter()
                                .find(|s| s.id == session_id)
                                .cloned();
                            let Some(session) = session else { continue };
                            if !session.active {
                                continue;
                            }
                            // Reuse the resume flow: build a fresh terminal
                            // tab using the agent's --resume command. If the
                            // agent has no resume strategy we drop the tab
                            // (we can't safely re-attach to the old PTY).
                            let Some((cmd, args)) =
                                session.agent.resume_args(session.resume_id.as_deref())
                            else {
                                continue;
                            };
                            let resume_agent = CodingAgent {
                                display_name: session.agent.display_name.clone(),
                                command: cmd,
                                args,
                                resume: session.agent.resume.clone(),
                                compatibility: session.agent.compatibility.clone(),
                            };
                            let full_auto = self
                                .global_config
                                .agent_settings
                                .get(&resume_agent.command)
                                .is_some_and(|s| s.full_auto);
                            let next_id = &mut self.next_terminal_id;
                            let is_atlas = session.session_label.as_deref() == Some("atlas");
                            let tab_id = if is_atlas {
                                ws.begin_atlas_terminal(
                                    session.name.clone(),
                                    workshop::PendingTerminalKind::Agent(resume_agent.clone()),
                                    next_id,
                                    session_id.clone(),
                                    full_auto,
                                )
                            } else {
                                ws.begin_hand_terminal(
                                    session.name.clone(),
                                    workshop::PendingTerminalKind::Agent(resume_agent.clone()),
                                    next_id,
                                    session_id.clone(),
                                    full_auto,
                                )
                            };
                            follow_up.push(Self::dispatch_worktree_task(
                                ws,
                                tab_id,
                                session_id.clone(),
                            ));
                            if let Some(s) =
                                ws.agent_sessions.iter_mut().find(|s| s.id == session_id)
                            {
                                s.tab_id = Some(tab_id);
                            }
                        }
                        "log_tail" => {
                            // Spy view for a background Hand. Only restore
                            // if the session is still active and has a log
                            // path on disk.
                            let Some(session_id) = tab.payload else {
                                continue;
                            };
                            let ws = &mut self.workshops[idx];
                            let log_path = ws
                                .agent_sessions
                                .iter()
                                .find(|s| s.id == session_id && s.active)
                                .and_then(|s| s.log_path.clone());
                            let Some(log_path) = log_path else { continue };
                            if !log_path.exists() {
                                continue;
                            }
                            let (tab_id, is_new) = ws.open_log_tab(
                                &session_id,
                                log_path.clone(),
                                &mut self.next_terminal_id,
                            );
                            if is_new {
                                follow_up.push(Task::perform(
                                    log_tail::load_tail(tab_id, log_path),
                                    Message::LogTail,
                                ));
                            }
                        }
                        other => {
                            log::warn!("Unknown persisted tab kind: {other}");
                        }
                    }
                }

                // Mark this workshop as restored so we don't replay tabs
                // again on the next AgentSessionsLoaded (which fires every
                // SparksPoll tick).
                if let Some(ws) = self.workshops.get_mut(idx) {
                    ws.tabs_restored = true;
                }

                // Spark ryve-fa0f8f93 — Auto-spawn a pinned Atlas tab as
                // the leftmost tab if no active Atlas session already
                // exists and at least one coding agent is available.
                let has_atlas = self.workshops[idx]
                    .agent_sessions
                    .iter()
                    .any(|s| s.session_label.as_deref() == Some("atlas") && s.active);
                let atlas_agent = if !has_atlas {
                    let config_pref = self.workshops[idx].config.atlas_agent.as_deref();
                    coding_agents::resolve_atlas_agent(config_pref, &self.available_agents)
                } else {
                    None
                };
                if let Some(agent) = atlas_agent {
                    let atlas_task = self.spawn_atlas_pinned(idx, agent);
                    follow_up.push(atlas_task);
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
                if let Some(ws) = self.workshops.get_mut(idx)
                    && let file_explorer::Message::TreeLoaded(tree, statuses, diff_stats, branch) =
                        msg
                {
                    ws.file_explorer.tree = tree;
                    ws.file_explorer.git_statuses = statuses;
                    ws.file_explorer.diff_stats = diff_stats;
                    ws.file_explorer.branch = branch;
                    // Start collapsed — user expands directories on demand
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
                        if let Some(ref pool) = ws.sparks_db
                            && let Some(spark) = ws.sparks.first()
                        {
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
                                    let _ =
                                        data::sparks::file_link_repo::create(&pool, &link).await;
                                },
                                |_| Message::Sparks(screen::sparks::Message::Refresh),
                            );
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
                            if let Some(active_id) = ws.bench.active_tab
                                && let Some(viewer) = ws.file_viewers.get_mut(&active_id)
                            {
                                viewer.scroll_offset = offset_y;
                                viewer.viewport_height = viewport_height;
                            }
                        }
                    }
                    file_viewer::Message::ClickLine(idx) => {
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &mut self.workshops[ws_idx];
                            if let Some(active_id) = ws.bench.active_tab
                                && let Some(viewer) = ws.file_viewers.get_mut(&active_id)
                            {
                                if self.shift_held {
                                    viewer.selection_end = Some(idx);
                                } else {
                                    viewer.selection_anchor = Some(idx);
                                    viewer.selection_end = Some(idx);
                                }
                            }
                        }
                    }
                    file_viewer::Message::CopySelection => {
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &self.workshops[ws_idx];
                            if let Some(active_id) = ws.bench.active_tab
                                && let Some(viewer) = ws.file_viewers.get(&active_id)
                                && let Some(selected) = viewer.selected_text()
                                && let Ok(mut clip) = arboard::Clipboard::new()
                            {
                                let _ = clip.set_text(selected);
                            }
                        }
                    }
                    file_viewer::Message::ClearSelection => {
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &mut self.workshops[ws_idx];
                            if let Some(active_id) = ws.bench.active_tab
                                && let Some(viewer) = ws.file_viewers.get_mut(&active_id)
                            {
                                // Escape closes search first if it's open;
                                // otherwise it clears the line selection.
                                if viewer.search_open {
                                    viewer.close_search();
                                } else {
                                    viewer.clear_selection();
                                }
                            }
                        }
                    }
                    file_viewer::Message::OpenSearch => {
                        if let Some(ws_idx) = self.active_workshop
                            && let Some(active_id) = self.workshops[ws_idx].bench.active_tab
                            && let Some(viewer) =
                                self.workshops[ws_idx].file_viewers.get_mut(&active_id)
                        {
                            viewer.open_search();
                            return iced::widget::operation::focus(iced::widget::Id::new(
                                file_viewer::SEARCH_INPUT_ID,
                            ));
                        }
                    }
                    file_viewer::Message::CloseSearch => {
                        if let Some(ws_idx) = self.active_workshop
                            && let Some(active_id) = self.workshops[ws_idx].bench.active_tab
                            && let Some(viewer) =
                                self.workshops[ws_idx].file_viewers.get_mut(&active_id)
                        {
                            viewer.close_search();
                        }
                    }
                    file_viewer::Message::SearchQueryChanged(q) => {
                        if let Some(ws_idx) = self.active_workshop
                            && let Some(active_id) = self.workshops[ws_idx].bench.active_tab
                            && let Some(viewer) =
                                self.workshops[ws_idx].file_viewers.get_mut(&active_id)
                        {
                            viewer.set_search_query(q);
                            if let Some(target) = viewer.scroll_offset_for_current_match() {
                                return iced::widget::operation::scroll_to(
                                    iced::widget::Id::new(file_viewer::SCROLLABLE_ID),
                                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: target },
                                );
                            }
                        }
                    }
                    file_viewer::Message::SearchNext => {
                        if let Some(ws_idx) = self.active_workshop
                            && let Some(active_id) = self.workshops[ws_idx].bench.active_tab
                            && let Some(viewer) =
                                self.workshops[ws_idx].file_viewers.get_mut(&active_id)
                        {
                            viewer.next_match();
                            if let Some(target) = viewer.scroll_offset_for_current_match() {
                                return iced::widget::operation::scroll_to(
                                    iced::widget::Id::new(file_viewer::SCROLLABLE_ID),
                                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: target },
                                );
                            }
                        }
                    }
                    file_viewer::Message::SearchPrev => {
                        if let Some(ws_idx) = self.active_workshop
                            && let Some(active_id) = self.workshops[ws_idx].bench.active_tab
                            && let Some(viewer) =
                                self.workshops[ws_idx].file_viewers.get_mut(&active_id)
                        {
                            viewer.prev_match();
                            if let Some(target) = viewer.scroll_offset_for_current_match() {
                                return iced::widget::operation::scroll_to(
                                    iced::widget::Id::new(file_viewer::SCROLLABLE_ID),
                                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: target },
                                );
                            }
                        }
                    }
                    file_viewer::Message::NavigateToDir(target) => {
                        // Reveal the target directory in the file explorer:
                        // expand every ancestor between the workshop root
                        // and the target (inclusive) and select the target.
                        // Clicking the workshop-root segment just clears
                        // the selection — there's nothing to expand above it.
                        if let Some(ws_idx) = self.active_workshop {
                            let ws = &mut self.workshops[ws_idx];
                            if target == ws.directory {
                                ws.file_explorer.selected = None;
                            } else if target.starts_with(&ws.directory) {
                                let mut cur: Option<&Path> = Some(target.as_path());
                                while let Some(p) = cur {
                                    if p == ws.directory.as_path() {
                                        break;
                                    }
                                    ws.file_explorer.expanded.insert(p.to_path_buf());
                                    cur = p.parent();
                                }
                                ws.file_explorer.selected = Some(target);
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
            Message::LogTail(msg) => {
                match msg {
                    log_tail::Message::Loaded {
                        tab_id,
                        path,
                        content,
                    } => {
                        for ws in &mut self.workshops {
                            if let Some(tail) = ws.log_tails.get_mut(&tab_id) {
                                if tail.path == path {
                                    tail.content = content;
                                    tail.error = None;
                                }
                                break;
                            }
                        }
                    }
                    log_tail::Message::LoadFailed {
                        tab_id,
                        path,
                        error,
                    } => {
                        for ws in &mut self.workshops {
                            if let Some(tail) = ws.log_tails.get_mut(&tab_id) {
                                if tail.path == path {
                                    tail.error = Some(error);
                                }
                                break;
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
                    screen::agents::Message::SelectAgent(session_id) => {
                        // Decide what clicking the row should do based on
                        // session state. We compute an Outcome first so that
                        // the mutable borrow of `ws` ends before we call
                        // `self.push_toast` (which needs `&mut self`).
                        enum Outcome {
                            Focused,
                            /// Background Hand: opened (or focused) a spy view
                            /// tab tailing the Hand's log file. Carries the
                            /// new tab id so we can fire the initial load.
                            Spying {
                                tab_id: u64,
                                log_path: PathBuf,
                            },
                            Stale {
                                name: String,
                            },
                            Past {
                                name: String,
                                started_at: String,
                                can_resume: bool,
                            },
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
                                Some(tab_id) if ws.bench.tabs.iter().any(|t| t.id == tab_id) => {
                                    ws.bench.active_tab = Some(tab_id);
                                    Outcome::Focused
                                }
                                // No live terminal tab, but the Hand was
                                // launched detached and we know where its log
                                // lives — open a read-only spy view instead
                                // of erroring. Spark ryve-8c14734a.
                                _ if session.log_path.is_some() => {
                                    let log_path = session.log_path.clone().unwrap();
                                    let (tab_id, _) = ws.open_log_tab(
                                        &session_id,
                                        log_path.clone(),
                                        &mut self.next_terminal_id,
                                    );
                                    Outcome::Spying { tab_id, log_path }
                                }
                                _ => Outcome::Stale { name: session.name },
                            },
                            Some(session) => {
                                // Spark `ryve-a677498c`: dead sessions with
                                // a log file open the spy view on click so
                                // the user can inspect the last output.
                                if session.log_path.is_some() {
                                    let log_path = session.log_path.clone().unwrap();
                                    let (tab_id, _) = ws.open_log_tab(
                                        &session_id,
                                        log_path.clone(),
                                        &mut self.next_terminal_id,
                                    );
                                    Outcome::Spying { tab_id, log_path }
                                } else {
                                    let can_resume = session.can_resume();
                                    Outcome::Past {
                                        name: session.name,
                                        started_at: session.started_at,
                                        can_resume,
                                    }
                                }
                            }
                        };

                        match outcome {
                            Outcome::Focused | Outcome::NotFound => {}
                            Outcome::Spying { tab_id, log_path } => {
                                return Task::perform(
                                    log_tail::load_tail(tab_id, log_path),
                                    Message::LogTail,
                                );
                            }
                            Outcome::Stale { name } => {
                                return self.push_toast(
                                    format!("{name} is no longer running"),
                                    "Its terminal tab has closed. Use the resume button to restart it.",
                                    ToastKind::Warning,
                                );
                            }
                            Outcome::Past {
                                name,
                                started_at,
                                can_resume,
                            } => {
                                let when = screen::agents::format_relative_time(&started_at);
                                let body = if can_resume {
                                    format!(
                                        "Past session started {when}. Click \u{25B6} to resume."
                                    )
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
                        if let Some(session) = session
                            && let Some((cmd, args)) =
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
                                .is_some_and(|s| s.full_auto);
                            let is_atlas = session.session_label.as_deref() == Some("atlas");
                            let tab_id = if is_atlas {
                                ws.begin_atlas_terminal(
                                    session.name.clone(),
                                    workshop::PendingTerminalKind::Agent(resume_agent.clone()),
                                    next_id,
                                    session_id.clone(),
                                    full_auto,
                                )
                            } else {
                                ws.begin_hand_terminal(
                                    session.name.clone(),
                                    workshop::PendingTerminalKind::Agent(resume_agent.clone()),
                                    next_id,
                                    session_id.clone(),
                                    full_auto,
                                )
                            };
                            let worktree_task =
                                Self::dispatch_worktree_task(ws, tab_id, session_id.clone());

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
                                let reactivate = Task::perform(
                                    async move {
                                        let _ = data::sparks::agent_session_repo::reactivate(
                                            &pool, &sid,
                                        )
                                        .await;
                                    },
                                    |_| Message::AgentSessionSaved,
                                );
                                return Task::batch([worktree_task, reactivate]);
                            }
                            return worktree_task;
                        }
                    }
                    screen::agents::Message::DeleteSession(session_id) => {
                        ws.agent_sessions.retain(|s| s.id != session_id);
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let sid = session_id.clone();
                            let ws_dir = ws.directory.clone();
                            return Task::perform(
                                async move {
                                    // Kill tmux session first, then delete DB row.
                                    crate::tmux::terminate_session(&pool, &ws_dir, &sid).await;
                                    let _ =
                                        data::sparks::agent_session_repo::delete(&pool, &sid).await;
                                },
                                |_| Message::AgentSessionSaved,
                            );
                        }
                    }
                    screen::agents::Message::OpenSpark(spark_id) => {
                        // Mirror screen::sparks::Message::SelectSpark — set the
                        // selected spark and load its contracts + bonds so the
                        // right panel hydrates immediately. Honour the
                        // description-draft guard (ryve-4742d98b): a pending
                        // unsaved edit defers the selection change and
                        // surfaces a save/discard/cancel dialog instead.
                        if !ws.try_change_selected_spark(Some(spark_id.clone())) {
                            return Task::none();
                        }
                        ws.selected_spark_contracts.clear();
                        ws.selected_spark_bonds.clear();
                        ws.contract_create_form.reset();
                        ws.reseed_intent_drafts();
                        if let Some(ref pool) = ws.sparks_db {
                            let pool_c = pool.clone();
                            let pool_b = pool.clone();
                            let ws_id = ws.id;
                            let sid_c = spark_id.clone();
                            let sid_b = spark_id.clone();
                            let contracts_task =
                                Task::perform(load_contracts(pool_c, sid_c.clone()), move |list| {
                                    Message::ContractsLoaded(ws_id, sid_c.clone(), list)
                                });
                            let bonds_task =
                                Task::perform(load_bonds(pool_b, sid_b.clone()), move |list| {
                                    Message::BondsLoaded(ws_id, sid_b.clone(), list)
                                });
                            return Task::batch([contracts_task, bonds_task]);
                        }
                    }
                    screen::agents::Message::SearchChanged(query) => {
                        ws.agents_panel.search = query;
                        // Reset history pagination when the query changes —
                        // otherwise a previous "Load more" leaks visible rows
                        // into a narrower filtered set.
                        ws.agents_panel.history_limit = screen::agents::HISTORY_PAGE_SIZE;
                    }
                    screen::agents::Message::LoadMoreHistory => {
                        ws.agents_panel.history_limit += screen::agents::HISTORY_PAGE_SIZE;
                    }
                    screen::agents::Message::ToggleStaleCollapsed => {
                        ws.agents_panel.stale_collapsed = !ws.agents_panel.stale_collapsed;
                    }
                    screen::agents::Message::ToggleHeadExpanded(session_id) => {
                        if !ws.agents_panel.collapsed_heads.remove(&session_id) {
                            ws.agents_panel.collapsed_heads.insert(session_id);
                        }
                    }
                    screen::agents::Message::ToggleCrewExpanded(crew_id) => {
                        if !ws.agents_panel.collapsed_crews.remove(&crew_id) {
                            ws.agents_panel.collapsed_crews.insert(crew_id);
                        }
                    }
                    screen::agents::Message::AttachSession(session_id, label) => {
                        // Spark ryve-8ba40d83: open a bench tab running
                        // `tmux attach` on the Ryve-private socket. If a
                        // TmuxAttach tab for this session already exists,
                        // focus it instead of creating a duplicate.
                        let tmux_name = tmux::session_name_for(&label, &session_id);
                        if let Some(existing) = ws.bench.tabs.iter().find(|t| {
                            matches!(
                                &t.kind,
                                screen::bench::TabKind::TmuxAttach { session_id: sid, .. }
                                if *sid == session_id
                            )
                        }) {
                            ws.bench.active_tab = Some(existing.id);
                        } else {
                            // Derive the tab title from the spark title if the
                            // session has an owner assignment, else fall back to
                            // the tmux session name.
                            let title = ws
                                .agent_sessions
                                .iter()
                                .find(|s| s.id == session_id)
                                .and_then(|s| {
                                    screen::agents::owner_spark_for_session(
                                        &s.id,
                                        &ws.hand_assignments,
                                    )
                                    .and_then(|sid| ws.sparks.iter().find(|sp| sp.id == sid))
                                    .map(|sp| sp.title.clone())
                                    .or_else(|| Some(s.name.clone()))
                                })
                                .unwrap_or_else(|| tmux_name.clone());

                            let tab_id = self.next_terminal_id;
                            self.next_terminal_id += 1;
                            ws.bench.create_tab(
                                tab_id,
                                title,
                                screen::bench::TabKind::TmuxAttach {
                                    session_id: session_id.clone(),
                                    tmux_session_name: tmux_name.clone(),
                                },
                            );

                            // Create the iced_term::Terminal with the tmux
                            // attach command as the backend program.
                            let (program, args) =
                                match tmux::attach_command(&ws.directory, &tmux_name) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        log::error!(
                                            "Cannot attach to tmux session '{tmux_name}': {e}"
                                        );
                                        return Task::none();
                                    }
                                };
                            let (shell, shell_args) =
                                workshop::wrap_command_with_bottom_pin(&program, &args);

                            let mut settings = iced_term::settings::Settings {
                                font: ws.terminal_font_settings(),
                                ..iced_term::settings::Settings::default()
                            };
                            settings.theme.color_pallete.background = ws.terminal_bg_hex();
                            settings.backend.working_directory = Some(ws.directory.clone());
                            settings.backend.program = shell;
                            settings.backend.args = shell_args;

                            if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
                                let focus =
                                    iced_term::TerminalView::focus(term.widget_id().clone());
                                ws.terminals.insert(tab_id, term);
                                return focus;
                            }
                        }
                    }
                    screen::agents::Message::ViewLog(session_id) => {
                        // Spark `ryve-a677498c`: open a read-only log view
                        // for a dead/ended session.
                        if let Some(log_path) = ws
                            .agent_sessions
                            .iter()
                            .find(|s| s.id == session_id)
                            .and_then(|s| s.log_path.clone())
                        {
                            let (tab_id, _) = ws.open_log_tab(
                                &session_id,
                                log_path.clone(),
                                &mut self.next_terminal_id,
                            );
                            return Task::perform(
                                log_tail::load_tail(tab_id, log_path),
                                Message::LogTail,
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
                            // ryve-4742d98b: dirty description draft? Defer
                            // the back-navigation until the user answers
                            // the save/discard/cancel prompt.
                            if !ws.try_change_selected_spark(None) {
                                return Task::none();
                            }
                            ws.selected_spark_contracts.clear();
                            ws.selected_spark_bonds.clear();
                            ws.contract_create_form.reset();
                            ws.acceptance_criteria_edit = Default::default();
                            ws.intent_list_drafts.clear();
                            ws.assignee_edit.end();
                        }
                    }
                    screen::spark_detail::Message::IntentList(list_msg) => {
                        use crate::screen::intent_list_editor as ile;
                        let ws = &mut self.workshops[idx];
                        // Mutate drafts first, then decide whether the
                        // change warrants a persist. `Edit` is purely
                        // local (keystroke-level) and must not write.
                        let mut persist = false;
                        match list_msg {
                            ile::Message::Edit { kind, index, value } => {
                                let list = ws.intent_list_drafts.list_mut(kind);
                                if let Some(slot) = list.get_mut(index) {
                                    *slot = value;
                                }
                            }
                            ile::Message::Add { kind } => {
                                ile::add_blank(ws.intent_list_drafts.list_mut(kind));
                                // Adding a blank row doesn't persist yet
                                // — we'd strip it on save. The first
                                // keystroke or Submit will persist.
                            }
                            ile::Message::Delete { kind, index } => {
                                if ile::delete_at(ws.intent_list_drafts.list_mut(kind), index) {
                                    persist = true;
                                }
                            }
                            ile::Message::MoveUp { kind, index } => {
                                if ile::move_up(ws.intent_list_drafts.list_mut(kind), index) {
                                    persist = true;
                                }
                            }
                            ile::Message::MoveDown { kind, index } => {
                                if ile::move_down(ws.intent_list_drafts.list_mut(kind), index) {
                                    persist = true;
                                }
                            }
                            ile::Message::Submit { kind } => {
                                // "Empty row on blur is auto-deleted" —
                                // fire on Enter. Prune blanks for the
                                // submitted kind so stray empties never
                                // leak into persisted state.
                                ile::prune_blanks(ws.intent_list_drafts.list_mut(kind));
                                persist = true;
                            }
                        }
                        if !persist {
                            return Task::none();
                        }
                        let Some(ref spark_id) = ws.intent_list_drafts.spark_id.clone() else {
                            return Task::none();
                        };
                        let Some(spark) = ws.sparks.iter().find(|s| &s.id == spark_id).cloned()
                        else {
                            return Task::none();
                        };
                        let new_metadata = ile::rebuild_metadata(
                            &spark.metadata,
                            &ws.intent_list_drafts.acceptance,
                            &ws.intent_list_drafts.invariants,
                            &ws.intent_list_drafts.non_goals,
                        );
                        let Some(ref pool) = ws.sparks_db else {
                            return Task::none();
                        };
                        let pool = pool.clone();
                        let ws_id = ws.workshop_id();
                        let id = ws.id;
                        let sid = spark_id.clone();
                        return Task::perform(
                            async move {
                                let upd = data::sparks::types::UpdateSpark {
                                    metadata: Some(new_metadata),
                                    ..Default::default()
                                };
                                let _ =
                                    data::sparks::spark_repo::update(&pool, &sid, upd, "ui").await;
                                load_sparks(pool, ws_id).await
                            },
                            move |sparks| Message::SparksLoaded(id, sparks),
                        );
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
                    screen::spark_detail::Message::SetPriority(spark_id, label) => {
                        // Parse the label back into the integer the data
                        // layer stores. Anything outside P0..P4 is rejected
                        // with a toast — that should be impossible from the
                        // pick_list, but we're at a trust boundary.
                        let Some(new_priority) = screen::spark_detail::parse_priority_label(&label)
                        else {
                            return self.push_toast(
                                "Invalid priority",
                                format!("'{label}' is not a valid priority"),
                                ToastKind::Error,
                            );
                        };
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        // No-op if the value didn't actually change.
                        if ws
                            .sparks
                            .iter()
                            .find(|s| s.id == spark_id)
                            .is_some_and(|s| s.priority == new_priority)
                        {
                            return Task::none();
                        }
                        let workshop_id = ws.id;
                        return self.update(Message::SparkUpdate {
                            workshop_id,
                            id: spark_id,
                            patch: SparkPatch {
                                priority: Some(new_priority),
                                ..Default::default()
                            },
                        });
                    }
                    screen::spark_detail::Message::SetType(spark_id, new_type_label) => {
                        // Validate the label is a known type.
                        if !matches!(
                            new_type_label.as_str(),
                            "bug" | "feature" | "task" | "epic" | "chore" | "spike" | "milestone"
                        ) {
                            return self.push_toast(
                                "Invalid type",
                                format!("'{new_type_label}' is not a valid spark type"),
                                ToastKind::Error,
                            );
                        }
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        // Snapshot the current type and check the
                        // no-orphan invariant before mutating anything.
                        let Some(current) = ws.sparks.iter().find(|s| s.id == spark_id) else {
                            return Task::none();
                        };
                        if current.spark_type == new_type_label {
                            return Task::none();
                        }
                        let was_epic = current.spark_type == "epic";
                        let becoming_non_epic = new_type_label != "epic";
                        if was_epic && becoming_non_epic {
                            // Demoting an epic: refuse if it has any
                            // children, since they'd be orphaned (their
                            // parent would no longer be an epic).
                            let child_count = ws
                                .sparks
                                .iter()
                                .filter(|s| s.parent_id.as_deref() == Some(spark_id.as_str()))
                                .count();
                            if child_count > 0 {
                                return self.push_toast(
                                    "Type change rejected",
                                    format!(
                                        "Cannot demote epic to {new_type_label}: {child_count} child spark(s) would be orphaned"
                                    ),
                                    ToastKind::Error,
                                );
                            }
                            // The spark itself would also become an
                            // orphan if it has no parent. Block that.
                            if current.parent_id.is_none() {
                                return self.push_toast(
                                    "Type change rejected",
                                    format!(
                                        "Cannot demote epic to {new_type_label}: spark has no parent"
                                    ),
                                    ToastKind::Error,
                                );
                            }
                        }
                        let workshop_id = ws.id;
                        return self.update(Message::SparkUpdate {
                            workshop_id,
                            id: spark_id,
                            patch: SparkPatch {
                                spark_type: Some(new_type_label),
                                ..Default::default()
                            },
                        });
                    }
                    screen::spark_detail::Message::AcceptanceCriterionChanged(i, val) => {
                        // Draft-only: update the in-memory row without
                        // touching the DB. Persistence happens on submit.
                        if let Some(ws) = self.workshops.get_mut(idx)
                            && let Some(slot) = ws.acceptance_criteria_edit.items.get_mut(i)
                        {
                            *slot = val;
                        }
                    }
                    screen::spark_detail::Message::AcceptanceCriterionSubmit(i) => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(selected_id) = ws.selected_spark.clone() else {
                            return Task::none();
                        };
                        if !ws.acceptance_criteria_edit.is_for(&selected_id) {
                            return Task::none();
                        }
                        // Auto-delete the row if it's blank on submit so we
                        // never persist stray empties. If that happens, still
                        // persist the shortened vec so the DB matches the UI.
                        screen::spark_detail::trim_blank_on_blur(
                            &mut ws.acceptance_criteria_edit,
                            i,
                        );
                        return self.persist_acceptance_criteria(idx, &selected_id);
                    }
                    screen::spark_detail::Message::AcceptanceCriterionAdd => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(selected_id) = ws.selected_spark.clone() else {
                            return Task::none();
                        };
                        if !ws.acceptance_criteria_edit.is_for(&selected_id) {
                            // Editor isn't bound to a spark yet — rebind from
                            // current sparks before appending.
                            ws.acceptance_criteria_edit = ws
                                .sparks
                                .iter()
                                .find(|s| s.id == selected_id)
                                .map(screen::spark_detail::AcceptanceCriteriaEdit::load)
                                .unwrap_or_default();
                        }
                        let new_index =
                            screen::spark_detail::add_criterion(&mut ws.acceptance_criteria_edit);
                        // Focus the freshly-added row for immediate typing.
                        // No DB write yet — the empty row only persists once
                        // the user types and submits (or gets auto-deleted).
                        return iced::widget::operation::focus(
                            screen::spark_detail::acceptance_row_id(new_index),
                        );
                    }
                    screen::spark_detail::Message::AcceptanceCriterionDelete(i) => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(selected_id) = ws.selected_spark.clone() else {
                            return Task::none();
                        };
                        if !ws.acceptance_criteria_edit.is_for(&selected_id) {
                            return Task::none();
                        }
                        if !screen::spark_detail::delete_criterion(
                            &mut ws.acceptance_criteria_edit,
                            i,
                        ) {
                            return Task::none();
                        }
                        let persist_task = self.persist_acceptance_criteria(idx, &selected_id);
                        let toast_task = self.push_toast(
                            "Criterion deleted",
                            "Press the undo button in the detail view to restore.",
                            ToastKind::Info,
                        );
                        return Task::batch([persist_task, toast_task]);
                    }
                    screen::spark_detail::Message::AcceptanceCriterionUndoDelete => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(selected_id) = ws.selected_spark.clone() else {
                            return Task::none();
                        };
                        if !ws.acceptance_criteria_edit.is_for(&selected_id) {
                            return Task::none();
                        }
                        if !screen::spark_detail::undo_delete(&mut ws.acceptance_criteria_edit) {
                            return Task::none();
                        }
                        return self.persist_acceptance_criteria(idx, &selected_id);
                    }
                    screen::spark_detail::Message::AcceptanceCriterionMoveUp(i) => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(selected_id) = ws.selected_spark.clone() else {
                            return Task::none();
                        };
                        if !ws.acceptance_criteria_edit.is_for(&selected_id) {
                            return Task::none();
                        }
                        if !screen::spark_detail::move_up(&mut ws.acceptance_criteria_edit, i) {
                            return Task::none();
                        }
                        return self.persist_acceptance_criteria(idx, &selected_id);
                    }
                    screen::spark_detail::Message::AcceptanceCriterionMoveDown(i) => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(selected_id) = ws.selected_spark.clone() else {
                            return Task::none();
                        };
                        if !ws.acceptance_criteria_edit.is_for(&selected_id) {
                            return Task::none();
                        }
                        if !screen::spark_detail::move_down(&mut ws.acceptance_criteria_edit, i) {
                            return Task::none();
                        }
                        return self.persist_acceptance_criteria(idx, &selected_id);
                    }
                    screen::spark_detail::Message::BeginEditField(field) => {
                        // Route every field-edit entry through the
                        // confirmation gate. If the spark is terminal and
                        // not yet acknowledged for this session, the
                        // session stashes the field and the modal pops;
                        // otherwise the gate returns Proceed and the
                        // field-specific handler seeds the draft for the
                        // requested field. See ryve-8ad372cf.
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(ref sid) = ws.selected_spark else {
                            return Task::none();
                        };
                        let Some(spark) = ws.sparks.iter().find(|s| &s.id == sid).cloned() else {
                            return Task::none();
                        };
                        let outcome = ws.spark_edit_session.begin_edit(&spark, field);
                        if outcome != screen::spark_detail::BeginEditOutcome::Proceed {
                            // Modal is now showing; wait for user action.
                            return Task::none();
                        }
                        // Gate passed — seed the draft for the requested
                        // field so the UI flips into edit mode.
                        match field {
                            screen::spark_detail::EditField::Title => {
                                let seed = spark.title.clone();
                                let edit = ws.spark_edit.get_or_insert_with(|| {
                                    screen::spark_detail::SparkEdit::new(spark.id.clone())
                                });
                                if !edit
                                    .in_flight
                                    .contains_key(&screen::spark_detail::Field::Title)
                                    && !edit
                                        .drafts
                                        .contains_key(&screen::spark_detail::Field::Title)
                                {
                                    edit.update_draft(screen::spark_detail::Field::Title, seed);
                                }
                            }
                            _ => {
                                // Other fields will be wired up by
                                // follow-up sparks as they land.
                            }
                        }
                    }
                    screen::spark_detail::Message::ConfirmClosedEdit => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            // Releasing the gate returns the pending
                            // field so the caller can resume whatever
                            // the user originally asked for. Until the
                            // sibling field-edit plumbing lands in this
                            // worktree the field is simply consumed —
                            // the confirmation still unlocks future
                            // edits in this session.
                            let _ = ws.spark_edit_session.confirm_closed_edit();
                        }
                    }
                    screen::spark_detail::Message::CancelClosedEdit => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_edit_session.cancel_closed_edit();
                        }
                    }
                    screen::spark_detail::Message::BeginEditAssignee => {
                        // Spark ryve-7e1cb491: enter inline edit mode for the
                        // assignee field. Compute the suggestion union
                        // (active agent session names ∪ distinct past
                        // assignees across sparks) from in-memory caches.
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            let current = ws
                                .selected_spark
                                .as_deref()
                                .and_then(|id| ws.sparks.iter().find(|s| s.id == id))
                                .and_then(|s| s.assignee.clone());
                            let agent_names: Vec<&str> = ws
                                .agent_sessions
                                .iter()
                                .filter(|a| a.active)
                                .map(|a| a.name.as_str())
                                .collect();
                            let suggestions = screen::spark_detail::build_assignee_suggestions(
                                &agent_names,
                                &ws.sparks,
                            );
                            ws.assignee_edit.begin(current.as_deref(), suggestions);
                        }
                    }
                    screen::spark_detail::Message::AssigneeInputChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.assignee_edit.input = val;
                        }
                    }
                    screen::spark_detail::Message::AssigneeSelected(val) => {
                        // A suggestion was picked (Enter on a highlighted
                        // option or mouse click). Persist as-is and end
                        // the edit.
                        if let Some(task) = self.commit_assignee_edit(idx, Some(val)) {
                            return task;
                        }
                    }
                    screen::spark_detail::Message::AssigneeClosed => {
                        // combo_box blurred. If Escape set the cancelled
                        // flag, drop the edit silently. Otherwise commit
                        // the current input (empty → clear).
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        if !ws.assignee_edit.is_active() {
                            return Task::none();
                        }
                        if ws.assignee_edit.cancelled {
                            ws.assignee_edit.end();
                            return Task::none();
                        }
                        let value = ws.assignee_edit.input.trim().to_string();
                        let new_value = if value.is_empty() { None } else { Some(value) };
                        if let Some(task) = self.commit_assignee_edit(idx, new_value) {
                            return task;
                        }
                    }
                    // -- Description inline edit (ryve-4742d98b) --
                    screen::spark_detail::Message::DescriptionClicked => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.begin_description_edit();
                        }
                    }
                    screen::spark_detail::Message::DescriptionAction(action) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            // Apply the action to the live `Content`,
                            // then mirror the resulting text into the
                            // SparkEdit draft so the "unsaved" pill
                            // reflects reality on the next frame.
                            if let Some(ref mut content) = ws.description_editor {
                                content.perform(action);
                                let text = content.text();
                                // iced's `Content::text()` appends a
                                // trailing newline that wasn't in the
                                // source — strip it so round-tripping
                                // an unchanged draft stays "clean".
                                let stripped = text.strip_suffix('\n').unwrap_or(&text).to_string();
                                if let Some(ref mut edit) = ws.spark_edit {
                                    edit.drafts
                                        .insert(screen::spark_detail::Field::Description, stripped);
                                }
                            }
                        }
                    }
                    screen::spark_detail::Message::DescriptionBlur => {
                        // Blur = commit. If the draft equals the
                        // persisted value we just close the editor;
                        // otherwise we dispatch a SparkUpdate through
                        // the shared write path (ryve-90174007).
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            let Some((spark_id, draft)) = ws.take_description_draft() else {
                                return Task::none();
                            };
                            let current = ws
                                .sparks
                                .iter()
                                .find(|s| s.id == spark_id)
                                .map(|s| s.description.clone())
                                .unwrap_or_default();
                            if draft == current {
                                return Task::none();
                            }
                            let workshop_id = ws.id;
                            return Task::done(Message::SparkUpdate {
                                workshop_id,
                                id: spark_id,
                                patch: SparkPatch {
                                    description: Some(draft),
                                    ..Default::default()
                                },
                            });
                        }
                    }
                    screen::spark_detail::Message::DescriptionRevert => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.revert_description_edit();
                        }
                    }
                    screen::spark_detail::Message::NavPromptSave => {
                        // Commit the draft, then finish the deferred
                        // navigation. SparkUpdate applies optimistically,
                        // so we can navigate immediately without waiting
                        // on the async write to confirm.
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            let Some(prompt) = ws.pending_nav_prompt.take() else {
                                return Task::none();
                            };
                            let mut follow_up: Vec<Task<Message>> = Vec::new();
                            if let Some((spark_id, draft)) = ws.take_description_draft() {
                                let current = ws
                                    .sparks
                                    .iter()
                                    .find(|s| s.id == spark_id)
                                    .map(|s| s.description.clone())
                                    .unwrap_or_default();
                                if draft != current {
                                    let workshop_id = ws.id;
                                    follow_up.push(Task::done(Message::SparkUpdate {
                                        workshop_id,
                                        id: spark_id,
                                        patch: SparkPatch {
                                            description: Some(draft),
                                            ..Default::default()
                                        },
                                    }));
                                }
                            }
                            // `change_selected_spark` (not the gated
                            // `try_` variant) — we already handled the
                            // dirty state by committing or discarding.
                            let _ = ws.change_selected_spark(prompt.target);
                            ws.selected_spark_contracts.clear();
                            ws.selected_spark_bonds.clear();
                            ws.contract_create_form.reset();
                            if !follow_up.is_empty() {
                                return Task::batch(follow_up);
                            }
                        }
                    }
                    screen::spark_detail::Message::NavPromptDiscard => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            let Some(prompt) = ws.pending_nav_prompt.take() else {
                                return Task::none();
                            };
                            ws.revert_description_edit();
                            let _ = ws.change_selected_spark(prompt.target);
                            ws.selected_spark_contracts.clear();
                            ws.selected_spark_bonds.clear();
                            ws.contract_create_form.reset();
                        }
                    }
                    screen::spark_detail::Message::NavPromptCancel => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.pending_nav_prompt = None;
                        }
                    }
                    // -- Editable problem statement (ryve-a5997352) --
                    screen::spark_detail::Message::BeginEditProblem(spark_id) => {
                        let ws = &mut self.workshops[idx];
                        // Don't blow away an in-flight editor for the same
                        // spark on a double-click; keep the existing content
                        // so the user's cursor stays put.
                        if ws
                            .problem_edit
                            .as_ref()
                            .is_some_and(|e| e.spark_id == spark_id)
                        {
                            return Task::none();
                        }
                        let current = ws
                            .sparks
                            .iter()
                            .find(|s| s.id == spark_id)
                            .and_then(|s| s.intent().problem_statement)
                            .unwrap_or_default();
                        ws.problem_edit = Some(screen::spark_detail::ProblemEditState::new(
                            spark_id, &current,
                        ));
                    }
                    screen::spark_detail::Message::ProblemAction(action) => {
                        if let Some(ws) = self.workshops.get_mut(idx)
                            && let Some(edit) = ws.problem_edit.as_mut()
                        {
                            edit.content.perform(action);
                        }
                    }
                    screen::spark_detail::Message::CancelProblem => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.problem_edit = None;
                        }
                    }
                    screen::spark_detail::Message::CommitProblem => {
                        let ws = &mut self.workshops[idx];
                        let Some(edit) = ws.problem_edit.take() else {
                            return Task::none();
                        };
                        // Normalize: text_editor::Content::text() appends a
                        // trailing newline for non-empty content. Strip a
                        // single trailing \n so a user typing "hello"
                        // doesn't get "hello\n" persisted, which would
                        // re-dirty the editor on every open.
                        let mut new_value = edit.content.text();
                        if new_value.ends_with('\n') {
                            new_value.pop();
                        }
                        if new_value == edit.original {
                            // No-op commit (e.g. clicked outside without
                            // typing) — silently drop the editor.
                            return Task::none();
                        }
                        let workshop_id = ws.id;
                        let spark_id = edit.spark_id.clone();
                        return Task::done(Message::SparkUpdate {
                            workshop_id,
                            id: spark_id,
                            patch: SparkPatch {
                                problem_statement: Some(new_value),
                                ..Default::default()
                            },
                        });
                    }
                    screen::spark_detail::Message::CycleStatus(spark_id, new_status) => {
                        if let Some(ws) = self.workshops.get(idx)
                            && let Some(ref pool) = ws.sparks_db
                        {
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
                                        let status =
                                            data::sparks::types::SparkStatus::from_str(&new_status);
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
                    // ── Title inline edit (spark ryve-f58d0492) ─────────
                    screen::spark_detail::Message::TitleChanged(val) => {
                        if let Some(ws) = self.workshops.get_mut(idx)
                            && let Some(ref mut edit) = ws.spark_edit
                        {
                            edit.update_draft(screen::spark_detail::Field::Title, val);
                        }
                    }
                    // Spark ryve-dba4b8c4: navigate to the agent session.
                    screen::spark_detail::Message::FocusAgentSession(session_id) => {
                        return self.update(Message::Agents(screen::agents::Message::SelectAgent(
                            session_id,
                        )));
                    }
                    screen::spark_detail::Message::TitleSubmit
                    | screen::spark_detail::Message::TitleBlur => {
                        let Some(ws) = self.workshops.get_mut(idx) else {
                            return Task::none();
                        };
                        let Some(ref mut edit) = ws.spark_edit else {
                            return Task::none();
                        };
                        // No draft → nothing to save (already persisted
                        // or never entered). Blur on a clean view is a
                        // harmless no-op.
                        let Some(draft) = edit
                            .drafts
                            .get(&screen::spark_detail::Field::Title)
                            .cloned()
                        else {
                            return Task::none();
                        };
                        let trimmed = draft.trim().to_string();
                        if trimmed.is_empty() {
                            // Empty title rejected inline — don't persist.
                            // Leave the draft in place so the red border +
                            // tooltip stay visible until the user fixes it
                            // or hits Escape.
                            return Task::none();
                        }
                        // No-op save (draft matches on-disk value): drop
                        // the draft without dispatching a DB write so we
                        // don't churn the event log.
                        let current = ws
                            .sparks
                            .iter()
                            .find(|s| s.id == edit.spark_id)
                            .map(|s| s.title.clone());
                        if current.as_deref() == Some(trimmed.as_str()) {
                            edit.rollback(screen::spark_detail::Field::Title);
                            return Task::none();
                        }
                        // Move draft into in_flight and emit SparkUpdate.
                        // We overwrite the draft with the trimmed value so
                        // `commit` captures the trimmed string.
                        edit.update_draft(screen::spark_detail::Field::Title, trimmed.clone());
                        let Some(write) = edit.commit(screen::spark_detail::Field::Title) else {
                            return Task::none();
                        };
                        let patch = SparkPatch {
                            title: Some(write.value),
                            ..Default::default()
                        };
                        return self.update(Message::SparkUpdate {
                            workshop_id: self.workshops[idx].id,
                            id: write.spark_id,
                            patch,
                        });
                    }
                }
                Task::none()
            }
            Message::SparkPicker(msg) => self.handle_spark_picker_message(msg),
            Message::HeadPicker(msg) => self.handle_head_picker_message(msg),
            Message::HandAssignmentSaved => Task::none(),
            Message::HandWorktreeReady {
                workshop_id,
                tab_id,
                result,
            } => {
                // Stage 2 of the Hand spawn flow (spark ryve-885ed3eb):
                // the async `create_hand_worktree` task has reported back.
                // Finalize the terminal and, if successful, focus it so
                // the user's next keystrokes land in the new Hand.
                let Some(idx) = self.workshops.iter().position(|ws| ws.id == workshop_id) else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                let created = ws.finalize_hand_terminal(tab_id, result);
                let mut tasks: Vec<Task<Message>> = Vec::new();
                if created && let Some(term) = ws.terminals.get(&tab_id) {
                    tasks.push(iced_term::TerminalView::focus(term.widget_id().clone()));
                }
                // Surface any fallback-to-workshop-root warning as a toast.
                if let Some(msg) = ws.take_worktree_warning() {
                    tasks.push(self.push_toast("Worktree fallback", msg, ToastKind::Warning));
                }
                if tasks.is_empty() {
                    Task::none()
                } else {
                    Task::batch(tasks)
                }
            }
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
            Message::HotkeyCmdF => {
                // Route Cmd+F to whichever search overlay is on the
                // active tab. File viewer and terminal both grab it.
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                let ws = &self.workshops[idx];
                if let Some(active_id) = ws.bench.active_tab {
                    let kind = ws
                        .bench
                        .tabs
                        .iter()
                        .find(|t| t.id == active_id)
                        .map(|t| &t.kind);
                    match kind {
                        Some(screen::bench::TabKind::FileViewer(_)) => {
                            self.update(Message::FileViewer(file_viewer::Message::OpenSearch))
                        }
                        Some(
                            screen::bench::TabKind::Terminal
                            | screen::bench::TabKind::CodingAgent(_)
                            | screen::bench::TabKind::TmuxAttach { .. },
                        ) => self.handle_bench_message(screen::bench::Message::OpenTerminalSearch),
                        _ => Task::none(),
                    }
                } else {
                    Task::none()
                }
            }
            Message::HotkeyEscape => {
                // Escape: close any open search overlay on the active
                // tab. Always also clears file-viewer selection so the
                // pre-existing behavior is preserved.
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                // Escape also reverts any in-progress title edit to the
                // on-disk value. Spark ryve-f58d0492. Skips in-flight
                // slots — rolling back an already-dispatched save would
                // race the async handler.
                if let Some(ws) = self.workshops.get_mut(idx)
                    && let Some(ref mut edit) = ws.spark_edit
                {
                    edit.drafts.remove(&screen::spark_detail::Field::Title);
                    if !edit.is_dirty() {
                        ws.spark_edit = None;
                    }
                }
                // Spark ryve-7e1cb491: if the assignee inline editor is
                // active, Escape cancels the edit (no persistence) and
                // the remaining Escape side-effects still run.
                if let Some(ws) = self.workshops.get_mut(idx)
                    && ws.assignee_edit.is_active()
                {
                    ws.assignee_edit.cancelled = true;
                    ws.assignee_edit.end();
                }
                let ws = &self.workshops[idx];
                let mut tasks: Vec<Task<Message>> = Vec::new();
                if let Some(active_id) = ws.bench.active_tab
                    && ws.bench.terminal_search.contains_key(&active_id)
                {
                    tasks.push(
                        self.handle_bench_message(screen::bench::Message::CloseTerminalSearch),
                    );
                }
                // Spark create form dismissal: Escape cancels the inline
                // "+" form without persisting, per spark ryve-d158cc9f
                // acceptance criteria.
                if self.workshops[idx].spark_create_form.visible {
                    tasks.push(self.update(Message::Sparks(screen::sparks::Message::CancelCreate)));
                }
                tasks.push(self.update(Message::FileViewer(file_viewer::Message::ClearSelection)));
                Task::batch(tasks)
            }
            Message::Bench(msg) => self.handle_bench_message(msg),
            Message::Sparks(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                match msg {
                    screen::sparks::Message::Refresh => {
                        // Explicit user refetch: bypass `poll_in_flight` (which
                        // only gates the 3s auto-poll) so the button always
                        // does what it says, and flip the per-workshop
                        // `sparks_refreshing` flag so the button renders a
                        // visible in-flight indicator until `SparksLoaded`
                        // comes back. Spark ryve-7805b38b.
                        if let Some(ws) = self.workshops.get_mut(idx)
                            && let Some(ref pool) = ws.sparks_db
                        {
                            let pool = pool.clone();
                            let ws_id = ws.workshop_id();
                            let id = ws.id;
                            ws.sparks_refreshing = true;
                            return Task::perform(load_sparks(pool, ws_id), move |sparks| {
                                Message::SparksLoaded(id, sparks)
                            });
                        }
                    }
                    screen::sparks::Message::SelectSpark(spark_id) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            // ryve-4742d98b: defer selection change if
                            // the current spark has a dirty description.
                            if !ws.try_change_selected_spark(Some(spark_id.clone())) {
                                return Task::none();
                            }
                            ws.selected_spark_contracts.clear();
                            ws.selected_spark_bonds.clear();
                            ws.contract_create_form.reset();
                            // Seed the acceptance-criteria editor from the
                            // newly selected spark's current intent so the
                            // draft vec starts life in sync with the DB.
                            ws.acceptance_criteria_edit = ws
                                .sparks
                                .iter()
                                .find(|s| s.id == spark_id)
                                .map(screen::spark_detail::AcceptanceCriteriaEdit::load)
                                .unwrap_or_default();
                            ws.reseed_intent_drafts();
                            ws.assignee_edit.end();
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
                                let bonds_task =
                                    Task::perform(load_bonds(pool_b, sid_b.clone()), move |list| {
                                        Message::BondsLoaded(ws_id, sid_b.clone(), list)
                                    });
                                return Task::batch([contracts_task, bonds_task]);
                            }
                        }
                    }
                    screen::sparks::Message::ShowReleases => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.show_releases = true;
                            // Trigger a releases load from the DB.
                            if let Some(ref pool) = ws.sparks_db {
                                let pool = pool.clone();
                                let id = ws.id;
                                return Task::perform(
                                    async move {
                                        let releases =
                                            data::sparks::release_repo::list(&pool, None)
                                                .await
                                                .unwrap_or_default();
                                        let mut epic_ids = Vec::new();
                                        for r in &releases {
                                            let ids =
                                                data::sparks::release_repo::list_member_epics(
                                                    &pool, &r.id,
                                                )
                                                .await
                                                .unwrap_or_default();
                                            epic_ids.push((r.id.clone(), ids));
                                        }
                                        (releases, epic_ids)
                                    },
                                    move |(releases, epic_ids)| {
                                        Message::ReleasesLoaded(id, releases, epic_ids)
                                    },
                                );
                            }
                        }
                    }
                    screen::sparks::Message::ShowCreateForm => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            // Default the parent picker to the focused
                            // spark's nearest epic ancestor (or the spark
                            // itself, if it is an epic). If nothing is
                            // focused the form opens with an empty parent
                            // and the user must pick one (unless they are
                            // creating a top-level epic).
                            let default_parent = screen::sparks::resolve_default_parent_epic(
                                &ws.sparks,
                                ws.selected_spark.as_deref(),
                            );
                            ws.spark_create_form
                                .open_with_default_parent(default_parent);
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
                        let spark_type_str = ws.spark_create_form.spark_type.clone();
                        let priority = ws.spark_create_form.priority;
                        let parent_id = ws.spark_create_form.parent_epic_id.clone();

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
                                    let bond_parent_id = parent_id.clone();
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
                                        metadata: None,
                                        risk_level: None,
                                        scope_boundary: None,
                                    };
                                    let new_id =
                                        match data::sparks::spark_repo::create(&pool, new).await {
                                            Ok(spark) => {
                                                if let Some(ref pid) = bond_parent_id {
                                                    let _ = data::sparks::bond_repo::create(
                                                        &pool,
                                                        pid,
                                                        &spark.id,
                                                        data::sparks::types::BondType::ParentChild,
                                                    )
                                                    .await;
                                                }
                                                Some(spark.id)
                                            }
                                            Err(_) => None,
                                        };
                                    let sparks = load_sparks(pool, ws_id).await;
                                    (new_id, sparks)
                                },
                                move |(new_id, sparks)| Message::SparkCreated(id, new_id, sparks),
                            );
                        }
                    }
                    screen::sparks::Message::OpenStatusMenu(spark_id) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_status_menu.open(spark_id);
                        }
                    }
                    screen::sparks::Message::CloseStatusMenu => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_status_menu.dismiss();
                        }
                    }
                    screen::sparks::Message::BeginCloseFlow(_spark_id) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_status_menu.enter_close_stage();
                        }
                    }
                    screen::sparks::Message::SetStatus(spark_id, new_status) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_status_menu.dismiss();
                            if let Some(ref pool) = ws.sparks_db {
                                let pool = pool.clone();
                                let ws_id = ws.workshop_id();
                                let id = ws.id;
                                return Task::perform(
                                    async move {
                                        if let Some(s) =
                                            data::sparks::types::SparkStatus::from_str(&new_status)
                                        {
                                            let upd = data::sparks::types::UpdateSpark {
                                                status: Some(s),
                                                ..Default::default()
                                            };
                                            let _ = data::sparks::spark_repo::update(
                                                &pool, &spark_id, upd, "user",
                                            )
                                            .await;
                                        }
                                        load_sparks(pool, ws_id).await
                                    },
                                    move |sparks| Message::SparksLoaded(id, sparks),
                                );
                            }
                        }
                    }
                    screen::sparks::Message::ToggleStatusFilter(status) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.toggle_status(&status);
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::ToggleShowClosed => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.show_closed = !ws.sparks_filter.show_closed;
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::ToggleEpicCollapse(epic_id) => {
                        // Flip the collapse flag in memory and persist the
                        // new snapshot to `.ryve/ui_state.json` so the
                        // choice survives restart. Fire-and-forget — a
                        // failed write is logged but never blocks the UI.
                        // Spark ryve-926870a9.
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.toggle_epic_collapse(&epic_id);
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    // ── Filter bar (spark ryve-baca34b0) ───────
                    screen::sparks::Message::FilterToggleType(ty) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.toggle_type(ty);
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::FilterTogglePriority(p) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.toggle_priority(p);
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::FilterSetAssignee(a) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.set_assignee(a);
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::SetSortMode(mode) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sort_mode = mode;
                            ws.sort_dropdown_open = false;
                            ws.sort_sparks();
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::ToggleSortDropdown => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sort_dropdown_open = !ws.sort_dropdown_open;
                        }
                    }
                    screen::sparks::Message::SearchChanged(query) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.search = query;
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::ClearSearch => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.sparks_filter.search.clear();
                            ws.recompute_filtered_sparks();
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::SparksFilterChanged => {
                        // Persist the updated filter state to
                        // `.ryve/ui_state.json`. Fire-and-forget — a
                        // failed write is logged but never blocks the UI.
                        // Spark ryve-27e33825.
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            let ryve_dir = ws.ryve_dir.clone();
                            let snapshot = ws.ui_state_snapshot();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    data::ryve_dir::save_ui_state(&ryve_dir, &snapshot).await
                                {
                                    log::warn!("failed to save .ryve/ui_state.json: {e}");
                                }
                            });
                        }
                    }
                    screen::sparks::Message::CloseSparkWithReason(spark_id, reason) => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.spark_status_menu.dismiss();
                            if let Some(ref pool) = ws.sparks_db {
                                let pool = pool.clone();
                                let ws_id = ws.workshop_id();
                                let id = ws.id;
                                return Task::perform(
                                    async move {
                                        let _ = data::sparks::spark_repo::close(
                                            &pool, &spark_id, &reason, "user",
                                        )
                                        .await;
                                        load_sparks(pool, ws_id).await
                                    },
                                    move |sparks| Message::SparksLoaded(id, sparks),
                                );
                            }
                        }
                    }
                    // Spark ryve-dba4b8c4: navigate to the agent session.
                    screen::sparks::Message::FocusAgentSession(session_id) => {
                        return self.update(Message::Agents(screen::agents::Message::SelectAgent(
                            session_id,
                        )));
                    }
                }
                Task::none()
            }

            // ── Releases ─────────────────────────────────
            Message::Releases(msg) => {
                let Some(idx) = self.active_workshop else {
                    return Task::none();
                };
                match msg {
                    screen::releases::Message::Back => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.show_releases = false;
                        }
                    }
                    screen::releases::Message::TogglePastReleases => {
                        if let Some(ws) = self.workshops.get_mut(idx) {
                            ws.releases_state.past_expanded = !ws.releases_state.past_expanded;
                        }
                    }
                    screen::releases::Message::RequestClose(release_id) => {
                        // Emit a toast prompting the user to spawn a Release
                        // Manager via Atlas if none is running. The workshop
                        // update loop does not mutate release state directly —
                        // the Release Manager archetype owns the close flow.
                        return self.push_toast(
                            "Release close requested",
                            format!(
                                "Ask Atlas to spawn a Release Manager to close release {release_id}."
                            ),
                            ToastKind::Info,
                        );
                    }
                }
                Task::none()
            }
            Message::ReleasesLoaded(id, releases, epic_ids_per_release) => {
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == id) {
                    // Build a HashMap keyed by release_id for robust
                    // joining instead of fragile index correlation.
                    let epic_map: std::collections::HashMap<&str, &[String]> = epic_ids_per_release
                        .iter()
                        .map(|(rid, ids)| (rid.as_str(), ids.as_slice()))
                        .collect();
                    let mut view_data = Vec::new();
                    for release in releases.into_iter() {
                        let epic_ids = epic_map.get(release.id.as_str()).copied().unwrap_or(&[]);
                        let member_epics: Vec<_> = epic_ids
                            .iter()
                            .filter_map(|eid| ws.sparks.iter().find(|s| s.id == *eid).cloned())
                            .collect();
                        view_data.push(screen::releases::ReleaseViewData {
                            release,
                            member_epics,
                        });
                    }
                    ws.release_view_data = view_data;
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
            Message::Noop => Task::none(),
            Message::BackupTick => {
                // Spark ryve-7c8573c4: periodic snapshot of every open
                // workshop's sparks.db so a crash or corruption leaves
                // at most `DEFAULT_BACKUP_INTERVAL_SECS` worth of work
                // unrecoverable.
                self.snapshot_all_workshops()
            }
            Message::BackupFinished(result) => {
                match result {
                    Ok(path) => {
                        log::info!("backup: wrote {}", path.display());
                        Task::none()
                    }
                    Err(err) => {
                        // Loud in logs, but don't spam a toast on every
                        // tick — quiet failure is preferable to a
                        // user-facing interruption. The next successful
                        // snapshot restores the invariant.
                        log::warn!("backup: snapshot failed: {err}");
                        Task::none()
                    }
                }
            }
            Message::SparksPoll => {
                // Opportunistically surface any worktree warnings that the
                // synchronous spawn paths accumulated since the last tick.
                let warnings: Vec<String> = self
                    .workshops
                    .iter_mut()
                    .filter_map(|ws| ws.take_worktree_warning())
                    .collect();
                let warning_tasks: Vec<Task<Message>> = warnings
                    .into_iter()
                    .map(|w| self.push_toast("Worktree fallback", w, ToastKind::Warning))
                    .collect();

                if self.poll_in_flight {
                    return Task::batch(warning_tasks);
                }

                // Spark `ryve-a5b9e4a1`: snapshot the OS process table once
                // per tick on a blocking thread, then resume the rest of the
                // poll body inside `ProcessSnapshotReady` so the UI thread
                // never calls `System::new_all` itself. Marking the poll
                // in-flight here keeps the next tick from racing this one
                // even though the work hasn't started yet.
                self.poll_in_flight = true;
                let snapshot_task = Task::perform(
                    async {
                        tokio::task::spawn_blocking(ProcessSnapshot::capture)
                            .await
                            .unwrap_or_default()
                    },
                    |snap| Message::ProcessSnapshotReady(Arc::new(snap)),
                );
                let mut all = warning_tasks;
                all.push(snapshot_task);
                Task::batch(all)
            }

            Message::ProcessSnapshotReady(snapshot) => {
                // Cache the snapshot so any handler that fires on the back
                // half of this tick (`AgentSessionsLoaded`, `SparksLoaded`,
                // …) can read liveness from the same scan instead of taking
                // its own. Spark `ryve-a5b9e4a1`.
                self.last_process_snapshot = Some(snapshot.clone());

                let mut tasks: Vec<Task<Message>> = Vec::new();

                // Auto-detect agent processes in plain terminals, sharing
                // the snapshot with `detect_untracked_agents` instead of
                // letting it walk /proc itself.
                for ws in self.workshops.iter_mut() {
                    let detected = ws.detect_untracked_agents(&snapshot);
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
                            log_path: None,
                            last_output_at: None,
                            parent_session_id: None,
                            session_label: Some("auto-detected".to_string()),
                            tmux_session_live: false,
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
                                log_path: None,
                                // UI-spawned: no parent Hand.
                                parent_session_id: None,
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
                //
                // Each reload is preceded by a lightweight tmux reconciliation
                // pass so that stale DB rows (whose tmux session died between
                // polls) are marked ended before the UI sees them. This makes
                // the Hands panel authoritative without PID-based liveness
                // for tmux-managed sessions. Spark [sp-0285181c].
                let session_tasks: Vec<_> = self
                    .workshops
                    .iter()
                    .filter(|ws| ws.sparks_db.is_some())
                    .map(|ws| {
                        let pool = ws.sparks_db.clone().unwrap();
                        let pool2 = pool.clone();
                        let ws_id = ws.workshop_id();
                        let ws_id2 = ws_id.clone();
                        let dir = ws.directory.clone();
                        let id = ws.id;
                        Task::perform(
                            async move {
                                tmux::reconcile_sessions(&dir, &pool2, &ws_id2).await;
                                load_agent_sessions(pool, ws_id).await
                            },
                            move |sessions| Message::AgentSessionsLoaded(id, sessions),
                        )
                    })
                    .collect();
                tasks.extend(session_tasks);

                // Refresh every open spy view (LogTail tab) so background
                // Hands' output streams in without the user having to
                // re-click them. Spark ryve-8c14734a.
                for ws in &self.workshops {
                    for (&tab_id, tail) in &ws.log_tails {
                        let path = tail.path.clone();
                        tasks.push(Task::perform(
                            log_tail::load_tail(tab_id, path),
                            Message::LogTail,
                        ));
                    }
                }

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

                if tasks.is_empty() {
                    // Nothing to wait on — release the in-flight gate now,
                    // otherwise the next 3-second tick would early-return.
                    self.poll_in_flight = false;
                    return Task::none();
                }
                Task::batch(tasks)
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
                self.handle_bench_message(screen::bench::Message::NewCodingAgent(agent))
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

            // ── Ember notification bar ───────────────────
            Message::EmberBar(screen::ember_bar::Message::Dismiss(ember_id)) => {
                // Drop from the DB so the next poll doesn't resurrect it.
                let Some(ws) = self.active_workshop_mut() else {
                    return Task::none();
                };
                let ws_uuid = ws.id;
                let Some(pool) = ws.sparks_db.clone() else {
                    // No DB yet — just drop it locally.
                    ws.embers.retain(|e| e.id != ember_id);
                    return Task::none();
                };
                // Optimistic: remove from the cached list immediately so the
                // bar collapses without waiting for the delete to round-trip.
                ws.embers.retain(|e| e.id != ember_id);
                let id_for_async = ember_id.clone();
                Task::perform(
                    async move {
                        if let Err(e) = data::sparks::ember_repo::delete(&pool, &id_for_async).await
                        {
                            log::warn!("Failed to delete ember {id_for_async}: {e}");
                        }
                        id_for_async
                    },
                    move |ember_id| Message::EmberDismissed {
                        workshop_id: ws_uuid,
                        ember_id,
                    },
                )
            }
            Message::EmberDismissed {
                workshop_id,
                ember_id,
            } => {
                // DB delete finished; make sure the local cache matches. No-op
                // most of the time because `EmberBar::Dismiss` already pruned.
                if let Some(ws) = self.workshops.iter_mut().find(|ws| ws.id == workshop_id) {
                    ws.embers.retain(|e| e.id != ember_id);
                }
                Task::none()
            }
            Message::OpenUrl(url) => {
                // Best-effort: if the OS can't open the URL we surface a toast
                // rather than crashing the workspace.
                if let Err(e) = open::that(&url) {
                    return self.push_toast(
                        "Could not open link",
                        format!("{url}: {e}"),
                        ToastKind::Error,
                    );
                }
                Task::none()
            }

            // -- Spark field edits (ryve-90174007) --
            Message::SparkUpdate {
                workshop_id,
                id,
                patch,
            } => {
                if patch.is_empty() {
                    return Task::none();
                }
                let Some(idx) = self.workshops.iter().position(|ws| ws.id == workshop_id) else {
                    return Task::none();
                };
                let ws = &mut self.workshops[idx];
                // Optimistic write: mutate the cached spark immediately so
                // the UI reflects the edit without waiting on the DB. The
                // returned `prior` is the inverse patch used for rollback
                // if the async write fails.
                let Some(prior) = ws.apply_spark_patch(&id, &patch) else {
                    // Spark isn't in the cache — nothing to roll back, nothing
                    // to persist. This happens if the detail view still holds
                    // a stale selection across a workshop reload.
                    return Task::none();
                };
                if prior.is_empty() {
                    // No-op edit (every field already matched) — don't churn
                    // the DB or the write-amplifying event log.
                    return Task::none();
                }
                let Some(pool) = ws.sparks_db.clone() else {
                    // Pool not ready yet (workshop still initializing). Roll
                    // back the optimistic apply and surface a toast so the
                    // user isn't staring at a stale cached value.
                    ws.apply_spark_patch(&id, &prior);
                    return self.push_toast(
                        "Save failed",
                        "Workshop is still initializing",
                        ToastKind::Error,
                    );
                };
                let mut upd = patch.to_update_spark();
                // `problem_statement` lives in metadata JSON — it can't be
                // represented in `to_update_spark`'s stateless translation
                // because merging requires the spark's existing metadata.
                // apply_spark_patch has already mutated ws.sparks[idx].metadata
                // to the post-merge value, so read it back here and ship the
                // whole blob to the DB (spark ryve-a5997352).
                if patch.problem_statement.is_some()
                    && let Some(s) = ws.sparks.iter().find(|s| s.id == id)
                {
                    upd.metadata = Some(s.metadata.clone());
                }
                let id_for_task = id.clone();
                let affected = patch.affected_fields();
                Task::perform(
                    async move {
                        data::sparks::spark_repo::update(&pool, &id_for_task, upd, "user")
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    },
                    move |res| match res {
                        Ok(()) => Message::SparkUpdateApplied {
                            workshop_id,
                            id: id.clone(),
                            fields: affected.clone(),
                        },
                        Err(error) => Message::SparkUpdateFailed {
                            workshop_id,
                            id: id.clone(),
                            prior: prior.clone(),
                            error,
                            fields: affected.clone(),
                        },
                    },
                )
            }
            Message::SparkUpdateApplied {
                workshop_id,
                id,
                fields,
            } => {
                // Durable write succeeded; the optimistic cache is now the
                // source of truth. Clear only the in-flight slots that were
                // part of *this* write so concurrent requests for other
                // fields keep their in-flight state intact.
                if let Some(idx) = self.workshops.iter().position(|ws| ws.id == workshop_id)
                    && let Some(ref mut edit) = self.workshops[idx].spark_edit
                    && edit.spark_id == id
                {
                    for field in &fields {
                        edit.in_flight.remove(field);
                    }
                    if !edit.is_dirty() {
                        self.workshops[idx].spark_edit = None;
                    }
                }
                Task::none()
            }
            Message::SparkUpdateFailed {
                workshop_id,
                id,
                prior,
                error,
                fields,
            } => {
                // Restore the pre-edit values. If the spark has since
                // disappeared from the cache (e.g. workshop closed mid-flight)
                // the rollback silently no-ops — we still push the toast so
                // the user knows the write didn't land.
                if let Some(idx) = self.workshops.iter().position(|ws| ws.id == workshop_id) {
                    let ws = &mut self.workshops[idx];
                    ws.apply_spark_patch(&id, &prior);
                    // Move only the failed request's in-flight slots back
                    // into drafts so the user can see what they tried to
                    // save and either fix it or re-submit. Other in-flight
                    // fields from concurrent requests are left alone.
                    if let Some(ref mut edit) = ws.spark_edit
                        && edit.spark_id == id
                    {
                        for field in &fields {
                            if let Some(value) = edit.in_flight.remove(field) {
                                edit.drafts.insert(field.clone(), value);
                            }
                        }
                    }
                }
                self.push_toast(format!("Could not save {id}"), error, ToastKind::Error)
            }
        }
    }

    /// Mutable accessor for the currently selected workshop, if any. Used by
    /// handlers that need to mutate workshop state and kick off an async task.
    fn active_workshop_mut(&mut self) -> Option<&mut Workshop> {
        let idx = self.active_workshop?;
        self.workshops.get_mut(idx)
    }

    /// Persist an assignee edit for the currently-selected spark and end
    /// the inline edit state. `new_value == None` clears the assignee
    /// (writes `assignee: Some(None)` so the repo updates the column to
    /// NULL); `Some(v)` sets it to `v` as-is. Returns the follow-up
    /// reload task, or `None` if there is no selected spark or no DB
    /// available (in which case the edit state is still torn down so
    /// the UI snaps back to display mode).
    ///
    /// Spark ryve-7e1cb491.
    fn commit_assignee_edit(
        &mut self,
        idx: usize,
        new_value: Option<String>,
    ) -> Option<Task<Message>> {
        let ws = self.workshops.get_mut(idx)?;
        let spark_id = ws.selected_spark.clone();
        ws.assignee_edit.end();
        let spark_id = spark_id?;
        // Optimistic update of the cached spark so the row flips back
        // to display mode showing the new value immediately, before
        // the async write completes.
        if let Some(s) = ws.sparks.iter_mut().find(|s| s.id == spark_id) {
            s.assignee = new_value.clone();
        }
        let pool = ws.sparks_db.as_ref()?.clone();
        let ws_id_str = ws.workshop_id();
        let id = ws.id;
        Some(Task::perform(
            async move {
                let upd = data::sparks::types::UpdateSpark {
                    assignee: Some(new_value),
                    ..Default::default()
                };
                if let Err(e) =
                    data::sparks::spark_repo::update(&pool, &spark_id, upd, "user").await
                {
                    return Err(e.to_string());
                }
                Ok(load_sparks(pool, ws_id_str).await)
            },
            move |res: Result<Vec<_>, String>| match res {
                Ok(sparks) => Message::SparksLoaded(id, sparks),
                Err(error) => Message::ShowToast {
                    title: "Could not save assignee".into(),
                    body: error,
                    kind: ToastKind::Error,
                },
            },
        ))
    }

    /// Route Home dashboard interactions: clicking a spark surfaces it in
    /// the workgraph panel; clicking a Hand focuses its bench tab if it's
    /// still alive. No DB writes — the Home view is read-only.
    fn handle_home_message(&mut self, msg: screen::home::Message) -> Task<Message> {
        let Some(idx) = self.active_workshop else {
            return Task::none();
        };
        let ws = &mut self.workshops[idx];
        match msg {
            screen::home::Message::SelectSpark(id) => {
                // ryve-4742d98b: guard on dirty description draft.
                if !ws.try_change_selected_spark(Some(id.clone())) {
                    return Task::none();
                }
                ws.reseed_intent_drafts();
                if let Some(ref pool) = ws.sparks_db {
                    let pool = pool.clone();
                    let ws_id = ws.id;
                    return Task::perform(load_contracts(pool, id.clone()), move |list| {
                        Message::ContractsLoaded(ws_id, id.clone(), list)
                    });
                }
                Task::none()
            }
            screen::home::Message::FocusHand(session_id) => {
                let tab_id = ws
                    .agent_sessions
                    .iter()
                    .find(|s| s.id == session_id)
                    .and_then(|s| s.tab_id);
                if let Some(tab_id) = tab_id {
                    ws.bench.active_tab = Some(tab_id);
                }
                Task::none()
            }
        }
    }

    /// Apply a terminal-font-size delta from a Cmd+scroll gesture, clamped
    /// to the configured min/max. Updates the global config, mirrors the
    /// new value onto every workshop, and broadcasts a `ChangeFont` command
    /// to every live terminal so the resize is immediate. Spark sp-ux0014.
    fn apply_terminal_font_delta(&mut self, delta: f32) -> Task<Message> {
        let current = self.global_config.effective_terminal_font_size();
        let next = (current + delta).clamp(
            data::config::MIN_TERMINAL_FONT_SIZE,
            data::config::MAX_TERMINAL_FONT_SIZE,
        );
        if (next - current).abs() < f32::EPSILON {
            return Task::none();
        }
        self.set_terminal_font(Some(next), self.global_config.terminal_font_family.clone())
    }

    /// Update the terminal font (size and/or family) globally. Mirrors the
    /// new values to every workshop, broadcasts `ChangeFont` to every live
    /// terminal, and persists the global config to disk. Spark sp-ux0014.
    fn set_terminal_font(&mut self, size: Option<f32>, family: Option<String>) -> Task<Message> {
        if let Some(s) = size {
            self.global_config.terminal_font_size = Some(s);
        }
        self.global_config.terminal_font_family = family;
        let effective_size = self.global_config.effective_terminal_font_size();
        let effective_family = self.global_config.terminal_font_family.clone();

        // Build the FontSettings once and clone into each terminal handle.
        let font_type = match &effective_family {
            Some(name) => iced::Font {
                family: iced::font::Family::Name(font_intern::intern(name)),
                ..iced::Font::MONOSPACE
            },
            None => iced::Font::MONOSPACE,
        };
        let font = iced_term::settings::FontSettings {
            size: effective_size,
            font_type,
            ..iced_term::settings::FontSettings::default()
        };

        for ws in self.workshops.iter_mut() {
            ws.terminal_font_size = effective_size;
            ws.terminal_font_family = effective_family.clone();
            for term in ws.terminals.values_mut() {
                let _ = term.handle(iced_term::Command::ChangeFont(font.clone()));
            }
        }

        let config = self.global_config.clone();
        Task::perform(
            async move {
                if let Err(e) = config.save() {
                    log::warn!("Failed to persist terminal font settings: {e}");
                }
            },
            |_| Message::BackgroundConfigSaved,
        )
    }

    fn handle_bench_message(&mut self, msg: screen::bench::Message) -> Task<Message> {
        // Cmd+scroll bubbles up as a FontSizeDelta event from iced_term.
        // Persist the new size to the global config and broadcast it to
        // every live terminal so the resize is uniform across panes.
        // Spark sp-ux0014.
        if let screen::bench::Message::TerminalEvent(iced_term::Event::FontSizeDelta(_id, delta)) =
            msg
        {
            return self.apply_terminal_font_delta(delta);
        }

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
                // A ProcessAlacrittyEvent is how iced_term delivers any PTY
                // activity (the alacritty event loop wakes up on new output,
                // title changes, bells, etc.). Treating any of these as
                // "recent activity" is what lets us later flip an idle Hand
                // back to blue the moment its agent starts speaking again.
                let is_pty_activity =
                    matches!(cmd, iced_term::BackendCommand::ProcessAlacrittyEvent(_));
                if is_pty_activity {
                    let now = std::time::Instant::now();
                    for session in ws.agent_sessions.iter_mut() {
                        if session.tab_id == Some(id) {
                            session.last_output_at = Some(now);
                        }
                    }
                }
                let mut tab_closed = false;
                if let Some(term) = ws.terminals.get_mut(&id) {
                    let action = term.handle(iced_term::Command::ProxyToBackend(cmd.clone()));
                    let was_shutdown = matches!(action, iced_term::actions::Action::Shutdown);
                    let ended_sessions = ws.handle_terminal_action(id, action);
                    if was_shutdown {
                        tab_closed = true;
                    }
                    if !ended_sessions.is_empty()
                        && let Some(ref pool) = ws.sparks_db
                    {
                        let pool = pool.clone();
                        let ws_dir = ws.directory.clone();
                        let mut tasks: Vec<Task<Message>> = ended_sessions
                            .into_iter()
                            .map(|sid| {
                                let pool = pool.clone();
                                let ws_dir = ws_dir.clone();
                                Task::perform(
                                    async move {
                                        crate::tmux::terminate_session(&pool, &ws_dir, &sid).await;
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
                if let Some(prev_id) = prev_tab
                    && prev_id != id
                    && let Some(prev_viewer) = ws.file_viewers.get_mut(&prev_id)
                {
                    prev_viewer.evict();
                }

                // Focus the terminal immediately so it accepts keyboard input
                if let Some(term) = ws.terminals.get(&id) {
                    return iced_term::TerminalView::focus(term.widget_id().clone());
                }

                // Reload an evicted file viewer when its tab becomes active
                if let Some(viewer) = ws.file_viewers.get(&id)
                    && !viewer.is_loaded()
                {
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
            screen::bench::Message::CloseTab(id) => {
                let ws = &mut self.workshops[idx];
                if ws.bench.is_pinned(id) {
                    return Task::none();
                }
                ws.terminals.remove(&id);
                ws.file_viewers.remove(&id);

                // Mark agent sessions as ended AND kill their tmux sessions
                let mut end_tasks: Vec<Task<Message>> = Vec::new();
                let ws_dir = ws.directory.clone();
                for session in ws.agent_sessions.iter_mut() {
                    if session.tab_id == Some(id) {
                        session.tab_id = None;
                        session.active = false;
                        session.stale = false;
                        if let Some(ref pool) = ws.sparks_db {
                            let pool = pool.clone();
                            let sid = session.id.clone();
                            let ws_dir = ws_dir.clone();
                            end_tasks.push(Task::perform(
                                async move {
                                    crate::tmux::terminate_session(&pool, &ws_dir, &sid).await;
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
            screen::bench::Message::CloseDropdown => {
                self.workshops[idx].bench.dropdown_open = false;
            }
            screen::bench::Message::NoOp => {}
            screen::bench::Message::OpenHome => {
                self.workshops[idx].bench.dropdown_open = false;
                let next_id = &mut self.next_terminal_id;
                self.workshops[idx].open_home_tab(next_id);
                // No persistence: Home is a singleton dashboard rebuilt
                // from in-memory data on demand.
                return Task::none();
            }
            screen::bench::Message::NewTerminal => {
                let next_id = &mut self.next_terminal_id;
                let tab_id =
                    self.workshops[idx].spawn_plain_terminal("Terminal".to_string(), next_id);
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
                    .is_some_and(|s| s.full_auto);

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
            screen::bench::Message::FocusAtlas => {
                // Spark ryve-fa0f8f93 — Atlas is auto-spawned on workshop
                // open. This message focuses the existing pinned Atlas tab.
                let ws = &mut self.workshops[idx];
                ws.bench.dropdown_open = false;
                if let Some(atlas_tab_id) = ws
                    .agent_sessions
                    .iter()
                    .find(|s| s.session_label.as_deref() == Some("atlas") && s.active)
                    .and_then(|s| s.tab_id)
                {
                    ws.bench.active_tab = Some(atlas_tab_id);
                }
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
            screen::bench::Message::OpenTerminalSearch => {
                let ws = &mut self.workshops[idx];
                let Some(active_id) = ws.bench.active_tab else {
                    return Task::none();
                };
                // Only meaningful for terminal tabs (CodingAgent counts as
                // a terminal — both render the same iced_term widget).
                let is_terminal_kind = ws.bench.tabs.iter().any(|t| {
                    t.id == active_id
                        && matches!(
                            t.kind,
                            screen::bench::TabKind::Terminal
                                | screen::bench::TabKind::CodingAgent(_)
                                | screen::bench::TabKind::TmuxAttach { .. }
                        )
                });
                if !is_terminal_kind || !ws.terminals.contains_key(&active_id) {
                    return Task::none();
                }
                ws.bench.terminal_search.entry(active_id).or_default();
                return iced::widget::operation::focus(iced::widget::Id::new(
                    screen::bench::TERMINAL_SEARCH_INPUT_ID,
                ));
            }
            screen::bench::Message::CloseTerminalSearch => {
                let ws = &mut self.workshops[idx];
                let Some(active_id) = ws.bench.active_tab else {
                    return Task::none();
                };
                if ws.bench.terminal_search.remove(&active_id).is_some()
                    && let Some(term) = ws.terminals.get_mut(&active_id)
                {
                    term.clear_search_selection();
                }
            }
            screen::bench::Message::TerminalSearchQueryChanged(q) => {
                let ws = &mut self.workshops[idx];
                let Some(active_id) = ws.bench.active_tab else {
                    return Task::none();
                };
                let Some(term) = ws.terminals.get_mut(&active_id) else {
                    return Task::none();
                };
                let matches = term.search(&q);
                let entry = ws.bench.terminal_search.entry(active_id).or_default();
                entry.query = q;
                entry.match_count = matches.len();
                if matches.is_empty() {
                    entry.current_match = None;
                    term.clear_search_selection();
                } else {
                    entry.current_match = Some(0);
                    term.focus_match(&matches[0]);
                }
            }
            screen::bench::Message::TerminalSearchNext
            | screen::bench::Message::TerminalSearchPrev => {
                let forward = matches!(msg, screen::bench::Message::TerminalSearchNext);
                let ws = &mut self.workshops[idx];
                let Some(active_id) = ws.bench.active_tab else {
                    return Task::none();
                };
                let Some(term) = ws.terminals.get_mut(&active_id) else {
                    return Task::none();
                };
                let Some(entry) = ws.bench.terminal_search.get_mut(&active_id) else {
                    return Task::none();
                };
                if entry.query.is_empty() {
                    return Task::none();
                }
                // Re-run the search so navigation reflects any new
                // output that landed since the last query change. The
                // active terminal can grow under us at any moment.
                let matches = term.search(&entry.query);
                entry.match_count = matches.len();
                if matches.is_empty() {
                    entry.current_match = None;
                    term.clear_search_selection();
                    return Task::none();
                }
                let next = match entry.current_match {
                    None => 0,
                    Some(i) if forward => (i + 1) % matches.len(),
                    Some(0) => matches.len() - 1,
                    Some(i) => i - 1,
                };
                entry.current_match = Some(next);
                term.focus_match(&matches[next]);
            }
            screen::bench::Message::RefreshAtlas(tab_id) => {
                // Kill the current Atlas subprocess and relaunch a fresh
                // one in-place. Tab id, position, and label stay stable.
                // Spark ryve-71c3ec9f.
                let ws = &mut self.workshops[idx];
                let full_auto = ws
                    .agent_sessions
                    .iter()
                    .find(|s| s.tab_id == Some(tab_id) && s.active)
                    .and_then(|s| {
                        self.global_config
                            .agent_settings
                            .get(&s.agent.command)
                            .map(|cfg| cfg.full_auto)
                    })
                    .unwrap_or(false);

                let new_session_id = Uuid::new_v4().to_string();
                let Some((agent, ended_session_ids)) =
                    ws.prepare_atlas_refresh(tab_id, new_session_id.clone(), full_auto)
                else {
                    return Task::none();
                };

                // Dispatch the worktree task (idempotent — reuses existing
                // worktree directory).
                let worktree_task =
                    Self::dispatch_worktree_task(ws, tab_id, new_session_id.clone());

                // Register the new session in memory.
                let title = format!("Atlas ({})", agent.display_name);
                let agent_command = agent.command.clone();
                let agent_args = agent.args.clone();
                ws.agent_sessions.push(AgentSession {
                    id: new_session_id.clone(),
                    name: title.clone(),
                    agent,
                    tab_id: Some(tab_id),
                    active: true,
                    stale: false,
                    resume_id: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    log_path: None,
                    last_output_at: None,
                    parent_session_id: None,
                    session_label: Some("atlas".to_string()),
                    tmux_session_live: false,
                });

                let mut tasks: Vec<Task<Message>> = vec![worktree_task];

                // Persist the new session + end the old one(s) in DB + kill tmux.
                if let Some(ref pool) = self.workshops[idx].sparks_db {
                    let pool_end = pool.clone();
                    let pool_create = pool.clone();
                    let ws_id = self.workshops[idx].workshop_id();
                    let ws_dir = self.workshops[idx].directory.clone();

                    // End old sessions for this tab in DB and kill tmux.
                    for sid in ended_session_ids {
                        let p = pool_end.clone();
                        let wd = ws_dir.clone();
                        tasks.push(Task::perform(
                            async move {
                                crate::tmux::terminate_session(&p, &wd, &sid).await;
                            },
                            |_| Message::AgentSessionSaved,
                        ));
                    }

                    let new_session = data::sparks::types::NewAgentSession {
                        id: new_session_id.clone(),
                        workshop_id: ws_id,
                        agent_name: title,
                        agent_command,
                        agent_args,
                        session_label: Some("atlas".to_string()),
                        child_pid: None,
                        resume_id: None,
                        log_path: None,
                        parent_session_id: None,
                    };
                    tasks.push(Task::perform(
                        async move {
                            let _ = data::sparks::agent_session_repo::create(
                                &pool_create,
                                &new_session,
                            )
                            .await;
                        },
                        |_| Message::AgentSessionSaved,
                    ));
                }

                // Re-inject the Atlas Director prompt after a boot delay.
                let prompt = agent_prompts::compose_atlas_prompt();
                tasks.push(Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    },
                    move |_| Message::SendSparkPrompt { tab_id, prompt },
                ));

                return Task::batch(tasks);
            }
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
                if let Some(pending) = ws.pending_agent_spawn.as_mut()
                    && let Some(agent) = self
                        .available_agents
                        .iter()
                        .find(|a| a.command == command)
                        .cloned()
                {
                    let full_auto = self
                        .global_config
                        .agent_settings
                        .get(&agent.command)
                        .is_some_and(|s| s.full_auto);
                    pending.agent = Some(agent);
                    pending.full_auto = full_auto;
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
            screen::head_picker::Message::SelectEpic(epic_id) => {
                if let Some(state) = self.workshops[idx].pending_head_spawn.as_mut() {
                    state.selected_epic_id = epic_id;
                }
                Task::none()
            }
            screen::head_picker::Message::SelectAgent(command) => {
                let epic_id = self.workshops[idx]
                    .pending_head_spawn
                    .as_ref()
                    .and_then(|s| s.selected_epic_id.clone());
                // Resolve the epic's title from the workshop's cached sparks
                // so the Head prompt can reference it without a round-trip.
                let epic_title = epic_id.as_ref().and_then(|id| {
                    self.workshops[idx]
                        .sparks
                        .iter()
                        .find(|s| &s.id == id)
                        .map(|s| s.title.clone())
                });
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
                self.spawn_head(idx, agent, epic_id, epic_title)
            }
            screen::head_picker::Message::Cancel => {
                self.workshops[idx].pending_head_spawn = None;
                Task::none()
            }
        }
    }

    /// Build a `Task` that drives the async `create_hand_worktree` call for
    /// a Hand terminal and dispatches `HandWorktreeReady` back to `update`.
    /// Spark ryve-885ed3eb: callers that begin a Hand spawn via
    /// `Workshop::begin_hand_terminal` use this to kick off stage 2 without
    /// blocking the UI thread on `git worktree add`.
    fn dispatch_worktree_task(ws: &Workshop, tab_id: u64, session_id: String) -> Task<Message> {
        let workshop_dir = ws.directory.clone();
        let ryve_dir = ws.ryve_dir.clone();
        let workshop_id = ws.id;
        Task::perform(
            async move { workshop::create_hand_worktree(&workshop_dir, &ryve_dir, &session_id).await },
            move |result| Message::HandWorktreeReady {
                workshop_id,
                tab_id,
                result,
            },
        )
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
                ws.begin_hand_terminal(
                    def.name.clone(),
                    workshop::PendingTerminalKind::CustomAgent(def.clone()),
                    &mut self.next_terminal_id,
                    session_id.clone(),
                    pending.full_auto,
                )
            } else {
                return Task::none();
            }
        } else {
            ws.begin_hand_terminal(
                title.clone(),
                workshop::PendingTerminalKind::Agent(pending_agent.clone()),
                &mut self.next_terminal_id,
                session_id.clone(),
                pending.full_auto,
            )
        };
        // Stage 2: drive the async worktree creation. `HandWorktreeReady`
        // will finalize the `iced_term::Terminal` and focus it.
        let worktree_task = Self::dispatch_worktree_task(ws, tab_id, session_id.clone());

        ws.agent_sessions.push(AgentSession {
            id: session_id.clone(),
            name: title.clone(),
            agent: pending_agent,
            tab_id: Some(tab_id),
            active: true,
            stale: false,
            resume_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            log_path: None,
            last_output_at: None,
            parent_session_id: None,
            session_label: None,
            tmux_session_live: false,
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
                log_path: None,
                // UI-spawned Hand: no orchestrator parent.
                parent_session_id: None,
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
        // Kick off the async worktree creation last so it is chained after
        // the DB persistence tasks in the batch — finalization + focus fire
        // when `HandWorktreeReady` lands, not synchronously here.
        tasks.push(worktree_task);
        Task::batch(tasks)
    }

    /// Spawn a Head — a coding agent launched with the Head system prompt
    /// instead of a Hand prompt. The Head has no spark assignment of its
    /// own; its job is to *create* sparks via the `ryve` CLI.
    fn spawn_head(
        &mut self,
        workshop_idx: usize,
        agent: CodingAgent,
        epic_id: Option<String>,
        epic_title: Option<String>,
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

        let tab_id = ws.begin_hand_terminal(
            title.clone(),
            workshop::PendingTerminalKind::Agent(agent.clone()),
            &mut self.next_terminal_id,
            session_id.clone(),
            full_auto,
        );
        let worktree_task = Self::dispatch_worktree_task(ws, tab_id, session_id.clone());

        ws.agent_sessions.push(AgentSession {
            id: session_id.clone(),
            name: title.clone(),
            agent: agent.clone(),
            tab_id: Some(tab_id),
            active: true,
            stale: false,
            resume_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            log_path: None,
            last_output_at: None,
            parent_session_id: None,
            session_label: Some("head".to_string()),
            tmux_session_live: false,
        });

        let mut tasks: Vec<Task<Message>> = Vec::new();
        tasks.push(worktree_task);
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
                log_path: None,
                // A Head is itself a top-level orchestrator — no parent.
                parent_session_id: None,
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
        let prompt = agent_prompts::compose_head_prompt(
            agent_prompts::HeadArchetype::Build,
            epic_id.as_deref(),
            epic_title.as_deref(),
        );
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

        // Focus is handled by the `HandWorktreeReady` handler once the
        // terminal widget actually exists.
        Task::batch(tasks)
    }

    /// Spawn Atlas as a pinned leftmost tab without stealing focus.
    /// Used by the auto-spawn logic in `OpenTabsLoaded` (spark ryve-fa0f8f93).
    fn spawn_atlas_pinned(&mut self, workshop_idx: usize, agent: CodingAgent) -> Task<Message> {
        let prev_active = self.workshops[workshop_idx].bench.active_tab;
        let task = self.spawn_atlas(workshop_idx, agent);
        let ws = &mut self.workshops[workshop_idx];
        // Move the newly-appended Atlas tab to position 0.
        if let Some(pos) = ws.bench.tabs.iter().position(|t| {
            ws.agent_sessions
                .iter()
                .any(|s| s.tab_id == Some(t.id) && s.session_label.as_deref() == Some("atlas"))
        }) {
            let tab = ws.bench.tabs.remove(pos);
            ws.bench.tabs.insert(0, tab);
        }
        // Restore previous focus (or default to Atlas if nothing was active).
        ws.bench.active_tab = prev_active.or(ws.bench.tabs.first().map(|t| t.id));
        task
    }

    /// Spawn **Atlas** — a coding agent launched with the Atlas Director
    /// system prompt. Atlas is the default entry point for user-originated
    /// requests. It has no spark assignment of its own and creates no Crew
    /// up front; its job is to talk to the user and delegate to a Head
    /// (multi-spark goal) or a Hand (single spark) via the `ryve` CLI.
    ///
    /// Mechanically nearly identical to `spawn_head`: only the session
    /// label and the injected prompt differ.
    fn spawn_atlas(&mut self, workshop_idx: usize, agent: CodingAgent) -> Task<Message> {
        let ws = &mut self.workshops[workshop_idx];

        let session_id = Uuid::new_v4().to_string();
        let title = format!("Atlas ({})", agent.display_name);
        let agent_command = agent.command.clone();
        let agent_args = agent.args.clone();
        let full_auto = self
            .global_config
            .agent_settings
            .get(&agent.command)
            .is_some_and(|s| s.full_auto);

        let tab_id = ws.begin_atlas_terminal(
            title.clone(),
            workshop::PendingTerminalKind::Agent(agent.clone()),
            &mut self.next_terminal_id,
            session_id.clone(),
            full_auto,
        );
        // Atlas tabs are created via begin_atlas_terminal which sets both
        // pinned and is_atlas. This guard is defensive — ensure both flags
        // are set even if the create path changes (sparks ryve-59983890,
        // ryve-71c3ec9f).
        if let Some(tab) = ws.bench.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.pinned = true;
            tab.is_atlas = true;
        }
        let worktree_task = Self::dispatch_worktree_task(ws, tab_id, session_id.clone());

        ws.agent_sessions.push(AgentSession {
            id: session_id.clone(),
            name: title.clone(),
            agent: agent.clone(),
            tab_id: Some(tab_id),
            active: true,
            stale: false,
            resume_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            log_path: None,
            last_output_at: None,
            // Atlas is the top of the hierarchy — no parent.
            parent_session_id: None,
            session_label: Some("atlas".to_string()),
            tmux_session_live: false,
        });

        let mut tasks: Vec<Task<Message>> = Vec::new();
        tasks.push(worktree_task);
        if let Some(ref pool) = ws.sparks_db {
            let pool = pool.clone();
            let ws_id = ws.workshop_id();
            let new_session = data::sparks::types::NewAgentSession {
                id: session_id.clone(),
                workshop_id: ws_id,
                agent_name: title,
                agent_command,
                agent_args,
                // Distinct label so traces, the Hands panel, and any
                // future Atlas-aware UI can pick Atlas out from regular
                // Heads and Hands.
                session_label: Some("atlas".to_string()),
                child_pid: None,
                resume_id: None,
                log_path: None,
                parent_session_id: None,
            };
            tasks.push(Task::perform(
                async move {
                    let _ = data::sparks::agent_session_repo::create(&pool, &new_session).await;
                },
                |_| Message::AgentSessionSaved,
            ));
        }

        // Inject the Atlas Director system prompt the same way the Head and
        // Hand flows do — typed into the terminal after a short boot delay.
        let prompt = agent_prompts::compose_atlas_prompt();
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

        // Focus is handled by the `HandWorktreeReady` handler once the
        // terminal widget actually exists.
        Task::batch(tasks)
    }

    fn handle_background_message(
        &mut self,
        msg: screen::background_picker::Message,
    ) -> Task<Message> {
        // Terminal font controls don't touch any per-workshop picker state,
        // and they need `&mut self` to broadcast across all workshops, so
        // handle them up front before borrowing the active workshop.
        // Spark sp-ux0014.
        match &msg {
            screen::background_picker::Message::StepTerminalFontSize(delta) => {
                return self.apply_terminal_font_delta(*delta);
            }
            screen::background_picker::Message::SetTerminalFontFamily(name) => {
                let trimmed = name.trim();
                let family = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
                return self.set_terminal_font(None, family);
            }
            screen::background_picker::Message::ClearTerminalFontFamily => {
                return self.set_terminal_font(None, None);
            }
            _ => {}
        }

        let Some(idx) = self.active_workshop else {
            return Task::none();
        };
        let ws = &mut self.workshops[idx];

        match msg {
            screen::background_picker::Message::Close => {
                ws.background_picker.open = false;
                ws.background_picker.clear_preview();
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
                    Message::UnsplashSearchResult,
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

            // ── Dim opacity ──────────────────────────────────
            screen::background_picker::Message::DimOpacityChanged(value) => {
                ws.config.background.dim_opacity = value.clamp(0.0, 1.0);
                Task::none()
            }
            screen::background_picker::Message::DimOpacityCommitted => {
                let ryve_dir = ws.ryve_dir.clone();
                let config = ws.config.clone();
                Task::perform(
                    async move {
                        data::ryve_dir::save_config(&ryve_dir, &config).await.ok();
                    },
                    |_| Message::BackgroundConfigSaved,
                )
            }

            // ── Preview ──────────────────────────────────────
            screen::background_picker::Message::PreviewPhoto(id) => {
                ws.background_picker.set_preview(&id);
                Task::none()
            }
            screen::background_picker::Message::ClearPreview => {
                ws.background_picker.clear_preview();
                Task::none()
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
            // Terminal font arms are handled up-front (above) before the
            // workshop borrow; these arms exist only to keep the match
            // exhaustive. Spark sp-ux0014.
            screen::background_picker::Message::StepTerminalFontSize(_)
            | screen::background_picker::Message::SetTerminalFontFamily(_)
            | screen::background_picker::Message::ClearTerminalFontFamily => Task::none(),
            screen::background_picker::Message::SetDelegationVisibility(level) => {
                self.global_config.delegation_visibility = level;
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

        // Spark ryve-7c8573c4: periodic `.backup` snapshot of every
        // open workshop so a crash or corruption leaves at most one
        // interval's worth of work unrecoverable.
        let backup_tick = iced::time::every(std::time::Duration::from_secs(
            data::backup::DEFAULT_BACKUP_INTERVAL_SECS,
        ))
        .map(|_| Message::BackupTick);

        // Translate Iced keyboard events into the framework-agnostic
        // [`perf_core::KeyKind`] / [`perf_core::KeyModifiers`] pair so the
        // routing decision lives in [`perf_core::classify_key_event`]. The
        // smoke test in `perf_core/tests/sparks_poll_smoke.rs` drives the
        // *same* classifier with synthetic events to assert no key path
        // ever resolves to `SparksPoll`. This subsumes the lean
        // event::listen_with fix from hand/0e2ed795 ([sp-18253584]) by
        // making the same correctness property automatically tested.
        // Sparks ryve-5b9c5d93 + ryve-a13f9d3a.
        let hotkeys = keyboard::listen().map(|event| {
            let (kind, mods) = match &event {
                keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(c),
                    modifiers,
                    ..
                } => {
                    let ch = c.chars().next().unwrap_or('\0');
                    (
                        perf_core::KeyKind::Character(ch),
                        perf_core::KeyModifiers {
                            command: modifiers.command(),
                        },
                    )
                }
                keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                } => (
                    perf_core::KeyKind::Escape,
                    perf_core::KeyModifiers::default(),
                ),
                keyboard::Event::ModifiersChanged(modifiers) => (
                    perf_core::KeyKind::ModifiersChanged {
                        shift: modifiers.shift(),
                    },
                    perf_core::KeyModifiers::default(),
                ),
                _ => (
                    perf_core::KeyKind::Other,
                    perf_core::KeyModifiers::default(),
                ),
            };

            match perf_core::classify_key_event(kind, mods) {
                perf_core::KeyDispatch::NewDefaultHand => Message::NewDefaultHand,
                perf_core::KeyDispatch::CopySelection => {
                    Message::FileViewer(file_viewer::Message::CopySelection)
                }
                perf_core::KeyDispatch::HotkeyCmdF => Message::HotkeyCmdF,
                perf_core::KeyDispatch::NewWorkshopDialog => Message::NewWorkshopDialog,
                perf_core::KeyDispatch::HotkeyEscape => Message::HotkeyEscape,
                perf_core::KeyDispatch::ShiftStateChanged(shift) => {
                    Message::ShiftStateChanged(shift)
                }
                // SparksPoll is a sentinel that classify_key_event must
                // never return. The regression test asserts this; if it
                // ever did escape, we still want a no-op rather than a
                // workgraph reload — so map it to Noop here too.
                perf_core::KeyDispatch::Noop | perf_core::KeyDispatch::SparksPoll => Message::Noop,
            }
        });

        // Spark ryve-7e1cb491: Tab pressed while the assignee inline
        // editor is active commits the current value as a selection.
        // The SparkDetail handler no-ops if no workshop is editing, so
        // emitting unconditionally is safe.
        let assignee_tab = event::listen_with(|event, _status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Tab),
                ..
            }) => Some(Message::SparkDetail(
                screen::spark_detail::Message::AssigneeClosed,
            )),
            _ => None,
        });

        // Track window resizes so the splitter can convert vertical
        // drag deltas into a sensible sidebar split ratio.
        let resizes = window::resize_events().map(|(_, size)| Message::WindowResized(size));

        // Drag-and-drop a folder onto the window to open it as a workshop.
        // We accept drops regardless of which screen is showing — the open
        // handler dedupes against already-open workshops and rejects files
        // that aren't directories. Welcome-screen onboarding for sp-ux0016.
        let file_drops = event::listen_with(|event, _status, _id| match event {
            iced::Event::Window(window::Event::FileDropped(path)) if path.is_dir() => {
                Some(Message::OpenWorkshopPath(path))
            }
            _ => None,
        });

        let mut subs: Vec<Subscription<Message>> = term_subs
            .into_iter()
            .chain(std::iter::once(poll))
            .chain(std::iter::once(backup_tick))
            .chain(std::iter::once(hotkeys))
            .chain(std::iter::once(assignee_tab))
            .chain(std::iter::once(resizes))
            .chain(std::iter::once(file_drops))
            .collect();

        // Only listen to global mouse events while a splitter drag is
        // in progress — otherwise we'd waste cycles on every cursor
        // move when nothing cares about them.
        if self.splitter_drag.is_some() {
            subs.push(event::listen_with(splitter_event_filter));
        }

        // Single-instance accept loop. Hashes a fixed id so iced never
        // restarts the stream — the stream owns the only listener and
        // restarting would lose it. The stream factory is a free fn
        // (see `ipc_subscription_stream`) so the closure stays `Fn`.
        #[cfg(unix)]
        {
            subs.push(Subscription::run(ipc_subscription_stream));
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

        // Spark sp-ux0021: confirmation dialog when closing a workshop
        // with active Hands. Built once here and layered onto whichever
        // render path runs below.
        let close_dialog: Option<Element<'_, Message>> =
            self.pending_close_workshop.and_then(|idx| {
                let target = self.workshops.get(idx)?;
                let active_hands = target.agent_sessions.iter().filter(|s| s.active).count();
                let pal = match target.bg_is_dark {
                    Some(true) => style::Palette::dark(),
                    Some(false) => style::Palette::light(),
                    None => self.appearance.palette(),
                };
                Some(
                    screen::close_workshop_dialog::view(idx, target.name(), active_hands, &pal)
                        .map(|m| match m {
                            screen::close_workshop_dialog::Message::Confirm(i) => {
                                Message::ConfirmCloseWorkshop(i)
                            }
                            screen::close_workshop_dialog::Message::Cancel => {
                                Message::CancelCloseWorkshop
                            }
                        }),
                )
            });

        // Layer background image behind everything (including tab bar)
        if let Some(ws) = ws
            && (ws.background_handle.is_some() || ws.background_picker.open)
        {
            let mut layers: Vec<Element<'_, Message>> = Vec::new();

            // Prefer the picker preview thumbnail when the user is hovering
            // a candidate; otherwise show the committed background.
            let active_bg = ws
                .background_picker
                .preview_handle
                .as_ref()
                .or(ws.background_handle.as_ref());
            if let Some(handle) = active_bg {
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

            // Unsplash attribution chip — bottom-right, click opens the
            // photographer's profile. Spark sp-ux0033.
            if let Some(chip) = unsplash_attribution_overlay(ws) {
                layers.push(chip);
            }

            // Settings modal overlay
            if ws.background_picker.open {
                let has_bg = ws.config.background.image.is_some();
                let dim_opacity = ws.config.background.dim_opacity;
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
                            .is_some_and(|s| s.full_auto),
                        is_default: self.global_config.default_agent.as_ref() == Some(&a.command),
                    })
                    .collect();
                let terminal_font = screen::background_picker::TerminalFontInfo {
                    size: self.global_config.effective_terminal_font_size(),
                    family: self.global_config.terminal_font_family.clone(),
                };
                layers.push(
                    screen::background_picker::view(
                        &ws.background_picker,
                        &pal,
                        has_bg,
                        dim_opacity,
                        agents,
                        terminal_font,
                        self.global_config.delegation_visibility,
                    )
                    .map(Message::Background),
                );
            }

            if let Some(dialog) = close_dialog {
                layers.push(dialog);
            }

            let stacked: Element<'_, Message> = stack(layers)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            return overlay_with_toasts(stacked, toast_overlay);
        }

        if let Some(dialog) = close_dialog {
            let stacked: Element<'_, Message> = stack(vec![main_content, dialog])
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            return overlay_with_toasts(stacked, toast_overlay);
        }

        overlay_with_toasts(main_content, toast_overlay)
    }

    /// Top-level tab bar for workshops — liquid glass pill tabs.
    fn view_workshop_bar(&self) -> Element<'_, Message> {
        let pal = self.appearance.palette();
        let has_bg = self
            .active_workshop()
            .is_some_and(|ws| ws.background_handle.is_some());
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

    /// Welcome screen when no workshops are open. Onboards new users with a
    /// concept primer, recent-workshops list, drag-folder affordance and the
    /// keyboard shortcut for opening a workshop. Spark sp-ux0016.
    fn view_welcome(&self) -> Element<'_, Message> {
        let pal = self.appearance.palette();

        // ── Hero ────────────────────────────────────────────
        let hero = column![
            text("Ryve").size(48).color(pal.text_primary),
            text("A workshop for orchestrating coding agents.")
                .size(15)
                .color(pal.text_secondary),
        ]
        .spacing(6)
        .align_x(iced::Alignment::Center);

        // ── Concept primer ──────────────────────────────────
        // Three-line glossary so first-time users can map our vocabulary
        // ("Workshop / Hand / Spark") onto familiar concepts before they
        // open anything.
        let concept_row = |label: &'static str, body: &'static str| -> Element<'_, Message> {
            row![
                container(text(label).size(13).color(pal.text_primary)).width(Length::Fixed(96.0)),
                text(body).size(13).color(pal.text_secondary),
            ]
            .spacing(12)
            .into()
        };
        let concepts = container(
            column![
                concept_row("Workshop", "A project directory managed by Ryve."),
                concept_row("Spark", "A unit of work — feature, bug, or task."),
                concept_row("Hand", "A coding agent assigned to a spark."),
            ]
            .spacing(8),
        )
        .padding(16)
        .width(Length::Fixed(420.0))
        .style(move |_theme: &Theme| style::glass_panel(&pal, false));

        // ── Recent workshops ────────────────────────────────
        let recent_section: Element<'_, Message> = if self.global_config.recent_workshops.is_empty()
        {
            container(
                text("No recent workshops yet — open one to get started.")
                    .size(13)
                    .color(pal.text_secondary),
            )
            .padding(16)
            .width(Length::Fixed(420.0))
            .style(move |_theme: &Theme| style::glass_panel(&pal, false))
            .into()
        } else {
            let mut list =
                column![text("Recent workshops").size(12).color(pal.text_secondary),].spacing(8);
            for path in self.global_config.recent_workshops.iter().take(8) {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let parent = path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let row_content = column![
                    text(label).size(14).color(pal.text_primary),
                    text(parent).size(11).color(pal.text_secondary),
                ]
                .spacing(2);
                let entry = button(row_content)
                    .width(Length::Fill)
                    .padding([8, 12])
                    .style(button::text)
                    .on_press(Message::OpenWorkshopPath(path.clone()));
                list = list.push(entry);
            }
            container(list)
                .padding(12)
                .width(Length::Fixed(420.0))
                .style(move |_theme: &Theme| style::glass_panel(&pal, false))
                .into()
        };

        // ── Drag-folder affordance ──────────────────────────
        let drop_zone = container(
            column![
                text("Drop a folder here").size(14).color(pal.text_primary),
                text("…or use the button below to pick one.")
                    .size(12)
                    .color(pal.text_secondary),
            ]
            .spacing(4)
            .align_x(iced::Alignment::Center),
        )
        .padding(20)
        .width(Length::Fixed(420.0))
        .center_x(Length::Fill)
        .style(move |_theme: &Theme| {
            let mut s = style::glass_panel(&pal, false);
            s.border.width = 1.5;
            s.border.color = pal.text_secondary;
            s
        });

        // ── Open button + shortcut hint ─────────────────────
        let shortcut_hint = if cfg!(target_os = "macos") {
            "⌘O"
        } else {
            "Ctrl+O"
        };
        let actions = row![
            button(text("Open Workshop...").size(14))
                .style(button::primary)
                .padding([8, 20])
                .on_press(Message::NewWorkshopDialog),
            text(format!("or press {shortcut_hint}"))
                .size(12)
                .color(pal.text_secondary),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center);

        container(
            column![hero, concepts, recent_section, drop_zone, actions]
                .spacing(20)
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
        // Derive the delegation trace on demand so it always reflects the
        // latest poll-loaded state of hand_assignments / agent_sessions /
        // crews / crew_members / sparks rather than a snapshot taken at
        // BondsLoaded. Built unconditionally and cheaply (small filter +
        // sort over already-cached vectors); the binding lives for the
        // remainder of this function so the spark_detail::view borrow is
        // valid for the returned `Element`.
        let delegation = ws
            .selected_spark
            .as_deref()
            .map(|selected_id| {
                screen::delegation_trace::build_trace(
                    selected_id,
                    &ws.hand_assignments,
                    &ws.agent_sessions,
                    &ws.crews,
                    &ws.crew_members,
                    &ws.sparks,
                )
            })
            .unwrap_or_default();
        let sparks_panel = if ws.show_releases {
            screen::releases::view(&ws.release_view_data, &ws.releases_state, &pal, has_bg)
                .map(Message::Releases)
        } else if let Some(ref selected_id) = ws.selected_spark {
            if let Some(spark) = ws.sparks.iter().find(|s| s.id == *selected_id) {
                let description_draft = ws
                    .spark_edit
                    .as_ref()
                    .and_then(|e| e.drafts.get(&screen::spark_detail::Field::Description))
                    .map(|s| s.as_str());
                screen::spark_detail::view(
                    spark,
                    &ws.selected_spark_contracts,
                    &ws.selected_spark_bonds,
                    &ws.sparks,
                    &delegation,
                    &ws.contract_create_form,
                    &ws.acceptance_criteria_edit,
                    &ws.intent_list_drafts,
                    &ws.spark_edit_session,
                    ws.spark_edit.as_ref(),
                    &ws.assignee_edit,
                    &ws.agent_sessions,
                    ws.description_editor.as_ref(),
                    description_draft,
                    ws.pending_nav_prompt.as_ref(),
                    ws.problem_edit.as_ref(),
                    &pal,
                    has_bg,
                )
                .map(Message::SparkDetail)
            } else {
                screen::sparks::view(screen::sparks::ViewCtx {
                    sparks: &ws.sparks,
                    blocked_ids: &ws.blocked_spark_ids,
                    agent_sessions: &ws.agent_sessions,
                    pal,
                    has_bg,
                    create_form: &ws.spark_create_form,
                    status_menu: &ws.spark_status_menu,
                    collapsed: &ws.collapsed_epics,
                    refreshing: ws.sparks_refreshing,
                    filter: &ws.sparks_filter,
                    agent_session_names: &ws.agent_session_names,
                    filtered_sparks: &ws.filtered_sparks,
                    sort_mode: ws.sort_mode,
                    sort_dropdown_open: ws.sort_dropdown_open,
                })
                .map(Message::Sparks)
            }
        } else {
            screen::sparks::view(screen::sparks::ViewCtx {
                sparks: &ws.sparks,
                blocked_ids: &ws.blocked_spark_ids,
                agent_sessions: &ws.agent_sessions,
                pal,
                has_bg,
                create_form: &ws.spark_create_form,
                status_menu: &ws.spark_status_menu,
                collapsed: &ws.collapsed_epics,
                refreshing: ws.sparks_refreshing,
                filter: &ws.sparks_filter,
                agent_session_names: &ws.agent_session_names,
                filtered_sparks: &ws.filtered_sparks,
                sort_mode: ws.sort_mode,
                sort_dropdown_open: ws.sort_dropdown_open,
            })
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

        // Responsive layout: collapse side panels at small window widths
        // so the bench remains usable. sp-ux0025.
        let (show_sidebar, show_sparks) = Workshop::responsive_panels(self.window_size.width);

        let mut main_row_inner = row![].spacing(0).height(Length::Fill);
        if show_sidebar {
            main_row_inner = main_row_inner.push(sidebar).push(sidebar_bench_splitter);
        }
        main_row_inner = main_row_inner.push(bench);
        if show_sparks {
            main_row_inner = main_row_inner.push(bench_sparks_splitter).push(sparks_col);
        }

        let main_row = container(main_row_inner)
            .padding(style::PANEL_GAP)
            .width(Length::Fill)
            .height(Length::Fill);

        // Ember notification bar — sits above the main row so dismissible
        // Hand-to-Hand signals are visible without blocking the workgraph
        // panel. When there are no active embers the bar is skipped so it
        // costs zero vertical space. Spark sp-ux0008.
        let ember_bar = screen::ember_bar::view(&ws.embers, &pal).map(|e| e.map(Message::EmberBar));

        let workshop_content: Element<'a, Message> = match ember_bar {
            Some(bar) => column![bar, main_row, status_bar,]
                .height(Length::Fill)
                .into(),
            None => column![main_row, status_bar,].height(Length::Fill).into(),
        };

        // Layer background image behind content
        let mut layers: Vec<Element<'a, Message>> = Vec::new();

        let active_bg = ws
            .background_picker
            .preview_handle
            .as_ref()
            .or(ws.background_handle.as_ref());
        if let Some(handle) = active_bg {
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
            let dim_opacity = ws.config.background.dim_opacity;
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
                        .is_some_and(|s| s.full_auto),
                    is_default: self.global_config.default_agent.as_ref() == Some(&a.command),
                })
                .collect();
            let terminal_font = screen::background_picker::TerminalFontInfo {
                size: self.global_config.effective_terminal_font_size(),
                family: self.global_config.terminal_font_family.clone(),
            };
            layers.push(
                screen::background_picker::view(
                    &ws.background_picker,
                    &pal,
                    has_bg,
                    dim_opacity,
                    agents,
                    terminal_font,
                    self.global_config.delegation_visibility,
                )
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
                screen::head_picker::view(state, &ws.sparks, &self.available_agents, &pal)
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
        _has_bg: bool,
        pal: &style::Palette,
    ) -> Element<'a, Message> {
        screen::agents::view(
            &ws.agent_sessions,
            &ws.hand_assignments,
            &ws.crews,
            &ws.crew_members,
            &ws.sparks,
            &ws.agents_panel,
            *pal,
        )
        .map(Message::Agents)
    }

    fn view_bench<'a>(
        &'a self,
        ws: &'a Workshop,
        has_bg: bool,
        pal: &style::Palette,
    ) -> Element<'a, Message> {
        let tab_bar = ws.bench.view_tab_bar(pal).map(Message::Bench);

        let content: Element<'a, Message> = if let Some(active_id) = ws.bench.active_tab {
            let active_kind = ws
                .bench
                .tabs
                .iter()
                .find(|t| t.id == active_id)
                .map(|t| &t.kind);
            if matches!(active_kind, Some(screen::bench::TabKind::Home)) {
                screen::home::view(
                    screen::home::HomeData {
                        sparks: &ws.sparks,
                        agent_sessions: &ws.agent_sessions,
                        assignments: &ws.hand_assignments,
                        failing_contracts: &ws.failing_contracts_list,
                        embers: &ws.embers,
                    },
                    pal,
                    has_bg,
                )
                .map(Message::Home)
            } else if let Some(term) = ws.terminals.get(&active_id) {
                let term_view = iced_term::TerminalView::show_with_transparent_bg(term, has_bg)
                    .map(|e| Message::Bench(screen::bench::Message::TerminalEvent(e)));
                if let Some(search_bar) = ws.bench.view_terminal_search(pal) {
                    stack![term_view, search_bar.map(Message::Bench)]
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into()
                } else {
                    term_view
                }
            } else if let Some(viewer) = ws.file_viewers.get(&active_id) {
                file_viewer::view(viewer, &ws.directory, pal, has_bg).map(Message::FileViewer)
            } else if let Some(tail) = ws.log_tails.get(&active_id) {
                log_tail::view(tail, pal).map(Message::LogTail)
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

        // Overlay the dropdown menu on top of the content area. When it's
        // open, a full-size transparent backdrop sits between body and
        // menu so any click outside the menu dismisses it (sp-ux0022).
        if let Some(dropdown) =
            ws.bench
                .view_dropdown(&self.available_agents, &ws.custom_agents, pal)
        {
            let backdrop = ws
                .bench
                .view_dropdown_backdrop()
                .map(|b| b.map(Message::Bench))
                .unwrap_or_else(|| Space::new().width(Length::Fill).height(Length::Fill).into());
            stack![
                body,
                backdrop,
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
//
// Note: the previous `hotkey_for_keyboard_event` helper from
// hand/0e2ed795 ([sp-18253584]) was removed during the perf/p1
// integration merge — the regression-harness branch ([sp-961b4d5e])
// replaced it with `perf_core::classify_key_event`, which is exercised
// by the headless smoke test in `perf_core/tests/sparks_poll_smoke.rs`.
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

/// Format the chip label for an Unsplash attribution. Pure helper so the
/// rendering decision can be unit-tested without standing up an iced view.
/// Spark sp-ux0033.
fn unsplash_attribution_label(bg: &data::ryve_dir::BackgroundConfig) -> Option<String> {
    let photographer = bg.unsplash_photographer.as_deref()?.trim();
    if photographer.is_empty() {
        return None;
    }
    Some(format!("Photo by {photographer} on Unsplash"))
}

/// Render the translucent attribution chip in the bottom-right of the
/// workspace when an Unsplash image is the active background. Returns
/// `None` when there is no photographer credit on file (e.g. local
/// uploads), so the caller can skip pushing a layer entirely. Spark
/// sp-ux0033.
fn unsplash_attribution_overlay(ws: &Workshop) -> Option<Element<'_, Message>> {
    ws.background_handle.as_ref()?;
    let label = unsplash_attribution_label(&ws.config.background)?;
    let url = ws
        .config
        .background
        .unsplash_photographer_url
        .clone()
        .unwrap_or_else(|| "https://unsplash.com".to_string());

    let chip_text = text(label).size(11).color(Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 0.92,
    });

    let chip_button = button(chip_text)
        .style(button::text)
        .padding([4, 10])
        .on_press(Message::OpenUrl(url));

    let chip = container(chip_button)
        .padding(0)
        .style(|_theme: &Theme| style::attribution_chip());

    Some(
        container(chip)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right)
            .align_y(iced::alignment::Vertical::Bottom)
            .padding(12)
            .into(),
    )
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
async fn load_blocked_spark_ids(pool: sqlx::SqlitePool, workshop_id: String) -> HashSet<String> {
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

/// Load the full list of failing/pending required contracts for the Home
/// overview. Errors are swallowed since this is a non-critical display value.
async fn load_failing_contract_list(pool: sqlx::SqlitePool, workshop_id: String) -> Vec<Contract> {
    data::sparks::contract_repo::list_failing(&pool, &workshop_id)
        .await
        .unwrap_or_default()
}

/// Load all crews and their full membership join for a workshop. Used by
/// the Hands panel to render the Head → Crew → Hand hierarchy. Errors are
/// swallowed (treated as empty) since this is non-critical display data.
async fn load_crews(
    pool: sqlx::SqlitePool,
    workshop_id: String,
) -> (
    Vec<data::sparks::types::Crew>,
    Vec<data::sparks::types::CrewMember>,
) {
    let crews = data::sparks::crew_repo::list_for_workshop(&pool, &workshop_id)
        .await
        .unwrap_or_default();
    let mut all_members: Vec<data::sparks::types::CrewMember> = Vec::new();
    for c in &crews {
        if let Ok(mut members) = data::sparks::crew_repo::members(&pool, &c.id).await {
            all_members.append(&mut members);
        }
    }
    (crews, all_members)
}

/// Load all active hand assignments for the workshop, used by the Home
/// overview to join sparks ↔ Hands. Filters down to status='active' on
/// the SQL side already.
async fn load_hand_assignments(pool: sqlx::SqlitePool, workshop_id: String) -> Vec<HandAssignment> {
    // assignment_repo::list_active is workshop-agnostic — filter to this
    // workshop's sparks here so the Home view doesn't bleed across workshops
    // sharing the same database file.
    let all = data::sparks::assignment_repo::list_active(&pool)
        .await
        .unwrap_or_default();
    let workshop_spark_ids: std::collections::HashSet<String> = data::sparks::spark_repo::list(
        &pool,
        data::sparks::types::SparkFilter {
            workshop_id: Some(workshop_id),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|s| s.id)
    .collect();
    all.into_iter()
        .filter(|a| workshop_spark_ids.contains(&a.spark_id))
        .collect()
}

/// Load active embers for the Home overview.
async fn load_embers(pool: sqlx::SqlitePool, workshop_id: String) -> Vec<Ember> {
    data::sparks::ember_repo::list_active(&pool, &workshop_id)
        .await
        .unwrap_or_default()
}

/// Reconcile dead agent sessions: for each session whose process has
/// disappeared, mark the `agent_sessions` row as ended and transition all
/// active `hand_assignments` for that session to "abandoned".
///
/// Returns the list of session IDs that were reconciled. Errors on
/// individual sessions are logged but do not abort the rest of the batch.
///
/// Spark `ryve-a677498c`.
async fn reconcile_dead_sessions(
    pool: sqlx::SqlitePool,
    workshop_dir: PathBuf,
    session_ids: Vec<String>,
) -> Vec<String> {
    let mut reconciled = Vec::new();
    for session_id in &session_ids {
        // 1. End the agent_sessions row AND kill its tmux session.
        crate::tmux::terminate_session(&pool, &workshop_dir, session_id).await;
        // 2. Abandon all active assignments for this session.
        let assignments = data::sparks::assignment_repo::list_for_session(&pool, session_id)
            .await
            .unwrap_or_default();
        for a in assignments {
            if a.status == "active"
                && let Err(e) =
                    data::sparks::assignment_repo::abandon(&pool, session_id, &a.spark_id).await
            {
                log::warn!(
                    "Failed to abandon assignment for session {session_id} on spark {}: {e}",
                    a.spark_id
                );
            }
        }
        reconciled.push(session_id.clone());
    }
    reconciled
}

/// Auto-create an ember in response to a state transition detected during
/// the 3-second poll. Failures are logged but swallowed — missing a
/// notification must never break the poll loop. Spark sp-ux0008.
async fn create_ember_fire_and_forget(
    pool: sqlx::SqlitePool,
    workshop_id: String,
    ember_type: EmberType,
    content: String,
    source_agent: Option<String>,
) {
    if let Err(e) = data::sparks::ember_repo::create(
        &pool,
        NewEmber {
            ember_type,
            content,
            source_agent,
            workshop_id,
            ttl_seconds: Some(3600),
        },
    )
    .await
    {
        log::warn!("Failed to auto-create ember: {e}");
    }
}

/// Load all sparks for a workshop from the database.
async fn load_sparks(pool: sqlx::SqlitePool, workshop_id: String) -> Vec<Spark> {
    let mut sparks = data::sparks::spark_repo::list(
        &pool,
        data::sparks::types::SparkFilter {
            workshop_id: Some(workshop_id),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_default();

    // Spark ryve-dc66e998: parent-child relationships may live in the
    // `bonds` table (the CLI/Head path uses `ryve bond create ... parent_child`)
    // instead of the `sparks.parent_id` column. Fold bonds back onto each
    // spark's `parent_id` so the UI groupers (spark_picker) see a consistent
    // view regardless of which path created the edge.
    if let Ok(rows) = sqlx::query_as::<_, (String, String)>(
        "SELECT from_id, to_id FROM bonds WHERE bond_type = 'parent_child'",
    )
    .fetch_all(&pool)
    .await
    {
        use std::collections::HashMap;
        let mut child_to_parent: HashMap<String, String> = HashMap::new();
        for (parent, child) in rows {
            child_to_parent.entry(child).or_insert(parent);
        }
        for s in sparks.iter_mut() {
            if s.parent_id.is_none()
                && let Some(pid) = child_to_parent.get(&s.id)
            {
                s.parent_id = Some(pid.clone());
            }
        }
    }

    sparks
}

#[cfg(test)]
mod tests {
    use data::ryve_dir::BackgroundConfig;

    use super::*;

    #[test]
    fn attribution_label_present_when_photographer_set() {
        let bg = BackgroundConfig {
            image: Some("photo.jpg".into()),
            dim_opacity: 0.7,
            unsplash_photographer: Some("Jane Doe".into()),
            unsplash_photographer_url: Some("https://unsplash.com/@jane".into()),
        };
        assert_eq!(
            unsplash_attribution_label(&bg).as_deref(),
            Some("Photo by Jane Doe on Unsplash"),
        );
    }

    #[test]
    fn attribution_label_absent_for_local_upload() {
        let bg = BackgroundConfig {
            image: Some("local.jpg".into()),
            dim_opacity: 0.7,
            unsplash_photographer: None,
            unsplash_photographer_url: None,
        };
        assert!(unsplash_attribution_label(&bg).is_none());
    }

    // Note: hotkey filter unit tests from hand/0e2ed795 ([sp-18253584])
    // were removed during the perf/p1 integration merge. Their property
    // — that no key event ever resolves to SparksPoll — is now enforced
    // end to end by `perf_core/tests/sparks_poll_smoke.rs`, which drives
    // the same `perf_core::classify_key_event` classifier the live
    // subscription uses.

    /// Spark ryve-f65077f4: verify the predicate that gates Atlas
    /// availability — it should be false when the agent list is empty or
    /// every agent is unsupported.
    #[test]
    fn no_compatible_agent_when_all_unsupported_or_empty() {
        use coding_agents::{CodingAgent, CompatStatus, ResumeStrategy};

        let make = |compat: CompatStatus| CodingAgent {
            display_name: "test".into(),
            command: "test".into(),
            args: vec![],
            resume: ResumeStrategy::None,
            compatibility: compat,
        };

        // Empty list → no compatible agent.
        let agents: Vec<CodingAgent> = vec![];
        assert!(!agents.iter().any(|a| !a.compatibility.is_unsupported()));

        // All unsupported → no compatible agent.
        let agents = [make(CompatStatus::Unsupported {
            version: "0.1".into(),
            reason: "old".into(),
        })];
        assert!(!agents.iter().any(|a| !a.compatibility.is_unsupported()));

        // One compatible → has compatible agent.
        let agents = [make(CompatStatus::Compatible {
            version: "1.0".into(),
        })];
        assert!(agents.iter().any(|a| !a.compatibility.is_unsupported()));

        // Unknown counts as "not unsupported" — we give the benefit of
        // the doubt when the version probe couldn't run.
        let agents = [make(CompatStatus::Unknown)];
        assert!(agents.iter().any(|a| !a.compatibility.is_unsupported()));
    }

    #[test]
    fn attribution_label_absent_for_blank_photographer() {
        let bg = BackgroundConfig {
            image: Some("photo.jpg".into()),
            dim_opacity: 0.7,
            unsplash_photographer: Some("   ".into()),
            unsplash_photographer_url: None,
        };
        assert!(unsplash_attribution_label(&bg).is_none());
    }

    /// Spark `ryve-a677498c` acceptance criterion: create session, kill
    /// session externally, next reconcile pass marks the row stopped.
    ///
    /// This test exercises the full reconciliation flow:
    ///   1. Creates a workshop DB with an active agent session and an
    ///      active hand assignment.
    ///   2. Feeds the session's child_pid into `tmux::dead_sessions`
    ///      against an empty ProcessSnapshot (no processes alive).
    ///   3. Runs `reconcile_dead_sessions` with the detected dead IDs.
    ///   4. Asserts the `agent_sessions` row now has `ended_at` set, and
    ///      the `hand_assignments` row has `status = 'abandoned'`.
    #[tokio::test]
    async fn dead_session_reconciliation_marks_assignment_abandoned() {
        use data::sparks::types::{
            AssignmentRole, NewAgentSession, NewHandAssignment, NewSpark, SparkType,
        };
        use data::sparks::{agent_session_repo, assignment_repo, spark_repo};

        // 1. Set up a temporary workshop directory and DB.
        let tmp = std::env::temp_dir().join(format!("ryve-tmux-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let pool = data::db::open_sparks_db(&tmp).await.unwrap();

        let ws_id = tmp.file_name().unwrap().to_string_lossy().into_owned();

        // Create a spark for the assignment.
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "dead-session test spark".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
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
            },
        )
        .await
        .unwrap();

        // Create an active agent session with a fake child_pid that is
        // definitely not running (PID 2 on macOS is always a kernel
        // process that won't match; on Linux it's kthreadd which the
        // default sysinfo snapshot won't include in user-space).
        let session_id = uuid::Uuid::new_v4().to_string();
        let dead_pid: i64 = 999_999_999; // certainly not alive
        agent_session_repo::create(
            &pool,
            &NewAgentSession {
                id: session_id.clone(),
                workshop_id: ws_id.clone(),
                agent_name: "stub".into(),
                agent_command: "stub".into(),
                agent_args: Vec::new(),
                session_label: Some("hand".into()),
                child_pid: Some(dead_pid),
                resume_id: None,
                log_path: Some("/tmp/fake.log".into()),
                parent_session_id: None,
            },
        )
        .await
        .unwrap();

        // Create an active assignment.
        assignment_repo::assign(
            &pool,
            NewHandAssignment {
                session_id: session_id.clone(),
                spark_id: spark.id.clone(),
                role: AssignmentRole::Owner,
            },
        )
        .await
        .unwrap();

        // 2. Use tmux::dead_sessions against a default (empty) snapshot —
        //    no process is alive, so the session should be reported dead.
        let tracked = vec![(session_id.clone(), Some(dead_pid))];
        let empty_snap = process_snapshot::ProcessSnapshot::default();
        let dead = tmux::dead_sessions(&tracked, &empty_snap);
        assert_eq!(
            dead,
            vec![session_id.clone()],
            "session must be reported dead"
        );

        // 3. Run the reconciliation.
        let reconciled =
            reconcile_dead_sessions(pool.clone(), PathBuf::from("/tmp/ryve-test-fake"), dead).await;
        assert_eq!(reconciled, vec![session_id.clone()]);

        // 4. Verify: session ended, assignment abandoned.
        let sessions = agent_session_repo::list_for_workshop(&pool, &ws_id)
            .await
            .unwrap();
        let sess = sessions.iter().find(|s| s.id == session_id).unwrap();
        assert!(
            sess.ended_at.is_some(),
            "agent_sessions.ended_at must be set after reconciliation"
        );
        assert_eq!(
            sess.status, "ended",
            "agent_sessions.status must be 'ended'"
        );

        let assignments = assignment_repo::list_for_session(&pool, &session_id)
            .await
            .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(
            assignments[0].status, "abandoned",
            "hand_assignments.status must be 'abandoned' after reconciliation"
        );

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
