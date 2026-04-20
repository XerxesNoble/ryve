// SPDX-License-Identifier: AGPL-3.0-or-later

//! IRC projection view — scrollable, real-time projected message list.
//!
//! Renders the output of [`ipc::channel_projection::query`] as a live,
//! user-filterable surface. The five canonical projection axes (Epic,
//! Spark, Assignment, PR, Actor) are exposed as individual text inputs
//! that combine with AND semantics; an FTS search box layers a full-text
//! filter on top. A sidebar lists the saved [`ProjectionPreset`]s for
//! the current `(workshop, channel)` so the user can one-click between
//! "my work" / "review queue" / "merge status" etc. Each preset shows
//! an unread badge computed via
//! [`ipc::channel_projection::preset_unread_count`].
//!
//! ## Liveness model
//!
//! The view has **no timer of its own**. Fresh data arrives through the
//! three loader entry points below, which the caller invokes from the
//! existing app-level subscription ticks (`SparksPoll`), from the IPC
//! callbacks that drive `irc_messages` inserts, and at the explicit
//! refresh points (tab open, filter change, preset activation). When a
//! reload delivers new messages while the user is scrolled away from the
//! tail, the "new messages below" indicator is bumped so the user
//! notices without the list jumping under them.
//!
//! ## Empty state
//!
//! A load that returns [`ProjectionOutput::Empty`] or an empty vector is
//! rendered as an explicit empty-state component — never a blank pane.
//!
//! ## Navigation
//!
//! A fresh tab is created via
//! [`crate::workshop::Workshop::open_irc_view_tab`]; the dropdown entry
//! "Open IRC View" in the bench opens (or refocuses) a tab for the
//! first known epic channel. Keeping the channel identity on the tab
//! lets multiple IRC views coexist on different epics.

use std::collections::HashMap;

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Theme};
use ipc::channel_projection::{
    ChannelProjectionQuery, DEFAULT_LIMIT, ProjectedMessage, ProjectionError, ProjectionOutput,
    ProjectionPreset,
};
use sqlx::SqlitePool;

use crate::style::{self, FONT_BODY, FONT_HEADER, FONT_LABEL, FONT_SMALL, Palette};

// ── Messages ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    /// A filter input changed; the caller should re-query on the next tick.
    EpicFilterChanged(String),
    SparkFilterChanged(String),
    AssignmentFilterChanged(String),
    PrFilterChanged(String),
    ActorFilterChanged(String),
    /// FTS search query changed.
    FtsQueryChanged(String),
    /// Clear all axis and FTS filters.
    ClearFilters,
    /// Apply the named preset's filters into the form state.
    ActivatePreset(i64),
    /// Scrolled inside the message list. Tracks whether the user is
    /// pinned to the tail so the new-messages indicator only appears
    /// when it matters.
    Scrolled {
        tab_id: u64,
        offset_y: f32,
        viewport_height: f32,
        content_height: f32,
    },
    /// "Jump to latest" button pressed — the next render should stick
    /// to the tail and the new-messages banner must clear.
    JumpToTail(u64),
    /// A [`load_messages`] task completed.
    MessagesLoaded {
        tab_id: u64,
        messages: Vec<ProjectedMessage>,
        max_id: i64,
    },
    /// A [`load_messages`] task failed. Surfaced inline so the operator
    /// can see the reason without leaving the panel.
    MessagesLoadFailed {
        tab_id: u64,
        error: String,
    },
    /// A [`load_presets`] task completed.
    PresetsLoaded {
        tab_id: u64,
        presets: Vec<ProjectionPreset>,
    },
    /// A [`load_unread_counts`] task completed. `counts` is `(preset_id, unread)`.
    UnreadCountsLoaded {
        tab_id: u64,
        counts: Vec<(i64, i64)>,
    },
}

// ── State ────────────────────────────────────────────

/// Per-tab state for one IRC projection view. Owned by the workshop
/// exactly like [`crate::screen::log_tail::LogTailState`] so the view
/// function can remain pure and the render frame never touches the DB.
#[derive(Debug, Clone)]
pub struct IrcViewState {
    /// Stable tab id used by the bench to route messages back here.
    pub tab_id: u64,
    /// Canonical IRC channel name this view is pinned to.
    pub channel: String,
    /// Workshop id used as the scope for saved presets.
    pub workshop_id: String,
    /// Current session's actor id. Surfaced on the mention-override
    /// axis (a message addressed to this actor breaks through every
    /// filter, per the epic's hard invariant).
    pub current_actor_id: Option<String>,

    // ── Filter form inputs ────────────────────────────
    /// Optional epic_id filter. Kept as a free-form string because most
    /// real values are `sp-xxxx` and users will paste them in; an empty
    /// input means "no axis restriction".
    pub epic_id_input: String,
    pub spark_id_input: String,
    pub assignment_id_input: String,
    /// Raw PR-number text. Parsed at query time; an invalid value
    /// (non-numeric) is treated as "no filter" rather than an error so
    /// the panel stays usable while the user is typing.
    pub pr_number_input: String,
    pub actor_id_input: String,
    pub fts_query_input: String,

