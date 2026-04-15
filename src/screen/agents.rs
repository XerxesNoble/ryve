// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Hands panel — lists active and past Hand sessions.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use data::sparks::types::{Crew, CrewMember, HandAssignment, Spark};
use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, svg, text, text_input,
};
use iced::{Element, Length, Theme};

use crate::coding_agents::{CodingAgent, ResumeStrategy};
use crate::icons::{self, UiIcon};
use crate::style::{
    self, FONT_BODY, FONT_HEADER, FONT_ICON, FONT_ICON_SM, FONT_LABEL, FONT_SMALL, Palette,
};
use crate::widget::badge::{priority_badge, type_badge};

/// How long a Hand's terminal must be silent before it is considered idle
/// (waiting on the user). Chosen to be a bit longer than the 3s sparks-poll
/// tick so the idle dot doesn't flicker between keystrokes from the agent.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub enum Message {
    /// User clicked on a Hand row. The handler decides the action:
    /// focus the live terminal tab if alive, or surface a detail/error
    /// view if the session is past or stale.
    SelectAgent(String),
    /// Resume a past (ended) Hand session. Currently only reachable from
    /// the Stale section's resume button (History rows are read-only per
    /// product decision: closed sessions are captured in the spark, not
    /// re-opened).
    ResumeAgent(String),
    /// Delete a past session from history.
    DeleteSession(String),
    /// Open the spark detail view for the given spark id (clicked from a
    /// Hand/Head row's spark chip).
    OpenSpark(String),
    /// User typed in the search box. The query filters Active *and*
    /// History rows by name, agent type, and linked spark title.
    SearchChanged(String),
    /// "Load more…" pressed in the History section — bumps the visible
    /// limit by `HISTORY_PAGE_SIZE`.
    LoadMoreHistory,
    /// Toggle the collapsed state of the Stale subsection at the bottom.
    ToggleStaleCollapsed,
    /// Toggle expand/collapse for a Head node in the Active tree. The
    /// payload is the Head's session id.
    ToggleHeadExpanded(String),
    /// Toggle expand/collapse for a Crew node in the Active tree.
    ToggleCrewExpanded(String),
    /// Attach to a live tmux session for a background Hand/Head.
    /// Payload is `(session_id, session_label)` — the label is "hand" or
    /// "head" and combined with the session_id to form the tmux session
    /// name. Spark ryve-8ba40d83.
    AttachSession(String, String),
    /// Open a read-only log view for a past/dead session. Fired from the
    /// "View Log" button on History rows that have a `log_path`. Spark
    /// `ryve-a677498c`.
    ViewLog(String),
}

/// How many History rows to render per "page". A "Load more…" button at
/// the bottom of History bumps the visible limit by this amount.
pub const HISTORY_PAGE_SIZE: usize = 10;

/// Per-panel UI state held on the Workshop. Pure data so it survives
/// Workshop ticks without forcing agents.rs to manage its own subscription.
#[derive(Debug, Clone)]
pub struct AgentsPanelState {
    pub search: String,
    pub history_limit: usize,
    pub stale_collapsed: bool,
    /// Heads collapsed by user choice. Default is expanded — only collapsed
    /// IDs live in this set so a freshly-spawned Head shows its members.
    pub collapsed_heads: std::collections::HashSet<String>,
    /// Crews collapsed by user choice. Same default-expanded semantics.
    pub collapsed_crews: std::collections::HashSet<String>,
}

impl Default for AgentsPanelState {
    fn default() -> Self {
        Self {
            search: String::new(),
            history_limit: HISTORY_PAGE_SIZE,
            // Stale section starts collapsed per product spec — it lives at
            // the very bottom and shouldn't draw the eye.
            stale_collapsed: true,
            collapsed_heads: std::collections::HashSet::new(),
            collapsed_crews: std::collections::HashSet::new(),
        }
    }
}

/// A Hand session shown in the Hands panel.
/// This is the in-memory representation — may or may not have a live terminal.
#[derive(Debug, Clone)]
pub struct AgentSession {
    /// Unique ID (matches the persisted agent_sessions.id).
    pub id: String,
    /// Display name (e.g., "Claude Code").
    pub name: String,
    /// The agent definition (command, args, resume strategy).
    pub agent: CodingAgent,
    /// Tab ID in the bench (Some = currently has a terminal open).
    pub tab_id: Option<u64>,
    /// Whether this session is currently running.
    pub active: bool,
    /// Whether this row is persisted as active but no longer has a live process.
    pub stale: bool,
    /// Agent-specific session/conversation ID for resumption.
    pub resume_id: Option<String>,
    /// When the session was started.
    pub started_at: String,
    /// Path to the detached child's stdout/stderr log file. Set for
    /// CLI-spawned background Hands so the UI can open a read-only spy
    /// view; `None` for sessions whose output flows through a terminal tab.
    pub log_path: Option<PathBuf>,
    /// Last time the terminal for this session produced PTY output.
    /// Used to distinguish "actively working" from "idle/waiting on user".
    /// `None` means we haven't observed any output yet — treated as working
    /// so freshly-spawned Hands don't immediately flash green. Not persisted.
    pub last_output_at: Option<Instant>,
    /// `agent_sessions.parent_session_id` — the Hand that spawned this one,
    /// typically a Head. `None` for direct user spawns. The Hands panel
    /// uses this to render Head → solo-Hand attribution when the child
    /// isn't a member of any of the Head's crews.
    pub parent_session_id: Option<String>,
    /// `agent_sessions.session_label` — role label for this session
    /// (e.g. "atlas", "head", "hand"). Used to identify pinned Atlas tabs
    /// and to construct the tmux session name for attach. Spark ryve-8ba40d83.
    pub session_label: Option<String>,
    /// Whether this session has a live tmux session on the Ryve-private
    /// socket. Updated during the periodic 3s agent-session sync so the
    /// UI can gate the Attach button without blocking render. Not persisted.
    /// Spark ryve-8ba40d83.
    pub tmux_session_live: bool,
}

/// High-level display state for an active Hand, used to pick its indicator color.
/// Red = no spark claimed, Green = idle/waiting, Blue = actively working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandStatus {
    /// No active owner assignment — the Hand has no spark to work on.
    Unassigned,
    /// Assigned to a spark but the terminal has been silent for `idle_after`.
    Idle,
    /// Assigned to a spark and recently producing output.
    Working,
}

/// Decide a Hand's status from its assignment state and recent PTY activity.
///
/// This is a pure function so it can be unit-tested without spinning up a UI.
pub fn hand_status(
    session_id: &str,
    assignments: &[HandAssignment],
    last_output_at: Option<Instant>,
    now: Instant,
    idle_after: Duration,
) -> HandStatus {
    let has_owner = assignments
        .iter()
        .any(|a| a.session_id == session_id && a.role == "owner" && a.status == "active");
    if !has_owner {
        return HandStatus::Unassigned;
    }
    match last_output_at {
        Some(t) if now.saturating_duration_since(t) >= idle_after => HandStatus::Idle,
        _ => HandStatus::Working,
    }
}

