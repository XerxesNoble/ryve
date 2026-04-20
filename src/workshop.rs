// SPDX-License-Identifier: AGPL-3.0-or-later

//! A Workshop is a self-contained workspace bound to a directory.
//! Each workshop has its own `.ryve/` directory containing config,
//! sparks database, agent definitions, and context files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use data::agent_context::SyncCache as AgentContextSyncCache;
use data::ryve_dir::{AgentDef, RyveDir, UiState, WorkshopConfig};
use data::sparks::types::{Bond, Contract, Crew, CrewMember, Ember, HandAssignment, Spark};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::coding_agents::CodingAgent;
use crate::panel_state::agents::AgentSession;
use crate::panel_state::background_picker::PickerState;
use crate::panel_state::bench::{BenchState, TabKind};
use crate::panel_state::file_explorer::FileExplorerState;
use crate::panel_state::file_viewer::FileViewerState;
use crate::panel_state::irc_view::IrcViewState;
use crate::panel_state::log_tail::LogTailState;
use crate::process_snapshot::ProcessSnapshot;
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

/// Parameters captured when a Hand spawn begins (stage 1) so the terminal
/// can be constructed later, once the async worktree-creation task reports
/// back via `HandWorktreeReady`. Spark ryve-885ed3eb: deferring the
/// `iced_term::Terminal::new` call lets `App::update` return immediately
/// instead of blocking on `git worktree add`.
pub struct PendingTerminalSpawn {
    pub session_id: String,
    pub kind: PendingTerminalKind,
    pub full_auto: bool,
    /// Resolved system-prompt flag + value for the coding agent, if any.
    /// `(flag, value)` — e.g. `("--system-prompt", "/path/to/WORKSHOP.md")`
    /// for file-based agents or `("--system-prompt", "<inline text>")`
    /// for agents that want the prompt body on the command line. Computed
    /// at stage 1 so stage 2 doesn't have to touch the filesystem.
    pub system_prompt: Option<(String, String)>,
    /// Initial user-message prompt to type into the terminal once the agent
    /// subprocess is ready to accept input. Delivered by the
    /// `HandWorktreeReady` handler after `finalize_hand_terminal` inserts
    /// the terminal — removes the old spawn-time timer race where the
    /// prompt could fire before the terminal existed and be silently
    /// dropped.
    pub initial_prompt: Option<String>,
}

/// Outcome of [`Workshop::finalize_hand_terminal`] — lets the caller both
/// react to success/failure and learn whether an initial prompt needs to
/// be dispatched now that the terminal exists.
pub struct FinalizedTerminal {
    pub created: bool,
    pub initial_prompt: Option<String>,
}

pub enum PendingTerminalKind {
    /// Regular built-in coding agent (claude/codex/aider/opencode).
    Agent(CodingAgent),
    /// Custom agent defined under `.ryve/agents/`.
    CustomAgent(AgentDef),
}

/// A set of field edits to apply to a [`Spark`]. Each `Some` field is a
/// write; `None` means "leave alone". Produced by the detail view (and
/// anything else that edits a spark), then handed to
/// `Message::SparkUpdate` for optimistic apply + durable persist.
///
/// Spark ryve-90174007: every editable field goes through this single
/// patch type so the UI can apply the write optimistically to
/// `Workshop::sparks` and, on async failure, restore the prior values
/// from a symmetric patch (see [`Workshop::apply_spark_patch`]).
///
/// Field types mirror the cached [`Spark`] representation — strings for
/// status / spark_type / risk_level — so patches can round-trip through
/// the cache without an enum re-parse on every apply. The async handler
/// converts to `data::sparks::types::UpdateSpark` at the DB boundary via
/// [`SparkPatch::to_update_spark`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SparkPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<i32>,
    pub spark_type: Option<String>,
    pub assignee: Option<Option<String>>,
    pub owner: Option<Option<String>>,
    pub risk_level: Option<Option<String>>,
    pub scope_boundary: Option<Option<String>>,
    /// Structured intent: problem statement (lives in metadata JSON under
    /// `intent.problem_statement`). `None` = don't touch; `Some("")` =
    /// clear the field. Spark ryve-a5997352.
    pub problem_statement: Option<String>,
}

impl SparkPatch {
    /// Returns true when no fields are set — nothing to write.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.status.is_none()
            && self.priority.is_none()
            && self.spark_type.is_none()
            && self.assignee.is_none()
            && self.owner.is_none()
            && self.risk_level.is_none()
            && self.scope_boundary.is_none()
            && self.problem_statement.is_none()
    }

    /// Return the set of [`Field`](crate::panel_state::spark_detail::Field)s
    /// affected by this patch. Used by `SparkUpdateApplied` /
    /// `SparkUpdateFailed` to scope in-flight bookkeeping to just the
    /// fields that were part of a specific write.
    pub fn affected_fields(&self) -> Vec<crate::panel_state::spark_detail::Field> {
        use crate::panel_state::spark_detail::Field;
        let mut out = Vec::new();
        if self.title.is_some() {
            out.push(Field::Title);
        }
        if self.description.is_some() {
            out.push(Field::Description);
        }
        if self.priority.is_some() {
            out.push(Field::Priority);
        }
        if self.spark_type.is_some() {
            out.push(Field::Type);
        }
        if self.assignee.is_some() {
            out.push(Field::Assignee);
        }
        if self.problem_statement.is_some() {
            out.push(Field::Problem);
        }
        out
    }

    /// Convert the patch into a `data::sparks::types::UpdateSpark` suitable
    /// for `spark_repo::update`. Unknown enum values (a status string that
    /// does not match any `SparkStatus`, a spark_type string that does not
    /// match any `SparkType`, etc.) are dropped rather than persisted — the
    /// optimistic apply already validated the cache shape, and the DB
    /// constraint would reject garbage anyway.
    pub fn to_update_spark(&self) -> data::sparks::types::UpdateSpark {
        use data::sparks::types::{RiskLevel, SparkStatus, SparkType, UpdateSpark};
        let spark_type = self.spark_type.as_deref().and_then(|s| match s {
            "bug" => Some(SparkType::Bug),
            "feature" => Some(SparkType::Feature),
            "task" => Some(SparkType::Task),
            "epic" => Some(SparkType::Epic),
            "chore" => Some(SparkType::Chore),
            "spike" => Some(SparkType::Spike),
            "milestone" => Some(SparkType::Milestone),
            _ => None,
        });
        // risk_level is `Option<Option<String>>`: outer Some means "write this
        // field", inner None means "clear it". Only the write case needs a
        // parse; the clear case is passed through unchanged. We can't express
        // "clear" with the current `UpdateSpark::risk_level` shape
        // (`Option<RiskLevel>`), so clearing is a no-op here — risk_level is
        // stored as NOT NULL with a 'normal' default, so clearing is not a
        // meaningful operation and the UI does not expose it.
        let risk_level = self.risk_level.as_ref().and_then(|o| {
            o.as_deref().and_then(|s| match s {
                "trivial" => Some(RiskLevel::Trivial),
                "normal" => Some(RiskLevel::Normal),
                "elevated" => Some(RiskLevel::Elevated),
                "critical" => Some(RiskLevel::Critical),
                _ => None,
            })
        });
        UpdateSpark {
            title: self.title.clone(),
            description: self.description.clone(),
            status: self.status.as_deref().and_then(SparkStatus::from_str),
            priority: self.priority,
            spark_type,
            assignee: self.assignee.clone(),
            owner: self.owner.clone(),
            risk_level,
            scope_boundary: self.scope_boundary.clone(),
            ..Default::default()
        }
    }
}

/// A deferred selection change, held on the workshop while the user
/// decides what to do with an unsaved description draft. Produced by
/// [`Workshop::try_change_selected_spark`] when the current spark has
/// a dirty description draft; consumed when the user resolves the
/// "Save / Discard / Cancel" dialog. Spark ryve-4742d98b.
#[derive(Debug, Clone)]
pub struct PendingNavPrompt {
    /// The selection the user was trying to move to. `None` means
    /// "back to the sparks panel".
    pub target: Option<String>,
    /// The id of the spark whose description is dirty. Kept separately
    /// so the dialog can still name the spark even if the underlying
    /// `selected_spark` has already changed for any reason.
    pub dirty_spark_id: String,
}

pub struct Workshop {
    pub id: Uuid,
    pub directory: PathBuf,
    pub ryve_dir: Arc<RyveDir>,
    pub config: Arc<WorkshopConfig>,
    pub bench: BenchState,
    pub terminals: HashMap<u64, iced_term::Terminal>,
    /// Hand terminals whose worktree is being created asynchronously. Keyed
    /// by `tab_id`; the entry is consumed when the worktree task reports
    /// back and the real `iced_term::Terminal` can be constructed.
    /// Spark ryve-885ed3eb.
    pub pending_terminal_spawns: HashMap<u64, PendingTerminalSpawn>,
    pub agent_sessions: Vec<AgentSession>,
    /// Open file viewer states, keyed by tab ID.
    pub file_viewers: HashMap<u64, FileViewerState>,
    /// Open spy views (read-only log tails for background Hands), keyed by
    /// tab ID. Spark ryve-8c14734a.
    pub log_tails: HashMap<u64, LogTailState>,
    /// Open IRC projection views, keyed by tab ID. Spark ryve-5466c372 —
    /// each entry owns the per-tab filter form, loaded messages, preset
    /// sidebar state, and scroll cursor so the view renderer stays pure.
    pub irc_views: HashMap<u64, IrcViewState>,
    /// File explorer state for this workshop.
    pub file_explorer: FileExplorerState,
    /// Workgraph database for this workshop.
    pub sparks_db: Option<SqlitePool>,
    /// Cached sparks for display (loaded from DB).
    pub sparks: Vec<Spark>,
    /// True while an explicit Refresh-button refetch is in flight. Drives
    /// the Workgraph panel's refresh button indicator so users get visible
    /// feedback that their click did something. Cleared on the next
    /// `SparksLoaded` (whether from the Refresh or the 3s poll).
    /// Spark ryve-7805b38b.
    pub sparks_refreshing: bool,
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
    pub agents_panel: crate::panel_state::agents::AgentsPanelState,
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
    /// Status filter for the sparks panel. Pill state mirrors this directly.
    pub sparks_filter: crate::panel_state::sparks::SparksFilter,
    /// Inline spark create form state.
    pub spark_create_form: crate::panel_state::sparks::CreateForm,
    /// Inline status popover state for the workgraph panel.
    pub spark_status_menu: crate::panel_state::sparks::StatusMenu,
    /// Cached agent session names for the filter bar assignee dropdown.
    /// Refreshed alongside `agent_sessions`. Spark ryve-baca34b0.
    pub agent_session_names: Vec<String>,
    /// Sparks after applying `sparks_filter`. Recomputed whenever
    /// `sparks` or `sparks_filter` changes. Spark ryve-baca34b0.
    pub filtered_sparks: Vec<Spark>,
    /// Active sort mode for the sparks panel. Spark ryve-6f24ef2a.
    pub sort_mode: crate::panel_state::sparks::SortMode,
    /// Whether the sort mode dropdown is currently open. Spark ryve-6f24ef2a.
    pub sort_dropdown_open: bool,
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
    pub contract_create_form: crate::panel_state::spark_detail::ContractCreateForm,
    /// Per-spark inline-edit state. `None` when no spark is currently
    /// being edited; replaced (not merged) when the selected spark
    /// changes. Invariant: at most one `SparkEdit` per workshop at a
    /// time — see [`Workshop::change_selected_spark`]. Spark
    /// ryve-1d8c2847.
    pub spark_edit: Option<crate::panel_state::spark_detail::SparkEdit>,
    pub acceptance_criteria_edit: crate::panel_state::spark_detail::AcceptanceCriteriaEdit,
    pub intent_list_drafts: crate::panel_state::intent_list_editor::IntentListDrafts,
    pub spark_edit_session: crate::panel_state::spark_detail::SparkEditSession,
    pub assignee_edit: crate::panel_state::spark_detail::AssigneeEditState,
    pub description_editor: Option<iced::widget::text_editor::Content>,
    pub pending_nav_prompt: Option<PendingNavPrompt>,
    /// Active multi-line problem-statement editor, if any. Spark ryve-a5997352.
    pub problem_edit: Option<crate::panel_state::spark_detail::ProblemEditState>,
    /// Whether the releases panel is shown instead of the sparks panel.
    pub show_releases: bool,
    /// Cached release view data for display. Refreshed alongside sparks.
    pub release_view_data: Vec<crate::panel_state::releases::ReleaseViewData>,
    /// UI state for the releases panel.
    pub releases_state: crate::panel_state::releases::ReleasesState,
    /// Whether the background image is dark (for adaptive font color).
    /// `None` means no background or not yet computed.
    pub bg_is_dark: Option<bool>,
    /// Pending agent spawn -- shows spark picker before creating terminal.
    pub pending_agent_spawn: Option<PendingAgentSpawn>,
    /// Pending Head spawn -- shows the Head picker overlay (agent + goal).
    pub pending_head_spawn: Option<crate::panel_state::head_picker::PickerState>,
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
    /// Cached hashes of files written by `data::agent_context::sync` so the
    /// 3-second sync tick produces zero file writes when nothing has
    /// changed. Spark ryve-86b0b326.
    pub agent_context_sync_cache: Arc<Mutex<AgentContextSyncCache>>,
    /// IDs of epics the user has collapsed in the workgraph panel. Default
    /// semantics: only collapsed IDs are stored, so newly-observed epics
    /// render expanded. Persisted to `.ryve/ui_state.json` per workshop so
    /// the panel survives restart; stale IDs (epics deleted between runs)
    /// are pruned on load. Spark ryve-926870a9.
    pub collapsed_epics: HashSet<String>,
    /// Cached spark summary for the status bar. Recomputed on `SparksLoaded`
    /// instead of every frame. Spark ryve-252c5b6e.
    pub cached_spark_summary: crate::screen::status_bar::SparkSummary,
    /// Cached git diff stats for the status bar. Recomputed on `FilesScanned`
    /// instead of every frame. Spark ryve-252c5b6e.
    pub cached_git_stats: crate::screen::status_bar::GitStats,
    /// Cached active-hand count for the status bar. Recomputed on
    /// `AgentSessionsLoaded` instead of every frame. Spark ryve-252c5b6e.
    pub cached_active_hands: usize,
    /// Cached total (non-stale) hand count for the status bar. Recomputed on
    /// `AgentSessionsLoaded` instead of every frame. Spark ryve-252c5b6e.
    pub cached_total_hands: usize,
    /// Handle to the workshop's watch scheduler task. `Some` once the
    /// sqlx pool has been opened and the task spawned; `None` before the
    /// pool is ready or after the workshop has been torn down. Owned
    /// here so `do_close_workshop` can await graceful shutdown.
    /// Spark ryve-6ab1980c [sp-ee3f5c74].
    pub watch_runner: Option<crate::watch_runner::WatchRunnerHandle>,
    /// Handle to the workshop's IRC subsystem. Populated asynchronously
    /// after connect + channel bootstrap succeed; `None` when IRC is
    /// disabled (no `irc_server` configured) or while the boot task is
    /// still running. Held inside a mutex so the async boot task can
    /// install the runtime from another thread once it's ready.
    /// Spark ryve-5a0e1d97 [sp-ddf6fd7f].
    pub irc_runtime: Arc<Mutex<Option<ipc::lifecycle::IrcRuntime>>>,
    /// IDs of epics we've already called `ensure_epic_channel` for since
    /// IRC came up. Used to avoid re-joining on every sparks-poll tick
    /// while still catching newly-created epics without a dedicated
    /// signal. Cleared whenever the IRC runtime is torn down.
    /// Spark ryve-5a0e1d97.
    pub irc_known_epic_ids: HashSet<String>,
    /// Which secondary screen, if any, is currently rendered as a modal
    /// overlay. `None` is the steady state — the workshop just shows the
    /// sparks/bench layout. Switching between screens is mutually
    /// exclusive: opening Settings closes Integrations and vice versa,
    /// so the user is never staring at two stacked modals. Spark
    /// ryve-c3de335e.
    pub active_overlay: WorkshopOverlay,
    /// Form drafts for the integrations settings screen. Persisted on
    /// the workshop (rather than rebuilt every view) so the inputs
    /// don't lose state between renders. Spark ryve-c3de335e.
    pub settings_form: crate::screen::settings::SettingsFormState,
}