    // ── Loaded data ───────────────────────────────────
    pub messages: Vec<ProjectedMessage>,
    pub presets: Vec<ProjectionPreset>,
    pub preset_unread_counts: HashMap<i64, i64>,
    pub active_preset_id: Option<i64>,
    /// Human-readable error from the last failed load, if any. Shown
    /// inline above the message list so the operator can see why no
    /// rows came back.
    pub last_error: Option<String>,

    // ── Scroll tracking ───────────────────────────────
    pub scroll_offset_y: f32,
    pub viewport_height: f32,
    pub content_height: f32,
    /// Highest message id observed in a successful load. Used to drive
    /// the "new messages below" banner when later loads deliver bigger ids.
    pub last_max_message_id: i64,
    /// Count of messages that arrived while the user was scrolled away
    /// from the tail. Cleared when the user taps the banner or scrolls
    /// back down.
    pub new_messages_banner_count: usize,
}

/// Threshold in logical pixels below which the view is considered "at
/// the tail" — i.e. the bottom of the content is within this many
/// pixels of the viewport bottom. Matches the empirical scrollbar
/// snap-zone in iced's `scrollable` widget.
pub const TAIL_EPSILON_PX: f32 = 24.0;

impl IrcViewState {
    /// Build a fresh state for `(workshop_id, channel)`. No data is
    /// loaded here — the caller dispatches the refresh batch.
    pub fn new(
        tab_id: u64,
        workshop_id: String,
        channel: String,
        current_actor_id: Option<String>,
    ) -> Self {
        Self {
            tab_id,
            channel,
            workshop_id,
            current_actor_id,
            epic_id_input: String::new(),
            spark_id_input: String::new(),
            assignment_id_input: String::new(),
            pr_number_input: String::new(),
            actor_id_input: String::new(),
            fts_query_input: String::new(),
            messages: Vec::new(),
            presets: Vec::new(),
            preset_unread_counts: HashMap::new(),
            active_preset_id: None,
            last_error: None,
            scroll_offset_y: 0.0,
            viewport_height: 600.0,
            content_height: 0.0,
            last_max_message_id: 0,
            new_messages_banner_count: 0,
        }
    }

    /// Synthesize a [`ChannelProjectionQuery`] from the current form
    /// state. Blank inputs map to `None`; a garbled `pr_number_input`
    /// (non-numeric) also maps to `None` — we don't want typos to lock
    /// the panel into an empty state.
    pub fn current_query(&self) -> ChannelProjectionQuery {
        ChannelProjectionQuery {
            epic_id: non_empty(&self.epic_id_input),
            channel: Some(self.channel.clone()),
            spark_id: non_empty(&self.spark_id_input),
            assignment_id: non_empty(&self.assignment_id_input),
            pr_number: self.pr_number_input.trim().parse::<u64>().ok(),
            actor_id: non_empty(&self.actor_id_input),
            fts_query: non_empty(&self.fts_query_input),
            current_actor_id: self.current_actor_id.clone(),
            limit: DEFAULT_LIMIT,
        }
    }

    /// Whether any filter axis or FTS string is currently active. Drives
    /// the "Clear filters" chip and the empty-state wording.
    pub fn any_filter_active(&self) -> bool {
        !self.epic_id_input.trim().is_empty()
            || !self.spark_id_input.trim().is_empty()
            || !self.assignment_id_input.trim().is_empty()
            || !self.pr_number_input.trim().is_empty()
            || !self.actor_id_input.trim().is_empty()
            || !self.fts_query_input.trim().is_empty()
    }

    /// Returns `true` when the scrollable is pinned at (or within
    /// [`TAIL_EPSILON_PX`] of) the bottom. Used to decide whether
    /// incoming messages should scroll the view or bump the banner.
    pub fn is_at_tail(&self) -> bool {
        if self.content_height <= self.viewport_height {
            return true;
        }
        let max_offset = (self.content_height - self.viewport_height).max(0.0);
        (max_offset - self.scroll_offset_y).abs() <= TAIL_EPSILON_PX
    }

    /// Record a scroll event from the iced viewport. Clears the new-
    /// messages banner when the user scrolls back to the tail.
    pub fn apply_scroll(&mut self, offset_y: f32, viewport_height: f32, content_height: f32) {
        self.scroll_offset_y = offset_y;
        self.viewport_height = viewport_height;
        self.content_height = content_height;
        if self.is_at_tail() {
            self.new_messages_banner_count = 0;
        }
    }