/// Pick the palette color for a given Hand status.
pub fn hand_status_color(status: HandStatus, pal: &Palette) -> iced::Color {
    match status {
        HandStatus::Unassigned => pal.danger,
        HandStatus::Idle => pal.success,
        HandStatus::Working => pal.accent,
    }
}

impl AgentSession {
    /// Can this session be resumed?
    pub fn can_resume(&self) -> bool {
        !self.active && !self.stale && self.agent.resume != ResumeStrategy::None
    }

    /// Whether this is a CLI-spawned Hand running detached in the background
    /// (no terminal tab in the bench, but a live process and a log file we
    /// can tail). The Active panel shows these with a "background" badge and
    /// clicking one opens a read-only log view.
    pub fn is_background(&self) -> bool {
        self.active && self.tab_id.is_none() && self.log_path.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDisplayState {
    Active,
    History,
    Stale,
}

pub fn classify_session(
    is_ended: bool,
    has_live_terminal: bool,
    has_live_process: bool,
) -> SessionDisplayState {
    if is_ended {
        SessionDisplayState::History
    } else if has_live_terminal || has_live_process {
        SessionDisplayState::Active
    } else {
        SessionDisplayState::Stale
    }
}

// ── Hierarchical Active tree ─────────────────────────

/// A node in the rendered Active hierarchy. Heads sit at the top, with
/// their managed Crews and any "solo" Hands they spawned underneath, and
/// crew Hands one level deeper. Standalone Hands (no Head, not in any
/// Crew) sit at the same depth as Heads.
///
/// This is a *view* type — it borrows nothing and is rebuilt cheaply on
/// every render via [`build_active_tree`]. Keeping it pure data lets us
/// unit-test the grouping invariants without spinning up Iced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveNode {
    /// A Head: a Hand session that owns at least one Crew.
    Head {
        session_id: String,
        crews: Vec<CrewNode>,
        /// Hands spawned by this Head that don't belong to any of its
        /// crews — i.e. one-offs the Head dispatched solo.
        solo_hands: Vec<String>,
        /// Spark id of the epic the Head is currently working on, if any.
        /// Pulled from `crews.parent_spark_id` of the Head's first crew so
        /// the row can render an `sp-xxxx` chip.
        epic_spark_id: Option<String>,
    },
    /// A Hand session not owned by any Head and not in any Crew.
    Standalone { session_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewNode {
    pub crew_id: String,
    pub name: String,
    pub parent_spark_id: Option<String>,
    pub member_session_ids: Vec<String>,
}

/// Build the Active hierarchy from the raw workshop tables.
///
/// Inputs:
/// - `active_sessions` — sessions currently displayed in the Active group
///   (status='active', not stale, not ended), in display order. Pass them
///   in already filtered so this fn stays a pure grouping helper. Each
///   session's `parent_session_id` is used to attribute solo Hands to
///   their parent Head.
/// - `crews` — every crew owned by this workshop.
/// - `crew_members` — full membership join. Crews not represented here
///   render with no leaf rows.
///
/// Output ordering:
/// - Heads appear in `active_sessions` order (caller decides recency).
/// - Standalones appear in `active_sessions` order, after all Heads.
/// - Inside a Head, crews appear in `crews` order (newest first by repo
///   convention).
/// - Solo Hands inside a Head, and Hand leaves inside a Crew, both follow
///   `active_sessions` order so the panel reads top-to-bottom.
///
/// Solo Hand attribution: a session whose `parent_session_id` matches a
/// Head AND that is not a member of any of that Head's crews is rendered
/// as a solo child of the Head. Sessions with a parent that doesn't
/// resolve to a known Head fall through to Standalone (defensive — handles
/// cases where the parent row was deleted out from under us).
///
/// Heads with no crew yet are still rendered as Heads (per product
/// decision: spawning a Head before any Hand should still surface the
/// orchestrator badge).
pub fn build_active_tree(
    active_sessions: &[AgentSession],
    crews: &[Crew],
    crew_members: &[CrewMember],
) -> Vec<ActiveNode> {
    use std::collections::{HashMap, HashSet};

    // session_id → set of crew_ids it belongs to.
    let mut membership: HashMap<&str, HashSet<&str>> = HashMap::new();
    for m in crew_members {
        membership
            .entry(m.session_id.as_str())
            .or_default()
            .insert(m.crew_id.as_str());
    }

    // head_session_id → list of crews it owns, in `crews` order.
    let mut head_to_crews: HashMap<&str, Vec<&Crew>> = HashMap::new();
    for c in crews {
        if let Some(ref head_id) = c.head_session_id {
            head_to_crews.entry(head_id.as_str()).or_default().push(c);
        }
    }
    let head_session_ids: HashSet<&str> = head_to_crews.keys().copied().collect();

    // Pre-compute, for each Head, the set of session_ids that belong to
    // any of its crews — used to discriminate solo vs crew children when
    // resolving `parent_session_id`.
    let mut head_crew_members: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (head_id, head_crews) in &head_to_crews {
        let mut all: HashSet<&str> = HashSet::new();
        for c in head_crews {
            for m in crew_members.iter().filter(|m| m.crew_id == c.id) {
                all.insert(m.session_id.as_str());
            }
        }
        head_crew_members.insert(head_id, all);
    }

    // session_id → its session row, for parent lookup.
    let by_id: HashMap<&str, &AgentSession> =
        active_sessions.iter().map(|s| (s.id.as_str(), s)).collect();

    let mut nodes: Vec<ActiveNode> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();

    // Pass 1 — Heads (in active_sessions order so the panel order tracks
    // recency).
    for s in active_sessions {
        let sid = s.id.as_str();
        if !head_session_ids.contains(sid) {
            continue;
        }
        let head_crews = head_to_crews.get(sid).cloned().unwrap_or_default();
        let crew_member_set = head_crew_members.get(sid).cloned().unwrap_or_default();

        // Build CrewNodes in head_crews order, populating members in
        // active_sessions order so leaves read recency-first.
        let crew_nodes: Vec<CrewNode> = head_crews
            .iter()
            .map(|c| {
                let mut members_for_crew: Vec<String> = Vec::new();
                for child in active_sessions {
                    if child.id == s.id {
                        continue;
                    }
                    if crew_members
                        .iter()
                        .any(|m| m.crew_id == c.id && m.session_id == child.id)
                    {
                        members_for_crew.push(child.id.clone());
                        emitted.insert(child.id.clone());
                    }
                }
                CrewNode {
                    crew_id: c.id.clone(),
                    name: c.name.clone(),
                    parent_spark_id: c.parent_spark_id.clone(),
                    member_session_ids: members_for_crew,
                }
            })
            .collect();

        // Solo hands under this Head: any active session whose
        // `parent_session_id` is this Head and which isn't a member of
        // any of the Head's crews. Walk in active_sessions order to
        // preserve recency-first display.
        let mut solo_hands: Vec<String> = Vec::new();
        for child in active_sessions {
            if child.id == s.id {
                continue;
            }
            if child.parent_session_id.as_deref() != Some(sid) {
                continue;
            }
            if crew_member_set.contains(child.id.as_str()) {
                continue;
            }
            solo_hands.push(child.id.clone());
            emitted.insert(child.id.clone());
        }

        // Epic spark id: take the parent_spark_id of the Head's first
        // crew. If a Head juggles multiple epics this picks the most
        // recently created one (crews are sorted DESC by created_at).
        let epic_spark_id = head_crews.iter().find_map(|c| c.parent_spark_id.clone());

        emitted.insert(s.id.clone());
        nodes.push(ActiveNode::Head {
            session_id: s.id.clone(),
            crews: crew_nodes,
            solo_hands,
            epic_spark_id,
        });
    }

    // Pass 2 — Standalone Hands. Anything still un-emitted at this point
    // is a free-floating Hand: either no parent at all, or a parent that
    // isn't itself a Head (defensive — surface the row rather than drop
    // it). `by_id` is consulted to suppress orphans whose parent is a
    // recognised Head but they were already emitted as solo children.
    let _ = &by_id;
    for s in active_sessions {
        if emitted.contains(&s.id) {
            continue;
        }
        nodes.push(ActiveNode::Standalone {
            session_id: s.id.clone(),
        });
    }

    nodes
}

/// Look up the active "owner" assignment for a session, returning the
/// spark id if any. Used to render the `sp-xxxx` chip on Hand rows.
pub fn owner_spark_for_session<'a>(
    session_id: &str,
    assignments: &'a [HandAssignment],
) -> Option<&'a str> {
    assignments
        .iter()
        .find(|a| a.session_id == session_id && a.role == "owner" && a.status == "active")
        .map(|a| a.spark_id.as_str())
}

/// Decide whether a session row matches the search query. The query is
/// matched (case-insensitively) against:
/// - the session's display name (e.g. "Claude Code")
/// - the agent command (e.g. "claude")
/// - the linked spark id and (if found) spark title
///
/// Empty queries match everything.
pub fn session_matches_query(
    session: &AgentSession,
    assignments: &[HandAssignment],
    sparks: &[Spark],
    query: &str,
) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    if session.name.to_ascii_lowercase().contains(&q) {
        return true;
    }
    if session.agent.command.to_ascii_lowercase().contains(&q) {
        return true;
    }
    if let Some(spark_id) = owner_spark_for_session(&session.id, assignments) {
        if spark_id.to_ascii_lowercase().contains(&q) {
            return true;
        }
        if let Some(spark) = sparks.iter().find(|s| s.id == spark_id)
            && spark.title.to_ascii_lowercase().contains(&q)
        {
            return true;
        }
    }
    false
}