/// Which secondary screen the workshop is currently showing as an
/// overlay. The variants stay flat so the UI can dispatch a single
/// match in [`crate::app::App::view_workshop`]. Spark ryve-c3de335e.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkshopOverlay {
    /// No secondary screen is open — the default state.
    #[default]
    None,
    /// Settings form for IRC server + GitHub credentials.
    Settings,
    /// IRC + GitHub health / detail view.
    Integrations,
}

impl Workshop {
    pub fn new(directory: PathBuf) -> Self {
        let ryve_dir = Arc::new(RyveDir::new(&directory));
        Self {
            id: Uuid::new_v4(),
            directory,
            ryve_dir,
            config: Arc::new(WorkshopConfig::default()),
            bench: BenchState::new(),
            terminals: HashMap::new(),
            pending_terminal_spawns: HashMap::new(),
            agent_sessions: Vec::new(),
            file_viewers: HashMap::new(),
            log_tails: HashMap::new(),
            irc_views: HashMap::new(),
            file_explorer: FileExplorerState::new(),
            sparks_db: None,
            sparks: Vec::new(),
            sparks_refreshing: false,
            failing_contracts: 0,
            failing_contracts_list: Vec::new(),
            hand_assignments: Vec::new(),
            crews: Vec::new(),
            crew_members: Vec::new(),
            agents_panel: crate::panel_state::agents::AgentsPanelState::default(),
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
            sparks_filter: Default::default(),
            spark_create_form: Default::default(),
            spark_status_menu: Default::default(),
            agent_session_names: Vec::new(),
            filtered_sparks: Vec::new(),
            sort_mode: Default::default(),
            sort_dropdown_open: false,
            selected_spark: None,
            selected_spark_contracts: Vec::new(),
            selected_spark_bonds: Vec::new(),
            blocked_spark_ids: HashSet::new(),
            contract_create_form: Default::default(),
            spark_edit: None,
            acceptance_criteria_edit: Default::default(),
            intent_list_drafts: Default::default(),
            spark_edit_session: Default::default(),
            assignee_edit: Default::default(),
            description_editor: None,
            pending_nav_prompt: None,
            problem_edit: None,
            show_releases: false,
            release_view_data: Vec::new(),
            releases_state: Default::default(),
            bg_is_dark: None,
            pending_agent_spawn: None,
            pending_head_spawn: None,
            last_worktree_warning: None,
            tabs_restored: false,
            appearance: Appearance::Dark,
            terminal_font_size: data::config::DEFAULT_TERMINAL_FONT_SIZE,
            terminal_font_family: None,
            agent_context_sync_cache: Arc::new(Mutex::new(AgentContextSyncCache::new())),
            collapsed_epics: HashSet::new(),
            cached_spark_summary: Default::default(),
            cached_git_stats: Default::default(),
            cached_active_hands: 0,
            cached_total_hands: 0,
            watch_runner: None,
            irc_runtime: Arc::new(Mutex::new(None)),
            irc_known_epic_ids: HashSet::new(),
            active_overlay: WorkshopOverlay::None,
            settings_form: crate::screen::settings::SettingsFormState::default(),
        }
    }

    // ── Settings / Integrations overlays (spark ryve-c3de335e) ────

    /// Open the integrations settings form. Seeds the form drafts from
    /// the current config so the inputs reflect the persisted values.
    /// Idempotent — re-opening just refreshes the drafts.
    pub fn open_settings(&mut self) {
        self.settings_form.seed_from(&self.config);
        self.active_overlay = WorkshopOverlay::Settings;
    }

    /// Open the integrations detail / health view. Closes any other
    /// overlay (the two are mutually exclusive).
    pub fn open_integrations(&mut self) {
        self.active_overlay = WorkshopOverlay::Integrations;
    }

    /// Close any open overlay and return to the main workshop view.
    pub fn close_overlay(&mut self) {
        self.active_overlay = WorkshopOverlay::None;
    }

    /// Snapshot the IRC health for the integrations view. The
    /// `runtime_active` flag must be polled by the caller from the
    /// mutex-protected `irc_runtime` slot, since the renderer cannot
    /// block. (PR #49 Copilot c7: the mutex is a `std::sync::Mutex`,
    /// not a tokio async one — earlier wording implied async-aware
    /// locking semantics that do not apply here.)
    pub fn integrations_irc_health(
        &self,
        runtime_active: bool,
    ) -> crate::screen::integrations::IrcHealth {
        crate::screen::integrations::irc_health_from(
            &self.config,
            runtime_active,
            self.irc_known_epic_ids.len(),
        )
    }

    /// Snapshot the GitHub health for the integrations view.
    pub fn integrations_github_health(&self) -> crate::screen::integrations::GitHubHealth {
        crate::screen::integrations::github_health_from(&self.config)
    }

    // ── Cached aggregations (spark ryve-252c5b6e) ──────────

    /// Recompute the spark summary from `self.sparks`. Call after
    /// `SparksLoaded` replaces the spark list.
    pub fn recompute_spark_summary(&mut self) {
        let mut s = crate::screen::status_bar::SparkSummary::default();
        for spark in &self.sparks {
            match spark.status.as_str() {
                "open" => s.open += 1,
                "in_progress" => s.in_progress += 1,
                "blocked" => s.blocked += 1,
                "deferred" => s.deferred += 1,
                "closed" => s.closed += 1,
                _ => {}
            }
        }
        self.cached_spark_summary = s;
    }

    /// Recompute git diff stats from the file explorer state. Call after
    /// `FilesScanned` replaces git_statuses/diff_stats.
    pub fn recompute_git_stats(&mut self) {
        let mut gs = crate::screen::status_bar::GitStats::default();
        for stat in self.file_explorer.diff_stats.values() {
            gs.additions += stat.additions;
            gs.deletions += stat.deletions;
        }
        gs.changed_files = self.file_explorer.git_statuses.len();
        self.cached_git_stats = gs;
    }

    /// Recompute active/total hand counts from agent sessions. Call after
    /// `AgentSessionsLoaded` updates the session list.
    pub fn recompute_hand_counts(&mut self) {
        self.cached_active_hands = self.agent_sessions.iter().filter(|a| a.active).count();
        self.cached_total_hands = self.agent_sessions.iter().filter(|a| !a.stale).count();
    }

    // ── Sort mode (spark ryve-6f24ef2a) ─────────────────────