    /// Record a completed load. When new rows arrive and the user is
    /// scrolled away from the tail, the banner's counter is bumped by
    /// the number of rows beyond [`last_max_message_id`]; otherwise the
    /// banner stays cleared.
    pub fn apply_messages(&mut self, messages: Vec<ProjectedMessage>, max_id: i64) {
        let previous_max = self.last_max_message_id;
        let arrived = if max_id > previous_max {
            messages.iter().filter(|m| m.id > previous_max).count()
        } else {
            0
        };
        self.messages = messages;
        self.last_error = None;
        if max_id > self.last_max_message_id {
            self.last_max_message_id = max_id;
        }
        // On first load (`previous_max == 0`) we seed the banner from
        // zero — which would otherwise look like "every message is new"
        // even though the user just opened the panel. Skip that case.
        if previous_max > 0 && arrived > 0 && !self.is_at_tail() {
            self.new_messages_banner_count = self.new_messages_banner_count.saturating_add(arrived);
        } else if self.is_at_tail() {
            self.new_messages_banner_count = 0;
        }
    }

    pub fn apply_presets(&mut self, presets: Vec<ProjectionPreset>) {
        self.presets = presets;
        // Drop unread entries for presets that no longer exist.
        self.preset_unread_counts
            .retain(|id, _| self.presets.iter().any(|p| p.id == *id));
    }

    pub fn apply_unread(&mut self, counts: Vec<(i64, i64)>) {
        for (id, unread) in counts {
            self.preset_unread_counts.insert(id, unread);
        }
    }

    /// Load the preset's filters into the form, mark it active, and
    /// clear the new-messages banner (the caller re-queries straight
    /// after, so the list and tail state reset together).
    pub fn activate_preset(&mut self, id: i64) {
        if let Some(preset) = self.presets.iter().find(|p| p.id == id) {
            self.epic_id_input = preset.filters.epic_id.clone().unwrap_or_default();
            self.spark_id_input = preset.filters.spark_id.clone().unwrap_or_default();
            self.assignment_id_input = preset.filters.assignment_id.clone().unwrap_or_default();
            self.pr_number_input = preset
                .filters
                .pr_number
                .map(|n| n.to_string())
                .unwrap_or_default();
            self.actor_id_input = preset.filters.actor_id.clone().unwrap_or_default();
            self.fts_query_input = preset.filters.fts_query.clone().unwrap_or_default();
            self.active_preset_id = Some(id);
            self.new_messages_banner_count = 0;
        }
    }