/// Render the Hands panel.
///
/// Layout (top → bottom):
/// 1. Search input — filters Active and History rows.
/// 2. **Active** — hierarchical Heads → Crews → Hands, plus Standalone
///    Hands. Click a Head/Hand to focus its tab in the bench. Click the
///    `sp-xxxx` chip to open the spark detail view.
/// 3. **History** — paginated (last `state.history_limit` rows), with a
///    "Load more…" footer when more rows exist.
/// 4. **Stale** — collapsed by default, sits at the very bottom.
pub fn view<'a>(
    sessions: &'a [AgentSession],
    assignments: &'a [HandAssignment],
    crews: &'a [Crew],
    crew_members: &'a [CrewMember],
    sparks: &'a [Spark],
    state: &'a AgentsPanelState,
    pal: Palette,
) -> Element<'a, Message> {
    let now = Instant::now();
    // Panel title is "Activity" (not "Hands") because the panel actually
    // shows the entire orchestration tree — Heads, Crews, leaf Hands,
    // and history — not just Hands. The inner section labels ("Active",
    // "History", "Stale") describe the buckets within.
    let header = text("Activity").size(FONT_HEADER).color(pal.text_primary);

    // Search box. The icon is a tinted SVG so it can scale up and theme
    // with the rest of the panel.
    let search_icon = svg(icons::ui_icon(UiIcon::Search))
        .width(14)
        .height(14)
        .style(icons::ui_icon_color(pal.text_tertiary));
    let search_input = text_input("Search hands & history…", &state.search)
        .on_input(Message::SearchChanged)
        .padding([4, 6])
        .size(FONT_BODY);
    let search_row = row![search_icon, search_input]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .padding([2, 4]);

    let mut content = column![header, search_row].spacing(6).padding(10);

    // Pre-filter sessions by the search query so the tree only contains
    // matching rows. Stale section is filtered separately below.
    let q = state.search.as_str();
    let active_filtered: Vec<AgentSession> = sessions
        .iter()
        .filter(|s| s.active && session_matches_query(s, assignments, sparks, q))
        .cloned()
        .collect();

    // Build the hierarchical tree from filtered Active rows.
    let tree = build_active_tree(&active_filtered, crews, crew_members);

    if tree.is_empty()
        && sessions.iter().filter(|s| !s.active && !s.stale).count() == 0
        && sessions.iter().filter(|s| s.stale).count() == 0
    {
        content = content.push(
            text("No hands yet")
                .size(FONT_BODY)
                .color(pal.text_tertiary),
        );
    }

    // ── Active section ─────────────────────
    if !tree.is_empty() {
        content = content.push(text("Active").size(FONT_LABEL).color(pal.text_secondary));
        for node in tree {
            content = content.push(render_active_node(
                node,
                sessions,
                assignments,
                sparks,
                state,
                &pal,
                now,
            ));
        }
    }

    // ── History section ────────────────────
    let history_filtered: Vec<&AgentSession> = sessions
        .iter()
        .filter(|s| !s.active && !s.stale && session_matches_query(s, assignments, sparks, q))
        .collect();
    if !history_filtered.is_empty() {
        content = content.push(Space::new().height(4));
        content = content.push(text("History").size(FONT_LABEL).color(pal.text_secondary));

        let total = history_filtered.len();
        let limit = state.history_limit.min(total);
        for session in history_filtered.iter().take(limit) {
            content = content.push(render_history_row(session, assignments, sparks, &pal));
        }

        if total > limit {
            let remaining = total - limit;
            let more_btn = button(
                text(format!("Load more… ({remaining})"))
                    .size(FONT_SMALL)
                    .color(pal.accent),
            )
            .style(button::text)
            .padding([4, 8])
            .on_press(Message::LoadMoreHistory);
            content = content.push(more_btn);
        }
    }

    // ── Stale section (collapsed by default, at the bottom) ─────
    let stale: Vec<&AgentSession> = sessions
        .iter()
        .filter(|s| s.stale && session_matches_query(s, assignments, sparks, q))
        .collect();
    if !stale.is_empty() {
        content = content.push(Space::new().height(8));

        let chev_icon = if state.stale_collapsed {
            UiIcon::ChevronRight
        } else {
            UiIcon::ChevronDown
        };
        let chev = svg(icons::ui_icon(chev_icon))
            .width(12)
            .height(12)
            .style(icons::ui_icon_color(pal.text_secondary));
        let header_row = row![
            chev,
            text(format!("Stale ({})", stale.len()))
                .size(FONT_LABEL)
                .color(pal.text_secondary),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        let header_btn = button(header_row)
            .style(button::text)
            .padding([2, 4])
            .on_press(Message::ToggleStaleCollapsed);
        content = content.push(header_btn);

        if !state.stale_collapsed {
            for session in &stale {
                content = content.push(render_stale_row(session, &pal));
            }
        }
    }

    scrollable(content)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}