    /// Re-sort `self.sparks` in place according to the active `sort_mode`.
    /// Called after loading sparks from DB and after changing the sort mode.
    pub fn sort_sparks(&mut self) {
        use crate::panel_state::sparks::{SortMode, spark_type_rank, status_rank};
        match self.sort_mode {
            SortMode::Default => self.sparks.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| {
                        spark_type_rank(&a.spark_type).cmp(&spark_type_rank(&b.spark_type))
                    })
                    .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
                    .then_with(|| a.id.cmp(&b.id))
            }),
            SortMode::PriorityOnly => {
                self.sparks
                    .sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
            }
            SortMode::RecentlyUpdated => {
                self.sparks.sort_by(|a, b| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| a.id.cmp(&b.id))
                });
            }
            SortMode::TypeFirst => self.sparks.sort_by(|a, b| {
                spark_type_rank(&a.spark_type)
                    .cmp(&spark_type_rank(&b.spark_type))
                    .then_with(|| a.priority.cmp(&b.priority))
                    .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
                    .then_with(|| a.id.cmp(&b.id))
            }),
        }
    }

    // ── Collapsed epic state (spark ryve-926870a9) ──────────

    /// Recompute `filtered_sparks` from `sparks` and `sparks_filter`.
    /// Must be called after any mutation to either. Spark ryve-baca34b0.
    pub fn recompute_filtered_sparks(&mut self) {
        if self.sparks_filter.is_empty() {
            self.filtered_sparks = self.sparks.clone();
        } else {
            self.filtered_sparks = self
                .sparks
                .iter()
                .filter(|s| self.sparks_filter.matches_spark(s))
                .cloned()
                .collect();
        }
    }

    /// Flip the collapse state of the given epic. Returns `true` if the
    /// epic is now collapsed. Callers are expected to persist the updated
    /// UI state using [`Workshop::ui_state_snapshot`], with the actual
    /// save occurring at the existing persistence call sites.
    pub fn toggle_epic_collapse(&mut self, epic_id: &str) -> bool {
        if self.collapsed_epics.remove(epic_id) {
            false
        } else {
            self.collapsed_epics.insert(epic_id.to_string());
            true
        }
    }

    /// Is the given epic currently collapsed? Consumed by the sparks
    /// panel chevron/grouping renderer (spark ryve-8be256a8); exposed
    /// now so the blocked render-side spark can wire up without having
    /// to poke at `collapsed_epics` directly.
    #[allow(dead_code)]
    pub fn is_epic_collapsed(&self, epic_id: &str) -> bool {
        self.collapsed_epics.contains(epic_id)
    }

    /// Drop any collapsed-epic IDs that are not present in
    /// `live_epic_ids`. Called after every sparks reload so epics deleted
    /// on another machine or in another session silently disappear from
    /// the set instead of accumulating indefinitely. Returns `true` if
    /// anything was removed — the caller can use that to decide whether
    /// to persist the cleaned snapshot.
    ///
    /// Takes a pre-computed id set rather than `&[Spark]` so the
    /// `SparksLoaded` handler can build it from `self.sparks` and then
    /// call this method without tripping the borrow checker.
    pub fn prune_collapsed_epics(&mut self, live_epic_ids: &HashSet<String>) -> bool {
        if self.collapsed_epics.is_empty() {
            return false;
        }
        let before = self.collapsed_epics.len();
        self.collapsed_epics.retain(|id| live_epic_ids.contains(id));
        self.collapsed_epics.len() != before
    }

    /// Convenience wrapper: extract the live epic id set from a `Spark`
    /// slice. Kept next to [`Workshop::prune_collapsed_epics`] because the
    /// two are always used together (and the extraction rule —
    /// `spark_type == "epic"` — must stay in sync).
    pub fn live_epic_ids(sparks: &[Spark]) -> HashSet<String> {
        sparks
            .iter()
            .filter(|s| s.spark_type == "epic")
            .map(|s| s.id.clone())
            .collect()
    }

    /// Build a fresh [`UiState`] snapshot from the workshop's current UI
    /// fields. Used by the save side-effect helpers.
    pub fn ui_state_snapshot(&self) -> UiState {
        UiState {
            version: 1,
            collapsed_epics: self.collapsed_epics.clone(),
            sparks_filter: self.sparks_filter.to_persisted_with_sort(self.sort_mode),
        }
    }

    /// Rehydrate the intent-list drafts from the currently-selected spark.
    /// Called on selection change so the editor starts from the latest
    /// persisted state. If no spark is selected, the drafts are cleared.
    /// Spark ryve-212c63aa.
    pub fn reseed_intent_drafts(&mut self) {
        let Some(ref sid) = self.selected_spark else {
            self.intent_list_drafts.clear();
            return;
        };
        if let Some(sp) = self.sparks.iter().find(|s| &s.id == sid) {
            self.intent_list_drafts.seed_from(sp);
        } else {
            self.intent_list_drafts.clear();
        }
    }

    /// Rehydrate drafts only when the selection has *changed* relative
    /// to what's currently in the draft buffer. Called on the periodic
    /// spark poll so an in-flight keystroke isn't clobbered every 3
    /// seconds — the drafts only reset when the user actually picks a
    /// different spark. Spark ryve-212c63aa.
    pub fn reseed_intent_drafts_if_selection_changed(&mut self) {
        let selected = self.selected_spark.as_deref();
        let draft_id = self.intent_list_drafts.spark_id.as_deref();
        if selected == draft_id {
            return;
        }
        self.reseed_intent_drafts();
    }

    /// Build the iced_term font settings used when spawning a new terminal
    /// or coding-agent pane. Reads the workshop's currently-effective
    /// terminal font size and family. Spark sp-ux0014.
    pub fn terminal_font_settings(&self) -> iced_term::settings::FontSettings {
        let font_type = match &self.terminal_font_family {
            Some(name) => iced::Font {
                family: iced::font::Family::Name(crate::font_intern::intern(name)),
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

    /// Change the currently selected spark, clearing the inline-edit
    /// state as required by the spark-detail edit epic (ryve-1d8c2847).
    ///
    /// Returns the previous [`SparkEdit`] **if it was dirty** so the
    /// caller can surface a "discard unsaved changes?" prompt to the
    /// user. A non-dirty edit (or no edit at all) returns `None` and is
    /// dropped silently. The selected spark is always updated before
    /// this method returns — rollback of the selection change is the
    /// UI layer's problem once the prompt lands.
    pub fn change_selected_spark(
        &mut self,
        new: Option<String>,
    ) -> Option<crate::panel_state::spark_detail::SparkEdit> {
        let discarded = self
            .spark_edit
            .take()
            .filter(crate::panel_state::spark_detail::SparkEdit::is_dirty);
        self.description_editor = None;
        // Problem editor is bound to a specific spark; clear it unless
        // the caller re-selected the same spark.
        if self.problem_edit.as_ref().map(|e| &e.spark_id) != new.as_ref() {
            self.problem_edit = None;
        }
        self.selected_spark = new;
        discarded
    }

    /// Attempt to change the selected spark. If the current spark has
    /// any dirty draft (description, title, etc.) the change is deferred
    /// and a [`PendingNavPrompt`] is stashed so the UI can render a
    /// save/discard/cancel dialog. Returns `true` if the navigation
    /// happened immediately, `false` if the dialog is now blocking.
    /// Spark ryve-4742d98b.
    pub fn try_change_selected_spark(&mut self, new: Option<String>) -> bool {
        // Guard on any dirty field, not just description. In-flight
        // writes always count as dirty. For drafts, only count those
        // whose value actually differs from the persisted spark so
        // that opening an editor without typing anything does not
        // trap the user.
        if let Some(ref edit) = self.spark_edit
            && self.has_unsaved_edits(edit)
        {
            self.pending_nav_prompt = Some(PendingNavPrompt {
                target: new,
                dirty_spark_id: edit.spark_id.clone(),
            });
            return false;
        }
        let _ = self.change_selected_spark(new);
        true
    }

    /// Check whether `edit` has any in-flight writes or drafts that
    /// differ from the persisted spark. A draft whose value matches
    /// the persisted field is not considered unsaved — the user
    /// merely opened the editor without typing.
    fn has_unsaved_edits(&self, edit: &crate::panel_state::spark_detail::SparkEdit) -> bool {
        use crate::panel_state::spark_detail::Field;

        if !edit.in_flight.is_empty() {
            return true;
        }
        let Some(spark) = self.sparks.iter().find(|s| s.id == edit.spark_id) else {
            // Spark not in cache — treat as clean to avoid blocking nav.
            return false;
        };
        for (field, draft) in &edit.drafts {
            let persisted = match field {
                Field::Title => &spark.title,
                Field::Description => &spark.description,
                _ => {
                    // For fields without a simple string comparison
                    // (e.g. Priority, Type), a draft entry means a
                    // user-initiated edit — conservatively treat as dirty.
                    return true;
                }
            };
            if draft != persisted {
                return true;
            }
        }
        false
    }

    /// Return the id of the currently selected spark **if** it has a
    /// description draft that differs from the persisted value. The
    /// "differs from persisted" check is what makes the navigation
    /// guard Free of false positives: opening the editor and typing
    /// nothing should not trap the user. Spark ryve-4742d98b.
    #[cfg(test)]
    pub fn dirty_description_spark_id(&self) -> Option<String> {
        let edit = self.spark_edit.as_ref()?;
        let draft = edit
            .drafts
            .get(&crate::panel_state::spark_detail::Field::Description)?;
        let spark = self.sparks.iter().find(|s| s.id == edit.spark_id)?;
        if draft != &spark.description {
            Some(edit.spark_id.clone())
        } else {
            None
        }
    }

    /// Begin a description edit on the currently-selected spark.
    /// Idempotent: calling it when the editor is already open leaves
    /// the existing draft and `Content` alone so re-entering the field
    /// does not wipe in-progress text. Spark ryve-4742d98b.
    pub fn begin_description_edit(&mut self) {
        let Some(ref selected) = self.selected_spark else {
            return;
        };
        let selected = selected.clone();
        let Some(spark) = self.sparks.iter().find(|s| s.id == selected) else {
            return;
        };
        let current = spark.description.clone();

        // Make sure SparkEdit exists for this spark (replace if it was
        // left behind by a different spark for any reason).
        let need_new = self
            .spark_edit
            .as_ref()
            .is_none_or(|e| e.spark_id != selected);
        if need_new {
            self.spark_edit = Some(crate::panel_state::spark_detail::SparkEdit::new(
                selected.clone(),
            ));
        }
        let edit = self.spark_edit.as_mut().expect("just inserted");

        // Seed the draft if this is the first time the field is opened.
        // Re-entering an already-open field must not clobber in-progress
        // text — mirror the `begin_edit` invariant covered by the
        // ryve-1d8c2847 tests.
        let seeded = edit
            .drafts
            .entry(crate::panel_state::spark_detail::Field::Description)
            .or_insert_with(|| current.clone())
            .clone();

        // Create the `text_editor::Content` if it doesn't already exist.
        // If a content already exists we leave it alone so the cursor
        // and any in-progress text survive re-entry.
        if self.description_editor.is_none() {
            self.description_editor = Some(iced::widget::text_editor::Content::with_text(&seeded));
        }
    }

    /// Drop the description draft and close the inline editor. Called
    /// from the Escape handler and from "Discard" in the nav-away
    /// dialog. Spark ryve-4742d98b.
    pub fn revert_description_edit(&mut self) {
        self.description_editor = None;
        if let Some(edit) = self.spark_edit.as_mut() {
            edit.drafts
                .remove(&crate::panel_state::spark_detail::Field::Description);
            edit.in_flight
                .remove(&crate::panel_state::spark_detail::Field::Description);
        }
    }

    /// Return the description draft value if one is live, else `None`.
    /// Used by the blur handler to decide whether to dispatch a
    /// `SparkUpdate`. Spark ryve-4742d98b.
    pub fn take_description_draft(&mut self) -> Option<(String, String)> {
        let edit = self.spark_edit.as_mut()?;
        let draft = edit
            .drafts
            .remove(&crate::panel_state::spark_detail::Field::Description)?;
        let spark_id = edit.spark_id.clone();
        self.description_editor = None;
        Some((spark_id, draft))
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
                    // TmuxAttach tabs are transient — the tmux session
                    // survives independently, but re-attaching on relaunch
                    // could surprise the user, so we skip persistence.
                    // Spark ryve-8ba40d83.
                    TabKind::TmuxAttach { .. } => return None,
                    // Home is a singleton dashboard rebuilt from in-memory
                    // data on demand; persisting it would just create a
                    // duplicate when the user reopens it manually.
                    TabKind::Home => return None,
                    // IRC projection views rebuild from live DB state on
                    // open. Persisting the per-tab filter form would
                    // snapshot transient UI state onto disk, so skip it.
                    // Spark ryve-5466c372.
                    TabKind::IrcView { .. } => return None,
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

    /// Open (or focus) an IRC projection view for `channel`. Idempotent
    /// per channel: if a tab already exists for the same channel, it
    /// is activated instead of creating a duplicate. Returns the tab
    /// id of the activated or newly-created tab. Caller is responsible
    /// for resolving "what channel?" before calling — see
    /// [`Workshop::default_irc_view_channel`]. PR #53 Copilot c8: doc
    /// previously claimed `Option<u64>` return semantics that don't
    /// match the actual `u64` signature. Spark ryve-5466c372.
    pub fn open_irc_view_tab(
        &mut self,
        channel: String,
        current_actor_id: Option<String>,
        next_terminal_id: &mut u64,
    ) -> u64 {
        if let Some(existing) = self
            .bench
            .tabs
            .iter()
            .find(|t| matches!(&t.kind, TabKind::IrcView { channel: c } if c == &channel))
            .map(|t| t.id)
        {
            self.bench.active_tab = Some(existing);
            return existing;
        }

        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        let title = format!("IRC {channel}");
        self.bench.create_tab(
            tab_id,
            title,
            TabKind::IrcView {
                channel: channel.clone(),
            },
        );
        self.irc_views.insert(
            tab_id,
            IrcViewState::new(tab_id, self.workshop_id(), channel, current_actor_id),
        );
        tab_id
    }

    /// Pick the channel name for the default "Open IRC View" action —
    /// the channel of the first open epic (in the order
    /// [`Workshop::sparks`] reports them). Returns `None` when no open
    /// epic exists so the caller can surface a toast instead of
    /// opening an empty tab. PR #53 Copilot c9: now actually filters
    /// to `status != "closed"` so a closed-out epic from a prior
    /// release isn't picked as the default. Spark ryve-5466c372.
    pub fn default_irc_view_channel(&self) -> Option<String> {
        let epic = self
            .sparks
            .iter()
            .find(|s| s.spark_type == "epic" && s.status != "closed")?;
        Some(ipc::channel_manager::channel_name(
            &ipc::channel_manager::EpicRef {
                id: epic.id.clone(),
                name: epic.title.clone(),
            },
        ))
    }

    /// Effective actor id for mention-override filtering in IRC view.
    /// Returns the user's explicitly configured `irc_nick` when set,
    /// otherwise `None` so mention-override is disabled.
    ///
    /// PR #53 Copilot c10: previously called `effective_irc_nick()`
    /// which always falls back to `"ryve"`, making this method
    /// effectively always return `Some`. That meant every workshop
    /// with default config would treat the literal string `@ryve`
    /// as a mention bypass — surprising and rarely what the user
    /// wants. Now we read the raw `irc_nick` field so the override
    /// only fires when the user opted into a nick. Spark ryve-5466c372.
    pub fn irc_view_actor_id(&self) -> Option<String> {
        self.config
            .irc_nick
            .as_ref()
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .map(|n| n.to_string())
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

    /// Spawn a plain (no coding agent) shell terminal. Synchronous — plain
    /// terminals run in the workshop root, so there is no worktree to
    /// create and no need to defer. Spark ryve-885ed3eb.
    pub fn spawn_plain_terminal(&mut self, title: String, next_terminal_id: &mut u64) -> u64 {
        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        self.bench.create_tab(tab_id, title, TabKind::Terminal);

        let mut settings = iced_term::settings::Settings {
            font: self.terminal_font_settings(),
            ..iced_term::settings::Settings::default()
        };
        settings.theme.color_pallete.background = self.terminal_bg_hex();
        settings.backend.working_directory = Some(self.directory.clone());

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        (settings.backend.program, settings.backend.args) =
            wrap_command_with_bottom_pin(&shell, &[]);

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
        }

        tab_id
    }

    /// Stage 1 of a two-step Hand terminal spawn (spark ryve-885ed3eb):
    /// allocate the tab id, create the bench tab placeholder, and store the
    /// parameters needed to build the real `iced_term::Terminal` once the
    /// async worktree-creation task reports back via `HandWorktreeReady`.
    ///
    /// Never touches `git worktree add` or the filesystem — completes in
    /// microseconds so `App::update` can return immediately. The tab shows
    /// the bench's "Loading..." placeholder until `finalize_hand_terminal`
    /// is called with the resolved working directory.
    pub fn begin_hand_terminal(
        &mut self,
        title: String,
        kind: PendingTerminalKind,
        next_terminal_id: &mut u64,
        session_id: String,
        full_auto: bool,
    ) -> u64 {
        self.begin_hand_terminal_inner(title, kind, next_terminal_id, session_id, full_auto, false)
    }

    /// Like [`begin_hand_terminal`] but marks the resulting tab as the Atlas
    /// director — pinned to index 0 with distinct visual treatment.
    pub fn begin_atlas_terminal(
        &mut self,
        title: String,
        kind: PendingTerminalKind,
        next_terminal_id: &mut u64,
        session_id: String,
        full_auto: bool,
    ) -> u64 {
        self.begin_hand_terminal_inner(title, kind, next_terminal_id, session_id, full_auto, true)
    }

    fn begin_hand_terminal_inner(
        &mut self,
        title: String,
        kind: PendingTerminalKind,
        next_terminal_id: &mut u64,
        session_id: String,
        full_auto: bool,
        is_atlas: bool,
    ) -> u64 {
        let tab_kind = match &kind {
            PendingTerminalKind::Agent(a) => TabKind::CodingAgent(a.clone()),
            PendingTerminalKind::CustomAgent(def) => TabKind::CodingAgent(CodingAgent {
                display_name: def.name.clone(),
                command: def.command.clone(),
                args: def.args.clone(),
                resume: crate::coding_agents::ResumeStrategy::None,
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            }),
        };

        let tab_id = *next_terminal_id;
        *next_terminal_id += 1;
        if is_atlas {
            self.bench.create_atlas_tab(tab_id, title, tab_kind);
        } else {
            self.bench.create_tab(tab_id, title, tab_kind);
        }

        self.pending_terminal_spawns.insert(
            tab_id,
            PendingTerminalSpawn {
                session_id,
                kind,
                full_auto,
                system_prompt: None,
                initial_prompt: None,
            },
        );

        tab_id
    }

    /// Attach an initial user-message prompt to a pending terminal spawn.
    /// Call this after `begin_hand_terminal` / `begin_atlas_terminal` so
    /// the `HandWorktreeReady` handler can dispatch the prompt once the
    /// terminal is actually alive. No-op if the pending spawn has already
    /// been finalised (e.g. the worktree race resolved between the begin
    /// call and this setter).
    pub fn set_pending_initial_prompt(&mut self, tab_id: u64, prompt: String) {
        if let Some(pending) = self.pending_terminal_spawns.get_mut(&tab_id) {
            pending.initial_prompt = Some(prompt);
        }
    }

    /// Stage 2 of a two-step Hand terminal spawn (spark ryve-885ed3eb):
    /// given the outcome of the async `create_hand_worktree` task, finish
    /// wiring up the terminal for `tab_id`. Returns `true` if the terminal
    /// was successfully created so the caller can dispatch a focus task.
    ///
    /// If the worktree task failed, the terminal falls back to the workshop
    /// root and records a warning via `last_worktree_warning` so the UI can
    /// surface a toast.
    pub fn finalize_hand_terminal(
        &mut self,
        tab_id: u64,
        worktree_result: Result<PathBuf, String>,
        system_prompt: Option<(String, String)>,
    ) -> FinalizedTerminal {
        let Some(mut pending) = self.pending_terminal_spawns.remove(&tab_id) else {
            return FinalizedTerminal {
                created: false,
                initial_prompt: None,
            };
        };

        // Take the initial-prompt payload now; the caller dispatches it once
        // we return `created == true`. If terminal creation fails below the
        // prompt is dropped along with the tab.
        let initial_prompt = pending.initial_prompt.take();

        // Prefer the asynchronously-resolved system prompt passed in from
        // the worktree task (spark ryve-2c7d348b). Fall back to whatever
        // was stored in the pending spawn (always None after the async
        // migration, but kept for safety).
        if system_prompt.is_some() {
            pending.system_prompt = system_prompt;
        }

        let working_dir = match worktree_result {
            Ok(path) => path,
            Err(e) => {
                let sid = &pending.session_id;
                log::warn!("Failed to create worktree for hand {sid}: {e}");
                self.last_worktree_warning = Some(format!(
                    "Failed to create worktree for hand {sid}: {e}. Falling back to workshop root."
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

        // Ryve env vars + session id so any nested `ryve hand spawn` in
        // this Hand is correctly attributed as its own parent.
        for (k, v) in hand_env_vars(&self.directory) {
            settings.backend.env.insert(k, v);
        }
        settings
            .backend
            .env
            .insert("RYVE_HAND_SESSION_ID".to_string(), pending.session_id);

        match pending.kind {
            PendingTerminalKind::Agent(agent) => {
                let mut args = agent.args.clone();
                if pending.full_auto {
                    args.extend(agent.full_auto_flags());
                }
                if let Some((flag, value)) = pending.system_prompt {
                    args.push(flag);
                    args.push(value);
                }
                (settings.backend.program, settings.backend.args) =
                    wrap_command_with_bottom_pin(&agent.command, &args);
            }
            PendingTerminalKind::CustomAgent(def) => {
                (settings.backend.program, settings.backend.args) =
                    wrap_command_with_bottom_pin(&def.command, &def.args);
                // Custom agent env overrides layer on top of the Ryve vars.
                for (k, v) in &def.env {
                    settings.backend.env.insert(k.clone(), v.clone());
                }
            }
        }

        if let Ok(term) = iced_term::Terminal::new(tab_id, settings) {
            self.terminals.insert(tab_id, term);
            FinalizedTerminal {
                created: true,
                initial_prompt,
            }
        } else {
            FinalizedTerminal {
                created: false,
                initial_prompt: None,
            }
        }
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

    /// Tear down the running terminal for an Atlas tab and queue a fresh
    /// `PendingTerminalSpawn` so the next `HandWorktreeReady` rebuilds it
    /// in-place. The tab itself (id, position, title, `is_atlas` flag) is
    /// left untouched. Returns the `CodingAgent` that was running and the
    /// IDs of sessions that were ended so the caller can persist the new
    /// session and end only those specific sessions in the DB.
    ///
    /// Spark ryve-71c3ec9f.
    pub fn prepare_atlas_refresh(
        &mut self,
        tab_id: u64,
        new_session_id: String,
        full_auto: bool,
    ) -> Option<(CodingAgent, Vec<String>)> {
        // Find the agent from the session attached to this tab.
        let agent = self
            .agent_sessions
            .iter()
            .find(|s| s.tab_id == Some(tab_id) && s.active)
            .map(|s| s.agent.clone())?;

        // Drop the old terminal — `Backend::drop` sends Msg::Shutdown to
        // the PTY, killing the subprocess.
        self.terminals.remove(&tab_id);

        // End the old session(s) on this tab, collecting their IDs so the
        // caller can persist the change in the DB.
        let mut ended_ids = Vec::new();
        for session in self.agent_sessions.iter_mut() {
            if session.tab_id == Some(tab_id) {
                ended_ids.push(session.id.clone());
                session.tab_id = None;
                session.active = false;
                session.stale = false;
            }
        }

        // Queue the pending spawn so `finalize_hand_terminal` picks it up
        // when `HandWorktreeReady` arrives. System prompt is resolved
        // asynchronously in dispatch_worktree_task (spark ryve-2c7d348b).
        self.pending_terminal_spawns.insert(
            tab_id,
            PendingTerminalSpawn {
                session_id: new_session_id,
                kind: PendingTerminalKind::Agent(agent.clone()),
                full_auto,
                system_prompt: None,
                initial_prompt: None,
            },
        );

        Some((agent, ended_ids))
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

    /// Apply a [`SparkPatch`] to the cached spark with `id`, returning the
    /// *prior* values of any field the patch overwrote (as another patch).
    /// The returned prior-patch, when re-applied via `apply_spark_patch`,
    /// restores the spark to its pre-edit state — this is how
    /// `Message::SparkUpdate` rolls back when the async DB write fails.
    ///
    /// Returns `None` if no spark with `id` exists in the cache. Fields in
    /// `patch` that are `None` are left untouched (and absent from the
    /// returned prior); fields whose new value equals the current value
    /// are also skipped so the prior-patch only carries real changes.
    ///
    /// Spark ryve-90174007.
    pub fn apply_spark_patch(&mut self, id: &str, patch: &SparkPatch) -> Option<SparkPatch> {
        let spark = self.sparks.iter_mut().find(|s| s.id == id)?;
        let mut prior = SparkPatch::default();
        if let Some(new) = patch.title.as_ref()
            && *new != spark.title
        {
            prior.title = Some(std::mem::replace(&mut spark.title, new.clone()));
        }
        if let Some(new) = patch.description.as_ref()
            && *new != spark.description
        {
            prior.description = Some(std::mem::replace(&mut spark.description, new.clone()));
        }
        if let Some(new) = patch.status.as_ref()
            && *new != spark.status
        {
            prior.status = Some(std::mem::replace(&mut spark.status, new.clone()));
        }
        if let Some(new) = patch.priority
            && new != spark.priority
        {
            prior.priority = Some(spark.priority);
            spark.priority = new;
        }
        if let Some(new) = patch.spark_type.as_ref()
            && *new != spark.spark_type
        {
            prior.spark_type = Some(std::mem::replace(&mut spark.spark_type, new.clone()));
        }
        if let Some(new) = patch.assignee.as_ref()
            && *new != spark.assignee
        {
            prior.assignee = Some(std::mem::replace(&mut spark.assignee, new.clone()));
        }
        if let Some(new) = patch.owner.as_ref()
            && *new != spark.owner
        {
            prior.owner = Some(std::mem::replace(&mut spark.owner, new.clone()));
        }
        if let Some(new) = patch.risk_level.as_ref()
            && *new != spark.risk_level
        {
            prior.risk_level = Some(std::mem::replace(&mut spark.risk_level, new.clone()));
        }
        if let Some(new) = patch.scope_boundary.as_ref()
            && *new != spark.scope_boundary
        {
            prior.scope_boundary = Some(std::mem::replace(&mut spark.scope_boundary, new.clone()));
        }
        if let Some(new_problem) = patch.problem_statement.as_ref() {
            // Problem statement lives in metadata JSON under
            // `intent.problem_statement`. Merge it in without clobbering
            // sibling intent fields (invariants, non_goals, etc.).
            let old_problem = spark.intent().problem_statement.unwrap_or_default();
            if *new_problem != old_problem {
                let mut metadata_json: serde_json::Value =
                    serde_json::from_str(&spark.metadata).unwrap_or_else(|_| serde_json::json!({}));
                if !metadata_json.is_object() {
                    metadata_json = serde_json::json!({});
                }
                let obj = metadata_json
                    .as_object_mut()
                    .expect("metadata normalized to object above");
                let intent_entry = obj
                    .entry("intent".to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if !intent_entry.is_object() {
                    *intent_entry = serde_json::json!({});
                }
                let intent_obj = intent_entry
                    .as_object_mut()
                    .expect("intent normalized to object above");
                if new_problem.is_empty() {
                    intent_obj.remove("problem_statement");
                } else {
                    intent_obj.insert(
                        "problem_statement".to_string(),
                        serde_json::Value::String(new_problem.clone()),
                    );
                }
                spark.metadata = metadata_json.to_string();
                prior.problem_statement = Some(old_problem);
            }
        }
        Some(prior)
    }
}

pub fn wrap_command_with_bottom_pin(program: &str, args: &[String]) -> (String, Vec<String>) {
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

/// Validate a branch name against git's `check-ref-format` rules.
///
/// Mirrors `git check-ref-format refs/heads/<name>` so we can reject
/// obviously bad names (whitespace, `..`, `~`, `^`, `:`, `@{`, trailing
/// `.lock`, …) before shelling out to `git worktree add`. Pure Rust so
/// unit tests don't need a git binary. Spark ryve-7aa05933.
pub(crate) fn validate_git_branch_name(name: &str) -> Result<(), String> {
    let reject = |reason: &str| Err(format!("invalid branch name '{name}': {reason}"));

    if name.is_empty() {
        return reject("branch name must not be empty");
    }
    if name == "@" {
        return reject("branch name must not be a single '@'");
    }
    if name.starts_with('/') || name.ends_with('/') {
        return reject("must not start or end with '/'");
    }
    if name.starts_with('.') {
        return reject("must not start with '.'");
    }
    if name.ends_with('.') {
        return reject("must not end with '.'");
    }
    if name.ends_with(".lock") {
        return reject("must not end with '.lock'");
    }
    if name.contains("..") {
        return reject("must not contain '..'");
    }
    if name.contains("//") {
        return reject("must not contain '//'");
    }
    if name.contains("/.") {
        return reject("path components must not start with '.'");
    }
    if name.contains("@{") {
        return reject("must not contain '@{'");
    }

    for (i, ch) in name.char_indices() {
        let code = ch as u32;
        // ASCII control chars (0-31) and DEL (127).
        if code < 0x20 || code == 0x7f {
            return reject(&format!("contains control character at byte {i}"));
        }
        match ch {
            ' ' => return reject("must not contain whitespace"),
            '~' => return reject("must not contain '~'"),
            '^' => return reject("must not contain '^'"),
            ':' => return reject("must not contain ':'"),
            '?' => return reject("must not contain '?'"),
            '*' => return reject("must not contain '*'"),
            '[' => return reject("must not contain '['"),
            '\\' => return reject("must not contain '\\'"),
            _ => {}
        }
    }

    // Each slash-delimited component has its own rules.
    for component in name.split('/') {
        if component.is_empty() {
            return reject("must not contain empty path component");
        }
        if component.ends_with(".lock") {
            return reject("path component must not end with '.lock'");
        }
    }

    Ok(())
}

/// Create a git worktree for a Hand session. Async: uses `tokio::process`
/// and `tokio::fs` so callers in the UI thread can drive this via
/// `Task::perform` without freezing the iced runtime. On large repos
/// `git worktree add` takes 500ms-2s and must not block `App::update`.
/// Spark ryve-885ed3eb.
///
/// The branch is named `<actor>/<short>` so every Hand lives in its own
/// actor-scoped namespace (spark ryve-c44b92e5). `actor` must be a single
/// path segment — no `/` — or the resulting ref would collide with
/// `epic/` / `crew/` / `release/` prefixes the rest of the system relies on.
/// The computed branch is also passed through [`validate_git_branch_name`]
/// so a malformed `actor` (e.g. from a weird `$USER`) can't produce a ref
/// git will choke on. Spark ryve-7aa05933.
///
/// Visible to the rest of the crate so the `hand_spawn` CLI helper can call
/// it without re-implementing the worktree convention.
///
/// When `base_ref` is `Some`, the new Hand branch is cut from that ref
/// (`git worktree add -b <branch> <path> <ref>`). When `None`, the branch
/// is cut from the current HEAD — the historical behavior. Spark ryve-a64b3a06.
pub(crate) async fn create_hand_worktree(
    workshop_dir: &Path,
    ryve_dir: &RyveDir,
    session_id: &str,
    actor: &str,
    base_ref: Option<&str>,
) -> Result<PathBuf, String> {
    // Only create worktrees for git repos
    let git_dir = workshop_dir.join(".git");
    if !tokio::fs::try_exists(&git_dir).await.unwrap_or(false) {
        return Err("not a git repository".to_string());
    }

    if actor.is_empty() || actor.contains('/') {
        return Err(format!(
            "invalid actor segment '{actor}': must be non-empty and contain no '/'"
        ));
    }

    let short_id = &session_id[..8.min(session_id.len())];
    let branch = format!("{actor}/{short_id}");
    // Spark ryve-7aa05933: `actor` can come from the `USER` env var, so the
    // branch name may violate git ref-format rules (whitespace, `..`, `~`,
    // `^`, `:`, `@{`, trailing `.lock`, …). Reject those up front with an
    // actionable message instead of letting `git worktree add` produce a
    // cryptic error — or worse, succeed with a surprising ref.
    validate_git_branch_name(&branch)?;
    let wt_dir = ryve_dir.root().join("worktrees").join(short_id);

    // Skip if worktree already exists
    if tokio::fs::try_exists(&wt_dir).await.unwrap_or(false) {
        return Ok(wt_dir);
    }

    // Create parent dir
    tokio::fs::create_dir_all(wt_dir.parent().unwrap_or(ryve_dir.root()))
        .await
        .map_err(|e| e.to_string())?;

    let wt_dir_str = wt_dir.to_string_lossy();
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(wt_dir_str.as_ref());
    if let Some(base) = base_ref {
        cmd.arg(base);
    }
    let output = cmd
        .current_dir(workshop_dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    // Drop AGENTS.md into the worktree so agents without a system-prompt
    // CLI flag (codex, opencode) still see WORKSHOP.md instructions.
    let workshop_md = ryve_dir.workshop_md_path();
    if tokio::fs::try_exists(&workshop_md).await.unwrap_or(false) {
        let agents_md = wt_dir.join("AGENTS.md");
        if !tokio::fs::try_exists(&agents_md).await.unwrap_or(false)
            && let Err(e) = tokio::fs::copy(&workshop_md, &agents_md).await
        {
            log::warn!("Failed to write AGENTS.md to worktree: {e}");
        }
    }

    Ok(wt_dir)
}

/// Asynchronously resolve the system-prompt flag + value for a coding agent.
///
/// Spark ryve-2c7d348b: moved out of `begin_hand_terminal_inner` /
/// `prepare_atlas_refresh` (which ran on the UI thread) into the async
/// worktree task so no `std::fs` calls block `update()`.
pub(crate) async fn resolve_system_prompt_async(
    ryve_dir: &RyveDir,
    prompt_flag: Option<(&str, bool)>,
) -> Option<(String, String)> {
    let (flag, is_file) = prompt_flag?;
    let prompt_path = ryve_dir.workshop_md_path();
    if !tokio::fs::try_exists(&prompt_path).await.unwrap_or(false) {
        return None;
    }
    let value = if is_file {
        prompt_path.to_string_lossy().into_owned()
    } else {
        tokio::fs::read_to_string(&prompt_path)
            .await
            .unwrap_or_default()
    };
    Some((flag.to_string(), value))
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

    // Expose the bundled tmux path and version so Hands can locate it
    // without $PATH. Only meaningful on unix where tmux is available.
    #[cfg(unix)]
    if let Some(tmux) = crate::bundled_tmux::bundled_tmux_path() {
        vars.push((
            "RYVE_TMUX_PATH".to_string(),
            tmux.to_string_lossy().into_owned(),
        ));
        vars.push((
            "RYVE_TMUX_VERSION".to_string(),
            crate::bundled_tmux::PINNED_TMUX_VERSION.to_string(),
        ));
    }

    vars
}

/// Result of async workshop initialization.
pub struct WorkshopInit {
    pub pool: SqlitePool,
    pub config: WorkshopConfig,
    pub custom_agents: Vec<AgentDef>,
    pub agent_context: Option<String>,
    /// Hash cache populated by the initial `agent_context::sync`. Handed
    /// off to the `Workshop` so subsequent sync ticks share the same warm
    /// cache and skip re-reading the just-written files. Spark ryve-86b0b326.
    pub agent_context_sync_cache: Arc<Mutex<AgentContextSyncCache>>,
    /// Persisted per-workshop UI state (collapsed epics, ...). Loaded on
    /// workshop open and applied to the initial render. Spark ryve-926870a9.
    pub ui_state: UiState,
}

/// Provision the workshop-scoped ngIRCd daemon: allocate a free
/// `127.0.0.1` port, record it in the workshop config, write
/// `.ryve/ircd/ircd.conf`, and explicitly flip `irc_enabled` on so the
/// supervisor (spark ryve-242252b0) picks it up on next launch.
///
/// Idempotent — re-running against a workshop that already has an
/// allocated port keeps the same port and leaves an existing
/// `ircd.conf` untouched. Callers are expected to persist the mutated
/// `config` to disk themselves (via [`data::ryve_dir::save_config`]).
///
/// Spark ryve-4d5881c2 [sp-31659bbb].
pub async fn provision_workshop_ircd(
    ryve_dir: &RyveDir,
    config: &mut WorkshopConfig,
) -> std::io::Result<()> {
    // Re-init preserves an explicit opt-out: if the workshop already
    // has a bundled port, we've been here before, so leave
    // `irc_enabled` alone. Users who set `irc_enabled = false` after
    // their first init keep that setting across every subsequent
    // `ryve init`. Only a *fresh* workshop (no bundled port yet) gets
    // the default-on flip below, which also round-trips through
    // config.toml so downstream serde code doesn't fall back to an
    // inferred default.
    let is_fresh = config.irc_bundled_port.is_none();
    if is_fresh {
        config.irc_enabled = true;
    }

    let ircd_dir = ryve_dir.root().join("ircd");
    tokio::fs::create_dir_all(&ircd_dir).await?;

    let port = match config.irc_bundled_port {
        Some(p) => p,
        None => {
            // Bind to port 0 to let the kernel pick a free ephemeral
            // port, then drop the listener so ngIRCd can rebind. There
            // is a TOCTOU window between this and the first supervisor
            // spawn but it's acceptable for init — the supervisor
            // retries on failure and the chance of collision on a
            // freshly-picked ephemeral is negligible.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let allocated = listener.local_addr()?.port();
            drop(listener);
            config.irc_bundled_port = Some(allocated);
            allocated
        }
    };

    let conf_path = ryve_dir
        .root()
        .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
    if !conf_path.exists() {
        let body = render_ircd_conf(port);
        tokio::fs::write(&conf_path, body).await?;
    }

    Ok(())
}

/// Minimal ngIRCd config bound to `127.0.0.1:<port>` suitable for a
/// workshop-local agent coordination daemon. No PAM, no ident, no
/// TLS — the daemon only ever sees loopback traffic from Ryve-spawned
/// clients.
fn render_ircd_conf(port: u16) -> String {
    format!(
        "# ngIRCd config for the Ryve workshop-local daemon.\n\
         # Auto-generated by `ryve init` (spark ryve-4d5881c2).\n\
         # Safe to edit by hand; ryve init will not overwrite an\n\
         # existing file on re-init.\n\
         \n\
         [Global]\n\
         \tName = ryve.local\n\
         \tAdminInfo1 = Ryve workshop\n\
         \tAdminInfo2 = workshop-local\n\
         \tAdminEMail = noreply@ryve.local\n\
         \tInfo = Ryve workshop IRC\n\
         \tListen = 127.0.0.1\n\
         \tPorts = {port}\n\
         \tMotdPhrase = \"Ryve workshop\"\n\
         \n\
         [Options]\n\
         \tPAM = no\n"
    )
}

/// Initialize a workshop's `.ryve/` directory, DB, and load config.
/// This is the single async entry point called when a workshop opens.
pub async fn init_workshop(directory: PathBuf) -> Result<WorkshopInit, data::sparks::SparksError> {
    let ryve_dir = RyveDir::new(&directory);

    // Run any pending workshop schema migrations. Returns the (now-current)
    // config plus a log of what ran so the caller can surface it to the user.
    let (mut config, migration_log) = data::migrations::migrate_workshop(&ryve_dir)
        .await
        .map_err(data::sparks::SparksError::Io)?;

    // Auto-provision the workshop-scoped IRC daemon if the workshop
    // wants IRC (`irc_enabled = true`) but has never been allocated a
    // bundled port. This covers the upgrade path: workshops created
    // before 0.2.1 land here on first launch with `irc_enabled = true`
    // (default) and `irc_bundled_port = None`. Without this, the
    // supervisor would skip cleanly, the IRC client would have no
    // dial target, and the status-bar pill would (correctly, per
    // PR #50 c6) display Disabled — but the user would have no clear
    // path to re-provision short of running `ryve init` manually.
    //
    // This caller guards on `irc_enabled()` so a user who has opted
    // out (`irc_enabled = false` in .ryve/config.toml) skips the
    // auto-provision entirely. `provision_workshop_ircd` itself does
    // not check `irc_enabled()` — it only short-circuits on the port
    // being already allocated. Re-launching a workshop that already
    // has `irc_bundled_port = Some(_)` is therefore a no-op (PR #51
    // Copilot c2). Errors are logged but non-fatal — workshop open
    // must still succeed if the IRC daemon can't be provisioned (no
    // bundled binary on this platform, ircd dir not writable, etc.).
    //
    // PR #51 Copilot c3 — recovering from a save_config failure:
    // provision_workshop_ircd allocates a new port and writes
    // ircd.conf at that port. If save_config then fails, the on-disk
    // config keeps `irc_bundled_port = None` while ircd.conf carries
    // a now-orphaned port. On the next launch we'd allocate ANOTHER
    // fresh port but leave the stale conf in place (the existence
    // check at `provision_workshop_ircd` short-circuits the write),
    // producing a conf↔config mismatch that breaks the supervisor.
    // Defend by deleting ircd.conf on save-failure so the next launch
    // regenerates it cleanly against whatever port we allocate then.
    if config.irc_enabled() && config.irc_bundled_port.is_none() {
        match provision_workshop_ircd(&ryve_dir, &mut config).await {
            Ok(()) => {
                if let Err(e) = data::ryve_dir::save_config(&ryve_dir, &config).await {
                    let conf_path = ryve_dir
                        .root()
                        .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
                    let cleanup = tokio::fs::remove_file(&conf_path).await;
                    log::warn!(
                        "ircd: auto-provision succeeded but persisting config failed: {e}; \
                         removed ircd.conf to avoid a port mismatch on next launch \
                         (cleanup result: {cleanup:?}); the bundled port will be re-allocated \
                         on next launch"
                    );
                } else {
                    log::info!(
                        "ircd: auto-provisioned bundled daemon for upgraded workshop \
                         (port={:?})",
                        config.irc_bundled_port
                    );
                }
            }
            Err(e) => {
                // PR #51 Copilot c4: this branch is reached only after
                // the `irc_enabled && port is None` guard passed, so
                // `provision_workshop_ircd` actually failed (I/O error,
                // port allocation, etc.) — it wasn't a deliberate skip.
                log::warn!(
                    "ircd: auto-provision failed — {e}; the IRC pill will stay Disabled \
                     until `ryve init` is run manually or the user sets an explicit \
                     `irc_server` override"
                );
            }
        }
    }

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
    // Load persisted per-workshop UI state so the sparks panel can
    // rehydrate collapse/expand decisions on the first render. Silent
    // fall-back to default on any I/O or parse failure — UI state is
    // cosmetic. Spark ryve-926870a9.
    let ui_state = data::ryve_dir::load_ui_state(&ryve_dir).await;

    // Generate WORKSHOP.md and inject pointers into agent boot files
    // (also propagates into any existing worktrees).
    let agent_context_sync_cache = Arc::new(Mutex::new(AgentContextSyncCache::new()));
    if !config.agents.disable_sync {
        let _ =
            data::agent_context::sync(&directory, &ryve_dir, &config, &agent_context_sync_cache)
                .await;
    }

    Ok(WorkshopInit {
        pool,
        config,
        custom_agents,
        agent_context,
        agent_context_sync_cache,
        ui_state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_agents::{CodingAgent, ResumeStrategy};
    use crate::panel_state::agents::AgentSession;

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
    fn sparks_refreshing_defaults_false() {
        // Spark ryve-7805b38b: a fresh workshop must not pretend a
        // Refresh is already in flight, otherwise the button would
        // ship stuck in its in-flight style.
        let ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        assert!(!ws.sparks_refreshing);
    }

    #[test]
    fn workshop_id_derives_from_directory_name() {
        let ws = Workshop::new(PathBuf::from("/home/user/projects/my-project"));
        assert_eq!(ws.workshop_id(), "my-project");
    }

    /// Regression: the old spawn paths scheduled `SendSparkPrompt` on a
    /// standalone 3s timer, independent of whether the worktree task had
    /// actually produced a terminal yet. On slow startup the timer could
    /// fire before `finalize_hand_terminal` inserted the terminal, and
    /// the prompt was silently dropped.
    ///
    /// The replacement contract: spawn sites call
    /// `set_pending_initial_prompt`, and `finalize_hand_terminal` hands
    /// the prompt back to the caller via `FinalizedTerminal.initial_prompt`
    /// so `HandWorktreeReady` dispatches it only after the terminal exists.
    /// These tests lock in the flow-independent half: the prompt is
    /// stashed on, and later removed from, the pending spawn.
    #[test]
    fn set_pending_initial_prompt_stashes_on_pending_spawn() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));

        // Simulate what begin_hand_terminal does: insert a pending spawn.
        let tab_id = 42;
        ws.pending_terminal_spawns.insert(
            tab_id,
            PendingTerminalSpawn {
                session_id: "sess-1".to_string(),
                kind: PendingTerminalKind::Agent(CodingAgent {
                    display_name: "claude".to_string(),
                    command: "claude".to_string(),
                    args: vec![],
                    resume: ResumeStrategy::None,
                    compatibility: crate::coding_agents::CompatStatus::Unknown,
                }),
                full_auto: false,
                system_prompt: None,
                initial_prompt: None,
            },
        );

        ws.set_pending_initial_prompt(tab_id, "Hello Atlas".to_string());

        let pending = ws
            .pending_terminal_spawns
            .get(&tab_id)
            .expect("pending spawn must still exist");
        assert_eq!(pending.initial_prompt.as_deref(), Some("Hello Atlas"));
    }

    #[test]
    fn set_pending_initial_prompt_is_noop_if_no_pending_spawn() {
        // Race safety: if the pending spawn has already been finalised
        // (or the tab never existed), this must not panic or insert a
        // ghost entry.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.set_pending_initial_prompt(9999, "into the void".to_string());
        assert!(ws.pending_terminal_spawns.is_empty());
    }

    // ── Collapsed epic state (spark ryve-926870a9) ──────────

    /// Build a bare `Spark` useful for collapse/prune tests. Only `id`
    /// and `spark_type` matter to the functions under test.
    fn make_spark(id: &str, spark_type: &str) -> Spark {
        Spark {
            id: id.to_string(),
            title: String::new(),
            description: String::new(),
            status: "open".to_string(),
            priority: 2,
            spark_type: spark_type.to_string(),
            assignee: None,
            owner: None,
            parent_id: None,
            workshop_id: "ws-test".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: "{}".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: None,
            scope_boundary: None,
        }
    }

    #[test]
    fn toggle_epic_collapse_flips_membership() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        assert!(!ws.is_epic_collapsed("ep-1"));

        // First toggle collapses.
        assert!(ws.toggle_epic_collapse("ep-1"));
        assert!(ws.is_epic_collapsed("ep-1"));
        assert!(ws.collapsed_epics.contains("ep-1"));

        // Second toggle expands again.
        assert!(!ws.toggle_epic_collapse("ep-1"));
        assert!(!ws.is_epic_collapsed("ep-1"));
        assert!(!ws.collapsed_epics.contains("ep-1"));
    }

    #[test]
    fn toggle_epic_collapse_is_per_epic() {
        // Invariant: collapsing one epic never touches another.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.toggle_epic_collapse("ep-1");
        ws.toggle_epic_collapse("ep-2");
        assert!(ws.is_epic_collapsed("ep-1"));
        assert!(ws.is_epic_collapsed("ep-2"));

        ws.toggle_epic_collapse("ep-1");
        assert!(!ws.is_epic_collapsed("ep-1"));
        assert!(ws.is_epic_collapsed("ep-2"));
    }

    #[test]
    fn prune_collapsed_epics_drops_stale_ids() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.collapsed_epics.insert("ep-live".to_string());
        ws.collapsed_epics.insert("ep-dead".to_string());
        ws.collapsed_epics.insert("ep-also-dead".to_string());

        let sparks = vec![make_spark("ep-live", "epic"), make_spark("sp-task", "task")];
        let live = Workshop::live_epic_ids(&sparks);
        let pruned = ws.prune_collapsed_epics(&live);
        assert!(pruned, "prune should report that something was removed");
        assert_eq!(ws.collapsed_epics.len(), 1);
        assert!(ws.collapsed_epics.contains("ep-live"));
        assert!(!ws.collapsed_epics.contains("ep-dead"));
    }

    #[test]
    fn prune_collapsed_epics_ignores_non_epic_sparks_with_matching_id() {
        // A task whose id happens to match a stored collapsed id must not
        // keep that id alive — only actual epics count.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.collapsed_epics.insert("sp-42".to_string());
        let sparks = vec![make_spark("sp-42", "task")];
        let live = Workshop::live_epic_ids(&sparks);
        assert!(ws.prune_collapsed_epics(&live));
        assert!(ws.collapsed_epics.is_empty());
    }

    #[test]
    fn prune_collapsed_epics_is_noop_when_all_live() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.collapsed_epics.insert("ep-1".to_string());
        let sparks = vec![make_spark("ep-1", "epic")];
        let live = Workshop::live_epic_ids(&sparks);
        assert!(!ws.prune_collapsed_epics(&live));
        assert!(ws.collapsed_epics.contains("ep-1"));
    }

    #[test]
    fn prune_collapsed_epics_is_noop_when_set_empty() {
        // The short-circuit path: no stored ids means no work to do.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let sparks = vec![make_spark("ep-1", "epic")];
        let live = Workshop::live_epic_ids(&sparks);
        assert!(!ws.prune_collapsed_epics(&live));
    }

    #[test]
    fn ui_state_snapshot_mirrors_collapsed_epics() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.toggle_epic_collapse("ep-a");
        ws.toggle_epic_collapse("ep-b");
        let snap = ws.ui_state_snapshot();
        assert_eq!(snap.collapsed_epics, ws.collapsed_epics);
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
            session_label: None,
            tmux_session_live: false,
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
        const _: () = assert!(BOTTOM_PIN_NEWLINES <= 30);
        const _: () = assert!(BOTTOM_PIN_NEWLINES >= 10);
    }

    // ── SparkPatch / apply_spark_patch (ryve-90174007) ───────────────────
    //
    // These tests exercise the optimistic-write + rollback primitive that
    // `Message::SparkUpdate` is built on. The invariant under test: applying
    // a patch then applying the returned prior-patch is an identity
    // operation on the cached spark.

    fn test_spark(id: &str) -> data::sparks::types::Spark {
        data::sparks::types::Spark {
            id: id.to_string(),
            title: "old title".to_string(),
            description: "old body".to_string(),
            status: "open".to_string(),
            priority: 2,
            spark_type: "task".to_string(),
            assignee: None,
            owner: Some("owner-a".to_string()),
            parent_id: None,
            workshop_id: "test-ws".to_string(),
            estimated_minutes: None,
            github_issue_number: None,
            github_repo: None,
            metadata: "{}".to_string(),
            created_at: "2026-04-09T00:00:00Z".to_string(),
            updated_at: "2026-04-09T00:00:00Z".to_string(),
            closed_at: None,
            closed_reason: None,
            due_at: None,
            defer_until: None,
            risk_level: Some("normal".to_string()),
            scope_boundary: Some("src/lib.rs".to_string()),
        }
    }

    #[test]
    fn apply_spark_patch_writes_fields_and_returns_prior() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));

        let patch = SparkPatch {
            title: Some("new title".to_string()),
            priority: Some(0),
            status: Some("in_progress".to_string()),
            assignee: Some(Some("alice".to_string())),
            ..Default::default()
        };

        let prior = ws.apply_spark_patch("sp-1", &patch).expect("spark exists");

        // Cache reflects the new values.
        let spark = &ws.sparks[0];
        assert_eq!(spark.title, "new title");
        assert_eq!(spark.priority, 0);
        assert_eq!(spark.status, "in_progress");
        assert_eq!(spark.assignee.as_deref(), Some("alice"));

        // Prior-patch carries only the changed fields, with their old values.
        assert_eq!(prior.title.as_deref(), Some("old title"));
        assert_eq!(prior.priority, Some(2));
        assert_eq!(prior.status.as_deref(), Some("open"));
        assert_eq!(prior.assignee, Some(None));
        // Untouched fields stay None in the prior-patch.
        assert!(prior.description.is_none());
        assert!(prior.spark_type.is_none());
    }

    #[test]
    fn apply_spark_patch_then_prior_is_identity() {
        // Core rollback invariant: patch ∘ prior == no-op. Without this
        // property `SparkUpdateFailed` can't restore the pre-edit state.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        let before = ws.sparks[0].clone();

        let patch = SparkPatch {
            title: Some("edited".to_string()),
            description: Some("new description".to_string()),
            priority: Some(4),
            spark_type: Some("bug".to_string()),
            assignee: Some(Some("bob".to_string())),
            owner: Some(None),
            scope_boundary: Some(Some("src/main.rs".to_string())),
            risk_level: Some(Some("critical".to_string())),
            status: Some("blocked".to_string()),
            problem_statement: None,
        };
        let prior = ws.apply_spark_patch("sp-1", &patch).expect("spark exists");
        ws.apply_spark_patch("sp-1", &prior).expect("spark exists");

        let after = &ws.sparks[0];
        assert_eq!(after.title, before.title);
        assert_eq!(after.description, before.description);
        assert_eq!(after.priority, before.priority);
        assert_eq!(after.spark_type, before.spark_type);
        assert_eq!(after.assignee, before.assignee);
        assert_eq!(after.owner, before.owner);
        assert_eq!(after.scope_boundary, before.scope_boundary);
        assert_eq!(after.risk_level, before.risk_level);
        assert_eq!(after.status, before.status);
    }

    #[test]
    fn apply_spark_patch_skips_unchanged_fields() {
        // Fields whose new value equals the current value must not appear
        // in the prior-patch — otherwise `is_empty` would always be false
        // and the handler would churn the DB on no-op edits.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));

        let patch = SparkPatch {
            title: Some("old title".to_string()), // unchanged
            priority: Some(7),                    // changed
            ..Default::default()
        };
        let prior = ws.apply_spark_patch("sp-1", &patch).expect("spark exists");

        assert!(prior.title.is_none());
        assert_eq!(prior.priority, Some(2));
        assert!(!prior.is_empty());
    }

    #[test]
    fn apply_spark_patch_returns_none_for_missing_spark() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let patch = SparkPatch {
            title: Some("whatever".to_string()),
            ..Default::default()
        };
        assert!(ws.apply_spark_patch("sp-missing", &patch).is_none());
    }

    #[test]
    fn spark_patch_to_update_spark_translates_enums() {
        // Catch typos/drift between SparkPatch's string representation and
        // the enum variants in `data::sparks::types`.
        let patch = SparkPatch {
            title: Some("t".to_string()),
            status: Some("in_progress".to_string()),
            spark_type: Some("bug".to_string()),
            risk_level: Some(Some("elevated".to_string())),
            ..Default::default()
        };
        let upd = patch.to_update_spark();
        assert_eq!(upd.title.as_deref(), Some("t"));
        assert!(matches!(
            upd.status,
            Some(data::sparks::types::SparkStatus::InProgress)
        ));
        assert!(matches!(
            upd.spark_type,
            Some(data::sparks::types::SparkType::Bug)
        ));
        assert!(matches!(
            upd.risk_level,
            Some(data::sparks::types::RiskLevel::Elevated)
        ));
    }

    #[test]
    fn spark_patch_is_empty_detects_no_op() {
        assert!(SparkPatch::default().is_empty());
        assert!(
            !SparkPatch {
                title: Some("x".to_string()),
                ..Default::default()
            }
            .is_empty()
        );
        assert!(
            !SparkPatch {
                problem_statement: Some("why".to_string()),
                ..Default::default()
            }
            .is_empty()
        );
    }

    // ── problem_statement metadata merging (ryve-a5997352) ───────────────

    #[test]
    fn apply_spark_patch_sets_problem_statement_from_empty_metadata() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        // starts with "{}" metadata — no intent at all
        let patch = SparkPatch {
            problem_statement: Some("line one\nline two".to_string()),
            ..Default::default()
        };
        let prior = ws.apply_spark_patch("sp-1", &patch).expect("exists");
        let spark = &ws.sparks[0];
        // New intent block exists with the multiline problem_statement.
        assert_eq!(
            spark.intent().problem_statement.as_deref(),
            Some("line one\nline two")
        );
        // Prior carries the old (empty) value so rollback works.
        assert_eq!(prior.problem_statement.as_deref(), Some(""));
    }

    #[test]
    fn apply_spark_patch_problem_statement_preserves_sibling_intent() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let mut s = test_spark("sp-1");
        s.metadata = serde_json::json!({
            "intent": {
                "problem_statement": "old",
                "invariants": ["never panic"],
                "non_goals": ["rewrites"],
                "acceptance_criteria": ["it builds"]
            },
            "other": "untouched"
        })
        .to_string();
        ws.sparks.push(s);

        let patch = SparkPatch {
            problem_statement: Some("new".to_string()),
            ..Default::default()
        };
        let _ = ws.apply_spark_patch("sp-1", &patch).expect("exists");

        let spark = &ws.sparks[0];
        let intent = spark.intent();
        assert_eq!(intent.problem_statement.as_deref(), Some("new"));
        assert_eq!(intent.invariants, vec!["never panic".to_string()]);
        assert_eq!(intent.non_goals, vec!["rewrites".to_string()]);
        assert_eq!(intent.acceptance_criteria, vec!["it builds".to_string()]);
        // Sibling top-level keys survive the merge.
        let v: serde_json::Value = serde_json::from_str(&spark.metadata).unwrap();
        assert_eq!(v["other"], serde_json::json!("untouched"));
    }

    #[test]
    fn apply_spark_patch_problem_statement_empty_clears_field() {
        // Empty string = cleared (the acceptance criterion allows
        // "no problem statement yet").
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let mut s = test_spark("sp-1");
        s.metadata = serde_json::json!({ "intent": { "problem_statement": "x" } }).to_string();
        ws.sparks.push(s);

        let patch = SparkPatch {
            problem_statement: Some(String::new()),
            ..Default::default()
        };
        let _ = ws.apply_spark_patch("sp-1", &patch).expect("exists");
        assert_eq!(ws.sparks[0].intent().problem_statement, None);
    }

    #[test]
    fn apply_spark_patch_problem_statement_skips_no_op_edit() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let mut s = test_spark("sp-1");
        s.metadata = serde_json::json!({ "intent": { "problem_statement": "x" } }).to_string();
        ws.sparks.push(s);

        let patch = SparkPatch {
            problem_statement: Some("x".to_string()),
            ..Default::default()
        };
        let prior = ws.apply_spark_patch("sp-1", &patch).expect("exists");
        // Unchanged → absent from prior → is_empty → no DB write.
        assert!(prior.problem_statement.is_none());
        assert!(prior.is_empty());
    }

    #[test]
    fn apply_spark_patch_problem_statement_rollback_restores_prior() {
        // Rollback invariant: patch ∘ prior restores problem_statement
        // exactly, including its metadata JSON shape.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let mut s = test_spark("sp-1");
        s.metadata = serde_json::json!({
            "intent": {
                "problem_statement": "original",
                "invariants": ["stay consistent"]
            }
        })
        .to_string();
        ws.sparks.push(s);

        let patch = SparkPatch {
            problem_statement: Some("drafted".to_string()),
            ..Default::default()
        };
        let prior = ws.apply_spark_patch("sp-1", &patch).expect("forward apply");
        assert_eq!(
            ws.sparks[0].intent().problem_statement.as_deref(),
            Some("drafted")
        );
        ws.apply_spark_patch("sp-1", &prior)
            .expect("rollback apply");
        assert_eq!(
            ws.sparks[0].intent().problem_statement.as_deref(),
            Some("original")
        );
        assert_eq!(
            ws.sparks[0].intent().invariants,
            vec!["stay consistent".to_string()]
        );
    }

    // ── Description inline edit (ryve-4742d98b) ──────────────────────────
    //
    // These tests pin the state-machine invariants behind the blur-to-save
    // description editor: begin_description_edit must seed the draft from
    // the persisted value, the dirty check must ignore unchanged drafts,
    // the nav guard must defer a selection change when dirty and fall
    // through when not, and revert must leave `spark_edit` in a clean
    // state so the next open is a fresh seed.

    use crate::panel_state::spark_detail::Field;

    #[test]
    fn begin_description_edit_seeds_draft_from_persisted() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();

        // SparkEdit exists and carries the persisted value as the draft.
        let edit = ws.spark_edit.as_ref().expect("edit created");
        assert_eq!(edit.spark_id, "sp-1");
        assert_eq!(
            edit.drafts.get(&Field::Description).map(String::as_str),
            Some("old body")
        );
        // Editor content is seeded with the same value.
        assert!(ws.description_editor.is_some());
    }

    #[test]
    fn begin_description_edit_is_idempotent() {
        // Re-clicking the description mid-edit must not clobber the
        // user's in-progress text. Mirrors the same guard that
        // SparkEdit::begin_edit enforces on the draft map.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();
        // Simulate typing: mirror what DescriptionAction does.
        if let Some(ref mut edit) = ws.spark_edit {
            edit.drafts
                .insert(Field::Description, "mid-edit draft".to_string());
        }
        ws.begin_description_edit();
        assert_eq!(
            ws.spark_edit
                .as_ref()
                .unwrap()
                .drafts
                .get(&Field::Description)
                .map(String::as_str),
            Some("mid-edit draft")
        );
    }

    #[test]
    fn dirty_description_requires_change_from_persisted() {
        // Opening the editor without typing anything must NOT count as
        // dirty — that would trap the user in a prompt on every click.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();
        assert!(ws.dirty_description_spark_id().is_none());

        // Now modify the draft: dirty.
        if let Some(ref mut edit) = ws.spark_edit {
            edit.drafts
                .insert(Field::Description, "new body".to_string());
        }
        assert_eq!(ws.dirty_description_spark_id().as_deref(), Some("sp-1"));
    }

    #[test]
    fn try_change_selected_spark_defers_when_dirty() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.sparks.push(test_spark("sp-2"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();
        if let Some(ref mut edit) = ws.spark_edit {
            edit.drafts.insert(Field::Description, "dirty".to_string());
        }

        let moved = ws.try_change_selected_spark(Some("sp-2".to_string()));
        assert!(!moved, "nav should be deferred");
        assert_eq!(ws.selected_spark.as_deref(), Some("sp-1"));
        let prompt = ws.pending_nav_prompt.as_ref().expect("prompt staged");
        assert_eq!(prompt.target.as_deref(), Some("sp-2"));
        assert_eq!(prompt.dirty_spark_id, "sp-1");
    }

    #[test]
    fn try_change_selected_spark_proceeds_when_not_dirty() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.sparks.push(test_spark("sp-2"));
        ws.selected_spark = Some("sp-1".to_string());
        // Open the editor but don't type anything — not dirty.
        ws.begin_description_edit();

        let moved = ws.try_change_selected_spark(Some("sp-2".to_string()));
        assert!(moved);
        assert_eq!(ws.selected_spark.as_deref(), Some("sp-2"));
        assert!(ws.pending_nav_prompt.is_none());
        // Selection change wipes the editor and any lingering draft.
        assert!(ws.description_editor.is_none());
    }

    #[test]
    fn revert_description_edit_clears_draft_and_editor() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();
        if let Some(ref mut edit) = ws.spark_edit {
            edit.drafts
                .insert(Field::Description, "unwanted".to_string());
        }
        ws.revert_description_edit();
        assert!(ws.description_editor.is_none());
        let edit = ws.spark_edit.as_ref().expect("spark_edit still present");
        assert!(!edit.drafts.contains_key(&Field::Description));
        // Reverting a draft with no persisted change should leave the
        // edit wrapper clean (not dirty) so subsequent nav is free.
        assert!(ws.dirty_description_spark_id().is_none());
    }

    #[test]
    fn take_description_draft_consumes_draft_and_closes_editor() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();
        if let Some(ref mut edit) = ws.spark_edit {
            edit.drafts
                .insert(Field::Description, "committed".to_string());
        }

        let taken = ws.take_description_draft().expect("draft exists");
        assert_eq!(taken.0, "sp-1");
        assert_eq!(taken.1, "committed");
        assert!(ws.description_editor.is_none());
        assert!(
            !ws.spark_edit
                .as_ref()
                .unwrap()
                .drafts
                .contains_key(&Field::Description)
        );
        assert!(ws.take_description_draft().is_none());
    }

    #[test]
    fn empty_description_is_permitted_as_committed_draft() {
        // Acceptance criterion: "Empty description is permitted
        // (unlike title)". A draft that clears the description to ""
        // must round-trip through take_description_draft and be
        // distinguishable from a no-op.
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.sparks.push(test_spark("sp-1"));
        ws.selected_spark = Some("sp-1".to_string());
        ws.begin_description_edit();
        if let Some(ref mut edit) = ws.spark_edit {
            edit.drafts.insert(Field::Description, String::new());
        }
        // An empty draft against a non-empty persisted value IS dirty.
        assert_eq!(ws.dirty_description_spark_id().as_deref(), Some("sp-1"));

        let (id, draft) = ws.take_description_draft().expect("draft exists");
        assert_eq!(id, "sp-1");
        assert_eq!(draft, "");
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

    // ── Arc-wrapped config & ryve_dir (spark ryve-7fc6006f) ──

    #[test]
    fn config_and_ryve_dir_are_arc_wrapped() {
        let ws = Workshop::new(PathBuf::from("/tmp/ryve-arc-test"));
        // Clone is a cheap Arc pointer bump, not a deep copy.
        let config_clone = Arc::clone(&ws.config);
        let ryve_dir_clone = Arc::clone(&ws.ryve_dir);
        assert!(Arc::ptr_eq(&ws.config, &config_clone));
        assert!(Arc::ptr_eq(&ws.ryve_dir, &ryve_dir_clone));
    }

    #[test]
    fn arc_make_mut_allows_config_mutation() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve-arc-mut"));
        // Shared clone first — refcount > 1.
        let shared = Arc::clone(&ws.config);
        assert_eq!(
            shared.background.dim_opacity,
            ws.config.background.dim_opacity
        );

        // make_mut triggers copy-on-write: ws.config gets a new allocation.
        Arc::make_mut(&mut ws.config).background.dim_opacity = 0.42;
        assert!((ws.config.background.dim_opacity - 0.42).abs() < f32::EPSILON);
        // The previously-shared Arc still holds the old value.
        assert!(!Arc::ptr_eq(&ws.config, &shared));
    }

    // ── Branch-name validation (spark ryve-7aa05933) ──

    #[test]
    fn validate_branch_name_accepts_typical_hand_branch() {
        // `<actor>/<short>` — the shape `create_hand_worktree` always builds.
        assert!(validate_git_branch_name("xerxes/abc12345").is_ok());
        assert!(validate_git_branch_name("hand/5fd445bc").is_ok());
        assert!(validate_git_branch_name("actor-1/deadbeef").is_ok());
    }

    #[test]
    fn validate_branch_name_rejects_whitespace() {
        let err = validate_git_branch_name("weird user/abc12345")
            .expect_err("whitespace in actor must fail");
        assert!(err.contains("whitespace"), "got: {err}");
        assert!(err.contains("weird user/abc12345"), "got: {err}");
    }

    #[test]
    fn validate_branch_name_rejects_dotdot() {
        assert!(
            validate_git_branch_name("foo/../bar")
                .unwrap_err()
                .contains("'..'")
        );
    }

    #[test]
    fn validate_branch_name_rejects_reflog_syntax() {
        assert!(
            validate_git_branch_name("foo@{0}/abc12345")
                .unwrap_err()
                .contains("'@{'")
        );
    }

    #[test]
    fn validate_branch_name_rejects_forbidden_metacharacters() {
        for bad in [
            "a~b/c", "a^b/c", "a:b/c", "a?b/c", "a*b/c", "a[b/c", "a\\b/c",
        ] {
            assert!(
                validate_git_branch_name(bad).is_err(),
                "expected {bad} to be rejected"
            );
        }
    }

    #[test]
    fn validate_branch_name_rejects_lock_suffix() {
        assert!(
            validate_git_branch_name("user/abc.lock")
                .unwrap_err()
                .contains(".lock")
        );
    }

    #[test]
    fn validate_branch_name_rejects_boundary_slashes_and_empty() {
        assert!(validate_git_branch_name("").is_err());
        assert!(validate_git_branch_name("/foo").is_err());
        assert!(validate_git_branch_name("foo/").is_err());
        assert!(validate_git_branch_name("@").is_err());
        assert!(validate_git_branch_name(".hidden/abc").is_err());
        assert!(validate_git_branch_name("foo/.hidden").is_err());
        assert!(validate_git_branch_name("foo//bar").is_err());
        assert!(validate_git_branch_name("foo.").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_control_characters() {
        assert!(
            validate_git_branch_name("foo\tbar/abc")
                .unwrap_err()
                .contains("control character")
        );
        assert!(validate_git_branch_name("foo\nbar/abc").is_err());
        assert!(validate_git_branch_name("foo\x7fbar/abc").is_err());
    }

    // ── Settings / Integrations overlay (spark ryve-c3de335e) ────

    #[test]
    fn workshop_default_overlay_is_none() {
        let ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        assert_eq!(ws.active_overlay, WorkshopOverlay::None);
    }

    #[test]
    fn open_settings_seeds_form_from_config() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        Arc::make_mut(&mut ws.config).irc_server = Some("irc.example.com".into());
        Arc::make_mut(&mut ws.config).github.webhook_secret = Some("shh".into());
        Arc::make_mut(&mut ws.config).github.poll_token = Some("ghp_xxx".into());

        ws.open_settings();

        assert_eq!(ws.active_overlay, WorkshopOverlay::Settings);
        assert_eq!(ws.settings_form.irc_server_draft, "irc.example.com");
        assert_eq!(ws.settings_form.webhook_secret_draft, "shh");
        assert_eq!(ws.settings_form.poll_token_draft, "ghp_xxx");
    }

    #[test]
    fn open_integrations_replaces_settings() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.open_settings();
        assert_eq!(ws.active_overlay, WorkshopOverlay::Settings);

        ws.open_integrations();
        assert_eq!(ws.active_overlay, WorkshopOverlay::Integrations);
    }

    #[test]
    fn close_overlay_returns_to_default_state() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        ws.open_integrations();
        assert_eq!(ws.active_overlay, WorkshopOverlay::Integrations);

        ws.close_overlay();
        assert_eq!(ws.active_overlay, WorkshopOverlay::None);
    }

    #[test]
    fn integrations_health_snapshot_matches_config() {
        // UI degradation: when IRC + GitHub are both unconfigured the
        // workshop must still produce a health snapshot rather than
        // panic. The `Disabled` / `Unconfigured` states drive the
        // graceful-degradation rendering in screen::integrations.
        let ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        let irc = ws.integrations_irc_health(false);
        assert_eq!(irc.status, crate::screen::integrations::IrcStatus::Disabled);

        let gh = ws.integrations_github_health();
        assert_eq!(
            gh.mode,
            crate::screen::integrations::GitHubMode::Unconfigured
        );
        assert!(!gh.configured);
    }

    #[test]
    fn integrations_health_reflects_runtime_active() {
        let mut ws = Workshop::new(PathBuf::from("/tmp/ryve"));
        Arc::make_mut(&mut ws.config).irc_server = Some("irc.example.com".into());
        ws.irc_known_epic_ids.insert("ep-1".into());
        ws.irc_known_epic_ids.insert("ep-2".into());

        let irc = ws.integrations_irc_health(true);
        assert_eq!(
            irc.status,
            crate::screen::integrations::IrcStatus::Connected { known_channels: 2 }
        );
    }
    // Spark ryve-4d5881c2: ryve init provisions the workshop IRC daemon.

    #[tokio::test]
    async fn provision_workshop_ircd_writes_conf_and_records_port_on_fresh_workshop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let mut config = WorkshopConfig::default();
        // Invariant check: a fresh config has no bundled port recorded.
        assert!(config.irc_bundled_port.is_none());

        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("provision succeeds on fresh workshop");

        // Workshop config now carries an explicit port + enabled flag.
        let port = config
            .irc_bundled_port
            .expect("provision must record a bundled port");
        assert!(port >= 1024, "ephemeral port should be unprivileged");
        assert!(config.irc_enabled, "irc_enabled must be recorded as true");

        // `.ryve/ircd/ircd.conf` exists with the allocated port and binds
        // only to loopback — the supervisor can cleanly dial it.
        let conf_path = ryve_dir
            .root()
            .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
        assert!(conf_path.exists(), "ircd.conf must be written");
        let body = tokio::fs::read_to_string(&conf_path).await.unwrap();
        assert!(
            body.contains(&format!("Ports = {port}")),
            "ircd.conf must record the allocated port, got:\n{body}"
        );
        assert!(
            body.contains("Listen = 127.0.0.1"),
            "ircd.conf must bind loopback only, got:\n{body}"
        );

        // And the address the supervisor resolves matches the allocated port.
        assert_eq!(
            config.effective_irc_server_address().as_deref(),
            Some(format!("127.0.0.1:{port}").as_str()),
        );
    }

    #[tokio::test]
    async fn provision_workshop_ircd_is_idempotent_on_rerun() {
        // Acceptance: "Running ryve init a second time in the same
        // workshop is idempotent (same port, same config contents)."
        let tmp = tempfile::TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let mut config = WorkshopConfig::default();
        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("first provision");
        let first_port = config.irc_bundled_port.unwrap();

        let conf_path = ryve_dir
            .root()
            .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
        let first_body = tokio::fs::read_to_string(&conf_path).await.unwrap();

        // Re-run provision against the same (already-provisioned)
        // workshop. Idempotency is asserted below by byte-equality of
        // the conf file — a rewrite that happens to produce identical
        // bytes is still "idempotent" on disk, which is the contract
        // this test locks down. If the real concern is "did the file
        // get rewritten at all", a separate mtime-based test would be
        // needed; the acceptance criterion only calls for identical
        // config contents.
        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("second provision");

        assert_eq!(
            config.irc_bundled_port,
            Some(first_port),
            "re-init must not reallocate the port"
        );
        assert!(config.irc_enabled, "re-init must keep irc_enabled = true");

        let second_body = tokio::fs::read_to_string(&conf_path).await.unwrap();
        assert_eq!(
            first_body, second_body,
            "re-init must not rewrite ircd.conf"
        );
    }

    /// Regression for PR #48 Copilot c1: `provision_workshop_ircd`
    /// used to flip `config.irc_enabled = true` on every call, which
    /// clobbered a user who had explicitly set `false` between inits.
    /// The fix is to only set the default on a *fresh* workshop (no
    /// bundled port yet); re-init must preserve the user's opt-out.
    #[tokio::test]
    async fn provision_workshop_ircd_preserves_user_opt_out_on_rerun() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let mut config = WorkshopConfig::default();
        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("first provision");
        let first_port = config.irc_bundled_port.unwrap();
        assert!(
            config.irc_enabled,
            "fresh provision should default irc_enabled = true"
        );

        // Simulate the user explicitly disabling IRC after first init.
        config.irc_enabled = false;

        // Re-run provision. Port stays, conf stays, irc_enabled must
        // NOT be flipped back to true.
        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("second provision");
        assert_eq!(
            config.irc_bundled_port,
            Some(first_port),
            "re-init must not reallocate the port"
        );
        assert!(
            !config.irc_enabled,
            "re-init must preserve an explicit irc_enabled = false opt-out",
        );
    }

    #[tokio::test]
    async fn provision_workshop_ircd_preserves_user_edited_conf() {
        // If a user hand-edits `.ryve/ircd/ircd.conf` between inits
        // (e.g. to add a PASS directive), re-running `ryve init` must
        // not clobber their edits.
        let tmp = tempfile::TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let mut config = WorkshopConfig::default();
        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("first provision");

        let conf_path = ryve_dir
            .root()
            .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
        let marker = "# RYVE-USER-EDIT: hand-tweaked config\n";
        let doctored = format!(
            "{marker}{}",
            tokio::fs::read_to_string(&conf_path).await.unwrap()
        );
        tokio::fs::write(&conf_path, &doctored).await.unwrap();

        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("second provision");

        let after = tokio::fs::read_to_string(&conf_path).await.unwrap();
        assert!(
            after.contains(marker),
            "re-init must preserve user edits to ircd.conf, got:\n{after}"
        );
    }

    #[tokio::test]
    async fn provision_workshop_ircd_regenerates_missing_conf_against_recorded_port() {
        // If the conf file is deleted but the bundled port is still in
        // the config, re-init must regenerate the conf against the
        // already-allocated port (no port churn, no missing daemon
        // config).
        let tmp = tempfile::TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        let mut config = WorkshopConfig {
            irc_bundled_port: Some(45678),
            ..WorkshopConfig::default()
        };

        provision_workshop_ircd(&ryve_dir, &mut config)
            .await
            .expect("provision with pre-recorded port");

        assert_eq!(config.irc_bundled_port, Some(45678));
        let conf_path = ryve_dir
            .root()
            .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
        let body = tokio::fs::read_to_string(&conf_path).await.unwrap();
        assert!(
            body.contains("Ports = 45678"),
            "regenerated conf must use the recorded port, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn init_workshop_auto_provisions_bundled_irc_for_pre_021_workshop() {
        // Hotfix regression: a workshop directory created before 0.2.1's
        // bundled IRC has `irc_enabled = true` (default-on flip from
        // Epic B) but no `irc_bundled_port` and no `.ryve/ircd/`. On
        // first launch under 0.2.1+ the open path must auto-provision
        // the bundled daemon, otherwise the supervisor skips, the IRC
        // client has no dial target, and the status pill stays
        // Disabled with no clear remediation in the UI.
        //
        // PR #51 Copilot c1: drive this through `init_workshop` itself
        // rather than re-implementing the auto-provision branch inline.
        // The earlier revision could have silently regressed a
        // refactor of `init_workshop` that dropped the provision call,
        // because the test never called the function under test.
        let tmp = tempfile::TempDir::new().unwrap();
        let ryve_dir = RyveDir::new(tmp.path());
        ryve_dir.ensure_exists().await.unwrap();

        // Seed a config that mimics the post-upgrade pre-init state:
        // irc_enabled true, no bundled port, no conf. agents.disable_sync
        // is set so `init_workshop` skips the WORKSHOP.md generation
        // step (which spawns external processes and isn't what this
        // test is locking down).
        let pre_upgrade_config = WorkshopConfig {
            irc_enabled: true,
            irc_bundled_port: None,
            agents: data::ryve_dir::AgentsConfig {
                disable_sync: true,
                ..data::ryve_dir::AgentsConfig::default()
            },
            ..WorkshopConfig::default()
        };
        data::ryve_dir::save_config(&ryve_dir, &pre_upgrade_config)
            .await
            .unwrap();

        // Sanity: no conf yet.
        let conf_path = ryve_dir
            .root()
            .join(crate::ircd_process::IRCD_CONFIG_RELATIVE);
        assert!(!conf_path.exists(), "test setup: conf must not exist yet");

        // Drive the actual workshop-open path.
        let init = init_workshop(tmp.path().to_path_buf())
            .await
            .expect("init_workshop must succeed on a writable workshop dir");

        // After init_workshop: the returned config carries the
        // freshly-allocated bundled port, and ircd.conf exists.
        assert!(
            init.config.irc_bundled_port.is_some(),
            "init_workshop must auto-provision a bundled port for \
             pre-0.2.1 workshops"
        );
        assert!(
            conf_path.exists(),
            "init_workshop must auto-write ircd.conf alongside the port"
        );
        // The port round-trips through .ryve/config.toml so the
        // supervisor sees it on next launch.
        let persisted = data::ryve_dir::load_config(&ryve_dir).await;
        assert_eq!(
            persisted.irc_bundled_port, init.config.irc_bundled_port,
            "auto-provisioned port must persist through \
             .ryve/config.toml, not just the returned in-memory config"
        );

        // Second launch is a no-op (idempotency already covered in
        // isolation by `provision_workshop_ircd_is_idempotent_on_rerun`,
        // here we just confirm init_workshop doesn't churn the port).
        let first_port = init.config.irc_bundled_port;
        let second = init_workshop(tmp.path().to_path_buf())
            .await
            .expect("second init_workshop must succeed");
        assert_eq!(
            second.config.irc_bundled_port, first_port,
            "re-opening an already-provisioned workshop must not \
             reallocate the bundled port"
        );
    }

    /// Spark ryve-a64b3a06: when `base_ref` is `Some`, the new Hand branch
    /// must be cut from that ref rather than the current HEAD.
    ///
    /// Strategy: init a repo, seed two commits on `main` (`seed`, then
    /// `tip`). Checkout a throwaway branch so HEAD != main. Call
    /// create_hand_worktree twice:
    ///   1. `base_ref = None` — branch must sit on HEAD (the throwaway's
    ///      commit).
    ///   2. `base_ref = Some("main~1")` — branch must sit on the `seed`
    ///      commit, proving the ref flag was honoured.
    #[tokio::test]
    async fn create_hand_worktree_respects_base_ref() {
        let base = std::env::temp_dir().join(format!("ryve-test-br-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&base).await.unwrap();

        let run_git = |args: Vec<String>| {
            let base = base.clone();
            async move {
                let out = tokio::process::Command::new("git")
                    .args(&args)
                    .current_dir(&base)
                    .env("GIT_AUTHOR_NAME", "ryve-test")
                    .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
                    .env("GIT_COMMITTER_NAME", "ryve-test")
                    .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
                    .output()
                    .await
                    .unwrap();
                assert!(
                    out.status.success(),
                    "git {args:?} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                out
            }
        };

        run_git(vec!["init".into(), "-q".into(), "-b".into(), "main".into()]).await;
        run_git(vec![
            "config".into(),
            "commit.gpgsign".into(),
            "false".into(),
        ])
        .await;
        run_git(vec![
            "commit".into(),
            "-q".into(),
            "--allow-empty".into(),
            "-m".into(),
            "seed".into(),
        ])
        .await;
        run_git(vec![
            "commit".into(),
            "-q".into(),
            "--allow-empty".into(),
            "-m".into(),
            "tip".into(),
        ])
        .await;
        // HEAD is on a throwaway branch so we can tell "HEAD" and "main~1"
        // apart when we read the resulting worktree's commit.
        run_git(vec![
            "checkout".into(),
            "-q".into(),
            "-b".into(),
            "throwaway".into(),
        ])
        .await;
        run_git(vec![
            "commit".into(),
            "-q".into(),
            "--allow-empty".into(),
            "-m".into(),
            "throwaway-tip".into(),
        ])
        .await;

        let ryve_dir = RyveDir::new(&base);

        // Case 1: base_ref = None -> branch sits on current HEAD.
        let session_none = "none00000aaaaaaaaaaaaaaaaaaaaaaa";
        let wt_none = create_hand_worktree(&base, &ryve_dir, session_none, "hand", None)
            .await
            .expect("None base_ref should succeed");
        let head_msg = run_git(vec![
            "log".into(),
            "-1".into(),
            "--format=%s".into(),
            "HEAD".into(),
        ])
        .await;
        let workshop_head = String::from_utf8_lossy(&head_msg.stdout).trim().to_string();
        let wt_none_msg = tokio::process::Command::new("git")
            .args(["log", "-1", "--format=%s", "HEAD"])
            .current_dir(&wt_none)
            .output()
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&wt_none_msg.stdout).trim(),
            workshop_head,
            "worktree with base_ref=None must match the workshop's current HEAD"
        );

        // Case 2: base_ref = Some("main~1") -> branch sits on `seed`.
        let session_ref = "withref00bbbbbbbbbbbbbbbbbbbbbbb";
        let wt_ref = create_hand_worktree(&base, &ryve_dir, session_ref, "hand", Some("main~1"))
            .await
            .expect("Some base_ref should succeed");
        let wt_ref_msg = tokio::process::Command::new("git")
            .args(["log", "-1", "--format=%s", "HEAD"])
            .current_dir(&wt_ref)
            .output()
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&wt_ref_msg.stdout).trim(),
            "seed",
            "worktree with base_ref=Some(main~1) must be cut from that ref"
        );

        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