    /// Reset every filter input and clear the active preset pin.
    pub fn clear_filters(&mut self) {
        self.epic_id_input.clear();
        self.spark_id_input.clear();
        self.assignment_id_input.clear();
        self.pr_number_input.clear();
        self.actor_id_input.clear();
        self.fts_query_input.clear();
        self.active_preset_id = None;
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

// ── Update ───────────────────────────────────────────

/// Pure message handler. Mutates `state` and returns `true` if the
/// caller should dispatch a fresh message-reload (filters changed).
/// Returning a bool keeps this module free of `iced::Task` so the
/// workshop-owned state can be updated without dragging the async
/// runtime into unit tests.
pub fn update(state: &mut IrcViewState, msg: Message) -> bool {
    match msg {
        Message::EpicFilterChanged(v) => {
            state.epic_id_input = v;
            state.active_preset_id = None;
            true
        }
        Message::SparkFilterChanged(v) => {
            state.spark_id_input = v;
            state.active_preset_id = None;
            true
        }
        Message::AssignmentFilterChanged(v) => {
            state.assignment_id_input = v;
            state.active_preset_id = None;
            true
        }
        Message::PrFilterChanged(v) => {
            state.pr_number_input = v;
            state.active_preset_id = None;
            true
        }
        Message::ActorFilterChanged(v) => {
            state.actor_id_input = v;
            state.active_preset_id = None;
            true
        }
        Message::FtsQueryChanged(v) => {
            state.fts_query_input = v;
            state.active_preset_id = None;
            true
        }
        Message::ClearFilters => {
            state.clear_filters();
            true
        }
        Message::ActivatePreset(id) => {
            state.activate_preset(id);
            true
        }
        Message::Scrolled {
            offset_y,
            viewport_height,
            content_height,
            ..
        } => {
            state.apply_scroll(offset_y, viewport_height, content_height);
            false
        }
        Message::JumpToTail(_) => {
            let max = (state.content_height - state.viewport_height).max(0.0);
            state.scroll_offset_y = max;
            state.new_messages_banner_count = 0;
            false
        }
        Message::MessagesLoaded {
            messages, max_id, ..
        } => {
            state.apply_messages(messages, max_id);
            false
        }
        Message::MessagesLoadFailed { error, .. } => {
            state.last_error = Some(error);
            false
        }
        Message::PresetsLoaded { presets, .. } => {
            state.apply_presets(presets);
            false
        }
        Message::UnreadCountsLoaded { counts, .. } => {
            state.apply_unread(counts);
            false
        }
    }
}

// ── Loader tasks ─────────────────────────────────────

/// Run the projection query and map the outcome to a [`Message`] so
/// the caller can drive it via `Task::perform`.
pub async fn load_messages(
    pool: SqlitePool,
    tab_id: u64,
    query: ChannelProjectionQuery,
) -> Message {
    match ipc::channel_projection::query(&pool, &query).await {
        Ok(output) => {
            let messages = match output {
                ProjectionOutput::Empty => Vec::new(),
                ProjectionOutput::Messages(m) => m,
            };
            let max_id = messages.iter().map(|m| m.id).max().unwrap_or(0);
            Message::MessagesLoaded {
                tab_id,
                messages,
                max_id,
            }
        }
        Err(e) => Message::MessagesLoadFailed {
            tab_id,
            error: format_projection_error(&e),
        },
    }
}

/// Load all presets for `(workshop_id, channel)`. On failure an empty
/// list is returned so the sidebar renders as "no presets yet" rather
/// than blocking the panel entirely — callers see the underlying
/// database error in the workshop log.
pub async fn load_presets(
    pool: SqlitePool,
    tab_id: u64,
    workshop_id: String,
    channel: String,
) -> Message {
    match ipc::channel_projection::list_presets(&pool, &workshop_id, &channel).await {
        Ok(presets) => Message::PresetsLoaded { tab_id, presets },
        Err(e) => {
            log::warn!(
                "irc_view: list_presets failed for workshop={workshop_id} channel={channel}: {}",
                format_projection_error(&e),
            );
            Message::PresetsLoaded {
                tab_id,
                presets: Vec::new(),
            }
        }
    }
}

/// Query the unread count for every preset id in `ids`, skipping those
/// that error so a single bad preset does not blank the whole sidebar.
pub async fn load_unread_counts(pool: SqlitePool, tab_id: u64, ids: Vec<i64>) -> Message {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        match ipc::channel_projection::preset_unread_count(&pool, id).await {
            Ok(n) => out.push((id, n)),
            Err(e) => {
                log::warn!(
                    "irc_view: preset_unread_count({id}) failed: {}",
                    format_projection_error(&e),
                );
            }
        }
    }
    Message::UnreadCountsLoaded {
        tab_id,
        counts: out,
    }
}

fn format_projection_error(e: &ProjectionError) -> String {
    e.to_string()
}

// ── View ─────────────────────────────────────────────

/// Render the IRC projection screen. Layout:
///
/// ```text
/// ┌───────────────┬────────────────────────────────────────┐
/// │ Presets       │ Header (channel + filter chips)        │
/// │ ─────────     │ ──────────────────────────────────────  │
/// │ · My work   3 │ Filter inputs row (Epic / Spark / …)    │
/// │ · Review    0 │ FTS input row                           │
/// │ · Merges    1 │ (banner: N new messages below)          │
/// │               │ Scrollable message list                 │
/// └───────────────┴────────────────────────────────────────┘
/// ```
pub fn view<'a>(state: &'a IrcViewState, pal: &Palette, has_bg: bool) -> Element<'a, Message> {
    let pal = *pal;

    let sidebar = preset_sidebar(state, &pal);
    let body = main_body(state, &pal);

    let layout = row![sidebar, body]
        .spacing(12)
        .width(Length::Fill)
        .height(Length::Fill);

    container(layout)
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_: &Theme| style::glass_panel(&pal, has_bg))
        .into()
}

fn preset_sidebar<'a>(state: &'a IrcViewState, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let header = row![
        text("Presets").size(FONT_HEADER).color(pal.text_primary),
        Space::new().width(Length::Fill),
        text(format!("{}", state.presets.len()))
            .size(FONT_SMALL)
            .color(pal.text_tertiary),
    ]
    .align_y(iced::Alignment::Center);

    let mut list = column![header].spacing(6).padding([4, 4]);

    if state.presets.is_empty() {
        list = list.push(
            text("No saved presets for this channel.")
                .size(FONT_SMALL)
                .color(pal.text_tertiary),
        );
    } else {
        for preset in &state.presets {
            let unread = state
                .preset_unread_counts
                .get(&preset.id)
                .copied()
                .unwrap_or(0);
            let is_active = state.active_preset_id == Some(preset.id);
            let name_color = if is_active {
                pal.accent
            } else {
                pal.text_primary
            };
            let name = text(preset.name.clone())
                .size(FONT_BODY)
                .color(name_color)
                .width(Length::FillPortion(3));
            // Unread badge (hidden when zero to keep the sidebar calm).
            let badge_side: Element<'a, Message> = if unread > 0 {
                container(
                    text(if unread > 99 {
                        "99+".to_string()
                    } else {
                        unread.to_string()
                    })
                    .size(FONT_SMALL)
                    .color(pal.text_primary),
                )
                .padding([1, 6])
                .style(move |_: &Theme| unread_badge_style(&pal))
                .into()
            } else {
                Space::new().width(0).height(0).into()
            };
            let row_content = row![name, Space::new().width(Length::Fill), badge_side]
                .spacing(6)
                .align_y(iced::Alignment::Center);
            list = list.push(
                button(row_content)
                    .style(button::text)
                    .padding([4, 8])
                    .width(Length::Fill)
                    .on_press(Message::ActivatePreset(preset.id)),
            );
        }
    }

    container(scrollable(list).height(Length::Fill))
        .width(220)
        .height(Length::Fill)
        .into()
}