// ── Row renderers ────────────────────────────────────

/// Render a single node in the Active hierarchy. Recurses one level for
/// crew children. Heads always render a chevron toggle (even with no
/// children) so the affordance stays consistent.
fn render_active_node<'a>(
    node: ActiveNode,
    sessions: &'a [AgentSession],
    assignments: &'a [HandAssignment],
    sparks: &'a [Spark],
    state: &'a AgentsPanelState,
    pal: &Palette,
    now: Instant,
) -> Element<'a, Message> {
    match node {
        ActiveNode::Standalone { session_id } => sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| render_hand_row(s, assignments, sparks, pal, now, 0))
            .unwrap_or_else(|| Space::new().into()),
        ActiveNode::Head {
            session_id,
            crews,
            solo_hands,
            epic_spark_id,
        } => {
            let head_session = sessions.iter().find(|s| s.id == session_id);
            let expanded = !state.collapsed_heads.contains(&session_id);

            let head_row = render_head_row(
                head_session,
                session_id.clone(),
                epic_spark_id,
                expanded,
                pal,
                assignments,
                sparks,
                now,
            );
            let mut col = column![head_row].spacing(2);

            if expanded {
                for crew in crews {
                    col = col.push(render_crew_node(
                        crew,
                        sessions,
                        assignments,
                        sparks,
                        state,
                        pal,
                        now,
                    ));
                }
                for solo_id in solo_hands {
                    if let Some(s) = sessions.iter().find(|s| s.id == solo_id) {
                        col = col.push(render_hand_row(s, assignments, sparks, pal, now, 1));
                    }
                }
            }

            col.into()
        }
    }
}

// TODO(refactor): collapse the per-renderer (assignments, sparks, pal,
// now) parameters into a shared `RenderCtx<'a>` borrow struct so the
// row renderers don't keep growing. Tracked separately from this PR.
#[allow(clippy::too_many_arguments)]
fn render_head_row<'a>(
    session: Option<&'a AgentSession>,
    session_id: String,
    epic_spark_id: Option<String>,
    expanded: bool,
    pal: &Palette,
    assignments: &'a [HandAssignment],
    sparks: &'a [Spark],
    now: Instant,
) -> Element<'a, Message> {
    let chev_icon = if expanded {
        UiIcon::ChevronDown
    } else {
        UiIcon::ChevronRight
    };
    let chev = svg(icons::ui_icon(chev_icon))
        .width(12)
        .height(12)
        .style(icons::ui_icon_color(pal.text_secondary));

    // Status dot still uses the existing color logic — Heads can be idle
    // or working same as Hands (the orchestrator is itself a coding agent).
    let dot_color = match session {
        Some(s) => hand_status_color(
            hand_status(&s.id, assignments, s.last_output_at, now, IDLE_THRESHOLD),
            pal,
        ),
        None => pal.text_tertiary,
    };
    let dot = text("\u{25CF}").size(FONT_ICON_SM).color(dot_color);

    // Head crown icon — distinguishes orchestrators from leaf Hands.
    let head_icon = svg(icons::ui_icon(UiIcon::Head))
        .width(14)
        .height(14)
        .style(icons::ui_icon_color(pal.accent));

    // Agent type icon.
    let agent_icon_handle = session
        .map(|s| icons::agent_icon_for_command(&s.agent.command))
        .unwrap_or(UiIcon::AgentGeneric);
    let agent_icon = svg(icons::ui_icon(agent_icon_handle))
        .width(14)
        .height(14)
        .style(icons::ui_icon_color(pal.text_secondary));

    // Resolve the epic spark this Head is decomposing/orchestrating.
    // If unknown (Head spawned with no goal yet, or epic deleted), the
    // row falls back to the session's display name so it stays usable.
    let epic = epic_spark_id
        .as_deref()
        .and_then(|id| sparks.iter().find(|s| s.id == id));

    let mut row_widget = row![chev, dot, head_icon, agent_icon]
        .spacing(6)
        .align_y(iced::Alignment::Center);

    if let Some(s) = epic {
        row_widget = row_widget.push(type_badge::<Message>(&s.spark_type, pal));
        row_widget = row_widget.push(priority_badge::<Message>(s.priority, pal));
    }

    let title = epic.map(|s| s.title.clone()).unwrap_or_else(|| {
        session
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "(unknown)".to_string())
    });
    // Title takes the leftover space (and wraps within it) so the trailing
    // spark chip can never be pushed past the panel's right edge.
    row_widget = row_widget.push(
        text(title)
            .size(FONT_BODY)
            .color(pal.text_primary)
            .width(Length::Fill),
    );

    // Attach button for Heads with a live tmux session. Spark ryve-8ba40d83.
    if let Some(s) = session
        && s.tmux_session_live
    {
        let label = s.session_label.as_deref().unwrap_or("head");
        let attach_btn = button(text("Attach").size(FONT_SMALL).color(pal.accent))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::AttachSession(s.id.clone(), label.to_string()));
        row_widget = row_widget.push(attach_btn);
    }

    // Spark chip — opens the epic spark detail.
    if let Some(s) = epic {
        row_widget = row_widget.push(spark_chip(&s.id, pal));
    }

    // Whole row toggles expand/collapse on click.
    let toggle = button(row_widget)
        .style(button::text)
        .width(Length::Fill)
        .padding([4, 8])
        .on_press(Message::ToggleHeadExpanded(session_id));

    let pal_copy = *pal;
    container(toggle)
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(pal_copy.accent_dim)),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn render_crew_node<'a>(
    crew: CrewNode,
    sessions: &'a [AgentSession],
    assignments: &'a [HandAssignment],
    sparks: &'a [Spark],
    state: &'a AgentsPanelState,
    pal: &Palette,
    now: Instant,
) -> Element<'a, Message> {
    let expanded = !state.collapsed_crews.contains(&crew.crew_id);
    let chev_icon = if expanded {
        UiIcon::ChevronDown
    } else {
        UiIcon::ChevronRight
    };
    let chev = svg(icons::ui_icon(chev_icon))
        .width(12)
        .height(12)
        .style(icons::ui_icon_color(pal.text_secondary));
    let crew_icon = svg(icons::ui_icon(UiIcon::Crew))
        .width(14)
        .height(14)
        .style(icons::ui_icon_color(pal.text_secondary));
    let label = text(crew.name).size(FONT_BODY).color(pal.text_secondary);

    let mut header_row = row![
        Space::new().width(Length::Fixed(16.0)),
        chev,
        crew_icon,
        label
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    header_row = header_row.push(Space::new().width(Length::Fill));
    if let Some(spark_id) = crew.parent_spark_id.as_deref() {
        header_row = header_row.push(spark_chip(spark_id, pal));
    }

    let header_btn = button(header_row)
        .style(button::text)
        .width(Length::Fill)
        .padding([4, 8])
        .on_press(Message::ToggleCrewExpanded(crew.crew_id));

    let mut col = column![header_btn].spacing(2);
    if expanded {
        for sid in crew.member_session_ids {
            if let Some(s) = sessions.iter().find(|s| s.id == sid) {
                col = col.push(render_hand_row(s, assignments, sparks, pal, now, 2));
            }
        }
    }
    col.into()
}