fn main_body<'a>(state: &'a IrcViewState, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;

    let title = text(format!("IRC — {}", state.channel))
        .size(FONT_HEADER)
        .color(pal.text_primary);
    let active_chip: Element<'a, Message> = if let Some(id) = state.active_preset_id {
        let name = state
            .presets
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("preset {id}"));
        text(format!("Preset: {name}"))
            .size(FONT_SMALL)
            .color(pal.accent)
            .into()
    } else {
        text("No preset active")
            .size(FONT_SMALL)
            .color(pal.text_tertiary)
            .into()
    };

    let clear_btn: Element<'a, Message> = if state.any_filter_active() {
        button(
            text("Clear filters")
                .size(FONT_LABEL)
                .color(pal.text_secondary),
        )
        .style(button::text)
        .padding([2, 8])
        .on_press(Message::ClearFilters)
        .into()
    } else {
        Space::new().width(0).height(0).into()
    };

    let header = row![
        title,
        Space::new().width(Length::Fill),
        active_chip,
        clear_btn,
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);

    let filter_row = row![
        filter_chip("Epic", &state.epic_id_input, Message::EpicFilterChanged),
        filter_chip("Spark", &state.spark_id_input, Message::SparkFilterChanged),
        filter_chip(
            "Assignment",
            &state.assignment_id_input,
            Message::AssignmentFilterChanged,
        ),
        filter_chip("PR #", &state.pr_number_input, Message::PrFilterChanged),
        filter_chip("Actor", &state.actor_id_input, Message::ActorFilterChanged),
    ]
    .spacing(6);

    let search_row = text_input("Full-text search…", &state.fts_query_input)
        .on_input(Message::FtsQueryChanged)
        .padding(6)
        .size(FONT_BODY);

    let banner = new_messages_banner(state, &pal);
    let message_panel = message_list(state, &pal);

    column![header, filter_row, search_row, banner, message_panel,]
        .spacing(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn filter_chip<'a, F>(label: &'a str, value: &'a str, on_change: F) -> Element<'a, Message>
where
    F: 'static + Fn(String) -> Message,
{
    column![
        text(label).size(FONT_SMALL),
        text_input("", value)
            .on_input(on_change)
            .size(FONT_LABEL)
            .padding([3, 6])
            .width(Length::Fill),
    ]
    .spacing(2)
    .width(Length::Fill)
    .into()
}

fn new_messages_banner<'a>(state: &'a IrcViewState, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    if state.new_messages_banner_count == 0 {
        return Space::new().width(0).height(0).into();
    }
    let label = if state.new_messages_banner_count == 1 {
        "1 new message below — jump to latest".to_string()
    } else {
        format!(
            "{} new messages below — jump to latest",
            state.new_messages_banner_count
        )
    };
    let tab_id = state.tab_id;
    container(
        button(text(label).size(FONT_SMALL).color(pal.text_primary))
            .style(button::text)
            .padding([3, 10])
            .on_press(Message::JumpToTail(tab_id)),
    )
    .padding(0)
    .style(move |_: &Theme| banner_style(&pal))
    .into()
}

fn message_list<'a>(state: &'a IrcViewState, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;

    if let Some(err) = &state.last_error {
        return container(
            text(format!("Projection failed: {err}"))
                .size(FONT_BODY)
                .color(pal.danger),
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    if state.messages.is_empty() {
        return empty_state(state, &pal);
    }

    let mut list = column![].spacing(4);
    for msg in &state.messages {
        list = list.push(message_row(msg, &pal));
    }

    let tab_id = state.tab_id;
    scrollable(list)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_scroll(move |viewport| {
            let offset = viewport.absolute_offset();
            let bounds = viewport.bounds();
            let content = viewport.content_bounds();
            Message::Scrolled {
                tab_id,
                offset_y: offset.y,
                viewport_height: bounds.height,
                content_height: content.height,
            }
        })
        .into()
}

fn message_row<'a>(msg: &'a ProjectedMessage, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let sender = msg
        .sender_actor_id
        .clone()
        .unwrap_or_else(|| "system".to_string());
    let ts = format_timestamp(&msg.timestamp);
    let badge = msg.event_type.clone().unwrap_or_else(|| "chat".to_string());
    let body_color = if msg.matched_by_mention {
        pal.accent
    } else {
        pal.text_primary
    };

    let mention_mark: Element<'a, Message> = if msg.matched_by_mention {
        text("@").size(FONT_SMALL).color(pal.accent).into()
    } else {
        Space::new().width(0).height(0).into()
    };

    let header = row![
        text(sender).size(FONT_LABEL).color(pal.text_secondary),
        text(ts).size(FONT_SMALL).color(pal.text_tertiary),
        container(text(badge).size(FONT_SMALL).color(pal.text_primary))
            .padding([1, 6])
            .style(move |_: &Theme| badge_style(&pal)),
        mention_mark,
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let body = text(msg.raw_text.clone()).size(FONT_BODY).color(body_color);

    column![header, body]
        .spacing(2)
        .padding([4, 8])
        .width(Length::Fill)
        .into()
}