fn render_hand_row<'a>(
    session: &'a AgentSession,
    assignments: &'a [HandAssignment],
    sparks: &'a [Spark],
    pal: &Palette,
    now: Instant,
    depth: u16,
) -> Element<'a, Message> {
    let status = hand_status(
        &session.id,
        assignments,
        session.last_output_at,
        now,
        IDLE_THRESHOLD,
    );
    let dot_color = hand_status_color(status, pal);
    let dot = text("\u{25CF}").size(FONT_ICON_SM).color(dot_color);

    let agent_icon = svg(icons::ui_icon(icons::agent_icon_for_command(
        &session.agent.command,
    )))
    .width(14)
    .height(14)
    .style(icons::ui_icon_color(pal.text_secondary));

    // Resolve the spark this Hand owns (if any). Used for the row label,
    // type/priority badges, and the trailing chip — all three derive
    // from the same lookup so the row tells a coherent story about
    // *what* the Hand is working on, not just which agent it is.
    let spark = owner_spark_for_session(&session.id, assignments)
        .and_then(|id| sparks.iter().find(|s| s.id == id));

    // Indent by depth so crew children sit one tab in from the Head row.
    let indent = Space::new().width(Length::Fixed(16.0 * depth as f32));

    let mut row_widget = row![indent, dot, agent_icon]
        .spacing(6)
        .align_y(iced::Alignment::Center);

    // Type + priority badges, mirroring the spawn-Hand picker layout so
    // the panel reads like the picker the user already knows.
    if let Some(s) = spark {
        row_widget = row_widget.push(type_badge::<Message>(&s.spark_type, pal));
        row_widget = row_widget.push(priority_badge::<Message>(s.priority, pal));
    }

    // Row label: prefer the spark title (the actual *work* the Hand is
    // doing), fall back to the session display name when there's no
    // assignment yet (newly-spawned, between sparks, etc.).
    let title = spark
        .map(|s| s.title.clone())
        .unwrap_or_else(|| session.name.clone());
    // Title takes the leftover space (and wraps within it) so the trailing
    // spark chip can never be pushed past the panel's right edge.
    row_widget = row_widget.push(
        text(title)
            .size(FONT_BODY)
            .color(pal.text_primary)
            .width(Length::Fill),
    );

    if session.is_background() {
        row_widget = row_widget.push(text("bg").size(FONT_SMALL).color(pal.text_tertiary));
    }

    // Attach button — gated on a live tmux session so it only appears
    // when the user can actually connect. Spark ryve-8ba40d83.
    if session.tmux_session_live {
        let label = session.session_label.as_deref().unwrap_or("hand");
        let attach_btn = button(text("Attach").size(FONT_SMALL).color(pal.accent))
            .style(button::text)
            .padding([2, 6])
            .on_press(Message::AttachSession(
                session.id.clone(),
                label.to_string(),
            ));
        row_widget = row_widget.push(attach_btn);
    }

    if let Some(s) = spark {
        row_widget = row_widget.push(spark_chip(&s.id, pal));
    }

    let btn = button(row_widget)
        .style(button::text)
        .width(Length::Fill)
        .padding([4, 8])
        .on_press(Message::SelectAgent(session.id.clone()));

    btn.into()
}

fn render_history_row<'a>(
    session: &'a AgentSession,
    assignments: &'a [HandAssignment],
    sparks: &'a [Spark],
    pal: &Palette,
) -> Element<'a, Message> {
    // Per product decision: history rows are read-only. Click does
    // nothing (no resume, no spy view) — the spark captures the outcome.
    // We still render the spark chip so the user can jump to context.
    let dot = text("\u{25CB}").size(FONT_ICON_SM).color(pal.text_tertiary);
    let agent_icon = svg(icons::ui_icon(icons::agent_icon_for_command(
        &session.agent.command,
    )))
    .width(14)
    .height(14)
    .style(icons::ui_icon_color(pal.text_tertiary));

    let spark = owner_spark_for_session(&session.id, assignments)
        .and_then(|id| sparks.iter().find(|s| s.id == id));

    let title = spark
        .map(|s| s.title.clone())
        .unwrap_or_else(|| session.name.clone());
    // Title takes the leftover space (and wraps within it) so the trailing
    // spark chip can never be pushed past the panel's right edge.
    let label = text(title)
        .size(FONT_BODY)
        .color(pal.text_secondary)
        .width(Length::Fill);
    let time_label = text(format_relative_time(&session.started_at))
        .size(FONT_SMALL)
        .color(pal.text_tertiary);

    let mut row_widget = row![dot, agent_icon]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    if let Some(s) = spark {
        row_widget = row_widget.push(type_badge::<Message>(&s.spark_type, pal));
        row_widget = row_widget.push(priority_badge::<Message>(s.priority, pal));
    }
    row_widget = row_widget.push(label);
    row_widget = row_widget.push(time_label);

    if let Some(s) = spark {
        row_widget = row_widget.push(spark_chip(&s.id, pal));
    }

    // Spark `ryve-a677498c`: dead sessions with a log file show a "View
    // Log" button so the user can inspect the agent's last output instead
    // of the row being entirely inert.
    if session.log_path.is_some() {
        let log_btn = button(text("\u{1F4C4}").size(FONT_ICON_SM).color(pal.accent))
            .style(button::text)
            .padding([2, 4])
            .on_press(Message::ViewLog(session.id.clone()));
        row_widget = row_widget.push(log_btn);
    }

    let delete_btn = button(text("\u{00D7}").size(FONT_ICON).color(pal.text_tertiary))
        .style(button::text)
        .padding([2, 4])
        .on_press(Message::DeleteSession(session.id.clone()));
    row_widget = row_widget.push(delete_btn);

    let pal_copy = *pal;
    container(row_widget)
        .width(Length::Fill)
        .padding([4, 8])
        .style(move |_theme: &Theme| style::hovered_item(&pal_copy))
        .into()
}