fn empty_state<'a>(state: &'a IrcViewState, pal: &Palette) -> Element<'a, Message> {
    let pal = *pal;
    let heading = text("No messages match this projection.")
        .size(FONT_BODY)
        .color(pal.text_primary);
    let hint_text = if state.any_filter_active() {
        "Clear the filters or pick a different preset to see more of the channel."
    } else {
        "Nothing has been posted on this channel yet. New messages will appear here live."
    };
    let hint = text(hint_text).size(FONT_SMALL).color(pal.text_secondary);

    let action: Element<'a, Message> = if state.any_filter_active() {
        button(
            text("Clear filters")
                .size(FONT_LABEL)
                .color(pal.text_primary),
        )
        .style(button::secondary)
        .padding([4, 12])
        .on_press(Message::ClearFilters)
        .into()
    } else {
        Space::new().width(0).height(0).into()
    };

    container(
        column![heading, hint, action]
            .spacing(10)
            .align_x(iced::Alignment::Center),
    )
    .center(Length::Fill)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn format_timestamp(raw: &str) -> String {
    // DB timestamps are RFC3339 / ISO-8601; split on 'T' to produce a
    // tight "HH:MM:SS" for the row header. Anything unexpected falls
    // through as-is.
    let Some((_, rest)) = raw.split_once('T') else {
        return raw.to_string();
    };
    let trimmed = rest.split(['.', '+', 'Z']).next().unwrap_or(rest);
    trimmed.to_string()
}

// ── Styling helpers ──────────────────────────────────

fn unread_badge_style(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(pal.accent)),
        border: iced::Border {
            color: pal.accent,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

fn banner_style(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(pal.surface_active)),
        border: iced::Border {
            color: pal.accent,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

fn badge_style(pal: &Palette) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(pal.surface_active)),
        border: iced::Border {
            color: pal.border,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    }
}