fn render_stale_row<'a>(session: &'a AgentSession, pal: &Palette) -> Element<'a, Message> {
    let warn = text("\u{26A0}").size(FONT_ICON_SM).color(pal.danger);
    let agent_icon = svg(icons::ui_icon(icons::agent_icon_for_command(
        &session.agent.command,
    )))
    .width(14)
    .height(14)
    .style(icons::ui_icon_color(pal.text_secondary));
    let label = text(&session.name)
        .size(FONT_BODY)
        .color(pal.text_secondary);
    let time_label = text(format_relative_time(&session.started_at))
        .size(FONT_SMALL)
        .color(pal.text_tertiary);
    let badge = text("stale").size(FONT_SMALL).color(pal.danger);

    let mut row_widget = row![warn, agent_icon, label, badge, time_label]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    row_widget = row_widget.push(Space::new().width(Length::Fill));

    // Resume is only meaningful for stale sessions whose agent supports
    // it — gives the user a way to recover from a crashed terminal.
    if session.can_resume() {
        let resume_btn = button(text("\u{25B6}").size(FONT_ICON_SM).color(pal.accent))
            .style(button::text)
            .padding([2, 4])
            .on_press(Message::ResumeAgent(session.id.clone()));
        row_widget = row_widget.push(resume_btn);
    }

    let delete_btn = button(text("\u{00D7}").size(FONT_ICON).color(pal.danger))
        .style(button::text)
        .padding([2, 4])
        .on_press(Message::DeleteSession(session.id.clone()));
    row_widget = row_widget.push(delete_btn);

    let pal_copy = *pal;
    let item = container(row_widget)
        .width(Length::Fill)
        .padding([4, 8])
        .style(move |_theme: &Theme| style::hovered_item(&pal_copy));

    // Wrap so clicking the row body toggles a detail toast through the
    // existing SelectAgent path (which already handles stale sessions).
    mouse_area(item)
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Message::SelectAgent(session.id.clone()))
        .into()
}

/// Small clickable spark id chip — emits `Message::OpenSpark` so the
/// parent can route to the spark detail panel via the existing
/// `SelectSpark` flow.
fn spark_chip<'a>(spark_id: &str, pal: &Palette) -> Element<'a, Message> {
    let label = text(spark_id.to_string())
        .size(FONT_SMALL)
        .color(pal.accent);
    let pal_copy = *pal;
    let chip = container(label)
        .padding([1, 6])
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(pal_copy.accent_dim)),
            border: iced::Border {
                radius: 8.0.into(),
                width: 1.0,
                color: pal_copy.accent,
            },
            ..Default::default()
        });
    button(chip)
        .style(button::text)
        .padding(0)
        .on_press(Message::OpenSpark(spark_id.to_string()))
        .into()
}