// ── Tests ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ipc::channel_projection::{PresetFilters, ProjectionPreset, StructuredMetadata};

    use super::*;

    fn fresh() -> IrcViewState {
        IrcViewState::new(
            42,
            "ws-test".to_string(),
            "#epic-x".to_string(),
            Some("alice".to_string()),
        )
    }

    fn sample_message(id: i64, body: &str) -> ProjectedMessage {
        ProjectedMessage {
            id,
            epic_id: "e-1".into(),
            channel: "#epic-x".into(),
            sender_actor_id: Some("bob".into()),
            timestamp: "2026-04-20T12:34:56Z".into(),
            event_type: Some("assignment.created".into()),
            raw_text: body.into(),
            structured_event_id: Some("evt".into()),
            metadata: Some(StructuredMetadata {
                assignment_id: Some("asgn-1".into()),
                actor_id: Some("bob".into()),
                spark_id: Some("sp-1".into()),
                pr_number: Some(7),
                payload: serde_json::json!({}),
            }),
            matched_by_mention: false,
        }
    }

    fn sample_preset(id: i64, name: &str, epic: Option<&str>) -> ProjectionPreset {
        ProjectionPreset {
            id,
            workshop_id: "ws-test".into(),
            channel: "#epic-x".into(),
            name: name.into(),
            filters: PresetFilters {
                epic_id: epic.map(str::to_string),
                ..PresetFilters::default()
            },
            last_seen_message_id: 0,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn new_state_starts_empty_with_session_actor() {
        let s = fresh();
        assert_eq!(s.tab_id, 42);
        assert_eq!(s.channel, "#epic-x");
        assert_eq!(s.workshop_id, "ws-test");
        assert_eq!(s.current_actor_id.as_deref(), Some("alice"));
        assert!(s.messages.is_empty());
        assert!(s.presets.is_empty());
        assert!(!s.any_filter_active());
        assert_eq!(s.new_messages_banner_count, 0);
    }

    #[test]
    fn current_query_threads_form_inputs_into_axes() {
        let mut s = fresh();
        s.epic_id_input = "sp-epic-1".into();
        s.spark_id_input = "sp-child-2".into();
        s.assignment_id_input = "asgn-9".into();
        s.pr_number_input = "17".into();
        s.actor_id_input = "carol".into();
        s.fts_query_input = "approved".into();
        let q = s.current_query();
        assert_eq!(q.epic_id.as_deref(), Some("sp-epic-1"));
        assert_eq!(q.channel.as_deref(), Some("#epic-x"));
        assert_eq!(q.spark_id.as_deref(), Some("sp-child-2"));
        assert_eq!(q.assignment_id.as_deref(), Some("asgn-9"));
        assert_eq!(q.pr_number, Some(17));
        assert_eq!(q.actor_id.as_deref(), Some("carol"));
        assert_eq!(q.fts_query.as_deref(), Some("approved"));
        // Session state (mentions override + limit) always propagates.
        assert_eq!(q.current_actor_id.as_deref(), Some("alice"));
        assert_eq!(q.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn current_query_treats_blank_or_garbled_pr_as_no_filter() {
        let mut s = fresh();
        s.pr_number_input = "not a number".into();
        assert_eq!(s.current_query().pr_number, None);
        s.pr_number_input = "   ".into();
        assert_eq!(s.current_query().pr_number, None);
    }

    #[test]
    fn any_filter_active_reflects_every_axis_and_fts() {
        let mut s = fresh();
        assert!(!s.any_filter_active());
        s.fts_query_input = "hello".into();
        assert!(s.any_filter_active());
        s.fts_query_input.clear();
        s.pr_number_input = "5".into();
        assert!(s.any_filter_active());
    }

    #[test]
    fn update_reports_reload_on_filter_changes_not_on_scroll() {
        let mut s = fresh();
        assert!(update(&mut s, Message::EpicFilterChanged("x".into()),));
        assert!(update(&mut s, Message::FtsQueryChanged("y".into()),));
        assert!(update(&mut s, Message::ClearFilters));
        // Scroll / load-outcome variants do NOT request reloads — the
        // app would otherwise recurse on every frame.
        assert!(!update(
            &mut s,
            Message::Scrolled {
                tab_id: 42,
                offset_y: 10.0,
                viewport_height: 600.0,
                content_height: 5000.0,
            },
        ));
        assert!(!update(
            &mut s,
            Message::MessagesLoaded {
                tab_id: 42,
                messages: Vec::new(),
                max_id: 0,
            },
        ));
    }

    #[test]
    fn filter_change_clears_active_preset() {
        let mut s = fresh();
        s.active_preset_id = Some(1);
        update(&mut s, Message::SparkFilterChanged("sp-7".into()));
        assert!(s.active_preset_id.is_none());
    }

    #[test]
    fn activate_preset_loads_filters_and_marks_active() {
        let mut s = fresh();
        s.apply_presets(vec![
            sample_preset(1, "My work", Some("e-1")),
            sample_preset(2, "Review queue", None),
        ]);
        update(&mut s, Message::ActivatePreset(1));
        assert_eq!(s.epic_id_input, "e-1");
        assert_eq!(s.active_preset_id, Some(1));
    }

    #[test]
    fn clear_filters_resets_every_input_and_clears_preset() {
        let mut s = fresh();
        s.epic_id_input = "x".into();
        s.spark_id_input = "y".into();
        s.assignment_id_input = "z".into();
        s.pr_number_input = "1".into();
        s.actor_id_input = "a".into();
        s.fts_query_input = "b".into();
        s.active_preset_id = Some(99);
        s.clear_filters();
        assert!(!s.any_filter_active());
        assert!(s.active_preset_id.is_none());
    }

    #[test]
    fn apply_messages_seeds_max_id_without_bumping_banner_on_first_load() {
        let mut s = fresh();
        // Emulate "user opened the tab, messages were already on the
        // channel". The banner must stay at 0 — otherwise every fresh
        // open would look like there are N new messages below.
        s.scroll_offset_y = 0.0;
        s.content_height = 10_000.0;
        s.viewport_height = 400.0;
        s.apply_messages(vec![sample_message(1, "a"), sample_message(2, "b")], 2);
        assert_eq!(s.last_max_message_id, 2);
        assert_eq!(s.new_messages_banner_count, 0);
    }

    #[test]
    fn apply_messages_bumps_banner_when_scrolled_away_from_tail() {
        let mut s = fresh();
        // Seed first load.
        s.content_height = 10_000.0;
        s.viewport_height = 400.0;
        s.scroll_offset_y = 0.0;
        s.apply_messages(vec![sample_message(1, "a")], 1);
        // Scroll up away from tail.
        s.apply_scroll(100.0, 400.0, 10_000.0);
        assert!(!s.is_at_tail());
        // A new message arrives while scrolled up — banner bumps.
        s.apply_messages(vec![sample_message(1, "a"), sample_message(2, "b")], 2);
        assert_eq!(s.new_messages_banner_count, 1);
    }

    #[test]
    fn apply_messages_no_banner_when_at_tail() {
        let mut s = fresh();
        s.content_height = 300.0;
        s.viewport_height = 400.0;
        s.scroll_offset_y = 0.0;
        // Seed
        s.apply_messages(vec![sample_message(1, "a")], 1);
        // Still at tail (content < viewport).
        s.apply_messages(vec![sample_message(1, "a"), sample_message(2, "b")], 2);
        assert_eq!(s.new_messages_banner_count, 0);
    }

    #[test]
    fn scroll_back_to_tail_clears_banner() {
        let mut s = fresh();
        s.content_height = 10_000.0;
        s.viewport_height = 400.0;
        s.scroll_offset_y = 0.0;
        s.apply_messages(vec![sample_message(1, "a")], 1);
        s.apply_scroll(100.0, 400.0, 10_000.0);
        s.apply_messages(vec![sample_message(2, "b")], 2);
        assert!(s.new_messages_banner_count > 0);
        // Now scroll to the tail.
        s.apply_scroll(9_600.0, 400.0, 10_000.0);
        assert!(s.is_at_tail());
        assert_eq!(s.new_messages_banner_count, 0);
    }

    #[test]
    fn jump_to_tail_snaps_offset_and_clears_banner() {
        let mut s = fresh();
        s.new_messages_banner_count = 7;
        s.content_height = 5_000.0;
        s.viewport_height = 400.0;
        s.scroll_offset_y = 0.0;
        let tab = s.tab_id;
        update(&mut s, Message::JumpToTail(tab));
        assert_eq!(s.new_messages_banner_count, 0);
        // Offset clamps to max.
        assert!((s.scroll_offset_y - (5_000.0_f32 - 400.0)).abs() < 1.0);
    }

    #[test]
    fn apply_presets_drops_stale_unread_entries() {
        let mut s = fresh();
        s.apply_presets(vec![
            sample_preset(1, "a", None),
            sample_preset(2, "b", None),
        ]);
        s.apply_unread(vec![(1, 5), (2, 3)]);
        // Preset 2 was deleted on next refresh.
        s.apply_presets(vec![sample_preset(1, "a", None)]);
        assert!(s.preset_unread_counts.contains_key(&1));
        assert!(!s.preset_unread_counts.contains_key(&2));
    }

    #[test]
    fn apply_unread_merges_counts_without_wiping_others() {
        let mut s = fresh();
        s.apply_presets(vec![
            sample_preset(1, "a", None),
            sample_preset(2, "b", None),
        ]);
        s.apply_unread(vec![(1, 5)]);
        s.apply_unread(vec![(2, 2)]);
        assert_eq!(s.preset_unread_counts.get(&1), Some(&5));
        assert_eq!(s.preset_unread_counts.get(&2), Some(&2));
    }

    #[test]
    fn format_timestamp_trims_date_and_fractional_seconds() {
        assert_eq!(format_timestamp("2026-04-20T12:34:56Z"), "12:34:56");
        assert_eq!(format_timestamp("2026-04-20T12:34:56.789Z"), "12:34:56");
        assert_eq!(format_timestamp("2026-04-20T12:34:56+00:00"), "12:34:56");
        // Non-RFC3339 inputs fall through untouched.
        assert_eq!(format_timestamp("just a string"), "just a string");
    }

    /// Smoke test: the view function constructs cleanly for an empty
    /// state, a populated state, an error state, and a state with the
    /// new-messages banner active. Mirrors the `screen::log_tail` and
    /// `screen::home` smoke tests for consistency (acceptance: "passes
    /// the project existing UI test / smoke conventions").
    #[test]
    fn view_renders_in_every_state() {
        let pal = Palette::dark();

        // Empty (no presets, no messages, no filters).
        let s = fresh();
        let _ = view(&s, &pal, false);

        // With presets + messages + active preset + mention row.
        let mut s = fresh();
        s.apply_presets(vec![
            sample_preset(1, "My work", Some("e-1")),
            sample_preset(2, "Review", None),
        ]);
        s.apply_unread(vec![(1, 3), (2, 0)]);
        let m1 = sample_message(1, "hello");
        let mut m2 = sample_message(2, "@alice please review");
        m2.matched_by_mention = true;
        s.apply_messages(vec![m1, m2], 2);
        s.activate_preset(1);
        let _ = view(&s, &pal, true);

        // Error state.
        let mut s = fresh();
        s.last_error = Some("database error: disk I/O".into());
        let _ = view(&s, &pal, false);

        // New-messages banner active.
        let mut s = fresh();
        s.content_height = 10_000.0;
        s.viewport_height = 400.0;
        s.scroll_offset_y = 0.0;
        s.apply_messages(vec![sample_message(1, "a")], 1);
        s.apply_scroll(100.0, 400.0, 10_000.0);
        s.apply_messages(vec![sample_message(1, "a"), sample_message(2, "b")], 2);
        assert!(s.new_messages_banner_count > 0);
        let _ = view(&s, &pal, false);

        // Empty state with filters applied — exercises the "clear
        // filters" call-to-action branch.
        let mut s = fresh();
        s.fts_query_input = "no-matches".into();
        let _ = view(&s, &pal, false);
    }
}