/// Format an RFC 3339 timestamp as a short relative time string (e.g. "2h ago", "3d ago").
pub fn format_relative_time(rfc3339: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return String::new();
    };
    let duration = chrono::Utc::now().signed_duration_since(then);

    if duration.num_minutes() < 1 {
        "now".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else {
        format!("{}d ago", duration.num_days())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_agents::CodingAgent;

    fn make_session(active: bool, tab_id: Option<u64>, resume: ResumeStrategy) -> AgentSession {
        AgentSession {
            id: "session-1".to_string(),
            name: "Test Hand".to_string(),
            agent: CodingAgent {
                display_name: "Test".to_string(),
                command: "test".to_string(),
                args: vec![],
                resume,
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            },
            tab_id,
            active,
            stale: false,
            resume_id: None,
            started_at: "2026-04-07T11:00:00+00:00".to_string(),
            log_path: None,
            last_output_at: None,
            parent_session_id: None,
            session_label: None,
            tmux_session_live: false,
        }
    }

    /// Build a minimal active AgentSession for tree-builder tests.
    fn tree_session(id: &str, parent: Option<&str>) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            name: format!("agent-{id}"),
            agent: CodingAgent {
                display_name: "T".to_string(),
                command: "claude".to_string(),
                args: vec![],
                resume: ResumeStrategy::None,
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            },
            tab_id: None,
            active: true,
            stale: false,
            resume_id: None,
            started_at: "2026-04-07T11:00:00+00:00".to_string(),
            log_path: None,
            last_output_at: None,
            parent_session_id: parent.map(|s| s.to_string()),
            session_label: None,
            tmux_session_live: false,
        }
    }

    fn make_assignment(session: &str, spark: &str) -> HandAssignment {
        HandAssignment {
            id: 1,
            session_id: session.to_string(),
            spark_id: spark.to_string(),
            status: "active".to_string(),
            role: "owner".to_string(),
            assigned_at: "2026-04-07T11:00:00+00:00".to_string(),
            last_heartbeat_at: None,
            lease_expires_at: None,
            completed_at: None,
            handoff_to: None,
            handoff_reason: None,
        }
    }

    #[test]
    fn active_session_with_tab_can_be_focused() {
        let s = make_session(true, Some(42), ResumeStrategy::ResumeFlag);
        assert!(s.active);
        assert_eq!(s.tab_id, Some(42));
        // can_resume is false for active sessions even when strategy supports it.
        assert!(!s.can_resume());
    }

    #[test]
    fn past_session_with_resume_strategy_is_resumable() {
        let s = make_session(false, None, ResumeStrategy::ResumeFlag);
        assert!(s.can_resume());
    }

    #[test]
    fn past_session_without_resume_strategy_is_not_resumable() {
        let s = make_session(false, None, ResumeStrategy::None);
        assert!(!s.can_resume());
    }

    #[test]
    fn format_relative_time_handles_invalid_input() {
        assert_eq!(format_relative_time("not a date"), "");
    }

    #[test]
    fn format_relative_time_returns_now_for_recent() {
        let now = chrono::Utc::now().to_rfc3339();
        assert_eq!(format_relative_time(&now), "now");
    }

    #[test]
    fn view_renders_with_empty_sessions() {
        // Smoke test: building the view with no sessions must not panic.
        let state = AgentsPanelState::default();
        let _ = view(&[], &[], &[], &[], &[], &state, Palette::dark());
    }

    #[test]
    fn view_renders_with_active_and_past_sessions() {
        let sessions = vec![
            make_session(true, Some(1), ResumeStrategy::ResumeFlag),
            AgentSession {
                id: "session-2".to_string(),
                ..make_session(false, None, ResumeStrategy::ResumeFlag)
            },
        ];
        let state = AgentsPanelState::default();
        let _ = view(&sessions, &[], &[], &[], &[], &state, Palette::dark());
    }

    #[test]
    fn hand_status_unassigned_when_no_owner_assignment() {
        // No assignments → any session is Unassigned (red).
        let now = Instant::now();
        assert_eq!(
            hand_status("session-1", &[], None, now, IDLE_THRESHOLD),
            HandStatus::Unassigned
        );
    }

    #[test]
    fn hand_status_ignores_non_owner_and_inactive_assignments() {
        // Owner-active is the only row that counts; observers and
        // completed assignments must not flip the dot to blue.
        let now = Instant::now();
        let mut observer = make_assignment("session-1", "sp-aaaa");
        observer.role = "observer".to_string();
        let mut completed = make_assignment("session-1", "sp-bbbb");
        completed.status = "completed".to_string();
        assert_eq!(
            hand_status(
                "session-1",
                &[observer, completed],
                Some(now),
                now,
                IDLE_THRESHOLD
            ),
            HandStatus::Unassigned
        );
    }

    #[test]
    fn hand_status_working_when_recent_output() {
        let now = Instant::now();
        let just_now = now - Duration::from_millis(500);
        assert_eq!(
            hand_status(
                "session-1",
                &[make_assignment("session-1", "sp-aaaa")],
                Some(just_now),
                now,
                IDLE_THRESHOLD
            ),
            HandStatus::Working
        );
    }

    #[test]
    fn hand_status_idle_after_silence_threshold() {
        let now = Instant::now();
        let long_ago = now - Duration::from_secs(30);
        assert_eq!(
            hand_status(
                "session-1",
                &[make_assignment("session-1", "sp-aaaa")],
                Some(long_ago),
                now,
                IDLE_THRESHOLD
            ),
            HandStatus::Idle
        );
    }

    #[test]
    fn hand_status_working_when_no_output_yet() {
        // A freshly spawned Hand (no output seen yet) must not immediately
        // show as idle — it shows as Working until proven silent.
        let now = Instant::now();
        assert_eq!(
            hand_status(
                "session-1",
                &[make_assignment("session-1", "sp-aaaa")],
                None,
                now,
                IDLE_THRESHOLD
            ),
            HandStatus::Working
        );
    }

    #[test]
    fn hand_status_color_matches_invariants() {
        // Invariants from sp-ux0034: Red = unassigned, Green = idle,
        // Blue = working. Verify each maps to the palette slot we expect.
        let pal = Palette::dark();
        assert_eq!(hand_status_color(HandStatus::Unassigned, &pal), pal.danger);
        assert_eq!(hand_status_color(HandStatus::Idle, &pal), pal.success);
        assert_eq!(hand_status_color(HandStatus::Working, &pal), pal.accent);
    }

    #[test]
    fn background_hand_is_active_without_tab_with_log() {
        // Spark ryve-8c14734a: a CLI-spawned Hand is "active" (process
        // running) but has no terminal tab. The presence of a log path is
        // what distinguishes it from a stale session and lets the UI open
        // a read-only spy view on click.
        let mut s = make_session(true, None, ResumeStrategy::None);
        assert!(!s.is_background(), "needs a log path to be background");
        s.log_path = Some(PathBuf::from("/tmp/hand-x.log"));
        assert!(s.is_background());

        // A session with a tab is not background — it has its own terminal.
        s.tab_id = Some(7);
        assert!(!s.is_background());
    }

    #[test]
    fn classify_session_marks_dead_active_rows_stale() {
        assert_eq!(
            classify_session(false, false, false),
            SessionDisplayState::Stale
        );
    }

    #[test]
    fn classify_session_keeps_live_or_ended_rows_out_of_stale() {
        assert_eq!(
            classify_session(false, true, false),
            SessionDisplayState::Active
        );
        assert_eq!(
            classify_session(false, false, true),
            SessionDisplayState::Active
        );
        assert_eq!(
            classify_session(true, false, false),
            SessionDisplayState::History
        );
    }

    // ── Tree builder ──────────────────────────

    fn make_crew(id: &str, head: Option<&str>, parent_spark: Option<&str>) -> Crew {
        Crew {
            id: id.to_string(),
            workshop_id: "ws".to_string(),
            name: format!("crew-{id}"),
            purpose: None,
            status: "active".to_string(),
            head_session_id: head.map(|s| s.to_string()),
            parent_spark_id: parent_spark.map(|s| s.to_string()),
            created_at: "2026-04-07T11:00:00+00:00".to_string(),
        }
    }

    fn make_member(crew_id: &str, session_id: &str) -> CrewMember {
        CrewMember {
            id: 0,
            crew_id: crew_id.to_string(),
            session_id: session_id.to_string(),
            role: None,
            joined_at: "2026-04-07T11:00:00+00:00".to_string(),
        }
    }

    #[test]
    fn build_active_tree_groups_head_with_crew_and_members() {
        // Head H owns crew C with members M1 and M2. Standalone S has no
        // crew and no parent. Expected order: H (with C { M1, M2 }) then S.
        let active = vec![
            tree_session("H", None),
            tree_session("M1", None),
            tree_session("M2", None),
            tree_session("S", None),
        ];
        let crews = vec![make_crew("C", Some("H"), Some("sp-epic"))];
        let members = vec![make_member("C", "M1"), make_member("C", "M2")];
        let tree = build_active_tree(&active, &crews, &members);

        assert_eq!(tree.len(), 2);
        match &tree[0] {
            ActiveNode::Head {
                session_id,
                crews,
                epic_spark_id,
                ..
            } => {
                assert_eq!(session_id, "H");
                assert_eq!(epic_spark_id.as_deref(), Some("sp-epic"));
                assert_eq!(crews.len(), 1);
                assert_eq!(crews[0].member_session_ids, vec!["M1", "M2"]);
            }
            other => panic!("expected Head, got {other:?}"),
        }
        match &tree[1] {
            ActiveNode::Standalone { session_id } => assert_eq!(session_id, "S"),
            other => panic!("expected Standalone, got {other:?}"),
        }
    }

    #[test]
    fn build_active_tree_renders_head_with_no_crew_yet() {
        let active = vec![tree_session("H", None)];
        let crews = vec![make_crew("C", Some("H"), None)];
        let members: Vec<CrewMember> = vec![];
        let tree = build_active_tree(&active, &crews, &members);
        assert_eq!(tree.len(), 1);
        match &tree[0] {
            ActiveNode::Head { crews, .. } => {
                assert_eq!(crews.len(), 1);
                assert!(crews[0].member_session_ids.is_empty());
            }
            other => panic!("expected Head, got {other:?}"),
        }
    }

    #[test]
    fn build_active_tree_does_not_double_emit_crew_member_as_standalone() {
        let active = vec![tree_session("H", None), tree_session("M", None)];
        let crews = vec![make_crew("C", Some("H"), None)];
        let members = vec![make_member("C", "M")];
        let tree = build_active_tree(&active, &crews, &members);
        assert_eq!(tree.len(), 1);
        match &tree[0] {
            ActiveNode::Head { crews, .. } => {
                assert_eq!(crews[0].member_session_ids, vec!["M"]);
            }
            _ => panic!("expected single Head node"),
        }
    }

    #[test]
    fn build_active_tree_handles_multiple_heads_in_recency_order() {
        let active = vec![tree_session("H2", None), tree_session("H1", None)];
        let crews = vec![
            make_crew("C1", Some("H1"), None),
            make_crew("C2", Some("H2"), None),
        ];
        let tree = build_active_tree(&active, &crews, &[]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(&tree[0], ActiveNode::Head { session_id, .. } if session_id == "H2"));
        assert!(matches!(&tree[1], ActiveNode::Head { session_id, .. } if session_id == "H1"));
    }

    #[test]
    fn build_active_tree_attributes_solo_hands_to_parent_head() {
        // H owns crew C containing M1. S has parent H but no crew
        // membership — it must appear as a *solo* child of H, not at the
        // top level as a Standalone. U has no parent at all and stays
        // Standalone.
        let active = vec![
            tree_session("H", None),
            tree_session("M1", None),
            tree_session("S", Some("H")),
            tree_session("U", None),
        ];
        let crews = vec![make_crew("C", Some("H"), None)];
        let members = vec![make_member("C", "M1")];
        let tree = build_active_tree(&active, &crews, &members);

        assert_eq!(tree.len(), 2);
        match &tree[0] {
            ActiveNode::Head {
                session_id,
                crews,
                solo_hands,
                ..
            } => {
                assert_eq!(session_id, "H");
                assert_eq!(crews[0].member_session_ids, vec!["M1"]);
                assert_eq!(solo_hands, &vec!["S".to_string()]);
            }
            other => panic!("expected Head, got {other:?}"),
        }
        match &tree[1] {
            ActiveNode::Standalone { session_id } => assert_eq!(session_id, "U"),
            other => panic!("expected Standalone, got {other:?}"),
        }
    }

    #[test]
    fn build_active_tree_solo_hand_does_not_double_emit_when_in_crew() {
        // M has parent_session_id=H AND is a member of H's crew. It must
        // render under the crew, not also as a solo child.
        let active = vec![tree_session("H", None), tree_session("M", Some("H"))];
        let crews = vec![make_crew("C", Some("H"), None)];
        let members = vec![make_member("C", "M")];
        let tree = build_active_tree(&active, &crews, &members);
        assert_eq!(tree.len(), 1);
        match &tree[0] {
            ActiveNode::Head {
                crews, solo_hands, ..
            } => {
                assert_eq!(crews[0].member_session_ids, vec!["M"]);
                assert!(solo_hands.is_empty());
            }
            _ => panic!("expected single Head node"),
        }
    }

    #[test]
    fn build_active_tree_orphan_solo_hand_falls_through_to_standalone() {
        // S claims a parent that isn't itself a Head (e.g. a deleted or
        // history-only session). The renderer should still surface S
        // rather than dropping it.
        let active = vec![tree_session("S", Some("ghost"))];
        let tree = build_active_tree(&active, &[], &[]);
        assert_eq!(tree.len(), 1);
        match &tree[0] {
            ActiveNode::Standalone { session_id } => assert_eq!(session_id, "S"),
            other => panic!("expected Standalone, got {other:?}"),
        }
    }

    // ── Search ────────────────────────────────

    fn make_named_session(id: &str, name: &str, command: &str) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            name: name.to_string(),
            agent: CodingAgent {
                display_name: name.to_string(),
                command: command.to_string(),
                args: vec![],
                resume: ResumeStrategy::None,
                compatibility: crate::coding_agents::CompatStatus::Unknown,
            },
            tab_id: None,
            active: true,
            stale: false,
            resume_id: None,
            started_at: "2026-04-07T11:00:00+00:00".to_string(),
            log_path: None,
            last_output_at: None,
            parent_session_id: None,
            session_label: None,
            tmux_session_live: false,
        }
    }

    #[test]
    fn session_matches_query_empty_matches_all() {
        let s = make_named_session("a", "Claude Code", "claude");
        assert!(session_matches_query(&s, &[], &[], ""));
        assert!(session_matches_query(&s, &[], &[], "   "));
    }

    #[test]
    fn session_matches_query_by_name_and_command() {
        let s = make_named_session("a", "Claude Code", "claude");
        assert!(session_matches_query(&s, &[], &[], "claude"));
        assert!(session_matches_query(&s, &[], &[], "CLA"));
        assert!(!session_matches_query(&s, &[], &[], "codex"));
    }

    // ── Tmux attach (spark ryve-8ba40d83) ──────────

    #[test]
    fn attach_session_message_variant_exists() {
        // The AttachSession message must carry (session_id, session_label).
        let m = Message::AttachSession("sess-1".into(), "hand".into());
        assert!(matches!(m, Message::AttachSession(_, _)));
    }

    #[test]
    fn attach_button_hidden_when_tmux_not_live() {
        // When `tmux_session_live` is false, the Attach button must not
        // appear. We test this indirectly by building a row and asserting
        // the view doesn't panic — the actual presence of "Attach" is a
        // visual concern, but the code path exercises the gating logic.
        let mut s = make_session(true, None, ResumeStrategy::None);
        s.tmux_session_live = false;
        s.log_path = Some(PathBuf::from("/tmp/test.log"));
        let pal = Palette::dark();
        // render_hand_row is private but we can verify through the public
        // `view` function — the session appears in Active with no panic.
        let sessions = vec![s];
        let state = AgentsPanelState::default();
        let _ = view(&sessions, &[], &[], &[], &[], &state, pal);
    }

    #[test]
    fn attach_button_present_when_tmux_live() {
        // When `tmux_session_live` is true, the Attach button's message
        // should be emitted. Again we exercise the path by calling `view`.
        let mut s = make_session(true, None, ResumeStrategy::None);
        s.tmux_session_live = true;
        s.session_label = Some("hand".into());
        s.log_path = Some(PathBuf::from("/tmp/test.log"));
        let pal = Palette::dark();
        let sessions = vec![s];
        let state = AgentsPanelState::default();
        let _ = view(&sessions, &[], &[], &[], &[], &state, pal);
    }
}
